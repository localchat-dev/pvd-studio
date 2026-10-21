use super::*;
#[cfg(test)]
mod tests;

#[derive(Clone)]
pub(crate) struct Domain(pub(super) Arc<MemoryBudget>);

#[derive(Debug, Serialize)]
pub(crate) struct DomainStats {
    pub logical_used: usize,
    pub logical_peak: usize,
    pub limit: usize,
    pub object_allocations_total: usize,
    pub fixed_allocation_requests_total: usize,
    pub payload_allocation_requests_total: usize,
    pub live_strong_cells: usize,
    pub live_strong_allocation_bytes: usize,
    pub logical_weak_fixed_bytes: usize,
    pub manager_layout_bytes: usize,
    pub buffer_allocated_nodes: usize,
    pub buffer_copied_elements: usize,
    pub buffer_copied_layout_bytes: usize,
    pub buffer_moved_elements: usize,
    pub buffer_moved_layout_bytes: usize,
    pub tree_copied_entries: usize,
    pub tree_copied_layout_bytes: usize,
}

pub(crate) struct Reservation {
    _memory: MemoryLease,
}

impl Domain {
    pub(crate) fn new(bytes: usize) -> Self {
        Self(MemoryBudget::new(bytes.min(192 << 20)))
    }
    pub(crate) fn statistics(&self) -> DomainStats {
        let b = &self.0;
        DomainStats {
            logical_used: b.used.load(Ordering::Acquire),
            logical_peak: b.peak.load(Ordering::Acquire),
            limit: b.limit,
            object_allocations_total: b.resident_allocations.load(Ordering::Relaxed),
            fixed_allocation_requests_total: b.resident_fixed_allocated.load(Ordering::Relaxed),
            payload_allocation_requests_total: b.resident_payload_allocated.load(Ordering::Relaxed),
            live_strong_cells: b.resident_alive.load(Ordering::Relaxed),
            live_strong_allocation_bytes: b.resident_strong_bytes.load(Ordering::Relaxed),
            logical_weak_fixed_bytes: b.resident_weak_bytes.load(Ordering::Relaxed),
            manager_layout_bytes: size_of::<MemoryBudget>() + 2 * size_of::<usize>(),
            buffer_allocated_nodes: b.buffer_allocated_nodes.load(Ordering::Relaxed),
            buffer_copied_elements: b.buffer_copied_elements.load(Ordering::Relaxed),
            buffer_copied_layout_bytes: b.buffer_copied_layout_bytes.load(Ordering::Relaxed),
            buffer_moved_elements: b.buffer_moved_elements.load(Ordering::Relaxed),
            buffer_moved_layout_bytes: b.buffer_moved_layout_bytes.load(Ordering::Relaxed),
            tree_copied_entries: b.tree_copied_entries.load(Ordering::Relaxed),
            tree_copied_layout_bytes: b.tree_copied_layout_bytes.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub(crate) fn reserve(&self, bytes: usize) -> Result<Reservation, Error> {
        Ok(Reservation {
            _memory: self.0.reserve(bytes)?,
        })
    }
}

pub(super) fn cumulative(counter: &AtomicUsize, bytes: usize) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
        Some(n.saturating_add(bytes))
    });
}

struct Cell<T> {
    value: Option<T>,
    memory: MemoryLease,
    identity: usize,
}
impl<T> Drop for Cell<T> {
    fn drop(&mut self) {
        self.value.take();
        let budget = &self.memory.budget;
        budget.identities.release(self.identity);
        budget.resident_alive.fetch_sub(1, Ordering::Relaxed);
        budget
            .resident_strong_bytes
            .fetch_sub(self.memory.bytes, Ordering::Relaxed);
    }
}

pub(super) struct Owned<T>(Arc<Cell<T>>);
impl<T> Clone for Owned<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<T> Deref for Owned<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.0.value.as_ref().unwrap()
    }
}
impl<T> Owned<T> {
    pub(super) fn fixed_bytes() -> usize {
        size_of::<Cell<T>>() + 2 * size_of::<usize>()
    }
    pub(super) fn new(value: T, domain: &Domain, payload_bytes: usize) -> Result<Self, Error> {
        let bytes = add(Self::fixed_bytes(), payload_bytes)?;
        let memory = domain.0.reserve(bytes)?;
        Self::from_reserved(value, memory, payload_bytes)
    }
    pub(super) fn from_reserved(
        value: T,
        memory: MemoryLease,
        payload_bytes: usize,
    ) -> Result<Self, Error> {
        let budget = memory.budget.clone();
        let identity = budget.identities.local()?;
        Ok(Self::with_identity(value, memory, payload_bytes, identity))
    }
    pub(super) fn imported(
        value: T,
        domain: &Domain,
        payload_bytes: usize,
        wire: identity::WireIdentity,
    ) -> Result<Self, Error> {
        let memory = domain.0.reserve(add(Self::fixed_bytes(), payload_bytes)?)?;
        let identity = domain.0.identities.import(wire)?;
        Ok(Self::with_identity(value, memory, payload_bytes, identity))
    }
    pub(super) fn from_reserved_imported(
        value: T,
        memory: MemoryLease,
        payload_bytes: usize,
        wire: identity::WireIdentity,
    ) -> Result<Self, Error> {
        let budget = memory.budget.clone();
        let identity = budget.identities.import(wire)?;
        Ok(Self::with_identity(value, memory, payload_bytes, identity))
    }
    fn with_identity(
        value: T,
        memory: MemoryLease,
        payload_bytes: usize,
        identity: identity::Permit<'_>,
    ) -> Self {
        let budget = &memory.budget;
        cumulative(&budget.resident_allocations, 1);
        cumulative(&budget.resident_fixed_allocated, Self::fixed_bytes());
        cumulative(&budget.resident_payload_allocated, payload_bytes);
        budget.resident_alive.fetch_add(1, Ordering::Relaxed);
        budget
            .resident_strong_bytes
            .fetch_add(memory.bytes, Ordering::Relaxed);
        Self(Arc::new(Cell {
            value: Some(value),
            memory,
            identity: identity.take(),
        }))
    }
    pub(super) fn domain(&self) -> Domain {
        Domain(self.0.memory.budget.clone())
    }
    pub(super) fn same_domain(&self, domain: &Domain) -> bool {
        Arc::ptr_eq(&self.0.memory.budget, &domain.0)
    }
    pub(super) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub(super) fn identity(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }
    pub(super) fn semantic_identity(&self) -> usize {
        self.0.identity
    }
    pub(super) fn wire_identity(&self) -> Result<identity::WireIdentity, Error> {
        self.0.memory.budget.identities.wire(self.0.identity)
    }
    pub(super) fn weak(&self) -> Result<OwnedWeak<T>, Error> {
        let memory = self.0.memory.budget.reserve(Self::fixed_bytes())?;
        memory
            .budget
            .resident_weak_bytes
            .fetch_add(memory.bytes, Ordering::Relaxed);
        Ok(OwnedWeak {
            value: Arc::downgrade(&self.0),
            memory,
        })
    }
}

impl<T: Serialize> Serialize for Owned<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.deref().serialize(serializer)
    }
}
// Independent payloads use budgeted builders; inline COW clones only scalars and owned handles.
pub(super) trait InlineCow: Clone {
    fn copied(_domain: &Domain) {}
}
impl InlineCow for ResidentNode {}

impl<T: InlineCow> Owned<T> {
    pub(super) fn exclusive_mut(&mut self) -> Result<&mut T, Error> {
        if self.0.memory.bytes != Self::fixed_bytes() {
            return Err(error(
                "exclusive mutation cannot grow an independent heap payload",
            ));
        }
        Arc::get_mut(&mut self.0)
            .and_then(|cell| cell.value.as_mut())
            .ok_or_else(|| error("resident object is not exclusive"))
    }

    pub(super) fn make_mut(&mut self) -> Result<&mut T, Error> {
        if self.0.memory.bytes != Self::fixed_bytes() {
            return Err(error("inline COW cannot clone an independent heap payload"));
        }
        if Arc::get_mut(&mut self.0).is_none() {
            let domain = self.domain();
            let replacement = Self::new(self.0.value.as_ref().unwrap().clone(), &domain, 0)?;
            T::copied(&domain);
            self.0 = replacement.0;
        }
        Ok(Arc::get_mut(&mut self.0).unwrap().value.as_mut().unwrap())
    }
}

pub(super) struct OwnedWeak<T> {
    value: Weak<Cell<T>>,
    memory: MemoryLease,
}
impl<T> OwnedWeak<T> {
    pub(super) fn upgrade(&self) -> Option<Owned<T>> {
        self.value.upgrade().map(Owned)
    }
}
impl<T> Drop for OwnedWeak<T> {
    fn drop(&mut self) {
        self.memory
            .budget
            .resident_weak_bytes
            .fetch_sub(self.memory.bytes, Ordering::Relaxed);
    }
}

#[derive(Clone)]
pub(crate) struct Text(pub(super) Owned<Box<str>>);
impl Serialize for Text {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self)
    }
}
impl Deref for Text {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}
impl Text {
    pub(super) fn new(
        value: &str,
        domain: &Domain,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Self, Error> {
        Self::with_wire_identity(value, domain, cancelled, None)
    }
    pub(super) fn with_wire_identity(
        value: &str,
        domain: &Domain,
        cancelled: &mut impl FnMut() -> bool,
        wire: Option<identity::WireIdentity>,
    ) -> Result<Self, Error> {
        check_replay_cancelled(cancelled)?;
        let memory = domain
            .0
            .reserve(add(Owned::<Box<str>>::fixed_bytes(), value.len())?)?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(value.len())?;
        if bytes.capacity() != value.len() {
            return Err(error("resident text capacity exceeds its reservation"));
        }
        for part in value.as_bytes().chunks(MAX_PAGE_BYTES) {
            check_replay_cancelled(cancelled)?;
            bytes.extend_from_slice(part);
        }
        let boxed = String::from_utf8(bytes)?.into_boxed_str();
        Ok(Self(match wire {
            Some(wire) => Owned::from_reserved_imported(boxed, memory, value.len(), wire)?,
            None => Owned::from_reserved(boxed, memory, value.len())?,
        }))
    }
}

#[derive(Clone, Serialize)]
pub(crate) struct AllocationData {
    pub address: u64,
    pub size: u64,
    pub type_name: Text,
    pub file: Text,
    pub line: u32,
    pub received_frame: usize,
}
#[derive(Clone)]
pub(crate) struct ResidentValue(pub(super) Owned<AllocationData>);
impl Serialize for ResidentValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}
impl Deref for ResidentValue {
    type Target = AllocationData;
    fn deref(&self) -> &AllocationData {
        &self.0
    }
}
impl ResidentValue {
    #[cfg(test)]
    pub(crate) fn from_source(
        value: &Allocation,
        domain: &Domain,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Self, Error> {
        let type_name = Text::new(&value.type_name, domain, cancelled)?;
        let file = Text::new(&value.file, domain, cancelled)?;
        Self::new(
            AllocationData {
                address: value.address,
                size: value.size,
                type_name,
                file,
                line: value.line,
                received_frame: value.received_frame,
            },
            domain,
        )
    }
    pub(super) fn new(value: AllocationData, domain: &Domain) -> Result<Self, Error> {
        if !value.type_name.0.domain().same(domain) || !value.file.0.domain().same(domain) {
            return Err(error("resident value contains a foreign budget domain"));
        }
        Ok(Self(Owned::new(value, domain, 0)?))
    }
}

struct Entry<K, T, R> {
    key: K,
    value: OwnedWeak<T>,
    reference: R,
}
pub(super) struct Cache<K, T, R> {
    slots: Vec<Option<Entry<K, T, R>>>,
    directory: BTreeMap<K, usize>,
    next_slot: usize,
    dynamic_limit: Option<usize>,
    preserve_live: bool,
    _memory: MemoryLease,
}
impl<K: Copy + Ord, T, R: Copy> Cache<K, T, R> {
    pub(super) fn new(slots: usize, domain: &Domain) -> Result<Self, Error> {
        if slots > (1 << 20) {
            return Err(error("resident memo slot limit exceeded"));
        }
        let entry_bytes = add(
            size_of::<Option<Entry<K, T, R>>>(),
            size_of::<K>() + size_of::<usize>() + TREE_MEMO_ALLOWANCE,
        )?;
        let bytes = slots
            .checked_mul(entry_bytes)
            .ok_or_else(|| error("resident memo size overflow"))?;
        let memory = domain.0.reserve(bytes)?;
        let mut entries = Vec::new();
        entries.try_reserve_exact(slots)?;
        if entries.capacity() != slots {
            return Err(error("resident memo capacity exceeds its reservation"));
        }
        entries.resize_with(slots, || None);
        Ok(Self {
            slots: entries,
            directory: BTreeMap::new(),
            next_slot: 0,
            dynamic_limit: None,
            preserve_live: false,
            _memory: memory,
        })
    }
    pub(super) fn dynamic(limit: usize, domain: &Domain) -> Result<Self, Error> {
        if limit > (1 << 20) {
            return Err(error("resident memo slot limit exceeded"));
        }
        let mut cache = Self::new(0, domain)?;
        cache.dynamic_limit = Some(limit);
        Ok(cache)
    }
    pub(super) fn live(limit: usize, domain: &Domain) -> Result<Self, Error> {
        let mut cache = Self::dynamic(limit, domain)?;
        cache.preserve_live = true;
        Ok(cache)
    }
    fn grow(&mut self) -> Result<(), Error> {
        let Some(limit) = self.dynamic_limit else {
            return Ok(());
        };
        let old = self.slots.len();
        if self.directory.len() < old || old == limit {
            return Ok(());
        }
        let count = old.max(8).saturating_mul(2).min(limit);
        let replacement = Self::new(count, &Domain(self._memory.budget.clone()))?;
        let mut slots = replacement.slots;
        for (target, entry) in slots.iter_mut().zip(self.slots.iter_mut()) {
            *target = entry.take();
        }
        let previous = std::mem::replace(&mut self.slots, slots);
        drop(previous);
        self._memory = replacement._memory;
        self.next_slot = old;
        Ok(())
    }
    fn vacant_slot(&mut self, key: K) -> Option<usize> {
        if let Some(slot) = self.directory.remove(&key) {
            return Some(slot);
        }
        if self.preserve_live {
            if let Some(slot) = self.reusable_slot() {
                return Some(slot);
            }
            let before = self.slots.len();
            let _ = self.grow();
            if self.slots.len() > before {
                self.next_slot = (before + 1) % self.slots.len();
                return Some(before);
            }
            return None;
        }
        let _ = self.grow();
        if self.slots.is_empty() {
            return None;
        }
        let slot = self.next_slot;
        self.next_slot = (slot + 1) % self.slots.len();
        Some(slot)
    }
    fn reusable_slot(&mut self) -> Option<usize> {
        if self.slots.is_empty() {
            return None;
        }
        let mut empty = None;
        for _ in 0..self.slots.len().min(32) {
            let slot = self.next_slot % self.slots.len();
            self.next_slot = (slot + 1) % self.slots.len();
            match &self.slots[slot] {
                Some(entry) if entry.value.upgrade().is_none() => return Some(slot),
                None => {
                    empty.get_or_insert(slot);
                }
                _ => {}
            }
        }
        if let Some(slot) = empty {
            self.next_slot = (slot + 1) % self.slots.len();
        }
        empty
    }
    pub(super) fn get(&self, key: K) -> Option<(Owned<T>, R)> {
        if self.slots.is_empty() {
            return None;
        }
        let slot = *self.directory.get(&key)?;
        let entry = self.slots.get(slot)?.as_ref()?;
        (entry.key == key)
            .then(|| entry.value.upgrade().map(|value| (value, entry.reference)))
            .flatten()
    }
    pub(super) fn put(&mut self, key: K, value: &Owned<T>, reference: R) {
        if !Arc::ptr_eq(&self._memory.budget, &value.0.memory.budget) {
            return;
        }
        if self.preserve_live && self.get(key).is_some() {
            return;
        }
        let Some(slot) = self.vacant_slot(key) else {
            return;
        };
        if let Some(previous) = self.slots[slot].take() {
            self.directory.remove(&previous.key);
        }
        if let Ok(value) = value.weak() {
            self.slots[slot] = Some(Entry {
                key,
                value,
                reference,
            });
            self.directory.insert(key, slot);
        }
    }
    pub(super) fn absorb(&mut self, mut other: Self) -> Result<(), Error> {
        if !Arc::ptr_eq(&self._memory.budget, &other._memory.budget) {
            return Err(error("resident cache belongs to another domain"));
        }
        for entry in other.slots.iter_mut().filter_map(Option::take) {
            if self.preserve_live && self.get(entry.key).is_some() {
                continue;
            }
            let Some(slot) = self.vacant_slot(entry.key) else {
                continue;
            };
            if let Some(previous) = self.slots[slot].take() {
                self.directory.remove(&previous.key);
            }
            self.directory.insert(entry.key, slot);
            self.slots[slot] = Some(entry);
        }
        Ok(())
    }
    pub(super) fn visit(
        &self,
        mut visitor: impl FnMut(K, &Owned<T>, R) -> Result<(), Error>,
    ) -> Result<(), Error> {
        for entry in self.slots.iter().flatten() {
            if let Some(value) = entry.value.upgrade() {
                visitor(entry.key, &value, entry.reference)?;
            }
        }
        Ok(())
    }
    pub(super) fn reclaim_dead(
        &mut self,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), Error> {
        for index in 0..self.slots.len() {
            if index % 256 == 0 {
                check_replay_cancelled(cancelled)?;
            }
            if self.slots[index]
                .as_ref()
                .is_some_and(|entry| entry.value.upgrade().is_none())
            {
                let entry = self.slots[index].take().unwrap();
                self.directory.remove(&entry.key);
            }
        }
        check_replay_cancelled(cancelled)?;
        if self.directory.is_empty() {
            self.clear();
        }
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn live_keys(&self) -> impl Iterator<Item = K> + '_ {
        self.slots
            .iter()
            .flatten()
            .filter_map(|entry| entry.value.upgrade().map(|_| entry.key))
    }
    pub(super) fn clear(&mut self) {
        self.directory.clear();
        self.next_slot = 0;
        for slot in &mut self.slots {
            slot.take();
        }
        if self.dynamic_limit.is_some() {
            self.slots = Vec::new();
            self.directory = BTreeMap::new();
            self._memory.shrink(0).unwrap();
        }
    }
    pub(super) fn capacity_bytes(&self) -> usize {
        self._memory.bytes
    }
    #[cfg(test)]
    pub(super) fn slot_capacity_bytes(&self) -> usize {
        self.slots.capacity() * size_of::<Option<Entry<K, T, R>>>()
    }
}
