use super::*;
use crate::analysis::view::{MemoryRows, MemorySource, MemoryViewCursor};
use crate::native::export::memory_fixture as preserved;
use crate::{MemoryHistoryScope, NativeMemoryTotals};

mod input;
use input::{Combined, FIRST};

const TOTALS: [&[i32]; 8] = [
    &[64, 16],
    &[0, 24],
    &[-7, 16, 4],
    &[5, 16, 4],
    &[5, 17, 0],
    &[-7, 23, 0],
    &[-7, 23, 2],
    &[-7, 17, 2],
];

fn totals(value: &NativeMemoryTotals) -> Vec<i32> {
    (0..value.len())
        .map(|index| value.row(index).unwrap().bytes)
        .collect()
}

fn check_combined(recording: &Recording, first: usize, last: usize) {
    check(recording, first, last);
    for frame in (0..recording.frame_count())
        .chain((0..recording.frame_count()).rev())
        .chain([last - first, 0, (last - first) / 2])
    {
        let state = recording.state_at(frame).unwrap();
        assert_eq!(
            totals(&recording.native_memory_totals_at(&state).unwrap().unwrap()),
            TOTALS[first + frame],
            "global frame {}",
            first + frame
        );
    }
}

fn slots(class: u32) -> &'static [u32] {
    match class {
        0 => &[7],
        4 => &[0, 1, 2, 3],
        8 => &[8],
        9 => &[9],
        _ => panic!("unexpected instance class"),
    }
}

fn check_undo(bytes: &[u8]) {
    let raw = preserved::raw(bytes);
    let mut previous = BTreeMap::new();
    for (id, (class, _)) in &raw.initial {
        for &property in slots(*class) {
            previous.insert((*id, property), 0);
        }
    }
    for (ordinal, event) in raw.events.iter().enumerate() {
        let mut reader = Reader::new(event);
        match reader.u32().unwrap() {
            1 => {
                let id = reader.u32().unwrap();
                let class = reader.u32().unwrap();
                for &property in slots(class) {
                    previous.insert((id, property), ordinal);
                }
            }
            2 => {
                let id = reader.u32().unwrap();
                previous.retain(|key, _| key.0 != id);
            }
            kind @ (3 | 4) => {
                let id = reader.u32().unwrap();
                let property = reader.u32().unwrap();
                reader.blob().unwrap();
                let fields = if kind == 4 {
                    assert_eq!(property, 0);
                    vec![1, 2, 3]
                } else {
                    vec![property]
                };
                let expected: BTreeSet<_> = fields
                    .iter()
                    .map(|&property| previous[&(id, property)])
                    .collect();
                let actual: BTreeSet<_> = (0..reader.count().unwrap())
                    .map(|_| {
                        let distance = reader.u32().unwrap() as usize;
                        assert!(distance > 0 && distance <= ordinal);
                        ordinal - distance
                    })
                    .collect();
                assert_eq!(actual, expected, "event {ordinal}, instance {id}");
                reader.end().unwrap();
                for property in fields {
                    previous.insert((id, property), ordinal);
                }
            }
            8..=10 => {}
            _ => panic!("unexpected event kind"),
        }
    }
}

fn trimmed(source: &[u8], first: usize, last: usize) -> (Vec<u8>, Recording) {
    let bytes = trim(source, first, last);
    check_undo(&bytes);
    let recording = Recording::from_bytes(&bytes).unwrap();
    (bytes, recording)
}

#[test]
fn camera_memory_multi_scene_prefix_and_middle_preserve_independent_frame_values() {
    for initial in [false, true] {
        let source = Combined::new(initial, None).bytes();
        check_undo(&source);
        let (recording, info) = read_with_progress(&source, |_, _| true).unwrap();
        check_combined(&recording, 0, 7);
        assert_eq!(info.source_ids.get(&20), Some(&0xeee));
        for camera in [7, 8, 9, 10] {
            assert!(!info.source_ids.contains_key(&camera));
        }
        for last in [0, 1, 3, 4, 5, 7] {
            let (_, result) = trimmed(&source, 0, last);
            check_combined(&result, 0, last);
        }
        for (first, last) in [(1, 1), (1, 3), (2, 3), (2, 7), (4, 7), (5, 5), (6, 7)] {
            let (_, result) = trimmed(&source, first, last);
            check_combined(&result, first, last);
            assert_eq!(
                result.memory_history_scope(),
                MemoryHistoryScope::AfterInitialSnapshot
            );
        }
    }
}

#[test]
fn camera_memory_multi_scene_restores_gap_state_without_inventing_allocation_history() {
    for initial in [false, true] {
        let source = Combined::new(initial, None).bytes();
        let (bytes, recording) = trimmed(&source, 2, 7);
        let layout = Layout::parse(&bytes, &mut || true).unwrap();
        assert_eq!(
            initial_cameras(&layout),
            BTreeMap::from([(7, raw_camera(22, MAIN_C)), (8, raw_camera(15, LATE_A))])
        );
        assert_eq!(layout.communication.cameras, [(14, 7), (23, 9), (15, 8)]);
        let raw = preserved::raw(&bytes);
        assert_eq!(raw.reserved, 492);
        assert_eq!(raw.initial[&20].0, 8);
        let mut body = Reader::new(&raw.initial[&20].1);
        assert_eq!(body.blob().unwrap(), &[if initial { 0x5a } else { 0 }; 24]);
        assert_eq!(body.blob().unwrap(), FIRST);
        body.end().unwrap();
        assert_eq!(raw.memory, preserved::raw(&source).memory);
        assert_eq!(
            totals(&recording.native_initial_memory_totals().unwrap().unwrap()),
            [-7, 24]
        );
        assert_eq!(
            totals(&recording.native_end_memory_totals().unwrap().unwrap()),
            [-7, 17, 2]
        );
        let source_recording = Recording::from_bytes(&source).unwrap();
        assert_eq!(
            totals(
                &source_recording
                    .native_end_memory_totals()
                    .unwrap()
                    .unwrap()
            ),
            [-7, 17, 0]
        );
        for (id, class, bytes) in &raw.deleted {
            assert_eq!(*class, 4);
            let expected = match id {
                7 => raw_camera(22, MAIN_E),
                8 => raw_camera(15, LATE_F),
                _ => panic!("unexpected deleted instance"),
            };
            let mut body = Reader::new(bytes);
            assert_eq!(body.blob().unwrap(), expected);
            body.end().unwrap();
        }
        assert_eq!(raw.deleted.len(), 2);
        let memory = recording.analysis.memory_snapshot();
        let summary = memory.summary();
        assert_eq!(
            (
                summary.allocations,
                summary.frees,
                summary.unmatched_frees,
                summary.peak_bytes,
                summary.live_bytes,
                summary.total_allocated_bytes
            ),
            (4, 4, 1, 16, 2, 24)
        );
        assert_eq!((memory.event_count(), memory.sample_count()), (8, 8));
        assert_eq!(
            (0..memory.event_count())
                .map(|index| {
                    let event = memory.event_at(index).unwrap();
                    assert_eq!(event.buffer, 20);
                    (event.address, event.received_frame)
                })
                .collect::<Vec<_>>(),
            [
                (9, 0),
                (10, 0),
                (11, 1),
                (10, 2),
                (11, 3),
                (12, 3),
                (13, 4),
                (12, 5)
            ]
        );
        assert!(memory.event_at(0).unwrap().allocation.is_none());
        for (index, size, kind, file, line) in [
            (1, 4, "Future", "future.cpp", 9),
            (2, 12, "Allocation", "source.cpp", 7),
            (5, 6, "Other", "other.cpp", 8),
            (6, 2, "Future", "future.cpp", 9),
        ] {
            let allocation = memory.event_at(index).unwrap().allocation.unwrap();
            assert_eq!(
                (
                    allocation.size,
                    allocation.type_name,
                    allocation.file,
                    allocation.line
                ),
                (size, kind, file, line)
            );
        }
        let mut csv = Vec::new();
        assert!(
            crate::analysis_export::write_analysis_csv(
                &recording,
                &mut csv,
                crate::analysis_export::AnalysisCsvKind::Memory,
                |_| true
            )
            .is_err()
        );
        assert!(csv.is_empty());
    }
}

#[test]
fn camera_memory_multi_scene_retrim_matches_the_direct_raw_interval() {
    for initial in [false, true] {
        let source = Combined::new(initial, None).bytes();
        let (middle, _) = trimmed(&source, 2, 7);
        let (repeated, result) = trimmed(&middle, 2, 5);
        let (direct, _) = trimmed(&source, 4, 7);
        check_combined(&result, 4, 7);
        let raw = preserved::raw(&repeated);
        let expected = preserved::raw(&direct);
        assert_eq!(raw.initial, expected.initial);
        assert_eq!(raw.deleted, expected.deleted);
        assert_eq!(raw.memory, expected.memory);
        let left = Layout::parse(&repeated, &mut || true).unwrap();
        let right = Layout::parse(&direct, &mut || true).unwrap();
        assert_eq!(raw_history(&left), raw_history(&right));
        assert_eq!(left.communication.cameras, [(23, 9), (15, 8)]);
        assert_eq!(left.communication.cameras, right.communication.cameras);
        assert_eq!(
            initial_cameras(&left),
            BTreeMap::from([(8, raw_camera(15, LATE_D))])
        );
        assert_eq!(
            totals(&result.native_initial_memory_totals().unwrap().unwrap()),
            [5, 17, 4]
        );
        let memory = result.analysis.memory_snapshot();
        let summary = memory.summary();
        assert_eq!(
            (
                summary.allocations,
                summary.frees,
                summary.unmatched_frees,
                summary.total_allocated_bytes,
                summary.live_bytes
            ),
            (2, 3, 2, 8, 2)
        );
        assert_eq!((memory.event_count(), memory.sample_count()), (5, 5));
        let (_, prefix) = trimmed(&middle, 0, 1);
        check_combined(&prefix, 2, 3);
        let (prefix, _) = trimmed(&source, 0, 5);
        let (_, middle) = trimmed(&prefix, 2, 3);
        check_combined(&middle, 2, 3);
    }
}

#[test]
fn camera_memory_multi_scene_keeps_boundary_gap_and_undo_rejections() {
    for initial in [false, true] {
        let source = Combined::new(initial, None).bytes();
        for (first, last, reason) in [
            (0, 2, "ends inside"),
            (3, 7, "starts inside"),
            (7, 7, "starts inside"),
            (4, 6, "ends inside"),
        ] {
            let mut output = Vec::new();
            let failure =
                trim_scene_frames(&source, first, last, &mut output, || true).unwrap_err();
            assert!(failure.to_string().contains(reason), "{failure}");
            assert!(output.is_empty());
        }
        for gap in [&[][..], &[3, 0, 9]] {
            let source = Combined::new(initial, Some(gap)).bytes();
            let mut output = Vec::new();
            let failure = trim_scene_frames(&source, 2, 7, &mut output, || true).unwrap_err();
            assert!(failure.to_string().contains("first-frame gap"), "{failure}");
            assert!(output.is_empty());
        }
        for (id, property) in [(7u32, 1u32), (20, 8), (21, 9)] {
            let mut source = Combined::new(initial, None);
            let event = source
                .input
                .events
                .iter_mut()
                .find(|event| event.len() >= 12 && event[..12] == raw_words(&[3, id, property]))
                .unwrap();
            let offset = event.len() - 4;
            event[offset..].fill(0);
            let mut output = Vec::new();
            let failure =
                trim_scene_frames(&source.bytes(), 2, 7, &mut output, || true).unwrap_err();
            assert!(failure.to_string().contains("undo"), "{failure}");
            assert!(output.is_empty());
        }
    }
}

#[test]
fn camera_memory_multi_scene_disk_cursor_keeps_camera_and_activity_at_the_same_frame() {
    let source = Combined::new(true, None).bytes();
    let (bytes, expected) = trimmed(&source, 2, 7);
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../output/camera-memory-combo-tests")
        .join(format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    let history = crate::live::HistoryConfig {
        directory: directory.clone(),
        disk_budget_bytes: 32 << 20,
        segment_target_bytes: 1 << 20,
        segment_frames: 2,
        min_free_disk_bytes: 0,
    };
    let (recording, _) = crate::native::import::read_source_with_progress(
        std::io::Cursor::new(&bytes),
        Some(history),
        |_, _| true,
    )
    .unwrap();
    assert!(recording.is_disk_backed());
    assert_eq!(
        recording.memory_history_scope(),
        MemoryHistoryScope::AfterInitialSnapshot
    );
    let mut cursor = crate::live::ReplayCursor::default();
    let mut state = crate::SceneState::default();
    let mut memory_cursor = MemoryViewCursor::default();
    let summaries = [
        (1, 1, 4, 4, 1, 4),
        (2, 1, 16, 16, 2, 16),
        (2, 2, 16, 12, 1, 16),
        (3, 3, 16, 6, 1, 22),
        (4, 3, 16, 8, 2, 24),
        (4, 4, 16, 2, 1, 24),
    ];
    let event_ends = [2, 3, 4, 6, 7, 8];
    let sample_ends = [2, 3, 5, 6, 7, 8];
    for frame in (0..6).chain((0..6).rev()).chain([3, 0, 5, 1, 4, 2]) {
        let actual = cursor.seek(&recording, frame, || false).unwrap();
        actual.update.apply(&mut state);
        assert_eq!(state, expected.state_at(frame).unwrap());
        check_state(&state, frame + 2);
        assert_eq!(
            totals(&recording.native_memory_totals_at(&state).unwrap().unwrap()),
            TOTALS[frame + 2]
        );
        let memory = actual.analysis.memory_snapshot();
        let summary = memory.summary();
        assert_eq!(
            (
                summary.allocations,
                summary.frees,
                summary.peak_bytes,
                summary.live_bytes,
                summary.live_allocations,
                summary.total_allocated_bytes
            ),
            summaries[frame]
        );
        assert_eq!(summary.unmatched_frees, 1);
        assert_eq!(
            (memory.event_count(), memory.sample_count()),
            (event_ends[frame], sample_ends[frame])
        );
        memory.seek(&mut memory_cursor, frame).unwrap();
        assert_eq!(
            (memory_cursor.live_count(), memory_cursor.live_bytes()),
            (summary.live_allocations, summary.live_bytes)
        );
    }
    drop(memory_cursor);
    drop(cursor);
    drop(recording);
    assert!(std::fs::read_dir(&directory).unwrap().next().is_none());
    std::fs::remove_dir(directory).unwrap();
}

#[test]
#[ignore = "requires PVD_CAMERA_MEMORY_COMBO_DIRECTORY"]
fn write_camera_memory_multi_scene_fixtures() {
    let directory =
        std::path::PathBuf::from(std::env::var_os("PVD_CAMERA_MEMORY_COMBO_DIRECTORY").unwrap());
    std::fs::create_dir_all(&directory).unwrap();
    for initial in [false, true] {
        let source = Combined::new(initial, None).bytes();
        let mode = if initial { "initial" } else { "empty" };
        std::fs::write(
            directory.join(format!("camera-memory-{mode}.pxd2")),
            &source,
        )
        .unwrap();
        for (first, last) in [(0, 3), (2, 7), (4, 7)] {
            let (bytes, recording) = trimmed(&source, first, last);
            check_combined(&recording, first, last);
            std::fs::write(
                directory.join(format!("camera-memory-{mode}-{first}-{last}.pxd2")),
                bytes,
            )
            .unwrap();
        }
    }
}
