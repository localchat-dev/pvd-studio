use crate::{
    line_render::GridConfig,
    scene::{RenderCheck, Viewport},
    studio::Studio,
};
use bevy::{
    gizmos::config::GizmoConfigStore,
    prelude::*,
    render::view::screenshot::{Screenshot, ScreenshotCaptured},
};
use serde::Serialize;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

pub(crate) struct Plugin;
const PROBE_CAMERA: &str = "__pvd_grid_probe_camera";

#[derive(Clone, Copy, Serialize)]
struct Case {
    name: &'static str,
    target_x: f32,
    target_y: f32,
    yaw: f32,
    pitch: f32,
    distance: f32,
    show_grid: bool,
    show_static: bool,
    rolled_camera: bool,
    depth_bias: f32,
    far: f32,
}

fn cases() -> Vec<Case> {
    let mut cases = Vec::with_capacity(34);
    for depth_bias in [0.0, -0.001] {
        let base = Case {
            name: "rotation",
            target_x: 0.0,
            target_y: 0.0,
            yaw: 0.7,
            pitch: 0.45,
            distance: 25.0,
            show_grid: true,
            show_static: true,
            rolled_camera: false,
            depth_bias,
            far: 1_000_000.0,
        };
        for target_x in [0.0, 10.0, 25.0, 40.0] {
            cases.push(Case {
                name: "translation",
                target_x,
                ..base
            });
        }
        for show_static in [true, false] {
            for yaw in [0.7, 0.72, 0.74] {
                cases.push(Case {
                    yaw,
                    show_static,
                    ..base
                });
            }
        }
        for distance in [10.0, 60.0] {
            cases.push(Case {
                name: "zoom",
                distance,
                ..base
            });
        }
        cases.push(Case {
            name: "far-plane",
            far: 30.0,
            ..base
        });
    }
    let extended = Case {
        name: "long-translation",
        target_x: 0.0,
        target_y: 0.0,
        yaw: 0.7,
        pitch: 0.45,
        distance: 25.0,
        show_grid: true,
        show_static: false,
        rolled_camera: false,
        depth_bias: -0.001,
        far: 1_000_000.0,
    };
    cases.push(Case {
        target_x: 10_000.0,
        ..extended
    });
    for distance in [500.0, 5_000.0] {
        cases.push(Case {
            name: "far-zoom",
            distance,
            ..extended
        });
    }
    cases.extend([
        Case {
            name: "horizon",
            pitch: 0.025,
            ..extended
        },
        Case {
            name: "below-ground",
            pitch: -0.45,
            ..extended
        },
        Case {
            name: "looking-up",
            target_y: 40.0,
            pitch: -0.45,
            ..extended
        },
        Case {
            name: "rolled-camera",
            rolled_camera: true,
            ..extended
        },
        Case {
            name: "grid-hidden",
            show_grid: false,
            ..extended
        },
    ]);
    cases
}

#[derive(Resource)]
struct Probe {
    output: Option<PathBuf>,
    input: String,
    frame: usize,
    cases: Vec<Case>,
    next: usize,
    tick: u64,
    sought: bool,
    warmed_at: Option<u64>,
    applied_at: Option<u64>,
    screenshot_pending: bool,
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
        let directory = PathBuf::from(&check.output);
        let created =
            std::fs::create_dir(&directory).and_then(|()| std::fs::canonicalize(&directory));
        let (output, failure) = match created {
            Ok(output) => (Some(output), None),
            Err(error) => (
                None,
                Some(format!(
                    "grid probe requires a new output directory: {error}"
                )),
            ),
        };
        let input = {
            let mut studio = app.world_mut().resource_mut::<Studio>();
            studio.prefs = Default::default();
            studio.persist_layout = false;
            studio.prefs.show_axes = false;
            studio.prefs.show_bounds = false;
            studio.prefs.show_com = false;
            studio.prefs.show_velocity = false;
            studio.prefs.show_local_axes = false;
            studio.prefs.show_debug_draw = false;
            studio.prefs.show_contacts = false;
            studio.prefs.show_query_rays = false;
            studio.prefs.show_query_sweeps = false;
            studio.prefs.show_query_overlaps = false;
            studio.prefs.show_query_hits = false;
            studio.prefs.show_query_normals = false;
            studio.prefs.show_particles = false;
            studio.prefs.show_cloth_points = false;
            studio.prefs.show_cloth_constraints = false;
            studio.path_input.clone()
        };
        app.insert_resource(Probe {
            output,
            input,
            frame,
            cases: cases(),
            next: 0,
            tick: 0,
            sought: false,
            warmed_at: None,
            applied_at: None,
            screenshot_pending: false,
            snapshots: Vec::new(),
            failure,
            started: Instant::now(),
            shutdown_started: None,
            exited: false,
        })
        .add_systems(Last, drive);
    }
}

fn drive(
    mut commands: Commands,
    mut probe: ResMut<Probe>,
    mut studio: ResMut<Studio>,
    mut viewport: ResMut<Viewport>,
    mut configs: ResMut<GizmoConfigStore>,
    mut exit: MessageWriter<AppExit>,
) {
    if probe.exited {
        return;
    }
    probe.tick += 1;
    if probe.started.elapsed() > Duration::from_secs(180) && probe.failure.is_none() {
        probe.failure = Some("grid motion capture timed out".into());
    }
    if probe.failure.is_some() || probe.next == probe.cases.len() {
        let shutdown_started = *probe.shutdown_started.get_or_insert_with(Instant::now);
        let stopped = studio.poll_shutdown();
        if !stopped && shutdown_started.elapsed() < Duration::from_secs(30) {
            return;
        }
        if !stopped {
            probe.failure = Some("grid probe background shutdown timed out".into());
        }
        let result = write_report(&probe);
        if let Some(failure) = &probe.failure {
            eprintln!("Grid motion check failed: {failure}");
        }
        if let Err(error) = &result {
            eprintln!("Grid motion report failed: {error}");
        }
        probe.exited = true;
        exit.write(if probe.failure.is_none() && result.is_ok() {
            AppExit::Success
        } else {
            AppExit::error()
        });
        return;
    }
    if probe.screenshot_pending || studio.recording.is_none() || studio.job.is_some() {
        return;
    }
    if !probe.sought {
        if probe.frame >= studio.count() {
            probe.failure = Some("requested frame is outside the recording".into());
            return;
        }
        studio.pause_display();
        studio.seek(probe.frame);
        probe.sought = true;
        return;
    }
    if studio.seeking.is_some() {
        return;
    }
    if studio.frame != probe.frame {
        probe.failure = Some("capture frame changed during grid motion check".into());
        return;
    }
    let tick = probe.tick;
    let warmed_at = *probe.warmed_at.get_or_insert(tick);
    if tick - warmed_at < 90 {
        return;
    }
    if probe.next == 0
        && probe.applied_at.is_none()
        && (!viewport
            .objects
            .iter()
            .any(|object| !object.dynamic && matches!(object.geometry, pvd_core::Geometry::Plane))
            || !viewport.objects.iter().any(|object| {
                object.dynamic && !matches!(object.geometry, pvd_core::Geometry::Plane)
            }))
    {
        probe.failure = Some("grid probe requires a static Plane and a dynamic solid".into());
        return;
    }
    let case = probe.cases[probe.next];
    if probe.applied_at.is_none() {
        studio.prefs.show_grid = case.show_grid;
        studio.prefs.show_static = case.show_static;
        studio.prefs.camera_near = 0.01;
        studio.prefs.camera_far = case.far;
        studio.follow_selected = false;
        studio.focus_selected = false;
        studio.captured_camera = None;
        if probe.next > 0 && probe.cases[probe.next - 1].rolled_camera {
            studio.state.cameras.remove(PROBE_CAMERA);
        }
        viewport.captured_pose = None;
        viewport.target = Vec3::new(case.target_x, case.target_y, 0.0);
        viewport.yaw = case.yaw;
        viewport.pitch = case.pitch;
        viewport.distance = case.distance;
        if case.rolled_camera {
            if studio.state.cameras.contains_key(PROBE_CAMERA) {
                probe.failure =
                    Some("capture already contains the reserved diagnostic camera".into());
                return;
            }
            // Diagnostic camera only: exercise the actual recorded-camera path,
            // including its non-world up vector, without changing the input file.
            studio.state.cameras.insert(
                PROBE_CAMERA.into(),
                pvd_core::SourceCamera {
                    position: viewport
                        .coordinates
                        .source_point(viewport.position())
                        .to_array(),
                    target: viewport
                        .coordinates
                        .source_point(viewport.target)
                        .to_array(),
                    up: viewport
                        .coordinates
                        .source_direction(Vec3::new(1.0, 1.0, 0.0))
                        .to_array(),
                },
            );
            studio.captured_camera = Some(PROBE_CAMERA.into());
        }
        configs.config_mut::<GridConfig>().0.depth_bias = case.depth_bias;
        probe.applied_at = Some(tick);
        return;
    }
    if tick - probe.applied_at.unwrap() < 6 {
        return;
    }
    let planes = viewport
        .objects
        .iter()
        .filter(|object| matches!(object.geometry, pvd_core::Geometry::Plane))
        .count();
    let solids = viewport
        .objects
        .iter()
        .filter(|object| object.dynamic && !matches!(object.geometry, pvd_core::Geometry::Plane))
        .map(|object| object.id)
        .collect::<Vec<_>>();
    if solids.is_empty() || (planes > 0) != case.show_static {
        probe.failure = Some("grid motion fixture visibility did not settle".into());
        return;
    }
    let name = format!("{:02}-{}.png", probe.next, case.name);
    let path = probe.output.as_ref().unwrap().join(&name);
    if path.exists() {
        probe.failure = Some(format!("refusing to overwrite {name}"));
        return;
    }
    let row = serde_json::json!({
        "file": name,
        "case": case,
        "frame": studio.frame,
        "target": viewport.view_target().to_array(),
        "position": viewport.position().to_array(),
        "distance": viewport.view_distance(),
        "view_up": viewport.view_up().to_array(),
        "yaw": viewport.yaw,
        "pitch": viewport.pitch,
        "coordinate_scale": viewport.coordinates.scale,
        "grid_visible": studio.prefs.show_grid,
        "captured_camera": studio.captured_camera,
        "near": viewport.near,
        "far": viewport.far,
        "grid_depth_bias": configs.config::<GridConfig>().0.depth_bias,
        "static_planes": planes,
        "dynamic_solids": solids,
        "settle_frames": tick - probe.applied_at.unwrap(),
    });
    probe.screenshot_pending = true;
    commands
        .spawn(Screenshot::image(viewport.image.clone()))
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
                        let mut row = row.clone();
                        row["image_size"] = serde_json::json!([
                            capture.image.texture_descriptor.size.width,
                            capture.image.texture_descriptor.size.height,
                        ]);
                        probe.snapshots.push(row);
                        probe.next += 1;
                        probe.applied_at = None;
                    }
                    Err(error) => probe.failure = Some(format!("grid screenshot failed: {error}")),
                }
                probe.screenshot_pending = false;
            },
        );
}

fn write_report(probe: &Probe) -> Result<(), String> {
    let output = probe
        .output
        .as_ref()
        .ok_or("output directory unavailable")?;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output.join("grid-motion.json"))
        .map_err(|error| error.to_string())?;
    serde_json::to_writer_pretty(
        file,
        &serde_json::json!({
            "capture": probe.input,
            "frame": probe.frame,
            "failure": probe.failure,
            "expected_images": probe.cases.len(),
            "snapshots": probe.snapshots,
            "scope": "Fixed captured frame; offscreen 3D screenshots after camera and grid settings settle. Pixel comparison and visual inspection are separate checks.",
        }),
    )
    .map_err(|error| error.to_string())
}
