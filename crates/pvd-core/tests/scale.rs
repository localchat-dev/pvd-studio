use pvd_core::{Recording, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};

struct Pose {
    name: String,
    coordinates: [f32; 7],
}

#[test]
#[ignore = "requires official PhysX 3.4.2 --extra-boxes 500 capture and independent SDK CSV"]
fn many_sdk_actors_match_independent_poses_after_random_seeks() {
    let capture = std::env::var_os("PVD_SDK34_SCALE_CAPTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../captures/physx-3.4-scale.pvdstream")
        });
    let recording = Recording::from_path(&capture).unwrap();
    assert_eq!(recording.frame_count(), 120);
    let csv = fs::read_to_string(capture.with_extension("csv")).unwrap();
    let mut truth = BTreeMap::<usize, BTreeMap<u64, Pose>>::new();
    assert_eq!(
        csv.lines().next().unwrap(),
        "frame,actor_id,name,x,y,z,qx,qy,qz,qw"
    );
    for row in csv.lines().skip(1) {
        let columns = row.split(',').collect::<Vec<_>>();
        assert_eq!(columns.len(), 10);
        let frame = columns[0].parse::<usize>().unwrap();
        let actor = columns[1].parse::<u64>().unwrap();
        let coordinates = std::array::from_fn(|i| columns[i + 3].parse::<f32>().unwrap());
        assert!(coordinates.iter().all(|value| value.is_finite()));
        assert!(
            truth
                .entry(frame)
                .or_default()
                .insert(
                    actor,
                    Pose {
                        name: columns[2].to_owned(),
                        coordinates,
                    },
                )
                .is_none(),
            "duplicate SDK truth row for frame {frame}, actor {actor}"
        );
    }
    assert_eq!(truth.len(), 120);
    for poses in truth.values() {
        assert_eq!(poses.len(), 502);
        assert_eq!(
            poses
                .values()
                .filter(|pose| pose.name.starts_with("PvdStudioGridBox"))
                .count(),
            500
        );
    }

    let mut frames = vec![119, 0, 90, 60, 89];
    let mut selected: BTreeSet<_> = frames.iter().copied().collect();
    let mut random = 0x3141_5926u32;
    while frames.len() < 16 {
        random ^= random << 13;
        random ^= random >> 17;
        random ^= random << 5;
        let frame = random as usize % recording.frame_count();
        if selected.insert(frame) {
            frames.push(frame);
        }
    }
    for frame in frames {
        let state = recording.state_at(frame).unwrap();
        assert!(
            state.errors.is_empty(),
            "SDK reported errors: {:?}",
            state.errors
        );
        assert_eq!(state.objects.len(), 1513, "frame {frame}");
        let rendered = state.render_scene();
        assert!(
            rendered.diagnostics.is_empty(),
            "{:?}",
            rendered.diagnostics
        );
        assert_eq!(rendered.objects.len(), 503, "frame {frame}");
        let actual_ids: BTreeSet<_> = state
            .objects
            .values()
            .filter(|object| object.type_name.ends_with("::PxRigidDynamic"))
            .map(|object| object.id)
            .collect();
        assert_eq!(actual_ids, truth[&frame].keys().copied().collect());
        for (id, expected) in &truth[&frame] {
            let actor = &state.objects[id];
            assert_eq!(
                actor.properties["Name"],
                Value::String(expected.name.clone()),
                "frame {frame}, actor {id}"
            );
            let Value::Transform { position, rotation } = actor.properties["GlobalPose"] else {
                panic!("frame {frame}, actor {id} has no typed pose")
            };
            for (actual, expected) in position
                .into_iter()
                .chain(rotation)
                .zip(expected.coordinates)
            {
                assert!(
                    (actual - expected).abs() < 0.00001,
                    "frame {frame}, actor {id}: {actual} != {expected}"
                );
            }
        }
    }
    recording.verify_replay().unwrap();
    println!(
        "SDK scale fixture: 500 additional boxes, 1513 objects, 503 render shapes; \
         60240 SDK CSV poses, 8032 poses checked across 16 random/boundary frames; no SDK errors"
    );
}
