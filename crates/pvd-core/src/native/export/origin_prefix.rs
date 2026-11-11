use super::*;

const WORK_LIMIT: usize = 1 << 30;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Scene,
    Static,
    Dynamic,
    Other,
}

#[derive(Clone, Copy)]
enum Field {
    Members(Kind),
    Scene,
    Shift(usize),
}

struct Schema {
    kind: Kind,
    properties: Vec<u32>,
    fields: BTreeMap<u32, Field>,
}

struct Instance {
    class: u32,
    scene: u32,
    members: BTreeMap<u32, Vec<u32>>,
}

struct Budget {
    memory: usize,
    work: usize,
    limit: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            memory: 0,
            work: WORK_LIMIT,
            limit: MAX_METADATA,
        }
    }
}

impl Budget {
    fn allocate(&mut self, bytes: usize) -> Result<(), Error> {
        self.memory = self
            .memory
            .checked_add(bytes)
            .filter(|&bytes| bytes <= self.limit)
            .ok_or_else(|| error("OriginShift prefix exceeds its metadata budget"))?;
        Ok(())
    }

    fn scan(&mut self, bytes: usize, proceed: &mut dyn FnMut() -> bool) -> Result<(), Error> {
        check(proceed)?;
        self.work = self
            .work
            .checked_sub(bytes)
            .ok_or_else(|| error("OriginShift prefix exceeds its verification work budget"))?;
        Ok(())
    }
}

fn label(metadata: &Metadata, name: u32) -> Result<&str, Error> {
    metadata
        .strings
        .get(&name)
        .map(String::as_str)
        .ok_or_else(|| error("undefined OriginShift prefix metadata string"))
}

fn kind(metadata: &Metadata, class: u32) -> Result<Kind, Error> {
    let class = metadata.class(class)?;
    if label(metadata, class.name.namespace)? != "physx3" {
        return Ok(Kind::Other);
    }
    Ok(match label(metadata, class.name.name)? {
        "PxScene" => Kind::Scene,
        "PxRigidStatic" => Kind::Static,
        "PxRigidDynamic" => Kind::Dynamic,
        _ => Kind::Other,
    })
}

fn schema(
    layout: &Layout<'_>,
    class: u32,
    budget: &mut Budget,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<Schema, Error> {
    let metadata = &layout.metadata;
    let concrete = metadata.class(class)?;
    budget.allocate(128)?;
    let mut result = Schema {
        kind: kind(metadata, class)?,
        properties: Vec::new(),
        fields: BTreeMap::new(),
    };
    if result.kind == Kind::Other {
        return Ok(result);
    }
    let mut ancestors = [u32::MAX; 128];
    let mut depth = 0;
    let mut current = class;
    let mut ranges = Vec::new();
    while current != u32::MAX {
        budget.scan(32, proceed)?;
        if depth == ancestors.len() || ancestors[..depth].contains(&current) {
            return Err(error("invalid OriginShift prefix class inheritance"));
        }
        ancestors[depth] = current;
        depth += 1;
        let owner = metadata.class(current)?;
        for &id in &owner.properties {
            budget.scan(32 + result.properties.len() * 8, proceed)?;
            let property = metadata.property(id)?;
            let datatype = metadata.class(property.datatype_id)?;
            let property_name = label(metadata, property.name)?;
            if property.owner != owner.name || property.datatype != datatype.name {
                return Err(error(
                    "OriginShift prefix property identity is inconsistent",
                ));
            }
            for &previous in &result.properties {
                let previous_name = label(metadata, metadata.property(previous)?.name)?;
                budget.scan(8 + property_name.len().max(previous_name.len()), proceed)?;
                if previous == id || previous_name == property_name {
                    return Err(error("duplicate OriginShift prefix property"));
                }
            }
            let width = if property.kind == 2 {
                12
            } else {
                datatype.size
            };
            let end = property
                .offset32
                .checked_add(width)
                .filter(|&end| end <= concrete.size)
                .ok_or_else(|| error("OriginShift prefix property exceeds fixed body"))?;
            if datatype.dynamic
                || width == 0
                || ranges
                    .iter()
                    .any(|&(start, previous_end)| property.offset32 < previous_end && start < end)
            {
                return Err(error("unsupported OriginShift prefix property layout"));
            }
            budget.allocate(20)?;
            result.properties.try_reserve_exact(1)?;
            result.properties.push(id);
            ranges.try_reserve_exact(1)?;
            ranges.push((property.offset32, end));
            let field = match (result.kind, property_name) {
                (Kind::Scene, "RigidStatics") => Some(Field::Members(Kind::Static)),
                (Kind::Scene, "RigidDynamics") => Some(Field::Members(Kind::Dynamic)),
                (
                    Kind::Scene,
                    "Articulations" | "Cloths" | "ParticleSystems" | "ParticleFluids",
                ) => Some(Field::Members(Kind::Other)),
                (Kind::Static | Kind::Dynamic, "Scene") => Some(Field::Scene),
                (Kind::Static | Kind::Dynamic, "GlobalPose") => Some(Field::Shift(28)),
                (Kind::Static | Kind::Dynamic, "WorldBounds") => Some(Field::Shift(24)),
                (Kind::Dynamic, "KinematicTarget") => Some(Field::Shift(28)),
                (Kind::Static, "KinematicTarget")
                | (Kind::Static | Kind::Dynamic, "ProjectionPlane") => {
                    return Err(error("unsupported OriginShift prefix actor property"));
                }
                _ => None,
            };
            let Some(field) = field else { continue };
            let datatype_name = match field {
                Field::Members(_) | Field::Scene => "ObjectRef",
                Field::Shift(24) => "PxBounds3",
                Field::Shift(_) => "PxTransform",
            };
            let field_width = match field {
                Field::Members(_) | Field::Scene => 4,
                Field::Shift(width) => width,
            };
            let pointers = layout
                .pointer_layouts
                .get(property.datatype_id as usize)
                .ok_or_else(|| error("undefined OriginShift prefix datatype pointers"))?;
            let reference = matches!(field, Field::Members(_) | Field::Scene);
            if label(metadata, datatype.name.namespace)? != "physx3"
                || label(metadata, datatype.name.name)? != datatype_name
                || datatype.size != field_width
                || !datatype.string_offsets.is_empty()
                || datatype.base != u32::MAX
                || property.kind
                    != if matches!(field, Field::Members(_)) {
                        2
                    } else {
                        1
                    }
                || if reference {
                    !datatype.has_pointers
                        || pointers.as_slice() != [(1, 0)]
                        || !datatype.properties.is_empty()
                } else {
                    datatype.has_pointers
                        || !pointers.is_empty()
                        || !origin_shift::fixed_layout(
                            metadata,
                            property.datatype_id,
                            &mut |bytes| budget.scan(bytes, proceed),
                        )?
                }
            {
                return Err(error("unsupported OriginShift prefix field layout"));
            }
            budget.allocate(64)?;
            result.fields.insert(id, field);
        }
        current = owner.base;
    }
    budget.memory -= ranges.len() * 16;
    Ok(result)
}

fn references(
    bytes: &[u8],
    budget: &mut Budget,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<Vec<u32>, Error> {
    if !bytes.len().is_multiple_of(4) {
        return Err(error(
            "OriginShift prefix reference array has invalid width",
        ));
    }
    budget.allocate(bytes.len())?;
    let mut result = Vec::new();
    result.try_reserve_exact(bytes.len() / 4)?;
    for bytes in bytes.chunks_exact(4) {
        budget.scan(4, proceed)?;
        result.push(u32::from_le_bytes(bytes.try_into()?));
    }
    Ok(result)
}

fn set(
    instance: &mut Instance,
    schema: &Schema,
    property: u32,
    bytes: &[u8],
    budget: &mut Budget,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    if schema.kind == Kind::Other {
        return Ok(());
    }
    budget.scan(schema.properties.len() * 4, proceed)?;
    if !schema.properties.contains(&property) {
        return Err(error(
            "OriginShift prefix property does not belong to instance",
        ));
    }
    match schema.fields.get(&property) {
        Some(Field::Members(_)) => {
            let values = references(bytes, budget, proceed)?;
            let previous = instance
                .members
                .insert(property, values)
                .ok_or_else(|| error("missing OriginShift prefix reference field"))?;
            budget.memory -= previous.capacity() * 4;
        }
        Some(Field::Scene) if bytes.len() == 4 => {
            instance.scene = u32::from_le_bytes(bytes.try_into()?);
        }
        Some(Field::Shift(width)) if bytes.len() == *width => {}
        None => {}
        _ => return Err(error("OriginShift prefix field value has invalid width")),
    }
    Ok(())
}

fn verify_shift(
    event: &origin_shift::Event<'_>,
    schemas: &BTreeMap<u32, Schema>,
    instances: &BTreeMap<u32, Instance>,
    budget: &mut Budget,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    instances
        .get(&event.scene())
        .filter(|instance| schemas[&instance.class].kind == Kind::Scene)
        .ok_or_else(|| error("OriginShift prefix targets missing or unsupported Scene"))?;
    let mut ownership = BTreeMap::new();
    let mut expected = BTreeSet::new();
    for (&id, instance) in instances {
        budget.scan(16, proceed)?;
        let schema = &schemas[&instance.class];
        if schema.kind != Kind::Scene {
            continue;
        }
        for (&property, members) in &instance.members {
            let Field::Members(member_kind) = schema.fields[&property] else {
                return Err(error("invalid OriginShift prefix member field"));
            };
            if member_kind == Kind::Other && !members.is_empty() {
                return Err(error(
                    "OriginShift prefix only supports rigid Scene members",
                ));
            }
            for &member in members {
                budget.scan(16, proceed)?;
                let actor = instances
                    .get(&member)
                    .ok_or_else(|| error("OriginShift prefix Scene contains missing Actor"))?;
                let actor_schema = &schemas[&actor.class];
                if member == 0 || actor_schema.kind != member_kind || actor.scene != id {
                    return Err(error("OriginShift prefix Actor Scene membership disagrees"));
                }
                if ownership.contains_key(&member) {
                    return Err(error("duplicate OriginShift prefix Actor membership"));
                }
                budget.allocate(64)?;
                ownership.insert(member, id);
                if id == event.scene() {
                    for (&property, field) in &actor_schema.fields {
                        budget.scan(8, proceed)?;
                        if matches!(field, Field::Shift(_)) {
                            budget.allocate(64)?;
                            expected.insert((member, property));
                        }
                    }
                }
            }
        }
    }
    for (&id, instance) in instances {
        budget.scan(8, proceed)?;
        if matches!(schemas[&instance.class].kind, Kind::Static | Kind::Dynamic)
            && instance.scene != 0
            && ownership.get(&id) != Some(&instance.scene)
        {
            return Err(error("OriginShift prefix Actor is absent from its Scene"));
        }
    }
    let expected_count = expected.len();
    if expected_count == 0 {
        return Err(error(
            "OriginShift prefix requires captured rigid properties",
        ));
    }
    for row in event.properties().chunks_exact(8) {
        budget.scan(8, proceed)?;
        let pair = (
            u32::from_le_bytes(row[..4].try_into()?),
            u32::from_le_bytes(row[4..].try_into()?),
        );
        if !expected.remove(&pair) {
            return Err(error("OriginShift prefix auxiliary property set disagrees"));
        }
    }
    if !expected.is_empty() {
        return Err(error(
            "OriginShift prefix auxiliary property set is incomplete",
        ));
    }
    budget.memory -= (ownership.len() + expected_count) * 64;
    Ok(())
}

pub(super) fn verify_prefix_events(
    layout: &Layout<'_>,
    cut: usize,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<bool, Error> {
    verify_with_budget(layout, cut, &mut Budget::default(), proceed)
}

pub(super) fn contains_shift(
    layout: &Layout<'_>,
    cut: usize,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<bool, Error> {
    let mut shifted = false;
    for event in layout
        .events
        .get(..cut)
        .ok_or_else(|| error("OriginShift prefix cut exceeds history"))?
    {
        check(proceed)?;
        shifted |= matches!(event, HistoryEvent::OriginShift(_));
    }
    Ok(shifted)
}

fn verify_with_budget(
    layout: &Layout<'_>,
    cut: usize,
    budget: &mut Budget,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<bool, Error> {
    let events = layout
        .events
        .get(..cut)
        .ok_or_else(|| error("OriginShift prefix cut exceeds history"))?;
    let mut shifted = false;
    for event in events {
        budget.scan(4, proceed)?;
        shifted |= matches!(event, HistoryEvent::OriginShift(_));
    }
    if !shifted {
        return Ok(false);
    }
    if layout.snapshots.len() != 1
        || layout.snapshots[0].event != 0
        || layout.snapshot_bytes.len() != 1
        || layout.snapshot_bytes[0].as_ref() != [0; 4]
    {
        return Err(error(
            "OriginShift prefix requires one empty event-zero snapshot",
        ));
    }
    let mut schemas = BTreeMap::new();
    let mut instances = BTreeMap::new();
    let mut created = BTreeSet::new();
    for event in events {
        budget.scan(16, proceed)?;
        match event {
            HistoryEvent::Create(id, class) => {
                if *id == 0 || i32::try_from(*id).is_err() || created.contains(id) {
                    return Err(error(
                        "OriginShift prefix does not support instance ID reuse",
                    ));
                }
                budget.allocate(64)?;
                created.insert(*id);
                if let std::collections::btree_map::Entry::Vacant(entry) = schemas.entry(*class) {
                    entry.insert(schema(layout, *class, budget, proceed)?);
                }
                // Native Create zeroes reference fields and array headers. Only
                // reference topology is needed while fixed values stay in source bytes.
                let mut instance = Instance {
                    class: *class,
                    scene: 0,
                    members: BTreeMap::new(),
                };
                budget.allocate(128)?;
                for (&property, field) in &schemas[class].fields {
                    budget.scan(8, proceed)?;
                    if matches!(field, Field::Members(_)) {
                        budget.allocate(64)?;
                        instance.members.insert(property, Vec::new());
                    }
                }
                instances.insert(*id, instance);
            }
            HistoryEvent::Destroy(id) => {
                let instance = instances
                    .remove(id)
                    .ok_or_else(|| error("OriginShift prefix destroys missing instance"))?;
                budget.memory -= 128
                    + instance.members.len() * 64
                    + instance
                        .members
                        .values()
                        .map(|v| v.capacity() * 4)
                        .sum::<usize>();
            }
            HistoryEvent::Set {
                kind,
                id,
                property,
                data,
                ..
            } => {
                let instance = instances
                    .get_mut(id)
                    .ok_or_else(|| error("OriginShift prefix writes missing instance"))?;
                let schema = &schemas[&instance.class];
                if *kind == 3 {
                    set(instance, schema, *property, data, budget, proceed)?;
                } else {
                    middle::validate_message_instance(
                        &layout.metadata,
                        *property,
                        instance.class,
                        proceed,
                    )?;
                    middle::validate_message(&layout.metadata, *property, data, proceed)?;
                    let message = layout.metadata.messages[*property as usize]
                        .as_ref()
                        .ok_or_else(|| error("undefined OriginShift prefix message"))?;
                    for entry in &message.entries {
                        budget.scan(16, proceed)?;
                        let end = entry
                            .offset
                            .checked_add(entry.size)
                            .ok_or_else(|| error("OriginShift prefix message offset overflow"))?;
                        let bytes = data
                            .get(entry.offset..end)
                            .ok_or_else(|| error("OriginShift prefix message is truncated"))?;
                        set(instance, schema, entry.property, bytes, budget, proceed)?;
                    }
                }
            }
            HistoryEvent::Array {
                kind,
                id,
                property,
                index,
                data,
                ..
            } => {
                let instance = instances
                    .get_mut(id)
                    .ok_or_else(|| error("OriginShift prefix array targets missing instance"))?;
                let schema = &schemas[&instance.class];
                if schema.kind == Kind::Other {
                    continue;
                }
                budget.scan(schema.properties.len() * 4, proceed)?;
                if !schema.properties.contains(property) {
                    return Err(error(
                        "OriginShift prefix array does not belong to instance",
                    ));
                }
                let Some(values) = instance.members.get_mut(property) else {
                    continue;
                };
                let index = usize::try_from(*index)?;
                let inserted = match (*kind, data) {
                    (5 | 7, Some(data)) if data.len() == 4 => {
                        Some(u32::from_le_bytes((*data).try_into()?))
                    }
                    (6, None) => None,
                    _ => return Err(error("OriginShift prefix array element has invalid width")),
                };
                if index > values.len() || (*kind != 5 && index == values.len()) {
                    return Err(error("OriginShift prefix array index exceeds membership"));
                }
                let length = match kind {
                    5 => values
                        .len()
                        .checked_add(1)
                        .ok_or_else(|| error("OriginShift prefix array length overflow"))?,
                    6 => values.len() - 1,
                    _ => values.len(),
                };
                budget.allocate(length * 4)?;
                let mut replacement = Vec::new();
                replacement.try_reserve_exact(length)?;
                for (at, &value) in values.iter().enumerate() {
                    budget.scan(4, proceed)?;
                    if at == index && *kind == 5 {
                        replacement.push(inserted.ok_or_else(|| {
                            error("missing OriginShift prefix inserted reference")
                        })?);
                    }
                    if at != index || *kind == 5 {
                        replacement.push(value);
                    } else if let Some(value) = inserted {
                        replacement.push(value);
                    }
                }
                if *kind == 5 && index == values.len() {
                    replacement.push(
                        inserted.ok_or_else(|| {
                            error("missing OriginShift prefix appended reference")
                        })?,
                    );
                }
                budget.memory -= values.capacity() * 4;
                *values = replacement;
            }
            HistoryEvent::OriginShift(event) => {
                verify_shift(event, &schemas, &instances, budget, proceed)?;
            }
            HistoryEvent::Saved { .. } => {
                return Err(error(
                    "OriginShift prefix does not support retained saved instances",
                ));
            }
            _ => {}
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests;
