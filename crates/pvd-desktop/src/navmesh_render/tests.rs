use super::*;
use crate::navmesh::{LoadedNavMesh, OverlayBinding};
use std::sync::Arc;

fn source() -> pvd_core::navmesh::NavMesh {
    pvd_core::navmesh::NavMesh {
        vertices: vec![[0.0, 0.0, 0.0], [0.0, 0.0, 2.0], [2.0, 0.0, 0.0]],
        indices: vec![0, 1, 2],
        edges: vec![[[0.0; 3], [0.0, 0.0, 2.0]]],
        tile_count: 1,
        polygon_count: 1,
        bounds_min: [0.0; 3],
        bounds_max: [2.0, 0.0, 2.0],
    }
}

fn app() -> App {
    let mut app = App::new();
    let loaded = LoadedNavMesh {
        mesh: Arc::new(source()),
        path: "fixture.bin".into(),
        binding: OverlayBinding::Global,
    };
    let mut overlay = NavMeshOverlay::default();
    overlay.publish(loaded).unwrap();
    app.insert_resource(overlay)
        .init_resource::<Studio>()
        .init_resource::<Viewport>()
        .init_resource::<Cache>()
        .init_resource::<Assets<Mesh>>()
        .init_resource::<Assets<StandardMaterial>>()
        .add_systems(Update, sync);
    app
}

#[test]
fn navmesh_mesh_assets_survive_playback_and_display_changes_and_release_on_unload() {
    let mut app = app();
    app.update();
    let mesh_id = app
        .world()
        .resource::<Cache>()
        .entries
        .get(&OverlayBinding::Global)
        .unwrap()
        .surface
        .as_ref()
        .unwrap()
        .mesh
        .id();
    for frame in 0..20 {
        app.world_mut().resource_mut::<Studio>().revision = frame;
        app.world_mut()
            .resource_mut::<NavMeshOverlay>()
            .selected_mut()
            .unwrap()
            .settings
            .opacity = frame as f32 / 20.0;
        app.update();
        assert_eq!(
            app.world()
                .resource::<Cache>()
                .entries
                .get(&OverlayBinding::Global)
                .unwrap()
                .surface
                .as_ref()
                .unwrap()
                .mesh
                .id(),
            mesh_id
        );
        assert_eq!(app.world().resource::<Assets<Mesh>>().len(), 2);
        assert_eq!(app.world().resource::<Assets<StandardMaterial>>().len(), 2);
    }
    app.world_mut()
        .resource_mut::<NavMeshOverlay>()
        .selected_mut()
        .unwrap()
        .settings
        .visible = false;
    app.update();
    let entity = app
        .world()
        .resource::<Cache>()
        .entries
        .get(&OverlayBinding::Global)
        .unwrap()
        .surface
        .as_ref()
        .unwrap()
        .entity;
    assert_eq!(
        *app.world().get::<Visibility>(entity).unwrap(),
        Visibility::Hidden
    );
    for index in 0..10 {
        app.world_mut()
            .resource_mut::<NavMeshOverlay>()
            .selected_mut()
            .unwrap()
            .settings
            .alignment
            .offset[0] = index as f32;
        app.update();
        assert_eq!(app.world().resource::<Assets<Mesh>>().len(), 2);
        assert_eq!(app.world().resource::<Assets<StandardMaterial>>().len(), 2);
    }
    app.world_mut().resource_mut::<NavMeshOverlay>().unload();
    app.update();
    assert!(app.world().resource::<Assets<Mesh>>().is_empty());
    assert!(
        app.world()
            .resource::<Assets<StandardMaterial>>()
            .is_empty()
    );
    assert!(app.world().resource::<Cache>().entries.is_empty());
}

#[test]
fn navmesh_alignment_and_view_coordinates_apply_once_with_correct_triangle_winding() {
    use crate::coordinates::{Handedness, UpAxis};
    use bevy::mesh::VertexAttributeValues;
    let source = source();
    let coordinates = Coordinates::new(UpAxis::Z, Handedness::Left, 0.1);
    let alignment = Alignment {
        offset: [10.0, 20.0, 30.0],
        scale: 2.0,
        flip_x: true,
        ..default()
    };
    let mesh = surface_mesh(&source, alignment, coordinates);
    let VertexAttributeValues::Float32x3(positions) =
        mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap()
    else {
        panic!()
    };
    // Source (2,0,0) -> flip/scale/offset (6,20,30) -> Z-up and left-hand (-.6,3,-2).
    assert!(Vec3::from(positions[2]).abs_diff_eq(Vec3::new(-0.6, 3.0, -2.0), 1e-5));
    assert_eq!(
        mesh.indices().unwrap().iter().collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    let mesh = surface_mesh(
        &source,
        Alignment {
            flip_x: false,
            ..default()
        },
        coordinates,
    );
    assert_eq!(
        mesh.indices().unwrap().iter().collect::<Vec<_>>(),
        vec![0, 2, 1]
    );
}

#[test]
fn navmesh_focus_uses_aligned_bounds_and_turns_off_captured_camera_follow() {
    let mut app = app();
    {
        let mut overlay = app.world_mut().resource_mut::<NavMeshOverlay>();
        overlay.selected_mut().unwrap().settings.alignment.offset = [10.0, 3.0, 20.0];
        overlay.focus_requested = Some(OverlayBinding::Global);
    }
    app.world_mut().resource_mut::<Studio>().follow_selected = true;
    app.update();
    let viewport = app.world().resource::<Viewport>();
    assert!(viewport.target.abs_diff_eq(Vec3::new(9.0, 3.0, 21.0), 1e-5));
    assert!((viewport.distance - 8.0_f32.sqrt() * 1.4).abs() < 1e-5);
    assert!(!app.world().resource::<Studio>().follow_selected);
    assert!(
        app.world()
            .resource::<NavMeshOverlay>()
            .focus_requested
            .is_none()
    );
}

#[test]
fn scene_switches_reuse_independent_assets_and_all_scenes_respect_generations() {
    use pvd_core::{Object, frame_sources::FrameObject};
    let mut app = app();
    let rooms = [
        FrameObject {
            id: 101,
            generation: 0,
        },
        FrameObject {
            id: 102,
            generation: 2,
        },
    ];
    let bindings = rooms.map(|room| OverlayBinding::Room { epoch: 0, room });
    for (index, room) in rooms.into_iter().enumerate() {
        app.world_mut()
            .resource_mut::<Studio>()
            .state
            .objects
            .insert(
                room.id,
                Object {
                    id: room.id,
                    generation: room.generation,
                    type_name: "physx3::PxScene".into(),
                    properties: default(),
                },
            );
        let mut overlay = app.world_mut().resource_mut::<NavMeshOverlay>();
        overlay
            .publish(LoadedNavMesh {
                mesh: Arc::new(source()),
                path: format!("scene-{index}.bin").into(),
                binding: bindings[index],
            })
            .unwrap();
        overlay.select_binding(bindings[index]);
        overlay.selected_mut().unwrap().settings.alignment.offset[0] = index as f32 * 10.0;
    }
    app.update();
    let ids: Vec<_> = bindings
        .iter()
        .map(|binding| {
            app.world().resource::<Cache>().entries[binding]
                .surface
                .as_ref()
                .unwrap()
                .mesh
                .id()
        })
        .collect();
    assert_eq!(app.world().resource::<Assets<Mesh>>().len(), 6);
    for index in 0..20 {
        app.world_mut().resource_mut::<Studio>().room_view.displayed = Some(rooms[index % 2]);
        app.update();
        let cache = app.world().resource::<Cache>();
        for (slot, binding) in bindings.iter().enumerate() {
            let draw = cache.entries[binding].surface.as_ref().unwrap();
            assert_eq!(draw.mesh.id(), ids[slot]);
            assert_eq!(
                *app.world().get::<Visibility>(draw.entity).unwrap(),
                if slot == index % 2 {
                    Visibility::Inherited
                } else {
                    Visibility::Hidden
                }
            );
        }
        assert_eq!(app.world().resource::<Assets<Mesh>>().len(), 6);
    }
    app.world_mut().resource_mut::<Studio>().room_view.displayed = None;
    app.world_mut()
        .resource_mut::<Studio>()
        .state
        .objects
        .get_mut(&rooms[1].id)
        .unwrap()
        .generation += 1;
    app.update();
    let second = app.world().resource::<Cache>().entries[&bindings[1]]
        .surface
        .as_ref()
        .unwrap()
        .entity;
    assert_eq!(
        *app.world().get::<Visibility>(second).unwrap(),
        Visibility::Hidden
    );
    app.world_mut()
        .resource_mut::<NavMeshOverlay>()
        .select_binding(bindings[0]);
    app.world_mut().resource_mut::<NavMeshOverlay>().unload();
    app.update();
    assert!(
        !app.world()
            .resource::<Cache>()
            .entries
            .contains_key(&bindings[0])
    );
    assert_eq!(app.world().resource::<Assets<Mesh>>().len(), 4);
    app.add_systems(Update, crate::navmesh::poll.before(sync));
    app.update();
    app.world_mut().resource_mut::<Studio>().close();
    app.update();
    assert_eq!(app.world().resource::<Cache>().entries.len(), 1);
    assert_eq!(app.world().resource::<Assets<Mesh>>().len(), 2);
    assert_eq!(app.world().resource::<Assets<StandardMaterial>>().len(), 2);
}

#[test]
fn replacing_after_unload_before_render_cannot_reuse_stale_geometry() {
    let mut app = app();
    app.update();
    let first = app.world().resource::<Cache>().entries[&OverlayBinding::Global]
        .surface
        .as_ref()
        .unwrap()
        .mesh
        .id();
    let mut overlay = app.world_mut().resource_mut::<NavMeshOverlay>();
    overlay.unload();
    let mut mesh = source();
    mesh.vertices[0] = [25.0, 3.0, 1.0];
    overlay
        .publish(LoadedNavMesh {
            mesh: Arc::new(mesh),
            path: "replacement.bin".into(),
            binding: OverlayBinding::Global,
        })
        .unwrap();
    app.update();
    let second = app.world().resource::<Cache>().entries[&OverlayBinding::Global]
        .surface
        .as_ref()
        .unwrap()
        .mesh
        .id();
    assert_ne!(first, second);
    assert_eq!(app.world().resource::<Assets<Mesh>>().len(), 2);
}
