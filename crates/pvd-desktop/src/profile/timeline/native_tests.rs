use super::{
    tests::{poll, wait},
    *,
};
use pvd_core::Recording;
use std::{path::PathBuf, time::Duration};

fn fixture() -> PathBuf {
    std::env::var_os("PVD_NATIVE_RICH_CAPTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../captures/native/rich-paired-native.pxd2")
        })
}

fn fields(span: &pvd_core::analysis::ProfileSpan) -> (u64, u16, u32, u64, u64, u64, usize) {
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
#[ignore = "requires the original PVD3 paired rich container; set PVD_NATIVE_RICH_CAPTURE"]
fn native_thread_jobs_cover_complete_history_without_a_raw_source_and_survive_display_seek() {
    let path = fixture();
    let reference = Recording::from_bytes(&std::fs::read(&path).unwrap()).unwrap();
    let expected: Vec<_> = reference.analysis.profile_spans().map(fields).collect();
    assert!(!expected.is_empty());
    let context = egui::Context::default();
    let mut studio = Studio::default();
    studio.open(path, context.clone());
    let deadline = Instant::now() + Duration::from_secs(60);
    while studio.job.is_some() || studio.seeking.is_some() {
        assert!(Instant::now() < deadline, "native open timed out");
        studio.poll(&context);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(studio.recording.as_ref().unwrap().is_disk_backed());
    assert!(studio.complete_profile_source().is_none());
    assert!(studio.bytes.is_none());
    let source = studio.complete_timeline_source().unwrap();
    assert!(!source.can_rebuild_cpu_index());
    let recording = studio.recording.as_ref().unwrap().clone();
    let jobs = studio.background_jobs.clone();
    let mut state = State::default();
    let epoch = studio.profile_view_epoch;
    state.sync(epoch);
    wait(&mut state, &source, &recording, &context, &jobs, epoch);
    assert!(state.catalog.error.is_none());
    let overview = state.overview.as_ref().unwrap().clone();
    assert!(matches!(overview.identity, Identity::Native(_)));
    assert_eq!(overview.stats.complete_spans, expected.len() as u64);
    let all = WindowRequest {
        ticks: overview.range.unwrap(),
        thread: None,
        context: None,
        limits: WindowLimits::default(),
    };
    state.request_window(all);
    wait(&mut state, &source, &recording, &context, &jobs, epoch);
    let prepared = state.window.prepared.as_ref().unwrap().clone();
    assert_eq!(
        prepared
            .analysis
            .profile_spans()
            .map(fields)
            .collect::<Vec<_>>(),
        expected
    );
    assert!(matches!(
        state.progress.lock().unwrap().as_ref(),
        Some(TimelineProgress::Native(_))
    ));
    for frame in [119, 60, 0] {
        studio.seek(frame);
        while studio.seeking.is_some() {
            assert!(Instant::now() < deadline, "native seek timed out");
            studio.poll(&context);
            std::thread::sleep(Duration::from_millis(1));
        }
        let displayed = studio.recording.as_ref().unwrap().clone();
        let revision = displayed.analysis.revision();
        let source = studio.complete_timeline_source().unwrap();
        let catalog = source
            .thread_page(None, 1, &AtomicBool::new(false), |_| true)
            .unwrap();
        assert_eq!(catalog.identity, overview.identity);
        if let Some(next) = &catalog.next {
            let page = source
                .thread_page(Some(next), 1, &AtomicBool::new(false), |_| true)
                .unwrap();
            assert!(page.threads[0].thread > catalog.threads[0].thread);
        }
        poll(&mut state, &source, &displayed, &context, &jobs, epoch);
        assert!(Arc::ptr_eq(
            state.window.prepared.as_ref().unwrap(),
            &prepared
        ));
        let tail = reference.analysis.profile_spans().next_back().unwrap();
        let request = WindowRequest {
            ticks: TickRange {
                start: tail.start,
                end: tail.end,
            },
            thread: Some(tail.thread),
            context: Some(tail.context),
            limits: WindowLimits::default(),
        };
        let report = source
            .thread_window(&overview.identity, request, &AtomicBool::new(false), |_| {
                true
            })
            .unwrap();
        let expected: Vec<_> = reference
            .analysis
            .profile_spans()
            .filter(|span| {
                span.end >= tail.start
                    && span.start <= tail.end
                    && span.thread == tail.thread
                    && span.context == tail.context
            })
            .map(fields)
            .collect();
        assert_eq!(
            report.spans.iter().map(fields).collect::<Vec<_>>(),
            expected
        );
        assert_eq!(displayed.analysis.revision(), revision);
    }

    let mut no_raw_source = Studio::default();
    no_raw_source.recording = studio.recording.clone();
    assert!(no_raw_source.bytes.is_none());
    assert!(no_raw_source.capture_path.is_none());
    let source = no_raw_source.complete_timeline_source().unwrap();
    assert!(!source.can_rebuild_cpu_index());
    assert!(
        source
            .thread_page(None, 1, &AtomicBool::new(false), |_| false)
            .is_err()
    );
    assert!(
        source
            .thread_window(&overview.identity, all, &AtomicBool::new(true), |_| true)
            .is_err()
    );
    let sdk_bytes = crate::full_profile::tests::timeline_capture(2, "different source");
    let sdk_recording = Recording::from_bytes(&sdk_bytes).unwrap();
    let sdk_identity = crate::clip_store::Identity::from_digest(
        blake3::hash(&sdk_bytes),
        sdk_bytes.len() as u64,
        sdk_recording.frame_count(),
    );
    let sdk = Source::memory(Arc::new(sdk_bytes), sdk_identity);
    let sdk_catalog = sdk
        .thread_page(None, 1, &AtomicBool::new(false), |_| true)
        .unwrap();
    assert!(
        source
            .thread_window(&sdk_catalog.identity, all, &AtomicBool::new(false), |_| {
                true
            })
            .is_err()
    );
    let native_cursor = ThreadCursor {
        identity: overview.identity.clone(),
        after: 0,
    };
    assert!(
        sdk.thread_page(Some(&native_cursor), 1, &AtomicBool::new(false), |_| true)
            .is_err()
    );
    let mut too_many = all;
    too_many.limits.spans = 1;
    assert!(
        source
            .thread_window(
                &overview.identity,
                too_many,
                &AtomicBool::new(false),
                |_| true
            )
            .is_err()
    );
    for action in 0..2 {
        let mut pending = State::default();
        pending.sync(epoch);
        poll(
            &mut pending,
            &source,
            studio.recording.as_ref().unwrap(),
            &context,
            &jobs,
            epoch,
        );
        let cancel = pending.catalog.job.as_ref().unwrap().cancel.clone();
        if action == 0 {
            pending.sync(epoch + 1);
        } else {
            no_raw_source.profile_layout_cancel = Some(cancel.clone());
            no_raw_source.close();
        }
        assert!(cancel.load(Ordering::Relaxed));
        assert!(pending.catalog.prepared.is_none());
    }
    studio.close();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !studio.poll_shutdown() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
}
