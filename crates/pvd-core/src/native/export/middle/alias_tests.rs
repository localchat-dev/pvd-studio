use super::*;
use crate::Value;
use crate::native::ClassPacking;

#[path = "bool_alias_tests.rs"]
mod bool_alias_tests;

const SCENE: u32 = 90;
const BODY: u32 = 91;
const SECTION: u32 = 92;
const DERIVED: u32 = 93;

fn intern(metadata: &mut Metadata, value: &str) -> u32 {
    if let Some((&handle, _)) = metadata
        .strings
        .iter()
        .find(|(_, text)| text.as_str() == value)
    {
        return handle;
    }
    let handle = metadata.strings.last_key_value().unwrap().0 + 1;
    metadata.strings.insert(handle, value.into());
    handle
}

fn plain_class(name: Name, size: usize, properties: Vec<u32>) -> Class {
    Class {
        name,
        base: u32::MAX,
        packing: ClassPacking::default(),
        size,
        properties,
        dynamic: false,
        has_pointers: false,
        string_offsets: vec![],
    }
}

fn alias_metadata(source: u32, destination: u32, width: usize) -> Metadata {
    let mut metadata = fixture_metadata();
    metadata.classes = (0..94).map(|_| None).collect();
    metadata.properties.clear();
    metadata.messages.clear();
    for (id, label, size, uniform_width, class_type) in [
        (65, "PvdI8", 1, 1, 65),
        (66, "PvdU8", 1, 1, 66),
        (67, "PvdI16", 2, 2, 67),
        (68, "PvdU16", 2, 2, 68),
        (69, "PvdI32", 4, 4, 69),
        (70, "PvdU32", 4, 4, 70),
        (71, "PvdI64", 8, 8, 71),
        (72, "PvdU64", 8, 8, 72),
        (73, "PvdF32", 4, 4, 73),
        (74, "PvdF64", 8, 8, 74),
        (75, "PvdBool", 1, 1, 75),
        (83, "PxVec4", 16, 4, 73),
        (85, "PxQuat", 16, 4, 73),
    ] {
        let label = intern(&mut metadata, label);
        let mut class = plain_class(name(1, label), size, vec![]);
        class.packing = ClassPacking {
            uniform_width,
            class_type,
        };
        metadata.classes[id as usize] = Some(class);
    }
    let datatype = metadata.class(destination).unwrap().name;
    for (index, label) in [6, 7].into_iter().enumerate() {
        metadata.properties.push(Some(Property {
            owner: name(1, 5),
            name: label,
            datatype,
            datatype_id: destination,
            kind: 1,
            semantic: 0,
            offset32: index * width,
            values: vec![],
        }));
    }
    for id in [83, 85] {
        let owner = metadata.class(id).unwrap().name;
        let datatype = metadata.class(73).unwrap().name;
        let mut properties = vec![];
        for (index, label) in ["x", "y", "z", "w"].into_iter().enumerate() {
            let label = intern(&mut metadata, label);
            properties.push(metadata.properties.len() as u32);
            metadata.properties.push(Some(Property {
                owner,
                name: label,
                datatype,
                datatype_id: 73,
                kind: 1,
                semantic: 0,
                offset32: index * 4,
                values: vec![],
            }));
        }
        metadata.classes[id as usize].as_mut().unwrap().properties = properties;
    }
    metadata.classes[SCENE as usize] = Some(plain_class(name(1, 2), 0, vec![]));
    metadata.classes[BODY as usize] = Some(plain_class(name(1, 5), width * 2, vec![0, 1]));
    metadata.classes[SECTION as usize] = Some(plain_class(name(10, 11), 24, vec![]));
    let label = intern(&mut metadata, "DerivedBody");
    let mut class = plain_class(name(1, label), width * 2, vec![]);
    class.base = BODY;
    metadata.classes[DERIVED as usize] = Some(class);
    let datatype = metadata.class(source).unwrap().name;
    for index in 0..2 {
        intern(&mut metadata, &format!("AliasMessage{index}"));
        metadata.messages.push(Some(Message {
            class_name: name(1, 5),
            class: BODY,
            size: width,
            entries: vec![MessageEntry {
                datatype,
                datatype_id: source,
                property: 0,
                offset: 0,
                size: width,
                destination_size: width,
            }],
        }));
    }
    metadata
}

fn object_bytes(metadata: &Metadata) -> Vec<u8> {
    let mut bytes = metadata_bytes(metadata);
    let message_bytes = metadata
        .messages
        .iter()
        .map(|message| {
            message
                .as_ref()
                .map_or(4, |message| 40 + message.entries.len() * 28)
        })
        .sum::<usize>();
    let mut cursor = bytes.len() - message_bytes;
    for (index, message) in metadata.messages.iter().enumerate() {
        let Some(message) = message else {
            cursor += 4;
            continue;
        };
        let label = format!("AliasMessage{index}");
        let handle = metadata
            .strings
            .iter()
            .find(|(_, value)| value.as_str() == label)
            .unwrap()
            .0;
        bytes[cursor + 16..cursor + 20].copy_from_slice(&1u32.to_le_bytes());
        bytes[cursor + 20..cursor + 24].copy_from_slice(&handle.to_le_bytes());
        cursor += 40 + message.entries.len() * 28;
    }
    bytes
}

fn source(metadata: Metadata, width: usize, payloads: &[Vec<u8>], derived: bool) -> Vec<u8> {
    let zero = vec![0; width];
    let mut events = vec![
        HistoryEvent::Marker,
        HistoryEvent::Create(1, SCENE),
        HistoryEvent::Create(2, if derived { DERIVED } else { BODY }),
        HistoryEvent::Set {
            kind: 3,
            id: 2,
            property: 0,
            data: zero.clone().into(),
            undo: vec![1],
        },
        HistoryEvent::Set {
            kind: 3,
            id: 2,
            property: 1,
            data: zero.into(),
            undo: vec![2],
        },
    ];
    let mut previous = 3usize;
    let mut rows = vec![];
    for (frame, payload) in payloads.iter().enumerate() {
        let ticks = (frame as u64 + 1) * 100;
        let begin = events.len() as u64;
        events.push(HistoryEvent::Section {
            class: SECTION,
            source: 0x111,
            ticks,
            name: 12,
            begin: 1,
            padding: [0; 3],
        });
        let current = events.len();
        let undo = if frame == 0
            && metadata.messages[0]
                .as_ref()
                .unwrap()
                .entries
                .iter()
                .any(|entry| entry.property == 1)
        {
            vec![(current - 3) as u32, (current - 4) as u32]
        } else {
            vec![(current - previous) as u32]
        };
        events.push(HistoryEvent::Set {
            kind: 4,
            id: 2,
            property: u32::from(frame == 1),
            data: payload.clone().into(),
            undo,
        });
        previous = current;
        let end = events.len() as u64;
        events.push(HistoryEvent::Section {
            class: SECTION,
            source: 0x111,
            ticks: ticks + 1,
            name: 12,
            begin: 0,
            padding: [0; 3],
        });
        rows.push([ticks, begin, ticks + 1, end]);
    }
    let page = encode_page(&events, &mut || true).unwrap();
    let object_bytes = object_bytes(&metadata);
    let classes = metadata.classes.len();
    Layout {
        source: &[],
        object_bytes: &object_bytes,
        metadata,
        pointer_layouts: vec![vec![]; classes],
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
            len: 4,
            reserved: 0,
        }],
        snapshot_bytes: vec![Cow::Borrowed(&[0; 4])],
        saved: vec![],
        events,
        backing: &[],
        gaps: vec![],
    }
    .encode(false, &mut || true)
    .unwrap()
}

fn payloads(width: usize) -> Vec<Vec<u8>> {
    let words = match width {
        1 => [127u64, 128, 255, 1],
        2 => [32767, 32768, 65535, 1],
        4 => [2147483647, 2147483648, 4294967295, 1],
        8 => [
            9223372036854775807,
            9223372036854775808,
            18446744073709551615,
            1,
        ],
        _ => unreachable!(),
    };
    words
        .into_iter()
        .map(|word| word.to_le_bytes()[..width].to_vec())
        .collect()
}

fn expected(destination: u32) -> Vec<Value> {
    match destination {
        65 => [127, -128, -1, 1].into_iter().map(Value::I64).collect(),
        66 => [127, 128, 255, 1].into_iter().map(Value::U64).collect(),
        67 => [32767, -32768, -1, 1].into_iter().map(Value::I64).collect(),
        68 => [32767, 32768, 65535, 1]
            .into_iter()
            .map(Value::U64)
            .collect(),
        69 => [2147483647, -2147483648, -1, 1]
            .into_iter()
            .map(Value::I64)
            .collect(),
        70 => [2147483647, 2147483648, 4294967295, 1]
            .into_iter()
            .map(Value::U64)
            .collect(),
        71 => [9223372036854775807, i64::MIN, -1, 1]
            .into_iter()
            .map(Value::I64)
            .collect(),
        72 => [9223372036854775807, 9223372036854775808, u64::MAX, 1]
            .into_iter()
            .map(Value::U64)
            .collect(),
        _ => unreachable!(),
    }
}

fn verify(source: &[u8], expected: &[Value], other: &[Value]) -> Vec<u8> {
    let (recording, _) = crate::native::read_with_progress(source, |_, _| true).unwrap();
    assert_eq!(recording.frame_count(), 4);
    for frame in [3, 0, 2, 1, 0] {
        let state = recording.state_at(frame).unwrap();
        assert_eq!(state.objects[&2].properties["Value"], expected[frame]);
        assert_eq!(state.objects[&2].properties["Other"], other[frame]);
    }
    let mut prefix = vec![];
    trim_scene_frames(source, 0, 3, &mut prefix, || true).unwrap();
    let mut output = vec![];
    trim_scene_frames(source, 1, 3, &mut output, || true).unwrap();
    for (bytes, first, count) in [(&prefix, 0, 4), (&output, 1, 3)] {
        let (restored, _) = crate::native::read_with_progress(bytes, |_, _| true).unwrap();
        assert_eq!(restored.frame_count(), count);
        for frame in [count - 1, 0, 1, count - 1, 0] {
            let state = restored.state_at(frame).unwrap();
            assert_eq!(
                state.objects[&2].properties["Value"],
                expected[first + frame]
            );
            assert_eq!(state.objects[&2].properties["Other"], other[first + frame]);
        }
    }
    let mut retrimmed = vec![];
    trim_scene_frames(&output, 1, 2, &mut retrimmed, || true).unwrap();
    let (restored, _) = crate::native::read_with_progress(&retrimmed, |_, _| true).unwrap();
    for frame in [1, 0, 1] {
        assert_eq!(
            restored.state_at(frame).unwrap().objects[&2].properties["Value"],
            expected[frame + 2]
        );
    }
    let original = Layout::parse(source, &mut || true).unwrap();
    let trimmed = Layout::parse(&output, &mut || true).unwrap();
    assert_eq!(original.object_bytes, trimmed.object_bytes);
    let original_messages = original
        .events
        .iter()
        .filter_map(|event| match event {
            HistoryEvent::Set { kind: 4, data, .. } => Some(data.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let trimmed_messages = trimmed
        .events
        .iter()
        .filter_map(|event| match event {
            HistoryEvent::Set { kind: 4, data, .. } => Some(data.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(trimmed_messages, original_messages[1..]);
    let messages = trimmed
        .events
        .iter()
        .enumerate()
        .filter_map(|(ordinal, event)| match event {
            HistoryEvent::Set {
                kind: 4,
                property,
                undo,
                ..
            } => Some((ordinal, *property, undo)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        messages.iter().map(|message| message.1).collect::<Vec<_>>(),
        [1, 0, 0]
    );
    for pair in messages.windows(2) {
        assert_eq!(*pair[1].2, vec![(pair[1].0 - pair[0].0) as u32]);
    }
    output
}

#[test]
fn fixed_integer_alias_messages_preserve_bits_across_ids_inheritance_and_middle_reopen() {
    for (incoming, destination, width) in [
        (65, 66, 1),
        (66, 65, 1),
        (67, 68, 2),
        (68, 67, 2),
        (69, 70, 4),
        (70, 69, 4),
        (71, 72, 8),
        (72, 71, 8),
    ] {
        for derived in [false, true] {
            let source = source(
                alias_metadata(incoming, destination, width),
                width,
                &payloads(width),
                derived,
            );
            let values = expected(destination);
            let zero = if destination.is_multiple_of(2) {
                Value::U64(0)
            } else {
                Value::I64(0)
            };
            verify(&source, &values, &vec![zero; 4]);
        }
    }
}

#[test]
fn aliases_preserve_repeated_members_and_overlapping_source_ranges() {
    let mut repeated = alias_metadata(69, 70, 4);
    for message in repeated.messages.iter_mut().flatten() {
        let first = &message.entries[0];
        message.entries.push(MessageEntry {
            datatype: first.datatype,
            datatype_id: first.datatype_id,
            property: 0,
            offset: 4,
            size: 4,
            destination_size: 4,
        });
        message.size = 8;
    }
    let payloads = payloads(4)
        .into_iter()
        .map(|last| [vec![0x55; 4], last].concat())
        .collect::<Vec<_>>();
    verify(
        &source(repeated, 4, &payloads, false),
        &expected(70),
        &vec![Value::U64(0); 4],
    );
    let mut overlap = alias_metadata(69, 70, 4);
    for message in overlap.messages.iter_mut().flatten() {
        let first = &message.entries[0];
        message.entries.push(MessageEntry {
            datatype: first.datatype,
            datatype_id: first.datatype_id,
            property: 1,
            offset: 2,
            size: 4,
            destination_size: 4,
        });
        message.size = 6;
    }
    let payloads = vec![
        vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
        vec![0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
        vec![1, 2, 3, 4, 5, 6],
        vec![0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54],
    ];
    let values = [0x44332211, 0xddccbbaa, 0x04030201, 0x98badcfe]
        .into_iter()
        .map(Value::U64)
        .collect::<Vec<_>>();
    let others = [0x66554433, 0xffeeddcc, 0x06050403, 0x547698ba]
        .into_iter()
        .map(Value::U64)
        .collect::<Vec<_>>();
    verify(&source(overlap, 4, &payloads, true), &values, &others);
}

#[test]
fn vector_quaternion_aliases_preserve_all_float_bits() {
    let vectors = [
        [0.0f32, 0.0, 0.0, 1.0],
        [-0.0, 0.0, 0.6, 0.8],
        [0.0, 1.0, 0.0, 0.0],
        [1.0, 0.0, 0.0, -0.0],
    ];
    let payloads = vectors
        .iter()
        .map(|value| {
            value
                .iter()
                .flat_map(|n| n.to_le_bytes())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    for (incoming, destination) in [(83, 85), (85, 83)] {
        let expected = if destination == 85 {
            vectors.into_iter().map(Value::Quat).collect::<Vec<_>>()
        } else {
            vectors
                .iter()
                .map(|value| {
                    Value::Array(
                        value
                            .iter()
                            .map(|&n| Value::F32(n))
                            .collect::<Vec<_>>()
                            .into(),
                    )
                })
                .collect()
        };
        let zero = if destination == 85 {
            Value::Quat([0.0; 4])
        } else {
            Value::Array(vec![Value::F32(0.0); 4].into())
        };
        let output = verify(
            &source(
                alias_metadata(incoming, destination, 16),
                16,
                &payloads,
                true,
            ),
            &expected,
            &vec![zero; 4],
        );
        let (recording, _) = crate::native::read_with_progress(&output, |_, _| true).unwrap();
        for frame in [2, 0, 1] {
            let state = recording.state_at(frame).unwrap();
            let bits = match &state.objects[&2].properties["Value"] {
                Value::Quat(values) => values.iter().map(|n| n.to_bits()).collect::<Vec<_>>(),
                Value::Array(values) => values
                    .iter()
                    .map(|value| match value {
                        Value::F32(n) => n.to_bits(),
                        _ => unreachable!(),
                    })
                    .collect(),
                _ => unreachable!(),
            };
            assert_eq!(bits, vectors[frame + 1].map(f32::to_bits));
        }
    }
}

#[test]
fn alias_packing_roundtrips_and_rejects_forged_layouts_and_numeric_marshalling() {
    let original = alias_metadata(69, 70, 4);
    let parsed = Metadata::parse(&object_bytes(&original)).unwrap();
    assert_eq!(
        parsed.class(69).unwrap().packing,
        ClassPacking {
            uniform_width: 4,
            class_type: 69
        }
    );
    assert_eq!(
        parsed.class(70).unwrap().packing,
        ClassPacking {
            uniform_width: 4,
            class_type: 70
        }
    );
    for change in 0..10 {
        let mut metadata = alias_metadata(69, 70, 4);
        match change {
            0 => metadata.classes[69].as_mut().unwrap().packing.class_type = 73,
            1 => metadata.classes[70].as_mut().unwrap().packing.uniform_width = -1,
            2 => metadata.classes[69].as_mut().unwrap().has_pointers = true,
            3 => metadata.classes[70]
                .as_mut()
                .unwrap()
                .string_offsets
                .push(0),
            4 => metadata.classes[69].as_mut().unwrap().base = 70,
            5 => {
                let label = intern(&mut metadata, "custom");
                metadata.classes[69].as_mut().unwrap().name.namespace = label;
                metadata.messages[0].as_mut().unwrap().entries[0]
                    .datatype
                    .namespace = label;
            }
            6 => metadata.classes[69].as_mut().unwrap().properties.push(0),
            7 => metadata.classes[69].as_mut().unwrap().dynamic = true,
            8 => {
                metadata.classes[69].as_mut().unwrap().packing.class_type = 73;
                metadata.messages[0].as_mut().unwrap().entries[0].destination_size = 8;
            }
            9 => metadata.properties[0].as_mut().unwrap().kind = 2,
            _ => unreachable!(),
        }
        let error = validate_message(&metadata, 0, &[0; 4], &mut || true)
            .unwrap_err()
            .to_string();
        if change == 8 {
            assert!(error.contains("field width disagrees"), "{error}");
        }
        if change == 9 {
            assert!(error.contains("array-target"), "{error}");
        }
        if change == 2 {
            let parsed = Metadata::parse(&object_bytes(&metadata)).unwrap();
            assert!(parsed.class(69).unwrap().has_pointers);
            assert!(parsed.class(69).unwrap().string_offsets.is_empty());
        }
        let source = source(metadata, 4, &payloads(4), false);
        for first in [0, 1] {
            let mut output = vec![];
            assert!(
                trim_scene_frames(&source, first, 3, &mut output, || true).is_err(),
                "change {change}, first {first}"
            );
            assert!(output.is_empty());
        }
    }
    for (incoming, destination, width) in [(73, 70, 4), (69, 73, 4), (74, 72, 8), (66, 75, 1)] {
        let metadata = alias_metadata(incoming, destination, width);
        let error = validate_message(&metadata, 0, &vec![0; width], &mut || true)
            .unwrap_err()
            .to_string();
        assert!(error.contains("conversion is not verified"), "{error}");
    }
    let mut metadata = alias_metadata(83, 85, 16);
    let id = metadata.class(83).unwrap().properties[1] as usize;
    metadata.properties[id].as_mut().unwrap().offset32 = 0;
    assert!(validate_message(&metadata, 0, &[0; 16], &mut || true).is_err());
}

#[test]
fn alias_validation_cancels_before_raw_mutation_and_bad_undo_leaves_no_output() {
    let mut metadata = alias_metadata(69, 70, 4);
    let datatype = metadata.class(69).unwrap().name;
    metadata.messages[0].as_mut().unwrap().entries = (0..1024)
        .map(|_| MessageEntry {
            datatype,
            datatype_id: 69,
            property: 0,
            offset: 0,
            size: 4,
            destination_size: 4,
        })
        .collect();
    let mut state = BTreeMap::from([(2, RawInstance::create(&metadata, BODY).unwrap())]);
    let original = state.clone();
    let mut allocated = state.values().map(RawInstance::bytes).sum();
    let event = HistoryEvent::Set {
        kind: 4,
        id: 2,
        property: 0,
        data: vec![1; 4].into(),
        undo: vec![],
    };
    let mut checks = 0;
    let error = raw_apply(&metadata, &mut state, &event, &mut allocated, &mut || {
        checks += 1;
        checks < 32
    })
    .unwrap_err()
    .to_string();
    assert!(error.contains("cancel"), "{error}");
    assert_eq!(state, original);
    let source = source(alias_metadata(69, 70, 4), 4, &payloads(4), false);
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    let HistoryEvent::Set { undo, .. } = &mut layout.events[12] else {
        unreachable!()
    };
    undo[0] = u32::MAX;
    let invalid = encode_modified(&mut layout);
    let mut output = vec![];
    assert!(trim_scene_frames(&invalid, 1, 3, &mut output, || true).is_err());
    assert!(output.is_empty());
}

#[test]
#[ignore = "writes constructed native bit-copy alias sources and middle clips"]
fn retain_bit_copy_alias_fixtures() {
    let directory = std::path::PathBuf::from(
        std::env::var_os("PVD_NATIVE_BITCOPY_ALIAS_DIRECTORY")
            .expect("Set PVD_NATIVE_BITCOPY_ALIAS_DIRECTORY to a new output directory"),
    );
    std::fs::create_dir(&directory).unwrap();
    for (incoming, destination, width) in [
        (65, 66, 1),
        (66, 65, 1),
        (67, 68, 2),
        (68, 67, 2),
        (69, 70, 4),
        (70, 69, 4),
        (71, 72, 8),
        (72, 71, 8),
    ] {
        let source = source(
            alias_metadata(incoming, destination, width),
            width,
            &payloads(width),
            true,
        );
        let zero = if destination.is_multiple_of(2) {
            Value::U64(0)
        } else {
            Value::I64(0)
        };
        let output = verify(&source, &expected(destination), &vec![zero; 4]);
        let stem = format!("alias-{incoming}-{destination}");
        std::fs::write(directory.join(format!("{stem}-source.pxd2")), source).unwrap();
        std::fs::write(directory.join(format!("{stem}-middle.pxd2")), output).unwrap();
    }
}
