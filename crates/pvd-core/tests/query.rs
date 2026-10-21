use pvd_core::{Recording, queries};
use std::{collections::BTreeMap, fs, path::PathBuf};

fn vector(value: &serde_json::Value) -> [f32; 3] {
    std::array::from_fn(|index| value[index].as_f64().unwrap() as f32)
}

fn close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 0.00001,
        "{actual} != {expected}"
    );
}

#[test]
#[ignore = "requires official SDK --query-truth capture and independent query JSON"]
fn recorded_rays_and_hits_match_independent_sdk_results() {
    let capture = std::env::var_os("PVD_QUERY_CAPTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../captures/physx-3.4-query.pvdstream")
        });
    let recording = Recording::from_path(&capture).unwrap();
    assert_eq!(recording.frame_count(), 120);
    let truth: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(capture.with_extension("query.json")).unwrap())
            .unwrap();
    let mut by_frame = BTreeMap::<usize, Vec<&serde_json::Value>>::new();
    for ray in truth["queries"].as_array().unwrap() {
        by_frame
            .entry(ray["frame"].as_u64().unwrap() as usize)
            .or_default()
            .push(ray);
    }
    assert_eq!(by_frame.len(), 120);
    let mut hit_count = 0;
    for frame in (0..120).rev() {
        let state = recording.state_at(frame).unwrap();
        let query_scene = queries::collect(&state);
        assert!(
            query_scene.diagnostics.is_empty(),
            "frame {frame}: {:?}",
            query_scene.diagnostics
        );
        let rays = query_scene.raycasts;
        let expected = &by_frame[&frame];
        assert_eq!(rays.len(), 4, "frame {frame}");
        assert_eq!(rays.len(), expected.len());
        for (actual, expected) in rays.iter().zip(expected) {
            assert_eq!(actual.index, expected["index"].as_u64().unwrap() as usize);
            assert_eq!(actual.kind, 1);
            assert_eq!(actual.filter_data, [0; 4]);
            assert_eq!(actual.filter_flags, 3);
            for (actual, expected) in actual.origin.into_iter().zip(vector(&expected["origin"])) {
                close(actual, expected);
            }
            for (actual, expected) in actual
                .direction
                .into_iter()
                .zip(vector(&expected["direction"]))
            {
                close(actual, expected);
            }
            close(
                actual.distance,
                expected["distance"].as_f64().unwrap() as f32,
            );
            let expected_hits = expected["hits"].as_array().unwrap();
            assert_eq!(actual.hits.len(), expected_hits.len());
            for (actual, expected) in actual.hits.iter().zip(expected_hits) {
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
                for (actual, expected) in actual
                    .position
                    .unwrap()
                    .into_iter()
                    .zip(vector(&expected["position"]))
                {
                    close(actual, expected);
                }
                for (actual, expected) in actual
                    .normal
                    .unwrap()
                    .into_iter()
                    .zip(vector(&expected["normal"]))
                {
                    close(actual, expected);
                }
                close(
                    actual.distance,
                    expected["distance"].as_f64().unwrap() as f32,
                );
                if let Some(face) = actual.face_index {
                    assert_eq!(face, expected["face_index"].as_u64().unwrap() as u32);
                }
                if let Some(uv) = actual.uv {
                    for (index, actual) in uv.into_iter().enumerate() {
                        close(actual, expected["uv"][index].as_f64().unwrap() as f32);
                    }
                }
                hit_count += 1;
            }
        }
    }
    assert_eq!(hit_count, 360);
    recording.verify_replay().unwrap();
    println!(
        "SDK {}: 480 real raycasts, 360 hits, positions/normals/distances/object references match independent SDK truth",
        truth["sdk"]
    );
}
