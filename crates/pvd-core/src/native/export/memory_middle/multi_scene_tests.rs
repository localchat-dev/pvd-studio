use super::*;
use crate::analysis::MemoryEventKind;
use crate::analysis::view::{
    MemoryAction, MemoryRows, MemorySource, MemorySummary, MemoryViewCursor,
};
use crate::native::export::memory_fixture as preserved;
use crate::{MemoryHistoryScope, NativeMemoryTotals, Recording, Value};

#[path = "multi_scene_tests/fixture.rs"]
mod fixture;
use fixture::{STEPS, Step, build};

const SCENES: [u64; 8] = [1, 4, 4, 1, 4, 1, 1, 4];
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

fn check_frames(recording: &Recording, first: usize, last: usize) {
    assert_eq!(recording.frame_count(), last - first + 1);
    for frame in (0..recording.frame_count())
        .chain((0..recording.frame_count()).rev())
        .chain([last - first, 0, (last - first) / 2, last - first])
    {
        let state = recording.state_at(frame).unwrap();
        assert_eq!(recording.frames[frame].scene_id, SCENES[first + frame]);
        assert_eq!(
            totals(&recording.native_memory_totals_at(&state).unwrap().unwrap()),
            TOTALS[first + frame]
        );
        for (id, value, peer) in [(1, 7, 4), (4, 11, 1)] {
            assert_eq!(state.objects[&id].properties["Value"], Value::U64(value));
            assert_eq!(
                state.objects[&id].properties["Peers"],
                Value::Array(vec![Value::Ref(2), Value::Ref(3), Value::Ref(peer)].into())
            );
        }
    }
    recording.verify_replay().unwrap();
}

fn trim(source: &[u8], first: usize, last: usize) -> (Vec<u8>, Recording) {
    let original = Recording::from_bytes(source).unwrap();
    let mut bytes = Vec::new();
    trim_scene_frames(source, first, last, &mut bytes, || true).unwrap();
    let result = Recording::from_bytes(&bytes).unwrap();
    assert_eq!(result.frame_count(), last - first + 1);
    for frame in (0..result.frame_count()).rev() {
        assert_eq!(
            result.state_at(frame).unwrap(),
            original.state_at(first + frame).unwrap()
        );
        assert_eq!(
            result.frames[frame].scene_id,
            original.frames[first + frame].scene_id
        );
        assert_eq!(
            result.frames[frame].timestamp,
            original.frames[first + frame].timestamp
        );
    }
    let mut rewritten = Vec::new();
    rewrite(&bytes, &mut rewritten, || true).unwrap();
    assert_eq!(bytes, rewritten);
    (bytes, result)
}

#[test]
fn multi_scene_memory_preserves_global_frame_order_and_source_totals() {
    for initial in [false, true] {
        let source = build(initial, STEPS);
        check_frames(&Recording::from_bytes(&source).unwrap(), 0, 7);
        for (first, last) in [(1, 1), (1, 3), (2, 3), (2, 7), (4, 7), (5, 5), (6, 7)] {
            let (_, recording) = trim(&source, first, last);
            check_frames(&recording, first, last);
            assert_eq!(
                recording.memory_history_scope(),
                MemoryHistoryScope::AfterInitialSnapshot
            );
        }
        for last in [0, 1, 3, 4, 5, 7] {
            let (_, recording) = trim(&source, 0, last);
            check_frames(&recording, 0, last);
        }
    }
}

#[test]
fn multi_scene_memory_rebases_observed_activity_without_replaying_snapshot_buffers() {
    for initial in [false, true] {
        let source = build(initial, STEPS);
        let (bytes, recording) = trim(&source, 2, 7);
        let raw = preserved::raw(&bytes);
        let mut saved = Reader::new(&raw.initial[&2].1);
        assert_eq!(saved.blob().unwrap(), &[if initial { 0x5a } else { 0 }; 24]);
        assert_eq!(saved.blob().unwrap(), fixture::FIRST);
        saved.end().unwrap();
        assert_eq!(
            totals(&recording.native_initial_memory_totals().unwrap().unwrap()),
            [-7, 24]
        );
        assert_eq!(
            totals(&recording.native_end_memory_totals().unwrap().unwrap()),
            [-7, 17, 2]
        );
        assert_eq!(
            totals(
                &Recording::from_bytes(&source)
                    .unwrap()
                    .native_end_memory_totals()
                    .unwrap()
                    .unwrap()
            ),
            [-7, 17, 0]
        );
        let memory = &recording.analysis.legacy().unwrap().memory;
        assert_eq!(
            (
                memory.allocations,
                memory.frees,
                memory.unmatched_frees,
                memory.peak_bytes,
                memory.live_bytes,
                memory.total_allocated_bytes
            ),
            (4, 4, 1, 16, 2, 24)
        );
        assert_eq!(memory.samples.len(), 8);
        assert_eq!(
            memory
                .events()
                .iter()
                .map(|event| (event.address, event.received_frame))
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
        assert!(matches!(
            &memory.events()[0].kind,
            MemoryEventKind::Free { allocation: None }
        ));
        let allocations: Vec<_> = memory
            .events()
            .iter()
            .filter_map(|event| match &event.kind {
                MemoryEventKind::Allocate { allocation, .. } => Some((
                    event.address,
                    allocation.size,
                    allocation.type_name.as_ref(),
                    allocation.file.as_ref(),
                    allocation.line,
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            allocations,
            [
                (10, 4, "Future", "future.cpp", 9),
                (11, 12, "Allocation", "source.cpp", 7),
                (12, 6, "Other", "other.cpp", 8),
                (13, 2, "Future", "future.cpp", 9)
            ]
        );
        assert_eq!(raw.memory, preserved::raw(&source).memory);
        let layout = Layout::parse(&bytes, &mut || true).unwrap();
        let first = layout
            .events
            .iter()
            .enumerate()
            .filter_map(|(ordinal, event)| match event {
                HistoryEvent::Set {
                    id: 2 | 3, undo, ..
                } => Some((ordinal, undo.clone())),
                _ => None,
            })
            .take(3)
            .collect::<Vec<_>>();
        assert_eq!(first, [(2, vec![2]), (4, vec![2]), (5, vec![5])]);
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
fn multi_scene_memory_repeated_middle_matches_direct_interval() {
    for initial in [false, true] {
        let source = build(initial, STEPS);
        let (middle, _) = trim(&source, 2, 7);
        let (repeated, recording) = trim(&middle, 2, 5);
        let (direct, expected) = trim(&source, 4, 7);
        check_frames(&recording, 4, 7);
        assert_eq!(
            preserved::raw(&repeated).initial,
            preserved::raw(&direct).initial
        );
        assert_eq!(
            preserved::raw(&repeated).events,
            preserved::raw(&direct).events
        );
        assert_eq!(
            recording.initial_state().objects,
            expected.initial_state().objects
        );
        assert_eq!(
            totals(&recording.native_initial_memory_totals().unwrap().unwrap()),
            [5, 17, 4]
        );
        let memory = recording.analysis.memory_snapshot();
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
        let (_, prefix) = trim(&middle, 0, 1);
        check_frames(&prefix, 2, 3);
    }
}

#[test]
fn multi_scene_memory_keeps_gap_flush_and_partial_frame_rejections() {
    for initial in [false, true] {
        for data in [&[][..], &[3, 0, 9]] {
            let mut steps = STEPS.to_vec();
            steps.insert(7, Step::Buffer(data));
            let source = build(initial, &steps);
            let mut output = Vec::new();
            let failure = trim_scene_frames(&source, 2, 7, &mut output, || true).unwrap_err();
            assert!(failure.to_string().contains("first-frame gap"), "{failure}");
            assert!(output.is_empty());
        }
        let source = build(initial, STEPS);
        for (first, last, message) in [
            (3, 7, "starts inside"),
            (7, 7, "starts inside"),
            (2, 2, "ends inside"),
            (4, 6, "ends inside"),
            (1, 8, "out of range"),
        ] {
            let mut output = Vec::new();
            let failure =
                trim_scene_frames(&source, first, last, &mut output, || true).unwrap_err();
            assert!(failure.to_string().contains(message), "{failure}");
            assert!(output.is_empty());
        }
        for bytes in [
            [2u32, 2]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>(),
            [1u32, 5, 8]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect(),
        ] {
            let mut steps = STEPS.to_vec();
            steps.insert(13, Step::Raw(&bytes));
            let source = build(initial, &steps);
            let mut output = Vec::new();
            assert!(trim_scene_frames(&source, 2, 7, &mut output, || true).is_err());
            assert!(output.is_empty());
        }
    }
}

#[test]
fn multi_scene_memory_proof_binds_section_delivery_boundaries() {
    for initial in [false, true] {
        let source = build(initial, STEPS);
        let (bytes, _) = trim(&source, 2, 7);
        let layout = Layout::parse(&source, &mut || true).unwrap();
        let interval = layout.middle_frame_interval(2, 7, &mut || true).unwrap();
        let proof = Proof::prepare(
            &layout,
            interval.start,
            interval.end - 1,
            interval.previous_end,
            &BTreeMap::from([(1, 4), (4, 4), (2, 8), (3, 9)]),
            &mut 0,
            &mut || true,
        )
        .unwrap()
        .unwrap();
        let output = Layout::parse(&bytes, &mut || true).unwrap();
        proof.verify_layout(&output, 1, &mut || true).unwrap();
        for variant in 0..6 {
            let mut output = Layout::parse(&bytes, &mut || true).unwrap();
            let event = output
                .events
                .iter_mut()
                .find(|event| matches!(event, HistoryEvent::Section { begin: 0, .. }))
                .unwrap();
            let HistoryEvent::Section {
                class,
                source,
                ticks,
                name,
                begin,
                padding,
                ..
            } = event
            else {
                unreachable!()
            };
            match variant {
                0 => *source ^= 1,
                1 => *ticks += 1,
                2 => *name ^= 1,
                3 => *begin = 1,
                4 => padding[0] ^= 1,
                5 => *class ^= 1,
                _ => unreachable!(),
            }
            let failure = proof.verify_layout(&output, 1, &mut || true).unwrap_err();
            assert!(failure.to_string().contains("event history"), "{failure}");
        }
    }
}

#[test]
fn multi_scene_memory_requires_source_and_output_undo_chains() {
    for initial in [false, true] {
        let source = build(initial, STEPS);
        for variant in 0..4 {
            let mut layout = Layout::parse(&source, &mut || true).unwrap();
            let interval = layout.middle_frame_interval(2, 7, &mut || true).unwrap();
            let ordinal = interval.start + 1;
            let HistoryEvent::Set { id: 2, undo, .. } = &mut layout.events[ordinal] else {
                panic!("expected the first retained memory flush");
            };
            *undo = match variant {
                0 => vec![],
                1 => vec![0],
                2 => vec![(ordinal - interval.previous_end) as u32],
                3 => vec![ordinal as u32],
                _ => unreachable!(),
            };
            let failure = Proof::prepare(
                &layout,
                interval.start,
                interval.end - 1,
                interval.previous_end,
                &BTreeMap::from([(1, 4), (4, 4), (2, 8), (3, 9)]),
                &mut 0,
                &mut || true,
            )
            .unwrap_err();
            assert!(failure.to_string().contains("source undo"), "{failure}");
        }
        let layout = Layout::parse(&source, &mut || true).unwrap();
        let interval = layout.middle_frame_interval(2, 7, &mut || true).unwrap();
        let proof = Proof::prepare(
            &layout,
            interval.start,
            interval.end - 1,
            interval.previous_end,
            &BTreeMap::from([(1, 4), (4, 4), (2, 8), (3, 9)]),
            &mut 0,
            &mut || true,
        )
        .unwrap()
        .unwrap();
        let (bytes, _) = trim(&source, 2, 7);
        let mut output = Layout::parse(&bytes, &mut || true).unwrap();
        let HistoryEvent::Set { id: 2, undo, .. } = &mut output.events[2] else {
            panic!("expected the first output memory flush");
        };
        undo.clear();
        let failure = proof.verify_layout(&output, 1, &mut || true).unwrap_err();
        assert!(failure.to_string().contains("undo reference"), "{failure}");
    }
}

#[test]
fn multi_scene_memory_disk_cursor_preserves_forward_reverse_and_random_activity() {
    let source = build(true, STEPS);
    let (bytes, expected) = trim(&source, 2, 7);
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../output/multi-scene-memory-tests")
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
    let mut legacy_cursor = MemoryViewCursor::default();
    let legacy = expected.analysis.memory_snapshot();
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
    let events = [
        (9, 0, MemoryAction::Free, None),
        (
            10,
            0,
            MemoryAction::Allocate,
            Some((4, "Future", "future.cpp", 9, 0)),
        ),
        (
            11,
            1,
            MemoryAction::Allocate,
            Some((12, "Allocation", "source.cpp", 7, 1)),
        ),
        (
            10,
            2,
            MemoryAction::Free,
            Some((4, "Future", "future.cpp", 9, 0)),
        ),
        (
            11,
            3,
            MemoryAction::Free,
            Some((12, "Allocation", "source.cpp", 7, 1)),
        ),
        (
            12,
            3,
            MemoryAction::Allocate,
            Some((6, "Other", "other.cpp", 8, 3)),
        ),
        (
            13,
            4,
            MemoryAction::Allocate,
            Some((2, "Future", "future.cpp", 9, 4)),
        ),
        (
            12,
            5,
            MemoryAction::Free,
            Some((6, "Other", "other.cpp", 8, 3)),
        ),
    ];
    let samples = [
        (0, 0, 0, 0, 1),
        (0, 4, 1, 1, 1),
        (1, 16, 2, 2, 1),
        (2, 12, 1, 2, 2),
        (2, 12, 1, 2, 2),
        (3, 6, 1, 3, 3),
        (4, 8, 2, 4, 3),
        (5, 2, 1, 4, 4),
    ];
    let live: [&[(u64, u64, usize)]; 6] = [
        &[(10, 4, 0)],
        &[(10, 4, 0), (11, 12, 1)],
        &[(11, 12, 1)],
        &[(12, 6, 3)],
        &[(12, 6, 3), (13, 2, 4)],
        &[(13, 2, 4)],
    ];
    for frame in (0..6).chain((0..6).rev()).chain([3, 0, 5, 1, 4, 2]) {
        let actual = cursor.seek(&recording, frame, || false).unwrap();
        actual.update.apply(&mut state);
        assert_eq!(state, expected.state_at(frame).unwrap());
        assert_eq!(
            totals(&recording.native_memory_totals_at(&state).unwrap().unwrap()),
            TOTALS[frame + 2]
        );
        let actual = actual.analysis.memory_snapshot();
        let (allocations, frees, peak_bytes, live_bytes, live_allocations, total_allocated_bytes) =
            summaries[frame];
        assert_eq!(
            actual.summary(),
            MemorySummary {
                allocations,
                frees,
                peak_bytes,
                unmatched_frees: 1,
                replaced_addresses: 0,
                live_bytes,
                live_allocations,
                total_allocated_bytes,
            },
            "frame {frame}"
        );
        assert_eq!(
            (actual.event_count(), actual.sample_count()),
            (event_ends[frame], sample_ends[frame]),
            "frame {frame}"
        );
        let start = if frame == 0 { 0 } else { event_ends[frame - 1] };
        assert_eq!(actual.frame_events(frame), start..event_ends[frame]);
        assert_eq!(legacy.frame_events(frame), start..event_ends[frame]);
        for (index, &event) in events[..event_ends[frame]].iter().enumerate() {
            for value in [
                actual.event_at(index).unwrap(),
                legacy.event_at(index).unwrap(),
            ] {
                assert_eq!(value.buffer, 2);
                assert_eq!(
                    (
                        value.address,
                        value.received_frame,
                        value.action,
                        value.allocation.map(|a| (
                            a.size,
                            a.type_name,
                            a.file,
                            a.line,
                            a.received_frame
                        ))
                    ),
                    event
                );
            }
        }
        for (index, &sample) in samples[..sample_ends[frame]].iter().enumerate() {
            for value in [
                actual.sample_at(index).unwrap(),
                legacy.sample_at(index).unwrap(),
            ] {
                assert_eq!(
                    (
                        value.received_frame,
                        value.live_bytes,
                        value.live_allocations,
                        value.allocations,
                        value.frees
                    ),
                    sample
                );
            }
        }
        actual.seek(&mut memory_cursor, frame).unwrap();
        legacy.seek(&mut legacy_cursor, frame).unwrap();
        for rows in [&memory_cursor, &legacy_cursor] {
            assert_eq!(
                (rows.live_count(), rows.live_bytes()),
                (live_allocations, live_bytes)
            );
            let mut allocations = (0..rows.live_count())
                .map(|index| {
                    let value = rows.allocation_at(index).unwrap();
                    (value.address, value.size, value.received_frame)
                })
                .collect::<Vec<_>>();
            allocations.sort_unstable();
            assert_eq!(allocations, live[frame]);
        }
    }
    drop(memory_cursor);
    drop(legacy_cursor);
    drop(cursor);
    drop(recording);
    assert!(std::fs::read_dir(&directory).unwrap().next().is_none());
    std::fs::remove_dir(&directory).unwrap();
}
