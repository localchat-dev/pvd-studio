use super::*;
use crate::studio::ObjectScope;
use bevy::{
    ecs::system::RunSystemOnce,
    gizmos::{
        config::{DefaultGizmoConfigGroup, GizmoConfigStore},
        gizmos::GizmoStorage,
    },
};

#[derive(Resource, Default)]
pub(crate) struct Output {
    vertices: Vec<[u32; 3]>,
    pub(super) colors: Vec<LinearRgba>,
}
impl Output {
    pub fn capture(&mut self, gizmos: &Gizmos) {
        self.colors.clone_from(&gizmos.list_colors);
        self.vertices = gizmos
            .list_positions
            .iter()
            .chain(&gizmos.strip_positions)
            .map(|value| value.to_array().map(f32::to_bits))
            .collect();
        self.vertices.sort_unstable();
    }
}

fn draw(
    state: pvd_core::SceneState,
    objects: Vec<RenderObject>,
    selected: Option<u64>,
    flags: u8,
    scope: ObjectScope,
) -> (Vec<[u32; 3]>, Option<u64>) {
    let mut studio = Studio::default();
    studio.state = state;
    studio.selected = selected;
    studio.prefs.show_grid = false;
    studio.prefs.show_axes = false;
    studio.prefs.show_com = flags & 1 != 0;
    studio.prefs.show_velocity = flags & 2 != 0;
    studio.prefs.show_local_axes = flags & 4 != 0;
    studio.prefs.com_scope = scope;
    studio.prefs.velocity_scope = scope;
    studio.prefs.local_axes_scope = scope;
    let mut config = GizmoConfigStore::default();
    config.insert(default(), DefaultGizmoConfigGroup);
    let mut world = World::new();
    world.insert_resource(studio);
    world.insert_resource(Viewport {
        objects,
        ..default()
    });
    world.insert_resource(config);
    world.insert_resource(GizmoStorage::<DefaultGizmoConfigGroup, ()>::default());
    world.insert_resource(Output::default());
    world.run_system_once(draw_grid).unwrap();
    (
        world.remove_resource::<Output>().unwrap().vertices,
        world.resource::<Studio>().selected,
    )
}

fn draw_debug(state: pvd_core::SceneState, wireframe: bool, visible: bool) -> Output {
    let captured = state.clone();
    let mut studio = Studio::default();
    studio.state = state;
    studio.prefs.show_grid = false;
    studio.prefs.show_axes = false;
    studio.prefs.show_debug_draw = visible;
    studio.prefs.wireframe = wireframe;
    let mut config = GizmoConfigStore::default();
    config.insert(default(), DefaultGizmoConfigGroup);
    let mut world = World::new();
    world.insert_resource(studio);
    world.insert_resource(Viewport::default());
    world.insert_resource(config);
    world.insert_resource(GizmoStorage::<DefaultGizmoConfigGroup, ()>::default());
    world.insert_resource(Output::default());
    world.run_system_once(draw_grid).unwrap();
    assert_eq!(world.resource::<Studio>().state, captured);
    world.remove_resource::<Output>().unwrap()
}

#[test]
fn captured_triangles_draw_three_opaque_edges_with_each_start_vertex_rgb() {
    use pvd_core::debug_draw::Primitive;
    let mut state = pvd_core::SceneState::default();
    let positions = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    state.debug_draw.insert(
        7,
        std::sync::Arc::new(vec![
            Primitive::Triangle {
                instance: 9,
                positions,
                colors: [27, 0x80112233, 0x00ff0000],
            },
            Primitive::Triangle {
                instance: 10,
                positions: [[f32::NAN; 3]; 3],
                colors: [0xffffffff; 3],
            },
        ]),
    );
    let mut vertices: Vec<_> = positions
        .into_iter()
        .flat_map(|position| [position.map(f32::to_bits); 2])
        .collect();
    vertices.sort_unstable();
    let colors: Vec<_> = [
        Color::srgb_u8(0, 0, 27),
        Color::srgb_u8(17, 34, 51),
        Color::srgb_u8(255, 0, 0),
    ]
    .into_iter()
    .flat_map(|color| [color.to_linear(); 2])
    .collect();
    for wireframe in [false, true] {
        let output = draw_debug(state.clone(), wireframe, true);
        assert_eq!(output.vertices, vertices);
        assert_eq!(output.colors, colors);
        let hidden = draw_debug(state.clone(), wireframe, false);
        assert!(hidden.vertices.is_empty());
        assert!(hidden.colors.is_empty());
    }
}

#[test]
fn captured_points_and_lines_keep_rgb_independent_of_alpha() {
    use pvd_core::debug_draw::Primitive;
    let mut state = pvd_core::SceneState::default();
    state.debug_draw.insert(
        7,
        std::sync::Arc::new(vec![
            Primitive::Point {
                instance: 9,
                position: [1.0, 2.0, 3.0],
                color: 0x00ff0000,
            },
            Primitive::Line {
                instance: 9,
                positions: [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]],
                colors: [0x0000ff00, 0x800000ff],
            },
        ]),
    );
    let output = draw_debug(state, false, true);
    assert_eq!(output.vertices.len(), 8);
    assert_eq!(output.colors[..6], [LinearRgba::RED; 6]);
    assert_eq!(output.colors[6..], [LinearRgba::GREEN, LinearRgba::BLUE]);
}

fn fixture() -> (pvd_core::SceneState, Vec<RenderObject>) {
    let mut state = pvd_core::SceneState::default();
    let mut objects = Vec::new();
    for (id, x) in [(1, 10.0), (2, 40.0)] {
        let position = [x, 3.0, 2.0];
        let rotation = [0.0, 0.0, 0.0, 1.0];
        state.objects.insert(
            id,
            pvd_core::Object {
                id,
                generation: 0,
                type_name: "PxRigidDynamic".into(),
                properties: [
                    (
                        "GlobalPose".into(),
                        pvd_core::Value::Transform { position, rotation },
                    ),
                    (
                        "CMassLocalPose".into(),
                        pvd_core::Value::Transform {
                            position: [0.0, 2.0, 0.0],
                            rotation,
                        },
                    ),
                    (
                        "LinearVelocity".into(),
                        pvd_core::Value::Vec3([1.0, 0.0, 0.0]),
                    ),
                    (
                        "AngularVelocity".into(),
                        pvd_core::Value::Vec3([0.0, 0.5, 0.0]),
                    ),
                ]
                .into_iter()
                .collect(),
            },
        );
        objects.push(RenderObject {
            id,
            shape_id: id + 10,
            position,
            rotation,
            geometry: Geometry::Box {
                half_extents: [1.0; 3],
            },
            dynamic: true,
            sleeping: false,
        });
    }
    (state, objects)
}

#[test]
fn actual_overlay_geometry_preserves_actor_shape_selection_and_all_scopes() {
    let (state, objects) = fixture();
    for flags in 1..8 {
        for scope in [
            ObjectScope::All,
            ObjectScope::Selected,
            ObjectScope::Unselected,
        ] {
            let actor = draw(state.clone(), objects.clone(), Some(1), flags, scope);
            let shape = draw(state.clone(), objects.clone(), Some(11), flags, scope);
            assert!(!actor.0.is_empty());
            assert_eq!(actor.0, shape.0);
            assert_eq!(actor.1, Some(1));
            assert_eq!(shape.1, Some(11));
        }
        let selected = draw(
            state.clone(),
            objects.clone(),
            Some(1),
            flags,
            ObjectScope::Selected,
        )
        .0;
        let unselected = draw(
            state.clone(),
            objects.clone(),
            Some(1),
            flags,
            ObjectScope::Unselected,
        )
        .0;
        assert_ne!(selected, unselected);
        let mut union = selected;
        union.extend(unselected);
        union.sort_unstable();
        assert_eq!(
            union,
            draw(
                state.clone(),
                objects.clone(),
                None,
                flags,
                ObjectScope::All
            )
            .0
        );
        assert_eq!(
            union,
            draw(
                state.clone(),
                objects.clone(),
                None,
                flags,
                ObjectScope::Unselected
            )
            .0
        );
        assert!(
            draw(
                state.clone(),
                objects.clone(),
                None,
                flags,
                ObjectScope::Selected
            )
            .0
            .is_empty()
        );
    }
    for selected in [None, Some(1), Some(11), Some(999)] {
        let output = draw(
            state.clone(),
            objects.clone(),
            selected,
            0,
            ObjectScope::All,
        );
        assert!(output.0.is_empty());
        assert_eq!(output.1, selected);
    }
}

#[test]
#[ignore = "Requires PVD_LARGE_CAPTURE from the real PhysX 3.4 producer; captures actual Gizmos output without GPU"]
fn real_sdk_selected_actor_and_shape_emit_identical_overlay_geometry() {
    let path = std::env::var_os("PVD_LARGE_CAPTURE").expect("PVD_LARGE_CAPTURE");
    let recording = pvd_core::Recording::from_path(path).unwrap();
    let state = recording
        .state_at(600.min(recording.frame_count() - 1))
        .unwrap();
    let objects = state.render_scene().objects;
    let object = objects.iter().find(|object| object.dynamic).unwrap();
    let (actor, shape) = (object.id, object.shape_id);
    for flags in [1, 2, 4] {
        let a = draw(
            state.clone(),
            objects.clone(),
            Some(actor),
            flags,
            ObjectScope::Selected,
        );
        let b = draw(
            state.clone(),
            objects.clone(),
            Some(shape),
            flags,
            ObjectScope::Selected,
        );
        assert_eq!(a.0, b.0);
        assert_eq!(a.1, Some(actor));
        assert_eq!(b.1, Some(shape));
        if flags == 4 {
            assert!(!a.0.is_empty());
        }
    }
}
