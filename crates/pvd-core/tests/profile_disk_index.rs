use pvd_core::{
    analysis::ProfileSpan,
    profile_disk_index::{self as disk, IndexLimits, Phase},
    profile_timeline::{self as timeline, TickRange, WindowLimits, WindowRequest},
};
use std::{
    io::Cursor,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
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

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
struct Case {
    root: std::path::PathBuf,
    source: std::path::PathBuf,
    work: std::path::PathBuf,
}
impl Case {
    fn new(bytes: &[u8]) -> Self {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../output")
            .join(format!(
                "disk-index-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.pvdstream");
        std::fs::write(&source, bytes).unwrap();
        let work = root.join("index");
        Self { root, source, work }
    }
    fn identity(&self) -> pvd_core::profile_curves::SourceIdentity {
        timeline::catalog_page_streaming(
            &mut std::fs::File::open(&self.source).unwrap(),
            None,
            4096,
            &AtomicBool::new(false),
            |_| true,
        )
        .unwrap()
        .identity
    }
}
impl Drop for Case {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.source);
        let _ = std::fs::remove_dir(&self.root);
    }
}
fn limits() -> IndexLimits {
    IndexLimits {
        memory_bytes: 1 << 20,
        disk_bytes: 4 << 20,
        run_bytes: 128,
        page_records: 1,
        merge_fan_in: 2,
    }
}

#[test]
fn per_append_space_policy_failure_is_not_cancellation_and_cleans_the_workspace() {
    let bytes = fixture(true);
    for fail_at in [Some(0), Some(3), None] {
        let case = Case::new(&bytes);
        let writes = std::sync::Arc::new(AtomicU64::new(0));
        let counter = writes.clone();
        let result = disk::build_index(
            &case.source,
            &case.work,
            &case.identity(),
            disk::IndexOptions {
                limits: limits(),
                before_write: Some(std::sync::Arc::new(move |bytes| {
                    let count = counter.fetch_add(1, Ordering::Relaxed);
                    if fail_at.map_or(bytes == 0, |fail_at| count == fail_at) {
                        Err(std::io::Error::other("profile disk free-space reserve"))
                    } else {
                        Ok(())
                    }
                })),
            },
            &AtomicBool::new(false),
            |_| true,
        );
        let error = result.err().expect("space policy must fail");
        assert!(error.to_string().contains("free-space reserve"));
        if let Some(fail_at) = fail_at {
            assert_eq!(writes.load(Ordering::Relaxed), fail_at + 1);
        }
        assert!(!case.work.exists());
        assert_eq!(std::fs::read(&case.source).unwrap(), bytes);
    }
}
fn assert_window(index: &disk::CpuSpanIndex, case: &Case, query: WindowRequest) -> (f64, f64) {
    let cancel = AtomicBool::new(false);
    let started = std::time::Instant::now();
    let expected = timeline::window_streaming(
        &mut std::fs::File::open(&case.source).unwrap(),
        &index.info().identity,
        query,
        &cancel,
        |_| true,
    )
    .unwrap();
    let streaming_ms = started.elapsed().as_secs_f64() * 1000.0;
    let mut verified = 0;
    let started = std::time::Instant::now();
    let actual = index
        .window(query, &cancel, |p| {
            assert!(!matches!(p.phase, Phase::Decode | Phase::Merge));
            if p.phase == Phase::VerifySource {
                verified = verified.max(p.completed);
            }
            true
        })
        .unwrap();
    let index_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(verified, index.info().identity.bytes);
    assert_eq!(
        actual.spans.iter().map(fields).collect::<Vec<_>>(),
        expected.spans.iter().map(fields).collect::<Vec<_>>()
    );
    assert_eq!(actual.names, expected.names);
    assert_eq!(actual.clock, expected.clock);
    assert_eq!(actual.identity, expected.identity);
    assert_eq!(actual.global_pairing, expected.global_pairing);
    assert_eq!(actual.legacy_cuda_buffers, expected.legacy_cuda_buffers);
    (streaming_ms, index_ms)
}

#[test]
fn external_merge_pages_preserve_arrival_order_and_long_closed_overlap() {
    let bytes = fixture(true);
    let case = Case::new(&bytes);
    let identity = case.identity();
    let cancel = AtomicBool::new(false);
    let index = disk::build_index(
        &case.source,
        &case.work,
        &identity,
        limits(),
        &cancel,
        |_| true,
    )
    .unwrap();
    assert_eq!(index.info().initial_runs, 3);
    assert_eq!(index.info().merge_passes, 2);
    assert_eq!(index.info().global_pairing.complete_spans, 6);
    assert_eq!(
        index.info().completed_tick_range,
        Some(TickRange { start: 1, end: 90 })
    );
    assert!(index.info().peak_disk_bytes > index.info().disk_bytes);
    assert!(index.info().peak_disk_bytes <= limits().disk_bytes);
    for (start, end) in [
        (0, 0),
        (50, 50),
        (21, 80),
        (90, 90),
        (91, 900),
        (0, u64::MAX),
    ] {
        for thread in [None, Some(7), Some(8), Some(100)] {
            for context in [None, Some(8), Some(9)] {
                assert_window(
                    &index,
                    &case,
                    WindowRequest {
                        thread,
                        context,
                        ..request(start, end)
                    },
                );
            }
        }
    }
    assert_window(&index, &case, request(50, 50));
    drop(index);
    assert!(!case.work.exists());
    assert_eq!(std::fs::read(&case.source).unwrap(), bytes);
}

#[test]
fn empty_pending_metadata_and_gpu_semantics_match_streaming() {
    for complete in [false, true] {
        let mut bytes = fixture(complete);
        if complete {
            bytes.extend(group(&[span(3, 7, 9, 40, 60)]));
        }
        let mut gpu = vec![6, 0, 0, 0];
        gpu.extend([0; 12]);
        gpu.extend(0u32.to_le_bytes());
        gpu.extend([0; 8]);
        bytes.extend(group(&[payload(&gpu)]));
        let case = Case::new(&bytes);
        let cancel = AtomicBool::new(false);
        let index = disk::build_index(
            &case.source,
            &case.work,
            &case.identity(),
            limits(),
            &cancel,
            |_| true,
        )
        .unwrap();
        assert_eq!(index.info().legacy_cuda_buffers, 1);
        assert_eq!(
            index.info().completed_tick_range,
            complete.then_some(TickRange { start: 1, end: 90 })
        );
        assert_eq!(index.info().global_pairing.open_starts, 1);
        assert_eq!(index.info().global_pairing.unmatched_stops, 1);
        assert_window(&index, &case, request(0, u64::MAX));
        drop(index);
    }
    for (zone, event, zone_name, event_name, reject) in [
        (10, 1, "late zone", "same name", false),
        (10, 1, "late zone", "changed", true),
        (10, 1, "changed zone", "same name", true),
        (11, 1, "custom", "name", false),
    ] {
        let mut bytes = fixture(true);
        bytes.extend(group(&definition(zone, zone_name, event, event_name)));
        bytes.extend(group(&definition(
            11,
            "other",
            "other".len() as u16,
            "changed",
        )));
        let case = Case::new(&bytes);
        let cancel = AtomicBool::new(false);
        let index = disk::build_index(
            &case.source,
            &case.work,
            &case.identity(),
            limits(),
            &cancel,
            |_| true,
        )
        .unwrap();
        let result = index.window(request(50, 50), &cancel, |_| true);
        assert_eq!(result.is_err(), reject);
        if let Err(error) = result {
            assert!(error.to_string().contains("redefined profile metadata"));
        } else {
            assert_window(&index, &case, request(50, 50));
        }
        drop(index);
    }
}

#[test]
fn budgets_cancellation_source_changes_and_workspace_ownership_are_strict() {
    let bytes = fixture(true);
    let case = Case::new(&bytes);
    let identity = case.identity();
    let cancel = AtomicBool::new(false);
    std::fs::create_dir(&case.work).unwrap();
    assert!(
        disk::build_index(
            &case.source,
            &case.work,
            &identity,
            limits(),
            &cancel,
            |_| true
        )
        .is_err()
    );
    assert!(case.work.exists());
    std::fs::remove_dir(&case.work).unwrap();
    assert!(
        disk::build_index(
            &case.source,
            &case.source,
            &identity,
            limits(),
            &cancel,
            |_| true
        )
        .is_err()
    );
    for config in [
        IndexLimits {
            memory_bytes: 100,
            ..limits()
        },
        IndexLimits {
            disk_bytes: 250,
            ..limits()
        },
    ] {
        assert!(
            disk::build_index(&case.source, &case.work, &identity, config, &cancel, |_| {
                true
            })
            .is_err()
        );
        assert!(!case.work.exists());
    }
    for phase in [Phase::Decode, Phase::Metadata, Phase::Merge] {
        assert!(
            disk::build_index(
                &case.source,
                &case.work,
                &identity,
                limits(),
                &cancel,
                |p| p.phase != phase
            )
            .is_err()
        );
        assert!(!case.work.exists());
    }
    assert!(
        disk::build_index(
            &case.source,
            &case.work,
            &identity,
            limits(),
            &cancel,
            |p| {
                if p.phase == Phase::Decode && p.completed > 0 {
                    cancel.store(true, Ordering::Relaxed);
                }
                true
            }
        )
        .is_err()
    );
    assert!(!case.work.exists());
    cancel.store(false, Ordering::Relaxed);
    let mut wrong = identity.clone();
    wrong.blake3[0] ^= 1;
    assert!(
        disk::build_index(&case.source, &case.work, &wrong, limits(), &cancel, |_| {
            true
        })
        .is_err()
    );
    assert!(!case.work.exists());
    let index = disk::build_index(
        &case.source,
        &case.work,
        &identity,
        limits(),
        &cancel,
        |_| true,
    )
    .unwrap();
    for phase in [Phase::VerifySource, Phase::Query, Phase::Metadata] {
        assert!(
            index
                .window(request(0, u64::MAX), &cancel, |p| p.phase != phase)
                .is_err()
        );
        assert!(case.work.exists());
    }
    assert!(
        index
            .window(request(0, u64::MAX), &cancel, |p| {
                if p.phase == Phase::Query && p.completed == p.total {
                    cancel.store(true, Ordering::Relaxed);
                }
                true
            })
            .is_err()
    );
    cancel.store(false, Ordering::Relaxed);
    for constraints in [
        WindowLimits {
            spans: 1,
            bytes: 1 << 20,
        },
        WindowLimits {
            spans: 100,
            bytes: 64,
        },
    ] {
        assert!(
            index
                .window(
                    WindowRequest {
                        limits: constraints,
                        ..request(0, u64::MAX)
                    },
                    &cancel,
                    |_| true
                )
                .unwrap_err()
                .to_string()
                .contains("narrow")
        );
    }
    let mut changed = bytes.clone();
    let pos = changed.windows(9).position(|p| p == b"late zone").unwrap();
    changed[pos] = b'L';
    std::fs::write(&case.source, &changed).unwrap();
    assert!(
        index
            .window(request(0, u64::MAX), &cancel, |_| true)
            .unwrap_err()
            .to_string()
            .contains("identity changed")
    );
    std::fs::write(&case.source, &bytes).unwrap();
    assert_window(&index, &case, request(0, u64::MAX));
    let minimum = index.info().disk_bytes;
    drop(index);
    assert!(
        disk::build_index(
            &case.source,
            &case.work,
            &identity,
            IndexLimits {
                disk_bytes: minimum,
                ..limits()
            },
            &cancel,
            |_| true
        )
        .is_err()
    );
    assert!(!case.work.exists());
    let index = disk::build_index(
        &case.source,
        &case.work,
        &identity,
        limits(),
        &cancel,
        |_| true,
    )
    .unwrap();
    let foreign = case.work.join("unrelated.txt");
    std::fs::write(&foreign, b"keep").unwrap();
    drop(index);
    assert_eq!(std::fs::read(&foreign).unwrap(), b"keep");
    assert_eq!(std::fs::read_dir(&case.work).unwrap().count(), 1);
    std::fs::remove_file(foreign).unwrap();
    std::fs::remove_dir(&case.work).unwrap();
}

#[test]
fn corrupted_headers_payloads_metadata_and_truncation_never_produce_results() {
    let case = Case::new(&fixture(true));
    let cancel = AtomicBool::new(false);
    let index = disk::build_index(
        &case.source,
        &case.work,
        &case.identity(),
        limits(),
        &cancel,
        |_| true,
    )
    .unwrap();
    for entry in std::fs::read_dir(&case.work).unwrap() {
        let path = entry.unwrap().path();
        let original = std::fs::read(&path).unwrap();
        for at in [0, original.len() / 2, original.len() - 1] {
            let mut changed = original.clone();
            changed[at] ^= 0xff;
            std::fs::write(&path, changed).unwrap();
            assert!(
                index
                    .window(request(0, u64::MAX), &cancel, |_| true)
                    .is_err()
            );
            std::fs::write(&path, &original).unwrap();
        }
        std::fs::write(&path, &original[..original.len() - 1]).unwrap();
        assert!(
            index
                .window(request(0, u64::MAX), &cancel, |_| true)
                .is_err()
        );
        std::fs::write(&path, &original).unwrap();
    }
    assert_window(&index, &case, request(0, u64::MAX));
    drop(index);
}

#[test]
#[ignore = "requires two preserved official SDK QPC captures; functional build and repeated-query comparison"]
fn sdk_qpc_indexes_match_streaming_without_redecoding_queries() {
    let root =
        std::path::PathBuf::from(std::env::var_os("PVD_TIMELINE_ROOT").expect("PVD_TIMELINE_ROOT"));
    for name in ["physx-3.4-profile-metrics", "physx-3.4-profile-events"] {
        let bytes = std::fs::read(root.join("captures").join(format!("{name}.pvdstream"))).unwrap();
        let case = Case::new(&bytes);
        let identity = case.identity();
        let cancel = AtomicBool::new(false);
        let config = IndexLimits {
            run_bytes: 64 << 10,
            page_records: 32,
            ..limits()
        };
        let start = std::time::Instant::now();
        let index = disk::build_index(&case.source, &case.work, &identity, config, &cancel, |_| {
            true
        })
        .unwrap();
        let build_ms = start.elapsed().as_secs_f64() * 1000.0;
        let expected = timeline::window_streaming(
            &mut Cursor::new(&bytes),
            &identity,
            request(0, u64::MAX),
            &cancel,
            |_| true,
        )
        .unwrap();
        let first_query = assert_window(&index, &case, request(0, u64::MAX));
        let mut cancelled_at = 0;
        assert!(
            index
                .window(request(0, u64::MAX), &cancel, |p| {
                    if p.phase == Phase::VerifySource {
                        cancelled_at = p.completed;
                        return p.completed < 65536;
                    }
                    true
                })
                .is_err()
        );
        assert!(cancelled_at >= 65536 && cancelled_at < identity.bytes);
        let all = index
            .window(request(0, u64::MAX), &cancel, |_| true)
            .unwrap();
        let truth: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.join("captures").join(format!("{name}.json"))).unwrap(),
        )
        .unwrap();
        let scale = all.clock.numerator as f64 / all.clock.denominator as f64 / 1e6;
        assert!(
            (scale - 1000.0 / truth["counter_frequency"].as_u64().unwrap() as f64).abs() < 1e-15
        );
        if name.ends_with("metrics") {
            let mut checked = 0;
            for expected in truth["spans"].as_array().unwrap() {
                let count = all
                    .spans
                    .iter()
                    .filter(|span| {
                        all.names[&(span.zone, span.event)].event.as_deref()
                            == expected["event"].as_str()
                            && span.context == expected["context"].as_u64().unwrap()
                            && (expected["start_lower"].as_u64().unwrap()
                                ..=expected["start_upper"].as_u64().unwrap())
                                .contains(&span.start)
                            && (expected["end_lower"].as_u64().unwrap()
                                ..=expected["end_upper"].as_u64().unwrap())
                                .contains(&span.end)
                    })
                    .count();
                assert_eq!(count, 1);
                checked += 1;
            }
            assert_eq!(checked, 72);
        }
        let mut query_ms = Vec::new();
        for span in expected
            .spans
            .iter()
            .step_by((expected.spans.len() / 6).max(1))
            .take(7)
        {
            let tick = span.start + (span.end - span.start) / 2;
            let query = WindowRequest {
                thread: Some(span.thread),
                context: Some(span.context),
                ..request(tick, tick)
            };
            for iteration in 0..2 {
                let (streaming_ms, index_ms) = assert_window(&index, &case, query);
                query_ms.push(serde_json::json!({"iteration":iteration,"tick":tick,"streaming_ms":streaming_ms,"index_ms_including_source_hash":index_ms}));
            }
        }
        println!(
            "disk_qpc {}",
            serde_json::json!({"input":name,"bytes":bytes.len(),"spans":expected.spans.len(),"runs":index.info().initial_runs,"passes":index.info().merge_passes,"disk_bytes":index.info().disk_bytes,"peak_disk_bytes":index.info().peak_disk_bytes,"build_ms":build_ms,"first_full_query_streaming_index_ms":first_query,"queries":query_ms,"scope":"functional first/repeated queries; OS cache not controlled; no isolated performance claim"})
        );
        drop(index);
        assert!(!case.work.exists());
    }
}
