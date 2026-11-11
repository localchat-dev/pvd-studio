use super::*;
use pvd_core::native::export::Selection;

#[cfg(test)]
mod tests;

impl Studio {
    pub fn is_native_recording(&self) -> bool {
        self.recording
            .as_ref()
            .is_some_and(|recording| recording.source == pvd_core::RecordingSource::Pvd3Database)
    }

    fn can_export_native(&self, selection: Selection) -> bool {
        self.is_native_recording()
            && !self.exit_requested
            && self.job.is_none()
            && self.server.is_none()
            && self.save_job.is_none()
            && self.recording_export.is_none()
            && self.has_native_export_source(selection)
    }

    fn has_native_export_source(&self, selection: Selection) -> bool {
        let within_limit =
            |bytes| matches!(selection, Selection::Whole) || bytes <= MAX_CAPTURE_BYTES;
        self.bytes.as_ref().is_some_and(|bytes| {
            within_limit(bytes.len() as u64)
                && bytes.starts_with(&pvd_core::native::MAGIC.to_le_bytes())
        }) || (self.capture_path.is_some()
            && self.source_identity.as_ref().is_some_and(|identity| {
                identity
                    .fingerprint()
                    .is_some_and(|(_, bytes, _)| within_limit(bytes))
            }))
    }

    fn export_native(&mut self, selection: Selection, context: egui::Context) {
        if !self.can_export_native(selection) {
            return;
        }
        if !Path::new(&self.native_export_path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pxd2"))
        {
            self.notice(Message::new(
                "Save PVD3 recordings as .pxd2.",
                "PVD3 录制请保存为 .pxd2。",
            ));
            return;
        }
        if let Selection::Frames { first, last } = selection
            && (first > last || last >= self.count())
        {
            return;
        }
        match crate::recording_export::Job::start(
            self.export_source(),
            self.native_export_path.clone().into(),
            crate::recording_export::Kind::Native(selection),
            context,
            &self.export_workers,
        ) {
            Ok(job) => self.recording_export = Some(job),
            Err(message) => self.notice(message),
        }
    }

    pub fn native_export_controls(&mut self, ui: &mut egui::Ui) {
        let language = self.prefs.language;
        let available = self.can_export_native(Selection::Whole);
        let clip_available = self.can_export_native(Selection::Frames {
            first: self.trim_first,
            last: self.trim_last,
        });
        egui::CollapsingHeader::new(language.text("PVD3 recording", "PVD3 录制"))
            .default_open(true)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(language.text("Output", "输出"));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.native_export_path)
                            .desired_width(260.0),
                    );
                    if ui
                        .add_enabled(
                            !self.file_picker.is_busy(),
                            egui::Button::new(language.text("Choose path…", "选择路径…")),
                        )
                        .clicked()
                    {
                        self.pick_file(
                            crate::file_picker::PickerKind::SaveNativeCapture,
                            ui.ctx().clone(),
                        );
                    }
                    if ui
                        .add_enabled(
                            available,
                            egui::Button::new(language.text("Save whole recording", "保存整份录制")),
                        )
                        .clicked()
                    {
                        self.export_native(Selection::Whole, ui.ctx().clone());
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(language.text("Keep frames", "保留帧"));
                    let last = self.count().saturating_sub(1);
                    self.trim_first = self.trim_first.min(last);
                    self.trim_last = self.trim_last.clamp(self.trim_first, last);
                    ui.add(egui::DragValue::new(&mut self.trim_first).range(0..=self.trim_last));
                    ui.label("→");
                    ui.add(egui::DragValue::new(&mut self.trim_last).range(self.trim_first..=last));
                    if ui
                        .add_enabled(
                            clip_available && self.count() > 0,
                            egui::Button::new(language.text("Save clip", "保存片段")),
                        )
                        .on_hover_text(language.text(
                            "The selected interval must be recoverable on its own. If it depends on earlier data, save from frame 0. Events after the last kept frame are excluded.",
                            "所选区间必须能独立恢复；若依赖更早的数据，请从第 0 帧保存。末帧之后的事件不保留。",
                        ))
                        .clicked()
                    {
                        self.export_native(
                            Selection::Frames {
                                first: self.trim_first,
                                last: self.trim_last,
                            },
                            ui.ctx().clone(),
                        );
                    }
                });
                if !self.has_native_export_source(Selection::Whole) {
                    ui.label(language.text(
                        "Native export requires the complete source file.",
                        "原版导出需要完整源文件。",
                    ));
                } else if !self.has_native_export_source(Selection::Frames { first: 0, last: 0 }) {
                    ui.label(language.text(
                        "This recording can be saved whole. Clip export supports source files up to 512 MiB.",
                        "可保存整份录制；片段导出支持不超过 512 MiB 的源文件。",
                    ));
                } else if self.server.is_some() {
                    ui.label(language.text(
                        "Stop listening before exporting.",
                        "请先停止监听再导出。",
                    ));
                }
                crate::recording_export::show_progress(ui, self);
            });
    }
}
