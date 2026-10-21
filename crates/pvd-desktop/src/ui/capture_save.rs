use super::*;

fn raw_detail(studio: &Studio) -> &'static str {
    let language = studio.prefs.language;
    if studio.save_job.is_some() || studio.recording_export.is_some() {
        language.text(
            "Wait for the current save to finish.",
            "请等待当前保存任务完成。",
        )
    } else if studio
        .timeline_recording()
        .is_some_and(|recording| recording.source == pvd_core::RecordingSource::Pvd3Database)
    {
        language.text(
            "Saving original PVD3 containers is not supported.",
            "暂不支持另存原版 PVD3 容器。",
        )
    } else if studio.is_project_capture() {
        language.text(
            "This project capture does not contain the original SDK stream.",
            "项目捕获不包含完整 SDK 原始数据流。",
        )
    } else if studio.live_connected {
        language.text(
            "Raw stream copying requires the connection to end and a complete automatic recording. Save recording can save available history while receiving.",
            "原始流另存需要连接结束，并有完整的自动录制文件；接收期间可用“保存录制”保存当前历史。",
        )
    } else if studio.capture_path.as_ref().is_some_and(|path| {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("partial"))
    }) {
        language.text(
            "The raw recording is incomplete (.partial). Available history can still be saved as a project capture.",
            "原始录制未完整结束（.partial）；仍可将可用历史保存为项目捕获。",
        )
    } else if studio.can_save_capture() {
        language.text(
            "Copy the complete SDK stream to a new .pvdstream file.",
            "将完整 SDK 原始流另存为新的 .pvdstream 文件。",
        )
    } else {
        language.text(
            "No complete SDK stream is available. Automatic raw recording must be enabled before listening; available history can be saved independently.",
            "当前没有完整 SDK 原始流。自动原始录制须在监听前启用；历史录制可单独保存。",
        )
    }
}

pub(crate) fn show(ui: &mut egui::Ui, studio: &mut Studio) {
    let language = studio.prefs.language;
    let history = studio.recording_saves_history();
    let enabled = studio.can_save_recording();
    let detail = if studio.save_job.is_some() || studio.recording_export.is_some() {
        language
            .text(
                "A save is already running.",
                "保存任务正在进行，请等待完成。",
            )
            .to_owned()
    } else if history {
        language.text(
            &format!("Save available frames {}–{} as .pvdcapture. Previously expired history cannot be recovered. TCP reception continues.", studio.first_frame(), studio.count().saturating_sub(1)),
            &format!("将当前可用的 {}–{} 帧保存为 .pvdcapture；已淘汰历史无法补回，TCP 接收继续。", studio.first_frame(), studio.count().saturating_sub(1)),
        ).to_owned()
    } else {
        raw_detail(studio).to_owned()
    };
    let disabled_detail = if studio.job.is_some() {
        language
            .text(
                "Wait for the recording to finish opening.",
                "请等待录制打开完成。",
            )
            .to_owned()
    } else if history && !studio.has_history() {
        language
            .text(
                "Waiting for a completed history frame.",
                "等待历史中出现已完成的帧。",
            )
            .to_owned()
    } else {
        detail.clone()
    };
    ui.horizontal_wrapped(|ui| {
        let width =
            (ui.available_width() - if history { 290.0 } else { 150.0 }).clamp(120.0, 360.0);
        let path = if history {
            &mut studio.retained_path
        } else {
            &mut studio.export_path
        };
        ui.add(egui::TextEdit::singleline(path).desired_width(width))
            .on_hover_text(if history {
                language.text("Project capture (.pvdcapture)", "项目捕获（.pvdcapture）")
            } else {
                language.text("SDK stream (.pvdstream)", "SDK 原始流（.pvdstream）")
            });
        if ui
            .add_enabled(
                enabled && !studio.file_picker.is_busy(),
                egui::Button::new("…"),
            )
            .on_hover_text(language.text("Choose recording path", "选择录制保存位置"))
            .clicked()
        {
            studio.pick_file(
                if history {
                    PickerKind::SaveRetainedCapture
                } else {
                    PickerKind::SaveCapture
                },
                ui.ctx().clone(),
            );
        }
        if ui
            .add_enabled(
                enabled,
                egui::Button::new(language.text("Save capture", "保存录制")),
            )
            .on_hover_text(detail)
            .on_disabled_hover_text(disabled_detail)
            .clicked()
        {
            studio.save_recording(ui.ctx().clone());
        }
        if history {
            settings_menu(ui, language.text("Raw stream", "原始流"), |ui| {
                ui.add(egui::TextEdit::singleline(&mut studio.export_path).desired_width(280.0));
                if ui
                    .add_enabled(
                        !studio.file_picker.is_busy(),
                        egui::Button::new(language.text("Choose path…", "选择路径…")),
                    )
                    .clicked()
                {
                    studio.pick_file(PickerKind::SaveCapture, ui.ctx().clone());
                }
                let enabled = studio.can_save_capture()
                    && studio.save_job.is_none()
                    && studio.recording_export.is_none();
                let detail = raw_detail(studio);
                if ui
                    .add_enabled(
                        enabled,
                        egui::Button::new(language.text("Save raw stream as", "另存原始流")),
                    )
                    .on_hover_text(detail)
                    .on_disabled_hover_text(detail)
                    .clicked()
                {
                    studio.save_capture(ui.ctx().clone());
                }
                ui.label(egui::RichText::new(detail).small());
            });
        }
    });
}
