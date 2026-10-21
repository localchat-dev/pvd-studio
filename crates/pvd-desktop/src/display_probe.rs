mod actions;
mod trace;
use trace::Submission;
pub(crate) use trace::{RequestTrace, Stamp, Trace};

use crate::{
    scene::{RenderCheck, Viewport},
    studio::{Panel, Studio},
};
use bevy::{
    prelude::*,
    render::{
        Extract, ExtractSchedule, Render, RenderApp, RenderSystems,
        renderer::{RenderAdapterInfo, RenderDevice, RenderQueue, render_system},
        view::screenshot::{Screenshot, ScreenshotCaptured},
    },
};
use bevy_egui::{EguiPostUpdateSet, EguiPrimaryContextPass};
use serde::Serialize;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Clone, Serialize)]
pub(crate) struct Config {
    pub capture: PathBuf,
    pub output: PathBuf,
    pub first: usize,
    pub last: usize,
    pub fps: f32,
    pub hide_static: bool,
    pub bounds: bool,
    pub ui_actions: bool,
    pub lang: &'static str,
}

impl Config {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        if !(4..=10).contains(&args.len()) {
            return Err(
                "render check requires CAPTURE NEW_OUTPUT_DIRECTORY [FIRST LAST FPS [hide-static] [bounds] [zh]]"
                    .into(),
            );
        }
        let parse = |index: usize, fallback: usize| {
            args.get(index).map_or(Ok(fallback), |value| {
                value
                    .parse::<usize>()
                    .map_err(|_| "invalid frame index".to_owned())
            })
        };
        let first = parse(4, 540)?;
        let last = parse(5, 660)?;
        let fps = args.get(6).map_or(Ok(30.0), |value| {
            value
                .parse::<f32>()
                .map_err(|_| "invalid playback rate".to_owned())
        })?;
        if first >= last || last - first > 480 || !fps.is_finite() || !(1.0..=120.0).contains(&fps)
        {
            return Err("expected 1..480 adjacent steps and a finite 1..120 FPS target".into());
        }
        let ui_actions = args
            .get(1)
            .is_some_and(|arg| arg == "--render-ui-actions-check");
        if ui_actions && last - first < 4 {
            return Err("UI actions check requires at least four adjacent steps".into());
        }
        let mut hide_static = false;
        let mut bounds = false;
        let mut lang = "en";
        for option in args.iter().skip(7) {
            match option.as_str() {
                "hide-static" if !hide_static => hide_static = true,
                "bounds" if !bounds => bounds = true,
                "zh" if lang == "en" => lang = "zh",
                _ => return Err("unknown or repeated render-check option".into()),
            }
        }
        let capture = std::fs::canonicalize(&args[2]).map_err(|error| error.to_string())?;
        let output = PathBuf::from(&args[3]);
        std::fs::create_dir(&output)
            .map_err(|error| format!("output must be a new directory: {error}"))?;
        let output = std::fs::canonicalize(output).map_err(|error| error.to_string())?;
        Ok(Self {
            capture,
            output,
            first,
            last,
            fps,
            hide_static,
            bounds,
            ui_actions,
            lang,
        })
    }
}

pub(crate) struct Plugin(pub Config);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Loading,
    Positioning,
    Warm,
    Actions,
    Forward,
    PauseForward,
    Reverse,
    PauseReverse,
    Boundary,
    Drain,
    Shutdown,
}

#[derive(Resource)]
struct Probe {
    config: Config,
    trace: Trace,
    phase: Phase,
    tick: u64,
    phase_tick: u64,
    ui_passes: u64,
    scene: Stamp,
    ui: Stamp,
    final_submission: Option<Submission>,
    screenshot_pending: bool,
    snapshots: Vec<serde_json::Value>,
    actions: Vec<serde_json::Value>,
    action_fixture: Option<serde_json::Value>,
    shutdown_at: Option<Instant>,
    adapter: serde_json::Value,
}

#[derive(Resource, Default)]
struct RenderProbe {
    trace: Option<Trace>,
    submission: Option<Submission>,
    serial: u64,
}

impl bevy::prelude::Plugin for Plugin {
    fn build(&self, app: &mut App) {
        let trace = Trace::default();
        {
            let mut studio = app.world_mut().resource_mut::<Studio>();
            studio.display_trace = Some(trace.clone());
            studio.prefs = Default::default();
            studio.persist_layout = false;
            studio.prefs.language = if self.0.lang == "zh" {
                crate::i18n::Language::Chinese
            } else {
                crate::i18n::Language::English
            };
            studio.prefs.show_static = !self.0.hide_static;
            studio.prefs.show_bounds = self.0.bounds;
            studio.playback_fps = self.0.fps;
            studio.recorded_timing = false;
        }
        app.insert_resource(Probe {
            config: self.0.clone(),
            trace,
            phase: Phase::Loading,
            tick: 0,
            phase_tick: 0,
            ui_passes: 0,
            scene: Stamp::default(),
            ui: Stamp::default(),
            final_submission: None,
            screenshot_pending: false,
            snapshots: Vec::new(),
            actions: Vec::new(),
            action_fixture: None,
            shutdown_at: None,
            adapter: serde_json::Value::Null,
        });
        app.add_systems(Update, begin_tick.before(crate::ui::prepare))
            .add_systems(
                Update,
                after_prepare
                    .after(crate::ui::prepare)
                    .before(crate::scene::sync_scene),
            )
            .add_systems(
                Update,
                after_sync
                    .after(crate::scene::sync_scene)
                    .before(crate::scene::update_camera),
            )
            .add_systems(
                Update,
                after_camera
                    .after(crate::scene::update_camera)
                    .before(crate::scene::draw_grid),
            )
            .add_systems(Update, after_gizmos.after(crate::scene::draw_grid))
            .add_systems(EguiPrimaryContextPass, before_ui.before(crate::ui::draw))
            .add_systems(EguiPrimaryContextPass, after_ui.after(crate::ui::draw))
            .add_systems(
                PostUpdate,
                before_egui_output
                    .after(EguiPostUpdateSet::EndPass)
                    .before(EguiPostUpdateSet::ProcessOutput),
            )
            .add_systems(
                PostUpdate,
                final_output
                    .after(EguiPostUpdateSet::PostProcessOutput)
                    .after(bevy::camera::visibility::VisibilitySystems::CheckVisibility)
                    .after(bevy::transform::TransformSystems::Propagate),
            )
            .add_systems(Last, drive);
        if self.0.ui_actions {
            actions::install(app);
        }
        app.sub_app_mut(RenderApp)
            .init_resource::<RenderProbe>()
            .add_systems(ExtractSchedule, extract_submission)
            .add_systems(
                Render,
                render_begin
                    .before(render_system)
                    .in_set(RenderSystems::Render),
            )
            .add_systems(
                Render,
                render_submitted
                    .after(render_system)
                    .before(RenderSystems::Cleanup),
            );
    }
}

fn stamp(studio: &Studio, tick: u64) -> Stamp {
    Stamp {
        frame: studio.frame,
        revision: studio.revision,
        request: studio.display_request,
        tick,
        analysis_ready: studio.analysis_ready(),
    }
}
fn begin_tick(mut probe: ResMut<Probe>, studio: Res<Studio>) {
    probe.tick += 1;
    probe.ui_passes = 0;
    probe
        .trace
        .event("prepare_begin", stamp(&studio, probe.tick), 0);
}
fn after_prepare(probe: Res<Probe>, studio: Res<Studio>) {
    let stamp = stamp(&studio, probe.tick);
    probe.trace.event("prepare_end", stamp, 0);
    probe.trace.event("sync_begin", stamp, 0);
}
fn after_sync(mut probe: ResMut<Probe>, studio: Res<Studio>, viewport: Res<Viewport>) {
    let (revision, shapes, actors) = viewport.display_probe_stats();
    if revision == studio.revision {
        probe.scene = stamp(&studio, probe.tick);
    }
    probe.trace.event("sync_end", probe.scene, shapes as u64);
    probe
        .trace
        .event("last_sync_actor_count", probe.scene, actors as u64);
    probe.trace.event("camera_begin", probe.scene, 0);
}
fn after_camera(probe: Res<Probe>) {
    probe.trace.event("camera_end", probe.scene, 0);
    probe.trace.event("gizmos_begin", probe.scene, 0);
}
fn after_gizmos(probe: Res<Probe>) {
    probe.trace.event("gizmos_end", probe.scene, 0);
}
fn before_ui(mut probe: ResMut<Probe>, studio: Res<Studio>) {
    probe.ui_passes += 1;
    probe
        .trace
        .event("ui_begin", stamp(&studio, probe.tick), probe.ui_passes);
}
fn after_ui(mut probe: ResMut<Probe>, studio: Res<Studio>) {
    probe.ui = stamp(&studio, probe.tick);
    probe.trace.event("ui_end", probe.ui, probe.ui_passes);
}
fn before_egui_output(probe: Res<Probe>) {
    probe
        .trace
        .event("egui_output_begin", probe.ui, probe.ui_passes);
}
fn final_output(mut probe: ResMut<Probe>) {
    probe
        .trace
        .event("egui_output_and_postupdate_end", probe.ui, probe.ui_passes);
    probe.final_submission =
        (!matches!(probe.phase, Phase::Drain | Phase::Shutdown)).then_some(Submission {
            serial: 0,
            tick: probe.tick,
            phase: probe.trace.phase(),
            ui_passes: probe.ui_passes,
            scene: probe.scene,
            ui: probe.ui,
        });
}
fn extract_submission(mut render: ResMut<RenderProbe>, probe: Extract<Res<Probe>>) {
    render.trace = Some(probe.trace.clone());
    render.submission = probe.final_submission.map(|mut submission| {
        render.serial += 1;
        submission.serial = render.serial;
        probe.trace.submission_event("extract", submission);
        submission
    });
}
fn render_begin(probe: Res<RenderProbe>) {
    if let (Some(trace), Some(submission)) = (&probe.trace, probe.submission) {
        trace.submission_event("render_cpu_begin", submission);
    }
}
fn render_submitted(probe: Res<RenderProbe>, queue: Res<RenderQueue>) {
    if let (Some(trace), Some(submission)) = (&probe.trace, probe.submission) {
        trace.submission_event("render_cpu_end", submission);
        if trace.submitted(submission) {
            let trace = trace.clone();
            queue.on_submitted_work_done(move || trace.completed(submission));
        }
    }
}

fn activate(studio: &mut Studio, panel: Panel) {
    if let Some(location) = studio.prefs.dock.find_tab(&panel) {
        studio.prefs.dock.set_active_tab(location);
    }
}

#[derive(bevy::ecs::system::SystemParam)]
struct DisplayWorld<'w, 's> {
    viewport: Res<'w, Viewport>,
    check: Res<'w, RenderCheck>,
    transforms: Query<
        'w,
        's,
        (
            &'static Transform,
            &'static GlobalTransform,
            &'static Visibility,
            &'static Mesh3d,
            &'static MeshMaterial3d<StandardMaterial>,
        ),
    >,
    meshes: Res<'w, Assets<Mesh>>,
    adapter: Option<Res<'w, RenderAdapterInfo>>,
    device: Option<Res<'w, RenderDevice>>,
}

fn drive(
    mut commands: Commands,
    mut probe: ResMut<Probe>,
    mut studio: ResMut<Studio>,
    display: DisplayWorld,
    mut exit: MessageWriter<AppExit>,
) {
    let DisplayWorld {
        viewport,
        check,
        transforms,
        meshes,
        adapter,
        device,
    } = display;
    if probe.trace.elapsed() > Duration::from_secs(180) && probe.phase != Phase::Shutdown {
        probe.trace.fail("continuous render check timed out");
    }
    if probe.adapter.is_null()
        && let (Some(adapter), Some(device)) = (adapter, device)
    {
        probe.adapter = serde_json::json!({"name": adapter.name, "backend": format!("{:?}", adapter.backend), "device_type": format!("{:?}", adapter.device_type), "features": format!("{:?}", device.features())});
    }
    if probe.trace.failure().is_some() && !matches!(probe.phase, Phase::Drain | Phase::Shutdown) {
        studio.pause_display();
        probe.phase = Phase::Drain;
        probe.trace.set_phase(6);
        probe.phase_tick = probe.tick;
    }
    let synced = probe.scene.revision == studio.revision
        && probe.ui.revision == studio.revision
        && probe.scene.frame == studio.frame
        && probe.ui.frame == studio.frame;
    match probe.phase {
        Phase::Loading
            if studio.recording.is_some() && studio.job.is_none() && studio.seeking.is_none() =>
        {
            if !studio.recording.as_ref().unwrap().is_disk_backed()
                || probe.config.last >= studio.count()
            {
                probe.trace.fail(
                    "probe requires disk-backed input containing the complete requested range",
                );
                return;
            }
            let target = if probe.config.ui_actions {
                studio.count() - 1
            } else {
                probe.config.first
            };
            studio.seek(target);
            probe.phase = Phase::Positioning;
        }
        Phase::Positioning
            if studio.seeking.is_none()
                && studio.frame
                    == if probe.config.ui_actions {
                        studio.count() - 1
                    } else {
                        probe.config.first
                    }
                && synced =>
        {
            studio.selected = viewport
                .objects
                .iter()
                .find(|object| object.dynamic)
                .map(|object| object.id);
            probe.phase = Phase::Warm;
            probe.phase_tick = probe.tick;
            probe.trace.set_phase(1);
        }
        Phase::Warm if probe.tick >= probe.phase_tick + 90 && synced => {
            if let Err(error) = validate(
                &mut probe,
                &studio,
                &viewport,
                &transforms,
                &meshes,
                "start",
                false,
            ) {
                probe.trace.fail(error);
                return;
            }
            if probe.config.ui_actions {
                probe.trace.set_phase(8);
                probe.phase = Phase::Actions;
                probe.phase_tick = probe.tick;
            } else {
                probe.trace.set_phase(2);
                probe.trace.set_stop(probe.config.last);
                studio.toggle_playback();
                probe.phase = Phase::Forward;
            }
        }
        Phase::Forward
            if studio.frame == probe.config.last
                && studio.playing == 0
                && studio.seeking.is_none() =>
        {
            probe.trace.set_phase(3);
            probe.phase = Phase::PauseForward;
            activate(&mut studio, Panel::Memory);
        }
        Phase::PauseForward
            if synced
                && studio.analysis_ready()
                && studio.seeking.is_none()
                && !probe.screenshot_pending =>
        {
            if probe
                .snapshots
                .iter()
                .any(|value| value["name"] == "forward")
            {
                activate(&mut studio, Panel::Timeline);
                probe.trace.set_phase(4);
                probe.trace.set_stop(probe.config.first);
                studio.toggle_reverse();
                probe.phase = Phase::Reverse;
            } else {
                if let Err(error) = validate(
                    &mut probe,
                    &studio,
                    &viewport,
                    &transforms,
                    &meshes,
                    "forward",
                    true,
                ) {
                    probe.trace.fail(error);
                    return;
                }
                screenshot(&mut commands, &mut probe, &check, "forward.png");
            }
        }
        Phase::Reverse
            if studio.frame == probe.config.first
                && studio.playing == 0
                && studio.seeking.is_none() =>
        {
            probe.trace.set_phase(5);
            probe.phase = Phase::PauseReverse;
            activate(&mut studio, Panel::Memory);
        }
        Phase::PauseReverse
            if synced
                && studio.analysis_ready()
                && studio.seeking.is_none()
                && !probe.screenshot_pending =>
        {
            if probe
                .snapshots
                .iter()
                .any(|value| value["name"] == "reverse")
            {
                probe.trace.set_phase(7);
                probe.trace.set_stop(usize::MAX);
                let middle = probe.config.first + (probe.config.last - probe.config.first) / 2;
                studio.seek(middle);
                probe.phase = Phase::Boundary;
            } else {
                if let Err(error) = validate(
                    &mut probe,
                    &studio,
                    &viewport,
                    &transforms,
                    &meshes,
                    "reverse",
                    true,
                ) {
                    probe.trace.fail(error);
                    return;
                }
                screenshot(&mut commands, &mut probe, &check, "reverse.png");
            }
        }
        Phase::Boundary if synced && studio.analysis_ready() && studio.seeking.is_none() => {
            let middle = probe.config.first + (probe.config.last - probe.config.first) / 2;
            if studio.frame != middle {
                probe.trace.fail("boundary frame was not loaded");
                return;
            }
            if let Err(error) = validate(
                &mut probe,
                &studio,
                &viewport,
                &transforms,
                &meshes,
                "boundary",
                true,
            ) {
                probe.trace.fail(error);
                return;
            }
            probe.trace.set_phase(6);
            probe.phase = Phase::Drain;
            probe.phase_tick = probe.tick;
        }
        Phase::Drain
            if probe.tick > probe.phase_tick + 3
                && (probe.trace.pending() == 0 || probe.tick > probe.phase_tick + 600) =>
        {
            if probe.trace.pending() != 0 {
                probe.trace.fail("GPU completion drain timed out");
            }
            probe.phase = Phase::Shutdown;
            probe.shutdown_at = Some(Instant::now());
        }
        Phase::Shutdown => {
            if studio.poll_shutdown() {
                let result = write_report(&probe);
                if let Err(error) = &result {
                    eprintln!("Display probe report failed: {error}");
                }
                exit.write(if result.is_ok() && probe.trace.failure().is_none() {
                    AppExit::Success
                } else {
                    AppExit::error()
                });
            } else if probe
                .shutdown_at
                .is_some_and(|started| started.elapsed() > Duration::from_secs(30))
            {
                probe.trace.fail("background shutdown timed out");
                let _ = write_report(&probe);
                exit.write(AppExit::error());
            }
        }
        _ => (),
    }
}

fn validate(
    probe: &mut Probe,
    studio: &Studio,
    viewport: &Viewport,
    transforms: &Query<(
        &Transform,
        &GlobalTransform,
        &Visibility,
        &Mesh3d,
        &MeshMaterial3d<StandardMaterial>,
    )>,
    meshes: &Assets<Mesh>,
    name: &str,
    analysis: bool,
) -> Result<(), String> {
    let recording = studio.recording.as_ref().ok_or("recording missing")?;
    let expected = recording
        .state_at(studio.frame)
        .map_err(|error| error.to_string())?;
    if expected != studio.state {
        return Err(format!("full SceneState differs at {}", studio.frame));
    }
    let entities = viewport.validate_display(studio, transforms, meshes)?;
    let analysis_hash = if analysis {
        let expected = recording
            .analysis_at(studio.frame)
            .map_err(|error| error.to_string())?;
        let a = postcard::to_stdvec(&expected).map_err(|error| error.to_string())?;
        let b = postcard::to_stdvec(&recording.analysis).map_err(|error| error.to_string())?;
        if a != b {
            return Err(format!("full Analysis differs at {}", studio.frame));
        }
        Some(blake3::hash(&b).to_hex().to_string())
    } else {
        None
    };
    probe.snapshots.push(serde_json::json!({"name":name,"frame":studio.frame,"revision":studio.revision,"objects":studio.state.objects.len(),"rendered_shapes":entities,"full_scene_state_equal":true,"analysis_postcard_hash":analysis_hash,"scope":"same-build independent state_at/full render reconstruction, outside measured playback"}));
    Ok(())
}

fn screenshot(commands: &mut Commands, probe: &mut Probe, check: &RenderCheck, name: &str) {
    let path = probe.config.output.join(name);
    probe.screenshot_pending = true;
    commands
        .spawn(Screenshot::image(check.target.clone()))
        .observe(
            move |capture: On<ScreenshotCaptured>, mut probe: ResMut<Probe>| {
                let saved = capture
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
                if let Err(error) = saved {
                    probe.trace.fail(format!("screenshot failed: {error}"));
                }
                probe.screenshot_pending = false;
            },
        );
}

fn write_report(probe: &Probe) -> Result<(), String> {
    let path = probe.config.output.join("trace.json");
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    let mut output = std::io::BufWriter::new(output);
    probe.trace.with_records(|records| serde_json::to_writer(&mut output, &serde_json::json!({
        "config":probe.config,"adapter":probe.adapter,"failure":probe.trace.failure(),"pending_gpu_callbacks":probe.trace.pending(),
        "snapshots":probe.snapshots,"actions":probe.actions,"action_fixture":probe.action_fixture,"records":records,
        "phases":{"0":"loading","1":"warmup","2":"forward","3":"pause_forward","4":"reverse","5":"pause_reverse","6":"drain","7":"boundary_oracle","8":"ui_actions"},
        "scope":"offscreen queue completion; render_return_observed is after Bevy render_system returns, not the exact queue.submit timestamp; observation-to-callback is only remaining queue/notification latency, not total queue time, monitor presentation or GPU execution time",
        "analysis_oracle":"full postcard at paused endpoints; same-build full reader, not independent SDK truth",
        "tick_minimum_ms":16,"trace_capacity":65_536
    })).map_err(|error| error.to_string()))?;
    std::io::Write::flush(&mut output).map_err(|error| error.to_string())
}
