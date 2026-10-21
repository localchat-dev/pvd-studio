use super::*;
use pvd_core::{Recording, Value, cloth, particles, queries};
use std::{collections::BTreeMap, path::PathBuf};

fn object(id: u64, generation: u32, name: &str) -> Object {
    Object {
        id,
        generation,
        type_name: name.into(),
        properties: BTreeMap::new(),
    }
}

fn compare(index: &SourceIndex, state: &SceneState) {
    assert!(index.complete);
    assert_eq!(
        cloth::collect_from_ids(state, &index.cloths),
        cloth::collect(state)
    );
    assert_eq!(
        particles::collect_from_ids(state, &index.particles),
        particles::collect(state)
    );
    assert_eq!(
        queries::collect_from_ids(state, &index.scenes),
        queries::collect(state)
    );
}

#[test]
fn full_delta_type_reuse_removal_and_reset_match_complete_collectors() {
    let mut state = SceneState::default();
    for (id, name) in [
        (8, "physx3::PxScene"),
        (4, "physx3::PxCloth"),
        (9, "physx3::PxParticleSystem"),
        (1, "physx3::PxParticleFluid"),
        (3, "physx3::PxClothFabric"),
        (7, "other::PxParticleSystem"),
        (11, "PxCloth"),
    ] {
        state.objects.insert(id, object(id, 0, name));
    }
    let mut index = SourceIndex::default();
    index.update(&state, None);
    compare(&index, &state);
    assert_eq!(index.scenes, BTreeSet::from([8]));
    assert_eq!(index.cloths, BTreeSet::from([4]));
    assert_eq!(index.particles, BTreeSet::from([1, 9]));
    state.objects.remove(&4);
    state.objects.insert(4, object(4, 1, "physx3::PxScene"));
    state.objects.remove(&9);
    state.objects.insert(9, object(9, 1, "physx3::PxCloth"));
    state.objects.remove(&1);
    state
        .objects
        .insert(1, object(1, 2, "physx3::PxRigidStatic"));
    state
        .objects
        .insert(5, object(5, 0, "physx3::PxParticleFluid"));
    state.objects.remove(&5);
    index.update(&state, Some(&HashSet::from([1, 4, 5, 9])));
    compare(&index, &state);
    assert_eq!(index.scenes, BTreeSet::from([4, 8]));
    assert_eq!(index.cloths, BTreeSet::from([9]));
    assert!(index.particles.is_empty());
    state.objects.remove(&8);
    index.update(&state, Some(&HashSet::from([8])));
    compare(&index, &state);
    state.objects.clear();
    index.update(&state, None);
    compare(&index, &state);
    assert!(index.scenes.is_empty() && index.cloths.is_empty() && index.particles.is_empty());
}

#[test]
fn index_budget_falls_back_without_retaining_partial_or_stale_ids_and_can_recover() {
    let mut state = SceneState::default();
    for id in 0..4 {
        state.objects.insert(id, object(id, 0, "physx3::PxScene"));
    }
    let mut index = SourceIndex::default();
    index.update_with_limit(&state, None, 3);
    assert!(!index.complete);
    assert!(index.scenes.is_empty());
    state.objects.remove(&3);
    index.update_with_limit(&state, Some(&HashSet::from([3])), 3);
    compare(&index, &state);
    state.objects.remove(&0);
    state.objects.insert(10, object(10, 0, "physx3::PxCloth"));
    index.update_with_limit(&state, Some(&HashSet::from([0, 10])), 3);
    compare(&index, &state);
    state
        .objects
        .insert(11, object(11, 0, "physx3::PxParticleSystem"));
    index.update_with_limit(&state, Some(&HashSet::from([11])), 3);
    assert!(!index.complete);
    assert!(index.scenes.is_empty() && index.cloths.is_empty() && index.particles.is_empty());
}

#[test]
fn source_order_missing_ids_and_exact_type_filters_preserve_diagnostics() {
    let mut state = SceneState::default();
    for id in [91, 10, 3] {
        state.objects.insert(id, object(id, 0, "physx3::PxCloth"));
    }
    state
        .objects
        .insert(1, object(1, 0, "other::PxParticleSystem"));
    let ids = BTreeSet::from([91, 3, 77, 1, 10]);
    assert_eq!(
        cloth::collect_from_ids(&state, &ids),
        cloth::collect(&state)
    );
    assert_eq!(
        particles::collect_from_ids(&state, &ids),
        particles::collect(&state)
    );
    assert_eq!(
        queries::collect_from_ids(&state, &ids),
        queries::collect(&state)
    );
    let result = cloth::collect_from_ids(&state, &ids);
    assert_eq!(result.diagnostics.len(), 3);
    assert!(result.diagnostics[0].starts_with("PxCloth 0x3"));
    assert!(result.diagnostics[1].starts_with("PxCloth 0xA"));
}

#[test]
#[ignore = "requires preserved official SDK cloth/particles/query captures"]
fn real_sdk_state_sequences_match_complete_collectors_in_both_directions() {
    let root =
        PathBuf::from(std::env::var_os("PVD_SOURCE_INDEX_ROOT").expect("PVD_SOURCE_INDEX_ROOT"));
    for file in [
        "physx-3.4-cloth.pvdstream",
        "physx-3.4-cloth-reused-fabric.pvdstream",
        "physx-3.4-particles.pvdstream",
        "physx-3.4-query.pvdstream",
        "physx-3.4-query-shapes.pvdstream",
    ] {
        let recording = Recording::from_path(root.join("captures").join(file)).unwrap();
        let mut index = SourceIndex::default();
        let mut previous = SceneState::default();
        for frame in (0..recording.frame_count()).chain((0..recording.frame_count()).rev()) {
            let state = recording.state_at(frame).unwrap();
            let changed: HashSet<_> = previous
                .objects
                .keys()
                .chain(state.objects.keys())
                .filter(|id| previous.objects.get(id) != state.objects.get(id))
                .copied()
                .collect();
            index.update(&state, Some(&changed));
            compare(&index, &state);
            previous = state;
        }
        index.update(&SceneState::default(), None);
        compare(&index, &SceneState::default());
        println!(
            "{file}: {} forward + reverse states match",
            recording.frame_count()
        );
    }
}

#[test]
#[ignore = "requires preserved SDK cloth/particle/query fixtures for indirect mutation cases"]
fn indirect_fabric_particle_geometry_and_query_changes_use_current_state() {
    let root =
        PathBuf::from(std::env::var_os("PVD_SOURCE_INDEX_ROOT").expect("PVD_SOURCE_INDEX_ROOT"));
    let recording = Recording::from_path(root.join("captures/physx-3.4-cloth.pvdstream")).unwrap();
    let mut state = recording.state_at(0).unwrap();
    let mut index = SourceIndex::default();
    index.update(&state, None);
    let cloth_before = cloth::collect_from_ids(&state, &index.cloths);
    assert!(cloth_before.cloths.len() >= 2);
    let fabric = cloth_before.cloths[0].fabric.id;
    let Value::Array(values) = state
        .objects
        .get_mut(&fabric)
        .unwrap()
        .properties
        .get_mut("Restvalues")
        .unwrap()
    else {
        panic!("fabric Restvalues array");
    };
    let Value::F32(first) = &mut std::sync::Arc::make_mut(values)[0] else {
        panic!("fabric rest length");
    };
    *first += 0.1;
    index.update(&state, Some(&HashSet::from([fabric])));
    compare(&index, &state);
    let changed_cloth = cloth::collect_from_ids(&state, &index.cloths);
    assert!(changed_cloth.diagnostics.is_empty());
    assert!(std::sync::Arc::ptr_eq(
        &changed_cloth.cloths[0].fabric,
        &changed_cloth.cloths[1].fabric
    ));
    assert_ne!(changed_cloth, cloth_before);
    let recording =
        Recording::from_path(root.join("captures/physx-3.4-particles.pvdstream")).unwrap();
    state = recording.state_at(0).unwrap();
    index.update(&state, None);
    let before = particles::collect_from_ids(&state, &index.particles);
    assert!(!before.systems.is_empty());
    let id = before.systems[0].actor;
    let properties = &mut state.objects.get_mut(&id).unwrap().properties;
    properties.insert("NbParticles".into(), Value::U64(0));
    properties.insert("ValidParticleRange".into(), Value::U64(0));
    index.update(&state, Some(&HashSet::from([id])));
    compare(&index, &state);
    assert!(
        particles::collect_from_ids(&state, &index.particles)
            .systems
            .iter()
            .find(|s| s.actor == id)
            .unwrap()
            .particles
            .is_empty()
    );
    let recording =
        Recording::from_path(root.join("captures/physx-3.4-query-shapes.pvdstream")).unwrap();
    state = recording.state_at(0).unwrap();
    index.update(&state, None);
    let before = queries::collect_from_ids(&state, &index.scenes);
    let geometry = before
        .sweeps
        .first()
        .map(|q| q.geometry.reference.id)
        .or_else(|| before.overlaps.first().map(|q| q.geometry.reference.id))
        .unwrap();
    state.objects.remove(&geometry);
    index.update(&state, Some(&HashSet::from([geometry])));
    compare(&index, &state);
    assert_ne!(queries::collect_from_ids(&state, &index.scenes), before);
    for id in &index.scenes {
        state
            .objects
            .get_mut(id)
            .unwrap()
            .properties
            .retain(|name, _| {
                !name.starts_with("SceneQueries.") && !name.starts_with("BatchedQueries.")
            });
    }
    index.update(&state, Some(&index.scenes.iter().copied().collect()));
    compare(&index, &state);
    let empty = queries::collect_from_ids(&state, &index.scenes);
    assert!(empty.raycasts.is_empty() && empty.sweeps.is_empty() && empty.overlaps.is_empty());
}
