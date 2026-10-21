use pvd_core::{
    LiveRecording, SceneState,
    live::{HistoryConfig, ReplayCursor},
};
use std::{fs::File, io::Read, time::Instant};

struct DigestWriter(blake3::Hasher);

impl std::io::Write for DigestWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn digest(value: &impl serde::Serialize) -> Result<String, pvd_core::Error> {
    let output = postcard::to_io(value, DigestWriter(blake3::Hasher::new()))?;
    Ok(output.0.finalize().to_hex().to_string())
}

fn scene_digest(value: &SceneState) -> Result<String, pvd_core::Error> {
    digest(&(
        &value.objects,
        &value.debug_draw,
        &value.contacts,
        &value.errors,
    ))
}

fn main() -> Result<(), pvd_core::Error> {
    let args: Vec<_> = std::env::args_os().collect();
    let input_path = args
        .get(1)
        .ok_or("usage: cold_seek INPUT HISTORY_DIR [--frames=150,149,800]")?;
    let directory = args.get(2).ok_or("missing history directory")?;
    let value = |name: &str| {
        args.iter()
            .filter_map(|argument| argument.to_str())
            .find_map(|argument| argument.strip_prefix(name))
    };
    let option = |name: &str, default: usize| -> Result<usize, pvd_core::Error> {
        value(name).map_or(Ok(default), |value| Ok(value.parse()?))
    };
    let frames: Vec<usize> = value("--frames=")
        .unwrap_or("150,151,149,800,5,480,900,150")
        .split(',')
        .map(str::parse)
        .collect::<Result<_, _>>()?;
    let fresh = args.iter().any(|argument| argument == "--fresh-cursor");
    let verify = args.iter().any(|argument| argument == "--verify");
    let mut live = LiveRecording::with_history(HistoryConfig {
        directory: directory.into(),
        min_free_disk_bytes: 0,
        segment_frames: option("--segment-frames=", 40)?,
        disk_budget_bytes: (option("--budget-mib=", 8192)? as u64)
            .checked_mul(1 << 20)
            .ok_or("history budget overflow")?,
        ..Default::default()
    })?;
    let load_started = Instant::now();
    let mut input = File::open(input_path)?;
    let mut buffer = [0; 65536];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        live.feed(&buffer[..n])?;
    }
    let recording = live.finish()?;
    let load_seconds = load_started.elapsed().as_secs_f64();
    eprintln!(
        "load_seconds={load_seconds:.6}, disk={:?}, memory={:?}",
        recording.disk_usage(),
        recording.history_memory_usage()
    );
    let mut cursor = ReplayCursor::default();
    let mut displayed = SceneState::default();
    let mut displayed_analysis = None;
    let mut checks = Vec::new();
    for (sample, frame) in frames.into_iter().enumerate() {
        if fresh {
            cursor.clear();
            displayed = SceneState::default();
        }
        let (replay, metrics) = cursor.seek_profiled(&recording, frame, || false)?;
        let changed_objects = replay.update.changed_objects().count();
        let applied = Instant::now();
        replay.update.apply(&mut displayed);
        let apply_ms = applied.elapsed().as_secs_f64() * 1000.0;
        let ms = |duration: std::time::Duration| duration.as_secs_f64() * 1000.0;
        println!(
            "{}",
            serde_json::json!({
                "sample": sample,
                "frame": frame,
                "fresh_cursor": fresh || sample == 0,
                "seek_ms": ms(metrics.total),
                "index_ms": ms(metrics.index),
                "scene_ms": ms(metrics.scene),
                "analysis_ms": ms(metrics.analysis),
                "baseline_io_ms": ms(metrics.baseline_io),
                "baseline_decode_ms": ms(metrics.baseline_decode),
                "baseline_pages_ms": ms(metrics.baseline_pages),
                "resident_domain_bytes": metrics.resident_domain_bytes,
                "analysis_root_hits": metrics.analysis_root_hits,
                "baseline_sharing_ms": ms(metrics.baseline_sharing),
                "cache_maintenance_ms": ms(metrics.cache_maintenance),
                "baseline_bytes": metrics.baseline_bytes,
                "baseline_loads": metrics.baseline_loads,
                "analysis_frames": metrics.analysis_frames,
                "analysis_inputs": metrics.analysis_inputs,
                "cache_hits": metrics.cache_hits,
                "shared_cache_entries": metrics.shared_cache_entries,
                "shared_cache_bytes": metrics.shared_cache_bytes,
                "cursor_cache_bytes": metrics.cursor_cache_bytes,
                "apply_ms": apply_ms,
                "objects": displayed.objects.len(),
                "changed_objects": changed_objects,
                "analysis_bytes": replay.analysis.accounted_bytes(),
            })
        );
        if verify {
            checks.push((frame, scene_digest(&displayed)?, digest(&replay.analysis)?));
        }
        displayed_analysis = Some(replay.analysis);
    }
    for (frame, scene, analysis) in checks {
        if scene_digest(&recording.state_at(frame)?)? != scene
            || digest(&recording.analysis_at(frame)?)? != analysis
        {
            return Err(format!("reference replay differs at frame {frame}").into());
        }
        eprintln!("reference_match frame={frame} scene={scene} analysis={analysis}");
    }
    drop(displayed_analysis);
    Ok(())
}
