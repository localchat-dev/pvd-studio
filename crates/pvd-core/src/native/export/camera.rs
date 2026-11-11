use super::*;
use std::ops::Range;

const FIXED: usize = 40;
const WORK_LIMIT: usize = 1 << 30;

struct Budget {
    bytes: usize,
    limit: usize,
    work: usize,
}

impl Budget {
    fn allocate(&mut self, count: usize, width: usize) -> Result<(), Error> {
        metadata_budget(&mut self.bytes, count, width)?;
        if self.bytes > self.limit {
            return Err(error("camera proof exceeds its memory budget"));
        }
        Ok(())
    }

    fn step(&mut self, bytes: usize, proceed: &mut dyn FnMut() -> bool) -> Result<(), Error> {
        check(proceed)?;
        self.work = self
            .work
            .checked_sub(bytes.max(1))
            .ok_or_else(|| error("camera proof exceeds its work budget"))?;
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct Schema {
    class: u32,
    properties: [u32; 4],
    vector: u32,
}

#[derive(Clone, Copy)]
struct Update<'a> {
    kind: u32,
    property: u32,
    data: &'a [u8],
}

impl Schema {
    fn read(
        layout: &Layout<'_>,
        budget: &mut Budget,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Self, Error> {
        let label = |id| layout.metadata.strings.get(&id).map(String::as_str);
        let mut found = None;
        for (id, class) in layout.metadata.classes.iter().enumerate() {
            budget.step(1, proceed)?;
            if let Some(class) = class
                && label(class.name.namespace) == Some("")
                && label(class.name.name) == Some("Camera")
                && found.replace(id as u32).is_some()
            {
                return Err(error("camera trim requires a unique Camera class"));
            }
        }
        let id = found.ok_or_else(|| error("camera trim has no Camera class"))?;
        let class = layout.metadata.class(id)?;
        if class.base != u32::MAX
            || class.dynamic
            || class.size != FIXED
            || class.properties.len() != 4
            || !class.has_pointers
            || class.string_offsets.as_slice() != [0]
            || layout.pointer_layouts.get(id as usize).map(Vec::as_slice) != Some(&[(2, 0)][..])
        {
            return Err(error("camera trim requires the known 40-byte layout"));
        }
        let mut properties = [u32::MAX; 4];
        let mut vector = None;
        for &property in &class.properties {
            budget.step(1, proceed)?;
            let p = layout.metadata.property(property)?;
            let (slot, offset, width, datatype) = match label(p.name) {
                Some("Name") => (0, 0, 4, "StringHandle"),
                Some("Origin") => (1, 4, 12, "PxVec3"),
                Some("Up") => (2, 16, 12, "PxVec3"),
                Some("Target") => (3, 28, 12, "PxVec3"),
                _ => return Err(error("camera trim has an unknown property")),
            };
            let value = layout.metadata.class(p.datatype_id)?;
            let pointers = layout
                .pointer_layouts
                .get(p.datatype_id as usize)
                .ok_or_else(|| error("camera trim has no datatype pointer layout"))?;
            if p.owner != class.name
                || p.kind != 1
                || p.offset32 != offset
                || p.datatype != value.name
                || label(value.name.namespace) != Some("physx3")
                || label(value.name.name) != Some(datatype)
                || value.base != u32::MAX
                || value.dynamic
                || value.size != width
                || (slot == 0 && !value.properties.is_empty())
                || properties[slot] != u32::MAX
                || (slot == 0
                    && (pointers.as_slice() != [(2, 0)]
                        || !value.has_pointers
                        || value.string_offsets.as_slice() != [0]))
                || (slot != 0
                    && (!pointers.is_empty()
                        || value.has_pointers
                        || !value.string_offsets.is_empty()))
            {
                return Err(error("camera trim has an unsupported property layout"));
            }
            properties[slot] = property;
            if slot != 0
                && vector
                    .replace(p.datatype_id)
                    .is_some_and(|old| old != p.datatype_id)
            {
                return Err(error("camera vector properties use different datatypes"));
            }
        }
        let vector = vector.ok_or_else(|| error("camera trim has no vector datatype"))?;
        vector_layout(layout, vector, budget, proceed)?;
        Ok(Self {
            class: id,
            properties,
            vector,
        })
    }

    fn write(
        &self,
        layout: &Layout<'_>,
        fixed: &mut [u8; FIXED],
        update: Update<'_>,
        budget: &mut Budget,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Range<usize>, Error> {
        let Update {
            kind,
            property,
            data,
        } = update;
        budget.step(data.len(), proceed)?;
        if kind == 3 {
            let slot = self
                .properties
                .iter()
                .position(|id| *id == property)
                .ok_or_else(|| error("camera update targets an unknown property"))?;
            let (offset, width) = if slot == 0 {
                (0, 4)
            } else {
                (4 + (slot - 1) * 12, 12)
            };
            if data.len() != width {
                return Err(error("camera property update has an invalid width"));
            }
            fixed[offset..offset + width].copy_from_slice(data);
            return Ok(offset..offset + width);
        }
        let message = layout
            .metadata
            .messages
            .get(property as usize)
            .and_then(Option::as_ref)
            .ok_or_else(|| error("camera update has no property message"))?;
        if kind != 4
            || message.class != self.class
            || message.class_name != layout.metadata.class(self.class)?.name
            || message.size != 36
            || data.len() != 36
            || message.entries.len() != 3
        {
            return Err(error(
                "camera trim requires a complete 36-byte vector message",
            ));
        }
        for (index, entry) in message.entries.iter().enumerate() {
            budget.step(1, proceed)?;
            if entry.property != self.properties[index + 1]
                || entry.datatype_id != self.vector
                || entry.datatype != layout.metadata.class(self.vector)?.name
                || entry.offset != index * 12
                || entry.size != 12
                || entry.destination_size != 12
            {
                return Err(error(
                    "camera vector message has an unsupported member layout",
                ));
            }
        }
        fixed[4..].copy_from_slice(data);
        Ok(4..FIXED)
    }
}

fn vector_layout(
    layout: &Layout<'_>,
    id: u32,
    budget: &mut Budget,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    let vector = layout.metadata.class(id)?;
    if vector.properties.is_empty() {
        return Ok(());
    }
    if vector.properties.len() != 3 {
        return Err(error("camera Vec3 requires x/y/z scalar members"));
    }
    let mut float_id = None;
    for (index, (&property, name)) in vector.properties.iter().zip(["x", "y", "z"]).enumerate() {
        budget.step(1, proceed)?;
        let p = layout.metadata.property(property)?;
        let float = layout.metadata.class(p.datatype_id)?;
        if p.owner != vector.name
            || layout.metadata.strings.get(&p.name).map(String::as_str) != Some(name)
            || p.kind != 1
            || p.offset32 != index * 4
            || p.datatype != float.name
            || p.semantic != 0
            || !p.values.is_empty()
            || layout
                .metadata
                .strings
                .get(&float.name.namespace)
                .map(String::as_str)
                != Some("physx3")
            || layout
                .metadata
                .strings
                .get(&float.name.name)
                .map(String::as_str)
                != Some("PvdF32")
            || float.base != u32::MAX
            || float.size != 4
            || float.dynamic
            || float.has_pointers
            || !float.string_offsets.is_empty()
            || !float.properties.is_empty()
            || layout
                .pointer_layouts
                .get(p.datatype_id as usize)
                .is_none_or(|pointers| !pointers.is_empty())
            || float_id
                .replace(p.datatype_id)
                .is_some_and(|previous| previous != p.datatype_id)
        {
            return Err(error("camera Vec3 has an unsupported scalar member layout"));
        }
    }
    Ok(())
}

fn validate_name(
    layout: &Layout<'_>,
    directory: u32,
    fixed: &[u8; FIXED],
    budget: &mut Budget,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    let handle = u32::from_le_bytes(fixed[..4].try_into()?);
    let name = layout
        .metadata
        .strings
        .get(&handle)
        .ok_or_else(|| error("camera property has an undefined name handle"))?;
    budget.step(name.len(), proceed)?;
    if !name.is_empty() && layout.metadata.strings.get(&directory) != Some(name) {
        return Err(error("camera property name differs from its directory"));
    }
    Ok(())
}

fn fixed_body<'a>(record: &'a Saved<'_>, class: u32) -> Result<&'a [u8; FIXED], Error> {
    if record.class != class {
        return Err(error("camera saved body has a different class"));
    }
    let mut body = Reader::new(&record.body);
    let fixed = body.blob()?;
    body.end()?;
    fixed
        .try_into()
        .map_err(|_| error("camera saved body is not 40 bytes"))
}

#[derive(Clone, Copy)]
enum Mode {
    Prefix,
    SourceMiddle,
    OutputMiddle,
}

struct Scan {
    initial: BTreeMap<u32, [u8; FIXED]>,
    directory: Vec<(u32, u32)>,
    history: blake3::Hash,
}

fn scan(
    layout: &Layout<'_>,
    schema: &Schema,
    range: Range<usize>,
    mode: Mode,
    budget: &mut Budget,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<Scan, Error> {
    if layout.snapshots.len() != 1
        || layout.snapshots[0].event != 0
        || range.start >= range.end
        || range.end > layout.events.len()
    {
        return Err(error(
            "camera trim requires one event-zero snapshot and a valid range",
        ));
    }
    budget.allocate(layout.communication.cameras.len(), 1024)?;
    let mut catalog = BTreeMap::new();
    let mut names = BTreeSet::new();
    for &(name, id) in &layout.communication.cameras {
        budget.step(1, proceed)?;
        let text = layout
            .metadata
            .strings
            .get(&name)
            .ok_or_else(|| error("camera directory has an undefined name"))?;
        let mut digest = blake3::Hasher::new();
        field(&mut digest, text.as_bytes(), budget, proceed)?;
        if !names.insert(*digest.finalize().as_bytes()) || catalog.insert(id, name).is_some() {
            return Err(error("camera directory repeats a name or instance"));
        }
    }
    let mut records = BTreeMap::new();
    let mut deleted = BTreeMap::new();
    for record in &layout.saved {
        budget.step(1, proceed)?;
        if record.class == schema.class && !catalog.contains_key(&record.id) {
            return Err(error("camera saved body is absent from the directory"));
        }
        if catalog.contains_key(&record.id) {
            budget.allocate(1, 128)?;
            if records.insert(record.position, record).is_some() {
                return Err(error("camera saved positions are ambiguous"));
            }
            fixed_body(record, schema.class)?;
            if record.deleted && deleted.insert(record.id, record).is_some() {
                return Err(error("camera deleted bodies repeat a lifecycle"));
            }
        }
    }
    let mut alive = BTreeMap::<u32, [u8; FIXED]>::new();
    let mut seen = BTreeSet::new();
    let mut initial = Reader::new(&layout.snapshot_bytes[0]);
    for _ in 0..initial.count()? {
        budget.step(1, proceed)?;
        let id = initial.u32()?;
        let position = initial.u64()?;
        if let Some(&name) = catalog.get(&id) {
            let record = records
                .get(&position)
                .filter(|record| record.id == id)
                .ok_or_else(|| error("camera initial position has no matching saved body"))?;
            let fixed = *fixed_body(record, schema.class)?;
            validate_name(layout, name, &fixed, budget, proceed)?;
            if alive.insert(id, fixed).is_some() || !seen.insert(id) {
                return Err(error("camera initial directory repeats an instance"));
            }
        }
    }
    initial.end()?;
    let mut original = BTreeMap::new();
    for (&id, &fixed) in &alive {
        budget.step(FIXED, proceed)?;
        original.insert(id, fixed);
    }
    let mut baseline = None;
    let mut retained = BTreeSet::new();
    for (ordinal, event) in layout.events.iter().enumerate() {
        budget.step(1, proceed)?;
        if ordinal == range.start {
            let mut captured = BTreeMap::new();
            for (&id, &fixed) in &alive {
                budget.step(FIXED, proceed)?;
                captured.insert(id, fixed);
                retained.insert(id);
            }
            baseline = Some(captured);
        }
        let id = match event {
            HistoryEvent::Create(id, class) => {
                if *class == schema.class && !catalog.contains_key(id) {
                    return Err(error("camera creation is absent from the directory"));
                }
                *id
            }
            HistoryEvent::Destroy(id)
            | HistoryEvent::Saved { id, .. }
            | HistoryEvent::Set { id, .. }
            | HistoryEvent::Array { id, .. } => *id,
            HistoryEvent::OriginShift(_) if ordinal < range.end => {
                return Err(error("camera trim does not support OriginShift history"));
            }
            _ => continue,
        };
        let Some(&name) = catalog.get(&id) else {
            continue;
        };
        if matches!(mode, Mode::OutputMiddle) && ordinal < range.start {
            return Err(error(
                "camera output contains an invented bootstrap operation",
            ));
        }
        match event {
            HistoryEvent::Create(_, class) => {
                if *class != schema.class {
                    return Err(error("camera trim requires a known instance class"));
                }
                if !seen.insert(id) {
                    return Err(error("unsupported container local ID reuse"));
                }
                validate_name(layout, name, &[0; FIXED], budget, proceed)?;
                alive.insert(id, [0; FIXED]);
                if range.contains(&ordinal) {
                    retained.insert(id);
                }
            }
            HistoryEvent::Destroy(_) => {
                let captured = alive
                    .remove(&id)
                    .ok_or_else(|| error("camera destruction targets a missing instance"))?;
                let record = deleted
                    .get(&id)
                    .ok_or_else(|| error("camera destruction has no deleted saved body"))?;
                if fixed_body(record, schema.class)? != &captured {
                    return Err(error(
                        "camera deleted body differs from preceding raw state",
                    ));
                }
            }
            HistoryEvent::Set {
                kind,
                property,
                data,
                undo,
                ..
            } => {
                let fixed = alive
                    .get_mut(&id)
                    .ok_or_else(|| error("camera update targets a missing instance"))?;
                let before = *fixed;
                let affected = schema.write(
                    layout,
                    fixed,
                    Update {
                        kind: *kind,
                        property: *property,
                        data,
                    },
                    budget,
                    proceed,
                )?;
                UndoCheck {
                    layout,
                    schema,
                    original: &original,
                }
                .verify(
                    UndoEvent {
                        ordinal,
                        id,
                        affected,
                        before,
                        after: *fixed,
                        words: undo,
                    },
                    budget,
                    proceed,
                )?;
                validate_name(layout, name, fixed, budget, proceed)?;
            }
            HistoryEvent::Saved { position, .. } => {
                let record = records
                    .get(position)
                    .filter(|record| record.id == id)
                    .ok_or_else(|| error("camera saved event has no matching body"))?;
                let fixed = fixed_body(record, schema.class)?;
                if alive.get(&id) != Some(fixed) {
                    return Err(error(
                        "camera saved body differs from its captured raw state",
                    ));
                }
            }
            _ => return Err(error("camera trim does not support this history operation")),
        }
    }
    if seen.len() != catalog.len() {
        return Err(error("camera directory targets an uncaptured lifecycle"));
    }
    let baseline = baseline.ok_or_else(|| error("camera cut is outside captured history"))?;
    let mut directory = Vec::new();
    directory.try_reserve_exact(retained.len())?;
    budget.allocate(directory.capacity().saturating_sub(retained.len()), 8)?;
    for &row in &layout.communication.cameras {
        budget.step(1, proceed)?;
        if retained.contains(&row.1) {
            directory.push(row);
        }
    }
    let history = History {
        layout,
        schema,
        catalog: &catalog,
        records: &records,
    }
    .digest(range, mode, budget, proceed)?;
    Ok(Scan {
        initial: baseline,
        directory,
        history,
    })
}

fn field(
    hash: &mut blake3::Hasher,
    data: &[u8],
    budget: &mut Budget,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    hash.update(&(data.len() as u64).to_le_bytes());
    for chunk in data.chunks(64 << 10) {
        budget.step(chunk.len(), proceed)?;
        hash.update(chunk);
    }
    Ok(())
}

struct History<'a, 'b> {
    layout: &'a Layout<'b>,
    schema: &'a Schema,
    catalog: &'a BTreeMap<u32, u32>,
    records: &'a BTreeMap<u64, &'a Saved<'b>>,
}

impl History<'_, '_> {
    fn digest(
        &self,
        range: Range<usize>,
        mode: Mode,
        budget: &mut Budget,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<blake3::Hash, Error> {
        let Self {
            layout,
            schema,
            catalog,
            records,
        } = self;
        let mut hash = blake3::Hasher::new();
        field(&mut hash, b"camera-history-v2", budget, proceed)?;
        self.sections(&mut hash, range.clone(), budget, proceed)?;
        let mut count = 0u64;
        for ordinal in range.clone() {
            budget.step(1, proceed)?;
            let event = &layout.events[ordinal];
            if let HistoryEvent::Section {
                class,
                source,
                ticks,
                name,
                begin,
                padding,
            } = event
            {
                field(&mut hash, b"section", budget, proceed)?;
                field(
                    &mut hash,
                    &((ordinal - range.start) as u64).to_le_bytes(),
                    budget,
                    proceed,
                )?;
                field(&mut hash, &class.to_le_bytes(), budget, proceed)?;
                field(&mut hash, &source.to_le_bytes(), budget, proceed)?;
                field(&mut hash, &ticks.to_le_bytes(), budget, proceed)?;
                field(&mut hash, &name.to_le_bytes(), budget, proceed)?;
                field(&mut hash, &[*begin], budget, proceed)?;
                field(&mut hash, padding, budget, proceed)?;
                continue;
            }
            let (kind, id) = match event {
                HistoryEvent::Create(id, _) => (1, *id),
                HistoryEvent::Destroy(id) => (2, *id),
                HistoryEvent::Set { kind, id, .. } => (*kind, *id),
                HistoryEvent::Saved { id, .. } => (10, *id),
                _ => continue,
            };
            if !catalog.contains_key(&id) {
                continue;
            }
            count += 1;
            field(&mut hash, b"camera", budget, proceed)?;
            field(
                &mut hash,
                &((ordinal - range.start) as u64).to_le_bytes(),
                budget,
                proceed,
            )?;
            field(&mut hash, &kind.to_le_bytes(), budget, proceed)?;
            field(&mut hash, &id.to_le_bytes(), budget, proceed)?;
            match event {
                HistoryEvent::Create(_, class) => {
                    field(&mut hash, &class.to_le_bytes(), budget, proceed)?
                }
                HistoryEvent::Set {
                    property,
                    data,
                    undo,
                    ..
                } => {
                    field(&mut hash, &property.to_le_bytes(), budget, proceed)?;
                    field(&mut hash, data, budget, proceed)?;
                    undo_digest(&mut hash, ordinal, range.start, undo, mode, budget, proceed)?;
                }
                HistoryEvent::Saved { position, .. } => {
                    let record = records
                        .get(position)
                        .ok_or_else(|| error("camera history has no saved body"))?;
                    let fixed = fixed_body(record, schema.class)?;
                    field(&mut hash, fixed, budget, proceed)?;
                }
                _ => {}
            }
        }
        field(&mut hash, &count.to_le_bytes(), budget, proceed)?;
        Ok(hash.finalize())
    }

    fn sections(
        &self,
        hash: &mut blake3::Hasher,
        range: Range<usize>,
        budget: &mut Budget,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        // 相机不隶属单一 Scene；证明绑定全局帧边界并保留原始时间信息。
        field(hash, b"frames", budget, proceed)?;
        for value in self.layout.communication.clock {
            field(hash, &value.to_le_bytes(), budget, proceed)?;
        }
        let mut sections = 0u64;
        for section in &self.layout.communication.sections {
            budget.step(1, proceed)?;
            let mut rows = 0u64;
            for &[begin_ticks, begin, end_ticks, end] in &section.rows {
                budget.step(32, proceed)?;
                if end < range.start as u64 || begin >= range.end as u64 {
                    continue;
                }
                if begin < range.start as u64 || end >= range.end as u64 || begin >= end {
                    return Err(error("camera interval contains an incomplete Scene frame"));
                }
                if rows == 0 {
                    field(hash, &section.source.to_le_bytes(), budget, proceed)?;
                    field(hash, &section.name.to_le_bytes(), budget, proceed)?;
                    sections += 1;
                }
                for value in [
                    begin_ticks,
                    begin - range.start as u64,
                    end_ticks,
                    end - range.start as u64,
                ] {
                    field(hash, &value.to_le_bytes(), budget, proceed)?;
                }
                rows += 1;
            }
            if rows != 0 {
                field(hash, &rows.to_le_bytes(), budget, proceed)?;
            }
        }
        field(hash, &sections.to_le_bytes(), budget, proceed)
    }
}

struct UndoCheck<'a, 'b> {
    layout: &'a Layout<'b>,
    schema: &'a Schema,
    original: &'a BTreeMap<u32, [u8; FIXED]>,
}

struct UndoEvent<'a> {
    ordinal: usize,
    id: u32,
    affected: Range<usize>,
    before: [u8; FIXED],
    after: [u8; FIXED],
    words: &'a [u32],
}

impl UndoCheck<'_, '_> {
    fn verify(
        &self,
        event: UndoEvent<'_>,
        budget: &mut Budget,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let UndoEvent {
            ordinal,
            id,
            affected,
            before,
            after,
            words,
        } = event;
        let mut restored = after;
        let mut previous = None;
        let implicit = [u32_len(ordinal)?];
        let words = if words.is_empty() {
            &implicit[..]
        } else {
            words
        };
        for &distance in words {
            budget.step(1, proceed)?;
            let target = undo_target(ordinal, distance, &mut previous)?;
            let mut value = restored;
            let supplied = match &self.layout.events[target] {
                HistoryEvent::Marker if target == 0 => {
                    value = *self
                        .original
                        .get(&id)
                        .ok_or_else(|| error("camera undo marker has no initial instance"))?;
                    0..FIXED
                }
                HistoryEvent::Create(old_id, class)
                    if *old_id == id && *class == self.schema.class =>
                {
                    value = [0; FIXED];
                    0..FIXED
                }
                HistoryEvent::Set {
                    kind,
                    id: old_id,
                    property,
                    data,
                    ..
                } if *old_id == id => {
                    let supplied = self.schema.write(
                        self.layout,
                        &mut value,
                        Update {
                            kind: *kind,
                            property: *property,
                            data,
                        },
                        budget,
                        proceed,
                    )?;
                    if *kind == 3
                        && (supplied.start < affected.start || supplied.end > affected.end)
                    {
                        return Err(error("camera undo scalar targets another property"));
                    }
                    supplied
                }
                _ => return Err(error("camera undo targets an unrelated operation")),
            };
            let begin = affected.start.max(supplied.start);
            let end = affected.end.min(supplied.end);
            if begin < end {
                restored[begin..end].copy_from_slice(&value[begin..end]);
            }
        }
        if restored[affected.clone()] != before[affected] {
            return Err(error("camera source undo differs from preceding raw state"));
        }
        Ok(())
    }
}

fn undo_target(
    ordinal: usize,
    distance: u32,
    previous: &mut Option<usize>,
) -> Result<usize, Error> {
    let target = ordinal
        .checked_sub(distance as usize)
        .filter(|target| *target < ordinal)
        .ok_or_else(|| error("camera undo target must strictly precede its event"))?;
    if previous.is_some_and(|previous| target <= previous) {
        return Err(error("camera undo targets must be ordered and distinct"));
    }
    *previous = Some(target);
    Ok(target)
}

fn undo_digest(
    hash: &mut blake3::Hasher,
    ordinal: usize,
    start: usize,
    undo: &[u32],
    mode: Mode,
    budget: &mut Budget,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    if matches!(mode, Mode::Prefix) {
        let mut previous = None;
        field(hash, &(undo.len() as u64).to_le_bytes(), budget, proceed)?;
        for &word in undo {
            undo_target(ordinal, word, &mut previous)?;
            field(hash, &word.to_le_bytes(), budget, proceed)?;
        }
        return Ok(());
    }
    let mut previous = None;
    let mut baseline = undo.is_empty() && matches!(mode, Mode::SourceMiddle);
    let mut retained = 0u64;
    for &distance in undo {
        budget.step(1, proceed)?;
        let target = undo_target(ordinal, distance, &mut previous)?;
        if target < start {
            if matches!(mode, Mode::OutputMiddle) && target != 0 {
                return Err(error("camera undo cannot target an analysis bootstrap"));
            }
            baseline = true;
        } else {
            retained += 1;
            field(
                hash,
                &((target - start) as u64).to_le_bytes(),
                budget,
                proceed,
            )?;
        }
    }
    field(hash, &[u8::from(baseline)], budget, proceed)?;
    field(hash, &retained.to_le_bytes(), budget, proceed)
}

pub(super) struct Scope {
    source: (usize, usize),
    first: usize,
    last: usize,
    range: Range<usize>,
    schema: Schema,
    captured: Scan,
    metadata: blake3::Hash,
    reserved: u32,
    work: usize,
}

pub(super) struct Permit<'a> {
    scope: &'a Scope,
    layout: usize,
    source: (usize, usize),
    range: Range<usize>,
}

impl Permit<'_> {
    pub(super) fn bytes(&self) -> usize {
        self.scope.bytes()
    }

    pub(super) fn prefix(&self, layout: &Layout<'_>, end: usize) -> bool {
        self.layout == std::ptr::from_ref(layout) as usize
            && self.source == (layout.source.as_ptr() as usize, layout.source.len())
            && self.range.end == end
    }

    pub(super) fn middle(&self, layout: &Layout<'_>, start: usize, end: usize) -> bool {
        self.prefix(layout, end) && self.range.start == start
    }
}

impl Scope {
    pub(super) fn prepare(
        layout: &Layout<'_>,
        first: usize,
        last: usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Option<Self>, Error> {
        Self::prepare_bounded(layout, first, last, MAX_METADATA, WORK_LIMIT, proceed)
    }

    pub(super) fn prepare_bounded(
        layout: &Layout<'_>,
        first: usize,
        last: usize,
        memory_limit: usize,
        work_limit: usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Option<Self>, Error> {
        if layout.communication.cameras.is_empty() {
            return Ok(None);
        }
        if first > last {
            return Err(error("first frame exceeds last frame"));
        }
        let range = if first == 0 {
            0..layout.prefix_cut(last, proceed)?
        } else {
            let interval = layout.middle_frame_interval(first, last, proceed)?;
            interval.start..interval.end
        };
        let mut budget = Budget {
            bytes: std::mem::size_of::<Self>(),
            limit: memory_limit,
            work: work_limit,
        };
        budget.allocate(0, 1)?;
        let schema = Schema::read(layout, &mut budget, proceed)?;
        let captured = scan(
            layout,
            &schema,
            range.clone(),
            if first == 0 {
                Mode::Prefix
            } else {
                Mode::SourceMiddle
            },
            &mut budget,
            proceed,
        )?;
        let mut metadata = blake3::Hasher::new();
        field(&mut metadata, layout.object_bytes, &mut budget, proceed)?;
        let result = Self {
            source: (layout.source.as_ptr() as usize, layout.source.len()),
            first,
            last,
            range,
            schema,
            captured,
            metadata: metadata.finalize(),
            reserved: layout.snapshots[0].reserved,
            work: budget.work,
        };
        let mut initial_bytes = result.bytes();
        metadata_budget(
            &mut initial_bytes,
            Reader::new(&layout.snapshot_bytes[0]).count()?,
            std::mem::size_of::<initial::Entry>() + 64,
        )?;
        metadata_budget(&mut initial_bytes, layout.saved.len(), 64)?;
        if initial_bytes > memory_limit {
            return Err(error("camera initial indices exceed their memory budget"));
        }
        Ok(Some(result))
    }

    pub(super) fn permit<'a>(&'a self, layout: &'a Layout<'_>) -> Result<Permit<'a>, Error> {
        if self.source != (layout.source.as_ptr() as usize, layout.source.len()) {
            return Err(error("camera scope belongs to a different source"));
        }
        Ok(Permit {
            scope: self,
            layout: std::ptr::from_ref(layout) as usize,
            source: self.source,
            range: self.range.clone(),
        })
    }

    pub(super) fn verify_with_permit<'a>(
        &'a self,
        layout: &'a Layout<'_>,
        leading: usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Permit<'a>, Error> {
        self.verify_layout(layout, leading, proceed)?;
        Ok(Permit {
            scope: self,
            layout: std::ptr::from_ref(layout) as usize,
            source: (layout.source.as_ptr() as usize, layout.source.len()),
            range: leading..layout.events.len(),
        })
    }

    pub(super) fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.captured.initial.len() * 128
            + self.captured.directory.capacity() * 8
    }

    pub(super) fn authorizes(&self, layout: &Layout<'_>, first: usize, last: usize) -> bool {
        self.source == (layout.source.as_ptr() as usize, layout.source.len())
            && self.first == first
            && self.last == last
    }

    pub(super) fn prefix_end(&self, layout: &Layout<'_>, cut: usize) -> bool {
        self.source == (layout.source.as_ptr() as usize, layout.source.len())
            && self.range.end == cut
    }

    pub(super) fn verify_layout(
        &self,
        layout: &Layout<'_>,
        leading: usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        if layout.communication.cameras != self.captured.directory
            || layout.events.len() != self.range.len() + leading
            || layout.snapshots.len() != 1
            || layout.snapshots[0].reserved != self.reserved
            || (self.first == 0 && leading != 0)
            || (self.first != 0
                && (leading == 0
                    || !matches!(layout.events.first(), Some(HistoryEvent::Marker))
                    || layout
                        .communication
                        .sections
                        .iter()
                        .flat_map(|section| &section.rows)
                        .map(|row| row[1])
                        .min()
                        != Some(leading as u64)))
        {
            return Err(error("camera trim changed its directory or event interval"));
        }
        let mut budget = Budget {
            bytes: self.bytes(),
            limit: MAX_METADATA,
            work: self.work,
        };
        budget.allocate(0, 1)?;
        let mut metadata = blake3::Hasher::new();
        field(&mut metadata, layout.object_bytes, &mut budget, proceed)?;
        if metadata.finalize() != self.metadata {
            return Err(error("camera trim changed its captured metadata"));
        }
        let actual = scan(
            layout,
            &self.schema,
            leading..layout.events.len(),
            if self.first == 0 {
                Mode::Prefix
            } else {
                Mode::OutputMiddle
            },
            &mut budget,
            proceed,
        )?;
        if actual.initial != self.captured.initial || actual.history != self.captured.history {
            return Err(error(
                "camera trim changed captured baseline or history bytes",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_undo_digest_distinguishes_the_marker_from_analysis_seeds() {
        let mut budget = Budget {
            bytes: 0,
            limit: MAX_METADATA,
            work: WORK_LIMIT,
        };
        let mut expected = blake3::Hasher::new();
        undo_digest(
            &mut expected,
            10,
            8,
            &[7],
            Mode::SourceMiddle,
            &mut budget,
            &mut || true,
        )
        .unwrap();
        let mut actual = blake3::Hasher::new();
        undo_digest(
            &mut actual,
            4,
            2,
            &[4],
            Mode::OutputMiddle,
            &mut budget,
            &mut || true,
        )
        .unwrap();
        assert_eq!(actual.finalize(), expected.finalize());
        let mut rejected = blake3::Hasher::new();
        assert!(
            undo_digest(
                &mut rejected,
                4,
                2,
                &[3],
                Mode::OutputMiddle,
                &mut budget,
                &mut || true
            )
            .unwrap_err()
            .to_string()
            .contains("analysis bootstrap")
        );
        assert!(
            undo_digest(
                &mut rejected,
                11,
                0,
                &[3, 7],
                Mode::Prefix,
                &mut budget,
                &mut || true
            )
            .unwrap_err()
            .to_string()
            .contains("ordered and distinct")
        );
    }
}
