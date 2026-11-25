use super::*;
use bevy::{ecs::system::SystemParam, gizmos::config::GizmoConfig};

// Keep small overlays together so their captured line submission order is preserved.
const IMMEDIATE_EDGE_LIMIT: usize = 32 * 1024;

type MeshVariants<T> = HashMap<(AssetId<Mesh>, [u32; 4]), (u64, T)>;

#[derive(Default)]
struct Cache {
    frame: u64,
    assets: MeshVariants<Handle<GizmoAsset>>,
    extents: MeshVariants<[Vec3; 2]>,
    drawn: HashSet<Entity>,
}

#[derive(SystemParam)]
pub(crate) struct Surfaces<'w, 's> {
    commands: Commands<'w, 's>,
    assets: Option<ResMut<'w, Assets<GizmoAsset>>>,
    attached: Query<'w, 's, (Entity, &'static Gizmo), With<CapturedObject>>,
    cache: Local<'s, Cache>,
}

impl Surfaces<'_, '_> {
    pub(super) fn draw(&mut self, studio: &Studio, viewport: &Viewport, gizmos: &mut SourceGizmos) {
        self.cache.frame = self.cache.frame.wrapping_add(1);
        let frame = self.cache.frame;
        self.cache.drawn.clear();
        let wireframe = studio.prefs.wireframe;
        let retained = wireframe && self.assets.is_some() && many_edges(viewport);
        if gizmos.inner.config.enabled && (wireframe || studio.prefs.show_bounds) {
            for object in &viewport.objects {
                let selected = studio.is_selected(object.id) || studio.is_selected(object.shape_id);
                let bounds = show_object_bounds(studio, object, selected);
                if !wireframe && !bounds {
                    continue;
                }
                let Some(rendered) = viewport.rendered.get(&(object.id, object.shape_id)) else {
                    continue;
                };
                let mesh = rendered.mesh.id();
                let Some(edges) = viewport.wires.get(&mesh) else {
                    continue;
                };
                let mut rotation = Quat::from_array(object.rotation).normalize();
                if matches!(object.geometry, Geometry::Capsule { .. }) {
                    rotation *= Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
                }
                let translation = Vec3::from(object.position);
                if wireframe {
                    let fallback = if selected {
                        [0.25, 0.85, 0.9, 1.0]
                    } else {
                        [0.6, 0.68, 0.76, 1.0]
                    };
                    let rgba = rendered.display.rgba(
                        fallback,
                        selected,
                        captured_display::opacity(studio, selected),
                    );
                    let color = Color::srgba(rgba[0], rgba[1], rgba[2], rgba[3]);
                    if retained {
                        self.retain(rendered, edges, rgba, color, gizmos.inner.config);
                    } else {
                        let transform =
                            Transform::from_translation(translation).with_rotation(rotation);
                        for edge in edges {
                            let [a, b] = edge.map(|point| transform.transform_point(point));
                            gizmos.line(a, b, color);
                        }
                    }
                }
                if bounds {
                    let (last_drawn, extent) = self
                        .cache
                        .extents
                        .entry((mesh, rotation.to_array().map(f32::to_bits)))
                        .or_insert_with(|| (frame, rotated_extents(edges, rotation)));
                    *last_drawn = frame;
                    draw_bounds(gizmos, extent.map(|v| v + translation));
                }
            }
        }
        for (entity, _) in &self.attached {
            if !self.cache.drawn.contains(&entity) {
                self.commands.entity(entity).remove::<Gizmo>();
            }
        }
        self.cache.assets.retain(|_, (used, _)| *used == frame);
        self.cache.extents.retain(|_, (used, _)| *used == frame);
    }

    fn retain(
        &mut self,
        rendered: &RenderedObject,
        edges: &[[Vec3; 2]],
        rgba: [f32; 4],
        color: Color,
        config: &GizmoConfig,
    ) {
        let frame = self.cache.frame;
        let (last_drawn, handle) = self
            .cache
            .assets
            .entry((rendered.mesh.id(), rgba.map(f32::to_bits)))
            .or_insert_with(|| {
                let mut asset = GizmoAsset::new();
                let vertices = edges.len() * 2;
                asset.list_positions.reserve_exact(vertices);
                asset.list_positions.extend(edges.iter().flatten().copied());
                asset.list_colors.resize(vertices, color.to_linear());
                (frame, self.assets.as_mut().unwrap().add(asset))
            });
        *last_drawn = frame;
        let entity = rendered.entity;
        if !self.attached.get(entity).is_ok_and(|(_, wire)| {
            wire.handle == *handle
                && wire.depth_bias == config.depth_bias
                && wire.line_config.width == config.line.width
                && wire.line_config.style == config.line.style
                && wire.line_config.joints == config.line.joints
                && wire.line_config.perspective == config.line.perspective
        }) {
            // Retained gizmos use the entity's GlobalTransform independently of Mesh3d visibility.
            self.commands.entity(entity).insert(Gizmo {
                handle: handle.clone(),
                line_config: config.line.clone(),
                depth_bias: config.depth_bias,
            });
        }
        self.cache.drawn.insert(entity);
    }
}

fn many_edges(viewport: &Viewport) -> bool {
    let mut count = 0;
    for object in viewport.rendered.values() {
        if let Some(edges) = viewport.wires.get(&object.mesh.id()) {
            count += edges.len();
            if count > IMMEDIATE_EDGE_LIMIT {
                return true;
            }
        }
    }
    false
}

fn rotated_extents(edges: &[[Vec3; 2]], rotation: Quat) -> [Vec3; 2] {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for &point in edges.iter().flatten() {
        let point = rotation * point;
        min = min.min(point);
        max = max.max(point);
    }
    [min, max]
}

fn draw_bounds(gizmos: &mut SourceGizmos, [min, max]: [Vec3; 2]) {
    if !min.is_finite() || !max.is_finite() {
        return;
    }
    let corners = [
        Vec3::new(min.x, min.y, min.z),
        Vec3::new(max.x, min.y, min.z),
        Vec3::new(max.x, max.y, min.z),
        Vec3::new(min.x, max.y, min.z),
        Vec3::new(min.x, min.y, max.z),
        Vec3::new(max.x, min.y, max.z),
        Vec3::new(max.x, max.y, max.z),
        Vec3::new(min.x, max.y, max.z),
    ];
    for (a, b) in [
        (0, 1),
        (1, 2),
        (2, 3),
        (3, 0),
        (4, 5),
        (5, 6),
        (6, 7),
        (7, 4),
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7),
    ] {
        gizmos.line(corners[a], corners[b], Color::srgb(0.93, 0.73, 0.35));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::gizmos::{
        config::{DefaultGizmoConfigGroup, GizmoConfigStore},
        gizmos::GizmoStorage,
    };

    #[test]
    fn rotated_bounds_match_every_transformed_edge_in_source_space() {
        let edges = [
            [Vec3::new(-0.2, 3.5, -7.0), Vec3::new(5.4, 0.1, 6.3)],
            [Vec3::new(4.0, -2.1, -3.0), Vec3::new(-4.6, -0.2, 4.8)],
        ];
        let rotation = Quat::from_euler(EulerRot::ZYX, 0.37, 0.6, -0.4);
        let extent = rotated_extents(&edges, rotation);
        for translation in [Vec3::ZERO, Vec3::new(-15.0, 250.0, 80000.0)] {
            let transform = Transform::from_translation(translation).with_rotation(rotation);
            let mut min = Vec3::splat(f32::INFINITY);
            let mut max = Vec3::splat(f32::NEG_INFINITY);
            for point in edges.iter().flatten() {
                let position = transform.transform_point(*point);
                min = min.min(position);
                max = max.max(position);
            }
            assert_eq!(extent.map(|v| v + translation), [min, max]);
        }
    }

    #[test]
    fn large_wire_draws_share_vertices_and_track_transform_color_scope_and_width() {
        let mut app = App::new();
        let mut studio = Studio::default();
        studio.prefs.wireframe = true;
        studio.prefs.show_axes = false;
        studio.prefs.show_bounds = false;
        studio.prefs.opacity = 1.0;
        studio.state.objects.insert(
            1,
            pvd_core::Object {
                id: 1,
                generation: 1,
                type_name: "physx3::PxRigidStatic".into(),
                properties: Default::default(),
            },
        );
        let mut config = GizmoConfigStore::default();
        config.insert(default(), DefaultGizmoConfigGroup);
        app.insert_resource(studio)
            .insert_resource(config)
            .insert_resource(GizmoStorage::<DefaultGizmoConfigGroup, ()>::default())
            .init_resource::<Assets<GizmoAsset>>()
            .add_systems(Update, draw_grid);
        let coordinates = Coordinates::new(
            crate::coordinates::UpAxis::Z,
            crate::coordinates::Handedness::Left,
            0.1,
        );
        let mut viewport = Viewport {
            coordinates,
            ..default()
        };
        let edges: Vec<_> = (0..=IMMEDIATE_EDGE_LIMIT / 2)
            .map(|i| [Vec3::new(i as f32, 1.0, 2.0), Vec3::new(i as f32, 2.0, 3.0)])
            .collect();
        let sample = edges[0][0];
        let vertex_count = edges.len() * 2;
        viewport.wires.insert(Handle::<Mesh>::default().id(), edges);
        let mut entities = Vec::new();
        for id in 1..=3 {
            let source =
                Transform::from_xyz(id as f32, 2.0, 7.0).with_rotation(Quat::from_rotation_x(0.59));
            let transform = coordinates.transform(source);
            let entity = app.world_mut().spawn((CapturedObject, transform)).id();
            entities.push(entity);
            viewport.objects.push(RenderObject {
                id,
                shape_id: id + 10,
                position: source.translation.to_array(),
                rotation: source.rotation.to_array(),
                geometry: Geometry::Box {
                    half_extents: [1.0; 3],
                },
                dynamic: false,
                sleeping: false,
            });
            viewport.rendered.insert(
                (id, id + 10),
                RenderedObject {
                    entity,
                    mesh: default(),
                    material: default(),
                    transform,
                    visible: false,
                    display: captured_display::Style {
                        color: Some([128, 64, 255, 128]),
                        ..default()
                    },
                },
            );
            assert!(
                transform
                    .transform_point(sample)
                    .abs_diff_eq(coordinates.point(source.transform_point(sample)), 1e-5)
            );
        }
        app.insert_resource(viewport);
        app.update();
        let original = app
            .world()
            .get::<Gizmo>(entities[0])
            .unwrap()
            .handle
            .clone();
        for entity in &entities {
            assert_eq!(app.world().get::<Gizmo>(*entity).unwrap().handle, original);
        }
        let asset = app
            .world()
            .resource::<Assets<GizmoAsset>>()
            .get(&original)
            .unwrap();
        assert_eq!(asset.list_positions.len(), vertex_count);
        assert_eq!(asset.list_positions[0], sample);
        assert_eq!(
            asset.list_colors[0],
            Color::srgba_u8(128, 64, 255, 128).to_linear()
        );

        app.world_mut()
            .resource_mut::<Studio>()
            .select_object(Some(1), false);
        app.world_mut()
            .resource_mut::<GizmoConfigStore>()
            .config_mut::<DefaultGizmoConfigGroup>()
            .0
            .line
            .width = 3.5;
        app.update();
        let highlighted = app.world().get::<Gizmo>(entities[0]).unwrap();
        assert_ne!(highlighted.handle, original);
        assert_eq!(highlighted.line_config.width, 3.5);
        let asset = app
            .world()
            .resource::<Assets<GizmoAsset>>()
            .get(&highlighted.handle)
            .unwrap();
        assert_eq!(
            asset.list_colors[0],
            Color::srgb(0.25, 0.85, 0.9).to_linear()
        );
        assert_eq!(
            app.world().get::<Gizmo>(entities[1]).unwrap().handle,
            original
        );

        let mut viewport = app.world_mut().resource_mut::<Viewport>();
        viewport.objects.pop();
        viewport.rendered.remove(&(3, 13));
        app.update();
        assert!(app.world().get::<Gizmo>(entities[2]).is_none());
        assert!(app.world().get::<Gizmo>(entities[1]).is_some());
        app.world_mut().resource_mut::<Studio>().prefs.wireframe = false;
        app.update();
        assert!(
            entities
                .iter()
                .all(|e| app.world().get::<Gizmo>(*e).is_none())
        );
    }
}
