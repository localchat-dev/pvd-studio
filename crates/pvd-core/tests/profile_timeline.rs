use pvd_core::analysis::view::ProfileSource;
use pvd_core::{
    Recording,
    analysis::ProfileSpan,
    profile_timeline::{self as timeline, TickRange, WindowLimits, WindowRequest},
};
use std::{
    io::{self, Cursor, Read, Seek, SeekFrom},
    sync::atomic::{AtomicBool, Ordering},
};

fn text(value: &str) -> Vec<u8> {
    let mut bytes = (value.len() as u32 + 1).to_le_bytes().to_vec();
    bytes.extend(value.as_bytes());
    bytes.push(0);
    bytes
}
fn string(id: u32, value: &str) -> Vec<u8> {
    let mut bytes = vec![1];
    bytes.extend(text(value));
    bytes.extend(id.to_le_bytes());
    bytes
}
fn instance(class: u32, id: u64) -> Vec<u8> {
    let mut bytes = vec![6];
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(class.to_le_bytes());
    bytes.extend(id.to_le_bytes());
    bytes
}
fn frame(tick: u64) -> Vec<u8> {
    let mut bytes = vec![19];
    bytes.extend(9u64.to_le_bytes());
    bytes.extend(3u32.to_le_bytes());
    bytes.extend(tick.to_le_bytes());
    bytes
}
fn profile(kind: u8, event: u16, thread: u32, context: u8, tick: u64) -> Vec<u8> {
    let mut bytes = vec![kind, 3];
    bytes.extend(event.to_le_bytes());
    bytes.extend(thread.to_le_bytes());
    bytes.extend([context, 0, 0]);
    bytes.extend(tick.to_le_bytes());
    bytes
}
fn payload(data: &[u8]) -> Vec<u8> {
    let mut bytes = vec![7];
    bytes.extend(10u64.to_le_bytes());
    for value in [5, 1, 6, data.len() as u32, data.len() as u32] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.extend(data);
    bytes
}
fn span(event: u16, thread: u32, context: u8, start: u64, end: u64) -> Vec<u8> {
    let mut bytes = profile(1, event, thread, context, start);
    bytes.extend(profile(2, event, thread, context, end));
    payload(&bytes)
}
fn definition(zone: u64, name: &str, event: u16, event_name: &str) -> Vec<Vec<u8>> {
    let mut first = vec![24];
    first.extend(zone.to_le_bytes());
    first.extend(text(name));
    let mut second = vec![25];
    second.extend(zone.to_le_bytes());
    second.extend(text(event_name));
    second.extend(event.to_le_bytes());
    second.push(1);
    vec![first, second]
}
fn group(events: &[Vec<u8>]) -> Vec<u8> {
    let payload: Vec<_> = events.iter().flatten().copied().collect();
    let mut bytes = (payload.len() as u32).to_le_bytes().to_vec();
    bytes.extend((events.len() as u32).to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    bytes.extend(payload);
    bytes
}
fn fixture(complete: bool) -> Vec<u8> {
    let mut bytes = pvd_protocol::STREAM_MAGIC.to_le_bytes().to_vec();
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(1_000_000u64.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    let mut events = vec![
        string(1, "physx3"),
        string(2, "PxScene"),
        string(3, "frame"),
        string(4, "PxProfileZone"),
        string(5, "events"),
        string(6, "PvdU8"),
        instance(2, 9),
        instance(4, 10),
    ];
    if complete {
        events.extend([
            payload(&profile(1, 1, 7, 9, 10)),
            payload(&profile(1, 1, 7, 9, 20)),
            payload(&profile(2, 1, 7, 9, 30)),
            frame(100),
            span(1, 8, 9, 40, 60),
            span(1, 7, 8, 40, 60),
            span(2, 7, 9, 50, 50),
            frame(200),
            span(1, 2, 9, 1, 2),
            frame(300),
            payload(&profile(2, 1, 7, 9, 90)),
        ]);
    } else {
        events.push(frame(100));
    }
    events.extend([
        payload(&profile(1, 1, 100, 9, 1000)),
        payload(&profile(2, 1, 99, 9, 9000)),
    ]);
    events.extend(definition(10, "late zone", 1, "same name"));
    events.extend(definition(10, "late zone", 2, "same name"));
    bytes.extend(group(&events));
    bytes
}
fn request(start: u64, end: u64) -> WindowRequest {
    WindowRequest {
        ticks: TickRange { start, end },
        thread: None,
        context: None,
        limits: WindowLimits::default(),
    }
}
fn fields(span: &ProfileSpan) -> (u64, u16, u32, u64, u64, u64, usize) {
    (
        span.zone,
        span.event,
        span.thread,
        span.context,
        span.start,
        span.end,
        span.received_frame,
    )
}

#[test]
fn legacy_gpu_payloads_are_reported_without_fabricating_cpu_spans_or_bounds() {
    let mut bytes = fixture(true);
    let mut gpu = vec![6, 0, 0, 0];
    gpu.extend([0; 12]);
    gpu.extend(3u32.to_le_bytes());
    gpu.extend([1, 2, 3]);
    gpu.extend([0; 8]);
    bytes.extend(group(&[payload(&gpu), payload(&gpu)]));
    let cancelled = AtomicBool::new(false);
    let catalog =
        timeline::catalog_page_streaming(&mut Cursor::new(&bytes), None, 5, &cancelled, |_| true)
            .unwrap();
    assert_eq!(catalog.legacy_cuda_buffers, 2);
    assert_eq!(catalog.global_pairing.complete_spans, 6);
    assert_eq!(
        catalog.completed_tick_range,
        Some(TickRange { start: 1, end: 90 })
    );
    let window = timeline::window_streaming(
        &mut Cursor::new(&bytes),
        &catalog.identity,
        request(50, 50),
        &cancelled,
        |_| true,
    )
    .unwrap();
    assert_eq!(window.legacy_cuda_buffers, 2);
    assert_eq!(window.global_pairing, catalog.global_pairing);
    assert!(window.spans.iter().all(|span| span.end <= 90));
}

#[test]
fn thread_pages_keep_global_complete_range_and_include_pending_only_threads() {
    for complete in [false, true] {
        let bytes = fixture(complete);
        let cancelled = AtomicBool::new(false);
        let mut cursor = None;
        let mut threads = Vec::new();
        loop {
            let page = timeline::catalog_page_streaming(
                &mut Cursor::new(&bytes),
                cursor.as_ref(),
                2,
                &cancelled,
                |_| true,
            )
            .unwrap();
            assert_eq!(page.clock.numerator, 1_000_000);
            assert_eq!(page.clock.denominator, 1);
            assert_eq!(page.identity.blake3, *blake3::hash(&bytes).as_bytes());
            assert_eq!(
                page.global_pairing.complete_spans,
                if complete { 6 } else { 0 }
            );
            assert_eq!(page.global_pairing.open_starts, 1);
            assert_eq!(page.global_pairing.unmatched_stops, 1);
            assert_eq!(
                page.completed_tick_range,
                complete.then_some(TickRange { start: 1, end: 90 })
            );
            assert!(page.threads.len() <= 2);
            assert!(page.estimated_index_bytes <= 1024);
            threads.extend(page.threads);
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(
            threads.iter().map(|entry| entry.thread).collect::<Vec<_>>(),
            if complete {
                vec![2, 7, 8, 99, 100]
            } else {
                vec![99, 100]
            }
        );
        for entry in threads.iter().filter(|entry| entry.thread >= 99) {
            assert_eq!(entry.completed_tick_range, None);
            assert_eq!(entry.complete_spans, 0);
        }
        if complete {
            for (thread, count, start, end) in [(2, 1, 1, 2), (7, 4, 10, 90), (8, 1, 40, 60)] {
                let entry = threads.iter().find(|entry| entry.thread == thread).unwrap();
                assert_eq!(entry.complete_spans, count);
                assert_eq!(entry.completed_tick_range, Some(TickRange { start, end }));
            }
        }
        assert_eq!(threads.last().unwrap().open_starts, 1);
        assert_eq!(threads[threads.len() - 2].unmatched_stops, 1);
    }
}

#[test]
fn closed_windows_keep_whole_nested_delayed_spans_and_exact_thread_contexts() {
    let bytes = fixture(true);
    let recording = Recording::from_bytes(&bytes).unwrap();
    let cancelled = AtomicBool::new(false);
    let catalog =
        timeline::catalog_page_streaming(&mut Cursor::new(&bytes), None, 2, &cancelled, |_| true)
            .unwrap();
    for (start, end) in [
        (0, 0),
        (50, 50),
        (21, 80),
        (90, 90),
        (91, 900),
        (0, u64::MAX),
    ] {
        for thread in [None, Some(7), Some(8), Some(987)] {
            for context in [None, Some(9), Some(8)] {
                let query = WindowRequest {
                    thread,
                    context,
                    ..request(start, end)
                };
                let window = timeline::window_streaming(
                    &mut Cursor::new(&bytes),
                    &catalog.identity,
                    query,
                    &cancelled,
                    |_| true,
                )
                .unwrap();
                let expected: Vec<_> = recording
                    .analysis
                    .profile_spans()
                    .filter(|span| {
                        span.end >= start
                            && span.start <= end
                            && thread.is_none_or(|id| id == span.thread)
                            && context.is_none_or(|id| id == span.context)
                    })
                    .map(fields)
                    .collect();
                assert_eq!(
                    window.spans.iter().map(fields).collect::<Vec<_>>(),
                    expected
                );
                assert_eq!(window.global_pairing, catalog.global_pairing);
                assert!(window.estimated_index_bytes <= query.limits.bytes);
                for names in window.names.values() {
                    assert_eq!(names.zone.as_deref(), Some("late zone"));
                    assert_eq!(names.event.as_deref(), Some("same name"));
                }
            }
        }
    }
    let window = timeline::window_streaming(
        &mut Cursor::new(&bytes),
        &catalog.identity,
        request(50, 50),
        &cancelled,
        |_| true,
    )
    .unwrap();
    assert_eq!(
        window.spans.last().map(fields),
        Some((10, 1, 7, 9, 10, 90, 3))
    );
    assert!(
        window
            .spans
            .iter()
            .any(|span| span.start == 50 && span.end == 50)
    );
}

#[test]
fn window_limits_and_relevant_metadata_redefinitions_fail_without_partial_success() {
    let cancelled = AtomicBool::new(false);
    let bytes = fixture(true);
    let identity =
        timeline::catalog_page_streaming(&mut Cursor::new(&bytes), None, 2, &cancelled, |_| true)
            .unwrap()
            .identity;
    for limits in [
        WindowLimits {
            spans: 1,
            bytes: 1 << 20,
        },
        WindowLimits {
            spans: 100,
            bytes: 64,
        },
    ] {
        let error = timeline::window_streaming(
            &mut Cursor::new(&bytes),
            &identity,
            WindowRequest {
                limits,
                ..request(0, 100)
            },
            &cancelled,
            |_| true,
        )
        .unwrap_err();
        assert!(error.to_string().contains("narrow"));
    }
    for (zone, event, zone_name, event_name, reject) in [
        (10, 1, "late zone", "same name", false),
        (10, 1, "late zone", "renamed", true),
        (10, 1, "renamed zone", "same name", true),
        (10, 3, "late zone", "unselected", false),
        (11, 1, "custom", "name", false),
    ] {
        let mut bytes = bytes.clone();
        bytes.extend(group(&definition(zone, zone_name, event, event_name)));
        bytes.extend(group(&definition(11, "custom changed", 1, "changed")));
        let identity =
            timeline::catalog_page_streaming(&mut Cursor::new(&bytes), None, 2, &cancelled, |_| {
                true
            })
            .unwrap()
            .identity;
        let result = timeline::window_streaming(
            &mut Cursor::new(&bytes),
            &identity,
            request(21, 80),
            &cancelled,
            |_| true,
        );
        assert_eq!(result.is_err(), reject);
        if let Err(error) = result {
            assert!(error.to_string().contains("redefined profile metadata"));
        }
    }
    for query in [
        request(1, 0),
        WindowRequest {
            limits: WindowLimits {
                spans: 0,
                bytes: 10,
            },
            ..request(0, 100)
        },
        WindowRequest {
            limits: WindowLimits {
                spans: 200001,
                bytes: 1 << 20,
            },
            ..request(0, 100)
        },
    ] {
        assert!(
            timeline::window_streaming(
                &mut Cursor::new(&bytes),
                &identity,
                query,
                &cancelled,
                |_| true
            )
            .is_err()
        );
    }
}

struct Fragmented(Cursor<Vec<u8>>);
impl Read for Fragmented {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let limit = out.len().min(7);
        self.0.read(&mut out[..limit])
    }
}
impl Seek for Fragmented {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.0.seek(position)
    }
}

#[test]
fn cursors_windows_cancellation_and_source_changes_are_bound_to_the_full_input() {
    let bytes = fixture(true);
    let cancelled = AtomicBool::new(false);
    let first = timeline::catalog_page_streaming(
        &mut Fragmented(Cursor::new(bytes.clone())),
        None,
        1,
        &cancelled,
        |_| true,
    )
    .unwrap();
    let expected = timeline::window_streaming(
        &mut Cursor::new(&bytes),
        &first.identity,
        request(0, 100),
        &cancelled,
        |_| true,
    )
    .unwrap();
    let fragmented = timeline::window_streaming(
        &mut Fragmented(Cursor::new(bytes.clone())),
        &first.identity,
        request(0, 100),
        &cancelled,
        |_| true,
    )
    .unwrap();
    assert_eq!(
        expected.spans.iter().map(fields).collect::<Vec<_>>(),
        fragmented.spans.iter().map(fields).collect::<Vec<_>>()
    );
    let mut changed = bytes.clone();
    let position = changed
        .windows(9)
        .position(|part| part == b"late zone")
        .unwrap();
    changed[position] = b'L';
    assert!(
        timeline::catalog_page_streaming(
            &mut Cursor::new(&changed),
            first.next.as_ref(),
            1,
            &cancelled,
            |_| true
        )
        .is_err()
    );
    assert!(
        timeline::window_streaming(
            &mut Cursor::new(&changed),
            &first.identity,
            request(0, 100),
            &cancelled,
            |_| true
        )
        .is_err()
    );
    assert!(
        timeline::window_streaming(
            &mut Cursor::new(&bytes[..bytes.len() - 1]),
            &first.identity,
            request(0, 100),
            &cancelled,
            |_| true
        )
        .is_err()
    );
    for limit in [0, 4097] {
        assert!(
            timeline::catalog_page_streaming(
                &mut Cursor::new(&bytes),
                None,
                limit,
                &cancelled,
                |_| true
            )
            .is_err()
        );
    }
    for window in [false, true] {
        let cancelled = AtomicBool::new(false);
        let mut cancel = |_| {
            cancelled.store(true, Ordering::Relaxed);
            true
        };
        let error = if window {
            timeline::window_streaming(
                &mut Cursor::new(&bytes),
                &first.identity,
                request(0, 100),
                &cancelled,
                &mut cancel,
            )
            .unwrap_err()
        } else {
            timeline::catalog_page_streaming(
                &mut Cursor::new(&bytes),
                None,
                1,
                &cancelled,
                &mut cancel,
            )
            .unwrap_err()
        };
        assert!(error.to_string().contains("cancelled"));
    }
}

#[test]
#[ignore = "requires official SDK 3.4 QPC captures and independent callback truth"]
fn sdk_timeline_windows_match_complete_spans_and_independent_qpc_bounds() {
    let root =
        std::path::PathBuf::from(std::env::var_os("PVD_TIMELINE_ROOT").expect("PVD_TIMELINE_ROOT"));
    let cancelled = AtomicBool::new(false);
    for name in ["physx-3.4-profile-metrics", "physx-3.4-profile-events"] {
        let path = root.join("captures").join(format!("{name}.pvdstream"));
        let bytes = std::fs::read(&path).unwrap();
        let recording = Recording::from_bytes(&bytes).unwrap();
        let truth: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path.with_extension("json")).unwrap()).unwrap();
        let catalog = timeline::catalog_page_streaming(
            &mut Cursor::new(&bytes),
            None,
            4096,
            &cancelled,
            |_| true,
        )
        .unwrap();
        assert!(catalog.next.is_none());
        assert_eq!(
            catalog.global_pairing.complete_spans,
            recording.analysis.profile_spans().len() as u64
        );
        assert_eq!(
            catalog.global_pairing.open_starts,
            recording.analysis.open_spans() as u64
        );
        let range = catalog.completed_tick_range.unwrap();
        let full = timeline::window_streaming(
            &mut Cursor::new(&bytes),
            &catalog.identity,
            request(range.start, range.end),
            &cancelled,
            |_| true,
        )
        .unwrap();
        assert_eq!(
            full.spans.iter().map(fields).collect::<Vec<_>>(),
            recording
                .analysis
                .profile_spans()
                .map(fields)
                .collect::<Vec<_>>()
        );
        let scale = full.clock.numerator as f64 / full.clock.denominator as f64 / 1e6;
        assert!(
            (scale - 1000.0 / truth["counter_frequency"].as_u64().unwrap() as f64).abs() < 1e-15
        );
        for span in full
            .spans
            .iter()
            .step_by((full.spans.len() / 8).max(1))
            .take(9)
        {
            let tick = span.start + (span.end - span.start) / 2;
            let query = WindowRequest {
                thread: Some(span.thread),
                context: Some(span.context),
                ..request(tick, tick)
            };
            let window = timeline::window_streaming(
                &mut Cursor::new(&bytes),
                &catalog.identity,
                query,
                &cancelled,
                |_| true,
            )
            .unwrap();
            let expected: Vec<_> = recording
                .analysis
                .profile_spans()
                .filter(|other| {
                    other.thread == span.thread
                        && other.context == span.context
                        && other.start <= tick
                        && other.end >= tick
                })
                .map(fields)
                .collect();
            assert_eq!(
                window.spans.iter().map(fields).collect::<Vec<_>>(),
                expected
            );
        }
        if name.ends_with("metrics") {
            let mut checked = 0;
            for expected in truth["spans"].as_array().unwrap() {
                let matching: Vec<_> = full
                    .spans
                    .iter()
                    .filter(|span| {
                        full.names[&(span.zone, span.event)].event.as_deref()
                            == expected["event"].as_str()
                            && span.context == expected["context"].as_u64().unwrap()
                            && (expected["start_lower"].as_u64().unwrap()
                                ..=expected["start_upper"].as_u64().unwrap())
                                .contains(&span.start)
                            && (expected["end_lower"].as_u64().unwrap()
                                ..=expected["end_upper"].as_u64().unwrap())
                                .contains(&span.end)
                    })
                    .collect();
                assert_eq!(matching.len(), 1);
                checked += 1;
            }
            assert_eq!(checked, 72);
        }
        println!(
            "sdk_timeline {name}: {} complete spans, {} threads, {} open starts",
            full.spans.len(),
            catalog.threads.len(),
            full.global_pairing.open_starts
        );
    }
}

#[test]
#[ignore = "requires preserved 600-second SDK capture and producer analysis CSV; no history storage"]
fn long_sdk_timeline_queries_early_middle_and_tail_contexts_without_history() {
    let root = std::path::PathBuf::from(
        std::env::var_os("PVD_TIMELINE_LONG_ROOT").expect("PVD_TIMELINE_LONG_ROOT"),
    );
    let path = root.join("raw/capture-1792077385976907200-8920-1-0.pvdstream");
    let csv = std::fs::read_to_string(root.join("sdk-analysis.csv")).unwrap();
    let steps: Vec<_> = csv
        .lines()
        .filter(|line| line.starts_with("step,"))
        .map(|line| line.split(',').map(str::to_owned).collect::<Vec<_>>())
        .collect();
    assert_eq!(steps.len(), 19293);
    assert_eq!(steps.last().unwrap()[6], "154344");
    let cancelled = AtomicBool::new(false);
    let catalog = timeline::catalog_page_streaming(
        &mut std::fs::File::open(&path).unwrap(),
        None,
        1,
        &cancelled,
        |_| true,
    )
    .unwrap();
    assert_eq!(catalog.identity.frames, 19293);
    assert_eq!(catalog.identity.bytes, 251301039);
    assert_eq!(
        blake3::Hash::from(catalog.identity.blake3)
            .to_hex()
            .as_str(),
        "9a99d0525325e7ad2085bb2db7e175eb53e4d0be331d68ff1373f41bb9a181f1"
    );
    assert!(catalog.estimated_index_bytes < 1024);
    if let Some(cursor) = &catalog.next {
        let second = timeline::catalog_page_streaming(
            &mut std::fs::File::open(&path).unwrap(),
            Some(cursor),
            1,
            &cancelled,
            |_| true,
        )
        .unwrap();
        assert_eq!(catalog.identity, second.identity);
        assert_eq!(catalog.global_pairing, second.global_pairing);
        assert!(catalog.threads[0].thread < second.threads[0].thread);
    }
    let mut results = Vec::new();
    for frame in [1usize, 9646, 19292] {
        let pairs: u64 = steps[frame][6].parse().unwrap();
        let before: u64 = steps[frame - 1][6].parse().unwrap();
        assert_eq!(pairs - before, 8);
        let context = frame as u64 * 64 + 3;
        let query = WindowRequest {
            context: Some(context),
            ..request(0, u64::MAX)
        };
        let window = timeline::window_streaming(
            &mut std::fs::File::open(&path).unwrap(),
            &catalog.identity,
            query,
            &cancelled,
            |_| true,
        )
        .unwrap();
        assert_eq!(window.global_pairing, catalog.global_pairing);
        assert_eq!(window.spans.len(), 1);
        let span = &window.spans[0];
        assert_eq!(span.context, context);
        assert_eq!(span.received_frame, frame);
        assert_eq!(
            window.names[&(span.zone, span.event)].event.as_deref(),
            Some("PvdStudio.AnalysisBatch")
        );
        let tick = span.start + (span.end - span.start) / 2;
        let point = timeline::window_streaming(
            &mut std::fs::File::open(&path).unwrap(),
            &catalog.identity,
            WindowRequest {
                ticks: TickRange {
                    start: tick,
                    end: tick,
                },
                thread: Some(span.thread),
                ..query
            },
            &cancelled,
            |_| true,
        )
        .unwrap();
        assert_eq!(
            point.spans.iter().map(fields).collect::<Vec<_>>(),
            vec![fields(span)]
        );
        results.push(serde_json::json!({"frame":frame,"context":context,"thread":span.thread,"start":span.start,"end":span.end,"received_frame":span.received_frame,"window_index_bytes":window.estimated_index_bytes}));
    }
    let limited = WindowRequest {
        limits: WindowLimits {
            spans: 100,
            bytes: 1 << 20,
        },
        ..request(0, u64::MAX)
    };
    assert!(
        timeline::window_streaming(
            &mut std::fs::File::open(&path).unwrap(),
            &catalog.identity,
            limited,
            &cancelled,
            |_| true
        )
        .unwrap_err()
        .to_string()
        .contains("narrow")
    );
    let mut at = 0;
    assert!(
        timeline::window_streaming(
            &mut std::fs::File::open(&path).unwrap(),
            &catalog.identity,
            WindowRequest {
                context: Some(u64::MAX),
                ..request(0, u64::MAX)
            },
            &cancelled,
            |progress| {
                at = progress.bytes_read;
                at < 4 << 20
            }
        )
        .is_err()
    );
    assert!(at >= 4 << 20 && at < catalog.identity.bytes);
    println!(
        "long_timeline {}",
        serde_json::json!({"bytes":catalog.identity.bytes,"frames":catalog.identity.frames,"global_complete":catalog.global_pairing.complete_spans,"global_open":catalog.global_pairing.open_starts,"global_unmatched":catalog.global_pairing.unmatched_stops,"completed_tick_start":catalog.completed_tick_range.unwrap().start,"completed_tick_end":catalog.completed_tick_range.unwrap().end,"samples":results,"cancelled_after_bytes":at,"history_created":false})
    );
}
