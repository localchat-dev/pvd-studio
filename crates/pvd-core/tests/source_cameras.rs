use pvd_core::{
    LiveRecording, Recording, RecordingBuilder, SceneState, SourceCamera,
    clip::{trim_scene_frames, trim_scene_frames_streaming},
    live::{HistoryConfig, ReplayCursor},
};
use pvd_protocol::{Endian, Event, EventGroup, Header};
use std::{
    collections::BTreeMap,
    io::Cursor,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "pvd-cameras-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        )))
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        if self.0.exists() {
            std::fs::remove_dir(&self.0).unwrap();
        }
    }
}

fn pose(value: f32) -> SourceCamera {
    SourceCamera {
        position: [value, 2.0, -0.0],
        up: [0.0, 0.6, 0.8],
        target: [value + 1.0, 0.0, 7.0],
    }
}

fn word(bytes: &mut Vec<u8>, value: u32, big: bool) {
    bytes.extend(if big {
        value.to_be_bytes()
    } else {
        value.to_le_bytes()
    });
}
fn long(bytes: &mut Vec<u8>, value: u64, big: bool) {
    bytes.extend(if big {
        value.to_be_bytes()
    } else {
        value.to_le_bytes()
    });
}
fn text(bytes: &mut Vec<u8>, value: &str, big: bool) {
    word(bytes, value.len() as u32 + 1, big);
    bytes.extend(value.as_bytes());
    bytes.push(0);
}
fn camera(name: &str, camera: SourceCamera, big: bool) -> Vec<u8> {
    let mut bytes = vec![23];
    text(&mut bytes, name, big);
    for value in camera
        .position
        .into_iter()
        .chain(camera.up)
        .chain(camera.target)
    {
        word(&mut bytes, value.to_bits(), big);
    }
    bytes
}
fn group(bytes: &mut Vec<u8>, events: &[Vec<u8>], stream: u64, big: bool) {
    word(
        bytes,
        events.iter().map(Vec::len).sum::<usize>() as u32,
        big,
    );
    word(bytes, events.len() as u32, big);
    long(bytes, stream, big);
    long(bytes, 123, big);
    for event in events {
        bytes.extend(event);
    }
}
fn fixture(big: bool) -> (Vec<u8>, Vec<BTreeMap<String, SourceCamera>>) {
    let mut bytes = Vec::new();
    word(&mut bytes, pvd_protocol::STREAM_MAGIC, big);
    word(&mut bytes, 1, big);
    long(&mut bytes, 1, big);
    long(&mut bytes, 1, big);
    word(&mut bytes, 1, big);
    let mut string = vec![1];
    text(&mut string, "frame", big);
    word(&mut string, 1, big);
    group(&mut bytes, &[string], 1, big);
    let mut state = BTreeMap::new();
    let mut expected = Vec::new();
    for frame in 0..133 {
        if frame > 0 && frame % 3 != 0 {
            let name = "SDK 相机";
            group(&mut bytes, &[camera(name, pose(-1.0), big)], 1, big);
            let camera_pose = pose(frame as f32);
            group(&mut bytes, &[camera(name, camera_pose, big)], 2, big);
            state.insert(name.to_owned(), camera_pose);
        }
        if frame == 61 {
            let invalid = SourceCamera {
                position: [f32::from_bits(0x7fc0_0123), f32::INFINITY, -0.0],
                ..pose(0.0)
            };
            group(&mut bytes, &[camera("raw values", invalid, big)], 2, big);
            state.insert("raw values".into(), invalid);
        }
        let mut end = vec![19];
        long(&mut end, 17, big);
        word(&mut end, 1, big);
        long(&mut end, frame as u64, big);
        group(&mut bytes, &[end], 1, big);
        expected.push(state.clone());
    }
    group(
        &mut bytes,
        &[
            camera("SDK 相机", pose(999.0), big),
            camera("tail only", pose(9.0), big),
        ],
        2,
        big,
    );
    (bytes, expected)
}

fn check_recording(recording: &Recording, expected: &[BTreeMap<String, SourceCamera>]) {
    assert_eq!(recording.frame_count(), expected.len());
    let n = expected.len();
    let random = [
        n / 2,
        0,
        n - 1,
        n / 3,
        1.min(n - 1),
        n.saturating_sub(6),
        n / 2 + 1,
    ];
    for frame in (0..expected.len())
        .chain((0..expected.len()).rev())
        .chain(random)
    {
        assert_eq!(
            recording.state_at(frame).unwrap().cameras,
            expected[frame],
            "frame {frame}"
        );
    }
    if recording.disk_usage().is_some() {
        let mut cursor = ReplayCursor::default();
        let mut displayed = SceneState::default();
        for frame in (0..expected.len())
            .chain((0..expected.len()).rev())
            .chain(random)
        {
            cursor
                .seek(recording, frame, || false)
                .unwrap()
                .update
                .apply(&mut displayed);
            assert_eq!(displayed.cameras, expected[frame], "cursor frame {frame}");
        }
    }
    recording.verify_replay().unwrap();
}

#[test]
fn cameras_follow_frame_boundaries_across_memory_disk_undo_and_clip_replay() {
    for big in [false, true] {
        let (bytes, expected) = fixture(big);
        let recording = Recording::from_bytes(&bytes).unwrap();
        check_recording(&recording, &expected);
        let directory = Directory::new();
        let mut live = LiveRecording::with_history(HistoryConfig {
            directory: directory.0.clone(),
            segment_frames: 7,
            min_free_disk_bytes: 0,
            ..Default::default()
        })
        .unwrap();
        let mut displayed = SceneState::default();
        for chunk in bytes.chunks(37) {
            live.feed(chunk).unwrap();
            if let Some((frame, update)) = live.take_update() {
                update.apply(&mut displayed);
                assert_eq!(displayed.cameras, expected[frame]);
            }
        }
        let disk = live.finish().unwrap();
        check_recording(&disk, &expected);
        assert!(disk.disk_usage().unwrap().segments > 1);
        for (first, last) in [(0, 0), (1, 5), (60, 64), (125, 132)] {
            let mut output = Vec::new();
            trim_scene_frames_streaming(&mut Cursor::new(&bytes), &mut output, first, last, |_| {
                true
            })
            .unwrap();
            assert_eq!(output, trim_scene_frames(&bytes, first, last).unwrap());
            let clipped = Recording::from_bytes(&output).unwrap();
            assert_eq!(clipped.frame_count(), last - first + 1);
            for frame in 0..clipped.frame_count() {
                assert_eq!(
                    clipped.state_at(frame).unwrap().cameras,
                    expected[first + frame]
                );
            }
        }
    }
}

#[test]
fn camera_names_and_history_changes_respect_the_recording_memory_budget() {
    let mut builder = RecordingBuilder::with_memory_limit(
        Header {
            version: 1,
            timestamp_numerator: 1,
            timestamp_denominator: 1,
            flags: 1,
            endian: Endian::Little,
        },
        4096,
    );
    let mut rejected = false;
    for id in 0..100 {
        let camera = pose(id as f32);
        let result = builder.push(EventGroup {
            stream_id: 1,
            timestamp: id,
            offset: 0,
            events: vec![Event::SetCamera {
                name: format!("camera-{id}"),
                position: camera.position,
                up: camera.up,
                target: camera.target,
            }],
        });
        if let Err(error) = result {
            assert!(error.to_string().contains("budget"), "{error}");
            rejected = true;
            break;
        }
    }
    assert!(rejected);
}

#[test]
fn camera_state_survives_rolling_history_eviction_and_merged_publication() {
    let (bytes, expected) = fixture(false);
    let directory = Directory::new();
    let mut live = LiveRecording::with_history(HistoryConfig {
        directory: directory.0.clone(),
        segment_frames: 3,
        disk_budget_bytes: 12 << 10,
        min_free_disk_bytes: 0,
        ..Default::default()
    })
    .unwrap();
    live.feed(&bytes).unwrap();
    let (frame, update) = live.take_update().unwrap();
    let mut displayed = SceneState::default();
    update.apply(&mut displayed);
    assert_eq!(displayed.cameras, expected[frame]);
    let recording = live.finish().unwrap();
    let stats = recording.disk_usage().unwrap();
    assert!(stats.first_frame > 0, "{stats:?}");
    assert!(recording.state_at(0).is_err());
    let mut cursor = ReplayCursor::default();
    for frame in (stats.first_frame..expected.len()).rev() {
        cursor
            .seek(&recording, frame, || false)
            .unwrap()
            .update
            .apply(&mut displayed);
        assert_eq!(displayed.cameras, expected[frame], "retained frame {frame}");
    }
}

#[test]
#[ignore = "requires the official SDK 3.4 camera producer and independent truth JSON"]
fn official_sdk_cameras_match_the_producer_in_memory_disk_replay_and_clips() {
    #[derive(serde::Deserialize)]
    struct Camera {
        name: String,
        #[serde(flatten)]
        pose: SourceCamera,
    }
    #[derive(serde::Deserialize)]
    struct Frame {
        frame: usize,
        cameras: Vec<Camera>,
    }
    #[derive(serde::Deserialize)]
    struct Truth {
        frames: Vec<Frame>,
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = root.join("captures/cameras");
    let bytes = std::fs::read(directory.join("cameras.pvdstream"))
        .expect("run tools/physx-cameras producer");
    let truth: Truth =
        serde_json::from_slice(&std::fs::read(directory.join("truth.json")).unwrap()).unwrap();
    let expected: Vec<BTreeMap<String, SourceCamera>> = truth
        .frames
        .into_iter()
        .enumerate()
        .map(|(index, frame)| {
            assert_eq!(frame.frame, index);
            frame
                .cameras
                .into_iter()
                .map(|camera| (camera.name, camera.pose))
                .collect()
        })
        .collect();
    assert!(expected.len() > 8);
    let recording = Recording::from_bytes(&bytes).unwrap();
    check_recording(&recording, &expected);
    let cache = Directory::new();
    let mut live = LiveRecording::with_history(HistoryConfig {
        directory: cache.0.clone(),
        segment_frames: 3,
        min_free_disk_bytes: 0,
        ..Default::default()
    })
    .unwrap();
    let mut displayed = SceneState::default();
    for chunk in bytes.chunks(4093) {
        live.feed(chunk).unwrap();
        if let Some((frame, update)) = live.take_update() {
            update.apply(&mut displayed);
            assert_eq!(
                displayed.cameras, expected[frame],
                "published frame {frame}"
            );
        }
    }
    let disk = live.finish().unwrap();
    check_recording(&disk, &expected);
    assert!(disk.disk_usage().unwrap().segments > 1);
    let last = expected.len() - 1;
    for (first, last) in [(0, 0), (1, 4), (last / 2, last), (last, last)] {
        let mut output = Vec::new();
        trim_scene_frames_streaming(&mut Cursor::new(&bytes), &mut output, first, last, |_| true)
            .unwrap();
        assert_eq!(output, trim_scene_frames(&bytes, first, last).unwrap());
        let clipped = Recording::from_bytes(&output).unwrap();
        for frame in 0..clipped.frame_count() {
            assert_eq!(
                clipped.state_at(frame).unwrap().cameras,
                expected[first + frame]
            );
        }
    }
    println!(
        "SDK cameras: {} bytes, {} complete frames, source BLAKE3 {}",
        bytes.len(),
        expected.len(),
        blake3::hash(&bytes)
    );
}
