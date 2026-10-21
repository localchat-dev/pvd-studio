use super::*;

#[test]
#[ignore = "requires the fixed real SDK 970-frame capture"]
fn actual_default_history_replays_a_full_resident_window_and_releases_all_roots() {
    let bytes =
        std::fs::read(std::env::var_os("PVD_SHARED_HISTORY_RAW").expect("fixed real capture path"))
            .unwrap();
    assert_eq!(bytes.len(), 35_817_266);
    assert_eq!(
        blake3::hash(&bytes).to_hex().as_str(),
        "ede45af3f4efb98bae28c4510f4df2aace6d668dac03fbf4cc4a3d2baa807d60"
    );
    let mut configuration = config(8 << 30, 40);
    configuration.min_free_disk_bytes = 512 << 20;
    let mut legacy = crate::LiveRecording::with_legacy_history(configuration.clone()).unwrap();
    let mut paged = crate::LiveRecording::with_history(configuration).unwrap();
    for chunk in bytes.chunks(64 << 10) {
        legacy.feed(chunk).unwrap();
        paged.feed(chunk).unwrap();
    }
    let legacy = legacy.finish().unwrap();
    let paged = paged.finish().unwrap();
    assert_eq!(paged.frame_count(), 970);
    assert!(paged.analysis.resident().is_some());
    let paused = paged.analysis_at(0).unwrap();
    let paused_hash = blake3::hash(&postcard::to_stdvec(&paused).unwrap());
    let domain = paused.resident().unwrap().domain().clone();
    let resources = paged
        .disk
        .as_ref()
        .unwrap()
        .segments
        .front()
        .unwrap()
        .resources
        .clone();
    let mut cursor = ReplayCursor::default();
    let mut reference = ReplayCursor::default();
    let mut displayed = SceneState::default();
    let mut expected_scene = SceneState::default();
    let mut reverse_hits = 0;
    let mut maximum_entries = 0;
    let mut maximum_cache_bytes = 0;
    for (phase, frame) in (720..=840)
        .map(|frame| ("forward", frame))
        .chain((721..840).rev().map(|frame| ("reverse", frame)))
        .chain(
            [600, 969, 5, 720]
                .into_iter()
                .map(|frame| ("outside", frame)),
        )
    {
        let (actual, metrics) = cursor.seek_profiled(&paged, frame, || false).unwrap();
        let expected = reference.seek(&legacy, frame, || false).unwrap();
        actual.update.apply_owned(&mut displayed);
        expected.update.apply_owned(&mut expected_scene);
        assert_eq!(displayed, expected_scene, "scene {phase} {frame}");
        assert!(actual.analysis.resident().is_some());
        assert_eq!(
            postcard::to_stdvec(&actual.analysis).unwrap(),
            postcard::to_stdvec(&expected.analysis).unwrap(),
            "analysis {phase} {frame}"
        );
        maximum_entries = maximum_entries.max(metrics.shared_cache_entries);
        maximum_cache_bytes = maximum_cache_bytes.max(metrics.shared_cache_bytes);
        assert!(metrics.shared_cache_entries <= 120);
        assert!(domain.statistics().logical_used <= 192 << 20);
        if phase == "reverse" {
            assert_eq!(metrics.analysis_root_hits, 1, "reverse root {frame}");
            assert_eq!(metrics.analysis_frames, 0, "reverse ingest {frame}");
            assert_eq!(metrics.baseline_loads, 0, "reverse baseline {frame}");
            reverse_hits += 1;
        }
        println!(
            "default_window phase={phase} frame={frame} root_hits={} replay_frames={} baselines={} entries={} cache_bytes={} domain_used={} domain_peak={}",
            metrics.analysis_root_hits,
            metrics.analysis_frames,
            metrics.baseline_loads,
            metrics.shared_cache_entries,
            metrics.shared_cache_bytes,
            domain.statistics().logical_used,
            domain.statistics().logical_peak
        );
    }
    assert_eq!(reverse_hits, 119);
    assert_eq!(maximum_entries, 120);
    let before = displayed.clone();
    let mut checks = 0;
    assert!(
        cursor
            .seek(&paged, 42, || {
                checks += 1;
                checks == 3
            })
            .is_err()
    );
    assert_eq!(displayed, before);
    let actual = cursor.seek(&paged, 42, || false).unwrap();
    let expected = reference.seek(&legacy, 42, || false).unwrap();
    actual.update.apply_owned(&mut displayed);
    expected.update.apply_owned(&mut expected_scene);
    assert_eq!(displayed, expected_scene);
    assert_eq!(
        postcard::to_stdvec(&actual.analysis).unwrap(),
        postcard::to_stdvec(&expected.analysis).unwrap()
    );
    assert_eq!(
        blake3::hash(&postcard::to_stdvec(&paused).unwrap()),
        paused_hash
    );
    println!(
        "default_window final legacy_physical={} shared_physical={} reverse_hits={reverse_hits} maximum_entries={maximum_entries} maximum_cache_bytes={maximum_cache_bytes} domain_peak={}",
        legacy.disk_usage().unwrap().disk_bytes,
        paged.disk_usage().unwrap().disk_bytes,
        domain.statistics().logical_peak
    );
    drop((
        actual.analysis,
        expected.analysis,
        before,
        displayed,
        expected_scene,
        cursor,
        reference,
        paused,
        paged,
        legacy,
    ));
    resources.collect().unwrap();
    assert_eq!(resources.physical_bytes(), 0);
    assert_eq!(domain.statistics().logical_used, 0);
}
