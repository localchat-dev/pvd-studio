use super::*;

fn header() -> Header {
    Header {
        version: 1,
        timestamp_numerator: 1,
        timestamp_denominator: 1,
        flags: 1,
        endian: Endian::Little,
    }
}

fn name(id: u32) -> Name {
    Name {
        namespace: 6,
        name: id,
    }
}

fn group(events: Vec<Event>) -> EventGroup {
    EventGroup {
        stream_id: 1,
        timestamp: 0,
        offset: 0,
        events,
    }
}

fn property(class: u32, key: u32, datatype: u32, array: bool) -> Event {
    Event::CreateProperty {
        class: name(class),
        name: key,
        semantic: 0,
        datatype: name(datatype),
        kind: if array { 2 } else { 1 },
        named_values: Vec::new(),
    }
}

fn arg(property: u32, datatype: u32, offset: u32, size: u32) -> MessageArg {
    MessageArg {
        property,
        datatype: name(datatype),
        offset,
        size,
    }
}

fn message(args: Vec<MessageArg>, size: u32) -> Event {
    Event::CreatePropertyMessage {
        class: name(2),
        name: name(5),
        args,
        size,
    }
}

fn setup() -> RecordingBuilder {
    initialize(RecordingBuilder::new(header()))
}

fn initialize(mut builder: RecordingBuilder) -> RecordingBuilder {
    let mut events = [
        (1, "Parent"),
        (2, "Actor"),
        (3, "value"),
        (4, "PvdF32"),
        (5, "Message"),
        (6, "sdk"),
        (7, "String"),
        (8, "label"),
        (9, "ObjectRef"),
        (10, "target"),
        (11, "OpaqueThing"),
        (12, "blob"),
        (13, "StringHandle"),
        (14, "tag"),
        (15, "first"),
        (16, "frame"),
        (17, "OtherParent"),
        (18, "OtherActor"),
    ]
    .into_iter()
    .map(|(handle, value)| Event::StringHandle {
        handle,
        value: value.into(),
    })
    .collect::<Vec<_>>();
    events.extend([
        Event::CreateClass { name: name(1) },
        Event::CreateClass { name: name(2) },
        Event::CreateClass { name: name(17) },
        Event::CreateClass { name: name(18) },
        Event::DeriveClass {
            parent: name(1),
            child: name(2),
        },
        property(1, 3, 4, false),
        Event::CreateInstance {
            class: name(2),
            id: 1,
        },
        Event::CreateInstance {
            class: name(2),
            id: 2,
        },
        Event::CreateInstance {
            class: name(18),
            id: 3,
        },
        message(vec![arg(3, 4, 0, 4)], 4),
    ]);
    builder.push(group(events)).unwrap();
    builder
}

#[test]
fn cached_layout_tracks_inheritance_array_changes_and_actual_instance_class() {
    let mut builder = setup();
    builder
        .set_message(1, name(5), &1f32.to_le_bytes())
        .unwrap();
    assert_eq!(
        builder.state.objects[&1].properties["value"],
        Value::F32(1.0)
    );
    assert!(builder.message_cache_bytes > 0);
    builder.push(group(vec![property(1, 3, 4, true)])).unwrap();
    assert_eq!(builder.message_cache_bytes, 0);
    builder
        .set_message(1, name(5), &2f32.to_le_bytes())
        .unwrap();
    assert_eq!(
        builder.state.objects[&1].properties["value"],
        Value::Array(vec![Value::F32(2.0)].into())
    );
    builder
        .set_message(3, name(5), &3f32.to_le_bytes())
        .unwrap();
    assert_eq!(
        builder.state.objects[&3].properties["value"],
        Value::F32(3.0)
    );
    builder
        .push(group(vec![
            property(17, 3, 4, false),
            Event::DeriveClass {
                parent: name(17),
                child: name(2),
            },
        ]))
        .unwrap();
    builder
        .set_message(1, name(5), &4f32.to_le_bytes())
        .unwrap();
    assert_eq!(
        builder.state.objects[&1].properties["value"],
        Value::F32(4.0)
    );
    builder
        .push(group(vec![
            Event::DestroyInstance { id: 1 },
            Event::CreateInstance {
                class: name(18),
                id: 1,
            },
        ]))
        .unwrap();
    builder
        .set_message(1, name(5), &5f32.to_le_bytes())
        .unwrap();
    assert_eq!(builder.state.objects[&1].generation, 1);
    assert_eq!(
        builder.state.objects[&1].properties["value"],
        Value::F32(5.0)
    );
}

#[test]
fn cached_message_preserves_strings_opaque_values_and_reference_rebinding() {
    let mut builder = setup();
    builder
        .push(group(vec![message(
            vec![
                arg(3, 4, 0, 4),
                arg(10, 9, 4, 8),
                arg(12, 11, 12, 3),
                arg(8, 7, 15, 8),
                arg(14, 13, 23, 4),
            ],
            27,
        )]))
        .unwrap();
    let mut data = Vec::new();
    data.extend_from_slice(&1.25f32.to_le_bytes());
    data.extend_from_slice(&2u64.to_le_bytes());
    data.extend_from_slice(&[9, 8, 7]);
    data.extend_from_slice(&0u64.to_le_bytes());
    data.extend_from_slice(&15u32.to_le_bytes());
    data.extend_from_slice("中文标签\0".as_bytes());
    for _ in 0..2 {
        builder.set_message(1, name(5), &data).unwrap();
        assert_eq!(
            builder.state.objects[&1].properties["label"],
            Value::String("中文标签".into())
        );
        assert_eq!(
            builder.state.objects[&1].properties["tag"],
            Value::String("first".into())
        );
        assert_eq!(
            builder.state.objects[&1].properties["blob"],
            Value::Bytes(Arc::from([9, 8, 7]))
        );
    }
    assert_eq!(
        builder
            .state
            .resolve_reference(1, "target", 2)
            .unwrap()
            .generation,
        0
    );
    builder
        .push(group(vec![
            Event::DestroyInstance { id: 2 },
            Event::CreateInstance {
                class: name(2),
                id: 2,
            },
        ]))
        .unwrap();
    assert!(builder.state.resolve_reference(1, "target", 2).is_none());
    builder.set_message(1, name(5), &data).unwrap();
    assert_eq!(
        builder
            .state
            .resolve_reference(1, "target", 2)
            .unwrap()
            .generation,
        1
    );
    assert!(
        builder
            .set_message(1, name(5), &data[..data.len() - 1])
            .unwrap_err()
            .to_string()
            .contains("unterminated message string")
    );
    assert!(
        builder
            .set_message(1, name(5), &data[..26])
            .unwrap_err()
            .to_string()
            .contains("shorter than declared")
    );
    assert!(
        builder
            .set_message(999, name(5), &data)
            .unwrap_err()
            .to_string()
            .contains("missing instance")
    );
}

#[test]
fn metadata_and_message_redefinitions_release_the_compiled_cache() {
    let mut builder = setup();
    builder
        .set_message(1, name(5), &0f32.to_le_bytes())
        .unwrap();
    builder
        .push(group(vec![Event::StringHandle {
            handle: 3,
            value: "value".into(),
        }]))
        .unwrap();
    assert_eq!(builder.message_cache_bytes, 0);
    builder
        .set_message(1, name(5), &0f32.to_le_bytes())
        .unwrap();
    builder
        .push(group(vec![message(vec![arg(10, 9, 0, 8)], 8)]))
        .unwrap();
    assert_eq!(builder.message_cache_bytes, 0);
    builder
        .set_message(1, name(5), &2u64.to_le_bytes())
        .unwrap();
    assert_eq!(
        builder.state.objects[&1].properties["target"],
        Value::Ref(2)
    );
    assert!(
        builder
            .push(group(vec![Event::StringHandle {
                handle: 3,
                value: "different".into()
            }]))
            .unwrap_err()
            .to_string()
            .contains("redefined")
    );
    assert_eq!(builder.message_cache_bytes, 0);
}

#[test]
fn compiled_layouts_obey_cache_and_total_metadata_budgets() {
    let mut builder = setup();
    let before = builder.retained_bytes;
    assert!(builder.compile_message(1, name(5)).unwrap().is_some());
    assert!(builder.message_cache_bytes <= MESSAGE_CACHE_LIMIT);
    assert_eq!(builder.retained_bytes, before + builder.message_cache_bytes);
    builder.memory_limit = builder.retained_bytes + 1;
    builder.reserve_history(2).unwrap();
    assert_eq!(builder.message_cache_bytes, 0);
    assert_eq!(builder.retained_bytes, before + 2);
    builder.memory_limit = builder.retained_bytes;
    assert!(builder.compile_message(1, name(5)).unwrap().is_none());

    let mut builder = setup();
    let count = MESSAGE_CACHE_LIMIT / std::mem::size_of::<CompiledMessageArg>() + 1;
    builder
        .push(group(vec![message(vec![arg(3, 4, 0, 4); count], 4)]))
        .unwrap();
    assert!(builder.compile_message(1, name(5)).unwrap().is_none());
    assert_eq!(builder.message_cache_bytes, 0);
}

#[test]
fn streaming_state_takes_priority_over_optional_message_cache() {
    let directory = std::env::temp_dir().join(format!(
        "pvd-message-budget-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    let make = || {
        let mut builder = RecordingBuilder::new(header());
        builder.streaming = Some(Box::new(live::Streaming::new(
            live::HistoryWriter::new(live::HistoryConfig {
                directory: directory.clone(),
                min_free_disk_bytes: 0,
                ..Default::default()
            })
            .unwrap(),
        )));
        let mut builder = initialize(builder);
        builder
            .push(group(vec![message(vec![arg(8, 7, 0, 8)], 8)]))
            .unwrap();
        let state_bytes = builder.streaming.as_ref().unwrap().state_bytes;
        builder.memory_limit = builder.retained_bytes
            + builder.recording.frame_sources.index_bytes()
            + state_bytes
            + "label".len()
            + 96
            + 4096;
        builder
    };
    let mut cached = make();
    let mut uncached = make();
    let mut data = vec![0; 8];
    data.extend(std::iter::repeat_n(b'x', 4096));
    data.push(0);
    assert!(cached.compile_message(1, name(5)).unwrap().is_some());
    assert!(cached.message_cache_bytes > 0);
    cached.set_message(1, name(5), &data).unwrap();
    uncached.set_message_uncached(1, name(5), &data).unwrap();
    assert_eq!(cached.message_cache_bytes, 0);
    assert_eq!(cached.retained_bytes, uncached.retained_bytes);
    assert_eq!(cached.state, uncached.state);
    assert!(
        cached.retained_bytes + cached.streaming.as_ref().unwrap().state_bytes
            <= cached.memory_limit
    );
    drop(cached);
    drop(uncached);
    let _ = fs::remove_dir(directory);
}

#[test]
fn invalid_later_metadata_preserves_first_error_and_applied_prefix() {
    for first_size in [3u32, 4] {
        let mut cached = setup();
        let mut uncached = setup();
        for builder in [&mut cached, &mut uncached] {
            builder
                .push(group(vec![message(
                    vec![arg(3, 4, 0, first_size), arg(10, 999, first_size, 4)],
                    first_size + 4,
                )]))
                .unwrap();
        }
        let mut data = 2.5f32.to_le_bytes()[..first_size as usize].to_vec();
        data.extend_from_slice(&[0; 4]);
        let cached_error = cached
            .set_message(1, name(5), &data)
            .unwrap_err()
            .to_string();
        let reference_error = uncached
            .set_message_uncached(1, name(5), &data)
            .unwrap_err()
            .to_string();
        assert_eq!(cached_error, reference_error);
        assert_eq!(cached.state, uncached.state);
        if first_size == 3 {
            assert!(cached_error.contains("need 4 bytes"));
            assert!(!cached.state.objects[&1].properties.contains_key("value"));
        } else {
            assert!(cached_error.contains("undefined string handle 999"));
            assert_eq!(
                cached.state.objects[&1].properties["value"],
                Value::F32(2.5)
            );
        }
    }
}

#[test]
fn malformed_message_definitions_and_undefined_datatypes_still_fail() {
    let mut builder = setup();
    assert!(
        builder
            .push(group(vec![message(vec![arg(3, 4, 3, 4)], 4)]))
            .unwrap_err()
            .to_string()
            .contains("exceeds declared")
    );
    let mut builder = setup();
    builder
        .push(group(vec![message(vec![arg(3, 999, 0, 4)], 4)]))
        .unwrap();
    assert!(
        builder
            .set_message(1, name(5), &[0; 4])
            .unwrap_err()
            .to_string()
            .contains("undefined string handle 999")
    );
    let mut builder = setup();
    builder
        .push(group(vec![message(vec![arg(3, 4, 0, 3)], 3)]))
        .unwrap();
    assert!(
        builder
            .set_message(1, name(5), &[0; 3])
            .unwrap_err()
            .to_string()
            .contains("need 4 bytes")
    );
}

#[test]
fn scalar_fast_path_preserves_endian_empty_arrays_and_fixed_composites() {
    for endian in [Endian::Little, Endian::Big] {
        let header = Header { endian, ..header() };
        let float_bytes = |value: f32| match endian {
            Endian::Little => value.to_le_bytes(),
            Endian::Big => value.to_be_bytes(),
        };
        let strings = BTreeMap::new();
        let bytes = float_bytes(-3.5);
        assert_eq!(
            decode_value("sdk::PvdF32", &bytes, 1, false, &header, &strings).unwrap(),
            Value::F32(-3.5)
        );
        assert_eq!(
            decode_value("sdk::PvdF32", &bytes, 1, true, &header, &strings).unwrap(),
            Value::Array(vec![Value::F32(-3.5)].into())
        );
        assert_eq!(
            decode_value("sdk::PvdF32", &[], 0, false, &header, &strings).unwrap(),
            Value::Array(Vec::new().into())
        );
        let transform = [0., 0., 0., 1., 1., 2., 3.]
            .into_iter()
            .flat_map(float_bytes)
            .collect::<Vec<_>>();
        assert_eq!(
            decode_value("sdk::PxTransform", &transform, 1, false, &header, &strings).unwrap(),
            Value::Transform {
                rotation: [0., 0., 0., 1.],
                position: [1., 2., 3.]
            }
        );
        let matrix = (0..9)
            .flat_map(|n| float_bytes(n as f32))
            .collect::<Vec<_>>();
        assert_eq!(
            decode_value("sdk::PxMat33", &matrix, 1, false, &header, &strings).unwrap(),
            Value::Array(
                (0..9)
                    .map(|n| Value::F32(n as f32))
                    .collect::<Vec<_>>()
                    .into()
            )
        );
        assert!(decode_value("sdk::PvdF32", &bytes[..3], 1, false, &header, &strings).is_err());
        assert!(decode_value("sdk::PvdF32", &bytes, 1_000_001, false, &header, &strings).is_err());
    }
}

#[test]
fn borrowed_set_clears_reference_bindings_left_on_equal_scalars() {
    let mut builder = setup();
    builder
        .set_message(1, name(5), &1.25f32.to_le_bytes())
        .unwrap();
    for (id, key) in [(1, "before"), (1, "value"), (1, "zz_after"), (2, "value")] {
        builder.action(Action::Push(id, key.into(), 3)).unwrap();
    }
    let before = builder.recording.actions.len();
    builder
        .set_message(1, name(5), &1.25f32.to_le_bytes())
        .unwrap();
    assert_eq!(builder.recording.actions.len(), before + 1);
    assert_eq!(
        builder.state.objects[&1].properties["value"],
        Value::F32(1.25)
    );
    assert!(
        !builder
            .state
            .reference_lifetimes
            .contains_key(&(1, "value".into()))
    );
    for (id, key) in [(1, "before"), (1, "zz_after"), (2, "value")] {
        assert_eq!(
            builder.state.reference_lifetimes[&(id, key.into())].as_ref(),
            &BTreeMap::from([(3, 0)])
        );
    }
    builder
        .set_message(1, name(5), &1.25f32.to_le_bytes())
        .unwrap();
    assert_eq!(builder.recording.actions.len(), before + 1);

    builder.action(Action::Push(1, "value".into(), 3)).unwrap();
    builder
        .action(Action::Remove(1, "value".into(), 3))
        .unwrap();
    let before = builder.recording.actions.len();
    assert!(builder.state.reference_lifetimes[&(1, "value".into())].is_empty());
    builder
        .set_message(1, name(5), &1.25f32.to_le_bytes())
        .unwrap();
    assert_eq!(builder.recording.actions.len(), before);
}

#[test]
fn borrowed_set_preserves_capture_bits_and_exhausted_history_budget() {
    let mut builder = setup();
    let before = builder.recording.actions.len();
    for bits in [0x7fc00001u32, 0x7fc00001, 0x7fc00002, 0x80000000, 0] {
        builder
            .set_message(1, name(5), &bits.to_le_bytes())
            .unwrap();
        assert_eq!(
            builder.state.objects[&1].properties["value"],
            Value::F32(f32::from_bits(bits))
        );
    }
    assert_eq!(builder.recording.actions.len(), before + 4);
    builder.clear_message_cache();
    builder.memory_limit = builder.retained_bytes;
    let retained = builder.retained_bytes;
    let before = builder.recording.actions.len();
    for _ in 0..64 {
        builder
            .set_message(1, name(5), &0f32.to_le_bytes())
            .unwrap();
    }
    assert_eq!(builder.recording.actions.len(), before);
    assert_eq!(builder.retained_bytes, retained);
    let state = builder.state.clone();
    assert!(
        builder
            .set_message(1, name(5), &1f32.to_le_bytes())
            .unwrap_err()
            .to_string()
            .contains("memory budget")
    );
    assert_eq!(builder.state, state);
    assert_eq!(builder.retained_bytes, retained);
}

#[test]
fn borrowed_set_keeps_reference_generations_and_compound_bindings_exact() {
    let mut builder = setup();
    builder.set_value(1, "target", Value::Ref(2)).unwrap();
    builder.action(Action::Push(1, "target".into(), 3)).unwrap();
    assert_eq!(
        builder.state.reference_lifetimes[&(1, "target".into())].len(),
        2
    );
    builder.set_value(1, "target", Value::Ref(2)).unwrap();
    assert_eq!(
        builder.state.reference_lifetimes[&(1, "target".into())].as_ref(),
        &BTreeMap::from([(2, 0)])
    );

    let array = Value::Array(vec![Value::Ref(2), Value::Ref(2), Value::Ref(3)].into());
    builder.set_value(1, "targets", array.clone()).unwrap();
    let before = builder.recording.actions.len();
    builder.set_value(1, "targets", array.clone()).unwrap();
    assert_eq!(builder.recording.actions.len(), before);
    builder
        .push(group(vec![
            Event::DestroyInstance { id: 2 },
            Event::CreateInstance {
                class: name(2),
                id: 2,
            },
        ]))
        .unwrap();
    assert!(builder.state.resolve_reference(1, "target", 2).is_none());
    assert!(builder.state.resolve_reference(1, "targets", 2).is_none());
    let before = builder.recording.actions.len();
    builder.set_value(1, "target", Value::Ref(2)).unwrap();
    builder.set_value(1, "targets", array).unwrap();
    assert_eq!(builder.recording.actions.len(), before + 2);
    assert_eq!(
        builder.state.reference_lifetimes[&(1, "target".into())].as_ref(),
        &BTreeMap::from([(2, 1)])
    );
    assert_eq!(
        builder.state.reference_lifetimes[&(1, "targets".into())].as_ref(),
        &BTreeMap::from([(2, 1), (3, 0)])
    );
}

#[test]
fn borrowed_set_keeps_metadata_resolution_and_payload_validation_before_deduplication() {
    let mut builder = setup();
    let data = 1.25f32.to_le_bytes();
    builder.set_message(1, name(5), &data).unwrap();
    builder.push(group(vec![property(1, 3, 4, true)])).unwrap();
    builder.set_message(1, name(5), &data).unwrap();
    assert_eq!(
        builder.state.objects[&1].properties["value"],
        Value::Array(vec![Value::F32(1.25)].into())
    );
    builder
        .push(group(vec![
            Event::StringHandle {
                handle: 19,
                value: "PvdU32".into(),
            },
            property(1, 3, 19, false),
            message(vec![arg(3, 19, 0, 4)], 4),
        ]))
        .unwrap();
    builder.set_message(1, name(5), &data).unwrap();
    assert_eq!(
        builder.state.objects[&1].properties["value"],
        Value::U64(u32::from_le_bytes(data).into())
    );
    builder
        .push(group(vec![message(vec![arg(3, 19, 0, 3)], 3)]))
        .unwrap();
    let state = builder.state.clone();
    assert!(
        builder
            .set_message(1, name(5), &data[..3])
            .unwrap_err()
            .to_string()
            .contains("need 4 bytes")
    );
    assert_eq!(builder.state, state);
}

#[test]
fn borrowed_set_metrics_count_every_assignment_once_including_redundant_values() {
    let mut builder = setup();
    let mut metrics = Metrics::default();
    metrics.enable();
    builder.set_metrics(metrics.clone());
    for value in [1f32, 1., 2.] {
        builder
            .set_message(1, name(5), &value.to_le_bytes())
            .unwrap();
    }
    let snapshot = metrics.snapshot();
    let action = snapshot
        .stages
        .iter()
        .find(|stage| stage.stage == Stage::Action)
        .unwrap();
    let message = snapshot
        .stages
        .iter()
        .find(|stage| stage.stage == Stage::PropertyMessage)
        .unwrap();
    assert_eq!(action.calls, 3);
    assert_eq!(message.calls, 3);
    assert!(message.inclusive_ns >= action.inclusive_ns);
    assert_eq!(builder.skipped_redundant_actions, 1);
}

fn push_uncached(builder: &mut RecordingBuilder, group: EventGroup) {
    for event in group.events {
        match event {
            Event::SetPropertyMessage { id, message, data } => {
                builder.set_message_uncached(id, message, &data).unwrap()
            }
            Event::SendPropertyMessageFromGroup { id, data } => {
                let name = builder.groups[&group.stream_id];
                builder.set_message_uncached(id, name, &data).unwrap();
            }
            event => builder
                .push(EventGroup {
                    stream_id: group.stream_id,
                    timestamp: group.timestamp,
                    offset: group.offset,
                    events: vec![event],
                })
                .unwrap(),
        }
    }
}

#[test]
#[ignore = "requires an existing official PhysX 3.4 SDK capture; compares cached and uncached metadata resolution"]
fn official_sdk_capture_cached_messages_match_uncached_state_and_random_replay() {
    let path = std::env::var_os("PVD_MESSAGE_CAPTURE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../captures/physx-3.4.pvdstream")
        });
    let data = fs::read(path).unwrap();
    let mut decoder = Decoder::default();
    let mut cached: Option<RecordingBuilder> = None;
    let mut uncached: Option<RecordingBuilder> = None;
    let mut cached_groups = 0;
    for bytes in data.chunks(8191) {
        let groups = decoder.feed(bytes).unwrap();
        if cached.is_none()
            && let Some(header) = decoder.header()
        {
            cached = Some(RecordingBuilder::new(header.clone()));
            uncached = Some(RecordingBuilder::new(header.clone()));
        }
        for group in groups {
            let reference = EventGroup {
                stream_id: group.stream_id,
                timestamp: group.timestamp,
                offset: group.offset,
                events: group.events.clone(),
            };
            let builder = cached.as_mut().unwrap();
            builder.push(group).unwrap();
            cached_groups += usize::from(builder.message_cache_bytes > 0);
            push_uncached(uncached.as_mut().unwrap(), reference);
        }
    }
    decoder.finish().unwrap();
    assert!(
        cached_groups > 0,
        "capture must exercise cached property messages"
    );
    let cached = cached.unwrap().finish().unwrap();
    let uncached = uncached.unwrap().finish().unwrap();
    assert_eq!(cached.frame_count(), uncached.frame_count());
    assert!(cached.frame_count() > 0);
    for frame in [0, cached.frame_count() - 1, cached.frame_count() / 2, 0] {
        assert_eq!(
            cached.state_at(frame).unwrap(),
            uncached.state_at(frame).unwrap(),
            "frame {frame}"
        );
    }
    cached.verify_replay().unwrap();
}
