use super::*;
use crate::coordinates::{Handedness, UpAxis, UpMode};

pub(super) fn show(ui: &mut egui::Ui, studio: &mut Studio) {
    let language = studio.prefs.language;
    egui::ComboBox::from_id_salt("source-up-mode")
        .selected_text(studio.prefs.up_mode.label(language))
        .show_ui(ui, |ui| {
            for mode in UpMode::ALL {
                ui.selectable_value(&mut studio.prefs.up_mode, mode, mode.label(language));
            }
        });
    if studio.prefs.up_mode == UpMode::Custom {
        ui.label(language.text("Up vector", "上方向向量"));
        ui.horizontal(|ui| {
            for (index, name) in ["X", "Y", "Z"].into_iter().enumerate() {
                ui.label(name);
                ui.add(
                    egui::DragValue::new(&mut studio.prefs.custom_up[index])
                        .range(f32::MIN..=f32::MAX)
                        .speed(0.01),
                );
            }
        });
    }
    if studio.prefs.up_mode == UpMode::Auto {
        ui.label(language.text(
            "Uses gravity from the displayed frame, preferring the selected Scene. Other scenes must agree on the direction.",
            "使用当前显示帧的重力，优先采用选中的 Scene；多个场景须有相同方向。",
        ));
    }
    let resolution = studio.up_resolution();
    if studio.prefs.up_mode != UpMode::Preset {
        ui.label(language.text("Fallback preset", "回退预设"));
    }
    egui::ComboBox::from_id_salt("source-up-axis")
        .selected_text(format!(
            "{} {}",
            studio.prefs.up_axis.label(),
            language.text("up", "向上")
        ))
        .show_ui(ui, |ui| {
            for axis in UpAxis::ALL {
                ui.selectable_value(&mut studio.prefs.up_axis, axis, axis.label());
            }
        });
    if studio.prefs.up_mode != UpMode::Preset {
        let description = studio.up_resolution().description(language);
        if resolution.is_fallback() {
            ui.colored_label(egui::Color32::YELLOW, description);
        } else {
            ui.label(egui::RichText::new(description).small().color(MUTED));
        }
    }
    egui::ComboBox::from_id_salt("source-handedness")
        .selected_text(studio.prefs.handedness.label(language))
        .show_ui(ui, |ui| {
            for hand in [Handedness::Right, Handedness::Left] {
                ui.selectable_value(&mut studio.prefs.handedness, hand, hand.label(language));
            }
        });
    ui.horizontal(|ui| {
        ui.label(language.text("Meters / unit", "米 / 单位"));
        ui.add(
            egui::DragValue::new(&mut studio.prefs.meters_per_unit)
                .range(1e-6..=1e6)
                .speed(0.001),
        );
    });
    ui.label(egui::RichText::new(language.text(
        "Display conversion only. Properties, CSV and RepX keep recorded coordinates and units. Unit scale and handedness are set explicitly.",
        "仅转换显示。属性、CSV 和 RepX 保留录制坐标及单位，单位比例和手性由手动设置。",
    )).small());
}
