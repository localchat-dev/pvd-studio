use super::*;
use crate::native::{Class, Message, MessageEntry, Property};
use pvd_protocol::Name;

#[path = "message_tests.rs"]
mod message_tests;

#[path = "alias_tests.rs"]
mod alias_tests;

#[path = "error_tests.rs"]
mod error_tests;

#[path = "memory_tests.rs"]
mod memory_tests;

#[path = "render_context_tests.rs"]
mod render_context_tests;

fn name(namespace: u32, name: u32) -> Name {
    Name { namespace, name }
}

fn fixture_metadata() -> Metadata {
    let strings = [
        "",
        "physx3",
        "PxScene",
        "PvdU32",
        "ObjectRef",
        "Body",
        "Value",
        "Other",
        "Items",
        "Reference",
        "_debugger_",
        "SectionEventData",
        "frame",
    ]
    .into_iter()
    .enumerate()
    .map(|(i, value)| (i as u32, value.into()))
    .collect();
    let class = |class_name, size, properties| {
        Some(Class {
            name: class_name,
            base: u32::MAX,
            packing: crate::native::ClassPacking::default(),
            size,
            properties,
            dynamic: false,
            has_pointers: class_name == name(1, 4),
            string_offsets: Vec::new(),
        })
    };
    let property = |label, datatype, kind, offset32| {
        Some(Property {
            owner: name(1, 5),
            name: label,
            datatype: name(1, if datatype == 1 { 3 } else { 4 }),
            datatype_id: datatype,
            kind,
            semantic: 0,
            offset32,
            values: Vec::new(),
        })
    };
    Metadata {
        strings,
        classes: vec![
            class(name(1, 2), 0, vec![]),
            class(name(1, 3), 4, vec![]),
            class(name(1, 4), 4, vec![]),
            class(name(1, 5), 24, vec![0, 1, 2, 3]),
            class(name(10, 11), 24, vec![]),
        ],
        properties: vec![
            property(6, 1, 1, 0),
            property(7, 1, 1, 4),
            property(8, 1, 2, 8),
            property(9, 2, 1, 20),
        ],
        messages: vec![Some(Message {
            class_name: name(1, 5),
            class: 3,
            size: 8,
            entries: vec![
                MessageEntry {
                    datatype: name(1, 3),
                    datatype_id: 1,
                    property: 0,
                    offset: 0,
                    size: 4,
                    destination_size: 4,
                },
                MessageEntry {
                    datatype: name(1, 3),
                    datatype_id: 1,
                    property: 1,
                    offset: 4,
                    size: 4,
                    destination_size: 4,
                },
            ],
        })],
    }
}

pub(in crate::native::export) fn metadata_bytes(metadata: &Metadata) -> Vec<u8> {
    let mut out = Vec::new();
    for n in [
        1,
        metadata.classes.len() as u32,
        metadata.strings.len() as u32 - 1,
        metadata.strings.len() as u32,
    ] {
        w32(&mut out, n);
    }
    for (&id, value) in &metadata.strings {
        if id == 0 {
            continue;
        }
        w32(&mut out, id);
        blob(&mut out, &[value.as_bytes(), &[0]].concat()).unwrap();
    }
    w32(&mut out, metadata.properties.len() as u32);
    for (id, p) in metadata.properties.iter().enumerate() {
        w32(&mut out, u32::from(p.is_some()));
        let Some(p) = p else { continue };
        for n in [
            p.owner.namespace,
            p.owner.name,
            0,
            p.semantic,
            p.datatype_id,
            p.datatype.namespace,
            p.datatype.name,
            u32::from(p.kind),
            id as u32,
            p.offset32 as u32,
            p.offset32 as u32,
            0,
            p.name,
        ] {
            w32(&mut out, n);
        }
    }
    w32(&mut out, metadata.classes.len() as u32);
    for (id, c) in metadata.classes.iter().enumerate() {
        w32(&mut out, u32::from(c.is_some()));
        let Some(c) = c else { continue };
        for n in [
            c.name.namespace,
            c.name.name,
            id as u32,
            c.base,
            c.packing.uniform_width as u32,
            c.packing.class_type as u32,
        ] {
            w32(&mut out, n);
        }
        out.extend([0, u8::from(c.dynamic)]);
        for n in [
            c.size as u32,
            c.size as u32,
            4,
            c.size as u32,
            c.size as u32,
            4,
        ] {
            w32(&mut out, n);
        }
        w32(&mut out, c.properties.len() as u32);
        for &property in &c.properties {
            w32(&mut out, property);
        }
        let void_pointer = id == 2 || (c.has_pointers && c.string_offsets.is_empty());
        for _ in 0..2 {
            w32(
                &mut out,
                u32::from(void_pointer) + c.string_offsets.len() as u32,
            );
            if void_pointer {
                w32(&mut out, 1);
                w32(&mut out, 0);
            }
            for &offset in &c.string_offsets {
                w32(&mut out, 2);
                w32(&mut out, offset as u32);
            }
        }
    }
    w32(&mut out, metadata.messages.len() as u32);
    for (id, message) in metadata.messages.iter().enumerate() {
        w32(&mut out, u32::from(message.is_some()));
        let Some(message) = message else { continue };
        for n in [
            message.class_name.namespace,
            message.class_name.name,
            message.class,
            1,
            5,
            id as u32,
            message.size as u32,
            message.entries.len() as u32,
        ] {
            w32(&mut out, n);
        }
        for entry in &message.entries {
            for n in [
                entry.datatype.namespace,
                entry.datatype.name,
                entry.datatype_id,
                entry.offset as u32,
                entry.size as u32,
                entry.destination_size as u32,
                entry.property,
            ] {
                w32(&mut out, n);
            }
        }
        w32(&mut out, 0);
    }
    out
}

fn set(kind: u32, property: u32, data: &[u8], undo: &[u32]) -> Vec<u8> {
    let mut out = Vec::new();
    HistoryEvent::Set {
        kind,
        id: 2,
        property,
        data: data.into(),
        undo: undo.to_vec(),
    }
    .encode(&mut out)
    .unwrap();
    out
}

fn section(ticks: u64, begin: bool) -> Vec<u8> {
    let mut out = Vec::new();
    HistoryEvent::Section {
        class: 4,
        source: 0x111,
        ticks,
        name: 12,
        begin: u8::from(begin),
        padding: [0; 3],
    }
    .encode(&mut out)
    .unwrap();
    out
}

fn fixture() -> Vec<u8> {
    let metadata = fixture_metadata();
    let mut body = RawInstance::create(&metadata, 3).unwrap();
    for (property, data) in [
        (0, 14u32.to_le_bytes().to_vec()),
        (1, 22u32.to_le_bytes().to_vec()),
        (2, vec![]),
        (3, 2u32.to_le_bytes().to_vec()),
    ] {
        body.set(&metadata, property, &data).unwrap();
    }
    let record = Saved {
        id: 2,
        position: 4,
        class: 3,
        body: body.body(&metadata).unwrap().into(),
        deleted: true,
    };
    let mut saved_event = Vec::new();
    HistoryEvent::Saved { id: 2, position: 4 }
        .encode(&mut saved_event)
        .unwrap();
    let event_bytes = vec![
        9u32.to_le_bytes().to_vec(),
        [1u32.to_le_bytes(), 1u32.to_le_bytes(), 0u32.to_le_bytes()].concat(),
        [1u32.to_le_bytes(), 2u32.to_le_bytes(), 3u32.to_le_bytes()].concat(),
        set(3, 0, &10u32.to_le_bytes(), &[1]),
        set(3, 1, &20u32.to_le_bytes(), &[2]),
        set(3, 2, &[], &[3]),
        set(3, 3, &2u32.to_le_bytes(), &[4]),
        section(100, true),
        set(3, 0, &11u32.to_le_bytes(), &[5]),
        section(101, false),
        section(200, true),
        set(3, 0, &12u32.to_le_bytes(), &[3]),
        set(3, 1, &21u32.to_le_bytes(), &[8]),
        set(
            4,
            0,
            &[13u32.to_le_bytes(), 22u32.to_le_bytes()].concat(),
            &[2, 1],
        ),
        section(201, false),
        section(300, true),
        set(3, 0, &14u32.to_le_bytes(), &[3]),
        saved_event,
        [2u32.to_le_bytes(), 2u32.to_le_bytes()].concat(),
        section(301, false),
    ];
    let events = event_bytes
        .iter()
        .enumerate()
        .map(|(i, bytes)| HistoryEvent::parse(bytes, i as u64, &metadata, &mut 0).unwrap())
        .collect::<Vec<_>>();
    let page = encode_page(&events, &mut || true).unwrap();
    let object_bytes = metadata_bytes(&metadata);
    let layout = Layout {
        source: &[],
        object_bytes: &object_bytes,
        metadata,
        pointer_layouts: vec![vec![], vec![], vec![(1, 0)], vec![], vec![]],
        communication: Communication {
            clock: [1, 1],
            instances: vec![
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
            ],
            reverse: vec![(0x111, 1)],
            cameras: vec![],
            sections: vec![Section {
                source: 0x111,
                name: 12,
                rows: vec![[100, 7, 101, 9], [200, 10, 201, 14], [300, 15, 301, 19]],
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
            len: 4,
            reserved: 0,
        }],
        snapshot_bytes: vec![Cow::Borrowed(&[0; 4])],
        saved: vec![record],
        events,
        backing: &[],
        gaps: vec![],
    };
    layout.encode(false, &mut || true).unwrap()
}

#[test]
fn middle_clip_preserves_frames_and_distinct_live_and_destroyed_records() {
    let source = fixture();
    let (original, _) = crate::native::read_with_progress(&source, |_, _| true).unwrap();
    let mut bytes = Vec::new();
    let report = trim_scene_frames(&source, 1, 2, &mut bytes, || true).unwrap();
    assert_eq!((report.source_frames, report.frames), (3, 2));
    let layout = Layout::parse(&bytes, &mut || true).unwrap();
    assert_eq!(
        layout.saved.iter().filter(|record| record.id == 2).count(),
        2
    );
    assert_eq!(
        layout.saved.iter().filter(|record| record.deleted).count(),
        1
    );
    assert_ne!(
        layout.saved[0].position,
        layout.saved.last().unwrap().position
    );
    let mut copy = Vec::new();
    rewrite(&bytes, &mut copy, || true).unwrap();
    assert_eq!(copy, bytes);
    let (restored, _) = crate::native::read_with_progress(&bytes, |_, _| true).unwrap();
    for index in [1, 0, 1, 0] {
        assert_eq!(
            restored.state_at(index).unwrap(),
            original.state_at(index + 1).unwrap()
        );
    }
}

#[test]
fn mixed_undo_restores_baseline_before_retained_message_fields() {
    let source = fixture();
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    if let HistoryEvent::Set { undo, .. } = &mut layout.events[13] {
        *undo = vec![9, 2, 1];
    }
    layout.trim_middle(1, 2, &mut || true).unwrap();
    let HistoryEvent::Set { undo, .. } = &layout.events[4] else {
        panic!()
    };
    assert_eq!(undo, &[4, 2, 1]);
}

#[test]
fn middle_clip_rejects_unknown_baseline_fields_and_wrong_undo_without_output() {
    let source = fixture();
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    layout.events[4] = HistoryEvent::Marker;
    layout.metadata.strings.insert(3, "CustomU32".into());
    assert!(
        layout
            .trim_middle(1, 2, &mut || true)
            .unwrap_err()
            .to_string()
            .contains("unrecorded property")
    );
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    if let HistoryEvent::Set { undo, .. } = &mut layout.events[16] {
        *undo = vec![5];
    }
    assert!(
        layout
            .trim_middle(1, 2, &mut || true)
            .unwrap_err()
            .to_string()
            .contains("undo disagrees")
    );
}

#[test]
fn array_undo_rebases_to_snapshot_and_rejects_unordered_references() {
    let mut events = vec![HistoryEvent::Array {
        kind: 6,
        id: 2,
        property: 2,
        index: 0,
        data: None,
        undo: vec![1],
    }];
    rebase_undo(&mut events, 10, &[HistoryEvent::Marker], &mut || true).unwrap();
    let HistoryEvent::Array { undo, .. } = &events[0] else {
        panic!()
    };
    assert_eq!(undo, &[1]);
    let mut events = vec![HistoryEvent::Set {
        kind: 4,
        id: 2,
        property: 0,
        data: Cow::Borrowed(&[]),
        undo: vec![1, 2],
    }];
    assert!(
        rebase_undo(&mut events, 10, &[HistoryEvent::Marker], &mut || true)
            .unwrap_err()
            .to_string()
            .contains("ordered distinct")
    );
}

#[test]
fn snapshot_records_outside_deleted_directory_reject_conflicting_ids_and_truncation() {
    let source = fixture();
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    layout.trim_middle(1, 2, &mut || true).unwrap();
    let bytes = layout.encode(false, &mut || true).unwrap();
    let parsed = Layout::parse(&bytes, &mut || true).unwrap();
    let base = parsed.backing.as_ptr() as usize - bytes.as_ptr() as usize;
    let record = parsed
        .saved
        .iter()
        .find(|record| !record.deleted && record.id == 2)
        .unwrap();
    let offset = base + record.position as usize + 4;
    let mut invalid = bytes.clone();
    invalid[offset..offset + 4].copy_from_slice(&99u32.to_le_bytes());
    assert!(Layout::parse(&invalid, &mut || true).is_err());
    invalid = bytes;
    invalid[offset + 4..offset + 8].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(Layout::parse(&invalid, &mut || true).is_err());
}

#[test]
fn snapshot_padding_and_array_message_fields_are_rejected_before_copying() {
    let mut metadata = fixture_metadata();
    let mut instance = RawInstance::create(&metadata, 3).unwrap();
    instance.defined.extend([0, 1, 2, 3]);
    metadata.classes[3].as_mut().unwrap().size = MAX_METADATA;
    assert!(
        instance
            .body(&metadata)
            .unwrap_err()
            .to_string()
            .contains("budget")
    );

    let mut metadata = fixture_metadata();
    metadata.properties.push(Some(Property {
        owner: name(1, 5),
        name: 7,
        datatype: name(1, 3),
        datatype_id: 1,
        kind: 2,
        semantic: 0,
        offset32: 24,
        values: Vec::new(),
    }));
    let class = metadata.classes[3].as_mut().unwrap();
    class.properties.push(4);
    class.size = 36;
    metadata.messages[0] = Some(Message {
        class_name: name(1, 5),
        class: 3,
        size: 4,
        entries: vec![
            MessageEntry {
                datatype: name(1, 3),
                datatype_id: 1,
                property: 2,
                offset: 0,
                size: 4,
                destination_size: 4,
            },
            MessageEntry {
                datatype: name(1, 3),
                datatype_id: 1,
                property: 4,
                offset: 0,
                size: 4,
                destination_size: 4,
            },
        ],
    });
    let mut state = BTreeMap::from([(2, RawInstance::create(&metadata, 3).unwrap())]);
    let before = state.clone();
    let error = raw_apply(
        &metadata,
        &mut state,
        &HistoryEvent::Set {
            kind: 4,
            id: 2,
            property: 0,
            data: Cow::Borrowed(&[1; 4]),
            undo: Vec::new(),
        },
        &mut (MAX_METADATA - 6),
        &mut || true,
    )
    .unwrap_err();
    assert!(error.to_string().contains("array-target"));
    assert_eq!(state, before);
}

#[test]
fn message_string_tail_must_match_recorded_handles() {
    let mut metadata = fixture_metadata();
    metadata.strings.insert(3, "StringHandle".into());
    metadata.classes[1].as_mut().unwrap().string_offsets = vec![0];
    let mut payload = [6u32.to_le_bytes(), 7u32.to_le_bytes()].concat();
    payload.extend_from_slice(b"Value\0Other\0");
    validate_message(&metadata, 0, &payload, &mut || true).unwrap();
    let mut state = BTreeMap::from([(2, RawInstance::create(&metadata, 3).unwrap())]);
    let mut budget = state[&2].bytes();
    raw_apply(
        &metadata,
        &mut state,
        &HistoryEvent::Set {
            kind: 4,
            id: 2,
            property: 0,
            data: Cow::Borrowed(&payload),
            undo: Vec::new(),
        },
        &mut budget,
        &mut || true,
    )
    .unwrap();
    assert_eq!(state[&2].values[&0], 6u32.to_le_bytes());
    assert_eq!(state[&2].values[&1], 7u32.to_le_bytes());
    payload.push(0);
    assert!(
        validate_message(&metadata, 0, &payload, &mut || true)
            .unwrap_err()
            .to_string()
            .contains("trailing data")
    );
    payload.pop();
    payload[8] = b'X';
    assert!(
        validate_message(&metadata, 0, &payload, &mut || true)
            .unwrap_err()
            .to_string()
            .contains("string tail")
    );
}

#[test]
fn native_create_initial_values_are_typed_bounded_and_cancelable() {
    let metadata = fixture_metadata();
    let initial = snapshot::initial_properties(&metadata, &mut || true).unwrap();
    let mut builder = crate::RecordingBuilder::new(pvd_protocol::Header {
        version: 1,
        timestamp_numerator: 1,
        timestamp_denominator: 1,
        flags: 0,
        endian: pvd_protocol::Endian::Little,
    });
    metadata.register(&mut builder).unwrap();
    crate::native::push(
        &mut builder,
        pvd_protocol::Event::CreateInstance {
            class: name(1, 5),
            id: 2,
        },
    )
    .unwrap();
    initial
        .restore_instance(&metadata, &mut builder, 2, &mut || true)
        .unwrap();
    let properties = &builder.state.objects[&2].properties;
    assert_eq!(properties["Value"], crate::Value::U64(0));
    assert_eq!(properties["Other"], crate::Value::U64(0));
    assert_eq!(properties["Items"], crate::Value::Array(Vec::new().into()));
    assert_eq!(properties["Reference"], crate::Value::Ref(0));
    assert!(snapshot::initial_properties(&metadata, &mut || false).is_err());
    assert!(
        initial
            .restore_instance(&metadata, &mut builder, 2, &mut || false)
            .is_err()
    );

    let mut metadata = fixture_metadata();
    metadata.classes[3].as_mut().unwrap().size = 23;
    assert!(
        snapshot::initial_properties(&metadata, &mut || true)
            .err()
            .unwrap()
            .to_string()
            .contains("fixed class layout")
    );
    let mut metadata = fixture_metadata();
    metadata.strings.insert(1, "custom".into());
    assert!(
        snapshot::initial_value(&metadata, metadata.property(0).unwrap())
            .unwrap()
            .is_none()
    );
    let mut metadata = fixture_metadata();
    metadata.classes[1].as_mut().unwrap().size = 8;
    assert!(
        snapshot::initial_value(&metadata, metadata.property(0).unwrap())
            .unwrap()
            .is_none()
    );
    let mut metadata = fixture_metadata();
    metadata.properties[0].as_mut().unwrap().datatype = name(1, 2);
    assert!(
        snapshot::initial_value(&metadata, metadata.property(0).unwrap())
            .unwrap()
            .is_none()
    );
}

fn proof_fields(
    source: &[u8],
    id: u32,
    class: u32,
) -> Result<Option<BTreeMap<u32, Vec<u8>>>, Error> {
    let layout = Layout::parse(source, &mut || true)?;
    let mut input = read_source::Source::new(std::io::Cursor::new(layout.backing))?;
    let pages = layout
        .pages
        .iter()
        .map(|page| (page.first, page.position, page.count, page.len))
        .collect::<Vec<_>>();
    let saved = layout
        .saved
        .iter()
        .filter(|record| record.deleted)
        .map(|record| {
            (
                record.position,
                (record.id, (record.body.len() + 12) as u32),
            )
        })
        .collect();
    let proof = snapshot::proven_initial_properties(
        &layout.metadata,
        &mut input,
        0..layout.backing.len() as u64,
        &pages,
        &saved,
        &mut || true,
    )?;
    Ok(proof.fields(id, class).cloned())
}

fn encode_modified(layout: &mut Layout<'_>) -> Vec<u8> {
    assert_eq!(layout.pages.len(), 1);
    layout.pages[0].count = layout.events.len() as u32;
    layout.pages[0].len = encode_page(&layout.events, &mut || true).unwrap().len() as u32;
    layout.encode(false, &mut || true).unwrap()
}

#[test]
fn saved_body_proof_excludes_message_writes_and_reused_lifetimes() {
    let source = fixture();
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    layout.events[5] = HistoryEvent::Marker;
    let bytes = encode_modified(&mut layout);
    let fields = proof_fields(&bytes, 2, 3).unwrap().unwrap();
    assert_eq!(fields, BTreeMap::from([(2, Vec::new())]));

    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    layout.events[4] = HistoryEvent::Marker;
    layout.events[5] = HistoryEvent::Marker;
    let bytes = encode_modified(&mut layout);
    let fields = proof_fields(&bytes, 2, 3).unwrap().unwrap();
    assert!(!fields.contains_key(&1));
    assert_eq!(fields[&2], Vec::<u8>::new());

    let mut layout = Layout::parse(&bytes, &mut || true).unwrap();
    layout.events.push(HistoryEvent::Create(2, 3));
    let bytes = encode_modified(&mut layout);
    assert!(proof_fields(&bytes, 2, 3).unwrap().is_none());
}

#[test]
fn saved_body_proof_rejects_unwritten_nonzero_and_ambiguous_versions() {
    let source = fixture();
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    layout.events[6] = HistoryEvent::Marker;
    let bytes = encode_modified(&mut layout);
    assert!(
        proof_fields(&bytes, 2, 3)
            .unwrap_err()
            .to_string()
            .contains("contradicts native Create")
    );

    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    layout.events[5] = HistoryEvent::Marker;
    let single = encode_modified(&mut layout);
    assert!(proof_fields(&single, 2, 3).unwrap().is_some());
    let verified = Layout::parse(&single, &mut || true).unwrap();
    let mut version = layout.saved[0].clone();
    version.position = 128;
    layout.saved.push(version);
    let bytes = encode_modified(&mut layout);
    assert!(
        Layout::parse(&bytes, &mut || true)
            .err()
            .unwrap()
            .to_string()
            .contains("directory mismatch or duplicate")
    );
    let mut backing = verified.backing.to_vec();
    let first = &verified.saved[0];
    let record_length = first.body.len() + 12;
    backing.copy_within(
        first.position as usize..first.position as usize + record_length,
        128,
    );
    let pages = verified
        .pages
        .iter()
        .map(|page| (page.first, page.position, page.count, page.len))
        .collect::<Vec<_>>();
    let saved = BTreeMap::from([
        (first.position, (first.id, record_length as u32)),
        (128, (first.id, record_length as u32)),
    ]);
    let mut input = read_source::Source::new(std::io::Cursor::new(&backing)).unwrap();
    let proof = snapshot::proven_initial_properties(
        &verified.metadata,
        &mut input,
        0..backing.len() as u64,
        &pages,
        &saved,
        &mut || true,
    )
    .unwrap();
    assert!(proof.fields(2, 3).is_none());
}

#[test]
fn prefix_and_middle_reject_saved_values_inconsistent_with_native_initialization() {
    let source = fixture();
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    layout.events[6] = HistoryEvent::Marker;
    let bytes = encode_modified(&mut layout);
    for first in [0, 1] {
        let mut output = Vec::new();
        let reason = trim_scene_frames(&bytes, first, 2, &mut output, || true)
            .unwrap_err()
            .to_string();
        assert!(
            reason.contains("disagrees with state") || reason.contains("contradicts native Create"),
            "{reason}"
        );
        assert!(output.is_empty());
    }
}

#[test]
fn array_snapshot_undo_requires_exact_whole_property_before_edit() {
    let metadata = fixture_metadata();
    let mut body = RawInstance::create(&metadata, 3).unwrap();
    body.set(
        &metadata,
        2,
        &[10u32.to_le_bytes(), 11u32.to_le_bytes()].concat(),
    )
    .unwrap();
    let baseline = BTreeMap::from([(2, body)]);
    let mut events = vec![
        HistoryEvent::Marker,
        HistoryEvent::Array {
            kind: 6,
            id: 2,
            property: 2,
            index: 1,
            data: None,
            undo: vec![1],
        },
    ];
    verify_undo(&metadata, &baseline, &events, &[], &mut || true).unwrap();
    let value = 12u32.to_le_bytes();
    events[1] = HistoryEvent::Array {
        kind: 7,
        id: 2,
        property: 2,
        index: 1,
        data: Some(&value),
        undo: vec![1],
    };
    verify_undo(&metadata, &baseline, &events, &[], &mut || true).unwrap();
    events.insert(
        1,
        HistoryEvent::Array {
            kind: 5,
            id: 2,
            property: 2,
            index: 0,
            data: Some(&value),
            undo: Vec::new(),
        },
    );
    if let HistoryEvent::Array { undo, .. } = &mut events[2] {
        *undo = vec![2];
    }
    assert!(
        verify_undo(&metadata, &baseline, &events, &[], &mut || true)
            .unwrap_err()
            .to_string()
            .contains("undo disagrees")
    );
}

fn array_middle_fixture(kind: u32, index: u32) -> Vec<u8> {
    let source = fixture();
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    let values = [10u32, 11, 12]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    let replacement = 99u32.to_le_bytes();
    let encoded = [
        9u32.to_le_bytes().to_vec(),
        [1u32, 1, 0]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect(),
        [1u32, 2, 3]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect(),
        section(100, true),
        section(101, false),
        section(200, true),
        set(3, 2, &values, &[4]),
    ];
    let mut events = encoded
        .iter()
        .enumerate()
        .map(|(ordinal, bytes)| {
            HistoryEvent::parse(bytes, ordinal as u64, &layout.metadata, &mut 0).unwrap()
        })
        .collect::<Vec<_>>();
    events.push(HistoryEvent::Array {
        kind,
        id: 2,
        property: 2,
        index,
        data: (kind == 7).then_some(replacement.as_slice()),
        undo: vec![1],
    });
    let tail = [
        section(201, false),
        section(300, true),
        set(3, 0, &42u32.to_le_bytes(), &[8]),
        section(301, false),
    ];
    for bytes in &tail {
        events.push(
            HistoryEvent::parse(bytes, events.len() as u64, &layout.metadata, &mut 0).unwrap(),
        );
    }
    layout.events = events;
    layout.saved.clear();
    layout.communication.reverse = vec![(0x111, 1), (0x222, 2)];
    layout.communication.sections[0].rows =
        vec![[100, 3, 101, 4], [200, 5, 201, 8], [300, 9, 301, 11]];
    encode_modified(&mut layout)
}

#[test]
fn middle_array_undo_preserves_retained_whole_property_sources_at_each_index() {
    for kind in [6, 7] {
        for index in 0..3 {
            let source = array_middle_fixture(kind, index);
            let (original, _) = crate::native::read_with_progress(&source, |_, _| true).unwrap();
            let mut output = Vec::new();
            trim_scene_frames(&source, 1, 2, &mut output, || true).unwrap();
            let (restored, _) = crate::native::read_with_progress(&output, |_, _| true).unwrap();
            assert_eq!(restored.frame_count(), 2);
            let mut expected = vec![10u64, 11, 12];
            if kind == 6 {
                expected.remove(index as usize);
            } else {
                expected[index as usize] = 99;
            }
            let expected = crate::Value::Array(
                expected
                    .into_iter()
                    .map(crate::Value::U64)
                    .collect::<Vec<_>>()
                    .into(),
            );
            for frame in [0, 1, 0, 1] {
                let actual = restored.state_at(frame).unwrap();
                assert_eq!(actual, original.state_at(frame + 1).unwrap());
                assert_eq!(actual.objects[&2].properties["Items"], expected);
                assert_eq!(
                    actual.objects[&2].properties["Value"],
                    crate::Value::U64(if frame == 0 { 0 } else { 42 })
                );
            }
            let layout = Layout::parse(&output, &mut || true).unwrap();
            let current = layout
                .events
                .iter()
                .position(|event| matches!(event, HistoryEvent::Array { kind: 6 | 7, .. }))
                .unwrap();
            let HistoryEvent::Array { undo, .. } = &layout.events[current] else {
                unreachable!();
            };
            assert_eq!(undo.as_slice(), &[1]);
            assert!(matches!(
                &layout.events[current - 1],
                HistoryEvent::Set { kind: 3, id: 2, property: 2, data, .. }
                    if data.as_ref() == [10u32.to_le_bytes(), 11u32.to_le_bytes(), 12u32.to_le_bytes()].concat()
            ));
            if let Some(directory) = std::env::var_os("PVD_NATIVE_CONSTRUCTED_EXPORT_DIRECTORY") {
                let directory = std::path::PathBuf::from(directory);
                std::fs::create_dir_all(&directory).unwrap();
                let stem = format!("array-whole-source-{kind}-{index}");
                std::fs::write(directory.join(format!("{stem}-source.pxd2")), &source).unwrap();
                std::fs::write(directory.join(format!("{stem}-trimmed.pxd2")), &output).unwrap();
            }
        }
    }
}

#[test]
fn array_undo_rejects_a_complete_source_when_another_element_changed() {
    let metadata = fixture_metadata();
    let values = [
        10u32.to_le_bytes(),
        11u32.to_le_bytes(),
        12u32.to_le_bytes(),
    ]
    .concat();
    let changed = 99u32.to_le_bytes();
    let replacement = 88u32.to_le_bytes();
    let baseline = BTreeMap::from([(2, RawInstance::create(&metadata, 3).unwrap())]);
    for kind in [6, 7] {
        let events = vec![
            HistoryEvent::Marker,
            HistoryEvent::Set {
                kind: 3,
                id: 2,
                property: 2,
                data: values.as_slice().into(),
                undo: vec![1],
            },
            HistoryEvent::Array {
                kind: 7,
                id: 2,
                property: 2,
                index: 0,
                data: Some(&changed),
                undo: vec![1],
            },
            HistoryEvent::Array {
                kind,
                id: 2,
                property: 2,
                index: 2,
                data: (kind == 7).then_some(replacement.as_slice()),
                undo: vec![2],
            },
        ];
        let failure = verify_undo(&metadata, &baseline, &events, &[], &mut || true).unwrap_err();
        assert!(failure.to_string().contains("undo disagrees"), "{failure}");
    }
}

#[test]
fn array_undo_rejects_complete_and_element_sources_from_other_objects_or_properties() {
    let mut metadata = fixture_metadata();
    metadata.strings.insert(13, "OtherItems".into());
    metadata.properties.push(Some(Property {
        owner: name(1, 5),
        name: 13,
        datatype: name(1, 3),
        datatype_id: 1,
        kind: 2,
        semantic: 0,
        offset32: 24,
        values: vec![],
    }));
    let class = metadata.classes[3].as_mut().unwrap();
    class.size = 36;
    class.properties.push(4);
    let values = [10u32.to_le_bytes(), 11u32.to_le_bytes()].concat();
    let mut body = RawInstance::create(&metadata, 3).unwrap();
    for property in [2, 4] {
        body.set(&metadata, property, &values).unwrap();
    }
    let baseline = BTreeMap::from([(2, body.clone()), (3, body)]);
    for (id, property, undo) in [(3, 2, vec![1]), (2, 4, vec![1])] {
        let events = vec![
            HistoryEvent::Marker,
            HistoryEvent::Set {
                kind: 3,
                id: 2,
                property: 2,
                data: values.as_slice().into(),
                undo: vec![1],
            },
            HistoryEvent::Array {
                kind: 6,
                id,
                property,
                index: 1,
                data: None,
                undo,
            },
        ];
        let failure = verify_undo(&metadata, &baseline, &events, &[], &mut || true).unwrap_err();
        assert!(!failure.to_string().is_empty());
    }
    let element = 20u32.to_le_bytes();
    let replacement = 99u32.to_le_bytes();
    for kind in [5, 7] {
        for (id, property) in [(3, 2), (2, 4)] {
            let events = vec![
                HistoryEvent::Marker,
                HistoryEvent::Array {
                    kind,
                    id: 2,
                    property: 2,
                    index: 1,
                    data: Some(&element),
                    undo: if kind == 7 { vec![1] } else { vec![] },
                },
                HistoryEvent::Array {
                    kind: 7,
                    id,
                    property,
                    index: 1,
                    data: Some(&replacement),
                    undo: vec![1],
                },
            ];
            assert!(verify_undo(&metadata, &baseline, &events, &[], &mut || true).is_err());
        }
    }
}

struct ArrayChainStep {
    kind: u32,
    index: u32,
    values: Vec<u32>,
    sources: Vec<usize>,
}

fn array_step(kind: u32, index: u32, values: &[u32], sources: &[usize]) -> ArrayChainStep {
    ArrayChainStep {
        kind,
        index,
        values: values.to_vec(),
        sources: sources.to_vec(),
    }
}

fn array_chain_fixture(initial: &[u32], frames: &[Vec<ArrayChainStep>]) -> Vec<u8> {
    array_chain_fixture_with_metadata(initial, frames, fixture_metadata())
}

fn array_chain_fixture_with_metadata(
    initial: &[u32],
    frames: &[Vec<ArrayChainStep>],
    metadata: Metadata,
) -> Vec<u8> {
    let source = fixture();
    let object_bytes = metadata_bytes(&metadata);
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    layout.object_bytes = &object_bytes;
    layout.pointer_layouts = pointer_layouts(&object_bytes, &mut 0).unwrap();
    layout.metadata = metadata;
    let initial = initial
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect::<Vec<_>>();
    let mut encoded = vec![
        9u32.to_le_bytes().to_vec(),
        [1u32, 1, 0]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect(),
        [1u32, 2, 3]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect(),
        set(3, 2, &initial, &[1]),
    ];
    let mut operations = vec![3usize];
    let mut rows = Vec::new();
    for (frame, steps) in frames.iter().enumerate() {
        let ticks = 100 + frame as u64 * 100;
        let begin = encoded.len() as u64;
        encoded.push(section(ticks, true));
        for step in steps {
            let ordinal = encoded.len();
            let undo = step
                .sources
                .iter()
                .map(|&source| (ordinal - operations[source]) as u32)
                .collect::<Vec<_>>();
            let data = step
                .values
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect::<Vec<_>>();
            let event = if matches!(step.kind, 3 | 4) {
                HistoryEvent::Set {
                    kind: step.kind,
                    id: 2,
                    property: if step.kind == 4 { step.index } else { 2 },
                    data: data.as_slice().into(),
                    undo,
                }
            } else {
                HistoryEvent::Array {
                    kind: step.kind,
                    id: 2,
                    property: 2,
                    index: step.index,
                    data: (step.kind != 6).then_some(data.as_slice()),
                    undo,
                }
            };
            let mut bytes = Vec::new();
            event.encode(&mut bytes).unwrap();
            encoded.push(bytes);
            operations.push(ordinal);
        }
        let end = encoded.len() as u64;
        encoded.push(section(ticks + 1, false));
        rows.push([ticks, begin, ticks + 1, end]);
    }
    layout.events = encoded
        .iter()
        .enumerate()
        .map(|(ordinal, bytes)| {
            HistoryEvent::parse(bytes, ordinal as u64, &layout.metadata, &mut 0).unwrap()
        })
        .collect();
    layout.saved.clear();
    layout.communication.reverse = vec![(0x111, 1), (0x222, 2)];
    layout.communication.sections[0].rows = rows;
    encode_modified(&mut layout)
}

fn check_array_chain(
    label: &str,
    initial: &[u32],
    frames: &[Vec<ArrayChainStep>],
    expected: &[&[u64]],
    undo_targets: &[(usize, &[usize])],
) {
    check_array_chain_with_metadata(
        label,
        initial,
        frames,
        expected,
        undo_targets,
        fixture_metadata(),
    );
}

fn check_array_chain_with_metadata(
    label: &str,
    initial: &[u32],
    frames: &[Vec<ArrayChainStep>],
    expected: &[&[u64]],
    undo_targets: &[(usize, &[usize])],
    metadata: Metadata,
) -> Vec<u8> {
    let source = array_chain_fixture_with_metadata(initial, frames, metadata);
    let (original, _) = crate::native::read_with_progress(&source, |_, _| true).unwrap();
    let mut output = Vec::new();
    trim_scene_frames(&source, 1, frames.len() - 1, &mut output, || true).unwrap();
    let (restored, _) = crate::native::read_with_progress(&output, |_, _| true).unwrap();
    assert_eq!(restored.frame_count(), expected.len());
    for frame in (0..expected.len()).chain((0..expected.len()).rev()).chain([
        expected.len() - 1,
        0,
        expected.len() / 2,
        0,
    ]) {
        let actual = restored.state_at(frame).unwrap();
        assert_eq!(
            actual,
            original.state_at(frame + 1).unwrap(),
            "{label} frame {frame}"
        );
        assert_eq!(
            actual.objects[&2].properties["Items"],
            crate::Value::Array(
                expected[frame]
                    .iter()
                    .copied()
                    .map(crate::Value::U64)
                    .collect::<Vec<_>>()
                    .into()
            ),
            "{label} frame {frame}"
        );
    }
    let layout = Layout::parse(&output, &mut || true).unwrap();
    for &(current, expected_targets) in undo_targets {
        let undo = match &layout.events[current] {
            HistoryEvent::Set { undo, .. } | HistoryEvent::Array { undo, .. } => undo,
            event => panic!("{label} expected an undo event at {current}: {event:?}"),
        };
        let actual = undo
            .iter()
            .map(|&distance| current - distance as usize)
            .collect::<Vec<_>>();
        assert_eq!(
            actual.as_slice(),
            expected_targets,
            "{label} event {current}"
        );
    }
    let mut copy = Vec::new();
    rewrite(&output, &mut copy, || true).unwrap();
    assert_eq!(copy, output);
    if let Some(directory) = std::env::var_os("PVD_NATIVE_CONSTRUCTED_EXPORT_DIRECTORY") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join(format!("array-chain-{label}-source.pxd2")),
            &source,
        )
        .unwrap();
        std::fs::write(
            directory.join(format!("array-chain-{label}-trimmed.pxd2")),
            &output,
        )
        .unwrap();
    }
    output
}

#[test]
fn middle_array_element_sources_preserve_indices_after_insertions_and_removals() {
    check_array_chain(
        "indices",
        &[],
        &[
            vec![],
            vec![
                array_step(5, 0, &[10], &[]),
                array_step(5, 1, &[20], &[]),
                array_step(5, 2, &[30], &[]),
            ],
            vec![array_step(5, 1, &[15], &[]), array_step(7, 2, &[22], &[2])],
            vec![array_step(6, 1, &[], &[4])],
            vec![array_step(6, 0, &[], &[1])],
        ],
        &[&[10, 20, 30], &[10, 15, 22, 30], &[10, 22, 30], &[22, 30]],
        &[(8, &[3]), (11, &[7]), (14, &[2])],
    );
}

#[test]
fn middle_array_consecutive_updates_use_the_captured_previous_element() {
    check_array_chain(
        "updates",
        &[10, 20],
        &[
            vec![],
            vec![array_step(7, 0, &[11], &[0])],
            vec![array_step(7, 0, &[12], &[1])],
            vec![array_step(7, 0, &[13], &[2])],
        ],
        &[&[11, 20], &[12, 20], &[13, 20]],
        &[(2, &[0]), (5, &[2]), (8, &[5])],
    );
}

#[test]
fn middle_array_whole_assignment_restores_snapshot_then_retained_insertions() {
    check_array_chain(
        "snapshot-insert",
        &[10, 20],
        &[
            vec![],
            vec![array_step(5, 2, &[30], &[])],
            vec![array_step(3, 0, &[90], &[0, 1])],
            vec![array_step(3, 0, &[91, 92], &[2])],
        ],
        &[&[10, 20, 30], &[90], &[91, 92]],
        &[(5, &[0, 2]), (8, &[5])],
    );
    check_array_chain(
        "snapshot-whole",
        &[10, 20],
        &[
            vec![],
            vec![array_step(3, 0, &[11, 20], &[0])],
            vec![array_step(3, 0, &[90], &[0, 1])],
        ],
        &[&[11, 20], &[90]],
        &[(2, &[0]), (5, &[0, 2])],
    );
}

#[test]
fn middle_array_whole_assignment_can_restore_from_insertions_without_a_snapshot() {
    check_array_chain(
        "inserts",
        &[],
        &[
            vec![],
            vec![array_step(5, 0, &[10], &[]), array_step(5, 1, &[20], &[])],
            vec![array_step(3, 0, &[], &[1, 2])],
            vec![array_step(3, 0, &[50], &[3])],
        ],
        &[&[10, 20], &[], &[50]],
        &[(6, &[2, 3]), (9, &[6])],
    );
}

#[test]
fn middle_array_undo_uses_native_append_and_out_of_range_update_semantics() {
    check_array_chain(
        "append",
        &[],
        &[
            vec![],
            vec![
                array_step(5, 0, &[10], &[]),
                array_step(5, 1, &[20], &[]),
                array_step(5, 2, &[30], &[]),
                array_step(6, 0, &[], &[1]),
                array_step(6, 0, &[], &[2]),
            ],
            vec![array_step(3, 0, &[], &[3])],
        ],
        &[&[30], &[]],
        &[(9, &[4])],
    );
    check_array_chain(
        "update-noop",
        &[10, 20],
        &[
            vec![],
            vec![array_step(7, 1, &[21], &[0])],
            vec![array_step(6, 1, &[], &[0, 1])],
            vec![array_step(3, 0, &[10], &[1])],
        ],
        &[&[10, 21], &[10], &[10]],
        &[(2, &[0]), (5, &[0, 2]), (8, &[2])],
    );
}

fn reject_array_chain(source: &[u8]) {
    let (recording, _) = crate::native::read_with_progress(source, |_, _| true).unwrap();
    let mut output = Vec::new();
    assert!(
        trim_scene_frames(source, 1, recording.frame_count() - 1, &mut output, || true).is_err()
    );
    assert!(output.is_empty());
}

#[test]
fn array_undo_rejects_nonempty_whole_assignment_tails_and_stale_elements() {
    let source = array_chain_fixture(
        &[],
        &[
            vec![],
            vec![array_step(5, 0, &[10], &[]), array_step(5, 1, &[20], &[])],
            vec![array_step(3, 0, &[90], &[1, 2])],
        ],
    );
    reject_array_chain(&source);
    let source = array_chain_fixture(
        &[10, 20],
        &[
            vec![],
            vec![
                array_step(3, 0, &[11, 20], &[0]),
                array_step(7, 0, &[12], &[1]),
            ],
            vec![array_step(3, 0, &[90], &[0, 1])],
        ],
    );
    reject_array_chain(&source);
    let source = array_chain_fixture(
        &[10, 20],
        &[
            vec![],
            vec![array_step(7, 0, &[11], &[0]), array_step(7, 0, &[12], &[1])],
            vec![array_step(7, 0, &[13], &[1])],
        ],
    );
    reject_array_chain(&source);
}

#[test]
fn array_undo_does_not_insert_for_previous_updates_or_recurse_through_removals() {
    let source = array_chain_fixture(
        &[10, 20],
        &[
            vec![],
            vec![array_step(7, 0, &[11], &[0])],
            vec![array_step(6, 0, &[], &[1])],
        ],
    );
    reject_array_chain(&source);
    let source = array_chain_fixture(
        &[],
        &[
            vec![],
            vec![
                array_step(5, 0, &[10], &[]),
                array_step(5, 1, &[10], &[]),
                array_step(6, 1, &[], &[2]),
            ],
            vec![array_step(7, 0, &[99], &[3])],
        ],
    );
    reject_array_chain(&source);
}

#[test]
fn array_undo_rejects_reordered_duplicate_self_and_future_references() {
    for sources in [vec![2, 1], vec![1, 1]] {
        let source = array_chain_fixture(
            &[],
            &[
                vec![],
                vec![array_step(5, 0, &[10], &[]), array_step(5, 1, &[20], &[])],
                vec![array_step(3, 0, &[], &sources)],
            ],
        );
        reject_array_chain(&source);
    }
    let source = array_chain_fixture(&[10], &[vec![], vec![array_step(7, 0, &[11], &[0])]]);
    for distance in [0, u32::MAX] {
        let mut layout = Layout::parse(&source, &mut || true).unwrap();
        let HistoryEvent::Array { undo, .. } = &mut layout.events[7] else {
            unreachable!();
        };
        *undo = vec![distance];
        let malformed = encode_modified(&mut layout);
        let mut output = Vec::new();
        assert!(trim_scene_frames(&malformed, 1, 1, &mut output, || true).is_err());
        assert!(output.is_empty());
    }
}

#[test]
fn array_chain_rejects_invalid_forward_indices_and_element_widths_before_output() {
    let source = array_chain_fixture(&[10], &[vec![], vec![array_step(7, 0, &[11], &[0])]]);
    let invalid_width = [0u8; 3];
    for change in 0..2 {
        let mut layout = Layout::parse(&source, &mut || true).unwrap();
        let HistoryEvent::Array { index, data, .. } = &mut layout.events[7] else {
            unreachable!();
        };
        if change == 0 {
            *index = u32::MAX;
        } else {
            *data = Some(&invalid_width);
        }
        let malformed = encode_modified(&mut layout);
        let mut output = Vec::new();
        assert!(trim_scene_frames(&malformed, 1, 1, &mut output, || true).is_err());
        assert!(output.is_empty());
    }
    let source = array_chain_fixture(&[10], &[vec![], vec![array_step(5, 2, &[11], &[])]]);
    let mut output = Vec::new();
    assert!(trim_scene_frames(&source, 1, 1, &mut output, || true).is_err());
    assert!(output.is_empty());
}

#[test]
fn array_undo_property_messages_require_an_independent_array_conversion_contract() {
    let mut metadata = fixture_metadata();
    metadata.messages[0] = Some(Message {
        class_name: name(1, 5),
        class: 3,
        size: 8,
        entries: vec![MessageEntry {
            datatype: name(1, 3),
            datatype_id: 1,
            property: 2,
            offset: 0,
            size: 8,
            destination_size: 8,
        }],
    });
    let values = [10u32.to_le_bytes(), 20u32.to_le_bytes()].concat();
    let mut body = RawInstance::create(&metadata, 3).unwrap();
    body.set(&metadata, 2, &values).unwrap();
    let baseline = BTreeMap::from([(2, body)]);
    let replacement = 11u32.to_le_bytes();
    let events = vec![
        HistoryEvent::Marker,
        HistoryEvent::Set {
            kind: 4,
            id: 2,
            property: 0,
            data: values.as_slice().into(),
            undo: vec![1],
        },
        HistoryEvent::Array {
            kind: 7,
            id: 2,
            property: 2,
            index: 0,
            data: Some(&replacement),
            undo: vec![1],
        },
    ];
    assert!(verify_undo(&metadata, &baseline, &events, &[], &mut || true).is_err());
}

#[test]
fn array_chain_cannot_restore_an_element_from_a_reused_local_instance_id() {
    let source = array_chain_fixture(
        &[10, 20],
        &[vec![], vec![array_step(7, 0, &[11], &[0])], vec![]],
    );
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    let values = [11u32.to_le_bytes(), 20u32.to_le_bytes()].concat();
    let replacement = 12u32.to_le_bytes();
    let events = vec![
        HistoryEvent::Destroy(2),
        HistoryEvent::Create(2, 3),
        HistoryEvent::Set {
            kind: 3,
            id: 2,
            property: 2,
            data: values.into(),
            undo: vec![1],
        },
        HistoryEvent::Array {
            kind: 7,
            id: 2,
            property: 2,
            index: 0,
            data: Some(&replacement),
            undo: vec![6],
        },
    ];
    layout.events.splice(10..10, events);
    layout.communication.sections[0].rows[2][3] += 4;
    let source = encode_modified(&mut layout);
    let (original, _) = crate::native::read_with_progress(&source, |_, _| true).unwrap();
    assert_eq!(
        original.state_at(2).unwrap().objects[&2].properties["Items"],
        crate::Value::Array(vec![crate::Value::U64(12), crate::Value::U64(20)].into())
    );
    let mut output = Vec::new();
    assert!(trim_scene_frames(&source, 1, 2, &mut output, || true).is_err());
    assert!(output.is_empty());
}

#[test]
fn array_middle_undo_observes_cancellation_and_raw_state_budget() {
    let metadata = fixture_metadata();
    let values = [10u32.to_le_bytes(), 11u32.to_le_bytes()].concat();
    let baseline = BTreeMap::from([(2, RawInstance::create(&metadata, 3).unwrap())]);
    let events = vec![
        HistoryEvent::Marker,
        HistoryEvent::Set {
            kind: 3,
            id: 2,
            property: 2,
            data: values.as_slice().into(),
            undo: vec![1],
        },
        HistoryEvent::Array {
            kind: 6,
            id: 2,
            property: 2,
            index: 1,
            data: None,
            undo: vec![1],
        },
    ];
    for stop in 1..=5 {
        let mut checks = 0;
        let failure = verify_undo(&metadata, &baseline, &events, &[], &mut || {
            checks += 1;
            checks < stop
        })
        .unwrap_err();
        assert!(failure.to_string().contains("cancelled"), "{failure}");
        assert_eq!(checks, stop);
    }

    let mut state = BTreeMap::from([(2, RawInstance::create(&metadata, 3).unwrap())]);
    let before = state.clone();
    let data = 10u32.to_le_bytes();
    let event = HistoryEvent::Array {
        kind: 5,
        id: 2,
        property: 2,
        index: 0,
        data: Some(&data),
        undo: vec![],
    };
    let mut budget = MAX_METADATA;
    let failure = raw_apply(&metadata, &mut state, &event, &mut budget, &mut || true).unwrap_err();
    assert!(failure.to_string().contains("budget"), "{failure}");
    assert_eq!(state, before);
}

fn history_fixture(kind: &str, before: &[u8], current: &[u8], later: &[u8]) -> Vec<u8> {
    let source = fixture();
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    for (id, value) in [
        (13, kind),
        (14, "PvdU8"),
        (15, "events"),
        (16, "Zone"),
        (17, "Event"),
    ] {
        layout.metadata.strings.insert(id, value.into());
    }
    let class = |name, size, properties, string_offsets: Vec<usize>| {
        Some(Class {
            name,
            base: u32::MAX,
            packing: crate::native::ClassPacking::default(),
            size,
            properties,
            dynamic: false,
            has_pointers: !string_offsets.is_empty(),
            string_offsets,
        })
    };
    layout
        .metadata
        .classes
        .push(class(name(1, 14), 1, vec![], vec![]));
    layout
        .metadata
        .classes
        .push(class(name(10, 13), 12, vec![4], vec![]));
    layout.metadata.properties.push(Some(Property {
        owner: name(10, 13),
        name: 15,
        datatype: name(1, 14),
        datatype_id: 5,
        kind: 2,
        semantic: 0,
        offset32: 0,
        values: vec![],
    }));
    layout.pointer_layouts.extend([vec![], vec![]]);
    let errors = kind == "UserCustomedEvent";
    if errors {
        layout.metadata.strings.insert(15, "Events".into());
        for (id, text) in [
            (18, "PvdErrorMessage"),
            (19, "StringHandle"),
            (20, "Code"),
            (21, "Message"),
            (22, "File"),
            (23, "Line"),
            (24, "Index"),
            (25, "Error"),
            (26, "Source.cpp"),
        ] {
            layout.metadata.strings.insert(id, text.into());
        }
        layout
            .metadata
            .classes
            .push(class(name(1, 19), 4, vec![], vec![]));
        layout
            .metadata
            .classes
            .push(class(name(10, 18), 20, (5..10).collect(), vec![4, 8]));
        let p = layout.metadata.properties[4].as_mut().unwrap();
        p.datatype = name(10, 18);
        p.datatype_id = 8;
        for (label, datatype, offset) in
            [(20, 1, 0), (21, 7, 4), (22, 7, 8), (23, 1, 12), (24, 1, 16)]
        {
            layout.metadata.properties.push(Some(Property {
                owner: name(10, 18),
                name: label,
                datatype: name(1, if datatype == 1 { 3 } else { 19 }),
                datatype_id: datatype,
                kind: 1,
                semantic: 0,
                offset32: offset,
                values: vec![],
            }));
        }
        layout
            .pointer_layouts
            .extend([vec![(2, 0)], vec![(2, 4), (2, 8)]]);
    }
    let object_bytes = metadata_bytes(&layout.metadata);
    layout.object_bytes = &object_bytes;
    layout.events = vec![HistoryEvent::Marker, HistoryEvent::Create(1, 0)];
    let mut previous = 0;
    let mut error_index = 0;
    fn append<'a>(
        events: &mut Vec<HistoryEvent<'a>>,
        previous: &mut usize,
        bytes: &'a [u8],
        error_index: &mut u32,
        errors: bool,
    ) {
        if errors {
            events.push(HistoryEvent::Array {
                kind: 5,
                id: 3,
                property: 4,
                index: *error_index,
                data: Some(bytes),
                undo: vec![],
            });
            *error_index += 1;
        } else {
            let ordinal = events.len();
            events.push(HistoryEvent::Set {
                kind: 3,
                id: 3,
                property: 4,
                data: bytes.into(),
                undo: vec![(ordinal - *previous) as u32],
            });
            *previous = ordinal;
        }
    }
    if !errors || !before.is_empty() {
        previous = layout.events.len();
        layout.events.push(HistoryEvent::Create(3, 6));
        append(
            &mut layout.events,
            &mut previous,
            before,
            &mut error_index,
            errors,
        );
    }
    let mut rows = vec![];
    for (frame, data) in [None, Some(current), Some(later)].into_iter().enumerate() {
        let ticks = 100 * (frame + 1) as u64;
        let begin = layout.events.len() as u64;
        layout.events.push(HistoryEvent::Section {
            class: 4,
            source: 0x111,
            ticks,
            name: 12,
            begin: 1,
            padding: [0; 3],
        });
        if let Some(data) = data {
            if errors && before.is_empty() && frame == 1 {
                layout.events.push(HistoryEvent::Create(3, 6));
            }
            if !errors || !data.is_empty() {
                append(
                    &mut layout.events,
                    &mut previous,
                    data,
                    &mut error_index,
                    errors,
                );
            }
        }
        let end = layout.events.len() as u64;
        layout.events.push(HistoryEvent::Section {
            class: 4,
            source: 0x111,
            ticks: ticks + 1,
            name: 12,
            begin: 0,
            padding: [0; 3],
        });
        rows.push([ticks, begin, ticks + 1, end]);
    }
    layout.communication.instances = vec![
        Instance {
            id: 1,
            source: 0x111,
            flags: [0; 10],
        },
        Instance {
            id: 3,
            source: 0x333,
            flags: [0; 10],
        },
    ];
    layout.communication.sections[0].rows = rows;
    if kind == "PxProfileZone" {
        layout.communication.zones = vec![Zone {
            source: 0x333,
            saved_word: 0,
            name: b"Zone\0",
            id: 3,
            events: vec![(b"Event\0", 1, 1)],
        }];
    }
    if kind == "PxProfileMemoryEventBuffer" {
        layout.communication.memory_instances[0] = 3;
    }
    layout.saved.clear();
    let page = encode_page(&layout.events, &mut || true).unwrap();
    layout.pages = vec![Page {
        first: 0,
        position: 4096,
        count: layout.events.len() as u32,
        len: page.len() as u32,
    }];
    layout.encode(false, &mut || true).unwrap()
}

fn profile_buffer(events: &[(u8, u64)]) -> Vec<u8> {
    let mut bytes = vec![];
    for &(kind, tick) in events {
        bytes.extend([kind, 3]);
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(7u32.to_le_bytes());
        bytes.extend([9, 0, 0]);
        bytes.extend(tick.to_le_bytes());
    }
    bytes
}

fn drawing(instance: Option<u64>) -> Vec<u8> {
    let mut bytes = vec![];
    if let Some(id) = instance {
        bytes.push(1);
        bytes.extend(id.to_le_bytes());
    }
    bytes.push(2);
    bytes.extend(1u32.to_le_bytes());
    for v in [1f32, 2., 3.] {
        bytes.extend(v.to_le_bytes());
    }
    bytes.extend(0xff112233u32.to_le_bytes());
    bytes
}

#[test]
fn middle_history_preserves_self_contained_profile_memory_and_drawing() {
    let start = profile_buffer(&[(1, 100)]);
    let stop = profile_buffer(&[(2, 120), (1, 130)]);
    let header = 2u16 | (3 << 4) | (2 << 6) | (2 << 8) | (2 << 10) | (2 << 12);
    let mut allocation = header.to_le_bytes().to_vec();
    for value in [64u32, 5, 6, 42] {
        allocation.extend(value.to_le_bytes());
    }
    allocation.extend(0x8000u64.to_le_bytes());
    let mut free = 0x33u16.to_le_bytes().to_vec();
    free.extend(0x8000u64.to_le_bytes());
    for (kind, before, current, later) in [
        ("PxProfileZone", vec![], start, stop),
        ("PxProfileMemoryEventBuffer", vec![], allocation, free),
        (
            "PvdUserRenderer",
            drawing(Some(1)),
            drawing(Some(3)),
            drawing(None),
        ),
    ] {
        let source = history_fixture(kind, &before, &current, &later);
        let (original, _) = crate::native::read_with_progress(&source, |_, _| true).unwrap();
        for last in [1, 2] {
            let mut output = vec![];
            trim_scene_frames(&source, 1, last, &mut output, || true).unwrap();
            let (restored, _) = crate::native::read_with_progress(&output, |_, _| true).unwrap();
            assert_eq!(restored.frame_count(), last);
            for frame in (0..last).chain((0..last).rev()) {
                assert_eq!(
                    restored.state_at(frame).unwrap(),
                    original.state_at(frame + 1).unwrap()
                );
            }
            let a = restored.analysis.legacy().unwrap();
            match kind {
                "PxProfileZone" => {
                    assert_eq!(a.spans.len(), usize::from(last == 2));
                    assert_eq!(a.open_spans(), 1);
                    if last == 2 {
                        assert_eq!(
                            (a.spans[0].start, a.spans[0].end, a.spans[0].received_frame),
                            (100, 120, 1)
                        );
                    }
                }
                "PxProfileMemoryEventBuffer" => {
                    assert_eq!(a.memory.events().len(), last);
                    assert_eq!(a.memory.live_bytes, if last == 1 { 64 } else { 0 });
                    assert_eq!(a.memory.samples.len(), last);
                    assert_eq!(a.memory.state_at_frame(0).live_bytes, 64);
                }
                _ => assert_eq!(
                    restored
                        .state_at(0)
                        .unwrap()
                        .debug_draw
                        .values()
                        .map(|p| p.len())
                        .sum::<usize>(),
                    1
                ),
            }
        }
    }
}

#[test]
fn middle_history_refuses_incomplete_memory_history_without_writing() {
    let free = [0x33u16.to_le_bytes().as_slice(), &0x8000u64.to_le_bytes()].concat();
    let source = history_fixture("PxProfileMemoryEventBuffer", &free, &free, &[]);
    let mut output = vec![];
    let failure = trim_scene_frames(&source, 1, 1, &mut output, || true).unwrap_err();
    assert!(
        failure.to_string().contains("earlier memory history"),
        "{failure}"
    );
    assert!(output.is_empty());
}

#[test]
fn middle_history_restores_a_captured_renderer_instance_in_the_array_head_layout() {
    let source = history_fixture(
        "PvdUserRenderer",
        &drawing(Some(1)),
        &drawing(None),
        &drawing(None),
    );
    let original = crate::Recording::from_bytes(&source).unwrap();
    let mut output = vec![];
    trim_scene_frames(&source, 1, 2, &mut output, || true).unwrap();
    let restored = crate::Recording::from_bytes(&output).unwrap();
    for frame in [0, 1, 1, 0] {
        let actual = restored.state_at(frame).unwrap();
        assert_eq!(actual, original.state_at(frame + 1).unwrap());
        let drawing: Vec<_> = actual
            .debug_draw
            .values()
            .flat_map(|batch| batch.iter())
            .collect();
        assert!(matches!(
            drawing.as_slice(),
            [crate::debug_draw::Primitive::Point {
                instance: 1,
                position: [1.0, 2.0, 3.0],
                color: 0xff112233,
            }]
        ));
    }
    restored.verify_replay().unwrap();
}

#[test]
fn middle_history_preserves_errors_as_baseline_and_interval_occurrences() {
    let record = |index: u32| {
        [4u32, 25, 26, 42, index]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>()
    };
    let a = record(7);
    let b = record(9);
    let source = history_fixture("UserCustomedEvent", &[], &a, &b);
    let mut output = vec![];
    trim_scene_frames(&source, 1, 2, &mut output, || true).unwrap();
    let (restored, _) = crate::native::read_with_progress(&output, |_, _| true).unwrap();
    let events = restored.untimed_error_events();
    assert_eq!(events.len(), 2);
    assert_eq!((events[0].index, events[0].received_frame), (7, 0));
    assert_eq!((events[1].index, events[1].received_frame), (9, 1));
    let c = record(11);
    let source = history_fixture("UserCustomedEvent", &a, &b, &c);
    let (original, _) = crate::native::read_with_progress(&source, |_, _| true).unwrap();
    for last in [1, 2] {
        output.clear();
        trim_scene_frames(&source, 1, last, &mut output, || true).unwrap();
        let (restored, _) = crate::native::read_with_progress(&output, |_, _| true).unwrap();
        let events = restored.untimed_error_events();
        assert_eq!(events.len(), last);
        assert_eq!((events[0].index, events[0].received_frame), (9, 0));
        assert!(!events.iter().any(|event| event.index == 7));
        if last == 2 {
            assert_eq!((events[1].index, events[1].received_frame), (11, 1));
        }
        for frame in (0..last).chain((0..last).rev()) {
            assert_eq!(
                restored.state_at(frame).unwrap(),
                original.state_at(frame + 1).unwrap()
            );
            let state = restored.state_at(frame).unwrap();
            let crate::Value::Array(errors) = &state.objects[&3].properties["Events"] else {
                panic!("missing error snapshot")
            };
            assert_eq!(errors.len(), frame + 2);
        }
        restored.verify_replay().unwrap();
    }
    let source = history_fixture("UserCustomedEvent", &a, &[], &[]);
    let (original, _) = crate::native::read_with_progress(&source, |_, _| true).unwrap();
    output.clear();
    trim_scene_frames(&source, 1, 2, &mut output, || true).unwrap();
    let (restored, _) = crate::native::read_with_progress(&output, |_, _| true).unwrap();
    assert!(restored.untimed_error_events().is_empty());
    for frame in 0..2 {
        assert_eq!(
            restored.state_at(frame).unwrap(),
            original.state_at(frame + 1).unwrap()
        );
    }
}

#[test]
fn middle_profile_preserves_cross_boundary_nested_starts_without_completed_prehistory() {
    let before = profile_buffer(&[(1, 60), (2, 65), (1, 70), (1, 80)]);
    let current = profile_buffer(&[(2, 100), (1, 110)]);
    let later = profile_buffer(&[(2, 120), (2, 130)]);
    let source = history_fixture("PxProfileZone", &before, &current, &later);
    let (original, _) = crate::native::read_with_progress(&source, |_, _| true).unwrap();
    for last in [1, 2] {
        let mut output = vec![];
        trim_scene_frames(&source, 1, last, &mut output, || true).unwrap();
        let parsed = Layout::parse(&output, &mut || true).unwrap();
        assert!(matches!(parsed.events[0], HistoryEvent::Marker));
        let HistoryEvent::Set {
            id,
            property,
            data,
            undo,
            ..
        } = &parsed.events[1]
        else {
            panic!("missing starts")
        };
        assert_eq!((*id, *property, undo.as_slice()), (3, 4, &[1][..]));
        assert_eq!(data.len(), 52);
        assert_eq!(parsed.communication.sections[0].rows[0][1], 2);
        let HistoryEvent::Set { undo, .. } = &parsed.events[3] else {
            panic!("missing captured buffer")
        };
        assert_eq!(undo, &[2]);
        let (restored, _) = crate::native::read_with_progress(&output, |_, _| true).unwrap();
        for frame in (0..last).chain((0..last).rev()) {
            assert_eq!(
                restored.state_at(frame).unwrap(),
                original.state_at(frame + 1).unwrap()
            );
        }
        let analysis = restored.analysis.legacy().unwrap();
        let spans = analysis
            .spans
            .iter()
            .map(|span| (span.start, span.end, span.received_frame))
            .collect::<Vec<_>>();
        assert_eq!(
            spans,
            if last == 1 {
                vec![(80, 100, 0)]
            } else {
                vec![(80, 100, 0), (110, 120, 1), (70, 130, 1)]
            }
        );
        assert_eq!(analysis.open_spans(), if last == 1 { 2 } else { 0 });
        assert!(!spans.iter().any(|&(start, _, _)| start == 60));
        restored.verify_replay().unwrap();
    }
}

#[test]
fn middle_profile_closed_prehistory_needs_no_bootstrap_and_missing_first_frame_update_is_refused() {
    let before = profile_buffer(&[(1, 10), (2, 20)]);
    let current = profile_buffer(&[(1, 100), (2, 120)]);
    let source = history_fixture("PxProfileZone", &before, &current, &[]);
    let mut output = vec![];
    trim_scene_frames(&source, 1, 1, &mut output, || true).unwrap();
    let layout = Layout::parse(&output, &mut || true).unwrap();
    assert_eq!(layout.communication.sections[0].rows[0][1], 1);
    let (restored, _) = crate::native::read_with_progress(&output, |_, _| true).unwrap();
    assert_eq!(restored.analysis.legacy().unwrap().spans.len(), 1);
    let before = profile_buffer(&[(1, 10)]);
    let source = history_fixture("PxProfileZone", &before, &[], &[]);
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    let start = layout.communication.sections[0].rows[1][1] as usize;
    layout.events[start + 1] = HistoryEvent::Marker;
    let error = layout.trim_middle(1, 1, &mut || true).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("within the first retained frame")
    );
}

#[test]
fn middle_profile_bootstrap_keeps_error_snapshot_and_event_numbering() {
    let record = |index| {
        [4u32, 25, 26, 42, index]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>()
    };
    let source = history_fixture("UserCustomedEvent", &record(7), &record(9), &record(11));
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    for (id, text) in [(27, "PxProfileZone"), (28, "events")] {
        layout.metadata.strings.insert(id, text.into());
    }
    layout.metadata.classes.push(Some(Class {
        name: name(10, 27),
        base: u32::MAX,
        packing: crate::native::ClassPacking::default(),
        size: 12,
        properties: vec![10],
        dynamic: false,
        has_pointers: false,
        string_offsets: vec![],
    }));
    layout.metadata.properties.push(Some(Property {
        owner: name(10, 27),
        name: 28,
        datatype: name(1, 14),
        datatype_id: 5,
        kind: 2,
        semantic: 0,
        offset32: 0,
        values: vec![],
    }));
    layout.pointer_layouts.push(vec![]);
    let object_bytes = metadata_bytes(&layout.metadata);
    layout.object_bytes = &object_bytes;
    for id in [4, 5] {
        let source = u64::from(id) * 0x111;
        layout.communication.instances.push(Instance {
            id,
            source,
            flags: [0; 10],
        });
        layout.communication.zones.push(Zone {
            source,
            saved_word: 0,
            name: b"Zone\0",
            id,
            events: vec![(b"Event\0", 1, 1)],
        });
    }
    let before = layout.communication.sections[0].rows[0][1] as usize;
    let current = layout.communication.sections[0].rows[1][1] as usize + 1;
    let mut mapping = vec![];
    let mut events = vec![];
    let mut previous = BTreeMap::new();
    for (ordinal, event) in std::mem::take(&mut layout.events).into_iter().enumerate() {
        if ordinal == before {
            for (id, tick) in [(4, 70), (5, 80)] {
                events.push(HistoryEvent::Create(id, 9));
                previous.insert(id, events.len());
                events.push(HistoryEvent::Set {
                    kind: 3,
                    id,
                    property: 10,
                    data: profile_buffer(&[(1, tick)]).into(),
                    undo: vec![1],
                });
            }
        }
        if ordinal == current {
            for (id, tick) in [(4, 100), (5, 110)] {
                events.push(HistoryEvent::Set {
                    kind: 3,
                    id,
                    property: 10,
                    data: profile_buffer(&[(2, tick)]).into(),
                    undo: vec![(events.len() - previous[&id]) as u32],
                });
            }
        }
        mapping.push(events.len() as u64);
        events.push(event);
    }
    layout.events = events;
    for row in &mut layout.communication.sections[0].rows {
        row[1] = mapping[row[1] as usize];
        row[3] = mapping[row[3] as usize];
    }
    let page = encode_page(&layout.events, &mut || true).unwrap();
    layout.pages[0].count = layout.events.len() as u32;
    layout.pages[0].len = page.len() as u32;
    let source = layout.encode(false, &mut || true).unwrap();
    let (original, _) = crate::native::read_with_progress(&source, |_, _| true).unwrap();
    let mut output = vec![];
    trim_scene_frames(&source, 1, 2, &mut output, || true).unwrap();
    let (restored, _) = crate::native::read_with_progress(&output, |_, _| true).unwrap();
    let errors = restored.untimed_error_events();
    assert_eq!(errors.len(), 2);
    assert_eq!(
        (
            errors[0].index,
            errors[0].received_frame,
            errors[0].history_event
        ),
        (9, 0, 6)
    );
    assert_eq!((errors[1].index, errors[1].received_frame), (11, 1));
    for frame in [0, 1, 0, 1] {
        assert_eq!(
            restored.state_at(frame).unwrap(),
            original.state_at(frame + 1).unwrap()
        );
    }
    let analysis = restored.analysis.legacy().unwrap();
    assert_eq!(analysis.spans.len(), 2);
    assert_eq!(
        analysis
            .spans
            .iter()
            .map(|span| (span.zone, span.start, span.end))
            .collect::<Vec<_>>(),
        [(4, 70, 100), (5, 80, 110)]
    );
    let layout = Layout::parse(&output, &mut || true).unwrap();
    assert_eq!(layout.communication.sections[0].rows[0][1], 3);
    for (ordinal, expected_id, target) in [(4, 4, 1), (5, 5, 2)] {
        let HistoryEvent::Set { id, undo, .. } = &layout.events[ordinal] else {
            panic!("missing profile set")
        };
        assert_eq!(*id, expected_id);
        assert_eq!(undo.as_slice(), &[(ordinal - target) as u32]);
    }
}

#[test]
fn bootstrap_undo_lookup_is_bounded_and_cancellable() {
    let leading = [
        HistoryEvent::Marker,
        HistoryEvent::Set {
            kind: 3,
            id: 3,
            property: 4,
            data: Cow::Borrowed(&[]),
            undo: vec![1],
        },
    ];
    let mut events = vec![HistoryEvent::Set {
        kind: 3,
        id: 3,
        property: 4,
        data: Cow::Borrowed(&[]),
        undo: vec![9],
    }];
    let mut calls = 0;
    let error = rebase_undo(&mut events, 10, &leading, &mut || {
        calls += 1;
        calls < 4
    })
    .unwrap_err();
    assert_eq!(calls, 4);
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
    rebase_undo(&mut events, 10, &leading, &mut || true).unwrap();
    let HistoryEvent::Set { undo, .. } = &events[0] else {
        panic!()
    };
    assert_eq!(undo, &[1]);
}
