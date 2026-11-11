use super::*;

#[path = "memory_middle/initial.rs"]
mod initial_values;

#[cfg(test)]
#[path = "memory_middle/multi_scene_tests.rs"]
mod multi_scene_tests;

#[derive(Debug)]
pub(super) struct Proof {
    ids: [u32; 2],
    classes: [u32; 2],
    properties: [u32; 2],
    retained_events: usize,
    initial: blake3::Hash,
    history: blake3::Hash,
    allocation: usize,
    work_remaining: usize,
}

struct Work {
    remaining: usize,
}

impl Default for Work {
    fn default() -> Self {
        Self { remaining: 1 << 30 }
    }
}

impl Work {
    fn charge(&mut self, bytes: usize) -> Result<(), Error> {
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .ok_or_else(|| error("middle memory proof exceeds its work budget"))?;
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
            .ok_or_else(|| error("middle memory proof size overflow"))?,
    )?;
    hash.update(&(bytes.len() as u64).to_le_bytes());
    for chunk in bytes.chunks(64 * 1024) {
        check(proceed)?;
        hash.update(chunk);
    }
    Ok(())
}

fn memory_class(
    metadata: &Metadata,
    mut class: u32,
    work: &mut Work,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<bool, Error> {
    for _ in 0..128 {
        check(proceed)?;
        work.charge(4)?;
        let description = metadata.class(class)?;
        if metadata
            .strings
            .get(&description.name.name)
            .is_some_and(|name| {
                matches!(
                    name.as_str(),
                    "PxProfileMemoryEventBuffer" | "MemoryEventTotals"
                )
            })
        {
            return Ok(true);
        }
        if description.base == u32::MAX {
            return Ok(false);
        }
        class = description.base;
    }
    Err(error("middle memory has invalid class inheritance"))
}

fn has_preceding_memory(
    layout: &Layout<'_>,
    start: usize,
    classes: &BTreeMap<u32, u32>,
    work: &mut Work,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<bool, Error> {
    for event in layout
        .events
        .get(..start)
        .ok_or_else(|| error("middle memory start exceeds history"))?
    {
        check(proceed)?;
        work.charge(16)?;
        if let HistoryEvent::Set {
            kind: 3,
            id,
            property,
            data,
            ..
        } = event
            && !data.is_empty()
        {
            let p = layout.metadata.property(*property)?;
            let Some(class) = classes.get(id) else {
                continue;
            };
            let class = layout.metadata.class(*class)?;
            if layout
                .metadata
                .strings
                .get(&class.name.namespace)
                .map(String::as_str)
                == Some("_debugger_")
                && layout
                    .metadata
                    .strings
                    .get(&class.name.name)
                    .map(String::as_str)
                    == Some("PxProfileMemoryEventBuffer")
                && layout.metadata.strings.get(&p.name).map(String::as_str) == Some("events")
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn values(data: &[u8], slot: usize, sources: usize) -> Result<(), Error> {
    if slot == 1 && (!data.len().is_multiple_of(4) || data.len() / 4 > sources) {
        return Err(error(
            "middle memory totals exceed their captured source directory",
        ));
    }
    Ok(())
}

struct SourcePrevious {
    event: usize,
    initial: bool,
}

fn source_undo(
    layout: &Layout<'_>,
    current: usize,
    identity: (u32, u32),
    previous: SourcePrevious,
    references: &[u32],
    work: &mut Work,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    let (id, property) = identity;
    let mut last = None;
    for &distance in references {
        check(proceed)?;
        work.charge(4)?;
        let target = current
            .checked_sub(distance as usize)
            .filter(|target| *target < current)
            .ok_or_else(|| error("middle memory source undo must target preceding history"))?;
        if last.is_some_and(|last| target <= last) {
            return Err(error(
                "middle memory source undo targets must be ordered and distinct",
            ));
        }
        match &layout.events[target] {
            HistoryEvent::Marker if target == 0 && previous.initial => (),
            HistoryEvent::Create(candidate, _) if *candidate == id => (),
            HistoryEvent::Set {
                kind: 3,
                id: candidate,
                property: field,
                ..
            } if *candidate == id && *field == property => (),
            _ => {
                return Err(error(
                    "middle memory source undo targets another object or operation",
                ));
            }
        }
        last = Some(target);
    }
    if last != Some(previous.event) {
        return Err(error(
            "middle memory source undo does not restore its preceding complete property",
        ));
    }
    Ok(())
}

fn initial_hash(
    layout: &Layout<'_>,
    ids: [u32; 2],
    classes: [u32; 2],
    properties: [u32; 2],
    captured: initial_values::Values<'_>,
    work: &mut Work,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<blake3::Hash, Error> {
    let mut hash = blake3::Hasher::new();
    field(&mut hash, b"middle-memory-initial-v2", work, proceed)?;
    field(&mut hash, layout.object_bytes, work, proceed)?;
    for slot in 0..2 {
        for value in [ids[slot], classes[slot], properties[slot]] {
            field(&mut hash, &value.to_le_bytes(), work, proceed)?;
        }
        field(&mut hash, captured.fixed[slot], work, proceed)?;
        field(&mut hash, captured.payloads[slot], work, proceed)?;
        let mut mapped = 0u8;
        for instance in &layout.communication.instances {
            check(proceed)?;
            work.charge(24)?;
            if instance.id == ids[slot] {
                mapped += 1;
                if mapped > 1 {
                    return Err(error("middle memory has duplicate source identity"));
                }
                field(&mut hash, &instance.source.to_le_bytes(), work, proceed)?;
                field(&mut hash, &instance.flags, work, proceed)?;
            }
        }
        if slot == 0 && mapped != 1 {
            return Err(error(
                "middle memory buffer has no captured source identity",
            ));
        }
        field(&mut hash, &[mapped], work, proceed)?;
    }
    let mut reverse = 0usize;
    for &(source, id) in &layout.communication.reverse {
        check(proceed)?;
        work.charge(16)?;
        if ids.contains(&id) {
            reverse += 1;
            field(&mut hash, &source.to_le_bytes(), work, proceed)?;
            field(&mut hash, &id.to_le_bytes(), work, proceed)?;
        }
    }
    field(&mut hash, &(reverse as u64).to_le_bytes(), work, proceed)?;
    field(
        &mut hash,
        &(layout.communication.memory_names.len() as u64).to_le_bytes(),
        work,
        proceed,
    )?;
    for &(kind, file, line, index) in &layout.communication.memory_names {
        check(proceed)?;
        for bytes in [kind, file, &line.to_le_bytes(), &index.to_le_bytes()] {
            field(&mut hash, bytes, work, proceed)?;
        }
    }
    Ok(hash.finalize())
}

#[derive(Clone, Copy)]
enum Coordinates {
    Source { start: usize },
    Output { leading: usize },
}

fn undo(
    hash: &mut blake3::Hasher,
    current: usize,
    references: &[u32],
    coordinates: Coordinates,
    work: &mut Work,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    let mut previous = None;
    let mut anchor = matches!(coordinates, Coordinates::Source { .. }) && references.is_empty();
    let mut retained = 0usize;
    for &distance in references {
        check(proceed)?;
        work.charge(4)?;
        let target = current
            .checked_sub(distance as usize)
            .filter(|target| *target < current)
            .ok_or_else(|| error("middle memory undo target must precede its event"))?;
        if previous.is_some_and(|last| target <= last) {
            return Err(error(
                "middle memory undo targets must be ordered and distinct",
            ));
        }
        previous = Some(target);
        match coordinates {
            Coordinates::Source { start } if target < start => anchor = true,
            Coordinates::Output { .. } if target == 0 => anchor = true,
            Coordinates::Output { leading } if target < leading => {
                return Err(error("middle memory undo cannot target a bootstrap event"));
            }
            _ => retained += 1,
        }
    }
    if matches!(coordinates, Coordinates::Output { .. }) && references.is_empty() {
        return Err(error("middle memory output has no explicit undo reference"));
    }
    field(hash, &[u8::from(anchor)], work, proceed)?;
    field(hash, &(retained as u64).to_le_bytes(), work, proceed)?;
    for &distance in references {
        check(proceed)?;
        let target = current - distance as usize;
        let keep = match coordinates {
            Coordinates::Source { start } => target >= start,
            Coordinates::Output { leading } => target >= leading,
        };
        if keep {
            field(hash, &distance.to_le_bytes(), work, proceed)?;
        }
    }
    Ok(())
}

fn history_hash(
    layout: &Layout<'_>,
    ids: [u32; 2],
    properties: [u32; 2],
    range: std::ops::Range<usize>,
    coordinates: Coordinates,
    work: &mut Work,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<blake3::Hash, Error> {
    let start = range.start;
    let events = layout
        .events
        .get(range)
        .ok_or_else(|| error("middle memory interval exceeds history"))?;
    let mut hash = blake3::Hasher::new();
    field(&mut hash, b"middle-memory-history-v2", work, proceed)?;
    for (offset, event) in events.iter().enumerate() {
        check(proceed)?;
        work.charge(1)?;
        // 内存刷新按全局 Section End 归帧，证明须绑定交错 Scene 的边界。
        if let HistoryEvent::Section {
            class,
            source,
            ticks,
            name,
            begin,
            padding,
        } = event
        {
            field(&mut hash, b"section", work, proceed)?;
            field(&mut hash, &(offset as u64).to_le_bytes(), work, proceed)?;
            field(&mut hash, &class.to_le_bytes(), work, proceed)?;
            field(&mut hash, &source.to_le_bytes(), work, proceed)?;
            field(&mut hash, &ticks.to_le_bytes(), work, proceed)?;
            field(&mut hash, &name.to_le_bytes(), work, proceed)?;
            field(&mut hash, &[*begin], work, proceed)?;
            field(&mut hash, padding, work, proceed)?;
        }
        if let HistoryEvent::Set {
            kind,
            id,
            property,
            data,
            undo: references,
        } = event
            && let Some(slot) = ids.iter().position(|candidate| candidate == id)
        {
            if *kind != 3 || *property != properties[slot] {
                return Err(error(
                    "middle memory requires complete captured property buffers",
                ));
            }
            field(&mut hash, b"memory", work, proceed)?;
            field(&mut hash, &(offset as u64).to_le_bytes(), work, proceed)?;
            for value in [*kind, *id, *property] {
                field(&mut hash, &value.to_le_bytes(), work, proceed)?;
            }
            field(&mut hash, data, work, proceed)?;
            undo(
                &mut hash,
                start + offset,
                references,
                coordinates,
                work,
                proceed,
            )?;
        }
    }
    Ok(hash.finalize())
}

impl Proof {
    pub(super) fn prepare(
        layout: &Layout<'_>,
        start: usize,
        end: usize,
        previous_end: usize,
        created: &BTreeMap<u32, u32>,
        allocation: &mut usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Option<Self>, Error> {
        let mut work = Work::default();
        let source_initial = layout
            .snapshot_bytes
            .first()
            .is_some_and(|bytes| bytes.as_ref() != [0, 0, 0, 0])
            && layout.communication.memory_instances != [u32::MAX; 2];
        if !source_initial && !has_preceding_memory(layout, start, created, &mut work, proceed)? {
            return Ok(None);
        }
        if layout.snapshots.len() != 1 || layout.snapshots[0].event != 0 {
            return Err(error(
                "middle memory requires one event-zero source snapshot",
            ));
        }
        if previous_end >= start || start > end || end >= layout.events.len() {
            return Err(error("invalid middle memory frame boundaries"));
        }
        let ids = layout.communication.memory_instances;
        if ids.contains(&u32::MAX) || ids[0] == ids[1] {
            return Err(error(
                "middle trim cannot retain earlier memory history without a complete captured buffer/totals pair",
            ));
        }
        let mut buffer_source = false;
        for instance in &layout.communication.instances {
            check(proceed)?;
            work.charge(24)?;
            buffer_source |= instance.id == ids[0];
        }
        if !buffer_source {
            return Err(error(
                "middle memory buffer has no captured source identity",
            ));
        }
        let classes = [
            *created
                .get(&ids[0])
                .ok_or_else(|| error("middle memory buffer was not created"))?,
            *created
                .get(&ids[1])
                .ok_or_else(|| error("middle memory totals were not created"))?,
        ];
        metadata_budget(allocation, 1, std::mem::size_of::<Self>())?;
        let properties = initial::memory::properties(layout, classes, allocation, proceed)?;
        let initial = initial_values::read(layout, ids, classes, allocation, &mut work, proceed)?;
        let mut payloads = initial
            .as_ref()
            .map_or([None; 2], |values| values.payloads.map(Some));
        let mut creates = [usize::from(initial.is_some()); 2];
        let mut previous = [0usize; 2];
        for (ordinal, event) in layout.events.iter().enumerate() {
            check(proceed)?;
            work.charge(16)?;
            match event {
                HistoryEvent::Create(id, class) => {
                    if let Some(slot) = ids.iter().position(|candidate| candidate == id) {
                        if ordinal >= start || *class != classes[slot] || creates[slot] != 0 {
                            return Err(error(
                                "middle memory requires unique preexisting instance lifetimes",
                            ));
                        }
                        creates[slot] += 1;
                        previous[slot] = ordinal;
                    } else if memory_class(&layout.metadata, *class, &mut work, proceed)? {
                        return Err(error("middle memory has an additional memory instance"));
                    }
                }
                HistoryEvent::Destroy(id)
                    if ids.contains(id) && (ordinal <= end || initial.is_some()) =>
                {
                    return Err(error(
                        "middle memory instance is not live throughout the retained range",
                    ));
                }
                HistoryEvent::Saved { id, .. }
                    if ids.contains(id) && (ordinal <= end || initial.is_some()) =>
                {
                    return Err(error(
                        "middle memory saved witnesses are outside the supported scope",
                    ));
                }
                HistoryEvent::Array { id, .. } if ids.contains(id) && ordinal <= end => {
                    return Err(error("middle memory does not support array editing"));
                }
                HistoryEvent::Set {
                    kind,
                    id,
                    property,
                    data,
                    undo: references,
                } if ordinal <= end => {
                    if let Some(slot) = ids.iter().position(|candidate| candidate == id) {
                        if *kind != 3 || *property != properties[slot] || creates[slot] != 1 {
                            return Err(error(
                                "middle memory requires complete captured property buffers",
                            ));
                        }
                        values(data, slot, layout.communication.memory_names.len())?;
                        source_undo(
                            layout,
                            ordinal,
                            (*id, *property),
                            SourcePrevious {
                                event: previous[slot],
                                initial: initial.is_some(),
                            },
                            references,
                            &mut work,
                            proceed,
                        )?;
                        previous[slot] = ordinal;
                        if slot == 0 && ordinal > previous_end && ordinal < start {
                            return Err(error(
                                "middle memory first-frame gap contains a captured flush",
                            ));
                        }
                        if ordinal < start {
                            payloads[slot] = Some(data.as_ref());
                        }
                    }
                }
                HistoryEvent::OriginShift(_) if ordinal <= end => {
                    return Err(error("middle memory with OriginShift is not verified"));
                }
                _ => {}
            }
        }
        if creates != [1; 2] {
            return Err(error(
                "middle memory requires two captured instance lifetimes",
            ));
        }
        let payloads = [
            payloads[0]
                .ok_or_else(|| error("middle memory has no captured initial event property"))?,
            payloads[1]
                .ok_or_else(|| error("middle memory has no captured initial totals property"))?,
        ];
        let retained_events = end
            .checked_sub(start)
            .and_then(|length| length.checked_add(1))
            .ok_or_else(|| error("middle memory interval size overflow"))?;
        let initial = initial_hash(
            layout,
            ids,
            classes,
            properties,
            initial_values::Values {
                fixed: initial
                    .as_ref()
                    .map_or([&[0; 24][..]; 2], |values| values.fixed),
                payloads,
            },
            &mut work,
            proceed,
        )?;
        let after_end = end
            .checked_add(1)
            .ok_or_else(|| error("middle memory end overflow"))?;
        let history = history_hash(
            layout,
            ids,
            properties,
            start..after_end,
            Coordinates::Source { start },
            &mut work,
            proceed,
        )?;
        Ok(Some(Self {
            ids,
            classes,
            properties,
            retained_events,
            initial,
            history,
            allocation: *allocation,
            work_remaining: work.remaining,
        }))
    }

    pub(super) fn allows(&self, id: u32, property: u32) -> bool {
        self.ids
            .iter()
            .zip(self.properties)
            .any(|(candidate, field)| *candidate == id && field == property)
    }

    pub(super) fn verify_layout(
        &self,
        layout: &Layout<'_>,
        leading: usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let mut work = Work {
            remaining: self.work_remaining,
        };
        if leading == 0
            || layout.events.len().checked_sub(leading) != Some(self.retained_events)
            || layout.communication.memory_instances != self.ids
        {
            return Err(error(
                "middle memory output changed its interval or instance identity",
            ));
        }
        let mut allocated = self.allocation;
        let count = Reader::new(
            layout
                .snapshot_bytes
                .first()
                .ok_or_else(|| error("middle memory output has no initial snapshot"))?,
        )
        .count()?;
        metadata_budget(
            &mut allocated,
            count,
            std::mem::size_of::<initial::Entry>() + 64,
        )?;
        metadata_budget(&mut allocated, layout.saved.len(), 64)?;
        let initial = initial::State::read(layout, proceed)?;
        metadata_budget(
            &mut allocated,
            initial.entries.capacity() - count,
            std::mem::size_of::<initial::Entry>(),
        )?;
        let properties =
            initial::memory::properties(layout, self.classes, &mut allocated, proceed)?;
        if properties != self.properties {
            return Err(error("middle memory output changed its property identity"));
        }
        let mut payloads = [None; 2];
        let mut fixed_values = [None; 2];
        for entry in &initial.entries {
            check(proceed)?;
            work.charge(16)?;
            let saved = &layout.saved[entry.record];
            if let Some(slot) = self.ids.iter().position(|candidate| *candidate == entry.id) {
                if saved.class != self.classes[slot] {
                    return Err(error("middle memory snapshot class changed"));
                }
                let mut reader = Reader::new(&saved.body);
                let fixed = reader.blob()?;
                if fixed.len() != 24 {
                    return Err(error("middle memory snapshot fixed layout changed"));
                }
                let data = reader.blob()?;
                values(data, slot, layout.communication.memory_names.len())?;
                reader.end()?;
                payloads[slot] = Some(data);
                fixed_values[slot] = Some(fixed);
            } else if memory_class(&layout.metadata, saved.class, &mut work, proceed)? {
                return Err(error(
                    "middle memory snapshot has an additional memory instance",
                ));
            }
        }
        let payloads = [
            payloads[0]
                .ok_or_else(|| error("middle memory output lost its initial event property"))?,
            payloads[1]
                .ok_or_else(|| error("middle memory output lost its initial totals property"))?,
        ];
        if initial_hash(
            layout,
            self.ids,
            self.classes,
            properties,
            initial_values::Values {
                fixed: [
                    fixed_values[0]
                        .ok_or_else(|| error("middle memory output lost its initial fixed body"))?,
                    fixed_values[1]
                        .ok_or_else(|| error("middle memory output lost its initial fixed body"))?,
                ],
                payloads,
            },
            &mut work,
            proceed,
        )? != self.initial
        {
            return Err(error(
                "middle memory output changed captured initial properties or sources",
            ));
        }
        for (ordinal, event) in layout.events.iter().enumerate() {
            check(proceed)?;
            work.charge(16)?;
            match event {
                HistoryEvent::Create(id, class)
                    if self.ids.contains(id)
                        || memory_class(&layout.metadata, *class, &mut work, proceed)? =>
                {
                    return Err(error("middle memory output introduced a memory lifetime"));
                }
                HistoryEvent::Destroy(id)
                | HistoryEvent::Saved { id, .. }
                | HistoryEvent::Array { id, .. }
                    if self.ids.contains(id) =>
                {
                    return Err(error(
                        "middle memory output changed a memory lifecycle or operation",
                    ));
                }
                HistoryEvent::Set {
                    kind,
                    id,
                    property,
                    data,
                    ..
                } if self.ids.contains(id) => {
                    let slot = self
                        .ids
                        .iter()
                        .position(|candidate| candidate == id)
                        .unwrap();
                    if ordinal < leading || *kind != 3 || *property != self.properties[slot] {
                        return Err(error(
                            "middle memory output introduced a memory bootstrap or operation",
                        ));
                    }
                    values(data, slot, layout.communication.memory_names.len())?;
                }
                _ => {}
            }
        }
        if history_hash(
            layout,
            self.ids,
            properties,
            leading..layout.events.len(),
            Coordinates::Output { leading },
            &mut work,
            proceed,
        )? != self.history
        {
            return Err(error(
                "middle memory output changed captured event history, Scene boundaries or undo references",
            ));
        }
        Ok(())
    }

    pub(super) fn verify_recording(
        &self,
        output: &crate::Recording,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        check(proceed)?;
        if output.memory_history_scope() != crate::MemoryHistoryScope::AfterInitialSnapshot
            || output.native_initial_memory_totals()?.is_none()
        {
            return Err(error(
                "middle memory output does not retain a partial initial source view",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_proof_hash_checks_cancellation_inside_large_payloads() {
        let bytes = vec![7; 256 * 1024];
        let mut checks = 0;
        let result = field(
            &mut blake3::Hasher::new(),
            &bytes,
            &mut Work::default(),
            &mut || {
                checks += 1;
                checks < 4
            },
        );
        assert_eq!(checks, 4);
        assert_eq!(
            result
                .unwrap_err()
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .kind(),
            std::io::ErrorKind::Interrupted
        );
    }

    #[test]
    fn memory_proof_work_budget_counts_payloads_and_field_boundaries() {
        let mut work = Work { remaining: 11 };
        let failure = field(
            &mut blake3::Hasher::new(),
            &[1, 2, 3, 4],
            &mut work,
            &mut || true,
        )
        .unwrap_err();
        assert!(failure.to_string().contains("work budget"));
        let mut work = Work { remaining: 12 };
        field(
            &mut blake3::Hasher::new(),
            &[1, 2, 3, 4],
            &mut work,
            &mut || true,
        )
        .unwrap();
        assert_eq!(work.remaining, 0);
    }
}
