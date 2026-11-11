use crate::{
    Recording, RecordingBuilder, RecordingSource,
    analysis::view::ProfileSource,
    native::{self, ProfileCatalogEntry},
    profile_timeline::{
        GlobalPairingStats, MAX_THREAD_PAGE, MAX_WINDOW_BYTES, MAX_WINDOW_SPANS, TickRange,
        WindowLimits, WindowRequest, native::*,
    },
};
use pvd_protocol::{Endian, Event, Header, Name};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

struct Fixture {
    recording: Option<Recording>,
    directory: PathBuf,
}

impl Fixture {
    fn new(frames: bool) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "pvd-native-timeline-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut builder = RecordingBuilder::new(Header {
            version: 1,
            timestamp_numerator: 1_000_000,
            timestamp_denominator: 1,
            flags: 0,
            endian: Endian::Little,
        });
        builder.recording.source = RecordingSource::Pvd3Database;
        let history = crate::live::HistoryWriter::new_paged(crate::live::HistoryConfig {
            directory: directory.clone(),
            disk_budget_bytes: 64 << 20,
            segment_frames: 1,
            min_free_disk_bytes: 0,
            ..Default::default()
        })
        .unwrap();
        builder.recording.analysis = history.initial_analysis().unwrap();
        builder.streaming = Some(Box::new(crate::live::Streaming::new(history)));
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
            Event::CreateInstance { class, id: 9 },
        ] {
            native::push(&mut builder, event).unwrap();
        }
        native::register_profile_catalog(
            &mut builder,
            vec![ProfileCatalogEntry {
                local_id: 10,
                name: "timeline zone".into(),
                events: (1..=5)
                    .map(|event| (event, format!("event {event}"), true))
                    .collect(),
            }],
        )
        .unwrap();
        buffer(&mut builder, &[(1, 1, 8, 10), (2, 1, 8, 20)]);
        if frames {
            frame(&mut builder, 100);
            frame(&mut builder, 200);
        }
        buffer(
            &mut builder,
            &[
                (1, 1, 7, 40),
                (1, 1, 7, 45),
                (2, 1, 7, 50),
                (2, 1, 7, 60),
                (1, 2, 7, 15),
                (2, 2, 7, 25),
                (1, 3, 100, 1000),
                (2, 4, 99, 9000),
            ],
        );
        if frames {
            frame(&mut builder, 300);
        }
        buffer(
            &mut builder,
            &[(1, 1, 8, 10), (2, 1, 8, 90), (1, 5, 7, 50), (2, 5, 7, 50)],
        );
        Self {
            recording: Some(builder.finish().unwrap()),
            directory,
        }
    }

    fn recording(&self) -> &Recording {
        self.recording.as_ref().unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.recording = None;
        assert!(self.directory.starts_with(std::env::temp_dir()));
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}

fn buffer(builder: &mut RecordingBuilder, events: &[(u8, u16, u32, u64)]) {
    let mut bytes = Vec::new();
    for &(kind, event, thread, tick) in events {
        bytes.extend([kind, 3]);
        bytes.extend(event.to_le_bytes());
        bytes.extend(thread.to_le_bytes());
        bytes.extend([9, 0, 0]);
        bytes.extend(tick.to_le_bytes());
    }
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

fn frame(builder: &mut RecordingBuilder, timestamp: u64) {
    native::push(
        builder,
        Event::EndSection {
            id: 9,
            name: 3,
            timestamp,
        },
    )
    .unwrap();
}

fn request(start: u64, end: u64) -> WindowRequest {
    WindowRequest {
        ticks: TickRange { start, end },
        thread: None,
        context: None,
        limits: WindowLimits::default(),
    }
}

#[test]
fn native_thread_pages_preserve_global_bounds_pairing_and_seek_identity() {
    let mut fixture = Fixture::new(true);
    let recording = fixture.recording.as_mut().unwrap();
    let cancel = AtomicBool::new(false);
    let original = identity(recording).unwrap();
    recording.analysis = recording.analysis_at(0).unwrap();
    assert_eq!(recording.analysis.profile_spans().len(), 1);
    assert_eq!(identity(recording).unwrap(), original);
    assert_eq!(identity(&recording.clone()).unwrap(), original);
    let mut cursor = None;
    let mut threads = Vec::new();
    loop {
        let page = catalog_page(recording, cursor.as_ref(), 1, &cancel, |_| true).unwrap();
        assert_eq!(page.identity, original);
        assert_eq!(
            page.completed_tick_range,
            Some(TickRange { start: 10, end: 90 })
        );
        assert_eq!(
            page.global_pairing,
            GlobalPairingStats {
                complete_spans: 6,
                open_starts: 1,
                unmatched_stops: 1,
            }
        );
        assert_eq!(page.threads.len(), 1);
        assert!(page.estimated_index_bytes <= 1024);
        threads.extend(page.threads);
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(
        threads.iter().map(|entry| entry.thread).collect::<Vec<_>>(),
        [7, 8, 99, 100]
    );
    assert_eq!(threads[0].complete_spans, 4);
    assert_eq!(threads[1].complete_spans, 2);
    assert_eq!(threads[2].unmatched_stops, 1);
    assert_eq!(threads[3].open_starts, 1);
    assert!(
        threads[2..]
            .iter()
            .all(|entry| entry.completed_tick_range.is_none())
    );
}

#[test]
fn native_window_keeps_late_nested_tail_and_closed_boundary_spans() {
    let mut fixture = Fixture::new(true);
    let recording = fixture.recording.as_mut().unwrap();
    recording.analysis.retain_recent(3).unwrap();
    let cancel = AtomicBool::new(false);
    let source = identity(recording).unwrap();
    let expected = [
        (1, 8, 10, 20, 0),
        (1, 7, 45, 50, 2),
        (1, 7, 40, 60, 2),
        (2, 7, 15, 25, 2),
        (1, 8, 10, 90, 3),
        (5, 7, 50, 50, 3),
    ];
    for (start, end) in [
        (0, 0),
        (20, 20),
        (50, 50),
        (21, 80),
        (90, 90),
        (91, 900),
        (0, u64::MAX),
    ] {
        for thread in [None, Some(7), Some(8), Some(999)] {
            for context in [None, Some(9), Some(8)] {
                let query = WindowRequest {
                    thread,
                    context,
                    ..request(start, end)
                };
                let result = window(recording, &source, query, &cancel, |_| true).unwrap();
                let rows = result
                    .spans
                    .iter()
                    .map(|span| {
                        (
                            span.event,
                            span.thread,
                            span.start,
                            span.end,
                            span.received_frame,
                        )
                    })
                    .collect::<Vec<_>>();
                let filtered = expected
                    .iter()
                    .copied()
                    .filter(|&(_, t, s, e, _)| {
                        e >= start
                            && s <= end
                            && thread.is_none_or(|thread| thread == t)
                            && context.is_none_or(|context| context == 9)
                    })
                    .collect::<Vec<_>>();
                assert_eq!(rows, filtered);
                assert_eq!(result.global_pairing.complete_spans, 6);
                assert_eq!(result.global_pairing.open_starts, 1);
                assert_eq!(result.global_pairing.unmatched_stops, 1);
                assert!(result.estimated_index_bytes <= MAX_WINDOW_BYTES);
                for (&(zone, event), names) in &result.names {
                    assert_eq!(zone, 10);
                    assert_eq!(names.zone.as_deref(), Some("timeline zone"));
                    assert_eq!(names.event.as_ref().unwrap(), &format!("event {event}"));
                }
            }
        }
    }
}

#[test]
fn native_queries_reject_other_history_invalid_requests_and_result_overflow() {
    let left = Fixture::new(true);
    let right = Fixture::new(true);
    let cancel = AtomicBool::new(false);
    let page = catalog_page(left.recording(), None, 1, &cancel, |_| true).unwrap();
    assert!(catalog_page(right.recording(), page.next.as_ref(), 1, &cancel, |_| true).is_err());
    assert!(
        window(
            right.recording(),
            &page.identity,
            request(0, 100),
            &cancel,
            |_| true
        )
        .is_err()
    );
    for limit in [0, MAX_THREAD_PAGE + 1] {
        assert!(catalog_page(left.recording(), None, limit, &cancel, |_| true).is_err());
    }
    for query in [
        request(100, 0),
        WindowRequest {
            limits: WindowLimits {
                spans: 0,
                bytes: MAX_WINDOW_BYTES,
            },
            ..request(0, 100)
        },
        WindowRequest {
            limits: WindowLimits {
                spans: 1,
                bytes: MAX_WINDOW_BYTES,
            },
            ..request(0, 100)
        },
        WindowRequest {
            limits: WindowLimits {
                spans: MAX_WINDOW_SPANS,
                bytes: 1,
            },
            ..request(0, 100)
        },
        WindowRequest {
            limits: WindowLimits {
                spans: MAX_WINDOW_SPANS + 1,
                bytes: MAX_WINDOW_BYTES,
            },
            ..request(0, 100)
        },
    ] {
        assert!(window(left.recording(), &page.identity, query, &cancel, |_| true).is_err());
    }
    let mut changed = left.recording().clone();
    changed.header.timestamp_numerator += 1;
    assert!(window(&changed, &page.identity, request(0, 100), &cancel, |_| true).is_err());
    changed.source = RecordingSource::SdkStream;
    assert!(identity(&changed).is_err());
    let mut changed = left.recording().clone();
    changed
        .native_analysis_end
        .as_mut()
        .unwrap()
        .add_event(10, 1, "different meaning".into())
        .unwrap();
    let changed_identity = identity(&changed).unwrap();
    assert_ne!(changed_identity, page.identity);
    assert_eq!(identity(left.recording()).unwrap(), page.identity);
    assert!(window(&changed, &page.identity, request(0, 100), &cancel, |_| true).is_err());
    let failure = window(
        &changed,
        &changed_identity,
        request(0, 100),
        &cancel,
        |_| true,
    )
    .unwrap_err();
    assert!(failure.to_string().contains("redefined profile metadata"));
}

#[test]
fn native_query_cancellation_is_observed_before_during_and_after_roots() {
    let fixture = Fixture::new(true);
    let recording = fixture.recording();
    let source = identity(recording).unwrap();
    let cancel = AtomicBool::new(true);
    assert!(
        catalog_page(recording, None, 1, &cancel, |_| panic!(
            "cancelled before progress"
        ))
        .is_err()
    );
    assert!(
        window(recording, &source, request(0, 100), &cancel, |_| panic!(
            "cancelled before progress"
        ))
        .is_err()
    );
    cancel.store(false, Ordering::Relaxed);
    for cutoff in [0, 1, 3, 6] {
        let mut calls = 0;
        let result = window(recording, &source, request(0, 100), &cancel, |state| {
            assert!(state.frames_scanned <= state.total_frames);
            calls += 1;
            state.spans_scanned < cutoff
        });
        assert!(result.is_err());
        assert!(calls > 0);
    }
    let mut updates = Vec::new();
    catalog_page(recording, None, 3, &cancel, |state| {
        updates.push(state);
        true
    })
    .unwrap();
    assert_eq!(updates.last().unwrap().frames_scanned, 3);
    assert_eq!(updates.last().unwrap().spans_scanned, 6);
    assert!(
        updates
            .windows(2)
            .all(|pair| pair[0].spans_scanned <= pair[1].spans_scanned)
    );
}

#[test]
fn native_zero_scene_frames_keep_analysis_tail() {
    let fixture = Fixture::new(false);
    let recording = fixture.recording();
    assert_eq!(recording.frame_count(), 0);
    let cancel = AtomicBool::new(false);
    let page = catalog_page(recording, None, 4, &cancel, |_| true).unwrap();
    assert_eq!(page.global_pairing.complete_spans, 6);
    assert_eq!(page.threads.len(), 4);
    let result = window(recording, &page.identity, request(0, 100), &cancel, |_| {
        true
    })
    .unwrap();
    assert_eq!(result.spans.len(), 6);
    assert!(result.spans.iter().all(|span| span.received_frame == 0));
}
