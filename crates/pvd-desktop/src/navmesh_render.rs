//! A static overlay whose assets are independent of physics playback frames.
use bevy::{
    asset::RenderAssetUsages,
    light::{NotShadowCaster, NotShadowReceiver},
    mesh::Indices,
    prelude::*,
    render::render_resource::PrimitiveTopology,
};

use crate::{
    coordinates::Coordinates,
    navmesh::{Alignment, NavMeshOverlay, OverlayBinding},
    scene::Viewport,
    studio::Studio,
};
use std::collections::HashMap;

pub struct Plugin;

impl bevy::prelude::Plugin for Plugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NavMeshOverlay>()
            .init_resource::<Cache>()
            .add_systems(
                Update,
                crate::navmesh::poll.after(crate::ui::prepare).before(sync),
            )
            .add_systems(
                Update,
                sync.after(crate::scene::sync_scene)
                    .before(crate::scene::update_camera),
            );
    }
}

#[derive(Resource, Default)]
struct Cache {
    entries: HashMap<OverlayBinding, EntryCache>,
}

#[derive(Default)]
struct EntryCache {
    key: Option<(u64, Coordinates, Alignment)>,
    surface: Option<Draw>,
    edges: Option<Draw>,
    bounds: Option<(Vec3, Vec3)>,
    opacity: f32,
    visibility: Option<(bool, bool)>,
}

struct Draw {
    entity: Entity,
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
}

impl EntryCache {
    fn clear(
        &mut self,
        commands: &mut Commands,
        meshes: &mut Assets<Mesh>,
        materials: &mut Assets<StandardMaterial>,
    ) {
        for draw in [self.surface.take(), self.edges.take()]
            .into_iter()
            .flatten()
        {
            commands.entity(draw.entity).despawn();
            meshes.remove(draw.mesh.id());
            materials.remove(draw.material.id());
        }
        self.key = None;
        self.bounds = None;
        self.visibility = None;
    }
}

fn transformed_point(point: [f32; 3], transform: Mat4, coordinates: Coordinates) -> [f32; 3] {
    coordinates
        .point(transform.transform_point3(Vec3::from(point)))
        .to_array()
}

fn surface_mesh(
    source: &pvd_core::navmesh::NavMesh,
    alignment: Alignment,
    coordinates: Coordinates,
) -> Mesh {
    let transform = alignment.transform();
    let positions: Vec<_> = source
        .vertices
        .iter()
        .map(|&p| transformed_point(p, transform, coordinates))
        .collect();
    let mut indices = source.indices.clone();
    if coordinates.mirrored ^ alignment.mirrored() {
        for triangle in indices.chunks_exact_mut(3) {
            triangle.swap(1, 2);
        }
    }
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(
        Mesh::ATTRIBUTE_NORMAL,
        vec![[0.0, 1.0, 0.0]; positions.len()],
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_indices(Indices::U32(indices))
}

fn edge_mesh(
    source: &pvd_core::navmesh::NavMesh,
    alignment: Alignment,
    coordinates: Coordinates,
) -> Mesh {
    let transform = alignment.transform();
    let positions: Vec<_> = source
        .edges
        .iter()
        .flatten()
        .map(|&p| transformed_point(p, transform, coordinates))
        .collect();
    Mesh::new(PrimitiveTopology::LineList, RenderAssetUsages::default())
        .with_inserted_attribute(
            Mesh::ATTRIBUTE_NORMAL,
            vec![[0.0, 1.0, 0.0]; positions.len()],
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
}

fn bounds(
    source: &pvd_core::navmesh::NavMesh,
    alignment: Alignment,
    coordinates: Coordinates,
) -> (Vec3, Vec3) {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for mask in 0..8 {
        let point = std::array::from_fn(|axis| {
            if mask & (1 << axis) == 0 {
                source.bounds_min[axis]
            } else {
                source.bounds_max[axis]
            }
        });
        let point = coordinates.point(alignment.point(Vec3::from(point)));
        min = min.min(point);
        max = max.max(point);
    }
    (min, max)
}

fn spawn(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    mesh: Mesh,
    color: Color,
    depth_bias: f32,
) -> Draw {
    let mesh = meshes.add(mesh);
    let material = materials.add(StandardMaterial {
        base_color: color,
        unlit: true,
        cull_mode: None,
        double_sided: true,
        // Blend keeps the overlay out of the depth buffer, including opacity 1.
        alpha_mode: AlphaMode::Blend,
        depth_bias,
        ..default()
    });
    let entity = commands
        .spawn((
            Name::new("NavMesh overlay"),
            Mesh3d(mesh.clone()),
            MeshMaterial3d(material.clone()),
            Transform::default(),
            NotShadowCaster,
            NotShadowReceiver,
        ))
        .id();
    Draw {
        entity,
        mesh,
        material,
    }
}

fn sync(
    mut commands: Commands,
    mut overlay: ResMut<NavMeshOverlay>,
    mut studio: ResMut<Studio>,
    mut viewport: ResMut<Viewport>,
    mut cache: ResMut<Cache>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    cache.entries.retain(|binding, entry| {
        if overlay.entry(*binding).is_some() {
            true
        } else {
            entry.clear(&mut commands, &mut meshes, &mut materials);
            false
        }
    });
    let focus_requested = overlay.focus_requested.take();
    let mut focus_bounds = None;
    for entry in &overlay.entries {
        let loaded = &entry.loaded;
        let settings = entry.settings;
        let visible = entry.visible_for(&studio);
        if !visible && !cache.entries.contains_key(&loaded.binding) {
            continue;
        }
        let cache = cache.entries.entry(loaded.binding).or_default();
        let key = (entry.revision, viewport.coordinates, settings.alignment);
        if cache.key != Some(key) {
            cache.clear(&mut commands, &mut meshes, &mut materials);
            let source = &loaded.mesh;
            cache.surface = Some(spawn(
                &mut commands,
                &mut meshes,
                &mut materials,
                surface_mesh(source, settings.alignment, viewport.coordinates),
                Color::srgba(0.08, 0.65, 0.95, settings.opacity),
                1.0,
            ));
            cache.edges = Some(spawn(
                &mut commands,
                &mut meshes,
                &mut materials,
                edge_mesh(source, settings.alignment, viewport.coordinates),
                Color::srgba(0.25, 0.85, 1.0, 0.95),
                2.0,
            ));
            cache.bounds = Some(bounds(source, settings.alignment, viewport.coordinates));
            cache.opacity = settings.opacity;
            cache.key = Some(key);
        }
        if cache.opacity != settings.opacity {
            if let Some(draw) = &cache.surface
                && let Some(material) = materials.get_mut(&draw.material)
            {
                material.base_color = Color::srgba(0.08, 0.65, 0.95, settings.opacity);
            }
            cache.opacity = settings.opacity;
        }
        let visibility = (
            visible && settings.show_surface && settings.opacity > 0.0,
            visible && settings.show_edges,
        );
        if cache.visibility != Some(visibility) {
            for (draw, visible) in [(&cache.surface, visibility.0), (&cache.edges, visibility.1)] {
                if let Some(draw) = draw {
                    commands.entity(draw.entity).insert(if visible {
                        Visibility::Inherited
                    } else {
                        Visibility::Hidden
                    });
                }
            }
            cache.visibility = Some(visibility);
        }
        if focus_requested == Some(loaded.binding) && visible {
            focus_bounds = cache.bounds;
        }
    }
    if let Some((min, max)) = focus_bounds {
        studio.follow_selected = false;
        studio.focus_selected = false;
        studio.captured_camera = None;
        viewport.captured_pose = None;
        viewport.target = min * 0.5 + max * 0.5;
        viewport.distance = ((max - min).length() * 1.4).clamp(
            0.01 * viewport.coordinates.scale,
            1e12 * viewport.coordinates.scale,
        );
    }
}

#[cfg(test)]
mod tests;
