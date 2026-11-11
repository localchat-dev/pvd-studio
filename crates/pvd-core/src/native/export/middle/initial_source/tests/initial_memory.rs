use super::*;
use crate::analysis::view::MemorySource;
use crate::{MemoryHistoryScope, NativeMemoryTotals};

mod fixture;
use crate::native::export::memory_fixture as preserved;
use fixture::{Step, build};

fn rows(totals: &NativeMemoryTotals) -> Vec<i32> {
    (0..totals.len())
        .map(|index| totals.row(index).unwrap().bytes)
        .collect()
}

fn partial(recording: &Recording) {
    assert_eq!(
        recording.memory_history_scope(),
        MemoryHistoryScope::AfterInitialSnapshot
    );
    let mut csv = Vec::new();
    assert!(
        crate::analysis_export::write_analysis_csv(
            recording,
            &mut csv,
            crate::analysis_export::AnalysisCsvKind::Memory,
            |_| true,
        )
        .is_err()
    );
    assert!(csv.is_empty());
}

#[test]
fn initial_memory_middle_keeps_captured_totals_without_replaying_the_saved_buffer() {
    let source = preserved::source(false);
    let bytes = trim(&source, 1, 2);
    let original = preserved::raw(&source);
    let actual = preserved::raw(&bytes);
    assert_eq!(
        actual.initial[&2],
        (8, preserved::body(&preserved::first_flush()))
    );
    assert_eq!(
        actual.initial[&3],
        (9, preserved::body(&preserved::totals(&[0, 24])))
    );
    assert_eq!(actual.memory, original.memory);
    assert_eq!(actual.reserved, 492);
    assert_eq!(actual.events.len(), 9);
    assert_eq!(
        actual.events[2],
        set(3, 2, 3, &preserved::second_flush(), &[2])
    );
    assert_eq!(
        actual.events[3],
        set(3, 3, 4, &preserved::totals(&[0, 16]), &[3])
    );
    let recording = Recording::from_bytes(&bytes).unwrap();
    partial(&recording);
    let initial = recording.native_initial_memory_totals().unwrap().unwrap();
    assert_eq!(rows(&initial), [0, 24]);
    assert!(initial.row(2).is_none());
    let memory = recording.analysis.memory_snapshot();
    let summary = memory.summary();
    assert_eq!(
        (
            summary.allocations,
            summary.frees,
            summary.unmatched_frees,
            summary.live_bytes,
            summary.total_allocated_bytes
        ),
        (1, 1, 1, 4, 4)
    );
    assert_eq!((memory.event_count(), memory.sample_count()), (2, 2));
    assert_eq!(
        (
            memory.event_at(0).unwrap().address,
            memory.event_at(0).unwrap().received_frame
        ),
        (9, 0)
    );
    assert!(memory.event_at(0).unwrap().allocation.is_none());
    assert_eq!(
        (
            memory.event_at(1).unwrap().address,
            memory.event_at(1).unwrap().received_frame
        ),
        (10, 1)
    );
    assert_eq!(
        rows(&recording.native_end_memory_totals().unwrap().unwrap()),
        [0, 16, 4]
    );
    let twice = trim(&bytes, 1, 1);
    let direct = trim(&source, 2, 2);
    assert_eq!(
        preserved::raw(&twice).initial,
        preserved::raw(&direct).initial
    );
    assert_eq!(
        preserved::raw(&twice).events,
        preserved::raw(&direct).events
    );
}

#[test]
fn initial_memory_middle_without_preceding_sets_uses_the_exact_marker_baseline() {
    let source = build(
        &[
            Step::Begin(100),
            Step::End(101),
            Step::Begin(200),
            Step::Buffer(&[3, 0, 42]),
            Step::Totals(&[0, 16]),
            Step::End(201),
        ],
        &preserved::old_buffer(),
        &[64, 16],
    );
    let bytes = trim(&source, 1, 1);
    let actual = preserved::raw(&bytes);
    assert_eq!(
        actual.initial[&2],
        (8, preserved::body(&preserved::old_buffer()))
    );
    assert_eq!(
        actual.initial[&3],
        (9, preserved::body(&preserved::totals(&[64, 16])))
    );
    assert_eq!(actual.events[2], set(3, 2, 3, &[3, 0, 42], &[2]));
    let recording = Recording::from_bytes(&bytes).unwrap();
    partial(&recording);
    let summary = recording.analysis.memory_snapshot().summary();
    assert_eq!(
        (
            summary.allocations,
            summary.frees,
            summary.unmatched_frees,
            summary.live_bytes
        ),
        (0, 1, 1, 0)
    );
    assert_eq!(
        rows(&recording.native_initial_memory_totals().unwrap().unwrap()),
        [64, 16]
    );
}

#[test]
fn initial_memory_middle_without_flushes_keeps_signed_totals_and_empty_event_history() {
    for old_buffer in [&[][..], &[255][..]] {
        let source = build(
            &[
                Step::Begin(100),
                Step::Totals(&[-7]),
                Step::End(101),
                Step::Totals(&[-9, 3]),
                Step::Begin(200),
                Step::End(201),
            ],
            old_buffer,
            &[],
        );
        let bytes = trim(&source, 1, 1);
        let raw = preserved::raw(&bytes);
        assert_eq!(raw.initial[&2], (8, preserved::body(old_buffer)));
        assert_eq!(
            raw.initial[&3],
            (9, preserved::body(&preserved::totals(&[-9, 3])))
        );
        let recording = Recording::from_bytes(&bytes).unwrap();
        partial(&recording);
        let initial = recording.native_initial_memory_totals().unwrap().unwrap();
        assert_eq!(rows(&initial), [-9, 3]);
        assert!(initial.nonnegative_bytes().is_none());
        assert!(initial.row(2).is_none());
        let memory = recording.analysis.memory_snapshot();
        assert_eq!((memory.event_count(), memory.sample_count()), (0, 0));
        assert_eq!(memory.summary().total_allocated_bytes, 0);
    }
}

#[test]
fn initial_memory_middle_rejects_any_flush_in_the_first_frame_gap() {
    for gap in [&[][..], &[3, 0, 42][..]] {
        let source = build(
            &[
                Step::Begin(100),
                Step::End(101),
                Step::Buffer(gap),
                Step::Begin(200),
                Step::End(201),
            ],
            &preserved::old_buffer(),
            &[64, 16],
        );
        let mut output = Vec::new();
        let failure = trim_scene_frames(&source, 1, 1, &mut output, || true).unwrap_err();
        assert!(failure.to_string().contains("first-frame gap"), "{failure}");
        assert!(output.is_empty());
    }
}

#[test]
fn initial_memory_middle_rejects_pair_destruction_and_creation() {
    for raw_event in [destroy(2), create(2, 8), create(4, 8)] {
        let source = build(
            &[
                Step::Begin(100),
                Step::End(101),
                Step::Begin(200),
                Step::Raw(&raw_event),
                Step::End(201),
            ],
            &preserved::old_buffer(),
            &[64, 16],
        );
        let mut output = Vec::new();
        assert!(trim_scene_frames(&source, 1, 1, &mut output, || true).is_err());
        assert!(output.is_empty());
    }
}

#[test]
fn initial_memory_middle_proof_binds_fixed_payload_catalog_and_source_undo() {
    let source = preserved::source(false);
    let layout = Layout::parse(&source, &mut || true).unwrap();
    let proof = memory_middle::Proof::prepare(
        &layout,
        5,
        12,
        4,
        &BTreeMap::from([(1, 4), (2, 8), (3, 9)]),
        &mut 0,
        &mut || true,
    )
    .unwrap()
    .unwrap();
    let bytes = trim(&source, 1, 2);
    proof
        .verify_layout(
            &Layout::parse(&bytes, &mut || true).unwrap(),
            1,
            &mut || true,
        )
        .unwrap();
    for variant in 0..5 {
        let mut changed = Layout::parse(&bytes, &mut || true).unwrap();
        match variant {
            0 | 1 => {
                let saved = changed
                    .saved
                    .iter_mut()
                    .find(|saved| saved.id == 3 && !saved.deleted)
                    .unwrap();
                let at = if variant == 0 {
                    4
                } else {
                    saved.body.len() - 1
                };
                saved.body.to_mut()[at] ^= 1;
            }
            2 => changed.communication.memory_names[0].1 = b"other-source.cpp\0",
            3 | 4 => {
                let HistoryEvent::Set { data, undo, .. } = &mut changed.events[2] else {
                    unreachable!()
                };
                if variant == 3 {
                    data.to_mut()[2] ^= 1;
                } else {
                    *undo = vec![1];
                }
            }
            _ => unreachable!(),
        }
        let failure = proof.verify_layout(&changed, 1, &mut || true).unwrap_err();
        let expected = if variant <= 2 {
            "middle memory output changed captured initial properties or sources"
        } else {
            "middle memory output changed captured event history, Scene boundaries or undo references"
        };
        assert_eq!(failure.to_string(), expected, "variant {variant}");
    }
}

#[test]
fn initial_memory_middle_empty_totals_and_empty_retained_flush_remain_captured() {
    let source = build(
        &[
            Step::Begin(100),
            Step::End(101),
            Step::Begin(200),
            Step::Buffer(&[]),
            Step::End(201),
        ],
        &[],
        &[],
    );
    let bytes = trim(&source, 1, 1);
    let recording = Recording::from_bytes(&bytes).unwrap();
    partial(&recording);
    let totals = recording.native_initial_memory_totals().unwrap().unwrap();
    assert_eq!((totals.len(), totals.nonnegative_bytes()), (0, Some(0)));
    let memory = recording.analysis.memory_snapshot();
    assert_eq!((memory.event_count(), memory.sample_count()), (0, 1));
}

#[test]
fn initial_memory_middle_rejects_wrong_source_undo_before_writing() {
    let source = preserved::source(false);
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    let HistoryEvent::Set { undo, .. } = &mut layout.events[6] else {
        unreachable!()
    };
    *undo = vec![1];
    let bytes = layout.encode(false, &mut || true).unwrap();
    let mut output = Vec::new();
    let failure = trim_scene_frames(&bytes, 1, 2, &mut output, || true).unwrap_err();
    assert!(failure.to_string().contains("source undo"), "{failure}");
    assert!(output.is_empty());
}

#[test]
#[ignore = "retains fixtures in PVD_NATIVE_INITIAL_MEMORY_MIDDLE_DIRECTORY"]
fn retain_initial_memory_middle_fixtures() {
    let directory = std::path::PathBuf::from(
        std::env::var_os("PVD_NATIVE_INITIAL_MEMORY_MIDDLE_DIRECTORY")
            .expect("set PVD_NATIVE_INITIAL_MEMORY_MIDDLE_DIRECTORY"),
    );
    std::fs::create_dir_all(&directory).unwrap();
    let source = preserved::source(false);
    let no_flush = build(
        &[
            Step::Begin(100),
            Step::Totals(&[-7]),
            Step::End(101),
            Step::Totals(&[-9, 3]),
            Step::Begin(200),
            Step::End(201),
        ],
        &[],
        &[],
    );
    for (name, source, last) in [
        ("initial-memory-middle", source, 2),
        ("initial-memory-no-flush", no_flush, 1),
    ] {
        let output = trim(&source, 1, last);
        std::fs::write(directory.join(format!("{name}-source.pxd2")), source).unwrap();
        std::fs::write(directory.join(format!("{name}-trimmed.pxd2")), output).unwrap();
    }
}
