#[path = "support/analysis.rs"]
mod analysis_support;
use analysis_support::{legacy_fixture, memory_at};
use pvd_core::analysis::view::MemorySource;
use pvd_core::{
    Geometry, LiveRecording, Recording, SceneState,
    live::{HistoryConfig, SceneUpdate},
};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

static DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "pvd-live-disk-test-{}-{}",
            std::process::id(),
            DIRECTORY_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn config(&self) -> HistoryConfig {
        HistoryConfig {
            directory: self.0.clone(),
            disk_budget_bytes: 128 << 20,
            segment_target_bytes: 64 << 20,
            segment_frames: 19,
            min_free_disk_bytes: 0,
        }
    }

    fn segments(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for session in fs::read_dir(&self.0).unwrap() {
            let session = session.unwrap().path();
            for path in fs::read_dir(session).unwrap() {
                let path = path.unwrap().path();
                if path
                    .extension()
                    .is_some_and(|extension| extension == "pvdhistory")
                {
                    paths.push(path);
                }
            }
        }
        paths.sort();
        paths
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}

fn header() -> Vec<u8> {
    let mut bytes = pvd_protocol::STREAM_MAGIC.to_le_bytes().to_vec();
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(1_000_000u64.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    bytes
}

fn group(events: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = (events.iter().map(Vec::len).sum::<usize>() as u32)
        .to_le_bytes()
        .to_vec();
    bytes.extend((events.len() as u32).to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    for event in events {
        bytes.extend(event);
    }
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

fn set(id: u64, property: u32, datatype: u32, data: &[u8]) -> Vec<u8> {
    let mut bytes = vec![7];
    bytes.extend(id.to_le_bytes());
    bytes.extend(property.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(datatype.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    bytes.extend((data.len() as u32).to_le_bytes());
    bytes.extend(data);
    bytes
}

fn frame_event(frame: usize) -> Vec<u8> {
    let mut bytes = vec![19];
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(8u32.to_le_bytes());
    bytes.extend((frame as u64 * 16_667).to_le_bytes());
    bytes
}

fn initialization() -> Vec<u8> {
    let mut events: Vec<_> = [
        "physx",
        "PxScene",
        "PxRigidDynamic",
        "PvdU32",
        "ObjectRef",
        "Counter",
        "Actor",
        "frame",
    ]
    .iter()
    .enumerate()
    .map(|(index, name)| string_event(index as u32 + 1, name))
    .collect();
    for class in [2u32, 3] {
        let mut bytes = vec![2];
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(class.to_le_bytes());
        events.push(bytes);
    }
    events.extend([create(2, 1), create(3, 2)]);
    let mut bytes = header();
    bytes.extend(group(&events));
    bytes
}

fn lifecycle_frame(frame: usize) -> Vec<u8> {
    let mut events = Vec::new();
    if frame != 0 && frame.is_multiple_of(7) {
        let mut destroy = vec![15];
        destroy.extend(2u64.to_le_bytes());
        events.extend([destroy, create(3, 2)]);
    }
    events.push(set(2, 6, 4, &(frame as u32).to_le_bytes()));
    if frame.is_multiple_of(3) {
        events.push(set(1, 7, 5, &2u64.to_le_bytes()));
    }
    events.push(frame_event(frame));
    group(&events)
}

fn synthetic_capture(frames: usize) -> Vec<u8> {
    let mut bytes = initialization();
    for frame in 0..frames {
        bytes.extend(lifecycle_frame(frame));
    }
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

fn analysis_capture(frames: usize) -> Vec<u8> {
    let mut bytes = initialization();
    let mut memory_class = vec![2];
    memory_class.extend(1u32.to_le_bytes());
    memory_class.extend(9u32.to_le_bytes());
    bytes.extend(group(&[
        string_event(9, "PxProfileMemoryEventBuffer"),
        string_event(10, "events"),
        string_event(11, "PvdU8"),
        memory_class,
        create(9, 7),
    ]));
    for frame in 0..frames {
        let mut memory = Vec::new();
        if frame == 0 {
            memory.extend(memory_string(1, "shape"));
            memory.extend(memory_string(2, "synthetic.cpp"));
        }
        match frame % 3 {
            1 => memory.extend([3, 0, 88]),
            cycle => {
                memory.extend([2, 0]);
                memory.extend([if cycle == 0 { 64 } else { 96 }, 1, 2, 20, 88]);
            }
        }
        if frame.is_multiple_of(11) {
            memory.extend([3, 0, 99]);
        }
        bytes.extend(group(&[set(7, 10, 11, &memory), frame_event(frame)]));
    }
    bytes
}

fn selected_frames(first: usize, end: usize) -> Vec<usize> {
    let mut selected = vec![first, end - 1];
    let mut random = 0x475d_385a_u64;
    for _ in 0..16 {
        random = random
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        selected.push(first + (random >> 32) as usize % (end - first));
    }
    selected.extend((first..end).rev().step_by(((end - first) / 12).max(1)));
    selected
}

fn verify_disk_and_updates(bytes: &[u8], config: HistoryConfig) {
    let expected = Recording::from_bytes(bytes).unwrap();
    let mut live = LiveRecording::with_history(config).unwrap();
    let mut display = SceneState::default();
    let mut merged = SceneUpdate::default();
    let mut merged_frame = None;
    for (index, chunk) in bytes.chunks(8191).enumerate() {
        live.feed(chunk).unwrap();
        if let Some((frame, update)) = live.take_update() {
            merged.merge(update);
            merged_frame = Some(frame);
            if index.is_multiple_of(3) {
                merged.apply(&mut display);
                assert_eq!(
                    display,
                    expected.state_at(frame).unwrap(),
                    "display frame {frame}"
                );
                merged = SceneUpdate::default();
                merged_frame = None;
            }
        }
    }
    if let Some(frame) = merged_frame {
        merged.apply(&mut display);
        assert_eq!(
            display,
            expected.state_at(frame).unwrap(),
            "display frame {frame}"
        );
    }
    assert_eq!(live.frame_count(), expected.frame_count());
    let disk = live.finish().unwrap();
    assert!(disk.is_disk_backed());
    assert_eq!(disk.frame_count(), expected.frame_count());
    assert_eq!(disk.first_frame(), 0);
    let stats = disk.disk_usage().unwrap();
    assert!(stats.warning.is_none(), "{:?}", stats.warning);
    assert_eq!(stats.retained_frames, expected.frame_count());
    assert!(stats.segments > 1);
    let mut cursor = pvd_core::live::ReplayCursor::default();
    let mut replayed = SceneState::default();
    let mut published_analysis = None;
    for frame in selected_frames(0, disk.frame_count()) {
        let expected_scene = expected.state_at(frame).unwrap();
        assert_eq!(
            disk.state_at(frame).unwrap(),
            expected_scene,
            "disk frame {frame}"
        );
        let replay = cursor.seek(&disk, frame, || false).unwrap();
        replay.update.apply(&mut replayed);
        assert_eq!(replayed, expected_scene, "cursor frame {frame}");
        let analysis = disk.analysis_at(frame).unwrap();
        assert_eq!(
            postcard::to_stdvec(&replay.analysis).unwrap(),
            postcard::to_stdvec(&analysis).unwrap(),
            "cursor analysis at frame {frame}"
        );
        assert_eq!(
            memory_at(&analysis, frame),
            postcard::to_stdvec(&legacy_fixture(&expected).memory.state_at_frame(frame)).unwrap(),
            "memory frame {frame}"
        );
        assert_eq!(
            analysis.memory_snapshot().churn(frame),
            legacy_fixture(&expected).memory.churn_at_frame(frame),
            "churn frame {frame}"
        );
        published_analysis = Some(replay.analysis);
    }
    drop(published_analysis);
}

#[test]
fn disk_history_matches_lifetimes_references_and_merged_display_updates() {
    let directory = TestDirectory::new();
    verify_disk_and_updates(&synthetic_capture(160), directory.config());
    assert!(directory.segments().is_empty());
}

#[test]
fn display_change_detection_checks_reference_generations_without_counter_invalidations() {
    let directory = TestDirectory::new();
    let mut live = LiveRecording::with_history(directory.config()).unwrap();
    live.feed(&initialization()).unwrap();
    let mut display = SceneState::default();
    for frame in 0..18 {
        live.feed(&lifecycle_frame(frame)).unwrap();
        let (_, update) = live.take_update().unwrap();
        assert_eq!(
            update.changes_properties(&display, &["Actor"]),
            [0, 7, 9, 14, 15].contains(&frame),
            "frame {frame}"
        );
        assert!(update.changes_properties(&display, &["Counter"]));
        update.apply(&mut display);
    }
}

#[test]
fn disk_analysis_replays_across_live_compaction_and_segment_checkpoints() {
    let directory = TestDirectory::new();
    verify_disk_and_updates(&analysis_capture(1300), directory.config());
}

#[test]
fn replay_cursor_preserves_scenes_and_analysis_across_growing_snapshots_and_reverse_steps() {
    let mut lifecycle = initialization();
    for frame in 0..260 {
        if frame == 5 {
            lifecycle.extend(group(&[create(3, 99)]));
        }
        if frame == 11 {
            let mut destroy = vec![15];
            destroy.extend(99u64.to_le_bytes());
            lifecycle.extend(group(&[destroy]));
        }
        lifecycle.extend(lifecycle_frame(frame));
    }
    for bytes in [lifecycle, analysis_capture(740)] {
        let directory = TestDirectory::new();
        let mut config = directory.config();
        config.segment_frames = 137;
        let mut live = LiveRecording::with_history(config).unwrap();
        let mut cursor = pvd_core::live::ReplayCursor::default();
        let mut display = SceneState::default();
        let mut published_analysis = None;
        for chunk in bytes.chunks(2047) {
            live.feed(chunk).unwrap();
            let recording = live.snapshot().unwrap();
            if recording.frames.is_empty() {
                continue;
            }
            let last = recording.frame_count() - 1;
            for frame in [last.saturating_sub(3), last, last.saturating_sub(2)] {
                let replay = cursor.seek(&recording, frame, || false).unwrap();
                replay.update.apply(&mut display);
                assert_eq!(display, recording.state_at(frame).unwrap(), "frame {frame}");
                assert_eq!(
                    postcard::to_stdvec(&replay.analysis).unwrap(),
                    postcard::to_stdvec(&recording.analysis_at(frame).unwrap()).unwrap(),
                    "analysis at frame {frame}"
                );
                published_analysis = Some(replay.analysis);
            }
        }
        let recording = live.finish().unwrap();
        for frame in (0..recording.frame_count()).rev().step_by(7) {
            let replay = cursor.seek(&recording, frame, || false).unwrap();
            replay.update.apply(&mut display);
            assert_eq!(
                display,
                recording.state_at(frame).unwrap(),
                "reverse frame {frame}"
            );
            assert_eq!(
                postcard::to_stdvec(&replay.analysis).unwrap(),
                postcard::to_stdvec(&recording.analysis_at(frame).unwrap()).unwrap(),
                "reverse analysis at frame {frame}"
            );
            published_analysis = Some(replay.analysis);
        }
        drop(published_analysis);
        drop(cursor);
        drop(recording);
        assert!(directory.segments().is_empty());
    }
}

#[test]
fn replay_cursor_cancellation_discards_partial_progress_and_changed_sources_replace_display() {
    let directory = TestDirectory::new();
    let mut config = directory.config();
    config.segment_frames = 200;
    let mut live = LiveRecording::with_history(config.clone()).unwrap();
    live.feed(&synthetic_capture(160)).unwrap();
    let recording = live.finish().unwrap();
    let mut cursor = pvd_core::live::ReplayCursor::default();
    let mut display = SceneState::default();
    cursor
        .seek(&recording, 10, || false)
        .unwrap()
        .update
        .apply(&mut display);
    for stop_after in [4, 20, 40] {
        cursor
            .seek(&recording, 10, || false)
            .unwrap()
            .update
            .apply(&mut display);
        let before = display.clone();
        let mut checks = 0;
        let failure = cursor
            .seek(&recording, 99, || {
                checks += 1;
                checks > stop_after
            })
            .err()
            .expect("replay must be interrupted");
        assert_eq!(
            failure.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
        assert_eq!(display, before);
        let replay = cursor.seek(&recording, 99, || false).unwrap();
        assert!(replay.update.replace_all);
        replay.update.apply(&mut display);
        assert_eq!(display, recording.state_at(99).unwrap());
    }
    let mut other = LiveRecording::with_history(config).unwrap();
    other.feed(&analysis_capture(120)).unwrap();
    let other = other.finish().unwrap();
    let replay = cursor.seek(&other, 100, || false).unwrap();
    assert!(replay.update.replace_all);
    replay.update.apply(&mut display);
    assert_eq!(display, other.state_at(100).unwrap());
}

#[test]
fn cancelled_disk_replay_stops_mid_restore_and_leaves_history_readable() {
    let directory = TestDirectory::new();
    let mut config = directory.config();
    config.segment_frames = 200;
    let bytes = analysis_capture(160);
    let mut live = LiveRecording::with_history(config).unwrap();
    live.feed(&bytes).unwrap();
    let recording = live.finish().unwrap();
    let expected = Recording::from_bytes(&bytes).unwrap();
    for stop_after in [0, 4, 20, 40] {
        for analysis in [false, true] {
            let mut checkpoints = 0;
            let mut cancelled = || {
                checkpoints += 1;
                checkpoints > stop_after
            };
            let failure = if analysis {
                recording.analysis_at_with_cancel(99, &mut cancelled).err()
            } else {
                recording.state_at_with_cancel(99, &mut cancelled).err()
            }
            .expect("the replay must stop before finishing the requested frame");
            assert_eq!(
                failure.downcast_ref::<std::io::Error>().unwrap().kind(),
                std::io::ErrorKind::Interrupted
            );
            assert_eq!(checkpoints, stop_after + 1);
            assert_eq!(
                recording.state_at(99).unwrap(),
                expected.state_at(99).unwrap()
            );
            let restored = recording.analysis_at(99).unwrap();
            assert_eq!(
                memory_at(&restored, 99),
                postcard::to_stdvec(&legacy_fixture(&expected).memory.state_at_frame(99)).unwrap()
            );
        }
    }
}

#[test]
fn rolling_disk_budget_expires_whole_segments_and_preserves_absolute_frame_numbers() {
    let directory = TestDirectory::new();
    let mut config = directory.config();
    config.disk_budget_bytes = 12 << 10;
    config.segment_target_bytes = 3 << 10;
    config.segment_frames = 3;
    let mut live = LiveRecording::with_history(config).unwrap();
    live.feed(&initialization()).unwrap();
    let fixed_writer_reservation = (32 << 20) + (4 << 20) + (64 << 10);
    let rolling_memory_allowance = 192 << 10;
    let initial_usage = live.history_memory_usage().unwrap();
    let initial_memory =
        initial_usage.estimated_history_bytes + initial_usage.estimated_checkpoint_bytes;
    assert!(
        (fixed_writer_reservation..fixed_writer_reservation + rolling_memory_allowance)
            .contains(&initial_memory),
        "initial={initial_memory} fixed={fixed_writer_reservation}"
    );
    let expected = Recording::from_bytes(&synthetic_capture(1200)).unwrap();
    let mut first_snapshot = None;
    let mut display = SceneState::default();
    let mut maximum_memory = initial_memory;
    for frame in 0..1200 {
        live.feed(&lifecycle_frame(frame)).unwrap();
        let (actual_frame, update) = live.take_update().unwrap();
        assert_eq!(actual_frame, frame);
        update.apply(&mut display);
        assert_eq!(display, expected.state_at(frame).unwrap());
        let snapshot = live.snapshot().unwrap();
        if frame == 2 {
            first_snapshot = Some(snapshot.clone());
        }
        let stats = snapshot.disk_usage().unwrap();
        assert!(stats.warning.is_none(), "{:?}", stats.warning);
        assert!(stats.disk_bytes <= 12 << 10);
        assert_eq!(snapshot.frame_count(), frame + 1);
        assert_eq!(stats.first_frame, snapshot.first_frame());
        assert_eq!(stats.retained_frames, snapshot.frames.len());
        assert!(
            snapshot
                .frames
                .windows(2)
                .all(|pair| pair[1].index == pair[0].index + 1)
        );
        let memory = live.history_memory_usage().unwrap();
        maximum_memory =
            maximum_memory.max(memory.estimated_history_bytes + memory.estimated_checkpoint_bytes);
    }
    let snapshot = live.finish().unwrap();
    assert!(snapshot.first_frame() > 0);
    assert_eq!(snapshot.frame_count(), 1200);
    assert!(snapshot.state_at(snapshot.first_frame() - 1).is_err());
    assert!(first_snapshot.unwrap().state_at(0).is_err());
    for frame in selected_frames(snapshot.first_frame(), snapshot.frame_count()) {
        assert_eq!(
            snapshot.state_at(frame).unwrap(),
            expected.state_at(frame).unwrap()
        );
    }
    let rolling_growth = maximum_memory - initial_memory;
    let nonfixed_memory = maximum_memory - fixed_writer_reservation;
    eprintln!(
        "rolling_memory fixed={fixed_writer_reservation} initial={initial_memory} \
         maximum={maximum_memory} growth={rolling_growth} nonfixed={nonfixed_memory}"
    );
    assert!(
        rolling_growth < rolling_memory_allowance,
        "{rolling_growth}"
    );
    assert!(
        nonfixed_memory < rolling_memory_allowance,
        "{nonfixed_memory}"
    );
}

#[test]
fn scene_baseline_larger_than_segment_target_still_retains_multiple_delta_frames() {
    let directory = TestDirectory::new();
    let mut config = directory.config();
    config.disk_budget_bytes = 128 << 10;
    config.segment_target_bytes = 16 << 10;
    config.segment_frames = 100;
    let mut live = LiveRecording::with_history(config).unwrap();
    live.feed(&initialization()).unwrap();
    live.feed(&group(&[
        string_event(9, "Payload"),
        string_event(10, "Opaque"),
        set(1, 9, 10, &vec![37; 40 << 10]),
    ]))
    .unwrap();
    for frame in 0..50 {
        live.feed(&group(&[
            set(2, 6, 4, &(frame as u32).to_le_bytes()),
            frame_event(frame),
        ]))
        .unwrap();
    }
    let snapshot = live.finish().unwrap();
    let stats = snapshot.disk_usage().unwrap();
    assert!(stats.warning.is_none(), "{:?}", stats.warning);
    assert!(stats.disk_bytes <= 128 << 10);
    assert!(
        stats.retained_frames >= 40,
        "a static baseline must not force a new segment each frame: {stats:?}"
    );
    assert_eq!(snapshot.frame_count(), 50);
    assert_eq!(
        snapshot.state_at(49).unwrap().objects[&2].properties["Counter"],
        pvd_core::Value::U64(49)
    );
}

#[test]
fn large_properties_and_reference_bindings_are_reused_until_their_values_or_generation_change() {
    let directory = TestDirectory::new();
    let mut config = directory.config();
    config.disk_budget_bytes = 4 << 20;
    config.segment_target_bytes = 1 << 20;
    config.segment_frames = 5;
    let mut bytes = initialization();
    bytes.extend(group(&[
        string_event(9, "Payload"),
        string_event(10, "Opaque"),
        string_event(11, "Actors"),
    ]));
    for frame in 0..200 {
        let mut events = Vec::new();
        if frame == 100 {
            let mut destroy = vec![15];
            destroy.extend(1u64.to_le_bytes());
            events.extend([destroy, create(2, 1)]);
        }
        if [0, 50, 100, 150].contains(&frame) {
            events.push(set(
                1,
                9,
                10,
                &vec![if frame < 100 { 37 } else { 52 }; 40 << 10],
            ));
            let targets: Vec<_> = (2..=1001u64).flat_map(u64::to_le_bytes).collect();
            let mut references = set(1, 11, 5, &targets);
            references[21..25].copy_from_slice(&1000u32.to_le_bytes());
            events.push(references);
        }
        if frame == 175 {
            events.push(set(1, 9, 10, &[13]));
        }
        events.push(set(1, 6, 4, &(frame as u32).to_le_bytes()));
        events.push(frame_event(frame));
        bytes.extend(group(&events));
    }
    let expected = Recording::from_bytes(&bytes).unwrap();
    let mut live = LiveRecording::with_history(config).unwrap();
    for chunk in bytes.chunks(8191) {
        live.feed(chunk).unwrap();
    }
    let disk = live.finish().unwrap();
    let stats = disk.disk_usage().unwrap();
    assert!(stats.warning.is_none(), "{:?}", stats.warning);
    assert!(
        stats.disk_bytes < 256 << 10,
        "large unchanged properties must not be repeated per frame: {stats:?}"
    );
    assert_eq!(disk.first_frame(), 0);
    disk.verify_replay().unwrap();
    for frame in [0, 49, 50, 99, 100, 149, 150, 174, 175, 199, 100, 0] {
        assert_eq!(
            disk.state_at(frame).unwrap(),
            expected.state_at(frame).unwrap(),
            "frame {frame}"
        );
    }
}

#[test]
fn shared_baselines_keep_large_static_geometry_once_and_replay_deltas_across_segments() {
    let directory = TestDirectory::new();
    let mut config = directory.config();
    config.disk_budget_bytes = 2 << 20;
    config.segment_frames = 3;
    let mut random = 0x0475_d385_9158_a231u64;
    let geometry: Vec<_> = (0..768 << 10)
        .map(|_| {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            random as u8
        })
        .collect();
    let mut bytes = initialization();
    bytes.extend(group(&[
        string_event(9, "Payload"),
        string_event(10, "Opaque"),
        set(1, 9, 10, &geometry),
    ]));
    for frame in 0..180 {
        let mut events = Vec::new();
        if frame == 60 {
            let mut destroy = vec![15];
            destroy.extend(2u64.to_le_bytes());
            events.push(destroy);
        }
        if frame == 63 {
            events.push(create(3, 2));
        }
        if !(60..63).contains(&frame) {
            events.push(set(2, 6, 4, &(frame as u32).to_le_bytes()));
        }
        events.push(frame_event(frame));
        bytes.extend(group(&events));
    }
    let expected = Recording::from_bytes(&bytes).unwrap();
    let mut live = LiveRecording::with_history(config).unwrap();
    for chunk in bytes.chunks(8191) {
        live.feed(chunk).unwrap();
    }
    let disk = live.finish().unwrap();
    let stats = disk.disk_usage().unwrap();
    assert!(stats.warning.is_none(), "{:?}", stats.warning);
    assert_eq!(stats.retained_frames, 180);
    assert_eq!(stats.segments, 60);
    assert!(stats.disk_bytes < 1 << 20, "{stats:?}");
    let physical: u64 = directory
        .segments()
        .iter()
        .map(|path| fs::metadata(path).unwrap().len())
        .sum();
    assert_eq!(stats.disk_bytes, physical);
    disk.verify_replay().unwrap();
    let mut cursor = pvd_core::live::ReplayCursor::default();
    let mut displayed = SceneState::default();
    for (position, frame) in (0..180).chain((0..179).rev()).enumerate() {
        let replay = cursor.seek(&disk, frame, || false).unwrap();
        if position != 0 {
            assert!(!replay.update.replace_all, "frame {frame}");
            assert!(
                replay.update.changed_objects().count() <= 1,
                "frame {frame}"
            );
        }
        replay.update.apply(&mut displayed);
        assert_eq!(
            displayed,
            expected.state_at(frame).unwrap(),
            "frame {frame}"
        );
    }
}

#[test]
fn history_snapshots_own_files_until_dropped_and_corruption_is_reported() {
    let directory = TestDirectory::new();
    let mut live = LiveRecording::with_history(directory.config()).unwrap();
    live.feed(&synthetic_capture(6)).unwrap();
    let snapshot = live.snapshot().unwrap();
    drop(live);
    let paths = directory.segments();
    let event_paths: Vec<_> = paths
        .iter()
        .filter(|path| path.file_stem().unwrap().to_string_lossy().len() == 8)
        .collect();
    assert_eq!(event_paths.len(), 1);
    let path = event_paths[0];
    snapshot.state_at(0).unwrap();
    let original = fs::read(path).unwrap();
    let first_length = u32::from_le_bytes(original[8..12].try_into().unwrap()) as usize;
    let record = 8 + 36 + first_length;
    assert_eq!(original[record + 36], 4);
    let mut changed = original.clone();
    changed[record + 36] ^= 0x80;
    fs::write(path, &changed).unwrap();
    assert!(
        snapshot
            .verify_replay()
            .unwrap_err()
            .to_string()
            .contains("checksum")
    );
    fs::write(path, &original).unwrap();
    let mut file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(record as u64)).unwrap();
    file.write_all(&u32::MAX.to_le_bytes()).unwrap();
    drop(file);
    assert!(
        snapshot
            .verify_replay()
            .unwrap_err()
            .to_string()
            .contains("length")
    );
    fs::write(path, &original[..original.len() - 1]).unwrap();
    assert!(
        snapshot
            .verify_replay()
            .unwrap_err()
            .to_string()
            .contains("truncated")
    );
    fs::write(path, &original).unwrap();
    snapshot.state_at(0).unwrap();
    snapshot.verify_replay().unwrap();
    let mut changed = original.clone();
    let mut offset = 8;
    let mut changed_frame = false;
    while offset < changed.len() {
        let length = u32::from_le_bytes(changed[offset..offset + 4].try_into().unwrap()) as usize;
        let payload = offset + 36;
        if changed[payload] == 4 {
            assert_eq!(changed[payload + 1], 0);
            changed[payload + 1] = 1;
            let digest = blake3::hash(&changed[payload..payload + length]);
            changed[offset + 4..payload].copy_from_slice(digest.as_bytes());
            changed_frame = true;
            break;
        }
        offset = payload + length;
    }
    assert!(changed_frame);
    fs::write(path, &changed).unwrap();
    assert!(
        snapshot
            .verify_replay()
            .unwrap_err()
            .to_string()
            .contains("frame metadata")
    );
    fs::write(path, &original).unwrap();
    drop(snapshot);
    assert!(!path.exists());
    assert!(directory.segments().is_empty());
}

#[test]
#[cfg(target_os = "windows")]
fn unavailable_disk_history_does_not_stop_decoding_or_latest_scene_updates() {
    let directory = TestDirectory::new();
    let mut config = directory.config();
    config.min_free_disk_bytes = u64::MAX;
    let bytes = synthetic_capture(120);
    let expected = Recording::from_bytes(&bytes).unwrap();
    let mut live = LiveRecording::with_history(config).unwrap();
    let mut display = SceneState::default();
    for chunk in bytes.chunks(1531) {
        live.feed(chunk).unwrap();
        if let Some((frame, update)) = live.take_update() {
            update.apply(&mut display);
            assert_eq!(display, expected.state_at(frame).unwrap());
        }
    }
    assert_eq!(live.frame_count(), 120);
    assert_eq!(display, expected.state_at(119).unwrap());
    let snapshot = live.finish().unwrap();
    let stats = snapshot.disk_usage().unwrap();
    assert_eq!(snapshot.frame_count(), 120);
    assert!(snapshot.frames.is_empty());
    assert!(
        stats
            .warning
            .as_ref()
            .is_some_and(|warning| warning.contains("reserve"))
    );
    assert_eq!(stats.disk_bytes, 0);
    assert_eq!(stats.retained_frames, 0);
}

fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../captures")
        .join(name);
    fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "generate the official SDK fixture {}: {error}",
            path.display()
        )
    })
}

#[test]
#[ignore = "requires generated official PhysX 3.4 baseline, CCT and eALL SDK streams"]
fn real_sdk_scenes_cct_and_memory_analysis_match_disk_random_and_reverse_playback() {
    for name in [
        "physx-3.4.pvdstream",
        "physx-3.4-cct.pvdstream",
        "physx-3.4-all.pvdstream",
        "physx-3.4-heightfield.pvdstream",
    ] {
        let directory = TestDirectory::new();
        verify_disk_and_updates(&fixture(name), directory.config());
    }
}

#[test]
#[ignore = "requires official SDK resident allocation stress stream generated with 150000 allocations"]
fn real_sdk_resident_analysis_matches_disk_random_and_reverse_playback() {
    let path = std::env::var_os("PVD_ANALYSIS_STRESS_CAPTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../captures/analysis-resident.pvdstream")
        });
    let bytes = fs::read(path).unwrap();
    let reference = Recording::from_bytes(&bytes).unwrap();
    let initial_memory = legacy_fixture(&reference).memory.state_at_frame(0);
    assert_eq!(
        initial_memory
            .live
            .values()
            .filter(|allocation| allocation.type_name.as_ref() == "AnalysisResident")
            .count(),
        150000
    );
    assert!(reference.analysis.accounted_bytes() > 32 << 20);
    drop(initial_memory);
    drop(reference);
    let directory = TestDirectory::new();
    let mut config = directory.config();
    config.disk_budget_bytes = 4 << 30;
    config.segment_frames = 60;
    verify_disk_and_updates(&bytes, config);
    assert!(directory.segments().is_empty());
}

#[test]
#[ignore = "synthetic protocol stress test consumes over 512 MiB without storing the raw stream"]
fn continuous_protocol_input_can_exceed_512_mib_with_bounded_retention() {
    let directory = TestDirectory::new();
    let mut config = directory.config();
    config.disk_budget_bytes = 8 << 10;
    config.segment_target_bytes = 2 << 10;
    config.segment_frames = 5;
    let mut live = LiveRecording::with_history(config).unwrap();
    let init = initialization();
    live.feed(&init).unwrap();
    let mut received = init.len();
    let mut camera = vec![23];
    camera.extend((1024 * 1024 + 1u32).to_le_bytes());
    camera.resize(camera.len() + 1024 * 1024, b'a');
    camera.push(0);
    camera.extend([0; 36]);
    let mut display = SceneState::default();
    for frame in 0..513 {
        let bytes = group(&[camera.clone(), frame_event(frame)]);
        received += bytes.len();
        live.feed(&bytes).unwrap();
        if let Some((_, update)) = live.take_update() {
            update.apply(&mut display);
        }
    }
    assert!(received > 512 << 20);
    assert_eq!(live.frame_count(), 513);
    assert_eq!(display.objects.len(), 2);
    let snapshot = live.finish().unwrap();
    let stats = snapshot.disk_usage().unwrap();
    assert!(stats.warning.is_none(), "{:?}", stats.warning);
    assert!(stats.disk_bytes <= 8 << 10);
    assert_eq!(snapshot.state_at(512).unwrap(), display);
    assert!(snapshot.first_frame() > 0);
    assert!(snapshot.history_memory_usage().estimated_history_bytes < 64 << 10);
}

#[test]
#[ignore = "requires an official SDK mixed capture, static getter CSV and per-step statistics"]
fn real_sdk_static_and_resident_analysis_fixture_matches_independent_getters() {
    use pvd_core::Value;
    use std::collections::BTreeMap;

    let path = PathBuf::from(
        std::env::var_os("PVD_MIXED_STATIC_CAPTURE")
            .expect("set PVD_MIXED_STATIC_CAPTURE to the generated mixed fixture"),
    );
    let bytes = fs::read(&path).unwrap();
    let reference = Recording::from_bytes(&bytes).unwrap();
    let statics = fs::read_to_string(path.with_extension("stats.csv.statics.csv")).unwrap();
    let rows: Vec<_> = statics
        .lines()
        .skip(1)
        .map(|line| line.split(',').collect::<Vec<_>>())
        .collect();
    assert!(!rows.is_empty());
    let statistics = fs::read_to_string(path.with_extension("stats.csv")).unwrap();
    assert_eq!(statistics.lines().count() - 1, reference.frame_count());
    for line in statistics.lines().skip(1) {
        let fields: Vec<_> = line.split(',').collect();
        assert_eq!(fields[5].parse::<usize>().unwrap(), rows.len() + 2);
        assert_eq!(fields[6], fields[7]);
        assert_eq!(fields[6], fields[8]);
    }
    for frame in [reference.frame_count() - 1, 0, reference.frame_count() / 2] {
        let state = reference.state_at(frame).unwrap();
        let rendered = state.render_scene();
        assert!(
            rendered.diagnostics.is_empty(),
            "{:?}",
            rendered.diagnostics
        );
        let shapes: BTreeMap<_, _> = rendered
            .objects
            .iter()
            .map(|shape| (shape.shape_id, shape))
            .collect();
        for row in &rows {
            assert_eq!(row.len(), 13);
            let id = row[0].parse::<u64>().unwrap();
            let shape_id = row[2].parse::<u64>().unwrap();
            let actor = &state.objects[&id];
            assert!(actor.type_name.ends_with("::PxRigidStatic"));
            assert_eq!(actor.properties["Name"], Value::String(row[1].into()));
            let shape = shapes[&shape_id];
            assert_eq!(shape.id, id);
            assert!(!shape.dynamic);
            let Geometry::Box { half_extents } = shape.geometry else {
                panic!("static actor must retain its captured box geometry");
            };
            for (actual, expected) in shape
                .position
                .into_iter()
                .chain(shape.rotation)
                .chain(half_extents)
                .zip(&row[3..])
            {
                assert!((actual - expected.parse::<f32>().unwrap()).abs() < 0.00001);
            }
        }
    }
    assert_eq!(
        legacy_fixture(&reference)
            .memory
            .state_at_frame(0)
            .live
            .values()
            .filter(|allocation| allocation.type_name.as_ref() == "AnalysisResident")
            .count(),
        150000
    );
    drop(reference);
    let directory = TestDirectory::new();
    let mut config = directory.config();
    config.disk_budget_bytes = 4 << 30;
    config.segment_frames = 4;
    verify_disk_and_updates(&bytes, config);
}

fn close(actual: f32, expected: &serde_json::Value) {
    let expected = expected.as_f64().unwrap() as f32;
    assert!(
        (actual - expected).abs() < 0.00002,
        "{actual} != {expected}"
    );
}

fn vector<const N: usize>(actual: [f32; N], expected: &serde_json::Value) {
    for (index, component) in actual.into_iter().enumerate() {
        close(component, &expected[index]);
    }
}

fn assert_cct_truth(state: &SceneState, frame: usize, truth: &serde_json::Value) {
    let scene = state.render_scene();
    assert!(scene.diagnostics.is_empty(), "{:?}", scene.diagnostics);
    assert_eq!(
        scene.objects.len(),
        truth["static_boxes"].as_u64().unwrap() as usize + 3
    );
    for expected in truth["frames"][frame]["controllers"].as_array().unwrap() {
        let actor = expected["actor"].as_u64().unwrap();
        let rendered = scene
            .objects
            .iter()
            .find(|object| object.id == actor)
            .unwrap();
        assert_eq!(rendered.shape_id, expected["shape"].as_u64().unwrap());
        assert!(rendered.dynamic);
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

#[test]
#[ignore = "requires real 10000-frame official SDK3.4 CCT producer output and independent pose truth"]
fn real_long_capture_keeps_current_memory_stable_and_rolls_disk_history() {
    let path = std::env::var_os("PVD_LIVE_LONG_CAPTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../output/continuous-live-validation/long.pvdstream")
        });
    let truth: serde_json::Value =
        serde_json::from_slice(&fs::read(path.with_extension("json")).unwrap()).unwrap();
    let frame_count = truth["frames"].as_array().unwrap().len();
    assert!(frame_count >= 10_000);
    assert_eq!(truth["sdk"], "3.4.2");
    assert_eq!(truth["static_boxes"], 16_000);
    let directory = TestDirectory::new();
    let mut config = directory.config();
    config.disk_budget_bytes = 64 << 20;
    config.segment_target_bytes = 16 << 20;
    config.segment_frames = 600;
    let mut live = LiveRecording::with_history(config).unwrap();
    let mut source = fs::File::open(&path).unwrap();
    let mut buffer = vec![0; 64 << 10];
    let mut received = 0usize;
    let mut display = SceneState::default();
    let mut samples = Vec::new();
    let mut sample_after = 1000;
    let mut peak_core_bytes = 0;
    let started = Instant::now();
    loop {
        let length = source.read(&mut buffer).unwrap();
        if length == 0 {
            break;
        }
        received += length;
        live.feed(&buffer[..length]).unwrap();
        let memory = live.history_memory_usage().unwrap();
        let core_bytes = memory.estimated_history_bytes + memory.estimated_checkpoint_bytes;
        peak_core_bytes = peak_core_bytes.max(core_bytes);
        assert!(core_bytes < memory.limit_bytes);
        assert_eq!(memory.retained_actions, 0);
        assert_eq!(memory.checkpoint_count, 0);
        if let Some((frame, update)) = live.take_update() {
            update.apply(&mut display);
            if frame >= sample_after && frame + 1 < frame_count {
                assert_cct_truth(&display, frame, &truth);
                let stats = live.snapshot().unwrap().disk_usage().unwrap();
                assert!(stats.warning.is_none(), "{:?}", stats.warning);
                assert!(stats.disk_bytes <= 64 << 20);
                samples.push(serde_json::json!({"frame": frame, "core_estimated_bytes": core_bytes, "disk_bytes": stats.disk_bytes, "first_retained_frame": stats.first_frame, "retained_frames": stats.retained_frames}));
                sample_after += 1000;
            }
        }
    }
    let streaming_seconds = started.elapsed().as_secs_f64();
    assert_eq!(received as u64, source.metadata().unwrap().len());
    assert_eq!(live.frame_count(), frame_count);
    assert_cct_truth(&display, frame_count - 1, &truth);
    assert!(samples.len() >= 8);
    let early = samples.first().unwrap()["core_estimated_bytes"]
        .as_u64()
        .unwrap();
    let late = samples.last().unwrap()["core_estimated_bytes"]
        .as_u64()
        .unwrap();
    assert!(
        late <= early + (4 << 20),
        "current-state estimate grew from {early} to {late}"
    );
    let disk = live.finish().unwrap();
    let stats = disk.disk_usage().unwrap();
    assert!(stats.warning.is_none(), "{:?}", stats.warning);
    assert!(disk.first_frame() > 0);
    assert_eq!(disk.frame_count(), frame_count);
    let mut seek_milliseconds = Vec::new();
    let checked = selected_frames(disk.first_frame(), disk.frame_count());
    for &frame in &checked {
        let seek = Instant::now();
        let state = disk.state_at(frame).unwrap();
        seek_milliseconds.push(seek.elapsed().as_secs_f64() * 1000.0);
        assert_cct_truth(&state, frame, &truth);
    }
    assert!(disk.state_at(disk.first_frame() - 1).is_err());
    let report = serde_json::json!({
        "sdk": "3.4.2", "instrumentation": "debug", "source": path,
        "raw_bytes": received, "frames": frame_count, "static_boxes": 16000, "moving_cct_proxies": 2,
        "disk_budget_bytes": 64 << 20, "disk_bytes": stats.disk_bytes,
        "first_retained_frame": stats.first_frame, "retained_frames": stats.retained_frames,
        "peak_core_estimated_bytes": peak_core_bytes, "early_core_estimated_bytes": early,
        "late_core_estimated_bytes": late, "samples": samples,
        "streaming_seconds_including_sample_truth_checks": streaming_seconds,
        "seek_frames": checked, "seek_milliseconds_excluding_truth_render": seek_milliseconds,
        "memory_measurement": "Core accounting estimate only; excludes decoder buffers, display state, UI, allocator and process RSS",
        "build_profile": if cfg!(debug_assertions) {"debug"} else {"release"}
    });
    let report_path = std::env::var_os("PVD_LIVE_LONG_REPORT")
        .map(PathBuf::from)
        .unwrap_or_else(|| path.with_file_name("long-core-validation.json"));
    fs::write(&report_path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    eprintln!(
        "real SDK continuous capture: {frame_count} frames, {received} bytes; early/late core estimates {early}/{late}; report {}",
        report_path.display()
    );
}
