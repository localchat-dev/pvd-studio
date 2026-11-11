use super::*;

#[derive(Debug)]
pub(in crate::native::export) struct Proof {
    buffers: BTreeMap<u32, Buffer>,
    initial: blake3::Hash,
    catalog: blake3::Hash,
    history: blake3::Hash,
    seeds: blake3::Hash,
    retained: usize,
    leading: usize,
    allocated: usize,
    work_remaining: usize,
}

impl Scope {
    pub(in crate::native::export) fn prove(
        &self,
        seeds: &[history::Seed],
        allocated: &mut usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Proof, Error> {
        let source = self
            .middle
            .as_ref()
            .ok_or_else(|| error("prefix analysis cannot generate middle bootstrap proof"))?;
        let mut work = Work {
            remaining: source.work_remaining,
        };
        metadata_budget(allocated, 1, std::mem::size_of::<Proof>())?;
        metadata_budget(allocated, self.buffers.len(), 96)?;
        let mut buffers = BTreeMap::new();
        for (&id, &buffer) in &self.buffers {
            check(proceed)?;
            buffers.insert(id, buffer);
        }
        for seed in seeds {
            check(proceed)?;
            if !self
                .buffers
                .get(&seed.id)
                .is_some_and(|buffer| buffer.profile && buffer.property == seed.property)
            {
                return Err(error("initial analysis cannot seed an unknown buffer"));
            }
        }
        if seeds_hash(seeds, &mut work, proceed)? != source.seeds {
            return Err(error(
                "initial analysis seed differs from captured pending starts",
            ));
        }
        Ok(Proof {
            buffers,
            initial: source.initial,
            catalog: source.catalog,
            history: source.history,
            seeds: source.seeds,
            retained: source.retained,
            leading: seeds.len() + 1,
            allocated: *allocated,
            work_remaining: work.remaining,
        })
    }
}

impl Proof {
    pub(in crate::native::export) fn owned_buffer_bytes(&self) -> usize {
        self.buffers.len() * 96
    }

    pub(in crate::native::export) fn verify_layout(
        &self,
        layout: &Layout<'_>,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let mut work = Work {
            remaining: self.work_remaining,
        };
        if layout.events.len().checked_sub(self.leading) != Some(self.retained)
            || !matches!(layout.events.first(), Some(HistoryEvent::Marker))
        {
            return Err(error("initial analysis changed its retained interval"));
        }
        if catalog_hash(layout, &self.buffers, &mut work, proceed)? != self.catalog {
            return Err(error("initial analysis changed captured catalog bytes"));
        }
        let count = Reader::new(
            layout
                .snapshot_bytes
                .first()
                .ok_or_else(|| error("missing initial analysis directory"))?,
        )
        .count()?;
        let mut allocated = self.allocated;
        metadata_budget(&mut allocated, count, std::mem::size_of::<Entry>() + 64)?;
        metadata_budget(&mut allocated, layout.saved.len(), 64)?;
        let mut initial = State::read(layout, proceed)?;
        metadata_budget(
            &mut allocated,
            initial.entries.capacity().saturating_sub(count),
            std::mem::size_of::<Entry>(),
        )?;
        initial.entries.sort_by_key(|entry| entry.id);
        let mut payloads = BTreeMap::new();
        metadata_budget(&mut allocated, self.buffers.len(), 96)?;
        for (&id, buffer) in &self.buffers {
            check(proceed)?;
            let entry = initial
                .entries
                .binary_search_by_key(&id, |entry| entry.id)
                .ok()
                .map(|at| &initial.entries[at])
                .ok_or_else(|| error("initial analysis lost a captured instance"))?;
            let saved = &layout.saved[entry.record];
            if saved.class != buffer.class
                || saved.deleted
                || property(layout, buffer.class, buffer.profile, &mut work, proceed)?
                    != buffer.property
            {
                return Err(error("initial analysis changed captured instance identity"));
            }
            payloads.insert(id, payload(saved, 24)?);
        }
        if initial_hash(&self.buffers, |id| Ok(payloads[&id]), &mut work, proceed)? != self.initial
        {
            return Err(error(
                "initial analysis changed captured initial properties",
            ));
        }
        let mut hash = blake3::Hasher::new();
        field(
            &mut hash,
            &((self.leading - 1) as u64).to_le_bytes(),
            &mut work,
            proceed,
        )?;
        for (ordinal, event) in layout.events[1..self.leading].iter().enumerate() {
            check(proceed)?;
            let HistoryEvent::Set {
                kind: 3,
                id,
                property,
                data,
                undo,
            } = event
            else {
                return Err(error("initial analysis changed its bootstrap operation"));
            };
            if undo.as_slice() != [(ordinal + 1) as u32] {
                return Err(error("initial analysis bootstrap must restore Marker zero"));
            }
            seed_fields(&mut hash, *id, *property, data, &mut work, proceed)?;
        }
        if hash.finalize() != self.seeds {
            return Err(error("initial analysis changed captured pending starts"));
        }
        if history_hash(
            layout,
            &self.buffers,
            self.leading..layout.events.len(),
            self.leading,
            false,
            &mut work,
            proceed,
        )? != self.history
        {
            return Err(error("initial analysis changed captured history or undo"));
        }
        Ok(())
    }
}

fn field(
    hash: &mut blake3::Hasher,
    bytes: &[u8],
    work: &mut Work,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    check(proceed)?;
    work.charge(
        bytes
            .len()
            .checked_add(8)
            .ok_or_else(|| error("initial analysis hash size overflow"))?,
    )?;
    hash.update(&(bytes.len() as u64).to_le_bytes());
    for chunk in bytes.chunks(64 << 10) {
        check(proceed)?;
        hash.update(chunk);
    }
    Ok(())
}

pub(super) fn initial_hash<'a>(
    buffers: &BTreeMap<u32, Buffer>,
    mut values: impl FnMut(u32) -> Result<(&'a [u8], &'a [u8]), Error>,
    work: &mut Work,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<blake3::Hash, Error> {
    let mut hash = blake3::Hasher::new();
    field(
        &mut hash,
        &(buffers.len() as u64).to_le_bytes(),
        work,
        proceed,
    )?;
    for (&id, buffer) in buffers {
        for value in [id, buffer.class, buffer.property] {
            field(&mut hash, &value.to_le_bytes(), work, proceed)?;
        }
        let (fixed, payload) = values(id)?;
        field(&mut hash, fixed, work, proceed)?;
        field(&mut hash, payload, work, proceed)?;
    }
    Ok(hash.finalize())
}

pub(super) fn catalog_hash(
    layout: &Layout<'_>,
    buffers: &BTreeMap<u32, Buffer>,
    work: &mut Work,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<blake3::Hash, Error> {
    let mut hash = blake3::Hasher::new();
    field(&mut hash, layout.object_bytes, work, proceed)?;
    for clock in layout.communication.clock {
        field(&mut hash, &clock.to_le_bytes(), work, proceed)?;
    }
    field(
        &mut hash,
        &(layout.communication.zones.len() as u64).to_le_bytes(),
        work,
        proceed,
    )?;
    for zone in &layout.communication.zones {
        check(proceed)?;
        field(&mut hash, &zone.id.to_le_bytes(), work, proceed)?;
        field(&mut hash, &zone.source.to_le_bytes(), work, proceed)?;
        field(&mut hash, &zone.saved_word.to_le_bytes(), work, proceed)?;
        field(&mut hash, zone.name, work, proceed)?;
        field(
            &mut hash,
            &(zone.events.len() as u64).to_le_bytes(),
            work,
            proceed,
        )?;
        for &(name, id, enabled) in &zone.events {
            field(&mut hash, name, work, proceed)?;
            field(&mut hash, &id.to_le_bytes(), work, proceed)?;
            field(&mut hash, &[enabled], work, proceed)?;
        }
    }
    for instance in &layout.communication.instances {
        check(proceed)?;
        work.charge(16)?;
        if buffers.contains_key(&instance.id) {
            field(&mut hash, &instance.id.to_le_bytes(), work, proceed)?;
            field(&mut hash, &instance.source.to_le_bytes(), work, proceed)?;
            field(&mut hash, &instance.flags, work, proceed)?;
        }
    }
    Ok(hash.finalize())
}

fn seed_fields(
    hash: &mut blake3::Hasher,
    id: u32,
    property: u32,
    bytes: &[u8],
    work: &mut Work,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    field(hash, &id.to_le_bytes(), work, proceed)?;
    field(hash, &property.to_le_bytes(), work, proceed)?;
    field(hash, bytes, work, proceed)
}

pub(super) fn seeds_hash(
    seeds: &[history::Seed],
    work: &mut Work,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<blake3::Hash, Error> {
    let mut hash = blake3::Hasher::new();
    field(
        &mut hash,
        &(seeds.len() as u64).to_le_bytes(),
        work,
        proceed,
    )?;
    for seed in seeds {
        seed_fields(
            &mut hash,
            seed.id,
            seed.property,
            &seed.bytes,
            work,
            proceed,
        )?;
    }
    Ok(hash.finalize())
}

pub(super) fn history_hash(
    layout: &Layout<'_>,
    buffers: &BTreeMap<u32, Buffer>,
    range: std::ops::Range<usize>,
    boundary: usize,
    source: bool,
    work: &mut Work,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<blake3::Hash, Error> {
    let mut hash = blake3::Hasher::new();
    let start = range.start;
    let events = layout
        .events
        .get(range)
        .ok_or_else(|| error("initial analysis history exceeds its source"))?;
    field(
        &mut hash,
        &(events.len() as u64).to_le_bytes(),
        work,
        proceed,
    )?;
    for (relative, event) in events.iter().enumerate() {
        check(proceed)?;
        work.charge(16)?;
        let id = match event {
            HistoryEvent::Create(id, _)
            | HistoryEvent::Destroy(id)
            | HistoryEvent::Saved { id, .. }
            | HistoryEvent::Array { id, .. }
            | HistoryEvent::Set { id, .. } => id,
            _ => continue,
        };
        let Some(buffer) = buffers.get(id) else {
            continue;
        };
        let HistoryEvent::Set {
            kind: 3,
            property,
            data,
            undo,
            ..
        } = event
        else {
            return Err(error(
                "initial analysis history introduced an unsupported buffer operation",
            ));
        };
        if *property != buffer.property {
            return Err(error("initial analysis history changed a property"));
        }
        field(&mut hash, &(relative as u64).to_le_bytes(), work, proceed)?;
        seed_fields(&mut hash, *id, *property, data, work, proceed)?;
        let mut anchor = false;
        let mut retained = 0usize;
        let mut previous = None;
        let mut anchor_target = 0;
        if !source && buffer.profile {
            for (ordinal, event) in layout.events[..boundary].iter().enumerate().rev() {
                check(proceed)?;
                work.charge(16)?;
                if matches!(event, HistoryEvent::Set { kind: 3, id: seed_id, property: seed_property, .. } if seed_id == id && seed_property == property)
                {
                    anchor_target = ordinal;
                    break;
                }
            }
        }
        for &distance in undo {
            check(proceed)?;
            work.charge(4)?;
            let target = (start + relative)
                .checked_sub(distance as usize)
                .filter(|target| *target < start + relative)
                .ok_or_else(|| error("initial analysis undo must precede its event"))?;
            if previous.is_some_and(|last| target <= last) {
                return Err(error("initial analysis undo must be ordered and distinct"));
            }
            previous = Some(target);
            if target < boundary {
                if !source && target != anchor_target {
                    return Err(error("initial analysis undo targets another bootstrap"));
                }
                anchor = true;
            } else {
                retained += 1;
            }
        }
        if undo.is_empty() {
            return Err(error("initial analysis history has no captured undo"));
        }
        field(&mut hash, &[u8::from(anchor)], work, proceed)?;
        field(&mut hash, &(retained as u64).to_le_bytes(), work, proceed)?;
        for &distance in undo {
            check(proceed)?;
            if start + relative - distance as usize >= boundary {
                field(&mut hash, &distance.to_le_bytes(), work, proceed)?;
            }
        }
    }
    Ok(hash.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analysis_hash_work_is_shared_and_cancellable_inside_payloads() {
        let bytes = vec![7; 192 << 10];
        let mut work = Work {
            remaining: bytes.len() + 8,
        };
        field(&mut blake3::Hasher::new(), &bytes, &mut work, &mut || true).unwrap();
        assert_eq!(work.remaining, 0);
        assert!(
            field(
                &mut blake3::Hasher::new(),
                &bytes,
                &mut Work {
                    remaining: bytes.len() + 7
                },
                &mut || true
            )
            .is_err()
        );
        let mut calls = 0;
        let failure = field(
            &mut blake3::Hasher::new(),
            &bytes,
            &mut Work::new(),
            &mut || {
                calls += 1;
                calls < 3
            },
        )
        .unwrap_err();
        assert_eq!(calls, 3);
        assert_eq!(
            failure.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
    }
}
