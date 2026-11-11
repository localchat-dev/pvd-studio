use super::*;
use crate::{
    analysis::ProfileSpan,
    frame_metrics::{self, Metric, Status},
};
use std::sync::atomic::AtomicBool;

fn builder(reuse: bool) -> RecordingBuilder {
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
    if reuse {
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
    builder.recording.source = RecordingSource::Pvd3Database;
    builder
}

fn recording() -> Recording {
    let mut builder = builder(false);
    install(
        &mut builder,
        &BTreeMap::from([(7, 0x1000), (8, 0x2000)]),
        &mut 0,
        &mut || true,
    )
    .unwrap();
    builder
        .recording
        .analysis
        .add_zone(1, "PhysXSDK".into())
        .unwrap();
    builder
        .recording
        .analysis
        .add_event(1, 1, Metric::Simulate.name().into())
        .unwrap();
    builder
        .recording
        .analysis
        .legacy_mut()
        .unwrap()
        .spans
        .extend([
            ProfileSpan {
                zone: 1,
                event: 1,
                thread: frame_metrics::CROSS_THREAD_ID,
                context: 0x1000,
                start: 110,
                end: 150,
                received_frame: 3,
            },
            ProfileSpan {
                zone: 1,
                event: 1,
                thread: frame_metrics::CROSS_THREAD_ID,
                context: 0x2000,
                start: 111,
                end: 151,
                received_frame: 3,
            },
        ]);
    builder.finish().unwrap()
}

#[test]
fn source_context_survives_clone_and_preserves_scene_and_generation_checks() {
    let recording = recording();
    let cloned = recording.clone();
    assert!(Arc::ptr_eq(
        recording.native_scene_contexts.as_ref().unwrap(),
        cloned.native_scene_contexts.as_ref().unwrap(),
    ));
    assert_eq!(
        frame_metrics::source_context(&cloned, 7, 0).unwrap(),
        0x1000
    );
    assert!(frame_metrics::source_context(&cloned, 7, 1).is_err());
    let points = frame_metrics::sample(
        &cloned,
        7,
        0,
        Metric::Simulate,
        &[0, 1, 2],
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(points[0].status, Status::Missing);
    assert_eq!(points[1].status, Status::OtherScene);
    assert_eq!(points[2].status, Status::Recorded);
    assert_eq!(points[2].milliseconds, Some(40.0));
    assert_eq!(
        cloned.analysis.profile_spans().next().unwrap().context,
        0x1000
    );
}

#[test]
fn reused_source_across_local_instances_and_generations_stays_unavailable() {
    for reuse in [false, true] {
        let mut builder = builder(reuse);
        let sources = BTreeMap::from([(7, 0x1000), (8, if reuse { 0x2000 } else { 0x1000 })]);
        install(&mut builder, &sources, &mut 0, &mut || true).unwrap();
        let recording = builder.finish().unwrap();
        assert!(!frame_metrics::supports_stages(&recording, 7, 0));
        assert!(frame_metrics::source_context(&recording, 7, 0).is_err());
        if reuse {
            assert!(!frame_metrics::supports_stages(&recording, 7, 1));
        } else {
            assert!(!frame_metrics::supports_stages(&recording, 8, 0));
        }
        let points = frame_metrics::sample(
            &recording,
            7,
            0,
            Metric::FrameInterval,
            &[0, 1, 2],
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(points[1].status, Status::OtherScene);
        assert_eq!(points[2].milliseconds, Some(100.0));
    }
}

#[test]
fn absent_metadata_does_not_infer_a_context_from_equal_local_id() {
    let mut builder = builder(false);
    install(&mut builder, &BTreeMap::from([(8, 8)]), &mut 0, &mut || {
        true
    })
    .unwrap();
    let recording = builder.finish().unwrap();
    assert!(!frame_metrics::supports_stages(&recording, 7, 0));
    assert!(frame_metrics::source_context(&recording, 7, 0).is_err());
    assert_eq!(frame_metrics::source_context(&recording, 8, 0).unwrap(), 8);
}

#[test]
fn cancelled_or_budget_refused_mapping_is_not_published() {
    let sources = BTreeMap::from([(7, 0x1000), (8, 0x2000)]);
    let mut cancelled_builder = builder(false);
    let mut calls = 0;
    let action_count = cancelled_builder.recording.actions.len();
    assert!(
        install(&mut cancelled_builder, &sources, &mut 0, &mut || {
            calls += 1;
            calls <= action_count + 1
        })
        .is_err()
    );
    assert!(cancelled_builder.recording.native_scene_contexts.is_none());
    let mut metadata_builder = builder(false);
    let mut metadata_bytes = MAX_METADATA;
    assert!(
        install(
            &mut metadata_builder,
            &sources,
            &mut metadata_bytes,
            &mut || true
        )
        .is_err()
    );
    assert!(metadata_builder.recording.native_scene_contexts.is_none());
    let mut history_builder = builder(false);
    history_builder.memory_limit = history_builder.retained_bytes;
    assert!(install(&mut history_builder, &sources, &mut 0, &mut || true).is_err());
    assert!(history_builder.recording.native_scene_contexts.is_none());
}

#[test]
fn collected_scene_lifetimes_exclude_reused_sources_without_an_action_log() {
    let mut builder = builder(false);
    let sources = BTreeMap::from([(7, 0x1000), (8, 0x2000)]);
    let mut contexts = Collector::from_state(&mut builder, &sources, &mut 0, &mut || true).unwrap();
    let class = Name {
        namespace: 1,
        name: 2,
    };
    for event in [
        Event::DestroyInstance { id: 7 },
        Event::CreateInstance { class, id: 7 },
    ] {
        push(&mut builder, event).unwrap();
    }
    contexts.observe(&mut builder, &sources, 7, &mut 0).unwrap();
    builder.recording.actions = Default::default();
    contexts.install(&mut builder);
    let contexts = builder.recording.native_scene_contexts.as_ref().unwrap();
    assert_eq!(contexts.source(8, 0), Some(0x2000));
    assert_eq!(contexts.source(7, 0), None);
    assert_eq!(contexts.source(7, 1), None);
}

#[test]
fn cancelled_collector_does_not_publish_a_mapping() {
    let mut builder = builder(false);
    let failure = Collector::from_state(&mut builder, &BTreeMap::new(), &mut 0, &mut || false)
        .err()
        .unwrap();
    assert_eq!(
        failure.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
    assert!(builder.recording.native_scene_contexts.is_none());
}

#[test]
fn mapped_stages_preserve_window_duplicate_clock_and_cancellation_checks() {
    let mut recording = recording();
    let cancel = AtomicBool::new(false);
    let analysis = recording.analysis.legacy_mut().unwrap();
    let mut duplicate = analysis.spans[0].clone();
    duplicate.start = 120;
    duplicate.end = 160;
    analysis.spans.push(duplicate);
    let point = frame_metrics::sample(&recording, 7, 0, Metric::Simulate, &[2], &cancel).unwrap();
    assert_eq!(point[0].status, Status::Ambiguous);
    let analysis = recording.analysis.legacy_mut().unwrap();
    analysis.spans.pop();
    analysis.spans[0].start = 90;
    let point = frame_metrics::sample(&recording, 7, 0, Metric::Simulate, &[2], &cancel).unwrap();
    assert_eq!(point[0].status, Status::CrossBoundary);
    recording.frames[2].timestamp = 100;
    let point = frame_metrics::sample(&recording, 7, 0, Metric::Simulate, &[2], &cancel).unwrap();
    assert_eq!(point[0].status, Status::NonMonotonicClock);
    assert!(
        frame_metrics::sample(
            &recording,
            7,
            0,
            Metric::Simulate,
            &[2],
            &AtomicBool::new(true)
        )
        .is_err()
    );
}
