use pvd_core::{
    LiveRecording, SceneState,
    analysis::{
        SharedAnalysis,
        view::{MemorySource, ProfileMetadataSource, ProfileSource},
    },
    live::HistoryConfig,
};
use std::{
    fs::File,
    io::Read,
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Default)]
struct AnalysisObservations {
    samples: u64,
    max_accounted_bytes: usize,
    max_live_allocations: usize,
}

impl AnalysisObservations {
    fn observe(&mut self, analysis: &SharedAnalysis) {
        self.samples += 1;
        self.max_accounted_bytes = self.max_accounted_bytes.max(analysis.accounted_bytes());
        self.max_live_allocations = self
            .max_live_allocations
            .max(analysis.memory_snapshot().summary().live_allocations);
    }
}

fn main() -> Result<(), pvd_core::Error> {
    let args: Vec<_> = std::env::args_os().collect();
    let input = args
        .get(1)
        .ok_or("usage: throughput INPUT HISTORY_DIR [--profile] [--no-history]")?;
    let directory = PathBuf::from(args.get(2).ok_or("missing history directory")?);
    let mut live = LiveRecording::with_history(HistoryConfig {
        directory,
        min_free_disk_bytes: if args.iter().any(|v| v == "--no-history") {
            u64::MAX
        } else {
            512 << 20
        },
        ..Default::default()
    })?;
    let metrics = args
        .iter()
        .any(|v| v == "--profile")
        .then(|| live.enable_metrics());
    let mut input = File::open(input)?;
    let mut buffer = vec![0; 64 << 10];
    let mut bytes = 0u64;
    let mut state = SceneState::default();
    let mut latest = None;
    let started = Instant::now();
    let mut last_publish = started;
    let mut updates = 0u64;
    let mut apply_seconds = 0.0;
    let mut analysis_observations = AnalysisObservations::default();
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes += count as u64;
        live.feed(&buffer[..count])?;
        if last_publish.elapsed() >= Duration::from_micros(16_667) && live.frame_count() > 0 {
            let recording = live.snapshot()?;
            analysis_observations.observe(&recording.analysis);
            latest = Some(recording);
            if let Some((_, update)) = live.take_update() {
                let apply_start = Instant::now();
                update.apply(&mut state);
                apply_seconds += apply_start.elapsed().as_secs_f64();
                updates += 1;
            }
            last_publish = Instant::now();
        }
    }
    if let Some((_, update)) = live.take_update() {
        update.apply(&mut state);
    }
    let recording = live.finish()?;
    analysis_observations.observe(&recording.analysis);
    let elapsed = started.elapsed().as_secs_f64();
    let analysis = &recording.analysis;
    let profile = analysis.profile_summary();
    let memory = analysis.memory_snapshot();
    let summary = memory.summary();
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "input_bytes": bytes,
            "elapsed_seconds": elapsed,
            "frames": recording.frame_count(),
            "objects": state.objects.len(),
            "updates": updates,
            "display_apply_seconds": apply_seconds,
            "metrics": metrics.as_ref().map(|m| m.snapshot()),
            "published_frames": latest.as_ref().map(|r| r.frame_count()),
            "diagnostics": &recording.diagnostics,
            "analysis": {
                "samples": analysis_observations.samples,
                "sampled_max_accounted_bytes": analysis_observations.max_accounted_bytes,
                "sampled_max_live_allocations": analysis_observations.max_live_allocations,
                "accounted_bytes": analysis.accounted_bytes(),
                "first_retained_frame": profile.first_retained_frame,
                "profile_zones": analysis.profile_zones().count(),
                "profile_spans": profile.complete_spans,
                "profile_values": analysis.profile_values().len(),
                "open_spans": analysis.open_spans(),
                "unmatched_stops": profile.unmatched_stops,
                "legacy_cuda_buffers": profile.legacy_cuda_buffers,
                "decoding_stopped": recording.diagnostics.iter().any(|message| message.starts_with("Analysis decoding stopped")),
                "memory": {
                    "live_allocations": summary.live_allocations,
                    "live_bytes": summary.live_bytes,
                    "peak_bytes": summary.peak_bytes,
                    "total_allocated_bytes": summary.total_allocated_bytes,
                    "allocations": summary.allocations,
                    "frees": summary.frees,
                    "unmatched_frees": summary.unmatched_frees,
                    "replaced_addresses": summary.replaced_addresses,
                    "retained_events": memory.event_count(),
                    "samples": memory.sample_count()
                }
            }
        }))?
    );
    Ok(())
}
