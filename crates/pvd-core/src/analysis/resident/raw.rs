use super::*;
use std::ops::Range;

pub(super) enum DataRows {
    Values(Vec<AllocationRow>),
    Strings {
        bytes: Vec<u8>,
        ranges: Vec<Range<usize>>,
    },
}
pub(super) struct DataPage {
    pub(super) rows: DataRows,
    pub(super) identities: Option<Vec<identity::WireIdentity>>,
    _memory: MemoryLease,
}

pub(super) struct Traversal {
    pages: BTreeSet<u64>,
    memory: Vec<MemoryLease>,
    limit: usize,
}
impl Traversal {
    pub(super) fn new(nodes: u64) -> Result<Self, Error> {
        Ok(Self {
            pages: BTreeSet::new(),
            memory: Vec::new(),
            limit: usize::try_from(nodes)?.min(1_000_000),
        })
    }
    pub(super) fn insert(&mut self, id: u64, budget: &Arc<MemoryBudget>) -> Result<(), Error> {
        if self.pages.contains(&id) || self.pages.len() >= self.limit {
            return Err(error(
                "allocation subtrees repeat or exceed the traversal limit",
            ));
        }
        let memory = budget.reserve(256)?;
        self.memory.try_reserve(1)?;
        self.pages.insert(id);
        self.memory.push(memory);
        Ok(())
    }
}

pub(super) struct PageReader<I: PageIo> {
    pub(super) io: I,
    pages: BTreeMap<u64, Arc<DataPage>>,
    page_ages: BTreeMap<u64, u64>,
    page_bytes: usize,
    next_age: u64,
    budget: Arc<MemoryBudget>,
    limits: Limits,
    pub(super) statistics: Statistics,
}
impl<I: PageIo> PageReader<I> {
    pub(super) fn with_budget(
        io: I,
        limits: Limits,
        budget: Arc<MemoryBudget>,
    ) -> Result<Self, Error> {
        Ok(Self {
            io,
            pages: BTreeMap::new(),
            page_ages: BTreeMap::new(),
            page_bytes: 0,
            next_age: 0,
            budget,
            limits,
            statistics: Statistics::default(),
        })
    }
    pub(super) fn invalidate(&mut self) {
        self.pages.clear();
        self.page_ages.clear();
        self.page_bytes = 0;
        self.next_age = 0;
        self.io.invalidate_reader();
    }
    pub(super) fn debit(remaining: &mut usize, bytes: usize) -> Result<(), Error> {
        *remaining = remaining
            .checked_sub(bytes)
            .ok_or_else(|| error("allocation page understates its memory accounting"))?;
        Ok(())
    }

    pub(super) fn loaded(
        &mut self,
        id: u64,
        maximum: usize,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(LoadedPage, MemoryLease), Error> {
        check_replay_cancelled(cancelled)?;
        let (length, dependencies) = self.io.info(id)?;
        if length > maximum || dependencies > 512 {
            return Err(error("allocation page exceeds its read limits"));
        }
        let reserved = add(
            add(length, MAX_PAGE_BYTES * 3)?,
            dependencies * size_of::<u64>(),
        )?;
        let memory = loop {
            check_replay_cancelled(cancelled)?;
            match self.budget.reserve(reserved) {
                Ok(memory) => break memory,
                Err(failure) if failure.downcast_ref::<ResidentBudgetExceeded>().is_some() => {
                    let Some((_, oldest)) = self.page_ages.pop_first() else {
                        return Err(failure);
                    };
                    if let Some(page) = self.pages.remove(&oldest) {
                        self.page_bytes -= page._memory.bytes;
                    }
                }
                Err(failure) => return Err(failure),
            }
        };
        let loaded = self.io.read(id, maximum, cancelled)?;
        if loaded.bytes.len() != length || loaded.dependencies.len() != dependencies {
            return Err(error("allocation page changed during reading"));
        }
        self.statistics.read_pages += 1;
        Ok((loaded, memory))
    }

    pub(super) fn data_page(
        &mut self,
        id: u64,
        strings: bool,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Arc<DataPage>, Error> {
        if let Some(page) = self.pages.get(&id) {
            if strings != matches!(&page.rows, DataRows::Strings { .. }) {
                return Err(error("allocation reference has the wrong page kind"));
            }
            return Ok(page.clone());
        }
        let maximum = if strings {
            MAX_STRING_BYTES + 14
        } else {
            MAX_PAGE_BYTES
        };
        let (mut loaded, mut memory) = self.loaded(id, maximum, cancelled)?;
        let mut identities = None;
        let rows = if strings {
            let text_dictionary = loaded.bytes.get(..8) == Some(DEDUP_STRINGS_MAGIC);
            let table = text_dictionary
                || matches!(loaded.bytes.get(..8), Some(magic) if magic == TABLE_STRINGS_MAGIC || magic == TABLE_LARGE_STRING_MAGIC);
            let compact = table
                || matches!(loaded.bytes.get(..8), Some(magic) if magic == COMPACT_STRINGS_MAGIC || magic == COMPACT_LARGE_STRING_MAGIC);
            let identified = compact
                || matches!(loaded.bytes.get(..8), Some(magic) if magic == IDENTIFIED_STRINGS_MAGIC || magic == IDENTIFIED_LARGE_STRING_MAGIC);
            let large = matches!(loaded.bytes.get(..8), Some(magic) if magic == LARGE_STRING_MAGIC || magic == IDENTIFIED_LARGE_STRING_MAGIC || magic == COMPACT_LARGE_STRING_MAGIC || magic == TABLE_LARGE_STRING_MAGIC);
            if !large && loaded.bytes.len() > MAX_PAGE_BYTES {
                return Err(error("string page exceeds its byte limit"));
            }
            let mut input = Decoder::new(
                &loaded.bytes,
                if text_dictionary {
                    DEDUP_STRINGS_MAGIC
                } else {
                    match (table, compact, identified, large) {
                        (true, _, _, true) => TABLE_LARGE_STRING_MAGIC,
                        (true, _, _, false) => TABLE_STRINGS_MAGIC,
                        (false, true, _, true) => COMPACT_LARGE_STRING_MAGIC,
                        (false, true, _, false) => COMPACT_STRINGS_MAGIC,
                        (false, false, true, true) => IDENTIFIED_LARGE_STRING_MAGIC,
                        (false, false, true, false) => IDENTIFIED_STRINGS_MAGIC,
                        (false, false, false, true) => LARGE_STRING_MAGIC,
                        (false, false, false, false) => STRINGS_MAGIC,
                    }
                },
            )?;
            let count = input.count(if large { 1 } else { MAX_STRING_ROWS })?;
            let dictionary = if compact {
                Some(IdentityDictionary::read(&mut input, count)?)
            } else {
                None
            };
            let mut ranges = Vec::new();
            if !text_dictionary {
                ranges.try_reserve_exact(count)?;
            }
            if identified {
                let mut ids = Vec::new();
                ids.try_reserve_exact(count)?;
                identities = Some(ids);
            }
            if table {
                let dictionary = dictionary.as_ref().unwrap();
                let ids = identities.as_mut().unwrap();
                for _ in 0..count {
                    check_replay_cancelled(cancelled)?;
                    ids.push(dictionary.read_row(&mut input)?);
                }
            }
            if text_dictionary {
                let _scratch = self.budget.reserve(TEXT_READ_SCRATCH)?;
                ranges = read_text_ranges(&mut input, count, cancelled)?;
            } else {
                for _ in 0..count {
                    check_replay_cancelled(cancelled)?;
                    if !table && let Some(ids) = &mut identities {
                        ids.push(if let Some(dictionary) = &dictionary {
                            dictionary.read_row(&mut input)?
                        } else {
                            input.identity()?
                        });
                    }
                    let length = usize::try_from(input.u32()?)?;
                    if length > MAX_STRING_BYTES {
                        return Err(error("allocation string exceeds its byte limit"));
                    }
                    let start = input.offset;
                    std::str::from_utf8(input.take(length)?)?;
                    ranges.push(start..input.offset);
                }
            }
            input.finish()?;
            dependencies_match(&[], &loaded.dependencies)?;
            DataRows::Strings {
                bytes: std::mem::take(&mut loaded.bytes),
                ranges,
            }
        } else {
            let (rows, ids) = decode_identified_values(&loaded.bytes)?;
            identities = ids;
            dependencies_match(
                &dependency_ids(
                    rows.iter()
                        .flat_map(|row| [row.type_name.page, row.file.page]),
                ),
                &loaded.dependencies,
            )?;
            DataRows::Values(rows)
        };
        let bytes = add(
            size_of::<DataPage>()
                + 2 * size_of::<usize>()
                + 256
                + identities.as_ref().map_or(0, |ids| {
                    ids.capacity() * size_of::<identity::WireIdentity>()
                }),
            match &rows {
                DataRows::Values(rows) => rows.capacity() * size_of::<AllocationRow>(),
                DataRows::Strings { bytes, ranges } => add(
                    bytes.capacity(),
                    ranges.capacity() * size_of::<Range<usize>>(),
                )?,
            },
        )?;
        drop(loaded);
        memory.shrink(bytes)?;
        let page = Arc::new(DataPage {
            rows,
            identities,
            _memory: memory,
        });
        if bytes <= self.limits.page_cache_bytes {
            while self.page_bytes.saturating_add(bytes) > self.limits.page_cache_bytes {
                let Some((_, id)) = self.page_ages.pop_first() else {
                    break;
                };
                if let Some(old) = self.pages.remove(&id) {
                    self.page_bytes -= old._memory.bytes;
                }
            }
            if let Some(next) = self.next_age.checked_add(1) {
                self.page_bytes += bytes;
                self.pages.insert(id, page.clone());
                self.page_ages.insert(self.next_age, id);
                self.next_age = next;
            }
        }
        Ok(page)
    }
}

#[cfg(test)]
mod tests;
