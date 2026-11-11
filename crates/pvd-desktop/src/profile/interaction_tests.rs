use super::*;
use pvd_core::analysis::Analysis;

#[test]
fn shift_wheel_pans_without_zooming_and_keeps_plain_wheel_for_scrolling() {
    let context = egui::Context::default();
    let mut state = ViewState::default();
    let mut window = [3.0, 7.0];
    let mut draw = |modifiers: egui::Modifiers, scroll: bool, focused: bool| {
        let mut events = vec![egui::Event::PointerMoved(egui::pos2(100.0, 20.0))];
        if scroll {
            events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, 4.0),
                modifiers,
            });
        }
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 150.0),
                )),
                events,
                modifiers,
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    let (rect, response) = ui.allocate_exact_size(
                        egui::vec2(200.0, 30.0),
                        egui::Sense::click_and_drag(),
                    );
                    if focused {
                        let mut text = "editing".to_owned();
                        ui.text_edit_singleline(&mut text).request_focus();
                    }
                    interact(ui, &response, rect, &mut window, 10.0, &mut state, false);
                });
            },
        );
        window
    };
    draw(egui::Modifiers::NONE, false, false);
    assert_eq!(draw(egui::Modifiers::NONE, true, false), [3.0, 7.0]);
    let panned = draw(egui::Modifiers::SHIFT, true, false);
    assert!(panned[0] < 3.0);
    assert!((panned[1] - panned[0] - 4.0).abs() < 1e-9);
    assert_eq!(draw(egui::Modifiers::SHIFT, true, true), panned);
}

fn span() -> ProfileSpan {
    ProfileSpan {
        zone: 99,
        event: 4,
        thread: 7,
        context: 0x1234,
        start: 0,
        end: 50,
        received_frame: 3,
    }
}

fn text_position(output: &egui::FullOutput, label: &str) -> egui::Pos2 {
    output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing menu label: {label}"))
}

#[test]
fn bar_context_menu_keeps_exact_identity_after_pointer_moves_and_honors_plot_availability() {
    for enabled in [true, false] {
        let context = egui::Context::default();
        let span = span();
        let mut selected = Vec::new();
        let mut draw = |events| {
            context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(600.0, 220.0),
                    )),
                    events,
                    ..Default::default()
                },
                |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        let (_, response) = ui.allocate_exact_size(
                            egui::vec2(220.0, 30.0),
                            egui::Sense::click_and_drag(),
                        );
                        let hovered = response.hovered().then_some(&span);
                        if let Some(key) =
                            span_menu(ui, &response, hovered, 5, |_| enabled, Language::English)
                        {
                            selected.push(key);
                        }
                    });
                },
            )
        };
        let point = egui::pos2(100.0, 20.0);
        draw(vec![egui::Event::PointerMoved(point)]);
        for pressed in [true, false] {
            draw(vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Secondary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
        }
        let output = draw(Vec::new());
        let button = text_position(&output, "Plot event duration");
        for pressed in [true, false] {
            draw(vec![
                egui::Event::PointerMoved(button),
                egui::Event::PointerButton {
                    pos: button,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
        }
        if enabled {
            assert_eq!(
                selected,
                vec![ExactKey {
                    zone: 99,
                    event: 4,
                    thread: 7,
                    context: 0x1234
                }]
            );
        } else {
            assert!(selected.is_empty());
        }
    }
}

#[test]
fn grouped_bars_do_not_pick_an_arbitrary_event_for_plotting() {
    let context = egui::Context::default();
    let mut analysis = Analysis::default();
    let mut first = span();
    first.start = 10;
    first.end = 10;
    analysis.spans = vec![first.clone(), first];
    let layout = Layout::build(&analysis);
    let mut hit = None;
    let mut single_hit = None;
    let _ = context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 160.0),
            )),
            events: vec![egui::Event::PointerMoved(egui::pos2(8.5, 15.0))],
            ..Default::default()
        },
        |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let rect = egui::Rect::from_min_size(egui::pos2(8.0, 8.0), egui::vec2(100.0, 20.0));
                single_hit = paint_lane(ui, rect, &[0], &analysis, &layout, [0.0, 1000.0], 1.0);
                hit = paint_lane(ui, rect, &[0, 1], &analysis, &layout, [0.0, 1000.0], 1.0);
            });
        },
    );
    assert_eq!(single_hit, Some(0));
    assert_eq!(hit, None);
}
