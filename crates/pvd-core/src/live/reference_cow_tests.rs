use super::*;
use crate::RecordingBuilder;
use pvd_protocol::{Endian, Header};
use std::{fs, sync::atomic::Ordering};

fn state() -> SceneState {
    let mut state = SceneState::default();
    for id in [1, 2, 3] {
        Action::Create(Object {
            id,
            generation: 0,
            type_name: "Object".into(),
            properties: BTreeMap::new(),
        })
        .apply(&mut state);
    }
    Action::Set(
        1,
        "Children".into(),
        Value::Array(vec![Value::Ref(2)].into()),
    )
    .apply(&mut state);
    state
}

fn bindings(state: &SceneState) -> &Arc<BTreeMap<u64, u32>> {
    &state.reference_lifetimes[&(1, "Children".into())]
}

fn wire(state: &SceneState) -> Vec<u8> {
    postcard::to_stdvec(&(
        &state.objects,
        &state.reference_lifetimes,
        &state.generations,
        &state.debug_draw,
        &state.contacts,
        &state.errors,
        &state.cameras,
    ))
    .unwrap()
}

#[test]
fn shared_object_references_preserve_plain_map_wire_in_both_directions() {
    #[derive(Serialize, Deserialize)]
    struct PlainObjectState {
        object: Object,
        references: BTreeMap<String, BTreeMap<u64, u32>>,
    }
    let state = state();
    let shared = ObjectState::capture(&state, 1).unwrap();
    let plain = PlainObjectState {
        object: shared.object.clone(),
        references: shared
            .references
            .iter()
            .map(|(key, value)| (key.clone(), value.as_ref().clone()))
            .collect(),
    };
    let bytes = postcard::to_stdvec(&plain).unwrap();
    assert_eq!(postcard::to_stdvec(&shared).unwrap(), bytes);
    let restored: ObjectState = postcard::from_bytes(&bytes).unwrap();
    assert_eq!(restored.object, plain.object);
    assert_eq!(
        restored.references["Children"].as_ref(),
        &plain.references["Children"]
    );
    let restored_plain: PlainObjectState =
        postcard::from_bytes(&postcard::to_stdvec(&shared).unwrap()).unwrap();
    assert_eq!(restored_plain.object, plain.object);
    assert_eq!(restored_plain.references, plain.references);
}

#[test]
fn checkpoints_and_both_apply_paths_share_bindings_without_changing_held_snapshots() {
    let original = state();
    let expected = wire(&original);
    let checkpoint = crate::history::Checkpoint::new(&original, None);
    let restored = checkpoint.restore();
    assert!(Arc::ptr_eq(bindings(&original), bindings(&restored)));
    let update = SceneUpdate::capture(&original, &[1, 2, 3].into(), true);
    assert!(Arc::ptr_eq(
        bindings(&original),
        &update.objects[&1].as_ref().unwrap().references["Children"]
    ));
    drop(update);
    for shared_update in [false, true] {
        let update = SceneUpdate::capture(&original, &[1, 2, 3].into(), true);
        let held = shared_update.then(|| update.clone());
        let mut borrowed = SceneState::default();
        update.apply(&mut borrowed);
        let mut owned = SceneState::default();
        update.apply_owned(&mut owned);
        assert!(Arc::ptr_eq(bindings(&original), bindings(&borrowed)));
        assert!(Arc::ptr_eq(bindings(&original), bindings(&owned)));
        assert_eq!(wire(&borrowed), expected);
        assert_eq!(wire(&owned), expected);
        Action::Push(1, "Children".into(), 3).apply(&mut owned);
        assert!(!Arc::ptr_eq(bindings(&original), bindings(&owned)));
        assert_eq!(bindings(&owned).as_ref(), &BTreeMap::from([(2, 0), (3, 0)]));
        assert_eq!(wire(&borrowed), expected);
        if let Some(held) = held {
            let mut still_original = SceneState::default();
            held.apply_owned(&mut still_original);
            assert_eq!(wire(&still_original), expected);
        }
    }
    assert_eq!(wire(&checkpoint.restore()), expected);
    assert_eq!(wire(&original), expected);
}

#[test]
fn binding_mutations_copy_shared_map_once_and_preserve_target_generations() {
    let mut current = state();
    let held = current.clone();
    let expected = wire(&held);
    let before = Arc::as_ptr(bindings(&current));
    Action::Push(1, "Children".into(), 3).apply(&mut current);
    let after = Arc::as_ptr(bindings(&current));
    assert_ne!(before, after);
    assert_eq!(Arc::strong_count(bindings(&current)), 1);
    Action::Remove(1, "Children".into(), 3).apply(&mut current);
    assert_eq!(Arc::as_ptr(bindings(&current)), after);
    Action::Destroy(2).apply(&mut current);
    Action::Create(Object {
        id: 2,
        generation: 1,
        type_name: "Replacement".into(),
        properties: BTreeMap::new(),
    })
    .apply(&mut current);
    assert!(current.resolve_reference(1, "Children", 2).is_none());
    Action::Push(1, "Children".into(), 2).apply(&mut current);
    assert_eq!(Arc::as_ptr(bindings(&current)), after);
    assert_eq!(
        current
            .resolve_reference(1, "Children", 2)
            .unwrap()
            .generation,
        1
    );
    assert_eq!(
        current.objects[&1].properties["Children"],
        Value::Array(vec![Value::Ref(2)].into())
    );
    Action::Set(1, "Children".into(), Value::U64(7)).apply(&mut current);
    assert!(
        !current
            .reference_lifetimes
            .contains_key(&(1, "Children".into()))
    );
    Action::Destroy(1).apply(&mut current);
    assert_eq!(wire(&held), expected);
    assert_eq!(
        held.resolve_reference(1, "Children", 2).unwrap().generation,
        0
    );
}

fn builder() -> RecordingBuilder {
    let mut builder = RecordingBuilder::new(Header {
        version: 1,
        timestamp_numerator: 1,
        timestamp_denominator: 1,
        flags: 1,
        endian: Endian::Little,
    });
    builder.state = state();
    builder
}

#[test]
fn shared_binding_copy_budget_is_reserved_before_push_or_remove_mutates_state() {
    for action in [
        Action::Push(1, "Children".into(), 3),
        Action::Remove(1, "Children".into(), 2),
    ] {
        let mut builder = builder();
        let held = builder.state.clone();
        let old_wire = wire(&held);
        let old_actions = builder.recording.actions.len();
        let value_copy = builder.state.objects[&1].properties["Children"].retained_bytes();
        let binding_copy = 32 + bindings(&builder.state).len() * 80;
        let required = action.retained_bytes() + value_copy + binding_copy;
        let metadata = builder.recording.frame_sources.index_bytes();
        builder.memory_limit = metadata + required - 1;
        assert!(builder.apply_action(action.clone()).is_err());
        assert_eq!(builder.retained_bytes, 0);
        assert_eq!(builder.recording.actions.len(), old_actions);
        assert_eq!(wire(&builder.state), old_wire);
        assert!(Arc::ptr_eq(bindings(&builder.state), bindings(&held)));
        builder.memory_limit = metadata + required;
        builder.apply_action(action).unwrap();
        assert_eq!(builder.retained_bytes, required);
        assert_eq!(wire(&held), old_wire);
        assert!(!Arc::ptr_eq(bindings(&builder.state), bindings(&held)));
        let pointer = Arc::as_ptr(bindings(&builder.state));
        let next = Action::Push(1, "Children".into(), 99);
        builder.memory_limit += next.retained_bytes();
        builder.apply_action(next).unwrap();
        assert_eq!(Arc::as_ptr(bindings(&builder.state)), pointer);
    }
}

#[test]
fn a_shared_scalar_binding_also_requires_the_copy_reservation() {
    let mut builder = builder();
    Action::Set(1, "Children".into(), Value::U64(3)).apply(&mut builder.state);
    Action::Push(1, "Children".into(), 2).apply(&mut builder.state);
    let held = builder.state.clone();
    let action = Action::Remove(1, "Children".into(), 2);
    builder.memory_limit = action.retained_bytes();
    assert!(builder.apply_action(action).is_err());
    assert_eq!(wire(&builder.state), wire(&held));
    assert_eq!(builder.retained_bytes, 0);
}

#[test]
fn external_reference_decode_is_shared_across_objects_and_applied_states() {
    let directory = std::env::temp_dir().join(format!(
        "pvd-reference-cow-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let owner = CacheDirectory::new(&directory).unwrap();
    let mut writer = SegmentWriter::new(&owner, 0).unwrap();
    let targets: BTreeMap<_, _> = (10..266).map(|id| (id, 3)).collect();
    let shared = writer.write(&Record::References(targets.clone())).unwrap();
    let mut offsets = Vec::new();
    for id in [1, 2] {
        offsets.push(
            writer
                .write(&Record::Object(StoredObject {
                    object: Object {
                        id,
                        generation: 0,
                        type_name: "Object".into(),
                        properties: BTreeMap::new(),
                    },
                    references: [("Inline".into(), BTreeMap::from([(7, 2)]))].into(),
                    large_properties: BTreeMap::new(),
                    large_references: [("Children".into(), shared)].into(),
                }))
                .unwrap(),
        );
    }
    let inline_only = writer
        .write(&Record::Object(StoredObject {
            object: Object {
                id: 3,
                generation: 0,
                type_name: "Object".into(),
                properties: BTreeMap::new(),
            },
            references: [
                ("First".into(), BTreeMap::from([(7, 2)])),
                ("Second".into(), BTreeMap::from([(8, 3)])),
            ]
            .into(),
            large_properties: BTreeMap::new(),
            large_references: BTreeMap::new(),
        }))
        .unwrap();
    writer.file.flush().unwrap();
    let mut reader = HistoryReader {
        buffer: BufReader::new(File::open(&writer.view.path).unwrap()),
        capture: None,
        position: Some(0),
        resources: None,
        _segment_guard: None,
    };
    let mut values = BTreeMap::new();
    let mut references = BTreeMap::new();
    let first = replay::read_object(
        &mut reader,
        offsets[0],
        writer.view.bytes,
        &mut values,
        &mut references,
        &mut || false,
    )
    .unwrap();
    let second = replay::read_object(
        &mut reader,
        offsets[1],
        writer.view.bytes,
        &mut values,
        &mut references,
        &mut || false,
    )
    .unwrap();
    assert_eq!(references.len(), 1);
    assert_eq!(first.references["Children"].as_ref(), &targets);
    assert!(Arc::ptr_eq(
        &first.references["Children"],
        &second.references["Children"]
    ));
    assert!(Arc::ptr_eq(
        &first.references["Children"],
        &references[&shared]
    ));
    let mut applied = SceneState::default();
    first.apply(&mut applied);
    second.apply_owned(&mut applied);
    assert!(Arc::ptr_eq(
        &applied.reference_lifetimes[&(1, "Children".into())],
        &applied.reference_lifetimes[&(2, "Children".into())]
    ));
    assert_eq!(
        applied.reference_lifetimes[&(1, "Inline".into())].as_ref(),
        &BTreeMap::from([(7, 2)])
    );
    let mut read_checks = 0;
    read_record_with_cancel(&mut reader, inline_only, writer.view.bytes, &mut || {
        read_checks += 1;
        false
    })
    .unwrap();
    let mut checks = 0;
    let failure = replay::read_object(
        &mut reader,
        inline_only,
        writer.view.bytes,
        &mut values,
        &mut references,
        &mut || {
            checks += 1;
            checks == read_checks + 2
        },
    )
    .unwrap_err();
    assert_eq!(
        failure.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
    let restored = replay::read_object(
        &mut reader,
        inline_only,
        writer.view.bytes,
        &mut values,
        &mut references,
        &mut || false,
    )
    .unwrap();
    assert_eq!(restored.references.len(), 2);
    assert_eq!(
        restored.references["First"].as_ref(),
        &BTreeMap::from([(7, 2)])
    );
    assert_eq!(
        restored.references["Second"].as_ref(),
        &BTreeMap::from([(8, 3)])
    );
    drop((reader, writer, owner));
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
    fs::remove_dir(directory).unwrap();
}
