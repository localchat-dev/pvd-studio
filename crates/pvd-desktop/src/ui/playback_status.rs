use super::*;
use crate::studio::DisplayMode;

fn color(mode: DisplayMode) -> egui::Color32 {
    match mode {
        DisplayMode::Live => egui::Color32::from_rgb(112, 207, 158),
        DisplayMode::Paused | DisplayMode::Waiting | DisplayMode::Seeking => {
            egui::Color32::from_rgb(230, 189, 120)
        }
        DisplayMode::Forward | DisplayMode::Reverse => egui::Color32::from_rgb(137, 194, 235),
        DisplayMode::Empty => MUTED,
    }
}

pub(super) fn live_button(ui: &mut egui::Ui, studio: &Studio) -> egui::Response {
    let foreground = if studio.follow_live {
        color(studio.display_mode())
    } else {
        ACCENT
    };
    ui.add(egui::Button::new(
        egui::RichText::new(studio.live_follow_label(studio.prefs.language))
            .strong()
            .color(foreground),
    ))
}

pub(super) fn show(ui: &mut egui::Ui, studio: &Studio) -> egui::Response {
    let language = studio.prefs.language;
    let mode = studio.display_mode();
    let foreground = color(mode);
    let position = studio.display_position(language);
    let detail = studio.display_detail(language);
    let tooltip = format!("{detail}\n{position}\n{}", language.text(
        "Display mode is separate from TCP reception. LIVE follows the latest decoded frame; it can lag behind the sender. Pausing or navigating history keeps reception running.",
        "画面状态与 TCP 接收分别显示。实时跟随最新解码帧，仍可能落后于发送端。暂停或查看历史时，数据接收继续。",
    ));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 7.0;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if !position.is_empty() {
                let font = egui::TextStyle::Small.resolve(ui.style());
                let width = ui
                    .fonts_mut(|fonts| fonts.layout_no_wrap(position.clone(), font, MUTED).size().x)
                    .min(ui.available_width() * 0.5);
                ui.add_sized(
                    [width, ui.text_style_height(&egui::TextStyle::Body)],
                    egui::Label::new(egui::RichText::new(&position).small().color(MUTED))
                        .truncate(),
                );
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                let (dot, _) = ui.allocate_exact_size(egui::vec2(7.0, 7.0), egui::Sense::hover());
                ui.painter().circle_filled(dot.center(), 3.0, foreground);
                if let Some(room) = studio.displayed_room() {
                    ui.add(
                        egui::Label::new(egui::RichText::new(studio.room_label(room)).strong())
                            .truncate(),
                    )
                    .on_hover_text(format!("PxScene 0x{:X} / {}", room.id, room.generation));
                }
                ui.label(
                    egui::RichText::new(mode.label(language))
                        .strong()
                        .color(foreground),
                );
                if matches!(mode, DisplayMode::Paused | DisplayMode::Seeking) && !detail.is_empty()
                {
                    ui.add(
                        egui::Label::new(egui::RichText::new(detail).small().color(MUTED))
                            .truncate(),
                    );
                }
            });
        });
    })
    .response
    .on_hover_text(tooltip)
}
