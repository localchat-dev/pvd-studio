use super::*;
use crate::native::{Class, Message, MessageEntry, Property, container_tests, read_with_progress};
use pvd_protocol::Name;

fn name(namespace: u32, name: u32) -> Name {
    Name { namespace, name }
}

fn metadata() -> Metadata {
    let mut metadata = container_tests::fixture();
    for (id, value) in [
        (14, "Body"),
        (15, "UserCustomedEvent"),
        (16, "Events"),
        (17, "PvdErrorMessage"),
        (18, "Code"),
        (19, "Message"),
        (20, "File"),
        (21, "Line"),
        (22, "Index"),
        (23, "Error"),
        (24, "Source.cpp"),
    ] {
        metadata.strings.insert(id, value.into());
    }
    metadata.classes[1].as_mut().unwrap().string_offsets = vec![0];
    metadata.classes[1].as_mut().unwrap().has_pointers = true;
    metadata.classes[2].as_mut().unwrap().has_pointers = true;
    for (label, base, size, properties, string_offsets) in [
        (name(1, 14), 4, 20, vec![], vec![]),
        (name(11, 17), u32::MAX, 20, (3..8).collect(), vec![4, 8]),
        (name(11, 15), u32::MAX, 12, vec![8], vec![]),
    ] {
        metadata.classes.push(Some(Class {
            name: label,
            base,
            packing: crate::native::ClassPacking::default(),
            size,
            properties,
            dynamic: false,
            has_pointers: !string_offsets.is_empty(),
            string_offsets,
        }));
    }
    for (label, datatype, offset) in [(18, 0, 0), (19, 1, 4), (20, 1, 8), (21, 0, 12), (22, 0, 16)]
    {
        metadata.properties.push(Some(Property {
            owner: name(11, 17),
            name: label,
            datatype: name(1, if datatype == 0 { 2 } else { 3 }),
            datatype_id: datatype,
            kind: 1,
            semantic: 0,
            offset32: offset,
            values: vec![],
        }));
    }
    metadata.properties.push(Some(Property {
        owner: name(11, 15),
        name: 16,
        datatype: name(11, 17),
        datatype_id: 7,
        kind: 2,
        semantic: 0,
        offset32: 0,
        values: vec![],
    }));
    metadata.messages = vec![Some(Message {
        class_name: name(1, 5),
        class: 3,
        size: 4,
        entries: vec![MessageEntry {
            datatype: name(1, 2),
            datatype_id: 0,
            property: 0,
            offset: 0,
            size: 4,
            destination_size: 4,
        }],
    })];
    metadata
}

fn metadata_bytes(metadata: &Metadata) -> Vec<u8> {
    let mut bytes = Vec::new();
    for value in [
        1,
        metadata.classes.len() as u32,
        metadata.strings.len() as u32 - 1,
        metadata.strings.len() as u32,
    ] {
        w32(&mut bytes, value);
    }
    for (&id, value) in &metadata.strings {
        if id != 0 {
            w32(&mut bytes, id);
            blob(&mut bytes, &[value.as_bytes(), &[0]].concat()).unwrap();
        }
    }
    w32(&mut bytes, metadata.properties.len() as u32);
    for (index, property) in metadata.properties.iter().enumerate() {
        w32(&mut bytes, u32::from(property.is_some()));
        let Some(property) = property else { continue };
        for value in [
            property.owner.namespace,
            property.owner.name,
            0,
            property.semantic,
            property.datatype_id,
            property.datatype.namespace,
            property.datatype.name,
            u32::from(property.kind),
            index as u32,
            property.offset32 as u32,
            property.offset32 as u32,
            0,
            property.name,
        ] {
            w32(&mut bytes, value);
        }
    }
    w32(&mut bytes, metadata.classes.len() as u32);
    for (index, class) in metadata.classes.iter().enumerate() {
        w32(&mut bytes, u32::from(class.is_some()));
        let Some(class) = class else { continue };
        for value in [
            class.name.namespace,
            class.name.name,
            index as u32,
            class.base,
            u32::MAX,
            u32::MAX,
        ] {
            w32(&mut bytes, value);
        }
        bytes.extend([0, u8::from(class.dynamic)]);
        for value in [
            class.size as u32,
            class.size as u32,
            4,
            class.size as u32,
            class.size as u32,
            4,
        ] {
            w32(&mut bytes, value);
        }
        w32(&mut bytes, class.properties.len() as u32);
        for &property in &class.properties {
            w32(&mut bytes, property);
        }
        for _ in 0..2 {
            w32(
                &mut bytes,
                u32::from(index == 2) + class.string_offsets.len() as u32,
            );
            if index == 2 {
                w32(&mut bytes, 1);
                w32(&mut bytes, 0);
            }
            for &offset in &class.string_offsets {
                w32(&mut bytes, 2);
                w32(&mut bytes, offset as u32);
            }
        }
    }
    w32(&mut bytes, metadata.messages.len() as u32);
    for (index, message) in metadata.messages.iter().enumerate() {
        w32(&mut bytes, u32::from(message.is_some()));
        let Some(message) = message else { continue };
        for value in [
            message.class_name.namespace,
            message.class_name.name,
            message.class,
            1,
            5,
            index as u32,
            message.size as u32,
            message.entries.len() as u32,
        ] {
            w32(&mut bytes, value);
        }
        for entry in &message.entries {
            for value in [
                entry.datatype.namespace,
                entry.datatype.name,
                entry.datatype_id,
                entry.offset as u32,
                entry.size as u32,
                entry.destination_size as u32,
                entry.property,
            ] {
                w32(&mut bytes, value);
            }
        }
        w32(&mut bytes, 0);
    }
    bytes
}

fn error_record(index: u32) -> Vec<u8> {
    [4u32, 23, 24, 42, index]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect()
}

fn node(id: u32, class: u32, value: u32, peers: &[u32]) -> Saved<'static> {
    let bytes = container_tests::instance(id, value, peers);
    let mut reader = Reader::new(&bytes);
    assert_eq!(reader.u32().unwrap(), 4);
    assert_eq!(reader.u32().unwrap(), id);
    Saved {
        id,
        class,
        position: 0,
        body: reader.blob().unwrap().to_vec().into(),
        deleted: false,
    }
}

fn encode_event(encoded: &mut Vec<Vec<u8>>, event: HistoryEvent<'_>) {
    let mut bytes = Vec::new();
    event.encode(&mut bytes).unwrap();
    encoded.push(bytes);
}

fn append_error(encoded: &mut Vec<Vec<u8>>, slot: u32, index: u32) {
    let bytes = error_record(index);
    encode_event(
        encoded,
        HistoryEvent::Array {
            kind: 5,
            id: 3,
            property: 8,
            index: slot,
            data: Some(&bytes),
            undo: vec![],
        },
    );
}

fn set(encoded: &mut Vec<Vec<u8>>, id: u32, property: u32, value: &[u8], previous: usize) {
    let ordinal = encoded.len();
    encode_event(
        encoded,
        HistoryEvent::Set {
            kind: 3,
            id,
            property,
            data: value.into(),
            undo: vec![(ordinal - previous) as u32],
        },
    );
}

fn section(encoded: &mut Vec<Vec<u8>>, ticks: u64, begin: bool) {
    encode_event(
        encoded,
        HistoryEvent::Section {
            class: 5,
            source: 0x111,
            ticks,
            name: 13,
            begin: u8::from(begin),
            padding: [0; 3],
        },
    );
}

fn fixture(alias: bool, destroy: bool) -> Vec<u8> {
    let metadata = metadata();
    let object_bytes = metadata_bytes(&metadata);
    let mut error_body = Vec::new();
    blob(&mut error_body, &[0; 12]).unwrap();
    blob(&mut error_body, &error_record(7)).unwrap();
    let mut saved = vec![
        node(1, 4, 7, &[2]),
        node(2, 6, 11, &[1]),
        Saved {
            id: 3,
            class: 8,
            position: 0,
            body: error_body.into(),
            deleted: false,
        },
    ];
    let mut position = 40;
    let mut directory = Vec::new();
    w32(&mut directory, 3);
    for record in &mut saved {
        record.position = position;
        w32(&mut directory, record.id);
        w64(&mut directory, position);
        position += record.body.len() as u64 + 12;
    }
    let witness = if destroy {
        if alias {
            saved[1].deleted = true;
            saved[1].position
        } else {
            let mut final_body = node(2, 6, 13, &[1]);
            final_body.position = position;
            final_body.deleted = true;
            saved.push(final_body);
            position
        }
    } else {
        0
    };
    let mut encoded = Vec::new();
    encode_event(&mut encoded, HistoryEvent::Marker);
    append_error(&mut encoded, 1, 8);
    let mut rows = Vec::new();
    let mut previous_scene = 0;
    let mut previous_actor = 0;
    for frame in 0..3 {
        let ticks = 100 + frame as u64 * 100;
        let begin = encoded.len() as u64;
        section(&mut encoded, ticks, true);
        let ordinal = encoded.len();
        set(
            &mut encoded,
            1,
            0,
            &(8 + frame as u32).to_le_bytes(),
            previous_scene,
        );
        previous_scene = ordinal;
        if !alias && (!destroy || frame < 2) {
            let ordinal = encoded.len();
            set(
                &mut encoded,
                2,
                0,
                &(12 + frame as u32).to_le_bytes(),
                previous_actor,
            );
            previous_actor = ordinal;
        }
        append_error(&mut encoded, frame as u32 + 2, frame as u32 + 9);
        if destroy && frame == 1 {
            encode_event(
                &mut encoded,
                HistoryEvent::Saved {
                    id: 2,
                    position: witness,
                },
            );
            encode_event(&mut encoded, HistoryEvent::Destroy(2));
        }
        let end = encoded.len() as u64;
        section(&mut encoded, ticks + 1, false);
        rows.push([ticks, begin, ticks + 1, end]);
    }
    append_error(&mut encoded, 5, 12);
    let events = encoded
        .iter()
        .enumerate()
        .map(|(index, bytes)| HistoryEvent::parse(bytes, index as u64, &metadata, &mut 0).unwrap())
        .collect::<Vec<_>>();
    let page = encode_page(&events, &mut || true).unwrap();
    let layout = Layout {
        source: &[],
        object_bytes: &object_bytes,
        metadata,
        pointer_layouts: pointer_layouts(&object_bytes, &mut 0).unwrap(),
        communication: Communication {
            clock: [1, 1],
            instances: (1..=3)
                .map(|id| Instance {
                    id,
                    source: u64::from(id) * 0x111,
                    flags: [0; 10],
                })
                .collect(),
            reverse: vec![(0x111, 1), (0x222, 2), (0x333, 3)],
            cameras: vec![],
            sections: vec![Section {
                source: 0x111,
                name: 13,
                rows,
            }],
            zones: vec![],
            memory_instances: [u32::MAX; 2],
            memory_names: vec![],
        },
        metadata_padding: &[],
        history_padding: &[],
        pages: vec![Page {
            first: 0,
            position: 4096,
            count: events.len() as u32,
            len: page.len() as u32,
        }],
        snapshots: vec![Snapshot {
            event: 0,
            position: 0,
            len: directory.len() as u32,
            reserved: 0,
        }],
        snapshot_bytes: vec![directory.into()],
        saved,
        events,
        backing: &[],
        gaps: vec![],
    };
    layout.encode(false, &mut || true).unwrap()
}

fn encode_modified(layout: &mut Layout<'_>) -> Vec<u8> {
    assert_eq!(layout.pages.len(), 1);
    layout.pages[0].count = layout.events.len() as u32;
    layout.pages[0].len = encode_page(&layout.events, &mut || true).unwrap().len() as u32;
    layout.encode(false, &mut || true).unwrap()
}

fn persist(label: &str, source: &[u8], output: &[u8]) {
    if let Some(directory) = std::env::var_os("PVD_NATIVE_CONSTRUCTED_EXPORT_DIRECTORY") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join(format!("retrim-{label}-source.pxd2")),
            source,
        )
        .unwrap();
        std::fs::write(
            directory.join(format!("retrim-{label}-trimmed.pxd2")),
            output,
        )
        .unwrap();
    }
}

fn check_prefix(label: &str, source: &[u8], last: usize) -> Vec<u8> {
    let (original, _) = read_with_progress(source, |_, _| true).unwrap();
    let mut output = Vec::new();
    trim_scene_frames(source, 0, last, &mut output, || true).unwrap();
    let (restored, _) = read_with_progress(&output, |_, _| true).unwrap();
    assert_eq!(restored.frame_count(), last + 1);
    assert_eq!(
        restored.initial_untimed_errors(),
        original.initial_untimed_errors()
    );
    for frame in (0..=last).chain((0..=last).rev()).chain([last, 0]) {
        assert_eq!(
            restored.state_at(frame).unwrap(),
            original.state_at(frame).unwrap()
        );
    }
    restored.verify_replay().unwrap();
    let mut copy = Vec::new();
    rewrite(&output, &mut copy, || true).unwrap();
    assert_eq!(copy, output);
    persist(label, source, &output);
    output
}

#[test]
fn nonempty_prefix_preserves_initial_values_preframe_occurrences_and_repeated_cuts() {
    let source = fixture(false, false);
    let first = check_prefix("ordinary-errors", &source, 1);
    let second = check_prefix("twice", &first, 0);
    let direct = check_prefix("direct", &source, 0);
    assert_eq!(second, direct);
    let (recording, _) = read_with_progress(&second, |_, _| true).unwrap();
    let initial = recording.initial_untimed_errors();
    assert_eq!(initial.len(), 1);
    assert_eq!(initial[0].index, 7);
    assert_eq!(initial[0].message, "Error");
    assert_eq!(initial[0].file, "Source.cpp");
    assert_eq!(
        recording
            .untimed_error_events()
            .iter()
            .map(|error| (error.index, error.received_frame, error.history_event))
            .collect::<Vec<_>>(),
        vec![(8, 0, 1), (9, 0, 5)]
    );
    let state = recording.state_at(0).unwrap();
    assert_eq!(state.objects[&1].properties["Value"], crate::Value::U64(8));
    assert_eq!(state.objects[&2].properties["Value"], crate::Value::U64(12));
    assert_eq!(
        state.objects[&2].properties["Name"],
        crate::Value::String("sample".into())
    );
    assert_eq!(
        state.objects[&2].properties["Peers"],
        crate::Value::Array(vec![crate::Value::Ref(1)].into())
    );
}

#[test]
fn nonempty_prefix_distinguishes_initial_and_deleted_versions_and_shared_positions() {
    for alias in [false, true] {
        let source = fixture(alias, true);
        for last in [0, 1, 2] {
            let output = check_prefix(
                if alias {
                    "shared-witness"
                } else {
                    "distinct-witness"
                },
                &source,
                last,
            );
            let layout = Layout::parse(&output, &mut || true).unwrap();
            let state = State::read(&layout, &mut || true).unwrap();
            let initial = state.entries.iter().find(|entry| entry.id == 2).unwrap();
            let deleted = layout
                .saved
                .iter()
                .filter(|saved| saved.deleted)
                .collect::<Vec<_>>();
            if last == 0 {
                assert!(deleted.is_empty());
            } else {
                assert_eq!(deleted.len(), 1);
                assert_eq!(deleted[0].id, 2);
                assert_eq!(initial.position == deleted[0].position, alias);
            }
        }
    }
}

#[test]
fn nonempty_prefix_seeds_inherited_property_message_owners() {
    let source = fixture(false, false);
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    let HistoryEvent::Set { kind, .. } = &mut layout.events[3] else {
        unreachable!()
    };
    *kind = 4;
    let source = encode_modified(&mut layout);
    check_prefix("message-owner", &source, 0);
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    let message = layout.metadata.messages[0].as_mut().unwrap();
    message.class = 6;
    message.class_name = name(1, 14);
    let objects = metadata_bytes(&layout.metadata);
    layout.object_bytes = &objects;
    let source = encode_modified(&mut layout);
    let mut output = Vec::new();
    assert!(trim_scene_frames(&source, 0, 0, &mut output, || true).is_err());
    assert!(output.is_empty());
}

#[test]
fn nonempty_prefix_proof_rejects_snapshot_event_and_deleted_directory_changes() {
    let source = fixture(false, true);
    let layout = Layout::parse(&source, &mut || true).unwrap();
    let cut = layout.prefix_cut(0, &mut || true).unwrap();
    let proof = Proof::prepare(&layout, cut, &mut || true).unwrap();
    let output = check_prefix("proof", &source, 0);
    for change in 0..3 {
        let mut layout = Layout::parse(&output, &mut || true).unwrap();
        match change {
            0 => {
                let initial = State::read(&layout, &mut || true).unwrap();
                let index = initial
                    .entries
                    .iter()
                    .find(|entry| entry.id == 2)
                    .unwrap()
                    .record;
                layout.saved[index].body.to_mut()[4] ^= 1;
            }
            1 => {
                let HistoryEvent::Set { data, .. } = &mut layout.events[3] else {
                    unreachable!()
                };
                data.to_mut()[0] ^= 1;
            }
            2 => {
                layout
                    .saved
                    .iter_mut()
                    .find(|saved| saved.id == 2)
                    .unwrap()
                    .deleted = true;
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
fn nonempty_prefix_refuses_unknown_initial_analysis_reuse_and_missing_witnesses() {
    for kind in [
        "PxProfileZone",
        "PxProfileMemoryEventBuffer",
        "MemoryEventTotals",
        "PvdUserRenderer",
    ] {
        let source = fixture(false, false);
        let mut layout = Layout::parse(&source, &mut || true).unwrap();
        layout.metadata.strings.insert(15, kind.into());
        let objects = metadata_bytes(&layout.metadata);
        layout.object_bytes = &objects;
        let source = encode_modified(&mut layout);
        let mut output = Vec::new();
        assert!(
            trim_scene_frames(&source, 0, 0, &mut output, || true).is_err(),
            "{kind}"
        );
        assert!(output.is_empty());
    }
    let source = fixture(false, false);
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    layout.events.push(HistoryEvent::Create(2, 6));
    let source = encode_modified(&mut layout);
    let mut output = Vec::new();
    assert!(trim_scene_frames(&source, 0, 0, &mut output, || true).is_err());
    assert!(output.is_empty());
    let source = fixture(false, true);
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    let saved = layout
        .events
        .iter()
        .position(|event| matches!(event, HistoryEvent::Saved { .. }))
        .unwrap();
    layout.events[saved] = HistoryEvent::Marker;
    let source = encode_modified(&mut layout);
    let mut output = Vec::new();
    assert!(trim_scene_frames(&source, 0, 1, &mut output, || true).is_err());
    assert!(output.is_empty());
}

#[test]
fn nonempty_prefix_rejects_initial_references_to_future_instances_and_checks_cancellation() {
    let source = fixture(false, false);
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    let initial = State::read(&layout, &mut || true).unwrap();
    let index = initial
        .entries
        .iter()
        .find(|entry| entry.id == 1)
        .unwrap()
        .record;
    let body = layout.saved[index].body.to_mut();
    let length = body.len();
    body[length - 4..].copy_from_slice(&99u32.to_le_bytes());
    layout.events.push(HistoryEvent::Create(99, 6));
    let source = encode_modified(&mut layout);
    let mut output = Vec::new();
    assert!(trim_scene_frames(&source, 0, 0, &mut output, || true).is_err());
    assert!(output.is_empty());
    let source = fixture(false, false);
    let layout = Layout::parse(&source, &mut || true).unwrap();
    for stop in [1, 8, 24] {
        let mut checks = 0;
        let failure = Proof::prepare(
            &layout,
            layout.prefix_cut(1, &mut || true).unwrap(),
            &mut || {
                checks += 1;
                checks < stop
            },
        )
        .err()
        .unwrap();
        assert!(failure.to_string().contains("cancelled"));
        assert_eq!(checks, stop);
    }
}

fn created_source() -> Vec<u8> {
    let source = fixture(false, false);
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    let mut encoded = Vec::new();
    encode_event(&mut encoded, HistoryEvent::Marker);
    for (id, class) in [(1, 4), (2, 6), (3, 8)] {
        encode_event(&mut encoded, HistoryEvent::Create(id, class));
    }
    let mut bootstrap = BTreeMap::new();
    for (id, value, peer, create) in [(1, 7u32, 2u32, 1), (2, 11u32, 1u32, 2)] {
        for (property, data) in [
            (0, value.to_le_bytes()),
            (1, 10u32.to_le_bytes()),
            (2, peer.to_le_bytes()),
        ] {
            bootstrap.insert((id, property), encoded.len());
            set(&mut encoded, id, property, &data, create);
        }
    }
    append_error(&mut encoded, 0, 7);
    let offset = encoded.len() - 1;
    for (ordinal, event) in layout.events.iter().enumerate().skip(1) {
        let mut event = event.clone();
        if let HistoryEvent::Set {
            id, property, undo, ..
        } = &mut event
        {
            for distance in undo {
                if ordinal - *distance as usize == 0 {
                    *distance = (ordinal + offset - bootstrap[&(*id, *property)]) as u32;
                }
            }
        }
        encode_event(&mut encoded, event);
    }
    for row in &mut layout.communication.sections[0].rows {
        row[1] += offset as u64;
        row[3] += offset as u64;
    }
    layout.events = encoded
        .iter()
        .enumerate()
        .map(|(ordinal, bytes)| {
            HistoryEvent::parse(bytes, ordinal as u64, &layout.metadata, &mut 0).unwrap()
        })
        .collect();
    layout.saved.clear();
    layout.snapshots[0].len = 4;
    layout.snapshot_bytes = vec![vec![0; 4].into()];
    encode_modified(&mut layout)
}

#[test]
fn project_middle_output_can_be_trimmed_again_without_reclassifying_errors() {
    let source = created_source();
    let mut middle = Vec::new();
    trim_scene_frames(&source, 1, 2, &mut middle, || true).unwrap();
    let prefix = check_prefix("project-middle", &middle, 0);
    let (original, _) = read_with_progress(&source, |_, _| true).unwrap();
    let (restored, _) = read_with_progress(&prefix, |_, _| true).unwrap();
    assert_eq!(restored.state_at(0).unwrap(), original.state_at(1).unwrap());
    assert_eq!(
        restored
            .initial_untimed_errors()
            .iter()
            .map(|error| error.index)
            .collect::<Vec<_>>(),
        vec![7, 8, 9]
    );
    assert_eq!(
        restored
            .untimed_error_events()
            .iter()
            .map(|error| (error.index, error.received_frame, error.history_event))
            .collect::<Vec<_>>(),
        vec![(10, 0, 4)]
    );
}

fn renderer_point(explicit: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    if explicit {
        bytes.push(1);
        bytes.extend(2u64.to_le_bytes());
    }
    bytes.push(2);
    bytes.extend(1u32.to_le_bytes());
    for value in [1f32, 2., 3.] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.extend(0xff123456u32.to_le_bytes());
    bytes
}

fn renderer_fixture(buffer: Option<&[u8]>, initial_buffer: &[u8]) -> Vec<u8> {
    let source = fixture(false, false);
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    for (id, value) in [(25, "PvdUserRenderer"), (26, "events"), (27, "PvdU8")] {
        layout.metadata.strings.insert(id, value.into());
    }
    for (label, size, properties) in [(name(1, 27), 1, vec![]), (name(11, 25), 12, vec![9])] {
        layout.metadata.classes.push(Some(Class {
            name: label,
            base: u32::MAX,
            packing: crate::native::ClassPacking::default(),
            size,
            properties,
            dynamic: false,
            has_pointers: false,
            string_offsets: vec![],
        }));
    }
    layout.metadata.properties.push(Some(Property {
        owner: name(11, 25),
        name: 26,
        datatype: name(1, 27),
        datatype_id: 9,
        kind: 2,
        semantic: 0,
        offset32: 0,
        values: vec![],
    }));
    let objects = metadata_bytes(&layout.metadata);
    layout.object_bytes = &objects;
    layout.pointer_layouts = pointer_layouts(&objects, &mut 0).unwrap();
    let mut body = Vec::new();
    blob(&mut body, &[0; 12]).unwrap();
    blob(&mut body, initial_buffer).unwrap();
    layout.saved.push(Saved {
        id: 4,
        class: 10,
        position: 0,
        body: body.into(),
        deleted: false,
    });
    let mut directory = Vec::new();
    w32(&mut directory, 4);
    let mut position = 52;
    for saved in &mut layout.saved {
        saved.position = position;
        w32(&mut directory, saved.id);
        w64(&mut directory, position);
        position += saved.body.len() as u64 + 12;
    }
    layout.snapshot_bytes = vec![directory.into()];
    layout.snapshots[0].len = 52;
    layout.communication.instances.push(Instance {
        id: 4,
        source: 0x444,
        flags: [0; 10],
    });
    layout.communication.reverse.push((0x444, 4));
    if let Some(buffer) = buffer {
        let at = layout.communication.sections[0].rows[0][1] as usize;
        for (ordinal, event) in layout.events.iter_mut().enumerate() {
            if let HistoryEvent::Set { undo, .. } | HistoryEvent::Array { undo, .. } = event {
                for distance in undo {
                    if ordinal >= at && ordinal - (*distance as usize) < at {
                        *distance += 1;
                    }
                }
            }
        }
        layout.events.insert(
            at,
            HistoryEvent::Set {
                kind: 3,
                id: 4,
                property: 9,
                data: buffer.into(),
                undo: vec![at as u32],
            },
        );
        for row in &mut layout.communication.sections[0].rows {
            row[1] += 1;
            row[3] += 1;
        }
    }
    encode_modified(&mut layout)
}

#[test]
fn empty_initial_renderer_requires_its_first_nonempty_buffer_to_set_instance() {
    for (label, buffer) in [
        ("renderer-no-buffers", None),
        ("renderer-empty-buffer", Some(vec![])),
        ("renderer-explicit", Some(renderer_point(true))),
    ] {
        let source = renderer_fixture(buffer.as_deref(), &[]);
        let output = check_prefix(label, &source, 0);
        let (restored, _) = read_with_progress(&output, |_, _| true).unwrap();
        let state = restored.state_at(0).unwrap();
        if label == "renderer-explicit" {
            assert_eq!(state.debug_draw[&4].len(), 1);
            assert!(matches!(
                &state.debug_draw[&4][0],
                crate::debug_draw::Primitive::Point {
                    instance: 2,
                    color: 0xff123456,
                    ..
                }
            ));
        } else {
            assert!(state.debug_draw.is_empty());
        }
    }
    let buffer = renderer_point(false);
    let source = renderer_fixture(Some(&buffer), &[]);
    let mut output = Vec::new();
    let failure = trim_scene_frames(&source, 0, 0, &mut output, || true).unwrap_err();
    assert!(
        failure.to_string().contains("explicit instance"),
        "{failure}"
    );
    assert!(output.is_empty());
    let buffer = renderer_point(true);
    let source = renderer_fixture(None, &buffer);
    let mut output = Vec::new();
    assert!(trim_scene_frames(&source, 0, 0, &mut output, || true).is_err());
    assert!(output.is_empty());
}

#[test]
fn initial_renderer_context_is_covered_once_and_empty_buffers_do_not_establish_it() {
    let explicit = [vec![1], 2u64.to_le_bytes().to_vec()].concat();
    for first in [Vec::new(), explicit] {
        let source = renderer_fixture(Some(&first), &[]);
        let mut layout = Layout::parse(&source, &mut || true).unwrap();
        let at = layout.communication.sections[0].rows[0][1] as usize;
        for (ordinal, event) in layout.events.iter_mut().enumerate() {
            if let HistoryEvent::Set { undo, .. } | HistoryEvent::Array { undo, .. } = event {
                for distance in undo {
                    if ordinal >= at && ordinal - (*distance as usize) < at {
                        *distance += 1;
                    }
                }
            }
        }
        layout.events.insert(
            at,
            HistoryEvent::Set {
                kind: 3,
                id: 4,
                property: 9,
                data: renderer_point(false).into(),
                undo: vec![1],
            },
        );
        for row in &mut layout.communication.sections[0].rows {
            row[1] += 1;
            row[3] += 1;
        }
        let source = encode_modified(&mut layout);
        if first.is_empty() {
            let mut output = Vec::new();
            let failure = trim_scene_frames(&source, 0, 0, &mut output, || true).unwrap_err();
            assert!(
                failure.to_string().contains("explicit instance"),
                "{failure}"
            );
            assert!(output.is_empty());
        } else {
            let output = check_prefix("renderer-carry", &source, 0);
            let (restored, _) = read_with_progress(&output, |_, _| true).unwrap();
            assert!(matches!(
                &restored.state_at(0).unwrap().debug_draw[&4][0],
                crate::debug_draw::Primitive::Point { instance: 2, .. }
            ));
        }
    }
}

#[test]
fn nonempty_prefix_preserves_opaque_snapshot_word_and_detects_changes() {
    for word in [492, u32::MAX] {
        let source = fixture(false, false);
        let mut layout = Layout::parse(&source, &mut || true).unwrap();
        layout.snapshots[0].reserved = word;
        let source = encode_modified(&mut layout);
        let layout = Layout::parse(&source, &mut || true).unwrap();
        let proof = Proof::prepare(
            &layout,
            layout.prefix_cut(0, &mut || true).unwrap(),
            &mut || true,
        )
        .unwrap();
        let output = check_prefix("opaque-word", &source, 0);
        let mut layout = Layout::parse(&output, &mut || true).unwrap();
        assert_eq!(layout.snapshots[0].reserved, word);
        layout.snapshots[0].reserved ^= 1;
        assert!(proof.verify_layout(&layout, &mut || true).is_err());
    }
}

#[test]
fn nonempty_prefix_does_not_classify_custom_scene_names_as_physx_scenes() {
    let source = fixture(false, false);
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    layout.metadata.strings.insert(25, "custom".into());
    layout.metadata.strings.insert(26, "PxScene".into());
    layout.metadata.classes[6].as_mut().unwrap().name = name(25, 26);
    let objects = metadata_bytes(&layout.metadata);
    layout.object_bytes = &objects;
    let source = encode_modified(&mut layout);
    check_prefix("scene-namespace", &source, 0);
}

#[test]
fn borrowed_payload_hash_checks_cancellation_inside_large_values() {
    let payload = vec![0x5a; 2 << 20];
    let mut hash = blake3::Hasher::new();
    let mut checks = 0;
    let failure = field(&mut hash, &payload, &mut || {
        checks += 1;
        checks < 5
    })
    .unwrap_err();
    assert!(failure.to_string().contains("cancelled"));
    assert_eq!(checks, 5);
    let state = State {
        entries: vec![],
        classes: BTreeMap::new(),
    };
    let mut budget = Budget::new(&state).unwrap();
    budget.add(MAX_METADATA - 8, 1).unwrap();
    assert!(budget.add(16, 1).is_err());
}
