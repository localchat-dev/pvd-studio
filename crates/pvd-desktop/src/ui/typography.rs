use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use bevy_egui::egui;

#[cfg(windows)]
mod mapped_font;

struct SystemFont {
    bytes: FontBytes,
    index: u32,
}

enum FontBytes {
    Owned(Vec<u8>),
    #[cfg(windows)]
    Mapped(mapped_font::MappedFont),
}

impl FontBytes {
    fn load(path: &Path) -> Option<Self> {
        #[cfg(windows)]
        if let Some(font) = mapped_font::MappedFont::open(path) {
            return Some(Self::Mapped(font));
        }
        std::fs::read(path).ok().map(Self::Owned)
    }

    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Owned(bytes) => bytes,
            #[cfg(windows)]
            Self::Mapped(font) => font.as_slice(),
        }
    }
}

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

    // egui borrows static font data when building an atlas and clones owned font buffers.
    static FONTS: OnceLock<[Option<SystemFont>; 3]> = OnceLock::new();
    let system = FONTS.get_or_init(|| {
        let directory = std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join("Fonts");
        let load = |file: &str, index| {
            FontBytes::load(&directory.join(file)).map(|bytes| SystemFont { bytes, index })
        };
        [
            load("msyh.ttc", 1).or_else(|| load("simhei.ttf", 0)),
            load("segoeui.ttf", 0),
            load("consola.ttf", 0),
        ]
    });
    use egui::FontFamily::{Monospace, Proportional};
    let mut fonts = egui::FontDefinitions::default();
    for ((name, families), face) in [
        ("system-cjk", &[Proportional, Monospace][..]),
        ("system-ui", &[Proportional][..]),
        ("system-mono", &[Monospace][..]),
    ]
    .into_iter()
    .zip(system)
    {
        if let Some(face) = face {
            let mut font = egui::FontData::from_static(face.bytes.as_slice());
            font.index = face.index;
            insert_font(&mut fonts, name, font, families);
        }
    }
    context.set_fonts(fonts);
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
