use super::*;
use std::{fs, sync::atomic::Ordering};

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "pvd-external-cache-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        )))
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        assert_eq!(fs::read_dir(&self.0).unwrap().count(), 0);
        fs::remove_dir(&self.0).unwrap();
    }
}

struct Store {
    reader: HistoryReader,
    writer: SegmentWriter,
    resources: Resources,
    _owner: Arc<CacheDirectory>,
    _directory: Directory,
}

impl Store {
    fn new() -> Self {
        let directory = Directory::new();
        let owner = CacheDirectory::new(&directory.0).unwrap();
        let writer = SegmentWriter::new(&owner, 0).unwrap();
        let resources = Resources::new(owner.clone(), 32 << 20);
        let reader = HistoryReader {
            buffer: BufReader::new(File::open(&writer.view.path).unwrap()),
            capture: None,
            position: None,
            resources: Some(resources.reader()),
            _segment_guard: None,
        };
        Self {
            reader,
            writer,
            resources,
            _owner: owner,
            _directory: directory,
        }
    }

    fn save(&self, record: &Record, dependencies: &[ResourceHandle]) -> ResourceHandle {
        let mut plan = ResourcePlan::default();
        let handle = self
            .resources
            .plan(
                &postcard::to_stdvec(record).unwrap(),
                dependencies,
                &mut plan,
            )
            .unwrap();
        self.resources.commit(&plan).unwrap();
        handle
    }
}

fn array(value: u64, len: usize) -> Value {
    Value::Array(Arc::new(vec![Value::Ref(value); len]))
}

fn array_arc(value: &Value) -> &Arc<Vec<Value>> {
    let Value::Array(value) = value else {
        panic!("expected array")
    };
    value
}

fn bytes(value: Value) -> Vec<u8> {
    postcard::to_stdvec(&Record::Value(value)).unwrap()
}

#[test]
fn allowance_counts_capacity_nested_payloads_and_checked_limits() {
    let mut text = String::with_capacity(1000);
    text.push('x');
    let text_capacity = text.capacity();
    let mut children = Vec::with_capacity(100);
    children.push(Value::String(text));
    children.push(Value::Bytes(vec![1; 700].into()));
    let capacity = children.capacity();
    let value = Value::Array(Arc::new(children));
    let expected = ARC_ALLOWANCE
        + size_of::<Vec<Value>>()
        + capacity * size_of::<Value>()
        + text_capacity
        + ARC_ALLOWANCE
        + 700;
    assert_eq!(
        value_allowance(&value, expected, &mut || false).unwrap(),
        Some(expected)
    );
    assert_eq!(
        value_allowance(&value, expected - 1, &mut || false).unwrap(),
        None
    );
    assert_eq!(reference_allowance(usize::MAX), None);
    assert_eq!(
        value_allowance(&Value::String("x".into()), usize::MAX, &mut || false).unwrap(),
        None
    );
    let mut nested = array(1, 1);
    for _ in 0..DEPTH_LIMIT {
        nested = Value::Array(Arc::new(vec![nested]));
    }
    assert_eq!(
        value_allowance(&nested, usize::MAX, &mut || false).unwrap(),
        None
    );
    assert_eq!(
        value_allowance(&array(1, VISIT_LIMIT), usize::MAX, &mut || false).unwrap(),
        None
    );
    assert!(value_allowance(&value, usize::MAX, &mut || true).is_err());
}

#[test]
fn lru_has_fixed_slots_and_eviction_releases_only_the_cache_owner() {
    let mut cache = ExternalCache::default();
    let held: Arc<[u8]> = vec![1; 8].into();
    let weak = Arc::downgrade(&held);
    let digest = blake3::hash(b"entry");
    for id in 0..SLOTS {
        cache.insert(
            RESOURCE_ID + id as u64,
            Kind::Value(PropertyLayout::General),
            digest,
            40,
            || Payload::Value(Value::Bytes(held.clone())),
        );
    }
    assert_eq!(cache.slots.iter().flatten().count(), SLOTS);
    assert!(
        cache
            .get(RESOURCE_ID, Kind::Value(PropertyLayout::General), digest)
            .is_some()
    );
    cache.insert(
        RESOURCE_ID + SLOTS as u64,
        Kind::Value(PropertyLayout::General),
        digest,
        40,
        || Payload::Value(Value::Bytes(held.clone())),
    );
    assert!(
        cache
            .get(
                RESOURCE_ID + 1,
                Kind::Value(PropertyLayout::General),
                digest
            )
            .is_none()
    );
    assert!(
        cache
            .get(RESOURCE_ID, Kind::Value(PropertyLayout::General), digest)
            .is_some()
    );
    assert_eq!(cache.bytes, size_of::<ExternalCache>() + SLOTS * 40);
    for _ in 0..1000 {
        cache
            .get(RESOURCE_ID, Kind::Value(PropertyLayout::General), digest)
            .unwrap();
    }
    assert!(cache.slots.iter().flatten().all(|entry| entry.age < SLOTS));
    drop(cache);
    assert_eq!(Arc::strong_count(&held), 1);
    assert!(weak.upgrade().is_some());
    drop(held);
    assert!(weak.upgrade().is_none());
}

#[test]
fn byte_and_single_entry_limits_bypass_without_losing_decoded_values() {
    let store = Store::new();
    let payload = bytes(array(7, 100));
    let value: Record = postcard::from_bytes(&payload).unwrap();
    let Record::Value(value) = value else {
        unreachable!()
    };
    let cost = value_allowance(&value, usize::MAX, &mut || false)
        .unwrap()
        .unwrap();
    let mut cache = ExternalCache {
        byte_limit: size_of::<ExternalCache>() + cost,
        entry_limit: cost,
        ..Default::default()
    };
    let first = cache
        .value_bytes(
            &store.reader,
            RESOURCE_ID + 1,
            &payload,
            PropertyLayout::General,
            &mut || false,
        )
        .unwrap();
    assert_eq!(first, value);
    assert_eq!(cache.bytes, size_of::<ExternalCache>() + cost);
    cache
        .value_bytes(
            &store.reader,
            RESOURCE_ID + 2,
            &payload,
            PropertyLayout::General,
            &mut || false,
        )
        .unwrap();
    assert_eq!(cache.slots.iter().flatten().count(), 1);
    assert_eq!(
        cache.slots.iter().flatten().next().unwrap().id,
        RESOURCE_ID + 2
    );
    cache.entry_limit = cost - 1;
    assert_eq!(
        cache
            .value_bytes(
                &store.reader,
                RESOURCE_ID + 3,
                &payload,
                PropertyLayout::General,
                &mut || false
            )
            .unwrap(),
        value
    );
    assert_eq!(cache.slots.iter().flatten().count(), 1);
    let mut too_small = ExternalCache {
        byte_limit: size_of::<ExternalCache>() + cost - 1,
        ..Default::default()
    };
    assert_eq!(
        too_small
            .value_bytes(
                &store.reader,
                RESOURCE_ID + 1,
                &payload,
                PropertyLayout::General,
                &mut || false
            )
            .unwrap(),
        value
    );
    assert_eq!(too_small.bytes, size_of::<ExternalCache>());
}

#[test]
fn complete_record_digest_and_kind_are_required_even_for_the_same_id() {
    let store = Store::new();
    let mut cache = ExternalCache::default();
    let id = RESOURCE_ID + 1;
    let first_bytes = bytes(array(1, 4));
    let first = cache
        .value_bytes(
            &store.reader,
            id,
            &first_bytes,
            PropertyLayout::General,
            &mut || false,
        )
        .unwrap();
    let second = cache
        .value_bytes(
            &store.reader,
            id,
            &first_bytes,
            PropertyLayout::General,
            &mut || false,
        )
        .unwrap();
    assert!(Arc::ptr_eq(array_arc(&first), array_arc(&second)));
    let changed = cache
        .value_bytes(
            &store.reader,
            id,
            &bytes(array(2, 4)),
            PropertyLayout::General,
            &mut || false,
        )
        .unwrap();
    assert_eq!(changed, array(2, 4));
    assert!(!Arc::ptr_eq(array_arc(&first), array_arc(&changed)));
    assert!(
        cache
            .reference_bytes(&store.reader, id, &first_bytes, &mut || false)
            .is_err()
    );
    let references = postcard::to_stdvec(&Record::References([(1, 2)].into())).unwrap();
    assert!(
        cache
            .value_bytes(
                &store.reader,
                id,
                &references,
                PropertyLayout::General,
                &mut || false
            )
            .is_err()
    );
    let mut trailing = first_bytes.clone();
    trailing.push(0);
    assert!(
        cache
            .value_bytes(
                &store.reader,
                id,
                &trailing,
                PropertyLayout::General,
                &mut || false
            )
            .is_err()
    );
    assert!(
        cache
            .value_bytes(
                &store.reader,
                id,
                &first_bytes,
                PropertyLayout::General,
                &mut || true
            )
            .is_err()
    );
    let first_low = cache
        .value_bytes(
            &store.reader,
            40,
            &first_bytes,
            PropertyLayout::General,
            &mut || false,
        )
        .unwrap();
    let second_low = cache
        .value_bytes(
            &store.reader,
            40,
            &first_bytes,
            PropertyLayout::General,
            &mut || false,
        )
        .unwrap();
    assert!(!Arc::ptr_eq(array_arc(&first_low), array_arc(&second_low)));
    assert!(
        cache
            .slots
            .iter()
            .flatten()
            .all(|entry| entry.id & RESOURCE_ID != 0)
    );
}

#[test]
fn a_real_validated_hit_still_rejects_corrupted_or_truncated_resource_blocks() {
    for truncate in [false, true] {
        let mut store = Store::new();
        let handle = store.save(&Record::Value(array(5, 1000)), &[]);
        let mut cache = ExternalCache::default();
        let first = cache
            .property(
                &mut store.reader,
                handle.id(),
                0,
                PropertyLayout::General,
                &mut || false,
            )
            .unwrap();
        let second = cache
            .property(
                &mut store.reader,
                handle.id(),
                0,
                PropertyLayout::General,
                &mut || false,
            )
            .unwrap();
        assert!(Arc::ptr_eq(array_arc(&first), array_arc(&second)));
        let path = fs::read_dir(&store._owner.path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_stem()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.parse::<u64>().ok())
                    .is_some_and(|id| id & (1 << 62) != 0)
            })
            .unwrap();
        let mut file = fs::OpenOptions::new().write(true).open(path).unwrap();
        if truncate {
            file.set_len(8).unwrap();
        } else {
            file.seek(SeekFrom::Start(44)).unwrap();
            file.write_all(&[0xff]).unwrap();
            file.flush().unwrap();
        }
        drop(file);
        store.reader.resources = Some(store.resources.reader());
        assert!(
            cache
                .property(
                    &mut store.reader,
                    handle.id(),
                    0,
                    PropertyLayout::General,
                    &mut || false
                )
                .is_err()
        );
        assert_eq!(first, array(5, 1000));
    }
}

#[test]
fn cache_does_not_commit_pending_resources_or_keep_expired_resources_alive() {
    let mut store = Store::new();
    let payload = bytes(array(1, 100));
    let mut plan = ResourcePlan::default();
    let handle = store.resources.plan(&payload, &[], &mut plan).unwrap();
    let id = handle.id();
    let mut cache = ExternalCache::default();
    cache
        .value_bytes(
            &store.reader,
            id,
            &payload,
            PropertyLayout::General,
            &mut || false,
        )
        .unwrap();
    assert!(
        cache
            .property(
                &mut store.reader,
                id,
                0,
                PropertyLayout::General,
                &mut || false
            )
            .is_err()
    );
    store.resources.commit(&plan).unwrap();
    let decoded = cache
        .property(
            &mut store.reader,
            id,
            0,
            PropertyLayout::General,
            &mut || false,
        )
        .unwrap();
    drop(plan);
    let lease = ResourceLease::new(vec![handle]);
    lease.invalidate().unwrap();
    store.resources.collect().unwrap();
    assert_eq!(store.resources.physical_bytes(), 0);
    assert!(
        cache
            .property(
                &mut store.reader,
                id,
                0,
                PropertyLayout::General,
                &mut || false
            )
            .is_err()
    );
    assert_eq!(decoded, array(1, 100));
    assert!(cache.slots.iter().flatten().count() > 0);
}

#[test]
fn reference_hits_preserve_generations_and_low_offsets_never_cross_files() {
    let mut first = Store::new();
    let mut second = Store::new();
    let mut cache = ExternalCache::default();
    let references = first.save(&Record::References([(7, 3)].into()), &[]);
    let before = cache
        .references(&mut first.reader, references.id(), 0, &mut || false)
        .unwrap();
    let repeated = cache
        .references(&mut first.reader, references.id(), 0, &mut || false)
        .unwrap();
    assert!(Arc::ptr_eq(&before, &repeated));
    let newer = first.save(&Record::References([(7, 4)].into()), &[]);
    let after = cache
        .references(&mut first.reader, newer.id(), 0, &mut || false)
        .unwrap();
    assert_eq!(before[&7], 3);
    assert_eq!(after[&7], 4);
    assert!(!Arc::ptr_eq(&before, &after));

    let offset_a = first.writer.write(&Record::Value(array(10, 20))).unwrap();
    let offset_b = second.writer.write(&Record::Value(array(99, 20))).unwrap();
    assert_eq!(offset_a, offset_b);
    assert_eq!(offset_a & RESOURCE_ID, 0);
    first.writer.file.flush().unwrap();
    second.writer.file.flush().unwrap();
    let a = cache
        .property(
            &mut first.reader,
            offset_a,
            first.writer.view.bytes,
            PropertyLayout::General,
            &mut || false,
        )
        .unwrap();
    let b = cache
        .property(
            &mut second.reader,
            offset_b,
            second.writer.view.bytes,
            PropertyLayout::General,
            &mut || false,
        )
        .unwrap();
    assert_eq!(a, array(10, 20));
    assert_eq!(b, array(99, 20));
    assert!(
        cache
            .slots
            .iter()
            .flatten()
            .all(|entry| entry.id & RESOURCE_ID != 0)
    );
}

struct RecordingCase {
    recording: crate::Recording,
    _history: HistoryWriter,
    states: Vec<SceneState>,
    _directory: Directory,
}

impl RecordingCase {
    fn new(target: u64) -> Self {
        let directory = Directory::new();
        let mut history = HistoryWriter::new(HistoryConfig {
            directory: directory.0.clone(),
            segment_frames: 2,
            min_free_disk_bytes: 0,
            ..Default::default()
        })
        .unwrap();
        let mut state = SceneState::default();
        let mut states = Vec::new();
        for frame in 0..8 {
            state.objects.insert(
                1,
                Object {
                    id: 1,
                    generation: 0,
                    type_name: "PxScene".into(),
                    properties: [
                        ("Actors".into(), array(target, 256)),
                        ("Counter".into(), Value::U64(frame as u64)),
                    ]
                    .into(),
                },
            );
            state.reference_lifetimes.insert(
                (1, "Actors".into()),
                Arc::new((target..target + 256).map(|id| (id, 2)).collect()),
            );
            state.generations.insert(1, 0);
            state.cameras.insert(
                "camera".into(),
                SourceCamera {
                    position: [frame as f32, 1., 2.],
                    up: [0., 1., 0.],
                    target: [0.; 3],
                },
            );
            history.append(
                &FrameInfo {
                    index: frame,
                    timestamp: frame as u64,
                    scene_id: 1,
                },
                &state,
                &SceneUpdate::full(&state),
                &Analysis::default(),
                Vec::new(),
            );
            assert!(history.warning.is_none(), "{:?}", history.warning);
            states.push(state.clone());
        }
        history.close_segment();
        let mut recording = crate::RecordingBuilder::new(pvd_protocol::Header {
            version: 1,
            timestamp_numerator: 1,
            timestamp_denominator: 1,
            flags: 1,
            endian: pvd_protocol::Endian::Little,
        })
        .recording;
        recording.disk = Some(history.snapshot());
        recording.frames = recording.disk.as_ref().unwrap().frames();
        Self {
            recording,
            _history: history,
            states,
            _directory: directory,
        }
    }
}

#[test]
fn cursor_preserves_complete_scenes_across_segments_reverse_and_source_changes() {
    let first = RecordingCase::new(100);
    let second = RecordingCase::new(900);
    let mut cursor = ReplayCursor::default();
    let mut displayed = SceneState::default();
    for frame in [0, 1, 2, 3, 4, 3, 2, 1, 6, 7, 5] {
        cursor
            .seek_scene(&first.recording, frame, || false)
            .unwrap()
            .update
            .apply_owned(&mut displayed);
        assert_eq!(displayed, first.states[frame]);
        assert_eq!(displayed, first.recording.state_at(frame).unwrap());
        let cache = &cursor.current.as_ref().unwrap().external_cache;
        assert!(cache.bytes <= BYTE_LIMIT);
        assert!(cache.slots.iter().flatten().count() <= SLOTS);
    }
    assert!(
        cursor
            .current
            .as_ref()
            .unwrap()
            .external_cache
            .slots
            .iter()
            .flatten()
            .count()
            > 0
    );
    cursor
        .seek_scene(&second.recording, 1, || false)
        .unwrap()
        .update
        .apply_owned(&mut displayed);
    assert_eq!(displayed, second.states[1]);
    assert!(
        cursor.current.as_ref().unwrap().resources.same(
            &second
                .recording
                .disk
                .as_ref()
                .unwrap()
                .segment(1)
                .unwrap()
                .resources
        )
    );
    let before = displayed.clone();
    assert!(cursor.seek_scene(&second.recording, 2, || true).is_err());
    assert_eq!(displayed, before);
    assert_eq!(cursor.current.as_ref().unwrap().frame, Some(1));
    second
        .recording
        .disk
        .as_ref()
        .unwrap()
        .segment(1)
        .unwrap()
        .lease
        .invalidate()
        .unwrap();
    assert!(cursor.seek_scene(&second.recording, 1, || false).is_err());
    assert_eq!(displayed, before);
    cursor.clear();
    assert!(cursor.current.is_none());
}

#[test]
fn cached_payloads_do_not_bypass_object_offset_kind_or_duplicate_validation() {
    let mut store = Store::new();
    let value = store.save(&Record::Value(array(1, 100)), &[]);
    let refs = store.save(&Record::References([(1, 2)].into()), &[]);
    let mut cache = ExternalCache::default();
    cache
        .property(
            &mut store.reader,
            value.id(),
            0,
            PropertyLayout::General,
            &mut || false,
        )
        .unwrap();
    cache
        .references(&mut store.reader, refs.id(), 0, &mut || false)
        .unwrap();
    for (property_offset, inline) in [(value.id(), true), (u64::MAX, false), (refs.id(), false)] {
        let object = store.save(
            &Record::Object(StoredObject {
                object: Object {
                    id: 1,
                    generation: 0,
                    type_name: "Object".into(),
                    properties: if inline {
                        [("Array".into(), Value::U64(1))].into()
                    } else {
                        BTreeMap::new()
                    },
                },
                references: BTreeMap::new(),
                large_properties: [("Array".into(), property_offset)].into(),
                large_references: BTreeMap::new(),
            }),
            &[value.clone(), refs.clone()],
        );
        assert!(
            read_object_cached(
                &mut store.reader,
                object.id(),
                0,
                ObjectRecords {
                    values: &mut BTreeMap::new(),
                    references: &mut BTreeMap::new(),
                    external: Some(&mut cache)
                },
                &mut || false
            )
            .is_err()
        );
    }
}

#[test]
fn cached_object_restore_still_checks_index_identity_and_mid_operation_cancel() {
    let case = RecordingCase::new(100);
    let mut cursor = ReplayCursor::default();
    let mut displayed = SceneState::default();
    cursor
        .seek_scene(&case.recording, 0, || false)
        .unwrap()
        .update
        .apply_owned(&mut displayed);
    let before = displayed.clone();
    let offset = cursor.current.as_ref().unwrap().index.objects[&1];
    let segment = case.recording.disk.as_ref().unwrap().segment(0).unwrap();
    let mut reader = DiskRecording::open(segment).unwrap();
    let changes = IndexUpdate {
        objects: vec![(99, Some(offset))],
        ..Default::default()
    };
    assert!(
        read_update(
            &mut reader,
            segment,
            &changes,
            &mut cursor.current.as_mut().unwrap().external_cache,
            &mut || false
        )
        .is_err()
    );
    let cancelled_after_entry = std::cell::Cell::new(false);
    assert!(
        cursor
            .with_cursor(
                &case.recording,
                1,
                &mut || cancelled_after_entry.get(),
                |cursor, disk, segment, cancelled| {
                    cancelled_after_entry.set(true);
                    cursor.read_scene(
                        disk,
                        segment,
                        1,
                        false,
                        cancelled,
                        &mut SeekProfile::default(),
                    )
                }
            )
            .is_err()
    );
    assert!(cursor.current.is_none());
    assert_eq!(displayed, before);
    cancelled_after_entry.set(false);
    cursor
        .seek_scene(&case.recording, 1, || cancelled_after_entry.get())
        .unwrap()
        .update
        .apply_owned(&mut displayed);
    assert_eq!(displayed, case.states[1]);
}
