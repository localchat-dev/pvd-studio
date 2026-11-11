use super::*;

mod proven;
pub(super) use proven::{initial_properties, initial_value, proven_initial_properties};

struct Instance<'a> {
    id: u32,
    class: u32,
    body: &'a [u8],
}

struct SavedInstance {
    id: u32,
    class: u32,
    position: u64,
    length: usize,
}

pub(super) fn restore(
    metadata: &Metadata,
    builder: &mut RecordingBuilder,
    backing: &[u8],
    directory: &[u8],
    allocation: &mut usize,
    progress: &mut dyn FnMut() -> bool,
) -> Result<usize, Error> {
    let mut source = read_source::Source::new(std::io::Cursor::new(backing))?;
    restore_from_source(
        metadata,
        builder,
        &mut source,
        0..backing.len() as u64,
        directory,
        allocation,
        progress,
    )
}

pub(super) fn restore_from_source<R: std::io::Read + std::io::Seek>(
    metadata: &Metadata,
    builder: &mut RecordingBuilder,
    source: &mut read_source::Source<R>,
    backing: std::ops::Range<u64>,
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
    metadata_budget(allocation, count, size_of::<SavedInstance>() + 96)?;
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
        let range = import::backing_range(backing.start, position, 12)?;
        if range.end > backing.end {
            return Err(error("PVD3 snapshot instance exceeds backing range"));
        }
        let header_bytes = source.read_block(range, 12, progress)?;
        let mut header = Reader::new(&header_bytes);
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
        let range = import::backing_range(backing.start, start, length as u64)?;
        if range.end > backing.end {
            return Err(error("PVD3 snapshot instance exceeds backing range"));
        }
        source.check_range(range.clone())?;
        instances.push(SavedInstance {
            id,
            class,
            position: range.start,
            length,
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
        let body = source.read_block(
            instance.position..instance.position + instance.length as u64,
            MAX_METADATA,
            progress,
        )?;
        restore_properties(
            metadata,
            builder,
            &Instance {
                id: instance.id,
                class: instance.class,
                body: &body,
            },
            progress,
        )?;
    }
    if count != 0 {
        builder.recording.diagnostics.push(format!(
            "PVD3 initial snapshot restored {count} saved instances. Snapshot event-buffer attributes are retained as values and are not replayed as newly received buffers. Initial errors retain container identity and array order without receive positions. Earlier profile starts, memory allocations and error occurrences are unavailable; event analysis contains only subsequent history."
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
            restore_property(metadata, builder, instance.id, property_id, data, progress)?;
            if metadata.is_error_property(builder, property)? {
                metadata.restore_initial_errors(builder, u64::from(instance.id), data, progress)?;
            }
        }
    }
    reader.end()
}

fn restore_property(
    metadata: &Metadata,
    builder: &mut RecordingBuilder,
    instance_id: u32,
    property_id: u32,
    data: &[u8],
    progress: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    let property = metadata.property(property_id)?;
    let datatype = metadata.class(property.datatype_id)?;
    let id = u64::from(instance_id);
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
    Ok(())
}
