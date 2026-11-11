use super::*;
use crate::analysis::view::{MemoryRows, MemorySource, MemoryViewCursor};
use crate::analysis_export::{AnalysisCsvKind, write_analysis_csv};
use crate::{MemoryHistoryScope, NativeMemoryTotals, Recording, Value};

use crate::native::export::memory_fixture as fixture;
use fixture::*;

fn row_values(totals: &NativeMemoryTotals) -> Vec<i32> {
    (0..totals.len())
        .map(|index| totals.row(index).unwrap().bytes)
        .collect()
}

fn normalized_events(events: &[Vec<u8>]) -> Vec<Vec<u8>> {
    events
        .iter()
        .map(|event| {
            let mut bytes = event.clone();
            if bytes[..4] == 10u32.to_le_bytes() {
                bytes[8..16].fill(0);
            }
            bytes
        })
        .collect()
}

fn trim(source: &[u8], last: usize) -> Vec<u8> {
    let mut output = Vec::new();
    trim_scene_frames(source, 0, last, &mut output, || true).unwrap();
    let original = raw(source);
    let actual = raw(&output);
    assert_eq!(actual.initial, original.initial);
    assert_eq!(actual.initial_positions[&1], 40);
    assert_eq!(actual.memory, original.memory);
    assert_eq!(actual.reserved, 492);
    assert_eq!(
        normalized_events(&actual.events),
        normalized_events(&original.events[..actual.events.len()])
    );
    let mut rewritten = Vec::new();
    rewrite(&output, &mut rewritten, || true).unwrap();
    assert_eq!(rewritten, output);
    output
}

#[test]
fn prefix_keeps_initial_memory_bytes_and_observed_events_without_inventing_prehistory() {
    let source = source(false);
    assert_eq!(raw(&source).initial_positions[&1], 128);
    let output = trim(&source, 0);
    let raw = raw(&output);
    assert_eq!(raw.initial[&2], (8, body(&old_buffer())));
    assert_eq!(raw.initial[&3], (9, body(&totals(&[64, 16]))));
    assert_eq!(raw.events.len(), 5);
    assert!(raw.deleted.is_empty());
    let recording = Recording::from_bytes(&output).unwrap();
    assert_eq!(
        recording.memory_history_scope(),
        MemoryHistoryScope::AfterInitialSnapshot
    );
    assert_eq!(
        row_values(&recording.native_initial_memory_totals().unwrap().unwrap()),
        vec![64, 16]
    );
    let state = recording.state_at(0).unwrap();
    let current = recording.native_memory_totals_at(&state).unwrap().unwrap();
    assert_eq!(row_values(&current), vec![0, 24]);
    assert_eq!(current.row(1).unwrap().type_name, "Other");
    assert_eq!(current.row(1).unwrap().file, "other.cpp");
    assert!(current.row(2).is_none());
    assert_eq!(
        row_values(&recording.native_end_memory_totals().unwrap().unwrap()),
        vec![0, 24]
    );
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
        (1, 1, 1, 8, 8)
    );
    assert_eq!(memory.event_count(), 2);
    assert_eq!(memory.sample_count(), 1);
    assert_eq!(memory.event_at(0).unwrap().address, 42);
    assert!(memory.event_at(0).unwrap().allocation.is_none());
    assert_eq!(memory.event_at(1).unwrap().address, 9);
    assert_eq!(
        memory.event_at(1).unwrap().allocation.unwrap().type_name,
        "Other"
    );
    assert_eq!(memory.event_at(1).unwrap().received_frame, 0);
    let mut cursor = MemoryViewCursor::default();
    memory.seek(&mut cursor, 0).unwrap();
    assert_eq!(cursor.live_count(), 1);
    assert_eq!(cursor.allocation_at(0).unwrap().address, 9);
    assert_eq!(
        state.objects[&1].properties["Peers"],
        Value::Array(vec![Value::Ref(2), Value::Ref(3)].into())
    );
    let mut csv = Vec::new();
    assert!(
        write_analysis_csv(&recording, &mut csv, AnalysisCsvKind::Memory, |_| true)
            .unwrap_err()
            .to_string()
            .contains("before the initial snapshot")
    );
    assert!(csv.is_empty());
    recording.verify_replay().unwrap();
}

#[test]
fn repeated_prefixes_preserve_future_catalog_and_each_actual_totals_length() {
    let source = source(false);
    let first = trim(&source, 1);
    let twice = trim(&first, 0);
    assert_eq!(twice, trim(&source, 0));
    let recording = Recording::from_bytes(&first).unwrap();
    let full = Recording::from_bytes(&source).unwrap();
    for frame in [1, 0, 1, 0] {
        let state = recording.state_at(frame).unwrap();
        let totals = recording.native_memory_totals_at(&state).unwrap().unwrap();
        assert_eq!(
            row_values(&totals),
            if frame == 0 { vec![0, 24] } else { vec![0, 16] }
        );
        assert!(totals.row(2).is_none());
        assert_eq!(state, full.state_at(frame).unwrap());
    }
    let final_state = full.state_at(2).unwrap();
    assert_eq!(
        row_values(&full.native_memory_totals_at(&final_state).unwrap().unwrap()),
        vec![0, 16, 4]
    );
    assert_eq!(
        row_values(&full.native_end_memory_totals().unwrap().unwrap()),
        vec![0, 16]
    );
    assert_eq!(recording.analysis.memory_snapshot().summary().live_bytes, 0);
    assert_eq!(
        recording
            .analysis
            .memory_snapshot()
            .summary()
            .unmatched_frees,
        1
    );
    recording.verify_replay().unwrap();
}

#[test]
fn initial_and_deleted_memory_versions_follow_their_own_saved_positions() {
    let source = source(true);
    let before = trim(&source, 0);
    assert!(raw(&before).deleted.is_empty());
    let output = trim(&source, 1);
    let raw = raw(&output);
    assert_eq!(
        raw.deleted,
        vec![
            (2, 8, body(&second_flush())),
            (3, 9, body(&totals(&[0, 16])))
        ]
    );
    assert_ne!(raw.initial[&2].1, raw.deleted[0].2);
    assert_ne!(raw.initial[&3].1, raw.deleted[1].2);
    let recording = Recording::from_bytes(&output).unwrap();
    assert_eq!(
        row_values(&recording.native_initial_memory_totals().unwrap().unwrap()),
        vec![64, 16]
    );
    assert!(recording.native_end_memory_totals().unwrap().is_none());
    let state = recording.state_at(1).unwrap();
    assert!(!state.objects.contains_key(&2));
    assert!(!state.objects.contains_key(&3));
    assert_eq!(
        state.objects[&1].properties["Peers"],
        Value::Array(vec![].into())
    );
    let summary = recording.analysis.memory_snapshot().summary();
    assert_eq!(
        (
            summary.allocations,
            summary.frees,
            summary.unmatched_frees,
            summary.live_bytes
        ),
        (1, 2, 1, 0)
    );
    assert_eq!(trim(&output, 0), before);
    recording.verify_replay().unwrap();
}

fn encoded(layout: &mut Layout<'_>) -> Vec<u8> {
    layout.pages[0].count = layout.events.len() as u32;
    layout.pages[0].len = encode_page(&layout.events, &mut || true).unwrap().len() as u32;
    layout.encode(false, &mut || true).unwrap()
}

#[test]
fn dedicated_memory_proof_rejects_initial_totals_buffer_catalog_and_flush_changes() {
    let source = source(false);
    let layout = Layout::parse(&source, &mut || true).unwrap();
    let cut = layout.prefix_cut(0, &mut || true).unwrap();
    let proof = Proof::prepare(&layout, cut, &mut || true).unwrap().unwrap();
    let output = trim(&source, 0);
    for change in 0..5 {
        let mut layout = Layout::parse(&output, &mut || true).unwrap();
        match change {
            0 | 1 => {
                let id = if change == 0 { 2 } else { 3 };
                let initial = State::read(&layout, &mut || true).unwrap();
                let record = initial
                    .entries
                    .iter()
                    .find(|entry| entry.id == id)
                    .unwrap()
                    .record;
                let body = layout.saved[record].body.to_mut();
                let last = body.len() - 1;
                body[last] ^= 1;
            }
            2 => layout.communication.memory_names[0].2 += 1,
            3 => {
                let HistoryEvent::Set { data, .. } = &mut layout.events[2] else {
                    panic!()
                };
                data.to_mut()[2] = 41;
            }
            4 => {
                let HistoryEvent::Set { data, .. } = &mut layout.events[3] else {
                    panic!()
                };
                *data = totals(&[0, 24, 0]).into();
            }
            _ => unreachable!(),
        }
        assert!(
            proof.verify_layout(&layout, &mut || true).is_err(),
            "change {change}"
        );
    }
}

#[test]
fn unsupported_pair_layout_directory_array_edit_and_lifetimes_remain_rejected() {
    let source = source(false);
    for change in 0..8 {
        let mut layout = Layout::parse(&source, &mut || true).unwrap();
        match change {
            0 => layout.communication.memory_instances[1] = 99,
            1 => layout.communication.memory_names[2].3 = 0,
            2 => layout.metadata.classes[9].as_mut().unwrap().size = 12,
            3 => {
                layout.events[3] = HistoryEvent::Array {
                    kind: 7,
                    id: 3,
                    property: 4,
                    index: 0,
                    data: Some(&[0, 0, 0, 0]),
                    undo: vec![3],
                }
            }
            4 => layout.events.push(HistoryEvent::Create(4, 8)),
            5 => layout.events.push(HistoryEvent::Create(2, 8)),
            6 => layout.metadata.classes[9].as_mut().unwrap().base = 3,
            7 => {
                let HistoryEvent::Set { data, .. } = &mut layout.events[3] else {
                    panic!()
                };
                *data = totals(&[0, 0, 0, 0]).into();
            }
            _ => unreachable!(),
        }
        let initial = State::read(&layout, &mut || true).unwrap();
        assert!(
            initial.validate_prefix(&layout, 5, &mut || true).is_err(),
            "change {change}"
        );
    }
}

#[test]
fn aliasing_a_final_witness_to_initial_memory_does_not_replace_its_state() {
    let source = source(true);
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    let initial = State::read(&layout, &mut || true).unwrap();
    let entry = initial.entries.iter().find(|entry| entry.id == 2).unwrap();
    let position = entry.position;
    layout.saved[entry.record].deleted = true;
    layout
        .saved
        .retain(|record| record.id != 2 || record.position == position);
    let witness = layout
        .events
        .iter_mut()
        .find(|event| matches!(event, HistoryEvent::Saved { id: 2, .. }))
        .unwrap();
    let HistoryEvent::Saved {
        position: target, ..
    } = witness
    else {
        unreachable!()
    };
    *target = position;
    let source = encoded(&mut layout);
    let mut output = Vec::new();
    assert!(
        trim_scene_frames(&source, 0, 1, &mut output, || true)
            .unwrap_err()
            .to_string()
            .contains("saved record disagrees")
    );
    assert!(output.is_empty());
}

#[test]
fn source_index_budget_and_proof_cancellation_are_bounded() {
    let source = source(false);
    let layout = Layout::parse(&source, &mut || true).unwrap();
    let initial = State::read(&layout, &mut || true).unwrap();
    let count = layout.communication.memory_names.len();
    let mut budget = Budget {
        bytes: MAX_METADATA - count,
    };
    Scope::read(&initial, &layout, &mut budget, &mut || true)
        .unwrap()
        .unwrap();
    assert_eq!(budget.bytes, MAX_METADATA);
    let mut budget = Budget {
        bytes: MAX_METADATA - count + 1,
    };
    assert!(
        Scope::read(&initial, &layout, &mut budget, &mut || true)
            .err()
            .unwrap()
            .to_string()
            .contains("metadata exceeds 64 MiB")
    );
    let mut total = 0;
    Proof::prepare(&layout, 5, &mut || {
        total += 1;
        true
    })
    .unwrap();
    for stop in [1, 8, total / 2, total] {
        let mut checks = 0;
        let failure = Proof::prepare(&layout, 5, &mut || {
            checks += 1;
            checks < stop
        })
        .err()
        .unwrap();
        assert_eq!(checks, stop);
        assert_eq!(
            failure.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
    }
}

#[test]
fn signed_initial_totals_are_preserved_without_a_synthetic_nonnegative_sum() {
    let source = source(false);
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    let initial = State::read(&layout, &mut || true).unwrap();
    let record = initial
        .entries
        .iter()
        .find(|entry| entry.id == 3)
        .unwrap()
        .record;
    layout.saved[record].body = body(&totals(&[-1, 16])).into();
    let source = encoded(&mut layout);
    let output = trim(&source, 0);
    let recording = Recording::from_bytes(&output).unwrap();
    let initial = recording.native_initial_memory_totals().unwrap().unwrap();
    assert_eq!(row_values(&initial), vec![-1, 16]);
    assert_eq!(initial.nonnegative_bytes(), None);
    assert_eq!(
        recording.analysis.memory_snapshot().summary().allocations,
        1
    );
}

#[test]
fn memory_allowance_preserves_initial_reference_and_scene_lifetime_restrictions() {
    let source = source(false);
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    let initial = State::read(&layout, &mut || true).unwrap();
    let record = initial
        .entries
        .iter()
        .find(|entry| entry.id == 1)
        .unwrap()
        .record;
    let body = layout.saved[record].body.to_mut();
    let end = body.len();
    body[end - 4..].copy_from_slice(&99u32.to_le_bytes());
    assert!(
        initial
            .validate_prefix(&layout, 5, &mut || true)
            .unwrap_err()
            .to_string()
            .contains("outside retained object lifecycles")
    );

    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    layout.events.push(HistoryEvent::Create(1, 4));
    let source = encoded(&mut layout);
    let mut output = Vec::new();
    let failure = trim_scene_frames(&source, 0, 0, &mut output, || true).unwrap_err();
    assert!(
        failure
            .to_string()
            .contains("instance 0x1 created while still alive"),
        "{failure}"
    );
    assert!(output.is_empty());
}

#[test]
fn retained_memory_witnesses_must_remain_in_the_deleted_directory() {
    let source = source(true);
    let layout = Layout::parse(&source, &mut || true).unwrap();
    let cut = layout.prefix_cut(1, &mut || true).unwrap();
    let proof = super::super::Proof::prepare(&layout, cut, &mut || true).unwrap();
    let output = trim(&source, 1);
    let mut layout = Layout::parse(&output, &mut || true).unwrap();
    layout
        .saved
        .iter_mut()
        .find(|record| record.id == 2 && record.deleted)
        .unwrap()
        .deleted = false;
    assert!(
        proof
            .verify_layout(&layout, &mut || true)
            .unwrap_err()
            .to_string()
            .contains("deleted directory")
    );
}

#[test]
#[ignore = "writes constructed fixtures to PVD_NATIVE_MEMORY_PREFIX_FIXTURE_DIRECTORY"]
fn write_constructed_memory_prefix_fixtures() {
    let directory = std::path::PathBuf::from(
        std::env::var_os("PVD_NATIVE_MEMORY_PREFIX_FIXTURE_DIRECTORY")
            .expect("fixture output directory"),
    );
    std::fs::create_dir_all(&directory).unwrap();
    let ordinary = source(false);
    let deleted = source(true);
    for (name, bytes) in [
        ("memory-prefix-source.pxd2", ordinary.clone()),
        ("memory-prefix-frame-0.pxd2", trim(&ordinary, 0)),
        ("memory-prefix-frames-0-1.pxd2", trim(&ordinary, 1)),
        ("memory-prefix-deleted-source.pxd2", deleted.clone()),
        ("memory-prefix-deleted-frames-0-1.pxd2", trim(&deleted, 1)),
    ] {
        std::fs::write(directory.join(name), bytes).unwrap();
    }
}
