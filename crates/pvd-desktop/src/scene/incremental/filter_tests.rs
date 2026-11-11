use super::{tests::*, *};
use pvd_protocol::{Event, Name};
use std::sync::Arc;

fn set_mask(app: &mut App, mask: u8) {
    let mut studio = app.world_mut().resource_mut::<Studio>();
    studio.prefs.show_static = mask & 1 != 0;
    studio.prefs.show_dynamic = mask & 2 != 0;
    studio.prefs.show_sleeping = mask & 4 != 0;
    studio.prefs.show_kinematic = mask & 8 != 0;
}

fn assert_full(app: &App, state: &SceneState, mask: u8) {
    let full = state.render_scene();
    let source_shapes = full.objects.len();
    let mut expected: Vec<_> = full
        .objects
        .into_iter()
        .filter(|object| {
            valid_pose(object)
                && valid_geometry(&object.geometry)
                && if object.dynamic {
                    mask & 2 != 0
                        && (mask & 8 != 0 || !is_kinematic(state, object.id))
                        && (mask & 4 != 0 || !object.sleeping)
                } else {
                    mask & 1 != 0 && (mask & 4 != 0 || !object.sleeping)
                }
        })
        .collect();
    expected.sort_by_key(|object| (object.id, object.shape_id));
    assert_eq!(objects(app), expected, "mask {mask}");
    let viewport = app.world().resource::<Viewport>();
    assert_eq!(viewport.rendered.len(), expected.len());
    assert_eq!(viewport.incremental.source.shapes, source_shapes);
    assert_eq!(
        viewport.shared_geometry.len(),
        viewport.incremental.source.shared.len()
    );
    assert_eq!(viewport.meshes.len(), viewport.incremental.mesh_users.len());
    assert_eq!(
        viewport.materials.len(),
        viewport.incremental.material_users.len()
    );
    for object in &expected {
        let rendered = &viewport.rendered[&(object.id, object.shape_id)];
        assert_eq!(
            app.world().get::<Transform>(rendered.entity).unwrap(),
            &rendered.transform
        );
    }
}

#[test]
fn every_filter_mask_tracks_hidden_lifetimes_and_motion_across_revisions() {
    for mask in 0..16 {
        let (mut builder, initial) = fixture();
        let mut app = app();
        set_mask(&mut app, mask);
        update(&mut app, initial, &[]);
        let scenarios = [
            (vec![pose(1, 23, 4.0)], vec![1], 1),
            (vec![set(1, 26, 33, vec![1])], vec![1], 1),
            (
                vec![set(2, 27, 34, 1u32.to_le_bytes().to_vec())],
                vec![2],
                1,
            ),
            (vec![set(50, 26, 33, vec![1])], vec![50], 1),
            (
                vec![set(2, 27, 34, 0u32.to_le_bytes().to_vec())],
                vec![2],
                1,
            ),
            (vec![set(1, 26, 33, vec![0])], vec![1], 1),
            (vec![set(50, 26, 33, vec![0])], vec![50], 1),
            (vec![pose(10, 24, 5.0)], vec![10], 3),
            (vec![Event::DestroyInstance { id: 1 }], vec![1], 1),
            (
                vec![create(1, 1), reference(1, 20, 10), pose(1, 23, 12.0)],
                vec![1],
                1,
            ),
            (vec![Event::DestroyInstance { id: 10 }], vec![10], 3),
            (vec![create(10, 3), reference(10, 21, 20)], vec![10], 3),
            (
                vec![
                    reference(1, 20, 10),
                    reference(2, 20, 10),
                    reference(4, 20, 10),
                ],
                vec![1, 2, 4],
                3,
            ),
            (vec![Event::DestroyInstance { id: 20 }], vec![20], 3),
            (
                vec![create(20, 4), set(20, 22, 32, 7f32.to_le_bytes().to_vec())],
                vec![20],
                3,
            ),
            (vec![reference(10, 21, 20)], vec![10], 3),
            (vec![pose(3, 23, 28.0)], vec![3], 1),
        ];
        let mut last = SceneState::default();
        for (index, (events, changed, affected)) in scenarios.into_iter().enumerate() {
            let state = frame(&mut builder, index + 1, events);
            update(&mut app, state.clone(), &changed);
            assert_full(&app, &state, mask);
            assert_eq!(
                app.world()
                    .resource::<Viewport>()
                    .incremental
                    .last_actor_count,
                affected,
                "mask {mask}, frame {}",
                index + 1
            );
            last = state;
        }
        set_mask(&mut app, 15);
        app.update();
        assert_full(&app, &last, 15);
        if mask != 15 {
            assert_eq!(
                app.world()
                    .resource::<Viewport>()
                    .incremental
                    .last_actor_count,
                4
            );
        }
    }
}

#[test]
fn sleeping_scope_tracks_actor_selection_and_wake_updates() {
    use crate::studio::ObjectScope;
    let (mut builder, initial) = fixture();
    let mut app = app();
    update(&mut app, initial, &[]);
    let sleeping = frame(
        &mut builder,
        1,
        vec![set(1, 26, 33, vec![1]), set(2, 26, 33, vec![1])],
    );
    update(&mut app, sleeping.clone(), &[1, 2]);
    for (enabled, scope, selected, expected) in [
        (true, ObjectScope::All, None, &[1, 2, 3, 4][..]),
        (false, ObjectScope::All, Some(1), &[3, 4][..]),
        (true, ObjectScope::Selected, Some(1), &[1, 3, 4][..]),
        (true, ObjectScope::Selected, Some(2), &[2, 3, 4][..]),
        (true, ObjectScope::Unselected, Some(2), &[1, 3, 4][..]),
        (true, ObjectScope::Selected, None, &[3, 4][..]),
        (true, ObjectScope::Unselected, None, &[1, 2, 3, 4][..]),
        (true, ObjectScope::Selected, Some(10), &[3, 4][..]),
    ] {
        {
            let mut studio = app.world_mut().resource_mut::<Studio>();
            studio.prefs.show_sleeping = enabled;
            studio.prefs.sleeping_scope = scope;
            studio.selected = selected;
        }
        app.update();
        assert_eq!(
            objects(&app)
                .iter()
                .map(|object| object.id)
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(
            app.world().resource::<Viewport>().rendered.len(),
            expected.len()
        );
    }
    let awake = frame(&mut builder, 2, vec![set(1, 26, 33, vec![0])]);
    update(&mut app, awake, &[1]);
    assert_eq!(
        objects(&app)
            .iter()
            .map(|object| object.id)
            .collect::<Vec<_>>(),
        [1, 3, 4]
    );
    update(&mut app, sleeping, &[1]);
    assert_eq!(
        objects(&app)
            .iter()
            .map(|object| object.id)
            .collect::<Vec<_>>(),
        [3, 4]
    );
}

fn trigger_fixture() -> (pvd_core::RecordingBuilder, SceneState) {
    let (mut builder, _) = fixture();
    let state = frame(
        &mut builder,
        1,
        vec![
            Event::StringHandle {
                handle: 28,
                value: "Flags".into(),
            },
            Event::StringHandle {
                handle: 35,
                value: "PvdU8".into(),
            },
            create(12, 3),
            reference(12, 21, 20),
            array(
                1,
                20,
                30,
                2,
                [10_u64, 12]
                    .into_iter()
                    .flat_map(u64::to_le_bytes)
                    .collect(),
            ),
            pose(12, 24, 5.0),
            pose(2, 23, 10.0),
            pose(4, 23, 20.0),
            pose(3, 23, 30.0),
            set(10, 28, 35, vec![4]),
            set(11, 28, 35, vec![2]),
            set(12, 28, 35, vec![1]),
        ],
    );
    assert_eq!(state.render_scene().objects.len(), 5);
    (builder, state)
}

fn assert_trigger_view(app: &mut App, expected: &[(u64, u64)]) {
    assert_eq!(
        objects(app)
            .iter()
            .map(|object| (object.id, object.shape_id))
            .collect::<Vec<_>>(),
        expected
    );
    let mut viewport = app.world_mut().resource_mut::<Viewport>();
    let mut rendered = viewport.rendered.keys().copied().collect::<Vec<_>>();
    rendered.sort_unstable();
    assert_eq!(rendered, expected);
    viewport.yaw = 0.0;
    viewport.pitch = 0.0;
    viewport.distance = 10.0;
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
    for (x, actor, shape) in [
        (0.0, 1, 10),
        (5.0, 1, 12),
        (10.0, 2, 10),
        (20.0, 4, 10),
        (30.0, 3, 11),
    ] {
        viewport.target = Vec3::new(x, 0.0, 0.0);
        assert_eq!(
            viewport.select_at(rect.center(), rect),
            expected.contains(&(actor, shape)).then_some(actor),
            "Actor {actor}, Shape {shape}"
        );
    }
}

#[test]
fn trigger_scope_tracks_shared_shapes_and_keeps_ordinary_geometry_pickable() {
    use crate::studio::ObjectScope;
    let (_, state) = trigger_fixture();
    let mut app = app();
    update(&mut app, state, &[]);
    let all = &[(1, 10), (1, 12), (2, 10), (3, 11), (4, 10)][..];
    let ordinary = &[(1, 12), (3, 11)][..];
    for wireframe in [false, true] {
        for (enabled, scope, selected, expected) in [
            (true, ObjectScope::All, None, all),
            (false, ObjectScope::All, None, ordinary),
            (
                true,
                ObjectScope::Selected,
                Some(1),
                &[(1, 10), (1, 12), (3, 11)][..],
            ),
            (
                true,
                ObjectScope::Unselected,
                Some(1),
                &[(1, 12), (2, 10), (3, 11), (4, 10)][..],
            ),
            (true, ObjectScope::Selected, Some(10), all),
            (true, ObjectScope::Unselected, Some(10), ordinary),
            (true, ObjectScope::Selected, Some(12), ordinary),
            (true, ObjectScope::Selected, None, ordinary),
            (true, ObjectScope::Unselected, None, all),
        ] {
            {
                let mut studio = app.world_mut().resource_mut::<Studio>();
                studio.prefs.show_triggers = enabled;
                studio.prefs.trigger_scope = scope;
                studio.prefs.wireframe = wireframe;
                studio.selected = selected;
            }
            app.update();
            assert_trigger_view(&mut app, expected);
        }
    }
}

#[test]
fn trigger_flags_and_shape_lifetimes_refresh_incremental_visibility() {
    let (mut builder, initial) = trigger_fixture();
    let mut app = app();
    app.world_mut().resource_mut::<Studio>().prefs.show_triggers = false;
    update(&mut app, initial.clone(), &[]);
    let ordinary = &[(1, 12), (3, 11)][..];
    let all = &[(1, 10), (1, 12), (2, 10), (3, 11), (4, 10)][..];
    assert_trigger_view(&mut app, ordinary);
    let ordinary_entity = app.world().resource::<Viewport>().rendered[&(1, 12)].entity;
    let queries = frame(&mut builder, 2, vec![set(10, 28, 35, vec![2])]);
    update(&mut app, queries, &[10]);
    assert_trigger_view(&mut app, all);
    assert_eq!(
        app.world()
            .resource::<Viewport>()
            .incremental
            .last_actor_count,
        3
    );
    let triggers = frame(&mut builder, 3, vec![set(10, 28, 35, vec![4])]);
    update(&mut app, triggers.clone(), &[10]);
    assert_trigger_view(&mut app, ordinary);
    assert_eq!(
        app.world().resource::<Viewport>().rendered[&(1, 12)].entity,
        ordinary_entity
    );
    app.world_mut().resource_mut::<Studio>().prefs.show_triggers = true;
    app.update();
    assert_trigger_view(&mut app, all);
    let destroyed = frame(&mut builder, 4, vec![Event::DestroyInstance { id: 10 }]);
    update(&mut app, destroyed.clone(), &[10]);
    assert_trigger_view(&mut app, ordinary);
    let reused = frame(
        &mut builder,
        5,
        vec![
            create(10, 3),
            reference(10, 21, 20),
            set(10, 28, 35, vec![4]),
        ],
    );
    update(&mut app, reused.clone(), &[10]);
    assert_trigger_view(&mut app, ordinary);
    let rebound = frame(
        &mut builder,
        6,
        vec![
            array(
                1,
                20,
                30,
                2,
                [10_u64, 12]
                    .into_iter()
                    .flat_map(u64::to_le_bytes)
                    .collect(),
            ),
            reference(2, 20, 10),
            reference(4, 20, 10),
        ],
    );
    update(&mut app, rebound, &[1, 2, 4]);
    assert_trigger_view(&mut app, all);
    for (state, expected) in [
        (reused, ordinary),
        (destroyed, ordinary),
        (triggers, all),
        (initial, all),
    ] {
        update(&mut app, state, &[1, 2, 4, 10]);
        assert_trigger_view(&mut app, expected);
    }
}

#[test]
fn trigger_filter_requires_a_valid_captured_byte_flag() {
    let (_, mut state) = trigger_fixture();
    let mut app = app();
    app.world_mut().resource_mut::<Studio>().prefs.show_triggers = false;
    let all = &[(1, 10), (1, 12), (2, 10), (3, 11), (4, 10)][..];
    state
        .objects
        .get_mut(&10)
        .unwrap()
        .properties
        .remove("Flags");
    update(&mut app, state.clone(), &[]);
    assert_trigger_view(&mut app, all);
    for flags in [
        Value::I64(-1),
        Value::U64(260),
        Value::Bool(true),
        Value::String("eTRIGGER_SHAPE".into()),
        Value::U64(2),
    ] {
        state
            .objects
            .get_mut(&10)
            .unwrap()
            .properties
            .insert("Flags".into(), flags);
        update(&mut app, state.clone(), &[10]);
        assert_trigger_view(&mut app, all);
    }
    for flags in [Value::I64(4), Value::U64(4)] {
        state
            .objects
            .get_mut(&10)
            .unwrap()
            .properties
            .insert("Flags".into(), flags);
        update(&mut app, state.clone(), &[10]);
        assert_trigger_view(&mut app, &[(1, 12), (3, 11)]);
    }
}

fn array(id: u64, property: u32, datatype: u32, items: u32, data: Vec<u8>) -> Event {
    Event::SetPropertyValue {
        id,
        property,
        datatype: Name {
            namespace: 41,
            name: datatype,
        },
        items,
        data: data.into(),
    }
}

fn points(id: u64, scale: f32) -> Event {
    array(
        id,
        63,
        66,
        3,
        [0.0_f32, 0.0, 0.0, scale, 0.0, 0.0, 0.0, scale, 0.0]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect(),
    )
}

fn mesh_fixture() -> (pvd_core::RecordingBuilder, SceneState) {
    let (mut builder, _) = fixture();
    let mut events: Vec<_> = [
        (7, "PxTriangleMeshGeometry"),
        (8, "PxTriangleMesh"),
        (60, "TriangleMesh"),
        (61, "Scale.Scale"),
        (62, "Scale.Rotation"),
        (63, "Points"),
        (64, "Triangles"),
        (66, "PxVec3"),
        (67, "PxQuat"),
    ]
    .into_iter()
    .map(|(handle, value)| Event::StringHandle {
        handle,
        value: value.into(),
    })
    .collect();
    events.extend([
        Event::CreateClass {
            name: Name {
                namespace: 41,
                name: 7,
            },
        },
        Event::CreateClass {
            name: Name {
                namespace: 41,
                name: 8,
            },
        },
    ]);
    events.extend([
        create(30, 8),
        create(22, 7),
        create(23, 4),
        points(30, 1.0),
        array(
            30,
            64,
            34,
            3,
            [0u32, 1, 2]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect(),
        ),
        reference(22, 60, 30),
        set(
            22,
            61,
            66,
            [1.0_f32; 3]
                .into_iter()
                .flat_map(f32::to_le_bytes)
                .collect(),
        ),
        set(
            22,
            62,
            67,
            [0.0_f32, 0.0, 0.0, 1.0]
                .into_iter()
                .flat_map(f32::to_le_bytes)
                .collect(),
        ),
        set(23, 22, 32, 2f32.to_le_bytes().to_vec()),
        reference(10, 21, 22),
        reference(11, 21, 22),
    ]);
    let state = frame(&mut builder, 1, events);
    assert_eq!(state.render_scene().objects.len(), 4);
    (builder, state)
}

#[test]
fn hidden_mesh_users_keep_cpu_arcs_while_visible_assets_release() {
    let (mut builder, state) = mesh_fixture();
    let mut app = app();
    set_mask(&mut app, 14);
    update(&mut app, state.clone(), &[]);
    assert_full(&app, &state, 14);
    let (key, vertices) = {
        let viewport = app.world().resource::<Viewport>();
        assert_eq!(viewport.incremental.source.shared.len(), 1);
        let (key, value) = viewport.shared_geometry.iter().next().unwrap();
        assert_eq!(viewport.incremental.source.shared[key].users, 4);
        (*key, Arc::downgrade(&value.vertices))
    };
    let state = frame(&mut builder, 2, vec![reference(10, 21, 23)]);
    update(&mut app, state.clone(), &[10]);
    assert_full(&app, &state, 14);
    {
        let viewport = app.world().resource::<Viewport>();
        assert_eq!(viewport.incremental.source.shared[&key].users, 1);
        assert!(vertices.upgrade().is_some());
        assert!(
            viewport
                .objects
                .iter()
                .all(|o| matches!(o.geometry, Geometry::Sphere { .. }))
        );
        assert_eq!(viewport.meshes.len(), 1);
        assert!(
            viewport
                .meshes
                .keys()
                .all(|hash| *hash != viewport.shared_geometry[&key].hash)
        );
    }
    let state = frame(&mut builder, 3, vec![points(30, 9.0)]);
    update(&mut app, state.clone(), &[30]);
    assert_full(&app, &state, 14);
    assert!(vertices.upgrade().is_none());
    assert_eq!(
        app.world()
            .resource::<Viewport>()
            .incremental
            .last_actor_count,
        1
    );
    set_mask(&mut app, 15);
    app.update();
    assert_full(&app, &state, 15);
    let state = frame(
        &mut builder,
        4,
        vec![
            Event::DestroyInstance { id: 3 },
            Event::DestroyInstance { id: 30 },
        ],
    );
    update(&mut app, state.clone(), &[3, 30]);
    assert_full(&app, &state, 15);
    assert!(
        app.world()
            .resource::<Viewport>()
            .shared_geometry
            .is_empty()
    );
}

#[test]
fn hidden_invalid_and_unsupported_geometry_rebuild_then_resume_incrementally() {
    let (mut builder, state) = fixture();
    let mut app = app();
    set_mask(&mut app, 14);
    update(&mut app, state, &[]);
    for (index, radius) in [-1.0_f32, 2.0].into_iter().enumerate() {
        let state = frame(
            &mut builder,
            index + 1,
            vec![set(21, 22, 32, radius.to_le_bytes().to_vec())],
        );
        update(&mut app, state.clone(), &[21]);
        assert_full(&app, &state, 14);
        assert_eq!(
            app.world().resource::<Viewport>().invalid_shapes,
            usize::from(index == 0)
        );
        assert_eq!(
            app.world()
                .resource::<Viewport>()
                .incremental
                .last_actor_count,
            4
        );
    }
    let state = frame(&mut builder, 3, vec![pose(1, 23, 8.0)]);
    update(&mut app, state.clone(), &[1]);
    assert_full(&app, &state, 14);
    assert_eq!(
        app.world()
            .resource::<Viewport>()
            .incremental
            .last_actor_count,
        1
    );
    let mut unsupported = state.clone();
    unsupported.objects.get_mut(&21).unwrap().type_name = "physx::UnsupportedGeometry".into();
    update(&mut app, unsupported.clone(), &[21]);
    assert_full(&app, &unsupported, 14);
    assert!(!app.world().resource::<Viewport>().diagnostics.is_empty());
    assert!(!app.world().resource::<Viewport>().incremental.ready);
    update(&mut app, state.clone(), &[21]);
    assert!(app.world().resource::<Viewport>().incremental.ready);
    update(&mut app, state.clone(), &[1]);
    assert_full(&app, &state, 14);
    assert_eq!(
        app.world()
            .resource::<Viewport>()
            .incremental
            .last_actor_count,
        1
    );
}

fn render_object(id: u64, geometry: Geometry) -> RenderObject {
    RenderObject {
        id,
        shape_id: id + 100,
        position: [0.0; 3],
        rotation: [0.0, 0.0, 0.0, 1.0],
        geometry,
        dynamic: false,
        sleeping: false,
    }
}

#[test]
fn combined_shapes_include_hidden_sources_and_replacement_removes_old_footprint() {
    let objects: Vec<_> = (0..100_000)
        .map(|id| render_object(id, Geometry::Sphere { radius: 1.0 }))
        .collect();
    let source = SourceResources::from_objects(&objects);
    assert!(source.replacement_fits(&HashSet::from([0]), &objects[..1]));
    assert!(!source.replacement_fits(&HashSet::new(), &objects[..1]));
    assert!(source.replacement_fits(&HashSet::from([0, 1]), &objects[..1]));
}

#[test]
#[ignore = "Allocates 144 MB of real mesh buffers to exercise the unchanged 128 MiB display budget"]
fn hidden_shared_meshes_obey_global_budget_without_double_counting_replacements() {
    let meshes: Vec<_> = (0..6)
        .map(|id| {
            render_object(
                id,
                Geometry::TriangleMesh {
                    vertices: Arc::new(vec![[0.0; 3]; 1_000_000]),
                    indices: Arc::new(vec![0; 3_000_000]),
                },
            )
        })
        .collect();
    let mut source = SourceResources::from_objects(&meshes[..5]);
    assert_eq!(source.bytes, 120_000_000);
    assert!(!source.replacement_fits(&HashSet::new(), &meshes[5..]));
    assert!(source.replacement_fits(&HashSet::from([0]), &meshes[5..]));
    let shared = render_object(8, meshes[0].geometry.clone());
    source.add(&shared);
    assert_eq!(source.bytes, 120_000_000);
    assert!(!source.replacement_fits(&HashSet::from([0]), &meshes[5..]));
    assert!(source.replacement_fits(&HashSet::from([0, 8]), &meshes[5..]));
    source.remove(0);
    assert_eq!(source.bytes, 120_000_000);
    source.remove(8);
    assert_eq!(source.bytes, 96_000_000);
    source.add(&meshes[5]);
    assert_eq!(source.bytes, 120_000_000);
}

#[test]
fn selection_settings_coordinates_and_missing_revisions_still_rebuild() {
    let (_, state) = fixture();
    let mut app = app();
    set_mask(&mut app, 14);
    update(&mut app, state.clone(), &[]);
    update(&mut app, state.clone(), &[1]);
    assert_eq!(
        app.world()
            .resource::<Viewport>()
            .incremental
            .last_actor_count,
        1
    );
    app.world_mut().resource_mut::<Studio>().selected = Some(1);
    app.update();
    assert_eq!(
        app.world()
            .resource::<Viewport>()
            .incremental
            .last_actor_count,
        4
    );
    update(&mut app, state.clone(), &[1]);
    assert_eq!(
        app.world()
            .resource::<Viewport>()
            .incremental
            .last_actor_count,
        1
    );
    set_mask(&mut app, 13);
    app.update();
    assert_eq!(
        app.world()
            .resource::<Viewport>()
            .incremental
            .last_actor_count,
        4
    );
    update(&mut app, state.clone(), &[1]);
    assert_eq!(
        app.world()
            .resource::<Viewport>()
            .incremental
            .last_actor_count,
        1
    );
    app.world_mut().resource_mut::<Studio>().prefs.up_axis = crate::coordinates::UpAxis::Z;
    app.update();
    assert_eq!(
        app.world()
            .resource::<Viewport>()
            .incremental
            .last_actor_count,
        4
    );
    update(&mut app, state.clone(), &[1]);
    assert_eq!(
        app.world()
            .resource::<Viewport>()
            .incremental
            .last_actor_count,
        1
    );
    app.world_mut().resource_mut::<Studio>().revision += 2;
    app.update();
    assert_eq!(
        app.world()
            .resource::<Viewport>()
            .incremental
            .last_actor_count,
        4
    );
    assert_full(&app, &state, 13);
}

#[test]
#[ignore = "Requires the preserved 48k official SDK capture; CPU-only full/incremental parity and timings"]
fn real_capture_filtered_forward_reverse_and_random_seek_match_full_sync() {
    use std::{io::Read, time::Instant};
    let path =
        std::path::PathBuf::from(std::env::var_os("PVD_SCENE_CAPTURE").expect("PVD_SCENE_CAPTURE"));
    let report =
        std::path::PathBuf::from(std::env::var_os("PVD_FILTER_REPORT").expect("PVD_FILTER_REPORT"));
    std::fs::create_dir_all(&report).unwrap();
    let mut live = pvd_core::LiveRecording::with_history(pvd_core::live::HistoryConfig {
        directory: report.join("history"),
        disk_budget_bytes: 512 << 20,
        ..default()
    })
    .unwrap();
    let mut input = std::fs::File::open(&path).unwrap();
    let mut chunk = vec![0; 64 << 10];
    loop {
        let count = input.read(&mut chunk).unwrap();
        if count == 0 {
            break;
        }
        live.feed(&chunk[..count]).unwrap();
    }
    let recording = live.finish().unwrap();
    assert!(recording.frame_count() > 350);
    let last = recording.frame_count() - 1;
    let mut rows = Vec::new();
    let mut max_objects = 0;
    for mask in [15, 14, 13, 11, 7] {
        let mut incremental_app = app();
        let mut full_app = app();
        set_mask(&mut incremental_app, mask);
        set_mask(&mut full_app, mask);
        let mut cursor = pvd_core::live::ReplayCursor::default();
        let steps: Vec<_> = (300..332)
            .map(|frame| ("forward", frame))
            .chain((300..331).rev().map(|frame| ("reverse", frame)))
            .chain([
                ("random", 0),
                ("random", last),
                ("random", last / 2),
                ("random", 310),
            ])
            .collect();
        let mut incremental_steps = 0;
        for (direction, frame) in steps {
            let patch = cursor.seek(&recording, frame, || false).unwrap().update;
            let replaced = patch.replace_all;
            {
                let mut studio = incremental_app.world_mut().resource_mut::<Studio>();
                let revision = studio.revision;
                studio.scene_changes.record(&patch, revision);
                patch.apply(&mut studio.state);
                studio.revision += 1;
                max_objects = max_objects.max(studio.state.objects.len());
            }
            {
                let mut studio = full_app.world_mut().resource_mut::<Studio>();
                patch.apply_owned(&mut studio.state);
                studio.scene_changes.full = true;
                studio.revision += 1;
            }
            let start = Instant::now();
            incremental_app.update();
            let incremental_ms = start.elapsed().as_secs_f64() * 1000.0;
            let start = Instant::now();
            full_app.update();
            let full_ms = start.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(
                objects(&incremental_app),
                objects(&full_app),
                "mask {mask} {direction} frame {frame}"
            );
            let incremental = incremental_app.world().resource::<Viewport>();
            let full = full_app.world().resource::<Viewport>();
            assert_eq!(incremental.invalid_shapes, full.invalid_shapes);
            assert_eq!(
                incremental
                    .diagnostics
                    .iter()
                    .map(|m| m.text(crate::i18n::Language::English))
                    .collect::<Vec<_>>(),
                full.diagnostics
                    .iter()
                    .map(|m| m.text(crate::i18n::Language::English))
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                incremental.incremental.source.shapes,
                full.incremental.source.shapes
            );
            assert_eq!(
                incremental.incremental.source.bytes,
                full.incremental.source.bytes
            );
            assert_eq!(incremental.rendered.len(), full.rendered.len());
            assert_eq!(incremental.meshes.len(), full.meshes.len());
            assert_eq!(incremental.materials.len(), full.materials.len());
            for (key, rendered) in &incremental.rendered {
                assert_eq!(rendered.transform, full.rendered[key].transform);
                assert_eq!(rendered.visible, full.rendered[key].visible);
            }
            let subset =
                incremental.incremental.last_actor_count < full.incremental.last_actor_count;
            incremental_steps += usize::from(subset);
            rows.push(serde_json::json!({"mask": mask, "direction": direction, "frame": frame,
                "replace_all": replaced, "subset": subset,
                "affected_actors": incremental.incremental.last_actor_count,
                "full_actors": full.incremental.last_actor_count, "visible_shapes": incremental.objects.len(),
                "source_shapes": incremental.incremental.source.shapes,
                "incremental_ms": incremental_ms, "full_ms": full_ms}));
        }
        assert!(
            incremental_steps >= 60,
            "mask {mask}: {incremental_steps} subsets"
        );
    }
    assert!(max_objects >= 48_000);
    std::fs::write(
        report.join("comparison.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "capture": path, "max_protocol_objects": max_objects,
            "frames": recording.frame_count(), "gpu": false, "rows": rows
        }))
        .unwrap(),
    )
    .unwrap();
    drop(recording);
    assert_eq!(
        std::fs::read_dir(report.join("history")).unwrap().count(),
        0
    );
}

fn static_mesh_events(index: u64) -> Vec<Event> {
    let actor = 100 + index;
    let shape = 200 + index;
    let geometry = 300 + index;
    vec![
        create(actor, 2),
        create(shape, 3),
        create(geometry, 7),
        reference(actor, 20, shape),
        reference(shape, 21, geometry),
        reference(geometry, 60, 30),
        set(
            geometry,
            61,
            66,
            [index as f32 + 2.0; 3]
                .into_iter()
                .flat_map(f32::to_le_bytes)
                .collect(),
        ),
        set(
            geometry,
            62,
            67,
            [0.0_f32, 0.0, 0.0, 1.0]
                .into_iter()
                .flat_map(f32::to_le_bytes)
                .collect(),
        ),
    ]
}

#[test]
#[ignore = "Exercises the actual core 128 MiB limit with large captured mesh arrays and hidden actors"]
fn hidden_mesh_budget_crossing_matches_full_core_diagnostics_and_recovers() {
    let (mut builder, _) = mesh_fixture();
    let mut events = vec![
        array(30, 63, 66, 1_000_000, vec![0; 12_000_000]),
        array(30, 64, 34, 999_999, vec![0; 3_999_996]),
        reference(10, 21, 23),
    ];
    for index in 0..7 {
        events.extend(static_mesh_events(index));
    }
    let state = frame(&mut builder, 2, events);
    let mut app = app();
    set_mask(&mut app, 14);
    update(&mut app, state.clone(), &[]);
    assert_full(&app, &state, 14);
    assert_eq!(
        app.world().resource::<Viewport>().incremental.source.bytes,
        127_999_968
    );
    assert!(app.world().resource::<Viewport>().incremental.ready);
    let state = frame(&mut builder, 3, static_mesh_events(7));
    update(&mut app, state.clone(), &[107, 207, 307]);
    assert_full(&app, &state, 14);
    let viewport = app.world().resource::<Viewport>();
    assert!(!viewport.incremental.ready);
    assert_eq!(viewport.incremental.last_actor_count, 12);
    assert!(viewport.diagnostics.iter().any(|message| {
        message
            .text(crate::i18n::Language::English)
            .contains("128 MiB unique mesh")
    }));
    let state = frame(&mut builder, 4, vec![Event::DestroyInstance { id: 107 }]);
    update(&mut app, state.clone(), &[107]);
    assert_full(&app, &state, 14);
    assert!(app.world().resource::<Viewport>().incremental.ready);
    let state = frame(&mut builder, 5, vec![pose(1, 23, 29.0)]);
    update(&mut app, state.clone(), &[1]);
    assert_full(&app, &state, 14);
    assert_eq!(
        app.world()
            .resource::<Viewport>()
            .incremental
            .last_actor_count,
        1
    );
}
