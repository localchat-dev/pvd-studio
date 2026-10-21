#[path = "support/analysis.rs"]
mod analysis_support;
use analysis_support::legacy_fixture;
use pvd_core::analysis::view::ProfileSource;
use pvd_core::{
    LiveRecording, Recording, SceneState, Value,
    frame_metrics::{self, Metric, Status},
    live::{HistoryConfig, ReplayCursor},
    profile_curves::ReferenceScene,
};
use serde::Deserialize;
use std::{collections::BTreeMap, fs, path::PathBuf, sync::atomic::AtomicBool};

#[derive(Deserialize)]
struct Truth {
    counter_frequency: u64,
    rounds: usize,
    scenes: Vec<Scene>,
    frames: Vec<Frame>,
    spans: Vec<Span>,
    open_spans: Vec<OpenSpan>,
}

#[derive(Deserialize)]
struct Scene {
    id: u64,
    actor: u64,
    steps: usize,
}

#[derive(Deserialize)]
struct Frame {
    frame: usize,
    scene: u64,
    local_step: usize,
    lower: u64,
    upper: u64,
    poses: Vec<[f32; 7]>,
}

#[derive(Deserialize)]
struct Span {
    event: String,
    context: u64,
    local_step: usize,
    start_lower: u64,
    start_upper: u64,
    end_lower: u64,
    end_upper: u64,
}

#[derive(Deserialize)]
struct OpenSpan {
    event: String,
    context: u64,
    local_step: usize,
    start_lower: u64,
    start_upper: u64,
}

fn input() -> (PathBuf, Truth, Recording) {
    let path = std::env::var_os("PVD_MULTISCENE_CAPTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../captures/physx-3.4-multiscene.pvdstream")
        });
    let truth = serde_json::from_slice(&fs::read(path.with_extension("json")).unwrap()).unwrap();
    let recording = Recording::from_path(&path).unwrap();
    (path, truth, recording)
}

fn check_poses(state: &SceneState, truth: &Truth, expected: &Frame) {
    assert_eq!(expected.poses.len(), truth.scenes.len());
    for (index, source) in truth.scenes.iter().enumerate() {
        assert_eq!(state.objects[&source.id].type_name, "physx3::PxScene");
        let actor = &state.objects[&source.actor];
        assert_eq!(
            actor.properties["Name"],
            Value::String(if index == 0 { "AsyncBoxA" } else { "AsyncBoxB" }.into())
        );
        assert_eq!(actor.properties["Scene"], Value::Ref(source.id));
        let Value::Transform { position, rotation } = actor.properties["GlobalPose"] else {
            panic!("missing actor pose")
        };
        for (actual, expected_value) in position
            .into_iter()
            .chain(rotation)
            .zip(expected.poses[index])
        {
            assert!(
                (actual - expected_value).abs() < 0.00001,
                "frame {} Scene {} Actor {}: {actual} != {expected_value}",
                expected.frame,
                source.id,
                source.actor
            );
        }
    }
}

#[test]
#[ignore = "requires official SDK 3.4 multiscene producer capture and independent pose/QPC truth"]
fn sdk_interleaved_scenes_preserve_stream_order_and_forward_reverse_disk_history() {
    let (path, truth, recording) = input();
    assert_eq!(truth.rounds, 48);
    assert_eq!(truth.scenes.len(), 2);
    assert_eq!(truth.scenes[0].steps, 96);
    assert_eq!(truth.scenes[1].steps, 48);
    assert_eq!(truth.frames.len(), 144);
    assert_eq!(recording.frame_count(), truth.frames.len());
    recording.verify_replay().unwrap();
    let bytes = fs::read(&path).unwrap();
    let cache = std::env::temp_dir().join(format!(
        "pvd-multiscene-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&cache).unwrap();
    let mut live = LiveRecording::with_history(HistoryConfig {
        directory: cache.clone(),
        disk_budget_bytes: 128 << 20,
        segment_frames: 7,
        min_free_disk_bytes: 0,
        ..HistoryConfig::default()
    })
    .unwrap();
    let mut fragmented = LiveRecording::default();
    for chunk in bytes.chunks(997) {
        live.feed(chunk).unwrap();
        fragmented.feed(chunk).unwrap();
    }
    let disk = live.finish().unwrap();
    let fragmented = fragmented.finish().unwrap();
    assert_eq!(disk.frame_count(), truth.frames.len());
    assert!(disk.disk_usage().unwrap().segments > 1);
    let mut local_steps = BTreeMap::<u64, usize>::new();
    for expected in &truth.frames {
        let info = &recording.frames[expected.frame];
        assert_eq!(info.scene_id, expected.scene);
        assert!((expected.lower..=expected.upper).contains(&info.timestamp));
        let step = local_steps.entry(info.scene_id).or_default();
        assert_eq!(expected.local_step, *step);
        *step += 1;
        let state = recording.state_at(expected.frame).unwrap();
        check_poses(&state, &truth, expected);
        assert_eq!(state, fragmented.state_at(expected.frame).unwrap());
        assert_eq!(state, disk.state_at(expected.frame).unwrap());
    }
    let frames: Vec<_> = (0..truth.frames.len()).collect();
    let cancelled = AtomicBool::new(false);
    for source in &truth.scenes {
        let generation = recording.state_at(0).unwrap().objects[&source.id].generation;
        for metric in [
            Metric::FrameInterval,
            Metric::Simulate,
            Metric::Collision,
            Metric::Solver,
        ] {
            let expected = frame_metrics::sample(
                &recording, source.id, generation, metric, &frames, &cancelled,
            )
            .unwrap();
            let actual =
                frame_metrics::sample(&disk, source.id, generation, metric, &frames, &cancelled)
                    .unwrap();
            for (actual, expected) in actual.iter().zip(expected) {
                assert_eq!(actual.frame, expected.frame);
                assert_eq!(actual.status, expected.status);
                assert_eq!(actual.milliseconds, expected.milliseconds);
            }
        }
    }
    let mut cursor = ReplayCursor::default();
    let mut state = SceneState::default();
    for frame in (0..truth.frames.len()).chain((0..truth.frames.len()).rev()) {
        cursor
            .seek_scene(&disk, frame, || false)
            .unwrap()
            .update
            .apply_owned(&mut state);
        assert_eq!(state, recording.state_at(frame).unwrap());
        check_poses(&state, &truth, &truth.frames[frame]);
    }
    drop((cursor, disk));
    assert_eq!(fs::read_dir(&cache).unwrap().count(), 0);
    fs::remove_dir(cache).unwrap();
    println!(
        "144 SDK boundaries, 288 independent actor poses, 288 continuous forward/reverse scene seeks, zero remaining history files"
    );
}

#[test]
#[ignore = "requires official SDK 3.4 multiscene producer and delegated profiler callback QPC bounds"]
fn sdk_overlapping_scene_phases_match_context_clocks_and_complete_streaming_curves() {
    let (path, truth, recording) = input();
    let scale = recording.header.timestamp_numerator as f64
        / recording.header.timestamp_denominator as f64
        / 1e6;
    assert!((scale - 1000.0 / truth.counter_frequency as f64).abs() < 1e-12);
    assert_eq!(truth.spans.len(), 432);
    assert_eq!(truth.open_spans.len(), 144);
    let frames: Vec<_> = (0..recording.frame_count()).collect();
    let cancelled = AtomicBool::new(false);
    let mut compared = 0;
    for source in &truth.scenes {
        let expected_open: Vec<_> = truth
            .open_spans
            .iter()
            .filter(|span| span.context == source.id)
            .collect();
        assert_eq!(expected_open.len(), source.steps);
        let recorded_open: Vec<_> = legacy_fixture(&recording)
            .open_event_starts()
            .filter(|(key, _)| {
                key.3 == source.id
                    && key.2 == frame_metrics::CROSS_THREAD_ID
                    && recording.analysis.profile_event_name(key.0, key.1).unwrap()
                        == "Basic.rigidBodySolver"
            })
            .collect();
        assert_eq!(recorded_open.len(), 1);
        assert_eq!(recorded_open[0].1.len(), expected_open.len());
        for (step, (timestamp, expected)) in
            recorded_open[0].1.iter().zip(expected_open).enumerate()
        {
            assert_eq!(expected.event, "Basic.rigidBodySolver");
            assert_eq!(expected.local_step, step);
            assert!((expected.start_lower..=expected.start_upper).contains(timestamp));
        }
        let generation = recording.state_at(0).unwrap().objects[&source.id].generation;
        let intervals = frame_metrics::sample(
            &recording,
            source.id,
            generation,
            Metric::FrameInterval,
            &frames,
            &cancelled,
        )
        .unwrap();
        let mut previous = None;
        for (frame, point) in recording.frames.iter().zip(intervals) {
            if frame.scene_id != source.id {
                assert_eq!(point.status, Status::OtherScene);
                assert_eq!(point.milliseconds, None);
                continue;
            }
            if let Some(previous) = previous {
                assert_eq!(point.status, Status::Recorded);
                assert_eq!(
                    point.milliseconds,
                    Some((frame.timestamp - previous) as f64 * scale)
                );
            } else {
                assert_eq!(point.status, Status::FirstFrame);
            }
            previous = Some(frame.timestamp);
        }
        for metric in [Metric::Simulate, Metric::Collision, Metric::Solver] {
            let series = frame_metrics::sample(
                &recording, source.id, generation, metric, &frames, &cancelled,
            )
            .unwrap();
            let streamed = frame_metrics::sample_streaming(
                &mut fs::File::open(&path).unwrap(),
                &recording,
                ReferenceScene {
                    id: source.id,
                    generation,
                },
                metric,
                &frames,
                &cancelled,
                |_| true,
            )
            .unwrap();
            for (loaded, streamed) in series.iter().zip(&streamed.points) {
                assert_eq!(loaded.frame, streamed.frame);
                assert_eq!(loaded.status, streamed.status);
                assert_eq!(loaded.milliseconds, streamed.milliseconds);
            }
            for expected in truth
                .spans
                .iter()
                .filter(|span| span.context == source.id && span.event == metric.name())
            {
                let matching: Vec<_> = recording
                    .analysis
                    .profile_spans()
                    .filter(|span| {
                        span.context == source.id
                            && span.thread == frame_metrics::CROSS_THREAD_ID
                            && (expected.start_lower..=expected.start_upper).contains(&span.start)
                            && (expected.end_lower..=expected.end_upper).contains(&span.end)
                            && recording
                                .analysis
                                .profile_event_name(span.zone, span.event)
                                .unwrap()
                                == expected.event
                    })
                    .collect();
                assert_eq!(
                    matching.len(),
                    1,
                    "{} Scene {} step {}",
                    expected.event,
                    source.id,
                    expected.local_step
                );
                let frame = truth
                    .frames
                    .iter()
                    .find(|frame| {
                        frame.scene == source.id && frame.local_step == expected.local_step
                    })
                    .unwrap()
                    .frame;
                assert_eq!(series[frame].status, Status::Recorded);
                assert_eq!(
                    series[frame].milliseconds,
                    Some((matching[0].end - matching[0].start) as f64 * scale)
                );
                compared += 1;
            }
            for (frame, point) in recording.frames.iter().zip(&series) {
                if frame.scene_id != source.id {
                    assert_eq!(point.status, Status::OtherScene);
                    assert_eq!(point.milliseconds, None);
                }
            }
        }
    }
    assert_eq!(compared, 432);
    let mut overlaps = 0;
    for round in 0..truth.rounds {
        let find = |scene: usize, local_step| {
            truth
                .spans
                .iter()
                .find(|span| {
                    span.event == "Basic.simulate"
                        && span.context == truth.scenes[scene].id
                        && span.local_step == local_step
                })
                .unwrap()
        };
        let first = find(0, round * 2);
        let second = find(1, round);
        assert!(first.start_upper < second.start_lower);
        assert!(second.start_upper < first.end_lower.min(second.end_lower));
        overlaps += 1;
    }
    println!(
        "{compared} independent phase spans, {overlaps} overlapping Scene simulate intervals, six complete streaming curves; overlap does not prove simultaneous CPU execution"
    );
}
