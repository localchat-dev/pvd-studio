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
    fn new(frames: usize, segment_frames: usize, barriers: bool, large: bool) -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../output/replay-reuse-tests")
            .join(format!(
                "session-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
        let mut history = HistoryWriter::new(HistoryConfig {
            directory: path.clone(),
            segment_frames,
            min_free_disk_bytes: 0,
            ..Default::default()
        })
        .unwrap();
        let mut state = SceneState::default();
        let mut analysis = Analysis::default();
        let mut scenes = Vec::new();
        let mut analyses = Vec::new();
        for frame in 0..frames {
            let generation = if barriers { (frame / 3) as u32 } else { 0 };
            state.objects.insert(
                7,
                Object {
                    id: 7,
                    generation,
                    type_name: "Actor".into(),
                    properties: [("Counter".into(), Value::U64(frame as u64))].into(),
                },
            );
            if barriers {
                state.generations.insert(7, generation);
                if frame % 3 == 2 {
                    state.objects.remove(&7);
                    state.cameras.remove("source");
                } else {
                    state.cameras.insert(
                        "source".into(),
                        SourceCamera {
                            position: [frame as f32, 1.0, 2.0],
                            up: [0.0, 1.0, 0.0],
                            target: [0.0; 3],
                        },
                    );
                }
                if frame % 4 == 0 {
                    state.errors.push(ErrorEvent {
                        code: frame as u32,
                        message: "sample".into(),
                        file: "fixture".into(),
                        line: 1,
                        timestamp: frame as u64,
                    });
                }
            }
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
            if barriers && frame == 5 {
                inputs.insert(0, AnalysisInput::Reset(frame));
                inputs.insert(1, AnalysisInput::Zone(1, "reset zone".into()));
                inputs.insert(2, AnalysisInput::Event(1, 8, "reset event".into()));
            }
            if barriers && frame == 7 {
                inputs.clear();
            }
            if large && frame == 1 {
                inputs.push(AnalysisInput::Buffer {
                    id: 99,
                    kind: "unknown".into(),
                    bytes: vec![7; REUSED_ANALYSIS_BYTES].into(),
                    little_endian: true,
                    frame,
                });
            }
            if barriers && frame == 3 {
                let mut retained = analysis.clone();
                for input in &inputs {
                    input.apply(&mut retained).unwrap();
                }
                retained.retain_recent(2);
                inputs.push(AnalysisInput::Retain {
                    first_frame: 2,
                    accounted_bytes: retained.accounted_bytes(),
                });
            }
            for input in &inputs {
                input.apply(&mut analysis).unwrap();
            }
            let mut update = SceneUpdate::capture(&state, &[7].into(), barriers);
            if barriers {
                update
                    .cameras
                    .insert("source".into(), state.cameras.get("source").copied());
            }
            history.append(
                &FrameInfo {
                    index: frame,
                    timestamp: frame as u64,
                    scene_id: 1,
                },
                &state,
                &update,
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
        recording.disk.as_mut().unwrap().analysis_cache = AnalysisCache::default();
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
fn transient_inputs_are_bounded_by_capacity_and_release_payloads() {
    assert!(reusable_analysis_inputs(Vec::new()).is_some());
    let bytes: Arc<[u8]> = vec![1, 2, 3].into();
    let weak = Arc::downgrade(&bytes);
    let inputs = vec![AnalysisInput::Buffer {
        id: 1,
        kind: "unknown".into(),
        bytes,
        little_endian: true,
        frame: 0,
    }];
    let inputs = reusable_analysis_inputs(inputs).unwrap();
    assert!(weak.upgrade().is_some());
    drop(inputs);
    assert!(weak.upgrade().is_none());
    let excessive_capacity =
        Vec::with_capacity(REUSED_ANALYSIS_BYTES / size_of::<AnalysisInput>() + 1);
    assert!(reusable_analysis_inputs(excessive_capacity).is_none());
    let mut name = String::with_capacity(REUSED_ANALYSIS_BYTES);
    name.push('x');
    assert!(reusable_analysis_inputs(vec![AnalysisInput::Zone(1, name)]).is_none());
    let bytes: Arc<[u8]> = vec![0; REUSED_ANALYSIS_BYTES].into();
    let weak = Arc::downgrade(&bytes);
    assert!(
        reusable_analysis_inputs(vec![AnalysisInput::Buffer {
            id: 1,
            kind: "unknown".into(),
            bytes,
            little_endian: true,
            frame: 0,
        }])
        .is_none()
    );
    assert!(weak.upgrade().is_none());
}

#[test]
fn forward_reverse_boundaries_reset_and_retention_match_complete_states() {
    let fixture = Fixture::new(14, 4, true, false);
    let mut cursor = ReplayCursor::default();
    let mut displayed = SceneState::default();
    for frame in (0..14)
        .chain((0..13).rev())
        .chain([9, 10, 10, 11, 2, 3, 4, 5])
    {
        let replay = cursor.seek(&fixture.recording, frame, || false).unwrap();
        fixture.check(frame, &replay, &mut displayed);
    }
}

#[test]
fn mismatched_frame_metadata_is_rejected_and_retry_has_no_partial_cursor() {
    let mut fixture = Fixture::new(3, 4, false, false);
    let mut cursor = ReplayCursor::default();
    let mut displayed = SceneState::default();
    let replay = cursor.seek(&fixture.recording, 0, || false).unwrap();
    fixture.check(0, &replay, &mut displayed);
    let original = fixture.recording.disk.as_ref().unwrap().segments.clone();
    let mut modified = original.clone();
    Arc::make_mut(&mut Arc::make_mut(&mut modified)[0]).frames[1]
        .info
        .timestamp += 1;
    fixture.recording.disk.as_mut().unwrap().segments = modified;
    assert!(cursor.seek(&fixture.recording, 1, || false).is_err());
    assert!(cursor.current.is_none());
    fixture.recording.disk.as_mut().unwrap().segments = original;
    let replay = cursor.seek(&fixture.recording, 1, || false).unwrap();
    assert!(replay.update.replace_all);
    fixture.check(1, &replay, &mut displayed);
}

#[test]
fn corrupted_or_truncated_adjacent_records_fail_before_publication() {
    let fixture = Fixture::new(3, 4, false, false);
    let segment = fixture.recording.disk.as_ref().unwrap().segments[0].clone();
    let original = std::fs::read(&segment.path).unwrap();
    let offset = segment.frames[1].offset as usize;
    assert!(offset + 36 < original.len());
    for truncate in [false, true] {
        let mut cursor = ReplayCursor::default();
        let mut displayed = SceneState::default();
        let replay = cursor.seek(&fixture.recording, 0, || false).unwrap();
        fixture.check(0, &replay, &mut displayed);
        let mut damaged = original.clone();
        if truncate {
            damaged.truncate(offset + 36);
        } else {
            damaged[offset + 36] ^= 1;
        }
        cursor.current.as_mut().unwrap().reader = DiskRecording::open(&segment).unwrap();
        cursor
            .current
            .as_mut()
            .unwrap()
            .reader
            .buffer
            .seek(SeekFrom::Start(8))
            .unwrap();
        std::fs::write(&segment.path, &damaged).unwrap();
        assert!(cursor.seek(&fixture.recording, 1, || false).is_err());
        assert!(cursor.current.is_none());
        std::fs::write(&segment.path, &original).unwrap();
        let replay = cursor.seek(&fixture.recording, 1, || false).unwrap();
        assert!(replay.update.replace_all);
        fixture.check(1, &replay, &mut displayed);
    }
}

#[test]
fn expired_segment_is_not_served_from_previous_cursor_state() {
    let mut fixture = Fixture::new(6, 2, false, false);
    let mut cursor = ReplayCursor::default();
    let mut displayed = SceneState::default();
    let replay = cursor.seek(&fixture.recording, 0, || false).unwrap();
    fixture.check(0, &replay, &mut displayed);
    assert!(fixture.history.evict_one().unwrap());
    assert!(cursor.seek(&fixture.recording, 1, || false).is_err());
    let replay = cursor.seek(&fixture.recording, 2, || false).unwrap();
    fixture.check(2, &replay, &mut displayed);
}

#[cfg(feature = "replay-test-read-counts")]
mod counts {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Trace {
        reads: Vec<usize>,
        validated: Vec<usize>,
        payloads: Vec<Weak<[u8]>>,
    }
    thread_local! { static TRACE: RefCell<Option<Trace>> = const { RefCell::new(None) }; }

    pub(super) struct Guard;
    impl Guard {
        fn new() -> Self {
            TRACE.with(|trace| {
                assert!(trace.borrow().is_none());
                *trace.borrow_mut() = Some(Trace::default());
            });
            Self
        }
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            TRACE.with(|trace| *trace.borrow_mut() = None);
        }
    }
    pub(crate) fn record_read(frame: usize) {
        TRACE.with(|trace| {
            if let Some(trace) = trace.borrow_mut().as_mut() {
                trace.reads.push(frame);
            }
        });
    }
    pub(crate) fn record_validated(frame: usize, inputs: &[AnalysisInput]) {
        TRACE.with(|trace| {
            if let Some(trace) = trace.borrow_mut().as_mut() {
                trace.validated.push(frame);
                trace
                    .payloads
                    .extend(inputs.iter().filter_map(|input| match input {
                        AnalysisInput::Buffer { bytes, .. } => Some(Arc::downgrade(bytes)),
                        _ => None,
                    }));
            }
        });
    }
    fn reads(frame: usize) -> usize {
        TRACE.with(|trace| {
            trace
                .borrow()
                .as_ref()
                .unwrap()
                .reads
                .iter()
                .filter(|&&value| value == frame)
                .count()
        })
    }
    fn validated(frame: usize) -> bool {
        TRACE.with(|trace| trace.borrow().as_ref().unwrap().validated.contains(&frame))
    }
    fn released() -> bool {
        TRACE.with(|trace| {
            trace
                .borrow()
                .as_ref()
                .unwrap()
                .payloads
                .iter()
                .all(|value| value.upgrade().is_none())
        })
    }

    #[test]
    fn adjacent_inputs_are_read_once_and_oversize_uses_original_path() {
        for large in [false, true] {
            let fixture = Fixture::new(5, 3, false, large);
            let mut cursor = ReplayCursor::default();
            let mut displayed = SceneState::default();
            let replay = cursor.seek(&fixture.recording, 0, || false).unwrap();
            fixture.check(0, &replay, &mut displayed);
            let _guard = Guard::new();
            for frame in 1..5 {
                let replay = cursor.seek(&fixture.recording, frame, || false).unwrap();
                fixture.check(frame, &replay, &mut displayed);
                assert_eq!(reads(frame), if large && frame == 1 { 2 } else { 1 });
                assert!(released());
            }
            let before = reads(4);
            let replay = cursor.seek(&fixture.recording, 4, || false).unwrap();
            fixture.check(4, &replay, &mut displayed);
            assert_eq!(reads(4), before);
        }
    }

    #[test]
    fn discontinuous_segment_uses_its_baseline_and_drops_unused_inputs() {
        let mut fixture = Fixture::new(5, 3, false, false);
        let mut cursor = ReplayCursor::default();
        let mut displayed = SceneState::default();
        for frame in 0..3 {
            let replay = cursor.seek(&fixture.recording, frame, || false).unwrap();
            fixture.check(frame, &replay, &mut displayed);
        }
        let disk = fixture.recording.disk.as_mut().unwrap();
        Arc::make_mut(&mut Arc::make_mut(&mut disk.segments)[1]).analysis_continuity = false;
        let _guard = Guard::new();
        let (replay, metrics) = cursor
            .seek_profiled(&fixture.recording, 3, || false)
            .unwrap();
        fixture.check(3, &replay, &mut displayed);
        assert_eq!(metrics.baseline_loads, 1);
        assert_eq!(metrics.analysis_inputs, 0);
        assert_eq!(reads(3), 1);
        assert!(released());
    }

    #[test]
    fn cancellation_after_validated_read_releases_inputs_and_retry_rebuilds() {
        let fixture = Fixture::new(4, 4, false, false);
        let mut cursor = ReplayCursor::default();
        let mut displayed = SceneState::default();
        let replay = cursor.seek(&fixture.recording, 0, || false).unwrap();
        fixture.check(0, &replay, &mut displayed);
        {
            let _guard = Guard::new();
            assert!(cursor.seek(&fixture.recording, 1, || validated(1)).is_err());
            assert_eq!(reads(1), 1);
            assert!(validated(1));
            assert!(released());
            assert!(cursor.current.is_none());
        }
        let replay = cursor.seek(&fixture.recording, 1, || false).unwrap();
        assert!(replay.update.replace_all);
        fixture.check(1, &replay, &mut displayed);
    }

    #[test]
    fn expiration_after_validated_read_cannot_publish_reused_inputs() {
        let fixture = Fixture::new(4, 4, false, false);
        let mut cursor = ReplayCursor::default();
        let mut displayed = SceneState::default();
        let replay = cursor.seek(&fixture.recording, 0, || false).unwrap();
        fixture.check(0, &replay, &mut displayed);
        let lease = fixture.recording.disk.as_ref().unwrap().segments[0]
            .lease
            .clone();
        let _guard = Guard::new();
        let result = cursor.seek(&fixture.recording, 1, || {
            if validated(1) {
                lease.invalidate().unwrap();
            }
            false
        });
        assert!(result.is_err());
        assert!(validated(1));
        assert!(released());
        assert!(cursor.current.is_none());
    }
}

#[cfg(feature = "replay-test-read-counts")]
pub(super) use counts::{record_read, record_validated};

mod scene_tests;

mod scene_probe;
