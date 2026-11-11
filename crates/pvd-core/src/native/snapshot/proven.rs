use super::*;
use read_source::{EventPage, Source};
use std::io::{Read, Seek};
use std::ops::Range;

pub(in crate::native) struct InitialProperties {
    classes: BTreeMap<String, BTreeMap<u32, Vec<u8>>>,
}

impl InitialProperties {
    pub(in crate::native) fn restore_instance(
        &self,
        metadata: &Metadata,
        builder: &mut RecordingBuilder,
        id: u32,
        progress: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let object = builder
            .state
            .objects
            .get(&u64::from(id))
            .ok_or_else(|| error("PVD3 initial properties target missing instance"))?;
        let Some(fields) = self.classes.get(&object.type_name) else {
            return Ok(());
        };
        for (&property, data) in fields {
            if !progress() {
                return Err(cancelled());
            }
            super::restore_property(metadata, builder, id, property, data, progress)?;
        }
        Ok(())
    }
}

pub(in crate::native) fn initial_properties(
    metadata: &Metadata,
    progress: &mut dyn FnMut() -> bool,
) -> Result<InitialProperties, Error> {
    let mut classes = BTreeMap::new();
    let mut allocation = 0;
    for (id, class) in metadata.classes.iter().enumerate() {
        if !progress() {
            return Err(cancelled());
        }
        let Some(class) = class else { continue };
        let Some(properties) = property_ids(metadata, id as u32, progress)? else {
            continue;
        };
        let namespace = metadata
            .strings
            .get(&class.name.namespace)
            .ok_or_else(|| error("undefined PVD3 class namespace"))?;
        let name = metadata
            .strings
            .get(&class.name.name)
            .ok_or_else(|| error("undefined PVD3 class name"))?;
        let name = if namespace.is_empty() {
            name.clone()
        } else {
            format!("{namespace}::{name}")
        };
        let mut fields = BTreeMap::new();
        for id in properties {
            if !progress() {
                return Err(cancelled());
            }
            if let Some(data) = initial_value(metadata, metadata.property(id)?)? {
                metadata_budget(&mut allocation, data.len() + 96, 1)?;
                fields.insert(id, data);
            }
        }
        if !fields.is_empty() {
            metadata_budget(&mut allocation, name.len() + 128, 1)?;
            classes.insert(name, fields);
        }
    }
    Ok(InitialProperties { classes })
}

// Native Create zeroes fixed storage; array headers therefore describe an empty
// array. Only layouts with a defined decoder acquire a visible initial value.
pub(in crate::native) fn initial_value(
    metadata: &Metadata,
    property: &Property,
) -> Result<Option<Vec<u8>>, Error> {
    let datatype = metadata.class(property.datatype_id)?;
    if datatype.dynamic || datatype.size == 0 || datatype.size > 64 {
        return Ok(None);
    }
    let data = vec![0; if property.kind == 2 { 0 } else { datatype.size }];
    Ok(value_supported(metadata, property, &data)?.then_some(data))
}

#[derive(Default)]
pub(in crate::native) struct ProvenProperties {
    instances: BTreeMap<u32, ProvenInstance>,
}

struct ProvenInstance {
    class: u32,
    fields: BTreeMap<u32, Vec<u8>>,
}

struct Candidate {
    class: u32,
    position: u64,
    length: u32,
    untouched: BTreeSet<u32>,
    created: bool,
    alive: bool,
    witnessed: bool,
    invalid: bool,
}

impl ProvenProperties {
    pub(in crate::native) fn fields(&self, id: u32, class: u32) -> Option<&BTreeMap<u32, Vec<u8>>> {
        self.instances
            .get(&id)
            .filter(|instance| instance.class == class)
            .map(|instance| &instance.fields)
    }
}

fn property_ids(
    metadata: &Metadata,
    class: u32,
    progress: &mut dyn FnMut() -> bool,
) -> Result<Option<Vec<u32>>, Error> {
    let fixed_size = metadata.class(class)?.size;
    let mut ancestors = Vec::new();
    let mut current = class;
    while current != u32::MAX {
        if !progress() {
            return Err(cancelled());
        }
        if ancestors.len() == 128 || ancestors.contains(&current) {
            return Err(error("invalid PVD3 saved instance inheritance"));
        }
        ancestors.push(current);
        current = metadata.class(current)?.base;
    }
    let mut ids = Vec::new();
    let mut seen = BTreeSet::new();
    let mut ranges = BTreeMap::<usize, usize>::new();
    for class in ancestors.into_iter().rev() {
        let owner = metadata.class(class)?;
        for &id in &owner.properties {
            if !progress() {
                return Err(cancelled());
            }
            if ids.len() >= MAX_METADATA / 192 {
                return Ok(None);
            }
            let property = metadata.property(id)?;
            if property.owner != owner.name || !seen.insert(id) {
                return Err(error("invalid PVD3 saved instance property directory"));
            }
            let datatype = metadata.class(property.datatype_id)?;
            if datatype.dynamic || datatype.size == 0 {
                return Ok(None);
            }
            let width = if property.kind == 2 {
                12
            } else {
                datatype.size
            };
            let end = property
                .offset32
                .checked_add(width)
                .ok_or_else(|| error("PVD3 proven property offset overflow"))?;
            if end > fixed_size {
                return Err(error("PVD3 initial property exceeds fixed class layout"));
            }
            if ranges
                .range(..=property.offset32)
                .next_back()
                .is_some_and(|(_, previous_end)| *previous_end > property.offset32)
                || ranges
                    .range(property.offset32..)
                    .next()
                    .is_some_and(|(start, _)| *start < end)
            {
                return Ok(None);
            }
            ranges.insert(property.offset32, end);
            ids.push(id);
        }
    }
    Ok(Some(ids))
}

fn value_supported(metadata: &Metadata, property: &Property, data: &[u8]) -> Result<bool, Error> {
    let datatype = metadata.class(property.datatype_id)?;
    if property.datatype != datatype.name
        || metadata
            .strings
            .get(&datatype.name.namespace)
            .map(String::as_str)
            != Some("physx3")
    {
        return Ok(false);
    }
    if metadata
        .strings
        .get(&property.name)
        .is_some_and(|name| name == "events")
        || metadata
            .strings
            .get(&property.owner.namespace)
            .is_some_and(|name| name.starts_with("_debugger"))
    {
        return Ok(false);
    }
    if !data.len().is_multiple_of(datatype.size)
        || (property.kind != 2 && data.len() != datatype.size)
        || data.len() / datatype.size > MAX_ITEMS
    {
        return Err(error("invalid PVD3 proven property byte length"));
    }
    let name = metadata
        .strings
        .get(&datatype.name.name)
        .ok_or_else(|| error("undefined PVD3 proven datatype name"))?;
    let width = match name.as_str() {
        "ObjectRef" | "VoidPtr" => {
            return Ok(datatype.size == 4 && data.iter().all(|byte| *byte == 0));
        }
        "StringHandle" => {
            return Ok(datatype.size == 4
                && data.chunks_exact(4).all(|bytes| {
                    metadata
                        .strings
                        .contains_key(&u32::from_le_bytes(bytes.try_into().unwrap()))
                }));
        }
        "PvdU8" | "PvdI8" | "PvdBool" => 1,
        "PvdU16" | "PvdI16" => 2,
        "PvdU32" | "PvdI32" | "PvdF32" | "PvdHullPolygonData" | "PxHeightFieldSample" => 4,
        "PvdU64" | "PvdI64" | "PvdF64" | "PxVec2" => 8,
        "PxVec3" => 12,
        "PxVec4" | "PxQuat" | "U32Array4" => 16,
        "PxBounds3" => 24,
        "PxTransform" => 28,
        "PxMat33" => 36,
        "PxMat44" => 64,
        _ => return Ok(false),
    };
    Ok(datatype.size == width && datatype.string_offsets.is_empty())
}

// A saved body proves an initial field only when its exact lifetime contains no
// earlier write to that field. Non-null references need creation-order evidence.
pub(in crate::native) fn proven_initial_properties<R: Read + Seek>(
    metadata: &Metadata,
    source: &mut Source<R>,
    backing: Range<u64>,
    pages: &[(u64, u64, u32, u32)],
    saved: &BTreeMap<u64, (u32, u32)>,
    progress: &mut dyn FnMut() -> bool,
) -> Result<ProvenProperties, Error> {
    if source.len() > 512 << 20 || saved.len() > MAX_METADATA / 256 {
        return Ok(ProvenProperties::default());
    }
    let mut allocation = saved.len() * 256;
    let mut candidates = BTreeMap::<u32, Candidate>::new();
    for (&position, &(id, length)) in saved {
        if let Some(previous) = candidates.get_mut(&id) {
            previous.invalid = true;
            continue;
        }
        let bytes = source.read_block(
            import::backing_range(backing.start, position, 12)?,
            12,
            progress,
        )?;
        let mut header = Reader::new(&bytes);
        let class = header.u32()?;
        let saved_id = header.u32()?;
        let body_length = header.u32()?;
        if saved_id != id || body_length.checked_add(12) != Some(length) {
            return Err(error("PVD3 proven instance header differs from directory"));
        }
        let Some(properties) = property_ids(metadata, class, progress)? else {
            continue;
        };
        allocation = allocation.saturating_add(properties.len().saturating_mul(64));
        if allocation > MAX_METADATA {
            return Ok(ProvenProperties::default());
        }
        candidates.insert(
            id,
            Candidate {
                class,
                position,
                length: body_length,
                untouched: properties.into_iter().collect(),
                created: false,
                alive: false,
                witnessed: false,
                invalid: false,
            },
        );
    }
    let mut ordinal = 0;
    for &(first, position, count, length) in pages {
        if first != ordinal {
            return Err(error("invalid PVD3 proof page ordering"));
        }
        let index_bytes = (count as usize).saturating_add(1).saturating_mul(4);
        if index_bytes > MAX_METADATA.saturating_sub(allocation) {
            return Ok(ProvenProperties::default());
        }
        let page = EventPage::read(
            source,
            import::backing_range(backing.start, position, u64::from(length))?,
            count,
            index_bytes,
            progress,
        )?;
        for event in 0..page.event_count() {
            let range = page.event_range(event)?;
            let bytes =
                source.read_block(range.start..range.end.min(range.start + 16), 16, progress)?;
            let mut reader = Reader::new(&bytes);
            let kind = reader.u32()?;
            match kind {
                1..=7 | 10 => {
                    let id = reader.u32()?;
                    let Some(candidate) = candidates.get_mut(&id) else {
                        continue;
                    };
                    match kind {
                        1 => {
                            candidate.invalid |=
                                candidate.created || reader.u32()? != candidate.class;
                            candidate.created = true;
                            candidate.alive = true;
                        }
                        2 => {
                            candidate.invalid |= !candidate.witnessed || !candidate.alive;
                            candidate.alive = false;
                        }
                        3 | 5..=7 => {
                            candidate.invalid |= !candidate.alive;
                            if !candidate.witnessed {
                                candidate.untouched.remove(&reader.u32()?);
                            }
                        }
                        4 => {
                            candidate.invalid |= !candidate.alive;
                            let message = metadata
                                .messages
                                .get(reader.u32()? as usize)
                                .and_then(Option::as_ref)
                                .ok_or_else(|| error("undefined PVD3 proof property message"))?;
                            if !candidate.witnessed {
                                for entry in &message.entries {
                                    candidate.untouched.remove(&entry.property);
                                }
                            }
                        }
                        10 => {
                            let position = reader.u64()?;
                            if position == candidate.position {
                                candidate.invalid |= !candidate.alive || candidate.witnessed;
                                candidate.witnessed = true;
                            }
                        }
                        _ => unreachable!(),
                    }
                }
                8 | 9 => {}
                _ => return Ok(ProvenProperties::default()),
            }
        }
        ordinal += u64::from(count);
    }
    let mut result = ProvenProperties::default();
    for (id, candidate) in candidates {
        if candidate.invalid
            || !candidate.created
            || !candidate.witnessed
            || candidate.untouched.is_empty()
        {
            continue;
        }
        if candidate.length as usize > MAX_METADATA.saturating_sub(allocation) {
            return Ok(ProvenProperties::default());
        }
        let body_range = import::backing_range(
            backing.start,
            candidate.position + 12,
            u64::from(candidate.length),
        )?;
        if body_range.end > backing.end {
            return Err(error("PVD3 proven instance exceeds backing range"));
        }
        let bytes = source.read_block(body_range, MAX_METADATA - allocation, progress)?;
        let mut body = Reader::new(&bytes);
        let fixed = body.blob()?;
        if fixed.len() != metadata.class(candidate.class)?.size {
            return Err(error("PVD3 proven instance fixed size differs from class"));
        }
        let mut fields = BTreeMap::new();
        for property_id in property_ids(metadata, candidate.class, progress)?.unwrap() {
            if !progress() {
                return Err(cancelled());
            }
            let property = metadata.property(property_id)?;
            let width = if property.kind == 2 {
                12
            } else {
                metadata.class(property.datatype_id)?.size
            };
            let end = property
                .offset32
                .checked_add(width)
                .ok_or_else(|| error("PVD3 proven property offset overflow"))?;
            let scalar = fixed
                .get(property.offset32..end)
                .ok_or_else(|| error("PVD3 proven property exceeds fixed data"))?;
            let data = if property.kind == 2 {
                body.blob()?
            } else {
                scalar
            };
            if candidate.untouched.contains(&property_id)
                && let Some(initial) = initial_value(metadata, property)?
            {
                if data != initial {
                    return Err(error(
                        "saved instance contradicts native Create initialization",
                    ));
                }
                allocation = allocation.saturating_add(data.len() + 64);
                if allocation > MAX_METADATA.saturating_sub(bytes.len()) {
                    return Ok(ProvenProperties::default());
                }
                fields.insert(property_id, data.to_vec());
            }
        }
        body.end()?;
        if !fields.is_empty() {
            result.instances.insert(
                id,
                ProvenInstance {
                    class: candidate.class,
                    fields,
                },
            );
        }
    }
    Ok(result)
}
