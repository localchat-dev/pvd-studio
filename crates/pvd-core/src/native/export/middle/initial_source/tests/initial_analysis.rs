use super::*;
use crate::MemoryHistoryScope;

mod multizone;
mod prefix;
mod sample;

#[test]
fn initial_analysis_middle_keeps_captured_pending_memory_drawing_and_saved_ordinals() {
    let source = sample::source();
    let bytes = trim(&source, 1, 1);
    let output = raw(&bytes);
    assert_eq!(output.events.len(), 13);
    assert_eq!(output.rows, [[200, 2, 201, 12]]);
    assert_eq!(output.initial[&7], (11, sample::buffer(&[])));
    assert_eq!(
        output.initial[&6],
        (10, sample::buffer(&sample::drawing(2, 10., 0xff222222)))
    );
    assert_eq!(
        output.initial[&8],
        (12, sample::buffer(&[2, 0, 8, 37, 38, 7, 9]))
    );
    assert_eq!(
        output.initial[&9],
        (14, sample::buffer(&72i32.to_le_bytes()))
    );
    assert_eq!(
        output.events[1],
        set(3, 7, 11, &sample::profile(1, 18, 30), &[1])
    );
    assert_eq!(
        output.events[3],
        set(3, 6, 10, &sample::drawing(1, 20., 0xff333333), &[3])
    );
    assert_eq!(
        output.events[6],
        set(3, 7, 11, &sample::profile(2, 18, 80), &[5])
    );
    assert_eq!(output.events[8], array(5, 3, 9, 2, &error_record(9), &[]));
    assert_eq!(output.events[9], array(5, 4, 9, 1, &error_record(51), &[]));
    assert_eq!(
        &output.events[10][..8],
        [10u32.to_le_bytes(), 2u32.to_le_bytes()].concat()
    );
    assert_eq!(output.events[11], destroy(2));
    assert_eq!(output.deleted[&2].2, body(7, &[10, 20], 1, 0xa1b2c3d4));
    let recording = Recording::from_bytes(&bytes).unwrap();
    let analysis = recording.analysis.legacy().unwrap();
    assert_eq!(analysis.spans.len(), 1);
    let span = &analysis.spans[0];
    assert_eq!(
        (
            span.zone,
            span.event,
            span.thread,
            span.context,
            span.start,
            span.end,
            span.received_frame
        ),
        (7, 18, 99, 0x111, 30, 80, 0)
    );
    assert_eq!(analysis.unmatched_stops, 0);
    assert_eq!(analysis.open_event_starts().count(), 0);
    assert_eq!(
        recording.memory_history_scope(),
        MemoryHistoryScope::AfterInitialSnapshot
    );
    assert_eq!(
        (
            analysis.memory.allocations,
            analysis.memory.frees,
            analysis.memory.unmatched_frees
        ),
        (0, 1, 1)
    );
    let totals = recording.native_initial_memory_totals().unwrap().unwrap();
    assert_eq!((totals.len(), totals.row(0).unwrap().bytes), (1, 72));
    assert!(totals.row(1).is_none());
    assert_eq!(
        recording
            .untimed_error_events()
            .iter()
            .map(|error| (
                error.container_instance,
                error.index,
                error.history_event,
                error.received_frame
            ))
            .collect::<Vec<_>>(),
        [(3, 9, 8, 0), (4, 51, 9, 0)]
    );
    let state = recording.state_at(0).unwrap();
    assert_eq!(state.debug_draw.len(), 1);
    assert_eq!(state.debug_draw[&6].len(), 1);
    let crate::debug_draw::Primitive::Point {
        instance,
        position,
        color,
    } = &state.debug_draw[&6][0]
    else {
        panic!("expected captured point")
    };
    assert_eq!(
        (*instance, *position, *color),
        (1, [20., 2., 3.], 0xff333333)
    );
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

#[test]
fn initial_analysis_prefix_rejects_unknown_leading_history() {
    let mut fixture = sample::fixture_with_analysis();
    fixture.events[1] = set(3, 7, 11, &sample::profile(2, 17, 10), &[1]);
    let mut output = Vec::new();
    assert!(
        trim_scene_frames(
            &fixture.bytes_with_catalogs(&sample::catalogs()),
            0,
            0,
            &mut output,
            || true
        )
        .is_err()
    );
    assert!(output.is_empty());
}

#[test]
fn initial_analysis_preserves_unmatched_stops_without_replaying_old_drawing() {
    let mut fixture = sample::fixture_with_analysis();
    fixture
        .records
        .iter_mut()
        .find(|record| record.id == 6)
        .unwrap()
        .body = sample::buffer(&[255]);
    fixture.events[13] = set(
        3,
        7,
        11,
        &[sample::profile(2, 17, 70), sample::profile(2, 18, 80)].concat(),
        &[6],
    );
    let bytes = trim(&fixture.bytes_with_catalogs(&sample::catalogs()), 1, 1);
    let recording = Recording::from_bytes(&bytes).unwrap();
    let analysis = recording.analysis.legacy().unwrap();
    assert_eq!((analysis.unmatched_stops, analysis.spans.len()), (1, 1));
    assert_eq!(
        (
            analysis.spans[0].event,
            analysis.spans[0].start,
            analysis.spans[0].end
        ),
        (18, 30, 80)
    );
    assert_eq!(recording.state_at(0).unwrap().debug_draw[&6].len(), 1);
}

#[test]
fn closed_captured_starts_need_no_seed_and_keep_saved_and_error_positions() {
    let mut fixture = sample::fixture_with_analysis();
    fixture.events[6] = set(3, 7, 11, &sample::profile(2, 17, 40), &[5]);
    let bytes = trim(&fixture.bytes_with_catalogs(&sample::catalogs()), 1, 1);
    let output = raw(&bytes);
    assert_eq!(
        (output.events.len(), output.rows),
        (12, vec![[200, 1, 201, 11]])
    );
    assert_eq!(
        &output.events[9][..8],
        [10u32.to_le_bytes(), 2u32.to_le_bytes()].concat()
    );
    let recording = Recording::from_bytes(&bytes).unwrap();
    let analysis = recording.analysis.legacy().unwrap();
    assert_eq!((analysis.spans.len(), analysis.unmatched_stops), (0, 1));
    assert_eq!(
        recording
            .untimed_error_events()
            .iter()
            .map(|error| error.history_event)
            .collect::<Vec<_>>(),
        [7, 8]
    );
}

#[test]
fn initial_analysis_saved_proof_rejects_relative_ordinal_changes_after_the_seed() {
    let source = sample::source();
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    let proof = layout.trim_middle(1, 1, &mut || true).unwrap();
    let bytes = layout.encode(false, &mut || true).unwrap();
    let mut changed = Layout::parse(&bytes, &mut || true).unwrap();
    let proof = proof.initial.as_ref().unwrap();
    proof.verify_layout(&changed, &mut || true).unwrap();
    changed.events.swap(9, 10);
    assert_eq!(
        proof
            .verify_layout(&changed, &mut || true)
            .unwrap_err()
            .to_string(),
        "initial middle changed captured snapshot or saved bytes"
    );
}

fn refuses(mut change: impl FnMut(&mut Layout<'_>), expected: &str) {
    let source = sample::source();
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    change(&mut layout);
    let failure = Scope::prepare(&layout, 1, 1, &mut || true).err().unwrap();
    assert!(failure.to_string().contains(expected), "{failure}");
}

#[test]
fn initial_analysis_rejects_unknown_profile_initial_and_leading_history() {
    for variant in 0..6 {
        refuses(
            |layout| {
                if variant == 0 {
                    layout
                        .saved
                        .iter_mut()
                        .find(|saved| saved.id == 7)
                        .unwrap()
                        .body = sample::buffer(&sample::profile(1, 17, 10)).into();
                } else {
                    let HistoryEvent::Set {
                        data,
                        property,
                        undo,
                        ..
                    } = &mut layout.events[1]
                    else {
                        unreachable!()
                    };
                    match variant {
                        1 => *data = vec![].into(),
                        2 => data.to_mut()[0] = 2,
                        3 => data.to_mut()[1] = 3,
                        4 => *property = 10,
                        5 => *undo = vec![],
                        _ => unreachable!(),
                    }
                }
            },
            if variant == 4 {
                "complete captured buffers"
            } else if variant == 5 {
                "undo"
            } else {
                "profile"
            },
        );
    }
}

#[test]
fn initial_analysis_rejects_schema_catalog_lifetime_and_context_gaps() {
    refuses(
        |layout| layout.communication.zones[0].source ^= 1,
        "source differs",
    );
    refuses(
        |layout| {
            layout
                .communication
                .zones
                .push(layout.communication.zones[0].clone())
        },
        "zone identities are ambiguous",
    );
    refuses(
        |layout| layout.metadata.classes[11].as_mut().unwrap().base = 0,
        "class layout",
    );
    refuses(
        |layout| layout.metadata.properties[11].as_mut().unwrap().offset32 = 4,
        "event layout",
    );
    refuses(
        |layout| layout.events[18] = HistoryEvent::Destroy(7),
        "stable complete properties",
    );
    for ordinal in [3, 10] {
        refuses(
            |layout| {
                let HistoryEvent::Set { data, .. } = &mut layout.events[ordinal] else {
                    unreachable!()
                };
                *data = data[9..].to_vec().into();
            },
            "explicit captured instance",
        );
    }
    refuses(
        |layout| {
            layout.events[9] = HistoryEvent::Set {
                kind: 3,
                id: 6,
                property: 10,
                data: sample::drawing(1, 3., 0).into(),
                undo: vec![6],
            };
            layout.communication.sections[0].rows[1][1] = 10;
        },
        "interframe drawing",
    );
}

#[test]
fn initial_analysis_proof_independently_binds_catalog_initial_seeds_history_and_undo() {
    let source = sample::source();
    let layout = Layout::parse(&source, &mut || true).unwrap();
    let initial = initial::State::read(&layout, &mut || true).unwrap();
    let scope = initial::analysis::Scope::read(&initial, &layout, 1, 1, &mut 0, &mut || true)
        .unwrap()
        .unwrap();
    let seeds = [history::Seed {
        id: 7,
        property: 11,
        bytes: sample::profile(1, 18, 30),
    }];
    let proof = scope.prove(&seeds, &mut 0, &mut || true).unwrap();
    let bytes = trim(&source, 1, 1);
    proof
        .verify_layout(&Layout::parse(&bytes, &mut || true).unwrap(), &mut || true)
        .unwrap();
    assert!(
        scope
            .prove(
                &[history::Seed {
                    id: 7,
                    property: 11,
                    bytes: sample::profile(1, 18, 31)
                }],
                &mut 0,
                &mut || true
            )
            .is_err()
    );
    for variant in 0..12 {
        let mut changed = Layout::parse(&bytes, &mut || true).unwrap();
        match variant {
            0 => changed.communication.zones[0].saved_word ^= 1,
            1 => changed.communication.zones[0].events.reverse(),
            2 => changed.communication.zones[0].events[0].2 ^= 1,
            3 => changed.communication.zones[0].name = b"Changed\0",
            4 | 5 => {
                let saved = changed
                    .saved
                    .iter_mut()
                    .find(|saved| saved.id == 6)
                    .unwrap();
                let at = if variant == 4 {
                    4
                } else {
                    saved.body.len() - 1
                };
                saved.body.to_mut()[at] ^= 1;
            }
            6..=9 => {
                let ordinal = if variant == 6 { 1 } else { 3 };
                let HistoryEvent::Set { data, undo, .. } = &mut changed.events[ordinal] else {
                    unreachable!()
                };
                if variant == 8 {
                    *undo = vec![1];
                } else if variant == 9 {
                    changed.events.swap(3, 4);
                } else {
                    data.to_mut()[8] ^= 1;
                }
            }
            10 => {
                let HistoryEvent::Set { undo, .. } = &mut changed.events[6] else {
                    unreachable!()
                };
                *undo = vec![6];
            }
            11 => changed.communication.zones[0].source ^= 1,
            _ => unreachable!(),
        }
        assert!(
            proof.verify_layout(&changed, &mut || true).is_err(),
            "variant {variant}"
        );
    }
    for (id, class, property) in [(7, 11, 11), (6, 10, 10)] {
        let original = Layout::parse(&bytes, &mut || true).unwrap();
        let position = original
            .saved
            .iter()
            .find(|saved| saved.id == id)
            .unwrap()
            .position;
        for (operation, event) in [
            (
                "message",
                HistoryEvent::Set {
                    kind: 4,
                    id,
                    property: 0,
                    data: vec![0; 8].into(),
                    undo: vec![8],
                },
            ),
            ("create", HistoryEvent::Create(id, class)),
            ("destroy", HistoryEvent::Destroy(id)),
            ("saved", HistoryEvent::Saved { id, position }),
            (
                "array",
                HistoryEvent::Array {
                    kind: 5,
                    id,
                    property,
                    index: 0,
                    data: Some(&[0]),
                    undo: vec![],
                },
            ),
        ] {
            let mut changed = Layout::parse(&bytes, &mut || true).unwrap();
            assert!(matches!(
                changed.events[8],
                HistoryEvent::Array { id: 3, kind: 5, .. }
            ));
            changed.events[8] = event;
            assert_eq!(
                proof
                    .verify_layout(&changed, &mut || true)
                    .unwrap_err()
                    .to_string(),
                "initial analysis history introduced an unsupported buffer operation",
                "buffer {id}, operation {operation}"
            );
        }
    }
}

#[test]
#[ignore = "retains the constructed input in PVD_NATIVE_INITIAL_ANALYSIS_DIRECTORY"]
fn retain_initial_analysis_middle_fixture() {
    let directory = std::path::PathBuf::from(
        std::env::var_os("PVD_NATIVE_INITIAL_ANALYSIS_DIRECTORY")
            .expect("set PVD_NATIVE_INITIAL_ANALYSIS_DIRECTORY"),
    );
    std::fs::create_dir_all(&directory).unwrap();
    let source = sample::source();
    let output = trim(&source, 1, 1);
    std::fs::write(directory.join("initial-analysis-source.pxd2"), source).unwrap();
    std::fs::write(directory.join("initial-analysis-trimmed.pxd2"), output).unwrap();
}

#[test]
fn initial_analysis_shared_budget_and_cancellation_fail_before_output() {
    let source = sample::source();
    let layout = Layout::parse(&source, &mut || true).unwrap();
    let initial = initial::State::read(&layout, &mut || true).unwrap();
    let mut exact = 0;
    initial::analysis::Scope::read(&initial, &layout, 1, 1, &mut exact, &mut || true).unwrap();
    let mut allocation = MAX_METADATA - exact;
    initial::analysis::Scope::read(&initial, &layout, 1, 1, &mut allocation, &mut || true).unwrap();
    assert_eq!(allocation, MAX_METADATA);
    assert!(
        initial::analysis::Scope::read(
            &initial,
            &layout,
            1,
            1,
            &mut (MAX_METADATA - exact + 1),
            &mut || true
        )
        .is_err()
    );
    let mut calls = 0;
    let mut output = Vec::new();
    assert!(
        trim_scene_frames(&source, 1, 1, &mut output, || {
            calls += 1;
            calls < 40
        })
        .is_err()
    );
    assert!(output.is_empty());
}
