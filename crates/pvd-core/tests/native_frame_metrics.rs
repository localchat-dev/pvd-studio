#[path = "support/analysis.rs"]
mod analysis_support;
use analysis_support::edit_legacy_fixture;
use pvd_core::{
    Recording, RecordingBuilder, RecordingSource,
    analysis::ProfileSpan,
    frame_metrics::{self, Metric, Status},
    profile_curves::{self, ExactKey, ReferenceScene},
};
use pvd_protocol::{Endian, Event, EventGroup, Header, Name};
use std::{path::PathBuf, sync::atomic::AtomicBool};

fn synthetic_stage_recording(reuse_scene: bool) -> Recording {
    let mut builder = RecordingBuilder::new(Header {
        version: 1,
        timestamp_numerator: 1_000_000,
        timestamp_denominator: 1,
        flags: 1,
        endian: Endian::Little,
    });
    let class = Name {
        namespace: 1,
        name: 2,
    };
    let mut events = vec![
        Event::StringHandle {
            handle: 1,
            value: "physx3".into(),
        },
        Event::StringHandle {
            handle: 2,
            value: "PxScene".into(),
        },
        Event::StringHandle {
            handle: 3,
            value: "frame".into(),
        },
        Event::CreateClass { name: class },
        Event::CreateInstance { class, id: 7 },
        Event::CreateInstance { class, id: 8 },
        Event::EndSection {
            id: 7,
            name: 3,
            timestamp: 100,
        },
        Event::EndSection {
            id: 8,
            name: 3,
            timestamp: 125,
        },
        Event::EndSection {
            id: 7,
            name: 3,
            timestamp: 200,
        },
    ];
    if reuse_scene {
        events.extend([
            Event::DestroyInstance { id: 7 },
            Event::CreateInstance { class, id: 7 },
            Event::EndSection {
                id: 7,
                name: 3,
                timestamp: 300,
            },
        ]);
    }
    builder
        .push(EventGroup {
            stream_id: 1,
            timestamp: 0,
            offset: 0,
            events,
        })
        .unwrap();
    let mut recording = builder.finish().unwrap();
    recording.analysis.add_zone(1, "PhysXSDK".into()).unwrap();
    for (index, metric) in [Metric::Simulate, Metric::Collision, Metric::Solver]
        .into_iter()
        .enumerate()
    {
        let event = index as u16 + 1;
        recording
            .analysis
            .add_event(1, event, metric.name().into())
            .unwrap();
        edit_legacy_fixture(&mut recording, |analysis| {
            analysis.spans.push(ProfileSpan {
                zone: 1,
                event,
                thread: frame_metrics::CROSS_THREAD_ID,
                context: 7,
                start: 110,
                end: 150,
                received_frame: 2,
            });
        });
    }
    recording
}

#[test]
fn native_fixed_stages_reject_even_accidental_context_and_scene_id_equality() {
    let mut recording = synthetic_stage_recording(false);
    let cancelled = AtomicBool::new(false);
    assert!(matches!(&recording.source, RecordingSource::SdkStream));
    for metric in [Metric::Simulate, Metric::Collision, Metric::Solver] {
        let sdk = frame_metrics::sample(&recording, 7, 0, metric, &[0, 2], &cancelled).unwrap();
        assert_eq!(sdk[0].status, Status::Missing);
        assert_eq!(sdk[1].status, Status::Recorded);
        assert_eq!(sdk[1].milliseconds, Some(40.0));
    }
    recording.source = RecordingSource::Pvd3Database;
    for metric in [Metric::Simulate, Metric::Collision, Metric::Solver] {
        let message = frame_metrics::sample(&recording, 7, 0, metric, &[0, 2], &cancelled)
            .unwrap_err()
            .to_string();
        assert!(message.contains("PVD3 database"), "{message}");
        assert!(message.contains("SDK source pointers"), "{message}");
        assert!(message.contains("container-local"), "{message}");
        assert!(
            message.contains("explicit reference scene window"),
            "{message}"
        );
    }
    let cancelled = AtomicBool::new(true);
    let message = frame_metrics::sample(&recording, 7, 0, Metric::Solver, &[0, 2], &cancelled)
        .unwrap_err()
        .to_string();
    assert!(message.contains("cancelled"));
}

#[test]
fn native_intervals_keep_scene_generation_and_cancellation_checks() {
    let mut recording = synthetic_stage_recording(true);
    recording.source = RecordingSource::Pvd3Database;
    let cancelled = AtomicBool::new(false);
    let points = frame_metrics::sample(
        &recording,
        7,
        0,
        Metric::FrameInterval,
        &[0, 1, 2, 3],
        &cancelled,
    )
    .unwrap();
    assert_eq!(points[0].status, Status::FirstFrame);
    assert_eq!(points[1].status, Status::OtherScene);
    assert_eq!(points[2].status, Status::Recorded);
    assert_eq!(points[2].milliseconds, Some(100.0));
    assert_eq!(points[3].status, Status::OtherGeneration);
    assert!(
        points
            .iter()
            .filter(|point| point.frame != 2)
            .all(|point| point.milliseconds.is_none())
    );
    let reused =
        frame_metrics::sample(&recording, 7, 1, Metric::FrameInterval, &[3], &cancelled).unwrap();
    assert_eq!(reused[0].status, Status::OtherGeneration);
    assert!(reused[0].milliseconds.is_none());
    assert!(
        frame_metrics::sample(
            &recording,
            7,
            0,
            Metric::FrameInterval,
            &[2],
            &AtomicBool::new(true)
        )
        .unwrap_err()
        .to_string()
        .contains("cancelled")
    );
}

#[test]
fn native_generic_event_curve_uses_explicit_reference_window_without_context_mapping() {
    let mut recording = synthetic_stage_recording(false);
    recording.source = RecordingSource::Pvd3Database;
    edit_legacy_fixture(&mut recording, |analysis| {
        for span in &mut analysis.spans {
            span.context = 0x1234_5678_0000;
        }
    });
    let points = profile_curves::sample(
        &recording,
        ExactKey {
            zone: 1,
            event: 1,
            thread: frame_metrics::CROSS_THREAD_ID,
            context: 0x1234_5678_0000,
        },
        ReferenceScene {
            id: 7,
            generation: 0,
        },
        &[0, 2],
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(points[0].status, profile_curves::Status::FirstFrame);
    assert_eq!(points[1].status, profile_curves::Status::Recorded);
    assert_eq!(points[1].milliseconds, Some(40.0));
    assert_eq!(points[1].occurrence_count, 1);
}

#[test]
#[ignore = "requires paired baseline/rich files saved by installed PVD3 and their actual TCP bytes"]
fn actual_pvd3_frame_intervals_match_the_same_tcp_capture() {
    let directory = std::env::var_os("PVD_NATIVE_VALIDATION_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../captures/native"));
    for stem in ["baseline", "rich"] {
        let native =
            Recording::from_path(directory.join(format!("{stem}-paired-native.pxd2"))).unwrap();
        let raw = Recording::from_path(directory.join(format!("{stem}-paired.pvdstream"))).unwrap();
        assert!(matches!(&native.source, RecordingSource::Pvd3Database));
        assert!(matches!(&raw.source, RecordingSource::SdkStream));
        assert_eq!(native.frame_count(), 120);
        assert_eq!(native.frame_count(), raw.frame_count());
        let frames: Vec<_> = (0..native.frame_count()).collect();
        let cancelled = AtomicBool::new(false);
        let collect = |recording: &Recording, metric| {
            let scene = recording.frames[0].scene_id;
            let generation = recording.state_at(0).unwrap().objects[&scene].generation;
            frame_metrics::sample(recording, scene, generation, metric, &frames, &cancelled)
                .unwrap()
        };
        let actual = collect(&native, Metric::FrameInterval);
        let expected = collect(&raw, Metric::FrameInterval);
        for (actual, expected) in actual.iter().zip(&expected) {
            assert_eq!(actual.frame, expected.frame);
            assert_eq!(actual.status, expected.status);
            assert_eq!(actual.milliseconds, expected.milliseconds);
        }
        assert_eq!(actual[0].status, Status::FirstFrame);
        assert!(
            actual
                .iter()
                .skip(1)
                .all(|point| point.status == Status::Recorded)
        );
        let scene = native.frames[0].scene_id;
        let generation = native.state_at(0).unwrap().objects[&scene].generation;
        assert!(frame_metrics::supports_stages(&native, scene, generation));
        assert_eq!(
            frame_metrics::source_context(&native, scene, generation).unwrap(),
            raw.frames[0].scene_id
        );
        for metric in [Metric::Simulate, Metric::Collision, Metric::Solver] {
            let actual = collect(&native, metric);
            let expected = collect(&raw, metric);
            assert_eq!(actual.len(), expected.len());
            if stem == "rich" {
                assert!(
                    expected.iter().any(|point| point.milliseconds.is_some()),
                    "{stem} {metric:?}"
                );
            }
            for (actual, expected) in actual.iter().zip(&expected) {
                assert_eq!(actual.frame, expected.frame);
                assert_eq!(
                    actual.status, expected.status,
                    "{stem} {metric:?} frame {}",
                    actual.frame
                );
                assert_eq!(
                    actual.milliseconds, expected.milliseconds,
                    "{stem} {metric:?} frame {}",
                    actual.frame
                );
            }
        }
    }
}
