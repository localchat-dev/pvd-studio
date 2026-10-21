use super::*;

struct DigestWriter(blake3::Hasher);

impl Write for DigestWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn digest(value: &impl Serialize) -> String {
    let output = postcard::to_io(value, DigestWriter(blake3::Hasher::new())).unwrap();
    output.0.finalize().to_hex().to_string()
}

#[test]
#[ignore = "requires an existing real SDK capture; logical cold cache is independent of OS disk caching"]
fn real_capture_profiles_shared_and_logically_cold_seeks() {
    let input = std::env::var_os("PVD_COLD_SEEK_INPUT").expect("PVD_COLD_SEEK_INPUT is required");
    let output =
        std::env::var_os("PVD_COLD_SEEK_DIRECTORY").expect("PVD_COLD_SEEK_DIRECTORY is required");
    let frame = std::env::var("PVD_COLD_SEEK_FRAME")
        .unwrap_or_else(|_| "150".into())
        .parse::<usize>()
        .unwrap();
    let mut input = File::open(input).unwrap();
    let started = Instant::now();
    let mut live = crate::LiveRecording::with_history(HistoryConfig {
        directory: output.into(),
        min_free_disk_bytes: 0,
        segment_frames: 40,
        ..Default::default()
    })
    .unwrap();
    let mut buffer = [0; 65536];
    loop {
        let n = input.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        live.feed(&buffer[..n]).unwrap();
    }
    let mut recording = live.finish().unwrap();
    eprintln!(
        "load_seconds={:.6}, disk={:?}, memory={:?}",
        started.elapsed().as_secs_f64(),
        recording.disk_usage(),
        recording.history_memory_usage()
    );
    let mut expected = None;
    eprintln!(
        "cache_loaded={:?}",
        recording.disk.as_ref().unwrap().analysis_cache.describe()
    );
    for phase in ["native_shared", "logical_cold", "cold_repeat"] {
        if phase == "logical_cold" {
            recording.disk.as_mut().unwrap().analysis_cache = AnalysisCache::default();
        }
        let mut cursor = ReplayCursor::default();
        let (replay, metrics) = cursor.seek_profiled(&recording, frame, || false).unwrap();
        let mut displayed = SceneState::default();
        let started = Instant::now();
        replay.update.apply(&mut displayed);
        let apply_ms = started.elapsed().as_secs_f64() * 1000.0;
        let scene = digest(&(
            &displayed.objects,
            &displayed.debug_draw,
            &displayed.contacts,
            &displayed.errors,
            &displayed.reference_lifetimes,
            &displayed.generations,
        ));
        let analysis = digest(&replay.analysis);
        if phase == "logical_cold" {
            assert_eq!(metrics.baseline_loads, 1);
        }
        eprintln!(
            "cache_after_{phase}={:?}",
            recording.disk.as_ref().unwrap().analysis_cache.describe()
        );
        let hashes = (scene.clone(), analysis.clone());
        if let Some(expected) = &expected {
            assert_eq!(&hashes, expected);
        } else {
            expected = Some(hashes);
        }
        let ms = |duration: Duration| duration.as_secs_f64() * 1000.0;
        println!(
            "PVD_COLD_SEEK {}",
            serde_json::json!({
                "phase": phase,
                "frame": frame,
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
                "changed_objects": replay.update.changed_objects().count(),
                "objects": displayed.objects.len(),
                "scene_digest": scene,
                "analysis_digest": analysis,
            })
        );
    }
}
