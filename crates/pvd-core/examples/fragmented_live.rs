use pvd_core::{LiveRecording, Recording, SceneState, live::HistoryConfig};
use std::{env, fs, path::PathBuf, time::Instant};

fn main() {
    let args: Vec<_> = env::args().collect();
    assert!(
        args.len() >= 3,
        "fragmented_live <capture> <history_directory> [repetitions] [chunk_sizes]"
    );
    let bytes = fs::read(&args[1]).unwrap();
    let directory = PathBuf::from(&args[2]);
    let repeats: usize = args.get(3).map_or(2, |v| v.parse().unwrap());
    let chunks: Vec<usize> = args
        .get(4)
        .map_or("1024,8191,65536", String::as_str)
        .split(',')
        .map(|v| v.parse().unwrap())
        .collect();
    assert!(chunks.iter().all(|size| *size > 0));
    let mut expected_state = None;
    println!(
        "repeat,chunk_bytes,input_bytes,feed_calls,frames,updates,changed_objects,explicit_snapshots,feed_ms,take_update_ms,apply_ms,finish_ms,final_seek_ms,cleanup_ms,max_feed_ms,history_bytes,final_objects"
    );
    for repetition in 0..repeats {
        for &chunk_size in &chunks {
            let mut live = LiveRecording::with_history(HistoryConfig {
                directory: directory.clone(),
                ..HistoryConfig::default()
            })
            .unwrap();
            let mut state = SceneState::default();
            let mut feed_calls = 0;
            let mut updates = 0;
            let mut changed_objects = 0;
            let mut feed_ns = 0;
            let mut take_update_ns = 0;
            let mut apply_ns = 0;
            let mut max_feed_ns = 0;
            for chunk in bytes.chunks(chunk_size) {
                let start = Instant::now();
                live.feed(chunk).unwrap();
                let elapsed = start.elapsed().as_nanos();
                feed_ns += elapsed;
                max_feed_ns = max_feed_ns.max(elapsed);
                feed_calls += 1;
                let start = Instant::now();
                let update = live.take_update();
                take_update_ns += start.elapsed().as_nanos();
                if let Some((_, update)) = update {
                    updates += 1;
                    changed_objects += update.changed_objects().count();
                    let start = Instant::now();
                    update.apply(&mut state);
                    apply_ns += start.elapsed().as_nanos();
                }
            }
            let start = Instant::now();
            let recording = live.finish().unwrap();
            let finish_ms = start.elapsed().as_secs_f64() * 1000.0;
            let frames = recording.frame_count();
            let history_bytes = recording.disk_usage().unwrap().disk_bytes;
            let start = Instant::now();
            let final_state = recording.state_at(frames - 1).unwrap();
            let seek_ms = start.elapsed().as_secs_f64() * 1000.0;
            assert!(
                state == final_state,
                "published state must match the final retained frame"
            );
            if let Some(expected) = &expected_state {
                assert!(
                    &final_state == expected,
                    "chunk size must not change final state"
                );
            } else {
                expected_state = Some(final_state.clone());
            }
            let start = Instant::now();
            drop(recording);
            let cleanup_ms = start.elapsed().as_secs_f64() * 1000.0;
            println!(
                "{repetition},{chunk_size},{},{feed_calls},{frames},{updates},{changed_objects},0,{:.3},{:.3},{:.3},{finish_ms:.3},{seek_ms:.3},{cleanup_ms:.3},{:.3},{history_bytes},{}",
                bytes.len(),
                feed_ns as f64 / 1e6,
                take_update_ns as f64 / 1e6,
                apply_ns as f64 / 1e6,
                max_feed_ns as f64 / 1e6,
                final_state.objects.len()
            );
        }
    }
    let mut cancelled_at = None;
    let result = Recording::from_path_with_progress(&args[1], |loaded, total| {
        if loaded >= total / 2 {
            cancelled_at = Some(Instant::now());
            false
        } else {
            true
        }
    });
    let cancellation_ms = cancelled_at.unwrap().elapsed().as_secs_f64() * 1000.0;
    assert!(result.is_err_and(|error| error.to_string().contains("cancelled")));
    println!("cancellation,load_callback_at_half_input,return_ms={cancellation_ms:.3}");
}
