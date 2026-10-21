use crate::{Error, Object, SceneState, Value, error, rotate};
use pvd_protocol::{Endian, Header};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

const MAX_CLOTHS: usize = 4096;
const MAX_PARTICLES: usize = 1_000_000;
const MAX_CONSTRAINTS: usize = 1_500_000;
const MAX_BYTES: usize = 128 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct Particle {
    pub position: [f32; 3],
    pub inverse_mass: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Constraint {
    pub particles: [u32; 2],
    pub rest_length: f32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Phase {
    pub kind: u32,
    pub set_index: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fabric {
    pub id: u64,
    pub generation: u32,
    pub particle_count: u32,
    pub constraints: Vec<Constraint>,
    pub sets: Vec<u32>,
    pub phases: Vec<Phase>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cloth {
    pub id: u64,
    pub generation: u32,
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    pub sleeping: bool,
    pub particles: Arc<Vec<Particle>>,
    pub previous_particles: Option<Arc<Vec<Particle>>>,
    pub fabric: Arc<Fabric>,
}

impl Cloth {
    pub fn world_position(&self, index: usize) -> Option<[f32; 3]> {
        let local = self.particles.get(index)?.position;
        let rotated = rotate(self.rotation, local);
        Some(std::array::from_fn(|axis| {
            rotated[axis] + self.position[axis]
        }))
    }

    pub fn world_positions(&self) -> impl ExactSizeIterator<Item = [f32; 3]> + '_ {
        self.particles.iter().map(|particle| {
            let rotated = rotate(self.rotation, particle.position);
            std::array::from_fn(|axis| rotated[axis] + self.position[axis])
        })
    }

    pub fn world_constraints(&self) -> impl ExactSizeIterator<Item = [[f32; 3]; 2]> + '_ {
        self.fabric.constraints.iter().map(|constraint| {
            constraint.particles.map(|index| {
                let rotated = rotate(self.rotation, self.particles[index as usize].position);
                std::array::from_fn(|axis| rotated[axis] + self.position[axis])
            })
        })
    }
}

#[derive(Default, Debug, PartialEq)]
pub struct ClothFrame {
    pub cloths: Vec<Cloth>,
    pub particle_count: usize,
    pub constraint_count: usize,
    pub unique_fabric_count: usize,
    pub diagnostics: Vec<String>,
}

pub fn collect(state: &SceneState) -> ClothFrame {
    collect_with_limit(state, MAX_BYTES)
}

pub fn collect_from_ids(state: &SceneState, ids: &BTreeSet<u64>) -> ClothFrame {
    collect_objects(
        state,
        ids.iter().filter_map(|id| state.objects.get(id)),
        MAX_BYTES,
    )
}

fn collect_with_limit(state: &SceneState, byte_limit: usize) -> ClothFrame {
    collect_objects(state, state.objects.values(), byte_limit)
}

fn collect_objects<'a>(
    state: &SceneState,
    objects: impl Iterator<Item = &'a Object>,
    byte_limit: usize,
) -> ClothFrame {
    let mut result = ClothFrame::default();
    let mut cache = BTreeMap::<(u64, u32), Result<Arc<Fabric>, String>>::new();
    let mut budget = byte_limit;
    for (index, object) in objects
        .filter(|object| object.type_name.ends_with("::PxCloth"))
        .enumerate()
    {
        if index == MAX_CLOTHS {
            result.diagnostics.push(format!(
                "Cloth display stopped at its {MAX_CLOTHS} object limit."
            ));
            break;
        }
        let collected = (|| {
            let Value::Ref(id) = required(object, "Fabric")? else {
                return Err(error("Fabric is not an object reference"));
            };
            let fabric = state
                .resolve_reference(object.id, "Fabric", *id)
                .filter(|fabric| fabric.type_name.ends_with("::PxClothFabric"))
                .ok_or_else(|| {
                    error("Fabric reference is missing, stale or has an unsupported type")
                })?;
            let fabric = cache
                .entry((fabric.id, fabric.generation))
                .or_insert_with(|| {
                    decode_fabric(fabric, &mut budget)
                        .map(Arc::new)
                        .map_err(|e| e.to_string())
                })
                .as_ref()
                .map_err(error)?
                .clone();
            decode_cloth(object, fabric, &mut budget)
        })();
        match collected {
            Ok(cloth) => {
                result.particle_count += cloth.particles.len();
                result.constraint_count += cloth.fabric.constraints.len();
                result.cloths.push(cloth);
            }
            Err(problem) => result.diagnostics.push(format!(
                "PxCloth 0x{:X} generation {}: {problem}",
                object.id, object.generation
            )),
        }
    }
    result.unique_fabric_count = result
        .cloths
        .iter()
        .map(|cloth| (cloth.fabric.id, cloth.fabric.generation))
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    result
}

fn reserve(budget: &mut usize, bytes: usize) -> Result<(), Error> {
    *budget = budget
        .checked_sub(bytes)
        .ok_or_else(|| error("Cloth display exceeds its decoded allocation budget"))?;
    Ok(())
}

fn required<'a>(object: &'a Object, name: &str) -> Result<&'a Value, Error> {
    object
        .properties
        .get(name)
        .ok_or_else(|| error(format!("missing {name}")))
}

fn array(value: &Value) -> Result<&[Value], Error> {
    match value {
        Value::Array(values) => Ok(values),
        _ => Err(error(
            "unsupported cloth property layout: expected an array",
        )),
    }
}

fn uint(value: &Value) -> Result<u32, Error> {
    match value {
        Value::U64(value) => {
            u32::try_from(*value).map_err(|_| error("cloth index/count exceeds u32"))
        }
        _ => Err(error("unsupported cloth property layout: expected u32")),
    }
}

fn nonnegative(value: &Value) -> Result<f32, Error> {
    match value {
        Value::F32(value) if value.is_finite() && *value >= 0.0 => Ok(*value),
        _ => Err(error(
            "cloth mass/rest length must be finite and non-negative",
        )),
    }
}

fn decode_fabric(object: &Object, budget: &mut usize) -> Result<Fabric, Error> {
    let particle_count = uint(required(object, "NbParticles")?)?;
    if particle_count == 0 || particle_count as usize > MAX_PARTICLES {
        return Err(error(
            "Fabric particle count exceeds the 1,000,000 display limit or is zero",
        ));
    }
    let indices = array(required(object, "ParticleIndices")?)?;
    let rest = array(required(object, "Restvalues")?)?;
    let sets = array(required(object, "Sets")?)?;
    let phases = array(required(object, "Phases")?)?;
    if !indices.len().is_multiple_of(2) || indices.len() / 2 != rest.len() {
        return Err(error(
            "Fabric constraints require two indices per rest length",
        ));
    }
    if rest.len() > MAX_CONSTRAINTS
        || sets.len() > MAX_CONSTRAINTS
        || phases.len() > MAX_CONSTRAINTS
    {
        return Err(error(
            "Fabric exceeds the 1,500,000 constraint/phase/set display limit",
        ));
    }
    reserve(
        budget,
        std::mem::size_of::<Fabric>()
            + rest.len() * std::mem::size_of::<Constraint>()
            + sets.len() * std::mem::size_of::<u32>()
            + phases.len() * std::mem::size_of::<Phase>(),
    )?;
    let mut constraints = Vec::with_capacity(rest.len());
    for (pair, length) in indices.chunks_exact(2).zip(rest) {
        let particles = [uint(&pair[0])?, uint(&pair[1])?];
        if particles.iter().any(|index| *index >= particle_count) || particles[0] == particles[1] {
            return Err(error("Fabric constraint has an invalid particle index"));
        }
        constraints.push(Constraint {
            particles,
            rest_length: nonnegative(length)?,
        });
    }
    let sets = sets.iter().map(uint).collect::<Result<Vec<_>, _>>()?;
    if sets.windows(2).any(|values| values[0] > values[1])
        || sets.last().copied().unwrap_or(0) as usize != constraints.len()
    {
        return Err(error(
            "Fabric sets are not monotonic cumulative constraint offsets",
        ));
    }
    let mut decoded_phases = Vec::with_capacity(phases.len());
    for phase in phases {
        let fields = array(phase)?;
        if fields.len() != 2 {
            return Err(error("Fabric phase requires type and set index"));
        }
        let kind = uint(&fields[0])?;
        let set_index = uint(&fields[1])?;
        if !(1..=4).contains(&kind) || set_index as usize >= sets.len() {
            return Err(error("Fabric phase type or set index is unsupported"));
        }
        decoded_phases.push(Phase { kind, set_index });
    }
    Ok(Fabric {
        id: object.id,
        generation: object.generation,
        particle_count,
        constraints,
        sets,
        phases: decoded_phases,
    })
}

fn decode_cloth(object: &Object, fabric: Arc<Fabric>, budget: &mut usize) -> Result<Cloth, Error> {
    let count = uint(required(object, "NbParticles")?)?;
    if count != fabric.particle_count {
        return Err(error("Cloth and Fabric particle counts disagree"));
    }
    let recorded = array(required(object, "ParticleBuffer")?)?;
    if recorded.len() != count as usize {
        return Err(error("ParticleBuffer length disagrees with NbParticles"));
    }
    let Value::Transform { position, rotation } = required(object, "GlobalPose")? else {
        return Err(error("Cloth GlobalPose has an unsupported layout"));
    };
    if position.iter().chain(rotation).any(|v| !v.is_finite())
        || (rotation.iter().map(|v| v * v).sum::<f32>() - 1.0).abs() > 0.001
    {
        return Err(error("Cloth pose must be finite with a unit quaternion"));
    }
    let Value::Bool(sleeping) = required(object, "IsSleeping")? else {
        return Err(error("Cloth IsSleeping is not a boolean"));
    };
    reserve(
        budget,
        std::mem::size_of::<Cloth>() + recorded.len() * std::mem::size_of::<Particle>(),
    )?;
    let mut particles = Vec::with_capacity(recorded.len());
    for particle in recorded {
        let fields = array(particle)?;
        if fields.len() != 2 {
            return Err(error("Cloth particle requires position and inverse mass"));
        }
        let Value::Vec3(local) = fields[0] else {
            return Err(error("Cloth particle position is not a vector"));
        };
        if local.iter().any(|v| !v.is_finite()) {
            return Err(error("Cloth particle position is not finite"));
        }
        let world = rotate(*rotation, local);
        if world
            .iter()
            .zip(position)
            .any(|(a, b)| !(a + b).is_finite())
        {
            return Err(error("Cloth world position overflow"));
        }
        particles.push(Particle {
            position: local,
            inverse_mass: nonnegative(&fields[1])?,
        });
    }
    Ok(Cloth {
        id: object.id,
        generation: object.generation,
        position: *position,
        rotation: *rotation,
        sleeping: *sleeping,
        particles: Arc::new(particles),
        previous_particles: None,
        fabric,
    })
}

pub(crate) fn decode_property(
    typename: &str,
    data: &[u8],
    items: u32,
    array: bool,
    header: &Header,
) -> Result<Option<Value>, Error> {
    let stride = match typename {
        "physx3::PxClothParticle" => 16,
        "physx3::PxClothFabricPhase" => 8,
        _ => return Ok(None),
    };
    if items as usize > MAX_PARTICLES || data.len() != items as usize * stride {
        return Err(error(
            "Cloth property length disagrees with its record count or exceeds 1,000,000 records",
        ));
    }
    let mut values = Vec::with_capacity(items as usize);
    for bytes in data.chunks_exact(stride) {
        let uint = |offset: usize| {
            let bytes = bytes[offset..offset + 4].try_into().unwrap();
            match header.endian {
                Endian::Little => u32::from_le_bytes(bytes),
                Endian::Big => u32::from_be_bytes(bytes),
            }
        };
        let fields = if stride == 16 {
            vec![
                Value::Vec3(std::array::from_fn(|axis| f32::from_bits(uint(axis * 4)))),
                Value::F32(f32::from_bits(uint(12))),
            ]
        } else {
            vec![
                Value::U64(u64::from(uint(0))),
                Value::U64(u64::from(uint(4))),
            ]
        };
        values.push(Value::Array(fields.into()));
    }
    Ok(Some(if array || items != 1 {
        Value::Array(values.into())
    } else {
        values.remove(0)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Action;

    fn array(values: impl IntoIterator<Item = Value>) -> Value {
        Value::Array(values.into_iter().collect::<Vec<_>>().into())
    }

    fn sample() -> SceneState {
        let mut state = SceneState::default();
        Action::Create(Object {
            id: 3,
            generation: 0,
            type_name: "physx3::PxClothFabric".into(),
            properties: [
                ("NbParticles".into(), Value::U64(3)),
                (
                    "ParticleIndices".into(),
                    array([0, 1, 1, 2].map(Value::U64)),
                ),
                (
                    "Restvalues".into(),
                    array([Value::F32(1.0), Value::F32(2.0)]),
                ),
                ("Sets".into(), array([Value::U64(2)])),
                (
                    "Phases".into(),
                    array([array([Value::U64(1), Value::U64(0)])]),
                ),
            ]
            .into(),
        })
        .apply(&mut state);
        for id in [1, 2] {
            Action::Create(Object {
                id,
                generation: 0,
                type_name: "physx3::PxCloth".into(),
                properties: [
                    ("NbParticles".into(), Value::U64(3)),
                    (
                        "ParticleBuffer".into(),
                        array([0.0, 1.0, 3.0].map(|x| {
                            array([
                                Value::Vec3([x, 0.0, 0.0]),
                                Value::F32(if x == 0.0 { 0.0 } else { 0.5 }),
                            ])
                        })),
                    ),
                    (
                        "GlobalPose".into(),
                        Value::Transform {
                            position: [10.0, 1.0, -2.0],
                            rotation: [0.0, 0.0, 0.0, 1.0],
                        },
                    ),
                    ("IsSleeping".into(), Value::Bool(false)),
                ]
                .into(),
            })
            .apply(&mut state);
            Action::Set(id, "Fabric".into(), Value::Ref(3)).apply(&mut state);
        }
        state
    }

    #[test]
    fn particles_constraints_shared_fabric_and_allocation_budget_are_consistent() {
        let state = sample();
        let limit = std::mem::size_of::<Fabric>()
            + 2 * std::mem::size_of::<Constraint>()
            + std::mem::size_of::<u32>()
            + std::mem::size_of::<Phase>()
            + 2 * (std::mem::size_of::<Cloth>() + 3 * std::mem::size_of::<Particle>());
        let frame = collect_with_limit(&state, limit);
        assert!(frame.diagnostics.is_empty());
        assert_eq!(
            (
                frame.cloths.len(),
                frame.unique_fabric_count,
                frame.particle_count,
                frame.constraint_count
            ),
            (2, 1, 6, 4)
        );
        assert!(Arc::ptr_eq(
            &frame.cloths[0].fabric,
            &frame.cloths[1].fabric
        ));
        assert_eq!(
            frame.cloths[0].world_constraints().collect::<Vec<_>>(),
            [
                [[10.0, 1.0, -2.0], [11.0, 1.0, -2.0]],
                [[11.0, 1.0, -2.0], [13.0, 1.0, -2.0]],
            ]
        );
        assert!(frame.cloths[0].previous_particles.is_none());
        let partial = collect_with_limit(&state, limit - 1);
        assert_eq!(partial.cloths.len(), 1);
        assert_eq!(partial.diagnostics.len(), 1);
        assert!(partial.diagnostics[0].contains("allocation budget"));
        assert!(collect_with_limit(&state, 0).cloths.is_empty());
    }

    #[test]
    fn invalid_metadata_and_particles_are_reported_without_constructing_draw_data() {
        let original = sample();
        for (id, property, value) in [
            (3, "NbParticles", Value::U64(1_000_001)),
            (3, "ParticleIndices", array([0, 1, 2].map(Value::U64))),
            (3, "ParticleIndices", array([0, 1, 1, 3].map(Value::U64))),
            (3, "ParticleIndices", array([0, 0, 1, 2].map(Value::U64))),
            (
                3,
                "Restvalues",
                array([Value::F32(1.0), Value::F32(f32::NAN)]),
            ),
            (3, "Sets", array([3, 2].map(Value::U64))),
            (3, "Sets", array([Value::U64(1)])),
            (3, "Phases", array([array([Value::U64(5), Value::U64(0)])])),
            (3, "Phases", array([array([Value::U64(1), Value::U64(1)])])),
            (1, "NbParticles", Value::U64(2)),
            (1, "ParticleBuffer", Value::Bytes(Arc::from([0u8; 48]))),
            (
                1,
                "ParticleBuffer",
                array([array([Value::Vec3([0.0; 3]), Value::F32(0.0)])]),
            ),
            (
                1,
                "ParticleBuffer",
                array((0..3).map(|_| array([Value::Vec3([f32::INFINITY; 3]), Value::F32(1.0)]))),
            ),
            (
                1,
                "ParticleBuffer",
                array((0..3).map(|_| array([Value::Vec3([0.0; 3]), Value::F32(-1.0)]))),
            ),
            (
                1,
                "GlobalPose",
                Value::Transform {
                    position: [0.0; 3],
                    rotation: [0.0; 4],
                },
            ),
        ] {
            let mut state = original.clone();
            Action::Set(id, property.into(), value).apply(&mut state);
            let frame = collect(&state);
            assert!(!frame.diagnostics.is_empty(), "accepted {id}.{property}");
            assert!(frame.cloths.iter().all(|cloth| cloth.id != 1));
        }
        let mut state = original;
        let mut fabric = state.objects[&3].clone();
        Action::Destroy(3).apply(&mut state);
        fabric.generation = 1;
        Action::Create(fabric).apply(&mut state);
        assert!(collect(&state).cloths.is_empty());
        Action::Set(1, "Fabric".into(), Value::Ref(3)).apply(&mut state);
        let frame = collect(&state);
        assert_eq!(frame.cloths.len(), 1);
        assert_eq!(frame.cloths[0].fabric.generation, 1);
    }

    #[test]
    fn structure_decoding_handles_endian_and_rejects_truncated_or_excessive_records() {
        for endian in [Endian::Little, Endian::Big] {
            let header = Header {
                version: 1,
                timestamp_numerator: 1,
                timestamp_denominator: 1,
                flags: 1,
                endian,
            };
            let words = [
                (-1.25f32).to_bits(),
                2.0f32.to_bits(),
                3.5f32.to_bits(),
                0.0f32.to_bits(),
            ];
            let bytes: Vec<u8> = words
                .into_iter()
                .flat_map(|word| match endian {
                    Endian::Little => word.to_le_bytes(),
                    Endian::Big => word.to_be_bytes(),
                })
                .collect();
            assert_eq!(
                decode_property("physx3::PxClothParticle", &bytes, 1, true, &header).unwrap(),
                Some(array([array([
                    Value::Vec3([-1.25, 2.0, 3.5]),
                    Value::F32(0.0)
                ])]))
            );
            assert!(
                decode_property("physx3::PxClothParticle", &bytes[..15], 1, true, &header).is_err()
            );
            assert!(
                decode_property("physx3::PxClothParticle", &[], 1_000_001, true, &header).is_err()
            );
            let phase: Vec<u8> = [4u32, 7]
                .into_iter()
                .flat_map(|word| match endian {
                    Endian::Little => word.to_le_bytes(),
                    Endian::Big => word.to_be_bytes(),
                })
                .collect();
            assert_eq!(
                decode_property("physx3::PxClothFabricPhase", &phase, 1, false, &header).unwrap(),
                Some(array([Value::U64(4), Value::U64(7)]))
            );
            assert!(
                decode_property("physx3::PxClothFabricPhase", &phase[..7], 1, true, &header)
                    .is_err()
            );
            assert!(
                decode_property("other::PxClothParticle", &bytes, 1, true, &header)
                    .unwrap()
                    .is_none()
            );
        }
    }
    #[test]
    fn selected_sources_preserve_shared_fabric_budget_and_order() {
        let state = sample();
        let ids = BTreeSet::from([2, 1, 3, 777]);
        for limit in [0, 1, 500, 1000, MAX_BYTES] {
            assert_eq!(
                collect_with_limit(&state, limit),
                collect_objects(
                    &state,
                    ids.iter().filter_map(|id| state.objects.get(id)),
                    limit
                )
            );
        }
        assert_eq!(collect_from_ids(&state, &ids), collect(&state));
    }
}
