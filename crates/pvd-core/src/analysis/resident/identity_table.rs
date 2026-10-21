use super::*;
use owner::OwnedWeak;
use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hasher},
};

const EMPTY: identity::WireIdentity = identity::WireIdentity {
    origin: [0; 16],
    sequence: 0,
};
const MAX_SLOTS: usize = 1 << 20;

pub(super) struct MetadataBudget {
    limit: usize,
    used: AtomicUsize,
}
pub(super) struct MetadataLease {
    budget: Arc<MetadataBudget>,
    bytes: usize,
}
#[derive(Debug)]
struct MetadataLimit;
impl std::fmt::Display for MetadataLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("identity directory metadata reservation is full")
    }
}
impl std::error::Error for MetadataLimit {}
impl MetadataBudget {
    pub(super) fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit,
            used: AtomicUsize::new(0),
        })
    }
    fn reserve(self: &Arc<Self>, bytes: usize) -> Result<MetadataLease, Error> {
        self.used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|&next| next <= self.limit)
            })
            .map_err(|_| Box::new(MetadataLimit) as Error)?;
        Ok(MetadataLease {
            budget: self.clone(),
            bytes,
        })
    }
}
impl Drop for MetadataLease {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

struct Slot<V> {
    key: identity::WireIdentity,
    value: Option<V>,
}

pub(super) struct IdentityTable<V> {
    slots: Vec<Slot<V>>,
    count: usize,
    occupied: usize,
    limit: usize,
    hash: RandomState,
    memory: Option<MemoryLease>,
    metadata: Option<MetadataLease>,
    limit_misses: usize,
    budget_misses: usize,
}
impl<V> IdentityTable<V> {
    pub(super) fn resident(domain: &Domain, slots: usize) -> Result<Self, Error> {
        if slots > MAX_SLOTS {
            return Err(error("identity directory slot limit exceeded"));
        }
        Ok(Self::new(slots, Some(domain.0.reserve(0)?), None))
    }
    pub(super) fn with_metadata(bytes: usize, budget: &Arc<MetadataBudget>) -> Self {
        Self::new(
            (bytes / size_of::<Slot<V>>()).min(MAX_SLOTS),
            None,
            Some(MetadataLease {
                budget: budget.clone(),
                bytes: 0,
            }),
        )
    }
    fn new(limit: usize, memory: Option<MemoryLease>, metadata: Option<MetadataLease>) -> Self {
        let limit = if limit == 0 {
            0
        } else {
            1usize << limit.ilog2()
        };
        Self {
            slots: Vec::new(),
            count: 0,
            occupied: 0,
            limit,
            hash: RandomState::new(),
            memory,
            metadata,
            limit_misses: 0,
            budget_misses: 0,
        }
    }
    fn start(&self, key: identity::WireIdentity, capacity: usize) -> usize {
        Self::index(&self.hash, key, capacity)
    }
    fn index(state: &RandomState, key: identity::WireIdentity, capacity: usize) -> usize {
        let mut hash = state.build_hasher();
        hash.write(&key.origin);
        hash.write_u64(key.sequence);
        hash.finish() as usize & (capacity - 1)
    }
    fn locate(&self, key: identity::WireIdentity) -> Option<usize> {
        if self.slots.is_empty() {
            return None;
        }
        let mut index = self.start(key, self.slots.len());
        for _ in 0..self.slots.len() {
            let slot = &self.slots[index];
            if slot.key.sequence == 0 {
                return None;
            }
            if slot.key == key {
                return Some(index);
            }
            index = (index + 1) & (self.slots.len() - 1);
        }
        None
    }
    pub(super) fn get(&self, key: identity::WireIdentity) -> Option<&V> {
        self.slots[self.locate(key)?].value.as_ref()
    }
    pub(super) fn remove(&mut self, key: identity::WireIdentity) {
        if let Some(index) = self.locate(key)
            && self.slots[index].value.take().is_some()
        {
            self.count -= 1;
        }
    }
    fn rebuild(
        &mut self,
        capacity: usize,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), Error> {
        check_replay_cancelled(cancelled)?;
        let bytes = capacity
            .checked_mul(size_of::<Slot<V>>())
            .ok_or_else(|| error("identity directory size overflow"))?;
        let memory = self
            .memory
            .as_ref()
            .map(|old| old.budget.reserve(bytes))
            .transpose()?;
        let metadata = self
            .metadata
            .as_ref()
            .map(|old| old.budget.reserve(bytes))
            .transpose()?;
        let mut slots = Vec::new();
        slots.try_reserve_exact(capacity)?;
        if slots.capacity() != capacity {
            return Err(error("identity directory capacity exceeds its reservation"));
        }
        for ordinal in 0..capacity {
            if ordinal & 255 == 0 {
                check_replay_cancelled(cancelled)?;
            }
            slots.push(Slot {
                key: EMPTY,
                value: None,
            });
        }
        for (ordinal, old) in self.slots.iter().enumerate() {
            if ordinal & 255 == 0 {
                check_replay_cancelled(cancelled)?;
            }
            if old.value.is_none() {
                continue;
            }
            let mut index = Self::index(&self.hash, old.key, capacity);
            while slots[index].key.sequence != 0 {
                index = (index + 1) & (capacity - 1);
            }
            slots[index].key = old.key;
        }
        check_replay_cancelled(cancelled)?;
        for old in &mut self.slots {
            if let Some(value) = old.value.take() {
                let mut index = Self::index(&self.hash, old.key, capacity);
                while slots[index].key != old.key {
                    index = (index + 1) & (capacity - 1);
                }
                slots[index].value = Some(value);
            }
        }
        let old = std::mem::replace(&mut self.slots, slots);
        drop(old);
        self.memory = memory;
        self.metadata = metadata;
        self.occupied = self.count;
        Ok(())
    }
    pub(super) fn insert(
        &mut self,
        key: identity::WireIdentity,
        value: V,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<bool, Error> {
        key.validate()?;
        check_replay_cancelled(cancelled)?;
        if let Some(index) = self.locate(key) {
            if self.slots[index].value.is_none() {
                self.count += 1;
            }
            self.slots[index].value = Some(value);
            return Ok(true);
        }
        let grow = self.count.saturating_add(1) > self.slots.len() / 5 * 4;
        let capacity = if grow {
            self.slots.len().max(8).saturating_mul(2).min(self.limit)
        } else {
            self.slots.len()
        };
        if capacity == 0 || (grow && capacity == self.slots.len()) {
            self.limit_misses = self.limit_misses.saturating_add(1);
            return Ok(false);
        }
        if (capacity != self.slots.len() || self.occupied.saturating_add(1) >= capacity)
            && let Err(failure) = self.rebuild(capacity, cancelled)
        {
            if failure.downcast_ref::<ResidentBudgetExceeded>().is_some()
                || failure.downcast_ref::<MetadataLimit>().is_some()
                || failure
                    .downcast_ref::<std::collections::TryReserveError>()
                    .is_some()
            {
                self.budget_misses = self.budget_misses.saturating_add(1);
                if capacity != self.slots.len() {
                    return Ok(false);
                }
            } else {
                return Err(failure);
            }
        }
        let mut index = self.start(key, self.slots.len());
        for _ in 0..self.slots.len() {
            let slot = &mut self.slots[index];
            if slot.value.is_none() {
                if slot.key.sequence == 0 {
                    self.occupied += 1;
                }
                slot.key = key;
                slot.value = Some(value);
                self.count += 1;
                return Ok(true);
            }
            index = (index + 1) & (self.slots.len() - 1);
        }
        Ok(false)
    }
    pub(super) fn absorb(
        &mut self,
        mut other: Self,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), Error> {
        let same_budget = match (&self.memory, &other.memory, &self.metadata, &other.metadata) {
            (Some(left), Some(right), None, None) => Arc::ptr_eq(&left.budget, &right.budget),
            (None, None, Some(left), Some(right)) => Arc::ptr_eq(&left.budget, &right.budget),
            _ => false,
        };
        if !same_budget {
            return Err(error(
                "identity directories belong to different reservations",
            ));
        }
        check_replay_cancelled(cancelled)?;
        if self.count == 0 && self.limit == other.limit {
            other.limit_misses = other.limit_misses.saturating_add(self.limit_misses);
            other.budget_misses = other.budget_misses.saturating_add(self.budget_misses);
            *self = other;
            return Ok(());
        }
        for slot in &mut other.slots {
            if let Some(value) = slot.value.take() {
                self.insert(slot.key, value, cancelled)?;
            }
        }
        Ok(())
    }
    pub(super) fn clear(&mut self) {
        self.slots = Vec::new();
        self.count = 0;
        self.occupied = 0;
        if let Some(memory) = &mut self.memory {
            memory.shrink(0).unwrap();
        }
        if let Some(metadata) = &mut self.metadata {
            metadata
                .budget
                .used
                .fetch_sub(metadata.bytes, Ordering::AcqRel);
            metadata.bytes = 0;
        }
    }
    pub(super) fn statistics(&self) -> [usize; 4] {
        [
            self.count,
            self.capacity_bytes(),
            self.limit_misses,
            self.budget_misses,
        ]
    }
    pub(super) fn capacity_bytes(&self) -> usize {
        self.slots.capacity() * size_of::<Slot<V>>()
    }
}

pub(super) trait ImmutableContent {
    fn same_content(&self, other: &Self) -> Result<bool, Error>;
}
impl ImmutableContent for Box<str> {
    fn same_content(&self, other: &Self) -> Result<bool, Error> {
        Ok(self == other)
    }
}
impl ImmutableContent for AllocationData {
    fn same_content(&self, other: &Self) -> Result<bool, Error> {
        Ok(self.address == other.address
            && self.size == other.size
            && self.line == other.line
            && self.received_frame == other.received_frame
            && self.type_name.0.wire_identity()? == other.type_name.0.wire_identity()?
            && self.file.0.wire_identity()? == other.file.0.wire_identity()?
            && *self.type_name == *other.type_name
            && *self.file == *other.file)
    }
}

pub(super) struct Canonical<T> {
    table: IdentityTable<OwnedWeak<T>>,
    domain: Domain,
    sweep_cursor: usize,
}
impl<T: ImmutableContent> Canonical<T> {
    pub(super) fn new(domain: &Domain, limit: usize) -> Result<Self, Error> {
        Ok(Self {
            table: IdentityTable::resident(domain, limit)?,
            domain: domain.clone(),
            sweep_cursor: 0,
        })
    }
    fn sweep_dead(&mut self, maximum: usize) {
        let capacity = self.table.slots.len();
        if capacity == 0 {
            return;
        }
        for _ in 0..maximum.min(capacity) {
            self.sweep_cursor %= capacity;
            let slot = &mut self.table.slots[self.sweep_cursor];
            if slot
                .value
                .as_ref()
                .is_some_and(|weak| weak.upgrade().is_none())
            {
                // Keeping the key preserves probe chains through this tombstone.
                slot.value = None;
                self.table.count -= 1;
            }
            self.sweep_cursor += 1;
        }
    }
    pub(super) fn get(&mut self, key: identity::WireIdentity) -> Option<Owned<T>> {
        let entry = self.table.get(key);
        let present = entry.is_some();
        let value = entry.and_then(OwnedWeak::upgrade);
        if present && value.is_none() {
            self.table.remove(key);
        }
        let pressure =
            value.is_none() && self.table.count.saturating_add(1) > self.table.slots.len() / 5 * 4;
        self.sweep_dead(if pressure { 32 } else { 1 });
        value
    }
    pub(super) fn put(
        &mut self,
        value: &Owned<T>,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), Error> {
        if !value.domain().same(&self.domain) {
            return Err(error("canonical object belongs to another domain"));
        }
        check_replay_cancelled(cancelled)?;
        let key = value.wire_identity()?;
        if let Some(current) = self.get(key) {
            if !current.same_content(value)? {
                return Err(error("stable object identity has conflicting content"));
            }
            return Ok(());
        }
        let weak = match value.weak() {
            Ok(weak) => weak,
            Err(failure) if failure.downcast_ref::<ResidentBudgetExceeded>().is_some() => {
                return Ok(());
            }
            Err(failure) => return Err(failure),
        };
        self.table.insert(key, weak, cancelled)?;
        Ok(())
    }
    pub(super) fn reclaim(&mut self, cancelled: &mut impl FnMut() -> bool) -> Result<(), Error> {
        for index in 0..self.table.slots.len() {
            if index % 256 == 0 {
                check_replay_cancelled(cancelled)?;
            }
            let slot = &mut self.table.slots[index];
            if slot
                .value
                .as_ref()
                .is_some_and(|weak| weak.upgrade().is_none())
            {
                slot.value = None;
                self.table.count -= 1;
            }
        }
        check_replay_cancelled(cancelled)?;
        if self.table.count == 0 {
            self.clear();
        }
        Ok(())
    }
    pub(super) fn clear(&mut self) {
        self.table.clear();
        self.sweep_cursor = 0;
    }
    pub(super) fn capacity_bytes(&self) -> usize {
        self.table.capacity_bytes()
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod lifecycle_tests;
