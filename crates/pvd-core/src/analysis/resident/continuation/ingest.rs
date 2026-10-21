use super::*;
use crate::analysis::{
    LIVE_ALLOCATION_ENTRY_BYTES, MAP_ENTRY_BYTES, MAX_RECORDS, MEMORY_EVENT_BYTES, ProfileZone,
    Reader,
};
use pvd_protocol::Endian;

impl ResidentAnalysis {
    pub(crate) fn reset_at(
        &mut self,
        first: usize,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        check_replay_cancelled(cancelled)?;
        let mut draft = Self::new(self.domain.clone())?;
        let mut data = (*draft.data).clone();
        data.first_retained_frame = first;
        draft.publish(data, cancelled)?;
        check_replay_cancelled(cancelled)?;
        *self = draft;
        Ok(())
    }
    pub(crate) fn new(domain: Domain) -> Result<Self, Error> {
        let base = StateRef(Owned::new(
            StateData {
                live: ResidentMap::empty(domain.clone()),
                live_bytes: 0,
                total_allocated_bytes: 0,
                total_freed_bytes: 0,
                allocations: 0,
                frees: 0,
                unmatched_frees: 0,
                replaced_addresses: 0,
            },
            &domain,
            0,
        )?);
        let memory = Owned::new(
            MemoryData {
                live: ResidentMap::empty(domain.clone()),
                samples: Buffer::empty(&domain)?,
                live_bytes: 0,
                peak_bytes: 0,
                total_allocated_bytes: 0,
                allocations: 0,
                frees: 0,
                unmatched_frees: 0,
                replaced_addresses: 0,
                strings: Tree::empty(&domain)?,
                events: Buffer::empty(&domain)?,
                base,
                first_retained_frame: 0,
            },
            &domain,
            0,
        )?;
        let data = Owned::new(
            Continuation {
                first_retained_frame: 0,
                zones: Tree::empty(&domain)?,
                spans: Buffer::empty(&domain)?,
                values: Buffer::empty(&domain)?,
                unmatched_stops: 0,
                unmatched_stop_events: Tree::empty(&domain)?,
                legacy_cuda_buffers: 0,
                memory,
                defined_zones: Set(Tree::empty(&domain)?),
                ambiguous_zones: Set(Tree::empty(&domain)?),
                ambiguous_events: Set(Tree::empty(&domain)?),
                starts: Tree::empty(&domain)?,
                pending_starts: 0,
                budget: Budget::default(),
            },
            &domain,
            0,
        )?;
        Ok(Self {
            domain,
            data,
            revision: crate::analysis::ANALYSIS_REVISION.fetch_add(1, Ordering::Relaxed),
        })
    }

    pub(super) fn publish(
        &mut self,
        data: Continuation,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        check_replay_cancelled(cancelled)?;
        let data = Owned::new(data, &self.domain, 0)?;
        check_replay_cancelled(cancelled)?;
        self.data = data;
        self.revision = crate::analysis::ANALYSIS_REVISION.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    pub(crate) fn add_zone(
        &mut self,
        id: u64,
        name: &str,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        check_replay_cancelled(cancelled)?;
        let mut data = (*self.data).clone();
        let first_definition = !data.defined_zones.0.contains_key(&id);
        let ambiguous = !first_definition
            && data.zones.get(&id).is_some_and(|zone| &*zone.name != name)
            && !data.ambiguous_zones.0.contains_key(&id);
        data.budget.charge(
            name.len()
                + usize::from(first_definition) * (MAP_ENTRY_BYTES + size_of::<u64>())
                + usize::from(ambiguous) * (MAP_ENTRY_BYTES + size_of::<u64>())
                + if data.zones.contains_key(&id) {
                    0
                } else {
                    MAP_ENTRY_BYTES + size_of::<ProfileZone>()
                },
        )?;
        let text = Text::new(name, &self.domain, &mut || cancelled())?;
        let mut defined = TreeEdit::new(&data.defined_zones.0);
        defined.insert(id, (), cancelled)?;
        data.defined_zones = Set(defined.finish()?);
        if ambiguous {
            let mut ambiguity = TreeEdit::new(&data.ambiguous_zones.0);
            ambiguity.insert(id, (), cancelled)?;
            data.ambiguous_zones = Set(ambiguity.finish()?);
        }
        let events = match data.zones.get(&id) {
            Some(zone) => zone.events.clone(),
            None => Tree::empty(&self.domain)?,
        };
        let mut zones = TreeEdit::new(&data.zones);
        zones.insert(id, ZoneData { name: text, events }, cancelled)?;
        data.zones = zones.finish()?;
        self.publish(data, cancelled)
    }

    pub(crate) fn add_event(
        &mut self,
        zone: u64,
        event: u16,
        name: &str,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        check_replay_cancelled(cancelled)?;
        let mut data = (*self.data).clone();
        let ambiguous = data
            .zones
            .get(&zone)
            .and_then(|zone| zone.events.get(&event))
            .is_some_and(|previous| &**previous != name)
            && !data.ambiguous_events.0.contains_key(&(zone, event));
        data.budget.charge(
            name.len()
                + MAP_ENTRY_BYTES
                + size_of::<ProfileZone>()
                + usize::from(ambiguous) * (MAP_ENTRY_BYTES + size_of::<(u64, u16)>()),
        )?;
        if ambiguous {
            let mut ambiguity = TreeEdit::new(&data.ambiguous_events.0);
            ambiguity.insert((zone, event), (), cancelled)?;
            data.ambiguous_events = Set(ambiguity.finish()?);
        }
        let mut zone_data = match data.zones.get(&zone) {
            Some(zone) => zone.clone(),
            None => ZoneData {
                name: Text::new("", &self.domain, &mut || cancelled())?,
                events: Tree::empty(&self.domain)?,
            },
        };
        let mut events = TreeEdit::new(&zone_data.events);
        events.insert(
            event,
            Text::new(name, &self.domain, &mut || cancelled())?,
            cancelled,
        )?;
        zone_data.events = events.finish()?;
        let mut zones = TreeEdit::new(&data.zones);
        zones.insert(zone, zone_data, cancelled)?;
        data.zones = zones.finish()?;
        self.publish(data, cancelled)
    }

    pub(crate) fn ingest(
        &mut self,
        id: u64,
        type_name: &str,
        bytes: &[u8],
        endian: Endian,
        frame: usize,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        check_replay_cancelled(cancelled)?;
        let mut data = (*self.data).clone();
        if type_name.ends_with("PxProfileZone") {
            self.profile_buffer(&mut data, id, bytes, endian, frame, cancelled)?;
        } else if type_name.ends_with("PxProfileMemoryEventBuffer") {
            self.memory_buffer(&mut data, id, bytes, endian, frame, None, cancelled)?;
        } else {
            return Ok(());
        }
        self.publish(data, cancelled)
    }

    #[cfg(test)]
    pub(crate) fn ingest_native_memory<'a>(
        &mut self,
        id: u64,
        bytes: &[u8],
        frame: usize,
        mut resolve: impl FnMut(u32) -> Result<&'a str, Error>,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let mut data = (*self.data).clone();
        self.memory_buffer(
            &mut data,
            id,
            bytes,
            Endian::Little,
            frame,
            Some(&mut resolve),
            cancelled,
        )?;
        self.publish(data, cancelled)
    }

    fn profile_buffer(
        &self,
        data: &mut Continuation,
        zone: u64,
        bytes: &[u8],
        endian: Endian,
        frame: usize,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        if frame < data.first_retained_frame {
            return Err(error("profile received_frame precedes retained history"));
        }
        let mut reader = Reader {
            bytes,
            position: 0,
            endian,
        };
        let mut spans = BufferEdit::new(&data.spans);
        let mut values = BufferEdit::new(&data.values);
        let mut starts = TreeEdit::new(&data.starts);
        let mut unmatched = TreeEdit::new(&data.unmatched_stop_events);
        let mut timestamp = 0u64;
        let mut thread = 0;
        let mut context = 0;
        while !reader.done() {
            check_replay_cancelled(cancelled)?;
            if spans.len() + values.len() + data.pending_starts + unmatched.len() + starts.len()
                >= MAX_RECORDS
            {
                return Err(error("profile analysis exceeds two million events"));
            }
            let kind = reader.uint(1)?;
            let options = reader.uint(1)?;
            let event = reader.uint(2)? as u16;
            let time_width = 1 << (options & 3);
            let context_width = 1 << ((options >> 2) & 3);
            match kind {
                1..=4 => {
                    if kind <= 2 {
                        thread = reader.uint(4)? as u32;
                        context = reader.uint(context_width)?;
                        reader.take(2)?;
                    }
                    let encoded = reader.uint(time_width)?;
                    timestamp = if time_width == 8 {
                        encoded
                    } else {
                        timestamp
                            .checked_add(encoded)
                            .ok_or_else(|| error("profile timestamp overflow"))?
                    };
                    let key = (zone, event, thread, context);
                    if kind == 1 || kind == 3 {
                        data.budget.charge(
                            2 * size_of::<u64>()
                                + if starts.get(&key).is_some() {
                                    0
                                } else {
                                    MAP_ENTRY_BYTES + size_of::<Vec<u64>>()
                                },
                        )?;
                        let previous = match starts.get(&key) {
                            Some(values) => values.clone(),
                            None => Buffer::empty(&self.domain)?,
                        };
                        let mut nested = previous.edit(&self.domain, cancelled)?;
                        nested.push(timestamp)?;
                        starts.insert(key, nested.finish()?, cancelled)?;
                        data.pending_starts = add(data.pending_starts, 1)?;
                    } else if let Some(start) = starts
                        .get(&key)
                        .and_then(|values| values.view().last())
                        .copied()
                    {
                        if timestamp < start {
                            return Err(error("profile stop precedes its start"));
                        }
                        data.budget.charge(2 * size_of::<ProfileSpan>())?;
                        let mut nested = starts.get(&key).unwrap().edit(&self.domain, cancelled)?;
                        nested.pop()?;
                        if nested.len() == 0 {
                            starts.remove(&key, cancelled)?;
                            data.budget.release(
                                MAP_ENTRY_BYTES + size_of::<Vec<u64>>() + 2 * size_of::<u64>(),
                            );
                        } else {
                            starts.insert(key, nested.finish()?, cancelled)?;
                        }
                        spans.push(
                            ProfileSpan {
                                zone,
                                event,
                                thread,
                                context,
                                start,
                                end: timestamp,
                                received_frame: frame,
                            },
                            cancelled,
                        )?;
                        data.pending_starts -= 1;
                    } else {
                        if unmatched.get(&key).is_none() {
                            data.budget
                                .charge(MAP_ENTRY_BYTES + size_of_val(&key) + size_of::<u64>())?;
                        }
                        data.unmatched_stops = data
                            .unmatched_stops
                            .checked_add(1)
                            .ok_or_else(|| error("unmatched profile count overflow"))?;
                        let count = unmatched
                            .get(&key)
                            .copied()
                            .unwrap_or(0)
                            .checked_add(1)
                            .ok_or_else(|| error("unmatched profile key count overflow"))?;
                        unmatched.insert(key, count, cancelled)?;
                    }
                }
                5 => {
                    let raw_value = reader.uint(time_width)?;
                    let value_context = reader.uint(context_width)?;
                    let value_thread = reader.uint(4)? as u32;
                    data.budget.charge(2 * size_of::<ProfileValue>())?;
                    values.push(
                        ProfileValue {
                            zone,
                            event,
                            thread: value_thread,
                            context: value_context,
                            raw_value,
                            received_frame: frame,
                        },
                        cancelled,
                    )?;
                }
                6 => {
                    reader.take(12)?;
                    let length = reader.uint(4)? as usize;
                    reader.take(length)?;
                    reader.take(8)?;
                    data.legacy_cuda_buffers = data
                        .legacy_cuda_buffers
                        .checked_add(1)
                        .ok_or_else(|| error("CUDA buffer count overflow"))?;
                }
                _ => {
                    return Err(error(format!(
                        "unknown profile event {kind} at embedded byte {}; event boundary is unavailable",
                        reader.position - 4
                    )));
                }
            }
        }
        data.spans = spans.finish()?;
        data.values = values.finish()?;
        data.starts = starts.finish()?;
        data.unmatched_stop_events = unmatched.finish()?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn memory_buffer<'a>(
        &self,
        data: &mut Continuation,
        id: u64,
        bytes: &[u8],
        endian: Endian,
        frame: usize,
        mut resolve: Option<&mut dyn FnMut(u32) -> Result<&'a str, Error>>,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let mut reader = Reader {
            bytes,
            position: 0,
            endian,
        };
        let mut memory = (*data.memory).clone();
        if frame < memory.first_retained_frame
            || memory
                .samples
                .view()
                .last()
                .is_some_and(|sample| sample.received_frame > frame)
            || memory
                .events
                .view()
                .last()
                .is_some_and(|event| event.received_frame > frame)
        {
            return Err(error("memory flush received_frame moved backwards"));
        }
        let mut strings = TreeEdit::new(&memory.strings);
        let mut events = BufferEdit::new(&memory.events);
        let mut samples = BufferEdit::new(&memory.samples);
        let budget = &mut data.budget;
        while !reader.done() {
            check_replay_cancelled(cancelled)?;
            if memory.live.len() >= 1_000_000 || strings.len() >= 1_000_000 {
                return Err(error(
                    "memory analysis exceeds one million live allocations or strings",
                ));
            }
            let header = reader.uint(2)?;
            let address_width = 1 << ((header >> 4) & 3);
            let type_width = (1 << ((header >> 6) & 3)).min(4);
            let file_width = (1 << ((header >> 8) & 3)).min(4);
            let size_width = (1 << ((header >> 10) & 3)).min(4);
            let line_width = (1 << ((header >> 12) & 3)).min(4);
            match header & 15 {
                1 => {
                    if resolve.is_some() {
                        return Err(error(
                            "PVD3 memory buffers must use container string handles",
                        ));
                    }
                    let length = reader.uint(4)? as usize;
                    let raw = reader.take(length)?;
                    let raw = raw
                        .strip_suffix(&[0])
                        .ok_or_else(|| error("memory string lacks null terminator"))?;
                    let handle = reader.uint(4)? as u32;
                    let value = lossy_text(raw, &self.domain, cancelled)?;
                    budget.charge(
                        value.len()
                            + MAP_ENTRY_BYTES
                            + size_of::<(u64, u32)>()
                            + size_of::<Arc<str>>()
                            + 2 * size_of::<usize>(),
                    )?;
                    strings.insert((id, handle), value, cancelled)?;
                }
                2 => {
                    let size = reader.uint(size_width)?;
                    let type_id = reader.uint(type_width)? as u32;
                    let file_id = reader.uint(file_width)? as u32;
                    let line = reader.uint(line_width)? as u32;
                    let address = reader.uint(address_width)?;
                    let mut string = |handle, field| -> Result<Text, Error> {
                        if let Some(value) = strings.get(&(id, handle)) {
                            return Ok(value.clone());
                        }
                        let resolver = resolve.as_mut().ok_or_else(|| {
                            error(format!("undefined memory {field} handle {handle}"))
                        })?;
                        if strings.len() >= 1_000_000 {
                            return Err(error("memory analysis exceeds one million strings"));
                        }
                        let value = resolver(handle)?;
                        budget.charge(
                            value.len()
                                + MAP_ENTRY_BYTES
                                + size_of::<(u64, u32)>()
                                + size_of::<Arc<str>>()
                                + 2 * size_of::<usize>(),
                        )?;
                        let value = Text::new(value, &self.domain, &mut || cancelled())?;
                        strings.insert((id, handle), value.clone(), cancelled)?;
                        Ok(value)
                    };
                    let type_name = string(type_id, "type")?;
                    let file = string(file_id, "file")?;
                    if events.len() >= MAX_RECORDS {
                        return Err(error(
                            "memory history exceeds two million allocation/free events",
                        ));
                    }
                    let replaced = memory.live.get(&(id, address)).cloned();
                    let live_bytes = memory
                        .live_bytes
                        .checked_sub(replaced.as_ref().map_or(0, |old| old.size))
                        .and_then(|bytes| bytes.checked_add(size))
                        .ok_or_else(|| error("live memory byte count overflow"))?;
                    let allocated_bytes = memory
                        .total_allocated_bytes
                        .checked_add(size)
                        .ok_or_else(|| error("allocated memory byte count overflow"))?;
                    budget.charge(
                        MEMORY_EVENT_BYTES
                            + size_of::<Allocation>()
                            + 2 * size_of::<usize>()
                            + if replaced.is_none() {
                                LIVE_ALLOCATION_ENTRY_BYTES
                            } else {
                                0
                            },
                    )?;
                    let allocation = ResidentValue::new(
                        AllocationData {
                            address,
                            size,
                            type_name,
                            file,
                            line,
                            received_frame: frame,
                        },
                        &self.domain,
                    )?;
                    memory
                        .live
                        .try_insert((id, address), allocation.clone(), &mut || cancelled())?;
                    memory.replaced_addresses = memory
                        .replaced_addresses
                        .checked_add(u64::from(replaced.is_some()))
                        .ok_or_else(|| error("replacement count overflow"))?;
                    let event = EventRef(Owned::new(
                        EventData {
                            buffer: id,
                            address,
                            received_frame: frame,
                            kind: EventKind::Allocate {
                                allocation,
                                replaced,
                            },
                        },
                        &self.domain,
                        0,
                    )?);
                    events.push(event, cancelled)?;
                    memory.live_bytes = live_bytes;
                    memory.peak_bytes = memory.peak_bytes.max(live_bytes);
                    memory.total_allocated_bytes = allocated_bytes;
                    memory.allocations = memory
                        .allocations
                        .checked_add(1)
                        .ok_or_else(|| error("allocation count overflow"))?;
                }
                3 => {
                    let address = reader.uint(address_width)?;
                    if events.len() >= MAX_RECORDS {
                        return Err(error(
                            "memory history exceeds two million allocation/free events",
                        ));
                    }
                    budget.charge(MEMORY_EVENT_BYTES)?;
                    let allocation = memory
                        .live
                        .try_remove(&(id, address), &mut || cancelled())?;
                    if let Some(value) = &allocation {
                        budget.release(LIVE_ALLOCATION_ENTRY_BYTES);
                        memory.live_bytes = memory
                            .live_bytes
                            .checked_sub(value.size)
                            .ok_or_else(|| error("live memory byte count underflow"))?;
                    } else {
                        memory.unmatched_frees = memory
                            .unmatched_frees
                            .checked_add(1)
                            .ok_or_else(|| error("unmatched free count overflow"))?;
                    }
                    let event = EventRef(Owned::new(
                        EventData {
                            buffer: id,
                            address,
                            received_frame: frame,
                            kind: EventKind::Free { allocation },
                        },
                        &self.domain,
                        0,
                    )?);
                    events.push(event, cancelled)?;
                    memory.frees = memory
                        .frees
                        .checked_add(1)
                        .ok_or_else(|| error("free count overflow"))?;
                }
                kind => {
                    return Err(error(format!(
                        "unsupported memory event {kind} at embedded byte {}; event boundary is unavailable",
                        reader.position - 2
                    )));
                }
            }
        }
        budget.charge(2 * size_of::<MemorySample>())?;
        samples.push(
            MemorySample {
                received_frame: frame,
                live_bytes: memory.live_bytes,
                live_allocations: memory.live.len(),
                allocations: memory.allocations,
                frees: memory.frees,
            },
            cancelled,
        )?;
        memory.events = events.finish()?;
        memory.samples = samples.finish()?;
        memory.strings = strings.finish()?;
        data.memory = Owned::new(memory, &self.domain, 0)?;
        Ok(())
    }
}

fn lossy_text(
    raw: &[u8],
    domain: &Domain,
    cancelled: &mut dyn FnMut() -> bool,
) -> Result<Text, Error> {
    if let Ok(text) = std::str::from_utf8(raw) {
        return Text::new(text, domain, &mut || cancelled());
    }
    let capacity = raw
        .len()
        .checked_mul(3)
        .ok_or_else(|| error("lossy string capacity overflow"))?;
    if capacity > MAX_STRING_BYTES {
        return Err(error("lossy memory string exceeds its byte limit"));
    }
    let memory = domain.0.reserve(capacity)?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(capacity)?;
    if bytes.capacity() != capacity {
        return Err(error("lossy memory string exceeds its reservation"));
    }
    let mut remaining = raw;
    while !remaining.is_empty() {
        check_replay_cancelled(cancelled)?;
        match std::str::from_utf8(remaining) {
            Ok(valid) => {
                bytes.extend_from_slice(valid.as_bytes());
                break;
            }
            Err(invalid) => {
                let valid = invalid.valid_up_to();
                bytes.extend_from_slice(&remaining[..valid]);
                bytes.extend_from_slice("\u{fffd}".as_bytes());
                let skip = valid + invalid.error_len().unwrap_or(remaining.len() - valid);
                remaining = &remaining[skip..];
            }
        }
    }
    let text = std::str::from_utf8(&bytes)?;
    let owned = Text::new(text, domain, &mut || cancelled())?;
    drop(bytes);
    drop(memory);
    Ok(owned)
}
