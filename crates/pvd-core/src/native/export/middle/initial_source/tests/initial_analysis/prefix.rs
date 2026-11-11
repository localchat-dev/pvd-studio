use super::*;

fn raw_catalogs(bytes: &[u8]) -> &[u8] {
    let word = |at| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    let start = 16 + word(8);
    let end = start + word(12);
    let mut reader = Reader::new(&bytes[start..end]);
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
    &bytes[start + reader.pos..end]
}

fn later_zone_fixture() -> (Fixture, Vec<u8>) {
    let mut fixture = sample::fixture_with_analysis();
    let first_begin = fixture.events[2].clone();
    let first_end = fixture.events[8].clone();
    let second_begin = fixture.events[9].clone();
    let second_end = fixture.events[19].clone();
    fixture.records.retain(|record| record.id < 6);
    fixture.initial = vec![2, 1, 0, 3];
    fixture.ids = vec![1, 2, 3, 4, 7];
    fixture.alive = vec![1, 3, 4, 7];
    fixture.events = vec![
        9u32.to_le_bytes().to_vec(),
        first_begin,
        create(7, 11),
        set(3, 7, 11, &sample::profile(1, 17, 10), &[1]),
        set(
            3,
            7,
            11,
            &[sample::profile(1, 18, 30), sample::profile(2, 17, 40)].concat(),
            &[1],
        ),
        set(3, 7, 11, &[], &[1]),
        first_end,
        second_begin,
        set(3, 7, 11, &sample::profile(2, 18, 80), &[3]),
        set(3, 7, 11, &[], &[1]),
        array(5, 3, 9, 2, &error_record(9), &[]),
        array(5, 4, 9, 1, &error_record(51), &[]),
        saved(2, 1280),
        destroy(2),
        second_end,
    ];
    let mut catalogs = Vec::new();
    let blob = |out: &mut Vec<u8>, bytes: &[u8]| {
        word(out, bytes.len() as u32);
        out.extend(bytes);
    };
    word(&mut catalogs, 1);
    catalogs.extend(0x777u64.to_le_bytes());
    catalogs.extend(0xfeedcafedeadbeefu64.to_le_bytes());
    blob(&mut catalogs, b"Zone\0");
    word(&mut catalogs, 7);
    word(&mut catalogs, 2);
    for (name, id, enabled) in [
        (b"Start18\0".as_slice(), 18u16, 0u8),
        (b"Start17\0".as_slice(), 17, 1),
    ] {
        blob(&mut catalogs, name);
        catalogs.extend(id.to_le_bytes());
        catalogs.push(enabled);
    }
    for value in [u32::MAX, u32::MAX, 0] {
        word(&mut catalogs, value);
    }
    (fixture, catalogs)
}

#[test]
fn initial_analysis_prefix_keeps_later_created_zone_events_and_analysis() {
    let (fixture, catalogs) = later_zone_fixture();
    let source = fixture.bytes_with_catalogs(&catalogs);
    let layout = Layout::parse(&source, &mut || true).unwrap();
    let initial = initial::State::read(&layout, &mut || true).unwrap();
    assert_eq!(
        initial.classes.keys().copied().collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
    assert!(
        initial
            .prefix_analysis(&layout, 7, &mut || true)
            .unwrap()
            .is_none()
    );
    let bytes = trim(&source, 0, 0);
    let output = raw(&bytes);
    assert_eq!(output.initial, raw(&source).initial);
    assert!(!output.initial.contains_key(&7));
    assert_eq!(output.rows, [[100, 1, 101, 6]]);
    assert_eq!(output.events, fixture.events[..7]);
    assert_eq!(output.events[2], create(7, 11));
    assert_eq!(raw_catalogs(&bytes), catalogs);
    assert!(output.deleted.is_empty());
    let recording = Recording::from_bytes(&bytes).unwrap();
    let analysis = recording.analysis.legacy().unwrap();
    assert_eq!(analysis.spans.len(), 1);
    let span = &analysis.spans[0];
    assert_eq!(
        (
            span.zone,
            span.event,
            span.start,
            span.end,
            span.received_frame
        ),
        (7, 17, 10, 40, 0)
    );
    assert_eq!(analysis.unmatched_stops, 0);
    assert_eq!(
        analysis
            .open_event_starts()
            .map(|(key, starts)| (*key, starts.to_vec()))
            .collect::<Vec<_>>(),
        [((7, 18, 99, 0x111), vec![30])]
    );
    assert!(recording.untimed_error_events().is_empty());
    let full = trim(&source, 0, 1);
    let output = raw(&full);
    let mut expected = fixture.events;
    expected[12] = saved(2, output.deleted[&2].0);
    assert_eq!(output.events, expected);
    assert_eq!(output.deleted[&2].2, body(7, &[10, 20], 1, 0xa1b2c3d4));
    assert_eq!(raw_catalogs(&full), catalogs);
    let recording = Recording::from_bytes(&full).unwrap();
    let analysis = recording.analysis.legacy().unwrap();
    assert_eq!(
        analysis
            .spans
            .iter()
            .map(|span| (span.event, span.start, span.end, span.received_frame))
            .collect::<Vec<_>>(),
        [(17, 10, 40, 0), (18, 30, 80, 1)]
    );
    assert_eq!(analysis.open_event_starts().count(), 0);
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
        [(3, 9, 10, 1), (4, 51, 11, 1)]
    );
    assert_eq!(trim(&full, 0, 0), bytes);
    let proof = initial::Proof::prepare(&layout, 7, &mut || true).unwrap();
    let mut changed = Layout::parse(&bytes, &mut || true).unwrap();
    proof.verify_layout(&changed, &mut || true).unwrap();
    let HistoryEvent::Set { data, .. } = &mut changed.events[3] else {
        unreachable!()
    };
    data.to_mut()[18] ^= 1;
    assert!(proof.verify_layout(&changed, &mut || true).is_err());
}

#[test]
fn initial_analysis_prefix_later_zone_cannot_authorize_unknown_initial_state() {
    let (mut fixture, catalogs) = later_zone_fixture();
    fixture.add_renderer();
    fixture.records.last_mut().unwrap().body = sample::buffer(&sample::drawing(1, 1., 0));
    let source = fixture.bytes_with_catalogs(&catalogs);
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    let initial = initial::State::read(&layout, &mut || true).unwrap();
    assert!(
        initial
            .prefix_analysis(&layout, 7, &mut || true)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        layout.trim_prefix(0, &mut || true).unwrap_err().to_string(),
        "initial renderer requires an empty captured buffer"
    );
    prefix_refuses(
        |layout| {
            let mut later = layout.communication.zones[0].clone();
            later.id = 19;
            layout.communication.zones.insert(0, later);
        },
        "no unique captured source",
    );
}

#[test]
fn initial_analysis_prefix_validates_section_references_before_selecting_scope() {
    let source = sample::source();
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    layout
        .communication
        .sections
        .push(layout.communication.sections[0].clone());
    assert_eq!(
        layout.trim_prefix(0, &mut || true).unwrap_err().to_string(),
        "section event referenced twice"
    );
}

#[test]
fn initial_analysis_prefix_preserves_raw_bootstrap_and_observed_interval() {
    let source = sample::source();
    let bytes = trim(&source, 0, 0);
    let original = raw(&source);
    let output = raw(&bytes);
    assert_eq!(output.initial, original.initial);
    assert_eq!(output.reserved, 492);
    assert_eq!(output.rows, [[100, 2, 101, 8]]);
    assert_eq!(output.events, original.events[..9]);
    assert_eq!(
        output.events[1],
        set(3, 7, 11, &sample::profile(1, 17, 10), &[1])
    );
    assert!(output.deleted.is_empty());
    assert_eq!(raw_catalogs(&bytes), sample::catalogs());
    let recording = Recording::from_bytes(&bytes).unwrap();
    let analysis = recording.analysis.legacy().unwrap();
    assert_eq!(analysis.spans.len(), 1);
    let span = &analysis.spans[0];
    assert_eq!(
        (
            span.zone,
            span.event,
            span.start,
            span.end,
            span.received_frame
        ),
        (7, 17, 10, 40, 0)
    );
    assert_eq!(analysis.unmatched_stops, 0);
    assert_eq!(
        analysis
            .open_event_starts()
            .map(|(key, starts)| (*key, starts.to_vec()))
            .collect::<Vec<_>>(),
        [((7, 18, 99, 0x111), vec![30])]
    );
    assert_eq!(
        (
            analysis.memory.allocations,
            analysis.memory.frees,
            analysis.memory.unmatched_frees,
            analysis.memory.live_bytes,
            analysis.memory.total_allocated_bytes,
        ),
        (1, 0, 0, 8, 8)
    );
    assert_eq!(
        recording.memory_history_scope(),
        MemoryHistoryScope::AfterInitialSnapshot
    );
    let initial = recording.native_initial_memory_totals().unwrap().unwrap();
    assert_eq!((initial.len(), initial.row(0).unwrap().bytes), (1, 64));
    let state = recording.state_at(0).unwrap();
    let totals = recording.native_memory_totals_at(&state).unwrap().unwrap();
    assert_eq!((totals.len(), totals.row(0).unwrap().bytes), (1, 72));
    assert!(totals.row(1).is_none());
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
        (2, [10., 2., 3.], 0xff222222)
    );
    assert!(recording.untimed_error_events().is_empty());
    let mut errors = initial_errors(&recording);
    errors.sort_unstable();
    assert_eq!(errors, [(3, 7), (3, 7), (4, 50)]);
}

#[test]
fn initial_analysis_prefix_keeps_saved_error_ordinals_and_repeated_trim_bytes() {
    let source = sample::source();
    let bytes = trim(&source, 0, 1);
    let original = raw(&source);
    let output = raw(&bytes);
    assert_eq!(output.initial, original.initial);
    assert_eq!(output.rows, [[100, 2, 101, 8], [200, 9, 201, 19]]);
    let mut expected = original.events;
    expected[17] = saved(2, output.deleted[&2].0);
    assert_eq!(output.events, expected);
    assert_eq!(output.deleted[&2].2, body(7, &[10, 20], 1, 0xa1b2c3d4));
    assert_eq!(raw_catalogs(&bytes), sample::catalogs());
    let recording = Recording::from_bytes(&bytes).unwrap();
    assert_eq!(
        recording
            .untimed_error_events()
            .iter()
            .map(|error| (
                error.container_instance,
                error.index,
                error.history_event,
                error.received_frame,
            ))
            .collect::<Vec<_>>(),
        [(3, 9, 15, 1), (4, 51, 16, 1)]
    );
    assert_eq!(trim(&bytes, 0, 0), trim(&source, 0, 0));
    let middle = trim(&source, 1, 1);
    assert_eq!(trim(&bytes, 1, 1), middle);
    let after_middle = trim(&middle, 0, 0);
    assert_eq!(after_middle, middle);
    let repeated = raw(&after_middle);
    assert_eq!(
        repeated.events[1],
        set(3, 7, 11, &sample::profile(1, 18, 30), &[1])
    );
    assert_eq!(repeated.events.len(), 13);
    assert_eq!(repeated.events[8], array(5, 3, 9, 2, &error_record(9), &[]));
    assert_eq!(
        repeated.events[9],
        array(5, 4, 9, 1, &error_record(51), &[])
    );
}

#[test]
fn initial_analysis_prefix_accepts_nonempty_last_profile_without_rebuilding_snapshot() {
    let mut fixture = sample::fixture_with_analysis();
    fixture.events[7] = set(3, 7, 11, &sample::profile(1, 18, 50), &[1]);
    let source = fixture.bytes_with_catalogs(&sample::catalogs());
    let bytes = trim(&source, 0, 0);
    assert_eq!(raw(&bytes).initial[&7], (11, sample::buffer(&[])));
    assert_eq!(raw(&bytes).events[7], fixture.events[7]);
    let recording = Recording::from_bytes(&bytes).unwrap();
    assert_eq!(
        recording
            .analysis
            .legacy()
            .unwrap()
            .open_event_starts()
            .map(|(key, starts)| (*key, starts.to_vec()))
            .collect::<Vec<_>>(),
        [((7, 18, 99, 0x111), vec![30, 50])]
    );
}

fn prefix_refuses(mut change: impl FnMut(&mut Layout<'_>), expected: &str) {
    let source = sample::source();
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    change(&mut layout);
    let initial = initial::State::read(&layout, &mut || true).unwrap();
    let failure = initial
        .prefix_analysis(&layout, 9, &mut || true)
        .err()
        .unwrap();
    assert!(failure.to_string().contains(expected), "{failure}");
}

#[test]
fn initial_analysis_prefix_keeps_schema_source_context_and_full_lifetime_guards() {
    prefix_refuses(
        |layout| layout.communication.zones[0].source ^= 1,
        "source differs",
    );
    prefix_refuses(
        |layout| {
            layout
                .communication
                .instances
                .retain(|instance| instance.id != 7)
        },
        "unique captured source",
    );
    prefix_refuses(
        |layout| {
            layout
                .communication
                .zones
                .push(layout.communication.zones[0].clone())
        },
        "zone identities are ambiguous",
    );
    prefix_refuses(|layout| layout.communication.cameras.push((1, 1)), "camera");
    prefix_refuses(
        |layout| layout.metadata.classes[11].as_mut().unwrap().size = 12,
        "class layout",
    );
    prefix_refuses(
        |layout| layout.metadata.properties[10].as_mut().unwrap().offset32 = 4,
        "event layout",
    );
    prefix_refuses(
        |layout| {
            layout
                .saved
                .iter_mut()
                .find(|saved| saved.id == 7)
                .unwrap()
                .body = sample::buffer(&sample::profile(1, 17, 10)).into();
        },
        "empty live captured property",
    );
    prefix_refuses(
        |layout| {
            let HistoryEvent::Set { data, .. } = &mut layout.events[3] else {
                unreachable!()
            };
            *data = data[9..].to_vec().into();
        },
        "explicit captured instance",
    );
    for id in [6, 7] {
        for operation in 0..5 {
            prefix_refuses(
                |layout| {
                    layout.events[18] = match operation {
                        0 => HistoryEvent::Destroy(id),
                        1 => HistoryEvent::Create(id, if id == 6 { 10 } else { 11 }),
                        2 => HistoryEvent::Set {
                            kind: 4,
                            id,
                            property: 0,
                            data: vec![0; 8].into(),
                            undo: vec![18],
                        },
                        3 => HistoryEvent::Array {
                            kind: 5,
                            id,
                            property: if id == 6 { 10 } else { 11 },
                            index: 0,
                            data: Some(&[0]),
                            undo: vec![],
                        },
                        _ => HistoryEvent::Saved {
                            id,
                            position: layout
                                .saved
                                .iter()
                                .find(|saved| saved.id == id)
                                .unwrap()
                                .position,
                        },
                    };
                },
                if operation == 1 {
                    "stable captured lifetimes"
                } else if operation == 2 {
                    "complete captured buffers"
                } else {
                    "stable complete properties"
                },
            );
        }
    }
}

#[test]
fn initial_analysis_prefix_without_zone_cannot_authorize_old_renderer_payload() {
    let mut fixture = fixture();
    fixture.add_renderer();
    fixture.records.last_mut().unwrap().body = renderer_body(&sample::drawing(1, 1., 0));
    let source = fixture.bytes();
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    let initial = initial::State::read(&layout, &mut || true).unwrap();
    assert!(
        initial
            .prefix_analysis(&layout, 6, &mut || true)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        layout.trim_prefix(0, &mut || true).unwrap_err().to_string(),
        "initial renderer requires an empty captured buffer"
    );
}

#[test]
fn initial_analysis_prefix_proof_binds_original_inputs_without_middle_bootstrap() {
    let source = sample::source();
    let layout = Layout::parse(&source, &mut || true).unwrap();
    let initial = initial::State::read(&layout, &mut || true).unwrap();
    let scope = initial
        .prefix_analysis(&layout, 20, &mut || true)
        .unwrap()
        .unwrap();
    assert_eq!(
        scope
            .prove(&[], &mut 0, &mut || true)
            .unwrap_err()
            .to_string(),
        "prefix analysis cannot generate middle bootstrap proof"
    );
    let proof = initial::Proof::prepare(&layout, 20, &mut || true).unwrap();
    let bytes = trim(&source, 0, 1);
    proof
        .verify_layout(&Layout::parse(&bytes, &mut || true).unwrap(), &mut || true)
        .unwrap();
    for variant in 0..15 {
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
            6 | 7 => {
                let HistoryEvent::Set { data, undo, .. } = &mut changed.events[1] else {
                    unreachable!()
                };
                if variant == 6 {
                    data.to_mut()[18] ^= 1;
                } else {
                    undo.clear();
                }
            }
            8 | 9 => {
                changed.events[15] = HistoryEvent::Set {
                    kind: 4,
                    id: if variant == 8 { 7 } else { 6 },
                    property: 0,
                    data: vec![0; 8].into(),
                    undo: vec![15],
                }
            }
            10 => changed.events.swap(15, 16),
            11 => changed.communication.zones[0].source ^= 1,
            12 => {
                changed
                    .saved
                    .iter_mut()
                    .find(|saved| saved.id == 7)
                    .unwrap()
                    .class = 10
            }
            13 => {
                let HistoryEvent::Set { id, .. } = &mut changed.events[1] else {
                    unreachable!()
                };
                *id = 6;
            }
            14 => changed.communication.memory_names.reverse(),
            _ => unreachable!(),
        }
        assert!(
            proof.verify_layout(&changed, &mut || true).is_err(),
            "variant {variant}"
        );
    }
}

#[test]
fn initial_analysis_prefix_budget_and_cancellation_stop_before_output() {
    let source = sample::source();
    let layout = Layout::parse(&source, &mut || true).unwrap();
    let initial = initial::State::read(&layout, &mut || true).unwrap();
    let mut exact = 0;
    let mut calls = 0usize;
    initial::analysis::Scope::read_prefix(&initial, &layout, 9, &mut exact, &mut || {
        calls += 1;
        true
    })
    .unwrap();
    assert!(calls > 1);
    let mut allocation = MAX_METADATA - exact;
    initial::analysis::Scope::read_prefix(&initial, &layout, 9, &mut allocation, &mut || true)
        .unwrap();
    assert_eq!(allocation, MAX_METADATA);
    assert!(
        initial::analysis::Scope::read_prefix(
            &initial,
            &layout,
            9,
            &mut (MAX_METADATA - exact + 1),
            &mut || true
        )
        .is_err()
    );
    let mut remaining = calls - 1;
    assert!(
        initial::analysis::Scope::read_prefix(&initial, &layout, 9, &mut 0, &mut || {
            remaining = remaining.saturating_sub(1);
            remaining != 0
        })
        .is_err()
    );
    let mut output = Vec::new();
    let mut calls = 0;
    assert!(
        trim_scene_frames(&source, 0, 0, &mut output, || {
            calls += 1;
            calls < 40
        })
        .is_err()
    );
    assert!(output.is_empty());
}

#[test]
#[ignore = "retains constructed inputs in PVD_NATIVE_INITIAL_ANALYSIS_PREFIX_DIRECTORY"]
fn retain_initial_analysis_prefix_fixtures() {
    let directory = std::path::PathBuf::from(
        std::env::var_os("PVD_NATIVE_INITIAL_ANALYSIS_PREFIX_DIRECTORY")
            .expect("set PVD_NATIVE_INITIAL_ANALYSIS_PREFIX_DIRECTORY"),
    );
    std::fs::create_dir_all(&directory).unwrap();
    let source = sample::source();
    let middle = trim(&source, 1, 1);
    for (name, bytes) in [
        ("source", source.clone()),
        ("first", trim(&source, 0, 0)),
        ("full", trim(&source, 0, 1)),
        ("after-middle", trim(&middle, 0, 0)),
    ] {
        std::fs::write(
            directory.join(format!("initial-analysis-prefix-{name}.pxd2")),
            bytes,
        )
        .unwrap();
    }
}
