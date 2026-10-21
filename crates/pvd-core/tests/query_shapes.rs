use pvd_core::{Geometry, Recording, queries};
use serde_json::Value;
use std::{collections::BTreeMap, fs, path::PathBuf};

fn close(actual: f32, expected: &Value) {
    let expected = expected.as_f64().unwrap() as f32;
    assert!(
        (actual - expected).abs() < 0.00002,
        "{actual} != {expected}"
    );
}
fn vector<const N: usize>(actual: [f32; N], expected: &Value) {
    for (index, actual) in actual.into_iter().enumerate() {
        close(actual, &expected[index]);
    }
}
fn geometry(actual: &queries::QueryGeometry, expected: &Value) {
    assert!(actual.reference.generation.is_some());
    vector(actual.position, &expected["position"]);
    vector(actual.rotation, &expected["rotation"]);
    match (
        &actual.geometry,
        expected["geometry_type"].as_u64().unwrap(),
    ) {
        (Geometry::Sphere { radius }, 0) => close(*radius, &expected["radius"]),
        (
            Geometry::Capsule {
                radius,
                half_height,
            },
            2,
        ) => {
            close(*radius, &expected["radius"]);
            close(*half_height, &expected["half_height"]);
        }
        (Geometry::Box { half_extents }, 3) => vector(*half_extents, &expected["half_extents"]),
        (Geometry::TriangleMesh { vertices, indices }, 4) => {
            assert_eq!(
                vertices.len(),
                expected["vertices"].as_u64().unwrap() as usize
            );
            assert!(!indices.is_empty());
            for (index, point) in vertices.iter().enumerate() {
                vector(*point, &expected["points"][index]);
            }
        }
        (actual, expected) => panic!("Geometry {actual:?} != SDK type {expected}"),
    }
}
fn hits(actual: &[queries::Hit], expected: &Value, state: &pvd_core::SceneState) {
    let expected = expected.as_array().unwrap();
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(actual.actor.id, expected["actor"].as_u64().unwrap());
        assert_eq!(actual.shape.id, expected["shape"].as_u64().unwrap());
        assert_eq!(
            actual.actor.generation,
            Some(state.objects[&actual.actor.id].generation)
        );
        assert_eq!(
            actual.shape.generation,
            Some(state.objects[&actual.shape.id].generation)
        );
        assert_eq!(actual.flags, expected["flags"].as_u64().unwrap() as u32);
        close(actual.distance, &expected["distance"]);
        if let Some(position) = actual.position {
            vector(position, &expected["position"]);
        }
        if let Some(normal) = actual.normal {
            vector(normal, &expected["normal"]);
        }
        if let Some(face_index) = actual.face_index {
            assert_eq!(face_index as u64, expected["face_index"].as_u64().unwrap());
        }
    }
}
fn filter(data: [u32; 4], flags: u32, expected: &Value) {
    for (i, actual) in data.into_iter().enumerate() {
        assert_eq!(actual as u64, expected["filter_data"][i].as_u64().unwrap());
    }
    assert_eq!(flags as u64, expected["filter_flags"].as_u64().unwrap());
}

#[test]
#[ignore = "requires official SDK pvd-query-shapes capture and independent result JSON"]
fn sweep_overlap_and_batched_queries_match_sdk_results_across_lifetimes() {
    let capture = std::env::var_os("PVD_QUERY_SHAPES_CAPTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../captures/physx-3.4-query-shapes.pvdstream")
        });
    let truth: Value =
        serde_json::from_str(&fs::read_to_string(capture.with_extension("json")).unwrap()).unwrap();
    let recording = Recording::from_path(capture).unwrap();
    assert_eq!(
        recording.frame_count(),
        truth["frames"].as_u64().unwrap() as usize
    );
    let mut by_frame = BTreeMap::<usize, Vec<&Value>>::new();
    for query in truth["queries"].as_array().unwrap() {
        by_frame
            .entry(query["frame"].as_u64().unwrap() as usize)
            .or_default()
            .push(query);
    }
    let mut total = [0; 3];
    let mut hits_count = 0;
    let mut generations = BTreeMap::<u64, std::collections::BTreeSet<u32>>::new();
    for frame in (0..recording.frame_count()).rev() {
        let state = recording.state_at(frame).unwrap();
        let actual = queries::collect(&state);
        assert!(
            actual.diagnostics.is_empty(),
            "frame {frame}: {:?}",
            actual.diagnostics
        );
        assert_eq!(actual.raycasts.len(), 2);
        assert_eq!(actual.sweeps.len(), 17);
        assert_eq!(actual.overlaps.len(), 16);
        for expected in &by_frame[&frame] {
            let index = expected["index"].as_u64().unwrap() as usize;
            let batched = expected["batched"].as_bool().unwrap();
            let actual_hits = match expected["type"].as_str().unwrap() {
                "ray" => {
                    let actual = actual
                        .raycasts
                        .iter()
                        .find(|q| q.index == index && q.batched == batched)
                        .unwrap();
                    vector(actual.origin, &expected["position"]);
                    vector(actual.direction, &expected["direction"]);
                    close(actual.distance, &expected["distance"]);
                    filter(actual.filter_data, actual.filter_flags, expected);
                    total[0] += 1;
                    &actual.hits
                }
                "sweep" => {
                    let actual = actual
                        .sweeps
                        .iter()
                        .find(|q| q.index == index && q.batched == batched)
                        .unwrap();
                    geometry(&actual.geometry, expected);
                    vector(actual.direction, &expected["direction"]);
                    close(actual.distance, &expected["distance"]);
                    filter(actual.filter_data, actual.filter_flags, expected);
                    total[1] += 1;
                    &actual.hits
                }
                "overlap" => {
                    let actual = actual
                        .overlaps
                        .iter()
                        .find(|q| q.index == index && q.batched == batched)
                        .unwrap();
                    geometry(&actual.geometry, expected);
                    filter(actual.filter_data, actual.filter_flags, expected);
                    assert!(
                        actual
                            .hits
                            .iter()
                            .all(|h| h.position.is_none() && h.normal.is_none())
                    );
                    total[2] += 1;
                    &actual.hits
                }
                other => panic!("Unexpected query type {other}"),
            };
            hits(actual_hits, &expected["hits"], &state);
            hits_count += actual_hits.len();
            for hit in actual_hits {
                generations
                    .entry(hit.actor.id)
                    .or_default()
                    .insert(hit.actor.generation.unwrap());
            }
        }
    }
    assert_eq!(total, [24, 204, 192]);
    assert_eq!(hits_count, 312);
    assert!(
        generations.values().any(|values| values.len() >= 2),
        "SDK fixture did not reuse an actor ID"
    );
    recording.verify_replay().unwrap();
    println!(
        "420 SDK queries and 312 hits matched across 12 reverse/random replay frames, including reused object IDs and negative MTD distances"
    );
}
