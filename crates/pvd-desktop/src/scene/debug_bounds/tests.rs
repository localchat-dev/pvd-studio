use super::super::{MAX_DEBUG_PRIMITIVES, MAX_OVERLAY_LINES, Studio, Viewport, sync_scene};
use super::*;
use crate::coordinates::{Handedness, UpAxis, UpMode};
use pvd_core::debug_draw::Pose;
use std::sync::Arc;

fn app() -> App {
    let mut app = App::new();
    app.insert_resource(Studio::default())
        .insert_resource(Viewport::default())
        .insert_resource(Assets::<Mesh>::default())
        .insert_resource(Assets::<StandardMaterial>::default())
        .add_systems(Update, sync_scene);
    app
}
fn set(app: &mut App, owner: u64, primitives: Vec<Primitive>) {
    let mut studio = app.world_mut().resource_mut::<Studio>();
    studio.state.debug_draw.insert(owner, Arc::new(primitives));
    studio.revision += 1;
}
fn point(instance: u64, position: [f32; 3]) -> Primitive {
    Primitive::Point {
        instance,
        position,
        color: 0xffffffff,
    }
}
fn pose(position: [f32; 3]) -> Pose {
    Pose {
        position,
        rotation: [0.0, 0.0, 0.0, 1.0],
    }
}
fn bounds(viewport: &Viewport, selected: Option<u64>, min: [f32; 3], max: [f32; 3]) {
    let (actual_min, actual_max) = viewport.focus_bounds(selected).unwrap();
    assert!(
        actual_min.abs_diff_eq(Vec3::from(min), 1e-5),
        "min {actual_min:?} / {min:?}"
    );
    assert!(
        actual_max.abs_diff_eq(Vec3::from(max), 1e-5),
        "max {actual_max:?} / {max:?}"
    );
}

#[test]
fn debug_only_geometry_frames_all_and_matches_owner_or_subject_selection() {
    let mut app = app();
    set(
        &mut app,
        7,
        vec![
            point(10, [-4.0, 2.0, 6.0]),
            Primitive::Line {
                instance: 20,
                positions: [[2.0, -5.0, 1.0], [6.0, 3.0, 9.0]],
                colors: [0xffffffff; 2],
            },
            Primitive::Triangle {
                instance: 20,
                positions: [[10.0, 0.0, 0.0], [12.0, 0.0, 0.0], [10.0, 4.0, 0.0]],
                colors: [0xffffffff; 3],
            },
            Primitive::Text {
                instance: 30,
                position: [-8.0, 6.0, -2.0],
                size: 1.0,
                color: 0xffffffff,
                text: "anchor".into(),
            },
        ],
    );
    set(&mut app, 8, vec![point(20, [20.0, 8.0, 11.0])]);
    app.update();
    let mut viewport = app.world_mut().resource_mut::<Viewport>();
    assert!(viewport.objects.is_empty());
    assert!(viewport.point_clouds.clouds.is_empty());
    bounds(&viewport, None, [-8.0, -5.0, -2.0], [20.0, 8.0, 11.0]);
    bounds(&viewport, Some(7), [-8.0, -5.0, -2.0], [12.0, 6.0, 9.0]);
    bounds(&viewport, Some(10), [-4.0, 2.0, 6.0], [-4.0, 2.0, 6.0]);
    bounds(&viewport, Some(20), [2.0, -5.0, 0.0], [20.0, 8.0, 11.0]);
    bounds(&viewport, Some(30), [-8.0, 6.0, -2.0], [-8.0, 6.0, -2.0]);
    assert!(viewport.focus_bounds(Some(0)).is_none());
    assert!(viewport.focus_bounds(Some(999)).is_none());
    viewport.focus(None);
    assert!(viewport.target.abs_diff_eq(Vec3::new(6.0, 1.5, 4.5), 1e-5));
    assert!((viewport.distance - Vec3::new(28.0, 13.0, 13.0).length() * 1.4).abs() < 1e-5);
    viewport.focus(Some(20));
    assert!(viewport.target.abs_diff_eq(Vec3::new(11.0, 1.5, 5.5), 1e-5));
    let distance = viewport.distance;
    viewport.focus(Some(10));
    assert_eq!(viewport.distance, distance);
    assert_eq!(viewport.target, Vec3::new(-4.0, 2.0, 6.0));
}

#[test]
fn coordinates_transform_captured_debug_bounds_once_and_do_not_change_the_capture() {
    let mut app = app();
    set(
        &mut app,
        7,
        vec![Primitive::Line {
            instance: 70,
            positions: [[-2.0, -4.0, -6.0], [8.0, 10.0, 12.0]],
            colors: [0xffffffff; 2],
        }],
    );
    app.update();
    let captured = app.world().resource::<Studio>().state.clone();
    {
        let mut studio = app.world_mut().resource_mut::<Studio>();
        studio.prefs.up_mode = UpMode::Preset;
        studio.prefs.up_axis = UpAxis::Z;
        studio.prefs.handedness = Handedness::Left;
        studio.prefs.meters_per_unit = 0.1;
    }
    app.update();
    {
        let mut viewport = app.world_mut().resource_mut::<Viewport>();
        bounds(&viewport, Some(70), [-0.8, -0.6, -1.0], [0.2, 1.2, 0.4]);
        let distance = viewport.distance;
        viewport.reset_camera();
        assert!(
            viewport
                .target
                .abs_diff_eq(Vec3::new(-0.3, 0.3, -0.3), 1e-5)
        );
        assert_eq!(viewport.distance, distance);
        viewport.focus(None);
        assert!(
            viewport
                .target
                .abs_diff_eq(Vec3::new(-0.3, 0.3, -0.3), 1e-5)
        );
    }
    {
        let mut studio = app.world_mut().resource_mut::<Studio>();
        studio.prefs.up_axis = UpAxis::Y;
        studio.prefs.handedness = Handedness::Right;
        studio.prefs.meters_per_unit = 1.0;
    }
    app.update();
    bounds(
        app.world().resource::<Viewport>(),
        Some(7),
        [-2.0, -4.0, -6.0],
        [8.0, 10.0, 12.0],
    );
    assert_eq!(app.world().resource::<Studio>().state, captured);
}

#[test]
fn invalid_debug_geometry_is_omitted_and_collinear_triangle_edges_are_framed() {
    let mut app = app();
    set(
        &mut app,
        7,
        vec![
            point(10, [1.0, 2.0, 3.0]),
            point(20, [f32::NAN, 1000.0, 1000.0]),
            Primitive::Line {
                instance: 20,
                positions: [[1000.0; 3], [f32::INFINITY; 3]],
                colors: [0xffffffff; 2],
            },
            Primitive::Text {
                instance: 20,
                position: [1000.0; 3],
                size: 0.0,
                color: 0xffffffff,
                text: "invalid".into(),
            },
            Primitive::JointFrames {
                instance: 20,
                parent: Pose {
                    position: [1000.0; 3],
                    rotation: [0.0; 4],
                },
                child: pose([1000.0; 3]),
            },
            Primitive::Triangle {
                instance: 30,
                positions: [[1000.0, 0.0, 0.0], [1001.0, 0.0, 0.0], [1002.0, 0.0, 0.0]],
                colors: [0xffffffff; 3],
            },
        ],
    );
    app.update();
    bounds(
        app.world().resource::<Viewport>(),
        None,
        [1.0, 0.0, 0.0],
        [1002.0, 2.0, 3.0],
    );
    assert!(
        app.world()
            .resource::<Viewport>()
            .focus_bounds(Some(20))
            .is_none()
    );
    bounds(
        app.world().resource::<Viewport>(),
        Some(30),
        [1000.0, 0.0, 0.0],
        [1002.0, 0.0, 0.0],
    );
}

#[test]
fn hidden_cleared_and_closed_debug_geometry_releases_its_bounds() {
    let mut app = app();
    set(&mut app, 7, vec![point(70, [10.0, 20.0, 30.0])]);
    app.update();
    app.world_mut()
        .resource_mut::<Studio>()
        .prefs
        .show_debug_draw = false;
    app.update();
    assert!(
        app.world()
            .resource::<Viewport>()
            .focus_bounds(None)
            .is_none()
    );
    assert_eq!(
        app.world()
            .resource::<Viewport>()
            .debug_bounds
            .selected
            .capacity(),
        0
    );
    app.world_mut()
        .resource_mut::<Studio>()
        .prefs
        .show_debug_draw = true;
    app.update();
    bounds(
        app.world().resource::<Viewport>(),
        Some(70),
        [10.0, 20.0, 30.0],
        [10.0, 20.0, 30.0],
    );
    {
        let mut studio = app.world_mut().resource_mut::<Studio>();
        studio.state.debug_draw.clear();
        studio.revision += 1;
    }
    app.update();
    assert!(
        app.world()
            .resource::<Viewport>()
            .focus_bounds(None)
            .is_none()
    );
    set(&mut app, 8, vec![point(80, [-4.0, 0.0, 0.0])]);
    app.update();
    assert!(
        app.world()
            .resource::<Viewport>()
            .focus_bounds(Some(70))
            .is_none()
    );
    app.world_mut().resource_mut::<Studio>().close();
    app.update();
    assert!(
        app.world()
            .resource::<Viewport>()
            .focus_bounds(None)
            .is_none()
    );
    assert_eq!(
        app.world()
            .resource::<Viewport>()
            .debug_bounds
            .selected
            .capacity(),
        0
    );
}

#[test]
fn primitive_budget_and_repeated_ids_do_not_expand_focus_storage() {
    let mut app = app();
    let mut points = vec![point(70, [1.0, 2.0, 3.0]); MAX_DEBUG_PRIMITIVES];
    points.push(point(99, [9000.0; 3]));
    set(&mut app, 7, points);
    set(&mut app, 8, vec![point(80, [8000.0; 3])]);
    app.update();
    let viewport = app.world().resource::<Viewport>();
    bounds(viewport, None, [1.0, 2.0, 3.0], [1.0, 2.0, 3.0]);
    assert_eq!(viewport.debug_bounds.selected.len(), 2);
    assert!(viewport.focus_bounds(Some(99)).is_none());
    assert!(viewport.focus_bounds(Some(8)).is_none());
}

#[test]
fn joint_frame_scale_and_only_admitted_limit_lines_contribute_to_focus() {
    let mut app = app();
    set(
        &mut app,
        7,
        vec![Primitive::JointFrames {
            instance: 70,
            parent: pose([0.0; 3]),
            child: pose([10.0, 0.0, 0.0]),
        }],
    );
    app.world_mut()
        .resource_mut::<Studio>()
        .prefs
        .joint_frame_scale = 2.0;
    app.update();
    bounds(
        app.world().resource::<Viewport>(),
        Some(70),
        [0.0; 3],
        [11.0, 1.0, 1.0],
    );
    app.world_mut()
        .resource_mut::<Studio>()
        .prefs
        .joint_frame_scale = 4.0;
    app.update();
    bounds(
        app.world().resource::<Viewport>(),
        Some(70),
        [0.0; 3],
        [12.0, 2.0, 2.0],
    );
    let limit = |instance, x| Primitive::AngularLimit {
        instance,
        pose: pose([x, 0.0, 0.0]),
        lower: 0.0,
        upper: 0.0,
        active: false,
    };
    set(&mut app, 7, vec![limit(70, 0.0); MAX_OVERLAY_LINES / 22]);
    set(&mut app, 8, vec![limit(80, 100.0)]);
    set(&mut app, 9, vec![limit(90, 1000.0)]);
    app.update();
    let viewport = app.world().resource::<Viewport>();
    assert_eq!(viewport.limit_lines.len(), MAX_OVERLAY_LINES);
    bounds(viewport, Some(70), [0.0; 3], [0.0, 1.0, 0.0]);
    bounds(viewport, Some(8), [100.0, 0.0, 0.0], [100.0, 1.0, 0.0]);
    bounds(viewport, Some(80), [100.0, 0.0, 0.0], [100.0, 1.0, 0.0]);
    assert!(viewport.focus_bounds(Some(9)).is_none());
    assert!(viewport.focus_bounds(Some(90)).is_none());
}
