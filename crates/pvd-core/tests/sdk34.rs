#[path = "support/analysis.rs"]
mod analysis_support;
use analysis_support::legacy_fixture;
use pvd_core::analysis::view::ProfileSource;
use pvd_core::{Geometry, LiveRecording, Recording, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn capture_path() -> PathBuf {
    std::env::var_os("PVD_SDK34_CAPTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../captures/physx-3.4.pvdstream")
        })
}

fn verify_sdk_poses(recording: &Recording, path: &Path, actors_per_frame: usize) {
    assert_eq!(recording.frame_count(), 120);
    recording.verify_replay().unwrap();
    let truth = fs::read_to_string(path.with_extension("csv")).unwrap();
    let mut poses = 0;
    let mut seen = std::collections::BTreeSet::new();
    let mut per_frame = [0usize; 120];
    for line in truth.lines().skip(1) {
        let c = line.split(',').collect::<Vec<_>>();
        assert_eq!(c.len(), 10);
        let frame = c[0].parse::<usize>().unwrap();
        let id = c[1].parse::<u64>().unwrap();
        assert!(frame < per_frame.len() && seen.insert((frame, id)));
        per_frame[frame] += 1;
        let state = recording.state_at(frame).unwrap();
        let actor = &state.objects[&id];
        assert_eq!(
            actor.properties["Name"],
            Value::String(c[2].to_owned()),
            "frame {frame}"
        );
        let Value::Transform { position, rotation } = actor.properties["GlobalPose"] else {
            panic!("actor transform missing")
        };
        let expected = c[3..]
            .iter()
            .map(|v| v.parse::<f32>().unwrap())
            .collect::<Vec<_>>();
        for (actual, expected) in position.into_iter().chain(rotation).zip(expected) {
            assert!(
                (actual - expected).abs() < 0.00001,
                "frame {frame} actor {id}: {actual} != {expected}"
            );
        }
        poses += 1;
    }
    assert_eq!(poses, 120 * actors_per_frame);
    assert_eq!(per_frame, [actors_per_frame; 120]);
}

#[test]
#[ignore = "requires tools/physx-sample generated PhysX 3.4.2 lifecycle capture and truth CSV"]
fn official_sdk_capture_matches_independent_sdk_poses_and_random_replay() {
    let path = capture_path();
    let recording = Recording::from_path(&path).expect("generate the official SDK fixture first");
    verify_sdk_poses(&recording, &path, 2);
    let before = recording.state_at(89).unwrap();
    let after = recording.state_at(90).unwrap();
    let original = before
        .objects
        .values()
        .find(|o| o.properties.get("Name") == Some(&Value::String("PvdStudioSphere".into())))
        .unwrap();
    let replacement = after
        .objects
        .values()
        .find(|o| {
            o.properties.get("Name") == Some(&Value::String("PvdStudioSphereReplacement".into()))
        })
        .unwrap();
    assert_eq!(
        original.id, replacement.id,
        "fixture is intended to exercise SDK pointer reuse"
    );
    assert_eq!(replacement.generation, original.generation + 1);
    for frame in [119, 0, 90, 60, 89, 1] {
        let state = recording.state_at(frame).unwrap();
        let scene = state.render_scene();
        assert!(scene.diagnostics.is_empty(), "{:?}", scene.diagnostics);
        let shapes = scene.objects;
        assert_eq!(shapes.len(), 3);
        assert!(shapes.iter().any(|s| s.geometry
            == Geometry::Box {
                half_extents: [0.5, 0.75, 1.0]
            }));
        assert!(shapes.iter().any(|s| matches!(s.geometry, Geometry::Plane)));
        assert!(shapes.iter().any(|s| s.geometry
            == Geometry::Sphere {
                radius: if frame < 90 { 0.6 } else { 0.4 }
            }));
    }
    let bytes = fs::read(path).unwrap();
    let mut live = LiveRecording::default();
    for chunk in bytes.chunks(997) {
        live.feed(chunk).unwrap();
    }
    let streamed = live.finish().unwrap();
    for frame in [0, 59, 60, 89, 90, 119] {
        assert_eq!(
            recording.state_at(frame).unwrap(),
            streamed.state_at(frame).unwrap()
        );
    }
}

#[test]
#[ignore = "requires tools/physx-sample --rich capture, SDK pose CSV and mesh truth JSON"]
fn rich_geometry_matches_sdk_cooked_resources_and_scaled_axes() {
    let path = std::env::var_os("PVD_SDK34_RICH_CAPTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../captures/physx-3.4-rich.pvdstream")
        });
    let recording = Recording::from_path(&path).unwrap();
    verify_sdk_poses(&recording, &path, 4);
    let state = recording.state_at(0).unwrap();
    let scene = state.render_scene();
    assert!(scene.diagnostics.is_empty(), "{:?}", scene.diagnostics);
    let shapes = scene.objects;
    assert_eq!(shapes.len(), 6);
    let truth: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(path.with_extension("mesh.json")).unwrap())
            .unwrap();
    let vector = |value: &serde_json::Value| {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap() as f32)
            .collect::<Vec<_>>()
    };
    for mesh in truth["meshes"].as_array().unwrap() {
        let id = mesh["actor_id"].as_u64().unwrap();
        let shape = shapes.iter().find(|s| s.id == id).unwrap();
        let Geometry::TriangleMesh { vertices, indices } = &shape.geometry else {
            panic!("SDK mesh not mapped")
        };
        let q = vector(&mesh["rotation"]);
        let scale = vector(&mesh["scale"]);
        let (x, y, z, w) = (q[0], q[1], q[2], q[3]);
        let matrix = [
            [
                1.0 - 2.0 * (y * y + z * z),
                2.0 * (x * y - z * w),
                2.0 * (x * z + y * w),
            ],
            [
                2.0 * (x * y + z * w),
                1.0 - 2.0 * (x * x + z * z),
                2.0 * (y * z - x * w),
            ],
            [
                2.0 * (x * z - y * w),
                2.0 * (y * z + x * w),
                1.0 - 2.0 * (x * x + y * y),
            ],
        ];
        let expected = mesh["vertices"].as_array().unwrap();
        assert_eq!(vertices.len(), expected.len());
        for (actual, raw) in vertices.iter().zip(expected) {
            let raw = vector(raw);
            let mut scaled = [0.0; 3];
            for i in 0..3 {
                scaled[i] = (0..3).map(|j| matrix[i][j] * raw[j]).sum::<f32>() * scale[i];
            }
            for i in 0..3 {
                let expected = (0..3).map(|j| matrix[j][i] * scaled[j]).sum::<f32>();
                assert!((actual[i] - expected).abs() < 0.00001);
            }
        }
        let mut expected_indices = Vec::new();
        if mesh["kind"] == "convex" {
            for polygon in mesh["polygons"].as_array().unwrap() {
                let p = polygon.as_array().unwrap();
                for i in 1..p.len() - 1 {
                    expected_indices.extend([
                        p[0].as_u64().unwrap() as u32,
                        p[i].as_u64().unwrap() as u32,
                        p[i + 1].as_u64().unwrap() as u32,
                    ]);
                }
            }
        } else {
            for triangle in mesh["triangles"].as_array().unwrap() {
                expected_indices.extend(
                    triangle
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_u64().unwrap() as u32),
                );
            }
        }
        assert_eq!(indices.as_ref(), &expected_indices);
    }
    let mut contacts = 0;
    let mut joints = 0;
    let mut primitives = 0;
    for frame in 0..recording.frame_count() {
        let state = recording.state_at(frame).unwrap();
        for contact in state.contacts.values().flat_map(|c| c.iter()) {
            contacts += 1;
            let length = contact.normal.iter().map(|v| v * v).sum::<f32>();
            assert!((length - 1.0).abs() < 0.001);
            assert!(contact.position.iter().all(|v| v.is_finite()));
        }
        for primitive in state.debug_draw.values().flat_map(|v| v.iter()) {
            primitives += 1;
            if matches!(
                primitive,
                pvd_core::debug_draw::Primitive::JointFrames { .. }
            ) {
                joints += 1;
            }
        }
    }
    println!(
        "SDK rich fixture: {contacts} frame contacts, {joints} joint frame primitives, {primitives} total primitives"
    );
    assert!(contacts > 0);
    assert_eq!(joints, 120);
    assert!(recording.analysis.profile_spans().next().is_some());
    assert!(legacy_fixture(&recording).memory.allocations > 0);
}
