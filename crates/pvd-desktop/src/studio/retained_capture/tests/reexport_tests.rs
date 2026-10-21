use super::*;

fn open_live_export(case: &mut Case) -> PathBuf {
    case.studio.export_retained(case.context.clone());
    case.wait(|studio| {
        studio.recording_export.is_none() && studio.export_workers.cancel_all_and_check_finished()
    });
    let path = PathBuf::from(&case.studio.retained_path);
    assert!(path.exists(), "{:?}", case.studio.notices);
    case.studio.clear_recording();
    case.live = None;
    case.studio.open(path.clone(), case.context.clone());
    case.wait(|studio| studio.job.is_none() && studio.seeking.is_none());
    assert!(case.studio.is_project_capture());
    assert!(!case.studio.live_connected);
    path
}

pub(super) fn remove_closed_source(case: &mut Case, path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match std::fs::remove_file(path) {
            Ok(()) => return,
            Err(error) if cfg!(windows) && error.raw_os_error() == Some(32) => {
                assert!(Instant::now() < deadline, "source is still open: {error}");
                case.studio.poll(&case.context);
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(error) => panic!("cannot remove closed source: {error}"),
        }
    }
}

#[test]
fn project_capture_worker_can_save_again_and_reopen_after_its_source_is_deleted() {
    let mut case = Case::new();
    let mut source = open_live_export(&mut case);
    for (number, first, last) in [(0, 4, 6), (1, 5, 5)] {
        assert!(case.studio.can_export_retained());
        assert!(!case.studio.can_save_capture());
        assert!(!case.studio.can_trim_capture());
        assert!(!case.studio.can_export_analysis());
        let source_bytes = std::fs::read(&source).unwrap();
        let destination = case.root.join(format!("再次裁剪-{number}.pvdcapture"));
        case.studio.retained_path = destination.display().to_string();
        case.studio.retained_first = first;
        case.studio.retained_last = last;
        let before = case.studio.frame;
        case.studio.export_retained(case.context.clone());
        case.wait(|studio| {
            studio.recording_export.is_none()
                && studio.export_workers.cancel_all_and_check_finished()
        });
        assert!(destination.exists(), "{:?}", case.studio.notices);
        assert_eq!(case.studio.frame, before);
        assert_eq!(std::fs::read(&source).unwrap(), source_bytes);
        case.studio.clear_recording();
        remove_closed_source(&mut case, &source);
        case.studio.open(destination.clone(), case.context.clone());
        case.wait(|studio| studio.job.is_none() && studio.seeking.is_none());
        assert!(case.studio.is_project_capture());
        assert_eq!(case.studio.first_frame(), first);
        assert_eq!(case.studio.count(), last + 1);
        assert_eq!(case.studio.frame, first);
        for frame in (first..=last).rev().chain(first..=last) {
            case.studio.seek(frame);
            case.wait(|studio| studio.seeking.is_none());
            let expected = Recording::from_bytes(&fixture::capture(frame + 1)).unwrap();
            assert_eq!(case.studio.frame, frame);
            assert_eq!(case.studio.state, expected.state_at(frame).unwrap());
            assert!(case.studio.analysis_ready());
            assert_eq!(
                postcard::to_allocvec(&case.studio.recording.as_ref().unwrap().analysis).unwrap(),
                postcard::to_allocvec(&expected.analysis).unwrap()
            );
        }
        assert!(
            case.studio
                .recording
                .as_ref()
                .unwrap()
                .state_at(first - 1)
                .is_err()
        );
        let hidden = case.root.join(format!("hidden-{number}.pvdcapture"));
        case.studio.retained_first = first - 1;
        case.studio.retained_last = last;
        case.studio.retained_path = hidden.display().to_string();
        case.studio.export_retained(case.context.clone());
        assert!(case.studio.recording_export.is_none());
        assert!(!hidden.exists());
        source = destination;
    }
}

#[test]
fn changing_project_capture_session_cancels_its_pending_reexport() {
    let mut case = Case::new();
    let original = open_live_export(&mut case);
    let original_bytes = std::fs::read(&original).unwrap();
    let recording = case.studio.recording.clone().unwrap();
    let expected_epoch = case.studio.capture_epoch.load(Ordering::Relaxed);
    case.studio.clear_recording();
    let destination = case.root.join("stale-project.pvdcapture");
    case.studio.recording_export = Some(
        crate::recording_export::Job::start(
            crate::recording_export::Source::Retained {
                recording,
                epoch: case.studio.capture_epoch.clone(),
                expected_epoch,
            },
            destination.clone(),
            crate::recording_export::Kind::Retained { first: 4, last: 5 },
            case.context.clone(),
            &case.studio.export_workers,
        )
        .unwrap(),
    );
    case.wait(|studio| {
        studio.recording_export.is_none() && studio.export_workers.cancel_all_and_check_finished()
    });
    assert!(!destination.exists());
    assert_eq!(std::fs::read(original).unwrap(), original_bytes);
}

fn visible_text(output: &egui::FullOutput, viewport: egui::Rect, expected: &str) -> bool {
    output.shapes.iter().any(|shape| match &shape.shape {
        egui::epaint::Shape::Text(text) if text.galley.text() == expected => {
            let rect = egui::Rect::from_min_size(text.pos, text.galley.size());
            let visible = rect.intersect(shape.clip_rect).intersect(viewport);
            visible.height() >= rect.height() - 0.5 && visible.width() >= rect.width() - 0.5
        }
        _ => false,
    })
}

#[test]
fn retained_render_form_uses_visible_absolute_range_and_application_fonts_in_both_languages() {
    let mut case = Case::new();
    open_live_export(&mut case);
    for language in [Language::English, Language::Chinese] {
        for (width, height) in [
            (620.0, 180.0),
            (400.0, 180.0),
            (620.0, 440.0),
            (400.0, 440.0),
        ] {
            let context = egui::Context::default();
            crate::ui::configure(&context);
            case.studio.prefs.language = language;
            case.studio.retained_first = 0;
            case.studio.retained_last = usize::MAX;
            case.studio.retained_path = "copy.pvdcapture".into();
            crate::recording_export::prepare_retained_render(&context, &mut case.studio).unwrap();
            assert_eq!(
                (case.studio.retained_first, case.studio.retained_last),
                (3, 6)
            );
            assert!(!case.studio.live_connected);
            let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height));
            let mut visible = false;
            for frame in 0..32 {
                let output = context.run(
                    egui::RawInput {
                        screen_rect: Some(viewport),
                        time: Some(f64::from(frame) / 30.0),
                        ..Default::default()
                    },
                    |context| {
                        egui::CentralPanel::default().show(context, |ui| {
                            crate::clips::show(ui, &mut case.studio);
                        });
                    },
                );
                visible = [
                    language.text("First", "首帧"),
                    language.text("Last", "末帧"),
                    language.text("Use available range", "使用可用范围"),
                    language.text("Choose path…", "选择路径…"),
                    language.text("Save retained capture", "保存保留捕获"),
                    "copy.pvdcapture",
                    "3",
                    "6",
                ]
                .into_iter()
                .all(|text| visible_text(&output, viewport, text));
                if visible {
                    break;
                }
            }
            assert!(visible, "{language:?}, width={width}, height={height}");
            case.studio.retained_first = 5;
            case.studio.retained_last = 5;
            crate::recording_export::prepare_retained_render(&context, &mut case.studio).unwrap();
            assert_eq!(
                (case.studio.retained_first, case.studio.retained_last),
                (5, 5)
            );
        }
    }
}

#[test]
fn short_clips_panel_scrolls_to_retained_help_and_back_without_editing_the_capture() {
    let mut case = Case::new();
    open_live_export(&mut case);
    for language in [Language::English, Language::Chinese] {
        for width in [620.0, 400.0] {
            let context = egui::Context::default();
            crate::ui::configure(&context);
            case.studio.prefs.language = language;
            case.studio.retained_path = "copy.pvdcapture".into();
            crate::recording_export::prepare_retained_render(&context, &mut case.studio).unwrap();
            let before = (
                case.studio.frame,
                case.studio.retained_first,
                case.studio.retained_last,
            );
            let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 96.0));
            let mut frame = 0;
            let mut draw = |studio: &mut Studio, delta: f32| {
                let mut events = vec![egui::Event::PointerMoved(viewport.center())];
                if delta != 0.0 {
                    events.push(egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(0.0, delta),
                        modifiers: egui::Modifiers::NONE,
                    });
                }
                frame += 1;
                context.run(
                    egui::RawInput {
                        screen_rect: Some(viewport),
                        time: Some(f64::from(frame) / 30.0),
                        focused: true,
                        events,
                        ..Default::default()
                    },
                    |context| {
                        egui::CentralPanel::default().show(context, |ui| {
                            crate::clips::show(ui, studio);
                        });
                    },
                )
            };
            for _ in 0..32 {
                let _ = draw(&mut case.studio, 0.0);
            }
            let notes = language.text(
                "The destination must be new. If history expires during saving, the export fails without stopping reception.",
                "目标文件必须不存在。若保存期间历史过期，导出会失败，接收不会停止。",
            );
            let definitions = language.text(
                "Definition editing requires a complete file capture and a stopped listener.",
                "编辑片段定义需要已完整打开的录制文件，且监听已停止。",
            );
            let save = language.text("Save retained capture", "保存保留捕获");
            let initial = draw(&mut case.studio, 0.0);
            assert!(!visible_text(&initial, viewport, notes));
            let mut seen = [false; 3];
            for _ in 0..100 {
                let output = draw(&mut case.studio, -8.0);
                for (index, label) in [save, notes, definitions].into_iter().enumerate() {
                    seen[index] |= visible_text(&output, viewport, label);
                }
                if seen.into_iter().all(|visible| visible) {
                    break;
                }
            }
            assert!(
                seen.into_iter().all(|visible| visible),
                "{language:?}, width={width}: {seen:?}"
            );
            let mut returned = false;
            for _ in 0..100 {
                let output = draw(&mut case.studio, 8.0);
                returned = visible_text(
                    &output,
                    viewport,
                    language.text("Save retained history", "保存保留历史"),
                ) && visible_text(&output, viewport, language.text("First", "首帧"));
                if returned {
                    break;
                }
            }
            assert!(returned, "{language:?}, width={width}");
            assert_eq!(
                (
                    case.studio.frame,
                    case.studio.retained_first,
                    case.studio.retained_last
                ),
                before
            );
            assert_eq!(case.studio.retained_path, "copy.pvdcapture");
            assert!(case.studio.recording_export.is_none());
            assert!(!case.studio.file_picker.is_busy());
        }
    }
}

#[test]
fn retained_render_does_not_change_the_offline_default_or_accept_an_empty_source() {
    let mut empty = Studio::default();
    assert!(
        crate::recording_export::prepare_retained_render(&egui::Context::default(), &mut empty)
            .is_err()
    );
    let mut case = Case::new();
    open_live_export(&mut case);
    let context = egui::Context::default();
    crate::ui::configure(&context);
    let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(620.0, 440.0));
    for frame in 0..4 {
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(viewport),
                time: Some(f64::from(frame) / 30.0),
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    crate::recording_export::retained_controls(ui, &mut case.studio);
                });
            },
        );
        assert!(!visible_text(
            &output,
            viewport,
            case.studio
                .prefs
                .language
                .text("Save retained capture", "保存保留捕获")
        ));
    }
    assert!(!case.studio.live_connected);
}
