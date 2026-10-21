use super::*;
use read_maps::logical_value_charge;
pub(super) mod memo;
#[cfg(test)]
mod pin_scope_tests;
use memo::ResidentMemos;

pub(super) fn tree_scratch_bound<I: PageIo>() -> Result<usize, Error> {
    if BUILD_PIN_BYTES + BUILD_STRING_BYTES + BUILD_TREE_BYTES != BUILD_SCRATCH_BYTES {
        return Err(error("allocation build scratch partitions disagree"));
    }
    let frames = usize::from((MAX_AVL_HEIGHT - 1) / HEIGHTS_PER_PAGE + 1);
    let tree_scratch_bound = frames
        * MAX_MAP_ROWS.next_power_of_two()
        * (size_of::<NodeRow>() + size_of::<ResidentValue>())
        + MAX_MAP_ROWS * 2 * 512
        + 4 * MAX_PAGE_BYTES
        + 512 * size_of::<I::Handle>()
        + MAX_MAP_ROWS * size_of::<identity::WireIdentity>()
        + size_of::<IdentityDictionary>()
        + 2 * size_of::<TextDictionary>();
    if tree_scratch_bound > BUILD_TREE_BYTES || size_of::<I::Handle>() + 256 > 512 {
        return Err(error(
            "allocation page temporary types exceed the tree scratch partition",
        ));
    }
    Ok(tree_scratch_bound)
}

pub(super) trait ImmutableRegistry {
    fn texts(&self, values: &[Text], cancelled: &mut dyn FnMut() -> bool) -> Result<(), Error>;
    fn allocations(
        &self,
        values: &[ResidentValue],
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error>;
}

pub(super) struct ResidentBuild<'a, I: PageIo, C> {
    pub(super) identified: bool,
    pub(super) registry: Option<&'a dyn ImmutableRegistry>,
    pub(super) io: &'a mut I,
    pub(super) old: &'a ResidentMemos,
    pub(super) staged: ResidentMemos,
    pub(super) pins: BTreeMap<u64, I::Handle>,
    pub(super) statistics: Statistics,
    pub(super) cancelled: &'a mut C,
}

impl<I: PageIo, C: FnMut() -> bool> ResidentBuild<'_, I, C> {
    pub(super) fn page(
        &mut self,
        bytes: &[u8],
        ids: impl IntoIterator<Item = u64>,
    ) -> Result<I::Handle, Error> {
        check_replay_cancelled(self.cancelled)?;
        let dependencies = dependency_ids(ids)
            .into_iter()
            .map(|id| self.io.pin(id))
            .collect::<Result<Vec<_>, _>>()?;
        let handle = self.io.plan(bytes, &dependencies)?;
        // A planned blob owns its dependencies until the enclosing frame commits or rolls back.
        for dependency in &dependencies {
            self.pins.remove(&self.io.id(dependency));
        }
        self.statistics.logical_bytes = self
            .statistics
            .logical_bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| error("allocation page byte count overflow"))?;
        Ok(handle)
    }

    pub(super) fn valid_ref(&mut self, id: u64, staged: bool) -> bool {
        let Ok(handle) = self.io.pin(id) else {
            return false;
        };
        if staged {
            return true;
        }
        if !self.io.committed(&handle) {
            return false;
        }
        if !self.pins.contains_key(&id) && self.pins.len() >= BUILD_PIN_BYTES / 512 {
            self.statistics.writer_pin_capacity_misses =
                self.statistics.writer_pin_capacity_misses.saturating_add(1);
            return false;
        }
        self.pins.insert(id, handle);
        self.statistics.writer_pin_bytes_peak = self
            .statistics
            .writer_pin_bytes_peak
            .max(self.pins.len() * 512);
        true
    }

    fn string_ref(&mut self, source: &Text) -> Option<RowRef> {
        self.staged
            .strings
            .get(&source.0)
            .filter(|row| self.valid_ref(row.page, true))
            .or_else(|| {
                self.old
                    .strings
                    .get(&source.0)
                    .filter(|row| self.valid_ref(row.page, false))
            })
    }

    fn value_ref(&mut self, source: &ResidentValue) -> Option<RowRef> {
        self.staged
            .values
            .get(&source.0)
            .filter(|row| self.valid_ref(row.page, true))
            .or_else(|| {
                self.old
                    .values
                    .get(&source.0)
                    .filter(|row| self.valid_ref(row.page, false))
            })
    }

    pub(super) fn strings(&mut self, sources: &[Text]) -> Result<Vec<RowRef>, Error> {
        if let Some(registry) = self.registry {
            registry.texts(sources, self.cancelled)?;
        }
        let mut results = vec![None; sources.len()];
        let mut pending: Vec<Text> = Vec::new();
        let mut positions = Vec::new();
        let mut identities = BTreeMap::new();
        for (index, source) in sources.iter().enumerate() {
            if let Some(reference) = self.string_ref(source) {
                results[index] = Some(reference);
            } else {
                let pointer = source.0.identity();
                let position = *identities.entry(pointer).or_insert_with(|| {
                    pending.push(source.clone());
                    pending.len() - 1
                });
                positions.push((index, position));
            }
        }
        let mut encoded = Vec::with_capacity(pending.len());
        let mut first = 0;
        while first < pending.len() {
            check_replay_cancelled(self.cancelled)?;
            let mut end = first;
            let mut bytes = 10usize;
            while end < pending.len() && end - first < MAX_STRING_ROWS {
                let length = pending[end].len();
                if length > MAX_WRITER_STRING_BYTES {
                    return Err(error(
                        "allocation string exceeds the prototype scratch budget",
                    ));
                }
                let next = add(bytes, add(if self.identified { 28 } else { 4 }, length)?)?;
                if end > first && next > STRING_PAGE_TARGET {
                    break;
                }
                bytes = next;
                end += 1;
                if bytes > STRING_PAGE_TARGET {
                    break;
                }
            }
            let mut dictionary = IdentityDictionary::new();
            if self.identified {
                for value in &pending[first..end] {
                    check_replay_cancelled(self.cancelled)?;
                    dictionary.insert(value.0.wire_identity()?)?;
                }
            }
            let compact = self.identified && dictionary.is_smaller(end - first)?;
            if compact {
                bytes = add(
                    bytes
                        .checked_sub(24 * (end - first))
                        .ok_or_else(|| error("identified string page size disagrees"))?,
                    dictionary.encoded_bytes(end - first)?,
                )?;
            }
            let text_dictionary = if compact {
                let table = TextDictionary::new(
                    end - first,
                    |index| &pending[first + index],
                    self.cancelled,
                )?;
                table.is_smaller().then_some(table)
            } else {
                None
            };
            if let Some(table) = &text_dictionary {
                bytes = add(
                    bytes
                        .checked_sub(table.original_bytes())
                        .ok_or_else(|| error("text dictionary size disagrees"))?,
                    table.encoded_bytes(),
                )?;
            }
            let magic = if text_dictionary.is_some() {
                DEDUP_STRINGS_MAGIC
            } else {
                match (compact, self.identified, bytes > MAX_PAGE_BYTES) {
                    (true, _, true) => TABLE_LARGE_STRING_MAGIC,
                    (true, _, false) => TABLE_STRINGS_MAGIC,
                    (false, true, true) => IDENTIFIED_LARGE_STRING_MAGIC,
                    (false, true, false) => IDENTIFIED_STRINGS_MAGIC,
                    (false, false, true) => LARGE_STRING_MAGIC,
                    (false, false, false) => STRINGS_MAGIC,
                }
            };
            let mut page = Encoder::new(magic);
            page.bytes
                .try_reserve_exact(bytes.saturating_sub(page.bytes.len()))?;
            page.u16((end - first).try_into()?);
            if compact {
                dictionary.write(&mut page);
                for value in &pending[first..end] {
                    check_replay_cancelled(self.cancelled)?;
                    dictionary.write_row(&mut page, value.0.wire_identity()?)?;
                }
            }
            if let Some(table) = &text_dictionary {
                table.write(&mut page, |index| &pending[first + index], self.cancelled)?;
            } else {
                for value in &pending[first..end] {
                    if self.identified && !compact {
                        page.identity(value.0.wire_identity()?);
                    }
                    page.u32(value.len().try_into()?);
                    for chunk in value.as_bytes().chunks(MAX_PAGE_BYTES) {
                        check_replay_cancelled(self.cancelled)?;
                        page.bytes.extend_from_slice(chunk);
                    }
                }
            }
            self.statistics.writer_string_buffer_bytes_peak = self
                .statistics
                .writer_string_buffer_bytes_peak
                .max(page.bytes.capacity());
            if page.bytes.capacity() > BUILD_STRING_BYTES {
                return Err(error(
                    "allocation string encoding exceeds its scratch partition",
                ));
            }
            let handle = self.page(&page.bytes, [])?;
            let id = self.io.id(&handle);
            for (row, source) in pending[first..end].iter().enumerate() {
                let reference = RowRef {
                    page: id,
                    row: row.try_into()?,
                };
                encoded.push(reference);
                self.staged
                    .strings
                    .insert(&source.0, reference, self.cancelled)?;
                self.statistics.new_strings += 1;
            }
            first = end;
        }
        for (index, position) in positions {
            results[index] = Some(encoded[position]);
        }
        results
            .into_iter()
            .map(|reference| reference.ok_or_else(|| error("missing allocation string reference")))
            .collect()
    }

    pub(super) fn values(&mut self, sources: &[ResidentValue]) -> Result<Vec<RowRef>, Error> {
        if let Some(registry) = self.registry {
            registry.allocations(sources, self.cancelled)?;
        }
        let mut results = vec![None; sources.len()];
        let mut pending: Vec<ResidentValue> = Vec::new();
        let mut positions = Vec::new();
        let mut identities = BTreeMap::new();
        for (index, source) in sources.iter().enumerate() {
            if let Some(reference) = self.value_ref(source) {
                results[index] = Some(reference);
            } else {
                let pointer = source.0.identity();
                let position = *identities.entry(pointer).or_insert_with(|| {
                    pending.push(source.clone());
                    pending.len() - 1
                });
                positions.push((index, position));
            }
        }
        let mut encoded = Vec::with_capacity(pending.len());
        for chunk in pending.chunks(MAX_MAP_ROWS) {
            let strings = chunk
                .iter()
                .flat_map(|value| [value.type_name.clone(), value.file.clone()])
                .collect::<Vec<_>>();
            let strings = self.strings(&strings)?;
            let rows = chunk
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    Ok(AllocationRow {
                        address: value.address,
                        size: value.size,
                        type_name: strings[2 * index],
                        file: strings[2 * index + 1],
                        line: value.line,
                        received_frame: value.received_frame.try_into()?,
                    })
                })
                .collect::<Result<Vec<_>, Error>>()?;
            let bytes = if self.identified {
                let identities = chunk
                    .iter()
                    .map(|value| value.0.wire_identity())
                    .collect::<Result<Vec<_>, _>>()?;
                encode_identified_values(&rows, &identities)?
            } else {
                encode_values(&rows)
            };
            let handle = self.page(
                &bytes,
                rows.iter()
                    .flat_map(|row| [row.type_name.page, row.file.page]),
            )?;
            let id = self.io.id(&handle);
            for (row, source) in chunk.iter().enumerate() {
                let reference = RowRef {
                    page: id,
                    row: row.try_into()?,
                };
                encoded.push(reference);
                self.staged
                    .values
                    .insert(&source.0, reference, self.cancelled)?;
                self.statistics.new_values += 1;
            }
        }
        for (index, position) in positions {
            results[index] = Some(encoded[position]);
        }
        results
            .into_iter()
            .map(|reference| reference.ok_or_else(|| error("missing allocation value reference")))
            .collect()
    }

    pub(super) fn map(&mut self, source: &ResidentMap) -> Result<MapRef, Error> {
        let Some(root) = &source.root else {
            if source.len != 0 {
                return Err(error("empty allocation map length disagrees"));
            }
            return Ok(MapRef::empty());
        };
        let result = self.tree(root)?;
        if result.len != source.len as u64 {
            return Err(error("allocation map length disagrees with tree"));
        }
        Ok(result)
    }

    fn tree(&mut self, source: &Owned<ResidentNode>) -> Result<MapRef, Error> {
        check_replay_cancelled(self.cancelled)?;
        if source.height > MAX_AVL_HEIGHT {
            return Err(error("allocation tree exceeds the prototype height limit"));
        }
        let previous = self
            .staged
            .trees
            .get(source)
            .filter(|row| self.valid_ref(row.page, true))
            .or_else(|| {
                self.old
                    .trees
                    .get(source)
                    .filter(|row| self.valid_ref(row.page, false))
            });
        if let Some(previous) = previous {
            self.statistics.reused_tree_pages += 1;
            return Ok(previous);
        }
        let band = source
            .height
            .checked_sub(1)
            .ok_or_else(|| error("zero allocation tree height"))?
            / HEIGHTS_PER_PAGE;
        let mut rows = Vec::new();
        let mut values = Vec::new();
        let (_, mut meta) = self.local(source, band, &mut rows, &mut values)?;
        let references = self.values(&values)?;
        for (row, reference) in rows.iter_mut().zip(references) {
            row.value = reference;
        }
        let ids = rows.iter().flat_map(|row| {
            [
                Some(row.value.page),
                match row.left {
                    Child::External(map) => Some(map.page),
                    _ => None,
                },
                match row.right {
                    Child::External(map) => Some(map.page),
                    _ => None,
                },
            ]
            .into_iter()
            .flatten()
        });
        let bytes = encode_map(meta, &rows)?;
        let handle = self.page(&bytes, ids)?;
        meta.page = self.io.id(&handle);
        self.staged.trees.insert(source, meta);
        self.statistics.new_tree_pages += 1;
        Ok(meta)
    }

    fn child(
        &mut self,
        source: &ResidentLink,
        band: u8,
        rows: &mut Vec<NodeRow>,
        values: &mut Vec<ResidentValue>,
    ) -> Result<(Child, MapRef), Error> {
        let Some(root) = source else {
            return Ok((Child::Empty, MapRef::empty()));
        };
        let child_band = root
            .height
            .checked_sub(1)
            .ok_or_else(|| error("zero allocation tree height"))?
            / HEIGHTS_PER_PAGE;
        if child_band < band {
            let reference = self.tree(root)?;
            Ok((Child::External(reference), reference))
        } else if child_band == band {
            self.local(root, band, rows, values)
        } else {
            Err(error("allocation child height exceeds parent band"))
        }
    }

    fn local(
        &mut self,
        source: &Owned<ResidentNode>,
        band: u8,
        rows: &mut Vec<NodeRow>,
        values: &mut Vec<ResidentValue>,
    ) -> Result<(Child, MapRef), Error> {
        check_replay_cancelled(self.cancelled)?;
        self.statistics.visited_nodes += 1;
        let (left, left_meta) = self.child(&source.left, band, rows, values)?;
        let (right, right_meta) = self.child(&source.right, band, rows, values)?;
        if rows.len() >= MAX_MAP_ROWS
            || left_meta.height.abs_diff(right_meta.height) > 1
            || source.height != 1 + left_meta.height.max(right_meta.height)
            || (left_meta.len != 0 && left_meta.maximum >= source.key)
            || (right_meta.len != 0 && right_meta.minimum <= source.key)
        {
            return Err(error("allocation source tree violates page invariants"));
        }
        let len = left_meta
            .len
            .checked_add(right_meta.len)
            .and_then(|n| n.checked_add(1))
            .ok_or_else(|| error("allocation subtree count overflow"))?;
        let own_charge = add(TREE_ARC_BYTES, logical_value_charge(&source.value)?)? as u64;
        let charge = left_meta
            .charge
            .checked_add(right_meta.charge)
            .and_then(|n| n.checked_add(own_charge))
            .ok_or_else(|| error("allocation subtree accounting overflow"))?;
        let meta = MapRef {
            page: 0,
            len,
            height: source.height,
            charge,
            minimum: if left_meta.len != 0 {
                left_meta.minimum
            } else {
                source.key
            },
            maximum: if right_meta.len != 0 {
                right_meta.maximum
            } else {
                source.key
            },
        };
        let index = rows.len().try_into()?;
        rows.push(NodeRow {
            key: source.key,
            value: RowRef { page: 0, row: 0 },
            left,
            right,
            height: source.height,
            len,
            charge,
        });
        values.push(source.value.clone());
        Ok((Child::Local(index), meta))
    }
}
