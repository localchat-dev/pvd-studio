use super::*;
use egui::{Event, PointerButton, Pos2, Rect};

#[derive(Default)]
struct Viewer {
    tabs: Vec<(Panel, Rect)>,
}

impl egui_dock::TabViewer for Viewer {
    type Tab = Panel;

    fn title(&mut self, tab: &mut Panel) -> egui::WidgetText {
        tab.label(crate::i18n::Language::English).into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Panel) {
        ui.label(tab.label(crate::i18n::Language::English));
    }

    fn is_closeable(&self, tab: &Panel) -> bool {
        tab.is_closeable()
    }

    fn on_tab_button(&mut self, tab: &mut Panel, response: &egui::Response) {
        self.tabs.push((*tab, response.rect));
    }
}

struct Harness {
    prefs: Preferences,
    context: egui::Context,
    viewer: Viewer,
    time: f64,
}

impl Harness {
    fn new() -> Self {
        let mut harness = Self {
            prefs: Preferences::default(),
            context: egui::Context::default(),
            viewer: Viewer::default(),
            time: 0.0,
        };
        for _ in 0..3 {
            harness.draw(vec![]);
        }
        harness
    }

    fn draw(&mut self, events: Vec<Event>) -> egui::FullOutput {
        self.time += 0.05;
        self.viewer.tabs.clear();
        let mut dock = self.prefs.dock.clone();
        let output = self.context.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1400.0, 900.0))),
                time: Some(self.time),
                events,
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(context, |ui| {
                        egui_dock::DockArea::new(&mut dock)
                            .show_close_buttons(false)
                            .show_leaf_close_all_buttons(false)
                            .show_inside(ui, &mut self.viewer);
                    });
            },
        );
        validate(&dock).unwrap();
        self.prefs.dock = dock;
        output
    }

    fn tab(&self, panel: Panel) -> Rect {
        self.viewer
            .tabs
            .iter()
            .find(|(tab, _)| *tab == panel)
            .unwrap()
            .1
    }

    fn click(&mut self, pos: Pos2, button: PointerButton) {
        self.draw(vec![Event::PointerMoved(pos)]);
        for pressed in [true, false] {
            self.draw(vec![Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers: egui::Modifiers::NONE,
            }]);
        }
    }
}

fn text_position(shape: &egui::Shape, label: &str) -> Option<Pos2> {
    match shape {
        egui::Shape::Text(text) if text.galley.text() == label => {
            Some(text.pos + text.galley.size() * 0.5)
        }
        egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| text_position(shape, label)),
        _ => None,
    }
}

#[test]
fn context_close_without_tab_cross_and_reopen_survives_reload() {
    let mut harness = Harness::new();
    harness.click(
        harness.tab(Panel::Memory).center(),
        PointerButton::Secondary,
    );
    harness.draw(vec![]);
    let output = harness.draw(vec![]);
    let close = output
        .shapes
        .iter()
        .find_map(|shape| text_position(&shape.shape, "Close"))
        .expect("context close action");
    harness.click(close, PointerButton::Primary);
    assert!(harness.prefs.dock.find_tab(&Panel::Memory).is_none());
    harness.prefs = Preferences::from_bytes(&serde_json::to_vec(&harness.prefs).unwrap());
    assert!(harness.prefs.dock.find_tab(&Panel::Memory).is_none());
    harness.prefs.show_panel(Panel::Memory);
    harness.draw(vec![]);
    assert!(harness.tab(Panel::Memory).is_positive());
    let location = harness.prefs.dock.find_tab(&Panel::Memory).unwrap();
    assert_eq!(
        harness.prefs.dock[location.0][location.1]
            .get_leaf()
            .unwrap()
            .active,
        location.2
    );
}

#[test]
fn dragging_reorders_tabs_without_losing_panels() {
    let mut harness = Harness::new();
    let start = harness.tab(Panel::Memory).center();
    let target_rect = harness.tab(Panel::Timeline);
    let target = egui::pos2(target_rect.left() + 3.0, target_rect.center().y);
    harness.draw(vec![Event::PointerMoved(start)]);
    harness.draw(vec![Event::PointerButton {
        pos: start,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    }]);
    for step in 1..=12 {
        harness.draw(vec![Event::PointerMoved(
            start.lerp(target, step as f32 / 12.0),
        )]);
    }
    for _ in 0..8 {
        harness.draw(vec![Event::PointerMoved(target)]);
    }
    harness.draw(vec![Event::PointerButton {
        pos: target,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    }]);
    harness.draw(vec![]);
    assert_eq!(
        harness.prefs.dock.find_tab(&Panel::Memory).unwrap().2,
        egui_dock::TabIndex(0)
    );
    assert_eq!(harness.prefs.dock.iter_all_tabs().count(), Panel::ALL.len());
}

#[test]
fn requested_panel_raises_its_overlapped_floating_group() {
    let mut harness = Harness::new();
    let rect = Rect::from_min_size(egui::pos2(300.0, 200.0), egui::vec2(500.0, 350.0));
    let mut surfaces = Vec::new();
    for panel in [Panel::Curves, Panel::Memory] {
        let location = harness.prefs.dock.find_tab(&panel).unwrap();
        surfaces.push(harness.prefs.dock.detach_tab(location, rect));
    }
    for _ in 0..4 {
        harness.draw(vec![]);
    }
    let point = harness.tab(Panel::Memory).center();
    let layer = |surface: SurfaceIndex| {
        egui::LayerId::new(
            egui::Order::Middle,
            egui::Id::new(format!("window {surface:?}")),
        )
    };
    harness.context.move_to_top(layer(surfaces[1]));
    harness.draw(vec![]);
    assert_eq!(harness.context.layer_id_at(point), Some(layer(surfaces[1])));
    harness.prefs.show_panel(Panel::Curves);
    harness.draw(vec![]);
    harness.draw(vec![]);
    assert_eq!(harness.context.layer_id_at(point), Some(layer(surfaces[0])));
}
