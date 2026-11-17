use super::*;
use pvd_core::analysis::view::MemorySource;

#[path = "../../../../../test-support/retained_capture_fixture.rs"]
mod fixture;
mod reexport_tests;
mod sdk;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Case {
    studio: Studio,
    live: Option<LiveRecording>,
    root: PathBuf,
    context: egui::Context,
}

impl Case {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "pvd-retained-entry-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let mut live = LiveRecording::with_history(pvd_core::live::HistoryConfig {
            directory: root.join("history"),
            disk_budget_bytes: 32 << 20,
            segment_target_bytes: 64 << 20,
            segment_frames: 2,
            min_free_disk_bytes: 0,
        })
        .unwrap();
        live.feed(&fixture::capture(9)).unwrap();
        let recording = Arc::new(live.snapshot().unwrap());
        let mut case = Self {
            studio: Studio::default(),
            live: Some(live),
            root,
            context: Default::default(),
        };
        case.studio.recording = Some(recording.clone());
        case.studio.live_recording = Some(recording);
        case.studio.live_connected = true;
        case.studio.live_frame = 8;
        case.studio.retained_first = 3;
        case.studio.retained_last = 6;
        case.studio.retained_path = case.root.join("retained.pvdcapture").display().to_string();
        case
    }

    fn wait(&mut self, mut done: impl FnMut(&Studio) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            self.studio.poll(&self.context);
            if done(&self.studio) {
                return;
            }
            assert!(Instant::now() < deadline, "worker timeout");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        self.studio.clear_recording();
        self.live = None;
        let deadline = Instant::now() + Duration::from_secs(20);
        while !self.studio.poll_shutdown() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        fn files(path: &Path) -> usize {
            if !path.exists() {
                return 0;
            }
            std::fs::read_dir(path)
                .unwrap()
                .map(|entry| {
                    let path = entry.unwrap().path();
                    if path.is_dir() { files(&path) } else { 1 }
                })
                .sum()
        }
        assert_eq!(
            files(&self.root.join("history")),
            0,
            "history payload leaked"
        );
        assert!(std::fs::read_dir(&self.root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".pvd-capture-")
        }));
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn timeline_save_button_preserves_tcp_history_without_a_complete_raw_stream() {
    for language in [Language::Chinese, Language::English] {
        for connected in [true, false] {
            let mut case = Case::new();
            let output = case.root.join(&Studio::default().retained_path);
            assert!(!output.parent().unwrap().exists());
            case.studio.retained_path = output.display().to_string();
            case.studio.prefs.language = language;
            case.studio.live_connected = connected;
            case.studio.follow_live = connected;
            case.studio.frame = 4;
            if !connected {
                let partial = case.root.join("interrupted.pvdstream.partial");
                std::fs::write(&partial, &fixture::capture(9)[..29]).unwrap();
                case.studio.capture_path = Some(partial);
            }
            assert!(!case.studio.can_save_capture());
            assert!(case.studio.can_save_recording());
            crate::ui::configure(&case.context);
            let draw = |case: &mut Case, events| {
                let context = case.context.clone();
                context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(620.0, 220.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |context| {
                        egui::CentralPanel::default().show(context, |ui| {
                            crate::ui::recording_save_controls(ui, &mut case.studio);
                        });
                    },
                )
            };
            let _ = draw(&mut case, vec![]);
            let output = draw(&mut case, vec![]);
            let point = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text)
                        if text.galley.text() == language.text("Save capture", "保存录制") =>
                    {
                        Some(text.galley.rect.translate(text.pos.to_vec2()).center())
                    }
                    _ => None,
                })
                .expect("primary save button is visible");
            let _ = draw(&mut case, vec![egui::Event::PointerMoved(point)]);
            for pressed in [true, false] {
                let _ = draw(
                    &mut case,
                    vec![egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    }],
                );
            }
            assert!(case.studio.recording_export.is_some());
            assert_eq!(
                (case.studio.retained_first, case.studio.retained_last),
                (0, 8)
            );
            assert_eq!(case.studio.frame, 4);
            assert_eq!(case.studio.live_connected, connected);
            assert_eq!(case.studio.follow_live, connected);
            case.wait(|studio| studio.recording_export.is_none());
            let path = PathBuf::from(&case.studio.retained_path);
            assert!(path.exists(), "{:?}", case.studio.notices);
            case.studio.clear_recording();
            case.live = None;
            case.studio.open(path, case.context.clone());
            case.wait(|studio| studio.job.is_none() && studio.seeking.is_none());
            assert!(case.studio.is_project_capture());
            assert_eq!((case.studio.first_frame(), case.studio.count()), (0, 9));
            let expected = Recording::from_bytes(&fixture::capture(9)).unwrap();
            let actual = case.studio.recording.as_ref().unwrap();
            for frame in [8, 0, 4] {
                assert_eq!(
                    actual.state_at(frame).unwrap(),
                    expected.state_at(frame).unwrap()
                );
            }
            actual.verify_replay().unwrap();
        }
    }
}

#[test]
fn primary_save_keeps_sdk_bytes_for_memory_sources_and_rejects_native_containers() {
    let mut case = Case::new();
    let bytes = fixture::capture(9);
    case.studio.live_recording = None;
    case.studio.recording = Some(Arc::new(Recording::from_bytes(&bytes).unwrap()));
    case.studio.bytes = Some(Arc::new(bytes.clone()));
    case.studio.live_connected = false;
    let output = case.root.join(&case.studio.export_path);
    assert!(!output.parent().unwrap().exists());
    case.studio.export_path = output.display().to_string();
    assert!(!case.studio.recording_saves_history());
    assert!(case.studio.can_save_recording());
    case.studio.save_recording(case.context.clone());
    case.wait(|studio| studio.save_job.is_none());
    assert_eq!(std::fs::read(&case.studio.export_path).unwrap(), bytes);
    Arc::make_mut(case.studio.recording.as_mut().unwrap()).source =
        pvd_core::RecordingSource::Pvd3Database;
    assert!(!case.studio.can_save_capture());
    assert!(!case.studio.can_save_recording());
}

#[test]
fn tcp_history_without_raw_exports_and_real_open_preserves_absolute_range() {
    let mut case = Case::new();
    assert!(case.studio.bytes.is_none() && case.studio.capture_path.is_none());
    assert!(case.studio.can_export_retained());
    case.studio.frame = 4;
    case.studio.export_retained(case.context.clone());
    assert_eq!(case.studio.frame, 4);
    case.wait(|studio| studio.recording_export.is_none());
    let path = PathBuf::from(&case.studio.retained_path);
    assert!(path.exists(), "{:?}", case.studio.notices);
    case.studio.clear_recording();
    case.live = None;
    std::fs::write(crate::clip_store::sidecar_path(&path), b"invalid sidecar").unwrap();
    case.studio.open(path.clone(), case.context.clone());
    case.wait(|studio| studio.job.is_none() && studio.seeking.is_none());
    assert!(case.studio.is_project_capture());
    assert_eq!(
        (
            case.studio.first_frame(),
            case.studio.count(),
            case.studio.frame
        ),
        (3, 7, 3)
    );
    assert!(case.studio.bytes.is_none() && case.studio.source_identity.is_none());
    assert_eq!(case.studio.capture_path.as_ref(), Some(&path));
    assert!(case.studio.complete_profile_source().is_none());
    assert!(!case.studio.can_save_capture());
    assert!(!case.studio.can_trim_capture());
    assert!(!case.studio.can_export_analysis());
    assert!(case.studio.can_export_retained());
    for frame in [3, 6, 4, 5, 3] {
        case.studio.seek(frame);
        case.wait(|studio| studio.seeking.is_none());
        let expected = Recording::from_bytes(&fixture::capture(frame + 1)).unwrap();
        assert_eq!(case.studio.state, expected.state_at(frame).unwrap());
        let actual = case
            .studio
            .recording
            .as_ref()
            .unwrap()
            .analysis_at(frame)
            .unwrap();
        assert_eq!(
            postcard::to_allocvec(&actual).unwrap(),
            postcard::to_allocvec(&expected.analysis).unwrap()
        );
        assert!(actual.memory_snapshot().sample_count() > 0);
        assert!(actual.profile_summary().complete_spans > 0);
    }
    assert!(case.studio.recording.as_ref().unwrap().state_at(2).is_err());
}

#[test]
fn stale_epoch_and_cancel_do_not_publish_and_shutdown_waits_for_export() {
    let mut case = Case::new();
    let old_epoch = case.studio.capture_epoch.load(Ordering::Relaxed);
    let recording = case.studio.recording.clone().unwrap();
    case.studio.clear_recording();
    let path = case.root.join("stale.pvdcapture");
    let job = crate::recording_export::Job::start(
        crate::recording_export::Source::Retained {
            recording,
            epoch: case.studio.capture_epoch.clone(),
            expected_epoch: old_epoch,
        },
        path.clone(),
        crate::recording_export::Kind::Retained { first: 3, last: 6 },
        case.context.clone(),
        &case.studio.export_workers,
    )
    .unwrap();
    case.studio.recording_export = Some(job);
    case.wait(|studio| studio.recording_export.is_none());
    assert!(!path.exists());
    let cancel = Arc::new(AtomicBool::new(false));
    let permit = case.studio.export_workers.acquire(cancel.clone()).unwrap();
    assert!(!case.studio.poll_shutdown());
    assert!(cancel.load(Ordering::Relaxed));
    drop(permit);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !case.studio.poll_shutdown() || case.studio.background_jobs.active_count() != 0 {
        assert!(
            Instant::now() < deadline,
            "shutdown did not finish after export cancellation"
        );
        std::thread::yield_now();
    }
}

#[test]
fn requested_expired_range_is_not_silently_shifted_and_destination_is_not_overwritten() {
    let mut case = Case::new();
    case.studio.retained_last = 10;
    case.studio.export_retained(case.context.clone());
    assert!(case.studio.recording_export.is_none());
    assert_eq!(case.studio.retained_last, 10);
    case.studio.use_retained_range();
    let path = PathBuf::from(&case.studio.retained_path);
    std::fs::write(&path, b"existing").unwrap();
    case.studio.export_retained(case.context.clone());
    case.wait(|studio| studio.recording_export.is_none());
    assert_eq!(std::fs::read(path).unwrap(), b"existing");
}
