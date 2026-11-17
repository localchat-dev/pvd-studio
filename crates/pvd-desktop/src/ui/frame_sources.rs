use super::*;
use pvd_core::frame_sources::{FrameSection, FrameSourceKey};

pub(super) fn show(ui: &mut egui::Ui, studio: &mut Studio) {
    let Some(recording) = studio.timeline_recording() else {
        return;
    };
    let sources = recording
        .frame_sources
        .sources()
        .map(|source| source.source())
        .collect::<Vec<_>>();
    if sources.len() < 2 {
        return;
    }
    if !recording.rooms.is_empty()
        && sources.iter().all(|source| {
            matches!(
                source,
                FrameSourceKey::Captured {
                    section: FrameSection::PhysX,
                    ..
                }
            )
        })
    {
        return;
    }
    let Some(current) = studio.frame_source() else {
        return;
    };
    let language = studio.prefs.language;
    let mut selected = current;
    egui::ComboBox::from_id_salt("frame-source")
        .width(140.0)
        .selected_text(label(current, language))
        .show_ui(ui, |ui| {
            for source in sources {
                ui.selectable_value(&mut selected, source, label(source, language));
            }
        })
        .response
        .on_hover_text(language.text(
            "Frame source. Playback follows this source's completed frames; switching aligns by captured event order.",
            "帧源。沿所选来源的完整帧回放；切换时按捕获事件顺序定位。",
        ));
    if selected != current {
        studio.request_ui_action(
            ui.ctx(),
            "frame-source-selection",
            UiAction::FrameSource(selected),
        );
    }
}

fn label(source: FrameSourceKey, language: Language) -> String {
    match source {
        FrameSourceKey::Captured { section, source } => {
            let name = match section {
                FrameSection::PhysX => "PhysX",
                FrameSection::Apex => "APEX",
            };
            format!("{name} · 0x{source:X}")
        }
        FrameSourceKey::Legacy => language.text("Recording frames", "录制帧").into(),
    }
}
