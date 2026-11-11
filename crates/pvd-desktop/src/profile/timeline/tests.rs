use super::*;
use pvd_core::Recording;
use std::{path::PathBuf, time::Duration};

fn capture(span_count: usize, name: &str) -> Vec<u8> {
    crate::full_profile::tests::timeline_capture(span_count, name)
}

fn memory(bytes: Vec<u8>) -> (Source, Arc<Recording>) {
    let recording = Arc::new(Recording::from_bytes(&bytes).unwrap());
    let identity = crate::clip_store::Identity::from_digest(
        blake3::hash(&bytes),
        bytes.len() as u64,
        recording.frame_count(),
    );
    (Source::memory(Arc::new(bytes), identity), recording)
}

pub(super) fn poll(
    state: &mut State,
    source: &Source,
    recording: &Arc<Recording>,
    context: &egui::Context,
    jobs: &crate::background_jobs::BackgroundJobs,
    epoch: u64,
) {
    state.poll(
        source,
        refresh::Request {
            recording,
            frame: 0,
            epoch,
            following: false,
            now: Instant::now(),
            context,
            jobs,
        },
    );
}

pub(super) fn wait(
    state: &mut State,
    source: &Source,
    recording: &Arc<Recording>,
    context: &egui::Context,
    jobs: &crate::background_jobs::BackgroundJobs,
    epoch: u64,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "timeline worker timed out");
        poll(state, source, recording, context, jobs, epoch);
        if !state.busy()
            && !state.catalog_pending
            && (state.requested.is_none()
                || state.window.prepared.is_some()
                || state.window.error.is_some())
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "pvd-timeline-{name}-{}-{}.pvdstream",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[test]
fn hidden_complete_window_releases_its_analysis_on_session_close() {
    let (source, recording) = memory(capture(3, "close-window"));
    let context = egui::Context::default();
    let mut studio = Studio::default();
    studio.persist_layout = false;
    studio.recording = Some(recording.clone());
    let jobs = studio.background_jobs.clone();
    let mut app = super::super::lifecycle_tests::preparation_app(studio, &context);
    app.update();
    let mut state = State::default();
    state.sync(0);
    wait(&mut state, &source, &recording, &context, &jobs, 0);
    let overview = state.overview.as_ref().unwrap();
    let request = WindowRequest {
        ticks: overview.range.unwrap(),
        thread: None,
        context: None,
        limits: WindowLimits::default(),
    };
    let cancel = AtomicBool::new(false);
    let report = source
        .thread_window(&overview.identity, request, &cancel, |_| true)
        .unwrap();
    let (loaded, analysis) = build_window(report, overview, &cancel).unwrap();
    let analysis = Arc::new(analysis);
    let weak = Arc::downgrade(&analysis);
    state.window.prepared = Some(Arc::new(refresh::Prepared {
        analysis: analysis.into(),
        value: Arc::new(loaded),
        frame: 0,
    }));
    context.data_mut(|data| data.insert_temp(egui::Id::new("hidden-complete-window"), state));
    drop((source, recording));
    let deadline = Instant::now() + Duration::from_secs(5);
    while jobs.active_count() != 0 {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    app.world_mut().resource_mut::<Studio>().close();
    assert!(weak.upgrade().is_some());
    app.update();
    assert!(weak.upgrade().is_none());
    assert!(
        context
            .data(|data| data.get_temp::<State>(egui::Id::new("hidden-complete-window")))
            .is_none()
    );
}

#[test]
fn relative_time_requests_keep_integer_origin_closed_endpoints_and_exact_context() {
    let first = (1u64 << 60) + 7;
    let overview = Overview {
        identity: Identity::Sdk(pvd_core::profile_curves::SourceIdentity {
            blake3: [1; 32],
            bytes: 0,
            frames: 0,
        }),
        clock: ClockRatio {
            numerator: 1000,
            denominator: 1,
        },
        range: Some(TickRange {
            start: first,
            end: first + 100_000,
        }),
        stats: GlobalPairingStats::default(),
        cuda_buffers: 0,
    };
    let mut form = Form {
        milliseconds: [1.0001, 2.0001],
        thread: Some(7),
        context: "0xfeed".into(),
    };
    let request = form.request(&overview).unwrap();
    assert_eq!(
        request.ticks,
        TickRange {
            start: first + 1000,
            end: first + 2001
        }
    );
    assert_eq!(request.context, Some(0xfeed));
    assert_eq!(request.thread, Some(7));
    assert_eq!(
        overview.milliseconds(TickRange {
            start: first + 1000,
            end: first + 2000
        }),
        [1.0, 2.0]
    );
    for range in [
        [f64::NAN, 2.0],
        [1.0, f64::INFINITY],
        [2.0, 1.0],
        [-1.0, 1.0],
        [0.0, 101.0],
    ] {
        form.milliseconds = range;
        assert!(form.request(&overview).is_err());
    }
    form.milliseconds = [0.0, 0.0];
    form.context = "18446744073709551616".into();
    assert!(form.request(&overview).is_err());
    let no_complete = Overview {
        range: None,
        ..overview
    };
    assert!(Form::default().request(&no_complete).is_err());
}

#[test]
fn complete_window_uses_its_own_analysis_and_preserves_original_source_coordinates() {
    let bytes = capture(2, "work");
    let (source, recording) = memory(bytes);
    let mut retained = (*recording).clone();
    let mut analysis = Analysis::default();
    analysis.first_retained_frame = 999;
    retained.analysis = analysis.into();
    let recording = Arc::new(retained);
    let revision = recording.analysis.revision();
    let context = egui::Context::default();
    let jobs = crate::background_jobs::BackgroundJobs::default();
    let mut state = State::default();
    state.sync(1);
    wait(&mut state, &source, &recording, &context, &jobs, 1);
    let overview = state.overview.as_ref().unwrap();
    assert_eq!(overview.stats.complete_spans, 2);
    assert!(
        state
            .catalog
            .prepared
            .as_ref()
            .unwrap()
            .analysis
            .profile_spans()
            .next()
            .is_none()
    );
    let mut request = state.form.request(overview).unwrap();
    request.ticks = TickRange {
        start: 130,
        end: 140,
    };
    request.thread = Some(8);
    request.context = Some(10);
    state.request_window(request);
    wait(&mut state, &source, &recording, &context, &jobs, 1);
    let loaded = state.window.prepared.as_ref().unwrap();
    assert_eq!(loaded.analysis.profile_spans().len(), 1);
    let span = loaded.analysis.profile_span(0).unwrap();
    assert_eq!(
        (
            span.start,
            span.end,
            span.thread,
            span.context,
            span.received_frame
        ),
        (130, 140, 8, 10, 1)
    );
    assert_eq!(loaded.value.layout.first, 110);
    assert_eq!(state.displayed, Some([20.0, 30.0]));
    assert_eq!(loaded.analysis.profile_event_name(10, 1).unwrap(), "work");
    assert!(recording.analysis.profile_spans().next().is_none());
    assert_eq!(recording.analysis.first_retained_frame(), 999);
    assert_eq!(recording.analysis.revision(), revision);
    let prepared = loaded.clone();
    state.displayed = Some([22.0, 25.0]);
    state.form.context = "0x9".into();
    for _ in 0..20 {
        poll(&mut state, &source, &recording, &context, &jobs, 1);
    }
    assert!(Arc::ptr_eq(
        &prepared,
        state.window.prepared.as_ref().unwrap()
    ));
    assert!(!state.busy());
    assert_eq!(state.displayed, Some([22.0, 25.0]));
}

#[test]
fn failed_large_window_can_be_narrowed_without_publishing_old_or_partial_results() {
    let (source, recording) = memory(capture(3, "work"));
    let context = egui::Context::default();
    let jobs = crate::background_jobs::BackgroundJobs::default();
    let mut state = State::default();
    state.sync(1);
    wait(&mut state, &source, &recording, &context, &jobs, 1);
    let mut query = state
        .form
        .request(state.overview.as_ref().unwrap())
        .unwrap();
    query.limits.spans = 1;
    state.request_window(query);
    wait(&mut state, &source, &recording, &context, &jobs, 1);
    assert!(state.window.prepared.is_none());
    assert!(
        state
            .window
            .error
            .as_ref()
            .unwrap()
            .text(Language::English)
            .contains("exceeds")
    );
    query.ticks = TickRange {
        start: 110,
        end: 120,
    };
    state.request_window(query);
    wait(&mut state, &source, &recording, &context, &jobs, 1);
    assert!(state.window.error.is_none());
    assert_eq!(
        state
            .window
            .prepared
            .as_ref()
            .unwrap()
            .analysis
            .profile_spans()
            .len(),
        1
    );
    query.ticks = TickRange {
        start: 150,
        end: 160,
    };
    state.request_window(query);
    assert!(state.window.prepared.is_none());
    assert!(state.displayed.is_none());
    wait(&mut state, &source, &recording, &context, &jobs, 1);
    assert_eq!(
        state
            .window
            .prepared
            .as_ref()
            .unwrap()
            .analysis
            .profile_span(0)
            .unwrap()
            .start,
        150
    );
}

#[test]
fn thread_source_guards_same_size_changes_for_both_catalog_and_window() {
    let bytes = capture(2, "work");
    let changed = capture(2, "draw");
    assert_eq!(bytes.len(), changed.len());
    let (source, recording) = memory(bytes.clone());
    let cancel = AtomicBool::new(false);
    let catalog = source.thread_page(None, 2, &cancel, |_| true).unwrap();
    let identity = crate::clip_store::Identity::from_digest(
        blake3::hash(&bytes),
        bytes.len() as u64,
        recording.frame_count(),
    );
    let path = scratch("identity");
    std::fs::write(&path, &changed).unwrap();
    let query = WindowRequest {
        ticks: catalog.completed_tick_range.unwrap(),
        thread: None,
        context: None,
        limits: WindowLimits::default(),
    };
    for source in [
        Source::memory(Arc::new(changed), identity.clone()),
        Source::file(path.clone(), identity),
    ] {
        assert!(source.thread_page(None, 2, &cancel, |_| true).is_err());
        assert!(
            source
                .thread_window(&catalog.identity, query, &cancel, |_| true)
                .is_err()
        );
    }
    std::fs::remove_file(path).unwrap();
}

#[test]
fn actual_thread_catalog_and_window_readers_cancel_on_close_shutdown_and_source_switch() {
    for kind in 0..3 {
        let window = kind != 0;
        for action in [0, 1, 2] {
            let bytes = capture(2, "work");
            let (normal, _) = memory(bytes.clone());
            let catalog = normal
                .thread_page(None, 2, &AtomicBool::new(false), |_| true)
                .unwrap();
            let (source, recording, started, release) = Source::gated_test_bytes(bytes);
            let context = egui::Context::default();
            let mut studio = Studio::default();
            studio.recording = Some(recording.clone());
            let jobs = studio.background_jobs.clone();
            let mut state = State::default();
            state.sync(1);
            if window {
                if kind == 2 {
                    state.set_read_mode(ReadMode::Streaming);
                }
                state.overview = Some(Overview {
                    identity: catalog.identity.clone(),
                    clock: catalog.clock,
                    range: catalog.completed_tick_range,
                    stats: catalog.global_pairing,
                    cuda_buffers: catalog.legacy_cuda_buffers,
                });
                state.form.around(state.overview.as_ref().unwrap(), None);
                state.request_window(
                    state
                        .form
                        .request(state.overview.as_ref().unwrap())
                        .unwrap(),
                );
            }
            poll(&mut state, &source, &recording, &context, &jobs, 1);
            started.recv_timeout(Duration::from_secs(5)).unwrap();
            let cancelled = state
                .catalog
                .job
                .as_ref()
                .map(|job| job.cancel.clone())
                .or_else(|| state.window.job.as_ref().map(|job| job.cancel.clone()))
                .unwrap();
            studio.profile_layout_cancel = Some(cancelled.clone());
            if action == 0 {
                studio.close();
                assert!(studio.recording.is_none());
            } else if action == 1 {
                assert!(!studio.poll_shutdown());
                assert!(studio.recording.is_none());
            } else {
                state.sync(2);
            }
            assert!(cancelled.load(Ordering::Relaxed));
            assert_eq!(jobs.active_count(), 1);
            release.send(()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while jobs.active_count() != 0 {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(state.window.prepared.is_none());
            if action == 1 {
                assert!(studio.poll_shutdown());
            }
            if action == 2 {
                let (next, recording) = memory(capture(2, "draw"));
                wait(&mut state, &next, &recording, &context, &jobs, 2);
                assert_ne!(state.overview.as_ref().unwrap().identity, catalog.identity);
                assert!(state.window.prepared.is_none());
            }
        }
    }
}

#[test]
fn full_thread_window_stays_visible_at_180px_with_the_application_fonts() {
    let path = scratch("layout");
    std::fs::write(&path, capture(2, "work")).unwrap();
    for language in [Language::English, Language::Chinese] {
        for width in [620.0, 400.0] {
            let context = egui::Context::default();
            crate::ui::configure(&context);
            let mut studio = Studio::default();
            studio.prefs.language = language;
            studio.open(path.clone(), context.clone());
            let deadline = Instant::now() + Duration::from_secs(10);
            while studio.job.is_some() {
                assert!(Instant::now() < deadline);
                studio.poll(&context);
                std::thread::sleep(Duration::from_millis(1));
            }
            let recording = studio.recording.as_ref().unwrap().clone();
            let original_revision = recording.analysis.revision();
            let id = std::cell::Cell::new(None);
            let draw = |events: Vec<egui::Event>, studio: &mut Studio| {
                context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 768.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |context| {
                        egui::TopBottomPanel::bottom("analysis")
                            .exact_height(180.0)
                            .show(context, |ui| {
                                id.set(Some(ui.id().with("complete-thread-profile")));
                                super::super::show(ui, studio);
                            });
                    },
                )
            };
            let mut output = draw(Vec::new(), &mut studio);
            loop {
                let ready = context.data(|data| {
                    data.get_temp::<State>(id.get().unwrap())
                        .unwrap()
                        .overview
                        .is_some()
                });
                if ready {
                    break;
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
                output = draw(Vec::new(), &mut studio);
            }
            let button = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text)
                        if text.galley.text() == language.text("Load window", "载入时间窗") =>
                    {
                        Some(egui::Rect::from_min_size(text.pos, text.galley.size()).center())
                    }
                    _ => None,
                })
                .unwrap();
            for pressed in [true, false] {
                output = draw(
                    vec![
                        egui::Event::PointerMoved(button),
                        egui::Event::PointerButton {
                            pos: button,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    &mut studio,
                );
            }
            loop {
                let state = context.data(|data| data.get_temp::<State>(id.get().unwrap()).unwrap());
                if let Some(error) = state.window.error {
                    panic!("{}", error.text(language));
                }
                if state.window.prepared.is_some() {
                    break;
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
                output = draw(Vec::new(), &mut studio);
            }
            let visible = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Rect(rect)
                        if rect.fill == egui::Color32::from_rgb(19, 27, 36) =>
                    {
                        Some(rect.rect.intersect(shape.clip_rect).height().max(0.0))
                    }
                    _ => None,
                })
                .max_by(f32::total_cmp)
                .unwrap_or(0.0);
            if visible < 10.0 {
                for shape in &output.shapes {
                    if let egui::epaint::Shape::Text(text) = &shape.shape {
                        println!(
                            "{:?} clip {:?}: {}",
                            text.pos,
                            shape.clip_rect,
                            text.galley.text()
                        );
                    }
                }
            }
            assert!(
                visible >= 10.0,
                "{language:?} {width}x180: visible track {visible}"
            );
            println!("{language:?} {width}x180: visible track {visible}px");
            assert_eq!(recording.analysis.revision(), original_revision);
            let deadline = Instant::now() + Duration::from_secs(5);
            while !studio.poll_shutdown() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
    std::fs::remove_file(path).unwrap();
}

#[test]
fn pending_only_source_shows_global_counts_without_a_fabricated_time_range() {
    let mut bytes = capture(1, "work");
    let mut stop = vec![2, 3, 1, 0];
    stop.extend(7u32.to_le_bytes());
    stop.extend([9, 0, 0]);
    stop.extend(120u64.to_le_bytes());
    let at = bytes
        .windows(stop.len())
        .position(|part| part == stop)
        .unwrap();
    bytes[at] = 1;
    let (source, recording) = memory(bytes);
    let context = egui::Context::default();
    crate::ui::configure(&context);
    let jobs = crate::background_jobs::BackgroundJobs::default();
    let mut state = State::default();
    state.sync(0);
    wait(&mut state, &source, &recording, &context, &jobs, 0);
    let overview = state.overview.as_ref().unwrap();
    assert!(overview.range.is_none());
    assert_eq!(overview.stats.open_starts, 2);
    assert!(state.form.request(overview).is_err());
    let mut studio = Studio::default();
    studio.prefs.language = Language::English;
    studio.recording = Some(recording);
    let output = context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(620.0, 180.0),
            )),
            ..Default::default()
        },
        |context| {
            egui::CentralPanel::default().show(context, |ui| {
                ui.ctx().data_mut(|data| {
                    data.insert_temp(ui.id().with("complete-thread-profile"), state.clone())
                });
                show(ui, &mut studio, source.clone(), true);
            });
        },
    );
    let text: Vec<_> = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) => Some(text.galley.text().to_owned()),
            _ => None,
        })
        .collect();
    assert!(
        text.iter()
            .any(|text| text.contains("No finished CPU spans"))
    );
    assert!(text.iter().any(|text| text.contains("2 open starts")));
    assert!(!text.iter().any(|text| text.contains("0–0")));
}

#[test]
#[ignore = "requires existing PhysX 3.4 profile fixtures; set PVD_PROFILE_STREAM_ROOT"]
fn sdk_thread_windows_match_the_in_memory_decoder_and_leave_recording_untouched() {
    let root = PathBuf::from(
        std::env::var_os("PVD_PROFILE_STREAM_ROOT").expect("PVD_PROFILE_STREAM_ROOT"),
    );
    let context = egui::Context::default();
    let jobs = crate::background_jobs::BackgroundJobs::default();
    let mut rows = Vec::new();
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
        let path = root.join("captures").join(name);
        let bytes = std::fs::read(&path).unwrap();
        let recording = Arc::new(Recording::from_bytes(&bytes).unwrap());
        let source = Source::file(
            path,
            crate::clip_store::Identity::from_digest(
                blake3::hash(&bytes),
                bytes.len() as u64,
                recording.frame_count(),
            ),
        );
        let revision = recording.analysis.revision();
        let mut state = State::default();
        state.sync(1);
        wait(&mut state, &source, &recording, &context, &jobs, 1);
        assert!(state.catalog.error.is_none());
        let overview = state.overview.as_ref().unwrap().clone();
        assert_eq!(
            overview.stats.complete_spans,
            recording.analysis.profile_spans().len() as u64
        );
        let first = recording.analysis.profile_spans().next().unwrap();
        let last = recording.analysis.profile_spans().next_back().unwrap();
        for (ticks, thread, filter_context) in [
            (overview.range.unwrap(), None, None),
            (
                TickRange {
                    start: first.start,
                    end: first.start,
                },
                Some(first.thread),
                Some(first.context),
            ),
            (
                TickRange {
                    start: last.start,
                    end: last.end,
                },
                Some(last.thread),
                Some(last.context),
            ),
        ] {
            let query = WindowRequest {
                ticks,
                thread,
                context: filter_context,
                limits: WindowLimits::default(),
            };
            state.request_window(query);
            wait(&mut state, &source, &recording, &context, &jobs, 1);
            assert!(
                state.window.error.is_none(),
                "{:?}",
                state
                    .window
                    .error
                    .as_ref()
                    .map(|error| error.text(Language::English))
            );
            let prepared = state.window.prepared.as_ref().unwrap();
            let expected: Vec<_> = recording
                .analysis
                .profile_spans()
                .filter(|span| {
                    span.end >= ticks.start
                        && span.start <= ticks.end
                        && thread.is_none_or(|id| id == span.thread)
                        && filter_context.is_none_or(|id| id == span.context)
                })
                .map(fields)
                .collect();
            let actual: Vec<_> = prepared.analysis.profile_spans().map(fields).collect();
            assert_eq!(actual, expected);
            assert_eq!(prepared.value.layout.first, overview.range.unwrap().start);
            assert_eq!(prepared.value.stats, overview.stats);
            assert_eq!(recording.analysis.revision(), revision);
            rows.push(serde_json::json!({"capture":name,"bytes":bytes.len(),"frames":recording.frame_count(),"blake3":blake3::hash(&bytes).to_hex().to_string(),"start":ticks.start,"end":ticks.end,"thread":thread,"context":filter_context,"complete_spans":actual.len(),"estimated_index_bytes":prepared.value.index_bytes}));
        }
    }
    if let Some(path) = std::env::var_os("PVD_THREAD_TIMELINE_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&rows).unwrap()).unwrap();
    }
    println!(
        "{} actual SDK file windows matched the memory decoder",
        rows.len()
    );
}

#[test]
fn render_request_waits_for_the_production_window_and_never_reuses_an_old_epoch() {
    let path = scratch("render");
    std::fs::write(&path, capture(2, "work")).unwrap();
    let context = egui::Context::default();
    crate::ui::configure(&context);
    let mut studio = Studio::default();
    studio.open(path.clone(), context.clone());
    let deadline = Instant::now() + Duration::from_secs(10);
    while studio.job.is_some() {
        assert!(Instant::now() < deadline);
        studio.poll(&context);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!super::super::prepare_thread_window_render(&context, &studio).unwrap());
    let id = std::cell::Cell::new(None);
    loop {
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(620.0, 180.0),
                )),
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    id.set(Some(ui.id().with("complete-thread-profile")));
                    super::super::show(ui, &mut studio);
                });
            },
        );
        if super::super::prepare_thread_window_render(&context, &studio).unwrap() {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let state = context.data(|data| data.get_temp::<State>(id.get().unwrap()).unwrap());
    let loaded = state.window.prepared.unwrap();
    assert_eq!(loaded.analysis.profile_spans().len(), 2);
    assert_eq!(
        loaded.value.request.ticks,
        TickRange {
            start: 110,
            end: 140
        }
    );
    assert_eq!(loaded.value.request.thread, None);
    assert_eq!(loaded.value.request.context, None);
    studio.profile_view_epoch = studio.profile_view_epoch.wrapping_add(1);
    assert!(!super::super::prepare_thread_window_render(&context, &studio).unwrap());
    studio.close();
    assert!(super::super::prepare_thread_window_render(&context, &studio).is_err());
    let deadline = Instant::now() + Duration::from_secs(5);
    while !studio.poll_shutdown() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    std::fs::remove_file(path).unwrap();
}

#[test]
fn choosing_streaming_keeps_catalog_work_and_cancels_an_old_window() {
    let bytes = capture(2, "work");
    let (source, recording, entered, release) = Source::gated_test_bytes(bytes);
    let context = egui::Context::default();
    let jobs = crate::background_jobs::BackgroundJobs::default();
    let mut state = State::default();
    state.sync(1);
    poll(&mut state, &source, &recording, &context, &jobs, 1);
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    let catalog_cancel = state.catalog.job.as_ref().unwrap().cancel.clone();
    state.set_read_mode(ReadMode::Streaming);
    assert!(!catalog_cancel.load(Ordering::Relaxed));
    release.send(()).unwrap();
    wait(&mut state, &source, &recording, &context, &jobs, 1);
    let overview = state.overview.as_ref().unwrap().clone();
    let (window_source, _, entered, release) = Source::gated_test_bytes(capture(2, "work"));
    state.request_window(state.form.request(&overview).unwrap());
    poll(&mut state, &window_source, &recording, &context, &jobs, 1);
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    let window_cancel = state.window.job.as_ref().unwrap().cancel.clone();
    state.set_read_mode(ReadMode::Indexed);
    assert!(window_cancel.load(Ordering::Relaxed));
    assert!(state.window.job.is_none());
    assert!(state.window.prepared.is_none());
    assert!(state.requested.is_none());
    assert!(state.overview.is_some());
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while jobs.active_count() != 0 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    poll(&mut state, &source, &recording, &context, &jobs, 1);
    assert!(state.window.prepared.is_none());
    state.sync(2);
    assert!(state.read_mode == ReadMode::Indexed);
}

#[cfg(windows)]
#[test]
fn file_scan_choice_and_error_recovery_keep_narrow_tracks_visible() {
    use crate::full_profile::IndexSlot;
    use pvd_core::profile_disk_index::IndexLimits;
    for language in [Language::English, Language::Chinese] {
        for (width, preselect) in [(620.0, true), (400.0, false)] {
            let root = scratch("stream-choice").with_extension("");
            std::fs::create_dir(&root).unwrap();
            let path = root.join("source.pvdstream");
            std::fs::write(&path, capture(2, "work")).unwrap();
            let context = egui::Context::default();
            crate::ui::configure(&context);
            let mut studio = Studio::default();
            studio.prefs.language = language;
            studio.open(path.clone(), context.clone());
            let deadline = Instant::now() + Duration::from_secs(10);
            while studio.job.is_some() {
                assert!(Instant::now() < deadline);
                studio.poll(&context);
                std::thread::sleep(Duration::from_millis(1));
            }
            let index = Arc::new(IndexSlot::with_test_limits(
                root.join("cache"),
                IndexLimits {
                    disk_bytes: 256,
                    ..Default::default()
                },
                &studio.background_jobs,
            ));
            let source = studio.complete_profile_source().unwrap();
            assert!(source.can_rebuild_cpu_index());
            let source = Source::from(source.with_cpu_index(path.clone(), index.clone()));
            let id = std::cell::Cell::new(None);
            let draw = |events, studio: &mut Studio| {
                context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 768.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |context| {
                        egui::TopBottomPanel::bottom("analysis")
                            .exact_height(180.0)
                            .show(context, |ui| {
                                id.set(Some(ui.id().with("complete-thread-profile")));
                                show(ui, studio, source.clone(), true);
                            });
                    },
                )
            };
            let label_position = |output: &egui::FullOutput, label: &str| {
                output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                            let rect = egui::Rect::from_min_size(text.pos, text.galley.size())
                                .intersect(shape.clip_rect);
                            (rect.is_positive()).then(|| rect.center())
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| panic!("visible label missing: {label}"))
            };
            let click = |position, studio: &mut Studio| {
                let mut result = None;
                for pressed in [true, false] {
                    result = Some(draw(
                        vec![
                            egui::Event::PointerMoved(position),
                            egui::Event::PointerButton {
                                pos: position,
                                button: egui::PointerButton::Primary,
                                pressed,
                                modifiers: egui::Modifiers::NONE,
                            },
                        ],
                        studio,
                    ));
                }
                result.unwrap()
            };
            let mut output = draw(Vec::new(), &mut studio);
            loop {
                let state = context.data(|data| data.get_temp::<State>(id.get().unwrap()).unwrap());
                if state.overview.is_some() {
                    break;
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
                output = draw(Vec::new(), &mut studio);
            }
            let held = if preselect {
                Some(crate::profile_cache::Lease::acquire(&root.join("cache"), || false).unwrap())
            } else {
                None
            };
            if preselect {
                let menu = label_position(&output, language.text("Window…", "时间窗…"));
                click(menu, &mut studio);
                output = draw(Vec::new(), &mut studio);
                let radio = label_position(
                    &output,
                    language.text("Scan file without index", "直接扫描（不建索引）"),
                );
                click(radio, &mut studio);
                let state = context.data(|data| data.get_temp::<State>(id.get().unwrap()).unwrap());
                assert!(state.read_mode == ReadMode::Streaming);
                click(egui::pos2(width - 5.0, 5.0), &mut studio);
                output = draw(Vec::new(), &mut studio);
            }
            click(
                label_position(&output, language.text("Load window", "载入时间窗")),
                &mut studio,
            );
            loop {
                output = draw(Vec::new(), &mut studio);
                let state = context.data(|data| data.get_temp::<State>(id.get().unwrap()).unwrap());
                if state.window.prepared.is_some() || state.window.error.is_some() {
                    break;
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
            if !preselect {
                let state = context.data(|data| data.get_temp::<State>(id.get().unwrap()).unwrap());
                assert!(state.window.prepared.is_none());
                assert!(
                    state
                        .window
                        .error
                        .as_ref()
                        .unwrap()
                        .text(Language::English)
                        .contains("budget")
                );
                click(
                    label_position(&output, language.text("Scan file", "直接扫描")),
                    &mut studio,
                );
            }
            loop {
                output = draw(Vec::new(), &mut studio);
                let state = context.data(|data| data.get_temp::<State>(id.get().unwrap()).unwrap());
                assert!(state.window.error.is_none());
                if let Some(prepared) = state.window.prepared {
                    assert!(state.read_mode == ReadMode::Streaming);
                    assert_eq!(prepared.analysis.profile_spans().len(), 2);
                    assert!(matches!(
                        state.progress.lock().unwrap().as_ref(),
                        Some(TimelineProgress::Streaming(_))
                    ));
                    break;
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
            let visible = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Rect(rect)
                        if rect.fill == egui::Color32::from_rgb(19, 27, 36) =>
                    {
                        Some(rect.rect.intersect(shape.clip_rect).height().max(0.0))
                    }
                    _ => None,
                })
                .max_by(f32::total_cmp)
                .unwrap_or(0.0);
            assert!(
                visible >= 14.0,
                "{language:?} {width}x180 visible tracks: {visible}"
            );
            if let Some(lease) = &held {
                assert!(!lease.workspace().exists());
            }
            drop(held);
            index.close();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !studio.poll_shutdown() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(
                std::fs::read_dir(root.join("cache"))
                    .unwrap()
                    .filter_map(Result::ok)
                    .filter(|entry| entry.file_type().unwrap().is_dir())
                    .count(),
                0
            );
            std::fs::remove_file(root.join("cache/.pvd-studio-profile-lock")).unwrap();
            std::fs::remove_dir(root.join("cache")).unwrap();
            std::fs::remove_file(&path).unwrap();
            std::fs::remove_dir(root).unwrap();
        }
    }
}
