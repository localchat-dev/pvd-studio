use super::*;
use crate::full_profile::{IndexSlot, TimelineProgress};
use pvd_core::{
    profile_disk_index::Phase,
    profile_timeline::{TickRange, WindowLimits, WindowRequest},
};
use std::{fs, sync::mpsc};

struct Case {
    root: PathBuf,
    path: PathBuf,
    studio: Studio,
}
impl Case {
    fn new() -> Self {
        Self::with_sidecar(false)
    }
    fn with_sidecar(bad_sidecar: bool) -> Self {
        Self::from_bytes(
            crate::full_profile::tests::timeline_capture(20, "studio test"),
            bad_sidecar,
        )
    }
    fn from_bytes(bytes: Vec<u8>, bad_sidecar: bool) -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../output/studio-index-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        fs::create_dir_all(&root).unwrap();
        let bytes = Arc::new(bytes);
        let path = root.join("source.pvdstream");
        fs::write(&path, bytes.as_slice()).unwrap();
        if bad_sidecar {
            fs::write(crate::clip_store::sidecar_path(&path), b"not valid JSON").unwrap();
        }
        let mut studio = Studio::default();
        let context = egui::Context::default();
        studio.open(path.clone(), context.clone());
        let deadline = Instant::now() + Duration::from_secs(5);
        while studio.job.is_some() {
            assert!(Instant::now() < deadline, "file load timed out");
            studio.poll(&context);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(studio.recording.is_some());
        assert!(studio.bytes.is_some());
        assert_eq!(studio.capture_path.as_ref(), Some(&path));
        assert!(
            studio
                .complete_profile_source()
                .unwrap()
                .can_rebuild_cpu_index()
        );
        studio.profile_cpu_index.close();
        studio.profile_cpu_index = Arc::new(IndexSlot::for_tests(
            root.join("cache"),
            &studio.background_jobs,
        ));
        Self { root, path, studio }
    }
    fn payloads(&self) -> usize {
        fs::read_dir(self.root.join("cache"))
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| entry.file_type().unwrap().is_dir())
            .count()
    }
}
impl Drop for Case {
    fn drop(&mut self) {
        self.studio.clear_recording();
        wait_cleanup(&self.studio);
        let _ = fs::remove_file(crate::clip_store::sidecar_path(&self.path));
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(self.root.join("cache/.pvd-studio-profile-lock"));
        let _ = fs::remove_dir(self.root.join("cache"));
        let _ = fs::remove_dir(&self.root);
    }
}
fn query() -> WindowRequest {
    WindowRequest {
        ticks: TickRange {
            start: 0,
            end: 1000,
        },
        thread: None,
        context: None,
        limits: WindowLimits::default(),
    }
}

#[test]
fn clearing_recording_invalidates_sources_held_by_old_view_state() {
    let mut case = Case::new();
    let source = case.studio.complete_profile_source().unwrap();
    let cancel = AtomicBool::new(false);
    let page = source.thread_page(None, 512, &cancel, |_| true).unwrap();
    assert_eq!(
        source
            .thread_window(&page.identity, query(), &cancel, |_| true)
            .unwrap()
            .spans
            .len(),
        20
    );
    assert_eq!(case.payloads(), 1);
    case.studio.clear_recording();
    assert!(case.studio.complete_profile_source().is_none());
    wait_cleanup(&case.studio);
    assert_eq!(case.payloads(), 0);
    source.rebuild_cpu_index();
    assert!(
        source
            .thread_window_streaming(&page.identity, query(), &cancel, |_| true)
            .is_err()
    );
    assert!(
        source
            .thread_window(&page.identity, query(), &cancel, |_| true)
            .is_err()
    );
}

#[test]
fn shutdown_waits_for_query_and_last_index_lease_to_retire() {
    let mut case = Case::new();
    let source = case.studio.complete_profile_source().unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let page = source.thread_page(None, 512, &cancel, |_| true).unwrap();
    source
        .thread_window(&page.identity, query(), &cancel, |_| true)
        .unwrap();
    let (entered_tx, entered) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let (result_tx, result) = mpsc::channel();
    let worker_cancel = cancel.clone();
    let worker = case
        .studio
        .background_jobs
        .spawn("index-shutdown", cancel, move || {
            let mut first = true;
            let value = source.thread_window(&page.identity, query(), &worker_cancel, |status| {
                if first
                    && matches!(status,TimelineProgress::Index(p) if p.phase==Phase::VerifySource)
                {
                    first = false;
                    entered_tx.send(()).unwrap();
                    released.recv().unwrap();
                }
                true
            });
            result_tx.send(value.is_err()).unwrap();
        })
        .unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(!case.studio.poll_shutdown());
    assert_eq!(case.payloads(), 1);
    release.send(()).unwrap();
    assert!(result.recv_timeout(Duration::from_secs(5)).unwrap());
    worker.join().unwrap();
    wait_cleanup(&case.studio);
    assert!(case.studio.poll_shutdown());
    assert_eq!(case.payloads(), 0);
}

fn wait_cleanup(studio: &Studio) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while studio.background_jobs.active_count() != 0 {
        assert!(Instant::now() < deadline, "cleanup did not complete");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn normal_small_file_open_binds_index_independently_of_sidecar_and_rejects_window_source_changes() {
    for bad_sidecar in [false, true] {
        let case = Case::with_sidecar(bad_sidecar);
        assert_eq!(case.studio.clip_persistence.issue.is_some(), bad_sidecar);
        assert!(case.studio.can_save_capture());
        assert!(case.studio.can_export_analysis());
        let source = case.studio.complete_profile_source().unwrap();
        let cancel = AtomicBool::new(false);
        let page = source.thread_page(None, 512, &cancel, |_| true).unwrap();
        let mut indexed = false;
        assert_eq!(
            source
                .thread_window(&page.identity, query(), &cancel, |status| {
                    indexed |= matches!(status, TimelineProgress::Index(_));
                    true
                })
                .unwrap()
                .spans
                .len(),
            20
        );
        assert!(indexed);
        assert_eq!(case.payloads(), 1);
        let mut streamed = false;
        assert_eq!(
            source
                .thread_window_streaming(&page.identity, query(), &cancel, |status| {
                    assert!(matches!(status, TimelineProgress::Streaming(_)));
                    streamed = true;
                    true
                })
                .unwrap()
                .spans
                .len(),
            20
        );
        assert!(streamed);
        let changed = crate::full_profile::tests::timeline_capture(20, "studio draw");
        assert_eq!(changed.len() as u64, page.identity.bytes);
        fs::write(&case.path, changed).unwrap();
        assert!(
            source
                .thread_window(&page.identity, query(), &cancel, |_| true)
                .is_err()
        );
        assert!(
            source
                .thread_window_streaming(&page.identity, query(), &cancel, |_| true)
                .is_err()
        );
        assert!(source.thread_page(None, 512, &cancel, |_| true).is_ok());
    }
}

#[test]
#[ignore = "requires existing PhysX 3.4 profile fixtures; set PVD_PROFILE_STREAM_ROOT"]
fn actual_sdk_open_dispatches_index_and_explicit_file_scan_for_the_same_windows() {
    let root = PathBuf::from(
        std::env::var_os("PVD_PROFILE_STREAM_ROOT").expect("PVD_PROFILE_STREAM_ROOT"),
    );
    let fields = |span: &pvd_core::analysis::ProfileSpan| {
        (
            span.zone,
            span.event,
            span.thread,
            span.context,
            span.start,
            span.end,
            span.received_frame,
        )
    };
    for name in [
        "physx-3.4-profile-metrics.pvdstream",
        "physx-3.4-profile-events.pvdstream",
    ] {
        let bytes = fs::read(root.join("captures").join(name)).unwrap();
        let digest = blake3::hash(&bytes);
        let mut case = Case::from_bytes(bytes, false);
        let source = case.studio.complete_profile_source().unwrap();
        let cancel = AtomicBool::new(false);
        let page = source.thread_page(None, 512, &cancel, |_| true).unwrap();
        let recording = case.studio.recording.as_ref().unwrap().clone();
        let mut counts = Vec::new();
        for (number, position) in [
            0,
            recording.analysis.profile_spans().len() / 2,
            recording.analysis.profile_spans().len() - 1,
        ]
        .into_iter()
        .enumerate()
        {
            let span = recording.analysis.profile_span(position).unwrap();
            let tick = span.start + (span.end - span.start) / 2;
            let request = WindowRequest {
                ticks: TickRange {
                    start: tick,
                    end: tick,
                },
                thread: Some(span.thread),
                context: Some(span.context),
                ..query()
            };
            let expected: Vec<_> = recording
                .analysis
                .profile_spans()
                .filter(|span| {
                    span.start <= tick
                        && span.end >= tick
                        && Some(span.thread) == request.thread
                        && Some(span.context) == request.context
                })
                .map(fields)
                .collect();
            let mut phases = Vec::new();
            let indexed = source
                .thread_window(&page.identity, request, &cancel, |status| {
                    match status {
                        TimelineProgress::Index(p) => phases.push(p.phase),
                        _ => panic!("actual file open did not use its CPU index"),
                    }
                    true
                })
                .unwrap();
            assert_eq!(phases.contains(&Phase::Decode), number == 0);
            assert!(phases.contains(&Phase::VerifySource));
            let scanned = source
                .thread_window_streaming(&page.identity, request, &cancel, |status| {
                    assert!(matches!(status, TimelineProgress::Streaming(_)));
                    true
                })
                .unwrap();
            assert_eq!(
                indexed.spans.iter().map(fields).collect::<Vec<_>>(),
                expected
            );
            assert_eq!(
                scanned.spans.iter().map(fields).collect::<Vec<_>>(),
                expected
            );
            assert_eq!(indexed.names, scanned.names);
            assert_eq!(indexed.identity, scanned.identity);
            assert_eq!(indexed.clock, scanned.clock);
            assert_eq!(indexed.global_pairing, scanned.global_pairing);
            assert_eq!(indexed.legacy_cuda_buffers, scanned.legacy_cuda_buffers);
            counts.push(expected.len());
        }
        let mut expected = pvd_core::profile_curves::catalog(&recording, &cancel).unwrap();
        expected.sort_by_key(|entry| (entry.first_start.is_none(), entry.first_start, entry.key));
        let mut cursor = None;
        let mut catalog_rows = 0;
        let mut catalog_pages = 0;
        loop {
            let mut phases = Vec::new();
            let (catalog, analysis) = source
                .catalog_page_first_start(cursor.as_ref(), 17, &cancel, |status| {
                    match status {
                        TimelineProgress::Index(p) => phases.push(p.phase),
                        _ => panic!("first-start catalog did not use its file index"),
                    }
                    true
                })
                .unwrap();
            assert_eq!(phases.contains(&Phase::Decode), catalog_pages == 0);
            assert!(phases.contains(&Phase::VerifySource));
            assert_eq!(catalog.identity, page.identity);
            assert_eq!(
                catalog.entries,
                expected[catalog_rows..catalog_rows + catalog.entries.len()]
            );
            for entry in &catalog.entries {
                assert_eq!(
                    analysis.profile_zone_name(entry.key.zone),
                    recording.analysis.profile_zone_name(entry.key.zone)
                );
                assert_eq!(
                    analysis.profile_event_name(entry.key.zone, entry.key.event),
                    recording
                        .analysis
                        .profile_event_name(entry.key.zone, entry.key.event)
                );
            }
            catalog_rows += catalog.entries.len();
            catalog_pages += 1;
            cursor = catalog.next;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(catalog_rows, expected.len());
        assert!(catalog_pages > 1);
        let first = recording.analysis.profile_span(0).unwrap();
        source
            .thread_window(
                &page.identity,
                WindowRequest {
                    ticks: TickRange {
                        start: first.start,
                        end: first.start,
                    },
                    ..query()
                },
                &cancel,
                |status| {
                    assert!(
                        !matches!(status, TimelineProgress::Index(p) if p.phase == Phase::Decode)
                    );
                    true
                },
            )
            .unwrap();
        assert_eq!(case.payloads(), 1);
        case.studio.clear_recording();
        wait_cleanup(&case.studio);
        assert_eq!(case.payloads(), 0);
        assert!(
            source
                .catalog_page_first_start(None, 17, &cancel, |_| true)
                .is_err()
        );
        println!(
            "actual_sdk_open {}",
            serde_json::json!({"input":name,"source_blake3":digest.to_hex().to_string(),"source_bytes":page.identity.bytes,"window_counts":counts,"indexed_windows":3,"streamed_windows":3,"catalog_rows":catalog_rows,"catalog_pages":catalog_pages,"remaining_payloads":case.payloads()})
        );
    }
}
