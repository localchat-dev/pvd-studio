use super::*;

pub(super) mod analysis;
pub(super) mod memory;

pub(super) struct Entry {
    pub(super) id: u32,
    pub(super) position: u64,
    pub(super) record: usize,
}

pub(super) struct State {
    pub(super) entries: Vec<Entry>,
    pub(super) classes: BTreeMap<u32, u32>,
}

pub(super) struct Budget {
    bytes: usize,
}

impl Budget {
    pub(super) fn new(state: &State) -> Result<Self, Error> {
        let mut budget = Self { bytes: 0 };
        budget.add(state.entries.capacity(), std::mem::size_of::<Entry>())?;
        budget.add(state.classes.len(), 64)?;
        Ok(budget)
    }

    pub(super) fn add(&mut self, count: usize, width: usize) -> Result<(), Error> {
        metadata_budget(&mut self.bytes, count, width)
    }
}

impl State {
    pub(super) fn read(
        layout: &Layout<'_>,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Self, Error> {
        Self::read_with_held(layout, 0, proceed)
    }

    pub(super) fn read_with_held(
        layout: &Layout<'_>,
        held: usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Self, Error> {
        if layout.snapshots.len() != 1 || layout.snapshots[0].event != 0 {
            return Err(error("prefix trim requires one event-zero snapshot"));
        }
        let mut reader = Reader::new(&layout.snapshot_bytes[0]);
        let count = reader.count()?;
        let mut allocated = held;
        metadata_budget(&mut allocated, count, std::mem::size_of::<Entry>() + 64)?;
        metadata_budget(&mut allocated, layout.saved.len(), 64)?;
        let mut records = BTreeMap::new();
        for (index, record) in layout.saved.iter().enumerate() {
            check(proceed)?;
            if records.insert(record.position, index).is_some() {
                return Err(error("duplicate initial backing position"));
            }
        }
        let mut entries = Vec::new();
        entries.try_reserve_exact(count)?;
        let mut classes = BTreeMap::new();
        for _ in 0..count {
            check(proceed)?;
            let id = reader.u32()?;
            let position = reader.u64()?;
            let record = *records
                .get(&position)
                .ok_or_else(|| error("initial instance has no saved body"))?;
            let saved = &layout.saved[record];
            if saved.id != id || classes.insert(id, saved.class).is_some() {
                return Err(error("initial instance identity is inconsistent"));
            }
            entries.push(Entry {
                id,
                position,
                record,
            });
        }
        reader.end()?;
        Ok(Self { entries, classes })
    }

    #[cfg(test)]
    pub(super) fn validate_prefix(
        &self,
        layout: &Layout<'_>,
        cut: usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        self.validate_prefix_with_cameras(layout, cut, None, proceed)
    }

    fn validate_prefix_with_cameras(
        &self,
        layout: &Layout<'_>,
        cut: usize,
        cameras: Option<&camera::Permit<'_>>,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let analysis = self.prefix_analysis_with_cameras(layout, cut, cameras, proceed)?;
        self.validate_with_analysis(
            layout,
            cut,
            analysis.as_ref(),
            cameras.map_or(0, camera::Permit::bytes),
            proceed,
        )
    }

    #[cfg(test)]
    pub(super) fn prefix_analysis(
        &self,
        layout: &Layout<'_>,
        cut: usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Option<analysis::Scope>, Error> {
        self.prefix_analysis_with_cameras(layout, cut, None, proceed)
    }

    pub(super) fn prefix_analysis_with_cameras(
        &self,
        layout: &Layout<'_>,
        cut: usize,
        cameras: Option<&camera::Permit<'_>>,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Option<analysis::Scope>, Error> {
        self.prefix_analysis_bounded(layout, cut, cameras, &mut 0, proceed)
    }

    pub(super) fn prefix_analysis_bounded(
        &self,
        layout: &Layout<'_>,
        cut: usize,
        cameras: Option<&camera::Permit<'_>>,
        allocated: &mut usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Option<analysis::Scope>, Error> {
        let mut budget = Budget::new(self)?;
        budget.add(*allocated, 1)?;
        budget.add(cameras.map_or(0, camera::Permit::bytes), 1)?;
        *allocated = budget.bytes;
        analysis::Scope::read_prefix_with_cameras(self, layout, cut, cameras, allocated, proceed)
    }

    pub(super) fn validate_with_analysis(
        &self,
        layout: &Layout<'_>,
        cut: usize,
        analysis: Option<&analysis::Scope>,
        held: usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        if self.entries.is_empty() {
            return Ok(());
        }
        if !matches!(layout.events.first(), Some(HistoryEvent::Marker)) {
            return Err(error("nonempty prefix requires an event-zero marker"));
        }
        let mut budget = Budget::new(self)?;
        budget.add(held, 1)?;
        if let Some(scope) = analysis {
            budget.add(scope.bytes(), 1)?;
        }
        budget.add(self.classes.len(), 128)?;
        budget.add(128, 64)?;
        let ids = self.classes.keys().copied().collect();
        let memory = memory::Scope::read(self, layout, &mut budget, proceed)?;
        if layout.communication.zones.iter().any(|zone| {
            self.classes
                .get(&zone.id)
                .is_some_and(|class| analysis.is_none_or(|scope| !scope.contains(zone.id, *class)))
        }) {
            return Err(error("initial analysis state is not verified for trimming"));
        }
        let mut scenes = 0;
        let mut renderers = BTreeMap::new();
        for entry in &self.entries {
            check(proceed)?;
            let saved = &layout.saved[entry.record];
            let class = layout.metadata.class(saved.class)?;
            let memory_instance = memory
                .as_ref()
                .is_some_and(|scope| scope.contains(saved.id));
            let analysis_instance = analysis.is_some_and(|scope| scope.contains_record(saved));
            let name = layout
                .metadata
                .strings
                .get(&class.name.name)
                .ok_or_else(|| error("undefined initial class name"))?;
            let renderer = layout
                .metadata
                .strings
                .get(&class.name.namespace)
                .map(String::as_str)
                == Some("_debugger_")
                && name == "PvdUserRenderer";
            if renderer {
                let property = match analysis.and_then(|scope| scope.renderer_property(saved.id)) {
                    Some(property) if analysis_instance => property,
                    _ => empty_renderer_property(layout, saved)?,
                };
                renderers.insert(saved.id, property);
            }
            let mut ancestors = BTreeSet::new();
            let mut ancestor = saved.class;
            while ancestor != u32::MAX {
                check(proceed)?;
                if ancestors.len() == 128 || !ancestors.insert(ancestor) {
                    return Err(error("invalid initial class inheritance"));
                }
                let class = layout.metadata.class(ancestor)?;
                let namespace = layout
                    .metadata
                    .strings
                    .get(&class.name.namespace)
                    .ok_or_else(|| error("undefined initial class namespace"))?;
                let name = layout
                    .metadata
                    .strings
                    .get(&class.name.name)
                    .ok_or_else(|| error("undefined initial class name"))?;
                if !memory_instance
                    && !analysis_instance
                    && (matches!(
                        name.as_str(),
                        "PxProfileZone" | "PxProfileMemoryEventBuffer" | "MemoryEventTotals"
                    ) || (namespace.starts_with("_debugger")
                        && (namespace != "_debugger_"
                            || (name != "UserCustomedEvent"
                                && !(renderer && ancestor == saved.class)))))
                {
                    return Err(error(
                        "initial debugger analysis and renderer state are not verified for trimming",
                    ));
                }
                ancestor = class.base;
            }
            if name == "PxScene"
                && layout
                    .metadata
                    .strings
                    .get(&class.name.namespace)
                    .map(String::as_str)
                    == Some("physx3")
            {
                scenes += 1;
            }
            layout.verify_saved_pointers(saved, &ids, proceed)?;
        }
        if scenes == 0 {
            return Err(error("nonempty prefix requires an initial Scene"));
        }
        for event in layout
            .events
            .get(..cut)
            .ok_or_else(|| error("renderer prefix exceeds history"))?
        {
            check(proceed)?;
            match event {
                HistoryEvent::Set {
                    kind: 3,
                    id,
                    property,
                    data,
                    ..
                } if renderers.get(id) == Some(property) && !data.is_empty() => {
                    if data.first() != Some(&1) || data.len() < 9 {
                        return Err(error(
                            "initial renderer requires an explicit instance in its first retained buffer",
                        ));
                    }
                    renderers.remove(id);
                }
                HistoryEvent::Set { kind: 4, id, .. } | HistoryEvent::Array { id, .. }
                    if renderers.contains_key(id) =>
                {
                    return Err(error("initial renderer requires complete property buffers"));
                }
                HistoryEvent::Destroy(id) => {
                    renderers.remove(id);
                }
                _ => {}
            }
        }
        Ok(())
    }
}

fn empty_renderer_property(layout: &Layout<'_>, saved: &Saved<'_>) -> Result<u32, Error> {
    let class = layout.metadata.class(saved.class)?;
    if class.base != u32::MAX || class.properties.len() != 1 {
        return Err(error("unsupported initial renderer layout"));
    }
    let id = class.properties[0];
    let property = layout.metadata.property(id)?;
    let datatype = layout.metadata.class(property.datatype_id)?;
    if property.owner != class.name
        || layout
            .metadata
            .strings
            .get(&property.name)
            .map(String::as_str)
            != Some("events")
        || property.kind != 2
        || datatype.size != 1
        || datatype.dynamic
        || datatype.has_pointers
        || datatype.name != property.datatype
        || layout
            .metadata
            .strings
            .get(&datatype.name.namespace)
            .map(String::as_str)
            != Some("physx3")
        || layout
            .metadata
            .strings
            .get(&datatype.name.name)
            .map(String::as_str)
            != Some("PvdU8")
    {
        return Err(error("unsupported initial renderer event layout"));
    }
    let mut body = Reader::new(&saved.body);
    let fixed = body.blob()?;
    if fixed.len() != class.size
        || property
            .offset32
            .checked_add(12)
            .is_none_or(|end| end > fixed.len())
        || !body.blob()?.is_empty()
    {
        return Err(error("initial renderer requires an empty captured buffer"));
    }
    body.end()?;
    Ok(id)
}

pub(super) struct Proof {
    cut: usize,
    digest: blake3::Hash,
    memory: Option<memory::Proof>,
}

impl Proof {
    #[cfg(test)]
    pub(super) fn prepare(
        layout: &Layout<'_>,
        cut: usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Self, Error> {
        Self::prepare_with_cameras(layout, cut, None, proceed)
    }

    pub(super) fn prepare_with_cameras(
        layout: &Layout<'_>,
        cut: usize,
        cameras: Option<&camera::Permit<'_>>,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Self, Error> {
        let held = cameras.map_or(0, camera::Permit::bytes);
        verify_deleted(layout, cut, false, held, proceed)?;
        Ok(Self {
            cut,
            digest: digest(layout, cut, cameras, proceed)?,
            memory: memory::Proof::prepare_with_held(layout, cut, held, proceed)?,
        })
    }

    #[cfg(test)]
    pub(super) fn verify_layout(
        &self,
        layout: &Layout<'_>,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        self.verify_with_cameras(layout, None, proceed)
    }

    pub(super) fn verify_with_cameras(
        &self,
        layout: &Layout<'_>,
        cameras: Option<&camera::Permit<'_>>,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let held = cameras.map_or(0, camera::Permit::bytes);
        verify_deleted(layout, self.cut, true, held, proceed)?;
        if layout.events.len() != self.cut
            || digest(layout, self.cut, cameras, proceed)? != self.digest
        {
            return Err(error("prefix trim changed retained capture inputs"));
        }
        if let Some(memory) = &self.memory {
            memory.verify_with_held(layout, held, proceed)?;
        }
        Ok(())
    }

    pub(super) fn verify_errors(
        &self,
        output: &crate::Recording,
        source: &crate::Recording,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        if output.initial_untimed_errors().len() != source.initial_untimed_errors().len() {
            return Err(error("prefix trim changed initial error records"));
        }
        for (actual, expected) in output
            .initial_untimed_errors()
            .iter()
            .zip(source.initial_untimed_errors())
        {
            check(proceed)?;
            if actual != expected {
                return Err(error("prefix trim changed initial error records"));
            }
        }
        let mut expected = source
            .untimed_error_events()
            .iter()
            .filter(|event| event.history_event < self.cut as u64);
        for actual in output.untimed_error_events() {
            check(proceed)?;
            if expected.next() != Some(actual) {
                return Err(error("prefix trim changed captured error occurrences"));
            }
        }
        if expected.next().is_some() {
            return Err(error("prefix trim lost captured error occurrences"));
        }
        Ok(())
    }
}

fn field(
    hash: &mut blake3::Hasher,
    bytes: &[u8],
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    hash.update(&(bytes.len() as u64).to_le_bytes());
    for chunk in bytes.chunks(64 << 10) {
        check(proceed)?;
        hash.update(chunk);
    }
    Ok(())
}

fn digest(
    layout: &Layout<'_>,
    cut: usize,
    cameras: Option<&camera::Permit<'_>>,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<blake3::Hash, Error> {
    let held = cameras.map_or(0, camera::Permit::bytes);
    let initial = State::read_with_held(layout, held, proceed)?;
    initial.validate_prefix_with_cameras(layout, cut, cameras, proceed)?;
    let mut hash = blake3::Hasher::new();
    field(
        &mut hash,
        &layout.snapshots[0].reserved.to_le_bytes(),
        proceed,
    )?;
    field(&mut hash, layout.object_bytes, proceed)?;
    for clock in layout.communication.clock {
        field(&mut hash, &clock.to_le_bytes(), proceed)?;
    }
    field(
        &mut hash,
        &(initial.entries.len() as u64).to_le_bytes(),
        proceed,
    )?;
    let mut budget = Budget::new(&initial)?;
    budget.add(held, 1)?;
    budget.add(initial.classes.len(), 64)?;
    budget.add(layout.saved.len(), 64)?;
    let mut retained = initial.classes.keys().copied().collect::<BTreeSet<_>>();
    let mut positions = BTreeMap::new();
    for record in &layout.saved {
        check(proceed)?;
        positions.insert(record.position, record);
    }
    for entry in &initial.entries {
        check(proceed)?;
        let record = &layout.saved[entry.record];
        field(&mut hash, &record.id.to_le_bytes(), proceed)?;
        field(&mut hash, &record.class.to_le_bytes(), proceed)?;
        field(&mut hash, &record.body, proceed)?;
    }
    for event in layout
        .events
        .get(..cut)
        .ok_or_else(|| error("prefix proof exceeds history"))?
    {
        check(proceed)?;
        match event {
            HistoryEvent::Create(id, class) => {
                field(&mut hash, &1u32.to_le_bytes(), proceed)?;
                field(&mut hash, &id.to_le_bytes(), proceed)?;
                field(&mut hash, &class.to_le_bytes(), proceed)?;
            }
            HistoryEvent::Destroy(id) => {
                field(&mut hash, &2u32.to_le_bytes(), proceed)?;
                field(&mut hash, &id.to_le_bytes(), proceed)?;
            }
            HistoryEvent::Set {
                kind,
                id,
                property,
                data,
                undo,
            } => {
                field(&mut hash, &kind.to_le_bytes(), proceed)?;
                field(&mut hash, &id.to_le_bytes(), proceed)?;
                field(&mut hash, &property.to_le_bytes(), proceed)?;
                field(&mut hash, data, proceed)?;
                undo_words(&mut hash, undo, proceed)?;
            }
            HistoryEvent::Array {
                kind,
                id,
                property,
                index,
                data,
                undo,
            } => {
                field(&mut hash, &kind.to_le_bytes(), proceed)?;
                field(&mut hash, &id.to_le_bytes(), proceed)?;
                field(&mut hash, &property.to_le_bytes(), proceed)?;
                field(&mut hash, &index.to_le_bytes(), proceed)?;
                field(&mut hash, &[u8::from(data.is_some())], proceed)?;
                if let Some(data) = data {
                    field(&mut hash, data, proceed)?;
                }
                undo_words(&mut hash, undo, proceed)?;
            }
            HistoryEvent::Section {
                class,
                source,
                ticks,
                name,
                begin,
                padding,
            } => {
                field(&mut hash, &8u32.to_le_bytes(), proceed)?;
                field(&mut hash, &class.to_le_bytes(), proceed)?;
                field(&mut hash, &source.to_le_bytes(), proceed)?;
                field(&mut hash, &ticks.to_le_bytes(), proceed)?;
                field(&mut hash, &name.to_le_bytes(), proceed)?;
                field(&mut hash, &[*begin], proceed)?;
                field(&mut hash, padding, proceed)?;
            }
            HistoryEvent::Marker => {
                field(&mut hash, &9u32.to_le_bytes(), proceed)?;
            }
            HistoryEvent::Saved { id, position } => {
                let saved = positions
                    .get(position)
                    .filter(|record| record.id == *id)
                    .ok_or_else(|| error("prefix proof has no saved witness"))?;
                field(&mut hash, &10u32.to_le_bytes(), proceed)?;
                field(&mut hash, &id.to_le_bytes(), proceed)?;
                field(&mut hash, &saved.class.to_le_bytes(), proceed)?;
                field(&mut hash, &saved.body, proceed)?;
            }
            HistoryEvent::OriginShift(event) => {
                if !initial.entries.is_empty() {
                    return Err(error(
                        "nonempty prefix does not support OriginShift history",
                    ));
                }
                event.digest(&mut hash, proceed)?;
            }
        }
        if let HistoryEvent::Create(id, _) = event {
            budget.add(1, 64)?;
            retained.insert(*id);
        }
    }
    field(&mut hash, b"instances", proceed)?;
    for instance in &layout.communication.instances {
        check(proceed)?;
        if retained.contains(&instance.id) {
            field(&mut hash, &instance.id.to_le_bytes(), proceed)?;
            field(&mut hash, &instance.source.to_le_bytes(), proceed)?;
            field(&mut hash, &instance.flags, proceed)?;
        }
    }
    field(&mut hash, b"zones", proceed)?;
    for zone in &layout.communication.zones {
        check(proceed)?;
        if retained.contains(&zone.id) {
            field(&mut hash, &zone.id.to_le_bytes(), proceed)?;
            field(&mut hash, &zone.source.to_le_bytes(), proceed)?;
            field(&mut hash, &zone.saved_word.to_le_bytes(), proceed)?;
            field(&mut hash, zone.name, proceed)?;
            field(
                &mut hash,
                &(zone.events.len() as u64).to_le_bytes(),
                proceed,
            )?;
            for &(name, id, enabled) in &zone.events {
                field(&mut hash, name, proceed)?;
                field(&mut hash, &id.to_le_bytes(), proceed)?;
                field(&mut hash, &[enabled], proceed)?;
            }
        }
    }
    field(&mut hash, b"memory", proceed)?;
    for id in layout.communication.memory_instances {
        field(&mut hash, &id.to_le_bytes(), proceed)?;
    }
    for &(file, kind, line, handle) in &layout.communication.memory_names {
        field(&mut hash, file, proceed)?;
        field(&mut hash, kind, proceed)?;
        field(&mut hash, &line.to_le_bytes(), proceed)?;
        field(&mut hash, &handle.to_le_bytes(), proceed)?;
    }
    Ok(hash.finalize())
}

fn undo_words(
    hash: &mut blake3::Hasher,
    words: &[u32],
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    hash.update(&(words.len() as u64).to_le_bytes());
    for chunk in words.chunks(16 << 10) {
        check(proceed)?;
        for word in chunk {
            hash.update(&word.to_le_bytes());
        }
    }
    Ok(())
}

fn verify_deleted(
    layout: &Layout<'_>,
    cut: usize,
    exact: bool,
    held: usize,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    let mut saved = BTreeMap::new();
    let mut destroyed = BTreeSet::new();
    let mut allocated = held;
    for event in layout
        .events
        .get(..cut)
        .ok_or_else(|| error("prefix proof exceeds history"))?
    {
        check(proceed)?;
        match event {
            HistoryEvent::Saved { id, position } => {
                metadata_budget(&mut allocated, 1, 64)?;
                if saved.insert(*id, *position).is_some() {
                    return Err(error("prefix requires one saved witness per lifecycle"));
                }
            }
            HistoryEvent::Destroy(id) => {
                metadata_budget(&mut allocated, 1, 64)?;
                destroyed.insert(*id);
            }
            _ => {}
        }
    }
    for record in &layout.saved {
        check(proceed)?;
        let expected =
            destroyed.contains(&record.id) && saved.get(&record.id) == Some(&record.position);
        if (expected && !record.deleted) || (exact && record.deleted && !expected) {
            return Err(error(
                "prefix deleted directory disagrees with retained lifecycles",
            ));
        }
    }
    for id in destroyed {
        check(proceed)?;
        if !saved.contains_key(&id) {
            return Err(error("prefix destroy has no captured saved witness"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
