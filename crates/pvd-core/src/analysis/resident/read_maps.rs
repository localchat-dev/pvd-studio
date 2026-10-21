use super::*;
mod small;

pub(crate) struct ResidentReader<I: PageIo> {
    pub(super) raw: PageReader<I>,
    domain: Domain,
    pub(super) trees: Cache<u64, ResidentNode, MapRef>,
    pub(super) values: identity_table::Canonical<AllocationData>,
    pub(super) strings: identity_table::Canonical<Box<str>>,
    small: small::SmallSubtrees,
    pub statistics: Statistics,
    #[cfg(test)]
    pub(super) trace: Option<MapReadTrace>,
}
#[cfg(test)]
pub(super) struct MapReadTrace {
    pub initial: BTreeSet<u64>,
    pub requested: BTreeSet<u64>,
    pub overflow: bool,
}
impl<I: PageIo> ResidentReader<I> {
    pub(super) fn dynamic(io: I, domain: Domain, limits: ResidentLimits) -> Result<Self, Error> {
        let raw = PageReader::with_budget(
            io,
            Limits {
                page_cache_bytes: limits.page_cache_bytes,
            },
            domain.0.clone(),
        )?;
        Ok(Self {
            raw,
            trees: Cache::live(limits.tree_slots, &domain)?,
            values: identity_table::Canonical::new(&domain, limits.value_slots)?,
            strings: identity_table::Canonical::new(&domain, limits.string_slots)?,
            small: small::SmallSubtrees::new(&domain, limits.tree_slots != 0)?,
            domain,
            statistics: Statistics::default(),
            #[cfg(test)]
            trace: None,
        })
    }
    pub(crate) fn cache_capacity_bytes(&self) -> usize {
        self.trees.capacity_bytes()
            + self.values.capacity_bytes()
            + self.strings.capacity_bytes()
            + self.small.metadata_bytes()
    }

    #[cfg(test)]
    pub(super) fn reset_small_statistics(&mut self) {
        self.small.hits = 0;
    }

    #[cfg(test)]
    pub(super) fn report_small_statistics(&self) {
        self.small.report();
    }

    pub(crate) fn reclaim(&mut self, cancelled: &mut impl FnMut() -> bool) -> Result<(), Error> {
        self.trees.reclaim_dead(cancelled)?;
        self.small.reclaim(cancelled)?;
        self.raw.invalidate();
        self.values.reclaim(cancelled)?;
        self.strings.reclaim(cancelled)
    }
    pub(crate) fn invalidate(&mut self) {
        self.trees.clear();
        self.small.clear();
        self.values.clear();
        self.strings.clear();
        self.raw.invalidate();
    }
    pub(super) fn string(
        &mut self,
        reference: RowRef,
        remaining: &mut usize,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Text, Error> {
        check_replay_cancelled(cancelled)?;
        let page = self.raw.data_page(reference.page, true, cancelled)?;
        let DataRows::Strings { bytes, ranges } = &page.rows else {
            unreachable!()
        };
        let range = ranges
            .get(usize::try_from(reference.row)?)
            .ok_or_else(|| error("resident string row out of range"))?;
        let text = std::str::from_utf8(&bytes[range.clone()])?;
        let charge = string_charge(text)?;
        PageReader::<I>::debit(remaining, charge)?;
        if page.identities.is_none() {
            return Err(error("complete analysis requires stable object identities"));
        }
        let wire = page
            .identities
            .as_ref()
            .map(|ids| ids[reference.row as usize]);
        if let Some(wire) = wire
            && let Some(value) = self.strings.get(wire)
        {
            if &**value != text {
                return Err(error("stable text identity has conflicting content"));
            }
            self.statistics.cached_strings += 1;
            return Ok(Text(value));
        }
        let value = Text::with_wire_identity(text, &self.domain, cancelled, wire)?;
        if wire.is_some() {
            self.strings.put(&value.0, cancelled)?;
        }
        self.statistics.new_strings += 1;
        Ok(value)
    }
    pub(super) fn value(
        &mut self,
        reference: RowRef,
        remaining: &mut usize,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<ResidentValue, Error> {
        check_replay_cancelled(cancelled)?;
        let page = self.raw.data_page(reference.page, false, cancelled)?;
        let DataRows::Values(rows) = &page.rows else {
            unreachable!()
        };
        let row = *rows
            .get(usize::try_from(reference.row)?)
            .ok_or_else(|| error("resident value row out of range"))?;
        if page.identities.is_none() {
            return Err(error("complete analysis requires stable object identities"));
        }
        let wire = page
            .identities
            .as_ref()
            .map(|ids| ids[reference.row as usize]);
        if let Some(wire) = wire
            && let Some(value) = self.values.get(wire)
        {
            if value.address != row.address
                || value.size != row.size
                || value.line != row.line
                || value.received_frame != usize::try_from(row.received_frame)?
            {
                return Err(error("stable allocation identity has conflicting content"));
            }
            self.check_text(row.type_name, &value.type_name, cancelled)?;
            self.check_text(row.file, &value.file, cancelled)?;
            PageReader::<I>::debit(remaining, logical_value_charge(&value)?)?;
            self.statistics.cached_values += 1;
            return Ok(ResidentValue(value));
        }
        let type_name = self.string(row.type_name, remaining, cancelled)?;
        let file = self.string(row.file, remaining, cancelled)?;
        PageReader::<I>::debit(remaining, VALUE_ARC_BYTES)?;
        let allocation = AllocationData {
            address: row.address,
            size: row.size,
            type_name,
            file,
            line: row.line,
            received_frame: row.received_frame.try_into()?,
        };
        let value = ResidentValue(match wire {
            Some(wire) => Owned::imported(allocation, &self.domain, 0, wire)?,
            None => Owned::new(allocation, &self.domain, 0)?,
        });
        if wire.is_some() {
            self.values.put(&value.0, cancelled)?;
        }
        self.statistics.new_values += 1;
        Ok(value)
    }
    fn check_text(
        &mut self,
        reference: RowRef,
        expected: &Text,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), Error> {
        let page = self.raw.data_page(reference.page, true, cancelled)?;
        let DataRows::Strings { bytes, ranges } = &page.rows else {
            unreachable!()
        };
        let row = usize::try_from(reference.row)?;
        let range = ranges
            .get(row)
            .ok_or_else(|| error("resident string row out of range"))?;
        let actual = std::str::from_utf8(&bytes[range.clone()])?;
        if page.identities.is_none() {
            return Err(error("complete analysis requires stable object identities"));
        }
        let expected_identity = expected.0.wire_identity()?;
        if actual != &**expected
            || page
                .identities
                .as_ref()
                .is_some_and(|ids| ids[row] != expected_identity)
        {
            return Err(error(
                "stable allocation text reference has conflicting content",
            ));
        }
        check_replay_cancelled(cancelled)
    }
    pub(super) fn tree(
        &mut self,
        reference: MapRef,
        lower: Option<Key>,
        upper: Option<Key>,
        visited: &mut Traversal,
        remaining: &mut usize,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<ResidentLink, Error> {
        check_replay_cancelled(cancelled)?;
        if reference.len == 0 {
            return Ok(None);
        }
        if lower.is_some_and(|n| reference.minimum <= n)
            || upper.is_some_and(|n| reference.maximum >= n)
        {
            return Err(error("resident subtrees overlap or repeat"));
        }
        visited.insert(reference.page, &self.domain.0)?;
        #[cfg(test)]
        if let Some(trace) = &mut self.trace {
            if trace.requested.len() < 1 << 20 {
                trace.requested.insert(reference.page);
            } else if !trace.requested.contains(&reference.page) {
                trace.overflow = true;
            }
        }
        if let Some((node, meta)) = self.trees.get(reference.page) {
            if meta != reference {
                return Err(error("resident cached tree summary disagrees"));
            }
            PageReader::<I>::debit(remaining, reference.charge.try_into()?)?;
            self.statistics.cached_tree_pages += 1;
            return Ok(Some(node));
        }
        let (loaded, _temporary) = self.raw.loaded(reference.page, MAX_PAGE_BYTES, cancelled)?;
        let mut input = Decoder::new(&loaded.bytes, MAP_MAGIC)?;
        if input.map_header(reference.page)? != reference {
            return Err(error("resident tree summary disagrees"));
        }
        let count = input.count(MAX_MAP_ROWS)?;
        let mut rows = Vec::new();
        rows.try_reserve_exact(count)?;
        let mut dependencies = Vec::new();
        for index in 0..count {
            check_replay_cancelled(cancelled)?;
            let row = NodeRow {
                key: input.key()?,
                value: input.row()?,
                left: input.child(index)?,
                right: input.child(index)?,
                height: input.u8()?,
                len: input.u64()?,
                charge: input.u64()?,
            };
            dependencies.push(row.value.page);
            for child in [row.left, row.right] {
                if let Child::External(meta) = child {
                    dependencies.push(meta.page);
                }
            }
            rows.push(row);
        }
        input.finish()?;
        dependencies_match(&dependency_ids(dependencies), &loaded.dependencies)?;
        let band = (reference.height - 1) / HEIGHTS_PER_PAGE;
        let mut nodes: Vec<Option<(Owned<ResidentNode>, MapRef)>> = Vec::with_capacity(count);
        for row in rows {
            let (left, lm) = self.child(
                row.left,
                band,
                reference.page,
                lower,
                Some(row.key),
                &mut nodes,
                visited,
                remaining,
                cancelled,
            )?;
            let (right, rm) = self.child(
                row.right,
                band,
                reference.page,
                Some(row.key),
                upper,
                &mut nodes,
                visited,
                remaining,
                cancelled,
            )?;
            if lower.is_some_and(|n| row.key <= n)
                || upper.is_some_and(|n| row.key >= n)
                || lm.height.abs_diff(rm.height) > 1
                || row
                    .height
                    .checked_sub(1)
                    .is_none_or(|n| n / HEIGHTS_PER_PAGE != band)
                || row.height
                    != lm
                        .height
                        .max(rm.height)
                        .checked_add(1)
                        .ok_or_else(|| error("resident height overflow"))?
            {
                return Err(error("resident tree key or AVL invariants violated"));
            }
            let value = self.value(row.value, remaining, cancelled)?;
            PageReader::<I>::debit(remaining, TREE_ARC_BYTES)?;
            let own_charge = add(TREE_ARC_BYTES, logical_value_charge(&value)?)? as u64;
            let expected = MapRef {
                page: 0,
                len: lm
                    .len
                    .checked_add(rm.len)
                    .and_then(|n| n.checked_add(1))
                    .ok_or_else(|| error("resident subtree count overflow"))?,
                height: row.height,
                minimum: if lm.len == 0 { row.key } else { lm.minimum },
                maximum: if rm.len == 0 { row.key } else { rm.maximum },
                charge: lm
                    .charge
                    .checked_add(rm.charge)
                    .and_then(|n| n.checked_add(own_charge))
                    .ok_or_else(|| error("resident subtree charge overflow"))?,
            };
            if row.len != expected.len || row.charge != expected.charge {
                return Err(error("resident subtree count or wire charge disagrees"));
            }
            let (node, reused) = self.small.intern(
                ResidentNode {
                    key: row.key,
                    value,
                    left,
                    right,
                    subtree_len: expected.len.try_into()?,
                    height: row.height,
                },
                cancelled,
            )?;
            if node.height == 3 {
                if reused {
                    check_replay_cancelled(cancelled)?;
                } else {
                    self.small.register(&node, cancelled)?;
                }
            } else if node.height > 3 {
                for child in [&node.left, &node.right].into_iter().flatten() {
                    if child.height < 3 {
                        self.small.register(child, cancelled)?;
                    }
                }
            }
            nodes.push(Some((node, expected)));
            if !reused {
                self.statistics.visited_nodes += 1;
            }
        }
        let (root, mut actual) = nodes
            .last_mut()
            .and_then(Option::take)
            .ok_or_else(|| error("resident page lacks root"))?;
        if nodes.iter().any(Option::is_some) {
            return Err(error("resident page has unreachable rows"));
        }
        actual.page = reference.page;
        if actual != reference {
            return Err(error("resident root summary disagrees"));
        }
        if root.height < 3 {
            self.small.register(&root, cancelled)?;
        }
        self.trees.put(reference.page, &root, actual);
        Ok(Some(root))
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "The decoder keeps one bounded transaction across local and external links."
    )]
    fn child(
        &mut self,
        child: Child,
        band: u8,
        owner: u64,
        lower: Option<Key>,
        upper: Option<Key>,
        nodes: &mut [Option<(Owned<ResidentNode>, MapRef)>],
        visited: &mut Traversal,
        remaining: &mut usize,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(ResidentLink, MapRef), Error> {
        let (node, meta) = match child {
            Child::Empty => return Ok((None, MapRef::empty())),
            Child::Local(row) => nodes
                .get_mut(usize::from(row))
                .and_then(Option::take)
                .ok_or_else(|| error("resident local child repeated"))?,
            Child::External(meta) => {
                if meta.page >= owner || (meta.height - 1) / HEIGHTS_PER_PAGE >= band {
                    return Err(error("resident child violates page order or height band"));
                }
                let node = self
                    .tree(meta, lower, upper, visited, remaining, cancelled)?
                    .ok_or_else(|| error("resident external child empty"))?;
                (node, meta)
            }
        };
        if lower.is_some_and(|n| meta.minimum <= n) || upper.is_some_and(|n| meta.maximum >= n) {
            return Err(error("resident child violates global key bounds"));
        }
        Ok((Some(node), meta))
    }
}
pub(super) fn logical_value_charge(value: &AllocationData) -> Result<usize, Error> {
    add(
        add(VALUE_ARC_BYTES, string_charge(&value.type_name)?)?,
        string_charge(&value.file)?,
    )
}
