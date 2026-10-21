use std::path::{Path, PathBuf};

use bevy::{
    asset::io::embedded::EmbeddedAssetRegistry,
    gizmos::config::{DefaultGizmoConfigGroup, GizmoConfigStore},
    prelude::*,
};

pub const DEFAULT_LINE_WIDTH: f32 = 1.25;

pub struct Plugin {
    pub smooth_lines: bool,
}

impl bevy::prelude::Plugin for Plugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, sync_style.before(crate::scene::draw_grid));
        if !self.smooth_lines {
            return;
        }
        // The pinned Bevy 0.18.1 gizmo renderer resolves this path during RenderStartup.
        app.world()
            .resource::<EmbeddedAssetRegistry>()
            .insert_asset(
                PathBuf::from(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../vendor/bevy_gizmo_lines/lines.wgsl"
                )),
                Path::new("bevy_gizmos_render/lines.wgsl"),
                include_bytes!("../../../vendor/bevy_gizmo_lines/lines.wgsl").as_slice(),
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
}
