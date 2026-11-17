//! Offscreen checks exercise the production scene selection and NavMesh import paths.

use crate::{
    navmesh::{NavMeshOverlay, OverlayBinding, OverlaySettings},
    scene::{RenderCheck, Viewport},
    studio::Studio,
};
use bevy::{
    prelude::*,
    render::view::screenshot::{Screenshot, ScreenshotCaptured},
};
use pvd_core::frame_sources::FrameObject;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

pub(crate) struct Plugin {
    pub paths: [PathBuf; 2],
}

#[derive(Clone, Copy)]
enum Stage {
    Seek,
    Discover,
    SelectImport(usize),
    AwaitImport(usize),
    SelectView(usize),
    Capture(usize),
    Finished,
}

#[derive(Resource)]
struct Probe {
    paths: [PathBuf; 2],
    output: Option<PathBuf>,
    frame: usize,
    scenes: Vec<FrameObject>,
    settings: Vec<OverlaySettings>,
    stage: Stage,
    tick: u64,
    applied_at: u64,
    screenshot_pending: bool,
    geometry_only: bool,
    snapshots: Vec<serde_json::Value>,
    failure: Option<String>,
    started: Instant,
    shutdown_started: Option<Instant>,
    exited: bool,
}

impl bevy::prelude::Plugin for Plugin {
    fn build(&self, app: &mut App) {
        let check = app.world().resource::<RenderCheck>();
        let frame = check.desired_frame.unwrap_or(0);
        let path = PathBuf::from(&check.output);
        let (output, failure) =
            match std::fs::create_dir(&path).and_then(|()| std::fs::canonicalize(&path)) {
                Ok(path) => (Some(path), None),
                Err(error) => (
                    None,
                    Some(format!(
                        "NavMesh probe requires a new output directory: {error}"
                    )),
                ),
            };
        app.world_mut().resource_mut::<Studio>().persist_layout = false;
        app.insert_resource(Probe {
            paths: self.paths.clone(),
            output,
            frame,
            scenes: Vec::new(),
            settings: Vec::new(),
            stage: Stage::Seek,
            tick: 0,
            applied_at: 0,
            screenshot_pending: false,
            geometry_only: false,
            snapshots: Vec::new(),
            failure,
            started: Instant::now(),
            shutdown_started: None,
            exited: false,
        })
        .add_systems(Last, drive);
    }
}

fn binding(studio: &Studio, scene: FrameObject) -> OverlayBinding {
    OverlayBinding::Room {
        epoch: studio.room_session_epoch(),
        room: scene,
    }
}

fn drive(
    mut commands: Commands,
    mut probe: ResMut<Probe>,
    mut studio: ResMut<Studio>,
    mut overlay: ResMut<NavMeshOverlay>,
    mut viewport: ResMut<Viewport>,
    check: Res<RenderCheck>,
    mut exit: MessageWriter<AppExit>,
) {
    if probe.exited {
        return;
    }
    probe.tick += 1;
    if probe.started.elapsed() > Duration::from_secs(180) && probe.failure.is_none() {
        probe.failure = Some("NavMesh scene check timed out".into());
    }
    if probe.failure.is_some() || matches!(probe.stage, Stage::Finished) {
        let shutdown_started = *probe.shutdown_started.get_or_insert_with(Instant::now);
        let stopped = studio.poll_shutdown();
        if !stopped && shutdown_started.elapsed() < Duration::from_secs(30) {
            return;
        }
        if !stopped {
            probe.failure = Some("NavMesh probe background shutdown timed out".into());
        }
        let report = write_report(&probe);
        if let Some(error) = &probe.failure {
            eprintln!("NavMesh scene check failed: {error}");
        }
        if let Err(error) = &report {
            eprintln!("NavMesh scene report failed: {error}");
        }
        probe.exited = true;
        exit.write(if probe.failure.is_none() && report.is_ok() {
            AppExit::Success
        } else {
            AppExit::error()
        });
        return;
    }
    if probe.screenshot_pending || studio.recording.is_none() || studio.job.is_some() {
        return;
    }
    if studio.seeking.is_some() || studio.room_selection_pending() {
        return;
    }
    match probe.stage {
        Stage::Seek => {
            if probe.frame >= studio.count() {
                probe.failure = Some("requested frame is outside the recording".into());
                return;
            }
            studio.pause_display();
            studio.seek(probe.frame);
            probe.stage = Stage::Discover;
        }
        Stage::Discover => {
            probe.scenes = studio
                .room_directory()
                .into_iter()
                .flat_map(|rooms| rooms.iter())
                .map(|room| room.scene)
                .filter(|scene| {
                    studio.state.objects.get(&scene.id).is_some_and(|object| {
                        object.generation == scene.generation
                            && object.type_name.ends_with("::PxScene")
                    }) && studio.room_source(*scene).is_some()
                })
                .take(2)
                .collect();
            if probe.scenes.len() != 2 {
                probe.failure = Some(
                    "NavMesh scene check needs two surviving PxScenes with retained frames".into(),
                );
                return;
            }
            probe.stage = Stage::SelectImport(0);
        }
        Stage::SelectImport(index) => {
            let scene = probe.scenes[index];
            if studio.displayed_room() != Some(scene) {
                studio.select_room(Some(scene));
                return;
            }
            overlay.open = true;
            overlay.request_import(probe.paths[index].clone(), &studio, None);
            probe.stage = Stage::AwaitImport(index);
        }
        Stage::AwaitImport(index) => {
            if overlay.is_busy() {
                return;
            }
            let selected = binding(&studio, probe.scenes[index]);
            overlay.select_binding(selected);
            let Some(entry) = overlay.selected_mut() else {
                probe.failure = Some(format!("NavMesh import {index} did not publish its scene"));
                return;
            };
            if entry.loaded.path != probe.paths[index] || !entry.settings.alignment.flip_x {
                probe.failure = Some("import path or default Mirror X does not match".into());
                return;
            }
            entry.settings.opacity = if index == 0 { 0.28 } else { 0.68 };
            entry.settings.alignment.offset = if index == 0 {
                [-4.0, 0.0, 0.0]
            } else {
                [4.0, 0.0, 0.0]
            };
            entry.settings.alignment.flip_x = index == 0;
            probe.settings.push(entry.settings);
            probe.stage = if index == 0 {
                Stage::SelectImport(1)
            } else {
                Stage::SelectView(0)
            };
        }
        Stage::SelectView(index) => {
            let scene = probe.scenes.get(index).copied();
            if studio.displayed_room() != scene {
                studio.select_room(scene);
                return;
            }
            let selected = scene.map_or(OverlayBinding::Global, |scene| binding(&studio, scene));
            overlay.select_binding(selected);
            // Keep both imported geometries in view so the visibility change is directly comparable.
            let mut min = Vec3::splat(f32::INFINITY);
            let mut max = Vec3::splat(f32::NEG_INFINITY);
            for entry in &overlay.entries {
                for point in &entry.loaded.mesh.vertices {
                    let point = viewport
                        .coordinates
                        .point(entry.settings.alignment.point(Vec3::from_array(*point)));
                    min = min.min(point);
                    max = max.max(point);
                }
            }
            studio.follow_selected = false;
            studio.focus_selected = false;
            studio.captured_camera = None;
            viewport.captured_pose = None;
            viewport.target = (min + max) * 0.5;
            viewport.distance = (max - min).length().max(1.0) * 2.0;
            viewport.yaw = 0.75;
            viewport.pitch = 0.6;
            probe.applied_at = probe.tick;
            probe.stage = Stage::Capture(index);
        }
        Stage::Capture(index) => {
            if overlay.open == probe.geometry_only {
                overlay.open = !probe.geometry_only;
                probe.applied_at = probe.tick;
                return;
            }
            if probe.tick - probe.applied_at < 30
                || viewport.displayed_revision() != studio.revision
            {
                return;
            }
            let settings_preserved = probe.scenes.iter().enumerate().all(|(index, scene)| {
                overlay
                    .entry(binding(&studio, *scene))
                    .is_some_and(|entry| {
                        entry.settings == probe.settings[index]
                            && entry.loaded.path == probe.paths[index]
                    })
            });
            let visible = overlay
                .entries
                .iter()
                .filter(|entry| entry.visible_for(&studio))
                .count();
            let expected_visible = if index == 2 { 2 } else { 1 };
            if overlay.entries.len() != 2 || !settings_preserved || visible != expected_visible {
                probe.failure = Some(format!(
                    "independent NavMesh state failed: {} entries, {visible} visible, settings={settings_preserved}",
                    overlay.entries.len()
                ));
                return;
            }
            let name = if probe.geometry_only {
                [
                    "scene-a-geometry.png",
                    "scene-b-geometry.png",
                    "all-scenes-geometry.png",
                ][index]
            } else {
                ["scene-a.png", "scene-b.png", "all-scenes.png"][index]
            };
            let path = probe.output.as_ref().unwrap().join(name);
            if path.exists() {
                probe.failure = Some(format!("refusing to overwrite {name}"));
                return;
            }
            let row = serde_json::json!({
                "file": name,
                "frame": studio.frame,
                "displayed_scene": studio.displayed_room().map(|scene| scene.id),
                "visible_overlays": visible,
                "overlays": overlay.entries.iter().map(|entry| serde_json::json!({
                    "binding": format!("{:?}", entry.loaded.binding),
                    "path": entry.loaded.path,
                    "tiles": entry.loaded.mesh.tile_count,
                    "triangles": entry.loaded.mesh.indices.len() / 3,
                    "opacity": entry.settings.opacity,
                    "offset": entry.settings.alignment.offset,
                    "mirror_x": entry.settings.alignment.flip_x,
                    "visible": entry.visible_for(&studio),
                })).collect::<Vec<_>>(),
            });
            probe.screenshot_pending = true;
            commands
                .spawn(Screenshot::image(check.target.clone()))
                .observe(
                    move |capture: On<ScreenshotCaptured>, mut probe: ResMut<Probe>| {
                        let result = capture
                            .image
                            .clone()
                            .try_into_dynamic()
                            .map_err(|error| error.to_string())
                            .and_then(|image| {
                                image
                                    .to_rgb8()
                                    .save(&path)
                                    .map_err(|error| error.to_string())
                            });
                        match result {
                            Ok(()) => {
                                probe.snapshots.push(row.clone());
                                probe.stage = if !probe.geometry_only {
                                    probe.geometry_only = true;
                                    Stage::Capture(index)
                                } else if index < 2 {
                                    probe.geometry_only = false;
                                    Stage::SelectView(index + 1)
                                } else {
                                    Stage::Finished
                                };
                            }
                            Err(error) => {
                                probe.failure = Some(format!("NavMesh screenshot failed: {error}"))
                            }
                        }
                        probe.screenshot_pending = false;
                    },
                );
        }
        Stage::Finished => {}
    }
}

fn write_report(probe: &Probe) -> Result<(), String> {
    let output = probe
        .output
        .as_ref()
        .ok_or("output directory unavailable")?;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output.join("navmesh-scenes.json"))
        .map_err(|error| error.to_string())?;
    serde_json::to_writer_pretty(file, &serde_json::json!({
        "inputs": probe.paths,
        "initial_frame": probe.frame,
        "failure": probe.failure,
        "snapshots": probe.snapshots,
        "scope": "Two asynchronous file imports in separate captured scenes; independent settings and visibility through scene switches and All scenes. Constructed navigation geometry is not paired to the captured physics map.",
    })).map_err(|error| error.to_string())
}
