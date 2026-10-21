use super::*;
use crate::analysis::resident::{Domain, ResidentAnalysis, full_fixture_inputs};
use std::sync::atomic::Ordering;

mod reclaim_cancel_tests;

struct Fixture {
    history: HistoryWriter,
    recording: crate::Recording,
    scenes: Vec<SceneState>,
    analyses: Vec<Vec<u8>>,
}

impl Fixture {
    fn new(frames: usize, segment_frames: usize, discontinuity: bool) -> Self {
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../output/resident-replay-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
        let mut history = HistoryWriter::new_paged(HistoryConfig {
            directory,
            segment_frames,
            min_free_disk_bytes: 0,
            ..Default::default()
        })
        .unwrap();
        let mut state = SceneState::default();
        let mut expected = Analysis::default();
        let mut scenes = Vec::new();
        let mut analyses = Vec::new();
        let buffers = full_fixture_inputs(pvd_protocol::Endian::Little);
        for frame in 0..frames {
            state.objects.insert(
                7,
                Object {
                    id: 7,
                    generation: (frame / 5) as u32,
                    type_name: "CapturedActor".into(),
                    properties: [("frame".into(), Value::U64(frame as u64))].into(),
                },
            );
            state.generations.insert(7, (frame / 5) as u32);
            if frame % 5 == 4 {
                state.objects.remove(&7);
            }
            let reset = if discontinuity {
                frame == 3
            } else {
                frame == 64
            };
            let mut inputs = Vec::new();
            if reset {
                history.break_analysis_continuity();
                inputs.push(AnalysisInput::Reset(frame));
            }
            if frame == 0 || reset {
                inputs.extend([
                    AnalysisInput::Zone(3, "Physics".into()),
                    AnalysisInput::Event(3, 4, "simulate".into()),
                    AnalysisInput::Zone(1, "Values".into()),
                    AnalysisInput::Event(1, 8, "counter".into()),
                ]);
            }
            if discontinuity {
                if frame == 1 {
                    inputs.push(AnalysisInput::Zone(77, "x".repeat(1 << 20)));
                } else if frame > 3 {
                    inputs.push(AnalysisInput::Zone(
                        80 + frame as u64,
                        format!("frame {frame}"),
                    ));
                }
            } else {
                if let Some((id, kind, bytes)) = buffers.get(frame % 64) {
                    inputs.push(AnalysisInput::Buffer {
                        id: *id,
                        kind: (*kind).into(),
                        bytes: bytes.clone().into(),
                        little_endian: true,
                        frame,
                    });
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
            }
            if !discontinuity && frame == 110 {
                let mut retained = expected.clone();
                for input in &inputs {
                    input.apply(&mut retained).unwrap();
                }
                retained.retain_recent(80);
                inputs.push(AnalysisInput::Retain {
                    first_frame: 80,
                    accounted_bytes: retained.accounted_bytes(),
                });
            }
            for input in &inputs {
                input.apply(&mut expected).unwrap();
            }
            history.append(
                &FrameInfo {
                    index: frame,
                    timestamp: frame as u64,
                    scene_id: 1,
                },
                &state,
                &SceneUpdate::capture(&state, &[7].into(), false),
                &expected,
                inputs,
            );
            assert!(history.warning.is_none(), "{:?}", history.warning);
            scenes.push(state.clone());
            analyses.push(postcard::to_stdvec(&expected).unwrap());
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
        }
    }

    fn check(&self, frame: usize, replay: &ReplayFrame, displayed: &mut SceneState) {
        assert!(replay.analysis.resident().is_some());
        replay.update.apply(displayed);
        assert_eq!(*displayed, self.scenes[frame], "scene at {frame}");
        assert_eq!(
            postcard::to_stdvec(&replay.analysis).unwrap(),
            self.analyses[frame],
            "complete analysis at {frame}"
        );
    }
}

#[test]
fn recent_reverse_uses_complete_roots_and_older_seeks_reconstruct_identical_frames() {
    let fixture = Fixture::new(160, 17, false);
    let domain = fixture
        .history
        .analysis_pages
        .as_ref()
        .unwrap()
        .current()
        .domain()
        .clone();
    let resources = fixture.history.resources.as_ref().unwrap().clone();
    let mut cursor = ReplayCursor::default();
    let mut displayed = SceneState::default();
    let mut old = None;
    for frame in 0..160 {
        let replay = cursor.seek(&fixture.recording, frame, || false).unwrap();
        fixture.check(frame, &replay, &mut displayed);
        if frame == 0 {
            old = Some(replay.analysis);
        }
    }
    let disk = fixture.recording.disk.as_ref().unwrap();
    assert_eq!(disk.analysis_cache.stats().0, 120);
    for frame in (40..159).rev() {
        let (replay, metrics) = cursor
            .seek_profiled(&fixture.recording, frame, || false)
            .unwrap();
        fixture.check(frame, &replay, &mut displayed);
        assert_eq!(metrics.analysis_root_hits, 1, "frame {frame}");
        assert_eq!(metrics.analysis_frames, 0, "frame {frame}");
        assert_eq!(metrics.analysis_inputs, 0, "frame {frame}");
        assert_eq!(metrics.baseline_loads, 0, "frame {frame}");
    }
    let (replay, metrics) = cursor
        .seek_profiled(&fixture.recording, 5, || false)
        .unwrap();
    fixture.check(5, &replay, &mut displayed);
    assert!(metrics.baseline_loads > 0);
    assert_eq!(
        postcard::to_stdvec(old.as_ref().unwrap()).unwrap(),
        fixture.analyses[0]
    );
    drop((replay, cursor, fixture));
    resources.collect().unwrap();
    assert_eq!(resources.physical_bytes(), 0);
    assert!(domain.statistics().logical_used > 0);
    drop(old);
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn failed_or_cancelled_frame_preserves_published_root_and_does_not_reclaim_other_frames() {
    let fixture = Fixture::new(1, 2, false);
    let disk = fixture.recording.disk.as_ref().unwrap();
    let before = disk
        .resident_analysis_at_with_cancel(0, &mut || false)
        .unwrap();
    let before: SharedAnalysis = before.into();
    let domain = before.resident().unwrap().domain().clone();
    let lease = ResourceLease::new(Vec::new());
    disk.analysis_cache.insert(42, before.clone(), &lease);
    let wire = postcard::to_stdvec(&before).unwrap();
    let bytes = domain.statistics().logical_used;
    let inputs = [
        AnalysisInput::Zone(90, "unpublished".into()),
        AnalysisInput::Zone(91, "also unpublished".into()),
    ];
    let mut checks = 0;
    let failure = apply_analysis_frame(disk, &before, &inputs, &mut || {
        checks += 1;
        checks == 4
    })
    .unwrap_err();
    assert_eq!(
        failure.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
    assert_eq!(postcard::to_stdvec(&before).unwrap(), wire);
    assert_eq!(domain.statistics().logical_used, bytes);
    let malformed = [
        inputs[0].clone(),
        AnalysisInput::Buffer {
            id: 4,
            kind: "PxProfileMemoryEventBuffer".into(),
            bytes: Arc::from([255]),
            little_endian: true,
            frame: 1,
        },
    ];
    assert!(apply_analysis_frame(disk, &before, &malformed, &mut || false).is_err());
    assert_eq!(postcard::to_stdvec(&before).unwrap(), wire);
    assert_eq!(domain.statistics().logical_used, bytes);
    assert!(disk.analysis_cache.best(42, 42).is_some());
}

#[test]
fn budget_pressure_reclaims_cached_roots_and_retries_from_the_unchanged_frame() {
    let fixture = Fixture::new(0, 2, false);
    let disk = fixture.recording.disk.as_ref().unwrap();
    let domain = Domain::new(8 << 20);
    let before: SharedAnalysis = ResidentAnalysis::new(domain.clone()).unwrap().into();
    let wire = postcard::to_stdvec(&before).unwrap();
    let mut cached = before.clone();
    cached.add_zone(7, "x".repeat(1 << 20)).unwrap();
    let lease = ResourceLease::new(Vec::new());
    disk.analysis_cache.insert(4, cached, &lease);
    let stats = domain.statistics();
    let pressure = domain.reserve(stats.limit - stats.logical_used).unwrap();
    let inputs = [AnalysisInput::Zone(9, "published after reclaim".into())];
    let after = apply_analysis_frame(disk, &before, &inputs, &mut || false).unwrap();
    assert!(disk.analysis_cache.best(4, 4).is_none());
    assert_eq!(postcard::to_stdvec(&before).unwrap(), wire);
    let mut expected = Analysis::default();
    inputs[0].apply(&mut expected).unwrap();
    assert_eq!(
        postcard::to_stdvec(&after).unwrap(),
        postcard::to_stdvec(&expected).unwrap()
    );
    drop((after, pressure));
    let stats = domain.statistics();
    let pressure = domain.reserve(stats.limit - stats.logical_used).unwrap();
    let failure = apply_analysis_frame(disk, &before, &inputs, &mut || false).unwrap_err();
    assert!(failure.downcast_ref::<ResidentBudgetExceeded>().is_some());
    assert_eq!(postcard::to_stdvec(&before).unwrap(), wire);
    drop((pressure, before));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn discontinuous_baseline_releases_obsolete_checkpoint_before_budget_retry() {
    let fixture = Fixture::new(6, 3, true);
    let disk = fixture.recording.disk.as_ref().unwrap();
    let domain = fixture
        .history
        .analysis_pages
        .as_ref()
        .unwrap()
        .current()
        .domain()
        .clone();
    let mut cursor = ReplayCursor::default();
    let mut displayed = SceneState::default();
    let old = cursor.seek(&fixture.recording, 0, || false).unwrap();
    fixture.check(0, &old, &mut displayed);
    let replay = cursor.seek(&fixture.recording, 1, || false).unwrap();
    fixture.check(1, &replay, &mut displayed);
    drop(replay);
    disk.analysis_cache.clear_resident();
    disk.analysis_pages
        .as_ref()
        .unwrap()
        .clone()
        .reclaim()
        .unwrap();
    let stats = domain.statistics();
    let pressure = domain.reserve(stats.limit - stats.logical_used).unwrap();
    let (replay, metrics) = cursor
        .seek_profiled(&fixture.recording, 3, || false)
        .unwrap();
    assert_eq!(metrics.baseline_loads, 1);
    fixture.check(3, &replay, &mut displayed);
    assert_eq!(
        postcard::to_stdvec(&old.analysis).unwrap(),
        fixture.analyses[0]
    );
    drop((pressure, replay, old, cursor, fixture));
    assert_eq!(domain.statistics().logical_used, 0);
}
