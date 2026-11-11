use crate::native::{Class, ClassPacking, Metadata};

pub(super) fn is_bit_copy(metadata: &Metadata, source: u32, destination: u32) -> bool {
    // 固定内置 ID 和 packing 排除同宽但需要原版 marshaller 的类型。
    if !matches!(
        (source, destination),
        (65, 66)
            | (66, 65)
            | (67, 68)
            | (68, 67)
            | (69, 70)
            | (70, 69)
            | (71, 72)
            | (72, 71)
            | (83, 85)
            | (85, 83)
    ) {
        return false;
    }
    builtin(metadata, source) && builtin(metadata, destination)
}

pub(super) fn is_bool_alias(metadata: &Metadata, source: u32, destination: u32) -> bool {
    matches!((source, destination), (75, 65) | (75, 66))
        && builtin(metadata, source)
        && builtin(metadata, destination)
}

fn builtin(metadata: &Metadata, id: u32) -> bool {
    let (name, size, uniform_width, packed_type) = match id {
        65 => ("PvdI8", 1, 1, 65),
        66 => ("PvdU8", 1, 1, 66),
        67 => ("PvdI16", 2, 2, 67),
        68 => ("PvdU16", 2, 2, 68),
        69 => ("PvdI32", 4, 4, 69),
        70 => ("PvdU32", 4, 4, 70),
        71 => ("PvdI64", 8, 8, 71),
        72 => ("PvdU64", 8, 8, 72),
        73 => ("PvdF32", 4, 4, 73),
        75 => ("PvdBool", 1, 1, 75),
        83 => ("PxVec4", 16, 4, 73),
        85 => ("PxQuat", 16, 4, 73),
        _ => return false,
    };
    let Ok(class) = metadata.class(id) else {
        return false;
    };
    if metadata
        .strings
        .get(&class.name.namespace)
        .map(String::as_str)
        != Some("physx3")
        || metadata.strings.get(&class.name.name).map(String::as_str) != Some(name)
        || class.base != u32::MAX
        || class.size != size
        || class.dynamic
        || class.has_pointers
        || !class.string_offsets.is_empty()
        || class.packing
            != (ClassPacking {
                uniform_width,
                class_type: packed_type,
            })
    {
        return false;
    }
    if size == 16 {
        components(metadata, class)
    } else {
        class.properties.is_empty()
    }
}

fn components(metadata: &Metadata, class: &Class) -> bool {
    if class.properties.len() != 4 || !builtin(metadata, 73) {
        return false;
    }
    let Ok(float) = metadata.class(73) else {
        return false;
    };
    class
        .properties
        .iter()
        .zip(["x", "y", "z", "w"])
        .enumerate()
        .all(|(index, (&id, name))| {
            let Ok(property) = metadata.property(id) else {
                return false;
            };
            property.owner == class.name
                && metadata.strings.get(&property.name).map(String::as_str) == Some(name)
                && property.datatype_id == 73
                && property.datatype == float.name
                && property.kind == 1
                && property.offset32 == index * 4
                && property.semantic == 0
                && property.values.is_empty()
        })
}
