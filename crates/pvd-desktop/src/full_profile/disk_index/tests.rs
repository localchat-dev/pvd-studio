use super::*;
use crate::{clip_store::Identity, full_profile::Source, i18n::Language};
use pvd_core::profile_disk_index::Phase;
use pvd_core::profile_timeline::{TickRange, WindowLimits};
use std::{fs, sync::mpsc, time::SystemTime};

struct Case {
    root: PathBuf,
    path: PathBuf,
    bytes: Arc<Vec<u8>>,
    identity: SourceIdentity,
    slot: Arc<IndexSlot>,
}

impl Case {
    fn new() -> Self {
        Self::from_bytes(crate::full_profile::tests::timeline_capture(20, "CPU test"))
    }

    fn from_bytes(bytes: Vec<u8>) -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../output/index-slot-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        fs::create_dir_all(&root).unwrap();
        let bytes = Arc::new(bytes);
        let path = root.join("source.pvdstream");
        fs::write(&path, bytes.as_slice()).unwrap();
        let identity = SourceIdentity {
            blake3: *blake3::hash(&bytes).as_bytes(),
            bytes: bytes.len() as u64,
            frames: pvd_core::Recording::from_bytes(&bytes)
                .unwrap()
                .frame_count(),
        };
        let slot = Arc::new(IndexSlot::new(
            root.join("cache"),
            IndexLimits::default(),
            BackgroundJobs::default(),
        ));
        Self {
            root,
            path,
            bytes,
            identity,
            slot,
        }
    }
    fn source(&self) -> Source {
        Source::memory(
            self.bytes.clone(),
            Identity::from_digest(
                blake3::hash(&self.bytes),
                self.identity.bytes,
                self.identity.frames,
            ),
        )
        .with_cpu_index(self.path.clone(), self.slot.clone())
    }
    fn query(&self) -> WindowReport {
        self.slot
            .window(
                &self.path,
                &self.identity,
                request(),
                &AtomicBool::new(false),
                |_| true,
            )
            .unwrap()
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
        self.slot.close();
        wait_cleanup(&self.slot);
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(self.root.join("cache/.pvd-studio-profile-lock"));
        let _ = fs::remove_dir(self.root.join("cache"));
        let _ = fs::remove_dir(&self.root);
    }
}
fn request() -> WindowRequest {
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
fn fields(report: &WindowReport) -> Vec<(u64, u16, u32, u64, u64, u64, usize)> {
    report
        .spans
        .iter()
        .map(|span| {
            (
                span.zone,
                span.event,
                span.thread,
                span.context,
                span.start,
                span.end,
                span.received_frame,
            )
        })
        .collect()
}

fn catalog(case: &Case) -> FirstStartPageReport {
    case.slot
        .catalog_page(
            &case.path,
            &case.identity,
            None,
            1024,
            &AtomicBool::new(false),
            |_| true,
        )
        .unwrap()
}

#[test]
fn cpu_and_catalog_capabilities_upgrade_once_and_share_the_root_budget() {
    for catalog_first in [false, true] {
        let case = Case::new();
        if catalog_first {
            catalog(&case);
        } else {
            case.query();
        }
        assert_eq!(case.slot.builds.load(Ordering::Relaxed), 1);
        let page = catalog(&case);
        let window = case.query();
        assert!(!page.entries.is_empty() && !window.spans.is_empty());
        assert_eq!(case.slot.builds.load(Ordering::Relaxed), 2);
        catalog(&case);
        case.query();
        assert_eq!(case.slot.builds.load(Ordering::Relaxed), 2);
        let state = case.slot.state.lock().unwrap();
        let ready = state.ready.as_ref().unwrap();
        let owner = &ready.owner;
        assert!(owner.supports(Capabilities {
            cpu: true,
            catalog: true
        }));
        assert!(
            owner.index.as_ref().unwrap().info().disk_bytes
                + owner.catalog.as_ref().unwrap().info().peak_disk_bytes
                <= owner._lease.disk_budget()
        );
        assert_eq!(case.payloads(), 1);
    }
}

#[test]
fn capability_upgrade_waits_for_old_queries_and_close_cancels_the_wait() {
    let case = Case::new();
    case.query();
    let held = case.slot.state.lock().unwrap().ready.clone().unwrap();
    let (started, waiting) = mpsc::channel();
    let slot = case.slot.clone();
    let path = case.path.clone();
    let identity = case.identity.clone();
    let worker = std::thread::spawn(move || {
        started.send(()).unwrap();
        slot.catalog_page(
            &path,
            &identity,
            None,
            1024,
            &AtomicBool::new(false),
            |_| true,
        )
    });
    waiting.recv_timeout(Duration::from_secs(5)).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while case.slot.state.lock().unwrap().building.is_none() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(case.slot.builds.load(Ordering::Relaxed), 1);
    assert_eq!(case.payloads(), 1);
    case.slot.close();
    assert!(worker.join().unwrap().is_err());
    assert_eq!(case.slot.jobs.active_work_count(), 1);
    assert!(
        held.owner
            .index
            .as_ref()
            .unwrap()
            .window(request(), &AtomicBool::new(false), |_| true)
            .is_ok()
    );
    drop(held);
    wait_cleanup(&case.slot);
    assert_eq!(case.payloads(), 0);
}

#[test]
fn first_start_catalog_uses_actual_source_and_can_fall_back_to_memory_key_pages() {
    let case = Case::new();
    let source = case.source();
    let cancel = AtomicBool::new(false);
    let (page, _) = source
        .catalog_page_first_start(None, 1024, &cancel, |_| true)
        .unwrap();
    let (key_page, _) = source.catalog_page(None, 1024, &cancel, |_| true).unwrap();
    assert_eq!(page.entries.len(), key_page.entries.len());
    let mut changed = case.bytes.as_ref().clone();
    let last = changed.len() - 1;
    changed[last] ^= 1;
    fs::write(&case.path, &changed).unwrap();
    assert!(
        source
            .catalog_page_first_start(None, 1024, &cancel, |_| true)
            .is_err()
    );
    assert!(source.catalog_page(None, 1024, &cancel, |_| true).is_ok());
    fs::write(&case.path, case.bytes.as_slice()).unwrap();
    assert!(
        source
            .catalog_page_first_start(None, 1024, &cancel, |_| true)
            .is_ok()
    );
}

#[test]
fn combined_build_budget_failure_does_not_publish_partial_capabilities() {
    let sizing = Case::new();
    sizing.query();
    let (cpu_bytes, cpu_peak) = {
        let state = sizing.slot.state.lock().unwrap();
        let info = state
            .ready
            .as_ref()
            .unwrap()
            .owner
            .index
            .as_ref()
            .unwrap()
            .info();
        (info.disk_bytes, info.peak_disk_bytes)
    };
    let catalog_case = Case::new();
    catalog(&catalog_case);
    let catalog_peak = catalog_case
        .slot
        .state
        .lock()
        .unwrap()
        .ready
        .as_ref()
        .unwrap()
        .owner
        .catalog
        .as_ref()
        .unwrap()
        .info()
        .peak_disk_bytes;
    let budget = cpu_bytes + catalog_peak - 1;
    assert!(budget >= cpu_peak && budget >= catalog_peak);
    let case = Case::new();
    let limited = IndexSlot::new(
        case.root.join("limited"),
        IndexLimits {
            disk_bytes: budget,
            ..Default::default()
        },
        BackgroundJobs::default(),
    );
    limited
        .window(
            &case.path,
            &case.identity,
            request(),
            &AtomicBool::new(false),
            |_| true,
        )
        .unwrap();
    let error = limited
        .catalog_page(
            &case.path,
            &case.identity,
            None,
            1024,
            &AtomicBool::new(false),
            |_| true,
        )
        .err()
        .unwrap();
    assert!(error.text(Language::English).contains("budget"));
    assert!(limited.state.lock().unwrap().ready.is_none());
    limited
        .window(
            &case.path,
            &case.identity,
            request(),
            &AtomicBool::new(false),
            |_| true,
        )
        .unwrap();
    assert!(
        !limited
            .state
            .lock()
            .unwrap()
            .ready
            .as_ref()
            .unwrap()
            .owner
            .supports(Capabilities {
                cpu: false,
                catalog: true
            })
    );
    limited.close();
    wait_cleanup(&limited);
    let names = fs::read_dir(case.root.join("limited"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [std::ffi::OsString::from(".pvd-studio-profile-lock")]
    );
    fs::remove_file(case.root.join("limited/.pvd-studio-profile-lock")).unwrap();
    fs::remove_dir(case.root.join("limited")).unwrap();
}

#[test]
fn repeated_source_handles_share_one_build_and_match_streaming() {
    let case = Case::new();
    let source = case.source();
    let cancel = AtomicBool::new(false);
    let expected = pvd_core::profile_timeline::window_streaming(
        &mut std::io::Cursor::new(case.bytes.as_slice()),
        &case.identity,
        request(),
        &cancel,
        |_| true,
    )
    .unwrap();
    let mut phases = Vec::new();
    let first = source
        .thread_window(&case.identity, request(), &cancel, |p| {
            if let crate::full_profile::TimelineProgress::Index(p) = p {
                phases.push(p.phase);
            }
            true
        })
        .unwrap();
    assert!(phases.contains(&Phase::Decode));
    phases.clear();
    let second = case
        .source()
        .thread_window(&case.identity, request(), &cancel, |p| {
            if let crate::full_profile::TimelineProgress::Index(p) = p {
                phases.push(p.phase);
            }
            true
        })
        .unwrap();
    assert!(!phases.contains(&Phase::Decode));
    assert!(phases.contains(&Phase::VerifySource));
    assert_eq!(fields(&first), fields(&expected));
    assert_eq!(fields(&second), fields(&expected));
    assert_eq!(second.names, expected.names);
    assert_eq!(case.slot.builds.load(Ordering::Relaxed), 1);
    assert_eq!(case.payloads(), 1);
    case.slot.close();
    wait_cleanup(&case.slot);
    assert_eq!(case.payloads(), 0);
    assert!(
        source
            .thread_window(&case.identity, request(), &cancel, |_| true)
            .is_err()
    );
    source.rebuild_cpu_index();
    assert!(
        source
            .thread_window(&case.identity, request(), &cancel, |_| true)
            .is_err()
    );
}

#[test]
fn concurrent_waiter_cancellation_and_close_do_not_publish_a_build() {
    let case = Case::new();
    let slot = case.slot.clone();
    let path = case.path.clone();
    let identity = case.identity.clone();
    let (entered_tx, entered) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut first = true;
        slot.window(&path, &identity, request(), &AtomicBool::new(false), |p| {
            if first && p.phase == Phase::Decode {
                first = false;
                entered_tx.send(()).unwrap();
                released.recv().unwrap();
            }
            true
        })
    });
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let waiting_cancel = cancel.clone();
    let slot = case.slot.clone();
    let path = case.path.clone();
    let identity = case.identity.clone();
    let (done_tx, done) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        done_tx
            .send(
                slot.window(&path, &identity, request(), &waiting_cancel, |_| true)
                    .is_err(),
            )
            .unwrap();
    });
    cancel.store(true, Ordering::Release);
    assert!(done.recv_timeout(Duration::from_secs(2)).unwrap());
    waiter.join().unwrap();
    case.slot.close();
    release.send(()).unwrap();
    assert!(worker.join().unwrap().is_err());
    assert!(case.slot.state.lock().unwrap().ready.is_none());
    wait_cleanup(&case.slot);
    assert_eq!(case.payloads(), 0);
}

#[test]
fn rebuild_retires_old_query_and_serializes_the_next_root_owner() {
    let case = Case::new();
    case.query();
    let slot = case.slot.clone();
    let path = case.path.clone();
    let identity = case.identity.clone();
    let (entered_tx, entered) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut first = true;
        slot.window(&path, &identity, request(), &AtomicBool::new(false), |p| {
            if first && p.phase == Phase::VerifySource {
                first = false;
                entered_tx.send(()).unwrap();
                released.recv().unwrap();
            }
            true
        })
    });
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    case.slot.close();
    assert_eq!(case.payloads(), 1);
    let next = Arc::new(IndexSlot::new(
        case.root.join("cache"),
        IndexLimits::default(),
        BackgroundJobs::default(),
    ));
    let (waiting_tx, waiting) = mpsc::channel();
    let (done_tx, done) = mpsc::channel();
    let new_slot = next.clone();
    let path = case.path.clone();
    let identity = case.identity.clone();
    let new_worker = std::thread::spawn(move || {
        waiting_tx.send(()).unwrap();
        let result = new_slot.window(&path, &identity, request(), &AtomicBool::new(false), |_| {
            true
        });
        done_tx.send(result.is_ok()).unwrap();
    });
    waiting.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(done.recv_timeout(Duration::from_millis(80)).is_err());
    release.send(()).unwrap();
    assert!(worker.join().unwrap().is_err());
    assert!(done.recv_timeout(Duration::from_secs(5)).unwrap());
    new_worker.join().unwrap();
    assert_eq!(next.builds.load(Ordering::Relaxed), 1);
    next.close();
    wait_cleanup(&next);
    wait_cleanup(&case.slot);
    assert_eq!(case.payloads(), 0);
}

#[test]
fn a_stale_build_ticket_cannot_clear_a_new_generation() {
    let slot = IndexSlot::new(
        PathBuf::from("unused"),
        IndexLimits::default(),
        BackgroundJobs::default(),
    );
    let old_cancel = Arc::new(AtomicBool::new(false));
    slot.state.lock().unwrap().building = Some(Building {
        generation: 1,
        cancel: old_cancel.clone(),
    });
    let ticket = BuildTicket {
        slot: &slot,
        generation: 1,
    };
    slot.rebuild();
    assert!(old_cancel.load(Ordering::Acquire));
    slot.state.lock().unwrap().building = Some(Building {
        generation: 2,
        cancel: Arc::new(AtomicBool::new(false)),
    });
    drop(ticket);
    assert_eq!(
        slot.state
            .lock()
            .unwrap()
            .building
            .as_ref()
            .unwrap()
            .generation,
        2
    );
}

#[test]
fn changed_file_is_rejected_even_when_source_retains_original_memory() {
    let case = Case::new();
    let source = case.source();
    case.query();
    let changed = crate::full_profile::tests::timeline_capture(20, "bad test");
    assert_eq!(changed.len(), case.bytes.len());
    fs::write(&case.path, &changed).unwrap();
    let error = source
        .thread_window(&case.identity, request(), &AtomicBool::new(false), |_| true)
        .err()
        .unwrap();
    assert!(error.text(Language::English).contains("changed"));
    source.rebuild_cpu_index();
    assert!(
        source
            .thread_window(&case.identity, request(), &AtomicBool::new(false), |_| true)
            .is_err()
    );
    wait_cleanup(&case.slot);
    assert_eq!(case.payloads(), 0);
}

fn wait_cleanup(slot: &IndexSlot) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while slot.jobs.active_count() != 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "cleanup did not complete"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn retirement_never_runs_blocking_file_cleanup_on_the_caller() {
    let case = Case::new();
    let (entered_tx, entered) = mpsc::channel();
    let (release, released) = mpsc::channel();
    *case.slot.cleanup_gate.lock().unwrap() = Some(Box::new(move || {
        entered_tx.send(()).unwrap();
        let _ = released.recv();
    }));
    case.query();
    let slot = case.slot.clone();
    let (returned_tx, returned) = mpsc::channel();
    let caller = std::thread::spawn(move || {
        slot.close();
        returned_tx.send(()).unwrap();
    });
    returned.recv_timeout(Duration::from_secs(2)).unwrap();
    caller.join().unwrap();
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(case.payloads(), 1);
    assert_eq!(case.slot.jobs.active_work_count(), 0);
    assert_eq!(case.slot.jobs.active_count(), 1);
    assert!(!case.slot.jobs.cancel_all_and_check_finished());
    release.send(()).unwrap();
    wait_cleanup(&case.slot);
    assert_eq!(case.payloads(), 0);
}

#[test]
fn unavailable_cleanup_registry_never_publishes_a_ready_index() {
    let case = Case::new();
    assert!(case.slot.jobs.cancel_all_and_check_finished());
    let result = case.slot.window(
        &case.path,
        &case.identity,
        request(),
        &AtomicBool::new(false),
        |_| true,
    );
    assert!(
        result
            .err()
            .unwrap()
            .text(Language::English)
            .contains("shutdown")
    );
    assert!(case.slot.state.lock().unwrap().ready.is_none());
    assert_eq!(case.payloads(), 0);
}

#[test]
#[ignore = "requires PVD_TIMELINE_ROOT with two official SDK QPC inputs; verifies the desktop indexed Source"]
fn official_sdk_source_windows_reuse_one_index_and_release_all_payloads() {
    use crate::full_profile::TimelineProgress;
    let root = PathBuf::from(std::env::var_os("PVD_TIMELINE_ROOT").expect("PVD_TIMELINE_ROOT"));
    for name in ["physx-3.4-profile-metrics", "physx-3.4-profile-events"] {
        let original = fs::read(root.join("captures").join(format!("{name}.pvdstream"))).unwrap();
        let case = Case::from_bytes(original.clone());
        let source = Source::file(
            case.path.clone(),
            Identity::from_digest(
                blake3::hash(&original),
                case.identity.bytes,
                case.identity.frames,
            ),
        )
        .with_cpu_index(case.path.clone(), case.slot.clone());
        let cancel = AtomicBool::new(false);
        let catalog = source.thread_page(None, 512, &cancel, |_| true).unwrap();
        assert_eq!(catalog.identity, case.identity);
        let all = pvd_core::profile_timeline::window_streaming(
            &mut std::io::Cursor::new(&original),
            &case.identity,
            WindowRequest {
                ticks: TickRange {
                    start: 0,
                    end: u64::MAX,
                },
                ..request()
            },
            &cancel,
            |_| true,
        )
        .unwrap();
        let mut reports = Vec::new();
        for (number, position) in [0, all.spans.len() / 2, all.spans.len() - 1]
            .into_iter()
            .enumerate()
        {
            let span = &all.spans[position];
            let tick = span.start + (span.end - span.start) / 2;
            let query = WindowRequest {
                ticks: TickRange {
                    start: tick,
                    end: tick,
                },
                thread: Some(span.thread),
                context: Some(span.context),
                ..request()
            };
            let expected = pvd_core::profile_timeline::window_streaming(
                &mut std::io::Cursor::new(&original),
                &case.identity,
                query,
                &cancel,
                |_| true,
            )
            .unwrap();
            let mut phases = Vec::new();
            let actual = source
                .thread_window(&case.identity, query, &cancel, |status| {
                    match status {
                        TimelineProgress::Index(progress) => phases.push(progress.phase),
                        TimelineProgress::Streaming(_) => {
                            panic!("indexed Source fell back to streaming")
                        }
                    }
                    true
                })
                .unwrap();
            assert_eq!(phases.contains(&Phase::Decode), number == 0);
            assert!(phases.contains(&Phase::VerifySource));
            assert!(!actual.spans.is_empty());
            assert_eq!(fields(&actual), fields(&expected));
            assert_eq!(actual.names, expected.names);
            assert_eq!(actual.global_pairing, expected.global_pairing);
            assert_eq!(actual.legacy_cuda_buffers, expected.legacy_cuda_buffers);
            assert_eq!(actual.clock, expected.clock);
            reports.push(actual.spans.len());
        }
        assert_eq!(case.slot.builds.load(Ordering::Relaxed), 1);
        let mut changed = original.clone();
        changed[0] ^= 1;
        fs::write(&case.path, &changed).unwrap();
        assert!(
            source
                .thread_window(&case.identity, request(), &cancel, |_| true)
                .is_err()
        );
        case.slot.close();
        wait_cleanup(&case.slot);
        assert_eq!(case.payloads(), 0);
        println!(
            "desktop_cpu_index {}",
            serde_json::json!({"input":name,"source_bytes":original.len(),"source_blake3":blake3::hash(&original).to_hex().to_string(),"full_complete_spans":all.spans.len(),"window_counts":reports,"builds":case.slot.builds.load(Ordering::Relaxed),"remaining_payloads":case.payloads()})
        );
    }
}

fn assert_window_same(actual: &WindowReport, expected: &WindowReport) {
    assert_eq!(fields(actual), fields(expected));
    assert_eq!(actual.identity, expected.identity);
    assert_eq!(actual.clock, expected.clock);
    assert_eq!(actual.names, expected.names);
    assert_eq!(actual.global_pairing, expected.global_pairing);
    assert_eq!(actual.legacy_cuda_buffers, expected.legacy_cuda_buffers);
    assert_eq!(actual.request.ticks, expected.request.ticks);
    assert_eq!(actual.request.thread, expected.request.thread);
    assert_eq!(actual.request.context, expected.request.context);
    assert_eq!(actual.request.limits.spans, expected.request.limits.spans);
    assert_eq!(actual.request.limits.bytes, expected.request.limits.bytes);
    assert_eq!(actual.estimated_index_bytes, expected.estimated_index_bytes);
}

#[test]
fn explicit_streaming_recovers_from_index_budget_failure_without_payloads() {
    let mut case = Case::new();
    Arc::get_mut(&mut case.slot).unwrap().limits.disk_bytes = 256;
    let source = case.source();
    let cancel = AtomicBool::new(false);
    let error = source
        .thread_window(&case.identity, request(), &cancel, |_| true)
        .unwrap_err();
    assert!(error.text(Language::English).contains("budget"));
    assert_eq!(case.slot.builds.load(Ordering::Relaxed), 1);
    assert_eq!(case.payloads(), 0);
    let expected = pvd_core::profile_timeline::window_streaming(
        &mut std::io::Cursor::new(case.bytes.as_slice()),
        &case.identity,
        request(),
        &cancel,
        |_| true,
    )
    .unwrap();
    let mut progress = 0;
    let actual = source
        .thread_window_streaming(&case.identity, request(), &cancel, |status| {
            assert!(matches!(
                status,
                crate::full_profile::TimelineProgress::Streaming(_)
            ));
            progress += 1;
            true
        })
        .unwrap();
    assert!(progress > 0);
    assert_window_same(&actual, &expected);
    assert_eq!(case.slot.builds.load(Ordering::Relaxed), 1);
    assert_eq!(case.payloads(), 0);
}

#[test]
fn explicit_streaming_does_not_wait_for_another_profile_cache_lease() {
    let case = Case::new();
    let held = profile_cache::Lease::acquire(&case.root.join("cache"), || false).unwrap();
    let existing = case.payloads();
    let source = case.source();
    let identity = case.identity.clone();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = cancel.clone();
    let (done, result) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let report = source.thread_window_streaming(&identity, request(), &worker_cancel, |_| true);
        done.send(report).unwrap();
    });
    let result = result.recv_timeout(Duration::from_secs(2));
    cancel.store(true, Ordering::Release);
    worker.join().unwrap();
    assert_eq!(result.unwrap().unwrap().spans.len(), 20);
    assert_eq!(case.slot.builds.load(Ordering::Relaxed), 0);
    assert_eq!(case.payloads(), existing);
    assert!(!held.workspace().exists());
    drop(held);
    assert_eq!(case.payloads(), 0);
}

#[test]
fn explicit_streaming_checks_actual_file_identity_limits_cancellation_and_close() {
    let case = Case::new();
    let source = case.source();
    let cancel = AtomicBool::new(false);
    let mut query = request();
    query.limits.spans = 1;
    assert!(
        source
            .thread_window_streaming(&case.identity, query, &cancel, |_| true)
            .is_err()
    );
    query = request();
    query.limits.bytes = 1;
    assert!(
        source
            .thread_window_streaming(&case.identity, query, &cancel, |_| true)
            .is_err()
    );
    assert!(
        source
            .thread_window_streaming(&case.identity, request(), &cancel, |_| false)
            .is_err()
    );
    cancel.store(true, Ordering::Release);
    assert!(
        source
            .thread_window_streaming(&case.identity, request(), &cancel, |_| true)
            .is_err()
    );
    cancel.store(false, Ordering::Release);
    assert_eq!(
        source
            .thread_window_streaming(&case.identity, request(), &cancel, |_| true)
            .unwrap()
            .spans
            .len(),
        20
    );
    let changed = crate::full_profile::tests::timeline_capture(20, "CPU draw");
    assert_eq!(changed.len(), case.bytes.len());
    fs::write(&case.path, changed).unwrap();
    assert!(
        source
            .thread_window_streaming(&case.identity, request(), &cancel, |_| true)
            .is_err()
    );
    assert!(
        source
            .thread_window(&case.identity, request(), &cancel, |_| true)
            .is_err()
    );
    fs::write(&case.path, case.bytes.as_slice()).unwrap();
    case.slot.close();
    assert!(
        source
            .thread_window_streaming(&case.identity, request(), &cancel, |_| true)
            .is_err()
    );
    assert_eq!(case.payloads(), 0);
}

#[test]
fn closing_during_explicit_file_scan_prevents_publication() {
    let case = Case::new();
    let source = case.source();
    let identity = case.identity.clone();
    let (entered_tx, entered) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut first = true;
        source.thread_window_streaming(&identity, request(), &AtomicBool::new(false), |_| {
            if first {
                first = false;
                entered_tx.send(()).unwrap();
                released.recv().unwrap();
            }
            true
        })
    });
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    case.slot.close();
    release.send(()).unwrap();
    assert!(worker.join().unwrap().is_err());
    assert_eq!(case.slot.builds.load(Ordering::Relaxed), 0);
    assert!(!case.root.join("cache").exists());
}
