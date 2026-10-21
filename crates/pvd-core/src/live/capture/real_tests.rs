use super::*;

#[test]
#[ignore = "requires PVD_SHARED_HISTORY_RAW pointing to the fixed 970-frame SDK capture"]
fn actual_sdk_retained_capture_survives_source_cleanup_and_matches_full_legacy_wire() {
    let bytes = std::fs::read(std::env::var_os("PVD_SHARED_HISTORY_RAW").expect("fixed SDK input"))
        .unwrap();
    assert_eq!(bytes.len(), 35_817_266);
    assert_eq!(
        blake3::hash(&bytes).to_hex().as_str(),
        "ede45af3f4efb98bae28c4510f4df2aace6d668dac03fbf4cc4a3d2baa807d60"
    );
    let root = std::env::temp_dir().join(format!(
        "pvd-capture-sdk-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir(&root).unwrap();
    let config = |name: &str| HistoryConfig {
        directory: root.join(name),
        disk_budget_bytes: 8 << 30,
        segment_target_bytes: 64 << 20,
        segment_frames: 40,
        min_free_disk_bytes: 512 << 20,
    };
    let mut source = crate::LiveRecording::with_history(config("resident")).unwrap();
    let mut oracle = crate::LiveRecording::with_legacy_history(config("legacy")).unwrap();
    for chunk in bytes.chunks(64 << 10) {
        source.feed(chunk).unwrap();
        oracle.feed(chunk).unwrap();
    }
    let source = source.finish().unwrap();
    let oracle = oracle.finish().unwrap();
    assert_eq!(source.frame_count(), 970);
    assert!(source.analysis.resident().is_some());
    assert!(source.disk_usage().unwrap().warning.is_none());
    let path = root.join("retained.pvdcapture");
    let report = write_retained_new(
        &source,
        &path,
        CaptureRange {
            first: 121,
            last: 160,
        },
        &CaptureLimits::default(),
        512 << 20,
        |_| true,
    )
    .unwrap();
    println!(
        "capture bytes={} resources={} blocks={} index_bytes={}",
        report.bytes, report.resource_count, report.block_count, report.index_bytes
    );
    drop(source);
    assert_eq!(std::fs::read_dir(root.join("resident")).unwrap().count(), 0);
    let restored = crate::Recording::from_path(&path).unwrap();
    let mut cursor = ReplayCursor::default();
    let mut expected = ReplayCursor::default();
    let mut scene = SceneState::default();
    let mut expected_scene = SceneState::default();
    for frame in [121, 122, 139, 140, 159, 160, 140, 139, 121] {
        let actual = cursor.seek(&restored, frame, || false).unwrap();
        let reference = expected.seek(&oracle, frame, || false).unwrap();
        actual.update.apply_owned(&mut scene);
        reference.update.apply_owned(&mut expected_scene);
        assert!(
            scene == expected_scene,
            "full scene differs at frame {frame}"
        );
        assert_eq!(
            postcard::to_stdvec(&actual.analysis).unwrap(),
            postcard::to_stdvec(&reference.analysis).unwrap(),
            "full analysis differs at frame {frame}"
        );
        assert!(actual.analysis.resident().is_some());
        println!(
            "capture target={frame} objects={} domain_used={}",
            scene.objects.len(),
            actual
                .analysis
                .resident()
                .unwrap()
                .domain()
                .statistics()
                .logical_used
        );
    }
    assert!(restored.state_at(120).is_err());
    assert!(restored.analysis_at(120).is_err());
    drop((restored, oracle, cursor, expected));
    assert_eq!(std::fs::read_dir(root.join("legacy")).unwrap().count(), 0);
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(root.join("resident")).unwrap();
    std::fs::remove_dir(root.join("legacy")).unwrap();
    std::fs::remove_dir(root).unwrap();
}
