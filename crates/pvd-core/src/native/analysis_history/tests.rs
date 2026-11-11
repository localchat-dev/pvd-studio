use super::*;
use crate::{
    RecordingBuilder,
    analysis::view::MemorySource,
    frame_metrics::{self, Metric},
    native::{self, ProfileCatalogEntry},
    profile_curves::{self, ExactKey, ReferenceScene},
};
use pvd_protocol::{Endian, Event, Header, Name};
use std::{collections::BTreeMap, path::PathBuf};

fn profile(kind: u8, event: u16, thread: u32, context: u8, timestamp: u64) -> Vec<u8> {
    let mut bytes = vec![kind, 3];
    bytes.extend(event.to_le_bytes());
    bytes.extend(thread.to_le_bytes());
    bytes.extend([context, 0, 0]);
    bytes.extend(timestamp.to_le_bytes());
    bytes
}

fn buffer(builder: &mut RecordingBuilder, events: &[(u8, u16, u32, u8, u64)]) {
    let bytes = events
        .iter()
        .flat_map(|&(kind, event, thread, context, timestamp)| {
            profile(kind, event, thread, context, timestamp)
        })
        .collect::<Vec<_>>();
    let frame = native::received_frame(builder);
    native::apply_analysis_input(
        builder,
        crate::live::AnalysisInput::Buffer {
            id: 10,
            kind: "_debugger_::PxProfileZone".into(),
            bytes: bytes.into(),
            little_endian: true,
            frame,
        },
        &mut || true,
    )
    .unwrap();
}

fn allocate_tail_memory(builder: &mut RecordingBuilder) {
    let mut bytes = Vec::new();
    for (handle, text) in [(7u32, "Allocation"), (9, "Source.cpp")] {
        bytes.extend(1u16.to_le_bytes());
        bytes.extend((text.len() as u32 + 1).to_le_bytes());
        bytes.extend_from_slice(text.as_bytes());
        bytes.push(0);
        bytes.extend(handle.to_le_bytes());
    }
    bytes.extend((2u16 | (3 << 4) | (2 << 6) | (2 << 8) | (2 << 10) | (2 << 12)).to_le_bytes());
    for word in [64u32, 7, 9, 42] {
        bytes.extend(word.to_le_bytes());
    }
    bytes.extend(0x8000u64.to_le_bytes());
    let frame = native::received_frame(builder);
    native::apply_analysis_input(
        builder,
        crate::live::AnalysisInput::Buffer {
            id: 11,
            kind: "_debugger_::PxProfileMemoryEventBuffer".into(),
            bytes: bytes.into(),
            little_endian: true,
            frame,
        },
        &mut || true,
    )
    .unwrap();
}

fn fixture(config: Option<crate::live::HistoryConfig>) -> Recording {
    let mut builder = RecordingBuilder::new(Header {
        version: 1,
        timestamp_numerator: 1_000_000,
        timestamp_denominator: 1,
        flags: 0,
        endian: Endian::Little,
    });
    builder.recording.source = RecordingSource::Pvd3Database;
    if let Some(config) = config {
        let history = crate::live::HistoryWriter::new_paged(config).unwrap();
        builder.recording.analysis = history.initial_analysis().unwrap();
        builder.streaming = Some(Box::new(crate::live::Streaming::new(history)));
    }
    let class = Name {
        namespace: 1,
        name: 2,
    };
    for event in [
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
    ] {
        native::push(&mut builder, event).unwrap();
    }
    native::register_profile_catalog(
        &mut builder,
        vec![ProfileCatalogEntry {
            local_id: 10,
            name: "PhysXSDK".into(),
            events: [
                "Basic.simulate",
                "nested",
                "late",
                "crossing",
                "open",
                "unmatched",
            ]
            .into_iter()
            .enumerate()
            .map(|(index, name)| (index as u16 + 1, name.into(), true))
            .collect(),
        }],
    )
    .unwrap();
    let contexts = native::contexts::Collector::from_state(
        &mut builder,
        &BTreeMap::from([(7, 0x11), (8, 0x12)]),
        &mut 0,
        &mut || true,
    )
    .unwrap();
    native::push(
        &mut builder,
        Event::EndSection {
            id: 7,
            name: 3,
            timestamp: 100,
        },
    )
    .unwrap();
    let cross = frame_metrics::CROSS_THREAD_ID;
    buffer(
        &mut builder,
        &[
            (1, 1, cross, 0x11, 110),
            (2, 1, cross, 0x11, 120),
            (1, 2, 42, 9, 111),
            (1, 2, 42, 9, 112),
            (2, 2, 42, 9, 115),
            (2, 2, 42, 9, 117),
        ],
    );
    for (scene, time) in [(8, 150), (7, 200), (7, 300)] {
        native::push(
            &mut builder,
            Event::EndSection {
                id: scene,
                name: 3,
                timestamp: time,
            },
        )
        .unwrap();
    }
    buffer(
        &mut builder,
        &[
            (1, 1, cross, 0x11, 210),
            (2, 1, cross, 0x11, 230),
            (1, 3, 42, 9, 210),
            (2, 3, 42, 9, 240),
        ],
    );
    native::push(
        &mut builder,
        Event::EndSection {
            id: 7,
            name: 3,
            timestamp: 400,
        },
    )
    .unwrap();
    buffer(
        &mut builder,
        &[
            (1, 1, cross, 0x11, 310),
            (2, 1, cross, 0x11, 330),
            (1, 2, 42, 9, 310),
            (2, 2, 42, 9, 340),
            (1, 4, 42, 9, 390),
            (2, 4, 42, 9, 410),
            (1, 5, 42, 9, 350),
            (2, 6, 42, 9, 360),
        ],
    );
    allocate_tail_memory(&mut builder);
    contexts.install(&mut builder);
    builder.finish().unwrap()
}

fn directory() -> PathBuf {
    std::env::temp_dir().join(format!(
        "pvd-native-profile-roots-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[test]
fn retained_native_curves_preserve_late_flush_tail_and_incomplete_event_identity() {
    let directory = directory();
    let expected = fixture(None);
    assert!(expected.native_end_analysis().is_none());
    let mut recording = fixture(Some(crate::live::HistoryConfig {
        directory: directory.clone(),
        disk_budget_bytes: 64 << 20,
        segment_frames: 2,
        min_free_disk_bytes: 0,
        ..Default::default()
    }));
    recording.analysis.retain_recent(4).unwrap();
    assert_eq!(recording.analysis.first_retained_frame(), 4);
    let cancel = AtomicBool::new(false);
    let frames: Vec<_> = (0..5).collect();
    assert_eq!(
        profile_curves::catalog(&recording, &cancel).unwrap(),
        profile_curves::catalog(&expected, &cancel).unwrap()
    );
    for metric in [Metric::FrameInterval, Metric::Simulate] {
        let actual = frame_metrics::sample(&recording, 7, 0, metric, &frames, &cancel).unwrap();
        let reference = frame_metrics::sample(&expected, 7, 0, metric, &frames, &cancel).unwrap();
        assert_eq!(
            actual
                .iter()
                .map(|point| (point.frame, point.milliseconds, point.status))
                .collect::<Vec<_>>(),
            reference
                .iter()
                .map(|point| (point.frame, point.milliseconds, point.status))
                .collect::<Vec<_>>()
        );
        if metric == Metric::Simulate {
            assert_eq!(
                (
                    actual[2].milliseconds,
                    actual[3].milliseconds,
                    actual[4].milliseconds
                ),
                (Some(10.0), Some(20.0), Some(20.0))
            );
        }
    }
    for event in 2..=6 {
        let key = ExactKey {
            zone: 10,
            event,
            thread: 42,
            context: 9,
        };
        let scene = ReferenceScene {
            id: 7,
            generation: 0,
        };
        let actual = profile_curves::sample(&recording, key, scene, &frames, &cancel).unwrap();
        assert_eq!(
            actual,
            profile_curves::sample(&expected, key, scene, &frames, &cancel).unwrap()
        );
        match event {
            2 => {
                assert_eq!(actual[2].occurrence_count, 2);
                assert_eq!(actual[4].milliseconds, Some(30.0));
            }
            3 => assert_eq!(actual[3].milliseconds, Some(30.0)),
            4 => assert_eq!(actual[4].status, profile_curves::Status::CrossBoundary),
            5 => assert_eq!(actual[4].status, profile_curves::Status::OpenSpan),
            6 => assert_eq!(actual[2].status, profile_curves::Status::UnmatchedStop),
            _ => unreachable!(),
        }
    }
    recording.analysis = recording.analysis_at(0).unwrap();
    assert_eq!(recording.analysis.profile_spans().len(), 0);
    assert_eq!(
        recording.analysis.memory_snapshot().summary().allocations,
        0
    );
    let memory_end = recording
        .native_end_analysis()
        .unwrap()
        .memory_snapshot()
        .summary();
    assert_eq!((memory_end.allocations, memory_end.live_bytes), (1, 64));
    assert_eq!(memory_end, expected.analysis.memory_snapshot().summary());
    assert_eq!(
        profile_curves::catalog(&recording, &cancel).unwrap(),
        profile_curves::catalog(&expected, &cancel).unwrap()
    );
    let key = ExactKey {
        zone: 10,
        event: 2,
        thread: 42,
        context: 9,
    };
    let scene = ReferenceScene {
        id: 7,
        generation: 0,
    };
    assert_eq!(
        profile_curves::sample(&recording, key, scene, &[2, 4], &cancel).unwrap(),
        profile_curves::sample(&expected, key, scene, &[2, 4], &cancel).unwrap()
    );
    let points =
        frame_metrics::sample(&recording, 7, 0, Metric::Simulate, &[2, 4], &cancel).unwrap();
    assert_eq!(
        points
            .iter()
            .map(|point| point.milliseconds)
            .collect::<Vec<_>>(),
        vec![Some(10.0), Some(20.0)]
    );
    let mut actual_csv = Vec::new();
    let mut expected_csv = Vec::new();
    crate::analysis_export::write_analysis_csv(
        &recording,
        &mut actual_csv,
        crate::analysis_export::AnalysisCsvKind::Profile,
        |_| true,
    )
    .unwrap();
    crate::analysis_export::write_analysis_csv(
        &expected,
        &mut expected_csv,
        crate::analysis_export::AnalysisCsvKind::Profile,
        |_| true,
    )
    .unwrap();
    assert_eq!(actual_csv, expected_csv);
    let rows = Spans::new(&recording, 2, &cancel)
        .unwrap()
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 3);
    assert!(rows[2].is_err());
    let mut rows = Spans::new(&recording, usize::MAX, &cancel).unwrap();
    assert!(rows.next().unwrap().is_ok());
    cancel.store(true, Ordering::Relaxed);
    assert_eq!(
        rows.next()
            .unwrap()
            .unwrap_err()
            .downcast_ref::<std::io::Error>()
            .unwrap()
            .kind(),
        std::io::ErrorKind::Interrupted
    );
    assert!(rows.next().is_none());
    drop(rows);
    drop(recording);
    assert!(directory.starts_with(std::env::temp_dir()));
    std::fs::remove_dir_all(directory).unwrap();
}
