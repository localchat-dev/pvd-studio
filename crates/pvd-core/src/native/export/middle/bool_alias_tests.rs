use super::*;

fn value(destination: u32, one: bool) -> Value {
    match destination {
        65 => Value::I64(i64::from(one)),
        66 => Value::U64(u64::from(one)),
        _ => unreachable!(),
    }
}

fn copy_entry(entry: &MessageEntry) -> MessageEntry {
    MessageEntry {
        datatype: entry.datatype,
        datatype_id: entry.datatype_id,
        property: entry.property,
        offset: entry.offset,
        size: entry.size,
        destination_size: entry.destination_size,
    }
}

fn boolean_payloads() -> Vec<Vec<u8>> {
    [0, 1, 0, 1].into_iter().map(|byte| vec![byte]).collect()
}

fn reject(source: &[u8], first: usize, expected: &str) {
    let mut output = vec![];
    let error = trim_scene_frames(source, first, 3, &mut output, || true)
        .unwrap_err()
        .to_string();
    assert!(error.contains(expected), "{error}");
    assert!(output.is_empty());
}

#[test]
fn canonical_boolean_messages_preserve_integer_bytes_inheritance_and_snapshots() {
    for destination in [65, 66] {
        for derived in [false, true] {
            let source = source(
                alias_metadata(75, destination, 1),
                1,
                &boolean_payloads(),
                derived,
            );
            let expected = [false, true, false, true].map(|one| value(destination, one));
            let other = vec![value(destination, false); 4];
            verify(&source, &expected, &other);
            let mut output = vec![];
            trim_scene_frames(&source, 2, 3, &mut output, || true).unwrap();
            let layout = Layout::parse(&output, &mut || true).unwrap();
            let body = &layout
                .saved
                .iter()
                .find(|saved| saved.id == 2)
                .unwrap()
                .body;
            assert_eq!(body.as_ref(), &[2, 0, 0, 0, 1, 0]);
            let (recording, _) = crate::native::read_with_progress(&output, |_, _| true).unwrap();
            for frame in [1, 0, 1] {
                assert_eq!(
                    recording.state_at(frame).unwrap().objects[&2].properties["Value"],
                    expected[frame + 2]
                );
            }
        }
    }
}

#[test]
fn canonical_boolean_message_entries_preserve_padding_duplicates_and_shared_sources() {
    for destination in [65, 66] {
        let mut metadata = alias_metadata(75, destination, 1);
        metadata.messages[0].as_mut().unwrap().size = 3;
        metadata.messages[0].as_mut().unwrap().entries[0].offset = 1;
        validate_message(&metadata, 0, &[2, 1, 255], &mut || true).unwrap();
        assert!(
            validate_message(&metadata, 0, &[0, 2, 0], &mut || true)
                .unwrap_err()
                .to_string()
                .contains("boolean alias")
        );
        let mut metadata = alias_metadata(75, destination, 1);
        let ordinary_type = metadata.class(destination).unwrap().name;
        let message = metadata.messages[0].as_mut().unwrap();
        message.size = 2;
        let mut ordinary = copy_entry(&message.entries[0]);
        ordinary.datatype = ordinary_type;
        ordinary.datatype_id = destination;
        ordinary.property = 1;
        ordinary.offset = 1;
        message.entries.push(ordinary);
        validate_message(&metadata, 0, &[1, 255], &mut || true).unwrap();
        assert!(
            validate_message(&metadata, 0, &[2, 255], &mut || true)
                .unwrap_err()
                .to_string()
                .contains("boolean alias")
        );
        for shared in [false, true] {
            let mut metadata = alias_metadata(75, destination, 1);
            for message in metadata.messages.iter_mut().flatten() {
                message.size = if shared { 1 } else { 2 };
                let mut entry = copy_entry(&message.entries[0]);
                entry.property = u32::from(shared);
                entry.offset = usize::from(!shared);
                message.entries.push(entry);
            }
            let payloads = if shared {
                boolean_payloads()
            } else {
                vec![vec![0, 1], vec![1, 0], vec![1, 1], vec![0, 0]]
            };
            let expected = if shared {
                [false, true, false, true]
            } else {
                [true, false, true, false]
            }
            .map(|one| value(destination, one));
            let other = if shared {
                expected.to_vec()
            } else {
                vec![value(destination, false); 4]
            };
            let source = source(metadata, 1, &payloads, true);
            verify(&source, &expected, &other);
        }
    }
}

#[test]
fn boolean_alias_payload_guard_rejects_noncanonical_bytes_before_snapshot_and_undo() {
    for destination in [65, 66] {
        let metadata = alias_metadata(75, destination, 1);
        for byte in 2..=u8::MAX {
            assert!(
                validate_message(&metadata, 0, &[byte], &mut || true)
                    .unwrap_err()
                    .to_string()
                    .contains("boolean alias")
            );
        }
        for bad in [2, 128, 255] {
            for frame in 0..4 {
                let mut payloads = boolean_payloads();
                payloads[frame][0] = bad;
                let source = source(alias_metadata(75, destination, 1), 1, &payloads, false);
                for first in [0, 1] {
                    reject(&source, first, "boolean alias");
                }
            }
        }
        let source = source(
            alias_metadata(75, destination, 1),
            1,
            &boolean_payloads(),
            false,
        );
        let mut nonempty = vec![];
        trim_scene_frames(&source, 1, 3, &mut nonempty, || true).unwrap();
        for bad_message in [0, 1] {
            let mut layout = Layout::parse(&nonempty, &mut || true).unwrap();
            let HistoryEvent::Set { data, .. } = layout
                .events
                .iter_mut()
                .filter(|event| matches!(event, HistoryEvent::Set { kind: 4, .. }))
                .nth(bad_message)
                .unwrap()
            else {
                unreachable!()
            };
            *data = vec![2].into();
            let bad = encode_modified(&mut layout);
            for first in [0, 1] {
                let mut output = vec![];
                let error = trim_scene_frames(&bad, first, 2, &mut output, || true)
                    .unwrap_err()
                    .to_string();
                assert!(error.contains("boolean alias"), "{error}");
                assert!(output.is_empty());
            }
        }
    }
}

#[test]
fn boolean_alias_validation_is_atomic_cancelable_and_keeps_reverse_conversions_closed() {
    for destination in [65, 66] {
        let mut metadata = alias_metadata(75, destination, 1);
        let message = metadata.messages[0].as_mut().unwrap();
        message.size = 2;
        let mut entry = copy_entry(&message.entries[0]);
        entry.property = 1;
        entry.offset = 1;
        message.entries.push(entry);
        let mut state = BTreeMap::from([(2, RawInstance::create(&metadata, BODY).unwrap())]);
        let before = state.clone();
        let mut allocated = state.values().map(RawInstance::bytes).sum();
        let before_allocated = allocated;
        let event = HistoryEvent::Set {
            kind: 4,
            id: 2,
            property: 0,
            data: vec![1, 2].into(),
            undo: vec![],
        };
        let error = raw_apply(&metadata, &mut state, &event, &mut allocated, &mut || true)
            .unwrap_err()
            .to_string();
        assert!(error.contains("boolean alias"), "{error}");
        assert_eq!(state, before);
        assert_eq!(allocated, before_allocated);
        let message = metadata.messages[0].as_mut().unwrap();
        message.size = 1;
        let entries = (0..1024).map(|_| copy_entry(&message.entries[0])).collect();
        message.entries = entries;
        let event = HistoryEvent::Set {
            kind: 4,
            id: 2,
            property: 0,
            data: vec![1].into(),
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
        assert_eq!(state, before);
        assert_eq!(allocated, before_allocated);
    }
    for incoming in [65, 66] {
        let metadata = alias_metadata(incoming, 75, 1);
        for byte in [0, 1] {
            assert!(
                validate_message(&metadata, 0, &[byte], &mut || true)
                    .unwrap_err()
                    .to_string()
                    .contains("conversion is not verified")
            );
        }
    }
    for mutation in 0..9 {
        let mut metadata = alias_metadata(75, 66, 1);
        let name = intern(&mut metadata, "InvalidBool");
        let class = metadata.classes[75].as_mut().unwrap();
        match mutation {
            0 => class.packing.uniform_width = -1,
            1 => class.packing.class_type = 66,
            2 => class.has_pointers = true,
            3 => class.string_offsets.push(0),
            4 => class.size = 2,
            5 => class.name.name = name,
            6 => class.properties.push(0),
            7 => class.dynamic = true,
            8 => class.base = 66,
            _ => unreachable!(),
        }
        assert!(validate_message(&metadata, 0, &[1], &mut || true).is_err());
    }
}

fn with_saved_integer(destination: u32, saved_byte: u8) -> Vec<u8> {
    let source = source(
        alias_metadata(75, destination, 1),
        1,
        &[vec![1], vec![0], vec![1], vec![0]],
        false,
    );
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    let insertion = 7;
    for (ordinal, event) in layout.events.iter_mut().enumerate() {
        if let HistoryEvent::Set { undo, .. } = event {
            let current = ordinal + usize::from(ordinal >= insertion);
            for distance in undo {
                let target = ordinal - *distance as usize;
                let target = target + usize::from(target >= insertion);
                *distance = (current - target) as u32;
            }
        }
    }
    for row in &mut layout.communication.sections[0].rows {
        for index in [1, 3] {
            if row[index] >= insertion as u64 {
                row[index] += 1;
            }
        }
    }
    layout.events.insert(
        insertion,
        HistoryEvent::Saved {
            id: 2,
            position: 8192,
        },
    );
    layout.saved.push(Saved {
        id: 2,
        position: 8192,
        class: BODY,
        body: vec![2, 0, 0, 0, saved_byte, 0].into(),
        // type 10 的对象体必须登记到保存目录；对象此时可以仍然存活。
        deleted: true,
    });
    encode_modified(&mut layout)
}

#[test]
fn canonical_boolean_integer_saved_records_compare_bytes_without_boolean_coercion() {
    for destination in [65, 66] {
        let valid = with_saved_integer(destination, 1);
        for first in [0, 1] {
            let mut output = vec![];
            trim_scene_frames(&valid, first, 3, &mut output, || true).unwrap();
            let (recording, _) = crate::native::read_with_progress(&output, |_, _| true).unwrap();
            for frame in [recording.frame_count() - 1, 0, 1] {
                assert_eq!(
                    recording.state_at(frame).unwrap().objects[&2].properties["Value"],
                    value(destination, (first + frame).is_multiple_of(2))
                );
            }
        }
        reject(
            &with_saved_integer(destination, 2),
            0,
            "saved record disagrees",
        );
    }
}

#[test]
#[ignore = "writes canonical native Bool-to-I8/U8 alias sources and middle clips"]
fn retain_canonical_boolean_alias_fixtures() {
    let directory = std::path::PathBuf::from(
        std::env::var_os("PVD_NATIVE_BOOL_ALIAS_DIRECTORY")
            .expect("Set PVD_NATIVE_BOOL_ALIAS_DIRECTORY to a new output directory"),
    );
    std::fs::create_dir(&directory).unwrap();
    for (destination, label) in [(65, "i8"), (66, "u8")] {
        let source = source(
            alias_metadata(75, destination, 1),
            1,
            &boolean_payloads(),
            true,
        );
        let mut output = vec![];
        trim_scene_frames(&source, 1, 3, &mut output, || true).unwrap();
        std::fs::write(
            directory.join(format!("bool-to-{label}-source.pxd2")),
            source,
        )
        .unwrap();
        std::fs::write(
            directory.join(format!("bool-to-{label}-middle.pxd2")),
            output,
        )
        .unwrap();
    }
}
