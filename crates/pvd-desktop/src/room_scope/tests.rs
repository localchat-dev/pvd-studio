use super::*;
use pvd_core::{Recording, RecordingBuilder};
use pvd_protocol::{Endian, Event, EventGroup, Header, Name};

fn name(handle: u32) -> Name {
    Name {
        namespace: 1,
        name: handle,
    }
}

fn create(id: u64, class: u32) -> Event {
    Event::CreateInstance {
        id,
        class: name(class),
    }
}

fn reference(id: u64, property: u32, target: u64) -> Event {
    Event::SetPropertyValue {
        id,
        property,
        datatype: name(20),
        items: 1,
        data: target.to_le_bytes().to_vec().into(),
    }
}

fn recording() -> Recording {
    let mut builder = RecordingBuilder::new(Header {
        version: 1,
        timestamp_numerator: 1,
        timestamp_denominator: 1,
        flags: 1,
        endian: Endian::Little,
    });
    let mut events: Vec<_> = [
        (1, "physx3"),
        (2, "PxScene"),
        (3, "PxRigidDynamic"),
        (4, "PxShape"),
        (5, "PxSphereGeometry"),
        (6, "PxMaterial"),
        (7, "PxArticulation"),
        (8, "PxArticulationLink"),
        (9, "PxCloth"),
        (10, "PxClothFabric"),
        (11, "PxParticleSystem"),
        (20, "ObjectRef"),
        (21, "Scene"),
        (22, "Shapes"),
        (23, "Geometry"),
        (24, "Materials"),
        (25, "Parent"),
        (26, "Fabric"),
        (27, "Actors"),
        (28, "frame"),
        (29, "Actor"),
    ]
    .into_iter()
    .map(|(handle, value)| Event::StringHandle {
        handle,
        value: value.into(),
    })
    .collect();
    events.extend((2..=11).map(|handle| Event::CreateClass { name: name(handle) }));
    events.extend([
        create(100, 2),
        create(200, 2),
        create(10, 3),
        create(20, 3),
        create(30, 4),
        create(40, 5),
        create(50, 6),
        create(60, 7),
        create(61, 8),
        create(62, 8),
        create(70, 9),
        create(71, 10),
        create(80, 11),
        reference(10, 21, 100),
        reference(20, 21, 200),
        reference(100, 27, 10),
        reference(200, 27, 20),
        reference(10, 22, 30),
        reference(20, 22, 30),
        reference(30, 23, 40),
        reference(30, 24, 50),
        reference(30, 29, 20),
        reference(60, 21, 100),
        reference(61, 25, 60),
        reference(62, 25, 61),
        reference(70, 21, 100),
        reference(70, 26, 71),
        reference(80, 21, 200),
    ]);
    for (index, mut events) in [
        events,
        vec![reference(10, 21, 200)],
        vec![Event::DestroyInstance { id: 100 }, create(100, 2)],
        vec![reference(60, 21, 100)],
        vec![Event::DestroyInstance { id: 30 }, create(30, 4)],
    ]
    .into_iter()
    .enumerate()
    {
        events.push(Event::EndSection {
            id: 200,
            name: 28,
            timestamp: index as u64 + 1,
        });
        builder
            .push(EventGroup {
                stream_id: 0,
                timestamp: 0,
                offset: 0,
                events,
            })
            .unwrap();
    }
    builder.snapshot().unwrap()
}

fn room(id: u64, generation: u32) -> Option<FrameObject> {
    Some(FrameObject { id, generation })
}

#[test]
fn shared_resources_articulation_and_point_sources_follow_verified_room_membership() {
    let state = recording().state_at(0).unwrap();
    let mut scope = RoomScope::default();
    scope.update(&state, room(100, 0), 0);
    assert_eq!(
        scope.objects(),
        &BTreeSet::from([100, 10, 30, 40, 50, 60, 61, 62, 70, 71])
    );
    assert_eq!(scope.actors(), &BTreeSet::from([10, 61, 62]));
    assert_eq!(scope.cloths(), &BTreeSet::from([70]));
    assert!(scope.particles().is_empty());
    scope.update(&state, room(200, 0), 0);
    assert_eq!(scope.objects(), &BTreeSet::from([200, 20, 30, 40, 50, 80]));
    assert_eq!(scope.actors(), &BTreeSet::from([20]));
    assert_eq!(scope.particles(), &BTreeSet::from([80]));
    assert!(scope.cloths().is_empty());
}

#[test]
fn transfers_and_reused_scene_shape_ids_never_restore_stale_membership() {
    let recording = recording();
    let mut scope = RoomScope::default();
    let transferred = recording.state_at(1).unwrap();
    scope.update(&transferred, room(100, 0), 1);
    assert!(!scope.contains(10));
    assert!(!scope.contains(30));
    scope.update(&transferred, room(200, 0), 1);
    assert_eq!(scope.actors(), &BTreeSet::from([10, 20]));
    let reused = recording.state_at(2).unwrap();
    scope.update(&reused, room(100, 0), 2);
    assert!(scope.objects().is_empty());
    scope.update(&reused, room(100, 1), 2);
    assert_eq!(scope.objects(), &BTreeSet::from([100]));
    let refreshed = recording.state_at(3).unwrap();
    scope.update(&refreshed, room(100, 1), 3);
    assert_eq!(scope.objects(), &BTreeSet::from([100, 60, 61, 62]));
    scope.update(&recording.state_at(4).unwrap(), room(200, 0), 4);
    assert_eq!(scope.objects(), &BTreeSet::from([200, 10, 20, 80]));
}

#[test]
fn pose_updates_reuse_membership_while_reference_changes_invalidate_it() {
    let recording = recording();
    let mut state = recording.state_at(0).unwrap();
    let mut scope = RoomScope::default();
    scope.update(&state, room(100, 0), 1);
    let version = scope.version();
    let mut moved = state.clone();
    moved.objects.get_mut(&10).unwrap().properties.insert(
        "GlobalPose".into(),
        Value::Transform {
            position: [1.0, 2.0, 3.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
        },
    );
    let mut motion = SceneUpdate::full(&moved);
    motion.replace_all = false;
    scope.retain_for_update(&motion, &state, 1);
    assert_eq!(scope.revision, Some(2));
    motion.apply_owned(&mut state);
    scope.update(&state, room(100, 0), 2);
    assert_eq!(scope.version(), version);
    let next = recording.state_at(1).unwrap();
    let mut transferred = SceneUpdate::full(&next);
    transferred.replace_all = false;
    scope.retain_for_update(&transferred, &state, 2);
    assert_eq!(scope.revision, Some(2));
    transferred.apply_owned(&mut state);
    scope.update(&state, room(100, 0), 3);
    assert!(!scope.contains(10));
    assert_ne!(scope.version(), version);
}

#[test]
fn activity_summary_uses_room_actors_and_its_own_sdk_statistics() {
    let mut state = recording().state_at(0).unwrap();
    for (id, dynamic, kinematic) in [(100, 3, 0), (200, 0, 1)] {
        state.objects.get_mut(&id).unwrap().properties.extend([
            (
                "SimulationStatistics.NbActiveDynamicBodies".into(),
                Value::U64(dynamic),
            ),
            (
                "SimulationStatistics.NbActiveKinematicBodies".into(),
                Value::U64(kinematic),
            ),
        ]);
    }
    let mut activity = crate::activity::ActivityCache::default();
    activity.rebuild(&state);
    let mut scope = RoomScope::default();
    scope.update(&state, room(100, 0), 1);
    let a = activity.summary_scoped(&scope);
    assert_eq!(a.total_actors, 3);
    assert_eq!(a.sdk_active_total(), Some(3));
    assert_eq!(a.sdk_scene_count, 1);
    scope.update(&state, room(200, 0), 1);
    let b = activity.summary_scoped(&scope);
    assert_eq!(b.total_actors, 1);
    assert_eq!(b.sdk_active_total(), Some(1));
    scope.update(&state, None, 1);
    assert_eq!(activity.summary_scoped(&scope), activity.summary());
}

#[test]
fn viewport_switches_same_revision_rooms_and_removes_transferred_actors() {
    use crate::{
        scene::{Viewport, sync_scene},
        studio::Studio,
    };
    use bevy::prelude::*;
    let recording = recording();
    let mut state = recording.state_at(0).unwrap();
    state
        .objects
        .get_mut(&40)
        .unwrap()
        .properties
        .insert("Radius".into(), Value::F32(1.0));
    let mut app = App::new();
    app.insert_resource(Studio::default())
        .insert_resource(Viewport::default())
        .insert_resource(Assets::<Mesh>::default())
        .insert_resource(Assets::<StandardMaterial>::default())
        .add_systems(Update, sync_scene);
    {
        let studio = &mut *app.world_mut().resource_mut::<Studio>();
        studio.state = state;
        studio.revision = 1;
    }
    for (selected_room, expected) in [
        (None, vec![10, 20]),
        (room(100, 0), vec![10]),
        (room(200, 0), vec![20]),
        (room(100, 0), vec![10]),
    ] {
        {
            let studio = &mut *app.world_mut().resource_mut::<Studio>();
            studio
                .room_scope
                .update(&studio.state, selected_room, studio.revision);
        }
        app.update();
        let viewport = app.world().resource::<Viewport>();
        assert_eq!(
            viewport
                .objects
                .iter()
                .map(|object| object.id)
                .collect::<Vec<_>>(),
            expected
        );
    }
    {
        let studio = &mut *app.world_mut().resource_mut::<Studio>();
        studio.state = recording.state_at(1).unwrap();
        studio
            .state
            .objects
            .get_mut(&40)
            .unwrap()
            .properties
            .insert("Radius".into(), Value::F32(1.0));
        studio.revision += 1;
        studio
            .room_scope
            .update(&studio.state, room(100, 0), studio.revision);
    }
    app.update();
    assert!(app.world().resource::<Viewport>().objects.is_empty());
}

#[test]
fn bare_debug_instance_ids_remain_unassigned_including_text() {
    use pvd_core::debug_draw::Primitive;
    let mut studio = crate::studio::Studio::default();
    studio.state = recording().state_at(0).unwrap();
    studio.state.debug_draw.insert(
        999,
        std::sync::Arc::new(vec![
            Primitive::Point {
                instance: 100,
                position: [0.0; 3],
                color: 0xffffff,
            },
            Primitive::Text {
                instance: 10,
                position: [0.0; 3],
                size: 1.0,
                color: 0xffffff,
                text: "captured".into(),
            },
        ]),
    );
    assert_eq!(crate::scene::display_debug_primitives(&studio).count(), 2);
    studio.room_scope.update(&studio.state, room(100, 0), 1);
    assert_eq!(crate::scene::display_debug_primitives(&studio).count(), 0);
    studio.room_scope.update(&studio.state, None, 1);
    assert_eq!(crate::scene::display_debug_primitives(&studio).count(), 2);
}

#[test]
#[ignore = "requires PhysX 3.4 multiscene capture and same-run SDK pose truth"]
fn sdk_rooms_render_only_their_own_actors_at_every_captured_boundary() {
    let path = std::path::PathBuf::from(std::env::var_os("PVD_MULTISCENE_CAPTURE").unwrap());
    let truth: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path.with_extension("json")).unwrap()).unwrap();
    let recording = Recording::from_path(path).unwrap();
    let scenes = truth["scenes"].as_array().unwrap();
    let frames = truth["frames"].as_array().unwrap();
    let mut scope = RoomScope::default();
    let mut meshes = pvd_core::MeshCache::default();
    for (frame, expected) in frames.iter().enumerate() {
        let state = recording.state_at(frame).unwrap();
        for (index, scene) in scenes.iter().enumerate() {
            let scene_id = scene["id"].as_u64().unwrap();
            let actor = scene["actor"].as_u64().unwrap();
            scope.update(
                &state,
                room(scene_id, state.objects[&scene_id].generation),
                frame as u64,
            );
            assert!(scope.actors().contains(&actor));
            assert_eq!(scope.actors().len(), 2);
            let rendered = state.render_actors_cached(scope.actors().iter().copied(), &mut meshes);
            assert_eq!(rendered.objects.len(), 2);
            assert_eq!(
                rendered
                    .objects
                    .iter()
                    .filter(|object| !object.dynamic)
                    .count(),
                1
            );
            let dynamic = rendered
                .objects
                .iter()
                .find(|object| object.id == actor)
                .unwrap();
            let actual = dynamic.position.into_iter().chain(dynamic.rotation);
            for (actual, expected) in actual.zip(expected["poses"][index].as_array().unwrap()) {
                assert!((actual as f64 - expected.as_f64().unwrap()).abs() < 0.00001);
            }
        }
    }
}
