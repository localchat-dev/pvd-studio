use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use bevy_egui::egui;

pub(super) fn configure(context: &egui::Context) {
    context.style_mut(|style| {
        for (style_name, font) in [
            (egui::TextStyle::Heading, egui::FontId::proportional(18.0)),
            (egui::TextStyle::Body, egui::FontId::proportional(15.0)),
            (egui::TextStyle::Button, egui::FontId::proportional(15.0)),
            (egui::TextStyle::Small, egui::FontId::proportional(13.0)),
            (egui::TextStyle::Monospace, egui::FontId::monospace(14.0)),
        ] {
            style.text_styles.insert(style_name, font);
        }
    });

    let directory = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("Fonts");
    let mut fonts = egui::FontDefinitions::default();
    let cjk = [("msyh.ttc", 1), ("simhei.ttf", 0)]
        .into_iter()
        .find_map(|(file, index)| load_font(&directory, file, index));
    if let Some(font) = cjk {
        insert_font(
            &mut fonts,
            "system-cjk",
            font,
            &[egui::FontFamily::Proportional, egui::FontFamily::Monospace],
        );
    }
    for (name, file, family) in [
        ("system-ui", "segoeui.ttf", egui::FontFamily::Proportional),
        ("system-mono", "consola.ttf", egui::FontFamily::Monospace),
    ] {
        if let Some(font) = load_font(&directory, file, 0) {
            insert_font(&mut fonts, name, font, &[family]);
        }
    }
    context.set_fonts(fonts);
}

fn load_font(directory: &Path, file: &str, index: u32) -> Option<egui::FontData> {
    let mut font = egui::FontData::from_owned(std::fs::read(directory.join(file)).ok()?);
    font.index = index;
    Some(font)
}

fn insert_font(
    fonts: &mut egui::FontDefinitions,
    name: &str,
    font: egui::FontData,
    families: &[egui::FontFamily],
) {
    fonts.font_data.insert(name.into(), Arc::new(font));
    for family in families {
        fonts
            .families
            .entry(family.clone())
            .or_default()
            .insert(0, name.into());
    }
}
