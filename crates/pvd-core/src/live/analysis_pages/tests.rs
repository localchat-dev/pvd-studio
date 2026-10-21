use super::*;
use crate::analysis::resident::full_fixture_inputs;
use std::sync::atomic::Ordering;

fn config(budget: u64, segment_frames: usize) -> HistoryConfig {
    HistoryConfig {
        directory: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../output/resident-history-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            )),
        disk_budget_bytes: budget,
        segment_target_bytes: 64 << 20,
        segment_frames,
        min_free_disk_bytes: 0,
    }
}

fn scene(frame: usize) -> SceneState {
    let mut scene = SceneState::default();
    scene.objects.insert(
        71,
        Object {
            id: 71,
            generation: 3,
            type_name: "Captured".into(),
            properties: [
                ("mesh".into(), Value::Bytes(Arc::from(vec![41; 8192]))),
                ("frame".into(), Value::U64(frame as u64)),
            ]
            .into(),
        },
    );
    scene
}

fn stream_header() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(pvd_protocol::STREAM_MAGIC.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    bytes
}

#[test]
fn blocked_history_directory_selects_legacy_before_live_analysis_begins() {
    let configuration = config(32 << 20, 2);
    let directory = configuration.directory.clone();
    std::fs::create_dir_all(directory.parent().unwrap()).unwrap();
    std::fs::write(&directory, b"blocked directory").unwrap();
    let mut capture = crate::LiveRecording::with_history(configuration).unwrap();
    assert!(
        capture
            .history
            .as_ref()
            .unwrap()
            .initial_analysis()
            .is_none()
    );
    assert!(capture.history.as_ref().unwrap().warning.is_some());
    assert_eq!(capture.feed(&stream_header()).unwrap(), 0);
    let builder = capture.builder.as_mut().unwrap();
    assert!(builder.recording.analysis.legacy().is_some());
    builder
        .apply_analysis_input(AnalysisInput::Zone(3, "Physics".into()))
        .unwrap();
    builder
        .push(pvd_protocol::EventGroup {
            stream_id: 1,
            timestamp: 0,
            offset: 0,
            events: vec![
                pvd_protocol::Event::StringHandle {
                    handle: 1,
                    value: "frame".into(),
                },
                pvd_protocol::Event::EndSection {
                    id: 0,
                    name: 1,
                    timestamp: 0,
                },
            ],
        })
        .unwrap();
    assert_eq!(capture.frame_count(), 1);
    let builder = capture.builder.as_ref().unwrap();
    assert!(builder.recording.analysis.legacy().is_some());
    assert!(
        builder
            .recording
            .analysis
            .legacy()
            .unwrap()
            .zones
            .contains_key(&3)
    );
    assert!(
        builder
            .streaming
            .as_ref()
            .unwrap()
            .history
            .analysis_pages
            .is_none()
    );
    assert!(
        builder
            .streaming
            .as_ref()
            .unwrap()
            .history
            .warning
            .is_some()
    );
    drop(capture);
    assert_eq!(std::fs::read(&directory).unwrap(), b"blocked directory");
    std::fs::remove_file(directory).unwrap();
}

#[test]
fn default_live_builder_and_history_adopt_the_same_completed_analysis_root() {
    let mut capture = crate::LiveRecording::with_history(config(32 << 20, 2)).unwrap();
    assert_eq!(capture.feed(&stream_header()).unwrap(), 0);
    let builder = capture.builder.as_mut().unwrap();
    let domain = builder
        .recording
        .analysis
        .resident()
        .unwrap()
        .domain()
        .clone();
    let pages = builder
        .streaming
        .as_ref()
        .unwrap()
        .history
        .analysis_pages
        .as_ref()
        .unwrap();
    assert!(domain.same(pages.current().domain()));
    assert!(
        builder
            .recording
            .analysis
            .resident()
            .unwrap()
            .same(pages.current())
    );
    builder
        .push(pvd_protocol::EventGroup {
            stream_id: 1,
            timestamp: 0,
            offset: 0,
            events: vec![pvd_protocol::Event::StringHandle {
                handle: 1,
                value: "frame".into(),
            }],
        })
        .unwrap();
    let mut expected = Analysis::default();
    for input in [
        AnalysisInput::Zone(3, "Physics".into()),
        AnalysisInput::Event(3, 4, "simulate".into()),
    ] {
        input.apply(&mut expected).unwrap();
        builder.apply_analysis_input(input).unwrap();
    }
    let mut published = Vec::new();
    for (frame, (id, kind, bytes)) in full_fixture_inputs(pvd_protocol::Endian::Little)
        .into_iter()
        .enumerate()
    {
        let input = AnalysisInput::Buffer {
            id,
            kind: kind.into(),
            bytes: bytes.into(),
            little_endian: true,
            frame,
        };
        input.apply(&mut expected).unwrap();
        builder.apply_analysis_input(input).unwrap();
        builder
            .push(pvd_protocol::EventGroup {
                stream_id: 1,
                timestamp: frame as u64,
                offset: 0,
                events: vec![pvd_protocol::Event::EndSection {
                    id: 0,
                    name: 1,
                    timestamp: frame as u64,
                }],
            })
            .unwrap();
        let history = &builder.streaming.as_ref().unwrap().history;
        assert!(history.warning.is_none(), "{:?}", history.warning);
        assert!(
            builder
                .recording
                .analysis
                .resident()
                .unwrap()
                .same(history.analysis_pages.as_ref().unwrap().current())
        );
        assert_eq!(
            postcard::to_stdvec(&builder.recording.analysis).unwrap(),
            postcard::to_stdvec(&expected).unwrap()
        );
        published.push(builder.recording.analysis.clone());
    }
    let recording = capture.finish().unwrap();
    for (frame, old) in published.iter().enumerate() {
        let replay = recording.analysis_at(frame).unwrap();
        assert!(replay.resident().is_some());
        assert_eq!(
            postcard::to_stdvec(&replay).unwrap(),
            postcard::to_stdvec(old).unwrap()
        );
    }
    drop((recording, published));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn actual_history_frame_plan_keeps_scene_and_paged_analysis_in_one_commit() {
    for endian in [pvd_protocol::Endian::Little, pvd_protocol::Endian::Big] {
        let mut history = HistoryWriter::new_paged(config(16 << 20, 2)).unwrap();
        let resources = history.resources.as_ref().unwrap().clone();
        let domain = history
            .analysis_pages
            .as_ref()
            .unwrap()
            .current()
            .domain()
            .clone();
        let mut expected = Analysis::default();
        let mut frames = Vec::new();
        let mut old = None;
        for (frame, (id, kind, bytes)) in full_fixture_inputs(endian).into_iter().enumerate() {
            let mut inputs = Vec::new();
            if frame == 0 {
                inputs.extend([
                    AnalysisInput::Zone(3, "Physics".into()),
                    AnalysisInput::Event(3, 4, "simulate".into()),
                ]);
            }
            inputs.push(AnalysisInput::Buffer {
                id,
                kind: kind.into(),
                bytes: bytes.into(),
                little_endian: endian == pvd_protocol::Endian::Little,
                frame,
            });
            for input in &inputs {
                input.apply(&mut expected).unwrap();
            }
            let state = scene(frame);
            let patch = SceneUpdate::capture(&state, &[71].into(), false);
            history.append(
                &FrameInfo {
                    index: frame,
                    timestamp: frame as u64,
                    scene_id: 0,
                },
                &state,
                &patch,
                &expected,
                inputs,
            );
            let disk = history.snapshot();
            assert!(disk.stats.warning.is_none(), "{:?}", disk.stats.warning);
            assert_eq!(disk.frames.len(), frame + 1);
            assert_eq!(
                disk.state_at_with_cancel(frame, &mut || false).unwrap(),
                state
            );
            let resident = disk
                .resident_analysis_at_with_cancel(frame, &mut || false)
                .unwrap();
            assert_eq!(
                postcard::to_stdvec(&resident).unwrap(),
                postcard::to_stdvec(&expected).unwrap()
            );
            if frame == 0 {
                old = Some(resident.clone());
            }
            frames.push(postcard::to_stdvec(&expected).unwrap());
        }
        let disk = history.snapshot();
        assert_eq!(disk.stats.segments, 4);
        let mut reads = Vec::new();
        for frame in [7, 2, 0, 6, 1, 7, 4] {
            let resident = disk
                .resident_analysis_at_with_cancel(frame, &mut || false)
                .unwrap();
            assert_eq!(postcard::to_stdvec(&resident).unwrap(), frames[frame]);
            reads.push(resident);
        }
        {
            let shared = disk.analysis_at_with_cancel(0, &mut || false).unwrap();
            assert!(shared.resident().is_some());
            assert_eq!(postcard::to_stdvec(&shared).unwrap(), frames[0]);
        }
        let failure = disk
            .resident_analysis_at_with_cancel(0, &mut || true)
            .err()
            .unwrap();
        assert_eq!(
            failure.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
        assert_eq!(
            postcard::to_stdvec(old.as_ref().unwrap()).unwrap(),
            frames[0]
        );
        assert!(domain.statistics().logical_peak <= 192 << 20);
        drop((old, reads, disk, history));
        resources.collect().unwrap();
        assert_eq!(resources.physical_bytes(), 0);
        assert_eq!(domain.statistics().logical_used, 0);
    }
}

#[test]
fn failed_input_or_disk_reservation_does_not_publish_an_incomplete_frame() {
    let mut history = HistoryWriter::new_paged(config(8 << 20, 2)).unwrap();
    let state = scene(0);
    let patch = SceneUpdate::capture(&state, &[71].into(), false);
    history.append(
        &FrameInfo {
            index: 0,
            timestamp: 0,
            scene_id: 0,
        },
        &state,
        &patch,
        &Analysis::default(),
        Vec::new(),
    );
    let disk = history.snapshot();
    let old = disk
        .resident_analysis_at_with_cancel(0, &mut || false)
        .unwrap();
    let old_wire = postcard::to_stdvec(&old).unwrap();
    history.append(
        &FrameInfo {
            index: 1,
            timestamp: 1,
            scene_id: 0,
        },
        &state,
        &patch,
        &Analysis::default(),
        vec![AnalysisInput::Buffer {
            id: 1,
            kind: "PxProfileMemoryEventBuffer".into(),
            bytes: Arc::from([255]),
            little_endian: true,
            frame: 1,
        }],
    );
    assert_eq!(history.frames(), 1);
    assert!(history.snapshot().stats.warning.is_some());
    history.append(
        &FrameInfo {
            index: 2,
            timestamp: 2,
            scene_id: 0,
        },
        &state,
        &patch,
        &Analysis::default(),
        Vec::new(),
    );
    assert_eq!(history.frames(), 1);
    assert_eq!(postcard::to_stdvec(&old).unwrap(), old_wire);

    let mut small = HistoryWriter::new_paged(config(4096, 2)).unwrap();
    let mut state = state;
    let mut generator = 0x1234_5678_9abc_def0u64;
    let payload: Vec<u8> = (0..32_768)
        .map(|_| {
            generator ^= generator << 13;
            generator ^= generator >> 7;
            generator ^= generator << 17;
            generator as u8
        })
        .collect();
    state
        .objects
        .get_mut(&71)
        .unwrap()
        .properties
        .insert("mesh".into(), Value::Bytes(payload.into()));
    let patch = SceneUpdate::capture(&state, &[71].into(), false);
    small.append(
        &FrameInfo {
            index: 0,
            timestamp: 0,
            scene_id: 0,
        },
        &state,
        &patch,
        &Analysis::default(),
        Vec::new(),
    );
    assert_eq!(small.frames(), 0);
    assert!(small.snapshot().stats.warning.is_some());
}

#[test]
#[ignore = "requires the fixed real SDK 970-frame capture"]
fn actual_970_history_writer_legacy_and_shared_pages_match_without_expiry() {
    let path = PathBuf::from(
        std::env::var_os("PVD_SHARED_HISTORY_RAW")
            .expect("set PVD_SHARED_HISTORY_RAW to the fixed 970-frame SDK capture"),
    );
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.len(), 35_817_266);
    assert_eq!(
        blake3::hash(&bytes).to_hex().as_str(),
        "ede45af3f4efb98bae28c4510f4df2aace6d668dac03fbf4cc4a3d2baa807d60"
    );
    let mut full_config = config(8 << 30, 40);
    full_config.min_free_disk_bytes = 512 << 20;
    let mut legacy = crate::LiveRecording::with_legacy_history(full_config.clone()).unwrap();
    let mut paged = crate::LiveRecording::with_paged_history(full_config).unwrap();
    for chunk in bytes.chunks(64 << 10) {
        legacy.feed(chunk).unwrap();
        paged.feed(chunk).unwrap();
    }
    let legacy = legacy.finish().unwrap();
    let paged = paged.finish().unwrap();
    assert_eq!(legacy.frame_count(), 970);
    assert_eq!(paged.frame_count(), 970);
    let old_disk = legacy.disk.as_ref().unwrap();
    let new_disk = paged.disk.as_ref().unwrap();
    assert!(
        old_disk.stats.warning.is_none(),
        "{:?}",
        old_disk.stats.warning
    );
    assert!(
        new_disk.stats.warning.is_none(),
        "{:?}",
        new_disk.stats.warning
    );
    assert_eq!(old_disk.stats.first_frame, 0);
    assert_eq!(new_disk.stats.first_frame, 0);
    assert_eq!(old_disk.stats.retained_frames, 970);
    assert_eq!(new_disk.stats.retained_frames, 970);
    println!(
        "actual970_history legacy_total={} shared_total={} segments={}/{} retained={}/{}",
        old_disk.stats.disk_bytes,
        new_disk.stats.disk_bytes,
        old_disk.stats.segments,
        new_disk.stats.segments,
        old_disk.stats.retained_frames,
        new_disk.stats.retained_frames
    );
    let mut pinned = None;
    let mut displayed = None;
    for frame in [0, 1, 39, 40, 120, 150, 500, 969, 950, 120] {
        let statistics = new_disk
            .analysis_pages
            .as_ref()
            .unwrap()
            .0
            .domain()
            .statistics();
        println!(
            "actual970_history before frame={frame} used={} peak={} strong_layout={} weak_fixed={} other={} cells={}",
            statistics.logical_used,
            statistics.logical_peak,
            statistics.live_strong_allocation_bytes,
            statistics.logical_weak_fixed_bytes,
            statistics
                .logical_used
                .saturating_sub(statistics.live_strong_allocation_bytes)
                .saturating_sub(statistics.logical_weak_fixed_bytes),
            statistics.live_strong_cells
        );
        let actual = new_disk
            .resident_analysis_at_with_cancel(frame, &mut || false)
            .unwrap();
        let expected = legacy.analysis_at(frame).unwrap();
        assert!(
            postcard::to_stdvec(&actual).unwrap() == postcard::to_stdvec(&expected).unwrap(),
            "analysis frame {frame}"
        );
        assert!(
            paged.state_at(frame).unwrap() == legacy.state_at(frame).unwrap(),
            "scene frame {frame}"
        );
        if pinned.is_none() {
            pinned = Some(actual.clone());
        }
        displayed = Some(actual);
        let statistics = new_disk
            .analysis_pages
            .as_ref()
            .unwrap()
            .0
            .domain()
            .statistics();
        println!(
            "actual970_history after frame={frame} used={} peak={} strong_layout={} weak_fixed={} other={} cells={}",
            statistics.logical_used,
            statistics.logical_peak,
            statistics.live_strong_allocation_bytes,
            statistics.logical_weak_fixed_bytes,
            statistics
                .logical_used
                .saturating_sub(statistics.live_strong_allocation_bytes)
                .saturating_sub(statistics.logical_weak_fixed_bytes),
            statistics.live_strong_cells
        );
    }
    println!(
        "actual970_history legacy_total={} shared_total={} segments={}/{} retained={}/{}",
        old_disk.stats.disk_bytes,
        new_disk.stats.disk_bytes,
        old_disk.stats.segments,
        new_disk.stats.segments,
        old_disk.stats.retained_frames,
        new_disk.stats.retained_frames
    );
    assert!(
        postcard::to_stdvec(pinned.as_ref().unwrap()).unwrap()
            == postcard::to_stdvec(&legacy.analysis_at(0).unwrap()).unwrap()
    );
    let _ = displayed.as_ref().unwrap().memory();
    let resources = new_disk.segments.front().unwrap().resources.clone();
    let domain = pinned.as_ref().unwrap().domain().clone();
    drop((pinned, displayed, legacy, paged));
    resources.collect().unwrap();
    assert_eq!(resources.physical_bytes(), 0);
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn paged_frame_preparation_reclaims_cached_roots_and_rolls_back_failed_plans() {
    for reclaimable in [true, false] {
        let mut history = HistoryWriter::new_paged(config(32 << 20, 1)).unwrap();
        let resources = history.resources.as_ref().unwrap().clone();
        let mut analysis = history.initial_analysis().unwrap();
        analysis.add_zone(3, "Physics".into()).unwrap();
        let domain = analysis.resident().unwrap().domain().clone();
        let old_wire = postcard::to_stdvec(&analysis).unwrap();
        let lease = ResourceLease::new(Vec::new());
        if reclaimable {
            let mut cached = analysis.clone();
            cached.add_zone(19, "temporary".repeat(20_000)).unwrap();
            history.analysis_cache.insert_checkpoint(99, cached, &lease);
        }
        let before = domain.statistics();
        let blocked = domain.reserve(before.limit - before.logical_used).unwrap();
        let state = scene(0);
        let patch = SceneUpdate::capture(&state, &[71].into(), false);
        history.append_shared(
            &FrameInfo {
                index: 0,
                timestamp: 0,
                scene_id: 0,
            },
            &state,
            &patch,
            &analysis,
            vec![AnalysisInput::Zone(3, "Physics".into())],
        );
        assert_eq!(postcard::to_stdvec(&analysis).unwrap(), old_wire);
        assert_eq!(
            history.warning.is_none(),
            reclaimable,
            "{:?}",
            history.warning
        );
        let disk = history.snapshot();
        assert_eq!(disk.frames.len(), usize::from(reclaimable));
        drop(blocked);
        if reclaimable {
            assert_eq!(disk.state_at(0).unwrap(), state);
            assert_eq!(
                postcard::to_stdvec(&disk.analysis_at_with_cancel(0, &mut || false).unwrap())
                    .unwrap(),
                old_wire
            );
        } else {
            resources.collect().unwrap();
            assert_eq!(resources.physical_bytes(), 0);
        }
        assert!(domain.statistics().logical_peak <= 192 << 20);
        drop((analysis, history, disk, lease));
        resources.collect().unwrap();
        assert_eq!(resources.physical_bytes(), 0);
        assert_eq!(domain.statistics().logical_used, 0);
    }
}

#[path = "text05_tests.rs"]
mod text05_tests;

#[path = "rolling_tests.rs"]
mod rolling_tests;

#[path = "replay_window_tests.rs"]
mod replay_window_tests;

#[path = "failure_reset_tests.rs"]
mod failure_reset_tests;
