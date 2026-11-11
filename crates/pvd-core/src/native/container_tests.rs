use super::*;

pub(super) fn fixture() -> Metadata {
    let strings = [
        "",
        "physx3",
        "PvdU32",
        "StringHandle",
        "ObjectRef",
        "Base",
        "PxScene",
        "Value",
        "Name",
        "Peers",
        "sample",
        "_debugger_",
        "SectionEventData",
        "frame",
    ]
    .into_iter()
    .enumerate()
    .map(|(id, text)| (id as u32, text.into()))
    .collect();
    let name = |name| Name { namespace: 1, name };
    let class = |label, base, size, properties, dynamic| {
        Some(Class {
            name: label,
            base,
            packing: crate::native::ClassPacking::default(),
            size,
            properties,
            dynamic,
            has_pointers: false,
            string_offsets: Vec::new(),
        })
    };
    Metadata {
        strings,
        classes: vec![
            class(name(2), u32::MAX, 4, vec![], false),
            class(name(3), u32::MAX, 4, vec![], false),
            class(name(4), u32::MAX, 4, vec![], false),
            class(name(5), u32::MAX, 4, vec![0], false),
            class(name(6), 3, 20, vec![1, 2], true),
            class(
                Name {
                    namespace: 11,
                    name: 12,
                },
                u32::MAX,
                24,
                vec![],
                false,
            ),
        ],
        properties: vec![
            Some(Property {
                owner: name(5),
                name: 7,
                datatype: name(2),
                datatype_id: 0,
                kind: 1,
                semantic: 0,
                offset32: 0,
                values: vec![],
            }),
            Some(Property {
                owner: name(6),
                name: 8,
                datatype: name(3),
                datatype_id: 1,
                kind: 1,
                semantic: 0,
                offset32: 4,
                values: vec![],
            }),
            Some(Property {
                owner: name(6),
                name: 9,
                datatype: name(4),
                datatype_id: 2,
                kind: 2,
                semantic: 0,
                offset32: 8,
                values: vec![],
            }),
        ],
        messages: Vec::new(),
    }
}

fn builder(metadata: &Metadata) -> RecordingBuilder {
    let mut builder = RecordingBuilder::new(Header {
        version: 1,
        timestamp_numerator: 1,
        timestamp_denominator: 1,
        flags: 0,
        endian: Endian::Little,
    });
    metadata.register(&mut builder).unwrap();
    builder
}

pub(super) fn word(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend(value.to_le_bytes());
}

pub(super) fn blob(bytes: &mut Vec<u8>, data: &[u8]) {
    word(bytes, data.len() as u32);
    bytes.extend(data);
}

pub(super) fn instance(id: u32, value: u32, peers: &[u32]) -> Vec<u8> {
    let mut fixed = Vec::new();
    word(&mut fixed, value);
    word(&mut fixed, 10);
    fixed.extend([0xa5; 12]);
    let mut body = Vec::new();
    blob(&mut body, &fixed);
    blob(
        &mut body,
        &peers
            .iter()
            .flat_map(|peer| peer.to_le_bytes())
            .collect::<Vec<_>>(),
    );
    let mut bytes = Vec::new();
    word(&mut bytes, 4);
    word(&mut bytes, id);
    blob(&mut bytes, &body);
    bytes
}

fn snapshot_fixture() -> (Vec<u8>, Vec<u8>) {
    let mut backing = instance(1, 7, &[2]);
    let second = backing.len() as u64;
    backing.extend(instance(2, 11, &[1]));
    let mut directory = Vec::new();
    word(&mut directory, 2);
    word(&mut directory, 1);
    directory.extend(0u64.to_le_bytes());
    word(&mut directory, 2);
    directory.extend(second.to_le_bytes());
    (backing, directory)
}

#[test]
fn constructed_snapshot_restores_inherited_scalars_strings_and_mutual_references() {
    let metadata = fixture();
    let mut builder = builder(&metadata);
    let (backing, directory) = snapshot_fixture();
    assert_eq!(
        snapshot::restore(
            &metadata,
            &mut builder,
            &backing,
            &directory,
            &mut 0,
            &mut || true
        )
        .unwrap(),
        2
    );
    let recording = builder.finish().unwrap();
    assert_eq!(recording.frame_count(), 0);
    let state = recording.initial_state();
    assert_eq!(state.objects[&1].properties["Value"], Value::U64(7));
    assert_eq!(state.objects[&2].properties["Value"], Value::U64(11));
    assert_eq!(
        state.objects[&1].properties["Name"],
        Value::String("sample".into())
    );
    assert_eq!(
        state.objects[&1].properties["Peers"],
        Value::Array(vec![Value::Ref(2)].into())
    );
    assert_eq!(
        state.objects[&2].properties["Peers"],
        Value::Array(vec![Value::Ref(1)].into())
    );
    assert!(
        recording
            .diagnostics
            .iter()
            .any(|value| value.contains("initial snapshot restored 2"))
    );
}

#[test]
fn constructed_snapshot_rejects_invalid_directories_records_and_limits() {
    let metadata = fixture();
    let (backing, directory) = snapshot_fixture();
    let reject = |data: &[u8], directory: &[u8], expected: &str| {
        let mut target = builder(&metadata);
        let failure =
            snapshot::restore(&metadata, &mut target, data, directory, &mut 0, &mut || {
                true
            })
            .unwrap_err();
        assert!(failure.to_string().contains(expected), "{failure}");
    };
    let mut bad = directory.clone();
    bad[..4].copy_from_slice(&3u32.to_le_bytes());
    reject(&backing, &bad, "directory length");
    bad = directory.clone();
    bad[16..20].copy_from_slice(&1u32.to_le_bytes());
    reject(&backing, &bad, "duplicate");
    bad = directory.clone();
    bad[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
    reject(&backing, &bad, "range");
    let mut bad = backing.clone();
    bad[4..8].copy_from_slice(&3u32.to_le_bytes());
    reject(&bad, &directory, "ID differs");
    bad = backing.clone();
    bad[8..12].copy_from_slice(&((MAX_METADATA + 1) as u32).to_le_bytes());
    reject(&bad, &directory, "64 MiB");
    bad = backing.clone();
    bad[12..16].copy_from_slice(&21u32.to_le_bytes());
    reject(&bad, &directory, "fixed data size");
    reject(&backing[..backing.len() - 1], &directory, "range");
    let mut target = builder(&metadata);
    let mut allocation = MAX_METADATA;
    assert!(
        snapshot::restore(
            &metadata,
            &mut target,
            &backing,
            &directory,
            &mut allocation,
            &mut || true
        )
        .unwrap_err()
        .to_string()
        .contains("memory budget")
    );
    let mut calls = 0;
    let failure = snapshot::restore(
        &metadata,
        &mut target,
        &backing,
        &directory,
        &mut 0,
        &mut || {
            calls += 1;
            calls < 5
        },
    )
    .unwrap_err();
    assert_eq!(
        failure.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
}

#[test]
fn constructed_snapshot_rejects_cyclic_inheritance_and_dynamic_nested_fields() {
    let (backing, directory) = snapshot_fixture();
    for (cycle, expected) in [(true, "inheritance"), (false, "nested dynamic")] {
        let mut metadata = fixture();
        let mut target = builder(&metadata);
        if cycle {
            metadata.classes[3].as_mut().unwrap().base = 4;
        } else {
            metadata.classes[0].as_mut().unwrap().dynamic = true;
        }
        assert!(
            snapshot::restore(
                &metadata,
                &mut target,
                &backing,
                &directory,
                &mut 0,
                &mut || true
            )
            .unwrap_err()
            .to_string()
            .contains(expected)
        );
    }
}

#[test]
fn snapshot_event_buffers_are_values_without_replayed_analysis_side_effects() {
    let mut metadata = fixture();
    for (id, value) in [
        (14, "PxProfileMemoryEventBuffer"),
        (15, "PvdU8"),
        (16, "events"),
    ] {
        metadata.strings.insert(id, value.into());
    }
    let buffer_name = Name {
        namespace: 11,
        name: 14,
    };
    let byte_name = Name {
        namespace: 1,
        name: 15,
    };
    let buffer = metadata.classes[4].as_mut().unwrap();
    buffer.name = buffer_name;
    buffer.base = u32::MAX;
    buffer.size = 12;
    buffer.properties = vec![2];
    let byte = metadata.classes[2].as_mut().unwrap();
    byte.name = byte_name;
    byte.size = 1;
    metadata.properties[1] = None;
    let property = metadata.properties[2].as_mut().unwrap();
    property.owner = buffer_name;
    property.name = 16;
    property.offset32 = 0;
    property.datatype = byte_name;
    let mut body = Vec::new();
    blob(&mut body, &[0xa5; 12]);
    blob(&mut body, &[0xff]);
    let mut record = Vec::new();
    word(&mut record, 4);
    word(&mut record, 5);
    blob(&mut record, &body);
    let mut directory = Vec::new();
    word(&mut directory, 1);
    word(&mut directory, 5);
    directory.extend(0u64.to_le_bytes());
    let mut target = builder(&metadata);
    snapshot::restore(
        &metadata,
        &mut target,
        &record,
        &directory,
        &mut 0,
        &mut || true,
    )
    .unwrap();
    assert_eq!(
        target.state.objects[&5].properties["events"],
        Value::Bytes(vec![0xff].into())
    );
    assert!(
        target
            .recording
            .analysis
            .legacy()
            .unwrap()
            .memory
            .events()
            .is_empty()
    );
    assert!(
        target
            .recording
            .analysis
            .legacy()
            .unwrap()
            .memory
            .samples
            .is_empty()
    );
}

pub(super) fn metadata_bytes(metadata: &Metadata) -> Vec<u8> {
    let mut bytes = Vec::new();
    word(&mut bytes, 1);
    word(&mut bytes, metadata.classes.len() as u32);
    word(&mut bytes, metadata.strings.len() as u32 - 1);
    word(&mut bytes, metadata.strings.len() as u32);
    for (&id, value) in &metadata.strings {
        if id != 0 {
            word(&mut bytes, id);
            blob(&mut bytes, &[value.as_bytes(), &[0]].concat());
        }
    }
    word(&mut bytes, metadata.properties.len() as u32);
    for (index, entry) in metadata.properties.iter().enumerate() {
        word(&mut bytes, u32::from(entry.is_some()));
        let Some(property) = entry else { continue };
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
            word(&mut bytes, value);
        }
    }
    word(&mut bytes, metadata.classes.len() as u32);
    for (index, entry) in metadata.classes.iter().enumerate() {
        word(&mut bytes, u32::from(entry.is_some()));
        let Some(class) = entry else { continue };
        for value in [
            class.name.namespace,
            class.name.name,
            index as u32,
            class.base,
            class.packing.uniform_width as u32,
            class.packing.class_type as u32,
        ] {
            word(&mut bytes, value);
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
            word(&mut bytes, value);
        }
        word(&mut bytes, class.properties.len() as u32);
        for &id in &class.properties {
            word(&mut bytes, id);
        }
        word(&mut bytes, 0);
        word(&mut bytes, 0);
    }
    word(&mut bytes, 0);
    bytes
}

pub(super) fn frame(timestamp: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    word(&mut bytes, 8);
    word(&mut bytes, 5);
    let mut data = 0x111u64.to_le_bytes().to_vec();
    data.extend(timestamp.to_le_bytes());
    word(&mut data, 13);
    data.extend([0; 4]);
    blob(&mut bytes, &data);
    bytes
}

fn set(value: u32, undo: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for value in [3, 1, 0] {
        word(&mut bytes, value);
    }
    blob(&mut bytes, &value.to_le_bytes());
    word(&mut bytes, undo.len() as u32);
    for &value in undo {
        word(&mut bytes, value);
    }
    bytes
}

fn constructed_container() -> Vec<u8> {
    constructed_container_with_saved_event(false)
}

fn constructed_container_with_saved_event(saved_event: bool) -> Vec<u8> {
    let metadata = metadata_bytes(&fixture());
    let mut comm = Vec::new();
    word(&mut comm, 1);
    comm.extend(1u64.to_le_bytes());
    comm.extend(1u64.to_le_bytes());
    word(&mut comm, 2);
    for id in 1..=2 {
        word(&mut comm, id);
        comm.extend((u64::from(id) * 0x111).to_le_bytes());
        word(&mut comm, id);
        comm.extend([0; 10]);
    }
    for _ in 0..7 {
        word(&mut comm, 0);
    }
    let mut insert = Vec::new();
    for value in [5, 1, 2, 1] {
        word(&mut insert, value);
    }
    blob(&mut insert, &1u32.to_le_bytes());
    let mut remove = Vec::new();
    for value in [6, 1, 2, 0, 1, if saved_event { 2 } else { 1 }] {
        word(&mut remove, value);
    }
    let mut pages = [
        vec![set(8, &[]), insert, frame(100)],
        vec![
            set(9, &[u32::from(saved_event)]),
            remove,
            frame(200),
            [2u32.to_le_bytes(), 2u32.to_le_bytes()].concat(),
            frame(300),
        ],
    ];
    let mut backing = vec![0; 64];
    let mut directory = Vec::new();
    word(&mut directory, 2);
    let mut instances = Vec::new();
    for id in 1..=2 {
        let position = backing.len() as u64;
        let instance = instance(id, if id == 1 { 7 } else { 11 }, &[3 - id]);
        instances.push((id, position, instance.len() as u32));
        word(&mut directory, id);
        directory.extend(position.to_le_bytes());
        backing.extend(instance);
    }
    if saved_event {
        let position = backing.len() as u64;
        let saved = instance(1, 7, &[2]);
        instances.push((1, position, saved.len() as u32));
        backing.extend(saved);
        let mut event = Vec::new();
        word(&mut event, 10);
        word(&mut event, 1);
        event.extend(position.to_le_bytes());
        pages[0].insert(0, event);
    }
    backing[..directory.len()].copy_from_slice(&directory);
    let mut page_table = Vec::new();
    let mut index = 0;
    for events in pages {
        backing.resize(next_page(backing.len()).unwrap(), 0);
        let position = backing.len() as u64;
        let mut bytes = Vec::new();
        word(&mut bytes, events.len() as u32);
        let mut offset = 0;
        for event in &events {
            word(&mut bytes, offset);
            offset += event.len() as u32;
        }
        let count = events.len() as u32;
        for event in events {
            bytes.extend(event);
        }
        page_table.push((index, position, count, bytes.len() as u32));
        index += u64::from(count);
        backing.extend(bytes);
    }
    let mut bytes = Vec::new();
    for value in [MAGIC, 1, metadata.len() as u32, comm.len() as u32] {
        word(&mut bytes, value);
    }
    bytes.extend(metadata);
    bytes.extend(comm);
    bytes.resize(next_page(bytes.len()).unwrap(), 0);
    bytes.extend(index.to_le_bytes());
    word(&mut bytes, page_table.len() as u32);
    for (index, pos, count, len) in page_table {
        bytes.extend(index.to_le_bytes());
        bytes.extend(pos.to_le_bytes());
        word(&mut bytes, count);
        word(&mut bytes, len);
    }
    word(&mut bytes, 1);
    bytes.extend(0u64.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    word(&mut bytes, directory.len() as u32);
    word(&mut bytes, 0);
    word(&mut bytes, instances.len() as u32);
    for (id, pos, len) in instances {
        word(&mut bytes, id);
        bytes.extend(pos.to_le_bytes());
        word(&mut bytes, len);
    }
    bytes.resize(next_page(bytes.len()).unwrap(), 0);
    bytes.extend(backing);
    bytes
}

#[test]
fn constructed_nonempty_snapshot_and_two_pages_replay_across_boundaries() {
    let bytes = constructed_container();
    let (recording, info) = read_with_progress(&bytes, |_, _| true).unwrap();
    assert_eq!(
        (
            info.history_pages,
            info.history_events,
            recording.frame_count()
        ),
        (2, 8, 3)
    );
    assert_eq!(recording.source, RecordingSource::Pvd3Database);
    for frame in [0, 2, 1, 0, 2] {
        let state = recording.state_at(frame).unwrap();
        assert_eq!(
            state.objects[&1].properties["Value"],
            Value::U64(if frame == 0 { 8 } else { 9 })
        );
        assert_eq!(
            state.objects[&1].properties["Name"],
            Value::String("sample".into())
        );
        assert_eq!(
            state.objects[&1].properties["Peers"],
            Value::Array(
                if frame == 0 {
                    vec![Value::Ref(2), Value::Ref(1)]
                } else {
                    vec![Value::Ref(1)]
                }
                .into()
            )
        );
        assert_eq!(state.objects.contains_key(&2), frame < 2);
    }
    recording.verify_replay().unwrap();
    assert!(
        from_bytes_with_progress(&bytes, |event, _| event < 4)
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
}

pub(super) fn file_history_config(name: &str) -> crate::live::HistoryConfig {
    let directory = std::env::temp_dir().join(format!(
        "pvd-native-file-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    crate::live::HistoryConfig {
        directory,
        disk_budget_bytes: 32 << 20,
        segment_target_bytes: 1 << 20,
        segment_frames: 2,
        min_free_disk_bytes: 0,
    }
}

struct SparseContainer {
    bytes: Vec<u8>,
    length: u64,
    position: u64,
    read_bytes: usize,
    largest_read: usize,
}

impl std::io::Read for SparseContainer {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let length = output
            .len()
            .min(self.length.saturating_sub(self.position) as usize);
        output[..length].fill(0);
        if let Some(available) = self.bytes.get(self.position as usize..) {
            let copied = length.min(available.len());
            output[..copied].copy_from_slice(&available[..copied]);
        }
        self.read_bytes += length;
        self.largest_read = self.largest_read.max(length);
        self.position += length as u64;
        Ok(length)
    }
}

impl std::io::Seek for SparseContainer {
    fn seek(&mut self, from: std::io::SeekFrom) -> std::io::Result<u64> {
        let position = match from {
            std::io::SeekFrom::Start(position) => i128::from(position),
            std::io::SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
            std::io::SeekFrom::End(offset) => i128::from(self.length) + i128::from(offset),
        };
        self.position = u64::try_from(position)
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        Ok(self.position)
    }
}

#[test]
fn constructed_random_access_file_above_512_mib_reads_only_declared_blocks() {
    let bytes = constructed_container();
    let memory = Recording::from_bytes(&bytes).unwrap();
    let config = file_history_config("sparse");
    let root = config.directory.clone();
    let mut source = SparseContainer {
        bytes,
        length: 513 << 20,
        position: 0,
        read_bytes: 0,
        largest_read: 0,
    };
    let (disk, info) =
        import::read_source_with_progress(&mut source, Some(config), |_, _| true).unwrap();
    assert_eq!(info.source_bytes, 513 << 20);
    assert!(info.source_blake3.is_none());
    assert!(
        source.read_bytes < 128 << 10,
        "read {} bytes",
        source.read_bytes
    );
    assert!(source.largest_read <= 64 << 10);
    assert!(disk.is_disk_backed());
    assert_eq!(disk.frame_count(), 3);
    for frame in [0, 1, 2, 2, 1, 0] {
        assert_eq!(
            disk.state_at(frame).unwrap(),
            memory.state_at(frame).unwrap()
        );
        assert_eq!(
            crate::frame_metrics::source_context(&disk, 1, 0).unwrap(),
            0x111
        );
    }
    disk.verify_replay().unwrap();
    drop(disk);
    assert!(root.starts_with(std::env::temp_dir()));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn constructed_disk_import_rejects_cancelled_or_incomplete_history() {
    let bytes = constructed_container();
    let config = file_history_config("cancel");
    let root = config.directory.clone();
    let failure = import::read_source_with_progress(
        std::io::Cursor::new(&bytes),
        Some(config),
        |event, _| event < 4,
    )
    .unwrap_err();
    assert_eq!(
        failure.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
    if root.exists() {
        assert!(root.starts_with(std::env::temp_dir()));
        std::fs::remove_dir_all(root).unwrap();
    }
    let mut config = file_history_config("unavailable");
    let root = config.directory.clone();
    std::fs::create_dir(&root).unwrap();
    let unavailable = root.join("not-a-directory");
    std::fs::write(&unavailable, b"reserved").unwrap();
    config.directory = unavailable;
    let failure =
        import::read_source_with_progress(std::io::Cursor::new(&bytes), Some(config), |_, _| true)
            .unwrap_err();
    assert!(failure.to_string().contains("disk history"), "{failure}");
    assert!(root.starts_with(std::env::temp_dir()));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "requires original PVD3 baseline/rich containers with saved instance records"]
fn original_saved_instances_match_event_replay_before_destruction() {
    let mut compared = 0;
    let mut restored_instances = 0;
    for stem in ["baseline", "rich"] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../captures/native")
            .join(format!("{stem}-paired-native.pxd2"));
        let bytes = std::fs::read(path).unwrap();
        let mut reader = Reader::new(&bytes);
        assert_eq!(reader.u32().unwrap(), MAGIC);
        assert_eq!(reader.u32().unwrap(), 1);
        let metadata_len = reader.u32().unwrap() as usize;
        let communication_len = reader.u32().unwrap() as usize;
        let metadata = Metadata::parse(reader.take(metadata_len).unwrap()).unwrap();
        let mut comm = Reader::new(reader.take(communication_len).unwrap());
        assert_eq!(comm.u32().unwrap(), 1);
        comm.take(16).unwrap();
        let mut instance_sources = BTreeMap::new();
        let mut source_ids = BTreeMap::<u64, Vec<u64>>::new();
        for _ in 0..comm.count().unwrap() {
            let id = comm.u32().unwrap();
            let source = comm.u64().unwrap();
            assert_eq!(comm.u32().unwrap(), id);
            comm.take(10).unwrap();
            instance_sources.insert(u64::from(id), source);
            source_ids.entry(source).or_default().push(u64::from(id));
        }
        let catalog = profile_catalog(&mut comm, &instance_sources, &mut 0, || true).unwrap();
        reader
            .take(next_page(reader.pos).unwrap() - reader.pos)
            .unwrap();
        let total = reader.u64().unwrap();
        let mut pages = Vec::new();
        for _ in 0..reader.count().unwrap() {
            pages.push((
                reader.u64().unwrap(),
                reader.u64().unwrap(),
                reader.u32().unwrap(),
                reader.u32().unwrap(),
            ));
        }
        reader.words(24).unwrap();
        reader.words(16).unwrap();
        let backing = &bytes[next_page(reader.pos).unwrap()..];
        let mut replay = builder(&metadata);
        register_profile_catalog(&mut replay, catalog).unwrap();
        let mut index = 0;
        for (first, position, count, length) in pages {
            assert_eq!(first, index);
            let mut page =
                Reader::new(checked_range(backing, position, u64::from(length)).unwrap());
            assert_eq!(page.count().unwrap(), count as usize);
            let mut offsets = (0..count)
                .map(|_| page.u32().unwrap() as usize)
                .collect::<Vec<_>>();
            let payload = page.take(page.bytes.len() - page.pos).unwrap();
            offsets.push(payload.len());
            for window in offsets.windows(2) {
                let event = &payload[window[0]..window[1]];
                let mut saved = Reader::new(event);
                if saved.u32().unwrap() == 10 {
                    let id = saved.u32().unwrap();
                    let position = saved.u64().unwrap();
                    let mut directory = Vec::new();
                    word(&mut directory, 1);
                    word(&mut directory, id);
                    directory.extend(position.to_le_bytes());
                    let mut restored = builder(&metadata);
                    snapshot::restore(
                        &metadata,
                        &mut restored,
                        backing,
                        &directory,
                        &mut 0,
                        &mut || true,
                    )
                    .unwrap();
                    let expected = &replay.state.objects[&u64::from(id)];
                    let actual = &restored.state.objects[&u64::from(id)];
                    assert_eq!(actual.type_name, expected.type_name);
                    for (name, value) in &expected.properties {
                        assert_eq!(
                            &actual.properties[name], value,
                            "{stem}, instance {id}, event {index}, property {name}"
                        );
                        compared += 1;
                    }
                    restored_instances += 1;
                }
                import_event(
                    &metadata,
                    &source_ids,
                    &mut replay,
                    event,
                    index,
                    backing.len(),
                )
                .unwrap();
                index += 1;
            }
        }
        assert_eq!(index, total);
    }
    assert!(restored_instances >= 41);
    assert!(compared > 800);
    eprintln!(
        "original saved instances: {restored_instances} records, {compared} transmitted properties match history replay"
    );
}

struct SavedFixtureLayout {
    count: usize,
    rows: Vec<usize>,
    backing: usize,
    event: usize,
}

fn saved_fixture_layout(bytes: &[u8]) -> SavedFixtureLayout {
    let mut reader = Reader::new(bytes);
    reader.take(8).unwrap();
    let metadata = reader.u32().unwrap() as usize;
    let communication = reader.u32().unwrap() as usize;
    reader.take(metadata + communication).unwrap();
    reader
        .take(next_page(reader.pos).unwrap() - reader.pos)
        .unwrap();
    assert_eq!(reader.u64().unwrap(), 9);
    let count = reader.count().unwrap();
    let mut first = None;
    for _ in 0..count {
        reader.u64().unwrap();
        let position = reader.u64().unwrap() as usize;
        reader.take(8).unwrap();
        first.get_or_insert(position);
    }
    reader.words(24).unwrap();
    let count = reader.pos;
    let entries = reader.count().unwrap();
    let mut rows = Vec::new();
    for _ in 0..entries {
        rows.push(reader.pos);
        reader.take(16).unwrap();
    }
    let backing = next_page(reader.pos).unwrap();
    let page = backing + first.unwrap();
    let mut reader = Reader::new(&bytes[page..]);
    let count_events = reader.count().unwrap();
    assert_eq!(reader.u32().unwrap(), 0);
    reader.take((count_events - 1) * 4).unwrap();
    SavedFixtureLayout {
        count,
        rows,
        backing,
        event: page + reader.pos,
    }
}

fn reject_saved_container(bytes: &[u8], expected: &str) {
    let failure = read_with_progress(bytes, |_, _| true).unwrap_err();
    assert!(failure.to_string().contains(expected), "{failure}");
}

#[test]
fn saved_directory_binds_auxiliary_versions_without_changing_scene_or_analysis() {
    let original = constructed_container();
    let (expected, _) = read_with_progress(&original, |_, _| true).unwrap();
    let bytes = constructed_container_with_saved_event(true);
    let layout = saved_fixture_layout(&bytes);
    assert_eq!(layout.rows.len(), 3);
    let mut reordered = bytes.clone();
    let first = reordered[layout.rows[0]..layout.rows[0] + 16].to_vec();
    let last = reordered[layout.rows[2]..layout.rows[2] + 16].to_vec();
    reordered[layout.rows[0]..layout.rows[0] + 16].copy_from_slice(&last);
    reordered[layout.rows[2]..layout.rows[2] + 16].copy_from_slice(&first);
    for bytes in [&bytes, &reordered] {
        let (actual, info) = read_with_progress(bytes, |_, _| true).unwrap();
        assert_eq!((info.history_pages, info.history_events), (2, 9));
        assert_eq!(actual.frame_count(), expected.frame_count());
        for frame in [0, 2, 1, 0, 2] {
            assert_eq!(
                actual.state_at(frame).unwrap(),
                expected.state_at(frame).unwrap()
            );
            assert_eq!(
                postcard::to_stdvec(&actual.analysis_at(frame).unwrap()).unwrap(),
                postcard::to_stdvec(&expected.analysis_at(frame).unwrap()).unwrap()
            );
        }
        actual.verify_replay().unwrap();
    }
}

#[test]
fn saved_directory_rejects_auxiliary_header_length_and_identity_corruption() {
    let bytes = constructed_container_with_saved_event(true);
    let layout = saved_fixture_layout(&bytes);
    let row = layout.rows[2];
    let position = u64::from_le_bytes(bytes[row + 4..row + 12].try_into().unwrap()) as usize;
    let record = layout.backing + position;
    let length = u32::from_le_bytes(bytes[row + 12..row + 16].try_into().unwrap());
    let mut bad = bytes.clone();
    bad[row..row + 4].copy_from_slice(&99u32.to_le_bytes());
    reject_saved_container(&bad, "ID differs from its directory");
    bad = bytes.clone();
    bad[record..record + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    reject_saved_container(&bad, "undefined PVD3 class");
    bad = bytes.clone();
    bad[record + 4..record + 8].copy_from_slice(&99u32.to_le_bytes());
    reject_saved_container(&bad, "ID differs from its directory");
    bad = bytes.clone();
    bad[record + 8..record + 12].copy_from_slice(&(length - 11).to_le_bytes());
    reject_saved_container(&bad, "truncated");
    for (len, expected) in [(length - 1, "truncated"), (length + 1, "trailing")] {
        bad = bytes.clone();
        bad[row + 12..row + 16].copy_from_slice(&len.to_le_bytes());
        reject_saved_container(&bad, expected);
    }
    bad = bytes.clone();
    bad[row + 4..row + 12].copy_from_slice(&u64::MAX.to_le_bytes());
    reject_saved_container(&bad, "range");
    let first = &bytes[layout.rows[0] + 4..layout.rows[0] + 12];
    bad = bytes.clone();
    bad[row..row + 4].copy_from_slice(&2u32.to_le_bytes());
    bad[row + 4..row + 12].copy_from_slice(first);
    reject_saved_container(&bad, "conflicting PVD3 saved instance directory");
}

#[test]
fn saved_auxiliary_events_reject_in_range_unlisted_or_mismatched_targets() {
    let bytes = constructed_container_with_saved_event(true);
    let layout = saved_fixture_layout(&bytes);
    let mut bad = bytes.clone();
    bad[layout.event + 4..layout.event + 8].copy_from_slice(&99u32.to_le_bytes());
    reject_saved_container(&bad, "event 0: PVD3 saved instance event differs");
    let second = u64::from_le_bytes(
        bytes[layout.rows[1] + 4..layout.rows[1] + 12]
            .try_into()
            .unwrap(),
    );
    for position in [0, second, second + 1] {
        bad = bytes.clone();
        bad[layout.event + 8..layout.event + 16].copy_from_slice(&position.to_le_bytes());
        reject_saved_container(&bad, "event 0: PVD3 saved instance event differs");
    }
}

#[test]
fn saved_directory_metadata_and_cancellation_fail_before_import_is_published() {
    let bytes = constructed_container_with_saved_event(true);
    let layout = saved_fixture_layout(&bytes);
    let count = MAX_METADATA / 128 + 1;
    let mut amplified = bytes.clone();
    amplified[layout.count..layout.count + 4].copy_from_slice(&(count as u32).to_le_bytes());
    amplified.resize(layout.count + 4 + count, 0);
    reject_saved_container(&amplified, "metadata");
    let mut checks = 0;
    let failure = read_with_progress(&bytes, |event, total| {
        if event == 0 && total == 9 {
            checks += 1;
        }
        checks < 4
    })
    .unwrap_err();
    assert!(failure.to_string().contains("cancelled"), "{failure}");
    assert_eq!(checks, 4);
}

#[test]
fn origin_shift_event_consumes_bounded_auxiliary_rows_before_updating_state() {
    let metadata = fixture();
    let mut event = Vec::new();
    for value in [
        11,
        1,
        1.25f32.to_bits(),
        (-2.5f32).to_bits(),
        0.0f32.to_bits(),
        2,
    ] {
        word(&mut event, value);
    }
    event.extend([0u8; 16]);
    for length in 0..=event.len() {
        let mut builder = builder(&metadata);
        builder
            .action(Action::Create(crate::Object {
                id: 1,
                generation: 0,
                type_name: "physx3::PxScene".into(),
                properties: BTreeMap::new(),
            }))
            .unwrap();
        let before = builder.state.clone();
        let result = import_event(
            &metadata,
            &BTreeMap::new(),
            &mut builder,
            &event[..length],
            0,
            0,
        );
        if length == event.len() {
            result.unwrap();
            assert_eq!(
                builder.state.objects[&1].properties["$origin_shift"],
                Value::Vec3([1.25, -2.5, 0.0])
            );
        } else {
            assert!(result.is_err(), "length {length}");
            assert_eq!(builder.state, before);
        }
    }
    for (offset, value) in [(4, 9), (8, f32::NAN.to_bits()), (20, u32::MAX)] {
        let mut builder = builder(&metadata);
        builder
            .action(Action::Create(crate::Object {
                id: 1,
                generation: 0,
                type_name: "physx3::PxScene".into(),
                properties: BTreeMap::new(),
            }))
            .unwrap();
        let before = builder.state.clone();
        let mut malformed = event.clone();
        malformed[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        assert!(import_event(&metadata, &BTreeMap::new(), &mut builder, &malformed, 0, 0).is_err());
        assert_eq!(builder.state, before);
    }
}
