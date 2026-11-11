use pvd_core::{
    analysis::{ProfileSpan, view::ProfileSource},
    native,
    profile_timeline::{self as timeline, TickRange, WindowLimits, WindowRequest},
};
use std::{collections::BTreeMap, io::Cursor, path::PathBuf, sync::atomic::AtomicBool};

fn span_fields(span: &ProfileSpan, zone: u64) -> (u64, u16, u32, u64, u64, u64, usize) {
    (
        zone,
        span.event,
        span.thread,
        span.context,
        span.start,
        span.end,
        span.received_frame,
    )
}

#[test]
#[ignore = "requires original PVD3 rich container and exact same SDK capture"]
fn original_rich_complete_timeline_matches_sdk_threads_windows_and_received_frames() {
    let fixtures = std::env::var_os("PVD_NATIVE_FIXTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../captures/native"));
    let raw = std::fs::read(fixtures.join("rich-paired.pvdstream")).unwrap();
    let directory = std::env::temp_dir().join(format!(
        "pvd-native-timeline-paired-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let config = pvd_core::live::HistoryConfig {
        directory: directory.clone(),
        disk_budget_bytes: 256 << 20,
        segment_frames: 17,
        min_free_disk_bytes: 0,
        ..Default::default()
    };
    let (mut recording, info) = native::read_path_with_progress(
        fixtures.join("rich-paired-native.pxd2"),
        config,
        |_, _| true,
    )
    .unwrap();
    assert_eq!(recording.frame_count(), 120);
    let cancel = AtomicBool::new(false);
    let identity = timeline::native::identity(&recording).unwrap();
    recording.analysis.retain_recent(119).unwrap();
    recording.analysis = recording.analysis_at(0).unwrap();
    assert!(recording.analysis.profile_spans().len() < 19_215);
    assert_eq!(timeline::native::identity(&recording).unwrap(), identity);
    let (mut sdk_cursor, mut native_cursor) = (None, None);
    let mut thread_count = 0;
    let (native_catalog, sdk_catalog) = loop {
        let sdk = timeline::catalog_page_streaming(
            &mut Cursor::new(&raw),
            sdk_cursor.as_ref(),
            1,
            &cancel,
            |_| true,
        )
        .unwrap();
        let native =
            timeline::native::catalog_page(&recording, native_cursor.as_ref(), 1, &cancel, |_| {
                true
            })
            .unwrap();
        assert_eq!(native.clock, sdk.clock);
        assert_eq!(native.completed_tick_range, sdk.completed_tick_range);
        assert_eq!(native.global_pairing, sdk.global_pairing);
        assert_eq!(native.legacy_cuda_buffers, sdk.legacy_cuda_buffers);
        assert_eq!(native.threads, sdk.threads);
        thread_count += native.threads.len();
        assert_eq!(native.next.is_some(), sdk.next.is_some());
        if native.next.is_none() {
            break (native, sdk);
        }
        native_cursor = native.next;
        sdk_cursor = sdk.next;
    };
    assert!(thread_count > 0);
    assert_eq!(native_catalog.global_pairing.complete_spans, 19_215);
    let range = native_catalog.completed_tick_range.unwrap();
    let middle = range.start + (range.end - range.start) / 2;
    let ranges = [
        TickRange {
            start: 0,
            end: u64::MAX,
        },
        TickRange {
            start: middle,
            end: middle,
        },
        TickRange {
            start: range.start,
            end: middle,
        },
        TickRange {
            start: middle,
            end: range.end,
        },
    ];
    let sample = timeline::window_streaming(
        &mut Cursor::new(&raw),
        &sdk_catalog.identity,
        WindowRequest {
            ticks: ranges[0],
            thread: None,
            context: None,
            limits: WindowLimits::default(),
        },
        &cancel,
        |_| true,
    )
    .unwrap();
    let first = sample.spans.first().unwrap();
    let mut compared = 0;
    for ticks in ranges {
        for (thread, context) in [
            (None, None),
            (Some(first.thread), None),
            (Some(first.thread), Some(first.context)),
        ] {
            let request = WindowRequest {
                ticks,
                thread,
                context,
                limits: WindowLimits::default(),
            };
            let sdk = timeline::window_streaming(
                &mut Cursor::new(&raw),
                &sdk_catalog.identity,
                request,
                &cancel,
                |_| true,
            )
            .unwrap();
            let native = timeline::native::window(
                &recording,
                &native_catalog.identity,
                request,
                &cancel,
                |_| true,
            )
            .unwrap();
            assert_eq!(native.clock, sdk.clock);
            assert_eq!(native.global_pairing, sdk.global_pairing);
            assert_eq!(native.legacy_cuda_buffers, sdk.legacy_cuda_buffers);
            assert_eq!(native.spans.len(), sdk.spans.len());
            for (span, expected) in native.spans.iter().zip(&sdk.spans) {
                assert_eq!(
                    span_fields(span, info.source_ids[&span.zone]),
                    span_fields(expected, expected.zone)
                );
            }
            let names: BTreeMap<_, _> = native
                .names
                .into_iter()
                .map(|((zone, event), names)| ((info.source_ids[&zone], event), names))
                .collect();
            assert_eq!(names, sdk.names);
            compared += native.spans.len();
        }
    }
    eprintln!(
        "native timeline: {thread_count} threads, 19215 complete spans, 12 windows, {compared} matched rows"
    );
    drop(recording);
    assert!(directory.starts_with(std::env::temp_dir()));
    std::fs::remove_dir_all(directory).unwrap();
}
