use super::*;

#[cfg(test)]
mod tests;

pub(super) fn show(ui: &mut egui::Ui, studio: &mut Studio) {
    let Some(directory) = studio.room_directory() else {
        return;
    };
    if directory.is_empty() {
        return;
    }
    let entries: Vec<_> = directory.iter().cloned().collect();
    let omitted = directory.omitted_count();
    let language = studio.prefs.language;
    ui.add_space(5.0);
    ui.horizontal(|ui| {
        let title = format!(
            "{}  {}",
            language.text("Physics Scenes", "物理场景"),
            entries.len()
        );
        ui.label(egui::RichText::new(title).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button(language.text(
                    if studio.room_view.expanded {
                        "Collapse"
                    } else {
                        "Expand"
                    },
                    if studio.room_view.expanded {
                        "收起"
                    } else {
                        "展开"
                    },
                ))
                .clicked()
            {
                studio.request_ui_action(
                    ui.ctx(),
                    "room-list-expanded",
                    UiAction::RoomListExpanded(!studio.room_view.expanded),
                );
            }
        });
    });
    if studio.room_view.expanded {
        if entries.len() > 4 {
            ui.add(
                egui::TextEdit::singleline(&mut studio.room_view.filter)
                    .hint_text(language.text("Find scene", "搜索场景"))
                    .desired_width(f32::INFINITY),
            );
        }
        let filter = studio.room_view.filter.trim().to_lowercase();
        let rows: Vec<_> = entries
            .iter()
            .filter(|room| {
                (studio.room_view.show_destroyed
                    || room.destroyed_at_frame.is_none()
                    || studio.displayed_room() == Some(room.scene))
                    && (filter.is_empty()
                        || studio
                            .room_label(room.scene)
                            .to_lowercase()
                            .contains(&filter)
                        || format!("{:x}", room.scene.id)
                            .contains(filter.strip_prefix("0x").unwrap_or(&filter)))
            })
            .collect();
        let height = (ui.available_height() * 0.36).clamp(60.0, 210.0);
        egui::ScrollArea::vertical()
            .id_salt("room-list")
            .max_height(height)
            .show_rows(ui, 28.0, rows.len(), |ui, range| {
                for index in range {
                    let room = rows[index];
                    let has_history = studio.room_has_history(room.scene);
                    let status = if room.destroyed_at_frame.is_some() {
                        language.text("Destroyed", "已销毁")
                    } else if studio.viewing_temporary_recording() {
                        language.text("Ended", "已结束")
                    } else if studio.live_session.is_some() && !studio.live_connected {
                        language.text("Disconnected", "已断开")
                    } else if !has_history {
                        language.text("No history", "无历史")
                    } else if studio.live_connected {
                        language.text("Online", "在线")
                    } else {
                        language.text("Recorded", "录制")
                    };
                    let label = studio.room_label(room.scene);
                    let (rect, response) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 28.0),
                        egui::Sense::click(),
                    );
                    let selected = studio.displayed_room() == Some(room.scene);
                    if selected || response.hovered() {
                        ui.painter().rect_filled(
                            rect,
                            3.0,
                            if selected {
                                ui.visuals().selection.bg_fill
                            } else {
                                ui.visuals().widgets.hovered.bg_fill
                            },
                        );
                    }
                    let count = studio.room_view.counts.get(&room.scene).copied();
                    let summary = count
                        .filter(|_| room.destroyed_at_frame.is_none() && rect.width() >= 220.0)
                        .map_or_else(|| status.to_owned(), |count| format!("{count} · {status}"));
                    let split = rect.left() + rect.width() * 0.49;
                    let title_rect =
                        egui::Rect::from_min_max(rect.min, egui::pos2(split - 3.0, rect.bottom()));
                    let status_rect =
                        egui::Rect::from_min_max(egui::pos2(split + 3.0, rect.top()), rect.max);
                    ui.painter().with_clip_rect(title_rect).text(
                        rect.left_center() + egui::vec2(6.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        label,
                        egui::TextStyle::Body.resolve(ui.style()),
                        if selected {
                            ACCENT
                        } else {
                            ui.visuals().text_color()
                        },
                    );
                    ui.painter().with_clip_rect(status_rect).text(
                        rect.right_center() - egui::vec2(6.0, 0.0),
                        egui::Align2::RIGHT_CENTER,
                        summary,
                        egui::TextStyle::Small.resolve(ui.style()),
                        MUTED,
                    );
                    let detail = format!(
                        "{}\nPxScene 0x{:X} · {} {}\n{}\n{}",
                        studio.room_label(room.scene),
                        room.scene.id,
                        language.text("generation", "代际"),
                        room.scene.generation,
                        count.map_or_else(
                            || language
                                .text("No current object count", "暂无对象数量")
                                .into(),
                            |count| localized_format!(
                                language,
                                "{} rigid bodies · latest display snapshot",
                                "{} 个刚体 · 最近显示快照",
                                count
                            )
                        ),
                        if !has_history {
                            language.text("No retained complete frames", "无可用的完整帧历史")
                        } else if !studio.viewing_temporary_recording()
                            && studio.live_session.is_some()
                            && !studio.live_connected
                            && room.destroyed_at_frame.is_none()
                        {
                            language.text("Destruction was not observed", "未确认销毁")
                        } else {
                            status
                        }
                    );
                    if response.on_hover_text(detail).clicked() {
                        studio.request_ui_action(
                            ui.ctx(),
                            "room-selection",
                            UiAction::Room(Some(room.scene)),
                        );
                    }
                }
            });
        ui.horizontal(|ui| {
            if ui
                .selectable_label(
                    studio.displayed_room().is_none(),
                    language.text("All scenes", "全部场景"),
                )
                .on_hover_text(language.text(
                    "Includes drawing with unknown scene ownership.",
                    "包含无法确定场景归属的绘制。",
                ))
                .clicked()
            {
                studio.request_ui_action(ui.ctx(), "room-all", UiAction::Room(None));
            }
            if entries.iter().any(|room| room.destroyed_at_frame.is_some()) {
                let mut show_destroyed = studio.room_view.show_destroyed;
                if ui
                    .checkbox(&mut show_destroyed, language.text("Destroyed", "已销毁"))
                    .changed()
                {
                    studio.request_ui_action(
                        ui.ctx(),
                        "room-list-destroyed",
                        UiAction::RoomListDestroyed(show_destroyed),
                    );
                }
            }
        });
        if omitted > 0 {
            ui.small(localized_format!(
                language,
                "{} older scene summaries omitted",
                "已省略 {} 个场景摘要",
                omitted
            ));
        }
    }
    if studio.room_selection_pending()
        && let Some(room) = studio.room_view.requested
    {
        ui.small(localized_format!(
            language,
            "Waiting for {}…",
            "正在等待{}…",
            studio.room_label(room)
        ));
    }
    ui.separator();
}
