use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use bevy::{
    asset::io::embedded::EmbeddedAssetRegistry,
    gizmos::{
        AppGizmoBuilder,
        config::{DefaultGizmoConfigGroup, GizmoConfig, GizmoConfigGroup, GizmoConfigStore},
    },
    prelude::*,
    render::{
        Extract, ExtractSchedule, Render, RenderApp, RenderSystems,
        render_resource::PollType,
        renderer::{RenderDevice, RenderQueue, render_system},
    },
};

pub const DEFAULT_LINE_WIDTH: f32 = 1.25;
const LARGE_LINE_SUBMISSION: usize = 8_000_000;
const LARGE_TRIANGLE_SUBMISSION: usize = 8_000_000;

#[derive(Default, Reflect, GizmoConfigGroup)]
pub(crate) struct GridConfig;

pub struct Plugin {
    pub smooth_lines: bool,
}

impl bevy::prelude::Plugin for Plugin {
    fn build(&self, app: &mut App) {
        app.insert_gizmo_config(
            GridConfig,
            GizmoConfig {
                depth_bias: -0.001,
                ..default()
            },
        );
        app.add_systems(Update, sync_style.before(crate::scene::draw_grid));
        if let Some(render) = app.get_sub_app_mut(RenderApp) {
            render
                .init_resource::<RenderLoad>()
                .add_systems(ExtractSchedule, measure_render_load)
                .add_systems(
                    Render,
                    limit_large_submissions
                        .after(render_system)
                        .before(RenderSystems::Cleanup),
                );
        }
        let shader = format!(
            "const SMOOTH_EDGES: bool = {};\n{}",
            self.smooth_lines,
            include_str!("../../../vendor/bevy_gizmo_lines/lines.wgsl"),
        );
        // The pinned Bevy 0.18.1 gizmo renderer resolves this path during RenderStartup.
        app.world()
            .resource::<EmbeddedAssetRegistry>()
            .insert_asset(
                PathBuf::from(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../vendor/bevy_gizmo_lines/lines.wgsl"
                )),
                Path::new("bevy_gizmos_render/lines.wgsl"),
                shader.into_bytes(),
            );
    }
}

#[derive(Default, Resource)]
struct RenderLoad {
    large_lines: bool,
    large_surfaces: bool,
}

fn measure_render_load(
    mut load: ResMut<RenderLoad>,
    viewport: Extract<Res<crate::scene::Viewport>>,
    gizmos: Extract<Query<&Gizmo>>,
    assets: Extract<Res<Assets<GizmoAsset>>>,
) {
    let mut vertices = 0;
    load.large_lines = gizmos.iter().any(|gizmo| {
        if let Some(asset) = assets.get(&gizmo.handle) {
            vertices += asset.list_positions.len();
        }
        vertices >= LARGE_LINE_SUBMISSION
    });
    load.large_surfaces = viewport.solid_triangles_reach(LARGE_TRIANGLE_SUBMISSION);
}

fn limit_large_submissions(
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    load: Res<RenderLoad>,
    mut previous: Local<Option<Arc<AtomicBool>>>,
) {
    // Bound work retained by the driver when repeated instanced draws outpace the GPU.
    let previous_pending = previous
        .as_ref()
        .is_some_and(|done| !done.load(Ordering::Acquire));
    if load.large_lines || (load.large_surfaces && previous_pending) {
        let _ = device.poll(PollType::wait_indefinitely());
        *previous = None;
    } else if load.large_surfaces {
        let done = Arc::new(AtomicBool::new(false));
        *previous = Some(done.clone());
        queue.on_submitted_work_done(move || done.store(true, Ordering::Release));
    }
}

fn sync_style(studio: Res<crate::studio::Studio>, mut configs: ResMut<GizmoConfigStore>) {
    let requested = studio.prefs.gizmo_line_width;
    let width = if requested.is_finite() {
        requested.clamp(0.5, 4.0)
    } else {
        DEFAULT_LINE_WIDTH
    };
    if configs.config::<DefaultGizmoConfigGroup>().0.line.width != width {
        configs.config_mut::<DefaultGizmoConfigGroup>().0.line.width = width;
    }
    if configs.config::<GridConfig>().0.line.width != width {
        configs.config_mut::<GridConfig>().0.line.width = width;
    }
}
