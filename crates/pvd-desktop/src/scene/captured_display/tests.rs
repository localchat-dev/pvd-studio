use super::*;
use crate::scene::*;
use bevy::{
    ecs::system::RunSystemOnce,
    gizmos::{
        config::{DefaultGizmoConfigGroup, GizmoConfigStore},
        gizmos::GizmoStorage,
    },
};
use pvd_core::{Recording, RecordingBuilder, live::SceneUpdate};
use pvd_protocol::{Endian, Event, EventGroup, Header, Name};
use std::{collections::BTreeMap, sync::Arc};

fn app() -> App {
    let mut app = App::new();
    let mut studio = Studio::default();
    studio.prefs.show_grid = false;
    studio.prefs.show_axes = false;
    app.insert_resource(studio)
        .insert_resource(Viewport {
            yaw: 0.0,
            pitch: 0.0,
            distance: 10.0,
            ..default()
        })
        .insert_resource(Assets::<Mesh>::default())
        .insert_resource(Assets::<StandardMaterial>::default())
        .add_systems(Update, sync_scene);
    app
}

fn update(app: &mut App, state: SceneState, full: bool) {
    let mut patch = SceneUpdate::full(&state);
    patch.replace_all = full;
    {
        let mut studio = app.world_mut().resource_mut::<Studio>();
        let revision = studio.revision;
        studio.scene_changes.record(&patch, revision);
        studio.state = state;
        studio.revision += 1;
    }
    app.update();
}

fn name(name: u32) -> Name {
    Name { namespace: 1, name }
}

fn create(id: u64, class: u32) -> Event {
    Event::CreateInstance {
        class: name(class),
        id,
    }
}

fn set(id: u64, property: u32, datatype: u32, data: Vec<u8>) -> Event {
    Event::SetPropertyValue {
        id,
        property,
        datatype: name(datatype),
        items: 1,
        data: data.into(),
    }
}

fn reference(id: u64, property: u32, target: u64) -> Event {
    set(id, property, 10, target.to_le_bytes().to_vec())
}

fn recording() -> Recording {
    let mut builder = RecordingBuilder::new(Header {
        version: 1,
        timestamp_numerator: 1,
        timestamp_denominator: 1,
        flags: 1,
        endian: Endian::Little,
    });
    let mut initial: Vec<_> = [
        (1, "physx3"),
        (2, "PxRigidDynamic"),
        (3, "PxShape"),
        (4, "PxSphereGeometry"),
        (5, "PxScene"),
        (6, "Shapes"),
        (7, "Geometry"),
        (8, "Radius"),
        (9, "GlobalPose"),
        (10, "ObjectRef"),
        (11, "PvdF32"),
        (12, "PxTransform"),
        (13, "frame"),
    ]
    .into_iter()
    .map(|(handle, value)| Event::StringHandle {
        handle,
        value: value.into(),
    })
    .collect();
    initial.extend((2..=5).map(|class| Event::CreateClass { name: name(class) }));
    initial.extend([
        create(1, 2),
        create(2, 2),
        create(10, 3),
        create(20, 4),
        create(100, 5),
        reference(1, 6, 10),
        reference(2, 6, 10),
        reference(10, 7, 20),
        set(20, 8, 11, 1f32.to_le_bytes().to_vec()),
        set(
            1,
            9,
            12,
            [0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 2.0]
                .into_iter()
                .flat_map(f32::to_le_bytes)
                .collect(),
        ),
        set(
            2,
            9,
            12,
            [0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, -2.0]
                .into_iter()
                .flat_map(f32::to_le_bytes)
                .collect(),
        ),
    ]);
    for (index, mut events) in [
        initial,
        vec![
            Event::SetColor {
                id: 10,
                rgba: [255, 0, 0, 128],
            },
            Event::SetColor {
                id: 1,
                rgba: [0, 0, 255, 255],
            },
            Event::SetPickable {
                id: 100,
                value: false,
            },
        ],
        vec![Event::SetPickable {
            id: 1,
            value: false,
        }],
        vec![Event::SetPickable {
            id: 10,
            value: false,
        }],
        vec![
            Event::SetPickable { id: 1, value: true },
            Event::SetPickable {
                id: 10,
                value: true,
            },
            Event::SetColor {
                id: 10,
                rgba: [0, 255, 0, 255],
            },
        ],
        vec![Event::DestroyInstance { id: 10 }],
        vec![create(10, 3), reference(10, 7, 20)],
        vec![reference(1, 6, 10), reference(2, 6, 10)],
    ]
    .into_iter()
    .enumerate()
    {
        events.push(Event::EndSection {
            id: 100,
            name: 13,
            timestamp: index as u64,
        });
        builder
            .push(EventGroup {
                stream_id: 1,
                timestamp: 0,
                offset: 0,
                events,
            })
            .unwrap();
    }
    builder.snapshot().unwrap()
}

fn material(app: &App, id: u64) -> &StandardMaterial {
    let viewport = app.world().resource::<Viewport>();
    let handle = &viewport.rendered[&(id, 10)].material;
    app.world()
        .resource::<Assets<StandardMaterial>>()
        .get(handle)
        .unwrap()
}

fn rect() -> egui::Rect {
    egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))
}

#[test]
fn shared_shapes_pickability_and_colors_follow_forward_reverse_and_reused_ids() {
    let recording = recording();
    let mut app = app();
    for frame in [0, 1, 2, 3, 4, 5, 6, 7, 4, 3, 2, 1, 0] {
        update(&mut app, recording.state_at(frame).unwrap(), false);
        let viewport = app.world().resource::<Viewport>();
        let pick = match frame {
            2 => Some(2),
            3 | 5 | 6 => None,
            _ => Some(1),
        };
        assert_eq!(
            viewport.select_at(rect().center(), rect()),
            pick,
            "frame {frame}"
        );
        if frame == 5 || frame == 6 {
            assert!(viewport.rendered.is_empty());
            assert!(viewport.materials.is_empty());
        } else {
            let expected = match frame {
                1..=3 => [1.0, 0.0, 0.0, 128.0 / 255.0],
                4 => [0.0, 1.0, 0.0, 1.0],
                _ => [0.56, 0.67, 0.79, 1.0],
            };
            for id in [1, 2] {
                assert_eq!(
                    material(&app, id).base_color.to_srgba().to_f32_array(),
                    expected,
                    "frame {frame}, Actor {id}"
                );
            }
            assert_eq!(viewport.materials.len(), 1);
            assert_eq!(app.world().resource::<Assets<StandardMaterial>>().len(), 1);
        }
    }
    update(&mut app, SceneState::default(), true);
    assert!(app.world().resource::<Viewport>().rendered.is_empty());
    assert!(
        app.world()
            .resource::<Assets<StandardMaterial>>()
            .is_empty()
    );
}

#[test]
fn sleeping_kinematic_tint_preserves_display_overrides() {
    let mut state = recording().state_at(0).unwrap();
    state
        .objects
        .get_mut(&1)
        .unwrap()
        .properties
        .insert("RigidBodyFlags".into(), Value::U64(1));
    let mut app = app();
    for (sleeping, expected) in [
        (true, [0.36, 0.43, 0.52, 1.0]),
        (false, [0.68, 0.47, 0.81, 1.0]),
        (true, [0.36, 0.43, 0.52, 1.0]),
    ] {
        state
            .objects
            .get_mut(&1)
            .unwrap()
            .properties
            .insert("IsSleeping".into(), Value::Bool(sleeping));
        update(&mut app, state.clone(), false);
        assert_eq!(
            material(&app, 1).base_color.to_srgba().to_f32_array(),
            expected
        );
        assert_eq!(
            material(&app, 2).base_color.to_srgba().to_f32_array(),
            [0.56, 0.67, 0.79, 1.0]
        );
    }
    state
        .objects
        .get_mut(&10)
        .unwrap()
        .properties
        .insert("$color".into(), Value::Bytes(Arc::from([255, 0, 0, 128])));
    update(&mut app, state, false);
    assert_eq!(
        material(&app, 1).base_color.to_srgba().to_f32_array(),
        [1.0, 0.0, 0.0, 128.0 / 255.0]
    );
    app.world_mut().resource_mut::<Studio>().selected = Some(1);
    app.update();
    assert_eq!(
        material(&app, 1).base_color.to_srgba().to_f32_array(),
        [0.24, 0.82, 0.92, 1.0]
    );
}

#[test]
fn selection_highlight_and_opacity_scopes_override_captured_alpha() {
    use crate::studio::ObjectScope;
    let recording = recording();
    let mut app = app();
    update(&mut app, recording.state_at(1).unwrap(), false);
    for scope in [
        ObjectScope::All,
        ObjectScope::Selected,
        ObjectScope::Unselected,
    ] {
        {
            let mut studio = app.world_mut().resource_mut::<Studio>();
            studio.selected = Some(1);
            studio.prefs.opacity = 0.4;
            studio.prefs.opacity_scope = scope;
        }
        app.update();
        let selected_alpha = if scope == ObjectScope::Unselected {
            1.0
        } else {
            0.4
        };
        let other_alpha = 128.0 / 255.0
            * if scope == ObjectScope::Selected {
                1.0
            } else {
                0.4
            };
        assert_eq!(
            material(&app, 1).base_color.to_srgba().to_f32_array(),
            [0.24, 0.82, 0.92, selected_alpha]
        );
        assert_eq!(
            material(&app, 2).base_color.to_srgba().to_f32_array(),
            [1.0, 0.0, 0.0, other_alpha]
        );
        assert_eq!(material(&app, 2).alpha_mode, AlphaMode::Blend);
    }
    {
        let mut studio = app.world_mut().resource_mut::<Studio>();
        studio.selected = Some(10);
        studio.prefs.opacity = f32::NAN;
    }
    app.update();
    for id in [1, 2] {
        assert_eq!(
            material(&app, id).base_color.to_srgba().to_f32_array(),
            [0.24, 0.82, 0.92, 1.0]
        );
        assert_eq!(material(&app, id).alpha_mode, AlphaMode::Opaque);
    }
}

#[test]
fn multiple_selection_refreshes_materials_and_visibility_without_primary_changes() {
    use crate::studio::ObjectScope;
    let mut state = recording().state_at(0).unwrap();
    for id in [1, 2] {
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .properties
            .insert("IsSleeping".into(), Value::Bool(true));
    }
    let mut app = app();
    update(&mut app, state, false);
    {
        let mut studio = app.world_mut().resource_mut::<Studio>();
        studio.select_object(Some(1), false);
        studio.select_object(Some(2), true);
        studio.prefs.opacity = 0.4;
        studio.prefs.opacity_scope = ObjectScope::Selected;
    }
    app.update();
    for id in [1, 2] {
        assert_eq!(
            material(&app, id).base_color.to_srgba().to_f32_array(),
            [0.24, 0.82, 0.92, 0.4]
        );
    }
    app.world_mut()
        .resource_mut::<Studio>()
        .select_object(Some(1), true);
    assert_eq!(app.world().resource::<Studio>().selected, Some(2));
    app.update();
    assert_eq!(
        material(&app, 1).base_color.to_srgba().to_f32_array(),
        [0.36, 0.43, 0.52, 1.0]
    );
    assert_eq!(
        material(&app, 2).base_color.to_srgba().to_f32_array(),
        [0.24, 0.82, 0.92, 0.4]
    );
    {
        let mut studio = app.world_mut().resource_mut::<Studio>();
        studio.prefs.sleeping_scope = ObjectScope::Selected;
    }
    app.update();
    assert_eq!(
        app.world()
            .resource::<Viewport>()
            .objects
            .iter()
            .map(|object| object.id)
            .collect::<Vec<_>>(),
        [2]
    );
    app.world_mut()
        .resource_mut::<Studio>()
        .select_object(Some(1), true);
    app.update();
    assert_eq!(
        app.world()
            .resource::<Viewport>()
            .objects
            .iter()
            .map(|object| object.id)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    app.world_mut()
        .resource_mut::<Studio>()
        .select_object(Some(2), true);
    assert_eq!(app.world().resource::<Studio>().selected, Some(1));
    app.update();
    assert_eq!(
        app.world()
            .resource::<Viewport>()
            .objects
            .iter()
            .map(|object| object.id)
            .collect::<Vec<_>>(),
        [1]
    );
}

#[test]
fn multiple_selection_focus_unions_geometry_points_and_debug_bounds() {
    let mut state = recording().state_at(0).unwrap();
    state.objects.extend(particle_state().objects);
    state.debug_draw.insert(
        90,
        Arc::new(vec![pvd_core::debug_draw::Primitive::Point {
            instance: 91,
            position: [50.0, 25.0, 10.0],
            color: 0xffffffff,
        }]),
    );
    let mut app = app();
    update(&mut app, state, false);
    let viewport = app.world().resource::<Viewport>();
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for id in [1, 2, 17, 91] {
        let (a, b) = viewport.focus_bounds(Some(id)).unwrap();
        min = min.min(a);
        max = max.max(b);
    }
    assert_eq!(
        viewport.selection_bounds(&[1, 2, 17, 91].into()),
        Some((min, max))
    );
    assert_eq!(
        viewport.selection_bounds(&[10, 17, 91].into()),
        Some((min, max))
    );
}

fn particle_state() -> SceneState {
    let mut state = SceneState::default();
    state.objects.insert(
        17,
        Object {
            id: 17,
            generation: 0,
            type_name: "physx3::PxParticleSystem".into(),
            properties: BTreeMap::from([
                ("NbParticles".into(), Value::U64(1)),
                ("ValidParticleRange".into(), Value::U64(1)),
                ("MaxParticles".into(), Value::U64(1)),
                ("ParticleBaseFlags".into(), Value::U64(4)),
                ("ParticleReadDataFlags".into(), Value::U64(1)),
                ("RestOffset".into(), Value::F32(0.0)),
                ("Scene".into(), Value::Ref(0)),
                (
                    "ValidParticleBitmap".into(),
                    Value::Array(Arc::new(vec![Value::U64(1)])),
                ),
                (
                    "Positions".into(),
                    Value::Array(Arc::new(vec![Value::Vec3([0.0; 3])])),
                ),
                ("$color".into(), Value::Bytes(Arc::from([0, 255, 0, 128]))),
                ("$pickable".into(), Value::Bool(false)),
            ]),
        },
    );
    state
}

fn drawn_colors(app: &mut App) -> Vec<LinearRgba> {
    let mut config = GizmoConfigStore::default();
    config.insert(default(), DefaultGizmoConfigGroup);
    app.insert_resource(config)
        .insert_resource(crate::scene::selected_overlay_tests::Output::default())
        .insert_resource(GizmoStorage::<DefaultGizmoConfigGroup, ()>::default());
    app.world_mut().run_system_once(draw_grid).unwrap();
    app.world()
        .resource::<crate::scene::selected_overlay_tests::Output>()
        .colors
        .clone()
}

#[test]
fn palette_edits_refresh_paused_materials_and_preserve_display_overrides() {
    let mut state = recording().state_at(0).unwrap();
    state.objects.get_mut(&2).unwrap().type_name = "physx3::PxRigidStatic".into();
    let mut app = app();
    {
        let mut studio = app.world_mut().resource_mut::<Studio>();
        studio.prefs.display_colors.static_body = [0.1, 0.2, 0.3];
        studio.prefs.display_colors.sleeping_body = [0.3, 0.4, 0.5];
        studio.prefs.display_colors.kinematic_body = [0.4, 0.5, 0.6];
    }
    update(&mut app, state.clone(), false);
    let revision = app.world().resource::<Studio>().revision;
    for color in [[0.2, 0.3, 0.4], [0.8, 0.6, 0.4], [0.1, 0.7, 0.3]] {
        app.world_mut()
            .resource_mut::<Studio>()
            .prefs
            .display_colors
            .dynamic_body = color;
        app.update();
        assert_eq!(app.world().resource::<Studio>().revision, revision);
        assert_eq!(
            material(&app, 1).base_color.to_srgba().to_f32_array(),
            [color[0], color[1], color[2], 1.0]
        );
        assert_eq!(
            material(&app, 2).base_color.to_srgba().to_f32_array(),
            [0.1, 0.2, 0.3, 1.0]
        );
        assert_eq!(app.world().resource::<Assets<StandardMaterial>>().len(), 2);
    }
    state
        .objects
        .get_mut(&1)
        .unwrap()
        .properties
        .insert("RigidBodyFlags".into(), Value::U64(1));
    for (sleeping, expected) in [(false, [0.4, 0.5, 0.6, 1.0]), (true, [0.3, 0.4, 0.5, 1.0])] {
        state
            .objects
            .get_mut(&1)
            .unwrap()
            .properties
            .insert("IsSleeping".into(), Value::Bool(sleeping));
        update(&mut app, state.clone(), false);
        assert_eq!(
            material(&app, 1).base_color.to_srgba().to_f32_array(),
            expected
        );
    }
    state
        .objects
        .get_mut(&10)
        .unwrap()
        .properties
        .insert("$color".into(), Value::Bytes(Arc::from([255, 0, 0, 128])));
    update(&mut app, state, false);
    assert_eq!(
        material(&app, 1).base_color.to_srgba().to_f32_array(),
        [1.0, 0.0, 0.0, 128.0 / 255.0]
    );
    app.world_mut().resource_mut::<Studio>().selected = Some(1);
    app.update();
    assert_eq!(
        material(&app, 1).base_color.to_srgba().to_f32_array(),
        [0.24, 0.82, 0.92, 1.0]
    );
}

#[test]
fn cloth_palette_preserves_captured_color_and_selection() {
    let mut app = app();
    app.world_mut()
        .resource_mut::<Studio>()
        .prefs
        .display_colors
        .cloth = [0.2, 0.4, 0.6];
    app.world_mut()
        .resource_mut::<Viewport>()
        .point_clouds
        .clouds
        .push(crate::point_clouds::Cloud {
            id: 17,
            kind: crate::point_clouds::Kind::Cloth,
            points: vec![Vec3::ZERO],
            inactive: false,
        });
    let colors = drawn_colors(&mut app);
    assert_eq!(colors.len(), 6);
    assert!(
        colors
            .iter()
            .all(|color| *color == Color::srgb(0.2, 0.4, 0.6).to_linear())
    );
    app.world_mut()
        .resource_mut::<Viewport>()
        .point_display
        .insert(
            17,
            Style {
                color: Some([255, 0, 0, 128]),
                pickable: true,
            },
        );
    assert!(
        drawn_colors(&mut app)
            .iter()
            .all(|color| *color == Color::srgba(1.0, 0.0, 0.0, 128.0 / 255.0).to_linear())
    );
    app.world_mut().resource_mut::<Studio>().selected = Some(17);
    assert!(
        drawn_colors(&mut app)
            .iter()
            .all(|color| *color == Color::srgb(0.25, 0.85, 0.9).to_linear())
    );
}

#[test]
fn wireframes_use_shape_color_and_highlight_every_shared_shape_instance() {
    let mut app = app();
    {
        let mut studio = app.world_mut().resource_mut::<Studio>();
        studio.prefs.wireframe = true;
        studio.prefs.opacity = 0.5;
    }
    update(&mut app, recording().state_at(1).unwrap(), false);
    let colors = drawn_colors(&mut app);
    assert!(!colors.is_empty());
    assert!(
        colors.iter().all(|color| {
            *color == Color::srgba(1.0, 0.0, 0.0, 128.0 / 255.0 * 0.5).to_linear()
        })
    );
    app.world_mut().resource_mut::<Studio>().selected = Some(10);
    app.update();
    assert!(
        drawn_colors(&mut app)
            .iter()
            .all(|color| { *color == Color::srgba(0.25, 0.85, 0.9, 0.5).to_linear() })
    );
}

#[test]
fn point_color_and_pickability_do_not_prevent_explicit_selection_or_focus() {
    let mut app = app();
    update(&mut app, particle_state(), false);
    let viewport = app.world().resource::<Viewport>();
    assert_eq!(viewport.point_clouds.point_count, 1);
    assert_eq!(viewport.select_at(rect().center(), rect()), None);
    let colors = drawn_colors(&mut app);
    assert_eq!(colors.len(), 6);
    assert!(
        colors
            .iter()
            .all(|color| *color == Color::srgba(0.0, 1.0, 0.0, 128.0 / 255.0).to_linear())
    );
    app.world_mut().resource_mut::<Studio>().selected = Some(17);
    app.update();
    app.world_mut().resource_mut::<Viewport>().focus(Some(17));
    assert_eq!(app.world().resource::<Studio>().selected, Some(17));
    assert!(
        app.world()
            .resource::<Viewport>()
            .target
            .abs_diff_eq(Vec3::ZERO, 1e-6)
    );
    assert!(
        drawn_colors(&mut app)
            .iter()
            .all(|color| *color == Color::srgb(0.25, 0.85, 0.9).to_linear())
    );
    let mut state = particle_state();
    let properties = &mut state.objects.get_mut(&17).unwrap().properties;
    properties.insert("$pickable".into(), Value::U64(0));
    properties.insert("$color".into(), Value::Bytes(Arc::from([1, 2, 3])));
    app.world_mut().resource_mut::<Studio>().selected = None;
    update(&mut app, state, false);
    assert_eq!(
        app.world()
            .resource::<Viewport>()
            .select_at(rect().center(), rect()),
        Some(17)
    );
    assert!(
        drawn_colors(&mut app)
            .iter()
            .all(|color| *color == Color::srgb(0.64, 0.86, 0.56).to_linear())
    );
    update(&mut app, SceneState::default(), true);
    assert!(app.world().resource::<Viewport>().point_display.is_empty());
}
