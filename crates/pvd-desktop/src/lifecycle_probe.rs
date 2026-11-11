use crate::{scene::Viewport, studio::Studio};
use bevy::prelude::*;
use bevy_egui::EguiContexts;
use serde::Serialize;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Clone, Serialize)]
pub(crate) struct Config {
    captures: Vec<PathBuf>,
    output: PathBuf,
    rounds: usize,
}

impl Config {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        if !(4..=6).contains(&args.len()) {
            return Err("expected CAPTURE NEW_OUTPUT_DIRECTORY [ROUNDS [SECOND_CAPTURE]]".into());
        }
        let rounds = args
            .get(4)
            .map_or(Ok(20), |value| value.parse::<usize>())
            .map_err(|error| error.to_string())?;
        if !(2..=1000).contains(&rounds) {
            return Err("lifecycle check requires 2..1000 rounds".into());
        }
        let mut captures = Vec::new();
        for path in std::iter::once(&args[2]).chain(args.get(5)) {
            let path = std::fs::canonicalize(path).map_err(|error| error.to_string())?;
            if !path.is_file() {
                return Err("capture must be a file".into());
            }
            captures.push(path);
        }
        let output = PathBuf::from(&args[3]);
        std::fs::create_dir(&output)
            .map_err(|error| format!("output must be a new directory: {error}"))?;
        Ok(Self {
            captures,
            output: std::fs::canonicalize(output).map_err(|error| error.to_string())?,
            rounds,
        })
    }
}

pub(crate) struct Plugin(pub Config);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
enum Phase {
    Warm,
    Loading,
    Seeking,
    Closed,
    Cancelled,
    FailedOpen,
    Shutdown,
    Done,
}

#[derive(Clone, Copy, Serialize, PartialEq, Eq)]
struct AssetsCount {
    meshes: usize,
    materials: usize,
    images: usize,
    mesh_entities: usize,
}

#[derive(Serialize)]
struct Sample {
    elapsed_ms: u128,
    round: usize,
    phase: Phase,
    assets: AssetsCount,
    jobs: usize,
    objects: usize,
    frame: usize,
    cache_files: usize,
    cache_bytes: u64,
}

#[derive(Resource)]
struct Probe {
    config: Config,
    phase: Phase,
    started: Instant,
    phase_started: Instant,
    ticks: u32,
    round: usize,
    target_index: usize,
    targets: Vec<usize>,
    empty: Option<AssetsCount>,
    samples: Vec<Sample>,
    failure: Option<String>,
}

impl bevy::prelude::Plugin for Plugin {
    fn build(&self, app: &mut App) {
        {
            let mut studio = app.world_mut().resource_mut::<Studio>();
            studio.prefs = Default::default();
            studio.persist_layout = false;
            studio.prefs.history_directory =
                self.0.output.join("history").to_string_lossy().into_owned();
        }
        app.insert_resource(Probe {
            config: self.0.clone(),
            phase: Phase::Warm,
            started: Instant::now(),
            phase_started: Instant::now(),
            ticks: 0,
            round: 0,
            target_index: 0,
            targets: Vec::new(),
            empty: None,
            samples: Vec::new(),
            failure: None,
        })
        .add_systems(Last, drive);
    }
}

#[derive(bevy::ecs::system::SystemParam)]
struct Display<'w, 's> {
    viewport: Res<'w, Viewport>,
    meshes: Res<'w, Assets<Mesh>>,
    materials: Res<'w, Assets<StandardMaterial>>,
    images: Res<'w, Assets<Image>>,
    entities: Query<
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
}

impl Display<'_, '_> {
    fn counts(&self) -> AssetsCount {
        AssetsCount {
            meshes: self.meshes.len(),
            materials: self.materials.len(),
            images: self.images.len(),
            mesh_entities: self.entities.iter().count(),
        }
    }
}

impl Probe {
    fn transition(&mut self, phase: Phase) {
        self.phase = phase;
        self.phase_started = Instant::now();
        self.ticks = 0;
    }
    fn capture(&self) -> PathBuf {
        self.config.captures[self.round % self.config.captures.len()].clone()
    }
    fn sample(&mut self, studio: &Studio, display: &Display) -> Result<(), String> {
        let (cache_files, cache_bytes) = file_totals(&self.config.output.join("history"))?;
        self.samples.push(Sample {
            elapsed_ms: self.started.elapsed().as_millis(),
            round: self.round,
            phase: self.phase,
            assets: display.counts(),
            jobs: studio.background_jobs.active_count(),
            objects: studio.state.objects.len(),
            frame: studio.frame,
            cache_files,
            cache_bytes,
        });
        Ok(())
    }
    fn empty_ready(&mut self, studio: &Studio, display: &Display) -> Result<bool, String> {
        if studio.recording.is_some()
            || studio.job.is_some()
            || studio.seeking.is_some()
            || !studio.state.objects.is_empty()
            || display.viewport.display_probe_stats().1 != 0
        {
            return Ok(false);
        }
        if self.empty != Some(display.counts()) {
            return Ok(false);
        }
        let (files, bytes) = file_totals(&self.config.output.join("history"))?;
        if files != 0 || bytes != 0 {
            return Ok(false);
        }
        self.sample(studio, display)?;
        Ok(true)
    }
}

fn file_totals(root: &std::path::Path) -> Result<(usize, u64), String> {
    if !root.exists() {
        return Ok((0, 0));
    }
    let mut pending = vec![root.to_owned()];
    let (mut files, mut bytes, mut entries) = (0usize, 0u64, 0usize);
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            entries += 1;
            if entries > 100_000 {
                return Err("probe cache inventory exceeds its entry limit".into());
            }
            let kind = entry.file_type().map_err(|error| error.to_string())?;
            if kind.is_symlink() {
                return Err("probe cache contains a symbolic link".into());
            }
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                files += 1;
                bytes = bytes
                    .checked_add(entry.metadata().map_err(|error| error.to_string())?.len())
                    .ok_or("cache byte count overflow")?;
            }
        }
    }
    Ok((files, bytes))
}

fn drive(
    mut probe: ResMut<Probe>,
    mut studio: ResMut<Studio>,
    display: Display,
    mut contexts: EguiContexts,
    mut exit: MessageWriter<AppExit>,
) {
    if probe.phase == Phase::Done {
        return;
    }
    probe.ticks += 1;
    let result = (|| -> Result<(), String> {
        if probe.phase_started.elapsed() > Duration::from_secs(90) {
            probe.sample(&studio, &display)?;
            return Err(format!("lifecycle phase {:?} timed out", probe.phase));
        }
        if probe.phase == Phase::Shutdown {
            if studio.poll_shutdown() && probe.empty_ready(&studio, &display)? {
                if studio.background_jobs.active_count() != 0 {
                    return Err("background workers survived shutdown".into());
                }
                let report = serde_json::json!({"config":probe.config,"completed_rounds":probe.round,
                    "elapsed_ms":probe.started.elapsed().as_millis(),"failure":probe.failure,"samples":probe.samples,
                    "scope":"offscreen same-process load/seek/close/cancel/error recovery; mesh/material/image handles and owned history files; not OS input, VRAM byte usage or remote TCP"});
                let file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(probe.config.output.join("lifecycle.json"))
                    .map_err(|error| error.to_string())?;
                serde_json::to_writer_pretty(file, &report).map_err(|error| error.to_string())?;
                exit.write(if probe.failure.is_none() {
                    AppExit::Success
                } else {
                    AppExit::error()
                });
                probe.transition(Phase::Done);
            }
            return Ok(());
        }
        let context = contexts
            .ctx_mut()
            .map_err(|error| error.to_string())?
            .clone();
        let synced = display.viewport.display_probe_stats().0 == studio.revision;
        match probe.phase {
            Phase::Warm if probe.ticks >= 60 && studio.initialized && synced => {
                probe.empty = Some(display.counts());
                probe.sample(&studio, &display)?;
                studio.open(probe.capture(), context);
                probe.transition(Phase::Loading);
            }
            Phase::Loading if studio.job.is_none() && studio.seeking.is_none() && synced => {
                let recording = studio.recording.as_ref().ok_or("capture did not load")?;
                let first = recording.first_frame();
                if recording.frame_count().saturating_sub(first) < 2 {
                    return Err("capture needs at least two retained frames".into());
                }
                let last = recording.frame_count() - 1;
                probe.targets = vec![first, last, first + (last - first) / 2, first];
                probe.target_index = 0;
                studio.seek(probe.targets[0]);
                probe.transition(Phase::Seeking);
            }
            Phase::Seeking if studio.seeking.is_none() && synced && probe.ticks >= 3 => {
                let target = probe.targets[probe.target_index];
                if studio.frame != target {
                    return Err("seek displayed an unexpected frame".into());
                }
                let expected = studio
                    .recording
                    .as_ref()
                    .ok_or("recording missing")?
                    .state_at(target)
                    .map_err(|error| error.to_string())?;
                if expected != studio.state {
                    return Err("seek differs from complete state recovery".into());
                }
                display
                    .viewport
                    .validate_display(&studio, &display.entities, &display.meshes)?;
                probe.sample(&studio, &display)?;
                probe.target_index += 1;
                if probe.target_index < probe.targets.len() {
                    studio.seek(probe.targets[probe.target_index]);
                    probe.transition(Phase::Seeking);
                } else {
                    studio.close();
                    probe.transition(Phase::Closed);
                }
            }
            Phase::Closed if probe.ticks >= 45 && synced => {
                if !probe.empty_ready(&studio, &display)? {
                    return Ok(());
                }
                studio.open(probe.capture(), context);
                studio.close();
                probe.transition(Phase::Cancelled);
            }
            Phase::Cancelled if probe.ticks >= 45 && synced => {
                if !probe.empty_ready(&studio, &display)? {
                    return Ok(());
                }
                studio.open(
                    probe.config.output.join("missing-capture.pvdstream"),
                    context,
                );
                probe.transition(Phase::FailedOpen);
            }
            Phase::FailedOpen if probe.ticks >= 45 && studio.job.is_none() && synced => {
                if !probe.empty_ready(&studio, &display)? {
                    return Ok(());
                }
                probe.round += 1;
                if probe.round == probe.config.rounds {
                    probe.transition(Phase::Shutdown);
                } else {
                    studio.open(probe.capture(), context);
                    probe.transition(Phase::Loading);
                }
            }
            _ => {}
        }
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!("Lifecycle check: {error}");
        if probe.failure.is_none() {
            probe.failure = Some(error);
        }
        if probe.phase == Phase::Shutdown {
            exit.write(AppExit::error());
            probe.transition(Phase::Done);
        } else {
            probe.transition(Phase::Shutdown);
        }
    }
}
