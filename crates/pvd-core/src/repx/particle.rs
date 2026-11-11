use super::*;

#[cfg(test)]
mod tests;

const FIELDS: &[&str] = &[
    "Name",
    "ActorFlags",
    "DominanceGroup",
    "OwnerClient",
    "ClientBehaviorFlags",
    "Scene",
    "ConcreteTypeName",
    "Aggregate",
    "Damping",
    "ExternalAcceleration",
    "ParticleMass",
    "Restitution",
    "DynamicFriction",
    "StaticFriction",
    "SimulationFilterData",
    "ParticleBaseFlags",
    "MaxParticles",
    "MaxMotionDistance",
    "RestOffset",
    "ContactOffset",
    "GridSize",
    "ParticleReadDataFlags",
    "ProjectionPlane",
    "NbParticles",
    "ValidParticleRange",
    "ValidParticleBitmap",
    "Positions",
    "Velocities",
    "RestOffsets",
    "Flags",
];
const FLUID_FIELDS: &[&str] = &["Stiffness", "Viscosity", "RestParticleDistance"];

pub(super) fn supported(name: &str) -> bool {
    matches!(name, "PxParticleSystem" | "PxParticleFluid")
}

fn array<'a>(object: &'a Object, name: &str, length: usize) -> Result<&'a [Value], Error> {
    match value(object, name)? {
        Value::Array(values) if values.len() == length => Ok(values),
        _ => Err(error(format!(
            "RepX Particle {name} must contain {length} values"
        ))),
    }
}

fn scalar(value: &Value) -> Result<f32, Error> {
    match value {
        Value::F32(number) if number.is_finite() => Ok(*number),
        _ => Err(error("RepX Particle buffer requires finite PhysX scalars")),
    }
}

fn word(value: &Value) -> Result<u32, Error> {
    match value {
        Value::U64(number) => {
            u32::try_from(*number).map_err(|_| error("RepX Particle word exceeds u32"))
        }
        _ => Err(error("RepX Particle buffer requires unsigned words")),
    }
}

fn quantized_grid(grid: f32, cell: f32) -> Result<f32, Error> {
    let ratio = (grid / cell).ceil();
    if !ratio.is_finite() || !(1.0..=1_073_741_824.0).contains(&ratio) {
        return Err(error("RepX Particle grid exceeds the supported SDK range"));
    }
    Ok((ratio as u32).next_power_of_two().max(4) as f32 * cell)
}

fn validate(object: &Object, budget: &mut usize) -> Result<Vec<usize>, Error> {
    let fluid = kind(object) == "PxParticleFluid";
    let count = integer(object, "NbParticles")?;
    let mut fields = FIELDS.to_vec();
    if count == 0 {
        fields.push("CollisionNormals");
        if fluid {
            fields.push("Densities");
        }
    }
    validate_fields(object, &fields, if fluid { FLUID_FIELDS } else { &[] })?;
    if !supported(kind(object))
        || !matches!(value(object, "ConcreteTypeName")?, Value::String(name) if name == kind(object))
        || !matches!(value(object, "Name")?, Value::String(_))
    {
        return Err(error(
            "RepX requires a captured ParticleSystem or ParticleFluid type and name",
        ));
    }
    if integer(object, "DominanceGroup")? > 31
        || integer(object, "OwnerClient")? > 255
        || integer(object, "ClientBehaviorFlags")? & !15 != 0
        || integer(object, "ActorFlags")? & !7 != 0
    {
        return Err(error(
            "RepX Particle actor flags, dominance or client are unsupported",
        ));
    }
    let base = integer(object, "ParticleBaseFlags")?;
    let per_rest = base & 16 != 0;
    // The official importer sets present flags but never clears constructor flags.
    if base & 38 != 38 || base & !63 != 0 {
        return Err(error(
            "RepX Particle requires CPU flags with collision, enabled and cache hint retained",
        ));
    }
    let read_flags = integer(object, "ParticleReadDataFlags")?;
    let read_flags_supported = if count == 0 {
        read_flags & 9 == 9
            && read_flags & !(if fluid { 127 } else { 63 }) == 0
            && (per_rest || read_flags & 4 == 0)
    } else {
        read_flags == if per_rest { 15 } else { 11 }
    };
    if !read_flags_supported {
        return Err(error(
            "RepX Particle read flags must retain constructor defaults and provide all active particle buffers",
        ));
    }
    let capacity = integer(object, "MaxParticles")?;
    let range = integer(object, "ValidParticleRange")?;
    if count > range
        || (count == 0 && range != 0)
        || range > capacity
        || !(1..=1_000_000).contains(&capacity)
    {
        return Err(error(
            "RepX Particle requires consistent valid indices and capacity between 1 and 1000000",
        ));
    }
    let count = count as usize;
    let range = range as usize;
    let required = capacity as usize * 64 + range.max(1) * 256;
    if required > *budget {
        return Err(error("RepX Particle exceeds the export resource budget"));
    }
    // 空帧只刷新数量与范围，SDK 可保留上一帧的数组属性。
    let indices = if count == 0 {
        Vec::new()
    } else {
        validate_buffers(object, count, range, capacity as usize, per_rest)?
    };
    for field in [
        "Damping",
        "StaticFriction",
        "MaxMotionDistance",
        "RestOffset",
        "ContactOffset",
    ] {
        if real(object, field)? < 0.0 {
            return Err(error(format!("RepX Particle {field} must be non-negative")));
        }
    }
    if real(object, "ParticleMass")? <= 0.0 || real(object, "GridSize")? <= 0.0 {
        return Err(error("RepX Particle mass and grid size must be positive"));
    }
    for field in ["Restitution", "DynamicFriction"] {
        if !(0.0..=1.0).contains(&real(object, field)?) {
            return Err(error(format!(
                "RepX Particle {field} must lie between zero and one"
            )));
        }
    }
    vector(object, "ExternalAcceleration")?;
    let plane = array(object, "ProjectionPlane", 4)?;
    let components = plane.iter().map(scalar).collect::<Result<Vec<_>, _>>()?;
    if components[..3].iter().all(|number| *number == 0.0) {
        return Err(error(
            "RepX Particle projection plane requires a nonzero normal",
        ));
    }
    for value in array(object, "SimulationFilterData", 4)? {
        word(value)?;
    }
    let grid = real(object, "GridSize")?;
    // SDK metadata applies GridSize before Fluid.RestParticleDistance, quantizing it twice.
    let imported_grid = if fluid {
        for field in FLUID_FIELDS {
            if real(object, field)? <= 0.0 {
                return Err(error(format!("RepX Particle {field} must be positive")));
            }
        }
        quantized_grid(
            quantized_grid(grid, 0.02 * 2.0)?,
            real(object, "RestParticleDistance")? * 2.0,
        )?
    } else {
        quantized_grid(grid, 0.06)?
    };
    if imported_grid != grid {
        return Err(error(
            "RepX SDK importer would change the captured Particle grid size",
        ));
    }
    *budget -= required;
    Ok(indices)
}

fn validate_buffers(
    object: &Object,
    count: usize,
    range: usize,
    capacity: usize,
    per_rest: bool,
) -> Result<Vec<usize>, Error> {
    let words = range.div_ceil(32);
    let bitmap = match value(object, "ValidParticleBitmap")? {
        Value::Array(values)
            if values.len() == words || (range.is_multiple_of(32) && values.len() == words + 1) =>
        {
            values
        }
        _ => {
            return Err(error(
                "RepX Particle bitmap length disagrees with its range",
            ));
        }
    };
    let mut indices = Vec::with_capacity(count);
    for (index, bits) in bitmap.iter().enumerate() {
        let mut bits = word(bits)?;
        // SDK 在整字容量边界多发送一个 word，其内容不属于粒子 bitmap。
        if index == words && range == capacity && range.is_multiple_of(32) {
            continue;
        }
        let remaining = range.saturating_sub(index * 32).min(32);
        let allowed = if remaining == 32 {
            u32::MAX
        } else {
            (1u32 << remaining) - 1
        };
        if bits & !allowed != 0 {
            return Err(error(
                "RepX Particle bitmap has active bits outside its range",
            ));
        }
        while bits != 0 {
            indices.push(index * 32 + bits.trailing_zeros() as usize);
            bits &= bits - 1;
        }
    }
    if indices.len() != count || indices.last() != Some(&(range - 1)) {
        return Err(error(
            "RepX Particle bitmap count or highest index disagrees with its range",
        ));
    }
    for field in ["Positions", "Velocities"] {
        for value in array(object, field, count)? {
            if !matches!(value, Value::Vec3(vector) if vector.iter().all(|number| number.is_finite()))
            {
                return Err(error(
                    "RepX Particle requires complete finite position and velocity vectors",
                ));
            }
        }
    }
    // ParticleCreationData flags are read-only SDK results; collision flags cannot be restored.
    if array(object, "Flags", count)?
        .iter()
        .any(|value| !matches!(value, Value::U64(1)))
    {
        return Err(error(
            "RepX Particle can restore only eVALID particle flags",
        ));
    }
    if per_rest {
        let maximum = real(object, "RestOffset")?;
        for value in array(object, "RestOffsets", count)? {
            if !(0.0..=maximum).contains(&scalar(value)?) {
                return Err(error(
                    "RepX Particle rest offset exceeds the captured common rest offset",
                ));
            }
        }
    } else if object.properties.contains_key("RestOffsets") {
        return Err(error(
            "RepX Particle has rest offsets without the per-particle flag",
        ));
    }
    Ok(indices)
}

fn write_buffer(
    xml: &mut String,
    object: &Object,
    field: &str,
    indices: &[usize],
    range: usize,
    unused: &str,
) -> Result<(), Error> {
    let values = array(object, field, indices.len())?;
    let mut active = indices.iter().zip(values).peekable();
    write!(xml, "<{field}>").unwrap();
    for index in 0..range {
        if active
            .peek()
            .is_some_and(|(expected, _)| **expected == index)
        {
            let (_, value) = active.next().unwrap();
            write!(xml, "{} ", value_text(value)?).unwrap();
        } else {
            // The importer only reads bitmap-selected slots; padding creates no particles.
            write!(xml, "{unused} ").unwrap();
        }
    }
    writeln!(xml, "</{field}>").unwrap();
    Ok(())
}

pub(super) fn write(
    xml: &mut String,
    state: &SceneState,
    object: &Object,
    budget: &mut usize,
) -> Result<(), Error> {
    let indices = validate(object, budget)?;
    let range = integer(object, "ValidParticleRange")? as usize;
    let field = if kind(object) == "PxParticleFluid" {
        "ParticleFluids"
    } else {
        "ParticleSystems"
    };
    let scene = references(state, object, "Scene")?;
    if scene.len() != 1
        || kind(scene[0]) != "PxScene"
        || !matches!(scene[0].properties.get(field), Some(Value::Array(values)) if values.iter().filter(|value| **value == Value::Ref(object.id)).count() == 1)
        || state
            .resolve_reference(scene[0].id, field, object.id)
            .is_none()
    {
        return Err(error("RepX Particle and Scene relationships disagree"));
    }
    writeln!(xml, "<{}>", kind(object)).unwrap();
    tag(xml, "Id", &object.id.to_string())?;
    property(xml, object, "NbParticles")?;
    if indices.is_empty() {
        // 官方读取器要求非零扫描范围；全零 bitmap 不创建粒子。
        tag(xml, "ValidParticleRange", "1")?;
        tag(xml, "ValidParticleBitmap", "0")?;
        if integer(object, "ParticleBaseFlags")? & 16 != 0 {
            tag(xml, "RestOffsets", "0")?;
        }
    } else {
        property(xml, object, "ValidParticleRange")?;
        let mut bitmap = vec![0u32; range.div_ceil(32)];
        for index in &indices {
            bitmap[index / 32] |= 1 << (index % 32);
        }
        tag(
            xml,
            "ValidParticleBitmap",
            &bitmap
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(" "),
        )?;
        for field in ["Positions", "Velocities"] {
            write_buffer(xml, object, field, &indices, range, "0 0 0")?;
        }
        if integer(object, "ParticleBaseFlags")? & 16 != 0 {
            write_buffer(xml, object, "RestOffsets", &indices, range, "0")?;
        }
        tag(xml, "Flags", &vec!["eVALID"; range].join(" "))?;
    }
    property(xml, object, "MaxParticles")?;
    flag_property(
        xml,
        object,
        "ParticleBaseFlags",
        &[
            "eCOLLISION_TWOWAY",
            "eCOLLISION_WITH_DYNAMIC_ACTORS",
            "eENABLED",
            "ePROJECT_TO_PLANE",
            "ePER_PARTICLE_REST_OFFSET",
            "ePER_PARTICLE_COLLISION_CACHE_HINT",
        ],
    )?;
    flag_property(
        xml,
        object,
        "ParticleReadDataFlags",
        &[
            "ePOSITION_BUFFER",
            "eVELOCITY_BUFFER",
            "eREST_OFFSET_BUFFER",
            "eFLAGS_BUFFER",
            "eCOLLISION_NORMAL_BUFFER",
            "eCOLLISION_VELOCITY_BUFFER",
            "eDENSITY_BUFFER",
        ],
    )?;
    property(xml, object, "Name")?;
    flag_property(
        xml,
        object,
        "ActorFlags",
        &["eVISUALIZATION", "eDISABLE_GRAVITY", "eSEND_SLEEP_NOTIFIES"],
    )?;
    flag_property(
        xml,
        object,
        "ClientBehaviorFlags",
        &[
            "eREPORT_TO_FOREIGN_CLIENTS_TRIGGER_NOTIFY",
            "eREPORT_TO_FOREIGN_CLIENTS_CONTACT_NOTIFY",
            "eREPORT_TO_FOREIGN_CLIENTS_CONSTRAINT_BREAK_NOTIFY",
            "eREPORT_TO_FOREIGN_CLIENTS_SCENE_QUERY",
        ],
    )?;
    for field in [
        "DominanceGroup",
        "OwnerClient",
        "Damping",
        "ExternalAcceleration",
        "ParticleMass",
        "Restitution",
        "DynamicFriction",
        "StaticFriction",
        "SimulationFilterData",
        "MaxMotionDistance",
        "RestOffset",
        "ContactOffset",
        "GridSize",
        "ProjectionPlane",
    ] {
        property(xml, object, field)?;
    }
    if kind(object) == "PxParticleFluid" {
        for field in FLUID_FIELDS {
            property(xml, object, field)?;
        }
    }
    writeln!(xml, "</{}>", kind(object)).unwrap();
    Ok(())
}
