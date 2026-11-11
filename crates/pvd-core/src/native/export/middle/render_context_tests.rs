use super::*;
use crate::Recording;

const EMPTY_BULK: &[u8] = &[11, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
const SET_ZERO: &[u8] = &[1, 0, 0, 0, 0, 0, 0, 0, 0];
const SET_17: &[u8] = &[1, 17, 0, 0, 0, 0, 0, 0, 0];
const SET_18: &[u8] = &[1, 18, 0, 0, 0, 0, 0, 0, 0];
const POINT: &[u8] = &[
    2, 1, 0, 0, 0, 0, 0, 128, 63, 0, 0, 0, 64, 0, 0, 64, 64, 68, 51, 34, 17,
];
const TRIANGLE: &[u8] = &[
    4, 1, 0, 0, 0, 0, 0, 128, 63, 0, 0, 0, 64, 0, 0, 64, 64, 68, 51, 34, 17, 0, 0, 128, 64, 0, 0,
    160, 64, 0, 0, 192, 64, 136, 119, 102, 85, 0, 0, 224, 64, 0, 0, 0, 65, 0, 0, 16, 65, 204, 187,
    170, 153,
];

#[derive(Clone, Copy)]
enum Step<'a> {
    Buffer(&'a [u8]),
    Memory(&'a [u8]),
    Totals(&'a [i32]),
    Profile(&'a [u8]),
    Begin(u64),
    End(u64),
    Destroy,
    Create,
}

fn metadata(analysis: bool) -> Metadata {
    let mut metadata = fixture_metadata();
    for (id, text) in [(13, "PvdU8"), (14, "PvdUserRenderer"), (15, "events")] {
        metadata.strings.insert(id, text.into());
    }
    metadata.classes.push(Some(Class {
        name: name(1, 13),
        base: u32::MAX,
        packing: crate::native::ClassPacking::default(),
        size: 1,
        properties: vec![],
        dynamic: false,
        has_pointers: false,
        string_offsets: vec![],
    }));
    metadata.classes.push(Some(Class {
        name: name(10, 14),
        base: u32::MAX,
        packing: crate::native::ClassPacking::default(),
        size: 24,
        properties: vec![4],
        dynamic: true,
        has_pointers: false,
        string_offsets: vec![],
    }));
    metadata.properties.push(Some(Property {
        owner: name(10, 14),
        name: 15,
        datatype: name(1, 13),
        datatype_id: 5,
        kind: 2,
        semantic: 0,
        offset32: 0,
        values: vec![],
    }));
    if analysis {
        for (id, text) in [
            (16, "PvdI32"),
            (17, "PxProfileMemoryEventBuffer"),
            (18, "MemoryEventTotals"),
            (19, "totals"),
            (20, "Allocation"),
            (21, "source.cpp"),
            (22, "PxProfileZone"),
            (23, "_debugger"),
        ] {
            metadata.strings.insert(id, text.into());
        }
        for (label, size, properties, dynamic) in [
            (name(1, 16), 4, vec![], false),
            (name(10, 17), 24, vec![5], true),
            (name(23, 18), 24, vec![6], true),
            (name(10, 22), 24, vec![7], true),
        ] {
            metadata.classes.push(Some(Class {
                name: label,
                base: u32::MAX,
                packing: crate::native::ClassPacking::default(),
                size,
                properties,
                dynamic,
                has_pointers: false,
                string_offsets: vec![],
            }));
        }
        for (owner, label, datatype, datatype_id) in [
            (name(10, 17), 15, name(1, 13), 5),
            (name(23, 18), 19, name(1, 16), 7),
            (name(10, 22), 15, name(1, 13), 5),
        ] {
            metadata.properties.push(Some(Property {
                owner,
                name: label,
                datatype,
                datatype_id,
                kind: 2,
                semantic: 0,
                offset32: 0,
                values: vec![],
            }));
        }
    }
    metadata
}

fn fixture(steps: &[Step<'_>]) -> Vec<u8> {
    let base = super::fixture();
    let mut layout = Layout::parse(&base, &mut || true).unwrap();
    let analysis = steps
        .iter()
        .any(|step| matches!(step, Step::Memory(_) | Step::Totals(_) | Step::Profile(_)));
    layout.metadata = metadata(analysis);
    let object_bytes = metadata_bytes(&layout.metadata);
    layout.object_bytes = &object_bytes;
    layout
        .pointer_layouts
        .resize(layout.metadata.classes.len(), vec![]);
    let mut encoded = Vec::new();
    let mut append = |event: HistoryEvent<'_>| {
        let mut bytes = Vec::new();
        event.encode(&mut bytes).unwrap();
        encoded.push(bytes);
    };
    append(HistoryEvent::Marker);
    append(HistoryEvent::Create(1, 0));
    append(HistoryEvent::Create(2, 6));
    if analysis {
        append(HistoryEvent::Create(3, 8));
        append(HistoryEvent::Create(4, 9));
        append(HistoryEvent::Create(5, 10));
    }
    let mut previous = 2usize;
    let mut renderer_alive = true;
    let mut previous_analysis = [3usize, 4, 5];
    let mut begin = None;
    let mut rows = Vec::new();
    for step in steps {
        let ordinal = encoded.len();
        let event = match *step {
            Step::Buffer(data) => {
                let undo = vec![(ordinal - previous) as u32];
                previous = ordinal;
                HistoryEvent::Set {
                    kind: 3,
                    id: 2,
                    property: 4,
                    data: data.into(),
                    undo,
                }
            }
            Step::Memory(data) | Step::Profile(data) => {
                let slot = if matches!(step, Step::Memory(_)) {
                    0
                } else {
                    2
                };
                let undo = vec![(ordinal - previous_analysis[slot]) as u32];
                previous_analysis[slot] = ordinal;
                HistoryEvent::Set {
                    kind: 3,
                    id: [3, 4, 5][slot],
                    property: [5, 6, 7][slot],
                    data: data.into(),
                    undo,
                }
            }
            Step::Totals(values) => {
                let undo = vec![(ordinal - previous_analysis[1]) as u32];
                previous_analysis[1] = ordinal;
                let bytes: Vec<_> = values
                    .iter()
                    .flat_map(|value| value.to_le_bytes())
                    .collect();
                HistoryEvent::Set {
                    kind: 3,
                    id: 4,
                    property: 6,
                    data: bytes.into(),
                    undo,
                }
            }
            Step::Begin(ticks) => {
                assert!(begin.replace((ticks, ordinal as u64)).is_none());
                let bytes = section(ticks, true);
                encoded.push(bytes);
                continue;
            }
            Step::End(ticks) => {
                let (before, event) = begin.take().unwrap();
                rows.push([before, event, ticks, ordinal as u64]);
                encoded.push(section(ticks, false));
                continue;
            }
            Step::Destroy => {
                renderer_alive = false;
                HistoryEvent::Destroy(2)
            }
            Step::Create => {
                renderer_alive = true;
                previous = ordinal;
                HistoryEvent::Create(2, 6)
            }
        };
        let mut bytes = Vec::new();
        event.encode(&mut bytes).unwrap();
        encoded.push(bytes);
    }
    assert!(begin.is_none());
    layout.events = encoded
        .iter()
        .enumerate()
        .map(|(ordinal, bytes)| {
            HistoryEvent::parse(bytes, ordinal as u64, &layout.metadata, &mut 0).unwrap()
        })
        .collect();
    layout.communication.instances = vec![
        Instance {
            id: 1,
            source: 0x111,
            flags: [0; 10],
        },
        Instance {
            id: 2,
            source: 0x222,
            flags: [0; 10],
        },
    ];
    layout.communication.reverse = vec![(0x111, 1), (0x222, 2)];
    if !renderer_alive {
        layout.communication.reverse.retain(|(_, id)| *id != 2);
    }
    layout.communication.sections[0].rows = rows;
    layout.communication.zones.clear();
    layout.communication.memory_instances = [u32::MAX; 2];
    layout.communication.memory_names.clear();
    if analysis {
        layout.communication.instances.extend([
            Instance {
                id: 3,
                source: 0x333,
                flags: [0; 10],
            },
            Instance {
                id: 5,
                source: 0x555,
                flags: [0; 10],
            },
        ]);
        layout
            .communication
            .reverse
            .extend([(0x333, 3), (0x555, 5)]);
        layout.communication.memory_instances = [3, 4];
        layout.communication.memory_names = vec![(b"Allocation\0", b"source.cpp\0", 7, 0)];
        layout.communication.zones = vec![Zone {
            source: 0x555,
            saved_word: 0,
            name: b"Zone\0",
            id: 5,
            events: vec![(b"Event\0", 7, 1)],
        }];
    }
    layout.saved.clear();
    encode_modified(&mut layout)
}

fn source() -> Vec<u8> {
    fixture(&[
        Step::Begin(100),
        Step::Buffer(SET_17),
        Step::Buffer(POINT),
        Step::Buffer(EMPTY_BULK),
        Step::End(101),
        Step::Begin(200),
        Step::Buffer(TRIANGLE),
        Step::Buffer(SET_18),
        Step::Buffer(POINT),
        Step::End(201),
        Step::Begin(300),
        Step::Buffer(TRIANGLE),
        Step::End(301),
        Step::Buffer(POINT),
    ])
}

fn trim(source: &[u8], first: usize, last: usize) -> (Vec<u8>, Recording) {
    let mut output = Vec::new();
    trim_scene_frames(source, first, last, &mut output, || true).unwrap();
    let restored = Recording::from_bytes(&output).unwrap();
    let original = Recording::from_bytes(source).unwrap();
    for frame in (0..restored.frame_count())
        .chain((0..restored.frame_count()).rev())
        .chain([0, last - first, 0])
    {
        assert_eq!(
            restored.state_at(frame).unwrap(),
            original.state_at(frame + first).unwrap()
        );
    }
    restored.verify_replay().unwrap();
    let mut rewritten = Vec::new();
    rewrite(&output, &mut rewritten, || true).unwrap();
    assert_eq!(rewritten, output);
    if let Some(directory) = std::env::var_os("PVD_NATIVE_RENDER_CONTEXT_DIRECTORY") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("constructed-renderer-source.pxd2"), source).unwrap();
        std::fs::write(directory.join("constructed-renderer-middle.pxd2"), &output).unwrap();
    }
    (output, restored)
}

fn is_triangle(value: &crate::debug_draw::Primitive, instance: u64) -> bool {
    matches!(value, crate::debug_draw::Primitive::Triangle { instance: actual, positions, colors }
        if *actual == instance
            && *positions == [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]]
            && *colors == [0x11223344, 0x55667788, 0x99aabbcc])
}

fn drawing(state: &crate::SceneState) -> Vec<&crate::debug_draw::Primitive> {
    state
        .debug_draw
        .values()
        .flat_map(|batch| batch.iter())
        .collect()
}

#[test]
fn renderer_middle_preserves_captured_context_without_replaying_old_primitives() {
    let source = source();
    let (output, restored) = trim(&source, 1, 2);
    let first = restored.state_at(0).unwrap();
    let first = drawing(&first);
    assert_eq!(first.len(), 2);
    assert!(is_triangle(first[0], 17));
    assert!(matches!(
        first[1],
        crate::debug_draw::Primitive::Point {
            instance: 18,
            position: [1.0, 2.0, 3.0],
            color: 0x11223344,
        }
    ));
    let second = restored.state_at(1).unwrap();
    let second = drawing(&second);
    assert_eq!(second.len(), 1);
    assert!(is_triangle(second[0], 18));
    let layout = Layout::parse(&output, &mut || true).unwrap();
    assert!(matches!(&layout.events[1], HistoryEvent::Set {
        kind: 3, id: 2, property: 4, data, undo,
    } if data.as_ref() == SET_17 && undo == &[1]));
    assert!(
        matches!(&layout.events[3], HistoryEvent::Set { data, undo, .. }
        if data.as_ref() == TRIANGLE && undo == &[2])
    );
    let initial = initial::State::read(&layout, &mut || true).unwrap();
    let entry = initial.entries.iter().find(|entry| entry.id == 2).unwrap();
    let mut body = Reader::new(&layout.saved[entry.record].body);
    assert_eq!(body.blob().unwrap(), [0; 24]);
    assert_eq!(body.blob().unwrap(), EMPTY_BULK);
    body.end().unwrap();
}

#[test]
fn renderer_middle_distinguishes_default_zero_from_captured_explicit_zero() {
    for (prior, seeded) in [(&[][..], false), (SET_ZERO, true)] {
        let source = fixture(&[
            Step::Begin(100),
            Step::Buffer(prior),
            Step::Buffer(POINT),
            Step::Buffer(EMPTY_BULK),
            Step::End(101),
            Step::Begin(200),
            Step::Buffer(TRIANGLE),
            Step::End(201),
        ]);
        let (output, restored) = trim(&source, 1, 1);
        let state = restored.state_at(0).unwrap();
        let drawing = drawing(&state);
        assert_eq!(drawing.len(), 1);
        assert!(is_triangle(drawing[0], 0));
        let layout = Layout::parse(&output, &mut || true).unwrap();
        assert_eq!(
            matches!(&layout.events[1], HistoryEvent::Set { data, .. }
            if data.as_ref() == SET_ZERO),
            seeded
        );
    }
}

#[test]
fn renderer_middle_empty_buffers_and_no_drawing_do_not_create_primitives() {
    for first_buffer in [None, Some(&[][..]), Some(EMPTY_BULK), Some(SET_18)] {
        let mut steps = vec![
            Step::Begin(100),
            Step::Buffer(SET_17),
            Step::Buffer(POINT),
            Step::End(101),
            Step::Begin(200),
        ];
        if let Some(buffer) = first_buffer {
            steps.push(Step::Buffer(buffer));
        }
        steps.push(Step::End(201));
        let source = fixture(&steps);
        let (output, restored) = trim(&source, 1, 1);
        assert!(drawing(&restored.state_at(0).unwrap()).is_empty());
        let layout = Layout::parse(&output, &mut || true).unwrap();
        assert!(matches!(
            layout.events[1],
            HistoryEvent::Section { begin: 1, .. }
        ));
    }
}

#[test]
fn renderer_middle_rejects_uncaptured_first_frame_and_interframe_primitives_before_output() {
    let missing = fixture(&[
        Step::Begin(100),
        Step::Buffer(SET_17),
        Step::End(101),
        Step::Begin(200),
        Step::End(201),
        Step::Begin(300),
        Step::Buffer(TRIANGLE),
        Step::End(301),
    ]);
    let gap = fixture(&[
        Step::Begin(100),
        Step::Buffer(SET_17),
        Step::End(101),
        Step::Buffer(POINT),
        Step::Begin(200),
        Step::Buffer(TRIANGLE),
        Step::End(201),
    ]);
    for (source, last, expected) in [
        (missing, 2, "first retained frame"),
        (gap, 1, "first-frame gap"),
    ] {
        let mut output = Vec::new();
        let failure = trim_scene_frames(&source, 1, last, &mut output, || true).unwrap_err();
        assert!(failure.to_string().contains(expected), "{failure}");
        assert!(output.is_empty());
    }
    let source = fixture(&[
        Step::Begin(100),
        Step::Buffer(SET_17),
        Step::End(101),
        Step::Buffer(SET_18),
        Step::Begin(200),
        Step::Buffer(TRIANGLE),
        Step::End(201),
    ]);
    let (_, restored) = trim(&source, 1, 1);
    assert!(is_triangle(drawing(&restored.state_at(0).unwrap())[0], 18));
}

#[test]
fn renderer_middle_rejects_recreated_renderer_and_wrong_source_undo_before_output() {
    let recreated = fixture(&[
        Step::Begin(100),
        Step::Buffer(SET_17),
        Step::End(101),
        Step::Destroy,
        Step::Create,
        Step::Begin(200),
        Step::Buffer(TRIANGLE),
        Step::End(201),
    ]);
    let source = source();
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    let target = layout
        .events
        .iter_mut()
        .find(|event| {
            matches!(event,
        HistoryEvent::Set { id: 2, data, .. } if data.as_ref() == TRIANGLE)
        })
        .unwrap();
    if let HistoryEvent::Set { undo, .. } = target {
        *undo = vec![1];
    }
    let wrong_undo = encode_modified(&mut layout);
    for source in [recreated, wrong_undo] {
        let mut output = Vec::new();
        assert!(trim_scene_frames(&source, 1, 1, &mut output, || true).is_err());
        assert!(output.is_empty());
    }
}

#[test]
fn renderer_middle_proof_rejects_seed_snapshot_retained_payload_and_undo_tampering() {
    let source = source();
    let (output, _) = trim(&source, 1, 2);
    let source_layout = Layout::parse(&source, &mut || true).unwrap();
    let proof = history::prepare(&source_layout, 1, 2, &mut || true).unwrap();
    proof
        .verify_layout(&Layout::parse(&output, &mut || true).unwrap(), &mut || true)
        .unwrap();
    for variant in 0..7 {
        let mut changed = Layout::parse(&output, &mut || true).unwrap();
        match variant {
            0 => {
                if let HistoryEvent::Set { data, .. } = &mut changed.events[1] {
                    *data = SET_18.into();
                }
            }
            1 => {
                if let HistoryEvent::Set { undo, .. } = &mut changed.events[1] {
                    undo.clear();
                }
            }
            2 => {
                if let HistoryEvent::Set { undo, .. } = &mut changed.events[3] {
                    *undo = vec![3];
                }
            }
            3 => {
                if let HistoryEvent::Set { data, .. } = &mut changed.events[3] {
                    let mut bytes = data.to_vec();
                    bytes[17] ^= 1;
                    *data = bytes.into();
                }
            }
            4 => {
                let initial = initial::State::read(&changed, &mut || true).unwrap();
                let record = initial
                    .entries
                    .iter()
                    .find(|entry| entry.id == 2)
                    .unwrap()
                    .record;
                let mut body = changed.saved[record].body.to_vec();
                *body.last_mut().unwrap() ^= 1;
                changed.saved[record].body = body.into();
            }
            5 => {
                changed
                    .communication
                    .instances
                    .iter_mut()
                    .find(|entry| entry.id == 2)
                    .unwrap()
                    .source ^= 1
            }
            6 => changed.events[1] = HistoryEvent::Marker,
            _ => unreachable!(),
        }
        assert!(
            proof.verify_layout(&changed, &mut || true).is_err(),
            "variant {variant}"
        );
    }
}

#[test]
fn renderer_middle_cancelled_export_publishes_no_bytes() {
    let source = source();
    let mut output = Vec::new();
    let failure = trim_scene_frames(&source, 1, 2, &mut output, || false).unwrap_err();
    assert_eq!(
        failure.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
    assert!(output.is_empty());
}

#[test]
fn renderer_middle_context_coexists_with_pending_profile_and_partial_memory_history() {
    let start = [
        1, 15, 7, 0, 99, 0, 0, 0, 42, 0, 0, 0, 0, 0, 0, 0, 13, 21, 100, 0, 0, 0, 0, 0, 0, 0,
    ];
    let stop = [
        2, 15, 7, 0, 99, 0, 0, 0, 42, 0, 0, 0, 0, 0, 0, 0, 13, 21, 130, 0, 0, 0, 0, 0, 0, 0,
    ];
    let source = fixture(&[
        Step::Begin(100),
        Step::Memory(&[2, 0, 64, 20, 21, 7, 42]),
        Step::Totals(&[64]),
        Step::Profile(&start),
        Step::Buffer(SET_17),
        Step::End(101),
        Step::Begin(200),
        Step::Buffer(TRIANGLE),
        Step::Profile(&stop),
        Step::Memory(&[3, 0, 42, 2, 0, 16, 20, 21, 7, 43]),
        Step::Totals(&[16]),
        Step::End(201),
    ]);
    let (output, restored) = trim(&source, 1, 1);
    let layout = Layout::parse(&output, &mut || true).unwrap();
    assert!(
        matches!(&layout.events[1], HistoryEvent::Set { kind: 3, id: 5, data, undo, .. }
        if data.as_ref() == start && undo == &[1])
    );
    assert!(
        matches!(&layout.events[2], HistoryEvent::Set { kind: 3, id: 2, data, undo, .. }
        if data.as_ref() == SET_17 && undo == &[2])
    );
    assert!(
        matches!(&layout.events[4], HistoryEvent::Set { kind: 3, id: 2, data, undo, .. }
        if data.as_ref() == TRIANGLE && undo == &[2])
    );
    let analysis = restored.analysis.legacy().unwrap();
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
        (5, 7, 99, 42, 100, 130, 0)
    );
    assert_eq!(
        (
            analysis.memory.allocations,
            analysis.memory.frees,
            analysis.memory.unmatched_frees,
            analysis.memory.live_bytes
        ),
        (1, 1, 1, 16)
    );
    assert_eq!(
        restored.memory_history_scope(),
        crate::MemoryHistoryScope::AfterInitialSnapshot
    );
    let initial = restored.native_initial_memory_totals().unwrap().unwrap();
    assert_eq!(initial.row(0).unwrap().bytes, 64);
    let state = restored.state_at(0).unwrap();
    assert!(is_triangle(drawing(&state)[0], 17));
    let totals = restored.native_memory_totals_at(&state).unwrap().unwrap();
    assert_eq!(totals.row(0).unwrap().bytes, 16);
}

#[test]
fn renderer_middle_supports_known_array_heads_and_preserves_owner_flags() {
    let source = source();
    for (size, dynamic) in [(12, false), (12, true), (24, false), (24, true)] {
        let mut layout = Layout::parse(&source, &mut || true).unwrap();
        let class = layout.metadata.classes[6].as_mut().unwrap();
        class.size = size;
        class.dynamic = dynamic;
        let object_bytes = metadata_bytes(&layout.metadata);
        layout.object_bytes = &object_bytes;
        let source = encode_modified(&mut layout);
        let (output, restored) = trim(&source, 1, 2);
        assert!(is_triangle(drawing(&restored.state_at(0).unwrap())[0], 17));
        let layout = Layout::parse(&output, &mut || true).unwrap();
        let class = layout.metadata.class(6).unwrap();
        assert_eq!((class.size, class.dynamic), (size, dynamic));
        let initial = initial::State::read(&layout, &mut || true).unwrap();
        let entry = initial.entries.iter().find(|entry| entry.id == 2).unwrap();
        let mut body = Reader::new(&layout.saved[entry.record].body);
        assert_eq!(body.blob().unwrap(), vec![0; size]);
        assert_eq!(body.blob().unwrap(), EMPTY_BULK);
        body.end().unwrap();
    }
}

#[test]
fn renderer_middle_unknown_array_head_layouts_remain_rejected_before_output() {
    let source = source();
    for size in [16, 32] {
        let mut layout = Layout::parse(&source, &mut || true).unwrap();
        layout.metadata.classes[6].as_mut().unwrap().size = size;
        let object_bytes = metadata_bytes(&layout.metadata);
        layout.object_bytes = &object_bytes;
        let source = encode_modified(&mut layout);
        let mut output = Vec::new();
        let failure = trim_scene_frames(&source, 1, 2, &mut output, || true).unwrap_err();
        assert!(
            failure
                .to_string()
                .contains("unsupported renderer context class layout"),
            "{failure}"
        );
        assert!(output.is_empty());
    }
}

#[test]
fn renderer_middle_preserves_the_selected_end_identity_before_later_destruction() {
    let source = fixture(&[
        Step::Begin(100),
        Step::Buffer(SET_17),
        Step::Buffer(POINT),
        Step::Buffer(EMPTY_BULK),
        Step::End(101),
        Step::Begin(200),
        Step::Buffer(TRIANGLE),
        Step::End(201),
        Step::Begin(300),
        Step::Buffer(TRIANGLE),
        Step::End(301),
        Step::Destroy,
    ]);
    let original = Layout::parse(&source, &mut || true).unwrap();
    assert_eq!(original.communication.reverse, [(0x111, 1)]);
    let (output, restored) = trim(&source, 1, 2);
    assert!(is_triangle(drawing(&restored.state_at(0).unwrap())[0], 17));
    let actual = Layout::parse(&output, &mut || true).unwrap();
    assert_eq!(actual.communication.reverse, [(0x111, 1), (0x222, 2)]);
}

#[test]
fn renderer_middle_proof_rejects_wrong_missing_and_extra_source_identities() {
    let source = source();
    let (output, _) = trim(&source, 1, 2);
    let source_layout = Layout::parse(&source, &mut || true).unwrap();
    let proof = history::prepare(&source_layout, 1, 2, &mut || true).unwrap();
    for variant in 0..3 {
        let mut changed = Layout::parse(&output, &mut || true).unwrap();
        match variant {
            0 => {
                changed
                    .communication
                    .reverse
                    .iter_mut()
                    .find(|(_, id)| *id == 2)
                    .unwrap()
                    .0 ^= 1
            }
            1 => changed.communication.reverse.retain(|(_, id)| *id != 2),
            _ => changed.communication.reverse.push((0x999, 2)),
        }
        assert!(
            proof.verify_layout(&changed, &mut || true).is_err(),
            "variant {variant}"
        );
    }
}

#[test]
fn renderer_middle_reverse_identities_follow_source_order_for_multiple_renderers() {
    let source = source();
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    for (ordinal, event) in layout.events.iter_mut().enumerate() {
        if let HistoryEvent::Set { undo, .. } = event {
            for distance in undo {
                if ordinal - (*distance as usize) < 3 {
                    *distance += 2;
                }
            }
        }
    }
    layout.events.splice(
        3..3,
        [
            HistoryEvent::Create(6, 6),
            HistoryEvent::Set {
                kind: 3,
                id: 6,
                property: 4,
                data: (&[][..]).into(),
                undo: vec![1],
            },
        ],
    );
    for row in &mut layout.communication.sections[0].rows {
        row[1] += 2;
        row[3] += 2;
    }
    layout.communication.instances.push(Instance {
        id: 6,
        source: 0x100,
        flags: [0; 10],
    });
    layout.communication.reverse = vec![(0x100, 6), (0x111, 1), (0x222, 2)];
    let source = encode_modified(&mut layout);
    let (output, _) = trim(&source, 1, 2);
    let actual = Layout::parse(&output, &mut || true).unwrap();
    assert_eq!(
        actual.communication.reverse,
        [(0x100, 6), (0x111, 1), (0x222, 2)]
    );
}
