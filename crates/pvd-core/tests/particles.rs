use pvd_core::{
    Object, Recording, SceneState, Value,
    particles::{self, Kind, Limits},
};
use serde_json::Value as Json;
use std::{collections::BTreeMap, fs, path::PathBuf, sync::Arc};

fn array(values: Vec<Value>) -> Value {
    Value::Array(Arc::new(values))
}

fn structural_state() -> SceneState {
    let properties = BTreeMap::from([
        ("NbParticles".into(), Value::U64(2)),
        ("ValidParticleRange".into(), Value::U64(64)),
        ("MaxParticles".into(), Value::U64(128)),
        ("ParticleBaseFlags".into(), Value::U64(54)),
        ("ParticleReadDataFlags".into(), Value::U64(31)),
        ("RestOffset".into(), Value::F32(0.1)),
        ("Scene".into(), Value::Ref(0)),
        (
            "ValidParticleBitmap".into(),
            array(vec![Value::U64(1), Value::U64(1 << 31), Value::U64(0)]),
        ),
        (
            "Positions".into(),
            array(vec![
                Value::Vec3([1.0, 2.0, 3.0]),
                Value::Vec3([4.0, 5.0, 6.0]),
            ]),
        ),
        ("Velocities".into(), array(vec![Value::Vec3([0.0; 3]); 2])),
        ("RestOffsets".into(), array(vec![Value::F32(0.05); 2])),
        (
            "CollisionNormals".into(),
            array(vec![Value::Vec3([0.0; 3]); 2]),
        ),
        ("Flags".into(), array(vec![Value::U64(1), Value::U64(3)])),
    ]);
    let mut state = SceneState::default();
    state.objects.insert(
        17,
        Object {
            id: 17,
            generation: 4,
            type_name: "physx3::PxParticleSystem".into(),
            properties,
        },
    );
    state
}

#[test]
fn sparse_bitmap_boundaries_optional_data_and_empty_state_are_explicit() {
    let mut state = structural_state();
    let result = particles::collect(&state);
    assert!(result.diagnostics.is_empty());
    assert_eq!(result.systems[0].actor_generation, 4);
    assert_eq!(
        result.systems[0]
            .particles
            .iter()
            .map(|value| value.index)
            .collect::<Vec<_>>(),
        [0, 63]
    );
    assert_eq!(result.systems[0].particles[1].position, [4.0, 5.0, 6.0]);
    state
        .objects
        .get_mut(&17)
        .unwrap()
        .properties
        .remove("Velocities");
    let missing = particles::collect(&state);
    assert_eq!(missing.systems.len(), 1);
    assert!(
        missing.systems[0]
            .particles
            .iter()
            .all(|value| value.velocity.is_none())
    );
    assert_eq!(missing.diagnostics.len(), 1);
    assert!(
        missing.diagnostics[0]
            .message
            .contains("Velocities was not captured")
    );
    let properties = &mut state.objects.get_mut(&17).unwrap().properties;
    properties.insert("NbParticles".into(), Value::U64(0));
    properties.insert("ValidParticleRange".into(), Value::U64(0));
    properties.insert("ValidParticleBitmap".into(), Value::Bytes(Arc::from([255])));
    properties.insert("Positions".into(), Value::F32(f32::NAN));
    let empty = particles::collect(&state);
    assert!(empty.diagnostics.is_empty());
    assert!(empty.systems[0].particles.is_empty());
}

#[test]
fn malformed_particle_fields_reject_the_system_without_consuming_other_budget() {
    for (property, value) in [
        ("NbParticles", Value::U64(3)),
        ("NbParticles", Value::I64(2)),
        ("ValidParticleRange", Value::U64(129)),
        ("MaxParticles", Value::U64(u32::MAX.into())),
        (
            "ValidParticleBitmap",
            array(vec![Value::U64(1), Value::U64(1 << 31)]),
        ),
        (
            "ValidParticleBitmap",
            array(vec![Value::U64(1), Value::U64(0), Value::U64(1)]),
        ),
        (
            "ValidParticleBitmap",
            array(vec![Value::U64(1), Value::U64(1 << 30), Value::U64(0)]),
        ),
        (
            "ValidParticleBitmap",
            array(vec![Value::U64(3), Value::U64(1 << 31), Value::U64(0)]),
        ),
        (
            "Positions",
            array(vec![Value::Vec3([f32::NAN, 0.0, 0.0]); 2]),
        ),
        ("Positions", array(vec![Value::Vec3([0.0; 3])])),
        (
            "Velocities",
            array(vec![Value::Vec3([f32::INFINITY; 3]); 2]),
        ),
        ("RestOffset", Value::F32(-0.1)),
        ("RestOffsets", array(vec![Value::F32(0.2); 2])),
        ("Flags", array(vec![Value::U64(0); 2])),
        ("Flags", array(vec![Value::U64(33); 2])),
        ("ParticleBaseFlags", Value::U64(128)),
        ("Scene", Value::Ref(999)),
    ] {
        let mut state = structural_state();
        let mut valid = state.objects[&17].clone();
        valid.id = 18;
        state.objects.insert(18, valid);
        state
            .objects
            .get_mut(&17)
            .unwrap()
            .properties
            .insert(property.into(), value);
        let result = particles::collect_with_limits(
            &state,
            Limits {
                max_particles: 2,
                max_systems: 2,
            },
        );
        assert_eq!(result.systems.len(), 1, "{property}");
        assert_eq!(result.systems[0].actor, 18, "{property}");
        assert_eq!(result.diagnostics.len(), 1, "{property}");
        assert_eq!(result.diagnostics[0].actor, 17);
    }
    let state = structural_state();
    let limited = particles::collect_with_limits(
        &state,
        Limits {
            max_particles: 1,
            max_systems: 2,
        },
    );
    assert!(limited.systems.is_empty());
    assert_eq!(limited.diagnostics.len(), 1);
    let limited = particles::collect_with_limits(
        &state,
        Limits {
            max_particles: 5,
            max_systems: 0,
        },
    );
    assert!(limited.systems.is_empty());
    assert_eq!(limited.diagnostics.len(), 1);
}

#[test]
fn unavailable_buffers_disabled_updates_and_gpu_paths_are_not_inferred() {
    let mut state = structural_state();
    let properties = &mut state.objects.get_mut(&17).unwrap().properties;
    properties.insert("ParticleBaseFlags".into(), Value::U64(38));
    properties.insert("ParticleReadDataFlags".into(), Value::U64(11));
    properties.insert("CollisionNormals".into(), Value::F32(f32::NAN));
    properties.insert("RestOffsets".into(), Value::F32(f32::NAN));
    let unavailable = particles::collect(&state);
    assert!(unavailable.diagnostics.is_empty());
    assert!(
        unavailable.systems[0]
            .particles
            .iter()
            .all(|particle| particle.rest_offset.is_none()
                && particle.collision_normal.is_none()
                && particle.density.is_none())
    );
    state
        .objects
        .get_mut(&17)
        .unwrap()
        .properties
        .insert("ParticleBaseFlags".into(), Value::U64(34));
    let disabled = particles::collect(&state);
    assert!(!disabled.systems[0].enabled());
    assert_eq!(disabled.diagnostics.len(), 1);
    assert!(
        disabled.diagnostics[0]
            .message
            .contains("last captured state")
    );
    state
        .objects
        .get_mut(&17)
        .unwrap()
        .properties
        .insert("ParticleBaseFlags".into(), Value::U64(102));
    let gpu = particles::collect(&state);
    assert!(gpu.systems.is_empty());
    assert!(
        gpu.diagnostics[0]
            .message
            .contains("GPU particle state is not validated")
    );
}

fn number(actual: f32, expected: &Json) {
    let expected = expected.as_f64().unwrap() as f32;
    assert!(
        (actual - expected).abs() <= 1e-6 * expected.abs().max(1.0),
        "{actual} != {expected}"
    );
}
fn vector(actual: [f32; 3], expected: &Json) {
    for (index, value) in actual.into_iter().enumerate() {
        number(value, &expected[index]);
    }
}
fn optional_real(actual: Option<f32>, expected: &Json) {
    if expected.is_null() {
        assert!(actual.is_none());
    } else {
        number(actual.unwrap(), expected);
    }
}
fn optional_vector(actual: Option<[f32; 3]>, expected: &Json) {
    if expected.is_null() {
        assert!(actual.is_none());
    } else {
        vector(actual.unwrap(), expected);
    }
}

#[test]
#[ignore = "requires official PhysX 3.4.2 CPU particle producer capture and SDK lockParticleReadData truth"]
fn real_sdk_particles_match_sparse_readback_and_replay_lifetimes() {
    let path = std::env::var_os("PVD_PARTICLE_CAPTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../captures/physx-3.4-particles.pvdstream")
        });
    let bytes = fs::read(&path).unwrap();
    let truth: Json =
        serde_json::from_slice(&fs::read(path.with_extension("particles.json")).unwrap()).unwrap();
    let recording = Recording::from_bytes(&bytes).unwrap();
    assert_eq!(recording.frame_count(), 10);
    recording.verify_replay().unwrap();
    let mut contacts = 0;
    let mut fluid_density = 0;
    for frame in [9, 0, 6, 1, 4, 8, 2, 5, 7, 3] {
        let state = recording.state_at(frame).unwrap();
        let actual = particles::collect(&state);
        assert!(
            actual.diagnostics.is_empty(),
            "frame {frame}: {:?}",
            actual.diagnostics
        );
        assert_eq!(actual.systems.len(), 2);
        for expected in truth["frames"][frame]["systems"].as_array().unwrap() {
            let system = actual
                .systems
                .iter()
                .find(|system| Some(system.actor) == expected["actor"].as_u64())
                .unwrap();
            assert_eq!(
                system.kind,
                if expected["kind"] == "Fluid" {
                    Kind::Fluid
                } else {
                    Kind::System
                }
            );
            assert_eq!(system.scene, expected["scene"].as_u64());
            assert_eq!(
                system.max_particles as u64,
                expected["max_particles"].as_u64().unwrap()
            );
            assert_eq!(
                system.valid_range as u64,
                expected["valid_range"].as_u64().unwrap()
            );
            assert_eq!(
                system.base_flags as u64,
                expected["base_flags"].as_u64().unwrap()
            );
            assert_eq!(
                system.read_flags as u64,
                expected["read_flags"].as_u64().unwrap()
            );
            assert_eq!(
                system.particles.len() as u64,
                expected["count"].as_u64().unwrap()
            );
            number(system.rest_offset, &expected["rest_offset"]);
            optional_real(
                system.rest_particle_distance,
                &expected["rest_particle_distance"],
            );
            for (particle, expected) in system
                .particles
                .iter()
                .zip(expected["particles"].as_array().unwrap())
            {
                assert_eq!(particle.index as u64, expected["index"].as_u64().unwrap());
                vector(particle.position, &expected["position"]);
                optional_vector(particle.velocity, &expected["velocity"]);
                optional_real(particle.rest_offset, &expected["rest_offset"]);
                optional_vector(particle.collision_normal, &expected["collision_normal"]);
                optional_real(particle.density, &expected["density"]);
                assert_eq!(particle.flags.map(u64::from), expected["flags"].as_u64());
                if particle.flags.is_some_and(|flags| flags & 2 != 0) {
                    contacts += 1;
                }
                if particle.density.is_some_and(|density| density > 0.0) {
                    fluid_density += 1;
                }
            }
        }
    }
    assert!(
        contacts > 0,
        "fixture must contain actual static collisions"
    );
    assert!(
        fluid_density > 0,
        "fixture must contain actual fluid density output"
    );
    let indices = |frame, kind| {
        particles::collect(&recording.state_at(frame).unwrap())
            .systems
            .into_iter()
            .find(|system| system.kind == kind)
            .unwrap()
            .particles
            .into_iter()
            .map(|particle| particle.index)
            .collect::<Vec<_>>()
    };
    assert!(indices(1, Kind::System).contains(&7));
    assert!(!indices(2, Kind::System).contains(&7));
    assert!(indices(3, Kind::System).contains(&7));
    assert!(indices(4, Kind::System).is_empty());
    assert_eq!(indices(5, Kind::System), [31]);
    assert_eq!(indices(6, Kind::System), [31, 32, 63]);
    assert_eq!(indices(7, Kind::System), [32, 63]);
    assert!(indices(5, Kind::Fluid).is_empty());
    assert_eq!(indices(6, Kind::Fluid), [0, 31, 32]);
    assert_eq!(indices(7, Kind::Fluid), [31, 32]);
    let clipped =
        Recording::from_bytes(&pvd_core::clip::trim_scene_frames(&bytes, 2, 8).unwrap()).unwrap();
    for frame in (0..7).rev() {
        assert_eq!(
            particles::collect(&clipped.state_at(frame).unwrap()),
            particles::collect(&recording.state_at(frame + 2).unwrap())
        );
    }
    let mut stale = recording.state_at(0).unwrap();
    let scene = particles::collect(&stale).systems[0].scene.unwrap();
    stale.objects.get_mut(&scene).unwrap().generation += 1;
    let invalid = particles::collect(&stale);
    assert!(invalid.systems.is_empty());
    assert_eq!(invalid.diagnostics.len(), 2);
    eprintln!(
        "10 SDK frames: positions, velocities, rest offsets, flags, collision normals and fluid densities matched; {contacts} colliding particles, {fluid_density} nonzero densities"
    );
}

#[test]
fn selected_sources_preserve_system_limit_and_diagnostic_order() {
    let mut state = structural_state();
    let template = state.objects.remove(&17).unwrap();
    for id in 0..=(particles::MAX_SYSTEMS as u64) {
        let mut object = template.clone();
        object.id = id;
        state.objects.insert(id, object);
    }
    let mut selected: std::collections::BTreeSet<_> = state.objects.keys().copied().collect();
    selected.insert(u64::MAX);
    let complete = particles::collect(&state);
    assert_eq!(complete, particles::collect_from_ids(&state, &selected));
    assert_eq!(complete.systems.len(), particles::MAX_SYSTEMS);
    assert_eq!(complete.diagnostics.len(), 1);
    assert_eq!(complete.diagnostics[0].actor, particles::MAX_SYSTEMS as u64);
    assert!(complete.diagnostics[0].message.contains("system count"));
}
