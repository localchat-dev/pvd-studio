use super::*;
use crate::analysis::{
    LIVE_ALLOCATION_ENTRY_BYTES, MAP_ENTRY_BYTES, MemoryEvent, MemoryState, ProfileZone,
};
#[cfg(test)]
mod tests;

impl ResidentAnalysis {
    pub(crate) fn try_retain_recent(
        &mut self,
        first_frame: usize,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        self.retain(first_frame, None, cancelled)
    }
    pub(crate) fn try_retain_captured(
        &mut self,
        first_frame: usize,
        accounted_bytes: usize,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        if accounted_bytes > self.data.budget.limit {
            return Err(error("captured analysis accounting exceeds its budget"));
        }
        self.retain(first_frame, Some(accounted_bytes), cancelled)
    }
    fn retain(
        &mut self,
        first_frame: usize,
        accounted: Option<usize>,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        check_replay_cancelled(cancelled)?;
        let mut data = (*self.data).clone();
        let first_frame = first_frame.max(data.first_retained_frame);
        let mut spans = data.spans.edit(&self.domain, cancelled)?;
        spans.retain(|span| span.received_frame >= first_frame, cancelled)?;
        spans.compact()?;
        data.spans = spans.finish()?;
        let mut values = data.values.edit(&self.domain, cancelled)?;
        values.retain(|value| value.received_frame >= first_frame, cancelled)?;
        values.compact()?;
        data.values = values.finish()?;
        let mut starts = TreeEdit::new(&data.starts);
        for (key, values) in data.starts.iter() {
            check_replay_cancelled(cancelled)?;
            if values.capacity() != values.len() {
                let mut compact = values.edit(&self.domain, cancelled)?;
                compact.compact()?;
                starts.insert(*key, compact.finish()?, cancelled)?;
            }
        }
        data.starts = starts.finish()?;
        let mut memory = (*data.memory).clone();
        let end = memory
            .events
            .view()
            .partition_point(|event| event.received_frame < first_frame);
        if end != 0 {
            let mut base = (*memory.base).clone();
            for event in memory.events.view().range(0..end)? {
                check_replay_cancelled(cancelled)?;
                base.apply(event, cancelled)?;
            }
            memory.base = StateRef(Owned::new(base, &self.domain, 0)?);
        }
        let mut events = memory.events.edit(&self.domain, cancelled)?;
        events.remove_prefix(end)?;
        events.compact()?;
        memory.events = events.finish()?;
        let mut samples = memory.samples.edit(&self.domain, cancelled)?;
        samples.retain(|sample| sample.received_frame >= first_frame, cancelled)?;
        samples.compact()?;
        memory.samples = samples.finish()?;
        memory.first_retained_frame = first_frame;
        data.memory = Owned::new(memory, &self.domain, 0)?;
        data.first_retained_frame = first_frame;
        data.budget.used = match accounted {
            Some(bytes) => bytes,
            None => retained_bytes(&data, &self.domain, cancelled)?,
        };
        if data.budget.used > data.budget.limit {
            return Err(error("retained analysis exceeds its semantic budget"));
        }
        self.publish(data, cancelled)
    }
}

fn retained_bytes(
    data: &Continuation,
    domain: &Domain,
    cancelled: &mut dyn FnMut() -> bool,
) -> Result<usize, Error> {
    let mut bytes = 2 * data.spans.capacity() * size_of::<ProfileSpan>()
        + 2 * data.values.capacity() * size_of::<ProfileValue>()
        + data.defined_zones.0.len() * (MAP_ENTRY_BYTES + size_of::<u64>())
        + data.ambiguous_zones.0.len() * (MAP_ENTRY_BYTES + size_of::<u64>())
        + data.ambiguous_events.0.len() * (MAP_ENTRY_BYTES + size_of::<(u64, u16)>())
        + data.unmatched_stop_events.len()
            * (MAP_ENTRY_BYTES + size_of::<(u64, u16, u32, u64)>() + size_of::<u64>());
    for zone in data.zones.values() {
        check_replay_cancelled(cancelled)?;
        bytes = add(
            bytes,
            MAP_ENTRY_BYTES + size_of::<u64>() + size_of::<ProfileZone>() + zone.name.len(),
        )?;
        for name in zone.events.values() {
            bytes = add(
                bytes,
                MAP_ENTRY_BYTES + size_of::<u16>() + size_of::<String>() + name.len(),
            )?;
        }
    }
    for starts in data.starts.values() {
        check_replay_cancelled(cancelled)?;
        bytes = add(
            bytes,
            MAP_ENTRY_BYTES
                + size_of::<(u64, u16, u32, u64)>()
                + size_of::<Vec<u64>>()
                + 2 * starts.capacity() * size_of::<u64>(),
        )?;
    }
    let memory = &data.memory;
    bytes = add(
        bytes,
        (memory.live.len() + memory.base.live.len()) * LIVE_ALLOCATION_ENTRY_BYTES
            + 2 * memory.samples.capacity() * size_of::<MemorySample>()
            + 2 * memory.events.capacity() * size_of::<Arc<MemoryEvent>>()
            + memory.events.len() * (size_of::<MemoryEvent>() + 2 * size_of::<usize>())
            + size_of::<MemoryState>()
            + 2 * size_of::<usize>(),
    )?;
    let mut allocation_refs = add(memory.live.len(), memory.base.live.len())?;
    for event in memory.events.view().iter() {
        check_replay_cancelled(cancelled)?;
        let count = match &event.kind {
            EventKind::Allocate { replaced, .. } => 1 + usize::from(replaced.is_some()),
            EventKind::Free { allocation } => usize::from(allocation.is_some()),
        };
        allocation_refs = add(allocation_refs, count)?;
    }
    let string_refs = allocation_refs
        .checked_mul(2)
        .and_then(|count| count.checked_add(memory.strings.len()))
        .ok_or_else(|| error("retained string reference count overflow"))?;
    let identities = match identity_bytes(
        memory,
        domain,
        Some((allocation_refs, string_refs)),
        cancelled,
    ) {
        Ok(bytes) => bytes,
        Err(failure) if scratch::allocation_failure(&failure) => {
            identity_bytes(memory, domain, None, cancelled)?
        }
        Err(failure) => return Err(failure),
    };
    let entries = memory
        .strings
        .len()
        .checked_mul(MAP_ENTRY_BYTES + size_of::<(u64, u32)>() + size_of::<Arc<str>>())
        .ok_or_else(|| error("retained dictionary layout overflow"))?;
    add(add(bytes, entries)?, identities)
}

fn identity_bytes(
    memory: &MemoryData,
    domain: &Domain,
    hints: Option<(usize, usize)>,
    cancelled: &mut dyn FnMut() -> bool,
) -> Result<usize, Error> {
    check_replay_cancelled(cancelled)?;
    let mut allocations = match hints {
        Some((allocation_refs, _)) => ScratchSet::with_hint(domain, allocation_refs, cancelled)?,
        None => ScratchSet::new(domain)?,
    };
    let mut strings = match hints {
        Some((_, string_refs)) => ScratchSet::with_hint(domain, string_refs, cancelled)?,
        None => ScratchSet::new(domain)?,
    };
    let mut bytes = 0usize;
    fn text(
        value: &Text,
        seen: &mut ScratchSet,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<usize, Error> {
        if seen
            .insert(value.0.semantic_identity(), (), cancelled)?
            .is_none()
        {
            add(value.len(), 2 * size_of::<usize>())
        } else {
            Ok(0)
        }
    }
    fn allocation(
        value: &ResidentValue,
        allocations: &mut ScratchSet,
        strings: &mut ScratchSet,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<usize, Error> {
        if allocations
            .insert(value.0.semantic_identity(), (), cancelled)?
            .is_some()
        {
            return Ok(0);
        }
        add(
            size_of::<Allocation>() + 2 * size_of::<usize>(),
            add(
                text(&value.type_name, strings, cancelled)?,
                text(&value.file, strings, cancelled)?,
            )?,
        )
    }
    for value in memory.strings.values() {
        check_replay_cancelled(cancelled)?;
        bytes = add(bytes, text(value, &mut strings, cancelled)?)?;
    }
    for (_, value) in memory.live.iter().chain(memory.base.live.iter()) {
        check_replay_cancelled(cancelled)?;
        bytes = add(
            bytes,
            allocation(value, &mut allocations, &mut strings, cancelled)?,
        )?;
    }
    for event in memory.events.view().iter() {
        check_replay_cancelled(cancelled)?;
        match &event.kind {
            EventKind::Allocate {
                allocation: value,
                replaced,
            } => {
                bytes = add(
                    bytes,
                    allocation(value, &mut allocations, &mut strings, cancelled)?,
                )?;
                if let Some(value) = replaced {
                    bytes = add(
                        bytes,
                        allocation(value, &mut allocations, &mut strings, cancelled)?,
                    )?;
                }
            }
            EventKind::Free {
                allocation: Some(value),
            } => {
                bytes = add(
                    bytes,
                    allocation(value, &mut allocations, &mut strings, cancelled)?,
                )?;
            }
            EventKind::Free { allocation: None } => (),
        }
    }
    Ok(bytes)
}
