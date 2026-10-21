use pvd_core::{Geometry, LiveRecording, Recording, Value};
use serde_json::Value as Json;
use std::{collections::BTreeSet, fs, path::PathBuf};

fn close(actual: f32, expected: &Json) {
    let expected = expected.as_f64().unwrap() as f32;
    assert!(
        (actual - expected).abs() < 0.00002,
        "{actual} != {expected}"
    );
}

fn vector<const N: usize>(actual: [f32; N], expected: &Json) {
    for (index, actual) in actual.into_iter().enumerate() {
        close(actual, &expected[index]);
    }
}

#[test]
#[ignore = "requires pvd-cct official SDK producer, stream and independent pre-simulate proxy truth"]
fn moving_cct_proxies_match_sdk_poses_after_live_capture_and_random_seek() {
    let path = std::env::var_os("PVD_CCT_CAPTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../captures/physx-3.4-cct.pvdstream")
        });
    let truth: Json =
        serde_json::from_slice(&fs::read(path.with_extension("json")).unwrap()).unwrap();
    let frames = truth["frames"].as_array().unwrap();
    assert!(
        frames.len() >= 120,
        "fixture must include a direction change"
    );
    let bytes = fs::read(&path).unwrap();
    let recording = Recording::from_bytes(&bytes).unwrap();
    assert_eq!(recording.frame_count(), frames.len());
    let mut live = LiveRecording::default();
    for chunk in bytes.chunks(8191) {
        live.feed(chunk).unwrap();
    }
    let streamed = live.finish().unwrap();
    assert_eq!(streamed.frame_count(), frames.len());
    if frames.len() <= 120 {
        recording.verify_replay().unwrap();
    }
    let mut selected = BTreeSet::from([0, 59, 60, 119, 120, 355, 356, 357, frames.len() - 1]);
    let mut random = 0x38a8_154f_u64;
    for _ in 0..16 {
        random = random
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        selected.insert((random >> 32) as usize % frames.len());
    }
    selected.retain(|index| *index < frames.len());
    for frame in selected
        .iter()
        .copied()
        .chain(selected.iter().rev().copied())
    {
        let state = recording.state_at(frame).unwrap();
        assert_eq!(state, streamed.state_at(frame).unwrap(), "frame {frame}");
        let scene = state.render_scene();
        assert!(scene.diagnostics.is_empty(), "{:?}", scene.diagnostics);
        assert_eq!(
            scene.objects.len(),
            truth["static_boxes"].as_u64().unwrap() as usize + 3
        );
        for expected in frames[frame]["controllers"].as_array().unwrap() {
            let actor = expected["actor"].as_u64().unwrap();
            let shape = expected["shape"].as_u64().unwrap();
            let rendered = scene
                .objects
                .iter()
                .find(|object| object.id == actor)
                .unwrap();
            assert_eq!(rendered.shape_id, shape);
            assert!(rendered.dynamic);
            assert_eq!(
                state.objects[&actor].properties["RigidBodyFlags"],
                Value::U64(1)
            );
            vector(rendered.position, &expected["position"]);
            vector(rendered.rotation, &expected["rotation"]);
            match (&rendered.geometry, expected["geometry"].as_str().unwrap()) {
                (
                    Geometry::Capsule {
                        radius,
                        half_height,
                    },
                    "capsule",
                ) => {
                    close(*radius, &expected["radius"]);
                    close(*half_height, &expected["half_height"]);
                }
                (Geometry::Box { half_extents }, "box") => {
                    vector(*half_extents, &expected["half_extents"])
                }
                (actual, expected) => panic!("{actual:?} != {expected}"),
            }
        }
    }
    for (first, last) in [(0, 99), (100, 119)] {
        for (start, end) in frames[first]["controllers"]
            .as_array()
            .unwrap()
            .iter()
            .zip(frames[last]["controllers"].as_array().unwrap())
        {
            assert_ne!(start["position"], end["position"]);
        }
    }
}
