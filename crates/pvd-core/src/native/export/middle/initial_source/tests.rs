use super::*;
use crate::{Recording, Value};

mod fixture;
mod initial_analysis;
mod initial_memory;
mod saved_lifetimes;
use fixture::*;

fn trim(source: &[u8], first: usize, last: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    trim_scene_frames(source, first, last, &mut bytes, || true).unwrap();
    let original = Recording::from_bytes(source).unwrap();
    let result = Recording::from_bytes(&bytes).unwrap();
    assert_eq!(result.frame_count(), last - first + 1);
    for frame in (0..result.frame_count())
        .chain((0..result.frame_count()).rev())
        .chain([0, last - first, 0])
    {
        let actual = result.state_at(frame).unwrap();
        let expected = original.state_at(frame + first).unwrap();
        assert_eq!(actual.objects, expected.objects);
        assert_eq!(actual.debug_draw, expected.debug_draw);
        assert_eq!(actual.contacts, expected.contacts);
        assert_eq!(actual.errors, expected.errors);
    }
    result.verify_replay().unwrap();
    let mut rewritten = Vec::new();
    rewrite(&bytes, &mut rewritten, || true).unwrap();
    assert_eq!(rewritten, bytes);
    bytes
}

fn fail(source: &[u8], expected: &str) {
    let mut output = Vec::new();
    let failure = trim_scene_frames(source, 1, 2, &mut output, || true).unwrap_err();
    assert!(
        failure.to_string().contains(expected),
        "expected {expected:?}, got {failure}"
    );
    assert!(output.is_empty());
}

fn initial_errors(recording: &Recording) -> Vec<(u64, u32)> {
    recording
        .initial_untimed_errors()
        .iter()
        .map(|error| {
            assert_eq!(
                (
                    error.code,
                    error.message.as_str(),
                    error.file.as_str(),
                    error.line,
                    error.container_generation
                ),
                (4, "Failure", "source.cpp", 42, 0)
            );
            (error.container_instance, error.index)
        })
        .collect()
}

#[test]
fn middle_from_initial_snapshot_keeps_raw_fields_occurrences_and_saved_versions() {
    let source = fixture().bytes();
    let original = raw(&source);
    assert_eq!(original.initial[&2], (4, body(7, &[10, 20], 1, 0xa1b2c3d4)));
    assert_eq!(original.initial[&4], (8, error_body(&[50])));
    assert_eq!(original.positions[&2], 256);
    let bytes = trim(&source, 1, 2);
    let actual = raw(&bytes);
    assert_eq!(
        actual.initial.keys().copied().collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert_eq!(actual.initial[&1], (0, vec![0; 4]));
    assert_eq!(
        actual.initial[&2],
        (4, body(9, &[10, 25, 30], 1, 0xa1b2c3d4))
    );
    assert_eq!(actual.initial[&3], (8, error_body(&[7, 7, 8])));
    assert_eq!(actual.reserved, 492);
    assert_eq!(actual.rows, [[200, 1, 201, 7], [300, 8, 301, 14]]);
    assert_eq!(actual.reverse, [1, 3, 5]);
    assert_eq!(actual.deleted.len(), 1);
    assert_eq!(
        (actual.deleted[&2].1, &actual.deleted[&2].2),
        (4, &body(12, &[25, 30, 40], 0, 0xdeadbeef))
    );
    assert_ne!(actual.positions[&2], actual.deleted[&2].0);
    let expected = vec![
        9u32.to_le_bytes().to_vec(),
        original.events[9].clone(),
        set(3, 2, 1, &12u32.to_le_bytes(), &[2]),
        array(6, 2, 2, 0, &[], &[3]),
        array(5, 3, 9, 3, &error_record(9), &[]),
        create(5, 4),
        set(3, 5, 1, &42u32.to_le_bytes(), &[1]),
        original.events[15].clone(),
        original.events[16].clone(),
        array(5, 2, 2, 2, &40u32.to_le_bytes(), &[]),
        set(3, 2, 3, &0u32.to_le_bytes(), &[10]),
        saved(2, actual.deleted[&2].0),
        destroy(2),
        array(5, 3, 9, 4, &error_record(10), &[]),
        original.events[22].clone(),
    ];
    assert_eq!(actual.events, expected);
    let result = Recording::from_bytes(&bytes).unwrap();
    assert_eq!(initial_errors(&result), [(3, 7), (3, 7), (3, 8)]);
    assert_eq!(
        result
            .untimed_error_events()
            .iter()
            .map(|event| (event.index, event.history_event, event.received_frame))
            .collect::<Vec<_>>(),
        [(9, 4, 0), (10, 13, 1)]
    );
    let first = result.state_at(0).unwrap();
    assert_eq!(first.objects[&2].properties["base"], Value::U64(11));
    assert_eq!(first.objects[&2].properties["counter"], Value::U64(12));
    assert_eq!(first.objects[&2].properties["parent"], Value::Ref(1));
    assert_eq!(
        first.objects[&2].properties["values"],
        Value::Array(vec![Value::U64(25), Value::U64(30)].into())
    );
    assert_eq!(first.objects[&5].properties["counter"], Value::U64(42));
    assert!(!first.objects.contains_key(&4));
    assert!(!result.state_at(1).unwrap().objects.contains_key(&2));
}

#[test]
fn repeated_middle_merges_existing_and_received_errors_without_assigning_positions() {
    let source = fixture().bytes();
    let intermediate = trim(&source, 1, 3);
    let repeated = trim(&intermediate, 1, 1);
    let direct = trim(&source, 2, 2);
    let recording = Recording::from_bytes(&repeated).unwrap();
    assert_eq!(initial_errors(&recording), [(3, 7), (3, 7), (3, 8), (3, 9)]);
    assert_eq!(
        recording
            .untimed_error_events()
            .iter()
            .map(|event| (event.index, event.history_event, event.received_frame))
            .collect::<Vec<_>>(),
        [(10, 6, 0)]
    );
    assert_eq!(raw(&repeated).initial, raw(&direct).initial);
    assert_eq!(
        recording.state_at(0).unwrap(),
        Recording::from_bytes(&direct).unwrap().state_at(0).unwrap()
    );
}

#[test]
fn initial_middle_preserves_each_live_error_container_without_inventing_global_order() {
    let mut source = fixture();
    source.records.remove(4);
    source.events[7] = set(3, 2, 1, &9u32.to_le_bytes(), &[6]);
    source.events[8] = set(3, 2, 1, &9u32.to_le_bytes(), &[1]);
    source.alive.push(4);
    source.alive.sort_unstable();
    let output = trim(&source.bytes(), 1, 2);
    assert_eq!(
        initial_errors(&Recording::from_bytes(&output).unwrap()),
        [(3, 7), (3, 7), (3, 8), (4, 50)]
    );
    assert_eq!(raw(&output).initial[&4], (8, error_body(&[50])));
}

#[test]
fn initial_middle_scalar_message_uses_captured_inherited_baseline() {
    let mut fixture = fixture();
    fixture.events[10] = set(
        4,
        2,
        0,
        &[12u32.to_le_bytes(), 11u32.to_le_bytes()].concat(),
        &[9],
    );
    let output = trim(&fixture.bytes(), 1, 2);
    assert_eq!(
        raw(&output).initial[&2],
        (4, body(9, &[10, 25, 30], 1, 0xa1b2c3d4))
    );
    assert_eq!(
        raw(&output).events[2],
        set(
            4,
            2,
            0,
            &[12u32.to_le_bytes(), 11u32.to_le_bytes()].concat(),
            &[2]
        )
    );
}

#[test]
fn initial_middle_preserves_forward_and_mutual_snapshot_references() {
    let mut fixture = fixture();
    fixture.records[1].body = body(7, &[10, 20], 6, 0xa1b2c3d4);
    fixture.initial.insert(0, fixture.records.len());
    fixture.records.push(Record {
        id: 6,
        class: 4,
        position: 1536,
        body: body(77, &[], 2, 0x10203040),
        deleted: false,
    });
    fixture.ids.push(6);
    fixture.alive.push(6);
    let result = raw(&trim(&fixture.bytes(), 1, 1));
    assert_eq!(
        result.initial[&2],
        (4, body(9, &[10, 25, 30], 6, 0xa1b2c3d4))
    );
    assert_eq!(result.initial[&6], (4, body(77, &[], 2, 0x10203040)));
}

#[test]
fn initial_middle_rejects_future_references_reuse_and_wrong_saved_versions() {
    let mut source = fixture();
    source.records[1].body = body(7, &[10, 20], 5, 0xa1b2c3d4);
    fail(&source.bytes(), "outside retained object lifecycles");
    let mut source = fixture();
    source.events[1] = set(3, 2, 3, &5u32.to_le_bytes(), &[1]);
    fail(&source.bytes(), "outside retained object lifecycles");
    let mut source = fixture();
    source.events[1] = set(3, 2, 3, &4u32.to_le_bytes(), &[1]);
    source.events[10] = set(3, 2, 1, &12u32.to_le_bytes(), &[10]);
    source.events[18] = set(3, 2, 3, &0u32.to_le_bytes(), &[17]);
    fail(&source.bytes(), "outside retained object lifecycles");
    let mut source = fixture();
    source.events[13] = create(2, 4);
    fail(&source.bytes(), "");
    let mut source = fixture();
    source.events[19] = saved(2, 256);
    fail(&source.bytes(), "saved");
    let mut source = fixture();
    source.records[1].deleted = true;
    source.records[5].deleted = false;
    fail(&source.bytes(), "saved");
}

#[test]
fn initial_middle_rejects_overlapping_or_ambiguous_fixed_layouts() {
    for mutation in 0..5 {
        let mut source = fixture();
        match mutation {
            0 => source.metadata.properties[1].as_mut().unwrap().offset32 = 0,
            1 => source.metadata.properties[1].as_mut().unwrap().owner.name = 5,
            2 => source.records[1].body[0..4].copy_from_slice(&32u32.to_le_bytes()),
            3 => source.records[1].body.push(9),
            _ => source.metadata.classes[1].as_mut().unwrap().dynamic = true,
        }
        fail(&source.bytes(), "");
    }
}

#[test]
fn initial_middle_rejects_error_replacement_and_unknown_analysis() {
    let mut source = fixture();
    source.events[12] = set(3, 3, 9, &error_record(9), &[12]);
    fail(&source.bytes(), "error array replacement");
    let mut source = fixture();
    source.metadata.strings.insert(6, "PxProfileZone".into());
    fail(&source.bytes(), "analysis");
    let mut source = fixture();
    source
        .metadata
        .strings
        .insert(6, "MemoryEventTotals".into());
    fail(&source.bytes(), "analysis");
}

fn rendered_fixture() -> Fixture {
    let mut source = fixture();
    source.add_renderer();
    source.ids.retain(|id| *id != 5);
    source.alive.retain(|id| *id != 5);
    source.events[13] = set(3, 6, 10, &point(true), &[13]);
    source.events[14] = set(3, 6, 10, &point(false), &[1]);
    source
}

#[test]
fn initial_middle_keeps_empty_renderer_with_explicit_retained_context() {
    let source = rendered_fixture();
    let result = trim(&source.bytes(), 1, 2);
    assert_eq!(raw(&result).initial[&6], (10, renderer_body(&[])));
    let recording = Recording::from_bytes(&result).unwrap();
    let state = recording.state_at(0).unwrap();
    assert_eq!(state.debug_draw[&6].len(), 2);
    for point in state.debug_draw[&6].iter() {
        assert!(matches!(
            point,
            crate::debug_draw::Primitive::Point {
                instance: 1,
                position: [1.0, 2.0, 3.0],
                color: 0xff123456
            }
        ));
    }
    assert!(recording.state_at(1).unwrap().debug_draw.is_empty());
}

#[test]
fn initial_middle_refuses_old_renderer_buffer_missing_context_and_interframe_drawing() {
    let mut source = rendered_fixture();
    source.records.last_mut().unwrap().body = renderer_body(&point(true));
    fail(&source.bytes(), "empty captured buffer");
    let mut source = rendered_fixture();
    source.events[13] = set(3, 6, 10, &point(false), &[13]);
    fail(&source.bytes(), "explicit instance");
    let mut source = rendered_fixture();
    source.events[6] = set(3, 6, 10, &point(true), &[6]);
    source.events[12] = array(5, 3, 9, 2, &error_record(9), &[]);
    source.events[21] = array(5, 3, 9, 3, &error_record(10), &[]);
    source.events[23] = array(5, 3, 9, 4, &error_record(11), &[]);
    source.events[13] = set(3, 6, 10, &point(true), &[7]);
    fail(&source.bytes(), "interframe drawing");
}

#[test]
fn initial_middle_proof_checks_raw_padding_reserved_and_deleted_bodies() {
    let source = fixture().bytes();
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    let proof = layout.trim_middle(1, 2, &mut || true).unwrap();
    let encoded = layout.encode(false, &mut || true).unwrap();
    proof
        .verify_layout(&Layout::parse(&encoded, &mut || true).unwrap(), &mut || {
            true
        })
        .unwrap();
    for field in 0..4 {
        let mut actual = Layout::parse(&encoded, &mut || true).unwrap();
        match field {
            0 => {
                let state = initial::State::read(&actual, &mut || true).unwrap();
                let index = state
                    .entries
                    .iter()
                    .find(|entry| entry.id == 2)
                    .unwrap()
                    .record;
                actual.saved[index].body.to_mut()[28] ^= 1;
            }
            1 => actual.snapshots[0].reserved ^= 1,
            2 => {
                actual
                    .saved
                    .iter_mut()
                    .find(|record| record.deleted)
                    .unwrap()
                    .body
                    .to_mut()[28] ^= 1
            }
            _ => {
                actual
                    .saved
                    .iter_mut()
                    .find(|record| record.deleted)
                    .unwrap()
                    .deleted = false
            }
        }
        assert!(proof.verify_layout(&actual, &mut || true).is_err());
    }
}

#[test]
fn initial_middle_proof_rejects_generated_body_changes_before_hashing() {
    let source = fixture().bytes();
    let layout = Layout::parse(&source, &mut || true).unwrap();
    let scope = Scope::prepare(&layout, 1, 2, &mut || true)
        .unwrap()
        .unwrap();
    let mut allocated = 0;
    let mut state = scope
        .restore(&layout, &mut allocated, &mut || true)
        .unwrap();
    for event in &layout.events[..9] {
        raw_apply(
            &layout.metadata,
            &mut state,
            event,
            &mut allocated,
            &mut || true,
        )
        .unwrap();
    }
    let initial = state
        .iter()
        .map(|(&id, instance)| Saved {
            id,
            position: 0,
            class: instance.class,
            body: scope
                .body(&layout, id, instance, &mut || true)
                .unwrap()
                .into(),
            deleted: false,
        })
        .collect::<Vec<_>>();
    scope
        .prove(
            &layout,
            9..23,
            &state,
            &initial,
            ProofContext {
                index_bytes: 12,
                bootstrap: &[],
            },
            &mut || true,
        )
        .unwrap();
    for offset in [8, 12, 28, 36] {
        let mut changed = initial.clone();
        changed
            .iter_mut()
            .find(|record| record.id == 2)
            .unwrap()
            .body
            .to_mut()[offset] ^= 1;
        assert!(
            scope
                .prove(
                    &layout,
                    9..23,
                    &state,
                    &changed,
                    ProofContext {
                        index_bytes: 12,
                        bootstrap: &[]
                    },
                    &mut || true
                )
                .unwrap_err()
                .to_string()
                .contains("captured raw state")
        );
    }
}

#[test]
fn initial_middle_rejects_unclaimed_class_reference_and_string_slots() {
    for target in [4, 5, 999] {
        let mut source = fixture();
        source.metadata.classes[4].as_mut().unwrap().has_pointers = true;
        source.records[1].body[28..32].copy_from_slice(&u32::to_le_bytes(target));
        fail(
            &source.bytes(),
            "class pointer is outside known scalar fields",
        );
    }
    let mut source = fixture();
    source.metadata.classes[4].as_mut().unwrap().string_offsets = vec![24];
    source.records[1].body[28..32].copy_from_slice(&27u32.to_le_bytes());
    fail(
        &source.bytes(),
        "class pointer is outside known scalar fields",
    );
}

#[test]
fn initial_middle_preserves_captured_scalar_float_bits_before_first_frame() {
    let mut source = fixture();
    source.metadata.strings.insert(28, "PvdF32".into());
    source.metadata.classes.push(Some(crate::native::Class {
        name: pvd_protocol::Name {
            namespace: 1,
            name: 28,
        },
        base: u32::MAX,
        packing: crate::native::ClassPacking::default(),
        size: 4,
        properties: vec![],
        dynamic: false,
        has_pointers: false,
        string_offsets: vec![],
    }));
    source.metadata.messages.clear();
    let property = source.metadata.properties[0].as_mut().unwrap();
    property.datatype.name = 28;
    property.datatype_id = 11;
    for index in [1, 5] {
        source.records[index].body[4..8].copy_from_slice(&0x80000000u32.to_le_bytes());
    }
    let result = raw(&trim(&source.bytes(), 1, 2));
    assert_eq!(&result.initial[&2].1[4..8], &0x80000000u32.to_le_bytes());
    assert_eq!(&result.deleted[&2].2[4..8], &0x80000000u32.to_le_bytes());
}

#[test]
fn initial_middle_restore_uses_combined_budget_and_can_cancel_before_copying() {
    let source = fixture().bytes();
    let layout = Layout::parse(&source, &mut || true).unwrap();
    let scope = Scope::prepare(&layout, 1, 2, &mut || true)
        .unwrap()
        .unwrap();
    let mut allocated = 0;
    let state = scope
        .restore(&layout, &mut allocated, &mut || true)
        .unwrap();
    assert_eq!(allocated, 1168);
    assert_eq!(
        state.values().map(RawInstance::bytes).sum::<usize>(),
        allocated
    );
    let mut exact = (64 << 20) - allocated;
    scope.restore(&layout, &mut exact, &mut || true).unwrap();
    assert_eq!(exact, 64 << 20);
    let mut over = (64 << 20) - allocated + 1;
    assert!(
        scope
            .restore(&layout, &mut over, &mut || true)
            .unwrap_err()
            .to_string()
            .contains("budget")
    );
    let mut steps = 0;
    assert!(
        scope
            .restore(&layout, &mut 0, &mut || {
                steps += 1;
                steps < 8
            })
            .is_err()
    );
    let mut output = Vec::new();
    let mut calls = 0;
    assert!(
        trim_scene_frames(&source, 1, 2, &mut output, || {
            calls += 1;
            calls < 300
        })
        .is_err()
    );
    assert!(output.is_empty());
}

#[test]
fn initial_middle_long_copy_and_hash_check_cancellation_between_chunks() {
    let source = vec![3; CHUNK * 3];
    let mut bytes = Vec::new();
    let mut calls = 0;
    assert!(
        append(&mut bytes, &source, &mut || {
            calls += 1;
            calls < 3
        })
        .is_err()
    );
    assert_eq!(bytes.len(), CHUNK * 2);
    let mut calls = 0;
    assert!(
        digest(&source, &mut Work { remaining: 1 << 30 }, &mut || {
            calls += 1;
            calls < 3
        })
        .is_err()
    );
    let mut expected = blake3::Hasher::new();
    expected.update(&(source.len() as u64).to_le_bytes());
    expected.update(&source);
    let mut exact = Work {
        remaining: source.len() + 8,
    };
    assert_eq!(
        digest(&source, &mut exact, &mut || true).unwrap(),
        expected.finalize()
    );
    assert_eq!(exact.remaining, 0);
    assert!(
        digest(
            &source,
            &mut Work {
                remaining: source.len() + 7
            },
            &mut || true
        )
        .unwrap_err()
        .to_string()
        .contains("work budget")
    );
}

#[test]
fn initial_middle_shared_indexes_and_raw_growth_are_checked_before_allocation() {
    let source = fixture().bytes();
    let layout = Layout::parse(&source, &mut || true).unwrap();
    let initial = initial::State::read(&layout, &mut || true).unwrap();
    let retained_indexes = 4 * 96 + 96 + 3 * std::mem::size_of::<u32>();
    history::prepare_with_initial(
        &layout,
        1,
        2,
        &initial.classes,
        (64 << 20) - retained_indexes,
        &mut || true,
    )
    .unwrap();
    assert!(
        history::prepare_with_initial(
            &layout,
            1,
            2,
            &initial.classes,
            (64 << 20) - retained_indexes + 1,
            &mut || true
        )
        .unwrap_err()
        .to_string()
        .contains("budget")
    );
    let mut state = RawState::new();
    let mut allocated = (64 << 20) - 127;
    assert!(
        raw_apply(
            &layout.metadata,
            &mut state,
            &HistoryEvent::Create(99, 0),
            &mut allocated,
            &mut || true
        )
        .is_err()
    );
    assert!(state.is_empty());
    let scope = Scope::prepare(&layout, 1, 2, &mut || true)
        .unwrap()
        .unwrap();
    let mut state = scope.restore(&layout, &mut 0, &mut || true).unwrap();
    let previous = state[&2].values[&2].clone();
    let capacity = state[&2].values[&2].capacity();
    let data = 42u32.to_le_bytes();
    let event = HistoryEvent::Array {
        kind: 5,
        id: 2,
        property: 2,
        index: 0,
        data: Some(&data),
        undo: vec![],
    };
    assert!(
        raw_apply(
            &layout.metadata,
            &mut state,
            &event,
            &mut ((64 << 20) - 3),
            &mut || true
        )
        .is_err()
    );
    assert_eq!(state[&2].values[&2], previous);
    assert_eq!(state[&2].values[&2].capacity(), capacity);
    let mut renderer = RawInstance::create(&layout.metadata, 10).unwrap();
    let bytes = vec![0; CHUNK * 3];
    renderer.set(&layout.metadata, 10, &bytes).unwrap();
    let mut allocated = renderer.bytes();
    let mut calls = 0;
    assert!(
        renderer
            .set_bounded(&layout.metadata, 10, &bytes, &mut allocated, &mut || {
                calls += 1;
                calls < 3
            })
            .is_err()
    );
    assert_eq!(renderer.values[&10].len(), CHUNK);
}

#[test]
#[ignore = "retains independent native initial-middle fixtures in PVD_NATIVE_INITIAL_MIDDLE_FIXTURE_DIRECTORY"]
fn retain_initial_middle_fixtures() {
    let directory = std::path::PathBuf::from(
        std::env::var_os("PVD_NATIVE_INITIAL_MIDDLE_FIXTURE_DIRECTORY")
            .expect("set PVD_NATIVE_INITIAL_MIDDLE_FIXTURE_DIRECTORY"),
    );
    std::fs::create_dir_all(&directory).unwrap();
    for (label, source) in [("ordinary", fixture()), ("renderer", rendered_fixture())] {
        let bytes = source.bytes();
        let output = trim(&bytes, 1, 2);
        std::fs::write(
            directory.join(format!("initial-middle-{label}-source.pxd2")),
            bytes,
        )
        .unwrap();
        std::fs::write(
            directory.join(format!("initial-middle-{label}-trimmed.pxd2")),
            output,
        )
        .unwrap();
    }
}
