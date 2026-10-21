use super::*;

struct Instance<'a> {
    id: u32,
    class: u32,
    body: &'a [u8],
}

pub(super) fn restore(
    metadata: &Metadata,
    builder: &mut RecordingBuilder,
    backing: &[u8],
    directory: &[u8],
    allocation: &mut usize,
    progress: &mut dyn FnMut() -> bool,
) -> Result<usize, Error> {
    let mut reader = Reader::new(directory);
    let count = reader.count()?;
    if reader.bytes.len() - reader.pos != count * 12 {
        return Err(error(
            "PVD3 snapshot directory length differs from its instance count",
        ));
    }
    metadata_budget(allocation, count, size_of::<Instance<'_>>() + 96)?;
    let mut instances = Vec::with_capacity(count);
    let mut ids = BTreeSet::new();
    for _ in 0..count {
        if !progress() {
            return Err(cancelled());
        }
        let id = reader.u32()?;
        if !ids.insert(id) {
            return Err(error("duplicate PVD3 snapshot instance ID"));
        }
        let position = reader.u64()?;
        let mut header = Reader::new(checked_range(backing, position, 12)?);
        let class = header.u32()?;
        if header.u32()? != id {
            return Err(error(
                "PVD3 snapshot instance ID differs from its directory",
            ));
        }
        let length = header.u32()? as usize;
        if length > MAX_METADATA {
            return Err(error("PVD3 snapshot instance exceeds 64 MiB"));
        }
        metadata.class(class)?;
        let start = position
            .checked_add(12)
            .ok_or_else(|| error("PVD3 snapshot instance position overflow"))?;
        instances.push(Instance {
            id,
            class,
            body: checked_range(backing, start, length as u64)?,
        });
    }
    reader.end()?;
    for instance in &instances {
        if !progress() {
            return Err(cancelled());
        }
        push(
            builder,
            Event::CreateInstance {
                class: metadata.class(instance.class)?.name,
                id: u64::from(instance.id),
            },
        )?;
    }
    for instance in &instances {
        restore_properties(metadata, builder, instance, progress)?;
    }
    if count != 0 {
        builder.recording.diagnostics.push(format!(
            "PVD3 initial snapshot restored {count} saved instances. Snapshot event-buffer attributes are retained as values and are not replayed as newly received buffers. Profile starts, memory allocations and untimed error history preceding the snapshot are unavailable; analysis contains only subsequent history events."
        ));
    }
    Ok(count)
}

fn restore_properties(
    metadata: &Metadata,
    builder: &mut RecordingBuilder,
    instance: &Instance<'_>,
    progress: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    let class = metadata.class(instance.class)?;
    let mut reader = Reader::new(instance.body);
    let fixed = reader.blob()?;
    if fixed.len() != class.size {
        return Err(error(
            "PVD3 snapshot fixed data size differs from its class",
        ));
    }
    let mut ancestors = Vec::new();
    let mut current = instance.class;
    loop {
        if ancestors.len() >= 128 || ancestors.contains(&current) {
            return Err(error("invalid PVD3 snapshot class inheritance"));
        }
        ancestors.push(current);
        let class = metadata.class(current)?;
        if class.base == u32::MAX {
            break;
        }
        current = class.base;
    }
    for class_id in ancestors.into_iter().rev() {
        let owner = metadata.class(class_id)?;
        for &property_id in &owner.properties {
            if !progress() {
                return Err(cancelled());
            }
            let property = metadata.property(property_id)?;
            if property.owner != owner.name {
                return Err(error(
                    "PVD3 snapshot property owner differs from its class directory",
                ));
            }
            let datatype = metadata.class(property.datatype_id)?;
            if datatype.dynamic {
                return Err(error(
                    "PVD3 snapshot nested dynamic datatypes are not supported",
                ));
            }
            let width = if property.kind == 2 {
                12
            } else {
                datatype.size
            };
            let end = property
                .offset32
                .checked_add(width)
                .ok_or_else(|| error("PVD3 snapshot property offset overflow"))?;
            let scalar = fixed
                .get(property.offset32..end)
                .ok_or_else(|| error("PVD3 snapshot property exceeds fixed data"))?;
            let data = if property.kind == 2 {
                reader.blob()?
            } else {
                scalar
            };
            let id = u64::from(instance.id);
            let name = builder.string(property.name)?;
            let object_type = &builder.state.objects[&id].type_name;
            if name == "events"
                && matches!(
                    object_type.as_str(),
                    "_debugger_::PxProfileZone"
                        | "_debugger_::PxProfileMemoryEventBuffer"
                        | "_debugger_::PvdUserRenderer"
                )
            {
                if property.kind != 2
                    || builder.name(property.datatype)? != "physx3::PvdU8"
                    || datatype.size != 1
                {
                    return Err(error("unsupported PVD3 snapshot event buffer layout"));
                }
                value_budget(builder, data.len())?;
                builder.action(Action::Set(id, name, Value::Bytes(Arc::from(data))))?;
            } else if metadata.is_error_property(builder, property)? {
                metadata.validate_error_layout(builder, property)?;
                let width = metadata_size(metadata, property)?;
                if !data.len().is_multiple_of(width) || data.len() / width > MAX_ITEMS {
                    return Err(error("invalid PVD3 snapshot error array length"));
                }
                value_budget(
                    builder,
                    data.len() / width * size_of::<Value>() + data.len(),
                )?;
                let mut values = Vec::with_capacity(data.len() / width);
                for data in data.chunks_exact(width) {
                    if !progress() {
                        return Err(cancelled());
                    }
                    values.push(metadata.value(builder, property, data, false)?);
                }
                builder.action(Action::Set(id, name, Value::Array(values.into())))?;
            } else {
                metadata.set_with_progress(builder, id, property_id, data, progress)?;
            }
        }
    }
    reader.end()
}
