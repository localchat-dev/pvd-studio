use pvd_core::{
    analysis::ProfileSpan,
    profile_curves::SourceIdentity,
    profile_disk_index::{self as disk, CpuSpanIndex, IndexLimits, Phase, Progress},
    profile_timeline::{self as timeline, TickRange, WindowLimits, WindowReport, WindowRequest},
};
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Instant,
};

struct StageTimes {
    current: Phase,
    started: Instant,
    durations: [f64; 5],
    verified: u64,
}
impl StageTimes {
    fn new(current: Phase) -> Self {
        Self {
            current,
            started: Instant::now(),
            durations: [0.; 5],
            verified: 0,
        }
    }
    fn slot(phase: Phase) -> usize {
        match phase {
            Phase::Decode => 0,
            Phase::Metadata => 1,
            Phase::Merge => 2,
            Phase::VerifySource => 3,
            Phase::Query => 4,
        }
    }
    fn observe(&mut self, progress: Progress) {
        if progress.phase != self.current {
            self.durations[Self::slot(self.current)] +=
                self.started.elapsed().as_secs_f64() * 1000.;
            self.started = Instant::now();
            self.current = progress.phase;
        }
        if progress.phase == Phase::VerifySource {
            self.verified = self.verified.max(progress.completed);
        }
    }
    fn finish(mut self) -> serde_json::Value {
        self.durations[Self::slot(self.current)] += self.started.elapsed().as_secs_f64() * 1000.;
        serde_json::json!({
            "decode_ms":self.durations[0], "metadata_ms":self.durations[1],
            "merge_ms":self.durations[2], "source_hash_ms":self.durations[3],
            "query_ms":self.durations[4], "source_bytes_verified":self.verified,
            "scope":"callback stage intervals; includes intervening work; functional, not isolated"
        })
    }
}

struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../output")
            .join(format!("long-span-index-{}", std::process::id()));
        assert!(!path.exists());
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}

fn fields(span: &ProfileSpan) -> (u64, u16, u32, u64, u64, u64, usize) {
    (
        span.zone,
        span.event,
        span.thread,
        span.context,
        span.start,
        span.end,
        span.received_frame,
    )
}
fn request(start: u64, end: u64) -> WindowRequest {
    WindowRequest {
        ticks: TickRange { start, end },
        thread: None,
        context: None,
        limits: Default::default(),
    }
}
fn checked_window(index: &CpuSpanIndex, source: &Path, query: WindowRequest) -> WindowReport {
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    let expected = timeline::window_streaming(
        &mut File::open(source).unwrap(),
        &index.info().identity,
        query,
        &cancel,
        |_| true,
    )
    .unwrap();
    let streaming_ms = started.elapsed().as_secs_f64() * 1000.;
    let mut stages = StageTimes::new(Phase::VerifySource);
    let started = Instant::now();
    let actual = index
        .window(query, &cancel, |progress| {
            assert!(!matches!(progress.phase, Phase::Decode | Phase::Merge));
            stages.observe(progress);
            true
        })
        .unwrap();
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.;
    assert_eq!(stages.verified, index.info().identity.bytes);
    assert_eq!(
        actual.spans.iter().map(fields).collect::<Vec<_>>(),
        expected.spans.iter().map(fields).collect::<Vec<_>>()
    );
    assert_eq!(actual.names, expected.names);
    assert_eq!(actual.identity, expected.identity);
    assert_eq!(actual.clock, expected.clock);
    assert_eq!(actual.global_pairing, expected.global_pairing);
    assert_eq!(actual.legacy_cuda_buffers, expected.legacy_cuda_buffers);
    println!(
        "disk_long_window {}",
        serde_json::json!({
            "start":query.ticks.start,"end":query.ticks.end,"thread":query.thread,"context":query.context,
            "spans":actual.spans.len(),"streaming_ms":streaming_ms,"index_total_ms":elapsed_ms,"stages":stages.finish()
        })
    );
    actual
}

#[test]
#[ignore = "requires preserved 600-second SDK raw/analysis CSV and 1 GiB scratch quota; functional index validation"]
fn long_sdk_disk_index_matches_streaming_early_middle_tail_and_cleans_its_lease() {
    let root =
        PathBuf::from(std::env::var_os("PVD_TIMELINE_LONG_ROOT").expect("PVD_TIMELINE_LONG_ROOT"));
    let source = root.join("raw/capture-1792077385976907200-8920-1-0.pvdstream");
    let csv = std::fs::read_to_string(root.join("sdk-analysis.csv")).unwrap();
    let steps: Vec<_> = csv
        .lines()
        .filter(|line| line.starts_with("step,"))
        .map(|line| line.split(',').collect::<Vec<_>>())
        .collect();
    assert_eq!(steps.len(), 19293);
    assert_eq!(steps.last().unwrap()[6], "154344");
    let expected_hash = "9a99d0525325e7ad2085bb2db7e175eb53e4d0be331d68ff1373f41bb9a181f1";
    let mut hash = [0u8; 32];
    for (index, byte) in hash.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&expected_hash[index * 2..index * 2 + 2], 16).unwrap();
    }
    let identity = SourceIdentity {
        blake3: hash,
        bytes: 251301039,
        frames: 19293,
    };
    let workspace = Workspace::new();
    let cancelled = AtomicBool::new(false);
    let limits = IndexLimits {
        memory_bytes: 32 << 20,
        ..Default::default()
    };
    let cancelled_workspace = workspace.0.join("cancelled-build");
    let mut at = 0;
    assert!(
        disk::build_index(
            &source,
            &cancelled_workspace,
            &identity,
            limits,
            &cancelled,
            |p| {
                if p.phase == Phase::Decode {
                    at = p.completed;
                    return p.completed < 4 << 20;
                }
                true
            }
        )
        .is_err()
    );
    assert!(at >= 4 << 20 && at < identity.bytes);
    assert!(!cancelled_workspace.exists());
    let path = workspace.0.join("complete-index");
    let mut stages = StageTimes::new(Phase::Decode);
    let started = Instant::now();
    let index = disk::build_index(&source, &path, &identity, limits, &cancelled, |p| {
        stages.observe(p);
        true
    })
    .unwrap();
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.;
    let info = index.info();
    assert_eq!(info.identity, identity);
    assert_eq!(info.global_pairing.complete_spans, 3_275_660);
    assert_eq!(info.global_pairing.open_starts, 19295);
    assert_eq!(info.global_pairing.unmatched_stops, 19293);
    assert!(info.initial_runs > 8);
    assert_eq!(info.merge_passes, 2);
    assert!(info.peak_disk_bytes > info.disk_bytes && info.peak_disk_bytes <= limits.disk_bytes);
    println!(
        "disk_long_build {}",
        serde_json::json!({
            "bytes":identity.bytes,"frames":identity.frames,"complete":info.global_pairing.complete_spans,
            "pending":info.global_pairing.open_starts,"unmatched":info.global_pairing.unmatched_stops,
            "runs":info.initial_runs,"passes":info.merge_passes,"disk_bytes":info.disk_bytes,"peak_disk_bytes":info.peak_disk_bytes,
            "total_ms":elapsed_ms,"stages":stages.finish()
        })
    );
    for frame in [1usize, 9646, 19292] {
        let pairs: u64 = steps[frame][6].parse().unwrap();
        let before: u64 = steps[frame - 1][6].parse().unwrap();
        assert_eq!(pairs - before, 8);
        let context = frame as u64 * 64 + 3;
        let query = WindowRequest {
            context: Some(context),
            ..request(0, u64::MAX)
        };
        let full = checked_window(&index, &source, query);
        assert_eq!(full.spans.len(), 1);
        let span = &full.spans[0];
        assert_eq!(span.received_frame, frame);
        assert_eq!(span.context, context);
        assert_eq!(
            full.names[&(span.zone, span.event)].event.as_deref(),
            Some("PvdStudio.AnalysisBatch")
        );
        assert!(span.end - span.start >= 2);
        let tick = span.start + (span.end - span.start) / 2;
        let point = checked_window(
            &index,
            &source,
            WindowRequest {
                thread: Some(span.thread),
                ..WindowRequest {
                    ticks: TickRange {
                        start: tick,
                        end: tick,
                    },
                    ..query
                }
            },
        );
        assert_eq!(
            point.spans.iter().map(fields).collect::<Vec<_>>(),
            vec![fields(span)]
        );
        assert!(point.spans[0].start < tick && point.spans[0].end > tick);
        let all_threads = checked_window(&index, &source, request(tick, tick));
        assert!(
            all_threads
                .spans
                .iter()
                .any(|other| fields(other) == fields(span))
        );
    }
    let limited = WindowRequest {
        limits: WindowLimits {
            spans: 100,
            bytes: 1 << 20,
        },
        ..request(0, u64::MAX)
    };
    assert!(
        index
            .window(limited, &cancelled, |_| true)
            .unwrap_err()
            .to_string()
            .contains("narrow")
    );
    let mut verified = 0;
    assert!(
        index
            .window(request(0, u64::MAX), &cancelled, |p| {
                if p.phase == Phase::VerifySource {
                    verified = p.completed;
                    return p.completed < 4 << 20;
                }
                true
            })
            .is_err()
    );
    assert!(verified >= 4 << 20 && verified < identity.bytes);
    let still_valid = index
        .window(
            WindowRequest {
                context: Some(67),
                ..request(0, u64::MAX)
            },
            &cancelled,
            |_| true,
        )
        .unwrap();
    assert_eq!(still_valid.spans.len(), 1);
    drop(index);
    assert!(!path.exists());
    assert_eq!(std::fs::read_dir(&workspace.0).unwrap().count(), 0);
    println!(
        "disk_long_cleanup {}",
        serde_json::json!({"index_files":0,"build_cancelled_after_bytes":at,"query_cancelled_after_bytes":verified,"scope":"one functional long run; no quiet or OS cache control"})
    );
}
