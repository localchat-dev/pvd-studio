#[path = "support/analysis.rs"]
mod analysis_support;
use analysis_support::{edit_legacy_fixture, legacy_fixture};
use pvd_core::analysis::view::ProfileSource;
use pvd_core::{
    Recording,
    frame_metrics::{self, Metric, Status},
};
use std::sync::atomic::AtomicBool;

#[test]
#[ignore = "requires official SDK 3.4 capture and independent QPC callback bounds"]
fn sdk_profile_ticks_and_scene_frame_mapping_match_independent_callback_probe() {
    let path = std::env::var_os("PVD_PROFILE_METRICS_CAPTURE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../captures/physx-3.4-profile-metrics.pvdstream")
        });
    let mut recording = Recording::from_path(&path).unwrap();
    let truth: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path.with_extension("json")).unwrap()).unwrap();
    let scene = truth["scene"].as_u64().unwrap();
    let frequency = truth["counter_frequency"].as_u64().unwrap();
    let scale = recording.header.timestamp_numerator as f64
        / recording.header.timestamp_denominator as f64
        / 1e6;
    assert!((scale - 1000.0 / frequency as f64).abs() < 1e-12);
    assert_eq!(recording.frame_count(), 24);
    let frames: Vec<_> = (0..recording.frame_count()).collect();
    let generation = recording.state_at(0).unwrap().objects[&scene].generation;
    let cancelled = AtomicBool::new(false);
    for step in truth["steps"].as_array().unwrap() {
        let frame = &recording.frames[step["frame"].as_u64().unwrap() as usize];
        assert_eq!(frame.scene_id, scene);
        assert!(
            (step["lower"].as_u64().unwrap()..=step["upper"].as_u64().unwrap())
                .contains(&frame.timestamp)
        );
    }
    let intervals = frame_metrics::sample(
        &recording,
        scene,
        generation,
        Metric::FrameInterval,
        &frames,
        &cancelled,
    )
    .unwrap();
    assert_eq!(intervals[0].status, Status::FirstFrame);
    for (frame, point) in intervals.iter().enumerate().skip(1) {
        assert_eq!(
            point.milliseconds,
            Some(
                (recording.frames[frame].timestamp - recording.frames[frame - 1].timestamp) as f64
                    * scale
            )
        );
    }
    let mut compared = 0;
    for metric in [Metric::Simulate, Metric::Collision, Metric::Solver] {
        let series =
            frame_metrics::sample(&recording, scene, generation, metric, &frames, &cancelled)
                .unwrap();
        assert!(
            series.iter().all(|point| point.status == Status::Recorded),
            "{metric:?}: {series:?}"
        );
        for expected in truth["spans"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|span| span["event"].as_str() == Some(metric.name()))
        {
            assert_eq!(expected["context"].as_u64().unwrap(), scene);
            let start = expected["start_lower"].as_u64().unwrap()
                ..=expected["start_upper"].as_u64().unwrap();
            let end =
                expected["end_lower"].as_u64().unwrap()..=expected["end_upper"].as_u64().unwrap();
            let matching: Vec<_> = recording
                .analysis
                .profile_spans()
                .filter(|span| {
                    span.context == scene
                        && start.contains(&span.start)
                        && end.contains(&span.end)
                        && legacy_fixture(&recording).zones[&span.zone].events[&span.event]
                            == metric.name()
                })
                .collect();
            assert_eq!(matching.len(), 1);
            let span = matching[0];
            assert_eq!(span.thread, frame_metrics::CROSS_THREAD_ID);
            let frame = expected["frame"].as_u64().unwrap() as usize;
            assert_eq!(
                series[frame].milliseconds,
                Some((span.end - span.start) as f64 * scale)
            );
            compared += 1;
        }
    }
    assert_eq!(compared, 72);
    let wrong_generation = frame_metrics::sample(
        &recording,
        scene,
        generation + 1,
        Metric::Solver,
        &frames,
        &cancelled,
    )
    .unwrap();
    assert!(
        wrong_generation
            .iter()
            .all(|point| point.status == Status::OtherGeneration)
    );
    edit_legacy_fixture(&mut recording, |analysis| {
        for span in &mut analysis.spans {
            span.context = 0;
            span.received_frame = usize::MAX;
        }
    });
    let wrong_context = frame_metrics::sample(
        &recording,
        scene,
        generation,
        Metric::Solver,
        &frames,
        &cancelled,
    )
    .unwrap();
    assert!(
        wrong_context
            .iter()
            .all(|point| point.status == Status::Missing)
    );
    println!(
        "24 scene end timestamps and 72 phase start/stop ticks match independent QPC bounds; context/generation isolation verified"
    );
}
