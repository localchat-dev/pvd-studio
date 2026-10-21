use super::*;
use serde::de::Error as DeserializeError;
use std::sync::atomic::Ordering;

fn properties(seed: usize) -> BTreeMap<String, Value> {
    (0..48)
        .map(|index| {
            (
                format!("Property_{index:03}_with_a_stable_name"),
                Value::U64((seed + index) as u64),
            )
        })
        .collect()
}

struct Store {
    root: PathBuf,
    resources: Option<Resources>,
    owner: Option<Arc<CacheDirectory>>,
}

impl Store {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "pvd-object-keys-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let owner = CacheDirectory::new(&root).unwrap();
        Self {
            resources: Some(Resources::new(owner.clone(), 32 << 20)),
            root,
            owner: Some(owner),
        }
    }
    fn resources(&self) -> &Resources {
        self.resources.as_ref().unwrap()
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        self.resources.take();
        self.owner.take();
        assert_eq!(std::fs::read_dir(&self.root).unwrap().count(), 0);
        std::fs::remove_dir(&self.root).unwrap();
    }
}

#[test]
fn schema_codec_preserves_unknown_unicode_and_empty_names_and_rejects_noncanonical_data() {
    let mut values = properties(0);
    for key in ["", "$unknown", "未知属性"] {
        values.insert(key.into(), Value::Bool(true));
    }
    let external = BTreeMap::new();
    let keys = InlineKeys {
        values: &values,
        external: &external,
    };
    let (size, digest) = keys.profile().unwrap();
    let mut encoded = Vec::new();
    keys.write(&mut encoded).unwrap();
    assert_eq!(encoded.len(), size);
    assert_eq!(*blake3::hash(&encoded).as_bytes(), digest);
    assert_eq!(
        decode(&encoded, &mut || false).unwrap(),
        values.keys().cloned().collect::<Vec<_>>()
    );
    assert!(keys.matches(&encoded));
    assert!(decode(&encoded, &mut || true).is_err());
    for cut in [0, 7, 8, 11, encoded.len() - 1] {
        assert!(decode(&encoded[..cut], &mut || false).is_err());
    }
    let mut damaged = encoded.clone();
    damaged.push(0);
    assert!(decode(&damaged, &mut || false).is_err());
    let mut damaged = encoded.clone();
    damaged[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(decode(&damaged, &mut || false).is_err());
    let mut duplicate = MAGIC.to_vec();
    duplicate.extend(2u32.to_le_bytes());
    for _ in 0..2 {
        duplicate.extend(1u32.to_le_bytes());
        duplicate.push(b'x');
    }
    assert!(decode(&duplicate, &mut || false).is_err());
    duplicate[16] = 0xff;
    assert!(decode(&duplicate, &mut || false).is_err());
    assert!(decode(&vec![0; MAX_BYTES + 1], &mut || false).is_err());
}

#[test]
fn schema_memo_reuses_only_names_and_direct_dependencies_keep_resources_alive() {
    let store = Store::new();
    let resources = store.resources();
    let mut memo = KeyLists::default();
    let mut staged = memo.staged();
    let mut plan = ResourcePlan::default();
    let external = BTreeMap::new();
    let first = memo
        .prepare(&mut staged, resources, &mut plan, &properties(0), &external)
        .unwrap()
        .unwrap();
    let reused = memo
        .prepare(
            &mut staged,
            resources,
            &mut plan,
            &properties(1000),
            &external,
        )
        .unwrap()
        .unwrap();
    assert_eq!(first.id(), reused.id());
    let owner = resources
        .plan(b"owner", std::slice::from_ref(&first), &mut plan)
        .unwrap();
    resources.commit(&plan).unwrap();
    memo.publish(staged, resources).unwrap();
    let id = first.id();
    let bytes = resources
        .reader()
        .read_dependency(owner.id(), id, MAX_BYTES, &mut || false)
        .unwrap();
    assert_eq!(decode(&bytes, &mut || false).unwrap().len(), 48);
    drop(plan);
    drop(first);
    drop(reused);
    assert!(resources.try_pin(id).unwrap().is_some());
    let mut staged = memo.staged();
    let mut plan = ResourcePlan::default();
    let hit = memo
        .prepare(&mut staged, resources, &mut plan, &properties(7), &external)
        .unwrap()
        .unwrap();
    assert_eq!(hit.id(), id);
    drop(hit);
    drop(owner);
    drop(plan);
    drop(staged);
    assert!(resources.try_pin(id).unwrap().is_none());
    assert!(memo.budget.0.load(Ordering::Relaxed) > 0);
    drop(memo);
    resources.collect().unwrap();
    assert_eq!(resources.physical_bytes(), 0);
}

#[test]
fn schema_admission_is_optional_and_failed_prepare_does_not_publish_memo() {
    let store = Store::new();
    let resources = store.resources();
    let mut memo = KeyLists::default();
    let mut staged = memo.staged();
    let mut plan = ResourcePlan::default();
    let held = memo.budget.reserve(resources, MEMO_BYTES).unwrap().unwrap();
    assert!(
        memo.prepare(
            &mut staged,
            resources,
            &mut plan,
            &properties(0),
            &BTreeMap::new()
        )
        .unwrap()
        .is_none()
    );
    assert!(memo.slots.is_empty() && staged.slots.is_empty());
    drop(held);
    let handle = memo
        .prepare(
            &mut staged,
            resources,
            &mut plan,
            &properties(0),
            &BTreeMap::new(),
        )
        .unwrap()
        .unwrap();
    let id = handle.id();
    drop(handle);
    drop(plan);
    drop(staged);
    assert!(resources.try_pin(id).unwrap().is_none());
    assert!(memo.slots.is_empty());
    assert_eq!(memo.budget.0.load(Ordering::Relaxed), 0);
    let mut staged = memo.staged();
    let mut plan = ResourcePlan::default();
    assert!(
        memo.prepare(
            &mut staged,
            resources,
            &mut plan,
            &properties(0),
            &BTreeMap::new()
        )
        .unwrap()
        .is_some()
    );
    drop(plan);
    drop(staged);
    drop(memo);
}

#[test]
fn schema_domains_pending_publication_and_owner_links_are_checked() {
    let a = Store::new();
    let b = Store::new();
    let resources = a.resources();
    let mut memo = KeyLists::default();
    let mut staged = memo.staged();
    let mut plan = ResourcePlan::default();
    let keys = memo
        .prepare(
            &mut staged,
            resources,
            &mut plan,
            &properties(0),
            &BTreeMap::new(),
        )
        .unwrap()
        .unwrap();
    assert!(memo.publish(staged, resources).is_err());
    let mut staged = memo.staged();
    assert!(
        memo.prepare(
            &mut staged,
            b.resources(),
            &mut ResourcePlan::default(),
            &properties(0),
            &BTreeMap::new()
        )
        .is_err()
    );
    let unlinked = resources.plan(b"unlinked", &[], &mut plan).unwrap();
    let linked = resources
        .plan(b"linked", std::slice::from_ref(&keys), &mut plan)
        .unwrap();
    resources.commit(&plan).unwrap();
    let reader = resources.reader();
    assert!(
        reader
            .read_dependency(unlinked.id(), keys.id(), MAX_BYTES, &mut || false)
            .is_err()
    );
    assert!(
        reader
            .read_dependency(linked.id(), keys.id(), 8, &mut || false)
            .is_err()
    );
    assert!(
        reader
            .read_dependency(linked.id(), linked.id(), MAX_BYTES, &mut || false)
            .is_err()
    );
    assert!(
        reader
            .read_dependency(linked.id(), keys.id(), MAX_BYTES, &mut || true)
            .is_err()
    );
    assert!(
        reader
            .read_dependency(linked.id(), keys.id(), MAX_BYTES, &mut || false)
            .is_ok()
    );
    drop(reader);
    drop(keys);
    drop(unlinked);
    drop(linked);
    drop(plan);
    drop(staged);
    drop(memo);
}

#[test]
fn schema_wire_ref_matches_owned_record_and_scene_partition_includes_schema() {
    let values = properties(5);
    let external = BTreeMap::new();
    let object = KeyedObject {
        id: 3,
        generation: 4,
        type_name: "Unknown".into(),
        keys: (1 << 63) | 1,
        values: values.values().cloned().collect(),
        references: BTreeMap::new(),
        large_properties: BTreeMap::new(),
        large_references: BTreeMap::new(),
    };
    let owned = postcard::to_stdvec(&Record::KeyedObject(object)).unwrap();
    let borrowed = postcard::to_stdvec(&RecordRef::KeyedObject(KeyedObjectRef {
        id: 3,
        generation: 4,
        type_name: "Unknown",
        keys: (1 << 63) | 1,
        values: SmallValues(SmallProperties {
            values: &values,
            external: &external,
        }),
        references: SmallReferences {
            entries: ReferenceEntries::Object(BTreeMap::new().iter()),
            external: &external,
        },
        large_properties: &external,
        large_references: &external,
    }))
    .unwrap();
    assert_eq!(owned, borrowed);
    assert_eq!(owned[0], 10);
    let mut encoded = Vec::new();
    InlineKeys {
        values: &values,
        external: &external,
    }
    .write(&mut encoded)
    .unwrap();
    let mut report =
        disk_accounting::scene_records::SceneRecords::new(disk_accounting::Limits::default());
    report
        .observe((1 << 63) | 1, &encoded, &mut || false)
        .unwrap();
    report
        .observe((1 << 63) | 2, &owned, &mut || false)
        .unwrap();
    report.verify().unwrap();
    assert_eq!(
        report.logical_by_kind["object_keys"].encoded_bytes,
        encoded.len() as u64
    );
    assert_eq!(report.object_total.components.property_keys, 0);
    assert!(report.object_total.components.key_schema_id > 0);
    assert_eq!(report.object_total.encoded_bytes, owned.len() as u64);
}

#[test]
fn full_live_memo_skips_new_names_and_recovers_expired_slots_without_pinning_schema() {
    let store = Store::new();
    let resources = store.resources();
    let mut memo = KeyLists::default();
    let mut staged = memo.staged();
    let mut plan = ResourcePlan::default();
    let mut owners = Vec::new();
    for number in 0..SLOTS {
        let mut values = properties(0);
        values.insert(format!("schema_{number:03}"), Value::U64(0));
        owners.push(
            memo.prepare(&mut staged, resources, &mut plan, &values, &BTreeMap::new())
                .unwrap()
                .unwrap(),
        );
    }
    resources.commit(&plan).unwrap();
    memo.publish(staged, resources).unwrap();
    drop(plan);
    let mut values = properties(0);
    values.insert("different_schema".into(), Value::U64(0));
    let mut staged = memo.staged();
    let mut plan = ResourcePlan::default();
    assert!(
        memo.prepare(&mut staged, resources, &mut plan, &values, &BTreeMap::new())
            .unwrap()
            .is_none()
    );
    assert_eq!(memo.slots.iter().flatten().count(), SLOTS);
    drop(owners);
    assert!(
        memo.prepare(&mut staged, resources, &mut plan, &values, &BTreeMap::new())
            .unwrap()
            .is_some()
    );
    drop(plan);
    drop(staged);
    drop(memo);
}

#[test]
fn metadata_pressure_falls_back_without_hiding_resource_errors() {
    let store = Store::new();
    let resources = store.resources();
    let mut memo = KeyLists::default();
    let mut staged = memo.staged();
    let mut plan = ResourcePlan::default();
    let held = resources
        .reserve_metadata(resources.metadata_capacity() - resources.metadata_bytes())
        .unwrap();
    assert!(
        memo.prepare(
            &mut staged,
            resources,
            &mut plan,
            &properties(0),
            &BTreeMap::new()
        )
        .unwrap()
        .is_none()
    );
    assert!(memo.slots.is_empty() && staged.slots.is_empty());
    drop(held);
    assert!(
        memo.prepare(
            &mut staged,
            resources,
            &mut plan,
            &properties(0),
            &BTreeMap::new()
        )
        .unwrap()
        .is_some()
    );
    assert!(
        memo.prepare(
            &mut KeyLists::default(),
            resources,
            &mut plan,
            &properties(0),
            &BTreeMap::new()
        )
        .is_err()
    );
    drop(staged);
    drop(plan);
    drop(memo);
}

#[test]
fn decoded_schema_cache_cannot_hide_later_resource_corruption_or_wrong_value_count() {
    let store = Store::new();
    let resources = store.resources();
    let values = properties(0);
    let external = BTreeMap::new();
    let mut encoded = Vec::new();
    InlineKeys {
        values: &values,
        external: &external,
    }
    .write(&mut encoded)
    .unwrap();
    let mut plan = ResourcePlan::default();
    let keys = resources.plan(&encoded, &[], &mut plan).unwrap();
    let owner = resources
        .plan(b"owner", std::slice::from_ref(&keys), &mut plan)
        .unwrap();
    resources.commit(&plan).unwrap();
    let path = store
        .owner
        .as_ref()
        .unwrap()
        .files
        .lock()
        .unwrap()
        .iter()
        .next()
        .unwrap()
        .clone();
    let mut reader = HistoryReader {
        buffer: BufReader::new(File::open(&path).unwrap()),
        capture: None,
        position: None,
        resources: Some(resources.reader()),
        _segment_guard: None,
    };
    let object = |count: usize| KeyedObject {
        id: 9,
        generation: 4,
        type_name: "Unknown".into(),
        keys: keys.id(),
        values: values.values().take(count).cloned().collect(),
        references: BTreeMap::new(),
        large_properties: BTreeMap::new(),
        large_references: BTreeMap::new(),
    };
    let mut cache = replay::ExternalCache::default();
    assert_eq!(
        expand(
            object(48),
            &mut reader,
            owner.id(),
            Some(&mut cache),
            &mut || false
        )
        .unwrap()
        .object
        .properties,
        values
    );
    assert!(
        expand(
            object(47),
            &mut reader,
            owner.id(),
            Some(&mut cache),
            &mut || false
        )
        .is_err()
    );
    assert!(
        expand(
            object(48),
            &mut reader,
            owner.id(),
            Some(&mut cache),
            &mut || true
        )
        .is_err()
    );
    let mut file = OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::Start(8 + 36)).unwrap();
    file.write_all(b"X").unwrap();
    file.flush().unwrap();
    drop(file);
    reader.resources = Some(resources.reader());
    let failure = match expand(
        object(48),
        &mut reader,
        owner.id(),
        Some(&mut cache),
        &mut || false,
    ) {
        Ok(_) => panic!("schema cache hid damaged source bytes"),
        Err(failure) => failure.to_string(),
    };
    assert!(failure.contains("checksum"), "{failure}");
    drop(reader);
    drop(cache);
    drop(owner);
    drop(keys);
    drop(plan);
}

#[test]
fn schema_and_keyed_object_are_distinct_typed_physical_buckets_with_complete_partition() {
    let store = Store::new();
    let resources = store.resources();
    let values = properties(0);
    let mut encoded = Vec::new();
    InlineKeys {
        values: &values,
        external: &BTreeMap::new(),
    }
    .write(&mut encoded)
    .unwrap();
    let mut plan = ResourcePlan::default();
    let keys = resources.plan(&encoded, &[], &mut plan).unwrap();
    let object = postcard::to_stdvec(&Record::KeyedObject(KeyedObject {
        id: 7,
        generation: 5,
        type_name: "Unknown".into(),
        keys: keys.id(),
        values: values.values().cloned().collect(),
        references: BTreeMap::new(),
        large_properties: BTreeMap::new(),
        large_references: BTreeMap::new(),
    }))
    .unwrap();
    let owner = resources
        .plan(&object, std::slice::from_ref(&keys), &mut plan)
        .unwrap();
    resources.commit(&plan).unwrap();
    let limits = disk_accounting::Limits::default();
    let roots = BTreeMap::from([(owner.id(), resources::accounting::SCENE)]);
    let logical = resources
        .scene_record_accounting(&roots, limits, &mut || false)
        .unwrap();
    logical.verify().unwrap();
    assert_eq!(logical.kinds.len(), 2);
    let physical = resources
        .physical_accounting_with_kinds(&roots, Some(&logical.kinds), limits, &mut || false)
        .unwrap();
    assert_eq!(
        physical.partition_sum().unwrap(),
        resources.physical_bytes()
    );
    assert_eq!(physical.missing_semantic_roots, 0);
    let buckets = physical.record_kind_partition.as_ref().unwrap();
    assert_eq!(
        buckets[&disk_accounting::scene_records::OBJECT_KEYS].payload_bytes,
        encoded.len() as u64
    );
    assert_eq!(
        buckets[&disk_accounting::scene_records::OBJECT].payload_bytes,
        object.len() as u64
    );
    drop(plan);
    drop(owner);
    drop(keys);
}

struct ValuesInput<'a> {
    remaining: usize,
    hint: Option<usize>,
    poison_last: bool,
    calls: &'a std::cell::Cell<usize>,
    parsed_tail: &'a std::cell::Cell<bool>,
}

struct UnreadValue<'a>(&'a std::cell::Cell<bool>);

impl<'de> serde::Deserializer<'de> for UnreadValue<'_> {
    type Error = serde::de::value::Error;
    fn deserialize_any<V: serde::de::Visitor<'de>>(self, _: V) -> Result<V::Value, Self::Error> {
        self.0.set(true);
        Err(Self::Error::custom("the excess Value was decoded"))
    }
    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf
        option unit unit_struct newtype_struct seq tuple tuple_struct map struct enum identifier ignored_any
    }
}

impl<'de> serde::de::SeqAccess<'de> for ValuesInput<'_> {
    type Error = serde::de::value::Error;
    fn next_element_seed<T: serde::de::DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Self::Error> {
        if self.remaining == 0 {
            return Ok(None);
        }
        self.calls.set(self.calls.get() + 1);
        let poison = self.poison_last && self.remaining == 1;
        self.remaining -= 1;
        if poison {
            seed.deserialize(UnreadValue(self.parsed_tail)).map(Some)
        } else {
            const BOOLEAN: &[u8] = &[0, 0];
            seed.deserialize(&mut postcard::Deserializer::from_bytes(BOOLEAN))
                .map(Some)
                .map_err(Self::Error::custom)
        }
    }
    fn size_hint(&self) -> Option<usize> {
        self.hint
    }
}

#[test]
fn values_count_guard_rejects_hint_and_unhinted_overflow_before_the_next_value() {
    use serde::de::value::SeqAccessDeserializer;
    let calls = std::cell::Cell::new(0);
    let parsed = std::cell::Cell::new(false);
    for hint in [Some(MAX_KEYS + 1), Some(usize::MAX)] {
        let failure = deserialize_values(SeqAccessDeserializer::new(ValuesInput {
            remaining: MAX_KEYS + 1,
            hint,
            poison_last: true,
            calls: &calls,
            parsed_tail: &parsed,
        }))
        .unwrap_err();
        assert!(failure.to_string().contains("key count limit"));
        assert_eq!(calls.get(), 0);
        assert!(!parsed.get());
    }
    for hint in [None, Some(1)] {
        calls.set(0);
        let failure = deserialize_values(SeqAccessDeserializer::new(ValuesInput {
            remaining: MAX_KEYS + 1,
            hint,
            poison_last: true,
            calls: &calls,
            parsed_tail: &parsed,
        }))
        .unwrap_err();
        assert!(failure.to_string().contains("key count limit"));
        assert_eq!(calls.get(), MAX_KEYS + 1);
        assert!(!parsed.get());
    }
    for hint in [None, Some(MAX_KEYS)] {
        calls.set(0);
        let values = deserialize_values(SeqAccessDeserializer::new(ValuesInput {
            remaining: MAX_KEYS,
            hint,
            poison_last: false,
            calls: &calls,
            parsed_tail: &parsed,
        }))
        .unwrap();
        assert_eq!(values.len(), MAX_KEYS);
        assert!(values.capacity() <= MAX_KEYS);
        assert_eq!(calls.get(), MAX_KEYS);
    }
}

#[test]
fn keyed_record_count_guard_applies_to_internal_and_capture_decoders_without_bypassing_nested_budget()
 {
    let object = |values| KeyedObject {
        id: 7,
        generation: 1,
        type_name: "Unknown".into(),
        keys: (1 << 63) | 1,
        values,
        references: BTreeMap::new(),
        large_properties: BTreeMap::new(),
        large_references: BTreeMap::new(),
    };
    let excessive = postcard::to_stdvec(&Record::KeyedObject(object(vec![
        Value::Bool(false);
        MAX_KEYS + 1
    ])))
    .unwrap();
    assert!(decode_record(&excessive, &mut || false).is_err());
    assert!(capture::record::decode(&excessive, &mut || false).is_err());
    let nested = postcard::to_stdvec(&Record::KeyedObject(object(vec![Value::String(
        "x".repeat(4096),
    )])))
    .unwrap();
    assert!(decode_record(&nested, &mut || false).is_ok());
    assert!(capture::record::decode(&nested, &mut || false).is_ok());
    let short = postcard::to_stdvec(&Record::KeyedObject(object(vec![Value::String(
        String::new(),
    )])))
    .unwrap();
    assert!(capture::record::decode_for_accounting(&short, &mut || false, 8192).is_ok());
    assert!(capture::record::decode_for_accounting(&nested, &mut || false, 8192).is_err());
}
