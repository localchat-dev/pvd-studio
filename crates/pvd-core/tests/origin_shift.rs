use pvd_core::{
    LiveRecording, Recording, SceneState, Value,
    live::{HistoryConfig, ReplayCursor},
};
use pvd_protocol::{Decoder, Event};
use serde::Deserialize;
use std::{fs, path::PathBuf};

#[derive(Deserialize)]
struct Truth {
    sdk: String,
    warmup_frames: usize,
    frames: Vec<Frame>,
}
#[derive(Deserialize)]
struct Frame {
    frame: usize,
    shifted_scene: i32,
    shift: [f32; 3],
    worlds: Vec<World>,
}
#[derive(Deserialize)]
struct World {
    scene: u64,
    poses: Vec<PoseEntry>,
    local_poses: Vec<PoseEntry>,
    particles: Vec<Particles>,
}
#[derive(Deserialize)]
struct PoseEntry {
    id: u64,
    pose: Pose,
}
#[derive(Deserialize)]
struct Pose {
    position: [f32; 3],
    rotation: [f32; 4],
}
#[derive(Deserialize)]
struct Particles {
    id: u64,
    values: Vec<Particle>,
}
#[derive(Deserialize)]
struct Particle {
    index: u32,
    position: [f32; 3],
    velocity: [f32; 3],
}

fn floats(actual: &[f32], expected: &[f32]) {
    assert_eq!(
        actual.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        expected.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
    );
}

fn check(state: &SceneState, frame: &Frame) {
    assert_eq!(frame.worlds.len(), 2);
    for world in &frame.worlds {
        assert_eq!(state.objects[&world.scene].type_name, "physx3::PxScene");
        assert_eq!(world.poses.len(), 6);
        for (property, poses) in [
            ("GlobalPose", &world.poses),
            ("LocalPose", &world.local_poses),
        ] {
            for entry in poses {
                let Value::Transform { position, rotation } =
                    state.objects[&entry.id].properties[property]
                else {
                    panic!("{property}")
                };
                assert_eq!(
                    position.map(f32::to_bits),
                    entry.pose.position.map(f32::to_bits),
                    "frame {} scene {} object {} ({}) {property}",
                    frame.frame,
                    world.scene,
                    entry.id,
                    state.objects[&entry.id].type_name
                );
                floats(&rotation, &entry.pose.rotation);
            }
        }
        for particles in &world.particles {
            let object = &state.objects[&particles.id];
            let Value::Array(indices) = &object.properties["ValidParticleBitmap"] else {
                panic!("bitmap")
            };
            assert!(
                matches!(indices.first(), Some(Value::U64(value)) if *value == (1 << 1 | 1 << 5))
            );
            let Value::Array(positions) = &object.properties["Positions"] else {
                panic!("positions")
            };
            let Value::Array(velocities) = &object.properties["Velocities"] else {
                panic!("velocities")
            };
            assert_eq!(positions.len(), particles.values.len());
            for (packed, particle) in particles.values.iter().enumerate() {
                assert_eq!(particle.index, if packed == 0 { 1 } else { 5 });
                let Value::Vec3(position) = positions[packed] else {
                    panic!("position")
                };
                let Value::Vec3(velocity) = velocities[packed] else {
                    panic!("velocity")
                };
                floats(&position, &particle.position);
                floats(&velocity, &particle.velocity);
            }
        }
    }
}

#[test]
#[ignore = "requires the official PhysX 3.4 origin-shift producer and independent SDK getter truth"]
fn sdk_origin_shifts_match_both_scenes_and_exact_forward_reverse_disk_replay() {
    let path = std::env::var_os("PVD_ORIGIN_SHIFT_CAPTURE")
        .map(PathBuf::from)
        .expect("PVD_ORIGIN_SHIFT_CAPTURE must name the generated SDK capture");
    let truth: Truth =
        serde_json::from_slice(&fs::read(path.with_extension("json")).unwrap()).unwrap();
    let bytes = fs::read(&path).unwrap();
    let mut decoder = Decoder::default();
    let groups = decoder.feed(&bytes).unwrap();
    decoder.finish().unwrap();
    let shifts: Vec<_> = groups
        .iter()
        .flat_map(|g| &g.events)
        .filter_map(|event| {
            if let Event::OriginShift { id, shift } = event {
                Some((*id, *shift))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(shifts.len(), 5);
    for ((id, value), frame) in shifts
        .iter()
        .zip(truth.frames.iter().filter(|frame| frame.shifted_scene >= 0))
    {
        assert_eq!(*id, frame.worlds[frame.shifted_scene as usize].scene);
        floats(value, &frame.shift);
    }
    assert_eq!(truth.sdk, "3.4.2");
    assert_eq!(truth.warmup_frames, 2);
    assert_eq!(truth.frames.len(), 144);
    let recording = Recording::from_path(&path).unwrap();
    assert_eq!(recording.frame_count(), 146);
    recording.verify_replay().unwrap();
    assert!(
        !recording
            .diagnostics
            .iter()
            .any(|message| message.contains("Origin shifts are retained"))
    );
    let cache = path
        .parent()
        .unwrap()
        .join(format!("disk-history-{}", std::process::id()));
    assert!(!cache.exists());
    let mut live = LiveRecording::with_history(HistoryConfig {
        directory: cache.clone(),
        disk_budget_bytes: 128 << 20,
        segment_frames: 3,
        min_free_disk_bytes: 0,
        ..HistoryConfig::default()
    })
    .unwrap();
    let mut fragmented = LiveRecording::default();
    for chunk in bytes.chunks(997) {
        live.feed(chunk).unwrap();
        fragmented.feed(chunk).unwrap();
    }
    let disk = live.finish().unwrap();
    let fragmented = fragmented.finish().unwrap();
    assert!(disk.disk_usage().unwrap().segments > 1);
    let mut cursor = ReplayCursor::default();
    let mut displayed = SceneState::default();
    let expected: Vec<_> = truth
        .frames
        .iter()
        .map(|frame| {
            let state = recording.state_at(frame.frame).unwrap();
            check(&state, frame);
            state
        })
        .collect();
    for index in (0..truth.frames.len())
        .chain((0..truth.frames.len()).rev())
        .chain([7, 0, 143, 3, 1, 132])
    {
        let frame = &truth.frames[index];
        assert_eq!(fragmented.state_at(frame.frame).unwrap(), expected[index]);
        assert_eq!(disk.state_at(frame.frame).unwrap(), expected[index]);
        cursor
            .seek_scene(&disk, frame.frame, || false)
            .unwrap()
            .update
            .apply_owned(&mut displayed);
        assert_eq!(displayed, expected[index]);
        check(&displayed, frame);
    }
    let saved = path.parent().unwrap().join(format!(
        "retained-origin-shift-{}.pvdcap",
        std::process::id()
    ));
    pvd_core::capture::write_retained_new(
        &disk,
        &saved,
        pvd_core::capture::CaptureRange {
            first: 4,
            last: 145,
        },
        &Default::default(),
        0,
        |_| true,
    )
    .unwrap();
    drop(cursor);
    drop(disk);
    let remaining: Vec<_> = fs::read_dir(&cache)
        .unwrap()
        .flat_map(|entry| {
            let entry = entry.unwrap();
            if entry.path().is_dir() {
                fs::read_dir(entry.path())
                    .unwrap()
                    .map(|child| child.unwrap().path())
                    .collect::<Vec<_>>()
            } else {
                vec![entry.path()]
            }
        })
        .collect();
    assert!(remaining.is_empty(), "{remaining:?}");
    fs::remove_dir(&cache).unwrap();
    let reopened = Recording::from_path(&saved).unwrap();
    assert!(reopened.state_at(3).is_err());
    for index in (2..truth.frames.len()).chain((2..truth.frames.len()).rev()) {
        assert_eq!(
            reopened.state_at(truth.frames[index].frame).unwrap(),
            expected[index]
        );
    }
    drop(reopened);
    fs::remove_file(saved).unwrap();
    println!(
        "5 SDK origin shifts, 144 exact getter snapshots, two Scenes, 12 poses and 4 particle systems per snapshot, 288 forward/reverse steps plus random disk replay, source cleanup and retained capture reopen verified"
    );
}
