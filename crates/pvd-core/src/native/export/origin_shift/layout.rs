use super::*;
use crate::native::{Class, ClassPacking};

pub(in crate::native::export) fn fixed_layout(
    metadata: &Metadata,
    id: u32,
    scan: &mut dyn FnMut(usize) -> Result<(), Error>,
) -> Result<bool, Error> {
    scan(64)?;
    let class = metadata.class(id)?;
    let (name, size) = match label(metadata, class.name.name) {
        Some("PxTransform") => ("PxTransform", 28),
        Some("PxBounds3") => ("PxBounds3", 24),
        _ => return Ok(false),
    };
    if !fixed_class(metadata, class, name, size) {
        return Ok(false);
    }
    if class.properties.is_empty() {
        return Ok(true);
    }
    canonical(metadata, id, class, scan)
}

fn label(metadata: &Metadata, id: u32) -> Option<&str> {
    metadata.strings.get(&id).map(String::as_str)
}

fn fixed_class(metadata: &Metadata, class: &Class, name: &str, size: usize) -> bool {
    label(metadata, class.name.namespace) == Some("physx3")
        && label(metadata, class.name.name) == Some(name)
        && class.base == u32::MAX
        && class.size == size
        && !class.dynamic
        && !class.has_pointers
        && class.string_offsets.is_empty()
}

fn canonical(
    metadata: &Metadata,
    id: u32,
    class: &Class,
    scan: &mut dyn FnMut(usize) -> Result<(), Error>,
) -> Result<bool, Error> {
    let (name, size, fields): (&str, usize, &[(&str, u32, usize)]) = match id {
        73 => ("PvdF32", 4, &[]),
        82 => ("PxVec3", 12, &[("x", 73, 0), ("y", 73, 4), ("z", 73, 8)]),
        84 => ("PxBounds3", 24, &[("minimum", 82, 0), ("maximum", 82, 12)]),
        85 => (
            "PxQuat",
            16,
            &[("x", 73, 0), ("y", 73, 4), ("z", 73, 8), ("w", 73, 12)],
        ),
        86 => ("PxTransform", 28, &[("q", 85, 0), ("p", 82, 16)]),
        _ => return Ok(false),
    };
    if !fixed_class(metadata, class, name, size)
        || class.packing
            != (ClassPacking {
                uniform_width: 4,
                class_type: 73,
            })
        || class.properties.len() != fields.len()
    {
        return Ok(false);
    }
    for (&property, &(name, datatype, offset)) in class.properties.iter().zip(fields) {
        scan(64)?;
        let property = metadata.property(property)?;
        if property.owner != class.name
            || label(metadata, property.name) != Some(name)
            || property.datatype_id != datatype
            || property.kind != 1
            || property.offset32 != offset
            || property.semantic != 0
            || !property.values.is_empty()
        {
            return Ok(false);
        }
        scan(64)?;
        let child = metadata.class(datatype)?;
        if property.datatype != child.name || !canonical(metadata, datatype, child, scan)? {
            return Ok(false);
        }
    }
    Ok(true)
}
