use super::*;
use analysis_demand::DisplayedAnalysis;
use pvd_core::analysis::view::MemorySource;
use pvd_core::{frame_metrics::Metric, live::HistoryConfig};

fn text(value: &str) -> Vec<u8> {
    let mut bytes = (value.len() as u32 + 1).to_le_bytes().to_vec();
    bytes.extend(value.as_bytes());
    bytes.push(0);
    bytes
}

fn string(id: u32, value: &str) -> Vec<u8> {
    let mut bytes = vec![1];
    bytes.extend(text(value));
    bytes.extend(id.to_le_bytes());
    bytes
}

fn instance(class: u32, id: u64) -> Vec<u8> {
    let mut bytes = vec![6];
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(class.to_le_bytes());
    bytes.extend(id.to_le_bytes());
    bytes
}

fn property(id: u64, name: u32, datatype: u32, data: &[u8], count: u32) -> Vec<u8> {
    let mut bytes = vec![7];
    bytes.extend(id.to_le_bytes());
    for value in [name, 1, datatype, count, data.len() as u32] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.extend(data);
    bytes
}

fn group(events: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = (events.iter().map(Vec::len).sum::<usize>() as u32)
        .to_le_bytes()
        .to_vec();
    bytes.extend((events.len() as u32).to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    for event in events {
        bytes.extend(event);
    }
    bytes
}

fn frame_groups(first: usize, count: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    for frame in first..first + count {
        let mut profile = Vec::new();
        for (kind, timestamp) in [(1, frame as u64 * 100 + 10), (2, frame as u64 * 100 + 20)] {
            profile.extend([kind, 3, 1, 0]);
            profile.extend(7u32.to_le_bytes());
            profile.extend([9, 0, 0]);
            profile.extend(timestamp.to_le_bytes());
        }
        let memory = [2, 0, 20, 1, 2, 13, (frame % 200 + 40) as u8];
        let mut end = vec![19];
        end.extend(9u64.to_le_bytes());
        end.extend(6u32.to_le_bytes());
        end.extend(((frame as u64 + 1) * 100).to_le_bytes());
        bytes.extend(group(&[
            property(10, 4, 5, &profile, profile.len() as u32),
            property(11, 4, 5, &memory, memory.len() as u32),
            property(9, 8, 9, &(frame as u32).to_le_bytes(), 1),
            end,
        ]));
    }
    bytes
}

fn capture(frames: usize) -> Vec<u8> {
    let mut bytes = pvd_protocol::STREAM_MAGIC.to_le_bytes().to_vec();
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(1_000_000u64.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    let mut events: Vec<_> = [
        (1, "physx3"),
        (2, "PxProfileZone"),
        (3, "PxProfileMemoryEventBuffer"),
        (4, "events"),
        (5, "PvdU8"),
        (6, "frame"),
        (7, "PxScene"),
        (8, "Counter"),
        (9, "PvdU32"),
    ]
    .into_iter()
    .map(|(id, value)| string(id, value))
    .collect();
    events.extend([instance(2, 10), instance(3, 11), instance(7, 9)]);
    let mut zone = vec![24];
    zone.extend(10u64.to_le_bytes());
    zone.extend(text("Demand fixture"));
    events.push(zone);
    let mut event = vec![25];
    event.extend(10u64.to_le_bytes());
    event.extend(text("work"));
    event.extend(1u16.to_le_bytes());
    event.push(1);
    events.push(event);
    let mut memory = Vec::new();
    for (id, value) in [(1u32, "Allocation"), (2, "fixture.cpp")] {
        memory.extend(1u16.to_le_bytes());
        memory.extend(text(value));
        memory.extend(id.to_le_bytes());
    }
    events.push(property(11, 4, 5, &memory, memory.len() as u32));
    bytes.extend(group(&events));
    bytes.extend(frame_groups(0, frames));
    bytes
}

fn prefix_recording(bytes: &[u8], target: usize) -> Recording {
    use pvd_protocol::{Decoder, Event, EventGroup};
    let mut decoder = Decoder::default();
    let groups = decoder.feed(bytes).unwrap();
    decoder.finish().unwrap();
    let mut builder = pvd_core::RecordingBuilder::new(decoder.header().unwrap().clone());
    let mut strings = std::collections::BTreeMap::new();
    let mut frame = 0;
    for group in groups {
        for event in group.events {
            if let Event::StringHandle { handle, value } = &event {
                strings.insert(*handle, value.clone());
            }
            let boundary = matches!(&event, Event::EndSection { name, .. }
                if strings.get(name).is_some_and(|value| value == "frame"));
            builder
                .push(EventGroup {
                    stream_id: group.stream_id,
                    timestamp: group.timestamp,
                    offset: group.offset,
                    events: vec![event],
                })
                .unwrap();
            if boundary {
                if frame == target {
                    let recording = builder.snapshot().unwrap();
                    assert!(!recording.is_disk_backed());
                    assert_eq!(recording.frame_count(), target + 1);
                    return recording;
                }
                frame += 1;
            }
        }
    }
    panic!("raw stream has no completed frame {target}");
}

struct Case {
    studio: Studio,
    context: egui::Context,
    root: PathBuf,
    live: Option<LiveRecording>,
    disk: Option<Recording>,
    expected: Recording,
    bytes: Vec<u8>,
}

impl Case {
    fn new() -> Self {
        Self::with_bytes(capture(8), 8 << 20)
    }

    fn with_bytes(bytes: Vec<u8>, budget: u64) -> Self {
        let root = std::env::temp_dir().join(format!(
            "pvd-analysis-demand-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let expected = Recording::from_bytes(&bytes).unwrap();
        assert!(expected.analysis.profile_spans().next().is_some());
        let mut live = LiveRecording::with_history(HistoryConfig {
            directory: root.join("history"),
            segment_frames: if expected.frame_count() > 8 { 64 } else { 2 },
            disk_budget_bytes: budget,
            min_free_disk_bytes: 0,
            ..Default::default()
        })
        .unwrap();
        live.feed(&bytes).unwrap();
        let disk = live.snapshot().unwrap();
        assert!(disk.is_disk_backed());
        let mut studio = Studio::default();
        studio.persist_layout = false;
        studio.prefs.language = Language::English;
        studio.prefs.dock = DockState::new(vec![Panel::Viewport]);
        studio.connect_live_session(73);
        let last = disk.frame_count() - 1;
        studio.accept_live_snapshot(LiveSnapshot {
            source_frame: None,
            display_updated: true,
            session: 73,
            frame: last,
            update: pvd_core::live::SceneUpdate::full(&expected.state_at(last).unwrap()),
            recording: disk.clone(),
        });
        Self {
            studio,
            context: egui::Context::default(),
            root,
            live: Some(live),
            disk: Some(disk),
            expected,
            bytes,
        }
    }

    fn wait_seek(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while self.studio.seeking.is_some() {
            assert!(Instant::now() < deadline, "{:?}", self.studio.notices);
            self.studio.poll_seek(&self.context);
            std::thread::park_timeout(Duration::from_millis(1));
        }
    }

    fn scene(&mut self, frame: usize) {
        self.studio.pause_display();
        self.studio.analysis_demand = false;
        self.studio.seek(frame);
        self.wait_seek();
        assert_eq!(self.studio.frame, frame);
        assert_eq!(self.studio.state, self.expected.state_at(frame).unwrap());
        assert!(!self.studio.analysis_ready());
        assert!(
            self.studio
                .recording
                .as_ref()
                .unwrap()
                .analysis
                .profile_spans()
                .next()
                .is_none()
        );
    }

    fn assert_analysis(&self, frame: usize) {
        assert!(self.studio.analysis_ready(), "{:?}", self.studio.notices);
        assert_eq!(self.studio.frame, frame);
        let actual = &self.studio.recording.as_ref().unwrap().analysis;
        let expected = prefix_recording(&self.bytes, frame);
        assert!(expected.analysis.profile_spans().next().is_some());
        assert_eq!(
            postcard::to_stdvec(actual).unwrap(),
            postcard::to_stdvec(&expected.analysis).unwrap()
        );
        assert_eq!(self.studio.state, self.expected.state_at(frame).unwrap());
    }

    fn demand(&mut self, panel: Panel) {
        self.studio.prefs.dock = DockState::new(vec![panel]);
        self.studio.update_analysis_demand();
    }

    fn complete_source(&mut self) {
        self.studio.live_connected = false;
        self.studio.live_recording = None;
        self.studio.follow_live = false;
        self.studio.bytes = Some(Arc::new(self.bytes.clone()));
        self.studio.source_identity = Some(crate::clip_store::Identity::from_digest(
            blake3::hash(&self.bytes),
            self.bytes.len() as u64,
            self.expected.frame_count(),
        ));
        assert!(self.studio.complete_profile_source().is_some());
    }

    fn stop_worker(&mut self) {
        self.studio.seek_worker = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.studio.background_jobs.active_count() != 0 {
            assert!(Instant::now() < deadline, "seek cleanup timed out");
            std::thread::park_timeout(Duration::from_millis(1));
        }
        self.studio.poll_seek(&self.context);
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.studio.poll_shutdown() {
            assert!(Instant::now() < deadline, "case shutdown timed out");
            std::thread::park_timeout(Duration::from_millis(1));
        }
        self.disk = None;
        self.live = None;
        let history = self.root.join("history");
        if history.exists() {
            let remaining: Vec<_> = std::fs::read_dir(&history)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
            assert!(remaining.is_empty(), "history owners leaked: {remaining:?}");
            std::fs::remove_dir(history).unwrap();
        }
        let export = self.root.join("all-spans.csv");
        if export.exists() {
            std::fs::remove_file(export).unwrap();
        }
        std::fs::remove_dir(&self.root).unwrap();
    }
}

#[test]
fn disk_scene_only_memory_and_performance_upgrade_to_nonempty_analysis() {
    for panel in [Panel::Memory, Panel::Performance] {
        let mut case = Case::new();
        assert!(
            case.expected
                .analysis
                .memory_snapshot()
                .summary()
                .allocations
                >= 8
        );
        case.scene(3);
        let state = case.studio.state.clone();
        case.demand(panel);
        assert_eq!(case.studio.seeking, Some(3));
        case.wait_seek();
        case.assert_analysis(3);
        assert_eq!(case.studio.state, state);
        assert!(
            case.studio
                .recording
                .as_ref()
                .unwrap()
                .analysis
                .memory_snapshot()
                .summary()
                .allocations
                > 0
        );
    }
}

fn preparation_app(studio: Studio, context: &egui::Context) -> bevy::prelude::App {
    use bevy::prelude::*;
    use bevy_egui::{EguiContext, EguiUserTextures, PrimaryEguiContext};

    let mut app = App::new();
    app.insert_resource(studio)
        .init_resource::<crate::navmesh::NavMeshOverlay>()
        .init_resource::<crate::scene::Viewport>()
        .init_resource::<Assets<Mesh>>()
        .init_resource::<Assets<StandardMaterial>>()
        .init_resource::<Assets<Image>>()
        .init_resource::<EguiUserTextures>()
        .add_systems(Update, crate::ui::prepare.before(crate::scene::sync_scene))
        .add_systems(Update, crate::scene::sync_scene);
    let mut entity = app.world_mut().spawn(PrimaryEguiContext);
    *entity.get_mut::<EguiContext>().unwrap().get_mut() = context.clone();
    app
}

fn run_primary_ui(app: &mut bevy::prelude::App, context: &egui::Context) {
    use bevy::ecs::system::RunSystemOnce;
    let _ = context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1440.0, 960.0),
            )),
            ..Default::default()
        },
        |_| {
            let result: bevy::prelude::Result =
                app.world_mut().run_system_once(crate::ui::draw).unwrap();
            result.unwrap();
        },
    );
}

#[test]
fn ui_prepare_accepts_completed_seek_before_scene_sync_and_ui_does_not_poll() {
    let mut case = Case::new();
    case.scene(2);
    case.studio.initialized = true;
    let revision = case.studio.revision;
    case.studio.seek(3);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !case.studio.seek_worker.as_ref().unwrap().has_completed() {
        assert!(Instant::now() < deadline);
        std::thread::park_timeout(Duration::from_millis(1));
    }
    let mut app = preparation_app(std::mem::take(&mut case.studio), &case.context);
    for _ in 0..3 {
        run_primary_ui(&mut app, &case.context);
        let studio = app.world().resource::<Studio>();
        assert_eq!(studio.frame, 2);
        assert_eq!(studio.revision, revision);
        assert_eq!(studio.seeking, Some(3));
    }
    app.update();
    let studio = app.world().resource::<Studio>();
    assert_eq!(studio.frame, 3);
    assert_eq!(studio.state, case.expected.state_at(3).unwrap());
    assert!(studio.seeking.is_none());
    assert!(!studio.analysis_ready());
    assert_eq!(
        app.world()
            .resource::<crate::scene::Viewport>()
            .display_probe_stats()
            .0,
        studio.revision
    );
    let accepted_revision = studio.revision;
    for _ in 0..3 {
        run_primary_ui(&mut app, &case.context);
        assert_eq!(app.world().resource::<Studio>().revision, accepted_revision);
    }
    case.studio = app.world_mut().remove_resource::<Studio>().unwrap();
}

#[test]
fn ui_prepare_registers_background_completion_wake_before_any_ui_pass() {
    use std::sync::atomic::AtomicUsize;
    let mut case = Case::new();
    case.studio.initialized = true;
    let mut app = preparation_app(std::mem::take(&mut case.studio), &case.context);
    app.update();
    for _ in 0..3 {
        let _ = case.context.run(egui::RawInput::default(), |_| {});
    }
    let wakes = Arc::new(AtomicUsize::new(0));
    let count = wakes.clone();
    case.context.set_request_repaint_callback(move |request| {
        if request.delay.is_zero() {
            count.fetch_add(1, Ordering::Relaxed);
        }
    });
    app.world()
        .resource::<Studio>()
        .background_jobs
        .spawn(
            "ui-prepare-wake-test",
            Arc::new(AtomicBool::new(false)),
            || {},
        )
        .unwrap()
        .join()
        .unwrap();
    assert!(wakes.load(Ordering::Relaxed) > 0);
    case.studio = app.world_mut().remove_resource::<Studio>().unwrap();
}

#[test]
fn ui_prepare_skips_a_removed_context_but_rejects_multiple_primary_contexts() {
    use bevy::{ecs::system::RunSystemOnce, prelude::*};
    use bevy_egui::PrimaryEguiContext;
    let mut case = Case::new();
    let mut app = preparation_app(std::mem::take(&mut case.studio), &case.context);
    let entity = app
        .world_mut()
        .query_filtered::<Entity, With<PrimaryEguiContext>>()
        .single(app.world())
        .unwrap();
    app.world_mut().despawn(entity);
    app.update();
    assert!(!app.world().resource::<Studio>().initialized);
    assert!(app.world().resource::<Studio>().job.is_none());
    app.world_mut().spawn(PrimaryEguiContext);
    app.update();
    assert!(app.world().resource::<Studio>().initialized);
    app.world_mut().spawn(PrimaryEguiContext);
    let result: bevy::prelude::Result =
        app.world_mut().run_system_once(crate::ui::prepare).unwrap();
    assert!(result.is_err());
    case.studio = app.world_mut().remove_resource::<Studio>().unwrap();
}

#[test]
fn ui_prepare_opens_initial_path_once_without_a_ui_pass() {
    let mut case = Case::new();
    let input = case.root.join("initial.pvdstream");
    std::fs::write(&input, &case.bytes).unwrap();
    case.studio.clear_recording();
    case.studio.initialized = false;
    case.studio.path_input = input.to_string_lossy().into_owned();
    let mut app = preparation_app(std::mem::take(&mut case.studio), &case.context);
    app.update();
    assert!(app.world().resource::<Studio>().initialized);
    app.world_mut().resource_mut::<Studio>().path_input = "must-not-reopen.pvdstream".into();
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.world().resource::<Studio>().job.is_some() {
        assert!(Instant::now() < deadline);
        std::thread::park_timeout(Duration::from_millis(1));
        app.update();
    }
    let studio = app.world().resource::<Studio>();
    assert_eq!(studio.count(), 8, "{:?}", studio.notices);
    assert_eq!(studio.frame, 0);
    assert_eq!(studio.state, case.expected.state_at(0).unwrap());
    assert_eq!(
        app.world()
            .resource::<crate::scene::Viewport>()
            .display_probe_stats()
            .0,
        studio.revision
    );
    app.world_mut().resource_mut::<Studio>().seek(3);
    app.update();
    assert_eq!(app.world().resource::<Studio>().frame, 3);
    assert!(app.world().resource::<Studio>().job.is_none());
    case.studio = app.world_mut().remove_resource::<Studio>().unwrap();
    std::fs::remove_file(input).unwrap();
}

#[test]
fn display_probe_endpoint_pause_does_not_reset_the_paused_analysis_upgrade() {
    let mut case = Case::new();
    case.scene(2);
    let trace = crate::display_probe::Trace::default();
    trace.set_stop(3);
    case.studio.display_trace = Some(trace);
    case.studio.playing = 1;
    case.studio
        .seek_with_analysis(3, playback_recovery::Intent::Playback, false, None);
    case.wait_seek();
    assert_eq!(case.studio.playing, 0);
    assert_eq!(case.studio.frame, 3);
    assert!(!case.studio.analysis_ready());
    case.demand(Panel::Memory);
    let requested = case.studio.seek_worker.as_ref().unwrap().generation();
    case.wait_seek();
    case.assert_analysis(3);
    assert_eq!(
        case.studio.seek_worker.as_ref().unwrap().generation(),
        requested
    );
}

#[test]
fn analysis_demand_during_unpublished_scene_seek_upgrades_the_same_frame() {
    let mut case = Case::new();
    case.studio.analysis_demand = false;
    case.studio.seek(3);
    let generation = case.studio.seek_worker.as_ref().unwrap().generation();
    for _ in 0..8 {
        case.demand(Panel::Memory);
        assert_eq!(
            case.studio.seek_worker.as_ref().unwrap().generation(),
            generation
        );
    }
    case.wait_seek();
    assert!(!case.studio.analysis_ready());
    assert_eq!(case.studio.frame, 3);
    case.studio.update_analysis_demand();
    assert!(case.studio.seek_worker.as_ref().unwrap().generation() > generation);
    case.wait_seek();
    case.assert_analysis(3);
}

#[test]
fn unpublished_analysis_upgrade_cannot_override_new_frame_live_or_clear() {
    for transition in 0..3 {
        let mut case = Case::new();
        case.scene(3);
        case.demand(Panel::Memory);
        assert_eq!(case.studio.seeking, Some(3));
        let generation = case.studio.seek_worker.as_ref().unwrap().generation();
        match transition {
            0 => {
                case.studio.analysis_demand = false;
                case.studio.seek(4);
                case.wait_seek();
                assert_eq!(case.studio.frame, 4);
                assert!(!case.studio.analysis_ready());
                assert_eq!(case.studio.state, case.expected.state_at(4).unwrap());
            }
            1 => {
                case.studio.return_to_live();
                assert!(case.studio.follow_live);
                case.assert_analysis(7);
            }
            _ => {
                case.studio.clear_recording();
                assert!(case.studio.recording.is_none());
                assert!(case.studio.state.objects.is_empty());
            }
        }
        assert!(case.studio.seek_worker.as_ref().unwrap().generation() > generation);
        let frame = case.studio.frame;
        let state = case.studio.state.clone();
        case.stop_worker();
        assert_eq!(case.studio.frame, frame);
        assert_eq!(case.studio.state, state);
        assert!(case.studio.seeking.is_none());
    }
}

#[test]
fn visible_analysis_panels_defer_during_both_playback_directions_then_upgrade_on_pause() {
    for direction in [-1, 1] {
        for panel in [Panel::Memory, Panel::Performance] {
            let mut case = Case::new();
            case.scene(3);
            case.studio.recorded_timing = false;
            case.studio.playback_fps = 1.0;
            case.studio.playing = direction;
            case.demand(panel);
            assert!(!case.studio.analysis_demand);
            assert!(case.studio.seeking.is_none());
            let output = draw(&mut case, vec![], |ui, studio| match panel {
                Panel::Memory => crate::memory::show(ui, studio),
                Panel::Performance => crate::profile::show(ui, studio),
                _ => unreachable!(),
            });
            assert!(labels(&output).iter().any(|(text, _)| {
                text == "Pause playback to load analysis for the displayed frame."
            }));
            assert!(case.studio.seeking.is_none());
            case.studio.last_step = Instant::now() - Duration::from_millis(1100);
            case.studio.poll(&case.context);
            let target = (3i32 + direction) as usize;
            assert_eq!(case.studio.seeking, Some(target));
            case.wait_seek();
            assert_eq!(case.studio.frame, target);
            assert!(!case.studio.analysis_ready());
            case.studio.update_analysis_demand();
            assert!(case.studio.seeking.is_none());
            case.studio.pause_display();
            case.studio.update_analysis_demand();
            assert_eq!(case.studio.seeking, Some(target));
            case.wait_seek();
            case.assert_analysis(target);
        }
    }
}

#[test]
fn stopped_playback_boundaries_upgrade_only_after_ui_demand_refresh() {
    for (frame, direction) in [(0, -1), (7, 1)] {
        let mut case = Case::new();
        case.scene(frame);
        case.studio.live_connected = false;
        case.studio.playing = direction;
        case.demand(Panel::Memory);
        assert!(!case.studio.analysis_demand);
        case.studio.poll(&case.context);
        assert_eq!(case.studio.playing, 0);
        assert!(!case.studio.analysis_ready());
        assert!(case.studio.seeking.is_none());
        case.studio.update_analysis_demand();
        assert_eq!(case.studio.seeking, Some(frame));
        case.wait_seek();
        case.assert_analysis(frame);
    }
}

fn draw(
    case: &mut Case,
    events: Vec<egui::Event>,
    panel: impl FnMut(&mut egui::Ui, &mut Studio),
) -> egui::FullOutput {
    draw_studio(&case.context, &mut case.studio, events, panel)
}

pub(super) fn draw_studio(
    context: &egui::Context,
    studio: &mut Studio,
    events: Vec<egui::Event>,
    mut panel: impl FnMut(&mut egui::Ui, &mut Studio),
) -> egui::FullOutput {
    context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 600.0),
            )),
            events,
            ..Default::default()
        },
        |context| {
            egui::CentralPanel::default().show(context, |ui| panel(ui, studio));
        },
    )
}

pub(super) fn labels(output: &egui::FullOutput) -> Vec<(String, egui::Rect)> {
    fn collect(shape: &egui::Shape, labels: &mut Vec<(String, egui::Rect)>) {
        match shape {
            egui::Shape::Text(text) => labels.push((
                text.galley.text().to_owned(),
                text.galley.rect.translate(text.pos.to_vec2()),
            )),
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    collect(shape, labels);
                }
            }
            _ => (),
        }
    }
    let mut labels = Vec::new();
    for shape in &output.shapes {
        collect(&shape.shape, &mut labels);
    }
    labels
}

#[test]
fn retry_button_requests_nonempty_analysis_for_a_retained_frame() {
    let mut case = Case::new();
    case.scene(3);
    case.studio.displayed_analysis = DisplayedAnalysis::Failed {
        frame: 3,
        message: Message::new("retry fixture", "retry fixture"),
    };
    let state = case.studio.state.clone();
    let generation = case.studio.seek_worker.as_ref().unwrap().generation();
    for _ in 0..3 {
        case.demand(Panel::Memory);
        case.studio.poll_seek(&case.context);
    }
    assert_eq!(
        case.studio.seek_worker.as_ref().unwrap().generation(),
        generation
    );
    assert!(case.studio.seeking.is_none());
    let show = |ui: &mut egui::Ui, studio: &mut Studio| {
        assert!(!studio.ensure_current_analysis(ui));
    };
    let output = draw(&mut case, vec![], show);
    assert!(case.studio.seeking.is_none());
    let retry = labels(&output)
        .into_iter()
        .find(|(text, _)| text == "Retry")
        .unwrap()
        .1
        .center();
    for pressed in [true, false] {
        draw(
            &mut case,
            vec![
                egui::Event::PointerMoved(retry),
                egui::Event::PointerButton {
                    pos: retry,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            show,
        );
    }
    assert_eq!(case.studio.seeking, Some(3));
    assert_eq!(case.studio.frame, 3);
    assert_eq!(case.studio.state, state);
    case.wait_seek();
    case.assert_analysis(3);
}

#[test]
fn successive_expired_analysis_frames_wait_for_explicit_latest_frame_clicks() {
    let mut case = Case::with_bytes(capture(8), 512 << 10);
    let mut held = 7;
    for first in [8, 264] {
        case.scene(held);
        let state = case.studio.state.clone();
        let more = frame_groups(first, 256);
        case.live.as_mut().unwrap().feed(&more).unwrap();
        case.bytes.extend(more);
        case.expected = Recording::from_bytes(&case.bytes).unwrap();
        let newest = case.live.as_ref().unwrap().snapshot().unwrap();
        assert!(!newest.is_frame_retained(held));
        let latest = newest
            .frames
            .iter()
            .rev()
            .find(|frame| newest.is_frame_retained(frame.index))
            .unwrap()
            .index;
        assert!(latest > held);
        case.studio.live_recording = Some(Arc::new(newest));
        case.demand(Panel::Memory);
        assert!(matches!(
            case.studio.displayed_analysis,
            DisplayedAnalysis::Failed { frame, .. } if frame == held
        ));
        let generation = case.studio.seek_worker.as_ref().unwrap().generation();
        let show = |ui: &mut egui::Ui, studio: &mut Studio| {
            assert!(!studio.ensure_current_analysis(ui));
        };
        for _ in 0..3 {
            case.studio.update_analysis_demand();
            case.studio.poll_seek(&case.context);
            draw(&mut case, vec![], show);
            assert!(case.studio.seeking.is_none());
            assert!(!case.studio.analysis_ready());
            assert!(!case.studio.follow_live);
            assert_eq!(case.studio.playing, 0);
            assert_eq!(case.studio.frame, held);
            assert_eq!(case.studio.state, state);
            assert_eq!(
                case.studio.seek_worker.as_ref().unwrap().generation(),
                generation
            );
        }
        let output = draw(&mut case, vec![], show);
        let button = labels(&output)
            .into_iter()
            .find(|(text, _)| text == "Load latest retained frame")
            .unwrap()
            .1
            .center();
        for pressed in [true, false] {
            draw(
                &mut case,
                vec![
                    egui::Event::PointerMoved(button),
                    egui::Event::PointerButton {
                        pos: button,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                show,
            );
        }
        assert_eq!(case.studio.seeking, Some(latest));
        assert_eq!(case.studio.frame, held);
        assert_eq!(case.studio.state, state);
        case.wait_seek();
        case.assert_analysis(latest);
        assert!(!case.studio.follow_live);
        assert_eq!(case.studio.playing, 0);
        held = latest;
    }
}

#[test]
fn first_playback_wrap_defers_analysis_even_with_stale_paused_panel_demand() {
    for (initial, direction, target) in [(7, 1, 0), (0, -1, 7)] {
        let mut case = Case::new();
        case.scene(initial);
        case.complete_source();
        case.demand(Panel::Memory);
        case.wait_seek();
        case.assert_analysis(initial);
        assert!(case.studio.analysis_demand);
        if direction == 1 {
            case.studio.toggle_playback();
        } else {
            case.studio.toggle_reverse();
        }
        assert_eq!(case.studio.playing, direction);
        assert!(case.studio.analysis_demand);
        assert_eq!(case.studio.seeking, Some(target));
        case.wait_seek();
        assert_eq!(case.studio.frame, target);
        assert_eq!(case.studio.state, case.expected.state_at(target).unwrap());
        assert!(!case.studio.analysis_ready());
        assert!(matches!(
            case.studio.displayed_analysis,
            DisplayedAnalysis::Deferred
        ));
        assert!(
            case.studio
                .recording
                .as_ref()
                .unwrap()
                .analysis
                .profile_spans()
                .next()
                .is_none()
        );
        assert_eq!(case.studio.playing, direction);
    }
}

#[test]
fn explicit_analysis_request_during_playback_preserves_full_seek_semantics() {
    let mut case = Case::new();
    case.scene(2);
    case.studio.playing = 1;
    case.studio
        .seek_with_analysis(3, playback_recovery::Intent::Playback, true, None);
    assert_eq!(case.studio.seeking, Some(3));
    case.wait_seek();
    case.assert_analysis(3);
    assert!(matches!(
        case.studio.displayed_analysis,
        DisplayedAnalysis::Frame(3)
    ));
    assert_eq!(case.studio.playing, 1);
}

#[test]
fn expired_scene_analysis_reports_failure_and_explicitly_loads_retained_history() {
    let mut case = Case::with_bytes(capture(8), 512 << 10);
    case.scene(7);
    let state = case.studio.state.clone();
    let more = frame_groups(8, 256);
    case.live.as_mut().unwrap().feed(&more).unwrap();
    let newest = case.live.as_ref().unwrap().snapshot().unwrap();
    assert!(!case.disk.as_ref().unwrap().is_frame_retained(7));
    let last = newest
        .frames
        .iter()
        .rev()
        .find(|frame| newest.is_frame_retained(frame.index))
        .unwrap()
        .index;
    assert!(last > 7);
    case.studio.live_recording = Some(Arc::new(newest));
    case.demand(Panel::Memory);
    assert!(matches!(
        case.studio.displayed_analysis,
        DisplayedAnalysis::Failed { frame: 7, .. }
    ));
    assert!(case.studio.seeking.is_none());
    let show = |ui: &mut egui::Ui, studio: &mut Studio| {
        assert!(!studio.ensure_current_analysis(ui));
    };
    let output = draw(&mut case, vec![], show);
    let retry = labels(&output)
        .into_iter()
        .find(|(text, _)| text == "Load latest retained frame")
        .unwrap()
        .1
        .center();
    for pressed in [true, false] {
        draw(
            &mut case,
            vec![
                egui::Event::PointerMoved(retry),
                egui::Event::PointerButton {
                    pos: retry,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            show,
        );
    }
    assert_eq!(case.studio.seeking, Some(last));
    assert_eq!(case.studio.frame, 7);
    assert_eq!(case.studio.state, state);
    case.bytes.extend(more);
    case.expected = Recording::from_bytes(&case.bytes).unwrap();
    case.wait_seek();
    case.assert_analysis(last);
}

#[test]
fn complete_source_queries_exports_and_owner_survive_scene_only_seeks() {
    let mut case = Case::new();
    case.scene(3);
    case.complete_source();
    let owner = case.studio.profile_cpu_index.clone();
    let epoch = case.studio.profile_view_epoch;
    case.scene(4);
    assert!(Arc::ptr_eq(&owner, &case.studio.profile_cpu_index));
    assert_eq!(epoch, case.studio.profile_view_epoch);
    case.studio.displayed_analysis = DisplayedAnalysis::Failed {
        frame: 4,
        message: Message::new("fixture analysis unavailable", "fixture"),
    };
    case.demand(Panel::Performance);
    assert!(!case.studio.analysis_demand);
    assert!(case.studio.seeking.is_none());
    let source = case.studio.complete_profile_source().unwrap();
    let cancel = AtomicBool::new(false);
    let (catalog, _) = source.catalog_page(None, 32, &cancel, |_| true).unwrap();
    assert!(!catalog.entries.is_empty());
    let overview = source.thread_page(None, 32, &cancel, |_| true).unwrap();
    let window = source
        .thread_window_streaming(
            &overview.identity,
            pvd_core::profile_timeline::WindowRequest {
                ticks: overview.completed_tick_range.unwrap(),
                thread: None,
                context: None,
                limits: Default::default(),
            },
            &cancel,
            |_| true,
        )
        .unwrap();
    assert_eq!(
        format!("{:?}", window.spans),
        format!(
            "{:?}",
            case.expected.analysis.profile_spans().collect::<Vec<_>>()
        )
    );
    case.studio.profile_events_view = true;
    let output = draw(&mut case, vec![], crate::profile::show);
    let shown = labels(&output);
    assert!(shown.iter().any(|(text, _)| text == "Event curves"));
    assert!(!shown.iter().any(|(text, _)| text == "Retry"));
    let output = draw(&mut case, vec![], crate::memory::show);
    assert!(
        labels(&output)
            .iter()
            .any(|(text, _)| text == "Export full recording CSV")
    );
    assert!(case.studio.can_export_analysis());
    let output_path = case.root.join("all-spans.csv");
    case.studio.analysis_export_path = output_path.to_string_lossy().into_owned();
    case.studio.export_analysis(
        pvd_core::analysis_export::AnalysisCsvKind::Profile,
        case.context.clone(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while case.studio.recording_export.is_some() {
        assert!(Instant::now() < deadline, "{:?}", case.studio.notices);
        case.studio.poll(&case.context);
        std::thread::park_timeout(Duration::from_millis(1));
    }
    assert_eq!(
        std::fs::read_to_string(&output_path)
            .unwrap()
            .lines()
            .count(),
        9
    );
    assert!(!case.studio.analysis_ready());
    case.studio.clear_recording();
    assert!(!Arc::ptr_eq(&owner, &case.studio.profile_cpu_index));
    assert_ne!(epoch, case.studio.profile_view_epoch);
}

fn curve_spec(source: crate::curve::Source) -> crate::curve::Spec {
    crate::curve::Spec {
        source,
        object: 9,
        generation: 0,
        property: "Counter".into(),
        component: 0,
        component_name: "value".into(),
    }
}

#[test]
fn pending_property_and_frame_interval_bypass_an_analysis_dependent_curve() {
    let mut case = Case::new();
    case.scene(3);
    let delayed = case
        .studio
        .curves
        .add(curve_spec(crate::curve::Source::Performance(
            Metric::Collision,
        )))
        .unwrap();
    let property = case
        .studio
        .curves
        .add(curve_spec(crate::curve::Source::Property))
        .unwrap();
    let interval = case
        .studio
        .curves
        .add(curve_spec(crate::curve::Source::Performance(
            Metric::FrameInterval,
        )))
        .unwrap();
    for (id, is_property) in [(property, true), (interval, false)] {
        case.studio.start_curve_job(case.context.clone());
        assert_eq!(case.studio.curve_job.as_ref().unwrap().id, id);
        let deadline = Instant::now() + Duration::from_secs(10);
        let result = loop {
            if let Some(result) = case.studio.curve_job.as_ref().unwrap().result() {
                break result.unwrap();
            }
            assert!(Instant::now() < deadline);
            std::thread::park_timeout(Duration::from_millis(1));
        };
        assert!(result.samples.iter().any(|(_, value)| value.is_some()));
        if is_property {
            assert_eq!(result.samples.last(), Some(&(7, Some(7.0))));
        }
        case.studio.curve_job = None;
        assert!(case.studio.curves.pending.contains(&delayed));
        assert!(!case.studio.analysis_ready());
    }
    case.wait_seek();
    case.assert_analysis(3);
    case.studio.start_curve_job(case.context.clone());
    assert_eq!(case.studio.curve_job.as_ref().unwrap().id, delayed);
}

#[test]
#[ignore = "requires PhysX 3.4 profile captures; set PVD_PROFILE_STREAM_ROOT"]
fn official_sdk_disk_scene_then_analysis_matches_loaded_capture() {
    let root = PathBuf::from(
        std::env::var_os("PVD_PROFILE_STREAM_ROOT").expect("PVD_PROFILE_STREAM_ROOT"),
    );
    for name in [
        "physx-3.4-profile-metrics.pvdstream",
        "physx-3.4-profile-events.pvdstream",
    ] {
        let bytes = std::fs::read(root.join("captures").join(name)).unwrap();
        let mut case = Case::with_bytes(bytes, 256 << 20);
        let last = case.expected.frame_count() - 1;
        for (frame, panel) in [
            (last / 2, Panel::Performance),
            (last, Panel::Memory),
            (1, Panel::Performance),
        ] {
            case.scene(frame);
            case.demand(panel);
            case.wait_seek();
            case.assert_analysis(frame);
        }
    }
}

fn queue_opened_disk_recording(case: &mut Case, recording: Recording) {
    assert!(recording.is_disk_backed());
    case.studio.clear_recording();
    case.studio.prefs.dock = DockState::new(vec![Panel::Memory]);
    case.studio.update_analysis_demand();
    assert!(case.studio.analysis_demand);
    let (sender, receiver) = mpsc::channel();
    sender
        .send(Ok(LoadedCapture {
            recording,
            bytes: None,
            source_path: None,
            source_identity: None,
            clips: None,
        }))
        .unwrap();
    case.studio.job = Some(LoadJob {
        result: Mutex::new(receiver),
        cancelled: Arc::new(AtomicBool::new(false)),
        progress: Arc::new(AtomicU64::new(case.bytes.len() as u64)),
        label: "opened disk fixture".into(),
        started: Instant::now(),
        live: false,
        final_snapshot: true,
    });
    case.studio.poll(&case.context);
    assert!(case.studio.job.is_none());
    assert_eq!(case.studio.frame, 0);
    assert_eq!(case.studio.seeking, Some(0));
}

#[test]
fn opening_disk_capture_defers_eof_analysis_until_initial_seek_is_accepted() {
    let mut case = Case::new();
    let recording = case.live.take().unwrap().finish().unwrap();
    let expected_zero = prefix_recording(&case.bytes, 0);
    let eof_spans = recording.analysis.profile_spans().len();
    assert!(eof_spans > expected_zero.analysis.profile_spans().len());
    assert!(
        expected_zero
            .analysis
            .memory_snapshot()
            .summary()
            .allocations
            > 0
    );
    queue_opened_disk_recording(&mut case, recording);
    assert_eq!(
        case.studio
            .recording
            .as_ref()
            .unwrap()
            .analysis
            .profile_spans()
            .len(),
        eof_spans
    );
    assert!(!case.studio.analysis_ready());
    assert!(matches!(
        case.studio.displayed_analysis,
        DisplayedAnalysis::Deferred
    ));
    let output = draw(&mut case, vec![], |ui, studio| {
        assert!(!studio.ensure_current_analysis(ui));
    });
    assert!(
        labels(&output)
            .iter()
            .any(|(text, _)| { text == "Loading analysis for the displayed frame…" })
    );
    case.wait_seek();
    assert!(matches!(
        case.studio.displayed_analysis,
        DisplayedAnalysis::Frame(0)
    ));
    case.assert_analysis(0);
    assert_eq!(
        case.studio
            .recording
            .as_ref()
            .unwrap()
            .analysis
            .profile_spans()
            .len(),
        expected_zero.analysis.profile_spans().len()
    );
}

#[test]
fn opening_disk_capture_failed_initial_seek_is_unready_and_retryable() {
    use std::io::{Read, Seek, SeekFrom, Write};

    let mut case = Case::new();
    let recording = case.live.take().unwrap().finish().unwrap();
    let directories: Vec<_> = std::fs::read_dir(case.root.join("history"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect();
    assert_eq!(directories.len(), 1);
    let first_segment = directories[0].join("00000000.pvdhistory");
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&first_segment)
        .unwrap();
    let mut header = [0; 8];
    file.read_exact(&mut header).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(b"invalid!").unwrap();
    file.flush().unwrap();
    drop(file);

    queue_opened_disk_recording(&mut case, recording);
    case.wait_seek();
    assert!(matches!(
        &case.studio.displayed_analysis,
        DisplayedAnalysis::Failed { frame: 0, message }
            if message.text(Language::English).contains("invalid internal history header")
    ));
    assert!(!case.studio.analysis_ready());
    let generation = case.studio.seek_worker.as_ref().unwrap().generation();
    for _ in 0..3 {
        case.studio.update_analysis_demand();
        case.studio.poll(&case.context);
    }
    assert!(case.studio.seeking.is_none());
    assert_eq!(
        case.studio.seek_worker.as_ref().unwrap().generation(),
        generation
    );

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&first_segment)
        .unwrap();
    file.write_all(&header).unwrap();
    file.flush().unwrap();
    drop(file);
    let show = |ui: &mut egui::Ui, studio: &mut Studio| {
        assert!(!studio.ensure_current_analysis(ui));
    };
    let output = draw(&mut case, vec![], show);
    let retry = labels(&output)
        .into_iter()
        .find(|(text, _)| text == "Retry")
        .unwrap()
        .1
        .center();
    for pressed in [true, false] {
        draw(
            &mut case,
            vec![
                egui::Event::PointerMoved(retry),
                egui::Event::PointerButton {
                    pos: retry,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            show,
        );
    }
    assert_eq!(case.studio.seeking, Some(0));
    case.wait_seek();
    case.assert_analysis(0);
}
