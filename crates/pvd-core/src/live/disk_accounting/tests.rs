use super::*;

fn directory() -> PathBuf {
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "pvd-disk-ledger-{}-{}-{sequence}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&path).unwrap();
    path
}

#[test]
fn resource_dependencies_partition_shared_blocks_once() {
    let path = directory();
    {
        let owner = CacheDirectory::new(&path).unwrap();
        let resources = Resources::new(owner, 8 << 20);
        let mut plan = ResourcePlan::default();
        let leaf = resources.plan(&[7; 512], &[], &mut plan).unwrap();
        let scene = resources
            .plan(&[2; 17], std::slice::from_ref(&leaf), &mut plan)
            .unwrap();
        let analysis = resources.plan(&[3; 19], &[leaf], &mut plan).unwrap();
        resources.commit(&plan).unwrap();
        let roots = BTreeMap::from([(scene.id(), SCENE), (analysis.id(), ANALYSIS)]);
        let report = resources
            .physical_accounting(&roots, Limits::default(), &mut || false)
            .unwrap();
        assert_eq!(report.scene_only.payload_bytes, 17);
        assert_eq!(report.analysis_only.payload_bytes, 19);
        assert_eq!(report.shared_scene_analysis.payload_bytes, 512);
        assert_eq!(report.shared_scene_analysis.blocks, 1);
        assert_eq!(report.current_block_header_bytes, 3 * 36);
        assert_eq!(report.file_header_bytes, 8);
        assert_eq!(report.partition_sum().unwrap(), resources.physical_bytes());
        let again = resources
            .physical_accounting(&roots, Limits::default(), &mut || false)
            .unwrap();
        assert_eq!(again.partition_sum().unwrap(), report.total_bytes);
    }
    std::fs::remove_dir(path).unwrap();
}

#[test]
fn distinct_blobs_with_equal_contents_share_one_physical_block() {
    let path = directory();
    {
        let resources = Resources::new(CacheDirectory::new(&path).unwrap(), 8 << 20);
        let mut plan = ResourcePlan::default();
        let scene = resources.plan(&[9; 512], &[], &mut plan).unwrap();
        let analysis = resources.plan(&[9; 512], &[], &mut plan).unwrap();
        assert_ne!(scene.id(), analysis.id());
        resources.commit(&plan).unwrap();
        let roots = BTreeMap::from([(scene.id(), SCENE), (analysis.id(), ANALYSIS)]);
        let report = resources
            .physical_accounting(&roots, Limits::default(), &mut || false)
            .unwrap();
        assert_eq!(report.live_blobs, 2);
        assert_eq!(report.unique_current_blocks, 1);
        assert_eq!(report.shared_scene_analysis.payload_bytes, 512);
        assert_eq!(report.total_bytes, 8 + 36 + 512);
    }
    std::fs::remove_dir(path).unwrap();
}

#[test]
fn unknown_live_parent_prevents_claiming_exclusive_scene_ownership() {
    let path = directory();
    {
        let resources = Resources::new(CacheDirectory::new(&path).unwrap(), 8 << 20);
        let mut plan = ResourcePlan::default();
        let leaf = resources.plan(&[5; 512], &[], &mut plan).unwrap();
        let _unclassified_parent = resources
            .plan(&[8; 7], std::slice::from_ref(&leaf), &mut plan)
            .unwrap();
        resources.commit(&plan).unwrap();
        let roots = BTreeMap::from([(leaf.id(), SCENE)]);
        let report = resources
            .physical_accounting(&roots, Limits::default(), &mut || false)
            .unwrap();
        assert_eq!(report.scene_only.payload_bytes, 0);
        assert_eq!(report.unattributed_live.payload_bytes, 519);
        assert_eq!(report.partition_sum().unwrap(), resources.physical_bytes());
    }
    std::fs::remove_dir(path).unwrap();
}

#[test]
fn dropped_and_pending_resources_are_not_added_as_live_physical_payload() {
    let path = directory();
    {
        let resources = Resources::new(CacheDirectory::new(&path).unwrap(), 8 << 20);
        let mut plan = ResourcePlan::default();
        let handle = resources.plan(&[6; 512], &[], &mut plan).unwrap();
        let pending = resources
            .physical_accounting(&BTreeMap::new(), Limits::default(), &mut || false)
            .unwrap();
        assert_eq!(pending.total_bytes, 0);
        assert_eq!(pending.pending_logical_bytes, 512);
        resources.commit(&plan).unwrap();
        drop((handle, plan));
        let report = resources
            .physical_accounting(&BTreeMap::new(), Limits::default(), &mut || false)
            .unwrap();
        assert_eq!(report.unique_current_blocks, 0);
        assert_eq!(report.file_header_bytes, 8);
        assert_eq!(report.residual_physical_bytes, 36 + 512);
        assert_eq!(report.partition_sum().unwrap(), resources.physical_bytes());
        resources.collect().unwrap();
        let empty = resources
            .physical_accounting(&BTreeMap::new(), Limits::default(), &mut || false)
            .unwrap();
        assert_eq!(empty.total_bytes, 0);
    }
    std::fs::remove_dir(path).unwrap();
}

#[test]
fn frame_inline_analysis_and_checkpoint_headers_reconcile_with_history_stats() {
    let path = directory();
    {
        let mut history = HistoryWriter::new(HistoryConfig {
            directory: path.clone(),
            disk_budget_bytes: 8 << 20,
            segment_target_bytes: 1 << 20,
            segment_frames: 1,
            min_free_disk_bytes: 0,
        })
        .unwrap();
        let scene = SceneState::default();
        for frame in 0..2 {
            history.append(
                &FrameInfo {
                    index: frame,
                    timestamp: frame as u64,
                    scene_id: 0,
                },
                &scene,
                &SceneUpdate::default(),
                &Analysis::default(),
                vec![AnalysisInput::Buffer {
                    id: 4,
                    kind: "ignored-kind".into(),
                    bytes: Arc::from([3_u8; 512]),
                    little_endian: true,
                    frame,
                }],
            );
        }
        assert!(history.snapshot().stats.warning.is_none());
        let report = history
            .physical_accounting(Limits::default(), &mut || false)
            .unwrap();
        assert_eq!(report.segment.frames, 2);
        assert_eq!(report.segment.checkpoints, 0);
        assert_eq!(report.resource_checkpoint_records, 2);
        assert!(report.resource_checkpoint_logical_bytes > 0);
        assert_eq!(report.segment.file_headers, 16);
        assert_eq!(report.segment.frame_record_headers, 72);
        assert_eq!(report.segment.checkpoint_record_headers, 0);
        assert_eq!(report.segment.checkpoint_payload, 0);
        assert_eq!(report.segment.frame_inline_analysis_raw_buffers, 1024);
        assert!(report.segment.frame_inline_analysis_structure > 0);
        assert_eq!(report.partition_sum, history.snapshot().stats.disk_bytes);
        assert!(report.resources.analysis_only.payload_bytes > 0);
        assert!(report.resources.scene_only.payload_bytes > 0);
        assert_eq!(report.resources.unattributed_live.payload_bytes, 0);
    }
    std::fs::remove_dir(path).unwrap();
}

#[test]
fn accounting_rejects_cancellation_and_limits_without_changing_history_bytes() {
    let path = directory();
    {
        let resources = Resources::new(CacheDirectory::new(&path).unwrap(), 8 << 20);
        let mut plan = ResourcePlan::default();
        let _handle = resources.plan(&[4; 12], &[], &mut plan).unwrap();
        resources.commit(&plan).unwrap();
        let before = resources.physical_bytes();
        assert!(
            resources
                .physical_accounting(&BTreeMap::new(), Limits::default(), &mut || true)
                .is_err()
        );
        assert!(
            resources
                .physical_accounting(
                    &BTreeMap::new(),
                    Limits {
                        items: 0,
                        ..Limits::default()
                    },
                    &mut || false
                )
                .is_err()
        );
        assert_eq!(resources.physical_bytes(), before);
    }
    std::fs::remove_dir(path).unwrap();
    assert!(add(u64::MAX, 1).is_err());
}

#[test]
fn inline_checkpoint_bytes_keep_their_own_physical_partition() {
    let path = directory();
    {
        let mut history = HistoryWriter::new(HistoryConfig {
            directory: path.clone(),
            min_free_disk_bytes: 0,
            ..Default::default()
        })
        .unwrap();
        history.append(
            &FrameInfo {
                index: 0,
                timestamp: 0,
                scene_id: 0,
            },
            &SceneState::default(),
            &SceneUpdate::default(),
            &Analysis::default(),
            Vec::new(),
        );
        assert!(history.warning.is_none());
        let active = history.active.as_mut().unwrap();
        let index = active.state.index.clone();
        active.write(&RecordRef::Checkpoint(&index)).unwrap();
        active.file.flush().unwrap();
        active.view.committed_end = active.view.bytes;
        let report = history
            .physical_accounting(Limits::default(), &mut || false)
            .unwrap();
        assert_eq!(report.segment.checkpoints, 1);
        assert_eq!(report.segment.checkpoint_record_headers, 36);
        assert_eq!(
            report.segment.checkpoint_payload,
            encoded_bytes(&RecordRef::Checkpoint(&index)).unwrap()
        );
        assert_eq!(report.resource_checkpoint_records, 1);
        assert_eq!(report.partition_sum, history.bytes());
    }
    std::fs::remove_dir(path).unwrap();
}

struct Fixture {
    name: &'static str,
    input_bytes: u64,
    input_blake3: &'static str,
    frames: usize,
    segment_frames: usize,
    segment_files: u64,
    checkpoints: u64,
    top_level_header_bytes: u64,
    rolling_allowed: bool,
}

#[test]
#[ignore = "requires the fixed970SDK input and a new diagnostic output directory; not a timing test"]
fn actual_970_physical_history_partition_reconciles_without_double_counting() {
    run_fixture(Fixture {
        name: "fixed970-40-frame-segments",
        input_bytes: 35_817_266,
        input_blake3: "ede45af3f4efb98bae28c4510f4df2aace6d668dac03fbf4cc4a3d2baa807d60",
        frames: 970,
        segment_frames: 40,
        segment_files: 25,
        checkpoints: 25,
        top_level_header_bytes: 36_020,
        rolling_allowed: true,
    });
}

#[test]
#[ignore = "requires the fixed600second SDK input and a new diagnostic output directory; not a timing test"]
fn actual_600_second_physical_history_partition_reconciles_without_double_counting() {
    run_fixture(Fixture {
        name: "fixed600second-600-frame-segments",
        input_bytes: 251_301_039,
        input_blake3: "9a99d0525325e7ad2085bb2db7e175eb53e4d0be331d68ff1373f41bb9a181f1",
        frames: 19_293,
        segment_frames: 600,
        segment_files: 33,
        checkpoints: 161,
        top_level_header_bytes: 700_608,
        rolling_allowed: false,
    });
}

fn run_fixture(fixture: Fixture) {
    let capture = PathBuf::from(
        std::env::var_os("PVD_DISK_ACCOUNTING_CAPTURE").expect("capture path required"),
    );
    let output = PathBuf::from(
        std::env::var_os("PVD_DISK_ACCOUNTING_OUTPUT").expect("new output path required"),
    );
    let family = std::env::var("PVD_DISK_ACCOUNTING_FAMILY").unwrap_or_else(|_| "resident".into());
    assert!(matches!(family.as_str(), "legacy" | "resident"));
    let rolling = std::env::var_os("PVD_DISK_ACCOUNTING_ROLLING").is_some();
    assert!(!rolling || family == "resident");
    assert!(!rolling || fixture.rolling_allowed);
    assert_eq!(
        std::fs::metadata(&capture).unwrap().len(),
        fixture.input_bytes
    );
    std::fs::create_dir(&output).unwrap();
    let history_path = output.join("history");
    let config = HistoryConfig {
        directory: history_path.clone(),
        disk_budget_bytes: if rolling { 64 << 20 } else { 8 << 30 },
        segment_frames: fixture.segment_frames,
        min_free_disk_bytes: 512 << 20,
        ..Default::default()
    };
    let configuration = serde_json::json!({
        "disk_budget_bytes": config.disk_budget_bytes,
        "segment_target_bytes": config.segment_target_bytes,
        "segment_frames": config.segment_frames,
        "min_free_disk_bytes": config.min_free_disk_bytes,
    });
    let mut live = if family == "legacy" {
        crate::LiveRecording::with_legacy_history(config).unwrap()
    } else {
        crate::LiveRecording::with_history(config).unwrap()
    };
    let mut input = File::open(&capture).unwrap();
    let mut buffer = [0_u8; 64 << 10];
    let mut digest = blake3::Hasher::new();
    let mut received = 0_u64;
    loop {
        let count = input.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
        received += count as u64;
        live.feed(&buffer[..count]).unwrap();
    }
    assert_eq!(received, fixture.input_bytes);
    let input_blake3 = digest.finalize().to_hex().to_string();
    assert_eq!(input_blake3, fixture.input_blake3);
    let streaming = live.builder.as_mut().unwrap().streaming.as_mut().unwrap();
    assert_eq!(streaming.frames_seen, fixture.frames);
    assert!(
        streaming.analysis_failure.is_none(),
        "{:?}",
        streaming.analysis_failure
    );
    let history = &mut streaming.history;
    let stats = history.snapshot().stats;
    assert!(stats.warning.is_none(), "{:?}", stats.warning);
    let report = history
        .physical_accounting(Limits::default(), &mut || false)
        .unwrap();
    assert_eq!(report.partition_sum, stats.disk_bytes);
    if !rolling {
        assert_eq!(stats.first_frame, 0);
        assert_eq!(stats.retained_frames, fixture.frames);
        assert_eq!(stats.segments as u64, fixture.segment_files);
        assert_eq!(report.segment.frames, fixture.frames as u64);
        assert_eq!(report.segment.files, fixture.segment_files);
        assert_eq!(report.segment.checkpoints, 0);
        assert_eq!(report.resource_checkpoint_records, fixture.checkpoints);
        assert_eq!(report.segment.checkpoint_record_headers, 0);
        assert_eq!(report.segment.checkpoint_payload, 0);
        assert_eq!(
            report.segment.file_headers
                + report.segment.frame_record_headers
                + report.segment.checkpoint_record_headers,
            fixture.top_level_header_bytes - fixture.checkpoints * 36
        );
    }
    let missing_roots = report.resources.missing_semantic_roots;
    let value = serde_json::json!({"fixture":fixture.name,"family":family,"rolling":rolling,
        "input":capture,"input_bytes":received,"input_blake3":input_blake3,"configuration":configuration,
        "first_frame":stats.first_frame,"retained_frames":stats.retained_frames,"ledger":report,
        "scope":"Quiescent test-only accounting before LiveRecording::finish, while writer owners remain live. Diagnostic temporary memory is outside ResidentDomain. No speed or savings claim."});
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output.join("ledger.json"))
        .unwrap();
    serde_json::to_writer_pretty(&mut file, &value).unwrap();
    file.flush().unwrap();
    if !rolling {
        assert_eq!(
            missing_roots, 0,
            "complete retained history has missing semantic roots; see ledger.json"
        );
    }
    let recording = live.finish().unwrap();
    assert_eq!(recording.frame_count(), fixture.frames);
    assert_eq!(
        recording.disk.as_ref().unwrap().stats.disk_bytes,
        stats.disk_bytes
    );
    drop(recording);
    assert!(std::fs::read_dir(&history_path).unwrap().next().is_none());
    std::fs::remove_dir(history_path).unwrap();
}
