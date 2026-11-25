use pvd_core::{LatestScene, LatestSceneView, Value, frame_sources::FrameObject};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

const SCENE: u64 = 1;
const ACTOR: u64 = 2;

fn initialization() -> Vec<u8> {
    let mut bytes = pvd_protocol::STREAM_MAGIC.to_le_bytes().to_vec();
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(1_000_000u64.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    let mut events: Vec<_> = [
        "physx3",
        "PxScene",
        "PxRigidDynamic",
        "frame",
        "RigidDynamics",
        "PvdU32",
        "values",
        "SceneQueries.Raycasts",
        "PvdRaycast",
        "ObjectRef",
        "PxTriangleMesh",
        "Triangles",
    ]
    .into_iter()
    .enumerate()
    .map(|(index, name)| {
        let mut event = vec![1];
        event.extend((name.len() as u32 + 1).to_le_bytes());
        event.extend(name.as_bytes());
        event.push(0);
        event.extend((index as u32 + 1).to_le_bytes());
        event
    })
    .collect();
    events.extend([create(2, SCENE), create(3, ACTOR), reference(ACTOR)]);
    bytes.extend(group(&events));
    bytes
}

fn group(events: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = (events.iter().map(Vec::len).sum::<usize>() as u32)
        .to_le_bytes()
        .to_vec();
    bytes.extend((events.len() as u32).to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    for event in events {
        bytes.extend(event);
    }
    bytes
}

fn create(class: u32, id: u64) -> Vec<u8> {
    let mut event = vec![6];
    event.extend(1u32.to_le_bytes());
    event.extend(class.to_le_bytes());
    event.extend(id.to_le_bytes());
    event
}

fn reference(target: u64) -> Vec<u8> {
    push_reference(SCENE, 5, target)
}

fn push_reference(owner: u64, property: u32, target: u64) -> Vec<u8> {
    let mut event = vec![16];
    event.extend(owner.to_le_bytes());
    event.extend(property.to_le_bytes());
    event.extend(target.to_le_bytes());
    event
}

fn remove_reference(target: u64) -> Vec<u8> {
    let mut event = reference(target);
    event[0] = 17;
    event
}

fn set_references(targets: &[u64]) -> Vec<u8> {
    let mut event = vec![7];
    event.extend(SCENE.to_le_bytes());
    event.extend(5u32.to_le_bytes());
    event.extend(1u32.to_le_bytes());
    event.extend(10u32.to_le_bytes());
    event.extend((targets.len() as u32).to_le_bytes());
    event.extend((size_of_val(targets) as u32).to_le_bytes());
    for target in targets {
        event.extend(target.to_le_bytes());
    }
    event
}

fn frame(index: u64) -> Vec<u8> {
    let mut event = vec![19];
    event.extend(SCENE.to_le_bytes());
    event.extend(4u32.to_le_bytes());
    event.extend(index.to_le_bytes());
    event
}

fn begin_values() -> Vec<u8> {
    let mut event = vec![8];
    event.extend(ACTOR.to_le_bytes());
    event.extend(7u32.to_le_bytes());
    event.extend(1u32.to_le_bytes());
    event.extend(6u32.to_le_bytes());
    event
}

fn append_value(value: u32) -> Vec<u8> {
    let mut event = vec![9];
    event.extend(4u32.to_le_bytes());
    event.extend(value.to_le_bytes());
    event.extend(1u32.to_le_bytes());
    event
}

fn destroy(id: u64) -> Vec<u8> {
    let mut event = vec![15];
    event.extend(id.to_le_bytes());
    event
}

fn raycast(origin_x: f32) -> Vec<u8> {
    let mut data = vec![0; 72];
    data[24..28].copy_from_slice(&origin_x.to_le_bytes());
    data[36..40].copy_from_slice(&1.0f32.to_le_bytes());
    data[48..52].copy_from_slice(&8.0f32.to_le_bytes());
    data.extend(b"SceneQueries.Hits\0");

    let mut event = vec![7];
    event.extend(SCENE.to_le_bytes());
    event.extend(8u32.to_le_bytes());
    event.extend(1u32.to_le_bytes());
    event.extend(9u32.to_le_bytes());
    event.extend(1u32.to_le_bytes());
    event.extend((data.len() as u32).to_le_bytes());
    event.extend(data);
    event
}

#[test]
fn packed_mesh_reference_edit_reaches_published_and_reader_scenes() {
    const MESH: u64 = 3;
    const TRIANGLES: u32 = 12;
    let mut live = LatestScene::default();
    let reader = live.reader();
    let mut view = LatestSceneView::default();
    live.feed(&initialization()).unwrap();

    let indices = [0u32, 1, 2].repeat(86);
    let mut set = vec![7];
    set.extend(MESH.to_le_bytes());
    set.extend(TRIANGLES.to_le_bytes());
    set.extend(1u32.to_le_bytes());
    set.extend(6u32.to_le_bytes());
    set.extend((indices.len() as u32).to_le_bytes());
    set.extend((size_of_val(indices.as_slice()) as u32).to_le_bytes());
    for index in &indices {
        set.extend(index.to_le_bytes());
    }
    live.feed(&group(&[create(11, MESH), set, frame(0)]))
        .unwrap();
    reader.sync_view(&mut view, Some(0)).unwrap();
    assert!(matches!(
        view.scene().objects[&MESH].properties["Triangles"],
        Value::MeshArray(_)
    ));

    live.feed(&group(&[push_reference(MESH, TRIANGLES, ACTOR), frame(1)]))
        .unwrap();
    reader.sync_view(&mut view, Some(1)).unwrap();
    let published = reader.latest().unwrap();
    for scene in [&published.scene, view.scene()] {
        let value = &scene.objects[&MESH].properties["Triangles"];
        assert_eq!(value.array_len(), Some(indices.len() + 1));
        assert_eq!(
            value.array_item(indices.len()).as_deref(),
            Some(&Value::Ref(ACTOR))
        );
        assert!(scene.resolve_reference(MESH, "Triangles", ACTOR).is_some());
    }
}

#[test]
fn reader_holds_complete_scene_during_fragmented_updates() {
    let mut live = LatestScene::default();
    let reader = live.reader();
    for chunk in initialization().chunks(3) {
        live.feed(chunk).unwrap();
    }
    assert!(reader.latest().is_none());

    live.feed(&group(&[frame(0), begin_values(), append_value(3)]))
        .unwrap();
    let first = reader.latest().unwrap();
    assert_eq!(first.index, 0);
    assert_eq!(reader.with_latest(|frame| frame.unwrap().index), 0);
    assert!(
        !first.scene.objects[&ACTOR]
            .properties
            .contains_key("values")
    );
    assert!(
        first
            .scene
            .resolve_reference(SCENE, "RigidDynamics", ACTOR)
            .is_some()
    );

    live.feed(&group(&[append_value(7), vec![10]])).unwrap();
    assert!(Arc::ptr_eq(&first, &reader.latest().unwrap()));
    live.feed(&group(&[frame(1)])).unwrap();
    let second = reader.latest().unwrap();
    assert_eq!(second.index, 1);
    assert_eq!(
        second
            .changed_objects
            .as_ref()
            .unwrap()
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        [ACTOR]
    );
    assert_eq!(
        second.scene.objects[&ACTOR].properties["values"],
        Value::Array(vec![Value::U64(3), Value::U64(7)].into())
    );
    assert!(
        !first.scene.objects[&ACTOR]
            .properties
            .contains_key("values")
    );
}

#[test]
fn lifetimes_and_room_directory_update_at_completed_boundaries() {
    let mut live = LatestScene::default();
    let room = FrameObject {
        id: SCENE,
        generation: 0,
    };
    live.feed(&initialization()).unwrap();
    live.feed(&group(&[frame(0), destroy(ACTOR), create(3, ACTOR)]))
        .unwrap();
    let first = live.latest().unwrap();
    assert_eq!(first.scene.objects[&ACTOR].generation, 0);
    assert_eq!(first.rooms.get(room).unwrap().first_observed_frame, 0);

    live.feed(&group(&[frame(1), reference(ACTOR), destroy(SCENE)]))
        .unwrap();
    let second = live.latest().unwrap();
    assert_eq!(second.scene.objects[&ACTOR].generation, 1);
    assert!(
        second
            .scene
            .resolve_reference(SCENE, "RigidDynamics", ACTOR)
            .is_none()
    );
    assert_eq!(second.rooms.get(room).unwrap().destroyed_at_frame, None);

    live.feed(&group(&[frame(2)])).unwrap();
    let (index, rooms) = live.summary().unwrap();
    assert_eq!(index, 2);
    assert_eq!(rooms.get(room).unwrap().destroyed_at_frame, Some(2));
    assert!(!live.latest().unwrap().scene.objects.contains_key(&SCENE));
}

#[test]
fn scene_query_properties_follow_the_latest_completed_endpoint() {
    let mut live = LatestScene::default();
    live.feed(&initialization()).unwrap();
    live.feed(&group(&[raycast(4.0), frame(0), raycast(12.0)]))
        .unwrap();
    let first = pvd_core::queries::collect(&live.latest().unwrap().scene);
    assert!(first.diagnostics.is_empty());
    assert_eq!(first.raycasts[0].origin, [4.0, 0.0, 0.0]);

    live.feed(&group(&[frame(1)])).unwrap();
    let second = pvd_core::queries::collect(&live.latest().unwrap().scene);
    assert!(second.diagnostics.is_empty());
    assert_eq!(second.raycasts[0].origin, [12.0, 0.0, 0.0]);
}

#[test]
fn detached_frames_reuse_the_snapshot_across_many_boundaries() {
    let mut live = LatestScene::default();
    live.feed(&initialization()).unwrap();
    live.feed(&group(&[frame(0)])).unwrap();
    let address = Arc::as_ptr(&live.latest().unwrap());
    let endpoint = group(&[frame(1)]);
    for _ in 0..20_000 {
        live.feed(&endpoint).unwrap();
        assert_eq!(Arc::as_ptr(&live.latest().unwrap()), address);
    }
    let latest = live.latest().unwrap();
    assert_eq!(latest.index, 20_000);
    assert!(latest.changed_objects.as_ref().unwrap().is_empty());
}

#[test]
fn changes_coalesce_across_recent_frames_and_expire() {
    let mut live = LatestScene::default();
    live.feed(&initialization()).unwrap();
    live.feed(&group(&[
        frame(0),
        begin_values(),
        append_value(3),
        vec![10],
        frame(1),
        destroy(ACTOR),
        frame(2),
        frame(3),
    ]))
    .unwrap();
    let latest = live.latest().unwrap();
    assert_eq!(latest.changes_since(0), Some((vec![ACTOR], true)));
    assert_eq!(latest.changes_since(2), Some((vec![], false)));
    assert_eq!(latest.changes_since(4), None);

    let endpoint = group(&[frame(4)]);
    for _ in 0..8 {
        live.feed(&endpoint).unwrap();
    }
    let frame = live.latest().unwrap();
    assert_eq!(frame.changes_since(0), None);
    assert_eq!(frame.changes_since(3), Some((vec![], false)));
}

#[test]
fn publishing_relative_reference_appends_reuses_the_array_after_the_initial_split() {
    let mut live = LatestScene::default();
    let reader = live.reader();
    let mut view = LatestSceneView::default();
    live.feed(&initialization()).unwrap();
    let mut events: Vec<_> = (100..8_292).map(reference).collect();
    events.push(frame(0));
    live.feed(&group(&events)).unwrap();
    reader.sync_view(&mut view, None).unwrap();
    live.feed(&group(&[reference(8_292), frame(1)])).unwrap();
    reader.sync_view(&mut view, None).unwrap();

    let references = |scene: &pvd_core::SceneState| -> *const Vec<Value> {
        let Value::Array(values) = &scene.objects[&SCENE].properties["RigidDynamics"] else {
            panic!("expected reference array");
        };
        Arc::as_ptr(values)
    };
    let committed = references(&live.latest().unwrap().scene);
    let displayed = references(view.scene());
    for (index, target) in (8_293..8_325).enumerate() {
        live.feed(&group(&[reference(target), frame(index as u64 + 2)]))
            .unwrap();
        reader.sync_view(&mut view, None).unwrap();
        let snapshot = live.latest().unwrap();
        let Value::Array(values) = &snapshot.scene.objects[&SCENE].properties["RigidDynamics"]
        else {
            panic!("expected reference array");
        };
        assert_eq!(values.last(), Some(&Value::Ref(target)));
        assert_eq!(Arc::as_ptr(values), committed);
        assert_eq!(references(view.scene()), displayed);
        assert_eq!(view.scene(), &snapshot.scene);
    }
}

#[test]
fn publishing_reference_removals_shares_the_resulting_array() {
    let mut live = LatestScene::default();
    let reader = live.reader();
    let mut view = LatestSceneView::default();
    live.feed(&initialization()).unwrap();
    live.feed(&group(&[reference(7), reference(8), frame(0)]))
        .unwrap();
    reader.sync_view(&mut view, None).unwrap();
    live.feed(&group(&[reference(9), frame(1)])).unwrap();
    reader.sync_view(&mut view, None).unwrap();
    live.feed(&group(&[
        remove_reference(7),
        reference(7),
        remove_reference(8),
        frame(2),
    ]))
    .unwrap();
    reader.sync_view(&mut view, None).unwrap();
    let published = live.latest().unwrap();
    let Value::Array(visible) = &view.scene().objects[&SCENE].properties["RigidDynamics"] else {
        panic!("expected reference array");
    };
    let Value::Array(latest) = &published.scene.objects[&SCENE].properties["RigidDynamics"] else {
        panic!("expected reference array");
    };
    assert_eq!(
        visible.as_slice(),
        &[Value::Ref(ACTOR), Value::Ref(9), Value::Ref(7)]
    );
    assert!(Arc::ptr_eq(visible, latest));
}

#[test]
fn reference_edits_catch_up_across_absolute_assignment_of_the_same_property() {
    let mut live = LatestScene::default();
    let reader = live.reader();
    let mut view = LatestSceneView::default();
    live.feed(&initialization()).unwrap();
    live.feed(&group(&[frame(0)])).unwrap();
    reader.sync_view(&mut view, None).unwrap();

    live.feed(&group(&[
        reference(9),
        frame(1),
        set_references(&[ACTOR, ACTOR]),
        frame(2),
        remove_reference(ACTOR),
        reference(8),
        reference(9),
        frame(3),
    ]))
    .unwrap();
    let update = reader.sync_view(&mut view, None).unwrap();
    assert_eq!(update.changes, Some((vec![SCENE], true)));
    assert_eq!(view.scene(), &live.latest().unwrap().scene);
    assert_eq!(
        view.scene().objects[&SCENE].properties["RigidDynamics"],
        Value::Array(vec![Value::Ref(8), Value::Ref(9)].into())
    );

    live.feed(&group(&[set_references(&[ACTOR]), frame(4)]))
        .unwrap();
    reader.sync_view(&mut view, None).unwrap();
    live.feed(&group(&[reference(9), create(3, 9), frame(5)]))
        .unwrap();
    reader.sync_view(&mut view, None).unwrap();
    assert_eq!(view.scene(), &live.latest().unwrap().scene);
    assert_eq!(
        view.scene().objects[&SCENE].properties["RigidDynamics"],
        Value::Ref(ACTOR)
    );
    assert!(
        view.scene()
            .resolve_reference(SCENE, "RigidDynamics", 9)
            .is_some()
    );
}

#[test]
fn view_catches_up_across_reference_rebinding_replacement_and_expired_changes() {
    let mut live = LatestScene::default();
    let reader = live.reader();
    let mut view = LatestSceneView::default();
    assert!(reader.sync_view(&mut view, None).is_err());
    live.feed(&initialization()).unwrap();
    live.feed(&group(&[
        frame(0),
        destroy(ACTOR),
        create(3, ACTOR),
        frame(1),
    ]))
    .unwrap();
    let update = reader.sync_view(&mut view, None).unwrap();
    assert_eq!(update.index, 1);
    assert_eq!(update.changes, None);
    assert_eq!(view.scene(), &live.latest().unwrap().scene);
    assert!(
        view.scene()
            .resolve_reference(SCENE, "RigidDynamics", ACTOR)
            .is_none()
    );

    let forward = 3;
    live.feed(&group(&[
        reference(forward),
        reference(ACTOR),
        reference(ACTOR),
        begin_values(),
        append_value(3),
        vec![10],
        frame(2),
        create(3, forward),
        remove_reference(ACTOR),
        reference(ACTOR),
        begin_values(),
        append_value(7),
        vec![10],
        frame(3),
    ]))
    .unwrap();
    let update = reader.sync_view(&mut view, None).unwrap();
    assert_eq!(update.index, 3);
    assert_eq!(update.changes, Some((vec![SCENE, ACTOR, forward], true)));
    assert_eq!(view.scene(), &live.latest().unwrap().scene);
    let membership = &view.scene().objects[&SCENE].properties["RigidDynamics"];
    assert_eq!(
        membership,
        &Value::Array(vec![Value::Ref(forward), Value::Ref(ACTOR)].into())
    );
    assert_eq!(
        view.scene()
            .resolve_reference(SCENE, "RigidDynamics", ACTOR)
            .unwrap()
            .generation,
        1
    );
    assert_eq!(
        view.scene()
            .resolve_reference(SCENE, "RigidDynamics", forward)
            .unwrap()
            .generation,
        0
    );
    assert_eq!(
        view.scene().objects[&ACTOR].properties["values"],
        Value::U64(7)
    );
    assert_eq!(
        reader.sync_view(&mut view, None).unwrap().changes,
        Some((vec![], false))
    );

    for index in 4..13 {
        live.feed(&group(&[reference(index + 10), frame(index)]))
            .unwrap();
    }
    let update = reader.sync_view(&mut view, None).unwrap();
    assert_eq!(update.index, 12);
    assert_eq!(update.changes, None);
    assert_eq!(view.scene(), &live.latest().unwrap().scene);
    assert_eq!(
        view.rooms().iter().collect::<Vec<_>>(),
        live.latest().unwrap().rooms.iter().collect::<Vec<_>>()
    );
}

#[test]
fn a_reader_can_use_its_view_while_the_producer_publishes() {
    let mut live = LatestScene::default();
    let reader = live.reader();
    let mut view = LatestSceneView::default();
    live.feed(&initialization()).unwrap();
    live.feed(&group(&[frame(0)])).unwrap();
    reader.sync_view(&mut view, None).unwrap();

    let visible = &view.scene().objects[&SCENE].properties["RigidDynamics"];
    let (sender, receiver) = std::sync::mpsc::channel();
    let producer = std::thread::spawn(move || {
        live.feed(&group(&[reference(4), frame(1)])).unwrap();
        sender.send(live.summary().unwrap().0).unwrap();
    });
    assert_eq!(
        receiver.recv_timeout(std::time::Duration::from_secs(5)),
        Ok(1)
    );
    assert_eq!(visible, &Value::Array(vec![Value::Ref(ACTOR)].into()));
    producer.join().unwrap();
    assert_eq!(reader.sync_view(&mut view, None).unwrap().index, 1);
    assert_eq!(
        view.scene().objects[&SCENE].properties["RigidDynamics"],
        Value::Array(vec![Value::Ref(ACTOR), Value::Ref(4)].into())
    );
}

#[test]
fn cancellation_keeps_the_last_published_frame() {
    let mut live = LatestScene::default();
    let stop = Arc::new(AtomicBool::new(false));
    live.set_cancellation(stop.clone());
    live.feed(&initialization()).unwrap();
    live.feed(&group(&[frame(0)])).unwrap();
    stop.store(true, Ordering::Release);
    let error = live.feed(&group(&[frame(1)])).unwrap_err();
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
    assert_eq!(live.latest().unwrap().index, 0);
}
