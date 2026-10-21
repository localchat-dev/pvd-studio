use super::*;

pub(super) fn show(ui: &mut egui::Ui, rect: egui::Rect, studio: &mut Studio) -> egui::Response {
    let language = studio.prefs.language;
    let visible = studio.prefs.show_viewport_help;
    let font = egui::TextStyle::Small.resolve(ui.style());
    let painter = ui.painter().with_clip_rect(rect);
    let background = egui::Color32::from_rgba_unmultiplied(16, 23, 31, 220);
    let handle = egui::Rect::from_min_size(
        rect.left_bottom() + egui::vec2(8.0, -40.0),
        egui::vec2(24.0, 32.0),
    );
    if visible {
        let camera_help = if let Some(name) = &studio.captured_camera {
            match language {
                Language::English => {
                    format!("Recorded camera: {name} · Choose Free camera in Display to navigate")
                }
                Language::Chinese => {
                    format!("录制相机：{name} · 在显示面板选择自由视角后可移动")
                }
            }
        } else if studio.prefs.camera_mode == crate::camera_controller::CameraMode::Fly {
            format!(
                "{} · {} {}",
                studio.prefs.camera_mode.help(language),
                crate::camera_controller::fly_speed(studio.prefs.fly_speed),
                language.text("units/s", "单位/秒")
            )
        } else {
            studio.prefs.camera_mode.help(language).into()
        };
        let help_text = format!(
            "{}\n{camera_help}",
            language.text(
                "Space: play/pause · ←/→: step · F: focus",
                "空格 播放/暂停 · ←/→ 逐帧 · F 聚焦",
            )
        );
        let foreground = egui::Color32::from_rgb(204, 215, 230);
        let help = painter.layout(
            help_text,
            font,
            foreground,
            (rect.width() - handle.width() - 40.0).max(1.0),
        );
        let size = egui::vec2(
            help.size().x + handle.width() + 24.0,
            (help.size().y + 12.0).max(handle.height()),
        );
        let bounds =
            egui::Rect::from_min_size(rect.left_bottom() + egui::vec2(8.0, -size.y - 8.0), size);
        painter.rect_filled(bounds, 3.0, background);
        painter.galley(
            bounds.min + egui::vec2(handle.width() + 12.0, 6.0),
            help,
            foreground,
        );
        painter.line_segment(
            [
                egui::pos2(handle.right() + 2.0, bounds.top() + 8.0),
                egui::pos2(handle.right() + 2.0, bounds.bottom() - 8.0),
            ],
            egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(76, 89, 106, 110)),
        );
    } else {
        painter.rect_filled(handle, 3.0, background);
    }
    let label = if visible {
        language.text("Hide controls", "隐藏操作提示")
    } else {
        language.text("Show controls", "显示操作提示")
    };
    let button = ui
        .interact(
            handle,
            ui.make_persistent_id("viewport-help-toggle"),
            egui::Sense::click(),
        )
        .on_hover_text(label)
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    button.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    let highlighted = button.hovered() || button.has_focus();
    if highlighted {
        painter.rect_filled(
            handle.shrink(2.0),
            2.0,
            egui::Color32::from_rgba_unmultiplied(204, 215, 230, 22),
        );
    }
    if button.has_focus() {
        painter.rect_stroke(
            handle.shrink(1.0),
            2.0,
            ui.visuals().selection.stroke,
            egui::StrokeKind::Inside,
        );
    }
    let direction = if visible { -1.0 } else { 1.0 };
    let center = handle.center();
    painter.add(egui::Shape::line(
        vec![
            center + egui::vec2(-direction * 2.0, -4.0),
            center + egui::vec2(direction * 2.0, 0.0),
            center + egui::vec2(-direction * 2.0, 4.0),
        ],
        egui::Stroke::new(
            1.4,
            if highlighted {
                egui::Color32::from_rgb(220, 228, 238)
            } else {
                MUTED
            },
        ),
    ));
    if button.clicked() {
        studio.prefs.show_viewport_help = !visible;
        button.surrender_focus();
        ui.ctx().request_repaint();
    }
    button
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_toggle_preserves_scene_input_and_releases_keyboard_focus() {
        for language in [Language::Chinese, Language::English] {
            let context = egui::Context::default();
            configure(&context);
            let mut studio = Studio::default();
            studio.prefs.language = language;
            studio.selected = Some(99);
            studio.live_connected = true;
            studio.follow_live = true;
            studio.frame = 17;
            let mut scene_clicks = 0;
            let mut draw = |studio: &mut Studio, events| {
                let mut button_rect = egui::Rect::NOTHING;
                let _ = context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(640.0, 360.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |context| {
                        crate::camera_controller::begin_frame(context);
                        egui::CentralPanel::default().show(context, |ui| {
                            let scene = ui.allocate_response(
                                ui.available_size(),
                                egui::Sense::click_and_drag(),
                            );
                            let button = show(ui, scene.rect, studio);
                            button_rect = button.rect;
                            if scene.clicked() && !button.contains_pointer() {
                                scene_clicks += 1;
                            }
                        });
                    },
                );
                button_rect
            };
            draw(&mut studio, vec![]);
            for visible in [false, true, false, true] {
                let point = draw(&mut studio, vec![]).center();
                draw(&mut studio, vec![egui::Event::PointerMoved(point)]);
                for pressed in [true, false] {
                    draw(
                        &mut studio,
                        vec![egui::Event::PointerButton {
                            pos: point,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        }],
                    );
                }
                draw(&mut studio, vec![]);
                assert_eq!(studio.prefs.show_viewport_help, visible);
                assert!(!crate::camera_controller::keyboard_input_blocked(&context));
                assert_eq!(studio.selected, Some(99));
                assert_eq!(studio.frame, 17);
                assert!(studio.follow_live && studio.live_connected);
                assert_eq!(studio.playing, 0);
            }
            assert_eq!(scene_clicks, 0);
        }
    }
}
