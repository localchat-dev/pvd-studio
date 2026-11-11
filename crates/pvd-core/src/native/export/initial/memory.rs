use super::*;

pub(super) struct Scope {
    ids: [u32; 2],
    classes: [u32; 2],
    properties: [u32; 2],
    sources: usize,
}

impl Scope {
    pub(super) fn read(
        initial: &State,
        layout: &Layout<'_>,
        budget: &mut Budget,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Option<Self>, Error> {
        let ids = layout.communication.memory_instances;
        if !ids
            .iter()
            .any(|id| *id != u32::MAX && initial.classes.contains_key(id))
        {
            return Ok(None);
        }
        let mut buffer_source = false;
        for instance in &layout.communication.instances {
            check(proceed)?;
            buffer_source |= instance.id == ids[0];
        }
        if ids.contains(&u32::MAX)
            || ids[0] == ids[1]
            || !ids.iter().all(|id| initial.classes.contains_key(id))
            || !buffer_source
        {
            return Err(error(
                "initial memory prefix requires a complete captured buffer/totals pair",
            ));
        }
        let classes = ids.map(|id| initial.classes[&id]);
        let properties = properties(layout, classes, &mut budget.bytes, proceed)?;
        let sources = layout.communication.memory_names.len();
        let scope = Self {
            ids,
            classes,
            properties,
            sources,
        };
        for entry in &initial.entries {
            check(proceed)?;
            let saved = &layout.saved[entry.record];
            if let Some(slot) = scope.slot(saved.id) {
                scope.payload(layout, saved, slot)?;
            } else if memory_class(layout, saved.class, proceed)? {
                return Err(error(
                    "initial memory prefix has an additional memory instance",
                ));
            }
        }
        let mut alive = [true; 2];
        let mut witnessed = [false; 2];
        for event in &layout.events {
            check(proceed)?;
            match event {
                HistoryEvent::Create(id, class)
                    if scope.contains(*id) || memory_class(layout, *class, proceed)? =>
                {
                    return Err(error(
                        "initial memory prefix does not support memory instance creation or reuse",
                    ));
                }
                HistoryEvent::Destroy(id) if scope.contains(*id) => {
                    let slot = scope.slot(*id).unwrap();
                    if !std::mem::replace(&mut alive[slot], false) {
                        return Err(error("initial memory instance is destroyed twice"));
                    }
                }
                HistoryEvent::Saved { id, .. } if scope.contains(*id) => {
                    let slot = scope.slot(*id).unwrap();
                    if !alive[slot] || std::mem::replace(&mut witnessed[slot], true) {
                        return Err(error("initial memory witness has no live instance"));
                    }
                }
                HistoryEvent::Set {
                    kind,
                    id,
                    property,
                    data,
                    ..
                } if scope.contains(*id) => {
                    let slot = scope.slot(*id).unwrap();
                    if !alive[slot] || *kind != 3 || *property != scope.properties[slot] {
                        return Err(error(
                            "initial memory prefix requires complete captured property buffers",
                        ));
                    }
                    scope.values(data, slot)?;
                }
                HistoryEvent::Array { id, .. } if scope.contains(*id) => {
                    return Err(error(
                        "initial memory prefix does not support array editing",
                    ));
                }
                _ => {}
            }
        }
        Ok(Some(scope))
    }

    pub(super) fn contains(&self, id: u32) -> bool {
        self.ids.contains(&id)
    }

    fn slot(&self, id: u32) -> Option<usize> {
        self.ids.iter().position(|candidate| *candidate == id)
    }

    fn values(&self, data: &[u8], slot: usize) -> Result<(), Error> {
        if slot == 1 && (!data.len().is_multiple_of(4) || data.len() / 4 > self.sources) {
            return Err(error(
                "initial memory totals exceed their captured source directory",
            ));
        }
        Ok(())
    }

    fn payload<'a>(
        &self,
        layout: &Layout<'_>,
        saved: &'a Saved<'_>,
        slot: usize,
    ) -> Result<&'a [u8], Error> {
        if saved.class != self.classes[slot] {
            return Err(error(
                "initial memory saved class differs from its instance",
            ));
        }
        let mut reader = Reader::new(&saved.body);
        if reader.blob()?.len() != layout.metadata.class(saved.class)?.size {
            return Err(error(
                "initial memory fixed body differs from its class size",
            ));
        }
        let data = reader.blob()?;
        self.values(data, slot)?;
        reader.end()?;
        Ok(data)
    }
}

pub(in crate::native::export) fn properties(
    layout: &Layout<'_>,
    classes: [u32; 2],
    allocation: &mut usize,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<[u32; 2], Error> {
    check(proceed)?;
    let properties = [
        property(layout, classes[0], 0, proceed)?,
        property(layout, classes[1], 1, proceed)?,
    ];
    let count = layout.communication.memory_names.len();
    metadata_budget(allocation, count, 1)?;
    let mut indices = Vec::new();
    indices.try_reserve_exact(count)?;
    indices.resize(count, false);
    for &(_, _, _, index) in &layout.communication.memory_names {
        check(proceed)?;
        let slot = indices
            .get_mut(index as usize)
            .ok_or_else(|| error("native memory source index exceeds its directory"))?;
        if std::mem::replace(slot, true) {
            return Err(error("native memory source indices are not unique"));
        }
    }
    Ok(properties)
}

fn property(
    layout: &Layout<'_>,
    class_id: u32,
    slot: usize,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<u32, Error> {
    let class = layout.metadata.class(class_id)?;
    let label = |id| layout.metadata.strings.get(&id).map(String::as_str);
    let (namespace, name, field, datatype, width) = if slot == 0 {
        (
            "_debugger_",
            "PxProfileMemoryEventBuffer",
            "events",
            "PvdU8",
            1,
        )
    } else {
        ("_debugger", "MemoryEventTotals", "totals", "PvdI32", 4)
    };
    let mut definitions = 0;
    for definition in layout.metadata.classes.iter().flatten() {
        check(proceed)?;
        if label(definition.name.namespace) == Some(namespace)
            && label(definition.name.name) == Some(name)
        {
            definitions += 1;
        }
    }
    if label(class.name.namespace) != Some(namespace)
        || label(class.name.name) != Some(name)
        || class.base != u32::MAX
        || class.size != 24
        || class.properties.len() != 1
        || !class.dynamic
        || class.has_pointers
        || definitions != 1
    {
        return Err(error("unsupported native memory class layout"));
    }
    let id = class.properties[0];
    let property = layout.metadata.property(id)?;
    let value = layout.metadata.class(property.datatype_id)?;
    if property.owner != class.name
        || label(property.name) != Some(field)
        || property.kind != 2
        || property.offset32 != 0
        || label(property.datatype.namespace) != Some("physx3")
        || label(property.datatype.name) != Some(datatype)
        || value.name != property.datatype
        || value.size != width
        || value.base != u32::MAX
        || !value.properties.is_empty()
        || value.dynamic
        || value.has_pointers
    {
        return Err(error("unsupported native memory property layout"));
    }
    Ok(id)
}

fn memory_class(
    layout: &Layout<'_>,
    mut id: u32,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<bool, Error> {
    for _ in 0..128 {
        check(proceed)?;
        let class = layout.metadata.class(id)?;
        if layout
            .metadata
            .strings
            .get(&class.name.name)
            .is_some_and(|name| {
                matches!(
                    name.as_str(),
                    "PxProfileMemoryEventBuffer" | "MemoryEventTotals"
                )
            })
        {
            return Ok(true);
        }
        if class.base == u32::MAX {
            return Ok(false);
        }
        id = class.base;
    }
    Err(error("initial memory prefix has invalid class inheritance"))
}

pub(super) struct Proof {
    cut: usize,
    signature: blake3::Hash,
}

impl Proof {
    #[cfg(test)]
    pub(super) fn prepare(
        layout: &Layout<'_>,
        cut: usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Option<Self>, Error> {
        Self::prepare_with_held(layout, cut, 0, proceed)
    }

    pub(super) fn prepare_with_held(
        layout: &Layout<'_>,
        cut: usize,
        held: usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Option<Self>, Error> {
        let initial = State::read_with_held(layout, held, proceed)?;
        let mut budget = Budget::new(&initial)?;
        budget.add(held, 1)?;
        let Some(scope) = Scope::read(&initial, layout, &mut budget, proceed)? else {
            return Ok(None);
        };
        let mut hash = blake3::Hasher::new();
        field(&mut hash, b"initial-memory-prefix-v1", proceed)?;
        for slot in 0..2 {
            for value in [scope.ids[slot], scope.classes[slot], scope.properties[slot]] {
                field(&mut hash, &value.to_le_bytes(), proceed)?;
            }
            let mut entry = None;
            for candidate in &initial.entries {
                check(proceed)?;
                if candidate.id == scope.ids[slot] {
                    entry = Some(candidate);
                    break;
                }
            }
            let entry = entry.ok_or_else(|| error("initial memory proof has no captured body"))?;
            field(&mut hash, &layout.saved[entry.record].body, proceed)?;
        }
        field(&mut hash, &(scope.sources as u64).to_le_bytes(), proceed)?;
        for &(kind, file, line, index) in &layout.communication.memory_names {
            for bytes in [kind, file, &line.to_le_bytes(), &index.to_le_bytes()] {
                field(&mut hash, bytes, proceed)?;
            }
        }
        for (ordinal, event) in layout
            .events
            .get(..cut)
            .ok_or_else(|| error("memory prefix exceeds history"))?
            .iter()
            .enumerate()
        {
            check(proceed)?;
            match event {
                HistoryEvent::Set {
                    kind,
                    id,
                    property,
                    data,
                    undo,
                } if scope.contains(*id) => {
                    field(&mut hash, &(ordinal as u64).to_le_bytes(), proceed)?;
                    for value in [*kind, *id, *property] {
                        field(&mut hash, &value.to_le_bytes(), proceed)?;
                    }
                    field(&mut hash, data, proceed)?;
                    undo_words(&mut hash, undo, proceed)?;
                }
                HistoryEvent::Saved { id, position } if scope.contains(*id) => {
                    let mut saved = None;
                    for candidate in &layout.saved {
                        check(proceed)?;
                        if candidate.id == *id && candidate.position == *position {
                            saved = Some(candidate);
                            break;
                        }
                    }
                    let saved =
                        saved.ok_or_else(|| error("memory prefix has no exact saved witness"))?;
                    scope.payload(layout, saved, scope.slot(*id).unwrap())?;
                    field(&mut hash, &(ordinal as u64).to_le_bytes(), proceed)?;
                    field(&mut hash, &10u32.to_le_bytes(), proceed)?;
                    field(&mut hash, &id.to_le_bytes(), proceed)?;
                    field(&mut hash, &saved.body, proceed)?;
                }
                HistoryEvent::Destroy(id) if scope.contains(*id) => {
                    field(&mut hash, &(ordinal as u64).to_le_bytes(), proceed)?;
                    field(&mut hash, &2u32.to_le_bytes(), proceed)?;
                    field(&mut hash, &id.to_le_bytes(), proceed)?;
                }
                _ => {}
            }
        }
        Ok(Some(Self {
            cut,
            signature: hash.finalize(),
        }))
    }

    #[cfg(test)]
    pub(super) fn verify_layout(
        &self,
        layout: &Layout<'_>,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        self.verify_with_held(layout, 0, proceed)
    }

    pub(super) fn verify_with_held(
        &self,
        layout: &Layout<'_>,
        held: usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let actual = Self::prepare_with_held(layout, self.cut, held, proceed)?
            .ok_or_else(|| error("prefix output lost its initial memory scope"))?;
        if actual.signature != self.signature {
            return Err(error(
                "prefix output changed captured initial memory inputs",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
