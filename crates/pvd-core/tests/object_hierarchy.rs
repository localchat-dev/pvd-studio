use pvd_core::{Geometry, Object, Recording, SceneState, Value};
use serde_json::Value as Json;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};

fn ids(json: &Json) -> BTreeSet<u64> {
    json.as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_u64().unwrap())
        .collect()
}

fn references(state: &SceneState, object: &Object, property: &str) -> BTreeSet<u64> {
    let ids = match object.properties.get(property) {
        Some(Value::Ref(id)) if *id != 0 => vec![*id],
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| match value {
                Value::Ref(id) => *id,
                _ => panic!("non-reference in {property}"),
            })
            .collect(),
        _ => Vec::new(),
    };
    for id in &ids {
        assert!(
            state.resolve_reference(object.id, property, *id).is_some(),
            "stale {}.{property} -> {id}",
            object.type_name
        );
    }
    ids.into_iter().collect()
}

fn close(actual: &[f32], expected: &Json) {
    let expected = expected.as_array().unwrap();
    assert_eq!(actual.len(), expected.len());
    for (a, b) in actual.iter().zip(expected) {
        let b = b.as_f64().unwrap() as f32;
        assert!((a - b).abs() < 0.00002, "{a} != {b}");
    }
}

fn pose(object: &Object, property: &str, expected: &Json) {
    let Value::Transform { position, rotation } = &object.properties[property] else {
        panic!("{} has no transform {property}", object.type_name);
    };
    close(position, &expected["position"]);
    close(rotation, &expected["rotation"]);
}

#[test]
#[ignore = "requires official SDK 3.4 pvd-objects producer and getter truth"]
fn aggregate_and_articulation_match_sdk_getters_across_membership_and_replay() {
    let capture = std::env::var_os("PVD_OBJECTS_CAPTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../captures/physx-3.4-objects.pvdstream")
        });
    let recording = Recording::from_bytes(&fs::read(&capture).unwrap()).unwrap();
    let truth: Json =
        serde_json::from_str(&fs::read_to_string(capture.with_extension("objects.json")).unwrap())
            .unwrap();
    let frames = truth["frames"].as_array().unwrap();
    assert_eq!(recording.frame_count(), 210);
    assert_eq!(recording.frame_count(), frames.len());
    recording.verify_replay().unwrap();
    let mut sleep_states = BTreeSet::new();
    let mut checked_shapes = 0;
    let random_order = [0, 209, 59, 60, 89, 90, 119, 120, 149, 150, 0, 151, 60];
    let mut prior_states = BTreeMap::new();
    for frame in (0..frames.len()).rev() {
        let expected = &frames[frame];
        let state = recording.state_at(frame).unwrap();
        assert!(state.errors.is_empty(), "SDK error at frame {frame}");
        let scene = state.render_scene();
        assert!(scene.diagnostics.is_empty(), "{:?}", scene.diagnostics);
        assert_eq!(scene.objects.len(), 6, "frame {frame}");
        let articulation_id = expected["articulation"]["id"].as_u64().unwrap();
        let articulation = &state.objects[&articulation_id];
        let sleeping = expected["articulation_sleeping_at_end"].as_bool().unwrap();
        sleep_states.insert(sleeping);
        assert_eq!(
            articulation.properties["IsSleeping"],
            Value::Bool(sleeping),
            "frame {frame}"
        );
        assert_eq!(articulation.generation, u32::from(frame >= 150));
        let aggregate_id = expected["aggregate"]["id"].as_u64().unwrap();
        let aggregate = &state.objects[&aggregate_id];
        assert_eq!(aggregate.properties["MaxNbActors"], Value::U64(16));
        assert_eq!(aggregate.properties["SelfCollision"], Value::Bool(false));
        let aggregate_actors = ids(&expected["aggregate"]["actors"]);
        let link_ids = ids(&expected["articulation"]["links"]);
        assert_eq!(
            references(&state, aggregate, "Actors"),
            aggregate_actors.difference(&link_ids).copied().collect(),
            "aggregate actor members, frame {frame}"
        );
        assert_eq!(
            references(&state, aggregate, "Articulations"),
            if expected["articulation"]["aggregate"].as_u64().unwrap() == aggregate_id {
                BTreeSet::from([articulation_id])
            } else {
                BTreeSet::new()
            },
            "aggregate articulation members, frame {frame}"
        );
        let recorded_scene = &state.objects[&expected["scene"].as_u64().unwrap()];
        assert!(references(&state, recorded_scene, "Aggregates").contains(&aggregate_id));
        assert!(references(&state, recorded_scene, "Articulations").contains(&articulation_id));
        for actor in expected["actors"].as_array().unwrap() {
            let actor_id = actor["id"].as_u64().unwrap();
            let object = &state.objects[&actor_id];
            let actor_type = actor["type"].as_str().unwrap();
            assert_eq!(object.type_name, format!("physx3::{actor_type}"));
            pose(object, "GlobalPose", &actor["pose"]);
            if actor_type != "PxRigidStatic" {
                let Value::Vec3(linear) = &object.properties["LinearVelocity"] else {
                    panic!("missing linear velocity");
                };
                let Value::Vec3(angular) = &object.properties["AngularVelocity"] else {
                    panic!("missing angular velocity");
                };
                close(linear, &actor["linear_velocity"]);
                close(angular, &actor["angular_velocity"]);
            }
            if actor_type == "PxArticulationLink" {
                assert!(!object.properties.contains_key("IsSleeping"));
                assert_eq!(
                    state.articulation_for_link(actor_id).unwrap().id,
                    articulation_id
                );
                assert_eq!(object.generation, articulation.generation);
                assert_eq!(references(&state, object, "Links"), ids(&actor["children"]));
                let joint_id = actor["joint"].as_u64().unwrap();
                if joint_id == 0 {
                    assert_eq!(
                        references(&state, articulation, "Links"),
                        BTreeSet::from([actor_id])
                    );
                    assert_eq!(
                        references(&state, object, "Parent"),
                        BTreeSet::from([articulation_id])
                    );
                } else {
                    assert_eq!(
                        references(&state, object, "InboundJoint"),
                        BTreeSet::from([joint_id])
                    );
                    let joint = &state.objects[&joint_id];
                    assert_eq!(
                        references(&state, joint, "Link"),
                        BTreeSet::from([actor_id])
                    );
                    pose(joint, "ParentPose", &actor["parent_pose"]);
                    pose(joint, "ChildPose", &actor["child_pose"]);
                    let parent = expected["actors"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|candidate| {
                            candidate["children"].as_array().is_some_and(|children| {
                                children.iter().any(|id| id.as_u64() == Some(actor_id))
                            })
                        })
                        .unwrap();
                    assert_eq!(
                        references(&state, object, "Parent"),
                        BTreeSet::from([parent["id"].as_u64().unwrap()])
                    );
                }
            } else {
                assert!(state.articulation_for_link(actor_id).is_none());
                if actor_type == "PxRigidDynamic" {
                    assert_eq!(object.generation, u32::from(frame >= 90));
                }
            }
            for shape in actor["shapes"].as_array().unwrap() {
                let shape_id = shape["id"].as_u64().unwrap();
                let rendered = scene
                    .objects
                    .iter()
                    .find(|object| object.id == actor_id && object.shape_id == shape_id)
                    .unwrap();
                assert_eq!(rendered.dynamic, actor_type != "PxRigidStatic");
                let actor_sleeping = if actor_type == "PxArticulationLink" {
                    sleeping
                } else {
                    actor["sleeping"].as_bool().unwrap()
                };
                assert_eq!(
                    rendered.sleeping, actor_sleeping,
                    "frame {frame}, {actor_type}"
                );
                close(&rendered.position, &shape["world_pose"]["position"]);
                close(&rendered.rotation, &shape["world_pose"]["rotation"]);
                pose(&state.objects[&shape_id], "LocalPose", &shape["local_pose"]);
                match &rendered.geometry {
                    Geometry::Box { half_extents } => {
                        assert_eq!(shape["geometry"], 3);
                        close(half_extents, &shape["dimensions"]);
                    }
                    Geometry::Sphere { radius } => {
                        assert_eq!(shape["geometry"], 0);
                        close(&[*radius], &shape["dimensions"]);
                    }
                    Geometry::Capsule {
                        radius,
                        half_height,
                    } => {
                        assert_eq!(shape["geometry"], 2);
                        close(&[*radius, *half_height], &shape["dimensions"]);
                    }
                    _ => panic!("unexpected fixture geometry"),
                }
                checked_shapes += 1;
            }
        }
        if random_order.contains(&frame) {
            prior_states.insert(frame, state);
        }
    }
    assert_eq!(sleep_states, BTreeSet::from([false, true]));
    assert_eq!(checked_shapes, 1260);
    for frame in random_order {
        assert_eq!(recording.state_at(frame).unwrap(), prior_states[&frame]);
    }
}
