use super::*;
use crate::{Recording, analysis::Analysis};
use pvd_protocol::Endian;

pub(super) mod profile;

#[derive(Debug)]
pub(super) struct Seed {
    pub(super) id: u32,
    pub(super) property: u32,
    pub(super) bytes: Vec<u8>,
}

#[derive(Debug)]
pub(super) struct Proof {
    pub(super) first_event: usize,
    last_event: usize,
    last_frame: usize,
    retained_offset: usize,
    initial_instances: Vec<u32>,
    pub(super) bootstrap: Vec<Seed>,
    analysis: blake3::Hash,
    pub(super) origin: Option<middle::OriginProof>,
    pub(super) initial: Option<middle::InitialProof>,
    memory: Option<memory_middle::Proof>,
    renderer: Option<render_context::Proof>,
}

impl Proof {
    pub(super) fn leading_count(&self) -> usize {
        self.retained_offset
    }

    pub(super) fn initial_index_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.initial_instances.capacity() * std::mem::size_of::<u32>()
            + self.bootstrap.capacity() * std::mem::size_of::<Seed>()
            + self
                .bootstrap
                .iter()
                .map(|seed| seed.bytes.capacity())
                .sum::<usize>()
    }

    pub(super) fn verify_layout(
        &self,
        layout: &Layout<'_>,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        if let Some(proof) = &self.initial {
            proof.verify_layout(layout, proceed)?;
        }
        if let Some(proof) = &self.origin {
            proof.verify_layout(layout, proceed)?;
        }
        if let Some(proof) = &self.memory {
            proof.verify_layout(layout, self.retained_offset, proceed)?;
        }
        if let Some(proof) = &self.renderer {
            proof.verify_layout(layout, self.retained_offset, proceed)?;
        }
        Ok(())
    }

    pub(super) fn verify(
        &self,
        output: &Recording,
        source: &Recording,
        first_frame: usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        self.verify_initial_errors(output, source, proceed)?;
        if let Some(proof) = &self.memory {
            proof.verify_recording(output, proceed)?;
        }
        let analysis = output
            .analysis
            .legacy()
            .ok_or_else(|| error("middle trim analysis is unavailable"))?;
        if digest(analysis, proceed)? != self.analysis {
            return Err(error("middle trim does not preserve interval analysis"));
        }
        let original = source
            .analysis
            .legacy()
            .ok_or_else(|| error("source analysis is unavailable"))?;
        let mut spans = original
            .spans
            .iter()
            .filter(|span| (first_frame..=self.last_frame).contains(&span.received_frame));
        for span in &analysis.spans {
            check(proceed)?;
            let Some(expected) = spans.next() else {
                return Err(error("middle trim introduced a profile span"));
            };
            if (
                span.zone,
                span.event,
                span.thread,
                span.context,
                span.start,
                span.end,
                span.received_frame + first_frame,
            ) != (
                expected.zone,
                expected.event,
                expected.thread,
                expected.context,
                expected.start,
                expected.end,
                expected.received_frame,
            ) {
                return Err(error("middle trim changed captured profile spans"));
            }
        }
        if spans.next().is_some() {
            return Err(error("middle trim lost a captured profile span"));
        }
        let mut values = original
            .values
            .iter()
            .filter(|value| (first_frame..=self.last_frame).contains(&value.received_frame));
        for value in &analysis.values {
            check(proceed)?;
            let Some(expected) = values.next() else {
                return Err(error("middle trim introduced a profile value"));
            };
            if (
                value.zone,
                value.event,
                value.thread,
                value.context,
                value.raw_value,
                value.received_frame + first_frame,
            ) != (
                expected.zone,
                expected.event,
                expected.thread,
                expected.context,
                expected.raw_value,
                expected.received_frame,
            ) {
                return Err(error("middle trim changed captured profile values"));
            }
        }
        if values.next().is_some() {
            return Err(error("middle trim lost a captured profile value"));
        }
        let mut actual = output.untimed_error_events().iter();
        for expected in source.untimed_error_events() {
            check(proceed)?;
            if expected.history_event < self.first_event as u64 {
                continue;
            }
            if expected.history_event > self.last_event as u64 {
                continue;
            }
            let Some(value) = actual.next() else {
                return Err(error("middle trim lost an error record"));
            };
            if value.code != expected.code
                || value.message != expected.message
                || value.file != expected.file
                || value.line != expected.line
                || value.index != expected.index
                || value.container_instance != expected.container_instance
                || value.container_generation != expected.container_generation
                || value.history_event
                    != expected.history_event - self.first_event as u64
                        + self.retained_offset as u64
                || expected.received_frame.checked_sub(first_frame) != Some(value.received_frame)
            {
                return Err(error("middle trim does not preserve error history"));
            }
        }
        if actual.next().is_some() {
            return Err(error("middle trim introduced an error record"));
        }
        Ok(())
    }

    fn verify_initial_errors(
        &self,
        output: &Recording,
        source: &Recording,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let mut expected = BTreeMap::new();
        let mut allocated = 0;
        enum Record<'a> {
            Initial(&'a crate::InitialUntimedError),
            Received(&'a crate::UntimedErrorEvent),
        }
        for event in source.initial_untimed_errors() {
            check(proceed)?;
            let id = u32::try_from(event.container_instance)?;
            if self.initial_instances.binary_search(&id).is_err() {
                continue;
            }
            let key = (event.container_instance, event.container_generation);
            if let std::collections::btree_map::Entry::Vacant(entry) = expected.entry(key) {
                metadata_budget(&mut allocated, 1, 128)?;
                entry.insert((Vec::new(), 0usize));
            }
            metadata_budget(&mut allocated, 2, std::mem::size_of::<Record<'_>>())?;
            let (records, _) = expected.get_mut(&key).unwrap();
            records.try_reserve(1)?;
            records.push(Record::Initial(event));
        }
        for event in source.untimed_error_events() {
            check(proceed)?;
            if event.history_event >= self.first_event as u64 {
                continue;
            }
            let id = u32::try_from(event.container_instance)?;
            if self.initial_instances.binary_search(&id).is_err() {
                continue;
            }
            let key = (event.container_instance, event.container_generation);
            if let std::collections::btree_map::Entry::Vacant(entry) = expected.entry(key) {
                metadata_budget(&mut allocated, 1, 128)?;
                entry.insert((Vec::new(), 0usize));
            }
            metadata_budget(&mut allocated, 2, std::mem::size_of::<Record<'_>>())?;
            let (records, _) = expected.get_mut(&key).unwrap();
            records.try_reserve(1)?;
            records.push(Record::Received(event));
        }
        for actual in output.initial_untimed_errors() {
            check(proceed)?;
            let key = (actual.container_instance, actual.container_generation);
            let (records, offset) = expected
                .get_mut(&key)
                .ok_or_else(|| error("middle trim introduced an initial error record"))?;
            let record = records
                .get(*offset)
                .ok_or_else(|| error("middle trim introduced an initial error record"))?;
            let (code, message, file, line, index) = match record {
                Record::Initial(record) => (
                    record.code,
                    &record.message,
                    &record.file,
                    record.line,
                    record.index,
                ),
                Record::Received(record) => (
                    record.code,
                    &record.message,
                    &record.file,
                    record.line,
                    record.index,
                ),
            };
            if actual.code != code
                || &actual.message != message
                || &actual.file != file
                || actual.line != line
                || actual.index != index
            {
                return Err(error("middle trim changed initial error records"));
            }
            *offset += 1;
        }
        for (records, offset) in expected.values() {
            check(proceed)?;
            if *offset != records.len() {
                return Err(error("middle trim lost an initial error record"));
            }
        }
        Ok(())
    }
}

fn digest(value: &Analysis, proceed: &mut dyn FnMut() -> bool) -> Result<blake3::Hash, Error> {
    struct Sink<'a> {
        hash: blake3::Hasher,
        proceed: &'a mut dyn FnMut() -> bool,
        stopped: bool,
    }
    impl Write for Sink<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if !(self.proceed)() {
                self.stopped = true;
                return Err(std::io::Error::other("analysis comparison cancelled"));
            }
            self.hash.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut sink = Sink {
        hash: blake3::Hasher::new(),
        proceed,
        stopped: false,
    };
    let result = postcard::to_io(value, &mut sink).map(|_| ());
    if sink.stopped {
        return Err(cancelled());
    }
    result?;
    Ok(sink.hash.finalize())
}

pub(super) fn prepare(
    layout: &Layout<'_>,
    first: usize,
    last: usize,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<Proof, Error> {
    let initial = initial::State::read(layout, proceed)?;
    let mut allocated = 0;
    metadata_budget(
        &mut allocated,
        initial.entries.capacity(),
        std::mem::size_of::<initial::Entry>(),
    )?;
    metadata_budget(&mut allocated, initial.classes.len(), 64)?;
    prepare_with_initial(layout, first, last, &initial.classes, allocated, proceed)
}

pub(super) fn prepare_with_initial(
    layout: &Layout<'_>,
    first: usize,
    last: usize,
    initial: &BTreeMap<u32, u32>,
    mut allocated: usize,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<Proof, Error> {
    let interval = layout.middle_frame_interval(first, last, proceed)?;
    let start = interval.start;
    let end = interval.end - 1;
    let events = layout
        .events
        .get(..=end)
        .ok_or_else(|| error("frame event out of range"))?;
    metadata_budget(&mut allocated, initial.len(), 96)?;
    let mut created = BTreeMap::new();
    let mut alive = BTreeSet::new();
    for (&id, &class) in initial {
        check(proceed)?;
        created.insert(id, class);
        alive.insert(id);
    }
    for event in events {
        check(proceed)?;
        if let HistoryEvent::Create(id, class) = event {
            metadata_budget(&mut allocated, 1, 96)?;
            if created.insert(*id, *class).is_some() {
                return Err(error("middle trim requires unique instance lifecycles"));
            }
        }
    }
    let previous_end = interval.previous_end;
    let memory = memory_middle::Proof::prepare(
        layout,
        start,
        end,
        previous_end,
        &created,
        &mut allocated,
        proceed,
    )?;
    let renderer = if initial.is_empty() {
        render_context::Proof::prepare(
            layout,
            start,
            end,
            previous_end,
            interval.first_end,
            &mut allocated,
            proceed,
        )?
    } else {
        None
    };
    let mut analysis = Analysis::default();
    let text = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes.strip_suffix(&[0]).unwrap_or(bytes)).into_owned()
    };
    for zone in &layout.communication.zones {
        check(proceed)?;
        if !created.contains_key(&zone.id) {
            continue;
        }
        analysis.add_zone(u64::from(zone.id), text(zone.name))?;
        for &(name, id, _) in &zone.events {
            check(proceed)?;
            analysis.add_event(u64::from(zone.id), id, text(name))?;
        }
    }
    let mut renderers = BTreeSet::new();
    let mut pending = profile::Pending::default();
    let mut original_profile = Analysis::default();
    let mut profile_properties = BTreeMap::new();
    let mut initial_instances = Vec::new();
    let mut profile_starts = vec![];
    let mut needs_overwrite = BTreeSet::new();
    let mut frame = 0;
    for (ordinal, event) in events.iter().enumerate() {
        check(proceed)?;
        let retained = ordinal >= start;
        if ordinal == start {
            metadata_budget(&mut allocated, alive.len(), std::mem::size_of::<u32>())?;
            initial_instances.try_reserve_exact(alive.len())?;
            metadata_budget(
                &mut allocated,
                initial_instances.capacity() - alive.len(),
                std::mem::size_of::<u32>(),
            )?;
            for &id in &alive {
                check(proceed)?;
                initial_instances.push(id);
            }
            pending.verify_pending(&original_profile, proceed)?;
            profile_starts =
                pending.seeds_bounded(&profile_properties, &alive, &mut allocated, proceed)?;
            for seed in &profile_starts {
                check(proceed)?;
                analysis.ingest_for_export(
                    u64::from(seed.id),
                    "PxProfileZone",
                    &seed.bytes,
                    Endian::Little,
                    0,
                    &mut |_| check(proceed),
                )?;
                needs_overwrite.insert(seed.id);
            }
            pending.verify(&analysis, proceed)?;
        }
        let mut inspect = |id: u32, property: u32, data: &[u8], kind: u32| -> Result<(), Error> {
            let p = layout.metadata.property(property)?;
            let namespace = layout
                .metadata
                .strings
                .get(&p.owner.namespace)
                .map(String::as_str);
            let name = layout.metadata.strings.get(&p.name).map(String::as_str);
            if !namespace.is_some_and(|name| name.starts_with("_debugger"))
                || !matches!(name, Some("events" | "Events"))
            {
                return Ok(());
            }
            let class = layout.metadata.class(
                *created
                    .get(&id)
                    .ok_or_else(|| error("event buffer has no instance"))?,
            )?;
            let class_name = layout
                .metadata
                .strings
                .get(&class.name.name)
                .map(String::as_str);
            if layout
                .metadata
                .strings
                .get(&class.name.namespace)
                .map(String::as_str)
                != Some("_debugger_")
            {
                return Err(error(
                    "middle trim does not support this event history class",
                ));
            }
            match (class_name, name, kind) {
                (Some("PvdUserRenderer"), Some("events"), 3) => {
                    if !data.is_empty()
                        && renderer
                            .as_ref()
                            .is_none_or(|proof| !proof.allows(id, property))
                    {
                        if !retained {
                            if renderers.insert(id) {
                                metadata_budget(&mut allocated, 1, 32)?;
                            }
                        } else if renderers.remove(&id)
                            && (data.first() != Some(&1) || data.len() < 9)
                        {
                            return Err(error(
                                "middle trim drawing requires an explicit initial instance",
                            ));
                        }
                    }
                }
                (
                    Some(kind @ ("PxProfileZone" | "PxProfileMemoryEventBuffer")),
                    Some("events"),
                    3,
                ) => {
                    if !retained
                        && !data.is_empty()
                        && kind == "PxProfileMemoryEventBuffer"
                        && memory
                            .as_ref()
                            .is_none_or(|proof| !proof.allows(id, property))
                    {
                        return Err(error("middle trim cannot retain earlier memory history"));
                    }
                    if !retained && kind == "PxProfileZone" {
                        if !profile_properties.contains_key(&id) {
                            metadata_budget(&mut allocated, 1, 64)?;
                        }
                        if profile_properties
                            .insert(id, property)
                            .is_some_and(|old| old != property)
                        {
                            return Err(error(
                                "profile bootstrap requires a single event property",
                            ));
                        }
                        pending.ingest_bounded(u64::from(id), data, allocated, proceed)?;
                    } else if retained {
                        if kind == "PxProfileZone" {
                            needs_overwrite.remove(&id);
                            analysis.ingest_for_export(
                                u64::from(id),
                                kind,
                                data,
                                Endian::Little,
                                frame,
                                &mut |_| check(proceed),
                            )?;
                        } else {
                            analysis.ingest_native_memory_for_export(
                                u64::from(id),
                                data,
                                frame,
                                |handle| {
                                    layout
                                        .metadata
                                        .strings
                                        .get(&handle)
                                        .map(String::as_str)
                                        .ok_or_else(|| error("undefined memory string handle"))
                                },
                                &mut |_| check(proceed),
                            )?;
                        }
                    }
                    if kind == "PxProfileZone" {
                        original_profile.ingest_for_export(
                            u64::from(id),
                            kind,
                            data,
                            Endian::Little,
                            frame,
                            &mut |_| check(proceed),
                        )?;
                        original_profile.discard_completed_exports(false);
                    }
                }
                (Some("UserCustomedEvent"), Some("Events"), 5) => {}
                (Some("UserCustomedEvent"), Some("Events"), _) => {
                    return Err(error("middle trim cannot retain replaced error history"));
                }
                _ if data.is_empty() => {}
                _ => {
                    return Err(error(
                        "middle trim does not support this event history operation",
                    ));
                }
            }
            Ok(())
        };
        match event {
            HistoryEvent::Create(id, _) => {
                alive.insert(*id);
            }
            HistoryEvent::Destroy(id) => {
                alive.remove(id);
            }
            HistoryEvent::Set {
                kind: 3,
                id,
                property,
                data,
                ..
            } => inspect(*id, *property, data, 3)?,
            HistoryEvent::Set {
                kind: 4,
                id,
                property,
                data,
                ..
            } => {
                let message = layout.metadata.messages[*property as usize]
                    .as_ref()
                    .ok_or_else(|| error("undefined property message"))?;
                for entry in &message.entries {
                    inspect(
                        *id,
                        entry.property,
                        data.get(entry.offset..entry.offset + entry.size)
                            .ok_or_else(|| error("message field exceeds payload"))?,
                        4,
                    )?;
                }
            }
            HistoryEvent::Array {
                kind,
                id,
                property,
                data,
                ..
            } => inspect(*id, *property, data.unwrap_or_default(), *kind)?,
            HistoryEvent::Section { begin: 0, .. } if retained => {
                if frame == 0 && !needs_overwrite.is_empty() {
                    return Err(error(
                        "profile bootstrap requires captured buffers within the first retained frame",
                    ));
                }
                frame += 1;
            }
            _ => {}
        }
    }
    check(proceed)?;
    if !original_profile
        .open_event_starts()
        .eq(analysis.open_event_starts())
    {
        return Err(error("middle trim changed source pending profile starts"));
    }
    let mut bootstrap = profile_starts;
    if let Some(proof) = &renderer {
        let mut seeds = proof.seeds(&mut allocated, proceed)?;
        let capacity = bootstrap
            .len()
            .checked_add(seeds.len())
            .ok_or_else(|| error("bootstrap count overflow"))?;
        metadata_budget(&mut allocated, capacity, std::mem::size_of::<Seed>())?;
        for seed in &bootstrap {
            check(proceed)?;
            metadata_budget(&mut allocated, seed.bytes.capacity(), 1)?;
        }
        bootstrap.try_reserve_exact(seeds.len())?;
        metadata_budget(
            &mut allocated,
            bootstrap.capacity() - capacity,
            std::mem::size_of::<Seed>(),
        )?;
        bootstrap.append(&mut seeds);
    }
    Ok(Proof {
        first_event: start,
        last_event: end,
        last_frame: last,
        retained_offset: bootstrap.len() + 1,
        initial_instances,
        bootstrap,
        analysis: digest(&analysis, proceed)?,
        origin: None,
        initial: None,
        memory,
        renderer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_digest_returns_without_retrying_the_sink() {
        let mut calls = 0;
        let error = digest(&Analysis::default(), &mut || {
            calls += 1;
            false
        })
        .unwrap_err();
        assert_eq!(calls, 1);
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
    }

    #[test]
    fn native_memory_comparison_checks_cancellation_within_a_buffer() {
        let event = [0x33u16.to_le_bytes().as_slice(), &0x8000u64.to_le_bytes()].concat();
        let mut analysis = Analysis::default();
        let mut calls = 0;
        let error = analysis
            .ingest_native_memory_for_export(
                1,
                &event.repeat(1024),
                0,
                |_| Err(error("unexpected string")),
                &mut |_| {
                    calls += 1;
                    if calls == 3 { Err(cancelled()) } else { Ok(()) }
                },
            )
            .unwrap_err();
        assert_eq!(calls, 3);
        assert_eq!(analysis.memory.events().len(), 512);
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
    }
}
