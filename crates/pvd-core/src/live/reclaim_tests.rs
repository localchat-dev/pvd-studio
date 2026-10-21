use super::*;
use crate::analysis::resident::full_fixture_inputs;
use std::{fs, sync::atomic::Ordering};

const BUDGET: u64 = 2 << 20;

fn history(paged: bool, segment_frames: usize) -> (PathBuf, HistoryWriter) {
    let directory = std::env::temp_dir().join(format!(
        "pvd-active-reclaim-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let config = HistoryConfig {
        directory: directory.clone(),
        disk_budget_bytes: BUDGET,
        segment_target_bytes: BUDGET / 4,
        segment_frames,
        min_free_disk_bytes: 0,
    };
    let mut history = if paged {
        HistoryWriter::new_paged(config)
    } else {
        HistoryWriter::new(config)
    }
    .unwrap();
    let mut metrics = Metrics::default();
    metrics.enable();
    history.set_metrics(metrics);
    (directory, history)
}

fn info(index: usize) -> FrameInfo {
    FrameInfo {
        index,
        timestamp: index as u64 * 10,
        scene_id: 1,
    }
}

fn scene(frame: usize, len: usize) -> SceneState {
    let mut seed = frame as u64 + 1;
    let bytes = (0..len)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as u8
        })
        .collect::<Vec<_>>();
    SceneState {
        objects: [(
            71,
            Object {
                id: 71,
                generation: 0,
                type_name: "Actor".into(),
                properties: [
                    ("frame".into(), Value::U64(frame as u64)),
                    ("payload".into(), Value::Bytes(bytes.into())),
                ]
                .into(),
            },
        )]
        .into(),
        ..Default::default()
    }
}

fn patch(scene: &SceneState) -> SceneUpdate {
    SceneUpdate::capture(scene, &[71].into(), false)
}

fn append(
    history: &mut HistoryWriter,
    frame: usize,
    state: &SceneState,
    analysis: &Analysis,
    inputs: Vec<AnalysisInput>,
) {
    history.append(&info(frame), state, &patch(state), analysis, inputs);
    assert!(history.warning.is_none(), "{:?}", history.warning);
    assert!(history.bytes() <= BUDGET);
}

fn assert_frame(
    disk: &DiskRecording,
    frame: usize,
    scene: &SceneState,
    analysis: &[u8],
    paged: bool,
) {
    assert_eq!(disk.state_at(frame).unwrap(), *scene);
    let actual = disk.analysis_at_with_cancel(frame, &mut || false).unwrap();
    assert_eq!(actual.resident().is_some(), paged);
    assert_eq!(postcard::to_stdvec(&actual).unwrap(), analysis);
}

fn cleanup(directory: PathBuf, history: HistoryWriter) {
    let resources = history.resources.as_ref().unwrap().clone();
    let domain = history
        .analysis_pages
        .as_ref()
        .map(|pages| pages.current().domain().clone());
    drop(history);
    resources.collect().unwrap();
    assert_eq!(resources.physical_bytes(), 0);
    if let Some(domain) = domain {
        assert_eq!(domain.statistics().logical_used, 0);
    }
    drop(resources);
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
    fs::remove_dir(directory).unwrap();
}

#[test]
fn completed_reclamation_keeps_the_active_segment_in_both_analysis_families() {
    for paged in [false, true] {
        let (directory, mut history) = history(paged, 17);
        let buffers = full_fixture_inputs(pvd_protocol::Endian::Little);
        let mut analysis = Analysis::default();
        let mut expected = Vec::new();
        let mut expected_analysis = Vec::new();
        let mut held = None;
        let mut reclaimed_without_closing = 0;
        for frame in 0..512 {
            let state = scene(frame, (12 << 10) + frame % 7 * 317);
            let mut inputs = Vec::new();
            if frame == 0 {
                inputs.extend([
                    AnalysisInput::Zone(3, "Physics".into()),
                    AnalysisInput::Event(3, 4, "simulate".into()),
                ]);
            }
            if let Some((id, kind, bytes)) = buffers.get(frame) {
                inputs.push(AnalysisInput::Buffer {
                    id: *id,
                    kind: (*kind).into(),
                    bytes: bytes.clone().into(),
                    little_endian: true,
                    frame,
                });
            }
            for input in &inputs {
                input.apply(&mut analysis).unwrap();
            }
            let first = history.frame_infos.first().map(|frame| frame.index);
            let active = history.active.as_ref().filter(|segment| {
                !segment.view.frames.is_empty()
                    && segment.view.frames.len() < history.config.segment_frames
                    && segment.payload_bytes() - segment.baseline_bytes
                        < history.config.segment_target_bytes
                    && !history.completed.is_empty()
            });
            let previous = active.map(|segment| {
                (
                    segment.view.path.clone(),
                    segment.view.frames.len(),
                    segment.view.analysis_base,
                )
            });
            append(&mut history, frame, &state, &analysis, inputs);
            expected.push(state);
            expected_analysis.push(postcard::to_stdvec(&analysis).unwrap());
            if frame == 0 {
                let disk = history.snapshot();
                held = Some((
                    disk.state_at(0).unwrap(),
                    disk.analysis_at_with_cancel(0, &mut || false).unwrap(),
                ));
            }
            if let Some((path, frames, baseline)) = previous
                && history.frame_infos.first().map(|frame| frame.index) > first
            {
                let active = history.active.as_ref().unwrap();
                assert_eq!(active.view.path, path, "paged={paged}, frame={frame}");
                assert_eq!(active.view.frames.len(), frames + 1);
                assert_eq!(active.view.analysis_base, baseline);
                reclaimed_without_closing += 1;
                if reclaimed_without_closing == 3 {
                    break;
                }
            }
        }
        assert_eq!(reclaimed_without_closing, 3, "paged={paged}");
        let disk = history.snapshot();
        assert!(disk.stats.first_frame > 0);
        assert!(disk.state_at(0).is_err());
        disk.verify_replay().unwrap();
        for frame in disk.frames().iter() {
            assert_frame(
                &disk,
                frame.index,
                &expected[frame.index],
                &expected_analysis[frame.index],
                paged,
            );
        }
        let (old_scene, old_analysis) = held.as_ref().unwrap();
        assert_eq!(old_scene, &expected[0]);
        assert_eq!(
            postcard::to_stdvec(old_analysis).unwrap(),
            expected_analysis[0]
        );
        let metrics = history.metrics.snapshot();
        for stage in [
            Stage::HistoryReservation,
            Stage::HistoryCollect,
            Stage::HistoryCompact,
            Stage::HistoryEvict,
        ] {
            assert!(metrics.stages[stage as usize].calls > 0, "{stage:?}");
        }
        drop((disk, held));
        cleanup(directory, history);
    }
}

fn fill_the_current_lease(history: &mut HistoryWriter) -> [SceneState; 2] {
    let states = [scene(0, 5 * BUDGET as usize / 8), scene(1, 8 << 10)];
    for (frame, state) in states.iter().enumerate() {
        append(history, frame, state, &Analysis::default(), Vec::new());
    }
    assert!(history.completed.is_empty());
    let active = history.active.as_ref().unwrap();
    assert_eq!(active.view.frames.len(), 2);
    assert!(active.payload_bytes() - active.baseline_bytes < history.config.segment_target_bytes);
    states
}

#[test]
fn exhausting_completed_reclamation_restarts_a_nonempty_segment_with_current_roots() {
    for paged in [false, true] {
        let (directory, mut history) = history(paged, 40);
        let states = fill_the_current_lease(&mut history);
        let before = history.active.as_ref().unwrap().view.path.clone();
        let compactions = history.metrics.snapshot().stages[Stage::HistoryCompact as usize].calls;
        let state = scene(2, BUDGET as usize / 2);
        append(&mut history, 2, &state, &Analysis::default(), Vec::new());
        assert_eq!(
            history.metrics.snapshot().stages[Stage::HistoryCompact as usize].calls - compactions,
            1,
            "paged={paged}"
        );
        let active = history.active.as_ref().unwrap();
        assert_ne!(active.view.path, before);
        assert_eq!(active.view.frames.len(), 1);
        assert_eq!(active.view.frames.first().unwrap().info.index, 2);
        assert!(history.completed.is_empty());
        assert_eq!(history.frame_infos.first().unwrap().index, 2);
        assert!(!before.exists());
        let disk = history.snapshot();
        disk.verify_replay().unwrap();
        assert_frame(
            &disk,
            2,
            &state,
            &postcard::to_stdvec(&Analysis::default()).unwrap(),
            paged,
        );
        for frame in 0..states.len() {
            assert!(disk.state_at(frame).is_err());
        }
        drop(disk);
        cleanup(directory, history);
    }
}

#[test]
fn insufficient_capacity_for_an_empty_segment_does_not_publish_or_retry_empty_files() {
    for paged in [false, true] {
        let (directory, mut history) = history(paged, 40);
        let state = scene(0, 3 << 20);
        history.append(
            &info(0),
            &state,
            &patch(&state),
            &Analysis::default(),
            Vec::new(),
        );
        assert!(history.warning.as_ref().unwrap().contains(
            "current scene and shared history resources cannot fit within the disk history budget"
        ));
        assert_eq!(history.sequence, 1);
        assert!(history.active.is_none());
        assert!(history.completed.is_empty());
        assert!(history.frame_infos.is_empty());
        let disk = history.snapshot();
        assert!(disk.frames().is_empty());
        assert!(disk.state_at(0).is_err());
        assert!(history.bytes() <= BUDGET);
        drop(disk);
        cleanup(directory, history);
    }
}

#[test]
fn reclaim_io_errors_preserve_the_active_directory_and_propagate_without_rollover() {
    for paged in [false, true] {
        let (directory, mut history) = history(paged, 40);
        let states = fill_the_current_lease(&mut history);
        let resources = history.resources.as_ref().unwrap().clone();
        resources.collect().unwrap();
        let owner = history.owner.as_ref().unwrap().clone();
        let before_files = owner.files.lock().unwrap().clone();
        let unused_scene = scene(900, 2 * resources.reserve_bytes() as usize);
        let Value::Bytes(unused_bytes) = &unused_scene.objects[&71].properties["payload"] else {
            panic!("resource fixture type");
        };
        let mut plan = ResourcePlan::default();
        let unused = resources.plan(unused_bytes, &[], &mut plan).unwrap();
        resources.commit(&plan).unwrap();
        let unused_path = owner
            .files
            .lock()
            .unwrap()
            .difference(&before_files)
            .next()
            .unwrap()
            .clone();
        let original_bytes = fs::read(&unused_path).unwrap();
        drop((unused, plan));
        fs::remove_file(&unused_path).unwrap();
        fs::create_dir(&unused_path).unwrap();
        let active = history.active.as_ref().unwrap();
        let original_path = active.view.path.clone();
        let committed = active.view.committed_end;
        let sequence = history.sequence;
        let state = scene(2, BUDGET as usize / 2);
        let failure = history
            .append_inner(
                &info(2),
                &state,
                &patch(&state),
                Some(&Analysis::default()),
                &[],
            )
            .unwrap_err();
        assert!(
            failure.downcast_ref::<std::io::Error>().is_some(),
            "{failure}"
        );
        let active = history.active.as_ref().unwrap();
        assert_eq!(active.view.path, original_path);
        assert_eq!(active.view.committed_end, committed);
        assert_eq!(active.view.frames.len(), 2);
        assert_eq!(history.sequence, sequence);
        assert!(history.completed.is_empty());
        assert_eq!(history.frame_infos.len(), 2);
        fs::remove_dir(&unused_path).unwrap();
        fs::write(&unused_path, original_bytes).unwrap();
        resources.collect().unwrap();
        let disk = history.snapshot();
        for (frame, state) in states.iter().enumerate() {
            assert_frame(
                &disk,
                frame,
                state,
                &postcard::to_stdvec(&Analysis::default()).unwrap(),
                paged,
            );
        }
        assert!(disk.state_at(2).is_err());
        drop((disk, owner, resources));
        cleanup(directory, history);
    }
}

fn progress_history(paged: bool, segment_frames: usize) -> (PathBuf, HistoryWriter, Metrics) {
    let (directory, history) = history(paged, segment_frames);
    let mut recording = crate::LiveRecording {
        history: Some(history),
        ..Default::default()
    };
    let observer = recording.enable_progress_metrics();
    (directory, recording.history.take().unwrap(), observer)
}

fn progress_stage(
    snapshot: &crate::metrics::MetricsSnapshot,
    stage: Stage,
) -> &crate::metrics::StageMetrics {
    snapshot
        .stages
        .iter()
        .find(|entry| entry.stage == stage)
        .unwrap()
}

fn assert_progress_idle(snapshot: &crate::metrics::MetricsSnapshot) {
    assert!(snapshot.enabled);
    assert!(!snapshot.timings_enabled);
    assert!(snapshot.stages.iter().all(|entry| {
        entry.started == entry.completed
            && entry.calls == entry.completed
            && entry.in_flight == 0
            && entry.inclusive_ns == 0
            && entry.self_ns == 0
            && entry.max_ns == 0
    }));
}

#[test]
fn progress_metrics_observe_real_segment_preparation_and_analysis_reclamation() {
    for paged in [false, true] {
        let (directory, mut history, observer) = progress_history(paged, 2);
        for frame in 0..3 {
            let state = scene(frame, 64);
            append(
                &mut history,
                frame,
                &state,
                &Analysis::default(),
                Vec::new(),
            );
            let snapshot = observer.snapshot();
            assert_progress_idle(&snapshot);
            assert_eq!(snapshot.last_committed_frame, Some(frame));
            let segments = 1 + frame / 2;
            assert_eq!(
                progress_stage(&snapshot, Stage::HistoryAppend).completed,
                frame as u64 + 1
            );
            assert_eq!(
                progress_stage(&snapshot, Stage::HistorySceneCheckpoint).completed,
                segments as u64
            );
            assert_eq!(
                progress_stage(&snapshot, Stage::HistoryAnalysisPrepare).completed,
                if paged { segments as u64 } else { 0 }
            );
            let disk = history.snapshot();
            assert_frame(
                &disk,
                frame,
                &state,
                &postcard::to_stdvec(&Analysis::default()).unwrap(),
                paged,
            );
        }
        history.reclaim_analysis().unwrap();
        let snapshot = observer.snapshot();
        assert_progress_idle(&snapshot);
        assert_eq!(
            progress_stage(&snapshot, Stage::HistoryAnalysisReclaim).completed,
            1
        );
        assert_eq!(snapshot.last_committed_frame, Some(2));
        cleanup(directory, history);
    }
}

#[test]
fn progress_metrics_do_not_report_a_commit_when_an_empty_segment_exceeds_capacity() {
    for paged in [false, true] {
        let (directory, mut history, observer) = progress_history(paged, 40);
        let state = scene(0, 3 << 20);
        history.append(
            &info(0),
            &state,
            &patch(&state),
            &Analysis::default(),
            Vec::new(),
        );
        assert!(history.warning.is_some());
        assert!(history.frame_infos.is_empty());
        let snapshot = observer.snapshot();
        assert_progress_idle(&snapshot);
        assert_eq!(snapshot.last_committed_frame, None);
        assert_eq!(progress_stage(&snapshot, Stage::HistoryAppend).completed, 1);
        cleanup(directory, history);
    }
}

#[test]
fn progress_metrics_preserve_the_last_commit_after_a_real_segment_flush_failure() {
    for paged in [false, true] {
        let (directory, mut history, observer) = progress_history(paged, 40);
        let state = scene(0, 64);
        append(&mut history, 0, &state, &Analysis::default(), Vec::new());
        let active = history.active.as_mut().unwrap();
        active.file.flush().unwrap();
        let readonly = File::open(&active.view.path).unwrap();
        #[cfg(not(feature = "resource-test-write-faults"))]
        {
            active.file = BufWriter::with_capacity(256 << 10, readonly);
        }
        #[cfg(feature = "resource-test-write-faults")]
        {
            active.file.get_mut().file = readonly;
        }
        let next = scene(1, 64);
        history.append(
            &info(1),
            &next,
            &patch(&next),
            &Analysis::default(),
            Vec::new(),
        );
        assert!(history.warning.is_some());
        assert_eq!(history.frame_infos.len(), 1);
        assert_eq!(history.frame_infos.first().unwrap().index, 0);
        let snapshot = observer.snapshot();
        assert_progress_idle(&snapshot);
        assert_eq!(snapshot.last_committed_frame, Some(0));
        assert_eq!(progress_stage(&snapshot, Stage::HistoryAppend).completed, 2);
        let disk = history.snapshot();
        assert_frame(
            &disk,
            0,
            &state,
            &postcard::to_stdvec(&Analysis::default()).unwrap(),
            paged,
        );
        assert!(disk.state_at(1).is_err());
        drop(disk);
        cleanup(directory, history);
    }
}

fn mixed_resource_chunks(history: &HistoryWriter, count: usize) -> Vec<(ResourceHandle, Vec<u8>)> {
    let resources = history.resources.as_ref().unwrap();
    resources.collect().unwrap();
    let mut dead = Vec::new();
    let mut retained = Vec::new();
    for index in 0..count {
        let mut plan = ResourcePlan::default();
        let handle = resources
            .plan(&[(index * 2 + 10) as u8; 4000], &[], &mut plan)
            .unwrap();
        resources.commit(&plan).unwrap();
        dead.push(handle);
        drop(plan);
        let bytes = vec![(index * 2 + 11) as u8; 1000];
        let mut plan = ResourcePlan::default();
        let handle = resources.plan(&bytes, &[], &mut plan).unwrap();
        resources.commit(&plan).unwrap();
        retained.push((handle, bytes));
        drop(plan);
        resources.collect().unwrap();
    }
    drop(dead);
    resources.collect().unwrap();
    retained
}

fn assert_history_physical_bytes(history: &HistoryWriter) {
    let actual = history
        .owner
        .as_ref()
        .unwrap()
        .files
        .lock()
        .unwrap()
        .iter()
        .map(|path| fs::metadata(path).unwrap().len())
        .sum::<u64>();
    assert_eq!(history.bytes(), actual);
    assert!(actual <= BUDGET);
}

#[test]
fn completed_segments_bound_opportunistic_compaction_before_eviction() {
    for paged in [false, true] {
        let (directory, mut history) = history(paged, 1);
        let old = scene(0, 16 << 10);
        let current = scene(1, 64);
        append(&mut history, 0, &old, &Analysis::default(), Vec::new());
        let resources = history.resources.as_ref().unwrap().clone();
        resources.collect().unwrap();
        let ballast = scene(900, 16 << 10);
        let Value::Bytes(bytes) = &ballast.objects[&71].properties["payload"] else {
            panic!("resource fixture type")
        };
        let mut plan = ResourcePlan::default();
        let ballast = resources.plan(bytes, &[], &mut plan).unwrap();
        resources.commit(&plan).unwrap();
        history
            .active
            .as_ref()
            .unwrap()
            .view
            .lease
            .extend(vec![ballast])
            .unwrap();
        drop(plan);
        resources.collect().unwrap();
        append(&mut history, 1, &current, &Analysis::default(), Vec::new());
        assert_eq!(history.completed.len(), 1);
        let active = history.active.as_ref().unwrap().view.path.clone();
        let held = {
            let disk = history.snapshot();
            (
                disk.state_at(0).unwrap(),
                disk.analysis_at_with_cancel(0, &mut || false).unwrap(),
            )
        };
        let retained = mixed_resource_chunks(&history, 3);
        let usable = BUDGET - resources.reserve_bytes();
        let reclaimable_per_chunk = 36 + 4000;
        let reservation = usable - history.bytes() + 2 * reclaimable_per_chunk;
        let before = history.metrics.snapshot();
        let mut opportunistic_compaction = true;
        assert!(
            history
                .reclaim(reservation, &resources, &mut opportunistic_compaction)
                .unwrap()
        );
        assert!(!opportunistic_compaction);
        let after = history.metrics.snapshot();
        assert_eq!(
            after.stages[Stage::HistoryCompact as usize].calls
                - before.stages[Stage::HistoryCompact as usize].calls,
            1,
            "paged={paged}"
        );
        assert!(
            after.stages[Stage::HistoryEvict as usize].calls
                > before.stages[Stage::HistoryEvict as usize].calls
        );
        assert!(history.bytes() + reservation <= usable);
        assert!(history.completed.is_empty());
        assert_eq!(history.active.as_ref().unwrap().view.path, active);
        assert_eq!(held.0, old);
        let expected_analysis = postcard::to_stdvec(&Analysis::default()).unwrap();
        assert_eq!(postcard::to_stdvec(&held.1).unwrap(), expected_analysis);
        let disk = history.snapshot();
        assert!(disk.state_at(0).is_err());
        assert_frame(&disk, 1, &current, &expected_analysis, paged);
        for (handle, bytes) in &retained {
            assert_eq!(
                resources.reader().read(handle.id(), &mut || false).unwrap(),
                *bytes
            );
        }
        assert_history_physical_bytes(&history);
        drop((disk, held, retained, resources));
        cleanup(directory, history);
    }
}

#[test]
fn compaction_without_completed_segments_preserves_capacity_recovery() {
    for paged in [false, true] {
        let (directory, mut history) = history(paged, 40);
        let current = scene(0, 64);
        append(&mut history, 0, &current, &Analysis::default(), Vec::new());
        assert!(history.completed.is_empty());
        let retained = mixed_resource_chunks(&history, 3);
        let resources = history.resources.as_ref().unwrap().clone();
        let usable = BUDGET - resources.reserve_bytes();
        let reclaimable_per_chunk = 36 + 4000;
        let reservation = usable - history.bytes() + 2 * reclaimable_per_chunk;
        let before = history.metrics.snapshot();
        let mut opportunistic_compaction = true;
        assert!(
            history
                .reclaim(reservation, &resources, &mut opportunistic_compaction)
                .unwrap()
        );
        assert!(!opportunistic_compaction);
        let after = history.metrics.snapshot();
        assert_eq!(
            after.stages[Stage::HistoryCompact as usize].calls
                - before.stages[Stage::HistoryCompact as usize].calls,
            2,
            "paged={paged}"
        );
        assert_eq!(
            after.stages[Stage::HistoryEvict as usize].calls,
            before.stages[Stage::HistoryEvict as usize].calls
        );
        assert!(history.bytes() + reservation <= usable);
        let disk = history.snapshot();
        assert_frame(
            &disk,
            0,
            &current,
            &postcard::to_stdvec(&Analysis::default()).unwrap(),
            paged,
        );
        for (handle, bytes) in &retained {
            assert_eq!(
                resources.reader().read(handle.id(), &mut || false).unwrap(),
                *bytes
            );
        }
        assert_history_physical_bytes(&history);
        drop((disk, retained, resources));
        cleanup(directory, history);
    }
}
