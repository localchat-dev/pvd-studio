use super::*;
use crate::analysis::resident::owner::OwnedWeak;

#[cfg(test)]
mod tests;

const MAX_SLOTS: usize = 1 << 18;
const MAX_BYTES: usize = 12 << 20;

struct Group {
    root: OwnedWeak<ResidentNode>,
    entries: AtomicUsize,
}

struct Slot {
    hash: u64,
    occupied: bool,
    path: u8,
    group: Option<Owned<Group>>,
}

pub(super) struct SmallSubtrees {
    domain: Domain,
    slots: Vec<Slot>,
    memory: MemoryLease,
    count: usize,
    occupied: usize,
    groups: usize,
    sweep: usize,
    limit_bytes: usize,
    limit_slots: usize,
    #[cfg(test)]
    pub hits: usize,
}

impl SmallSubtrees {
    pub fn new(domain: &Domain, enabled: bool) -> Result<Self, Error> {
        Self::with_limits(
            domain,
            if enabled { MAX_SLOTS } else { 0 },
            MAX_BYTES.min(domain.0.limit / 16),
        )
    }

    fn with_limits(domain: &Domain, slots: usize, bytes: usize) -> Result<Self, Error> {
        if slots > MAX_SLOTS || !slots.is_power_of_two() && slots != 0 {
            return Err(error("small subtree directory slot limit is invalid"));
        }
        Ok(Self {
            domain: domain.clone(),
            slots: Vec::new(),
            memory: domain.0.reserve(0)?,
            count: 0,
            occupied: 0,
            groups: 0,
            sweep: 0,
            limit_bytes: bytes.min(MAX_BYTES),
            limit_slots: slots,
            #[cfg(test)]
            hits: 0,
        })
    }

    fn group_bytes() -> usize {
        Owned::<Group>::fixed_bytes() + Owned::<ResidentNode>::fixed_bytes()
    }

    pub fn bytes(&self) -> usize {
        self.metadata_bytes() + self.groups * Owned::<ResidentNode>::fixed_bytes()
    }

    pub fn metadata_bytes(&self) -> usize {
        self.memory.bytes + self.groups * Owned::<Group>::fixed_bytes()
    }

    #[cfg(test)]
    pub fn report(&self) {
        eprintln!(
            "analysis_small_subtrees hits={} entries={} groups={} slots={} metadata_bytes={} weak_bytes={} total_bytes={} byte_limit={}",
            self.hits,
            self.count,
            self.groups,
            self.slots.len(),
            self.metadata_bytes(),
            self.groups * Owned::<ResidentNode>::fixed_bytes(),
            self.bytes(),
            self.limit_bytes,
        );
    }

    #[cfg(test)]
    pub fn entries(&self) -> usize {
        self.count
    }

    fn remove(&mut self, index: usize) {
        if let Some(group) = self.slots[index].group.take() {
            self.count -= 1;
            if group.entries.fetch_sub(1, Ordering::Relaxed) == 1 {
                self.groups -= 1;
            }
        }
    }

    fn sweep(&mut self, checks: usize) {
        for _ in 0..checks.min(self.slots.len()) {
            let index = self.sweep % self.slots.len();
            self.sweep = (index + 1) % self.slots.len();
            if self.slots[index]
                .group
                .as_ref()
                .is_some_and(|group| group.root.upgrade().is_none())
            {
                self.remove(index);
            }
        }
    }

    pub fn reclaim(&mut self, cancelled: &mut impl FnMut() -> bool) -> Result<(), Error> {
        for index in 0..self.slots.len() {
            if index % 256 == 0 {
                check_replay_cancelled(cancelled)?;
            }
            if self.slots[index]
                .group
                .as_ref()
                .is_some_and(|group| group.root.upgrade().is_none())
            {
                self.remove(index);
            }
        }
        check_replay_cancelled(cancelled)?;
        if self.count == 0 {
            self.clear();
        }
        Ok(())
    }

    pub fn clear(&mut self) {
        self.slots = Vec::new();
        self.count = 0;
        self.occupied = 0;
        self.groups = 0;
        self.sweep = 0;
        self.memory.shrink(0).unwrap();
    }

    fn grow(
        &mut self,
        capacity: usize,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<bool, Error> {
        check_replay_cancelled(cancelled)?;
        if capacity == 0
            || !capacity.is_power_of_two()
            || capacity > self.limit_slots
            || self.count >= capacity
        {
            return Err(error("small subtree directory capacity is invalid"));
        }
        let bytes = capacity
            .checked_mul(size_of::<Slot>())
            .ok_or_else(|| error("small subtree directory layout overflow"))?;
        if add(self.bytes(), bytes)? > self.limit_bytes {
            return Ok(false);
        }
        let Ok(memory) = self.domain.0.reserve(bytes) else {
            return Ok(false);
        };
        let mut slots = Vec::new();
        if slots.try_reserve_exact(capacity).is_err() || slots.capacity() != capacity {
            return Ok(false);
        }
        for index in 0..capacity {
            if index % 256 == 0 {
                check_replay_cancelled(cancelled)?;
            }
            slots.push(Slot {
                hash: 0,
                occupied: false,
                path: 0,
                group: None,
            });
        }
        for (offset, slot) in self.slots.iter().enumerate() {
            if offset % 256 == 0 {
                check_replay_cancelled(cancelled)?;
            }
            if let Some(group) = &slot.group {
                let mut index = slot.hash as usize & (capacity - 1);
                let mut checked = 0usize;
                while slots[index].occupied {
                    if checked.is_multiple_of(64) {
                        check_replay_cancelled(cancelled)?;
                    }
                    index = (index + 1) & (capacity - 1);
                    checked += 1;
                }
                slots[index] = Slot {
                    hash: slot.hash,
                    occupied: true,
                    path: slot.path,
                    group: Some(group.clone()),
                };
            }
        }
        check_replay_cancelled(cancelled)?;
        let previous = std::mem::replace(&mut self.slots, slots);
        drop(previous);
        self.memory = memory;
        self.occupied = self.count;
        self.sweep = 0;
        Ok(true)
    }

    fn candidate(
        &mut self,
        node: &ResidentNode,
        hash: u64,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Option<Owned<ResidentNode>>, Error> {
        self.sweep(1);
        if self.slots.is_empty() {
            return Ok(None);
        }
        let mut index = hash as usize & (self.slots.len() - 1);
        for checked in 0..self.slots.len() {
            if checked % 64 == 0 {
                check_replay_cancelled(cancelled)?;
            }
            let slot = &self.slots[index];
            if !slot.occupied {
                return Ok(None);
            }
            if slot.hash == hash
                && let Some(group) = &slot.group
                && let Some(root) = group.root.upgrade()
                && let Some(candidate) = at_path(&root, slot.path)
                && equivalent(node, &candidate, &self.domain)?
            {
                return Ok(Some(candidate));
            }
            index = (index + 1) & (self.slots.len() - 1);
        }
        Ok(None)
    }

    pub fn intern(
        &mut self,
        node: ResidentNode,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(Owned<ResidentNode>, bool), Error> {
        check_replay_cancelled(cancelled)?;
        check_domain(&node, &self.domain)?;
        if node.height <= 3 && self.limit_slots != 0 {
            let hash = fingerprint(&node, &self.domain)?;
            if let Some(existing) = self.candidate(&node, hash, cancelled)? {
                #[cfg(test)]
                {
                    self.hits = self.hits.saturating_add(1);
                }
                return Ok((existing, true));
            }
        }
        Ok((Owned::new(node, &self.domain, 0)?, false))
    }

    pub fn register(
        &mut self,
        root: &Owned<ResidentNode>,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), Error> {
        check_replay_cancelled(cancelled)?;
        if !root.same_domain(&self.domain) {
            return Err(error(
                "small subtree registration belongs to another domain",
            ));
        }
        if root.height > 3 || root.subtree_len > 7 {
            return Err(error("small subtree registration exceeds its shape bound"));
        }
        if self.limit_slots == 0 {
            return Ok(());
        }
        self.sweep(32);
        let mut paths = [0u8; 7];
        let count = paths_for(root, 1, &mut paths, 0)?;
        let mut hashes = [0u64; 7];
        let mut missing = [false; 7];
        let mut absent = 0;
        for index in 0..count {
            let node = at_path(root, paths[index]).unwrap();
            if !node.same_domain(&self.domain) {
                return Err(error(
                    "small subtree registration contains a foreign domain",
                ));
            }
            hashes[index] = fingerprint(&node, &self.domain)?;
            missing[index] = self.candidate(&node, hashes[index], cancelled)?.is_none();
            absent += usize::from(missing[index]);
        }
        if absent == 0 || add(self.bytes(), Self::group_bytes())? > self.limit_bytes {
            return Ok(());
        }
        let capacity = self.slots.len();
        if capacity == 0 || (self.occupied + absent) * 5 > capacity * 4 {
            let next = if (self.count + absent) * 5 <= capacity * 4 {
                capacity
            } else {
                capacity.max(32).saturating_mul(2).min(self.limit_slots)
            };
            if next != 0 && (next != capacity || self.occupied != self.count) {
                self.grow(next, cancelled)?;
            }
        }
        if self.slots.is_empty()
            || (self.count + 1) * 5 > self.slots.len() * 4
            || add(self.bytes(), Self::group_bytes())? > self.limit_bytes
        {
            return Ok(());
        }
        let Ok(weak) = root.weak() else {
            return Ok(());
        };
        let Ok(group) = Owned::new(
            Group {
                root: weak,
                entries: AtomicUsize::new(0),
            },
            &self.domain,
            0,
        ) else {
            return Ok(());
        };
        for index in 0..count {
            check_replay_cancelled(cancelled)?;
            if !missing[index] || (self.count + 1) * 5 > self.slots.len() * 4 {
                continue;
            }
            let mut slot = hashes[index] as usize & (self.slots.len() - 1);
            for checked in 0..self.slots.len() {
                if checked.is_multiple_of(64) {
                    check_replay_cancelled(cancelled)?;
                }
                if self.slots[slot].group.is_none() {
                    if !self.slots[slot].occupied {
                        self.occupied += 1;
                    }
                    if group.entries.fetch_add(1, Ordering::Relaxed) == 0 {
                        self.groups += 1;
                    }
                    self.slots[slot] = Slot {
                        hash: hashes[index],
                        occupied: true,
                        path: paths[index],
                        group: Some(group.clone()),
                    };
                    self.count += 1;
                    break;
                }
                slot = (slot + 1) & (self.slots.len() - 1);
            }
        }
        Ok(())
    }
}

fn paths_for(
    node: &Owned<ResidentNode>,
    path: u8,
    paths: &mut [u8; 7],
    mut count: usize,
) -> Result<usize, Error> {
    if count == paths.len() || path >= 8 {
        return Err(error("small subtree path exceeds its shape bound"));
    }
    paths[count] = path;
    count += 1;
    for (child, path) in [(&node.left, path * 2), (&node.right, path * 2 + 1)] {
        if let Some(child) = child {
            count = paths_for(child, path, paths, count)?;
        }
    }
    Ok(count)
}

fn at_path(root: &Owned<ResidentNode>, path: u8) -> Option<Owned<ResidentNode>> {
    let mut node = root;
    match path {
        1 => {}
        2 | 3 => node = if path == 2 { &node.left } else { &node.right }.as_ref()?,
        4..=7 => {
            node = if path < 6 { &node.left } else { &node.right }.as_ref()?;
            node = if path & 1 == 0 {
                &node.left
            } else {
                &node.right
            }
            .as_ref()?;
        }
        _ => return None,
    }
    Some(node.clone())
}

fn check_domain(node: &ResidentNode, domain: &Domain) -> Result<(), Error> {
    if !node.value.0.same_domain(domain)
        || !node.value.type_name.0.same_domain(domain)
        || !node.value.file.0.same_domain(domain)
        || [&node.left, &node.right]
            .into_iter()
            .flatten()
            .any(|child| !child.same_domain(domain))
    {
        return Err(error("small subtree contains a foreign domain"));
    }
    Ok(())
}

fn fingerprint(node: &ResidentNode, domain: &Domain) -> Result<u64, Error> {
    let mut hash = blake3::Hasher::new();
    hash.update(b"PVD small subtree 1");
    hash_node(node, domain, &mut hash)?;
    Ok(u64::from_le_bytes(
        hash.finalize().as_bytes()[..8].try_into()?,
    ))
}

fn hash_node(node: &ResidentNode, domain: &Domain, hash: &mut blake3::Hasher) -> Result<(), Error> {
    check_domain(node, domain)?;
    if node.height == 0 || node.height > 3 || node.subtree_len > 7 {
        return Err(error("small subtree fingerprint exceeds its shape bound"));
    }
    let left_height = node.left.as_ref().map_or(0, |child| child.height);
    let right_height = node.right.as_ref().map_or(0, |child| child.height);
    let length = 1u32
        .checked_add(node.left.as_ref().map_or(0, |child| child.subtree_len))
        .and_then(|length| {
            length.checked_add(node.right.as_ref().map_or(0, |child| child.subtree_len))
        });
    if left_height.abs_diff(right_height) > 1
        || left_height.max(right_height).checked_add(1) != Some(node.height)
        || length != Some(node.subtree_len)
    {
        return Err(error("small subtree AVL shape disagrees"));
    }
    hash.update(&node.key.0.to_le_bytes());
    hash.update(&node.key.1.to_le_bytes());
    hash.update(&node.value.0.semantic_identity().to_le_bytes());
    hash.update(&[node.height]);
    hash.update(&node.subtree_len.to_le_bytes());
    for child in [&node.left, &node.right] {
        hash.update(&[u8::from(child.is_some())]);
        if let Some(child) = child {
            hash_node(child, domain, hash)?;
        }
    }
    Ok(())
}

fn equivalent(a: &ResidentNode, b: &ResidentNode, domain: &Domain) -> Result<bool, Error> {
    check_domain(a, domain)?;
    check_domain(b, domain)?;
    if a.key != b.key || a.height != b.height || a.subtree_len != b.subtree_len {
        return Ok(false);
    }
    let a_value = &a.value;
    let b_value = &b.value;
    if a_value.0.semantic_identity() != b_value.0.semantic_identity()
        || a_value.address != b_value.address
        || a_value.size != b_value.size
        || a_value.line != b_value.line
        || a_value.received_frame != b_value.received_frame
        || a_value.type_name.0.semantic_identity() != b_value.type_name.0.semantic_identity()
        || a_value.file.0.semantic_identity() != b_value.file.0.semantic_identity()
        || *a_value.type_name != *b_value.type_name
        || *a_value.file != *b_value.file
    {
        return Ok(false);
    }
    for (a, b) in [(&a.left, &b.left), (&a.right, &b.right)] {
        match (a, b) {
            (None, None) => {}
            (Some(a), Some(b)) if equivalent(a, b, domain)? => {}
            _ => return Ok(false),
        }
    }
    Ok(true)
}
