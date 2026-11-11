use super::*;
use crate::curve::{Data, Source, Spec};
use crate::i18n::Message;
use pvd_protocol::{Endian, Event, EventGroup, Header};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct WorkDir(PathBuf);

impl WorkDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "pvd-curve-context-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        for file in ["first.csv", "selected.csv"] {
            let _ = fs::remove_file(self.0.join(file));
        }
        let _ = fs::remove_dir(&self.0);
    }
}

#[derive(Debug, PartialEq)]
struct PlotState {
    frame: usize,
    selected: Option<u64>,
    object: Option<u64>,
    playing: i32,
    follow_live: bool,
    bounds: Option<Bounds>,
    auto_fit: bool,
    box_zoom: bool,
    box_drag: Option<RectangleDrag>,
    markers: Vec<(u64, usize)>,
}

struct ChartUi {
    context: egui::Context,
    studio: Studio,
    graph_id: egui::Id,
    canvas: egui::Rect,
    time: f64,
    focused: bool,
}

impl ChartUi {
    fn new(language: Language) -> Self {
        let context = egui::Context::default();
        crate::ui::configure(&context);
        let mut builder = pvd_core::RecordingBuilder::new(Header {
            version: 1,
            timestamp_numerator: 1,
            timestamp_denominator: 1,
            flags: 1,
            endian: Endian::Little,
        });
        builder
            .push(EventGroup {
                stream_id: 1,
                timestamp: 0,
                offset: 0,
                events: std::iter::once(Event::StringHandle {
                    handle: 1,
                    value: "frame".into(),
                })
                .chain((0..4).map(|timestamp| Event::EndSection {
                    id: 1,
                    name: 1,
                    timestamp,
                }))
                .collect(),
            })
            .unwrap();
        let mut studio = Studio::default();
        studio.prefs.language = language;
        studio.recording = Some(Arc::new(builder.finish().unwrap()));
        studio.seek(1);
        for (object, property, values) in [
            (1, "First", [Some(101.0), Some(102.0), None, Some(104.0)]),
            (2, "Second", [Some(201.0), Some(202.0), None, Some(204.0)]),
        ] {
            let id = studio
                .curves
                .add(Spec {
                    source: Source::Property,
                    object,
                    generation: 0,
                    property: property.into(),
                    component: 0,
                    component_name: "value".into(),
                })
                .unwrap();
            let curve = studio
                .curves
                .curves
                .iter_mut()
                .find(|curve| curve.id == id)
                .unwrap();
            curve.pending = false;
            curve.data = Some(Data {
                samples: values.into_iter().enumerate().collect(),
                stride: 1,
                source_frames: 4,
                reference_frame_count: None,
                statuses: vec![],
                occurrence_counts: vec![],
                diagnostics: None,
            });
        }
        studio.curves.pending.clear();
        studio.select_curve(2);
        let mut harness = Self {
            context,
            studio,
            graph_id: egui::Id::NULL,
            canvas: egui::Rect::NOTHING,
            time: 0.0,
            focused: true,
        };
        let _ = harness.frame(vec![], egui::Modifiers::NONE);
        let _ = harness.frame(vec![], egui::Modifiers::NONE);
        harness
    }

    fn frame(&mut self, events: Vec<egui::Event>, modifiers: egui::Modifiers) -> egui::FullOutput {
        self.time += 0.05;
        let context = self.context.clone();
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 620.0),
                )),
                events,
                modifiers,
                time: Some(self.time),
                focused: self.focused,
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    self.graph_id = ui.id().with("curve-graphs");
                    show_content(ui, &mut self.studio);
                });
            },
        );
        self.canvas = self
            .context
            .read_response(self.graph_id.with("canvas"))
            .unwrap()
            .rect;
        self.studio.apply_ui_actions(&self.context);
        output
    }

    fn state(&self) -> PlotState {
        let views = self
            .context
            .data(|data| data.get_temp::<Views>(self.graph_id))
            .unwrap();
        let page = self.studio.curves.pages[self.studio.curves.active_page].id;
        let view = &views.pages[&page];
        PlotState {
            frame: self.studio.frame,
            selected: self.studio.curves.selected,
            object: self.studio.selected,
            playing: self.studio.playing,
            follow_live: self.studio.follow_live,
            bounds: view.bounds,
            auto_fit: view.auto_fit,
            box_zoom: view.box_zoom,
            box_drag: view.box_drag,
            markers: view
                .markers
                .iter()
                .map(|marker| (marker.curve, marker.frame))
                .collect(),
        }
    }

    fn menu_open(&self) -> bool {
        self.context
            .read_response(self.graph_id.with("canvas"))
            .unwrap()
            .context_menu_opened()
    }

    fn click(&mut self, position: egui::Pos2, button: egui::PointerButton) -> egui::FullOutput {
        self.time += 1.0;
        self.click_immediate(position, button)
    }

    fn click_immediate(
        &mut self,
        position: egui::Pos2,
        button: egui::PointerButton,
    ) -> egui::FullOutput {
        let _ = self.frame(
            vec![egui::Event::PointerMoved(position)],
            egui::Modifiers::NONE,
        );
        let _ = self.frame(
            vec![pointer(position, button, true, egui::Modifiers::NONE)],
            egui::Modifiers::NONE,
        );
        self.frame(
            vec![pointer(position, button, false, egui::Modifiers::NONE)],
            egui::Modifiers::NONE,
        )
    }

    fn open_menu(&mut self) -> egui::FullOutput {
        let _ = self.click(self.canvas.center(), egui::PointerButton::Secondary);
        self.settle_menu()
    }

    fn settle_menu(&mut self) -> egui::FullOutput {
        let mut previous = None;
        let mut stable = 0;
        for attempt in 0..10 {
            let output = self.frame(vec![], egui::Modifiers::NONE);
            assert!(self.menu_open());
            let label = self.studio.prefs.language.text("Export CSV", "导出 CSV");
            let Some(position) = self.visible_menu_text_position(&output, label) else {
                if attempt == 9 {
                    let _ = self.menu_text_position(&output, label);
                }
                continue;
            };
            let geometry = (self.popup_response().rect, position);
            if previous == Some(geometry) {
                stable += 1;
                if stable == 2 {
                    return output;
                }
            } else {
                stable = 0;
            }
            previous = Some(geometry);
        }
        panic!("context menu geometry did not settle: {previous:?}")
    }

    fn popup_response(&self) -> egui::Response {
        self.context
            .read_response(self.graph_id.with("canvas").with("popup"))
            .expect("context menu area response")
    }

    fn menu_text_position(&self, output: &egui::FullOutput, expected: &str) -> egui::Pos2 {
        self.visible_menu_text_position(output, expected).unwrap_or_else(|| {
            let popup = self.popup_response();
            let candidates: Vec<_> = output.shapes.iter().filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == expected => {
                    let point = text.galley.rect.translate(text.pos.to_vec2()).center();
                    Some((point, shape.clip_rect, self.context.layer_id_at(point)))
                }
                _ => None,
            }).collect();
            panic!("missing visible popup text {expected}: rect={:?}, layer={:?}, candidates={candidates:?}", popup.rect, popup.layer_id)
        })
    }

    fn visible_menu_text_position(
        &self,
        output: &egui::FullOutput,
        expected: &str,
    ) -> Option<egui::Pos2> {
        let popup = self.popup_response();
        output
            .shapes
            .iter()
            .rev()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == expected => {
                    let point = text.galley.rect.translate(text.pos.to_vec2()).center();
                    (popup.rect.contains(point)
                        && shape.clip_rect.contains(point)
                        && self.context.layer_id_at(point) == Some(popup.layer_id))
                    .then_some(point)
                }
                _ => None,
            })
    }

    fn escape(&mut self) {
        for pressed in [true, false] {
            let _ = self.frame(
                vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                egui::Modifiers::NONE,
            );
        }
        let _ = self.frame(vec![], egui::Modifiers::NONE);
        assert!(!self.menu_open());
    }

    fn drag(&mut self, button: egui::PointerButton, modifiers: egui::Modifiers) {
        let from = self.canvas.center();
        let to = from + egui::vec2(70.0, 25.0);
        self.drag_between(from, to, button, modifiers);
    }

    fn drag_between(
        &mut self,
        from: egui::Pos2,
        to: egui::Pos2,
        button: egui::PointerButton,
        modifiers: egui::Modifiers,
    ) {
        self.time += 1.0;
        let _ = self.frame(vec![egui::Event::PointerMoved(from)], modifiers);
        let _ = self.frame(vec![pointer(from, button, true, modifiers)], modifiers);
        let _ = self.frame(vec![egui::Event::PointerMoved(to)], modifiers);
        let _ = self.frame(vec![pointer(to, button, false, modifiers)], modifiers);
    }

    fn wait_export(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.studio.save_job.is_some() || self.studio.background_jobs.active_count() != 0 {
            self.studio.poll(&self.context);
            assert!(Instant::now() < deadline, "curve export did not finish");
            std::thread::park_timeout(Duration::from_millis(1));
        }
    }
}

fn pointer(
    pos: egui::Pos2,
    button: egui::PointerButton,
    pressed: bool,
    modifiers: egui::Modifiers,
) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers,
    }
}

#[test]
fn canvas_context_menu_names_the_selected_curve_and_closing_restores_primary_gestures() {
    for language in [Language::Chinese, Language::English] {
        let mut ui = ChartUi::new(language);
        let before = ui.state();
        let output = ui.open_menu();
        let _ = ui.menu_text_position(
            &output,
            &format!("{} #2", language.text("Selected curve", "当前曲线")),
        );
        let _ = ui.menu_text_position(&output, ui.studio.curves.curves[1].label.text(language));
        let _ = ui.menu_text_position(&output, language.text("Choose path…", "选择路径…"));
        let _ = ui.menu_text_position(&output, language.text("Export CSV", "导出 CSV"));
        assert_eq!(ui.state(), before);
        ui.escape();
        assert_eq!(ui.state(), before);
        let position = egui::pos2(
            ui.canvas.left() + ui.canvas.width() * 0.75,
            ui.canvas.center().y,
        );
        let _ = ui.click(position, egui::PointerButton::Primary);
        assert_eq!(ui.studio.frame, 2);
        assert_eq!(ui.state().markers.len(), 1);
        let _ = ui.click(position, egui::PointerButton::Primary);
        let _ = ui.click_immediate(position, egui::PointerButton::Primary);
        assert_eq!(ui.studio.frame, 3);
        assert_eq!(ui.state().markers.len(), 1);
        let marked = ui.state();
        ui.drag(egui::PointerButton::Primary, egui::Modifiers::NONE);
        let panned = ui.state();
        assert_ne!(panned.bounds, marked.bounds);
        assert!(!panned.auto_fit);
        assert_eq!(panned.frame, marked.frame);
        assert_eq!(panned.markers, marked.markers);
    }
}

#[test]
fn canvas_context_menu_exports_only_the_selected_curve_via_the_existing_job() {
    let directory = WorkDir::new();
    let first = directory.0.join("first.csv");
    let selected = directory.0.join("selected.csv");
    let mut ui = ChartUi::new(Language::English);
    ui.studio.curves.curves[0].export_path = first.to_string_lossy().into_owned();
    ui.studio.curve_export_path = selected.to_string_lossy().into_owned();
    let expected = crate::curve::csv(&ui.studio.curves.curves[1]).unwrap();
    let before = ui.state();
    let output = ui.open_menu();
    let export = ui.menu_text_position(&output, "Export CSV");
    let _ = ui.click(export, egui::PointerButton::Primary);
    assert!(
        ui.studio.save_job.is_some(),
        "popup={:?}, target={export:?}, layer={:?}, open={}, notices={:?}",
        ui.popup_response().rect,
        ui.context.layer_id_at(export),
        ui.menu_open(),
        ui.studio
            .notices
            .iter()
            .map(|notice| notice.text(Language::English))
            .collect::<Vec<_>>()
    );
    let _ = ui.frame(vec![], egui::Modifiers::NONE);
    assert!(!ui.menu_open());
    ui.wait_export();
    assert_eq!(ui.state(), before);
    assert!(!first.exists());
    let bytes = fs::read_to_string(&selected).unwrap();
    assert_eq!(bytes, expected);
    assert!(bytes.contains("0,201,0x2,0,\"Second\",\"value\""));
    assert!(!bytes.contains("0,101,"));
    assert!(ui.studio.notices.iter().any(|notice| {
        notice
            .text(Language::English)
            .starts_with("Exported sampled curve CSV to ")
    }));
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
}

#[test]
fn canvas_context_menu_keeps_pending_missing_and_active_export_actions_disabled() {
    let directory = WorkDir::new();
    for unavailable in 0..3 {
        let mut ui = ChartUi::new(Language::Chinese);
        ui.studio.curve_export_path = directory
            .0
            .join("selected.csv")
            .to_string_lossy()
            .into_owned();
        let _active_export = if unavailable == 2 {
            let (sender, receiver) = mpsc::channel::<Message>();
            ui.studio.save_job = Some(Mutex::new(receiver));
            Some(sender)
        } else {
            if unavailable == 0 {
                ui.studio.curves.curves[1].pending = true;
                ui.studio.curves.pending.push_back(2);
            } else {
                ui.studio.curves.curves[1].data = None;
            }
            None
        };
        let _ = ui.frame(vec![], egui::Modifiers::NONE);
        let before = ui.state();
        let output = ui.open_menu();
        let export = ui.menu_text_position(&output, "导出 CSV");
        let _ = ui.click(export, egui::PointerButton::Primary);
        assert_eq!(ui.studio.save_job.is_some(), unavailable == 2);
        assert_eq!(ui.studio.background_jobs.active_count(), 0);
        assert_eq!(ui.state(), before);
        assert!(fs::read_dir(&directory.0).unwrap().next().is_none());
    }
}

fn assert_navigation(actual: &PlotState, expected: &PlotState) {
    assert_eq!(
        (
            actual.frame,
            actual.selected,
            actual.object,
            actual.playing,
            actual.follow_live
        ),
        (
            expected.frame,
            expected.selected,
            expected.object,
            expected.playing,
            expected.follow_live
        )
    );
    assert_eq!(actual.markers, expected.markers);
}

fn assert_bounds(actual: Option<Bounds>, x: [f64; 2], y: [f64; 2]) {
    let actual = actual.unwrap();
    for (actual, expected) in actual
        .x
        .into_iter()
        .chain(actual.y)
        .zip(x.into_iter().chain(y))
    {
        assert!((actual - expected).abs() < 1e-4, "{actual} != {expected}");
    }
}

fn has_preview(output: &egui::FullOutput, expected: egui::Rect) -> bool {
    output.shapes.iter().any(|shape| match &shape.shape {
        egui::epaint::Shape::Rect(rect) => {
            rect.stroke.color == egui::Color32::WHITE
                && rect.fill == egui::Color32::TRANSPARENT
                && rect.rect.min.distance(expected.min) < 1.0
                && rect.rect.max.distance(expected.max) < 1.0
        }
        _ => false,
    })
}

#[test]
fn right_drag_zooms_both_axes_without_navigation_or_menu_and_click_still_opens_export() {
    for language in [Language::Chinese, Language::English] {
        for modifiers in [egui::Modifiers::NONE, egui::Modifiers::SHIFT] {
            let mut ui = ChartUi::new(language);
            ui.studio.playing = -1;
            let before = ui.state();
            let from = egui::pos2(
                ui.canvas.left() + ui.canvas.width() * 0.2,
                ui.canvas.top() + ui.canvas.height() * 0.25,
            );
            let to = egui::pos2(
                ui.canvas.left() + ui.canvas.width() * 0.8,
                ui.canvas.top() + ui.canvas.height() * 0.75,
            );
            let _ = ui.frame(vec![egui::Event::PointerMoved(from)], modifiers);
            let _ = ui.frame(
                vec![pointer(
                    from,
                    egui::PointerButton::Secondary,
                    true,
                    modifiers,
                )],
                modifiers,
            );
            assert!(ui.state().box_drag.is_none());
            assert!(!ui.menu_open());
            let output = ui.frame(vec![egui::Event::PointerMoved(to)], modifiers);
            assert!(has_preview(&output, egui::Rect::from_two_pos(from, to)));
            assert_eq!(ui.state().bounds, before.bounds);
            assert!(!ui.menu_open());
            let _ = ui.frame(
                vec![pointer(
                    to,
                    egui::PointerButton::Secondary,
                    false,
                    modifiers,
                )],
                modifiers,
            );
            let after = ui.state();
            assert_navigation(&after, &before);
            assert_bounds(after.bounds, [0.6, 2.4], [124.175, 180.825]);
            assert!(!after.auto_fit && after.box_drag.is_none());
            assert!(!ui.menu_open());
            let output = ui.open_menu();
            let _ = ui.menu_text_position(&output, language.text("Export CSV", "导出 CSV"));
            assert_eq!(ui.state(), after);
            ui.escape();
            assert_eq!(ui.state(), after);
        }
    }
}

#[test]
fn right_click_threshold_and_invalid_drag_release_preserve_the_plot() {
    let mut ui = ChartUi::new(Language::English);
    let before = ui.state();
    let from = ui.canvas.center();
    let to = from + egui::vec2(1.0, 1.0);
    let _ = ui.frame(vec![egui::Event::PointerMoved(from)], egui::Modifiers::NONE);
    let _ = ui.frame(
        vec![pointer(
            from,
            egui::PointerButton::Secondary,
            true,
            egui::Modifiers::NONE,
        )],
        egui::Modifiers::NONE,
    );
    let _ = ui.frame(vec![egui::Event::PointerMoved(to)], egui::Modifiers::NONE);
    assert!(ui.state().box_drag.is_none());
    let _ = ui.frame(
        vec![pointer(
            to,
            egui::PointerButton::Secondary,
            false,
            egui::Modifiers::NONE,
        )],
        egui::Modifiers::NONE,
    );
    let output = ui.settle_menu();
    let _ = ui.menu_text_position(&output, "Export CSV");
    assert_eq!(ui.state(), before);
    ui.escape();
    ui.drag_between(
        from,
        from + egui::vec2(70.0, 1.0),
        egui::PointerButton::Secondary,
        egui::Modifiers::NONE,
    );
    assert_eq!(ui.state(), before);
    assert!(!ui.menu_open());
    let outside = egui::pos2(ui.canvas.right() + 2.0, ui.canvas.bottom() + 2.0);
    ui.drag_between(
        from,
        outside,
        egui::PointerButton::Secondary,
        egui::Modifiers::NONE,
    );
    assert_eq!(ui.state(), before);
    assert!(!ui.menu_open());
}

#[test]
fn right_rectangle_cancellation_never_commits_rearms_or_leaves_a_preview() {
    for cancellation in 0..4 {
        let mut ui = ChartUi::new(Language::English);
        let before = ui.state();
        let from = ui.canvas.center() - egui::vec2(90.0, 45.0);
        let to = ui.canvas.center() + egui::vec2(90.0, 45.0);
        let preview = egui::Rect::from_two_pos(from, to);
        let _ = ui.frame(vec![egui::Event::PointerMoved(from)], egui::Modifiers::NONE);
        let _ = ui.frame(
            vec![pointer(
                from,
                egui::PointerButton::Secondary,
                true,
                egui::Modifiers::NONE,
            )],
            egui::Modifiers::NONE,
        );
        let output = ui.frame(vec![egui::Event::PointerMoved(to)], egui::Modifiers::NONE);
        assert!(has_preview(&output, preview));
        let output = match cancellation {
            0 => {
                ui.escape();
                ui.frame(vec![], egui::Modifiers::NONE)
            }
            1 => ui.frame(vec![egui::Event::PointerGone], egui::Modifiers::NONE),
            2 => ui.frame(
                vec![egui::Event::PointerMoved(egui::pos2(2.0, to.y))],
                egui::Modifiers::NONE,
            ),
            3 => {
                ui.focused = false;
                ui.frame(vec![], egui::Modifiers::NONE)
            }
            _ => unreachable!(),
        };
        assert!(!has_preview(&output, preview));
        assert!(ui.state().box_drag.is_none_or(|drag| drag.cancelled));
        ui.focused = true;
        let output = ui.frame(vec![egui::Event::PointerMoved(to)], egui::Modifiers::NONE);
        assert!(!has_preview(&output, preview));
        assert_eq!(ui.state().bounds, before.bounds);
        let _ = ui.frame(
            vec![pointer(
                to,
                egui::PointerButton::Secondary,
                false,
                egui::Modifiers::NONE,
            )],
            egui::Modifiers::NONE,
        );
        assert_eq!(ui.state(), before);
        assert!(!ui.menu_open());
        let _ = ui.frame(vec![], egui::Modifiers::NONE);
        assert_eq!(ui.state(), before);
        let _ = ui.open_menu();
        assert_eq!(ui.state(), before);
        ui.escape();
    }
}

#[test]
fn right_rectangle_batched_cancellation_stays_cancelled_after_returning() {
    for cancellation in 0..4 {
        let mut ui = ChartUi::new(Language::English);
        let before = ui.state();
        let from = ui.canvas.center() - egui::vec2(90.0, 45.0);
        let to = ui.canvas.center() + egui::vec2(90.0, 45.0);
        let preview = egui::Rect::from_two_pos(from, to);
        let _ = ui.frame(vec![egui::Event::PointerMoved(from)], egui::Modifiers::NONE);
        let _ = ui.frame(
            vec![pointer(
                from,
                egui::PointerButton::Secondary,
                true,
                egui::Modifiers::NONE,
            )],
            egui::Modifiers::NONE,
        );
        let output = ui.frame(vec![egui::Event::PointerMoved(to)], egui::Modifiers::NONE);
        assert!(has_preview(&output, preview));
        assert!(ui.state().box_drag.is_some_and(|drag| !drag.cancelled));
        let mut events = match cancellation {
            0 => vec![egui::Event::PointerMoved(egui::pos2(2.0, to.y))],
            1 => vec![egui::Event::PointerGone],
            2 => vec![
                egui::Event::WindowFocused(false),
                egui::Event::WindowFocused(true),
            ],
            3 => [true, false]
                .into_iter()
                .map(|pressed| egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                })
                .collect(),
            _ => unreachable!(),
        };
        events.push(egui::Event::PointerMoved(to));
        let output = ui.frame(events, egui::Modifiers::NONE);
        assert!(ui.context.input(|input| input.focused));
        assert!(!has_preview(&output, preview));
        assert!(ui.state().box_drag.is_none_or(|drag| drag.cancelled));
        assert_eq!(ui.state().bounds, before.bounds);
        assert_navigation(&ui.state(), &before);
        let output = ui.frame(
            vec![pointer(
                to,
                egui::PointerButton::Secondary,
                false,
                egui::Modifiers::NONE,
            )],
            egui::Modifiers::NONE,
        );
        assert!(!has_preview(&output, preview));
        assert_eq!(ui.state(), before);
        assert!(!ui.menu_open());
        let _ = ui.frame(vec![], egui::Modifiers::NONE);
        assert_eq!(ui.state(), before);
        let _ = ui.open_menu();
        assert_eq!(ui.state(), before);
        ui.escape();
    }
}

#[test]
fn right_rectangle_uses_release_position_before_later_batched_events() {
    for outside_release in [true, false] {
        let mut ui = ChartUi::new(Language::English);
        let before = ui.state();
        let from = egui::pos2(
            ui.canvas.left() + ui.canvas.width() * 0.2,
            ui.canvas.top() + ui.canvas.height() * 0.25,
        );
        let to = egui::pos2(
            ui.canvas.left() + ui.canvas.width() * 0.8,
            ui.canvas.top() + ui.canvas.height() * 0.75,
        );
        let outside = egui::pos2(ui.canvas.right() + 2.0, to.y);
        let _ = ui.frame(vec![egui::Event::PointerMoved(from)], egui::Modifiers::NONE);
        let _ = ui.frame(
            vec![pointer(
                from,
                egui::PointerButton::Secondary,
                true,
                egui::Modifiers::NONE,
            )],
            egui::Modifiers::NONE,
        );
        let output = ui.frame(vec![egui::Event::PointerMoved(to)], egui::Modifiers::NONE);
        assert!(has_preview(&output, egui::Rect::from_two_pos(from, to)));
        assert!(ui.state().box_drag.is_some_and(|drag| !drag.cancelled));
        let events = if outside_release {
            vec![
                pointer(
                    outside,
                    egui::PointerButton::Secondary,
                    false,
                    egui::Modifiers::NONE,
                ),
                egui::Event::PointerMoved(to),
            ]
        } else {
            vec![
                pointer(
                    to,
                    egui::PointerButton::Secondary,
                    false,
                    egui::Modifiers::NONE,
                ),
                egui::Event::PointerMoved(outside),
                egui::Event::PointerGone,
                egui::Event::WindowFocused(false),
                egui::Event::WindowFocused(true),
                egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: false,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        let _ = ui.frame(events, egui::Modifiers::NONE);
        let after = ui.state();
        assert_navigation(&after, &before);
        assert!(after.box_drag.is_none());
        assert!(!ui.menu_open());
        if outside_release {
            assert_eq!(after, before);
        } else {
            assert_bounds(after.bounds, [0.6, 2.4], [124.175, 180.825]);
            assert!(!after.auto_fit);
        }
        let _ = ui.frame(vec![], egui::Modifiers::NONE);
        assert_eq!(ui.state(), after);
    }
}

#[test]
fn right_drag_on_the_canvas_closes_an_existing_menu_before_zooming() {
    let mut ui = ChartUi::new(Language::English);
    let before = ui.state();
    let _ = ui.open_menu();
    let from = egui::pos2(
        ui.canvas.left() + ui.canvas.width() * 0.1,
        ui.canvas.top() + ui.canvas.height() * 0.1,
    );
    let to = egui::pos2(
        ui.canvas.left() + ui.canvas.width() * 0.6,
        ui.canvas.top() + ui.canvas.height() * 0.6,
    );
    assert!(!ui.popup_response().rect.contains(from));
    let _ = ui.frame(vec![egui::Event::PointerMoved(from)], egui::Modifiers::NONE);
    let _ = ui.frame(
        vec![pointer(
            from,
            egui::PointerButton::Secondary,
            true,
            egui::Modifiers::NONE,
        )],
        egui::Modifiers::NONE,
    );
    let output = ui.frame(vec![egui::Event::PointerMoved(to)], egui::Modifiers::NONE);
    assert!(!ui.menu_open());
    assert!(has_preview(&output, egui::Rect::from_two_pos(from, to)));
    let _ = ui.frame(
        vec![pointer(
            to,
            egui::PointerButton::Secondary,
            false,
            egui::Modifiers::NONE,
        )],
        egui::Modifiers::NONE,
    );
    let after = ui.state();
    assert_navigation(&after, &before);
    assert_bounds(after.bounds, [0.3, 1.8], [141.17, 197.82]);
    assert!(after.box_drag.is_none() && !after.auto_fit);
    assert!(!ui.menu_open());
}

#[test]
fn primary_shift_and_tool_rectangles_keep_clamping_at_the_canvas_edges() {
    for tool in [false, true] {
        let mut ui = ChartUi::new(Language::English);
        if tool {
            let output = ui.frame(vec![], egui::Modifiers::NONE);
            let position = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) if text.galley.text() == "Rectangle zoom" => {
                        Some(text.galley.rect.translate(text.pos.to_vec2()).center())
                    }
                    _ => None,
                })
                .expect("rectangle zoom tool");
            let _ = ui.click(position, egui::PointerButton::Primary);
            assert!(ui.state().box_zoom);
        }
        let before = ui.state();
        let from = egui::pos2(
            ui.canvas.left() + ui.canvas.width() * 0.2,
            ui.canvas.top() + ui.canvas.height() * 0.25,
        );
        let to = egui::pos2(ui.canvas.right() + 80.0, ui.canvas.top() - 35.0);
        ui.drag_between(
            from,
            to,
            egui::PointerButton::Primary,
            if tool {
                egui::Modifiers::NONE
            } else {
                egui::Modifiers::SHIFT
            },
        );
        let after = ui.state();
        assert_navigation(&after, &before);
        assert_bounds(after.bounds, [0.6, 3.0], [180.825, 209.15]);
        assert!(after.box_drag.is_none() && !after.auto_fit);
        assert!(!ui.menu_open());
    }
}
