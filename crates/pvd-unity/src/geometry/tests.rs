use super::*;
use pvd_core::{RenderObject, RenderScene};
use std::sync::Arc;

pub(super) fn frame(geometry: Geometry) -> (SceneState, RenderScene) {
    let mut state = SceneState::default();
    for (id, generation) in [(5, 2), (8, 3)] {
        state.objects.insert(
            id,
            Object {
                id,
                generation,
                type_name: String::new(),
                properties: Default::default(),
            },
        );
    }
    let render = RenderScene {
        objects: vec![RenderObject {
            id: 5,
            shape_id: 8,
            position: [100.; 3],
            rotation: [0., 0., 1., 0.],
            geometry,
            dynamic: true,
            sleeping: true,
        }],
        diagnostics: vec![],
    };
    (state, render)
}

fn bytes(geometry: Geometry) -> Result<Vec<u8>, String> {
    let (state, render) = frame(geometry);
    encode_shapes(&state, render, &BTreeMap::new())
}

#[test]
fn frames_identify_the_surface_protocol() {
    let data = bytes(Geometry::Plane).unwrap();
    assert_eq!(&data[..8], b"PVS1\x01\0\0\0");
    assert_eq!(data.len(), 8 + 90);
}

#[test]
fn records_keep_identity_local_pose_and_triangle_topology() {
    let (mut state, render) = frame(Geometry::TriangleMesh {
        vertices: Arc::new(vec![[1., 2., 3.], [4., 5., 6.], [7., 8., 9.]]),
        indices: Arc::new(vec![2, 0, 1]),
    });
    for (id, property, position, rotation) in [
        (5, "GlobalPose", [10., 20., 30.], [0., 0., -2., 0.]),
        (8, "LocalPose", [3., 2., 1.], [0., 2., 0., 0.]),
    ] {
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .properties
            .insert(property.into(), Value::Transform { position, rotation });
    }
    let data = encode_shapes(&state, render, &BTreeMap::from([(5, 17)])).unwrap();
    let mut expected = b"PVS1\x01\0\0\0".to_vec();
    expected.extend(17u64.to_le_bytes());
    expected.extend(5u64.to_le_bytes());
    expected.extend(2u32.to_le_bytes());
    expected.extend(8u64.to_le_bytes());
    expected.extend(3u32.to_le_bytes());
    expected.extend([2, 4]);
    for value in [10f32, 20., 30., 0., 0., -1., 0., 3., 2., 1., 0., 1., 0., 0.] {
        expected.extend(value.to_le_bytes());
    }
    expected.extend(3u32.to_le_bytes());
    expected.extend(3u32.to_le_bytes());
    for v in 1..=9 {
        expected.extend((v as f32).to_le_bytes());
    }
    for i in [2u32, 0, 1] {
        expected.extend(i.to_le_bytes());
    }
    assert_eq!(data, expected);
}

#[test]
fn primitives_encode_dimensions_and_default_poses() {
    for (geometry, kind, dimensions) in [
        (
            Geometry::Box {
                half_extents: [1., 2., 3.],
            },
            0,
            vec![1., 2., 3.],
        ),
        (Geometry::Sphere { radius: 7. }, 1, vec![7.]),
        (
            Geometry::Capsule {
                radius: 2.,
                half_height: 4.,
            },
            2,
            vec![2., 4.],
        ),
        (Geometry::Plane, 3, vec![]),
    ] {
        let data = bytes(geometry).unwrap();
        assert_eq!(data[41], kind);
        let values: Vec<_> = data[42..]
            .chunks_exact(4)
            .map(|word| f32::from_le_bytes(word.try_into().unwrap()))
            .collect();
        assert_eq!(
            values[..14],
            [0., 0., 0., 0., 0., 0., 1., 0., 0., 0., 0., 0., 0., 1.]
        );
        assert_eq!(&values[14..], dimensions);
    }
}

#[test]
fn invalid_geometry_pose_and_oversized_payload_return_errors() {
    for (point, indices) in [
        ([0.; 3], vec![0, 0]),
        ([0.; 3], vec![0, 0, 1]),
        ([f32::NAN; 3], vec![0, 0, 0]),
    ] {
        assert!(
            bytes(Geometry::TriangleMesh {
                vertices: Arc::new(vec![point]),
                indices: Arc::new(indices)
            })
            .is_err()
        );
    }
    assert!(bytes(Geometry::Sphere { radius: -1. }).is_err());
    assert!(
        bytes(Geometry::TriangleMesh {
            vertices: Arc::new(vec![[0.; 3]]),
            indices: Arc::new(vec![0; MAX_BYTES / 4 + 1])
        })
        .is_err()
    );
    for (position, rotation) in [([0.; 3], [0.; 4]), ([f32::NAN; 3], [0., 0., 0., 1.])] {
        let (mut state, render) = frame(Geometry::Plane);
        state
            .objects
            .get_mut(&5)
            .unwrap()
            .properties
            .insert("GlobalPose".into(), Value::Transform { position, rotation });
        assert!(encode_shapes(&state, render, &BTreeMap::new()).is_err());
    }
}

#[test]
fn capture_focus_requires_a_current_rigid_actor_and_recorded_position() {
    let mut state = SceneState::default();
    let mut request = FocusRequest {
        mode: 2,
        actor: 5,
        generation: 2,
        center: [f32::NAN; 3],
        radius: 5.,
    };
    let error = |request: FocusRequest, state: &SceneState| request.region(state).err().unwrap();
    assert!(error(request, &state).contains("missing"));
    state.objects.insert(
        5,
        Object {
            id: 5,
            generation: 2,
            type_name: "physx3::PxShape".into(),
            properties: Default::default(),
        },
    );
    assert!(error(request, &state).contains("rigid actor"));
    state.objects.get_mut(&5).unwrap().type_name = "physx3::PxArticulationLink".into();
    assert!(error(request, &state).contains("GlobalPose"));

    for position in [[f32::INFINITY, 0., 0.], [12., 0., 0.]] {
        state.objects.get_mut(&5).unwrap().properties.insert(
            "GlobalPose".into(),
            Value::Transform {
                position,
                rotation: [0., 0., 0., 1.],
            },
        );
        assert_eq!(request.region(&state).is_ok(), position[0].is_finite());
    }
    request.generation += 1;
    assert!(error(request, &state).contains("generation"));
}

#[test]
fn external_focus_uses_only_a_finite_center_and_positive_radius() {
    let state = SceneState::default();
    let mut request = FocusRequest {
        mode: 1,
        actor: 1,
        generation: 42,
        center: [0., 1., 2.],
        radius: 5.,
    };
    assert!(request.region(&state).is_ok());
    for radius in [0., -1., f32::NAN, f32::INFINITY] {
        request.radius = radius;
        assert!(request.region(&state).is_err());
    }
    request.radius = 5.;
    request.center[1] = f32::NAN;
    assert!(request.region(&state).is_err());
    request.center[1] = 1.;
    request.mode = 0;
    assert!(request.region(&state).is_err());
}

fn shared_shape_recording(far_statics: u64) -> pvd_core::Recording {
    pvd_core::Recording::from_bytes(&shared_shape_stream(far_statics)).unwrap()
}

fn shared_shape_stream(far_statics: u64) -> Vec<u8> {
    // The actor on aggregate 70 has no Scene property. Its only room evidence comes from
    // Scene.Aggregates -> Aggregate.Actors, while the other actor is listed directly.
    let strings = [
        "physx3",
        "frame",
        "PxScene",
        "PxRigidStatic",
        "PxRigidDynamic",
        "PxShape",
        "PxPlaneGeometry",
        "PxAggregate",
        "ObjectRef",
        "Shapes",
        "Geometry",
        "Actors",
        "Aggregates",
        "RigidStatics",
        "GlobalPose",
        "PxTransform",
    ];
    let handle = |name: &str| strings.iter().position(|v| *v == name).unwrap() as u32 + 1;
    let mut events = Vec::<Vec<u8>>::new();
    for name in strings {
        let mut e = vec![1];
        e.extend((name.len() as u32 + 1).to_le_bytes());
        e.extend(name.bytes());
        e.push(0);
        e.extend(handle(name).to_le_bytes());
        events.push(e);
    }
    for (id, name) in [
        (10u64, "PxScene"),
        (20, "PxScene"),
        (30, "PxRigidStatic"),
        (40, "PxRigidDynamic"),
        (50, "PxShape"),
        (60, "PxPlaneGeometry"),
        (70, "PxAggregate"),
    ] {
        let mut e = vec![6];
        e.extend(1u32.to_le_bytes());
        e.extend(handle(name).to_le_bytes());
        e.extend(id.to_le_bytes());
        events.push(e);
    }
    let property = |owner: u64, name: &str, target: u64| {
        property_event(
            owner,
            handle(name),
            handle("ObjectRef"),
            &target.to_le_bytes(),
        )
    };
    for (owner, name, target) in [
        (10, "RigidStatics", 30),
        (20, "Aggregates", 70),
        (70, "Actors", 40),
        (30, "Shapes", 50),
        (40, "Shapes", 50),
        (50, "Geometry", 60),
    ] {
        events.push(property(owner, name, target));
    }
    for id in 100..100 + far_statics {
        let mut event = vec![6];
        event.extend(1u32.to_le_bytes());
        event.extend(handle("PxRigidStatic").to_le_bytes());
        event.extend(id.to_le_bytes());
        events.push(event);
        events.push(property(id, "Shapes", 50));
    }
    for tick in 0..2u64 {
        if tick == 1 {
            events.push(property(20, "RigidStatics", 30));
        }
        events.push(scene_frame(tick));
    }

    let mut stream = vec![];
    stream.extend(837_150_850u32.to_le_bytes());
    stream.extend(1u32.to_le_bytes());
    stream.extend(1u64.to_le_bytes());
    stream.extend(10u64.to_le_bytes());
    stream.extend(1u32.to_le_bytes());
    stream.extend(event_group(events, 0));
    stream
}

fn property_event(owner: u64, property: u32, kind: u32, value: &[u8]) -> Vec<u8> {
    let mut event = vec![7];
    event.extend(owner.to_le_bytes());
    for word in [property, 1, kind, 1, value.len() as u32] {
        event.extend(word.to_le_bytes());
    }
    event.extend(value);
    event
}

fn scene_frame(tick: u64) -> Vec<u8> {
    let mut event = vec![19];
    event.extend(10u64.to_le_bytes());
    event.extend(2u32.to_le_bytes());
    event.extend(tick.to_le_bytes());
    event
}

fn event_group(events: Vec<Vec<u8>>, stamp: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend((events.iter().map(Vec::len).sum::<usize>() as u32).to_le_bytes());
    bytes.extend((events.len() as u32).to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(stamp.to_le_bytes());
    bytes.extend(events.into_iter().flatten());
    bytes
}

#[test]
fn focused_latest_frames_keep_the_index_and_update_movement_and_rooms_across_skipped_frames() {
    use pvd_core::{LatestScene, LatestSceneView};

    let mut producer = LatestScene::default();
    producer.feed(&shared_shape_stream(32)).unwrap();
    let reader = producer.reader();
    let mut view = LatestSceneView::default();
    let mut cache = FrameCache::default();
    let visible = |cache: &mut FrameCache, view: &LatestSceneView, x| {
        let region = RenderRegion::new([x, 0., 0.], 1.).unwrap();
        let (_, rooms) = cache
            .render(view.scene(), view.rooms(), (1, 2), 0, Some(region))
            .unwrap();
        rooms.into_keys().collect::<Vec<_>>()
    };
    let first = reader.sync_view(&mut view, None).unwrap();
    cache.sync_latest(1, view.scene(), first.changes);
    assert_eq!(visible(&mut cache, &view, 0.), [30, 40]);
    let revision = cache.rooms.scope.revision();

    let pose: Vec<_> = [0f32, 0., 0., 1., 100., 0., 0.]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect();
    for (stamp, actor) in [(2, 30), (3, 40)] {
        producer
            .feed(&event_group(
                vec![property_event(actor, 15, 16, &pose), scene_frame(stamp)],
                stamp,
            ))
            .unwrap();
    }
    assert_eq!(visible(&mut cache, &view, 0.), [30, 40]);
    assert!(visible(&mut cache, &view, 100.).is_empty());
    let moved = reader.sync_view(&mut view, None).unwrap();
    cache.sync_latest(1, view.scene(), moved.changes);
    assert!(cache.spatial.is_some());
    assert_eq!(cache.rooms.scope.revision(), revision);
    assert!(visible(&mut cache, &view, 0.).is_empty());
    assert_eq!(visible(&mut cache, &view, 100.), [30, 40]);

    producer
        .feed(&event_group(
            vec![
                property_event(20, 14, 9, &0u64.to_le_bytes()),
                scene_frame(4),
                scene_frame(5),
            ],
            4,
        ))
        .unwrap();
    assert_eq!(visible(&mut cache, &view, 100.), [30, 40]);
    let edited = reader.sync_view(&mut view, None).unwrap();
    cache.sync_latest(1, view.scene(), edited.changes);
    assert!(cache.spatial.is_some());
    assert_eq!(visible(&mut cache, &view, 100.), [40]);
    assert_ne!(cache.rooms.scope.revision(), revision);
}

#[test]
fn shared_shapes_keep_each_owner_and_seek_restores_verified_room_membership() {
    let recording = shared_shape_recording(0);
    let identities = |frame, room| {
        let state = recording.state_at(frame).unwrap();
        let bytes = encode(&state, &recording.rooms, room).unwrap();
        bytes[8..]
            .chunks_exact(RECORD_BYTES)
            .map(|record| {
                let word = |i: usize| u64::from_le_bytes(record[i..i + 8].try_into().unwrap());
                (word(0), word(8), word(20), record[32])
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(identities(1, 0), [(0, 30, 50, 0), (2, 40, 50, 1)]);
    assert_eq!(identities(0, 0), [(1, 30, 50, 0), (2, 40, 50, 1)]);
    assert_eq!(identities(0, 2), [(2, 40, 50, 1)]);

    let state = recording.state_at(0).unwrap();
    let region = RenderRegion::new([0.; 3], 1.).unwrap();
    let mut cache = FrameCache::default();
    let (scene, rooms) = cache
        .render(&state, &recording.rooms, (1, 2), 0, Some(region))
        .unwrap();
    assert_eq!(scene.objects.len(), 1);
    assert_eq!(scene.objects[0].id, 40);
    assert_eq!(rooms[&40], 2);

    let distant = RenderRegion::new([100.; 3], 1.).unwrap();
    let (scene, rooms) = cache
        .render(&state, &recording.rooms, (1, 0), 0, Some(distant))
        .unwrap();
    assert!(scene.objects.is_empty());
    assert!(
        rooms.is_empty(),
        "focused frames only label displayed actors"
    );

    cache.sync_spatial(1, &state, None);
    let (_, rooms) = cache
        .render(&state, &recording.rooms, (1, 0), 0, Some(region))
        .unwrap();
    assert_eq!(rooms, BTreeMap::from([(30, 1), (40, 2)]));
    let (_, rooms) = cache
        .render(&state, &recording.rooms, (1, 2), 0, Some(region))
        .unwrap();
    assert_eq!(rooms, BTreeMap::from([(40, 2)]));

    cache.retain_rooms(&SceneUpdate::default(), &state);
    cache.sync_spatial(1, &state, Some(&[]));
    assert_eq!(cache.rooms.scope.revision(), Some(cache.rooms.revision));

    let next = recording.state_at(1).unwrap();
    cache.sync_spatial(1, &next, Some(&[20]));
    let (_, rooms) = cache
        .render(&next, &recording.rooms, (1, 2), 0, Some(region))
        .unwrap();
    assert_eq!(rooms, BTreeMap::from([(30, 2), (40, 2)]));
    let (_, rooms) = cache
        .render(&next, &recording.rooms, (1, 0), 0, Some(region))
        .unwrap();
    assert_eq!(rooms, BTreeMap::from([(30, 0), (40, 2)]));

    cache.sync_spatial(1, &state, None);
    let (_, rooms) = cache
        .render(&state, &recording.rooms, (1, 2), 0, Some(region))
        .unwrap();
    assert_eq!(rooms, BTreeMap::from([(40, 2)]));
}

#[test]
fn focused_frames_keep_nearby_statics_and_follow_dynamic_and_static_motion() {
    let recording = shared_shape_recording(4096);
    let mut state = recording.state_at(0).unwrap();
    let move_actor = |state: &mut SceneState, id, x| {
        state.objects.get_mut(&id).unwrap().properties.insert(
            "GlobalPose".into(),
            Value::Transform {
                position: [x, 0., 0.],
                rotation: [0., 0., 0., 1.],
            },
        );
    };
    for id in 100..4196 {
        move_actor(&mut state, id, id as f32 * 20.);
    }
    let initial = state.clone();
    let region = RenderRegion::new([0.; 3], 1.).unwrap();
    let mut cache = FrameCache::default();
    let visible = |cache: &mut FrameCache, state: &SceneState, center: RenderRegion| {
        cache
            .render(state, &recording.rooms, (1, 0), 0, Some(center))
            .unwrap()
            .0
            .objects
            .iter()
            .map(|shape| shape.id)
            .collect::<Vec<_>>()
    };
    cache.sync_spatial(1, &state, None);
    assert_eq!(visible(&mut cache, &state, region), [30, 40]);
    let index = cache.spatial.as_mut().unwrap();
    assert_eq!(index.candidates(region).unwrap(), [30, 40]);
    assert_eq!(
        index
            .candidates(RenderRegion::new([3000., 0., 0.], 1.).unwrap())
            .unwrap(),
        [40, 150]
    );

    move_actor(&mut state, 40, 100.);
    cache.sync_spatial(1, &state, Some(&[40]));
    assert_eq!(visible(&mut cache, &state, region), [30]);

    move_actor(&mut state, 30, 100.);
    cache.sync_spatial(1, &state, Some(&[30]));
    assert!(visible(&mut cache, &state, region).is_empty());

    cache.sync_spatial(1, &initial, None);
    assert_eq!(visible(&mut cache, &initial, region), [30, 40]);
}

#[test]
fn acknowledged_meshes_use_references_and_changed_meshes_resend() {
    let mesh = |x| Geometry::TriangleMesh {
        vertices: Arc::new(vec![[x, 0., 0.]]),
        indices: Arc::new(vec![0, 0, 0]),
    };
    let mut cache = FrameCache::default();
    let mut request = |x, base, session, room| {
        let (state, render) = frame(mesh(x));
        cache
            .write(&state, render, &BTreeMap::new(), (session, room), base)
            .unwrap()
    };
    let token = |b: &[u8]| u64::from_le_bytes(b[4..12].try_into().unwrap());
    let first = request(1., 0, 1, 0);
    assert_eq!(&first[..4], b"PVS2");
    assert_eq!(first[57], 4);
    let repeated = request(1., token(&first), 1, 0);
    assert_eq!(repeated[57], 5);
    assert_eq!(repeated.len(), 24 + RECORD_BYTES);
    let changed = request(2., token(&repeated), 1, 0);
    assert_eq!(changed[57], 4);
    let back = request(1., token(&changed), 1, 0);
    assert_eq!(back[57], 4);
    let room = request(1., token(&back), 1, 2);
    assert_eq!(room[57], 4);
    let session = request(1., token(&room), 2, 2);
    assert_eq!(session[57], 4);
    let stale = request(1., token(&first), 2, 2);
    assert_eq!(stale[57], 4);
    let full = request(1., 0, 2, 2);
    assert_eq!(full[57], 4);
}

#[test]
fn rejected_frame_preserves_acknowledged_mesh_and_generation_changes_resend() {
    let geometry = |index| Geometry::TriangleMesh {
        vertices: Arc::new(vec![[0., 0., 0.], [1., 0., 0.]]),
        indices: Arc::new(vec![0, index, 0]),
    };
    let mut cache = FrameCache::default();
    let rooms = BTreeMap::new();
    let (mut state, scene) = frame(geometry(0));
    let first = cache
        .write(&state, scene.clone(), &rooms, (1, 0), 0)
        .unwrap();
    let token = |b: &[u8]| u64::from_le_bytes(b[4..12].try_into().unwrap());
    let (_, broken) = frame(geometry(2));
    assert!(
        cache
            .write(&state, broken, &rooms, (1, 0), token(&first))
            .is_err()
    );
    let retry = cache
        .write(&state, scene.clone(), &rooms, (1, 0), token(&first))
        .unwrap();
    assert_eq!(retry[57], 5);
    let (_, changed) = frame(geometry(1));
    let updated = cache
        .write(&state, changed.clone(), &rooms, (1, 0), token(&retry))
        .unwrap();
    assert_eq!(updated[57], 4);
    state.objects.get_mut(&8).unwrap().generation += 1;
    let respawn = cache
        .write(&state, changed, &rooms, (1, 0), token(&updated))
        .unwrap();
    assert_eq!(respawn[57], 4);
}
