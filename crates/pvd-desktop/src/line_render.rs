use std::path::{Path, PathBuf};

use bevy::{
    asset::io::embedded::EmbeddedAssetRegistry,
    gizmos::{
        AppGizmoBuilder,
        config::{DefaultGizmoConfigGroup, GizmoConfig, GizmoConfigGroup, GizmoConfigStore},
    },
    prelude::*,
};

pub const DEFAULT_LINE_WIDTH: f32 = 1.25;

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
