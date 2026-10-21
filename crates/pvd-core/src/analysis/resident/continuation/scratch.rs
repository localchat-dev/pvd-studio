use super::*;
use std::{collections::hash_map::RandomState, hash::BuildHasher};

#[cfg(test)]
mod tests;

const MAX_ENTRIES: usize = 1_000_000;
const MIN_SLOTS: usize = 16;
const MAX_SLOTS: usize = MAX_ENTRIES.next_power_of_two() * 2;
const CANCEL_INTERVAL: usize = 256;

struct Table {
    slots: Vec<usize>,
    memory: MemoryLease,
}

impl Table {
    fn new(
        budget: &Arc<MemoryBudget>,
        slots: usize,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<Self, Error> {
        check_replay_cancelled(cancelled)?;
        if slots != 0 && (!slots.is_power_of_two() || slots > MAX_SLOTS) {
            return Err(error("invalid resident scratch capacity"));
        }
        let payload = slots
            .checked_mul(size_of::<usize>())
            .ok_or_else(|| error("resident scratch size overflow"))?;
        let memory = budget.reserve(add(size_of::<ScratchSet>(), payload)?)?;
        let mut values = Vec::new();
        values.try_reserve_exact(slots)?;
        if values.capacity() != slots {
            return Err(error("resident scratch capacity exceeds its reservation"));
        }
        while values.len() < slots {
            check_replay_cancelled(cancelled)?;
            values.resize((values.len() + CANCEL_INTERVAL).min(slots), 0);
        }
        Ok(Self {
            slots: values,
            memory,
        })
    }
}

pub(super) struct ScratchSet {
    table: Table,
    hash: RandomState,
    len: usize,
    zero: bool,
}

impl ScratchSet {
    pub(super) fn new(domain: &Domain) -> Result<Self, Error> {
        Self::with_hint(domain, 0, &mut || false)
    }

    pub(super) fn with_hint(
        domain: &Domain,
        entries: usize,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<Self, Error> {
        let slots = hinted_slots(entries)?;
        let table = match Table::new(&domain.0, slots, cancelled) {
            Ok(table) => table,
            Err(failure) if slots != 0 && allocation_failure(&failure) => {
                Table::new(&domain.0, 0, cancelled)?
            }
            Err(failure) => return Err(failure),
        };
        Ok(Self {
            table,
            hash: RandomState::new(),
            len: 0,
            zero: false,
        })
    }

    pub(super) fn insert(
        &mut self,
        key: usize,
        _: (),
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<Option<()>, Error> {
        check_replay_cancelled(cancelled)?;
        if key == 0 && self.zero {
            return Ok(Some(()));
        }
        let place = if key != 0 && !self.table.slots.is_empty() {
            let (index, found) = locate(&self.table.slots, &self.hash, key, cancelled)?;
            if found {
                return Ok(Some(()));
            }
            Some(index)
        } else {
            None
        };
        let next_len = next_len(self.len)?;
        if key == 0 {
            check_replay_cancelled(cancelled)?;
            self.zero = true;
        } else if next_len > maximum_load(self.table.slots.len()) {
            let slots = self
                .table
                .slots
                .len()
                .checked_mul(2)
                .ok_or_else(|| error("resident scratch capacity overflow"))?
                .max(MIN_SLOTS);
            let mut next = Table::new(&self.table.memory.budget, slots, cancelled)?;
            for (index, &old_key) in self.table.slots.iter().enumerate() {
                if index.is_multiple_of(CANCEL_INTERVAL) {
                    check_replay_cancelled(cancelled)?;
                }
                if old_key != 0 {
                    let (place, found) = locate(&next.slots, &self.hash, old_key, cancelled)?;
                    if found {
                        return Err(error("resident scratch contains a duplicate identity"));
                    }
                    next.slots[place] = old_key;
                }
            }
            let (place, found) = locate(&next.slots, &self.hash, key, cancelled)?;
            if found {
                return Err(error("resident scratch contains a duplicate identity"));
            }
            check_replay_cancelled(cancelled)?;
            next.slots[place] = key;
            self.table = next;
        } else {
            check_replay_cancelled(cancelled)?;
            self.table.slots[place.unwrap()] = key;
        }
        self.len = next_len;
        Ok(None)
    }
}

pub(super) fn allocation_failure(failure: &Error) -> bool {
    failure.downcast_ref::<ResidentBudgetExceeded>().is_some()
        || failure
            .downcast_ref::<std::collections::TryReserveError>()
            .is_some()
}

fn maximum_load(slots: usize) -> usize {
    slots / 5 * 4 + slots % 5 * 4 / 5
}

fn hinted_slots(entries: usize) -> Result<usize, Error> {
    let entries = entries.min(MAX_ENTRIES);
    if entries == 0 {
        return Ok(0);
    }
    entries
        .checked_mul(5)
        .and_then(|count| count.checked_add(3))
        .map(|count| (count / 4).max(MIN_SLOTS))
        .and_then(usize::checked_next_power_of_two)
        .filter(|&slots| slots <= MAX_SLOTS)
        .ok_or_else(|| error("resident scratch hint exceeds its capacity limit"))
}

fn next_len(len: usize) -> Result<usize, Error> {
    let next = len
        .checked_add(1)
        .filter(|&next| next <= MAX_ENTRIES)
        .ok_or_else(|| error("resident scratch set exceeds its entry limit"))?;
    Ok(next)
}

fn locate(
    slots: &[usize],
    hash: &RandomState,
    key: usize,
    cancelled: &mut dyn FnMut() -> bool,
) -> Result<(usize, bool), Error> {
    if key == 0 || slots.is_empty() || !slots.len().is_power_of_two() {
        return Err(error("invalid resident scratch table"));
    }
    let mask = slots.len() - 1;
    let start = hash.hash_one(key) as usize & mask;
    for offset in 0..slots.len() {
        if offset.is_multiple_of(CANCEL_INTERVAL) {
            check_replay_cancelled(cancelled)?;
        }
        let index = start.wrapping_add(offset) & mask;
        if slots[index] == 0 || slots[index] == key {
            return Ok((index, slots[index] == key));
        }
    }
    Err(error("resident scratch table has no empty slot"))
}
