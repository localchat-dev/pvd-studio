use super::*;
use pvd_core::analysis::view::{
    MemorySource, MemoryViewCursor, ProfileMetadataSource, ProfileSource,
};
use std::collections::BTreeMap;
use std::process::{Child, Command, Stdio};
use std::{cell::RefCell, rc::Rc};

struct Producer(Child);

impl Drop for Producer {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

struct Probe {
    studio: Studio,
    context: egui::Context,
    deadline: Instant,
    started: Instant,
    max_history_bytes: u64,
    max_queued_bytes: u64,
    displayed_frames: Vec<(usize, usize, Option<RebaseWitness>)>,
    telemetry: Option<File>,
    next_sample: Instant,
    phase: &'static str,
    mode: ProbeMode,
    progress: Option<Rc<RefCell<TcpProgressBoundaries>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProbeMode {
    FullAnalysis,
    UiDemand,
}

const MAX_PROGRESS_BOUNDARIES: usize = 1024;

#[derive(serde::Serialize)]
struct TcpProgressBoundary {
    event: &'static str,
    seconds: f64,
    phase: &'static str,
    direction: i32,
    previous_frame: usize,
    last_frame: usize,
    live_before: usize,
    live_after: usize,
    displayed: usize,
    live_session: Option<u64>,
    live_profile: Option<crate::network::LiveProfileSnapshot>,
    pending_snapshot: Option<crate::network::PendingSnapshotProgress>,
    window: serde_json::Value,
}

struct TcpProgressBoundaries {
    records: Vec<TcpProgressBoundary>,
    dropped: usize,
}

impl TcpProgressBoundaries {
    fn new() -> Self {
        Self {
            records: Vec::with_capacity(MAX_PROGRESS_BOUNDARIES),
            dropped: 0,
        }
    }

    fn push(&mut self, boundary: TcpProgressBoundary) {
        if self.records.len() < MAX_PROGRESS_BOUNDARIES {
            self.records.push(boundary);
        } else {
            self.dropped = self.dropped.saturating_add(1);
        }
    }
}

struct TcpProgressDump {
    boundaries: Rc<RefCell<TcpProgressBoundaries>>,
    output: PathBuf,
    mode: ProbeMode,
    seconds: u64,
    dumped: bool,
}

impl TcpProgressDump {
    fn write(&self, partial: bool, panicking: bool) -> Result<(), pvd_core::Error> {
        let boundaries = self.boundaries.try_borrow()?;
        let file = File::create_new(self.output.join("progress-boundaries.json"))?;
        serde_json::to_writer_pretty(
            file,
            &serde_json::json!({
                "mode": format!("{:?}", self.mode),
                "requested_duration_seconds": self.seconds,
                "partial": partial,
                "panicking": panicking,
                "test_outcome": "not_asserted_by_progress",
                "record_capacity": MAX_PROGRESS_BOUNDARIES,
                "dropped_boundaries": boundaries.dropped,
                "truncated": boundaries.dropped != 0,
                "scope": "core progress counters; optional timing trace records stage clocks; boundary/profile clocks and non-atomic snapshots remain; pending-slot peek never waits or consumes; decoder input precedes feed; completed calls include failures; no per-boundary file IO",
                "records": &boundaries.records,
            }),
        )?;
        Ok(())
    }

    fn finish(&mut self) {
        self.write(false, false).unwrap();
        self.dumped = true;
    }
}

impl Drop for TcpProgressDump {
    fn drop(&mut self) {
        if self.dumped {
            return;
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.write(true, std::thread::panicking())
        }));
        if !matches!(result, Ok(Ok(()))) {
            let _ = std::io::Write::write_all(
                &mut std::io::stderr().lock(),
                b"TCP partial progress dump failed; original test failure is preserved\n",
            );
        }
    }
}

#[test]
fn progress_boundaries_are_bounded_and_dump_on_unwind_without_swallowing_failure() {
    let output = std::env::temp_dir().join(format!(
        "pvd-tcp-progress-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    std::fs::create_dir(&output).unwrap();
    let boundaries = Rc::new(RefCell::new(TcpProgressBoundaries::new()));
    for _ in 0..=MAX_PROGRESS_BOUNDARIES {
        boundaries.borrow_mut().push(TcpProgressBoundary {
            event: "play_frames_before_progress_assert",
            seconds: 1.0,
            phase: "forward",
            direction: 1,
            previous_frame: 3,
            last_frame: 19,
            live_before: 40,
            live_after: 40,
            displayed: 19,
            live_session: Some(1),
            live_profile: None,
            pending_snapshot: None,
            window: serde_json::Value::Null,
        });
    }
    assert_eq!(boundaries.borrow().records.len(), MAX_PROGRESS_BOUNDARIES);
    assert_eq!(boundaries.borrow().dropped, 1);
    let path = output.join("progress-boundaries.json");
    assert!(!path.exists());
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _dump = TcpProgressDump {
            boundaries: boundaries.clone(),
            output: output.clone(),
            mode: ProbeMode::UiDemand,
            seconds: 120,
            dumped: false,
        };
        panic!("original progress failure");
    }));
    let failure = failure.unwrap_err();
    assert_eq!(
        failure.downcast_ref::<&str>(),
        Some(&"original progress failure")
    );
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(report["partial"], true);
    assert_eq!(report["panicking"], true);
    assert_eq!(report["truncated"], true);
    assert_eq!(report["dropped_boundaries"], 1);
    assert_eq!(
        report["records"].as_array().unwrap().len(),
        MAX_PROGRESS_BOUNDARIES
    );
    assert_eq!(report["records"][0]["live_before"], 40);
    assert_eq!(report["records"][0]["live_after"], 40);
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(output).unwrap();
}

struct TcpTraceDump {
    trace: crate::display_probe::Trace,
    output: PathBuf,
    mode: ProbeMode,
    seconds: u64,
    origin_lower_seconds: f64,
    origin_upper_seconds: f64,
    workers_drained: bool,
    dumped: bool,
}

impl TcpTraceDump {
    fn write(&self, partial: bool, panicking: bool) -> Result<(), pvd_core::Error> {
        let name = if partial {
            "tcp-seek-trace.partial.json"
        } else {
            "tcp-seek-trace.json"
        };
        let file = File::create_new(self.output.join(name))?;
        self.trace.with_records(|records| {
            serde_json::to_writer_pretty(
                file,
                &serde_json::json!({
                    "mode": format!("{:?}", self.mode),
                    "requested_duration_seconds": self.seconds,
                    "origin_lower_seconds": self.origin_lower_seconds,
                    "origin_upper_seconds": self.origin_upper_seconds,
                    "record_capacity": 65_536,
                    "partial": partial,
                    "panicking": panicking,
                    "workers_drained": self.workers_drained,
                    "test_outcome": "not_asserted_by_trace",
                    "live_profile_enabled": true,
                    "failure": self.trace.failure(),
                    "scope": "diagnostic request/worker/core/wrap/accept/tree/apply; no render or GPU; trace and live-profile overhead included",
                    "records": records,
                }),
            )?;
            Ok(())
        })
    }

    fn finish(&mut self) {
        self.workers_drained = true;
        self.trace
            .with_records(|records| assert!(!records.is_empty()));
        self.write(false, false).unwrap();
        assert!(self.trace.failure().is_none(), "{:?}", self.trace.failure());
        self.dumped = true;
    }
}

impl Drop for TcpTraceDump {
    fn drop(&mut self) {
        if self.dumped {
            return;
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.write(true, std::thread::panicking())
        }));
        match result {
            Ok(Ok(())) => {}
            Ok(Err(failure)) => {
                let _ = std::io::Write::write_fmt(
                    &mut std::io::stderr().lock(),
                    format_args!("TCP partial trace dump failed: {failure}\n"),
                );
            }
            Err(_) => {
                let _ = std::io::Write::write_all(
                    &mut std::io::stderr().lock(),
                    b"TCP partial trace dump panicked; original test failure is preserved\n",
                );
            }
        }
    }
}

struct PlaybackSnapshot {
    progress: serde_json::Value,
    frame: usize,
    scene: SceneState,
    analysis: pvd_core::analysis::SharedAnalysis,
    with_analysis: bool,
}

pub(super) fn completed_history_step_at_live_edge(
    studio: &Studio,
    before_frame: usize,
    before_pending: Option<usize>,
    before_count: usize,
) -> bool {
    let Some(next) = before_frame.checked_add(1) else {
        return false;
    };
    studio.live_connected
        && studio.follow_live
        && studio.playing == 0
        && studio.seeking.is_none()
        && before_pending == Some(next)
        && studio.frame == next
        && studio.live_frame == next
        && next.checked_add(1) == Some(before_count)
        && studio.count() == before_count
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub(super) struct RebaseWitness {
    previous: usize,
    expected: usize,
    expected_retained: bool,
    target: usize,
    generation: u64,
}

impl RebaseWitness {
    pub(super) fn observe(recording: &Recording, previous: usize, target: usize) -> Option<Self> {
        let expected = previous.checked_add(1)?;
        (recording.is_disk_backed() && target > expected && target < recording.frame_count()).then(
            || Self {
                previous,
                expected,
                expected_retained: recording.is_frame_retained(expected),
                target,
                generation: 0,
            },
        )
    }

    pub(super) fn dispatched(mut self, generation: u64) -> Self {
        self.generation = generation;
        self
    }

    pub(super) fn accepted(self, target: usize, generation: u64) -> Option<Self> {
        (self.target == target && self.generation == generation).then_some(self)
    }

    pub(super) fn allows(self, previous: usize, frame: usize, direction: i32) -> bool {
        direction > 0
            && self.previous == previous
            && self.target == frame
            && previous.checked_add(1) == Some(self.expected)
            && !self.expected_retained
    }
}

impl Probe {
    fn snapshot(&self) -> PlaybackSnapshot {
        assert!(!self.studio.follow_live);
        let with_analysis = self.studio.analysis_ready();
        if self.mode == ProbeMode::FullAnalysis {
            assert!(with_analysis);
        }
        PlaybackSnapshot {
            progress: serde_json::Value::Null,
            frame: self.studio.frame,
            scene: self.studio.state.clone(),
            analysis: self.studio.recording.as_ref().unwrap().analysis.clone(),
            with_analysis,
        }
    }

    fn pump(&mut self) {
        assert!(
            Instant::now() < self.deadline,
            "TCP playback probe timed out: frame={}, decoded={}, first={}, seeking={:?}",
            self.studio.frame,
            self.studio.count(),
            self.studio.first_frame(),
            self.studio.seeking
        );
        self.displayed_frames.clear();
        let _ = self.context.run(egui::RawInput::default(), |context| {
            let before = self.studio.frame;
            if self.mode == ProbeMode::UiDemand {
                assert!(self.studio.prefs.dock.iter_all_nodes().any(|(_, node)| {
                    matches!(node, egui_dock::Node::Leaf(leaf)
                        if matches!(leaf.tabs.get(leaf.active.0), Some(Panel::Memory)))
                }));
                self.studio.update_analysis_demand();
                if self.studio.playing != 0 {
                    assert!(!self.studio.analysis_demand);
                }
            }
            self.studio.poll(context);
            if self.studio.frame != before {
                self.displayed_frames.push((
                    self.studio.frame,
                    self.studio.first_frame(),
                    self.studio.accepted_playback_rebase.take(),
                ));
            }
        });
        if let Some(stats) = self
            .studio
            .timeline_recording()
            .and_then(Recording::disk_usage)
        {
            self.max_history_bytes = self.max_history_bytes.max(stats.disk_bytes);
        }
        if let Some(stats) = &self.studio.receive_status {
            self.max_queued_bytes = self.max_queued_bytes.max(stats.queued_bytes);
        }
        if Instant::now() >= self.next_sample {
            self.next_sample = Instant::now() + Duration::from_secs(1);
            if let Some(file) = &mut self.telemetry {
                use std::io::Write;
                let history = self
                    .studio
                    .timeline_recording()
                    .and_then(Recording::disk_usage);
                let live_profile = if self.studio.display_trace.is_some() {
                    self.studio
                        .server
                        .as_ref()
                        .and_then(NetworkSession::profile_snapshot)
                } else {
                    None
                };
                let sample = serde_json::json!({
                    "live_profile": live_profile,
                    "seconds": self.started.elapsed().as_secs_f64(),
                    "phase": self.phase,
                    "decoded": self.studio.count(),
                    "displayed": self.studio.frame,
                    "first": self.studio.first_frame(),
                    "follow_live": self.studio.follow_live,
                    "playing": self.studio.playing,
                    "seeking": self.studio.seeking,
                    "connected": self.studio.live_connected,
                    "queued_bytes": self.studio.receive_status.as_ref().map(|s| s.queued_bytes),
                    "history_bytes": history.as_ref().map(|s| s.disk_bytes),
                    "history_warning": history.as_ref().and_then(|s| s.warning.as_ref()),
                    "mode": format!("{:?}", self.mode),
                    "analysis_ready": self.studio.analysis_ready(),
                    "analysis_demand": self.studio.analysis_demand,
                });
                writeln!(file, "{sample}").unwrap();
                file.flush().unwrap();
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    fn wait_until(&mut self, condition: impl Fn(&Studio) -> bool) {
        while !condition(&self.studio) {
            self.pump();
        }
    }

    fn trace_playback_boundary(
        &mut self,
        event: &'static str,
        direction: i32,
        previous_frame: usize,
        last_frame: usize,
        live_counts: (usize, usize),
        window: serde_json::Value,
    ) {
        if let Some(progress) = &self.progress {
            let (live_before, live_after) = live_counts;
            let live_profile = self
                .studio
                .server
                .as_ref()
                .and_then(NetworkSession::profile_snapshot);
            let pending_snapshot = self
                .studio
                .server
                .as_ref()
                .map(NetworkSession::pending_snapshot_progress);
            progress.borrow_mut().push(TcpProgressBoundary {
                event,
                seconds: self.started.elapsed().as_secs_f64(),
                phase: self.phase,
                direction,
                previous_frame,
                last_frame,
                live_before,
                live_after,
                displayed: self.studio.frame,
                live_session: self.studio.live_session,
                live_profile,
                pending_snapshot,
                window: window.clone(),
            });
        }
        if self.studio.display_trace.is_none() {
            return;
        }
        let Some(file) = &mut self.telemetry else {
            return;
        };
        let (live_before, live_after) = live_counts;
        use std::io::Write;
        let live_profile = self
            .studio
            .server
            .as_ref()
            .and_then(NetworkSession::profile_snapshot);
        let sample = serde_json::json!({
            "event": event,
            "seconds": self.started.elapsed().as_secs_f64(),
            "phase": self.phase,
            "direction": direction,
            "previous_frame": previous_frame,
            "last_frame": last_frame,
            "live_before": live_before,
            "live_after": live_after,
            "decoded": live_after,
            "displayed": self.studio.frame,
            "first": self.studio.first_frame(),
            "follow_live": self.studio.follow_live,
            "playing": self.studio.playing,
            "seeking": self.studio.seeking,
            "connected": self.studio.live_connected,
            "queued_bytes": self.studio.receive_status.as_ref().map(|s| s.queued_bytes),
            "live_profile": live_profile,
            "window": window,
        });
        writeln!(file, "{sample}").unwrap();
        file.flush().unwrap();
    }

    fn committed_frame(&self, session: u64) -> usize {
        assert_eq!(self.studio.live_session, Some(session));
        let profile = self
            .studio
            .server
            .as_ref()
            .and_then(NetworkSession::profile_snapshot)
            .expect("TCP playback requires core progress counters");
        assert_eq!(
            profile.session, session,
            "TCP session changed during playback"
        );
        assert!(profile.metrics.enabled);
        profile
            .metrics
            .last_committed_frame
            .expect("TCP playback requires a committed frame")
    }

    fn play_frames(&mut self, direction: i32, count: usize) -> PlaybackSnapshot {
        let observation_started = Instant::now();
        let session = self.studio.live_session.expect("TCP session is required");
        let live_before = self.studio.count();
        let previous_frame = self.studio.frame;
        let mut previous = previous_frame;
        self.trace_playback_boundary(
            "play_frames_begin",
            direction,
            previous_frame,
            previous,
            (live_before, live_before),
            serde_json::json!({ "requested_steps": count, "session": session }),
        );
        let commit_before = self.committed_frame(session);
        let mut commit_after;
        let mut live_after;
        let mut extra_started: Option<Instant> = None;
        let mut frames = Vec::new();
        let mut rebases = 0;
        let mut adjacent_steps = 0;
        let mut edge_snapshot: Option<PlaybackSnapshot> = None;
        let mut terminal = None;
        let mut scene_only_steps = 0;
        loop {
            assert_eq!(self.studio.playing, direction);
            assert!(!self.studio.follow_live);
            commit_after = self.committed_frame(session);
            live_after = self.studio.count();
            if extra_started.is_some_and(|started| started.elapsed() >= Duration::from_secs(5))
                || (frames.len() >= count
                    && commit_after > commit_before
                    && live_after > live_before)
            {
                break;
            }
            let before_frame = self.studio.frame;
            let before_pending = self.studio.seeking;
            let before_count = self.studio.count();
            let forward_history =
                direction == 1 && self.studio.playing == 1 && !self.studio.follow_live;
            if forward_history
                && !frames.is_empty()
                && previous == before_frame
                && before_frame >= self.studio.first_frame()
                && before_frame.saturating_add(2) >= before_count
                && edge_snapshot
                    .as_ref()
                    .is_none_or(|saved| saved.frame != before_frame)
            {
                edge_snapshot = Some(self.snapshot());
            }
            self.pump();
            assert!(self.studio.live_connected, "producer ended during playback");
            let committed = self.committed_frame(session);
            // The final disk seek can complete and return to Live within the same poll.
            if forward_history
                && completed_history_step_at_live_edge(
                    &self.studio,
                    before_frame,
                    before_pending,
                    before_count,
                )
            {
                assert_eq!(
                    edge_snapshot.as_ref().map(|saved| saved.frame),
                    Some(before_frame)
                );
                terminal = Some(serde_json::json!({
                    "frame": self.studio.frame,
                    "pending_before": before_pending,
                    "saved_history_frame": before_frame,
                    "completed_adjacent_steps": 1,
                    "minimum_steps_exception": "natural_live_edge",
                    "commit_before_live": commit_after,
                    "decoded_before_live": live_after,
                    "seconds": self.started.elapsed().as_secs_f64(),
                }));
                break;
            }
            assert_eq!(
                self.studio.playing,
                direction,
                "playback stopped: frame={}, live_frame={}, decoded={}, first={}, follow_live={}, seeking={:?}, completed_steps={}, notices={:?}",
                self.studio.frame,
                self.studio.live_frame,
                self.studio.count(),
                self.studio.first_frame(),
                self.studio.follow_live,
                self.studio.seeking,
                frames.len(),
                self.studio
                    .notices
                    .iter()
                    .map(|notice| notice.text(Language::English))
                    .collect::<Vec<_>>()
            );
            assert!(!self.studio.follow_live);
            commit_after = committed;
            live_after = self.studio.count();
            for &(frame, first, witness) in &self.displayed_frames {
                if self.mode == ProbeMode::UiDemand {
                    assert!(!self.studio.analysis_ready());
                    assert!(matches!(
                        self.studio.displayed_analysis,
                        analysis_demand::DisplayedAnalysis::Deferred
                    ));
                    assert_eq!(
                        postcard::to_stdvec(&self.studio.recording.as_ref().unwrap().analysis)
                            .unwrap(),
                        postcard::to_stdvec(&pvd_core::analysis::Analysis::default()).unwrap()
                    );
                    scene_only_steps += 1;
                }
                if frame as i64 == previous as i64 + i64::from(direction) {
                    adjacent_steps += 1;
                }
                if !frames.is_empty() {
                    let expected = previous as i64 + i64::from(direction);
                    if direction > 0 && frame as i64 != expected {
                        assert!(
                            witness
                                .is_some_and(|witness| witness.allows(previous, frame, direction)),
                            "unexpected history jump: {previous} -> {frame}, first={first}"
                        );
                        rebases += 1;
                    } else {
                        assert_eq!(frame as i64, expected);
                    }
                }
                frames.push(serde_json::json!({ "frame": frame, "first": first, "seconds": self.started.elapsed().as_secs_f64(), "rebase_witness": witness }));
                previous = frame;
            }
            if frames.len() >= count && (commit_after <= commit_before || live_after <= live_before)
            {
                extra_started.get_or_insert_with(Instant::now);
            }
        }
        let observed_steps = frames.len() + usize::from(terminal.is_some());
        let observation_seconds = observation_started.elapsed().as_secs_f64();
        let extra_observation_seconds =
            extra_started.map(|started| started.elapsed().as_secs_f64());
        let window = serde_json::json!({
            "requested_steps": count,
            "observed_steps": observed_steps,
            "observation_seconds": observation_seconds,
            "extra_observation_seconds": extra_observation_seconds,
            "session": session,
            "commit_before": commit_before,
            "commit_after": commit_after,
            "decoded_at_return": self.studio.count(),
            "natural_live_completion": terminal,
        });
        self.trace_playback_boundary(
            "play_frames_before_progress_assert",
            direction,
            previous_frame,
            previous,
            (live_before, live_after),
            window.clone(),
        );
        assert!(
            extra_observation_seconds.is_none_or(|seconds| seconds < 5.0)
                && commit_after > commit_before
                && live_after > live_before,
            "TCP core commit and UI count must advance during same-session historical playback: {window}, decoded {live_before} -> {live_after}"
        );
        assert!(frames.len() >= count || terminal.is_some());
        let mut snapshot = if terminal.is_some() {
            edge_snapshot.unwrap()
        } else {
            self.snapshot()
        };
        if self.mode == ProbeMode::UiDemand {
            assert!(scene_only_steps > 0);
            assert!(
                !snapshot.with_analysis,
                "the retained playback sample must be scene-only"
            );
        }
        snapshot.progress = serde_json::json!({
            "frames": frames,
            "requested_steps": count,
            "observed_steps": observed_steps,
            "observation_seconds": observation_seconds,
            "extra_observation_seconds": extra_observation_seconds,
            "session": session,
            "commit_before": commit_before,
            "commit_after": commit_after,
            "observed_adjacent_steps": adjacent_steps,
            "natural_live_completion": terminal,
            "decoded_before": live_before,
            "decoded_after": live_after,
            "expired_rebases": rebases,
            "scene_only_steps": scene_only_steps,
            "analysis_mode": format!("{:?}", self.mode),
        });
        snapshot
    }

    fn pause_and_upgrade(&mut self) -> PlaybackSnapshot {
        assert_eq!(self.mode, ProbeMode::UiDemand);
        assert!(!self.studio.follow_live);
        assert!(!self.studio.analysis_ready());
        self.studio.pause_display();
        let frame = self.studio.frame;
        let scene = debug_digest(&self.studio.state);
        let decoded = self.studio.count();
        let started = Instant::now();
        let mut target = frame;
        let mut recoveries = Vec::new();
        while !self.studio.analysis_ready() || self.studio.seeking.is_some() {
            assert!(started.elapsed() < Duration::from_secs(15));
            self.pump();
            assert!(self.studio.live_connected);
            assert!(!self.studio.follow_live);
            if self.studio.analysis_ready() && self.studio.seeking.is_none() {
                break;
            }
            assert_eq!(self.studio.frame, frame);
            if let analysis_demand::DisplayedAnalysis::Failed { frame: failed, .. } =
                self.studio.displayed_analysis
            {
                assert!(!self.studio.analysis_ready());
                assert!(self.studio.seeking.is_none());
                assert!(
                    !self
                        .studio
                        .timeline_recording()
                        .unwrap()
                        .is_frame_retained(target),
                    "analysis failed for retained target {target}; failed marker {failed}"
                );
                assert_eq!(debug_digest(&self.studio.state), scene);
                assert!(
                    recoveries.len() < 3,
                    "latest-frame analysis repeatedly expired"
                );
                for _ in 0..3 {
                    self.pump();
                    assert_eq!(self.studio.frame, frame);
                    assert!(self.studio.seeking.is_none());
                    assert!(!self.studio.analysis_ready());
                    assert!(matches!(
                        self.studio.displayed_analysis,
                        analysis_demand::DisplayedAnalysis::Failed { .. }
                    ));
                }
                let expired_target = target;
                target = self.click_latest_analysis();
                recoveries.push(serde_json::json!({
                    "expired_target": expired_target,
                    "held_frame": frame,
                    "first_at_retry": self.studio.first_frame(),
                    "explicit_retry_target": target,
                    "seconds": started.elapsed().as_secs_f64(),
                }));
            }
        }
        assert_eq!(self.studio.frame, target);
        assert!(matches!(
            self.studio.displayed_analysis,
            analysis_demand::DisplayedAnalysis::Frame(loaded) if loaded == target
        ));
        if target == frame {
            assert_eq!(debug_digest(&self.studio.state), scene);
        }
        let analysis = &self.studio.recording.as_ref().unwrap().analysis;
        assert!(analysis.profile_spans().next().is_some());
        assert!(analysis.memory_snapshot().summary().live_allocations > 0);
        let mut snapshot = self.snapshot();
        snapshot.progress = serde_json::json!({
            "paused_upgrade": true,
            "requested_frame": frame,
            "loaded_frame": target,
            "exact_frame_upgrade": target == frame,
            "expired_recoveries": recoveries,
            "decoded_before": decoded,
            "decoded_after": self.studio.count(),
            "analysis_nonempty": true,
            "seconds": started.elapsed().as_secs_f64(),
        });
        snapshot
    }

    fn click_latest_analysis(&mut self) -> usize {
        fn show(ui: &mut egui::Ui, studio: &mut Studio) {
            assert!(!studio.ensure_current_analysis(ui));
        }
        let held_frame = self.studio.frame;
        let recording = self.studio.timeline_recording().unwrap();
        let target = recording
            .frames
            .iter()
            .rev()
            .find(|frame| recording.is_frame_retained(frame.index))
            .unwrap()
            .index;
        assert!(target > held_frame);
        let output =
            analysis_demand_tests::draw_studio(&self.context, &mut self.studio, vec![], show);
        let button = analysis_demand_tests::labels(&output)
            .into_iter()
            .find(|(text, _)| text == "Load latest retained frame")
            .unwrap()
            .1
            .center();
        for pressed in [true, false] {
            analysis_demand_tests::draw_studio(
                &self.context,
                &mut self.studio,
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
        assert_eq!(self.studio.frame, held_frame);
        assert_eq!(self.studio.seeking, Some(target));
        assert!(!self.studio.analysis_ready());
        target
    }
}

fn memory_at(
    analysis: &pvd_core::analysis::SharedAnalysis,
    frame: usize,
) -> (Vec<u8>, pvd_core::analysis::MemoryChurn) {
    let memory = analysis.memory_snapshot();
    let mut cursor = MemoryViewCursor::default();
    memory.seek(&mut cursor, frame).unwrap();
    (postcard::to_stdvec(&cursor).unwrap(), memory.churn(frame))
}

#[test]
#[ignore = "Requires PVD_SDK34_SAMPLE official PhysX 3.4.2 producer; runs a 30-second TCP capture"]
fn real_sdk_tcp_playback_survives_expired_pause_and_continues_receiving_in_both_directions() {
    let sample = std::env::var_os("PVD_SDK34_SAMPLE").expect("PVD_SDK34_SAMPLE is required");
    let output = PathBuf::from(
        std::env::var_os("PVD_PLAYBACK_PROBE_OUTPUT")
            .expect("PVD_PLAYBACK_PROBE_OUTPUT is required"),
    );
    std::fs::create_dir_all(&output).unwrap();
    let context = egui::Context::default();
    let history = output.join("history");
    let server = NetworkSession::start_with_options(
        ListenConfig {
            address: "127.0.0.1:0".parse().unwrap(),
            capture_directory: Some(output.join("raw")),
            ..Default::default()
        },
        NetworkOptions {
            history: pvd_core::live::HistoryConfig {
                directory: history.clone(),
                disk_budget_bytes: 64 << 20,
                segment_frames: 40,
                min_free_disk_bytes: 0,
                ..Default::default()
            },
            receive_budget_bytes: 128 << 20,
            progress_only: true,
            ..Default::default()
        },
        context.clone(),
    )
    .unwrap();
    let port = server.address().port();
    let mut command = Command::new(sample);
    command
        .args([
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--frames",
            "3000",
            "--duration-seconds",
            "30",
            "--interval-ms",
            "16",
            "--flags",
            "all",
            "--rich",
            "--extra-boxes",
            "7",
            "--extra-static-boxes",
            "16000",
            "--continuous-motion",
            "--resident-allocations",
            "150000",
            "--analysis-batches-per-frame",
            "8",
        ])
        .arg("--stats-truth")
        .arg(output.join("sdk-stats.csv"))
        .arg("--analysis-truth")
        .arg(output.join("sdk-analysis.csv"))
        .stdout(Stdio::from(
            File::create(output.join("sdk.stdout.log")).unwrap(),
        ))
        .stderr(Stdio::from(
            File::create(output.join("sdk.stderr.log")).unwrap(),
        ));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut producer = Producer(command.spawn().unwrap());
    let started = Instant::now();
    let mut studio = Studio::default();
    studio.force_full_playback_analysis = true;
    studio.server = Some(server);
    studio.recorded_timing = false;
    studio.playback_fps = 60.0;
    let progress = Rc::new(RefCell::new(TcpProgressBoundaries::new()));
    let mut progress_dump = TcpProgressDump {
        boundaries: progress.clone(),
        output: output.clone(),
        mode: ProbeMode::FullAnalysis,
        seconds: 30,
        dumped: false,
    };
    let mut probe = Probe {
        studio,
        context,
        deadline: started + Duration::from_secs(90),
        started,
        max_history_bytes: 0,
        max_queued_bytes: 0,
        displayed_frames: Vec::new(),
        telemetry: None,
        next_sample: started,
        phase: "short_capture",
        mode: ProbeMode::FullAnalysis,
        progress: Some(progress),
    };
    while probe.studio.count() < 10 || !probe.studio.has_history() {
        assert!(
            producer.0.try_wait().unwrap().is_none(),
            "SDK producer exited before delivering frames; see sdk.stderr.log"
        );
        probe.pump();
    }
    probe.studio.pause_display();
    let expired_pause = probe.studio.frame;
    probe.wait_until(|studio| studio.first_frame() > expired_pause);
    assert_eq!(probe.studio.frame, expired_pause);
    let retained_first = probe.studio.first_frame();
    probe.studio.toggle_playback();
    assert_eq!(probe.studio.seeking, Some(retained_first));
    probe.wait_until(|studio| studio.seeking.is_none());
    assert!(
        probe.studio.frame >= retained_first,
        "frame={}, first={}, playing={}, notices={:?}",
        probe.studio.frame,
        probe.studio.first_frame(),
        probe.studio.playing,
        probe
            .studio
            .notices
            .iter()
            .map(|notice| notice.text(Language::English))
            .collect::<Vec<_>>()
    );
    probe.studio.pause_display();
    let paused = probe.studio.frame;
    probe.wait_until(|studio| studio.first_frame() > paused);
    assert_eq!(probe.studio.frame, paused);
    probe.studio.toggle_reverse();
    let reverse = probe.play_frames(-1, 20);
    probe.studio.toggle_reverse();
    let held = probe.studio.frame;
    let received = probe.studio.count();
    probe.wait_until(|studio| studio.count() > received + 5);
    assert_eq!(probe.studio.frame, held);
    assert!(probe.studio.seeking.is_none());
    probe.studio.toggle_playback();
    let forward = probe.play_frames(1, 20);
    probe.studio.pause_display();
    probe.studio.seek(probe.studio.first_frame());
    probe.studio.return_to_live();
    probe.wait_until(|studio| studio.count() > received + 30);
    assert!(probe.studio.follow_live);
    assert!(probe.studio.seeking.is_none());
    probe.wait_until(|studio| !studio.live_connected);
    assert!(producer.0.wait().unwrap().success());
    let sdk_frames = std::fs::read_to_string(output.join("sdk-stats.csv"))
        .unwrap()
        .lines()
        .skip(1)
        .count();
    assert_eq!(probe.studio.count(), sdk_frames);
    let raw_path = probe.studio.capture_path.as_ref().unwrap();
    assert_eq!(raw_path.extension().unwrap(), "pvdstream");
    let expected = Recording::from_bytes(&std::fs::read(raw_path).unwrap()).unwrap();
    assert_eq!(expected.frame_count(), sdk_frames);
    for displayed in [&reverse, &forward] {
        assert_eq!(displayed.scene, expected.state_at(displayed.frame).unwrap());
        assert_eq!(
            memory_at(&displayed.analysis, displayed.frame),
            memory_at(&expected.analysis, displayed.frame)
        );
    }
    assert_eq!(
        probe.studio.receive_status.as_ref().unwrap().queued_bytes,
        0
    );
    let stats = probe
        .studio
        .timeline_recording()
        .unwrap()
        .disk_usage()
        .unwrap();
    assert!(stats.warning.is_none(), "{:?}", stats.warning);
    assert!(probe.max_history_bytes <= 64 << 20);
    let summary = serde_json::json!({
        "elapsed_seconds": probe.started.elapsed().as_secs_f64(),
        "frames": sdk_frames,
        "expired_pause": expired_pause,
        "first_after_expiration": retained_first,
        "reverse": reverse.progress,
        "forward": forward.progress,
        "verified_frames": [reverse.frame, forward.frame],
        "raw_capture": raw_path,
        "max_history_bytes": probe.max_history_bytes,
        "history_budget_bytes": stats.budget_bytes,
        "last_history_bytes": stats.disk_bytes,
        "last_retained_first": stats.first_frame,
        "last_retained_frames": stats.retained_frames,
        "last_segments": stats.segments,
        "max_queued_bytes": probe.max_queued_bytes,
        "received_bytes": probe.studio.receive_status.as_ref().unwrap().received_bytes,
        "final_queued_bytes": 0,
        "objects": probe.studio.state.objects.len(),
        "analysis_diagnostics": probe.studio.recording.as_ref().unwrap().diagnostics,
        "includes_native_window_or_gpu": false,
    });
    std::fs::write(
        output.join("summary.json"),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
    drop(probe);
    progress_dump.finish();
}

#[derive(Default)]
struct DigestWriter(blake3::Hasher);

impl std::fmt::Write for DigestWriter {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        self.0.update(text.as_bytes());
        Ok(())
    }
}

fn debug_digest(value: &impl std::fmt::Debug) -> String {
    let mut writer = DigestWriter::default();
    std::fmt::write(&mut writer, format_args!("{value:?}")).unwrap();
    writer.0.finalize().to_hex().to_string()
}

fn frame_digests(
    frame: usize,
    scene: &SceneState,
    analysis: &pvd_core::analysis::SharedAnalysis,
) -> serde_json::Value {
    let spans = analysis
        .profile_spans()
        .filter(|span| span.received_frame == frame)
        .collect::<Vec<_>>();
    let values = analysis
        .profile_values()
        .filter(|value| value.received_frame == frame)
        .collect::<Vec<_>>();
    serde_json::json!({
        "scene": debug_digest(scene),
        "memory": debug_digest(&memory_at(analysis, frame)),
        "spans": debug_digest(&spans),
        "values": debug_digest(&values),
        "zones": debug_digest(&(analysis.profile_zones().collect::<Vec<_>>(), analysis.profile_names().collect::<Vec<_>>())),
    })
}

fn retain_sample(
    output: &Path,
    cycle: usize,
    direction: &'static str,
    snapshot: PlaybackSnapshot,
) -> serde_json::Value {
    let digests = if snapshot.with_analysis {
        frame_digests(snapshot.frame, &snapshot.scene, &snapshot.analysis)
    } else {
        serde_json::json!({ "scene": debug_digest(&snapshot.scene) })
    };
    let sample = serde_json::json!({
        "cycle": cycle,
        "direction": direction,
        "frame": snapshot.frame,
        "scope": if snapshot.with_analysis { "complete" } else { "scene" },
        "digests": digests,
        "progress": snapshot.progress,
    });
    std::fs::write(
        output.join(format!("sample-{cycle:03}-{direction}.json")),
        serde_json::to_vec_pretty(&sample).unwrap(),
    )
    .unwrap();
    sample
}

fn history_files(path: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    if !path.exists() {
        return result;
    }
    for entry in std::fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            result.extend(history_files(&entry.path()));
        } else {
            result.push(entry.path());
        }
    }
    result
}

fn raw_current_state_oracle(
    output: &Path,
    raw_path: &Path,
    samples: &[serde_json::Value],
) -> usize {
    use pvd_protocol::{Endian, Event, rewrite::map_group_events};
    use std::io::{BufReader, Read};

    let unavailable = output.join("oracle-history-disabled");
    File::create(&unavailable).unwrap();
    let mut live = pvd_core::LiveRecording::with_history(pvd_core::live::HistoryConfig {
        directory: unavailable,
        disk_budget_bytes: 4096,
        min_free_disk_bytes: 0,
        ..Default::default()
    })
    .unwrap();
    let mut source = BufReader::new(File::open(raw_path).unwrap());
    let mut header = [0; 28];
    source.read_exact(&mut header).unwrap();
    let mut header_decoder = pvd_protocol::Decoder::default();
    assert!(header_decoder.feed(&header).unwrap().is_empty());
    let endian = header_decoder.header().unwrap().endian;
    live.feed(&header).unwrap();
    let mut targets = BTreeMap::<usize, Vec<&serde_json::Value>>::new();
    for sample in samples {
        targets
            .entry(sample["frame"].as_u64().unwrap() as usize)
            .or_default()
            .push(sample);
    }
    let mut strings = BTreeMap::new();
    let mut frame_count = 0;
    let mut display = SceneState::default();
    loop {
        let mut group = vec![0; 24];
        if source.read(&mut group[..1]).unwrap() == 0 {
            break;
        }
        source.read_exact(&mut group[1..]).unwrap();
        let length = match endian {
            Endian::Little => u32::from_le_bytes(group[..4].try_into().unwrap()),
            Endian::Big => u32::from_be_bytes(group[..4].try_into().unwrap()),
        } as usize;
        assert!(length <= pvd_protocol::Limits::default().max_group_bytes);
        group.resize(24 + length, 0);
        source.read_exact(&mut group[24..]).unwrap();
        let mut cuts = Vec::new();
        let mut count = 0u32;
        map_group_events(&group, endian, |event, raw| {
            count += 1;
            match event {
                Event::StringHandle { handle, value } => {
                    strings.insert(*handle, value.clone());
                }
                Event::EndSection { name, .. }
                    if strings.get(name).is_some_and(|value| value == "frame") =>
                {
                    if targets.contains_key(&frame_count) {
                        let end = raw.as_ptr() as usize + raw.len() - group.as_ptr() as usize;
                        cuts.push((end, count, frame_count));
                        count = 0;
                    }
                    frame_count += 1;
                }
                _ => {}
            }
            Ok(None)
        })
        .unwrap();
        let mut start = 24;
        for (end, events, frame) in cuts {
            let mut part = group[..24].to_vec();
            part.extend_from_slice(&group[start..end]);
            let length = (end - start) as u32;
            let (length, events) = match endian {
                Endian::Little => (length.to_le_bytes(), events.to_le_bytes()),
                Endian::Big => (length.to_be_bytes(), events.to_be_bytes()),
            };
            part[..4].copy_from_slice(&length);
            part[4..8].copy_from_slice(&events);
            live.feed(&part).unwrap();
            let (current, update) = live.take_update().unwrap();
            assert_eq!(current, frame);
            update.apply(&mut display);
            let recording = live.snapshot().unwrap();
            assert_eq!(recording.disk_usage().unwrap().disk_bytes, 0);
            assert!(recording.diagnostics.is_empty());
            let actual = frame_digests(frame, &display, &recording.analysis);
            for sample in targets.remove(&frame).unwrap() {
                if sample["scope"] == "scene" {
                    assert_eq!(
                        actual["scene"], sample["digests"]["scene"],
                        "raw scene mismatch at frame {frame}"
                    );
                } else {
                    assert_eq!(
                        actual, sample["digests"],
                        "raw current-state mismatch at frame {frame}"
                    );
                }
            }
            start = end;
        }
        if start < group.len() {
            if start != 24 {
                let mut part = group[..24].to_vec();
                part.extend_from_slice(&group[start..]);
                let length = (group.len() - start) as u32;
                let (length, count) = match endian {
                    Endian::Little => (length.to_le_bytes(), count.to_le_bytes()),
                    Endian::Big => (length.to_be_bytes(), count.to_be_bytes()),
                };
                part[..4].copy_from_slice(&length);
                part[4..8].copy_from_slice(&count);
                live.feed(&part).unwrap();
            } else {
                live.feed(&group).unwrap();
            }
            if let Some((_, update)) = live.take_update() {
                update.apply(&mut display);
            }
        }
        assert_eq!(live.frame_count(), frame_count);
    }
    assert!(targets.is_empty(), "raw did not reach every sampled frame");
    let recording = live.finish().unwrap();
    assert!(recording.diagnostics.is_empty());
    assert_eq!(recording.disk_usage().unwrap().disk_bytes, 0);
    assert_eq!(recording.frame_count(), frame_count);
    frame_count
}

#[test]
#[ignore = "Requires PVD_SDK34_SAMPLE; runs 600 seconds of repeated TCP history playback, with an optional 60..3600 second PVD_LONG_PLAYBACK_SECONDS override"]
fn real_sdk_long_tcp_repeated_playback_and_retention_match_permanent_raw() {
    run_long_tcp_playback(ProbeMode::FullAnalysis);
}

#[test]
#[ignore = "Requires PVD_SDK34_SAMPLE; runs 120 seconds of UI-demand TCP playback, configurable with PVD_LONG_PLAYBACK_SECONDS (60..3600)"]
fn real_sdk_tcp_ui_demand_scene_playback_and_paused_analysis_match_permanent_raw() {
    run_long_tcp_playback(ProbeMode::UiDemand);
}

fn run_long_tcp_playback(mode: ProbeMode) {
    let trace_enabled = std::env::var_os("PVD_TCP_REPLAY_TRACE").is_some_and(|value| {
        assert!(
            value.to_str() == Some("1"),
            "PVD_TCP_REPLAY_TRACE must be unset or 1"
        );
        true
    });
    if let Some(value) = std::env::var_os("PVD_TCP_PROGRESS_PROBE") {
        assert!(
            value.to_str() == Some("1"),
            "PVD_TCP_PROGRESS_PROBE must be unset or 1"
        );
    }
    let sample = std::env::var_os("PVD_SDK34_SAMPLE").expect("PVD_SDK34_SAMPLE is required");
    let output = PathBuf::from(
        std::env::var_os("PVD_PLAYBACK_PROBE_OUTPUT")
            .expect("PVD_PLAYBACK_PROBE_OUTPUT is required"),
    );
    let seconds = std::env::var("PVD_LONG_PLAYBACK_SECONDS")
        .map(|value| value.parse::<u64>().unwrap())
        .unwrap_or(if mode == ProbeMode::UiDemand {
            120
        } else {
            600
        });
    assert!((60..=3600).contains(&seconds));
    std::fs::create_dir_all(&output).unwrap();
    assert!(history_files(&output.join("history")).is_empty());
    let context = egui::Context::default();
    let history = output.join("history");
    let mut studio = Studio::default();
    studio.persist_layout = false;
    if mode == ProbeMode::UiDemand {
        studio.prefs.dock = egui_dock::DockState::new(vec![Panel::Memory]);
    }
    let server = NetworkSession::start_tracked(
        ListenConfig {
            address: "127.0.0.1:0".parse().unwrap(),
            capture_directory: Some(output.join("raw")),
            ..Default::default()
        },
        NetworkOptions {
            history: pvd_core::live::HistoryConfig {
                directory: history.clone(),
                disk_budget_bytes: 64 << 20,
                segment_frames: 40,
                min_free_disk_bytes: 0,
                ..Default::default()
            },
            receive_budget_bytes: 128 << 20,
            live_profile: trace_enabled,
            progress_only: !trace_enabled,
            ..Default::default()
        },
        context.clone(),
        &studio.background_jobs,
    )
    .unwrap();
    let port = server.address().port();
    let mut command = Command::new(sample);
    command
        .args([
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--frames",
            "1000000",
            "--duration-seconds",
            &seconds.to_string(),
            "--interval-ms",
            "16",
            "--flags",
            "all",
            "--rich",
            "--extra-boxes",
            "7",
            "--extra-static-boxes",
            "16000",
            "--continuous-motion",
            "--resident-allocations",
            "150000",
            "--analysis-batches-per-frame",
            "8",
        ])
        .arg("--stats-truth")
        .arg(output.join("sdk-stats.csv"))
        .arg("--analysis-truth")
        .arg(output.join("sdk-analysis.csv"))
        .stdout(Stdio::from(
            File::create(output.join("sdk.stdout.log")).unwrap(),
        ))
        .stderr(Stdio::from(
            File::create(output.join("sdk.stderr.log")).unwrap(),
        ));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut producer = Producer(command.spawn().unwrap());
    let started = Instant::now();
    let seek_trace = trace_enabled.then(|| {
        let origin_lower_seconds = started.elapsed().as_secs_f64();
        let trace = crate::display_probe::Trace::default();
        let origin_upper_seconds = started.elapsed().as_secs_f64();
        studio.display_trace = Some(trace.clone());
        (trace, origin_lower_seconds, origin_upper_seconds)
    });
    studio.force_full_playback_analysis = mode == ProbeMode::FullAnalysis;
    studio.prefs.language = Language::English;
    studio.server = Some(server);
    studio.recorded_timing = false;
    studio.playback_fps = 60.0;
    let progress = Some(Rc::new(RefCell::new(TcpProgressBoundaries::new())));
    let mut probe = Probe {
        studio,
        context,
        deadline: started + Duration::from_secs(seconds + 180),
        started,
        max_history_bytes: 0,
        max_queued_bytes: 0,
        displayed_frames: Vec::new(),
        telemetry: Some(File::create(output.join("timeline.jsonl")).unwrap()),
        next_sample: started,
        phase: "startup",
        mode,
        progress: progress.clone(),
    };
    let mut seek_trace_dump = seek_trace.map(
        |(trace, origin_lower_seconds, origin_upper_seconds)| TcpTraceDump {
            trace,
            output: output.clone(),
            mode,
            seconds,
            origin_lower_seconds,
            origin_upper_seconds,
            workers_drained: false,
            dumped: false,
        },
    );
    let mut progress_dump = progress.map(|boundaries| TcpProgressDump {
        boundaries,
        output: output.clone(),
        mode,
        seconds,
        dumped: false,
    });
    while probe.studio.count() < 10 || !probe.studio.has_history() {
        assert!(producer.0.try_wait().unwrap().is_none());
        probe.pump();
    }
    let mut snapshots = Vec::new();
    let mut cycles = Vec::new();
    let mut paused_reverse_upgrades = 0;
    let mut paused_forward_upgrades = 0;
    let mut exact_paused_upgrades = 0;
    let mut analysis_expiration_recoveries = 0;
    let mut next_cycle = started;
    while probe.started.elapsed().as_secs() + 20 < seconds {
        probe.phase = "live_between_cycles";
        while Instant::now() < next_cycle {
            probe.pump();
            assert!(probe.studio.live_connected);
        }
        if probe.started.elapsed().as_secs() + 20 >= seconds {
            break;
        }
        let cycle = cycles.len();
        let cycle_started = probe.started.elapsed().as_secs_f64();
        let mut expired_pause = None;
        let mut first_after_expiration = None;
        probe.studio.return_to_live();
        if cycle.is_multiple_of(2) && probe.started.elapsed().as_secs() + 35 < seconds {
            probe.phase = "wait_for_paused_frame_expiration";
            probe.studio.pause_display();
            let paused = probe.studio.frame;
            probe.wait_until(|studio| studio.first_frame() > paused);
            assert_eq!(probe.studio.frame, paused);
            expired_pause = Some(paused);
            first_after_expiration = Some(probe.studio.first_frame());
            probe.phase = "forward_after_expiration";
            probe.studio.toggle_playback();
            let snapshot = probe.play_frames(1, 8);
            snapshots.push(retain_sample(&output, cycle, "expired-forward", snapshot));
            probe.studio.return_to_live();
        }
        probe.phase = "reverse";
        probe.studio.toggle_reverse();
        let reverse = probe.play_frames(-1, 16);
        snapshots.push(retain_sample(&output, cycle, "reverse", reverse));
        if mode == ProbeMode::UiDemand {
            probe.phase = "paused_reverse_analysis";
            let paused = probe.pause_and_upgrade();
            exact_paused_upgrades += usize::from(paused.progress["exact_frame_upgrade"] == true);
            analysis_expiration_recoveries += paused.progress["expired_recoveries"]
                .as_array()
                .unwrap()
                .len();
            snapshots.push(retain_sample(&output, cycle, "paused-reverse", paused));
            paused_reverse_upgrades += 1;
        } else {
            probe.studio.pause_display();
        }
        let held = probe.studio.frame;
        let received = probe.studio.count();
        probe.phase = "paused_while_receiving";
        probe.wait_until(|studio| studio.count() > received + 5);
        assert_eq!(probe.studio.frame, held);
        assert!(probe.studio.seeking.is_none());
        probe.phase = "forward";
        probe.studio.toggle_playback();
        let forward = probe.play_frames(1, 16);
        snapshots.push(retain_sample(&output, cycle, "forward", forward));
        let forward_reached_live = probe.studio.follow_live;
        if mode == ProbeMode::UiDemand && !forward_reached_live {
            probe.phase = "paused_forward_analysis";
            let paused = probe.pause_and_upgrade();
            exact_paused_upgrades += usize::from(paused.progress["exact_frame_upgrade"] == true);
            analysis_expiration_recoveries += paused.progress["expired_recoveries"]
                .as_array()
                .unwrap()
                .len();
            snapshots.push(retain_sample(&output, cycle, "paused-forward", paused));
            paused_forward_upgrades += 1;
        } else {
            probe.studio.pause_display();
        }
        probe.studio.seek(probe.studio.first_frame());
        probe.studio.return_to_live();
        probe.phase = "cancel_seek_and_return_to_live";
        let received = probe.studio.count();
        probe.wait_until(|studio| studio.count() > received + 5);
        assert!(probe.studio.follow_live);
        assert!(probe.studio.seeking.is_none());
        cycles.push(serde_json::json!({
            "cycle": cycle,
            "started_seconds": cycle_started,
            "finished_seconds": probe.started.elapsed().as_secs_f64(),
            "expired_pause": expired_pause,
            "first_after_expiration": first_after_expiration,
            "decoded": probe.studio.count(),
            "forward_reached_live": forward_reached_live,
        }));
        std::fs::write(
            output.join("cycles.json"),
            serde_json::to_vec_pretty(&cycles).unwrap(),
        )
        .unwrap();
        next_cycle = started + Duration::from_secs((cycle as u64 + 1) * 30);
    }
    probe.phase = "final_live_and_drain";
    probe.studio.return_to_live();
    probe.wait_until(|studio| !studio.live_connected);
    assert!(producer.0.wait().unwrap().success());
    let capture_seconds = probe.started.elapsed().as_secs_f64();
    assert!(capture_seconds >= seconds as f64);
    assert!(cycles.len() >= (seconds / 30 - 1) as usize);
    assert!(cycles.last().unwrap()["started_seconds"].as_f64().unwrap() >= seconds as f64 - 65.0);
    if mode == ProbeMode::UiDemand {
        assert!(paused_reverse_upgrades > 0);
        assert!(paused_forward_upgrades > 0);
        assert!(exact_paused_upgrades > 0);
    }
    let sdk_frames = std::fs::read_to_string(output.join("sdk-stats.csv"))
        .unwrap()
        .lines()
        .skip(1)
        .count();
    assert_eq!(probe.studio.count(), sdk_frames);
    assert_eq!(
        probe.studio.receive_status.as_ref().unwrap().queued_bytes,
        0
    );
    let raw_path = probe.studio.capture_path.as_ref().unwrap().clone();
    assert_eq!(raw_path.extension().unwrap(), "pvdstream");
    let stats = probe
        .studio
        .timeline_recording()
        .unwrap()
        .disk_usage()
        .unwrap();
    assert!(stats.warning.is_none(), "{:?}", stats.warning);
    assert!(probe.max_history_bytes <= 64 << 20);
    assert!(
        probe
            .studio
            .recording
            .as_ref()
            .unwrap()
            .diagnostics
            .is_empty()
    );
    let mut summary = serde_json::json!({
        "requested_duration_seconds": seconds,
        "mode": format!("{mode:?}"),
        "memory_panel_visible": mode == ProbeMode::UiDemand,
        "paused_reverse_upgrades": paused_reverse_upgrades,
        "paused_forward_upgrades": paused_forward_upgrades,
        "exact_paused_upgrades": exact_paused_upgrades,
        "analysis_expiration_recoveries": analysis_expiration_recoveries,
        "capture_and_drain_seconds": capture_seconds,
        "frames": sdk_frames,
        "cycles": cycles,
        "samples": snapshots,
        "raw_capture": raw_path,
        "max_history_bytes": probe.max_history_bytes,
        "history_budget_bytes": stats.budget_bytes,
        "last_history_bytes": stats.disk_bytes,
        "last_retained_first": stats.first_frame,
        "last_retained_frames": stats.retained_frames,
        "max_queued_bytes": probe.max_queued_bytes,
        "received_bytes": probe.studio.receive_status.as_ref().unwrap().received_bytes,
        "final_queued_bytes": 0,
        "objects": probe.studio.state.objects.len(),
        "analysis_diagnostics": probe.studio.recording.as_ref().unwrap().diagnostics,
        "includes_native_window_or_gpu": false,
        "sample_hash": "BLAKE3 of deterministic Rust Debug output; scene scope compares full SceneState only; complete scope additionally compares frame memory/churn, frame-filtered profile spans/values and zones",
        "oracle_verified": false,
    });
    std::fs::write(
        output.join("capture-summary.json"),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
    let cleanup_started = Instant::now();
    let mut pending_shutdown_updates = 0;
    while !probe.studio.poll_shutdown() {
        pending_shutdown_updates += 1;
        assert!(cleanup_started.elapsed() < Duration::from_secs(15));
        std::thread::sleep(Duration::from_millis(1));
    }
    summary["registered_shutdown_seconds"] =
        serde_json::json!(cleanup_started.elapsed().as_secs_f64());
    summary["pending_shutdown_updates"] = serde_json::json!(pending_shutdown_updates);
    assert_eq!(probe.studio.background_jobs.active_count(), 0);
    drop(probe);
    if let Some(trace) = seek_trace_dump.as_mut() {
        trace.finish();
    }
    let remaining = history_files(&history);
    assert!(
        remaining.is_empty(),
        "temporary files remain: {remaining:?}"
    );
    summary["history_files_after_release"] = serde_json::json!(remaining.len());
    std::fs::write(output.join("phase.txt"), "independent_raw_oracle").unwrap();
    assert_eq!(
        raw_current_state_oracle(&output, &raw_path, &snapshots),
        sdk_frames
    );
    if seconds <= 90 {
        let expected = Recording::from_path(&raw_path).unwrap();
        assert_eq!(expected.frame_count(), sdk_frames);
        for displayed in &snapshots {
            let frame = displayed["frame"].as_u64().unwrap() as usize;
            let actual = frame_digests(
                frame,
                &expected.state_at(frame).unwrap(),
                &expected.analysis,
            );
            // Final-file metadata can include profile definitions received after this frame.
            for component in ["scene", "memory", "spans", "values"] {
                if displayed["scope"] == "scene" && component != "scene" {
                    continue;
                }
                assert_eq!(
                    actual[component], displayed["digests"][component],
                    "raw memory-history {component} mismatch at frame {frame}"
                );
            }
        }
    }
    summary["oracle_verified"] = serde_json::json!(true);
    summary["oracle"] = serde_json::json!(
        "permanent raw independently decoded into current-state updates at sample frame boundaries; history storage disabled and disk bytes asserted zero; no history replay"
    );
    summary["short_run_additional_memory_history_oracle"] = serde_json::json!(seconds <= 90);
    summary["elapsed_seconds"] = serde_json::json!(started.elapsed().as_secs_f64());
    std::fs::write(
        output.join("summary.json"),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
    std::fs::write(output.join("phase.txt"), "complete").unwrap();
    if let Some(progress) = progress_dump.as_mut() {
        progress.finish();
    }
}
