use super::*;
use owner::OwnedWeak;

struct Entry<T, R> {
    source: OwnedWeak<T>,
    reference: R,
    age: u64,
    bytes: usize,
}

pub(in super::super) struct Memo<T, R> {
    entries: BTreeMap<usize, Entry<T, R>>,
    ages: BTreeMap<u64, usize>,
    next_age: u64,
    bytes: usize,
    limit: usize,
}

impl<T, R: Clone> Memo<T, R> {
    pub(in super::super) fn visit(
        &self,
        mut visitor: impl FnMut(&Owned<T>, R) -> Result<(), Error>,
    ) -> Result<(), Error> {
        for entry in self.entries.values() {
            if let Some(value) = entry.source.upgrade() {
                visitor(&value, entry.reference.clone())?;
            }
        }
        Ok(())
    }
    fn new(limit: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            ages: BTreeMap::new(),
            next_age: 0,
            bytes: 0,
            limit,
        }
    }

    pub(in super::super) fn get(&self, source: &Owned<T>) -> Option<R> {
        let entry = self.entries.get(&source.identity())?;
        let alive = entry.source.upgrade()?;
        alive.same(source).then(|| entry.reference.clone())
    }

    pub(in super::super) fn insert(&mut self, source: &Owned<T>, reference: R) {
        let bytes = size_of::<Entry<T, R>>()
            + size_of::<usize>()
            + size_of::<(u64, usize)>()
            + TREE_MEMO_ALLOWANCE
            + Owned::<T>::fixed_bytes();
        if bytes > self.limit {
            return;
        }
        let Ok(weak) = source.weak() else {
            return;
        };
        self.insert_entry(
            source.identity(),
            Entry {
                source: weak,
                reference,
                age: 0,
                bytes,
            },
        );
    }

    fn insert_entry(&mut self, key: usize, mut entry: Entry<T, R>) {
        if entry.bytes > self.limit {
            return;
        }
        if let Some(previous) = self.entries.remove(&key) {
            self.ages.remove(&previous.age);
            self.bytes -= previous.bytes;
        }
        while self.bytes.saturating_add(entry.bytes) > self.limit {
            let Some((_, key)) = self.ages.pop_first() else {
                break;
            };
            if let Some(previous) = self.entries.remove(&key) {
                self.bytes -= previous.bytes;
            }
        }
        let Some(next_age) = self.next_age.checked_add(1) else {
            self.entries.clear();
            self.ages.clear();
            self.bytes = 0;
            self.next_age = 0;
            return;
        };
        entry.age = self.next_age;
        self.next_age = next_age;
        self.bytes += entry.bytes;
        self.ages.insert(entry.age, key);
        self.entries.insert(key, entry);
    }

    fn absorb(&mut self, other: Self) {
        for (key, entry) in other.entries {
            self.insert_entry(key, entry);
        }
    }
}

pub(in super::super) struct StableRows<T> {
    rows: identity_table::IdentityTable<RowRef>,
    marker: std::marker::PhantomData<fn() -> T>,
}
impl<T> StableRows<T> {
    fn new(bytes: usize, budget: &Arc<identity_table::MetadataBudget>) -> Self {
        Self {
            rows: identity_table::IdentityTable::with_metadata(bytes, budget),
            marker: std::marker::PhantomData,
        }
    }
    pub(in super::super) fn get(&self, source: &Owned<T>) -> Option<RowRef> {
        self.rows.get(source.wire_identity().ok()?).copied()
    }
    pub(in super::super) fn insert(
        &mut self,
        source: &Owned<T>,
        reference: RowRef,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), Error> {
        self.rows
            .insert(source.wire_identity()?, reference, cancelled)?;
        Ok(())
    }
    fn absorb(&mut self, other: Self, cancelled: &mut impl FnMut() -> bool) -> Result<(), Error> {
        self.rows.absorb(other.rows, cancelled)
    }
}

pub(in super::super) struct ResidentMemos {
    pub(in super::super) trees: Memo<ResidentNode, MapRef>,
    pub(in super::super) values: StableRows<AllocationData>,
    pub(in super::super) strings: StableRows<Box<str>>,
}
impl ResidentMemos {
    pub(in super::super) fn new(
        limit: usize,
        budget: &Arc<identity_table::MetadataBudget>,
    ) -> Self {
        Self {
            trees: Memo::new(limit / 8),
            values: StableRows::new(limit / 4 * 3, budget),
            strings: StableRows::new(limit / 8, budget),
        }
    }
    pub(in super::super) fn rows_statistics(&self) -> [[usize; 4]; 2] {
        [
            self.values.rows.statistics(),
            self.strings.rows.statistics(),
        ]
    }

    pub(in super::super) fn absorb(
        &mut self,
        other: Self,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), Error> {
        self.trees.absorb(other.trees);
        self.values.absorb(other.values, cancelled)?;
        self.strings.absorb(other.strings, cancelled)
    }
}
