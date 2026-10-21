use pvd_core::{Recording, Value, cloth};
use serde_json::Value as Json;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    sync::Arc,
};

fn close(actual: &[f32], expected: &Json) {
    let expected = expected.as_array().unwrap();
    assert_eq!(actual.len(), expected.len());
    for (a, b) in actual.iter().zip(expected) {
        let b = b.as_f64().unwrap() as f32;
        assert!((a - b).abs() < 0.00002, "{a} != {b}");
    }
}

#[test]
#[ignore = "requires official SDK 3.4 pvd-cloth particle and constraint getter truth"]
fn current_particles_and_constraint_graph_match_sdk_across_replay_and_lifetimes() {
    let capture = std::env::var_os("PVD_CLOTH_CAPTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../captures/physx-3.4-cloth.pvdstream")
        });
    let bytes = fs::read(&capture).unwrap();
    let recording = Recording::from_bytes(&bytes).unwrap();
    let truth: Json =
        serde_json::from_str(&fs::read_to_string(capture.with_extension("cloth.json")).unwrap())
            .unwrap();
    assert_eq!(recording.frame_count(), 75);
    assert_eq!(truth["frames"].as_array().unwrap().len(), 75);
    recording.verify_replay().unwrap();
    let cloth_class = &recording.classes["physx3::PxCloth"];
    assert!(cloth_class.properties.contains_key("ParticleBuffer"));
    assert!(!cloth_class.properties.contains_key("PreviousParticles"));
    let fabric_class = &recording.classes["physx3::PxClothFabric"];
    assert!(!fabric_class.properties.contains_key("Triangles"));
    assert!(!fabric_class.properties.contains_key("Quads"));
    let mut particle_count = 0;
    let mut constraint_count = 0;
    let mut previous_differs = false;
    let mut sleeping_states = BTreeSet::new();
    let mut states = BTreeMap::new();
    for frame in (0..75).rev() {
        let expected = &truth["frames"][frame];
        let state = recording.state_at(frame).unwrap();
        assert!(state.errors.is_empty());
        let scene = cloth::collect(&state);
        assert!(
            scene.diagnostics.is_empty(),
            "frame {frame}: {:?}",
            scene.diagnostics
        );
        assert_eq!(scene.cloths.len(), 2);
        assert_eq!(scene.unique_fabric_count, 1);
        assert_eq!(scene.particle_count, if frame < 45 { 40 } else { 24 });
        assert!(Arc::ptr_eq(
            &scene.cloths[0].fabric,
            &scene.cloths[1].fabric
        ));
        let surface = state.render_scene();
        assert!(surface.objects.is_empty());
        assert!(
            surface
                .diagnostics
                .iter()
                .any(|text| text.contains("no surface triangle topology"))
        );
        let fabric = &scene.cloths[0].fabric;
        assert_eq!(fabric.id, expected["fabric"]["id"].as_u64().unwrap());
        assert_eq!(
            u64::from(fabric.particle_count),
            expected["fabric"]["particles"].as_u64().unwrap()
        );
        assert_eq!(
            fabric.constraints.len(),
            expected["fabric"]["rest"].as_array().unwrap().len()
        );
        assert_eq!(scene.constraint_count, fabric.constraints.len() * 2);
        for (i, constraint) in fabric.constraints.iter().enumerate() {
            for endpoint in 0..2 {
                assert_eq!(
                    u64::from(constraint.particles[endpoint]),
                    expected["fabric"]["indices"][i * 2 + endpoint]
                        .as_u64()
                        .unwrap()
                );
            }
            assert!(
                (constraint.rest_length - expected["fabric"]["rest"][i].as_f64().unwrap() as f32)
                    .abs()
                    < 0.000001
            );
        }
        for (i, set) in fabric.sets.iter().enumerate() {
            assert_eq!(
                u64::from(*set),
                expected["fabric"]["sets"][i].as_u64().unwrap()
            );
        }
        for (i, phase) in fabric.phases.iter().enumerate() {
            assert_eq!(
                u64::from(phase.kind),
                expected["fabric"]["phases"][i][0].as_u64().unwrap()
            );
            assert_eq!(
                u64::from(phase.set_index),
                expected["fabric"]["phases"][i][1].as_u64().unwrap()
            );
        }
        for expected_cloth in expected["cloths"].as_array().unwrap() {
            let id = expected_cloth["id"].as_u64().unwrap();
            let cloth = scene.cloths.iter().find(|cloth| cloth.id == id).unwrap();
            assert!(cloth.previous_particles.is_none());
            sleeping_states.insert(cloth.sleeping);
            assert_eq!(
                cloth.sleeping,
                expected_cloth["sleeping"].as_bool().unwrap()
            );
            close(&cloth.position, &expected_cloth["pose"]["position"]);
            close(&cloth.rotation, &expected_cloth["pose"]["rotation"]);
            assert_eq!(
                state
                    .resolve_reference(id, "Fabric", fabric.id)
                    .unwrap()
                    .generation,
                fabric.generation
            );
            assert_eq!(cloth.world_positions().len(), cloth.particles.len());
            assert_eq!(
                cloth.world_constraints().len(),
                cloth.fabric.constraints.len()
            );
            for (index, particle) in cloth.particles.iter().enumerate() {
                let expected_particle = &expected_cloth["particles"][index];
                close(&particle.position, &expected_particle["local"]);
                close(
                    &cloth.world_position(index).unwrap(),
                    &expected_particle["world"],
                );
                assert!(
                    (particle.inverse_mass
                        - expected_particle["inverse_mass"].as_f64().unwrap() as f32)
                        .abs()
                        < 0.000001
                );
                previous_differs |=
                    expected_particle["local"] != expected_particle["previous_local"];
                particle_count += 1;
            }
            for (constraint, world) in cloth
                .fabric
                .constraints
                .iter()
                .zip(cloth.world_constraints())
            {
                for endpoint in 0..2 {
                    close(
                        &world[endpoint],
                        &expected_cloth["particles"][constraint.particles[endpoint] as usize]["world"],
                    );
                }
                constraint_count += 1;
            }
            assert!(cloth.world_position(cloth.particles.len()).is_none());
        }
        if [0, 19, 20, 29, 30, 44, 45, 60, 61, 74].contains(&frame) {
            states.insert(frame, state);
        }
    }
    assert!(
        previous_differs,
        "SDK previous iteration positions must differ, proving they cannot be replaced by current data"
    );
    assert!(
        truth["frames"][1]["cloths"][0]["particles"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .any(|(index, particle)| particle["previous_local"]
                != truth["frames"][0]["cloths"][0]["particles"][index]["local"]),
        "SDK previous iteration particles are not the previous captured frame"
    );
    assert_eq!(particle_count, 2520);
    assert_eq!(sleeping_states, BTreeSet::from([false, true]));
    let asleep_id = truth["frames"][60]["cloths"][0]["id"].as_u64().unwrap();
    let (Value::Array(first_sleep), Value::Array(second_sleep)) = (
        &states[&60].objects[&asleep_id].properties["ParticleBuffer"],
        &states[&61].objects[&asleep_id].properties["ParticleBuffer"],
    ) else {
        panic!("sleeping particle buffer missing");
    };
    assert!(Arc::ptr_eq(first_sleep, second_sleep));
    let expected_constraints: usize = truth["frames"]
        .as_array()
        .unwrap()
        .iter()
        .map(|frame| frame["fabric"]["rest"].as_array().unwrap().len() * 2)
        .sum();
    assert_eq!(constraint_count, expected_constraints);
    let first_id = truth["frames"][0]["cloths"][0]["id"].as_u64().unwrap();
    assert_eq!(states[&29].objects[&first_id].generation, 0);
    assert_eq!(states[&30].objects[&first_id].generation, 1);
    let old_fabric = truth["frames"][44]["fabric"]["id"].as_u64().unwrap();
    assert_ne!(
        old_fabric,
        truth["frames"][45]["fabric"]["id"].as_u64().unwrap()
    );
    if let Some(retained) = states[&45].objects.get(&old_fabric) {
        assert_eq!(
            retained.generation,
            states[&44].objects[&old_fabric].generation
        );
        assert_eq!(
            retained.properties["Cloths"],
            Value::Array(Arc::new(Vec::new()))
        );
    }
    for frame in [0, 74, 44, 45, 29, 30, 20, 19, 60, 0] {
        assert_eq!(recording.state_at(frame).unwrap(), states[&frame]);
    }
    for truncated in [1, 27, bytes.len() - 1, bytes.len() - 7] {
        assert!(Recording::from_bytes(&bytes[..truncated]).is_err());
    }
    let old = &states[&29].objects[&first_id];
    assert!(matches!(old.properties["ParticleBuffer"], Value::Array(_)));
    println!(
        "verified {particle_count} SDK particles and {constraint_count} world constraint edges"
    );
}

#[test]
#[ignore = "requires official SDK cloth --reuse-fabric-address diagnostic probe"]
fn sdk_reused_fabric_without_new_metadata_is_reported_instead_of_using_old_constraints() {
    let capture = std::env::var_os("PVD_CLOTH_REUSE_CAPTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../captures/physx-3.4-cloth-reused-fabric.pvdstream")
        });
    let recording = Recording::from_bytes(&fs::read(&capture).unwrap()).unwrap();
    let truth: Json =
        serde_json::from_str(&fs::read_to_string(capture.with_extension("cloth.json")).unwrap())
            .unwrap();
    recording.verify_replay().unwrap();
    for frame in [0, 44, 45, 74, 30, 45, 0] {
        let state = recording.state_at(frame).unwrap();
        let collected = cloth::collect(&state);
        if frame < 45 {
            assert_eq!(collected.cloths.len(), 2);
            assert!(collected.diagnostics.is_empty());
        } else {
            assert!(collected.cloths.is_empty());
            assert_eq!(collected.diagnostics.len(), 2);
            assert!(
                collected
                    .diagnostics
                    .iter()
                    .all(|message| message.contains("Cloth and Fabric particle counts disagree"))
            );
            let id = truth["frames"][frame]["fabric"]["id"].as_u64().unwrap();
            assert_eq!(id, truth["frames"][0]["fabric"]["id"].as_u64().unwrap());
            assert_eq!(state.objects[&id].generation, 0);
            assert_eq!(state.objects[&id].properties["NbParticles"], Value::U64(20));
            assert_eq!(truth["frames"][frame]["fabric"]["particles"], 12);
        }
    }
}
