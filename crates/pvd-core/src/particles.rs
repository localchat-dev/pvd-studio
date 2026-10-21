use crate::{Error, Object, SceneState, Value, error};
use std::collections::BTreeSet;

pub const MAX_PARTICLES: usize = 1_000_000;
pub const MAX_SYSTEMS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    System,
    Fluid,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Particle {
    pub index: u32,
    pub position: [f32; 3],
    pub velocity: Option<[f32; 3]>,
    pub rest_offset: Option<f32>,
    pub collision_normal: Option<[f32; 3]>,
    pub density: Option<f32>,
    pub flags: Option<u16>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParticleSystem {
    pub actor: u64,
    pub actor_generation: u32,
    pub scene: Option<u64>,
    pub kind: Kind,
    pub max_particles: u32,
    pub valid_range: u32,
    pub base_flags: u16,
    pub read_flags: u16,
    pub rest_offset: f32,
    pub rest_particle_distance: Option<f32>,
    pub particles: Vec<Particle>,
}

impl ParticleSystem {
    pub fn enabled(&self) -> bool {
        self.base_flags & 4 != 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub actor: u64,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Collection {
    pub systems: Vec<ParticleSystem>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_particles: usize,
    pub max_systems: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_particles: MAX_PARTICLES,
            max_systems: MAX_SYSTEMS,
        }
    }
}

pub fn collect(state: &SceneState) -> Collection {
    collect_with_limits(state, Limits::default())
}

pub fn collect_from_ids(state: &SceneState, ids: &BTreeSet<u64>) -> Collection {
    collect_objects(
        state,
        ids.iter().filter_map(|id| state.objects.get(id)),
        Limits::default(),
    )
}

pub fn collect_with_limits(state: &SceneState, limits: Limits) -> Collection {
    collect_objects(state, state.objects.values(), limits)
}

fn collect_objects<'a>(
    state: &SceneState,
    objects: impl Iterator<Item = &'a Object>,
    limits: Limits,
) -> Collection {
    let mut output = Collection::default();
    let mut remaining = limits.max_particles.min(MAX_PARTICLES);
    let mut system_count = 0;
    for actor in objects {
        let kind = match actor.type_name.as_str() {
            "physx3::PxParticleSystem" => Kind::System,
            "physx3::PxParticleFluid" => Kind::Fluid,
            _ => continue,
        };
        if system_count >= limits.max_systems.min(MAX_SYSTEMS) {
            output.diagnostics.push(Diagnostic {
                actor: actor.id,
                message:
                    "particle system count exceeds the configured limit; remaining systems omitted"
                        .into(),
            });
            break;
        }
        system_count += 1;
        match system(state, actor, kind, remaining) {
            Ok((system, diagnostics)) => {
                remaining -= system.particles.len();
                output.systems.push(system);
                output.diagnostics.extend(diagnostics);
            }
            Err(error) => output.diagnostics.push(Diagnostic {
                actor: actor.id,
                message: error.to_string(),
            }),
        }
    }
    output
}

fn field<'a>(actor: &'a Object, property: &str) -> Result<&'a Value, Error> {
    actor
        .properties
        .get(property)
        .ok_or_else(|| error(format!("particle {property} is missing from the capture")))
}

fn unsigned(value: &Value, property: &str) -> Result<u32, Error> {
    match value {
        Value::U64(value) => {
            u32::try_from(*value).map_err(|_| error(format!("particle {property} exceeds u32")))
        }
        _ => Err(error(format!(
            "particle {property} is not an unsigned integer"
        ))),
    }
}

fn real(value: &Value, property: &str) -> Result<f32, Error> {
    match value {
        Value::F32(value) if value.is_finite() => Ok(*value),
        _ => Err(error(format!("particle {property} is not a finite f32"))),
    }
}

fn vector(value: &Value, property: &str) -> Result<[f32; 3], Error> {
    match value {
        Value::Vec3(value) if value.iter().all(|value| value.is_finite()) => Ok(*value),
        _ => Err(error(format!("particle {property} is not a finite vector"))),
    }
}

fn array<'a>(value: &'a Value, property: &str, count: usize) -> Result<&'a [Value], Error> {
    match value {
        Value::Array(values) if values.len() == count => Ok(values),
        _ => Err(error(format!(
            "particle {property} must contain exactly {count} packed entries"
        ))),
    }
}

fn optional_array<'a>(
    actor: &'a Object,
    property: &str,
    count: usize,
    expected: bool,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Option<&'a [Value]>, Error> {
    match actor.properties.get(property) {
        Some(value) => array(value, property, count).map(Some),
        None => {
            if expected {
                diagnostics.push(Diagnostic {
                    actor: actor.id,
                    message: format!(
                        "particle {property} was not captured; values are unavailable"
                    ),
                });
            }
            Ok(None)
        }
    }
}

fn available_array<'a>(
    actor: &'a Object,
    property: &str,
    count: usize,
    enabled: bool,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Option<&'a [Value]>, Error> {
    if enabled {
        optional_array(actor, property, count, true, diagnostics)
    } else {
        Ok(None)
    }
}

fn system(
    state: &SceneState,
    actor: &Object,
    kind: Kind,
    remaining: usize,
) -> Result<(ParticleSystem, Vec<Diagnostic>), Error> {
    let count = unsigned(field(actor, "NbParticles")?, "NbParticles")? as usize;
    let valid_range = unsigned(field(actor, "ValidParticleRange")?, "ValidParticleRange")?;
    let max_particles = unsigned(field(actor, "MaxParticles")?, "MaxParticles")?;
    if count > remaining
        || max_particles as usize > MAX_PARTICLES
        || valid_range > max_particles
        || count > valid_range as usize
        || (count == 0) != (valid_range == 0)
    {
        return Err(error(
            "particle count/range exceeds its capacity or configured particle limit",
        ));
    }
    let base_flags = u16::try_from(unsigned(
        field(actor, "ParticleBaseFlags")?,
        "ParticleBaseFlags",
    )?)?;
    let read_flags = u16::try_from(unsigned(
        field(actor, "ParticleReadDataFlags")?,
        "ParticleReadDataFlags",
    )?)?;
    if base_flags & !127 != 0 || read_flags & !127 != 0 {
        return Err(error("particle flags contain unsupported SDK bits"));
    }
    if base_flags & 64 != 0 {
        return Err(error(
            "GPU particle state is not validated; device-exclusive SDK access can omit PVD particle updates",
        ));
    }
    let rest_offset = real(field(actor, "RestOffset")?, "RestOffset")?;
    if rest_offset < 0.0 {
        return Err(error("particle RestOffset must be nonnegative"));
    }
    let rest_particle_distance = if kind == Kind::Fluid {
        let value = real(
            field(actor, "RestParticleDistance")?,
            "RestParticleDistance",
        )?;
        if value <= 0.0 {
            return Err(error("fluid RestParticleDistance must be positive"));
        }
        Some(value)
    } else {
        None
    };
    let scene = match field(actor, "Scene")? {
        Value::Ref(0) => None,
        Value::Ref(id)
            if state
                .resolve_reference(actor.id, "Scene", *id)
                .is_some_and(|scene| scene.type_name == "physx3::PxScene") =>
        {
            Some(*id)
        }
        _ => {
            return Err(error(
                "particle Scene reference is invalid, stale, or unavailable",
            ));
        }
    };
    let mut output = ParticleSystem {
        actor: actor.id,
        actor_generation: actor.generation,
        scene,
        kind,
        max_particles,
        valid_range,
        base_flags,
        read_flags,
        rest_offset,
        rest_particle_distance,
        particles: Vec::new(),
    };
    let mut diagnostics = Vec::new();
    if !output.enabled() {
        diagnostics.push(Diagnostic { actor: actor.id, message: "particle system is disabled; SDK state updates stop and these values are the last captured state".into() });
    }
    if count == 0 {
        return Ok((output, diagnostics));
    }
    let words = (valid_range as usize >> 5) + 1;
    let bitmap = array(
        field(actor, "ValidParticleBitmap")?,
        "ValidParticleBitmap",
        words,
    )?;
    let mut indices = Vec::with_capacity(count);
    for (word_index, value) in bitmap.iter().enumerate() {
        let mut bits = unsigned(value, "ValidParticleBitmap")?;
        while bits != 0 {
            let index = word_index as u32 * 32 + bits.trailing_zeros();
            if index >= valid_range || indices.len() >= count {
                return Err(error(
                    "particle bitmap has set bits outside its declared range/count",
                ));
            }
            indices.push(index);
            bits &= bits - 1;
        }
    }
    if indices.len() != count || indices.last().copied() != valid_range.checked_sub(1) {
        return Err(error(
            "particle bitmap population or highest index differs from NbParticles/ValidParticleRange",
        ));
    }
    let positions = array(field(actor, "Positions")?, "Positions", count)?;
    let velocities = optional_array(actor, "Velocities", count, true, &mut diagnostics)?;
    let offsets = available_array(
        actor,
        "RestOffsets",
        count,
        base_flags & 16 != 0,
        &mut diagnostics,
    )?;
    let normals = available_array(
        actor,
        "CollisionNormals",
        count,
        read_flags & 16 != 0,
        &mut diagnostics,
    )?;
    let flags = optional_array(actor, "Flags", count, true, &mut diagnostics)?;
    let densities = available_array(
        actor,
        "Densities",
        count,
        kind == Kind::Fluid && read_flags & 64 != 0,
        &mut diagnostics,
    )?;
    output.particles.reserve(count);
    for (packed, index) in indices.into_iter().enumerate() {
        let rest = offsets
            .map(|array| real(&array[packed], "RestOffsets"))
            .transpose()?;
        if rest.is_some_and(|value| value < 0.0 || value > rest_offset) {
            return Err(error(
                "particle RestOffsets entry lies outside [0, RestOffset]",
            ));
        }
        let density = densities
            .map(|array| real(&array[packed], "Densities"))
            .transpose()?;
        if density.is_some_and(|value| value < 0.0) {
            return Err(error("particle density must be nonnegative"));
        }
        let flags = flags
            .map(|array| unsigned(&array[packed], "Flags"))
            .transpose()?;
        if flags.is_some_and(|value| value & 1 == 0 || value & !31 != 0) {
            return Err(error(
                "particle Flags entry is invalid or contains unsupported bits",
            ));
        }
        output.particles.push(Particle {
            index,
            position: vector(&positions[packed], "Positions")?,
            velocity: velocities
                .map(|array| vector(&array[packed], "Velocities"))
                .transpose()?,
            rest_offset: rest,
            collision_normal: normals
                .map(|array| vector(&array[packed], "CollisionNormals"))
                .transpose()?,
            density,
            flags: flags.map(|value| value as u16),
        });
    }
    Ok((output, diagnostics))
}
