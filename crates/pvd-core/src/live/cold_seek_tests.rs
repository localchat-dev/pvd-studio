use super::*;
use std::sync::atomic::Ordering;

struct Directory(PathBuf);

impl Drop for Directory {
    fn drop(&mut self) {
        std::fs::remove_dir(&self.0).unwrap();
    }
}

struct Fixture {
    history: HistoryWriter,
    recording: crate::Recording,
    scenes: Vec<SceneState>,
    analyses: Vec<Vec<u8>>,
    _directory: Directory,
}

impl Fixture {
    fn new(frames: usize) -> Self {
        let path = std::env::temp_dir().join(format!(
            "pvd-cold-seek-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut history = HistoryWriter::new(HistoryConfig {
            directory: path.clone(),
            segment_frames: 2,
            min_free_disk_bytes: 0,
            ..Default::default()
        })
        .unwrap();
        let mut state = SceneState::default();
        let mut analysis = Analysis::default();
        let mut scenes = Vec::new();
        let mut analyses = Vec::new();
        for frame in 0..frames {
            state.objects.insert(
                7,
                Object {
                    id: 7,
                    generation: 0,
                    type_name: "Actor".into(),
                    properties: [("Counter".into(), Value::U64(frame as u64))].into(),
                },
            );
            let mut inputs = Vec::new();
            if frame == 0 {
                inputs.push(AnalysisInput::Zone(1, "zone".into()));
                inputs.push(AnalysisInput::Event(1, 8, "event".into()));
            }
            let mut bytes = vec![5, 0, 8, 0, (frame % 255) as u8, 9];
            bytes.extend(42u32.to_le_bytes());
            inputs.push(AnalysisInput::Buffer {
                id: 1,
                kind: "sdk::PxProfileZone".into(),
                bytes: bytes.into(),
                little_endian: true,
                frame,
            });
            for input in &inputs {
                input.apply(&mut analysis).unwrap();
            }
            history.append(
                &FrameInfo {
                    index: frame,
                    timestamp: frame as u64,
                    scene_id: 1,
                },
                &state,
                &SceneUpdate::capture(&state, &[7].into(), false),
                &analysis,
                inputs,
            );
            assert!(history.warning.is_none(), "{:?}", history.warning);
            scenes.push(state.clone());
            analyses.push(postcard::to_stdvec(&analysis).unwrap());
        }
        history.close_segment();
        let mut recording = crate::RecordingBuilder::new(pvd_protocol::Header {
            version: 1,
            timestamp_numerator: 1,
            timestamp_denominator: 1,
            flags: 1,
            endian: pvd_protocol::Endian::Little,
        })
        .recording;
        recording.disk = Some(history.snapshot());
        recording.frames = recording.disk.as_ref().unwrap().frames();
        Self {
            history,
            recording,
            scenes,
            analyses,
            _directory: Directory(path),
        }
    }

    fn check(&self, frame: usize, replay: &ReplayFrame, displayed: &mut SceneState) {
        replay.update.apply(displayed);
        assert_eq!(*displayed, self.scenes[frame], "scene at {frame}");
        assert_eq!(
            postcard::to_stdvec(&replay.analysis).unwrap(),
            self.analyses[frame],
            "analysis at {frame}"
        );
    }
}

#[test]
fn cold_analysis_is_shared_with_new_cursors_and_direct_queries() {
    let mut fixture = Fixture::new(80);
    fixture.recording.disk.as_mut().unwrap().analysis_cache = AnalysisCache::default();
    let mut displayed = SceneState::default();
    let (replay, metrics) = ReplayCursor::default()
        .seek_profiled(&fixture.recording, 0, || false)
        .unwrap();
    assert_eq!(metrics.baseline_loads, 1);
    assert_eq!(metrics.cache_hits, 0);
    assert_eq!(metrics.cursor_cache_bytes, 0);
    fixture.check(0, &replay, &mut displayed);
    let (replay, metrics) = ReplayCursor::default()
        .seek_profiled(&fixture.recording, 0, || false)
        .unwrap();
    assert_eq!(metrics.baseline_loads, 0);
    assert_eq!(metrics.cache_hits, 1);
    fixture.check(0, &replay, &mut displayed);
    fixture.recording.disk.as_mut().unwrap().analysis_cache = AnalysisCache::default();
    assert_eq!(
        postcard::to_stdvec(&fixture.recording.analysis_at(0).unwrap()).unwrap(),
        fixture.analyses[0]
    );
    let (replay, metrics) = ReplayCursor::default()
        .seek_profiled(&fixture.recording, 0, || false)
        .unwrap();
    assert_eq!(metrics.baseline_loads, 0);
    assert_eq!(metrics.cache_hits, 1);
    fixture.check(0, &replay, &mut displayed);
}

#[test]
fn sparse_analysis_anchors_cover_random_and_reverse_seeks_beyond_cache_capacity() {
    let fixture = Fixture::new(100);
    let mut cursor = ReplayCursor::default();
    let mut displayed = SceneState::default();
    for frame in [0, 99, 1, 80, 17, 98]
        .into_iter()
        .chain((0..100).rev().step_by(2))
    {
        let (replay, metrics) = cursor
            .seek_profiled(&fixture.recording, frame, || false)
            .unwrap();
        fixture.check(frame, &replay, &mut displayed);
        assert!(metrics.analysis_frames <= 512, "frame {frame}: {metrics:?}");
        assert!(metrics.shared_cache_entries <= 32);
        assert!(metrics.shared_cache_bytes <= 192 << 20);
        assert_eq!(metrics.cursor_cache_bytes, 0);
    }
}

#[test]
fn nearby_replayed_segment_start_coalesces_while_a_cold_baseline_is_retained() {
    let mut fixture = Fixture::new(80);
    fixture.recording.disk.as_mut().unwrap().analysis_cache = AnalysisCache::default();
    let mut cursor = ReplayCursor::default();
    let mut displayed = SceneState::default();
    let replay = cursor.seek(&fixture.recording, 0, || false).unwrap();
    fixture.check(0, &replay, &mut displayed);
    let (replay, metrics) = cursor
        .seek_profiled(&fixture.recording, 32, || false)
        .unwrap();
    fixture.check(32, &replay, &mut displayed);
    assert_eq!(metrics.baseline_loads, 0);
    let cache = &fixture.recording.disk.as_ref().unwrap().analysis_cache;
    assert!(
        cache
            .describe()
            .iter()
            .any(|entry| entry["frame"] == 32 && entry["baseline"] == true)
    );
    let replay = cursor.seek(&fixture.recording, 40, || false).unwrap();
    fixture.check(40, &replay, &mut displayed);
    let cache = &fixture.recording.disk.as_ref().unwrap().analysis_cache;
    assert!(!cache.describe().iter().any(|entry| entry["frame"] == 40));
    fixture.recording.disk.as_mut().unwrap().analysis_cache = AnalysisCache::default();
    let (replay, metrics) = ReplayCursor::default()
        .seek_profiled(&fixture.recording, 40, || false)
        .unwrap();
    fixture.check(40, &replay, &mut displayed);
    assert_eq!(metrics.baseline_loads, 1);
    let cache = &fixture.recording.disk.as_ref().unwrap().analysis_cache;
    assert!(
        cache
            .describe()
            .iter()
            .any(|entry| entry["frame"] == 40 && entry["baseline"] == true)
    );
}

#[test]
fn analysis_replay_limit_uses_a_cold_target_baseline_when_sparse_cache_is_too_far() {
    let mut fixture = Fixture::new(530);
    fixture.recording.disk.as_mut().unwrap().analysis_cache = AnalysisCache::default();
    let mut cursor = ReplayCursor::default();
    let mut displayed = SceneState::default();
    let replay = cursor.seek(&fixture.recording, 0, || false).unwrap();
    fixture.check(0, &replay, &mut displayed);
    let (replay, metrics) = cursor
        .seek_profiled(&fixture.recording, 529, || false)
        .unwrap();
    assert_eq!(metrics.baseline_loads, 1);
    assert_eq!(metrics.analysis_frames, 1);
    fixture.check(529, &replay, &mut displayed);
}

#[test]
fn segment_replay_coverage_survives_reverse_traversal_under_entry_pressure() {
    let mut fixture = Fixture::new(1200);
    fixture.recording.disk.as_mut().unwrap().analysis_cache =
        AnalysisCache::with_limits(8, 16 << 20);
    for frame in (0..1200).step_by(160) {
        fixture.recording.analysis_at(frame).unwrap();
    }
    let cache = &fixture.recording.disk.as_ref().unwrap().analysis_cache;
    assert_eq!(cache.stats().0, 8);
    let mut cursor = ReplayCursor::default();
    let mut displayed = SceneState::default();
    let mut cold_reads = 0;
    for frame in (18..1200).rev().step_by(20) {
        let (replay, metrics) = cursor
            .seek_profiled(&fixture.recording, frame, || false)
            .unwrap();
        fixture.check(frame, &replay, &mut displayed);
        cold_reads += metrics.baseline_loads;
        assert_eq!(cache.stats().0, 8);
        assert!(cache.stats().1 <= 16 << 20);
        assert!(
            cache
                .describe()
                .iter()
                .all(|entry| entry["baseline"] == true)
        );
    }
    assert_eq!(cold_reads, 0);
}

#[test]
fn expired_anchors_leave_old_snapshots_and_cancellation_retries_valid() {
    let mut fixture = Fixture::new(80);
    let mut cursor = ReplayCursor::default();
    let mut displayed = SceneState::default();
    let replay = cursor.seek(&fixture.recording, 0, || false).unwrap();
    fixture.check(0, &replay, &mut displayed);
    assert!(fixture.history.evict_one().unwrap());
    assert!(!fixture.recording.is_frame_retained(0));
    let cache = &fixture.recording.disk.as_ref().unwrap().analysis_cache;
    assert!(cache.best(0, 1).is_none());
    for stop in [3, 9, 18] {
        let mut checks = 0;
        let result = cursor.seek(&fixture.recording, 79, || {
            checks += 1;
            checks >= stop
        });
        assert!(result.is_err());
        let (replay, metrics) = cursor
            .seek_profiled(&fixture.recording, 79, || false)
            .unwrap();
        assert!(replay.update.replace_all);
        assert!(metrics.analysis_frames <= 1);
        fixture.check(79, &replay, &mut displayed);
        cursor.clear();
    }
    while fixture.history.evict_one().unwrap() {}
    assert_eq!(cache.stats().0, 0);
    assert!(cursor.seek(&fixture.recording, 79, || false).is_err());
}
