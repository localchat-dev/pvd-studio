use super::*;
use pvd_core::frame_sources::{FrameSection, FrameSourceKey, SourceFrame};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

#[path = "../../../../test-support/apex_clothing.rs"]
mod oracle;
mod producer;
mod rolling;

const TIMEOUT: Duration = Duration::from_secs(45);

struct Observation {
    source: FrameSourceKey,
    frame: SourceFrame,
    state: SceneState,
}

struct Case {
    studio: Studio,
    context: egui::Context,
    directory: PathBuf,
    producer: Option<producer::Producer>,
    observed: BTreeMap<usize, Observation>,
    trace: Vec<serde_json::Value>,
    steps: usize,
}

impl Case {
    fn new(mode: &str) -> Self {
        Self::with_history(mode, 36, 256 << 20, 8)
    }

    fn with_history(mode: &str, steps: usize, history_bytes: u64, segment_frames: usize) -> Self {
        let raw_budget_bytes = if steps == 36 { 64 << 20 } else { 96 << 20 };
        let output = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../output")
            .canonicalize()
            .unwrap();
        let base = if let Some(path) = std::env::var_os("PVD_APEX_TCP_OUTPUT") {
            PathBuf::from(path)
                .canonicalize()
                .expect("existing TCP output directory")
        } else {
            let base = output.join("apex-clothing-tcp");
            fs::create_dir_all(&base).unwrap();
            base.canonicalize().unwrap()
        };
        assert!(
            base.starts_with(&output),
            "TCP artifacts must stay under output"
        );
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = base.join(format!("{mode}-{}-{nonce}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let context = egui::Context::default();
        let mut studio = Studio::default();
        studio.prefs.language = Language::English;
        studio.prefs.history_directory = directory.join("history").display().to_string();
        studio.recorded_timing = false;
        studio.playback_fps = 1.0;
        studio.server = Some(
            NetworkSession::start_tracked(
                ListenConfig {
                    address: "127.0.0.1:0".parse().unwrap(),
                    capture_directory: Some(directory.join("received")),
                    ..Default::default()
                },
                NetworkOptions {
                    history: pvd_core::live::HistoryConfig {
                        directory: directory.join("history"),
                        disk_budget_bytes: history_bytes,
                        segment_target_bytes: 16 << 20,
                        segment_frames,
                        min_free_disk_bytes: 0,
                    },
                    receive_budget_bytes: raw_budget_bytes,
                    raw_limit_bytes: raw_budget_bytes,
                    live_profile: true,
                    ..Default::default()
                },
                context.clone(),
                &studio.background_jobs,
            )
            .unwrap(),
        );
        Self {
            studio,
            context,
            directory,
            producer: None,
            observed: BTreeMap::new(),
            trace: Vec::new(),
            steps,
        }
    }

    fn wait(&mut self, description: &str, ready: impl Fn(&Studio) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.studio.poll(&self.context);
            if ready(&self.studio) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{description} timed out; artifacts={} notices={:?}",
                self.directory.display(),
                self.studio.notices
            );
            std::thread::park_timeout(Duration::from_millis(2));
        }
    }

    fn control(&mut self, prefix: &str) -> Vec<u64> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.studio.poll(&self.context);
            let producer = self.producer.as_mut().unwrap();
            if let Ok(line) = producer.progress.try_recv() {
                let mut fields = line.split_ascii_whitespace();
                assert_eq!(fields.next(), Some(prefix), "producer control order");
                return fields.map(|field| field.parse().unwrap()).collect();
            }
            assert!(
                producer.status().is_none(),
                "producer stopped before {prefix}; see {}",
                self.directory.display()
            );
            assert!(Instant::now() < deadline, "{prefix} timed out");
            std::thread::park_timeout(Duration::from_millis(2));
        }
    }

    fn step(&mut self, step: usize, source: FrameSourceKey) -> u64 {
        self.producer
            .as_mut()
            .unwrap()
            .command(&format!("step {step}"));
        if self.steps == 36 && (step == 15 || step == 16) {
            self.timed_step(if step == 15 { 1 } else { -1 });
        }
        let progress = self.control("PVD_APEX_STEP");
        assert_eq!(progress.len(), 2);
        assert_eq!(progress[0], step as u64);
        let written = progress[1];
        self.wait("live SDK endpoint", |studio| {
            studio.count() == 2 * (step + 1)
                && studio
                    .live_frame_source
                    .is_some_and(|(key, frame)| key == source && frame.ordinal == step)
                && studio
                    .server
                    .as_ref()
                    .unwrap()
                    .profile_snapshot()
                    .is_some_and(|profile| {
                        profile.decoder_input_bytes >= written && !profile.capture_failed
                    })
        });
        assert!(self.studio.live_connected);
        assert_eq!(self.studio.live_session, Some(1));
        assert_eq!(self.studio.frame_source(), Some(source));
        if self.studio.follow_live {
            assert_eq!(
                self.studio.displayed_frame_source,
                self.studio.live_frame_source
            );
        }
        if self.steps == 36 || step.is_multiple_of(12) || step + 1 == self.steps {
            self.observe(&format!("received_step_{step}"));
        }
        written
    }

    fn observe(&mut self, action: &str) {
        let (source, frame) = self
            .studio
            .displayed_frame_source
            .expect("display identity");
        assert_eq!(self.studio.frame, frame.index);
        assert!(self.studio.state.objects.len() < 512);
        assert!(
            self.studio
                .state
                .debug_draw
                .values()
                .map(|draws| draws.len())
                .sum::<usize>()
                <= 4 * 1922
        );
        if let Some(previous) = self.observed.get(&frame.index) {
            assert_eq!((source, frame), (previous.source, previous.frame));
            assert!(
                self.studio.state == previous.state,
                "display changed at the same complete endpoint"
            );
        } else {
            assert!(self.observed.len() < 2 * self.steps);
            self.observed.insert(
                frame.index,
                Observation {
                    source,
                    frame,
                    state: self.studio.state.clone(),
                },
            );
        }
        self.trace.push(serde_json::json!({
            "action": action,
            "source": format!("{source:?}"),
            "index": frame.index,
            "ordinal": frame.ordinal,
            "live": self.studio.follow_live,
            "connected": self.studio.live_connected,
        }));
        fs::write(
            self.directory.join("actions.json"),
            serde_json::to_vec_pretty(&self.trace).unwrap(),
        )
        .unwrap();
    }

    fn seek_ordinal(&mut self, ordinal: usize) {
        let target = self.studio.frame_at_ordinal(ordinal).unwrap();
        self.studio.seek(target);
        self.wait("history seek", |studio| studio.seeking.is_none());
        assert_eq!(self.studio.frame, target);
        assert!(!self.studio.follow_live);
        self.observe("seek");
    }

    fn single_step(&mut self, direction: i32) {
        let target = self.studio.next_playback_frame(direction).unwrap();
        self.studio.step_frame(direction);
        self.wait("single step", |studio| studio.seeking.is_none());
        assert_eq!(self.studio.frame, target);
        assert!(!self.studio.follow_live);
        self.observe(if direction > 0 {
            "step_forward"
        } else {
            "step_reverse"
        });
    }

    fn timed_step(&mut self, direction: i32) {
        let target = self.studio.next_playback_frame(direction).unwrap();
        self.studio.start_playback(direction);
        assert_eq!(self.studio.playing, direction);
        self.studio.last_step = Instant::now() - Duration::from_secs(1);
        self.studio.poll(&self.context);
        let deadline = Instant::now() + TIMEOUT;
        while self.studio.seeking.is_some() {
            self.studio.poll_network(&self.context);
            self.studio.poll_seek(&self.context);
            assert!(Instant::now() < deadline, "timed playback seek stalled");
            std::thread::park_timeout(Duration::from_millis(2));
        }
        assert_eq!(self.studio.frame, target);
        assert_eq!(self.studio.playing, direction);
        self.studio.pause_display();
        self.observe(if direction > 0 {
            "play_forward"
        } else {
            "play_reverse"
        });
    }

    fn resume(&mut self) {
        self.studio.return_to_live();
        assert!(self.studio.follow_live);
        assert_eq!(
            self.studio.displayed_frame_source,
            self.studio.live_frame_source
        );
        self.observe("return_live");
    }

    fn finish_producer(&mut self) {
        self.producer.as_mut().unwrap().command("finish");
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.studio.poll(&self.context);
            if let Some(status) = self.producer.as_mut().unwrap().status() {
                assert!(
                    status.success(),
                    "SDK failed; see {}",
                    self.directory.display()
                );
                break;
            }
            assert!(Instant::now() < deadline, "SDK teardown timed out");
            std::thread::park_timeout(Duration::from_millis(2));
        }
        self.producer.as_mut().unwrap().join_output();
        self.wait("completed TCP connection", |studio| !studio.live_connected);
        assert_eq!(self.studio.count(), 2 * self.steps);
    }

    fn run_probe(&mut self, executable: &Path, capture: &Path) {
        let mut command = Command::new(executable);
        command
            .arg(capture)
            .arg(self.directory.join("event-probe"))
            .stdin(Stdio::null())
            .stdout(File::create(self.directory.join("probe.stdout.log")).unwrap())
            .stderr(File::create(self.directory.join("probe.stderr.log")).unwrap());
        let mut process = producer::Process::spawn(&mut command);
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.studio.poll(&self.context);
            if let Some(status) = process.status() {
                assert!(
                    status.success(),
                    "event probe failed; see {}",
                    self.directory.display()
                );
                break;
            }
            assert!(Instant::now() < deadline, "event probe timed out");
            std::thread::park_timeout(Duration::from_millis(2));
        }
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        self.producer = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.studio.poll_shutdown() && Instant::now() < deadline {
            std::thread::park_timeout(Duration::from_millis(2));
        }
    }
}

fn input(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("{name} is required")))
        .canonicalize()
        .unwrap()
}

fn digest(path: &Path) -> String {
    let mut file = File::open(path).unwrap();
    let mut digest = blake3::Hasher::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    digest.finalize().to_hex().to_string()
}

fn has_files(directory: &Path) -> bool {
    directory.exists()
        && fs::read_dir(directory).unwrap().any(|entry| {
            let entry = entry.unwrap();
            assert!(!entry.file_type().unwrap().is_symlink());
            if entry.file_type().unwrap().is_dir() {
                has_files(&entry.path())
            } else {
                true
            }
        })
}

#[test]
#[ignore = "requires the official APEX producer, plane32 asset and event probe; runs a fresh SDK TCP connection"]
fn official_apex_tcp_preserves_sources_playback_and_saved_frames() {
    let executable = input("PVD_APEX_CLOTHING_PRODUCER");
    let asset = input("PVD_APEX_CLOTHING_ASSET");
    let probe = input("PVD_APEX_EVENT_PROBE");
    let input_hashes = [digest(&executable), digest(&asset), digest(&probe)];
    let mode = std::env::var("PVD_APEX_TCP_MODE").expect("PVD_APEX_TCP_MODE must be sync or async");
    assert!(matches!(mode.as_str(), "sync" | "async"));
    let asynchronous = mode == "async";
    let mut case = Case::new(&mode);
    let port = case.studio.server.as_ref().unwrap().address().port();
    case.producer = Some(producer::Producer::start(
        &executable,
        &asset,
        &case.directory,
        port,
        asynchronous,
    ));
    let ready = case.control("PVD_APEX_READY");
    assert_eq!(ready.len(), 3);
    assert!(ready.iter().all(|value| *value > 0));
    let sources = [
        FrameSourceKey::Captured {
            section: FrameSection::PhysX,
            source: ready[0],
        },
        FrameSourceKey::Captured {
            section: FrameSection::Apex,
            source: ready[1],
        },
    ];
    let mut selected = sources[0];
    let mut offsets = Vec::new();
    let mut paused = None;
    for step in 0..36 {
        match step {
            6 | 20 => {
                selected = sources[1];
                case.studio.select_frame_source(selected);
            }
            10 | 30 => {
                case.studio.pause_display();
                paused = Some((case.studio.frame, case.studio.state.clone()));
            }
            17 | 32 => {
                case.resume();
                paused = None;
            }
            24 => {
                case.studio.select_frame_source(sources[0]);
                case.studio.select_frame_source(sources[1]);
            }
            _ => {}
        }
        offsets.push(case.step(step, selected));
        if let Some((frame, state)) = &paused {
            assert_eq!(case.studio.frame, *frame);
            assert!(
                case.studio.state == *state,
                "paused display changed during SDK reception"
            );
        }
        match step {
            12 | 31 => {
                case.single_step(-1);
                paused = None;
            }
            13 => case.single_step(1),
            14 => {
                selected = sources[0];
                case.studio.select_frame_source(selected);
                case.wait("paused source selection", |studio| studio.seeking.is_none());
                case.observe("select_paused_physx");
                case.seek_ordinal(5);
            }
            _ => {}
        }
    }
    assert!(case.studio.follow_live);
    assert_eq!(case.studio.frame_source(), Some(sources[1]));
    let final_display = case.studio.state.clone();
    let final_identity = case.studio.displayed_frame_source.unwrap();
    case.finish_producer();
    assert_eq!(case.studio.frame_source(), Some(sources[1]));
    assert_eq!(case.studio.displayed_frame_source, Some(final_identity));
    assert!(
        case.studio.state == final_display,
        "disconnect or uncompleted tail changed the displayed endpoint"
    );
    case.observe("disconnected");
    let received = case.studio.capture_path.clone().unwrap();
    assert_eq!(received.extension().unwrap(), "pvdstream");
    assert!(fs::metadata(&received).unwrap().len() < 64 << 20);
    let capture = case.directory.join("capture.pvdstream");
    case.studio.export_path = capture.display().to_string();
    assert!(case.studio.can_save_capture());
    case.studio.save_capture(case.context.clone());
    case.wait("raw save", |studio| studio.save_job.is_none());
    assert_eq!(digest(&capture), digest(&received));
    let profile = case
        .studio
        .server
        .as_ref()
        .unwrap()
        .profile_snapshot()
        .unwrap();
    assert!(profile.completed && !profile.capture_failed);
    assert_eq!(
        profile.received_bytes,
        Some(fs::metadata(&capture).unwrap().len())
    );
    assert_eq!(
        profile.decoder_input_bytes,
        fs::metadata(&capture).unwrap().len()
    );
    case.run_probe(&probe, &capture);
    let bytes = fs::read(&capture).unwrap();
    let fixture = oracle::Fixture::load(&case.directory, &bytes);
    assert_eq!(fixture.sources(), sources);
    assert_eq!(fixture.async_fetch(), asynchronous);
    let truth: serde_json::Value =
        serde_json::from_slice(&fs::read(case.directory.join("truth.json")).unwrap()).unwrap();
    assert_eq!(truth["transport"], "socket");
    assert_eq!(truth["controlled_steps"], true);
    for (step, written) in offsets.iter().enumerate() {
        assert_eq!(
            truth["steps"][step]["after_getters_bytes"].as_u64(),
            Some(*written)
        );
    }
    for (index, sample) in &case.observed {
        let (source, frame, _) = fixture
            .frames()
            .find(|(_, frame, _)| frame.index == *index)
            .unwrap();
        assert_eq!((sample.source, sample.frame), (source, frame));
        fixture.check_state(*index, &sample.state);
    }
    let recording = case.studio.timeline_recording().unwrap();
    assert!(recording.is_disk_backed());
    assert!(recording.disk_usage().unwrap().segments > 1);
    fixture.check_directory(recording);
    fixture.check_recording(recording);
    assert_ne!(
        case.studio.state.objects.len(),
        fixture.final_object_count()
    );
    let archive = case.directory.join("retained.pvdcapture");
    case.studio.retained_path = archive.display().to_string();
    assert!(case.studio.can_save_recording());
    case.studio.save_recording(case.context.clone());
    case.wait("project save", |studio| studio.recording_export.is_none());
    assert!(archive.is_file(), "{:?}", case.studio.notices);
    case.studio.stop_listener();
    case.wait("listener stop", |studio| studio.server.is_none());
    case.studio.close();
    let history = case.directory.join("history");
    case.wait("history release", |_| !has_files(&history));
    assert!(case.studio.frame_source().is_none());
    case.studio.open(archive.clone(), case.context.clone());
    case.wait("project reopen", |studio| {
        studio.job.is_none() && studio.seeking.is_none()
    });
    assert!(case.studio.is_project_capture());
    fixture.check_recording(case.studio.timeline_recording().unwrap());
    for source in sources {
        case.studio.select_frame_source(source);
        case.wait("reopened source selection", |studio| {
            studio.seeking.is_none()
        });
        for ordinal in [0, 1, 15, 35, 34, 20, 0] {
            case.seek_ordinal(ordinal);
            let (_, frame) = case.studio.displayed_frame_source.unwrap();
            assert_eq!(case.studio.frame_source(), Some(source));
            fixture.check_state(frame.index, &case.studio.state);
        }
    }
    case.studio.close();
    case.wait("reopened history release", |_| !has_files(&history));
    assert_eq!(
        input_hashes,
        [digest(&executable), digest(&asset), digest(&probe)]
    );
    let report = serde_json::json!({
        "mode": mode, "transport": "PxDefaultPvdSocketTransportCreate", "port": port,
        "sdk": truth["sdk"], "apex_commit": truth["apex_commit"], "instrumentation": "DEBUG",
        "producer": {"path": executable, "blake3": input_hashes[0]},
        "asset": {"path": asset, "blake3": input_hashes[1]},
        "probe": {"path": probe, "blake3": input_hashes[2]},
        "capture": {"bytes": bytes.len(), "blake3": blake3::hash(&bytes).to_hex().to_string()},
        "truth_blake3": digest(&case.directory.join("truth.json")),
        "archive_blake3": digest(&archive),
        "sampled_endpoints": case.observed.len(), "source_frames": [36, 36],
        "decoder_input_bytes": profile.decoder_input_bytes,
        "history_released": true, "passed": true,
    });
    fs::write(
        case.directory.join("result.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    println!(
        "APEX {mode} SDK TCP, source selection, pause, single-step, timed forward/reverse, resume, disconnect and PVDCAP03 verified; artifacts={}",
        case.directory.display()
    );
}
