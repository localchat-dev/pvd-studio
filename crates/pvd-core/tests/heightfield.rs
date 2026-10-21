use pvd_core::{Geometry, Recording};
use std::{fs, path::PathBuf, sync::Arc};

fn rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let cross = |a: [f32; 3], b: [f32; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let u = [q[0], q[1], q[2]];
    let t = cross(u, v).map(|v| v * 2.0);
    let c = cross(u, t);
    std::array::from_fn(|i| v[i] + q[3] * t[i] + c[i])
}

#[test]
#[ignore = "requires pvd-heightfield official SDK producer and independent triangle truth"]
fn height_fields_match_sdk_triangles_across_scale_holes_and_replay() {
    let capture = std::env::var_os("PVD_HEIGHTFIELD_CAPTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../captures/physx-3.4-heightfield.pvdstream")
        });
    let bytes = fs::read(&capture).unwrap();
    let recording = Recording::from_bytes(&bytes).unwrap();
    let truth: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(capture.with_extension("heightfield.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(recording.frame_count(), 75);
    assert_eq!(truth["frames"].as_array().unwrap().len(), 75);
    recording.verify_replay().unwrap();
    let mut triangle_count = 0;
    for frame in (0..75).rev() {
        let state = recording.state_at(frame).unwrap();
        let scene = state.render_scene();
        assert!(
            scene.diagnostics.is_empty(),
            "frame {frame}: {:?}",
            scene.diagnostics
        );
        assert_eq!(scene.objects.len(), 4);
        let expected = truth["frames"][frame]["actors"].as_array().unwrap();
        for actor in expected {
            let object = scene
                .objects
                .iter()
                .find(|v| v.id == actor["id"].as_u64().unwrap())
                .unwrap();
            assert_eq!(object.shape_id, actor["shape"].as_u64().unwrap());
            let Geometry::TriangleMesh { vertices, indices } = &object.geometry else {
                panic!("height field is not a mesh")
            };
            assert_eq!(vertices.len(), 20);
            let triangles = actor["triangles"].as_array().unwrap();
            assert_eq!(triangles.len(), 20);
            assert_eq!(indices.len(), triangles.len() * 3);
            for (actual, triangle) in indices.chunks_exact(3).zip(triangles) {
                for vertex in 0..3 {
                    assert_eq!(
                        u64::from(actual[vertex]),
                        triangle["indices"][vertex].as_u64().unwrap(),
                        "frame {frame}, triangle {}",
                        triangle["id"]
                    );
                    let world = rotate(object.rotation, vertices[actual[vertex] as usize]);
                    for axis in 0..3 {
                        let actual = world[axis] + object.position[axis];
                        let expected = triangle["vertices"][vertex][axis].as_f64().unwrap() as f32;
                        assert!(
                            (actual - expected).abs() < 0.00002,
                            "frame {frame}, triangle {}, axis {axis}: {actual} != {expected}",
                            triangle["id"]
                        );
                    }
                }
                triangle_count += 1;
            }
        }
        let first = scene
            .objects
            .iter()
            .find(|v| v.id == expected[0]["id"].as_u64().unwrap())
            .unwrap();
        let second = scene
            .objects
            .iter()
            .find(|v| v.id == expected[1]["id"].as_u64().unwrap())
            .unwrap();
        let (
            Geometry::TriangleMesh {
                vertices: a,
                indices: ai,
            },
            Geometry::TriangleMesh {
                vertices: b,
                indices: bi,
            },
        ) = (&first.geometry, &second.geometry)
        else {
            panic!()
        };
        assert!(Arc::ptr_eq(a, b));
        assert!(Arc::ptr_eq(ai, bi));
    }
    assert_eq!(triangle_count, 6000);
    for truncated in [1, 27, bytes.len() - 1, bytes.len() - 7] {
        assert!(
            Recording::from_bytes(&bytes[..truncated]).is_err(),
            "accepted truncation at {truncated}"
        );
    }
}
