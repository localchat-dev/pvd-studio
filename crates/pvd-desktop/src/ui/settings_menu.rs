use bevy_egui::egui;

pub(crate) fn settings_menu<R>(
    ui: &mut egui::Ui,
    label: &str,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> (egui::Response, Option<egui::InnerResponse<R>>) {
    egui::containers::menu::MenuButton::new(label)
        .config(
            egui::containers::menu::MenuConfig::new()
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside),
        )
        .ui(ui, contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Editor {
        context: egui::Context,
        label: &'static str,
        text: String,
        enabled: bool,
        open: bool,
        button: egui::Rect,
        field: egui::Rect,
        checkbox: egui::Rect,
        done: egui::Rect,
        focused: bool,
    }

    impl Editor {
        fn new(label: &'static str) -> Self {
            let mut editor = Self {
                context: egui::Context::default(),
                label,
                text: "cache".into(),
                enabled: false,
                open: false,
                button: egui::Rect::NOTHING,
                field: egui::Rect::NOTHING,
                checkbox: egui::Rect::NOTHING,
                done: egui::Rect::NOTHING,
                focused: false,
            };
            super::super::configure(&editor.context);
            editor.draw(vec![]);
            editor.draw(vec![]);
            editor.click(editor.button.center());
            editor.draw(vec![]);
            assert!(editor.open);
            editor
        }

        fn draw(&mut self, events: Vec<egui::Event>) {
            let context = self.context.clone();
            let _ = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 600.0),
                    )),
                    events,
                    ..Default::default()
                },
                |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        let (button, popup) = settings_menu(ui, self.label, |ui| {
                            let response = ui.text_edit_singleline(&mut self.text);
                            self.field = response.rect;
                            self.focused = response.has_focus();
                            self.checkbox = ui.checkbox(&mut self.enabled, "Enabled").rect;
                            let done = ui.button("Done");
                            self.done = done.rect;
                            if done.clicked() {
                                ui.close();
                            }
                        });
                        self.button = button.rect;
                        self.open = popup.is_some();
                    });
                },
            );
        }

        fn click(&mut self, pos: egui::Pos2) {
            self.draw(vec![egui::Event::PointerMoved(pos)]);
            for pressed in [true, false] {
                self.draw(vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }]);
            }
            self.draw(vec![]);
        }
    }

    #[test]
    fn settings_remain_open_for_text_and_multiple_checkbox_changes() {
        for label in [
            "Capture setup",
            "录制设置",
            "Graph settings",
            "图页设置",
            "RepX",
        ] {
            let mut editor = Editor::new(label);
            editor.click(editor.field.center());
            assert!(editor.open, "{label} closed while focusing its text field");
            assert!(editor.focused);
            editor.draw(vec![egui::Event::Text("-history".into())]);
            editor.draw(vec![]);
            assert!(editor.open);
            assert!(editor.text.contains("-history"));
            editor.click(editor.checkbox.center());
            assert!(editor.open);
            assert!(editor.enabled);
            editor.click(editor.checkbox.center());
            assert!(editor.open);
            assert!(!editor.enabled);
        }
    }

    #[test]
    fn settings_close_on_outside_escape_and_completed_action() {
        let mut editor = Editor::new("Capture setup");
        editor.click(egui::pos2(760.0, 560.0));
        assert!(!editor.open);

        editor.click(editor.button.center());
        assert!(editor.open);
        editor.draw(vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        editor.draw(vec![]);
        assert!(!editor.open);

        editor.click(editor.button.center());
        assert!(editor.open);
        editor.click(editor.done.center());
        assert!(!editor.open);
    }
}
