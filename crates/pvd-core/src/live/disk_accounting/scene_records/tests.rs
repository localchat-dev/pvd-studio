use super::*;

fn object(name: &str, generation: u32) -> StoredObject {
    StoredObject {
        object: Object {
            id: 7,
            generation,
            type_name: name.into(),
            properties: BTreeMap::from([
                ("Mass".into(), Value::F32(2.0)),
                ("Name".into(), Value::String("actor".into())),
            ]),
        },
        references: BTreeMap::from([("Owner".into(), BTreeMap::from([(4, 3)]))]),
        large_properties: BTreeMap::from([("Geometry".into(), (1 << 63) | 9)]),
        large_references: BTreeMap::from([("Actors".into(), (1 << 63) | 10)]),
    }
}

#[test]
fn object_components_cover_complete_wire_and_keep_generation_versions() {
    let mut report = SceneRecords::new(Limits::default());
    for generation in [1, 2] {
        let stored = object("PxRigidDynamic", generation);
        let expected_keys = stored
            .object
            .properties
            .keys()
            .map(|key| encoded_bytes(key).unwrap())
            .sum::<u64>();
        let bytes = postcard::to_stdvec(&Record::Object(stored)).unwrap();
        report
            .observe((1 << 63) | generation as u64, &bytes, &mut || false)
            .unwrap();
        assert_eq!(
            report.object_total.components.property_keys,
            expected_keys * generation as u64
        );
    }
    report.verify().unwrap();
    assert_eq!(report.object_total.records, 2);
    assert_eq!(report.object_classes["PxRigidDynamic"].records, 2);
    assert_eq!(report.object_total.components.record_variant, 2);
    assert!(report.object_total.components.property_values > 0);
    assert!(report.object_total.components.external_reference_ids > 0);
    assert_eq!(
        report.object_total.components.total().unwrap(),
        report.logical_by_kind["object"].encoded_bytes
    );
}

#[test]
fn class_limit_and_utf8_byte_limit_keep_an_explicit_complete_overflow_bucket() {
    let mut report = SceneRecords::new(Limits::default());
    report.class_limit = 1;
    report.class_name_byte_limit = 3;
    for (id, name) in [(1, "A"), (2, "B"), (3, "物体"), (4, "A")] {
        let bytes = postcard::to_stdvec(&Record::Object(object(name, 1))).unwrap();
        report
            .observe((1 << 63) | id, &bytes, &mut || false)
            .unwrap();
    }
    report.verify().unwrap();
    assert_eq!(report.object_classes.len(), 1);
    assert_eq!(report.object_classes["A"].records, 2);
    assert_eq!(report.overflow_classes.records, 2);
    assert_eq!(report.object_total.records, 4);
}

#[test]
fn diagnostic_limits_reject_expansion_duplicates_cancellation_and_non_scene_records() {
    let bytes = postcard::to_stdvec(&Record::Value(Value::Array(Arc::new(vec![
        Value::String(
            "hello".into()
        );
        20
    ]))))
    .unwrap();
    let mut report = SceneRecords::new(Limits::default());
    report.per_record_decoded_allowance = 16;
    assert!(report.observe(1 << 63, &bytes, &mut || false).is_err());
    assert!(report.kinds.is_empty());
    report.per_record_decoded_allowance = DECODE_ALLOWANCE;
    report.per_record_wire_limit = bytes.len() - 1;
    assert!(report.observe(1 << 63, &bytes, &mut || false).is_err());
    report.per_record_wire_limit = MAX_RECORD;
    assert!(report.observe(1 << 63, &bytes, &mut || true).is_err());
    report.max_directory_items = 1;
    report.observe(1 << 63, &bytes, &mut || false).unwrap();
    assert!(report.observe(1 << 63, &bytes, &mut || false).is_err());
    assert!(
        report
            .observe((1 << 63) | 1, &bytes, &mut || false)
            .is_err()
    );
    report.verify().unwrap();
    let mut empty = SceneRecords::new(Limits::default());
    let non_scene = postcard::to_stdvec(&Record::AnalysisPages(1)).unwrap();
    assert!(empty.observe(1 << 63, &non_scene, &mut || false).is_err());
    let mut trailing = bytes;
    trailing.push(0);
    assert!(empty.observe(1 << 63, &trailing, &mut || false).is_err());
    assert!(empty.kinds.is_empty());
}

fn directory() -> PathBuf {
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "pvd-scene-ledger-{}-{sequence}",
        std::process::id()
    ));
    std::fs::create_dir(&path).unwrap();
    path
}

#[test]
fn resource_graph_shared_values_are_decoded_once_and_physical_payload_is_not_double_counted() {
    let path = directory();
    {
        let resources = Resources::new(CacheDirectory::new(&path).unwrap(), 8 << 20);
        let mut plan = ResourcePlan::default();
        let value_bytes =
            postcard::to_stdvec(&Record::Value(Value::Bytes(Arc::from([7_u8; 512])))).unwrap();
        let value = resources.plan(&value_bytes, &[], &mut plan).unwrap();
        let analysis_alias = resources.plan(&value_bytes, &[], &mut plan).unwrap();
        let mut first = object("PxShape", 1);
        first.large_properties = BTreeMap::from([("Geometry".into(), value.id())]);
        first.large_references.clear();
        let first = resources
            .plan(
                &postcard::to_stdvec(&Record::Object(first)).unwrap(),
                std::slice::from_ref(&value),
                &mut plan,
            )
            .unwrap();
        let roots = BTreeMap::from([(first.id(), SCENE), (analysis_alias.id(), ANALYSIS)]);
        resources.commit(&plan).unwrap();
        let report = resources
            .scene_record_accounting(&roots, Limits::default(), &mut || false)
            .unwrap();
        assert_eq!(report.kinds.len(), 2);
        assert_eq!(report.logical_by_kind["value"].records, 1);
        assert_eq!(report.logical_by_kind["object"].records, 1);
        let physical = resources
            .physical_accounting_with_kinds(
                &roots,
                Some(&report.kinds),
                Limits::default(),
                &mut || false,
            )
            .unwrap();
        let partition = physical.record_kind_partition.as_ref().unwrap();
        assert_eq!(
            partition[&(VALUE | ANALYSIS_KIND)].payload_bytes,
            value_bytes.len() as u64
        );
        assert_eq!(partition[&(VALUE | ANALYSIS_KIND)].blocks, 1);
        assert_eq!(partition[&OBJECT].blocks, 1);
        assert_eq!(physical.unique_current_blocks, 2);
        assert_eq!(
            physical.partition_sum().unwrap(),
            resources.physical_bytes()
        );
    }
    std::fs::remove_dir(path).unwrap();
}

#[test]
fn unknown_live_users_remain_visible_and_missing_roots_do_not_fabricate_kinds() {
    let path = directory();
    {
        let resources = Resources::new(CacheDirectory::new(&path).unwrap(), 8 << 20);
        let mut plan = ResourcePlan::default();
        let bytes = postcard::to_stdvec(&Record::References(BTreeMap::from([(7, 2)]))).unwrap();
        let leaf = resources.plan(&bytes, &[], &mut plan).unwrap();
        let unknown = resources
            .plan(&[99; 12], std::slice::from_ref(&leaf), &mut plan)
            .unwrap();
        resources.commit(&plan).unwrap();
        let roots = BTreeMap::from([(leaf.id(), SCENE), (u64::MAX, SCENE)]);
        let report = resources
            .scene_record_accounting(&roots, Limits::default(), &mut || false)
            .unwrap();
        assert_eq!(report.kinds.len(), 1);
        assert!(!report.kinds.contains_key(&unknown.id()));
        let physical = resources
            .physical_accounting_with_kinds(
                &roots,
                Some(&report.kinds),
                Limits::default(),
                &mut || false,
            )
            .unwrap();
        assert_eq!(physical.missing_semantic_roots, 1);
        let partition = physical.record_kind_partition.unwrap();
        assert_eq!(
            partition[&(REFERENCES | UNKNOWN_KIND)].payload_bytes,
            bytes.len() as u64
        );
        assert_eq!(partition[&UNKNOWN_KIND].payload_bytes, 12);
    }
    std::fs::remove_dir(path).unwrap();
}

#[test]
fn graph_directory_and_record_limits_leave_resource_bytes_unchanged() {
    let path = directory();
    {
        let resources = Resources::new(CacheDirectory::new(&path).unwrap(), 8 << 20);
        let mut plan = ResourcePlan::default();
        let value = resources
            .plan(
                &postcard::to_stdvec(&Record::Value(Value::U64(1))).unwrap(),
                &[],
                &mut plan,
            )
            .unwrap();
        resources.commit(&plan).unwrap();
        let roots = BTreeMap::from([(value.id(), SCENE)]);
        let before = resources.physical_bytes();
        assert!(
            resources
                .scene_record_accounting(
                    &roots,
                    Limits {
                        items: 0,
                        ..Default::default()
                    },
                    &mut || false
                )
                .is_err()
        );
        assert!(
            resources
                .scene_record_accounting(
                    &roots,
                    Limits {
                        record_bytes: 0,
                        ..Default::default()
                    },
                    &mut || false
                )
                .is_err()
        );
        assert!(
            resources
                .scene_record_accounting(&roots, Limits::default(), &mut || true)
                .is_err()
        );
        assert_eq!(before, resources.physical_bytes());
        resources
            .scene_record_accounting(&roots, Limits::default(), &mut || false)
            .unwrap()
            .verify()
            .unwrap();
    }
    std::fs::remove_dir(path).unwrap();
}
