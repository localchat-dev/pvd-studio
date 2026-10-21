use super::*;

pub(super) fn run(
    mut recording: crate::Recording,
    ranges: &[(usize, usize)],
    stats: serde_json::Value,
    input_bytes: usize,
    input_blake3: String,
    reference_path: &Path,
    output: &Path,
) {
    let result_path = output.join("cold-phase-summary.json");
    assert!(!result_path.exists());
    assert!(std::fs::metadata(reference_path).unwrap().len() <= 4 << 20);
    let reference_bytes = std::fs::read(reference_path).unwrap();
    assert!(reference_bytes.len() <= 4 << 20);
    let legacy: serde_json::Value = serde_json::from_slice(&reference_bytes).unwrap();
    assert_eq!(legacy["input_bytes"], input_bytes);
    assert_eq!(legacy["input_blake3"], input_blake3);
    assert_eq!(legacy["stats"]["frames"], recording.frame_count());
    assert_eq!(legacy["stats"]["segments"], ranges.len());
    let legacy_groups = legacy["groups"].as_array().unwrap();
    assert_eq!(legacy_groups.len(), 33);
    let mut results = Vec::new();
    for index in [0, 24, 28] {
        let (first, last) = ranges[index];
        let prior = &legacy_groups[index];
        assert_eq!(prior["first"], first);
        assert_eq!(prior["last"], last);
        let expected = references(&mut recording, &sequence(first, last));
        assert_eq!(serde_json::to_value(&expected).unwrap(), prior["reference"]);
        assert_eq!(expected.len(), 11);
        let row = &expected[0];
        assert_eq!(row.phase, "logical_cold");
        assert_eq!(row.frame, first + 3);
        assert_eq!(prior["order"], serde_json::json!([false, true]));
        let scene_rows = run_arm(&mut recording, &expected, false);
        assert_eq!(scene_rows.len(), 11);
        reset(&mut recording);
        let mut cursor = ReplayCursor::default();
        let (replay, metrics) = cursor
            .seek_profiled(&recording, row.frame, || false)
            .unwrap();
        assert_eq!(cursor.current.as_ref().unwrap().frame, Some(row.frame));
        assert_eq!(metrics.baseline_loads, 1);
        assert_eq!(metrics.analysis_frames, 3);
        let mut scene = SceneState::default();
        replay.update.apply_owned(&mut scene);
        let actual = reference(row.phase, row.frame, &scene, &replay.analysis);
        assert_eq!(actual, *row);
        let ms = |duration: Duration| duration.as_secs_f64() * 1000.0;
        results.push(serde_json::json!({
            "segment_index": index,
            "segment_first": first,
            "segment_last": last,
            "frame": row.frame,
            "phase": "logical_cold_profiled",
            "seek_ms": ms(metrics.total),
            "index_ms": ms(metrics.index),
            "scene_ms": ms(metrics.scene),
            "analysis_ms": ms(metrics.analysis),
            "baseline_io_ms": ms(metrics.baseline_io),
            "baseline_decode_ms": ms(metrics.baseline_decode),
            "baseline_pages_ms": ms(metrics.baseline_pages),
            "baseline_sharing_ms": ms(metrics.baseline_sharing),
            "cache_maintenance_ms": ms(metrics.cache_maintenance),
            "baseline_bytes": metrics.baseline_bytes,
            "baseline_loads": metrics.baseline_loads,
            "analysis_frames": metrics.analysis_frames,
            "analysis_inputs": metrics.analysis_inputs,
            "cache_hits": metrics.cache_hits,
            "analysis_root_hits": metrics.analysis_root_hits,
            "resident_domain_bytes": metrics.resident_domain_bytes,
            "shared_cache_entries": metrics.shared_cache_entries,
            "shared_cache_bytes": metrics.shared_cache_bytes,
            "cursor_cache_bytes": metrics.cursor_cache_bytes,
            "objects": scene.objects.len(),
            "oracle": actual,
            "legacy_reference_targets_verified": expected.len(),
        }));
    }
    assert_eq!(
        results
            .iter()
            .map(|row| row["frame"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [3, 14403, 16803]
    );
    drop(recording);
    assert!(
        std::fs::read_dir(output.join("history"))
            .unwrap()
            .next()
            .is_none()
    );
    let summary = serde_json::json!({
        "scope": "Diagnostic phase timings, not a paired performance comparison. Each selected segment is reference-warmed and scene-only-warmed before a new cursor and AnalysisCache reset. FullReader and OS caches are not cleared; skipped segments change reader-cache history relative to the full 33-segment probe. Timed seek excludes Scene application and complete oracle hashes.",
        "metric_boundaries": {
            "analysis_ms": "Inclusive Analysis restoration, frame application and cache work; child metrics overlap it.",
            "baseline_io_ms": "Reading the AnalysisPages descriptor record, not the complete page graph.",
            "baseline_pages_ms": "Descriptor decoding and the complete Resident page restore, including reader lock wait, page IO/hash, graph reconstruction and validation, and any budget reclaim/retry.",
            "baseline_decode_ms": "Legacy inline baseline deserialization only; zero does not imply no Resident decoding.",
            "shared_cache_bytes": "AnalysisCache metadata only; Resident payloads are counted by Domain.",
            "unseparated": ["reader lock wait", "raw page IO and hashing", "individual field graphs", "whole-root validation", "budget retries", "reclaim cost"],
        },
        "input_bytes": input_bytes,
        "input_blake3": input_blake3,
        "legacy_reference_blake3": blake3::hash(&reference_bytes).to_hex().to_string(),
        "reader_diagnostics_enabled": std::env::var_os("PVD_SHARED_READ_DIAGNOSTICS").is_some(),
        "stats": stats,
        "rows": results,
        "profiled_targets": 3,
        "legacy_reference_targets_verified": 33,
        "closed_history_entries": 0,
    });
    std::fs::write(result_path, serde_json::to_vec_pretty(&summary).unwrap()).unwrap();
}
