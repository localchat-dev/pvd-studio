use super::*;
use pvd_core::{Recording, RecordingBuilder, live::SceneUpdate};
use pvd_protocol::{Endian, Event, EventGroup, Header, Name};

fn class(name: u32) -> Name {
    Name { namespace: 1, name }
}

fn create(id: u64, name: u32) -> Event {
    Event::CreateInstance {
        id,
        class: class(name),
    }
}

fn reference(id: u64, property: u32, target: u64) -> Event {
    Event::SetPropertyValue {
        id,
        property,
        datatype: class(15),
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
        (10, "Scene"),
        (11, "Actors"),
        (12, "Shapes"),
        (13, "Geometry"),
        (14, "Materials"),
        (15, "ObjectRef"),
        (16, "frame"),
    ]
    .into_iter()
    .map(|(handle, value)| Event::StringHandle {
        handle,
        value: value.into(),
    })
    .collect();
    events.extend((2..=6).map(|name| Event::CreateClass { name: class(name) }));
    events.extend([
        create(100, 2),
        create(200, 2),
        create(10, 3),
        create(20, 3),
        create(30, 4),
        create(40, 5),
        create(50, 6),
        create(60, 6),
        reference(10, 10, 100),
        reference(20, 10, 200),
        reference(100, 11, 10),
        reference(200, 11, 20),
        reference(10, 12, 30),
        reference(20, 12, 30),
        reference(30, 13, 40),
        reference(30, 14, 50),
        Event::SetIsTopLevel {
            id: 20,
            value: true,
        },
    ]);
    let frames = [
        events,
        vec![reference(10, 10, 200)],
        vec![
            Event::DestroyInstance { id: 100 },
            create(100, 2),
            create(11, 3),
            reference(11, 10, 100),
            reference(100, 11, 11),
        ],
        vec![Event::DestroyInstance { id: 30 }, create(30, 4)],
    ];
    for (index, mut events) in frames.into_iter().enumerate() {
        events.push(Event::EndSection {
            id: 100,
            name: 16,
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

fn scope(state: &SceneState, id: u64, generation: u32, revision: u64) -> RoomScope {
    let mut scope = RoomScope::default();
    scope.update(state, Some(FrameObject { id, generation }), revision);
    scope
}

fn visible(cache: &TreeCache) -> BTreeSet<u64> {
    cache.rows.iter().filter_map(|row| row.id).collect()
}

#[test]
fn room_tree_search_and_top_level_rows_include_only_members_and_shared_dependencies() {
    let state = recording().state_at(0).unwrap();
    let mut cache = TreeCache::default();
    for (room, expected, excluded) in [
        (100, BTreeSet::from([100, 10, 30, 40, 50]), [200, 20, 60]),
        (200, BTreeSet::from([200, 20, 30, 40, 50]), [100, 10, 60]),
    ] {
        let scope = scope(&state, room, 0, 1);
        cache.update_scoped(&state, 1, "", Language::Chinese, &scope);
        assert_eq!(visible(&cache), expected);
        for id in expected {
            assert_eq!(
                exact_object_id_scoped(&state, &format!("0x{id:016x}"), &scope),
                Some(id)
            );
        }
        for id in excluded {
            assert_eq!(
                exact_object_id_scoped(&state, &id.to_string(), &scope),
                None
            );
            cache.update_scoped(
                &state,
                1,
                &format!("0x{id:016x}"),
                Language::English,
                &scope,
            );
            assert!(cache.rows.is_empty());
        }
        cache.update_scoped(&state, 1, "PxMaterial", Language::English, &scope);
        assert_eq!(visible(&cache), BTreeSet::from([50]));
    }
    cache.update(&state, 1, "", Language::English);
    assert_eq!(visible(&cache), state.objects.keys().copied().collect());
}

#[test]
fn transferred_actor_invalidates_rows_and_stale_scene_membership_cannot_leak() {
    let recording = recording();
    let state = recording.state_at(0).unwrap();
    let next = recording.state_at(1).unwrap();
    let mut cache = TreeCache::default();
    cache.update_scoped(
        &state,
        10,
        "",
        Language::English,
        &scope(&state, 100, 0, 10),
    );
    let mut update = SceneUpdate::full(&next);
    update.replace_all = false;
    cache.retain_for_update(&update, &state, 10);
    assert_eq!(cache.revision, 10);
    cache.update_scoped(&next, 11, "", Language::English, &scope(&next, 100, 0, 11));
    assert_eq!(visible(&cache), BTreeSet::from([100]));
    cache.update_scoped(&next, 11, "", Language::English, &scope(&next, 200, 0, 11));
    assert_eq!(visible(&cache), BTreeSet::from([200, 10, 20, 30, 40, 50]));
}

#[test]
fn reused_scene_and_dependency_ids_keep_room_generation_and_reference_boundaries() {
    let recording = recording();
    let state = recording.state_at(2).unwrap();
    let mut cache = TreeCache::default();
    cache.update_scoped(
        &state,
        20,
        "",
        Language::English,
        &scope(&state, 100, 0, 20),
    );
    assert!(cache.rows.is_empty());
    cache.update_scoped(
        &state,
        20,
        "",
        Language::English,
        &scope(&state, 100, 1, 20),
    );
    assert_eq!(visible(&cache), BTreeSet::from([100, 11]));
    let next = recording.state_at(3).unwrap();
    cache.update_scoped(&next, 21, "", Language::English, &scope(&next, 200, 0, 21));
    assert_eq!(visible(&cache), BTreeSet::from([200, 10, 20]));
    assert!(
        !cache
            .rows
            .iter()
            .any(|row| row.id == Some(20) && row.children)
    );
}

#[test]
fn room_switch_keeps_collapse_local_to_objects_and_motion_reuses_rows() {
    let state = recording().state_at(0).unwrap();
    let mut cache = TreeCache::default();
    cache.collapsed.insert((100, 0));
    let first = scope(&state, 100, 0, 1);
    cache.update_scoped(&state, 1, "", Language::English, &first);
    assert_eq!(visible(&cache), BTreeSet::from([100]));
    cache.update_scoped(&state, 1, "", Language::English, &scope(&state, 200, 0, 1));
    assert_eq!(visible(&cache), BTreeSet::from([200, 20, 30, 40, 50]));
    cache.update_scoped(&state, 1, "", Language::English, &first);
    let address = cache.rows[0].label.as_ptr();
    let mut next = state.clone();
    next.objects
        .get_mut(&10)
        .unwrap()
        .properties
        .insert("GlobalPose".into(), Value::Vec3([1.0, 2.0, 3.0]));
    let mut update = SceneUpdate::full(&next);
    update.replace_all = false;
    cache.retain_for_update(&update, &state, 1);
    assert_eq!(cache.revision, 2);
    cache.update_scoped(&next, 2, "", Language::English, &scope(&next, 100, 0, 2));
    assert_eq!(cache.rows[0].label.as_ptr(), address);
    assert_eq!(visible(&cache), BTreeSet::from([100]));
}

#[test]
#[ignore = "requires tools/physx-multiscene capture and same-run SDK getter truth"]
fn actual_sdk_room_trees_follow_independent_scene_ids_and_poses() {
    let path = std::path::PathBuf::from(
        std::env::var_os("PVD_MULTISCENE_CAPTURE").expect("set PVD_MULTISCENE_CAPTURE"),
    );
    let truth: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path.with_extension("json")).unwrap()).unwrap();
    let scenes = truth["scenes"].as_array().unwrap();
    let frames = truth["frames"].as_array().unwrap();
    assert_eq!(scenes.len(), 2);
    assert_eq!(frames.len(), 144);
    let recording = Recording::from_path(path).unwrap();
    assert_eq!(recording.frame_count(), frames.len());
    let mut cache = TreeCache::default();
    for frame in (0..frames.len()).chain((0..frames.len()).rev()) {
        let state = recording.state_at(frame).unwrap();
        for (index, source) in scenes.iter().enumerate() {
            let room = source["id"].as_u64().unwrap();
            let actor = source["actor"].as_u64().unwrap();
            let scope = scope(&state, room, 0, frame as u64);
            cache.update_scoped(&state, frame as u64, "", Language::Chinese, &scope);
            let visible = visible(&cache);
            assert!(visible.contains(&room));
            assert!(visible.contains(&actor));
            for other in scenes.iter().filter(|other| *other != source) {
                assert!(!visible.contains(&other["id"].as_u64().unwrap()));
                assert!(!visible.contains(&other["actor"].as_u64().unwrap()));
            }
            let Value::Transform { position, rotation } =
                state.objects[&actor].properties["GlobalPose"]
            else {
                panic!("actor pose missing");
            };
            for (actual, expected) in position
                .into_iter()
                .chain(rotation)
                .zip(frames[frame]["poses"][index].as_array().unwrap())
            {
                assert!((actual as f64 - expected.as_f64().unwrap()).abs() < 0.00001);
            }
        }
    }
}
