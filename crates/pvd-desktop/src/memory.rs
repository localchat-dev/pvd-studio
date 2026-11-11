use pvd_core::analysis::view::{MemoryAction, MemoryRows, MemorySource, MemoryViewCursor};

use crate::studio::Studio;

mod groups;
mod totals;

pub(crate) use totals::Position as TotalsPosition;

#[derive(Default)]
pub struct PanelState<C: MemoryRows = MemoryViewCursor> {
    cursor: C,
    key: Option<(u64, usize)>,
    failed: Option<((u64, usize), String)>,
    groups: groups::Cache,
    group_sites: bool,
    churn: pvd_core::analysis::MemoryChurn,
    pub events: bool,
    pub at_end: bool,
    source_totals: bool,
    totals_position: TotalsPosition,
    totals: totals::Cache,
}

impl<C: MemoryRows> PanelState<C> {
    pub(crate) fn show_events(&mut self) {
        self.totals.keep_selection();
        self.source_totals = false;
        self.events = true;
        self.group_sites = false;
    }

    pub(crate) fn show_groups(&mut self) {
        self.totals.keep_selection();
        self.source_totals = false;
        self.group_sites = true;
    }

    pub(crate) fn show_totals(&mut self, position: TotalsPosition) {
        self.totals.keep_selection();
        self.source_totals = true;
        self.totals_position = position;
    }

    pub(crate) fn reset_source(&mut self) {
        self.cursor = C::default();
        self.key = None;
        self.failed = None;
        self.groups.reset();
        self.churn = Default::default();
        self.totals.clear_values();
    }

    fn update(
        &mut self,
        memory: &impl MemorySource<Cursor = C>,
        frame: usize,
    ) -> Result<(), String> {
        let key = (memory.revision(), frame);
        if let Some((failed_key, error)) = &self.failed
            && *failed_key == key
        {
            return Err(error.clone());
        }
        if self.key != Some(key) {
            self.groups.reset();
            let mut cursor = self.cursor.clone();
            if let Err(error) = memory.seek(&mut cursor, frame) {
                let message = error.to_string();
                self.failed = Some((key, message.clone()));
                return Err(message);
            }
            self.cursor = cursor;
            self.churn = memory.churn(frame);
            self.key = Some(key);
        }
        self.failed = None;
        Ok(())
    }

    fn retry(&mut self) {
        self.failed = None;
    }
}

pub(crate) fn prepare_groups_render(studio: &mut Studio) -> Result<bool, String> {
    let Some(recording) = studio.recording.clone() else {
        return Ok(false);
    };
    let memory = recording.analysis.memory_snapshot();
    studio.memory_panel.update(&memory, studio.frame)?;
    studio.memory_panel.groups.update(&memory, studio.frame);
    studio.memory_panel.groups.ready().map_err(str::to_owned)
}

pub fn show(ui: &mut egui::Ui, studio: &mut Studio) {
    let language = studio.prefs.language;
    let Some(recording) = studio.recording.clone() else {
        studio.memory_panel.totals.reset();
        ui.label(language.text(
            "Open a capture to inspect memory.",
            "打开录制后可查看内存。",
        ));
        return;
    };
    let native = recording.source == pvd_core::RecordingSource::Pvd3Database;
    if native {
        let default = studio.memory_panel.totals.default_position(&recording);
        if !studio.memory_panel.source_totals
            && let Some(position) = default
        {
            studio.memory_panel.show_totals(position);
        }
    } else {
        studio.memory_panel.source_totals = false;
        studio.memory_panel.totals.reset();
    }
    show_toolbar(ui, studio, &recording, native);
    if studio.memory_panel.source_totals {
        totals::show(ui, studio, &recording);
        return;
    }
    let observed =
        recording.memory_history_scope() == pvd_core::MemoryHistoryScope::AfterInitialSnapshot;
    if observed {
        ui.label(language.text(
            "Only events in this file are known; earlier allocations are unavailable.",
            "仅统计文件内事件，起点前分配未知。",
        ));
    }
    crate::recording_export::show_analysis_controls(
        ui,
        studio,
        pvd_core::analysis_export::AnalysisCsvKind::Memory,
    );
    if !studio.ensure_current_analysis(ui) {
        return;
    }
    if let Some(detail) = recording
        .diagnostics
        .iter()
        .find(|message| message.starts_with("Analysis decoding stopped at frame"))
    {
        ui.colored_label(
            egui::Color32::YELLOW,
            language.text(
                "Analysis is unavailable; scene decoding continues.",
                "分析数据不可用；场景解码继续。",
            ),
        );
        ui.label(detail);
        return;
    }
    let analysis = if studio.memory_panel.at_end {
        recording
            .native_end_analysis()
            .unwrap_or(&recording.analysis)
    } else {
        &recording.analysis
    };
    let memory = analysis.memory_snapshot();
    let summary = memory.summary();
    let first_retained = memory.first_retained_frame();
    if first_retained > 0 {
        ui.label(format!(
            "{} {first_retained}",
            language.text("Analysis retained from frame", "分析历史起始帧")
        ));
    }
    if memory.event_count() == 0 && summary.allocations == 0 && summary.frees == 0 {
        if native {
            ui.label(language.text(
                "No observable memory events. Source totals can be viewed separately.",
                "没有可观察的内存事件，可切换“来源总量”查看保存值。",
            ));
        } else {
            ui.label(language.text(
                "No memory events. Enable SDK memory instrumentation at capture time.",
                "没有内存事件。请在捕获时启用 SDK 内存事件采集。",
            ));
        }
        return;
    }
    let frame = if studio.memory_panel.at_end {
        memory.last_event_frame().unwrap_or(first_retained)
    } else {
        studio.frame
    };
    if frame < first_retained {
        ui.label(language.text("Memory events for this frame are outside the loaded analysis window. Seek the frame again to read its disk history.", "该帧的内存事件不在已加载的分析窗口中。请重新跳转到该帧以读取磁盘历史。"));
        return;
    }
    if let Err(error) = studio.memory_panel.update(&memory, frame) {
        ui.colored_label(
            egui::Color32::YELLOW,
            language.text(
                "Memory frame could not be loaded.",
                "无法加载该帧内存数据。",
            ),
        );
        ui.label(error);
        if ui
            .button(language.text("Retry memory frame", "重试加载内存帧"))
            .clicked()
        {
            studio.memory_panel.retry();
            ui.ctx().request_repaint();
        }
        return;
    }
    if studio.memory_panel.group_sites {
        ui.label(format!(
            "{} {frame} · {} {}",
            language.text("Flush frame", "刷新帧"),
            studio.memory_panel.churn.allocations,
            language.text("allocations", "项分配"),
        ));
        show_groups(
            ui,
            &mut studio.memory_panel.groups,
            &memory,
            frame,
            language,
        );
        return;
    }
    let state = &studio.memory_panel.cursor;
    let churn = &studio.memory_panel.churn;
    ui.horizontal_wrapped(|ui| {
        ui.label(format!(
            "{} {frame}",
            language.text("Flush frame", "刷新帧")
        ));
        ui.label(format!(
            "{} {:.3} MiB · {} {}",
            if observed {
                language.text("Known live", "已知存活")
            } else {
                language.text("Live", "存活")
            },
            state.live_bytes() as f64 / 1048576.0,
            state.live_count(),
            language.text("allocations", "项分配")
        ));
        ui.label(format!(
            "{} {:.3} MiB",
            if observed {
                language.text("Known peak", "已知峰值")
            } else {
                language.text("Capture peak", "录制峰值")
            },
            summary.peak_bytes as f64 / 1048576.0
        ));
    });
    ui.label(format!(
        "{}: {} B · {} {}",
        language.text("Churn", "本帧新增存活"),
        churn.surviving_bytes,
        churn.surviving_allocations,
        language.text("allocations", "项分配"),
    ))
    .on_hover_text(language.text(
        "Allocated in this flush frame and still live at its end. Releases of older allocations affect net change, not churn. Flush frames do not identify the actual allocation time.",
        "在本刷新帧分配，且帧末仍存活。释放早期分配只影响净变化，不影响本项。刷新帧不代表实际分配时刻。",
    ));
    let target = egui::CollapsingHeader::new(language.text("Activity and trend", "活动与趋势"))
        .id_salt("memory-activity")
        .show(ui, |ui| {
            ui.label(format!(
                "{}: +{} B / {} · −{} B / {} · {} {:+} B",
                language.text("Frame activity", "本帧活动"),
                churn.allocated_bytes,
                churn.allocations,
                churn.freed_bytes,
                churn.frees,
                language.text("net", "净变化"),
                churn.live_byte_change()
            ));
            if churn.unmatched_frees > 0 || churn.replaced_addresses > 0 {
                ui.label(format!(
                    "{} {} · {} {} ({} B)",
                    churn.unmatched_frees,
                    language.text("unknown frees", "次未知释放"),
                    churn.replaced_addresses,
                    language.text("live address replacements", "次存活地址替换"),
                    churn.replaced_bytes
                ));
            }
            let first_frame = recording.first_frame().max(first_retained);
            let frames = recording
                .frame_count()
                .saturating_sub(first_frame + 1)
                .max(1) as f32;
            let (rect, response) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::click());
            ui.painter()
                .rect_filled(rect, 2.0, egui::Color32::from_rgb(16, 23, 31));
            let peak = summary.peak_bytes.max(1) as f32;
            let points: Vec<_> = (0..memory.sample_count())
                .step_by(memory.sample_count().div_ceil(1000).max(1))
                .filter_map(|index| memory.sample_at(index))
                .map(|sample| {
                    egui::pos2(
                        rect.left()
                            + rect.width()
                                * (sample.received_frame.saturating_sub(first_frame) as f32
                                    / frames)
                                    .min(1.0),
                        rect.bottom() - rect.height() * sample.live_bytes as f32 / peak,
                    )
                })
                .collect();
            if points.len() > 1 {
                ui.painter().add(egui::Shape::line(
                    points,
                    egui::Stroke::new(1.5, egui::Color32::from_rgb(89, 207, 221)),
                ));
            }
            let x = rect.left()
                + rect.width() * (frame.saturating_sub(first_frame) as f32 / frames).min(1.0);
            ui.painter().line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                egui::Stroke::new(1.0, egui::Color32::WHITE),
            );
            if response.clicked()
                && let Some(point) = response.interact_pointer_pos()
            {
                Some(
                    first_frame
                        + (((point.x - rect.left()) / rect.width()).clamp(0.0, 1.0) * frames)
                            .round() as usize,
                )
            } else {
                None
            }
        })
        .body_returned
        .flatten();
    if let Some(target) = target {
        studio.memory_panel.at_end = false;
        studio.request_ui_action(
            ui.ctx(),
            ui.id().with("memory-frame"),
            crate::studio::UiAction::Locate(target, crate::studio::DisplayReason::Memory),
        );
        ui.ctx().request_repaint();
    }
    let events = memory.frame_events(frame);
    let count = if studio.memory_panel.events {
        events.len()
    } else {
        studio.memory_panel.cursor.live_count()
    };
    egui::ScrollArea::vertical()
        .id_salt("memory-detail")
        .show_rows(ui, 23.0, count, |ui, range| {
            for row in range {
                let (action, address, allocation) = if studio.memory_panel.events {
                    let Some(event) = memory.event_at(events.start + row) else {
                        continue;
                    };
                    let action = match event.action {
                        MemoryAction::Allocate => language.text("Alloc", "分配"),
                        MemoryAction::Replace => language.text("Replace", "替换"),
                        MemoryAction::Free => language.text("Free", "释放"),
                    };
                    (action, event.address, event.allocation)
                } else {
                    let Some(allocation) = studio.memory_panel.cursor.allocation_at(row) else {
                        continue;
                    };
                    (
                        language.text("Live", "存活"),
                        allocation.address,
                        Some(allocation),
                    )
                };
                ui.horizontal(|ui| {
                    ui.add_sized([45.0, 20.0], egui::Label::new(action));
                    ui.monospace(format!("0x{address:016x}"));
                    if let Some(allocation) = allocation {
                        ui.add_sized(
                            [80.0, 20.0],
                            egui::Label::new(format!("{} B", allocation.size)).truncate(),
                        );
                        ui.add(
                            egui::Label::new(format!(
                                "{} · {}:{}",
                                allocation.type_name, allocation.file, allocation.line
                            ))
                            .truncate(),
                        )
                        .on_hover_text(format!(
                            "{}\n{}:{}\n{} {}",
                            allocation.type_name,
                            allocation.file,
                            allocation.line,
                            language.text("Allocated in flush frame", "分配刷新帧"),
                            allocation.received_frame
                        ));
                    } else {
                        ui.label(language.text("Unknown allocation", "未知分配"));
                    }
                });
            }
        });
}

fn show_toolbar(
    ui: &mut egui::Ui,
    studio: &mut Studio,
    recording: &pvd_core::Recording,
    native: bool,
) {
    let language = studio.prefs.language;
    ui.horizontal_wrapped(|ui| {
        if studio.memory_panel.source_totals {
            totals::select_position(ui, &mut studio.memory_panel.totals_position, recording.frame_count(), language);
        } else {
            ui.selectable_value(&mut studio.memory_panel.at_end, false, language.text("Current frame", "当前帧"));
            ui.selectable_value(&mut studio.memory_panel.at_end, true, language.text("Capture end", "录制结束"))
                .on_hover_text(language.text("Events follow flush frames. Capture end includes flushes after the last scene frame.", "事件按刷新帧归属；录制结束包含末帧后的刷新。"));
        }
        ui.separator();
        let panel = &mut studio.memory_panel;
        if ui.selectable_label(!panel.source_totals && !panel.group_sites && !panel.events,
            language.text("Live allocations", "存活分配")).clicked() {
            panel.source_totals = false; panel.events = false; panel.group_sites = false;
        }
        if ui.selectable_label(!panel.source_totals && !panel.group_sites && panel.events,
            language.text("Frame alloc / free", "本帧分配 / 释放")).clicked() { panel.show_events(); }
        if ui.selectable_label(!panel.source_totals && panel.group_sites,
            language.text("Frame sources", "本帧来源")).on_hover_text(language.text(
            "Frame allocations grouped by file, line and type.", "按文件名、行号和类型汇总本帧分配。")).clicked() { panel.show_groups(); }
        if native && ui.selectable_label(panel.source_totals, language.text("Source totals", "来源总量"))
            .on_hover_text(language.text("Saved source values. Initial/end values have no frame or timestamp; current values belong to the displayed state, not a known receive time.", "保存的来源值。起点和结束值没有帧号或时间；当前值来自显示状态，不代表已知接收时间。"))
            .clicked() {
            panel.source_totals = true;
        }
    });
}

pub(crate) fn prepare_totals_render(studio: &mut Studio) -> Result<(), String> {
    totals::prepare_render(studio)
}

fn show_groups(
    ui: &mut egui::Ui,
    cache: &mut groups::Cache,
    memory: &impl MemorySource,
    frame: usize,
    language: crate::i18n::Language,
) {
    cache.update(memory, frame);
    if let Some((done, total)) = cache.progress() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(format!(
                "{} {done}/{total}",
                language.text("Grouping", "正在汇总")
            ));
            if ui.button(language.text("Cancel", "取消")).clicked() {
                cache.cancel();
            }
        });
        ui.ctx().request_repaint();
        return;
    }
    if cache.cancelled || cache.error.is_some() {
        ui.label(if cache.cancelled {
            language.text("Grouping cancelled.", "已取消汇总。")
        } else {
            language.text(
                "This frame could not be grouped. Event details remain available.",
                "无法汇总该帧，可查看事件明细。",
            )
        });
        if let Some(error) = cache.error {
            ui.label(error);
        }
        if ui.button(language.text("Retry", "重试")).clicked() {
            cache.reset();
            ui.ctx().request_repaint();
        }
        return;
    }
    if cache.rows.is_empty() {
        ui.label(language.text("No allocations in this frame.", "该帧没有新增分配。"));
        return;
    }
    let column = ((ui.available_width() - 180.0) / 3.0).clamp(95.0, 160.0);
    let source_width = (ui.available_width() - column * 3.0 - 24.0).max(120.0);
    ui.horizontal(|ui| {
        for (width, title) in [
            (source_width, language.text("Source", "来源")),
            (column, language.text("Allocated", "本帧分配")),
            (column, language.text("Freed in frame", "同帧释放")),
            (column, language.text("Churn", "新增存活")),
        ] {
            ui.add_sized([width, 20.0], egui::Label::new(title));
        }
    });
    egui::ScrollArea::vertical().id_salt("memory-groups").show_rows(ui, 23.0, cache.rows.len(), |ui, range| {
        for index in range {
            let group = &cache.rows[index];
            ui.horizontal(|ui| {
                let response = ui.add_sized([source_width, 20.0], egui::Label::new(&group.label).truncate());
                response.on_hover_ui(|ui| {
                    ui.label(&group.file);
                    if group.multiple_files {
                        ui.label(language.text("Includes matching file names in other directories. Full paths are available in event details.", "包含其他目录中的同名文件。完整路径可在事件明细中查看。"));
                    }
                });
                for (bytes, count) in [
                    (group.allocated_bytes, group.allocations),
                    (group.freed_bytes, group.frees),
                    (group.surviving_bytes, group.survivors),
                ] {
                    ui.add_sized([column, 20.0], egui::Label::new(format!("{bytes} B ({count})")));
                }
            });
        }
    });
}

#[cfg(test)]
mod tests;
