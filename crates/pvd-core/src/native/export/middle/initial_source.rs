use super::*;

const CHUNK: usize = 64 << 10;

struct Work {
    remaining: usize,
}

impl Work {
    fn charge(&mut self, bytes: usize) -> Result<(), Error> {
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .ok_or_else(|| error("initial middle proof exceeds its work budget"))?;
        Ok(())
    }
}

pub(super) struct Scope {
    initial: initial::State,
    fields: BTreeMap<u32, Vec<u32>>,
    index_bytes: usize,
    camera_bytes: usize,
    analysis: Option<initial::analysis::Scope>,
}

#[derive(Clone, Copy)]
enum Kind {
    Ordinary,
    Scene,
    Error,
    Memory,
    Profile,
    Renderer(u32),
}

#[derive(Clone, Copy)]
struct AnalysisClasses<'a> {
    memory: Option<[u32; 2]>,
    scope: Option<&'a initial::analysis::Scope>,
}

pub(super) struct ProofContext<'a> {
    pub(super) index_bytes: usize,
    pub(super) bootstrap: &'a [history::Seed],
}

impl Scope {
    #[cfg(test)]
    pub(super) fn prepare(
        layout: &Layout<'_>,
        first: usize,
        last: usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Option<Self>, Error> {
        Self::prepare_with_cameras(layout, first, last, None, proceed)
    }

    pub(super) fn prepare_with_cameras(
        layout: &Layout<'_>,
        first: usize,
        last: usize,
        cameras: Option<&camera::Scope>,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Option<Self>, Error> {
        let held = cameras.map_or(0, camera::Scope::bytes);
        let camera_permit = cameras.map(|scope| scope.permit(layout)).transpose()?;
        let mut initial = initial::State::read_with_held(layout, held, proceed)?;
        if initial.entries.is_empty() {
            return Ok(None);
        }
        if !layout.communication.cameras.is_empty()
            && cameras.is_none_or(|scope| !scope.authorizes(layout, first, last))
        {
            return Err(error("initial middle requires verified camera catalogs"));
        }
        let memory_ids = layout.communication.memory_instances;
        let memory = if memory_ids == [u32::MAX; 2] {
            if !layout.communication.memory_names.is_empty() {
                return Err(error("initial middle memory sources have no captured pair"));
            }
            None
        } else {
            if memory_ids.contains(&u32::MAX) || memory_ids[0] == memory_ids[1] {
                return Err(error(
                    "initial middle memory requires a complete captured pair",
                ));
            }
            Some([
                *initial
                    .classes
                    .get(&memory_ids[0])
                    .ok_or_else(|| error("initial middle memory buffer is absent"))?,
                *initial
                    .classes
                    .get(&memory_ids[1])
                    .ok_or_else(|| error("initial middle memory totals are absent"))?,
            ])
        };
        let interval = layout.middle_frame_interval(first, last, proceed)?;
        let start = interval.start;
        let end = interval.end;
        let previous_end = interval.previous_end;
        let mut allocated = cameras.map_or(0, camera::Scope::bytes);
        metadata_budget(
            &mut allocated,
            initial.entries.capacity(),
            std::mem::size_of::<initial::Entry>(),
        )?;
        metadata_budget(&mut allocated, initial.classes.len(), 224)?;
        metadata_budget(&mut allocated, layout.saved.len(), 64)?;
        let analysis = initial::analysis::Scope::read_with_cameras(
            &initial,
            layout,
            first,
            last,
            camera_permit.as_ref(),
            &mut allocated,
            proceed,
        )?;
        initial.validate_with_analysis(layout, end, analysis.as_ref(), held, proceed)?;
        let analysis_classes = AnalysisClasses {
            memory,
            scope: analysis.as_ref(),
        };
        if let Some(classes) = memory {
            initial::memory::properties(layout, classes, &mut allocated, proceed)?;
        }
        let mut records = BTreeMap::new();
        for (index, saved) in layout.saved.iter().enumerate() {
            check(proceed)?;
            records.insert(saved.position, index);
        }
        let mut classes = BTreeMap::new();
        let mut alive = BTreeSet::new();
        let mut saved_targets = BTreeSet::new();
        for (&id, &class) in &initial.classes {
            check(proceed)?;
            classes.insert(id, class);
            alive.insert(id);
            saved_targets.insert(id);
        }
        let mut fields = BTreeMap::new();
        let mut kinds = BTreeMap::new();
        let mut scenes = Vec::new();
        let mut renderers = BTreeMap::new();
        for (&id, &class) in &classes {
            let kind = classify(
                layout,
                class,
                analysis_classes,
                &mut fields,
                &mut kinds,
                &mut allocated,
                proceed,
            )?;
            match kind {
                Kind::Scene => scenes.push(id),
                Kind::Memory if !memory_ids.contains(&id) => {
                    return Err(error("initial middle has an additional memory instance"));
                }
                Kind::Renderer(property) => {
                    metadata_budget(&mut allocated, 1, 64)?;
                    renderers.insert(id, property);
                }
                _ => {}
            }
        }
        if scenes.is_empty() {
            return Err(error("initial middle requires an initial Scene"));
        }
        let mut needs_context = BTreeSet::new();
        for (ordinal, event) in layout.events.iter().enumerate() {
            check(proceed)?;
            if ordinal == start {
                saved_targets.clear();
                for &id in &alive {
                    check(proceed)?;
                    saved_targets.insert(id);
                }
                metadata_budget(&mut allocated, renderers.len(), 32)?;
                for &id in renderers.keys() {
                    check(proceed)?;
                    if alive.contains(&id) {
                        needs_context.insert(id);
                    }
                }
            }
            match event {
                HistoryEvent::Create(id, class) => {
                    if classes.contains_key(id) {
                        return Err(error("initial middle requires unique instance lifecycles"));
                    }
                    metadata_budget(&mut allocated, 1, 160)?;
                    classes.insert(*id, *class);
                    alive.insert(*id);
                    saved_targets.insert(*id);
                    match classify(
                        layout,
                        *class,
                        analysis_classes,
                        &mut fields,
                        &mut kinds,
                        &mut allocated,
                        proceed,
                    )? {
                        Kind::Memory | Kind::Profile => {
                            return Err(error(
                                "initial middle analysis lifetimes cannot be created or reused",
                            ));
                        }
                        Kind::Renderer(property) => {
                            metadata_budget(&mut allocated, 1, 64)?;
                            renderers.insert(*id, property);
                        }
                        _ => {}
                    }
                }
                HistoryEvent::Destroy(id) => {
                    if !alive.remove(id) {
                        return Err(error("initial middle destroy targets missing instance"));
                    }
                    needs_context.remove(id);
                }
                HistoryEvent::OriginShift(_) => {
                    return Err(error("initial middle OriginShift is not verified"));
                }
                _ => {}
            }
            if ordinal >= end {
                continue;
            }
            match event {
                HistoryEvent::Set {
                    id,
                    kind,
                    property,
                    data,
                    ..
                } => {
                    if !alive.contains(id) {
                        return Err(error("initial middle property targets missing instance"));
                    }
                    if *kind == 3 {
                        layout.verify_property_pointers(*property, data, &alive, proceed)?;
                        if renderers.get(id) == Some(property) && !data.is_empty() {
                            if previous_end < ordinal && ordinal < start {
                                return Err(error("initial middle cannot drop interframe drawing"));
                            }
                            if ordinal >= start
                                && needs_context.remove(id)
                                && (data.first() != Some(&1) || data.len() < 9)
                            {
                                return Err(error(
                                    "initial middle drawing requires an explicit instance",
                                ));
                            }
                        }
                    } else {
                        let message = layout
                            .metadata
                            .messages
                            .get(*property as usize)
                            .and_then(Option::as_ref)
                            .ok_or_else(|| error("undefined property message"))?;
                        for entry in &message.entries {
                            check(proceed)?;
                            let stop = entry
                                .offset
                                .checked_add(entry.size)
                                .ok_or_else(|| error("message field offset overflow"))?;
                            layout.verify_property_pointers(
                                entry.property,
                                data.get(entry.offset..stop)
                                    .ok_or_else(|| error("message field exceeds payload"))?,
                                &alive,
                                proceed,
                            )?;
                        }
                    }
                }
                HistoryEvent::Array {
                    id, property, data, ..
                } => {
                    if !alive.contains(id) {
                        return Err(error("initial middle array targets missing instance"));
                    }
                    if let Some(data) = data {
                        layout.verify_property_pointers(*property, data, &alive, proceed)?;
                    }
                }
                HistoryEvent::Saved { id, position } => {
                    let record = records
                        .get(position)
                        .map(|index| &layout.saved[*index])
                        .ok_or_else(|| error("missing initial middle saved body"))?;
                    if !alive.contains(id)
                        || record.id != *id
                        || classes.get(id) != Some(&record.class)
                    {
                        return Err(error(
                            "initial middle saved body has no matching live instance",
                        ));
                    }
                    layout.verify_saved_pointers(record, &saved_targets, proceed)?;
                }
                _ => {}
            }
        }
        initial.entries.sort_by_key(|entry| entry.id);
        check(proceed)?;
        let mut index_bytes = 0;
        metadata_budget(
            &mut index_bytes,
            initial.entries.capacity(),
            std::mem::size_of::<initial::Entry>(),
        )?;
        metadata_budget(&mut index_bytes, initial.classes.len(), 64)?;
        metadata_budget(&mut index_bytes, fields.len(), 64)?;
        if let Some(scope) = &analysis {
            metadata_budget(&mut index_bytes, scope.bytes(), 1)?;
        }
        for fields in fields.values() {
            check(proceed)?;
            metadata_budget(
                &mut index_bytes,
                fields.capacity(),
                std::mem::size_of::<u32>(),
            )?;
        }
        Ok(Some(Self {
            initial,
            fields,
            index_bytes,
            camera_bytes: held,
            analysis,
        }))
    }

    pub(super) fn classes(&self) -> &BTreeMap<u32, u32> {
        &self.initial.classes
    }

    pub(super) fn index_bytes(&self) -> usize {
        self.index_bytes
    }

    pub(super) fn analysis(&self) -> Option<&initial::analysis::Scope> {
        self.analysis.as_ref()
    }

    pub(super) fn restore(
        &self,
        layout: &Layout<'_>,
        allocated: &mut usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<RawState, Error> {
        let mut state = RawState::new();
        for entry in &self.initial.entries {
            check(proceed)?;
            let saved = &layout.saved[entry.record];
            let mut reader = Reader::new(&saved.body);
            let fixed = reader.blob()?;
            if fixed.len() != layout.metadata.class(saved.class)?.size {
                return Err(error("initial middle fixed data differs from its class"));
            }
            metadata_budget(allocated, 1, 128)?;
            let mut values = BTreeMap::new();
            let mut defined = BTreeSet::new();
            for &id in &self.fields[&saved.class] {
                check(proceed)?;
                let property = layout.metadata.property(id)?;
                let width = layout.metadata.class(property.datatype_id)?.size;
                let data = if property.kind == 2 {
                    let bytes = reader.blob()?;
                    if !bytes.len().is_multiple_of(width) {
                        return Err(error("initial middle array does not match datatype"));
                    }
                    bytes
                } else {
                    fixed
                        .get(property.offset32..property.offset32 + width)
                        .ok_or_else(|| error("initial middle property exceeds fixed data"))?
                };
                metadata_budget(allocated, 1, 96)?;
                metadata_budget(allocated, data.len(), 1)?;
                let mut bytes = Vec::new();
                bytes.try_reserve_exact(data.len())?;
                metadata_budget(allocated, bytes.capacity().saturating_sub(data.len()), 1)?;
                append(&mut bytes, data, proceed)?;
                values.insert(id, bytes);
                defined.insert(id);
            }
            reader.end()?;
            state.insert(
                saved.id,
                RawInstance {
                    class: saved.class,
                    values,
                    defined,
                },
            );
        }
        Ok(state)
    }

    pub(super) fn body(
        &self,
        layout: &Layout<'_>,
        id: u32,
        instance: &RawInstance,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Vec<u8>, Error> {
        check(proceed)?;
        let size = instance.body_size(&layout.metadata)?;
        let fixed_size = layout.metadata.class(instance.class)?.size;
        let mut body = Vec::new();
        body.try_reserve_exact(size)?;
        w32(&mut body, u32_len(fixed_size)?);
        if let Ok(index) = self
            .initial
            .entries
            .binary_search_by_key(&id, |entry| entry.id)
        {
            let entry = &self.initial.entries[index];
            let saved = &layout.saved[entry.record];
            if saved.class != instance.class {
                return Err(error("initial middle template changed class"));
            }
            let mut reader = Reader::new(&saved.body);
            append(&mut body, reader.blob()?, proceed)?;
        } else {
            let zeros = [0; CHUNK];
            let mut left = fixed_size;
            while left != 0 {
                let count = left.min(CHUNK);
                append(&mut body, &zeros[..count], proceed)?;
                left -= count;
            }
        }
        let fields = self
            .fields
            .get(&instance.class)
            .ok_or_else(|| error("initial middle snapshot class is unavailable"))?;
        for &id in fields {
            check(proceed)?;
            let property = layout.metadata.property(id)?;
            let value = &instance.values[&id];
            if property.kind != 2 {
                let offset = property
                    .offset32
                    .checked_add(4)
                    .ok_or_else(|| error("initial middle fixed offset overflow"))?;
                let stop = offset
                    .checked_add(value.len())
                    .ok_or_else(|| error("initial middle fixed offset overflow"))?;
                let target = body
                    .get_mut(offset..stop)
                    .ok_or_else(|| error("initial middle scalar exceeds fixed data"))?;
                for (target, source) in target.chunks_mut(CHUNK).zip(value.chunks(CHUNK)) {
                    check(proceed)?;
                    target.copy_from_slice(source);
                }
            }
        }
        for &id in fields {
            check(proceed)?;
            if layout.metadata.property(id)?.kind == 2 {
                let value = &instance.values[&id];
                w32(&mut body, u32_len(value.len())?);
                append(&mut body, value, proceed)?;
            }
        }
        if body.len() != size {
            return Err(error("initial middle snapshot body size mismatch"));
        }
        Ok(body)
    }

    pub(super) fn prove(
        &self,
        layout: &Layout<'_>,
        range: std::ops::Range<usize>,
        baseline: &RawState,
        initial: &[Saved<'_>],
        context: ProofContext<'_>,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Proof, Error> {
        let mut allocated = self.index_bytes;
        metadata_budget(&mut allocated, context.index_bytes, 1)?;
        metadata_budget(&mut allocated, baseline.len(), 64)?;
        if initial.len() != baseline.len() {
            return Err(error("initial middle snapshot omits captured objects"));
        }
        let mut ids = BTreeSet::new();
        for &id in baseline.keys() {
            check(proceed)?;
            ids.insert(id);
        }
        let mut seen = BTreeSet::new();
        let mut work = Work { remaining: 1 << 30 };
        for record in initial {
            check(proceed)?;
            if !seen.insert(record.id) {
                return Err(error("initial middle snapshot repeats an object"));
            }
            let expected = baseline
                .get(&record.id)
                .filter(|instance| instance.class == record.class)
                .ok_or_else(|| error("initial middle snapshot changed object identity"))?;
            self.verify_body(layout, record, expected, allocated, &mut work, proceed)?;
            layout.verify_saved_pointers(record, &ids, proceed)?;
        }
        let mut proof = Proof::prepare(
            layout,
            range.start,
            range.end,
            initial,
            allocated,
            work,
            proceed,
        )?;
        proof.leading = context.bootstrap.len() + 1;
        proof.camera_bytes = self.camera_bytes;
        proof.analysis = self
            .analysis
            .as_ref()
            .map(|scope| scope.prove(context.bootstrap, &mut allocated, proceed))
            .transpose()?;
        Ok(proof)
    }

    fn verify_body(
        &self,
        layout: &Layout<'_>,
        record: &Saved<'_>,
        expected: &RawInstance,
        mut allocated: usize,
        work: &mut Work,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let mut reader = Reader::new(&record.body);
        let fixed = reader.blob()?;
        if fixed.len() != layout.metadata.class(record.class)?.size {
            return Err(error("initial middle proof fixed size mismatch"));
        }
        let fields = &self.fields[&record.class];
        metadata_budget(&mut allocated, fields.len(), 64)?;
        let mut scalar_regions = BTreeMap::new();
        for &id in fields {
            check(proceed)?;
            let property = layout.metadata.property(id)?;
            let width = layout.metadata.class(property.datatype_id)?.size;
            let bytes = if property.kind == 2 {
                reader.blob()?
            } else {
                let end = property
                    .offset32
                    .checked_add(width)
                    .ok_or_else(|| error("initial middle proof field overflow"))?;
                scalar_regions.insert(property.offset32, end);
                fixed
                    .get(property.offset32..end)
                    .ok_or_else(|| error("initial middle proof field exceeds fixed data"))?
            };
            let captured = expected
                .values
                .get(&id)
                .filter(|_| expected.defined.contains(&id))
                .ok_or_else(|| error("initial middle proof has no captured field"))?;
            equal_bytes(bytes, captured, work, proceed)?;
        }
        reader.end()?;
        let template = self
            .initial
            .entries
            .binary_search_by_key(&record.id, |entry| entry.id)
            .ok()
            .map(|index| {
                let saved = &layout.saved[self.initial.entries[index].record];
                Reader::new(&saved.body).blob()
            })
            .transpose()?;
        let mut previous = 0;
        for (start, end) in scalar_regions
            .into_iter()
            .chain([(fixed.len(), fixed.len())])
        {
            let actual = &fixed[previous..start];
            if let Some(template) = template {
                equal_bytes(actual, &template[previous..start], work, proceed)?;
            } else {
                work.charge(actual.len())?;
                for chunk in actual.chunks(CHUNK) {
                    check(proceed)?;
                    if chunk.iter().any(|byte| *byte != 0) {
                        return Err(error("initial middle changed native Create fixed storage"));
                    }
                }
            }
            previous = end;
        }
        Ok(())
    }
}

fn equal_bytes(
    left: &[u8],
    right: &[u8],
    work: &mut Work,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    work.charge(left.len())?;
    if left.len() != right.len() {
        return Err(error(
            "initial middle snapshot differs from captured raw state",
        ));
    }
    for (left, right) in left.chunks(CHUNK).zip(right.chunks(CHUNK)) {
        check(proceed)?;
        if left != right {
            return Err(error(
                "initial middle snapshot differs from captured raw state",
            ));
        }
    }
    Ok(())
}

fn classify(
    layout: &Layout<'_>,
    class: u32,
    analysis: AnalysisClasses<'_>,
    fields: &mut BTreeMap<u32, Vec<u32>>,
    kinds: &mut BTreeMap<u32, Kind>,
    allocated: &mut usize,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<Kind, Error> {
    if let Some(kind) = kinds.get(&class) {
        return Ok(*kind);
    }
    metadata_budget(allocated, 1, 128)?;
    let description = layout.metadata.class(class)?;
    let label = |id| {
        layout
            .metadata
            .strings
            .get(&id)
            .map(String::as_str)
            .ok_or_else(|| error("undefined initial middle class name"))
    };
    let name = label(description.name.name)?;
    let namespace = label(description.name.namespace)?;
    let kind = if analysis
        .memory
        .is_some_and(|classes| classes.contains(&class))
    {
        Kind::Memory
    } else if analysis
        .scope
        .is_some_and(|scope| scope.profile_class(class))
    {
        Kind::Profile
    } else if namespace == "physx3" && name == "PxScene" {
        Kind::Scene
    } else if namespace == "_debugger_" && name == "UserCustomedEvent" {
        Kind::Error
    } else if namespace == "_debugger_" && name == "PvdUserRenderer" {
        let [property] = description.properties.as_slice() else {
            return Err(error("unsupported initial middle renderer layout"));
        };
        let p = layout.metadata.property(*property)?;
        let datatype = layout.metadata.class(p.datatype_id)?;
        if description.base != u32::MAX
            || p.kind != 2
            || label(p.name)? != "events"
            || datatype.size != 1
            || datatype.dynamic
            || datatype.has_pointers
            || label(datatype.name.namespace)? != "physx3"
            || label(datatype.name.name)? != "PvdU8"
        {
            return Err(error("unsupported initial middle renderer layout"));
        }
        Kind::Renderer(*property)
    } else {
        Kind::Ordinary
    };
    let mut ancestors = Vec::new();
    let mut current = class;
    while current != u32::MAX {
        check(proceed)?;
        if ancestors.len() == 128 || ancestors.contains(&current) {
            return Err(error("invalid initial middle class inheritance"));
        }
        ancestors.push(current);
        let ancestor = layout.metadata.class(current)?;
        let name = label(ancestor.name.name)?;
        let namespace = label(ancestor.name.namespace)?;
        if (matches!(
            name,
            "PxProfileZone" | "PxProfileMemoryEventBuffer" | "MemoryEventTotals"
        ) && !matches!(kind, Kind::Memory | Kind::Profile))
            || (namespace.starts_with("_debugger")
                && !(current == class
                    && matches!(
                        kind,
                        Kind::Error | Kind::Memory | Kind::Profile | Kind::Renderer(_)
                    )))
        {
            return Err(error("initial middle analysis state is not verified"));
        }
        current = ancestor.base;
    }
    let count = ancestors.iter().try_fold(0usize, |count, id| {
        count
            .checked_add(layout.metadata.class(*id)?.properties.len())
            .ok_or_else(|| error("initial middle property count overflow"))
    })?;
    metadata_budget(allocated, count, 128)?;
    let mut ids = Vec::new();
    ids.try_reserve_exact(count)?;
    let mut regions = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for ancestor in ancestors.iter().rev().copied() {
        let owner = layout.metadata.class(ancestor)?;
        for &id in &owner.properties {
            check(proceed)?;
            let property = layout.metadata.property(id)?;
            let datatype = layout.metadata.class(property.datatype_id)?;
            if property.owner != owner.name
                || property.datatype != datatype.name
                || !matches!(property.kind, 1 | 2)
                || !seen.insert(id)
                || datatype.dynamic
                || datatype.size == 0
            {
                return Err(error("unsupported initial middle property layout"));
            }
            let width = if property.kind == 2 {
                12
            } else {
                datatype.size
            };
            let end = property
                .offset32
                .checked_add(width)
                .filter(|end| *end <= description.size)
                .ok_or_else(|| error("initial middle property exceeds fixed layout"))?;
            if regions
                .range(..end)
                .next_back()
                .is_some_and(|(_, stop)| *stop > property.offset32)
            {
                return Err(error("initial middle properties overlap"));
            }
            regions.insert(property.offset32, end);
            ids.push(id);
        }
    }
    let mut pointers = BTreeSet::new();
    for &id in &ids {
        check(proceed)?;
        let property = layout.metadata.property(id)?;
        if property.kind == 2 {
            continue;
        }
        let width = layout.metadata.class(property.datatype_id)?.size;
        let offsets = layout
            .pointer_layouts
            .get(property.datatype_id as usize)
            .ok_or_else(|| error("initial middle datatype has no pointer layout"))?;
        for &(kind, offset) in offsets {
            check(proceed)?;
            if !matches!(kind, 1 | 2) || offset.checked_add(4).is_none_or(|end| end > width) {
                return Err(error("unsupported initial middle field pointer layout"));
            }
            metadata_budget(allocated, 1, 48)?;
            pointers.insert((
                kind,
                property
                    .offset32
                    .checked_add(offset)
                    .ok_or_else(|| error("initial middle pointer offset overflow"))?,
            ));
        }
    }
    for ancestor in ancestors {
        let offsets = layout
            .pointer_layouts
            .get(ancestor as usize)
            .ok_or_else(|| error("initial middle class has no pointer layout"))?;
        for &pointer in offsets {
            check(proceed)?;
            if !pointers.contains(&pointer) {
                return Err(error(
                    "initial middle class pointer is outside known scalar fields",
                ));
            }
        }
    }
    fields.insert(class, ids);
    kinds.insert(class, kind);
    Ok(kind)
}

fn append(out: &mut Vec<u8>, bytes: &[u8], proceed: &mut dyn FnMut() -> bool) -> Result<(), Error> {
    for chunk in bytes.chunks(CHUNK) {
        check(proceed)?;
        out.extend_from_slice(chunk);
    }
    Ok(())
}

#[derive(Debug)]
pub(in crate::native::export) struct Proof {
    metadata: blake3::Hash,
    initial: blake3::Hash,
    saved: blake3::Hash,
    reserved: u32,
    work_remaining: usize,
    leading: usize,
    camera_bytes: usize,
    analysis: Option<initial::analysis::Proof>,
}

impl Proof {
    fn prepare(
        layout: &Layout<'_>,
        start: usize,
        end: usize,
        initial: &[Saved<'_>],
        allocated: usize,
        mut work: Work,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Self, Error> {
        let mut hash = blake3::Hasher::new();
        work.charge(8)?;
        hash.update(&(initial.len() as u64).to_le_bytes());
        for record in initial {
            hash_record(&mut hash, record, &mut work, proceed)?;
        }
        let metadata = digest(layout.object_bytes, &mut work, proceed)?;
        let saved = saved_digest(layout, start, end, false, allocated, &mut work, proceed)?;
        Ok(Self {
            metadata,
            initial: hash.finalize(),
            saved,
            reserved: layout.snapshots[0].reserved,
            work_remaining: work.remaining,
            leading: 1,
            camera_bytes: 0,
            analysis: None,
        })
    }

    fn owned_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self
                .analysis
                .as_ref()
                .map_or(0, initial::analysis::Proof::owned_buffer_bytes)
    }

    pub(in crate::native::export) fn verify_layout(
        &self,
        layout: &Layout<'_>,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let mut held = self.camera_bytes;
        metadata_budget(&mut held, self.owned_bytes(), 1)?;
        let state = initial::State::read_with_held(layout, held, proceed)?;
        let mut work = Work {
            remaining: self.work_remaining,
        };
        let mut hash = blake3::Hasher::new();
        work.charge(8)?;
        hash.update(&(state.entries.len() as u64).to_le_bytes());
        for entry in &state.entries {
            hash_record(&mut hash, &layout.saved[entry.record], &mut work, proceed)?;
        }
        let initial_matches = hash.finalize() == self.initial;
        drop(state);
        if !initial_matches
            || digest(layout.object_bytes, &mut work, proceed)? != self.metadata
            || layout.snapshots[0].reserved != self.reserved
            || saved_digest(
                layout,
                self.leading,
                layout.events.len(),
                true,
                held,
                &mut work,
                proceed,
            )? != self.saved
        {
            return Err(error(
                "initial middle changed captured snapshot or saved bytes",
            ));
        }
        if let Some(analysis) = &self.analysis {
            analysis.verify_layout(layout, proceed)?;
        }
        Ok(())
    }
}

fn digest(
    bytes: &[u8],
    work: &mut Work,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<blake3::Hash, Error> {
    let mut hash = blake3::Hasher::new();
    hash_bytes(&mut hash, bytes, work, proceed)?;
    Ok(hash.finalize())
}

fn hash_bytes(
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
            .ok_or_else(|| error("initial middle proof size overflow"))?,
    )?;
    hash.update(&(bytes.len() as u64).to_le_bytes());
    for chunk in bytes.chunks(CHUNK) {
        check(proceed)?;
        hash.update(chunk);
    }
    Ok(())
}

fn hash_record(
    hash: &mut blake3::Hasher,
    record: &Saved<'_>,
    work: &mut Work,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    work.charge(8)?;
    hash.update(&record.id.to_le_bytes());
    hash.update(&record.class.to_le_bytes());
    hash_bytes(hash, &record.body, work, proceed)
}

fn saved_digest(
    layout: &Layout<'_>,
    start: usize,
    end: usize,
    exact: bool,
    mut allocated: usize,
    work: &mut Work,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<blake3::Hash, Error> {
    metadata_budget(&mut allocated, layout.saved.len(), 64)?;
    let mut records = BTreeMap::new();
    for record in &layout.saved {
        check(proceed)?;
        work.charge(8)?;
        records.insert(record.position, record);
    }
    let mut hash = blake3::Hasher::new();
    let mut destroyed = BTreeSet::new();
    for (ordinal, event) in layout
        .events
        .get(start..end)
        .ok_or_else(|| error("initial middle proof exceeds history"))?
        .iter()
        .enumerate()
    {
        check(proceed)?;
        work.charge(8)?;
        if let HistoryEvent::Saved { id, position } = event {
            let record = records
                .get(position)
                .filter(|record| record.id == *id)
                .ok_or_else(|| error("initial middle proof has no saved body"))?;
            hash.update(&(ordinal as u64).to_le_bytes());
            hash_record(&mut hash, record, work, proceed)?;
        } else if let HistoryEvent::Destroy(id) = event {
            metadata_budget(&mut allocated, 1, 96)?;
            destroyed.insert(*id);
        }
    }
    let mut deleted = BTreeMap::new();
    for record in &layout.saved {
        check(proceed)?;
        work.charge(8)?;
        if record.deleted {
            if destroyed.contains(&record.id) {
                if deleted.insert(record.id, record).is_some() {
                    return Err(error("initial middle deleted directory is ambiguous"));
                }
            } else if exact {
                return Err(error(
                    "initial middle deleted directory exceeds retained history",
                ));
            }
        }
    }
    if deleted.len() != destroyed.len() {
        return Err(error("initial middle destroy has no captured deleted body"));
    }
    hash.update(&(deleted.len() as u64).to_le_bytes());
    work.charge(8)?;
    for record in deleted.values() {
        hash_record(&mut hash, record, work, proceed)?;
    }
    Ok(hash.finalize())
}

#[cfg(test)]
mod tests;
