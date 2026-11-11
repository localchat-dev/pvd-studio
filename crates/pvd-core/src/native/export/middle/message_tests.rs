use super::*;

fn entry(property: u32, offset: usize) -> MessageEntry {
    MessageEntry {
        datatype: name(1, 3),
        datatype_id: 1,
        property,
        offset,
        size: 4,
        destination_size: 4,
    }
}

fn scalar_message_metadata() -> Metadata {
    let mut metadata = fixture_metadata();
    metadata.messages = vec![Some(Message {
        class_name: name(1, 5),
        class: 3,
        size: 4,
        entries: vec![entry(1, 0)],
    })];
    metadata
}

#[test]
fn metadata_preserves_message_entry_source_type_and_destination_width() {
    let mut metadata = scalar_message_metadata();
    let message = metadata.messages[0].as_mut().unwrap();
    message.size = 12;
    message.entries = vec![
        MessageEntry {
            datatype: name(1, 4),
            datatype_id: 2,
            property: 1,
            offset: 4,
            size: 4,
            destination_size: 7,
        },
        entry(0, 8),
    ];
    let bytes = metadata_bytes(&metadata);
    let raw = [1u32, 4, 2, 4, 4, 7, 1]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    assert_eq!(
        bytes
            .windows(raw.len())
            .filter(|part| *part == raw.as_slice())
            .count(),
        1
    );
    let parsed = Metadata::parse(&bytes).unwrap();
    assert_eq!(parsed.messages[0].as_ref().unwrap().class_name, name(1, 5));
    assert_eq!(parsed.messages[0].as_ref().unwrap().class, 3);
    let first = &parsed.messages[0].as_ref().unwrap().entries[0];
    assert_eq!(first.datatype, name(1, 4));
    assert_eq!(first.datatype_id, 2);
    assert_eq!(first.property, 1);
    assert_eq!(first.offset, 4);
    assert_eq!(first.size, 4);
    assert_eq!(first.destination_size, 7);
    let second = &parsed.messages[0].as_ref().unwrap().entries[1];
    assert_eq!(second.datatype, name(1, 3));
    assert_eq!(second.datatype_id, 1);
    assert_eq!(second.property, 0);
    assert_eq!(second.offset, 8);
    assert_eq!(second.size, 4);
    assert_eq!(second.destination_size, 4);
    for (index, class) in parsed.classes.iter().enumerate() {
        assert_eq!(class.as_ref().unwrap().has_pointers, index == 2);
    }
}

#[test]
fn array_undo_ignores_message_members_for_other_properties() {
    for (label, kind, values, expected) in [
        ("message-unrelated-whole", 3, vec![90], vec![90]),
        ("message-unrelated-remove", 6, vec![], vec![11]),
        ("message-unrelated-update", 7, vec![21], vec![11, 21]),
    ] {
        let output = check_array_chain_with_metadata(
            label,
            &[10, 20],
            &[
                vec![],
                vec![
                    array_step(3, 0, &[11, 20], &[0]),
                    array_step(4, 0, &[55], &[]),
                ],
                vec![array_step(kind, 1, &values, &[1, 2])],
            ],
            &[&[11, 20], &expected],
            &[(2, &[0]), (3, &[0]), (6, &[2, 3])],
            scalar_message_metadata(),
        );
        let (restored, _) = crate::native::read_with_progress(&output, |_, _| true).unwrap();
        for frame in [1, 0, 1] {
            assert_eq!(
                restored.state_at(frame).unwrap().objects[&2].properties["Other"],
                crate::Value::U64(55),
                "{label} frame {frame}"
            );
        }
    }
}

#[test]
fn array_undo_does_not_clear_the_post_value_for_an_unrelated_message() {
    let source = array_chain_fixture_with_metadata(
        &[],
        &[
            vec![],
            vec![array_step(4, 0, &[55], &[])],
            vec![array_step(3, 0, &[10], &[1])],
        ],
        scalar_message_metadata(),
    );
    reject_array_chain(&source);
}

#[test]
fn repeated_scalar_message_members_leave_the_array_undo_value_unchanged() {
    let mut metadata = scalar_message_metadata();
    metadata.messages[0] = Some(Message {
        class_name: name(1, 5),
        class: 3,
        size: 8,
        entries: vec![entry(1, 0), entry(1, 4)],
    });
    let output = check_array_chain_with_metadata(
        "message-unrelated-repeated",
        &[10, 20],
        &[
            vec![],
            vec![
                array_step(3, 0, &[11, 20], &[0]),
                array_step(4, 0, &[55, 66], &[]),
            ],
            vec![array_step(3, 0, &[90], &[1, 2])],
        ],
        &[&[11, 20], &[90]],
        &[(2, &[0]), (3, &[0]), (6, &[2, 3])],
        metadata,
    );
    let (restored, _) = crate::native::read_with_progress(&output, |_, _| true).unwrap();
    for frame in [1, 0, 1] {
        assert_eq!(
            restored.state_at(frame).unwrap().objects[&2].properties["Other"],
            crate::Value::U64(66)
        );
    }
}

#[test]
fn unrelated_array_undo_messages_must_belong_to_the_current_instance() {
    let metadata = scalar_message_metadata();
    let values = 10u32.to_le_bytes();
    let mut body = RawInstance::create(&metadata, 3).unwrap();
    body.set(&metadata, 2, &values).unwrap();
    let baseline = BTreeMap::from([(2, body.clone()), (3, body)]);
    let scalar = 55u32.to_le_bytes();
    let replacement = 11u32.to_le_bytes();
    let events = vec![
        HistoryEvent::Marker,
        HistoryEvent::Set {
            kind: 3,
            id: 2,
            property: 2,
            data: values.as_slice().into(),
            undo: vec![1],
        },
        HistoryEvent::Set {
            kind: 4,
            id: 3,
            property: 0,
            data: scalar.as_slice().into(),
            undo: vec![2],
        },
        HistoryEvent::Array {
            kind: 7,
            id: 2,
            property: 2,
            index: 0,
            data: Some(&replacement),
            undo: vec![2, 1],
        },
    ];
    assert!(verify_undo(&metadata, &baseline, &events, &[], &mut || true).is_err());
}

#[test]
fn array_target_messages_are_rejected_before_frame_output_without_later_edits() {
    for entries in [vec![entry(2, 0)], vec![entry(1, 0), entry(2, 4)]] {
        let mut metadata = fixture_metadata();
        let count = entries.len();
        metadata.messages = vec![Some(Message {
            class_name: name(1, 5),
            class: 3,
            size: count * 4,
            entries,
        })];
        let source = array_chain_fixture_with_metadata(
            &[],
            &[vec![], vec![array_step(4, 0, &[55, 10][..count], &[0])]],
            metadata,
        );
        for first in [0, 1] {
            let mut output = Vec::new();
            let reason = trim_scene_frames(&source, first, 1, &mut output, || true)
                .unwrap_err()
                .to_string();
            assert!(reason.contains("array-target"), "{reason}");
            assert!(output.is_empty());
        }
        let mut whole = Vec::new();
        rewrite(&source, &mut whole, || true).unwrap();
        assert_eq!(whole, source);
    }
}

#[test]
fn inconsistent_scalar_message_layouts_are_rejected_before_middle_output() {
    for change in 0..9 {
        let mut metadata = scalar_message_metadata();
        let mut values = vec![55];
        match change {
            0 => metadata.messages[0].as_mut().unwrap().entries[0].datatype_id = u32::MAX,
            1 => metadata.messages[0].as_mut().unwrap().entries[0].datatype = name(1, 4),
            2 => metadata.messages[0].as_mut().unwrap().entries[0].destination_size = 8,
            3 => {
                metadata.strings.insert(13, "PvdI32".into());
                metadata.classes.push(Some(Class {
                    name: name(1, 13),
                    base: u32::MAX,
                    packing: crate::native::ClassPacking::default(),
                    size: 4,
                    properties: vec![],
                    dynamic: false,
                    has_pointers: false,
                    string_offsets: vec![],
                }));
                let entry = &mut metadata.messages[0].as_mut().unwrap().entries[0];
                entry.datatype = name(1, 13);
                entry.datatype_id = 5;
            }
            4 => metadata.properties[1].as_mut().unwrap().datatype = name(1, 4),
            5 => metadata.classes[1].as_mut().unwrap().dynamic = true,
            6 => metadata.messages[0].as_mut().unwrap().entries[0].offset = 4,
            7 => metadata.messages[0].as_mut().unwrap().entries[0].offset = u32::MAX as usize,
            8 => {
                let message = metadata.messages[0].as_mut().unwrap();
                message.size = 8;
                message.entries[0].size = 8;
                values.push(66);
            }
            _ => unreachable!(),
        }
        let source = array_chain_fixture_with_metadata(
            &[],
            &[vec![], vec![array_step(4, 0, &values, &[])]],
            metadata,
        );
        let mut output = Vec::new();
        assert!(
            trim_scene_frames(&source, 1, 1, &mut output, || true).is_err(),
            "change {change}"
        );
        assert!(output.is_empty(), "change {change}");
    }
}

#[test]
fn truncated_scalar_message_payload_is_rejected_before_middle_output() {
    let source = array_chain_fixture_with_metadata(
        &[],
        &[vec![], vec![array_step(4, 0, &[55], &[])]],
        scalar_message_metadata(),
    );
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    let HistoryEvent::Set { data, .. } = &mut layout.events[7] else {
        unreachable!();
    };
    *data = Cow::Borrowed(&[1, 2, 3]);
    let source = encode_modified(&mut layout);
    let mut output = Vec::new();
    assert!(trim_scene_frames(&source, 1, 1, &mut output, || true).is_err());
    assert!(output.is_empty());
}

#[test]
fn scalar_message_undo_keeps_its_post_value_for_previous_array_events() {
    for (label, kind, values, sources, expected) in [
        (
            "message-array-insert-noop",
            5,
            vec![15],
            vec![],
            vec![10, 15, 20],
        ),
        ("message-array-remove-noop", 6, vec![], vec![0], vec![10]),
        (
            "message-array-update-noop",
            7,
            vec![21],
            vec![0],
            vec![10, 21],
        ),
    ] {
        let output = check_array_chain_with_metadata(
            label,
            &[10, 20],
            &[
                vec![],
                vec![
                    array_step(4, 0, &[55], &[]),
                    array_step(kind, 1, &values, &sources),
                ],
                vec![array_step(4, 0, &[55], &[2])],
            ],
            &[&expected, &expected],
            &[(2, &[0]), (6, &[3])],
            scalar_message_metadata(),
        );
        let (restored, _) = crate::native::read_with_progress(&output, |_, _| true).unwrap();
        for frame in [1, 0, 1] {
            assert_eq!(
                restored.state_at(frame).unwrap().objects[&2].properties["Other"],
                crate::Value::U64(55)
            );
        }
        let source = array_chain_fixture_with_metadata(
            &[10, 20],
            &[
                vec![],
                vec![
                    array_step(4, 0, &[55], &[]),
                    array_step(kind, 1, &values, &sources),
                ],
                vec![array_step(4, 0, &[66], &[2])],
            ],
            scalar_message_metadata(),
        );
        reject_array_chain(&source);
    }
}

#[test]
fn scalar_message_undo_can_restore_a_whole_source_then_ignore_array_events() {
    let output = check_array_chain_with_metadata(
        "message-whole-and-array-noop",
        &[],
        &[
            vec![],
            vec![array_step(4, 0, &[55], &[]), array_step(5, 0, &[10], &[])],
            vec![array_step(4, 0, &[66], &[1, 2])],
        ],
        &[&[10], &[10]],
        &[(2, &[0]), (6, &[2, 3])],
        scalar_message_metadata(),
    );
    let (restored, _) = crate::native::read_with_progress(&output, |_, _| true).unwrap();
    for (frame, value) in [(1, 66), (0, 55), (1, 66)] {
        assert_eq!(
            restored.state_at(frame).unwrap().objects[&2].properties["Other"],
            crate::Value::U64(value)
        );
    }
}

#[test]
fn scanning_unrelated_message_entries_observes_cancellation() {
    let mut metadata = scalar_message_metadata();
    metadata.messages[0].as_mut().unwrap().entries = (0..10_000).map(|_| entry(1, 0)).collect();
    let values = 10u32.to_le_bytes();
    let mut body = RawInstance::create(&metadata, 3).unwrap();
    body.set(&metadata, 2, &values).unwrap();
    let baseline = BTreeMap::from([(2, body.clone())]);
    let scalar = 55u32.to_le_bytes();
    let events = vec![
        HistoryEvent::Marker,
        HistoryEvent::Set {
            kind: 4,
            id: 2,
            property: 0,
            data: scalar.as_slice().into(),
            undo: vec![1],
        },
        HistoryEvent::Set {
            kind: 3,
            id: 2,
            property: 2,
            data: values.as_slice().into(),
            undo: vec![1],
        },
    ];
    let mut checks = 0;
    let error = array_undo::verify(
        &metadata,
        &baseline,
        &events,
        2,
        &body,
        &mut array_undo::Budget::default(),
        &mut || {
            checks += 1;
            checks < 16
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("cancelled"), "{error}");
    assert_eq!(checks, 16);
}

#[test]
fn validating_message_fields_observes_cancellation_before_mutating_raw_state() {
    let mut metadata = scalar_message_metadata();
    metadata.messages[0].as_mut().unwrap().entries = (0..10_000).map(|_| entry(1, 0)).collect();
    let mut state = BTreeMap::from([(2, RawInstance::create(&metadata, 3).unwrap())]);
    let before = state.clone();
    let mut checks = 0;
    let failure = raw_apply(
        &metadata,
        &mut state,
        &HistoryEvent::Set {
            kind: 4,
            id: 2,
            property: 0,
            data: 55u32.to_le_bytes().as_slice().into(),
            undo: vec![1],
        },
        &mut 0,
        &mut || {
            checks += 1;
            checks < 16
        },
    )
    .unwrap_err();
    assert!(failure.to_string().contains("cancelled"), "{failure}");
    assert_eq!(checks, 16);
    assert_eq!(state, before);
}

#[test]
fn scalar_property_undo_keeps_the_post_value_for_an_unrelated_message() {
    for replacement in [0, 1] {
        let source = array_chain_fixture_with_metadata(
            &[],
            &[
                vec![],
                vec![array_step(4, 0, &[55], &[])],
                vec![array_step(3, 0, &[replacement], &[1])],
            ],
            scalar_message_metadata(),
        );
        let mut layout = Layout::parse(&source, &mut || true).unwrap();
        let HistoryEvent::Set { property, .. } = &mut layout.events[10] else {
            unreachable!();
        };
        *property = 0;
        let source = encode_modified(&mut layout);
        let mut output = Vec::new();
        let result = trim_scene_frames(&source, 1, 2, &mut output, || true);
        if replacement == 1 {
            assert!(result.is_err());
            assert!(output.is_empty());
            continue;
        }
        result.unwrap();
        let (restored, _) = crate::native::read_with_progress(&output, |_, _| true).unwrap();
        for frame in [1, 0, 1] {
            let state = restored.state_at(frame).unwrap();
            assert_eq!(state.objects[&2].properties["Value"], crate::Value::U64(0));
            assert_eq!(state.objects[&2].properties["Other"], crate::Value::U64(55));
        }
    }
}

fn derived_message_metadata() -> Metadata {
    let mut metadata = scalar_message_metadata();
    metadata.strings.insert(13, "DerivedBody".into());
    metadata.classes.push(Some(Class {
        name: name(1, 13),
        base: 3,
        packing: crate::native::ClassPacking::default(),
        size: 24,
        properties: vec![],
        dynamic: false,
        has_pointers: true,
        string_offsets: vec![],
    }));
    metadata
}

#[test]
fn message_owners_accept_derived_instances_and_preserve_inherited_fields() {
    let source = array_chain_fixture_with_metadata(
        &[],
        &[
            vec![],
            vec![array_step(4, 0, &[55], &[])],
            vec![array_step(4, 0, &[66], &[1])],
        ],
        derived_message_metadata(),
    );
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    layout.events[2] = HistoryEvent::Create(2, 5);
    let source = encode_modified(&mut layout);
    for first in [0, 1] {
        let mut output = Vec::new();
        trim_scene_frames(&source, first, 2, &mut output, || true).unwrap();
        let (restored, _) = crate::native::read_with_progress(&output, |_, _| true).unwrap();
        for frame in (0..restored.frame_count()).rev() {
            assert_eq!(
                restored.state_at(frame).unwrap().objects[&2].properties["Other"],
                crate::Value::U64([0, 55, 66][frame + first])
            );
        }
    }
}

#[test]
fn invalid_message_owners_reject_prefix_and_middle_output() {
    for change in 0..4 {
        let mut metadata = derived_message_metadata();
        let message = metadata.messages[0].as_mut().unwrap();
        match change {
            0 => message.class_name = name(1, 2),
            1 => {
                message.class = 1;
                message.class_name = name(1, 3);
            }
            2 => message.class = u32::MAX,
            3 => {
                message.class = 5;
                message.class_name = name(1, 13);
            }
            _ => unreachable!(),
        }
        let source = array_chain_fixture_with_metadata(
            &[],
            &[vec![], vec![array_step(4, 0, &[55], &[])]],
            metadata,
        );
        for first in [0, 1] {
            let mut output = Vec::new();
            assert!(
                trim_scene_frames(&source, first, 1, &mut output, || true).is_err(),
                "owner change {change} first {first}"
            );
            assert!(output.is_empty());
        }
    }
}

#[test]
fn message_owner_walk_rejects_missing_classes_cycles_depth_and_cancellation() {
    let mut metadata = derived_message_metadata();
    validate_message_instance(&metadata, 0, 3, &mut || true).unwrap();
    validate_message_instance(&metadata, 0, 5, &mut || true).unwrap();
    metadata.classes[5].as_mut().unwrap().base = 5;
    assert!(
        validate_message_instance(&metadata, 0, 5, &mut || true)
            .unwrap_err()
            .to_string()
            .contains("inheritance")
    );
    metadata.classes[5].as_mut().unwrap().base = 99;
    assert!(
        validate_message_instance(&metadata, 0, 5, &mut || true)
            .unwrap_err()
            .to_string()
            .contains("undefined")
    );
    for _ in 0..129 {
        let base = metadata.classes.len() as u32 - 1;
        metadata.classes.push(Some(Class {
            name: name(1, 13),
            base,
            packing: crate::native::ClassPacking::default(),
            size: 24,
            properties: vec![],
            dynamic: false,
            has_pointers: true,
            string_offsets: vec![],
        }));
    }
    let last = metadata.classes.len() as u32 - 1;
    assert!(
        validate_message_instance(&metadata, 0, last, &mut || true)
            .unwrap_err()
            .to_string()
            .contains("inheritance")
    );
    let mut checks = 0;
    let failure = validate_message_instance(&metadata, 0, last, &mut || {
        checks += 1;
        checks < 16
    })
    .unwrap_err();
    assert!(failure.to_string().contains("cancelled"), "{failure}");
    assert_eq!(checks, 16);
}
