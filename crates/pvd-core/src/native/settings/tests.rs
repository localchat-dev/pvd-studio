use super::*;
use crate::native::container_tests::{blob, file_history_config, frame, instance, word};

fn metadata() -> Metadata {
    let mut metadata = crate::native::container_tests::fixture();
    for (id, value) in [
        (14, "Camera"),
        (15, "PxVec3"),
        (16, "Origin"),
        (17, "Up"),
        (18, "Target"),
        (19, "captured camera"),
    ] {
        metadata.strings.insert(id, value.into());
    }
    let vector = Name {
        namespace: 1,
        name: 15,
    };
    let camera = Name {
        namespace: 0,
        name: 14,
    };
    metadata.classes.extend([
        Some(Class {
            name: vector,
            base: u32::MAX,
            packing: crate::native::ClassPacking::default(),
            size: 12,
            properties: vec![],
            dynamic: false,
            has_pointers: false,
            string_offsets: vec![],
        }),
        Some(Class {
            name: camera,
            base: u32::MAX,
            packing: crate::native::ClassPacking::default(),
            size: 40,
            properties: vec![3, 4, 5, 6],
            dynamic: false,
            has_pointers: true,
            string_offsets: vec![0],
        }),
    ]);
    for (name, datatype_id, datatype, offset32) in [
        (
            8,
            1,
            Name {
                namespace: 1,
                name: 3,
            },
            0,
        ),
        (16, 6, vector, 4),
        (17, 6, vector, 16),
        (18, 6, vector, 28),
    ] {
        metadata.properties.push(Some(Property {
            owner: camera,
            name,
            datatype,
            datatype_id,
            kind: 1,
            semantic: 0,
            offset32,
            values: vec![],
        }));
    }
    metadata.messages.push(Some(Message {
        class_name: camera,
        class: 7,
        size: 36,
        entries: [4, 5, 6]
            .into_iter()
            .enumerate()
            .map(|(index, property)| MessageEntry {
                datatype: vector,
                datatype_id: 6,
                property,
                offset: index * 12,
                size: 12,
                destination_size: 12,
            })
            .collect(),
    }));
    metadata
}

fn camera(x: f32) -> SourceCamera {
    SourceCamera {
        position: [x, 2.0, 3.0],
        up: [0.0, 1.0, 0.0],
        target: [4.0, 5.0, 6.0],
    }
}

fn vectors(camera: SourceCamera) -> Vec<u8> {
    camera
        .position
        .into_iter()
        .chain(camera.up)
        .chain(camera.target)
        .flat_map(f32::to_le_bytes)
        .collect()
}

fn camera_record(id: u32, value: SourceCamera) -> Vec<u8> {
    let mut fixed = 19u32.to_le_bytes().to_vec();
    fixed.extend(vectors(value));
    let mut body = Vec::new();
    blob(&mut body, &fixed);
    let mut bytes = Vec::new();
    word(&mut bytes, 7);
    word(&mut bytes, id);
    blob(&mut bytes, &body);
    bytes
}

fn message(value: SourceCamera) -> Vec<u8> {
    let mut bytes = Vec::new();
    for value in [4, 7, 0] {
        word(&mut bytes, value);
    }
    blob(&mut bytes, &vectors(value));
    word(&mut bytes, 0);
    bytes
}

fn name_event() -> Vec<u8> {
    let mut bytes = Vec::new();
    for value in [3, 7, 3] {
        word(&mut bytes, value);
    }
    blob(&mut bytes, &19u32.to_le_bytes());
    word(&mut bytes, 0);
    bytes
}

fn words(values: &[u32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

fn container() -> Vec<u8> {
    let mut metadata = crate::native::container_tests::metadata_bytes(&metadata());
    metadata.truncate(metadata.len() - 4);
    for value in [1, 1, 0, 14, 0, 14, 7, 0, 36, 3] {
        word(&mut metadata, value);
    }
    for (index, property) in [4, 5, 6].into_iter().enumerate() {
        for value in [1, 15, 6, index as u32 * 12, 12, 12, property] {
            word(&mut metadata, value);
        }
    }
    word(&mut metadata, 0);
    let mut comm = words(&[1]);
    comm.extend(1u64.to_le_bytes());
    comm.extend(1u64.to_le_bytes());
    word(&mut comm, 1);
    word(&mut comm, 1);
    comm.extend(0x111u64.to_le_bytes());
    word(&mut comm, 1);
    word(&mut comm, 7);
    comm.extend([10, 20, 30, 40, 0, 1]);
    word(&mut comm, 1);
    comm.extend(0x111u64.to_le_bytes());
    word(&mut comm, 1);
    comm.extend(words(&[1, 19, 7, 0, 0, 0, 0, 0]));
    let mut backing = vec![0; 64];
    let mut snapshot = words(&[2]);
    let mut saved = Vec::new();
    for (id, bytes) in [(1, instance(1, 7, &[])), (7, camera_record(7, camera(1.0)))] {
        let offset = backing.len() as u64;
        word(&mut snapshot, id);
        snapshot.extend(offset.to_le_bytes());
        saved.push((id, offset, bytes.len() as u32));
        backing.extend(bytes);
    }
    backing[..snapshot.len()].copy_from_slice(&snapshot);
    let events = vec![
        frame(100),
        message(camera(2.0)),
        frame(200),
        words(&[2, 7]),
        frame(300),
        words(&[1, 7, 7]),
        name_event(),
        message(camera(3.0)),
        frame(400),
        words(&[2, 7]),
        frame(500),
        words(&[1, 7, 7]),
        name_event(),
        message(camera(4.0)),
    ];
    backing.resize(next_page(backing.len()).unwrap(), 0);
    let page_offset = backing.len() as u64;
    let mut page = words(&[events.len() as u32]);
    let mut offset = 0;
    for event in &events {
        word(&mut page, offset);
        offset += event.len() as u32;
    }
    let count = events.len();
    for event in events {
        page.extend(event);
    }
    let page_len = page.len();
    backing.extend(page);
    let mut bytes = words(&[MAGIC, 1, metadata.len() as u32, comm.len() as u32]);
    bytes.extend(metadata);
    bytes.extend(comm);
    bytes.resize(next_page(bytes.len()).unwrap(), 0);
    bytes.extend((count as u64).to_le_bytes());
    word(&mut bytes, 1);
    bytes.extend(0u64.to_le_bytes());
    bytes.extend(page_offset.to_le_bytes());
    word(&mut bytes, count as u32);
    word(&mut bytes, page_len as u32);
    word(&mut bytes, 1);
    bytes.extend([0; 16]);
    word(&mut bytes, snapshot.len() as u32);
    word(&mut bytes, 0);
    word(&mut bytes, saved.len() as u32);
    for (id, offset, len) in saved {
        word(&mut bytes, id);
        bytes.extend(offset.to_le_bytes());
        word(&mut bytes, len);
    }
    bytes.resize(next_page(bytes.len()).unwrap(), 0);
    bytes.extend(backing);
    bytes
}

fn assert_frames(recording: &Recording) {
    assert_eq!(recording.frame_count(), 5);
    for frame in [0, 1, 2, 3, 4, 3, 2, 1, 0, 4] {
        let state = recording.state_at(frame).unwrap();
        let expected = match frame {
            0 => Some(camera(1.0)),
            1 => Some(camera(2.0)),
            3 => Some(camera(3.0)),
            _ => None,
        };
        assert_eq!(state.cameras.get("captured camera").copied(), expected);
        assert_eq!(
            state.objects[&1].properties["$color"],
            Value::Bytes(Arc::from([10, 20, 30, 40]))
        );
        assert_eq!(
            state.objects[&1].properties["$pickable"],
            Value::Bool(false)
        );
        assert_eq!(
            state.objects[&1].properties["$top_level"],
            Value::Bool(true)
        );
    }
    recording.verify_replay().unwrap();
}

#[test]
fn constructed_camera_container_replays_snapshots_messages_deletion_and_reuse() {
    let (recording, info) = read_with_progress(&container(), |_, _| true).unwrap();
    assert!(!info.source_ids.contains_key(&7));
    assert_frames(&recording);
    assert_eq!(
        recording.initial_state().cameras.get("captured camera"),
        Some(&camera(1.0))
    );
}

#[test]
fn constructed_camera_container_disk_history_preserves_reverse_removals() {
    let config = file_history_config("settings");
    let directory = config.directory.clone();
    let (recording, _) = import::read_source_with_progress(
        std::io::Cursor::new(container()),
        Some(config),
        |_, _| true,
    )
    .unwrap();
    assert_frames(&recording);
    let mut cursor = crate::live::ReplayCursor::default();
    let mut state = crate::SceneState::default();
    for index in [0, 1, 2, 3, 4, 3, 2, 1, 0] {
        cursor
            .seek(&recording, index, || false)
            .unwrap()
            .update
            .apply(&mut state);
        assert_eq!(state.cameras, recording.state_at(index).unwrap().cameras);
    }
    drop(cursor);
    drop(recording);
    std::fs::remove_dir_all(directory).unwrap();
}

fn settings(metadata: &Metadata, bytes: &[u8]) -> Result<Settings, Error> {
    let mut result = Settings::default();
    result.read_cameras(
        &mut Reader::new(bytes),
        metadata,
        &BTreeMap::new(),
        &mut 0,
        &mut || true,
    )?;
    Ok(result)
}

#[test]
fn camera_directory_rejects_ambiguous_references_schema_truncation_and_budget() {
    let metadata = metadata();
    for bytes in [
        words(&[0, 1, 99, 7]),
        words(&[0, 2, 19, 7, 19, 8]),
        words(&[0, 2, 19, 7, 10, 7]),
        words(&[0, 1, 19]),
    ] {
        assert!(settings(&metadata, &bytes).is_err());
    }
    let bytes = words(&[0, 1, 19, 7]);
    let mut result = settings(&metadata, &bytes).unwrap();
    let mut allocation = MAX_METADATA;
    assert!(
        result
            .validate_references()
            .unwrap_err()
            .to_string()
            .contains("missing instance")
    );
    assert!(
        result
            .read_cameras(
                &mut Reader::new(&bytes),
                &metadata,
                &BTreeMap::new(),
                &mut allocation,
                &mut || true
            )
            .is_err()
    );
    assert!(
        result
            .read_cameras(
                &mut Reader::new(&bytes),
                &metadata,
                &BTreeMap::new(),
                &mut 0,
                &mut || false
            )
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
    let mut metadata = metadata;
    metadata.properties[4].as_mut().unwrap().offset32 = 8;
    assert!(settings(&metadata, &bytes).is_err());
}

#[test]
fn display_presence_bits_do_not_publish_unset_defaults() {
    let mut builder = RecordingBuilder::new(Header {
        version: 1,
        timestamp_numerator: 1,
        timestamp_denominator: 1,
        flags: 0,
        endian: Endian::Little,
    });
    builder
        .action(Action::Create(crate::Object {
            id: 1,
            generation: 0,
            type_name: "sample".into(),
            properties: BTreeMap::new(),
        }))
        .unwrap();
    let mut settings = Settings::default();
    settings
        .read_display(1, &mut Reader::new(&[0; 10]), &mut 0)
        .unwrap();
    settings.apply_display(&mut builder, 1).unwrap();
    assert!(builder.state.objects[&1].properties.is_empty());
    let mut bytes = vec![0; 10];
    bytes[0] = 8;
    assert!(
        settings
            .read_display(1, &mut Reader::new(&bytes), &mut 0)
            .is_err()
    );
    bytes[0] = 1;
    bytes[8] = 2;
    assert!(
        settings
            .read_display(1, &mut Reader::new(&bytes), &mut 0)
            .is_err()
    );
}

fn camera_object(id: u64, generation: u32, value: SourceCamera) -> crate::Object {
    crate::Object {
        id,
        generation,
        type_name: "Camera".into(),
        properties: [
            ("Name".into(), Value::String("captured camera".into())),
            ("Origin".into(), Value::Vec3(value.position)),
            ("Up".into(), Value::Vec3(value.up)),
            ("Target".into(), Value::Vec3(value.target)),
        ]
        .into(),
    }
}

#[test]
fn camera_retirement_preserves_a_different_owner_generation_or_source_update() {
    let mut builder = RecordingBuilder::new(Header {
        version: 1,
        timestamp_numerator: 1,
        timestamp_denominator: 1,
        flags: 0,
        endian: Endian::Little,
    });
    let mut settings = settings(&metadata(), &words(&[0, 1, 19, 7])).unwrap();
    settings.cameras.insert(
        8,
        CameraEntry {
            name: "captured camera".into(),
            seen: false,
            generation: None,
        },
    );
    for id in [7, 8] {
        builder
            .action(Action::Create(camera_object(id, 0, camera(id as f32))))
            .unwrap();
        settings.sync_camera(&mut builder, id).unwrap();
    }
    builder.action(Action::Destroy(7)).unwrap();
    settings.sync_camera(&mut builder, 7).unwrap();
    assert_eq!(builder.state.cameras["captured camera"], camera(8.0));
    settings
        .owners
        .get_mut("captured camera")
        .unwrap()
        .generation = 1;
    builder.action(Action::Destroy(8)).unwrap();
    settings.sync_camera(&mut builder, 8).unwrap();
    assert_eq!(builder.state.cameras["captured camera"], camera(8.0));
    builder
        .action(Action::Create(camera_object(7, 1, camera(7.0))))
        .unwrap();
    settings.sync_camera(&mut builder, 7).unwrap();
    builder
        .action(Action::Camera("captured camera".into(), camera(9.0)))
        .unwrap();
    builder.action(Action::Destroy(7)).unwrap();
    settings.sync_camera(&mut builder, 7).unwrap();
    assert_eq!(builder.state.cameras["captured camera"], camera(9.0));
}

#[test]
fn camera_waits_for_complete_values_and_preserves_invalid_captured_vectors() {
    let mut object = camera_object(7, 0, camera(f32::from_bits(0x7fc0_1234)));
    assert_eq!(
        camera_value("captured camera", &object).unwrap(),
        Some(camera(f32::from_bits(0x7fc0_1234)))
    );
    object.properties.remove("Up");
    assert!(camera_value("captured camera", &object).unwrap().is_none());
    object
        .properties
        .insert("Name".into(), Value::String("other".into()));
    assert!(camera_value("captured camera", &object).is_err());
}
