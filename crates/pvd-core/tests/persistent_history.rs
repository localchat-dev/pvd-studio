#[path = "support/analysis.rs"]
mod analysis_support;

use analysis_support::{legacy_fixture, memory_at};
use pvd_core::{
    LiveRecording, Recording, SceneState,
    analysis::view::MemorySource,
    capture::{
        CaptureLimits, CapturePhase, CaptureRange, CaptureReport, write_retained,
        write_retained_new,
    },
    live::{HistoryConfig, ReplayCursor},
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);
const FRAMES: usize = 73;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "pvd-persistent-history-test-{}-{}",
            std::process::id(),
            DIRECTORY_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn config(&self) -> HistoryConfig {
        HistoryConfig {
            directory: self.0.join("history"),
            disk_budget_bytes: 512 << 10,
            segment_target_bytes: 64 << 10,
            segment_frames: 1,
            min_free_disk_bytes: 0,
        }
    }

    fn assert_history_empty(&self) {
        assert_eq!(
            fs::read_dir(self.0.join("history")).unwrap().count(),
            0,
            "temporary banks survived their final recording and replay owners"
        );
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn group(events: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = (events.iter().map(Vec::len).sum::<usize>() as u32)
        .to_le_bytes()
        .to_vec();
    bytes.extend((events.len() as u32).to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    bytes.extend(events.iter().flatten());
    bytes
}

fn string_event(handle: u32, text: &str) -> Vec<u8> {
    let mut bytes = vec![1];
    bytes.extend((text.len() as u32 + 1).to_le_bytes());
    bytes.extend(text.bytes());
    bytes.push(0);
    bytes.extend(handle.to_le_bytes());
    bytes
}

fn create(class: u32, id: u64) -> Vec<u8> {
    let mut bytes = vec![6];
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(class.to_le_bytes());
    bytes.extend(id.to_le_bytes());
    bytes
}

fn set(id: u64, property: u32, datatype: u32, count: u32, data: &[u8]) -> Vec<u8> {
    let mut bytes = vec![7];
    bytes.extend(id.to_le_bytes());
    bytes.extend(property.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(datatype.to_le_bytes());
    bytes.extend(count.to_le_bytes());
    bytes.extend((data.len() as u32).to_le_bytes());
    bytes.extend(data);
    bytes
}

fn initialization() -> Vec<u8> {
    let mut bytes = pvd_protocol::STREAM_MAGIC.to_le_bytes().to_vec();
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(1_000_000u64.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    let mut events: Vec<_> = [
        "physx",
        "PxScene",
        "PxRigidDynamic",
        "PvdU32",
        "ObjectRef",
        "Counter",
        "Actor",
        "frame",
        "PxProfileMemoryEventBuffer",
        "events",
        "PvdU8",
        "Payload",
    ]
    .iter()
    .enumerate()
    .map(|(index, text)| string_event(index as u32 + 1, text))
    .collect();
    for class in [2u32, 3, 9] {
        let mut event = vec![2];
        event.extend(1u32.to_le_bytes());
        event.extend(class.to_le_bytes());
        events.push(event);
    }
    events.extend([create(2, 1), create(3, 2), create(9, 7)]);
    bytes.extend(group(&events));
    bytes
}

fn memory_string(handle: u32, text: &str) -> Vec<u8> {
    let mut bytes = 1u16.to_le_bytes().to_vec();
    bytes.extend((text.len() as u32 + 1).to_le_bytes());
    bytes.extend(text.bytes());
    bytes.push(0);
    bytes.extend(handle.to_le_bytes());
    bytes
}

fn frame_bytes(frame: usize) -> Vec<u8> {
    let mut events = Vec::new();
    if frame != 0 && frame.is_multiple_of(7) {
        let mut destroy = vec![15];
        destroy.extend(2u64.to_le_bytes());
        events.extend([destroy, create(3, 2)]);
    }
    events.push(set(2, 6, 4, 1, &(frame as u32).to_le_bytes()));
    events.push(set(2, 12, 11, 8192, &vec![frame as u8; 8192]));
    if frame.is_multiple_of(3) {
        events.push(set(1, 7, 5, 1, &2u64.to_le_bytes()));
    }
    let mut memory = Vec::new();
    if frame == 0 {
        memory.extend(memory_string(1, "shape"));
        memory.extend(memory_string(2, "persistent.cpp"));
    }
    if frame % 3 == 1 {
        memory.extend([3, 0, 88]);
    } else {
        memory.extend([2, 0, if frame % 3 == 0 { 64 } else { 96 }, 1, 2, 20, 88]);
    }
    events.push(set(7, 10, 11, memory.len() as u32, &memory));
    let mut end = vec![19];
    end.extend(1u64.to_le_bytes());
    end.extend(8u32.to_le_bytes());
    end.extend((frame as u64 * 16_667).to_le_bytes());
    events.push(end);
    group(&events)
}

fn synthetic_capture() -> Vec<u8> {
    let mut bytes = initialization();
    for frame in 0..FRAMES {
        bytes.extend(frame_bytes(frame));
    }
    bytes
}

fn persistent(bytes: &[u8], config: HistoryConfig) -> Recording {
    let mut live = LiveRecording::with_persistent_history(config).unwrap();
    for chunk in bytes.chunks(8191) {
        live.feed(chunk).unwrap();
    }
    live.finish().unwrap()
}

fn save(recording: &Recording, path: &Path, range: CaptureRange) -> CaptureReport {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap();
    write_retained(
        recording,
        &mut file,
        range,
        &CaptureLimits::default(),
        |_| true,
    )
    .unwrap()
}

fn assert_frames(
    expected: &Recording,
    actual: &Recording,
    frames: impl IntoIterator<Item = usize>,
) {
    let mut cursor = ReplayCursor::default();
    let mut state = SceneState::default();
    for frame in frames {
        assert!(actual.is_frame_retained(frame), "frame {frame}");
        assert_eq!(
            actual.frame_sources.frame(frame),
            expected.frame_sources.frame(frame),
            "source navigation at frame {frame}"
        );
        let expected_state = expected.state_at(frame).unwrap();
        assert_eq!(
            actual.state_at(frame).unwrap(),
            expected_state,
            "frame {frame}"
        );
        let replay = cursor.seek(actual, frame, || false).unwrap();
        replay.update.apply_owned(&mut state);
        assert_eq!(state, expected_state, "cursor frame {frame}");
        let analysis = actual.analysis_at(frame).unwrap();
        assert_eq!(
            postcard::to_stdvec(&replay.analysis).unwrap(),
            postcard::to_stdvec(&analysis).unwrap(),
            "cursor analysis at frame {frame}"
        );
        assert_eq!(
            memory_at(&analysis, frame),
            postcard::to_stdvec(&legacy_fixture(expected).memory.state_at_frame(frame)).unwrap(),
            "memory at frame {frame}"
        );
        assert_eq!(
            analysis.memory_snapshot().churn(frame),
            legacy_fixture(expected).memory.churn_at_frame(frame),
            "memory churn at frame {frame}"
        );
    }
}

#[test]
fn full_session_and_old_snapshots_replay_across_sealed_banks() {
    let directory = TestDirectory::new();
    let expected = Recording::from_bytes(&synthetic_capture()).unwrap();
    let config = directory.config();
    let budget = config.disk_budget_bytes;
    let mut live = LiveRecording::with_persistent_history(config).unwrap();
    live.feed(&initialization()).unwrap();
    for frame in 0..6 {
        live.feed(&frame_bytes(frame)).unwrap();
    }
    let old = live.snapshot().unwrap();
    for frame in 6..FRAMES {
        live.feed(&frame_bytes(frame)).unwrap();
    }
    let recording = live.finish().unwrap();
    let stats = recording.disk_usage().unwrap();
    assert!(stats.persistent);
    assert!(stats.warning.is_none(), "{:?}", stats.warning);
    assert_eq!(recording.first_frame(), 0);
    assert_eq!(recording.frames.len(), FRAMES);
    assert_eq!(recording.frame_sources.retained_frames(), FRAMES);
    assert_eq!(stats.retained_frames, FRAMES);
    assert!(
        stats.disk_bytes > stats.cache_disk_bytes,
        "no cold bank was produced"
    );
    assert!(stats.cache_disk_bytes <= budget);
    assert!(
        stats.disk_bytes > budget,
        "complete history did not outgrow its hot budget"
    );
    assert_frames(
        &expected,
        &recording,
        [72, 0, 39, 40, 19, 20, 59, 60, 3, 70, 1],
    );
    assert_eq!(old.frames.len(), 6);
    assert!(!old.is_frame_retained(6));
    assert_frames(&expected, &old, [5, 0, 4, 1]);
    drop(recording);
    assert_frames(&expected, &old, [0, 5]);
    let mut last_cursor = ReplayCursor::default();
    let replay = last_cursor.seek(&old, 0, || false).unwrap();
    drop(replay);
    drop(old);
    assert_eq!(
        fs::read_dir(directory.0.join("history")).unwrap().count(),
        1
    );
    drop(last_cursor);
    directory.assert_history_empty();
}

#[test]
fn saved_full_session_and_cold_crops_reopen_after_temporary_history_is_deleted() {
    let directory = TestDirectory::new();
    let bytes = synthetic_capture();
    let expected = Recording::from_bytes(&bytes).unwrap();
    let recording = persistent(&bytes, directory.config());
    let full_path = directory.0.join("full.pvdcapture");
    let crop_path = directory.0.join("cold-crop.pvdcapture");
    for (path, first, last) in [(&full_path, 0, FRAMES - 1), (&crop_path, 7, 34)] {
        let report = save(&recording, path, CaptureRange { first, last });
        assert_eq!(report.frames, last - first + 1);
    }
    let cancelled_path = directory.0.join("cancelled.pvdcapture");
    let error = write_retained_new(
        &recording,
        &cancelled_path,
        CaptureRange {
            first: 0,
            last: FRAMES - 1,
        },
        &CaptureLimits::default(),
        0,
        |progress| !(progress.phase == CapturePhase::Copy && progress.completed >= 21),
    )
    .unwrap_err();
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
    assert!(!cancelled_path.exists());
    drop(recording);
    directory.assert_history_empty();
    let full = Recording::from_path(&full_path).unwrap();
    assert_eq!(full.frames.len(), FRAMES);
    assert_frames(&expected, &full, (0..FRAMES).rev().chain(0..FRAMES));
    let crop = Recording::from_path(&crop_path).unwrap();
    assert_eq!(crop.first_frame(), 7);
    assert_eq!(crop.frames.len(), 28);
    assert!(!crop.is_frame_retained(6));
    assert!(!crop.is_frame_retained(35));
    assert_frames(&expected, &crop, (7..=34).rev().chain(7..=34));
}

fn verify_real_capture(relative_path: &str) {
    let directory = TestDirectory::new();
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative_path);
    let bytes = fs::read(&path)
        .unwrap_or_else(|error| panic!("required SDK fixture {}: {error}", path.display()));
    let expected = Recording::from_bytes(&bytes).unwrap();
    let mut config = directory.config();
    config.disk_budget_bytes = 32 << 20;
    let recording = persistent(&bytes, config);
    let stats = recording.disk_usage().unwrap();
    assert!(stats.warning.is_none(), "{:?}", stats.warning);
    assert_eq!(recording.first_frame(), 0);
    assert_eq!(recording.frames.len(), expected.frame_count());
    assert_eq!(
        recording.rooms.iter().collect::<Vec<_>>(),
        expected.rooms.iter().collect::<Vec<_>>()
    );
    assert!(
        stats.disk_bytes > stats.cache_disk_bytes,
        "fixture must cross a bank boundary"
    );
    let last = expected.frame_count() - 1;
    assert_frames(
        &expected,
        &recording,
        [last, 0, last / 2, 19, 20, 1, last.saturating_sub(1)],
    );
    let path = directory.0.join("real-full.pvdcapture");
    save(&recording, &path, CaptureRange { first: 0, last });
    drop(recording);
    directory.assert_history_empty();
    let reopened = Recording::from_path(&path).unwrap();
    assert_eq!(reopened.frames.len(), last + 1);
    assert_eq!(
        reopened.rooms.iter().collect::<Vec<_>>(),
        expected.rooms.iter().collect::<Vec<_>>()
    );
    assert_frames(&expected, &reopened, [last, 0, 20, 19, last / 2]);
    eprintln!(
        "persistent real capture {}: {} frames, {} total disk bytes, {} hot bytes",
        relative_path,
        last + 1,
        stats.disk_bytes,
        stats.cache_disk_bytes
    );
}

#[test]
#[ignore = "requires captures/physx-3.4.pvdstream generated with the official SDK"]
fn real_sdk_baseline_persistent_history_matches_memory_reference() {
    verify_real_capture("captures/physx-3.4.pvdstream");
}

#[test]
#[ignore = "requires PVD_PERSISTENT_ROOM_CAPTURE pointing to an official SDK multi-room capture"]
fn real_sdk_room_lifecycles_persistent_history_matches_memory_reference() {
    let path = std::env::var("PVD_PERSISTENT_ROOM_CAPTURE")
        .expect("set PVD_PERSISTENT_ROOM_CAPTURE to the official SDK multi-room capture");
    verify_real_capture(&path);
}
