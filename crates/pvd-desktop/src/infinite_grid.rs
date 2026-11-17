//! Analytic ground grid: one screen-covering triangle, independent of world extent.
use bevy::{
    asset::{RenderAssetUsages, load_internal_asset, uuid_handle},
    camera::visibility::NoFrustumCulling,
    gizmos::config::GizmoConfigStore,
    light::{NotShadowCaster, NotShadowReceiver},
    mesh::MeshVertexBufferLayoutRef,
    pbr::{MaterialPipeline, MaterialPipelineKey},
    prelude::*,
    render::render_resource::{
        AsBindGroup, PrimitiveTopology, RenderPipelineDescriptor, SpecializedMeshPipelineError,
    },
    shader::ShaderRef,
};

use crate::{line_render::GridConfig, scene::Viewport, studio::Studio};

const SHADER: Handle<Shader> = uuid_handle!("6753a9a1-840f-437b-933b-52b6597892ad");

pub struct Plugin {
    pub smooth_lines: bool,
}

impl bevy::prelude::Plugin for Plugin {
    fn build(&self, app: &mut App) {
        load_internal_asset!(app, SHADER, "infinite_grid.wgsl", Shader::from_wgsl);
        app.add_plugins(MaterialPlugin::<GridMaterial>::default())
            .insert_resource(Style {
                smooth_lines: self.smooth_lines,
            })
            .add_systems(Startup, setup)
            .add_systems(Update, sync.after(crate::scene::draw_grid));
    }
}

#[derive(Resource)]
struct Style {
    smooth_lines: bool,
}

#[derive(Component)]
struct GroundGrid;

#[derive(Asset, TypePath, AsBindGroup, Clone)]
struct GridMaterial {
    /// Display units per recorded unit, pixel line width, depth bias, antialias toggle.
    #[uniform(0)]
    settings: Vec4,
    #[uniform(1)]
    color: Vec4,
}

impl Material for GridMaterial {
    fn vertex_shader() -> ShaderRef {
        SHADER.into()
    }
    fn fragment_shader() -> ShaderRef {
        SHADER.into()
    }
    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }
    fn depth_bias(&self) -> f32 {
        // Bevy adds this to the transparent sort key (ascending, back to front).
        // Draw the reference behind blended scene geometry; the shader still tests real depth.
        -f32::MAX
    }
    fn enable_prepass() -> bool {
        false
    }
    fn enable_shadows() -> bool {
        false
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        descriptor.primitive.cull_mode = None;
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            // Each visible fragment tests the ground hit's depth but never occludes scene data.
            depth.depth_write_enabled = false;
        }
        Ok(())
    }
}

fn parameters(scale: f32, width: f32, depth_bias: f32, smooth_lines: bool) -> Vec4 {
    Vec4::new(
        if scale.is_finite() {
            scale.clamp(1e-6, 1e6)
        } else {
            1.0
        },
        if width.is_finite() {
            width.clamp(0.5, 4.0)
        } else {
            1.25
        },
        if depth_bias.is_finite() {
            depth_bias.clamp(-1.0, 1.0)
        } else {
            -0.001
        },
        if smooth_lines { 1.0 } else { 0.0 },
    )
}

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<GridMaterial>>,
    style: Res<Style>,
) {
    let mesh = meshes.add(
        Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        )
        .with_inserted_attribute(
            Mesh::ATTRIBUTE_POSITION,
            vec![[-1.0, -1.0, 0.0], [3.0, -1.0, 0.0], [-1.0, 3.0, 0.0]],
        ),
    );
    let material = materials.add(GridMaterial {
        settings: parameters(1.0, 1.25, -0.001, style.smooth_lines),
        color: Color::srgb(0.28, 0.35, 0.43).to_linear().to_vec4(),
    });
    commands.spawn((
        Name::new("Infinite reference grid"),
        GroundGrid,
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::default(),
        Visibility::Inherited,
        NoFrustumCulling,
        NotShadowCaster,
        NotShadowReceiver,
    ));
}

fn sync(
    studio: Res<Studio>,
    viewport: Res<Viewport>,
    style: Res<Style>,
    configs: Res<GizmoConfigStore>,
    mut materials: ResMut<Assets<GridMaterial>>,
    mut grids: Query<(&MeshMaterial3d<GridMaterial>, &mut Visibility), With<GroundGrid>>,
) {
    let config = configs.config::<GridConfig>().0;
    let settings = parameters(
        viewport.coordinates.scale,
        config.line.width,
        config.depth_bias,
        style.smooth_lines,
    );
    for (handle, mut visibility) in &mut grids {
        let visible = if studio.prefs.show_grid {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *visibility != visible {
            *visibility = visible;
        }
        if materials
            .get(&handle.0)
            .is_some_and(|material| material.settings != settings)
            && let Some(material) = materials.get_mut(&handle.0)
        {
            material.settings = settings;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let mut app = App::new();
        let mut configs = GizmoConfigStore::default();
        configs.insert(default(), GridConfig);
        app.init_resource::<Studio>()
            .init_resource::<Viewport>()
            .init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<GridMaterial>>()
            .insert_resource(Style { smooth_lines: true })
            .insert_resource(configs)
            .add_systems(Startup, setup)
            .add_systems(Update, sync);
        app
    }

    #[test]
    fn infinite_grid_keeps_one_triangle_and_material_across_camera_and_unit_changes() {
        let mut app = app();
        app.update();
        let (mesh_id, material_id) = {
            let world = app.world_mut();
            let mut query = world
                .query_filtered::<(&Mesh3d, &MeshMaterial3d<GridMaterial>), With<GroundGrid>>();
            let (mesh, material) = query.single(world).unwrap();
            (mesh.0.id(), material.0.id())
        };
        for scale in [1e-6, 0.001, 1.0, 1000.0, 1e6] {
            for (target, distance, pitch) in [
                (0.0, 25.0, 0.45),
                (10000.0, 5000.0, 0.025),
                (-10000.0, 60.0, -0.45),
            ] {
                {
                    let mut view = app.world_mut().resource_mut::<Viewport>();
                    view.target = Vec3::new(target, 0.0, target);
                    view.distance = distance;
                    view.pitch = pitch;
                    view.coordinates.scale = scale;
                }
                app.update();
                let meshes = app.world().resource::<Assets<Mesh>>();
                assert_eq!(meshes.len(), 1);
                assert_eq!(meshes.get(mesh_id).unwrap().count_vertices(), 3);
                let materials = app.world().resource::<Assets<GridMaterial>>();
                assert_eq!(materials.len(), 1);
                assert_eq!(materials.get(material_id).unwrap().settings.x, scale);
            }
        }
    }

    #[test]
    fn grid_visibility_and_existing_depth_bias_are_respected() {
        let mut app = app();
        app.update();
        app.world_mut().resource_mut::<Studio>().prefs.show_grid = false;
        app.world_mut()
            .resource_mut::<GizmoConfigStore>()
            .config_mut::<GridConfig>()
            .0
            .depth_bias = -0.001;
        app.update();
        let world = app.world_mut();
        let mut query = world
            .query_filtered::<(&Visibility, &MeshMaterial3d<GridMaterial>), With<GroundGrid>>();
        let (visibility, handle) = query.single(world).unwrap();
        assert_eq!(*visibility, Visibility::Hidden);
        assert_eq!(
            world
                .resource::<Assets<GridMaterial>>()
                .get(&handle.0)
                .unwrap()
                .settings
                .z,
            -0.001
        );
    }

    #[test]
    fn invalid_grid_configuration_never_reaches_the_shader() {
        let p = parameters(f32::NAN, f32::INFINITY, f32::NEG_INFINITY, true);
        assert!(p.is_finite());
        assert_eq!(p, Vec4::new(1.0, 1.25, -0.001, 1.0));
        assert_eq!(
            parameters(0.0, -10.0, -2.0, false),
            Vec4::new(1e-6, 0.5, -1.0, 0.0)
        );
    }
}
