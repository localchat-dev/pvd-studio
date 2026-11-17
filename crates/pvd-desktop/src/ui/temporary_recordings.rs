use super::*;
use crate::studio::temporary_recordings::Status;

pub(super) fn show(ui: &mut egui::Ui, studio: &mut Studio) {
    let language = studio.prefs.language;
    let selected = studio.temporary_recordings.selected;
    let rows: Vec<_> = studio
        .temporary_recordings
        .entries()
        .map(|entry| {
            (
                entry.id,
                entry.session,
                entry.status,
                entry.frame_count(),
                entry.disk_bytes(),
                studio.can_delete_temporary_recording(entry.id),
            )
        })
        .collect();
    if rows.is_empty() {
        return;
    }
    let total_bytes = rows.iter().map(|row| row.4).fold(0u64, u64::saturating_add);
    egui::CollapsingHeader::new(localized_format!(
        language,
        "Temporary recordings · {}",
        "临时录制 · {}",
        rows.len()
    ))
    .id_salt("temporary-recordings-list")
    .default_open(true)
    .show(ui, |ui| {
        ui.label(
            egui::RichText::new(localized_format!(
                language,
                "{:.2} GiB · listed completed sessions",
                "{:.2} GiB · 列表内已结束会话",
                total_bytes as f64 / 1_073_741_824.0
            ))
            .small()
            .color(MUTED),
        )
        .on_hover_text(language.text(
            "Open a recording to replay or save it from the timeline. Automatic cleanup only removes ended recordings that are not being viewed or saved. Locked files can remain until a later cleanup. Normal exit releases temporary files; save recordings to keep them.",
            "打开录制后可回放，或在时间轴保存。自动清理只移除未在查看或保存的已结束录制；被占用的文件可能延后清理。正常退出会释放临时文件，需要保留的录制请保存。",
        ));
        ui.add(
            egui::Label::new(egui::RichText::new(localized_format!(
                language,
                "Cleanup: {} GiB / {} sessions; the newest, opened and saving recordings are kept.",
                "清理预算：{} GiB / {} 条；最新一条及查看、保存中的录制继续保留。",
                studio.prefs.temporary_budget_gib,
                studio.prefs.temporary_max_count,
            )).small().color(MUTED)).wrap(),
        );
        if total_bytes > (u64::from(studio.prefs.temporary_budget_gib) << 30)
            || rows.len() > studio.prefs.temporary_max_count as usize
        {
            ui.add(egui::Label::new(egui::RichText::new(language.text(
                "Cleanup target exceeded by protected recordings. Save or delete recordings to release space.",
                "受保护录制超出清理预算。请保存需要的录制，或手动删除以释放空间。",
            )).small()).wrap());
        }
        egui::ScrollArea::vertical()
            .id_salt("temporary-recordings-scroll")
            .max_height((ui.available_height() * 0.25).clamp(70.0, 180.0))
            .show(ui, |ui| {
                for (id, session, status, frames, bytes, can_delete) in rows.iter().rev().copied() {
                    ui.push_id(id, |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .selectable_label(
                                    selected == Some(id),
                                    localized_format!(language, "Recording {id}", "录制 {id}"),
                                )
                                .on_hover_text(localized_format!(
                                    language,
                                    "Open temporary recording · session {session}",
                                    "打开临时录制 · 会话 {session}"
                                ))
                                .clicked()
                            {
                                studio.request_ui_action(
                                    ui.ctx(),
                                    ("open-temporary-recording", id),
                                    UiAction::SelectTemporary(id),
                                );
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui
                                    .add_enabled(
                                        can_delete,
                                        egui::Button::new(language.text("Delete", "删除")).small(),
                                    )
                                    .on_disabled_hover_text(language.text(
                                        "Close this recording and wait for saving to finish before deleting it.",
                                        "请先关闭此录制，并等待保存完成后再删除。",
                                    ))
                                    .clicked()
                                {
                                    studio.request_ui_action(
                                        ui.ctx(),
                                        ("delete-temporary-recording", id),
                                        UiAction::DeleteTemporary(id),
                                    );
                                }
                            });
                        });
                        let state = match status {
                            Status::Completed => language.text("Ended", "已结束"),
                            Status::Interrupted => language.text("Interrupted", "已中断"),
                            Status::Closed => language.text("Closed", "已关闭"),
                        };
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(localized_format!(
                                    language,
                                    "{state} · {frames} frames · {:.1} MiB",
                                    "{state} · {frames} 帧 · {:.1} MiB",
                                    bytes as f64 / 1_048_576.0
                                ))
                                .small()
                                .color(MUTED),
                            )
                            .wrap(),
                        );
                        ui.add_space(3.0);
                    });
                }
            });
        ui.add(
            egui::Label::new(
                egui::RichText::new(language.text(
                    "Cleared on exit; save recordings to keep them.",
                    "退出后清理；需要保留的录制请保存。",
                ))
                .small()
                .color(MUTED),
            )
            .wrap(),
        );
    });
}
