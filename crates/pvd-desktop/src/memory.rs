use pvd_core::analysis::view::{MemoryAction, MemoryRows, MemorySource, MemoryViewCursor};

use crate::studio::Studio;

#[derive(Default)]
pub struct PanelState<C: MemoryRows = MemoryViewCursor> {
    cursor: C,
    key: Option<(u64, usize)>,
    failed: Option<((u64, usize), String)>,
    pub events: bool,
    pub at_end: bool,
}

impl<C: MemoryRows> PanelState<C> {
    pub(crate) fn reset_source(&mut self) {
        self.cursor = C::default();
        self.key = None;
        self.failed = None;
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
            let mut cursor = self.cursor.clone();
            if let Err(error) = memory.seek(&mut cursor, frame) {
                let message = error.to_string();
                self.failed = Some((key, message.clone()));
                return Err(message);
            }
            self.cursor = cursor;
            self.key = Some(key);
        }
        self.failed = None;
        Ok(())
    }

    fn retry(&mut self) {
        self.failed = None;
    }
}

pub fn show(ui: &mut egui::Ui, studio: &mut Studio) {
    let language = studio.prefs.language;
    let Some(recording) = studio.recording.clone() else {
        ui.label(language.text(
            "Open a capture to inspect memory events.",
            "打开录制后可查看内存事件。",
        ));
        return;
    };
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
    let memory = recording.analysis.memory_snapshot();
    let summary = memory.summary();
    let first_retained = memory.first_retained_frame();
    if first_retained > 0 {
        ui.label(format!(
            "{} {first_retained}",
            language.text("Analysis retained from frame", "分析历史起始帧")
        ));
    }
    if memory.event_count() == 0 && summary.allocations == 0 && summary.frees == 0 {
        ui.label(language.text(
            "No memory events. Enable SDK memory instrumentation at capture time.",
            "没有内存事件。请在捕获时启用 SDK 内存事件采集。",
        ));
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.selectable_value(
            &mut studio.memory_panel.at_end,
            false,
            language.text("Current frame", "当前帧"),
        );
        ui.selectable_value(
            &mut studio.memory_panel.at_end,
            true,
            language.text("Capture end", "录制结束"),
        ).on_hover_text(language.text("Events are grouped by payload flush frame, not allocation time. Capture end includes flushes after the last scene frame.", "事件按数据刷新帧归属，不代表实际分配时刻。“录制结束”包含最后一个场景帧之后刷新的事件。"));
        ui.separator();
        ui.selectable_value(
            &mut studio.memory_panel.events,
            false,
            language.text("Live allocations", "存活分配"),
        );
        ui.selectable_value(
            &mut studio.memory_panel.events,
            true,
            language.text("Frame alloc / free", "本帧分配 / 释放"),
        );
    });
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
    let state = &studio.memory_panel.cursor;
    let churn = memory.churn(frame);
    ui.horizontal_wrapped(|ui| {
        ui.label(format!(
            "{} {frame}",
            language.text("Flush frame", "刷新帧")
        ));
        ui.label(format!(
            "{} {:.3} MiB · {} {}",
            language.text("Live", "存活"),
            state.live_bytes() as f64 / 1048576.0,
            state.live_count(),
            language.text("allocations", "项分配")
        ));
        ui.label(format!(
            "{} {:.3} MiB",
            language.text("Capture peak", "录制峰值"),
            summary.peak_bytes as f64 / 1048576.0
        ));
    });
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
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::click());
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
                        * (sample.received_frame.saturating_sub(first_frame) as f32 / frames)
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
    let x =
        rect.left() + rect.width() * (frame.saturating_sub(first_frame) as f32 / frames).min(1.0);
    ui.painter().line_segment(
        [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
        egui::Stroke::new(1.0, egui::Color32::WHITE),
    );
    if response.clicked()
        && let Some(point) = response.interact_pointer_pos()
    {
        studio.memory_panel.at_end = false;
        let target = first_frame
            + (((point.x - rect.left()) / rect.width()).clamp(0.0, 1.0) * frames).round() as usize;
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

#[cfg(test)]
mod tests;
