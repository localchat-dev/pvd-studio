use pvd_core::{
    LiveRecording, SceneState,
    live::{HistoryConfig, ReplayCursor},
};
use std::{fs::File, io::Read, time::Instant};

fn main() -> Result<(), pvd_core::Error> {
    let args: Vec<_> = std::env::args_os().collect();
    let input = args
        .get(1)
        .ok_or("usage: replay_latency INPUT HISTORY_DIR [--cursor]")?;
    let directory = args.get(2).ok_or("missing history directory")?;
    let option = |name: &str, default: usize| -> Result<usize, pvd_core::Error> {
        args.iter()
            .filter_map(|argument| argument.to_str())
            .find_map(|argument| argument.strip_prefix(name))
            .map_or(Ok(default), |value| Ok(value.parse()?))
    };
    let mut input = File::open(input)?;
    let mut live = LiveRecording::with_history(HistoryConfig {
        directory: directory.into(),
        min_free_disk_bytes: 0,
        segment_frames: option("--segment-frames=", 600)?,
        disk_budget_bytes: (option("--budget-mib=", 8192)? as u64)
            .checked_mul(1 << 20)
            .ok_or("history budget overflow")?,
        ..Default::default()
    })?;
    let started = Instant::now();
    let mut buffer = [0; 65536];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        live.feed(&buffer[..n])?;
    }
    let recording = live.finish()?;
    eprintln!("disk_usage={:?}", recording.disk_usage());
    eprintln!(
        "frames={}, load_seconds={:.6}",
        recording.frame_count(),
        started.elapsed().as_secs_f64()
    );
    if recording.frames.len() < 8 {
        return Err("at least eight retained frames are required".into());
    }
    println!("frame,seek_ms,apply_ms,changed_objects,objects,analysis_bytes,mode");
    let cursor_mode = args.iter().any(|argument| argument == "--cursor");
    let mut cursor = ReplayCursor::default();
    let mut displayed = SceneState::default();
    let mut displayed_analysis = None;
    let middle =
        option("--start-frame=", recording.frames.len() / 2)?.min(recording.frames.len() - 1);
    let forward_count = option("--forward-count=", 20.min(recording.frames.len() / 4))?
        .max(1)
        .min(recording.frames.len() - middle);
    let reverse_end = middle + forward_count - 1;
    let reverse_start = reverse_end.saturating_sub(option("--reverse-count=", 40)?);
    let forward = recording.frames.iter().skip(middle).take(forward_count);
    let reverse = recording
        .frames
        .iter()
        .skip(reverse_start)
        .take(reverse_end - reverse_start)
        .rev();
    for frame in forward.chain(reverse).map(|frame| frame.index) {
        if cursor_mode {
            let start = Instant::now();
            let replay = cursor.seek(&recording, frame, || false)?;
            let seek_ms = start.elapsed().as_secs_f64() * 1000.;
            let changed = replay.update.changed_objects().count();
            let start = Instant::now();
            replay.update.apply(&mut displayed);
            let apply_ms = start.elapsed().as_secs_f64() * 1000.;
            println!(
                "{frame},{seek_ms:.6},{apply_ms:.6},{changed},{},{},cursor",
                displayed.objects.len(),
                replay.analysis.accounted_bytes()
            );
            displayed_analysis = Some(replay.analysis);
            continue;
        }
        let start = Instant::now();
        let scene = recording.state_at(frame)?;
        let analysis = recording.analysis_at(frame)?;
        println!(
            "{frame},{:.6},0,{},{},{},full",
            start.elapsed().as_secs_f64() * 1000.,
            scene.objects.len(),
            scene.objects.len(),
            analysis.accounted_bytes()
        );
    }
    drop(displayed_analysis);
    Ok(())
}
