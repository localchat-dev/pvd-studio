use super::*;

mod cameras;
mod capture;

fn spans(recording: &Recording) -> Vec<(u64, u16, u64, u64, usize)> {
    recording
        .analysis
        .legacy()
        .unwrap()
        .spans
        .iter()
        .map(|span| {
            assert_eq!((span.thread, span.context), (99, 0x111));
            (
                span.zone,
                span.event,
                span.start,
                span.end,
                span.received_frame,
            )
        })
        .collect()
}

fn catalog_bytes(bytes: &[u8]) -> &[u8] {
    let word = |at| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    let begin = 16 + word(8);
    let end = begin + word(12);
    let mut reader = Reader::new(&bytes[begin..end]);
    assert_eq!(reader.u32().unwrap(), 1);
    reader.take(16).unwrap();
    let count = reader.u32().unwrap() as usize;
    reader.take(count * 26).unwrap();
    let count = reader.u32().unwrap() as usize;
    reader.take(count * 12).unwrap();
    assert_eq!(reader.u32().unwrap(), 0);
    assert_eq!(reader.u32().unwrap(), 1);
    reader.take(12).unwrap();
    let count = reader.u32().unwrap() as usize;
    reader.take(count * 32).unwrap();
    &bytes[begin + reader.pos..end]
}

fn expected_seeds() -> [history::Seed; 2] {
    [
        history::Seed {
            id: 7,
            property: 11,
            bytes: capture::LEFT_18_30.to_vec(),
        },
        history::Seed {
            id: 10,
            property: 11,
            bytes: capture::RIGHT_19_35.to_vec(),
        },
    ]
}

#[test]
fn multizone_prefix_preserves_raw_starts_catalogs_saved_and_received_positions() {
    let fixture = capture::fixture();
    let source = fixture.bytes_with_catalogs(&capture::catalogs());
    let first = trim(&source, 0, 0);
    let output = raw(&first);
    assert_eq!(output.initial, raw(&source).initial);
    assert_eq!(output.rows, [[100, 3, 101, 11]]);
    assert_eq!(output.events, fixture.events[..12]);
    assert_eq!(
        output.events[1],
        set(3, 10, 11, &capture::RIGHT_17_12, &[1])
    );
    assert_eq!(output.events[2], set(3, 7, 11, &capture::LEFT_17_10, &[2]));
    assert_eq!(catalog_bytes(&first), capture::catalogs());
    let recording = Recording::from_bytes(&first).unwrap();
    assert_eq!(spans(&recording), [(7, 17, 10, 40, 0), (10, 17, 12, 45, 0)]);
    let analysis = recording.analysis.legacy().unwrap();
    assert_eq!(analysis.zones[&7].name, "Shared");
    assert_eq!(analysis.zones[&10].name, "Shared");
    assert_eq!(analysis.zones[&7].events[&17], "Left17");
    assert_eq!(analysis.zones[&10].events[&17], "Right17");
    assert_eq!(analysis.unmatched_stops, 0);
    assert_eq!(
        analysis
            .open_event_starts()
            .map(|(key, starts)| (*key, starts.to_vec()))
            .collect::<Vec<_>>(),
        [
            ((7, 18, 99, 0x111), vec![30]),
            ((10, 19, 99, 0x111), vec![35]),
        ]
    );

    let full = trim(&source, 0, 2);
    let output = raw(&full);
    let mut expected = fixture.events;
    expected[22] = saved(2, output.deleted[&2].0);
    assert_eq!(output.events, expected);
    assert_eq!(output.deleted[&2].2, body(7, &[10, 20], 1, 0xa1b2c3d4));
    assert_eq!(catalog_bytes(&full), capture::catalogs());
    let recording = Recording::from_bytes(&full).unwrap();
    assert_eq!(
        recording
            .untimed_error_events()
            .iter()
            .map(|e| (
                e.container_instance,
                e.index,
                e.history_event,
                e.received_frame
            ))
            .collect::<Vec<_>>(),
        [(3, 9, 20, 1), (4, 51, 21, 1),]
    );
    assert_eq!(trim(&full, 0, 0), first);
}

#[test]
fn multizone_middle_keeps_independent_pending_and_exact_shared_remaps() {
    let source = capture::source();
    let middle = trim(&source, 1, 2);
    let output = raw(&middle);
    assert_eq!(output.rows, [[200, 3, 201, 15], [300, 16, 301, 22]]);
    assert_eq!(
        output.events,
        capture::expected_middle(output.deleted[&2].0)
    );
    assert_eq!(output.initial[&7], (11, sample::buffer(&[])));
    assert_eq!(output.initial[&10], (11, capture::right_buffer()));
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
    assert_eq!(output.deleted[&2].2, body(7, &[10, 20], 1, 0xa1b2c3d4));
    assert_eq!(catalog_bytes(&middle), capture::catalogs());
    let recording = Recording::from_bytes(&middle).unwrap();
    assert_eq!(
        spans(&recording),
        [
            (10, 19, 35, 85, 0),
            (7, 18, 30, 80, 0),
            (7, 17, 50, 140, 1),
            (10, 17, 55, 150, 1),
        ]
    );
    let analysis = recording.analysis.legacy().unwrap();
    assert_eq!(
        (
            analysis.unmatched_stops,
            analysis.open_event_starts().count()
        ),
        (0, 0)
    );
    assert_eq!(
        (
            analysis.memory.allocations,
            analysis.memory.frees,
            analysis.memory.unmatched_frees
        ),
        (0, 1, 1)
    );
    assert_eq!(
        recording.memory_history_scope(),
        MemoryHistoryScope::AfterInitialSnapshot
    );
    let totals = recording.native_initial_memory_totals().unwrap().unwrap();
    assert_eq!((totals.len(), totals.row(0).unwrap().bytes), (1, 72));
    assert!(totals.row(1).is_none());
    assert_eq!(
        recording
            .untimed_error_events()
            .iter()
            .map(|e| (
                e.container_instance,
                e.index,
                e.history_event,
                e.received_frame
            ))
            .collect::<Vec<_>>(),
        [(3, 9, 11, 0), (4, 51, 12, 0),]
    );
    for (frame, x, color) in [(0, 20., 0xff333333), (1, 30., 0xff444444)] {
        let state = recording.state_at(frame).unwrap();
        assert_eq!(state.debug_draw[&6].len(), 1);
        let crate::debug_draw::Primitive::Point {
            instance,
            position,
            color: actual,
        } = &state.debug_draw[&6][0]
        else {
            panic!("expected captured point")
        };
        assert_eq!((*instance, *position, *actual), (1, [x, 2., 3.], color));
    }
}

#[test]
fn multizone_repeated_prefix_and_middle_match_direct_raw_and_analysis_truth() {
    let source = capture::source();
    let prefix = trim(&source, 0, 2);
    let middle = trim(&source, 1, 2);
    assert_eq!(trim(&prefix, 1, 2), middle);
    assert_eq!(trim(&middle, 0, 0), trim(&source, 1, 1));
    let last = trim(&source, 2, 2);
    assert_eq!(trim(&middle, 1, 1), last);
    assert_eq!(trim(&last, 0, 0), last);
    let output = raw(&last);
    assert_eq!(output.rows, [[300, 3, 301, 9]]);
    assert_eq!(output.events, capture::expected_last());
    assert!(!output.initial.contains_key(&2));
    assert!(output.deleted.is_empty());
    assert_eq!(
        output.initial[&6],
        (10, sample::buffer(&sample::drawing(1, 20., 0xff333333)))
    );
    assert_eq!(output.initial[&8], (12, sample::buffer(&[3, 0, 9])));
    assert_eq!(
        output.initial[&9],
        (14, sample::buffer(&64i32.to_le_bytes()))
    );
    let recording = Recording::from_bytes(&last).unwrap();
    assert_eq!(
        spans(&recording),
        [(7, 17, 50, 140, 0), (10, 17, 55, 150, 0)]
    );
    let mut errors = initial_errors(&recording);
    errors.sort_unstable();
    assert_eq!(errors, [(3, 7), (3, 7), (3, 9), (4, 50), (4, 51)]);
    assert!(recording.untimed_error_events().is_empty());
}

#[test]
fn multizone_stops_cannot_borrow_another_zone_start_or_seed() {
    let mut fixture = capture::fixture();
    fixture.events[17] = set(3, 7, 11, &sample::profile(2, 18, 80), &[8]);
    let bytes = trim(&fixture.bytes_with_catalogs(&capture::catalogs()), 2, 2);
    let output = raw(&bytes);
    assert_eq!(output.rows, [[300, 2, 301, 8]]);
    assert_eq!(
        output.events[1],
        set(3, 10, 11, &capture::RIGHT_17_55, &[1])
    );
    assert_eq!(
        output.events[4],
        set(3, 7, 11, &sample::profile(2, 17, 140), &[4])
    );
    assert_eq!(
        output.events[5],
        set(3, 10, 11, &sample::profile(2, 17, 150), &[4])
    );
    let recording = Recording::from_bytes(&bytes).unwrap();
    assert_eq!(spans(&recording), [(10, 17, 55, 150, 0)]);
    assert_eq!(recording.analysis.legacy().unwrap().unmatched_stops, 1);
}

#[test]
fn multizone_pending_start_requires_its_own_zone_event_directory() {
    let mut fixture = capture::fixture();
    fixture.events[8] = set(
        3,
        10,
        11,
        &[sample::profile(1, 18, 35), sample::profile(2, 17, 45)].concat(),
        &[7],
    );
    let source = fixture.bytes_with_catalogs(&capture::catalogs());
    let layout = Layout::parse(&source, &mut || true).unwrap();
    let initial = initial::State::read(&layout, &mut || true).unwrap();
    let failure = initial::analysis::Scope::read(&initial, &layout, 1, 2, &mut 0, &mut || true)
        .err()
        .unwrap();
    assert!(
        failure.to_string().contains("known absolute starts"),
        "{failure}"
    );
    let mut output = Vec::new();
    assert!(trim_scene_frames(&source, 1, 2, &mut output, || true).is_err());
    assert!(output.is_empty());
}

fn scope_refuses(mut change: impl FnMut(&mut Layout<'_>), expected: &str) {
    let source = capture::source();
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    change(&mut layout);
    let initial = initial::State::read(&layout, &mut || true).unwrap();
    for middle in [false, true] {
        let result = if middle {
            initial::analysis::Scope::read(&initial, &layout, 1, 1, &mut 0, &mut || true)
        } else {
            initial::analysis::Scope::read_prefix(&initial, &layout, 12, &mut 0, &mut || true)
        };
        let failure = result
            .err()
            .expect("expected rejected initial analysis scope");
        assert!(failure.to_string().contains(expected), "{failure}");
    }
}

#[test]
fn multizone_scope_binds_each_leading_start_and_zone_directory() {
    scope_refuses(
        |layout| {
            let HistoryEvent::Set { data, .. } = &mut layout.events[1] else {
                unreachable!()
            };
            *data = capture::LEFT_18_30.to_vec().into();
        },
        "known absolute starts",
    );
    scope_refuses(
        |layout| {
            layout.events[2] = HistoryEvent::Marker;
            let HistoryEvent::Set { undo, .. } = &mut layout.events[7] else {
                unreachable!()
            };
            *undo = vec![7];
        },
        "captured starts",
    );
    scope_refuses(
        |layout| {
            let HistoryEvent::Set { id, undo, .. } = &mut layout.events[2] else {
                unreachable!()
            };
            *id = 10;
            *undo = vec![1];
        },
        "contiguous captured starts",
    );
    scope_refuses(
        |layout| {
            let HistoryEvent::Set { undo, .. } = &mut layout.events[2] else {
                unreachable!()
            };
            *undo = vec![1];
        },
        "another operation",
    );
    scope_refuses(
        |layout| {
            let HistoryEvent::Set { data, .. } = &mut layout.events[1] else {
                unreachable!()
            };
            data.to_mut()[0] = 2;
        },
        "known absolute starts",
    );
    scope_refuses(
        |layout| {
            layout.events.swap(1, 4);
            let HistoryEvent::Set { undo, .. } = &mut layout.events[1] else {
                unreachable!()
            };
            *undo = vec![1];
        },
        "contiguous captured starts",
    );
    scope_refuses(
        |layout| layout.communication.zones[0].source ^= 1,
        "source differs",
    );
    scope_refuses(
        |layout| layout.communication.zones[0].id = 19,
        "no unique captured source",
    );
    scope_refuses(
        |layout| layout.communication.zones[0].events[0].1 = 17,
        "event identities are ambiguous",
    );
    scope_refuses(
        |layout| layout.communication.zones[0].id = 7,
        "zone identities are ambiguous",
    );
    scope_refuses(
        |layout| {
            let saved = layout
                .saved
                .iter_mut()
                .find(|saved| saved.id == 10)
                .unwrap();
            saved.body = sample::buffer(&capture::RIGHT_17_12).into();
        },
        "empty live captured property",
    );
}

#[test]
fn multizone_scope_rejects_distinct_profile_classes_with_alias_names() {
    scope_refuses(
        |layout| {
            let class_id = layout.metadata.classes.len() as u32;
            let property_id = layout.metadata.properties.len() as u32;
            let namespace = layout.metadata.strings.keys().copied().max().unwrap() + 1;
            let name = namespace + 1;
            let alias = pvd_protocol::Name { namespace, name };
            let captured = layout.metadata.class(11).unwrap();
            let class = crate::native::Class {
                name: alias,
                base: captured.base,
                packing: captured.packing,
                size: captured.size,
                properties: vec![property_id],
                dynamic: captured.dynamic,
                has_pointers: captured.has_pointers,
                string_offsets: captured.string_offsets.clone(),
            };
            let captured = layout.metadata.property(11).unwrap();
            let property = crate::native::Property {
                owner: alias,
                name: captured.name,
                datatype: captured.datatype,
                datatype_id: captured.datatype_id,
                kind: captured.kind,
                semantic: captured.semantic,
                offset32: captured.offset32,
                values: captured.values.clone(),
            };
            layout
                .metadata
                .strings
                .insert(namespace, "_debugger_".into());
            layout.metadata.strings.insert(name, "PxProfileZone".into());
            layout.metadata.classes.push(Some(class));
            layout.metadata.properties.push(Some(property));
            layout.pointer_layouts.push(vec![]);
            layout
                .saved
                .iter_mut()
                .find(|saved| saved.id == 10)
                .unwrap()
                .class = class_id;
            for event in &mut layout.events {
                if let HistoryEvent::Set { id, property, .. } = event
                    && *id == 10
                {
                    *property = property_id;
                }
            }
        },
        "one captured class",
    );
}

#[test]
fn multizone_scope_checks_lifetimes_and_operations_beyond_the_selected_prefix() {
    for id in [7, 10] {
        for operation in 0..4 {
            scope_refuses(
                |layout| {
                    layout.events[30] = match operation {
                        0 => HistoryEvent::Create(id, 11),
                        1 => HistoryEvent::Destroy(id),
                        2 => HistoryEvent::Saved {
                            id,
                            position: layout.saved.iter().find(|s| s.id == id).unwrap().position,
                        },
                        _ => HistoryEvent::Set {
                            kind: 4,
                            id,
                            property: 0,
                            data: vec![0; 8].into(),
                            undo: vec![1],
                        },
                    };
                },
                if operation == 0 {
                    "stable captured lifetimes"
                } else if operation == 3 {
                    "complete captured buffers"
                } else {
                    "stable complete properties"
                },
            );
        }
    }
    scope_refuses(
        |layout| layout.events[30] = HistoryEvent::Create(19, 11),
        "stable captured lifetimes",
    );
}

#[test]
fn multizone_independent_proof_rejects_seed_count_order_identity_and_cross_zone_undo() {
    let source = capture::source();
    let layout = Layout::parse(&source, &mut || true).unwrap();
    let initial = initial::State::read(&layout, &mut || true).unwrap();
    let scope = initial::analysis::Scope::read(&initial, &layout, 1, 2, &mut 0, &mut || true)
        .unwrap()
        .unwrap();
    let seeds = expected_seeds();
    let proof = scope.prove(&seeds, &mut 0, &mut || true).unwrap();
    let bytes = trim(&source, 1, 2);
    let output = Layout::parse(&bytes, &mut || true).unwrap();
    proof.verify_layout(&output, &mut || true).unwrap();
    for variant in 0..5 {
        let mut seeds = Vec::from(expected_seeds());
        match variant {
            0 => {
                seeds.pop();
            }
            1 => seeds.swap(0, 1),
            2 => seeds[1].id = 7,
            3 => seeds[1].bytes = capture::LEFT_18_30.to_vec(),
            _ => seeds.push(history::Seed {
                id: 10,
                property: 11,
                bytes: capture::RIGHT_19_35.to_vec(),
            }),
        }
        assert!(
            scope.prove(&seeds, &mut 0, &mut || true).is_err(),
            "seed variant {variant}"
        );
    }
    for variant in 0..12 {
        let mut changed = Layout::parse(&bytes, &mut || true).unwrap();
        match variant {
            0 => {
                changed.events.swap(1, 2);
                for ordinal in 1..=2 {
                    let HistoryEvent::Set { undo, .. } = &mut changed.events[ordinal] else {
                        unreachable!()
                    };
                    *undo = vec![ordinal as u32];
                }
            }
            1 => {
                changed.events[2] = changed.events[1].clone();
                let HistoryEvent::Set { undo, .. } = &mut changed.events[2] else {
                    unreachable!()
                };
                *undo = vec![2];
            }
            2 => changed.events.insert(2, changed.events[1].clone()),
            3 => {
                let HistoryEvent::Set { id, .. } = &mut changed.events[2] else {
                    unreachable!()
                };
                *id = 7;
            }
            4 => {
                let HistoryEvent::Set { undo, .. } = &mut changed.events[7] else {
                    unreachable!()
                };
                *undo = vec![6];
            }
            5 => changed.communication.zones.swap(0, 1),
            6 => changed.communication.zones[0].saved_word ^= 1,
            7 => changed.communication.zones[0].events.reverse(),
            8 => {
                changed
                    .saved
                    .iter_mut()
                    .find(|s| s.id == 10)
                    .unwrap()
                    .body
                    .to_mut()[4] ^= 1
            }
            9 => {
                changed.events[11] = HistoryEvent::Set {
                    kind: 4,
                    id: 10,
                    property: 0,
                    data: vec![0; 8].into(),
                    undo: vec![11],
                }
            }
            10 => changed.events[11] = HistoryEvent::Destroy(10),
            _ => changed.communication.zones[0].source ^= 1,
        }
        let failure = proof.verify_layout(&changed, &mut || true).unwrap_err();
        if variant <= 1 {
            assert_eq!(
                failure.to_string(),
                "initial analysis changed captured pending starts"
            );
        }
    }
}

#[test]
fn multizone_prefix_raw_proof_binds_each_seed_and_saved_position() {
    let source = capture::source();
    let layout = Layout::parse(&source, &mut || true).unwrap();
    let proof = initial::Proof::prepare(&layout, 32, &mut || true).unwrap();
    let bytes = trim(&source, 0, 2);
    proof
        .verify_layout(&Layout::parse(&bytes, &mut || true).unwrap(), &mut || true)
        .unwrap();
    for variant in 0..4 {
        let mut changed = Layout::parse(&bytes, &mut || true).unwrap();
        match variant {
            0 => changed.events.swap(1, 2),
            1 => changed.communication.zones[0].events[0].2 ^= 1,
            2 => changed.events.swap(21, 22),
            _ => {
                changed
                    .saved
                    .iter_mut()
                    .find(|s| s.id == 10)
                    .unwrap()
                    .body
                    .to_mut()[4] ^= 1
            }
        }
        assert!(
            proof.verify_layout(&changed, &mut || true).is_err(),
            "prefix variant {variant}"
        );
    }
}

#[test]
fn multizone_scope_and_proof_share_budget_and_cancel_before_publication() {
    let source = capture::source();
    let layout = Layout::parse(&source, &mut || true).unwrap();
    let initial = initial::State::read(&layout, &mut || true).unwrap();
    for middle in [false, true] {
        let read = |allocated: &mut usize, proceed: &mut dyn FnMut() -> bool| {
            if middle {
                initial::analysis::Scope::read(&initial, &layout, 1, 2, allocated, proceed)
            } else {
                initial::analysis::Scope::read_prefix(&initial, &layout, 32, allocated, proceed)
            }
        };
        let mut bytes = 0;
        let mut calls = 0;
        read(&mut bytes, &mut || {
            calls += 1;
            true
        })
        .unwrap();
        let mut exact = MAX_METADATA - bytes;
        read(&mut exact, &mut || true).unwrap();
        assert_eq!(exact, MAX_METADATA);
        assert!(read(&mut (MAX_METADATA - bytes + 1), &mut || true).is_err());
        let mut remaining = calls - 1;
        let failure = read(&mut 0, &mut || {
            remaining -= 1;
            remaining != 0
        })
        .err()
        .unwrap();
        assert_eq!(
            failure.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
    }
    let scope = initial::analysis::Scope::read(&initial, &layout, 1, 2, &mut 0, &mut || true)
        .unwrap()
        .unwrap();
    let mut bytes = 0;
    scope
        .prove(&expected_seeds(), &mut bytes, &mut || true)
        .unwrap();
    let mut exact = MAX_METADATA - bytes;
    scope
        .prove(&expected_seeds(), &mut exact, &mut || true)
        .unwrap();
    assert_eq!(exact, MAX_METADATA);
    assert!(
        scope
            .prove(
                &expected_seeds(),
                &mut (MAX_METADATA - bytes + 1),
                &mut || true
            )
            .is_err()
    );
    for first in [0, 1] {
        let mut calls = 0;
        let mut output = Vec::new();
        trim_scene_frames(&source, first, 2, &mut output, || {
            calls += 1;
            true
        })
        .unwrap();
        assert!(output.len() < 64 << 10);
        output.clear();
        let mut remaining = calls - 1;
        assert!(
            trim_scene_frames(&source, first, 2, &mut output, || {
                remaining -= 1;
                remaining != 0
            })
            .is_err()
        );
        assert!(output.is_empty());
    }
}

#[test]
#[ignore = "retains constructed inputs in PVD_NATIVE_MULTIZONE_INITIAL_DIRECTORY"]
fn retain_multizone_initial_analysis_fixtures() {
    let directory = std::path::PathBuf::from(
        std::env::var_os("PVD_NATIVE_MULTIZONE_INITIAL_DIRECTORY")
            .expect("set PVD_NATIVE_MULTIZONE_INITIAL_DIRECTORY"),
    );
    std::fs::create_dir_all(&directory).unwrap();
    let source = capture::source();
    let middle = trim(&source, 1, 2);
    let repeated = trim(&middle, 1, 1);
    assert_eq!(repeated, trim(&source, 2, 2));
    for (name, bytes) in [
        ("source", source.clone()),
        ("prefix", trim(&source, 0, 1)),
        ("middle", middle.clone()),
        ("middle-prefix", trim(&middle, 0, 0)),
        ("repeated-middle", repeated),
    ] {
        std::fs::write(
            directory.join(format!("multizone-initial-{name}.pxd2")),
            bytes,
        )
        .unwrap();
    }
}
