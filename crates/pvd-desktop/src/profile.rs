use crate::{
    i18n::{Language, Message},
    studio::Studio,
};
use bevy_egui::egui;
use pvd_core::{
    Recording,
    analysis::{ProfileSpan, view::ProfileSource},
    profile_curves::{CatalogEntry, ExactKey},
};
use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, BinaryHeap},
    ops::Range,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

#[cfg(all(test, windows))]
pub(crate) mod analysis_probe;
#[cfg(test)]
mod interaction_tests;
#[cfg(test)]
mod lifecycle_tests;
mod pages;
mod refresh;
mod timeline;

pub(crate) fn sync_session(context: &egui::Context, epoch: u64) -> bool {
    context.data_mut(|data| {
        let id = egui::Id::new("profile-cache-session");
        if data.get_temp::<u64>(id) == Some(epoch) {
            return false;
        }
        // Hidden panels cannot observe a source change through their draw path.
        data.remove_by_type::<ViewState>();
        data.remove_by_type::<EventView>();
        data.remove_by_type::<SpanMenu>();
        timeline::clear_session(data);
        data.insert_temp(id, epoch);
        true
    })
}

pub(crate) fn prepare_first_start_render(
    context: &egui::Context,
    studio: &Studio,
) -> Result<bool, Message> {
    if !studio
        .complete_profile_source()
        .is_some_and(|source| source.can_rebuild_cpu_index())
    {
        return Err(Message::new(
            "First-start catalog render checks require an opened indexed SDK file.",
            "首次开始时间目录渲染检查需要已打开且可建立索引的 SDK 文件。",
        ));
    }
    pages::prepare_render(context, studio.profile_view_epoch)
}

pub(crate) fn prepare_thread_window_render(
    context: &egui::Context,
    studio: &Studio,
) -> Result<bool, Message> {
    if studio.complete_timeline_source().is_none() {
        return Err(Message::new(
            "Thread-window render checks require an opened complete recording.",
            "线程时间窗渲染检查需要已打开的完整录制。",
        ));
    }
    timeline::prepare_render(context, studio.profile_view_epoch)
}

use pages::CatalogPage;

const MUTED: egui::Color32 = egui::Color32::from_rgb(151, 167, 184);
const ACCENT: egui::Color32 = egui::Color32::from_rgb(78, 183, 189);
const ROW_HEIGHT: f32 = 24.0;

#[derive(Clone)]
enum Row {
    Thread {
        id: u32,
        spans: usize,
    },
    Lane {
        thread: u32,
        level: usize,
        spans: Vec<usize>,
    },
}

#[derive(Default)]
struct Layout {
    rows: Arc<Vec<Row>>,
    labels: BTreeMap<(u64, u16), String>,
    first: u64,
    last: u64,
    threads: usize,
}

impl Layout {
    #[cfg(test)]
    fn build(analysis: &impl ProfileSource) -> Self {
        Self::build_cancellable(analysis, &AtomicBool::new(false)).unwrap()
    }

    fn build_cancellable(
        analysis: &impl ProfileSource,
        cancel: &AtomicBool,
    ) -> Result<Self, Message> {
        let check = || {
            if cancel.load(Ordering::Relaxed) {
                Err(Message::new("Thread layout cancelled", "线程布局已取消"))
            } else {
                Ok(())
            }
        };
        let mut by_thread = BTreeMap::<u32, Vec<usize>>::new();
        for (index, span) in analysis.profile_spans().enumerate() {
            check()?;
            by_thread.entry(span.thread).or_default().push(index);
        }
        let mut rows = Vec::new();
        for (thread, mut indices) in by_thread {
            check()?;
            indices.sort_unstable_by_key(|index| {
                let span = profile_span(analysis, *index);
                (span.start, Reverse(span.end), *index)
            });
            rows.push(Row::Thread {
                id: thread,
                spans: indices.len(),
            });
            let mut lanes = Vec::<Vec<usize>>::new();
            let mut active = BinaryHeap::<Reverse<(u64, usize)>>::new();
            let mut free = BTreeSet::new();
            for index in indices {
                check()?;
                let span = profile_span(analysis, index);
                while let Some(Reverse((end, level))) = active.peek().copied() {
                    if end > span.start {
                        break;
                    }
                    active.pop();
                    free.insert(level);
                }
                let level = free.pop_first().unwrap_or_else(|| {
                    lanes.push(Vec::new());
                    lanes.len() - 1
                });
                lanes[level].push(index);
                active.push(Reverse((span.end, level)));
            }
            rows.extend(
                lanes
                    .into_iter()
                    .enumerate()
                    .map(|(level, spans)| Row::Lane {
                        thread,
                        level,
                        spans,
                    }),
            );
        }
        check()?;
        Ok(Self {
            threads: rows
                .iter()
                .filter(|row| matches!(row, Row::Thread { .. }))
                .count(),
            rows: Arc::new(rows),
            labels: analysis
                .profile_names()
                .map(|names| {
                    (
                        (names.zone, names.event),
                        format!("{} / {}", names.zone_name, names.event_name),
                    )
                })
                .collect(),
            first: analysis
                .profile_spans()
                .map(|span| span.start)
                .min()
                .unwrap_or(0),
            last: analysis
                .profile_spans()
                .map(|span| span.end)
                .max()
                .unwrap_or(0),
        })
    }

    fn filtered(&self, analysis: &impl ProfileSource, filter: &str) -> Arc<Vec<Row>> {
        if filter.is_empty() {
            return self.rows.clone();
        }
        let events: BTreeSet<_> = self
            .labels
            .iter()
            .filter(|(_, label)| label.to_lowercase().contains(filter))
            .map(|(key, _)| *key)
            .collect();
        let mut result = Vec::new();
        let mut header = None;
        for row in self.rows.iter() {
            match row {
                Row::Thread { .. } => header = Some(row.clone()),
                Row::Lane {
                    thread,
                    level,
                    spans,
                } => {
                    let thread_matches = thread.to_string().contains(filter);
                    let spans: Vec<_> = spans
                        .iter()
                        .copied()
                        .filter(|index| {
                            let span = profile_span(analysis, *index);
                            thread_matches || events.contains(&(span.zone, span.event))
                        })
                        .collect();
                    if !spans.is_empty() {
                        if let Some(header) = header.take() {
                            result.push(header);
                        }
                        result.push(Row::Lane {
                            thread: *thread,
                            level: *level,
                            spans,
                        });
                    }
                }
            }
        }
        Arc::new(result)
    }
}

#[derive(Clone, Default)]
struct ViewState {
    cache: refresh::Cache<Layout>,
    layout: Arc<Layout>,
    filter: String,
    rows: Arc<Vec<Row>>,
    measure: bool,
    selection: Option<[f64; 2]>,
    anchor: Option<f64>,
}

fn build_layout(recording: &Recording, cancel: &AtomicBool) -> Result<Layout, Message> {
    Layout::build_cancellable(&recording.analysis, cancel)
}

fn build_catalog(recording: &Recording, cancel: &AtomicBool) -> Result<CatalogPage, Message> {
    pvd_core::profile_curves::catalog(recording, cancel)
        .map(|entries| CatalogPage {
            entries,
            next: None,
        })
        .map_err(|error| {
            Message::detail(
                "Profile event catalog failed",
                "性能事件列表索引失败",
                error,
            )
        })
}

fn snapshot_label(ui: &mut egui::Ui, language: Language, frame: usize, following: bool) {
    if following {
        ui.label(
            egui::RichText::new(format!(
                "{} {frame} · {}",
                language.text("Analysis snapshot frame", "分析快照帧"),
                language.text(
                    "updates up to 5 Hz; scene updates independently",
                    "最多每秒更新 5 次；场景独立刷新"
                )
            ))
            .small()
            .color(MUTED),
        );
    }
}

fn time_window(window: [f64; 2], total: f64) -> [f64; 2] {
    if !window.iter().all(|value| value.is_finite()) {
        return [0.0, total];
    }
    let width = (window[1] - window[0]).clamp(1e-6_f64.min(total), total);
    let start = window[0].clamp(0.0, (total - width).max(0.0));
    [start, start + width]
}

fn profile_span(analysis: &impl ProfileSource, index: usize) -> &ProfileSpan {
    analysis
        .profile_span(index)
        .expect("profile layout index belongs to its prepared snapshot")
}

fn visible_range(
    indices: &[usize],
    analysis: &impl ProfileSource,
    ticks: [u64; 2],
) -> Range<usize> {
    let first = indices.partition_point(|index| profile_span(analysis, *index).end < ticks[0]);
    let end = indices.partition_point(|index| profile_span(analysis, *index).start <= ticks[1]);
    first.min(end)..end
}

fn bucket_end(indices: &[usize], analysis: &impl ProfileSource, at: usize, tick: u64) -> usize {
    at + indices[at..]
        .partition_point(|index| profile_span(analysis, *index).start <= tick)
        .max(1)
}

fn frame_window(
    analysis: &impl ProfileSource,
    frame: usize,
    origin: u64,
    ms_per_tick: f64,
    total: f64,
) -> [f64; 2] {
    let mut bounds = None::<[u64; 2]>;
    for span in analysis
        .profile_spans()
        .filter(|span| span.received_frame == frame)
    {
        let range = bounds.get_or_insert([span.start, span.end]);
        range[0] = range[0].min(span.start);
        range[1] = range[1].max(span.end);
    }
    bounds.map_or([0.0, total], |range| {
        time_window(
            range.map(|tick| tick.saturating_sub(origin) as f64 * ms_per_tick),
            total,
        )
    })
}

fn interact(
    ui: &egui::Ui,
    response: &egui::Response,
    rect: egui::Rect,
    window: &mut [f64; 2],
    total: f64,
    state: &mut ViewState,
    axis: bool,
) {
    if ui.ctx().wants_keyboard_input() || rect.width() < 1.0 {
        return;
    }
    let fraction =
        |point: egui::Pos2| ((point.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64;
    let width = window[1] - window[0];
    let measuring = state.measure || ui.input(|input| input.modifiers.shift);
    if response.drag_started() && measuring {
        let start = ui
            .input(|input| input.pointer.press_origin())
            .or_else(|| response.interact_pointer_pos());
        state.anchor = start.map(|point| window[0] + fraction(point) * width);
    }
    if response.dragged() {
        if let Some(anchor) = state.anchor {
            if let Some(point) = response.interact_pointer_pos() {
                let end = window[0] + fraction(point) * width;
                state.selection = Some([anchor.min(end), anchor.max(end)]);
            }
        } else {
            let delta =
                ui.input(|input| input.pointer.delta().x) as f64 / rect.width() as f64 * width;
            let start = (window[0] - delta).clamp(0.0, (total - width).max(0.0));
            *window = [start, start + width];
        }
    }
    if response.drag_stopped() {
        state.anchor = None;
    }
    if response.hovered() && ui.input(|input| input.modifiers.shift) {
        let scroll =
            ui.input(|input| input.smooth_scroll_delta.x + input.smooth_scroll_delta.y) as f64;
        if scroll != 0.0 {
            ui.input_mut(|input| input.smooth_scroll_delta = egui::Vec2::ZERO);
            let start = (window[0] - scroll / rect.width() as f64 * width)
                .clamp(0.0, (total - width).max(0.0));
            *window = [start, start + width];
        }
    } else if response.hovered() && (axis || ui.input(|input| input.modifiers.ctrl)) {
        let scroll = ui.input(|input| input.smooth_scroll_delta.y) as f64;
        if scroll != 0.0 {
            ui.input_mut(|input| input.smooth_scroll_delta.y = 0.0);
            let anchor = response.hover_pos().map_or(0.5, fraction);
            let next_width = (width * (-scroll * 0.005).exp()).clamp(1e-6_f64.min(total), total);
            let start =
                (window[0] + width * anchor - next_width * anchor).clamp(0.0, total - next_width);
            *window = [start, start + next_width];
        }
    }
}

fn paint_selection(ui: &egui::Ui, rect: egui::Rect, selection: Option<[f64; 2]>, window: [f64; 2]) {
    let Some(selection) = selection else { return };
    if selection[1] < window[0] || selection[0] > window[1] {
        return;
    }
    let x = |time: f64| {
        rect.left()
            + rect.width() * ((time - window[0]) / (window[1] - window[0])).clamp(0.0, 1.0) as f32
    };
    let selected = egui::Rect::from_min_max(
        egui::pos2(x(selection[0]), rect.top()),
        egui::pos2(x(selection[1]), rect.bottom()),
    );
    ui.painter().rect_filled(
        selected,
        0.0,
        egui::Color32::from_rgba_unmultiplied(247, 196, 89, 36),
    );
    for edge in [selected.left(), selected.right()] {
        ui.painter().line_segment(
            [
                egui::pos2(edge, rect.top()),
                egui::pos2(edge, rect.bottom()),
            ],
            egui::Stroke::new(1.0, egui::Color32::from_rgb(247, 196, 89)),
        );
    }
}

pub fn show(ui: &mut egui::Ui, studio: &mut Studio) {
    let language = studio.prefs.language;
    let Some(recording) = studio.recording.clone() else {
        ui.label(language.text(
            "Open or receive a capture to inspect SDK profile events.",
            "打开或接收录制后可查看 SDK 性能事件。",
        ));
        return;
    };
    let compact = ui.available_height() < 280.0;
    let complete_source = studio.complete_timeline_source();
    if !compact {
        crate::recording_export::show_analysis_controls(
            ui,
            studio,
            pvd_core::analysis_export::AnalysisCsvKind::Profile,
        );
    }
    if complete_source.is_none() && !studio.ensure_current_analysis(ui) {
        if compact {
            crate::recording_export::show_analysis_menu(
                ui,
                studio,
                pvd_core::analysis_export::AnalysisCsvKind::Profile,
            );
        }
        return;
    }
    if let Some(detail) = recording.diagnostics.iter().find(|message| {
        complete_source.is_none() && message.starts_with("Analysis decoding stopped at frame")
    }) {
        for cancel in [
            studio.profile_layout_cancel.take(),
            studio.profile_catalog_cancel.take(),
        ]
        .into_iter()
        .flatten()
        {
            cancel.store(true, Ordering::Relaxed);
        }
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
    let analysis = &recording.analysis;
    let native_container = recording.source == pvd_core::RecordingSource::Pvd3Database;
    ui.horizontal_wrapped(|ui| {
        ui.selectable_value(&mut studio.profile_events_view, false, language.text("Threads", "线程时间线"));
        ui.selectable_value(&mut studio.profile_events_view, true, language.text("Event curves", "事件曲线"));
        if !compact {
            if complete_source.is_none() && studio.analysis_ready() { ui.label(format!(
                "{} {} · {} {} · {} {}", analysis.profile_summary().complete_spans, language.text("spans", "个区间"),
                analysis.profile_summary().open_spans, language.text("open", "个未结束"),
                analysis.profile_summary().unmatched_stops, language.text("unmatched stops", "个未匹配结束事件")
            )); }
            profile_filter(ui, studio, false);
        }
        let fixed_stages = recording.frame_info(studio.frame)
            .and_then(|frame| studio.state.objects.get(&frame.scene_id))
            .is_some_and(|scene| pvd_core::frame_metrics::supports_stages(&recording, scene.id, scene.generation));
        let (plot_label, plot_help) = if !fixed_stages {
            (language.text("Plot frame interval", "绘制帧间隔"), language.text("Plot recorded frame-end intervals. Fixed phases require a unique recorded Scene source mapping; reused or missing contexts remain unavailable.", "绘制录制的帧结束间隔。固定阶段需要唯一的录制 Scene 源映射；源标识复用或映射缺失时不推断归属。"))
        } else {
            (language.text("Plot frame / phases", "绘制帧与阶段"), language.text("Add frame-end interval and complete simulate/collision/solver elapsed-time curves to the Curves panel. These are elapsed time, not accumulated CPU work.", "将帧结束间隔及完整的模拟/碰撞/求解阶段经过时间添加到曲线面板；这些值不是累计 CPU 工作时间。"))
        };
        if ui.button(plot_label).on_hover_text(plot_help).clicked() {
            studio.plot_performance(ui.ctx().clone());
        }
        if compact {
            crate::recording_export::show_analysis_menu(ui, studio, pvd_core::analysis_export::AnalysisCsvKind::Profile);
            ui.small_button("?").on_hover_ui(|ui| {
                ui.set_max_width(480.0);
                if let Some(frame) = recording.frame_info(studio.frame)
                    && let Some(scene) = studio.state.objects.get(&frame.scene_id)
                {
                    ui.monospace(format!("{} 0x{:x} · {} {}", language.text("Reference Scene", "参考 Scene"), scene.id, language.text("generation", "代际"), scene.generation));
                }
                if complete_source.is_none() && studio.analysis_ready() { ui.label(format!("{} {} · {} {} · {} {}", analysis.profile_summary().complete_spans, language.text("spans", "个区间"), analysis.profile_summary().open_spans, language.text("open", "个未结束"), analysis.profile_summary().unmatched_stops, language.text("unmatched stops", "个未匹配结束事件"))); }
                ui.label(language.text("Event curves use the current reference Scene's time windows, not Scene ownership. Inclusive sums add nested and overlapping durations; they are not CPU utilization.", "事件曲线按当前参考 Scene 的时间窗口对齐，不表示 Scene 归属；包含耗时总和会重复累计嵌套和重叠区间，不是 CPU 占用率。"));
                if studio.complete_profile_source().is_some() {
                    ui.label(language.text("Full-file catalog order can be selected in Order. The filter applies to the current page.", "完整文件目录可在“排序”中选择顺序；筛选只作用于当前页。"));
                }
                if native_container {
                    ui.label(language.text("Native event names use saved metadata. Fixed phases bind SDK Context identities through a unique recorded Scene lifetime.", "原版事件名称采用保存时元数据；固定阶段通过唯一的录制 Scene 生命周期映射 SDK Context。"));
                }
            });
        }
    });
    if native_container && !compact {
        ui.label(language.text(
            "PVD3 event names come from the saved catalog. Context values retain their SDK identities; select an event and reference Scene to plot its recorded timing.",
            "原版 PVD3 性能名称来自保存时的目录，Context 保留 SDK 标识；可选择性能事件与参考 Scene 绘制录制时间曲线。",
        ));
    }
    if studio.profile_events_view {
        show_events(ui, studio, compact);
        return;
    }
    if let Some(source) = complete_source {
        timeline::show(ui, studio, source, compact);
        return;
    }
    if compact {
        profile_filter(ui, studio, false);
    }
    let ms_per_tick = recording.header.timestamp_numerator as f64
        / recording.header.timestamp_denominator as f64
        / 1e6;
    if !ms_per_tick.is_finite() || ms_per_tick <= 0.0 {
        ui.colored_label(
            egui::Color32::LIGHT_RED,
            language.text("Invalid profile clock ratio.", "性能时钟比例无效。"),
        );
        return;
    }
    let id = ui.id().with("thread-profile-state");
    let mut view = ui
        .ctx()
        .data_mut(|data| data.get_temp::<ViewState>(id))
        .unwrap_or_default();
    let changed = view.cache.update(
        refresh::Request {
            recording: &recording,
            frame: studio.frame,
            epoch: studio.profile_view_epoch,
            following: studio.follow_live,
            now: Instant::now(),
            context: ui.ctx(),
            jobs: &studio.background_jobs,
        },
        build_layout,
    );
    studio.profile_layout_cancel = view.cache.job.as_ref().map(|job| job.cancel.clone());
    if view.cache.job.is_some() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(language.text("Preparing thread tracks…", "正在准备线程轨道…"));
            if ui.button(language.text("Cancel", "取消")).clicked() {
                view.cache.cancel();
                view.cache.error = Some(Message::new("Thread layout cancelled", "线程布局已取消"));
            }
        });
    }
    if let Some(error) = &view.cache.error {
        ui.label(error.text(language));
        if ui
            .button(language.text("Retry layout", "重新生成布局"))
            .clicked()
        {
            view.cache.retry();
            ui.ctx().request_repaint();
        }
    }
    let Some(prepared) = view.cache.prepared.clone() else {
        ui.ctx().data_mut(|data| data.insert_temp(id, view));
        return;
    };
    let analysis = &prepared.analysis;
    if changed {
        view.layout = prepared.value.clone();
        view.rows = view
            .layout
            .filtered(analysis, &studio.profile_filter.to_lowercase());
        view.filter = studio.profile_filter.to_lowercase();
    }
    snapshot_label(ui, language, prepared.frame, studio.follow_live);
    if analysis.profile_summary().complete_spans == 0 {
        ui.label(language.text(
            "No complete CPU profile spans. Enable SDK profile instrumentation to record them.",
            "没有完整的 CPU 性能区间。请启用 SDK 性能事件采集。",
        ));
        ui.ctx().data_mut(|data| data.insert_temp(id, view));
        return;
    }
    if !ui.input(|input| input.pointer.primary_down()) {
        view.anchor = None;
    }
    let filter = studio.profile_filter.to_lowercase();
    if filter != view.filter {
        view.rows = view.layout.filtered(analysis, &filter);
        view.filter = filter;
    }
    let first = view.layout.first;
    let total = (view.layout.last.saturating_sub(first) as f64 * ms_per_tick).max(1e-6);
    let mut window = studio
        .profile_window
        .unwrap_or_else(|| frame_window(analysis, studio.frame, first, ms_per_tick, total));
    ui.horizontal_wrapped(|ui| {
        if ui
            .small_button(language.text("Fit capture", "显示全部录制"))
            .clicked()
        {
            window = [0.0, total];
        }
        if ui
            .small_button(language.text("Current flush frame", "当前刷新帧"))
            .on_hover_text(language.text(
                "Payload flush attribution is not an exact SDK frame boundary.",
                "数据刷新归属并非精确的 SDK 帧边界。",
            ))
            .clicked()
        {
            window = frame_window(analysis, studio.frame, first, ms_per_tick, total);
        }
        ui.label(language.text("Window", "时间窗口"));
        ui.add(
            egui::DragValue::new(&mut window[0])
                .range(0.0..=total)
                .speed(0.001)
                .suffix(" ms")
                .max_decimals(6),
        );
        ui.label(language.text("to", "至"));
        ui.add(
            egui::DragValue::new(&mut window[1])
                .range(0.0..=total)
                .speed(0.001)
                .suffix(" ms")
                .max_decimals(6),
        );
        ui.toggle_value(
            &mut view.measure,
            language.text("Measure interval", "测量区间"),
        );
        if view.selection.is_some() && ui.small_button(language.text("Clear", "清除")).clicked() {
            view.selection = None;
        }
    });
    if let Some([start, end]) = view.selection {
        ui.label(format!(
            "{}: {:.6} → {:.6} ms  ·  Δ {:.6} ms",
            language.text("Measured interval", "测量区间"),
            start,
            end,
            end - start
        ));
    }
    window = time_window(window, total);
    window = show_tracks(
        ui,
        studio,
        analysis,
        &mut view,
        id,
        window,
        TrackScale { ms_per_tick, total },
    );
    studio.profile_window = Some(time_window(window, total));
    ui.ctx().data_mut(|data| data.insert_temp(id, view));
}

#[derive(Clone, Copy)]
struct TrackScale {
    ms_per_tick: f64,
    total: f64,
}

fn show_tracks(
    ui: &mut egui::Ui,
    studio: &mut Studio,
    analysis: &impl ProfileSource,
    view: &mut ViewState,
    id: egui::Id,
    mut window: [f64; 2],
    scale: TrackScale,
) -> [f64; 2] {
    let TrackScale { ms_per_tick, total } = scale;
    let first = view.layout.first;
    let language = studio.prefs.language;
    let dense = ui
        .available_height()
        .min(ui.clip_rect().bottom() - ui.cursor().top())
        < 120.0;
    let row_height = if dense { 18.0 } else { ROW_HEIGHT };
    let label_width = (ui.available_width() * 0.17).clamp(76.0, 132.0);
    let (whole_axis, _) = ui.allocate_exact_size(
        egui::vec2(
            ui.available_width().max(1.0),
            if dense { 22.0 } else { 30.0 },
        ),
        egui::Sense::hover(),
    );
    let axis = egui::Rect::from_min_max(
        egui::pos2(
            (whole_axis.left() + label_width).min(whole_axis.right() - 1.0),
            whole_axis.top(),
        ),
        whole_axis.max,
    );
    ui.painter().text(
        whole_axis.left_center(),
        egui::Align2::LEFT_CENTER,
        format!(
            "{} {}",
            view.layout.threads,
            language.text("thread groups", "个线程分组")
        ),
        egui::FontId::proportional(11.0),
        MUTED,
    );
    let track_help =
        egui::Rect::from_min_max(whole_axis.min, egui::pos2(axis.left(), whole_axis.bottom()));
    ui.interact(track_help, id.with("track-help"), egui::Sense::hover()).on_hover_text(language.text(
        "Nested and overlapping events occupy separate tracks. Grey bars group events smaller than a screen pixel; zoom to inspect them. Tracks do not establish a call stack.",
        "嵌套及重叠事件分配至不同轨道。灰色条合并不足一个屏幕像素的事件，缩放后可逐条查看；轨道不代表调用栈。",
    ));
    let response = ui.interact(axis, id.with("axis"), egui::Sense::click_and_drag());
    interact(ui, &response, axis, &mut window, total, view, true);
    response.on_hover_text(language.text(
        "Drag / Shift + wheel: pan · Wheel: zoom · Shift + drag: measure · time relative to first captured event",
        "拖动 / Shift + 滚轮：平移 · 滚轮：缩放 · Shift + 拖动：测量 · 时间相对首个捕获事件",
    ));
    for mark in 0..=4 {
        let fraction = mark as f32 / 4.0;
        let x = axis.left() + fraction * axis.width();
        let align = if mark == 0 {
            egui::Align2::LEFT_TOP
        } else if mark == 4 {
            egui::Align2::RIGHT_TOP
        } else {
            egui::Align2::CENTER_TOP
        };
        ui.painter().text(
            egui::pos2(x, axis.top()),
            align,
            format!(
                "{:.3}",
                window[0] + (window[1] - window[0]) * f64::from(fraction)
            ),
            egui::FontId::monospace(10.0),
            MUTED,
        );
        ui.painter().line_segment(
            [
                egui::pos2(x, axis.bottom() - 6.0),
                egui::pos2(x, axis.bottom()),
            ],
            egui::Stroke::new(1.0, MUTED),
        );
    }
    paint_selection(ui, axis, view.selection, window);
    let ticks = window.map(|ms| first.saturating_add((ms / ms_per_tick) as u64));
    let mut visible = Vec::<(usize, Range<usize>)>::new();
    let mut thread_header = None;
    for (index, row) in view.rows.iter().enumerate() {
        match row {
            Row::Thread { .. } => thread_header = Some(index),
            Row::Lane { spans, .. } => {
                let range = visible_range(spans, analysis, ticks);
                if !range.is_empty() {
                    if let Some(header) = thread_header.take() {
                        visible.push((header, 0..0));
                    }
                    visible.push((index, range));
                }
            }
        }
    }
    if visible.is_empty() {
        ui.label(language.text(
            "No spans match this window and filter.",
            "当前时间窗口及筛选条件下没有性能区间。",
        ));
    }
    let rows = view.rows.clone();
    let mut seek = None;
    let mut plot = None;
    let can_plot = studio.recording.as_ref().is_some_and(|recording| {
        recording
            .frame_info(studio.frame)
            .and_then(|frame| studio.state.objects.get(&frame.scene_id))
            .is_some_and(|scene| scene.type_name == "physx3::PxScene")
    });
    egui::ScrollArea::vertical()
        .id_salt("thread-profile-rows")
        .show_rows(ui, row_height, visible.len(), |ui, range| {
            for row_index in range {
                let (index, visible_range) = &visible[row_index];
                let (whole, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width().max(1.0), row_height),
                    egui::Sense::hover(),
                );
                match &rows[*index] {
                    Row::Thread { id: thread, spans } => {
                        ui.painter()
                            .rect_filled(whole, 2.0, egui::Color32::from_rgb(27, 37, 48));
                        ui.painter().text(
                            whole.left_center() + egui::vec2(4.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            format!(
                                "{}   ·   {spans} {}",
                                thread_label(*thread, language),
                                language.text("captured spans", "个捕获区间")
                            ),
                            egui::FontId::proportional(12.0),
                            MUTED,
                        );
                    }
                    Row::Lane { level, spans, .. } => {
                        ui.painter().text(
                            whole.left_center() + egui::vec2(8.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            format!("{} {}", language.text("Track", "轨道"), level + 1),
                            egui::FontId::proportional(11.0),
                            MUTED,
                        );
                        let rect = egui::Rect::from_min_max(
                            egui::pos2(
                                (whole.left() + label_width).min(whole.right() - 1.0),
                                whole.top() + 2.0,
                            ),
                            whole.right_bottom() - egui::vec2(0.0, 2.0),
                        );
                        let response = ui.interact(
                            rect,
                            id.with(("lane", *index)),
                            egui::Sense::click_and_drag(),
                        );
                        ui.painter()
                            .rect_filled(rect, 1.0, egui::Color32::from_rgb(19, 27, 36));
                        let individual = paint_lane(
                            ui,
                            rect,
                            &spans[visible_range.clone()],
                            analysis,
                            &view.layout,
                            window,
                            ms_per_tick,
                        )
                        .filter(|_| response.hovered());
                        if let Some(key) = span_menu(
                            ui,
                            &response,
                            individual.map(|index| profile_span(analysis, index)),
                            studio.profile_view_epoch,
                            |key| {
                                can_plot
                                    && studio.recording.as_ref().is_some_and(|recording| {
                                        let metadata = recording
                                            .native_end_analysis()
                                            .unwrap_or(&recording.analysis);
                                        !metadata.profile_metadata_ambiguous(key.zone, key.event)
                                    })
                            },
                            language,
                        ) {
                            plot = Some(key);
                        }
                        let hovered = response.hover_pos().and_then(|point| {
                            let time = window[0]
                                + ((point.x - rect.left()) / rect.width()) as f64
                                    * (window[1] - window[0]);
                            let tick = first.saturating_add((time / ms_per_tick) as u64);
                            let next = spans.partition_point(|index| {
                                profile_span(analysis, *index).start <= tick
                            });
                            next.checked_sub(1)
                                .map(|position| spans[position])
                                .filter(|index| profile_span(analysis, *index).end >= tick)
                        });
                        if response.clicked() && !view.measure && !ui.input(|i| i.modifiers.shift) {
                            seek =
                                hovered.map(|index| profile_span(analysis, index).received_frame);
                        }
                        interact(ui, &response, rect, &mut window, total, view, false);
                        paint_selection(ui, rect, view.selection, window);
                        if let Some(index) = hovered {
                            let span = profile_span(analysis, index);
                            response.on_hover_ui(|ui| {
                                ui.label(
                                    view.layout
                                        .labels
                                        .get(&(span.zone, span.event))
                                        .cloned()
                                        .unwrap_or_else(|| {
                                            format!(
                                                "{} {}",
                                                language.text("Event", "事件"),
                                                span.event
                                            )
                                        }),
                                );
                                ui.label(format!(
                                    "{:.6} ms · {} · 0x{:x}",
                                    span.end.saturating_sub(span.start) as f64 * ms_per_tick,
                                    thread_label(span.thread, language),
                                    span.context
                                ));
                                ui.monospace(format!(
                                    "ticks {}–{} · z0x{:x}/e{}",
                                    span.start, span.end, span.zone, span.event
                                ));
                                ui.label(format!(
                                    "{} {}",
                                    language.text(
                                        "Click to jump to payload flush frame",
                                        "点击跳转到数据刷新帧"
                                    ),
                                    span.received_frame
                                ));
                            });
                        }
                    }
                }
            }
        });
    if let Some(frame) = seek {
        studio.request_ui_action(
            ui.ctx(),
            ui.id().with("profile-span"),
            crate::studio::UiAction::Locate(
                frame.min(studio.count().saturating_sub(1)),
                crate::studio::DisplayReason::Performance,
            ),
        );
    }
    if let Some(key) = plot {
        studio.plot_profile_event(key, ui.ctx().clone());
    }
    time_window(window, total)
}

#[derive(Clone)]
struct SpanMenu {
    epoch: u64,
    key: ExactKey,
}

fn span_menu(
    ui: &egui::Ui,
    response: &egui::Response,
    hovered: Option<&ProfileSpan>,
    epoch: u64,
    can_plot: impl Fn(ExactKey) -> bool,
    language: Language,
) -> Option<ExactKey> {
    let id = response.id.with("event-curve");
    if response.secondary_clicked() {
        ui.ctx().data_mut(|data| {
            data.remove::<SpanMenu>(id);
            if let Some(span) = hovered {
                data.insert_temp(
                    id,
                    SpanMenu {
                        epoch,
                        key: ExactKey {
                            zone: span.zone,
                            event: span.event,
                            thread: span.thread,
                            context: span.context,
                        },
                    },
                );
            }
        });
    }
    let target = ui
        .ctx()
        .data(|data| data.get_temp::<SpanMenu>(id))
        .filter(|target| target.epoch == epoch);
    let mut selected = None;
    response.context_menu(|ui| {
        if let Some(target) = target {
            if ui.add_enabled(can_plot(target.key), egui::Button::new(language.text("Plot event duration", "绘制事件耗时")))
                .on_hover_text(language.text(
                    "Sum this exact event, thread and Context in each reference Scene interval. Nested and overlapping occurrences are included.",
                    "按参考 Scene 的每帧时间区间累计此事件、线程和 Context 的耗时；包含嵌套及重叠的出现次数。",
                )).clicked() {
                selected = Some(target.key);
                ui.close();
            }
        } else {
            ui.label(language.text("Zoom in and select an individual event.", "请放大后选择单个事件。"));
        }
    });
    selected
}

#[derive(Clone, Default)]
struct EventView {
    cache: refresh::Cache<CatalogPage>,
    pages: pages::Pages,
    rows: Arc<Vec<usize>>,
    filter: Option<String>,
    progress: Arc<Mutex<Option<crate::full_profile::TimelineProgress>>>,
}

impl EventView {
    fn restart_page(&mut self) {
        self.cache.cancel();
        self.cache = refresh::Cache::default();
        self.rows = Arc::default();
        self.filter = None;
        self.progress = Arc::default();
    }
}

fn event_name(entry: &CatalogEntry, analysis: &impl ProfileSource, language: Language) -> String {
    if entry.metadata_ambiguous {
        return language
            .text("Metadata identity is ambiguous", "元数据身份有歧义")
            .into();
    }
    analysis
        .profile_zone_name(entry.key.zone)
        .zip(analysis.profile_event_name(entry.key.zone, entry.key.event))
        .map(|(zone, event)| format!("{zone} / {event}"))
        .unwrap_or_else(|| {
            language
                .text("Unknown event metadata", "事件元数据未知")
                .into()
        })
}

fn event_identity(key: ExactKey) -> String {
    format!(
        "z0x{:x}/e{} · t{} · c0x{:x}",
        key.zone, key.event, key.thread, key.context
    )
}

fn event_matches(entry: &CatalogEntry, analysis: &impl ProfileSource, filter: &str) -> bool {
    filter.is_empty()
        || event_identity(entry.key).to_lowercase().contains(filter)
        || event_name(entry, analysis, Language::English)
            .to_lowercase()
            .contains(filter)
        || event_name(entry, analysis, Language::Chinese).contains(filter)
}

fn event_rows(
    ui: &mut egui::Ui,
    entries: &[CatalogEntry],
    rows: &[usize],
    analysis: &impl ProfileSource,
    language: Language,
    can_plot: bool,
) -> Option<ExactKey> {
    let mut plot = None;
    egui::ScrollArea::vertical()
        .id_salt("profile-event-catalog")
        .show_rows(ui, 42.0, rows.len(), |ui, visible| {
        for row in visible {
            let entry = &entries[rows[row]];
            let (rect, response) = ui.allocate_exact_size(
                egui::vec2(ui.available_width().max(1.0), 42.0),
                egui::Sense::hover(),
            );
            if row % 2 == 0 {
                ui.painter().rect_filled(rect, 2.0, egui::Color32::from_rgb(25, 34, 45));
            }
            let button = egui::Rect::from_min_max(
                egui::pos2((rect.right() - 58.0).max(rect.left()), rect.top() + 7.0),
                rect.right_bottom() - egui::vec2(2.0, 7.0),
            );
            if ui.add_enabled_ui(can_plot, |ui| {
                ui.put(button, egui::Button::new(language.text("Plot", "曲线")))
            }).inner
                .on_hover_text(language.text("Inclusive sum in the current reference Scene's consecutive time windows. Nested and overlapping occurrences are added; this is not Scene ownership or CPU utilization.", "按当前参考 Scene 连续时间窗口累计包含耗时；嵌套和重叠的出现次数都会累加，不表示 Scene 归属或 CPU 占用率。"))
                .clicked() {
                plot = Some(entry.key);
            }
            let content = egui::Rect::from_min_max(
                rect.min + egui::vec2(5.0, 1.0),
                egui::pos2((button.left() - 5.0).max(rect.left()), rect.bottom()),
            );
            let painter = ui.painter().with_clip_rect(content);
            let color = if entry.metadata_ambiguous {
                egui::Color32::from_rgb(234, 181, 92)
            } else {
                egui::Color32::from_rgb(215, 227, 239)
            };
            painter.text(content.min, egui::Align2::LEFT_TOP,
                event_name(entry, analysis, language), egui::FontId::proportional(12.0), color);
            let counts = format!("{} {} · {} {} · {} {}",
                entry.complete_spans, language.text("complete", "完整"),
                entry.open_spans, language.text("open", "未结束"),
                entry.unmatched_stops, language.text("unmatched stops", "未匹配结束"));
            painter.text(content.min + egui::vec2(0.0, 19.0), egui::Align2::LEFT_TOP,
                format!("{} · {counts}", event_identity(entry.key)), egui::FontId::proportional(11.0), MUTED);
            response.on_hover_ui(|ui| {
                ui.set_max_width(520.0);
                ui.label(event_name(entry, analysis, language));
                ui.monospace(format!("zone=0x{:x} · event={} · thread={} · context=0x{:x}", entry.key.zone, entry.key.event, entry.key.thread, entry.key.context));
                ui.label(counts);
                ui.label(format!("{}: {}", language.text("First start tick", "首次开始 tick"), entry.first_start.map(|value| value.to_string()).unwrap_or_else(|| language.text("unknown", "未知").into())));
                ui.label(language.text("Exact zone, event, thread and context identities keep same-name events separate.", "相同名称的事件仍按 zone、event、thread、context 精确身份分别列出。"));
            });
        }
    });
    plot
}

fn profile_filter(ui: &mut egui::Ui, studio: &mut Studio, page_only: bool) {
    let language = studio.prefs.language;
    ui.add(
        egui::TextEdit::singleline(&mut studio.profile_filter)
            .desired_width(190.0_f32.min(ui.available_width()))
            .hint_text(if page_only {
                language.text("Filter this page", "筛选当前页")
            } else {
                language.text("Filter thread, zone or event", "筛选线程、区域或事件")
            }),
    );
}

fn show_events(ui: &mut egui::Ui, studio: &mut Studio, compact: bool) {
    let language = studio.prefs.language;
    let Some(recording) = studio.recording.clone() else {
        return;
    };
    let id = ui.id().with("profile-event-view");
    let mut view = ui
        .ctx()
        .data_mut(|data| data.get_temp::<EventView>(id))
        .unwrap_or_default();
    let full_source = studio.complete_profile_source();
    let complete_file = full_source.is_some();
    let indexed = full_source
        .as_ref()
        .is_some_and(|source| source.can_rebuild_cpu_index());
    if view.pages.sync(studio.profile_view_epoch, complete_file) {
        view.restart_page();
    }
    if pages::start_render(ui.ctx(), studio.profile_view_epoch, &mut view.pages) {
        view.restart_page();
    }
    let request = refresh::Request {
        recording: &recording,
        frame: studio.frame,
        epoch: studio.profile_view_epoch,
        following: studio.follow_live,
        now: Instant::now(),
        context: ui.ctx(),
        jobs: &studio.background_jobs,
    };
    let changed = if let Some(source) = full_source {
        let progress = view.progress.clone();
        let cursor = view.pages.cursor.clone();
        let order = view.pages.order;
        view.cache.update_with(request, Some(0), move |_, cancel| {
            pages::load(&source, order, cursor.as_ref(), cancel, |status| {
                if let Ok(mut progress) = progress.lock() {
                    *progress = Some(status);
                }
                !cancel.load(Ordering::Relaxed)
            })
            .map(|(page, analysis)| (page, Some(analysis)))
        })
    } else {
        view.cache.update(request, build_catalog)
    };
    if changed {
        view.filter = None;
    }
    pages::poll_render(ui.ctx(), studio.profile_view_epoch, &view);
    let scene = recording
        .frame_info(studio.frame)
        .and_then(|frame| studio.state.objects.get(&frame.scene_id))
        .filter(|scene| scene.type_name == "physx3::PxScene")
        .map(|scene| (scene.id, scene.generation));
    if let Some(scene) = scene.filter(|_| !compact) {
        ui.label(
            egui::RichText::new(format!(
                "{} 0x{:x} · {} {} · Σ ms",
                language.text("Reference Scene", "参考 Scene"),
                scene.0,
                language.text("generation", "代际"),
                scene.1
            ))
            .small(),
        );
    } else if scene.is_none() {
        ui.colored_label(
            egui::Color32::LIGHT_RED,
            language.text(
                "No valid reference Scene in this frame; Plot is unavailable.",
                "当前帧没有有效参考 Scene，无法绘制事件曲线。",
            ),
        );
    }
    if !compact {
        ui.label(egui::RichText::new(language.text("Aligned to the current Scene's time windows, not Scene ownership. Inclusive sums include overlaps, not CPU utilization.", "按当前 Scene 时间窗口对齐，不表示 Scene 归属；包含耗时总和重复计入重叠，不代表 CPU 占用率。")).small().color(MUTED));
    }
    let next = view
        .cache
        .prepared
        .as_ref()
        .and_then(|prepared| prepared.value.next.clone());
    let mut page_changed = false;
    if compact {
        ui.horizontal_wrapped(|ui| {
            profile_filter(ui, studio, complete_file);
            if complete_file {
                page_changed = view.pages.show_compact(ui, next.as_ref(), language);
                page_changed |= view.pages.order_menu(ui, language, indexed);
            } else if let Some(scene) = scene {
                let label = if studio.follow_live
                    && let Some(prepared) = &view.cache.prepared
                {
                    format!(
                        "{} {}",
                        language.text("Analysis snapshot", "分析快照"),
                        prepared.frame
                    )
                } else {
                    format!(
                        "Scene 0x{:x} · {} {}",
                        scene.0,
                        language.text("generation", "代际"),
                        scene.1
                    )
                };
                ui.label(egui::RichText::new(label).small())
                    .on_hover_ui(|ui| {
                        ui.monospace(format!(
                            "Scene 0x{:x} · {} {}",
                            scene.0,
                            language.text("generation", "代际"),
                            scene.1
                        ));
                        if let Some(prepared) = &view.cache.prepared {
                            snapshot_label(ui, language, prepared.frame, studio.follow_live);
                        }
                    });
            }
        });
    } else if complete_file {
        ui.horizontal_wrapped(|ui| {
            page_changed = view.pages.show(ui, next.as_ref(), language);
            page_changed |= view.pages.order_menu(ui, language, indexed);
        });
    }
    if page_changed {
        view.restart_page();
        studio.profile_catalog_cancel = None;
        ui.ctx().data_mut(|data| data.insert_temp(id, view));
        ui.ctx().request_repaint();
        return;
    }
    if view.cache.job.is_some() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(
                if complete_file && view.pages.order == pages::Order::FirstStart {
                    language.text("Loading first-start catalog…", "正在载入首次开始时间目录…")
                } else if complete_file {
                    language.text(
                        "Scanning the complete file for this catalog page…",
                        "正在扫描完整文件，生成当前目录页…",
                    )
                } else {
                    language.text("Indexing exact event identities…", "正在索引精确事件身份…")
                },
            );
            if ui.button(language.text("Cancel", "取消")).clicked() {
                view.cache.cancel();
                view.cache.error = Some(Message::new(
                    "Event catalog cancelled",
                    "事件列表索引已取消",
                ));
            }
        });
        if complete_file && let Some(progress) = view.progress.lock().ok().and_then(|value| *value)
        {
            match progress {
                crate::full_profile::TimelineProgress::Streaming(progress) => {
                    ui.add(
                        egui::ProgressBar::new(
                            (progress.bytes_read as f64 / progress.total_bytes.max(1) as f64)
                                as f32,
                        )
                        .show_percentage(),
                    );
                }
                crate::full_profile::TimelineProgress::Index(progress) => {
                    let label = match progress.phase {
                        pvd_core::profile_disk_index::Phase::Decode => {
                            language.text("Reading recorded events", "读取录制事件")
                        }
                        pvd_core::profile_disk_index::Phase::Metadata => {
                            language.text("Processing event metadata", "处理事件元数据")
                        }
                        pvd_core::profile_disk_index::Phase::Merge => {
                            language.text("Sorting catalog pages", "排序目录页")
                        }
                        pvd_core::profile_disk_index::Phase::VerifySource => {
                            language.text("Verifying capture identity", "核验录制身份")
                        }
                        pvd_core::profile_disk_index::Phase::Query => {
                            language.text("Reading catalog page", "读取目录页")
                        }
                    };
                    ui.label(label);
                }
            }
        }
    }
    if let Some(error) = &view.cache.error {
        ui.label(error.text(language));
        if ui
            .button(language.text("Retry catalog", "重新索引列表"))
            .clicked()
        {
            view.cache.retry();
            ui.ctx().request_repaint();
        }
        if view.pages.order == pages::Order::FirstStart
            && ui
                .button(language.text("Use identity order", "使用标识顺序"))
                .clicked()
        {
            view.pages.set_order(pages::Order::Key);
            view.restart_page();
            studio.profile_catalog_cancel = None;
            ui.ctx().data_mut(|data| data.insert_temp(id, view));
            ui.ctx().request_repaint();
            return;
        }
    }
    studio.profile_catalog_cancel = view.cache.job.as_ref().map(|job| job.cancel.clone());
    let Some(prepared) = view.cache.prepared.clone() else {
        ui.ctx().data_mut(|data| data.insert_temp(id, view));
        return;
    };
    if complete_file && !compact {
        ui.label(match view.pages.order {
            pages::Order::Key => language.text(
                "Complete file catalog · identity order · filter applies to this page",
                "完整文件目录 · 标识顺序 · 名称筛选仅作用于当前页",
            ),
            pages::Order::FirstStart => language.text(
                "Complete file catalog · first-start order · filter applies to this page",
                "完整文件目录 · 首次开始时间顺序 · 名称筛选仅作用于当前页",
            ),
        });
    } else if !compact {
        snapshot_label(ui, language, prepared.frame, studio.follow_live);
    }
    let analysis = recording
        .native_end_analysis()
        .unwrap_or(&prepared.analysis);
    let entries = &prepared.value.entries;
    let filter = studio.profile_filter.to_lowercase();
    if view.filter.as_deref() != Some(&filter) {
        view.rows = Arc::new(
            entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| event_matches(entry, analysis, &filter))
                .map(|(index, _)| index)
                .collect(),
        );
        view.filter = Some(filter);
    }
    if view.cache.job.is_none() && view.cache.error.is_none() && view.rows.is_empty() {
        ui.label(language.text(
            "No CPU event identities match this filter.",
            "没有符合筛选条件的 CPU 事件身份。",
        ));
    }
    let plot = event_rows(ui, entries, &view.rows, analysis, language, scene.is_some());
    ui.ctx().data_mut(|data| data.insert_temp(id, view));
    if let Some(key) = plot {
        studio.plot_profile_event(key, ui.ctx().clone());
    }
}

fn thread_label(thread: u32, language: crate::i18n::Language) -> String {
    if thread == pvd_core::frame_metrics::CROSS_THREAD_ID {
        language
            .text(
                "SDK cross-thread stages (99999789)",
                "SDK 跨线程阶段（99999789）",
            )
            .into()
    } else {
        format!("{} {thread}", language.text("Thread", "线程"))
    }
}

fn paint_lane(
    ui: &egui::Ui,
    rect: egui::Rect,
    indices: &[usize],
    analysis: &impl ProfileSource,
    layout: &Layout,
    window: [f64; 2],
    ms_per_tick: f64,
) -> Option<usize> {
    let width = window[1] - window[0];
    let x = |tick: u64| {
        rect.left()
            + rect.width()
                * ((tick.saturating_sub(layout.first) as f64 * ms_per_tick - window[0]) / width)
                    .clamp(0.0, 1.0) as f32
    };
    let columns = rect.width().ceil().clamp(1.0, 4096.0);
    let mut at = 0;
    let pointer = ui.input(|input| input.pointer.hover_pos());
    let mut individual = None;
    while at < indices.len() {
        let span = profile_span(analysis, indices[at]);
        let left = x(span.start).min(rect.right() - 1.0);
        let column_end = (((left - rect.left()) / rect.width() * columns).floor() + 1.0) / columns;
        let bucket_tick = layout
            .first
            .saturating_add(((window[0] + f64::from(column_end) * width) / ms_per_tick) as u64);
        let next = bucket_end(indices, analysis, at, bucket_tick);
        let right = x(profile_span(analysis, indices[next - 1]).end)
            .max(left + 1.0)
            .min(rect.right());
        let bar = egui::Rect::from_min_max(
            egui::pos2(left, rect.top()),
            egui::pos2(right, rect.bottom()),
        );
        let color = if next > at + 1 {
            MUTED
        } else {
            event_color(span.zone, span.event)
        };
        ui.painter().rect_filled(bar, 2.0, color);
        if next == at + 1 && pointer.is_some_and(|pointer| bar.contains(pointer)) {
            individual = Some(indices[at]);
        }
        if next == at + 1 && bar.width() > 48.0 {
            let text = layout
                .labels
                .get(&(span.zone, span.event))
                .map_or("?", String::as_str);
            ui.painter().with_clip_rect(bar).text(
                bar.left_center() + egui::vec2(4.0, 0.0),
                egui::Align2::LEFT_CENTER,
                text,
                egui::FontId::proportional(10.0),
                egui::Color32::from_rgb(12, 21, 27),
            );
        }
        at = next;
    }
    individual
}

fn event_color(zone: u64, event: u16) -> egui::Color32 {
    match (zone ^ u64::from(event).wrapping_mul(31)) % 4 {
        0 => ACCENT,
        1 => egui::Color32::from_rgb(121, 171, 222),
        2 => egui::Color32::from_rgb(164, 151, 203),
        _ => egui::Color32::from_rgb(194, 173, 114),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pvd_core::analysis::{Analysis, ProfileSpan, ProfileZone};

    fn analysis() -> Analysis {
        let mut analysis = Analysis::default();
        analysis.zones.insert(
            1,
            ProfileZone {
                name: "Physics".into(),
                events: BTreeMap::from([(1, "simulate".into()), (2, "solver".into())]),
            },
        );
        for (thread, start, end, event, frame) in [
            (7, 15, 25, 2, 0),
            (7, 0, 100, 1, 0),
            (7, 10, 80, 2, 0),
            (7, 30, 50, 2, 0),
            (7, 70, 110, 2, 1),
            (7, 100, 120, 1, 1),
            (8, 20, 40, 1, 0),
            (8, 40, 40, 2, 0),
            (8, 40, 50, 1, 1),
        ] {
            analysis.spans.push(ProfileSpan {
                zone: 1,
                event,
                thread,
                context: 0,
                start,
                end,
                received_frame: frame,
            });
        }
        analysis
    }

    fn low_height_capture() -> Vec<u8> {
        let text = |value: &str| {
            let mut data = (value.len() as u32 + 1).to_le_bytes().to_vec();
            data.extend(value.as_bytes());
            data.push(0);
            data
        };
        let mut events = Vec::new();
        for (id, value) in [
            (1u32, "physx3"),
            (2, "PxScene"),
            (3, "frame"),
            (4, "PxProfileZone"),
            (5, "events"),
            (6, "PvdU8"),
        ] {
            events.push(1);
            events.extend(text(value));
            events.extend(id.to_le_bytes());
        }
        for (class, id) in [(2u32, 9u64), (4, 10)] {
            events.push(6);
            events.extend(1u32.to_le_bytes());
            events.extend(class.to_le_bytes());
            events.extend(id.to_le_bytes());
        }
        events.push(24);
        events.extend(10u64.to_le_bytes());
        events.extend(text("Fixture"));
        events.push(25);
        events.extend(10u64.to_le_bytes());
        events.extend(text("work"));
        events.extend(1u16.to_le_bytes());
        events.push(1);
        events.push(19);
        events.extend(9u64.to_le_bytes());
        events.extend(3u32.to_le_bytes());
        events.extend(100u64.to_le_bytes());
        let mut data = Vec::new();
        for (kind, tick) in [(1u8, 110u64), (2, 120)] {
            data.extend([kind, 3]);
            data.extend(1u16.to_le_bytes());
            data.extend(7u32.to_le_bytes());
            data.extend([9, 0, 0]);
            data.extend(tick.to_le_bytes());
        }
        events.push(7);
        events.extend(10u64.to_le_bytes());
        for value in [5, 1, 6, data.len() as u32, data.len() as u32] {
            events.extend(value.to_le_bytes());
        }
        events.extend(data);
        events.push(19);
        events.extend(9u64.to_le_bytes());
        events.extend(3u32.to_le_bytes());
        events.extend(200u64.to_le_bytes());
        let mut bytes = pvd_protocol::STREAM_MAGIC.to_le_bytes().to_vec();
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(1_000_000u64.to_le_bytes());
        bytes.extend(1u64.to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        bytes.extend((events.len() as u32).to_le_bytes());
        bytes.extend(13u32.to_le_bytes());
        bytes.extend(1u64.to_le_bytes());
        bytes.extend(0u64.to_le_bytes());
        bytes.extend(events);
        bytes
    }

    #[test]
    fn low_height_event_catalog_keeps_plot_buttons_and_event_counts_visible() {
        let bytes = Arc::new(low_height_capture());
        let recording = Arc::new(Recording::from_bytes(&bytes).unwrap());
        assert_eq!(recording.frame_count(), 2);
        assert_eq!(recording.analysis.profile_spans().len(), 1);
        let path = std::env::temp_dir().join(format!(
            "pvd-low-height-{}-{}.pvdstream",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, bytes.as_ref()).unwrap();
        for language in [Language::English, Language::Chinese] {
            for width in [620.0, 400.0] {
                for (complete, order) in [
                    (false, pages::Order::Key),
                    (true, pages::Order::Key),
                    (true, pages::Order::FirstStart),
                ] {
                    let context = egui::Context::default();
                    crate::ui::configure(&context);
                    let mut studio = Studio::default();
                    studio.prefs.language = language;
                    studio.state = recording.state_at(0).unwrap();
                    studio.recording = Some(recording.clone());
                    if complete {
                        studio.open(path.clone(), context.clone());
                        let deadline = Instant::now() + std::time::Duration::from_secs(5);
                        while studio.job.is_some() {
                            assert!(Instant::now() < deadline, "fixture load timed out");
                            studio.poll(&context);
                            std::thread::sleep(std::time::Duration::from_millis(1));
                        }
                        assert!(studio.complete_profile_source().is_some());
                    }
                    studio.profile_events_view = true;
                    let viewport =
                        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 180.0));
                    let deadline = Instant::now() + std::time::Duration::from_secs(5);
                    let mut visible_button = false;
                    while !visible_button {
                        assert!(
                            Instant::now() < deadline,
                            "catalog did not become visible: {language:?}, width={width}, complete={complete}"
                        );
                        let render_ready = order != pages::Order::FirstStart
                            || prepare_first_start_render(&context, &studio).unwrap();
                        let output = context.run(
                            egui::RawInput {
                                screen_rect: Some(viewport),
                                ..Default::default()
                            },
                            |context| {
                                egui::CentralPanel::default().show(context, |ui| {
                                    show(ui, &mut studio);
                                });
                            },
                        );
                        visible_button = render_ready
                            && output.shapes.iter().any(|shape| match &shape.shape {
                                egui::epaint::Shape::Text(text)
                                    if text.galley.text() == language.text("Plot", "曲线") =>
                                {
                                    let rect =
                                        egui::Rect::from_min_size(text.pos, text.galley.size());
                                    rect.intersect(shape.clip_rect).intersect(viewport).height()
                                        >= rect.height() - 0.5
                                }
                                _ => false,
                            });
                        if visible_button {
                            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::epaint::Shape::Text(text) if text.galley.text().contains(language.text("complete", "完整")) && egui::Rect::from_min_size(text.pos,text.galley.size()).intersect(shape.clip_rect).intersect(viewport).height() > 0.0)));
                        } else {
                            std::thread::sleep(std::time::Duration::from_millis(1));
                        }
                    }
                    let deadline = Instant::now() + std::time::Duration::from_secs(5);
                    while !studio.poll_shutdown() {
                        assert!(Instant::now() < deadline, "layout workers did not stop");
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                }
            }
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn nested_and_crossing_intervals_use_distinct_tracks_and_reuse_idle_tracks() {
        let analysis = analysis();
        let layout = Layout::build(&analysis);
        assert_eq!(layout.threads, 2);
        assert_eq!((layout.first, layout.last), (0, 120));
        let lanes: Vec<_> = layout
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Lane {
                    thread,
                    level,
                    spans,
                } => Some((*thread, *level, spans)),
                _ => None,
            })
            .collect();
        assert_eq!(
            lanes.iter().filter(|(thread, _, _)| *thread == 7).count(),
            3
        );
        assert_eq!(
            lanes.iter().filter(|(thread, _, _)| *thread == 8).count(),
            2
        );
        let mut seen = BTreeSet::new();
        for (thread, _, spans) in lanes {
            for index in spans {
                assert!(seen.insert(*index));
                assert_eq!(analysis.spans[*index].thread, thread);
            }
            for pair in spans.windows(2) {
                assert!(analysis.spans[pair[0]].end <= analysis.spans[pair[1]].start);
            }
        }
        assert_eq!(seen.len(), analysis.spans.len());
        assert!(layout.rows.iter().any(
            |row| matches!(row, Row::Lane { thread: 7, level: 0, spans } if spans == &vec![1, 5])
        ));
    }

    #[test]
    fn visible_ranges_retain_crossing_spans_and_zero_duration_boundaries() {
        let analysis = analysis();
        assert_eq!(visible_range(&[1, 5], &analysis, [50, 60]), 0..1);
        assert_eq!(visible_range(&[6, 7, 8], &analysis, [40, 40]), 0..3);
        assert_eq!(visible_range(&[6, 7, 8], &analysis, [51, 60]), 3..3);
        assert_eq!(visible_range(&[], &analysis, [0, 1]), 0..0);
        assert_eq!(frame_window(&analysis, 1, 0, 0.001, 0.12), [0.04, 0.12]);
    }

    #[test]
    fn filters_keep_thread_headers_and_preserve_original_track_assignments() {
        let analysis = analysis();
        let layout = Layout::build(&analysis);
        let rows = layout.filtered(&analysis, "solver");
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, Row::Thread { .. }))
                .count(),
            2
        );
        for row in rows.iter() {
            if let Row::Lane { spans, .. } = row {
                assert!(spans.iter().all(|index| analysis.spans[*index].event == 2));
            }
        }
        let rows = layout.filtered(&analysis, "8");
        assert!(rows.iter().all(|row| match row {
            Row::Thread { id, .. } => *id == 8,
            Row::Lane { thread, .. } => *thread == 8,
        }));
        assert!(layout.filtered(&analysis, "missing").is_empty());
        assert!(Arc::ptr_eq(&layout.rows, &layout.filtered(&analysis, "")));
    }

    #[test]
    fn time_windows_reject_nonfinite_bounds_and_keep_valid_measurement_ranges() {
        assert_eq!(time_window([f64::NAN, 1.0], 5.0), [0.0, 5.0]);
        assert_eq!(time_window([-3.0, 8.0], 5.0), [0.0, 5.0]);
        let [start, end] = time_window([4.0, 2.0], 5.0);
        assert!(start < end && end <= 5.0);
        let [start, end] = time_window([2.0, 2.0], 5.0);
        assert!(end > start && end - start <= 1.000001e-6);
    }

    #[test]
    fn sub_tick_pixels_group_coincident_zero_duration_events_in_one_bucket() {
        let analysis = analysis();
        let indices = vec![7; 100_000];
        assert_eq!(bucket_end(&indices, &analysis, 0, 40), indices.len());
        assert_eq!(bucket_end(&[0, 3, 4], &analysis, 0, 29), 1);
    }

    #[test]
    fn measurement_drag_retains_time_window_and_reports_precise_delta() {
        let context = egui::Context::default();
        let mut state = ViewState {
            measure: true,
            ..Default::default()
        };
        let mut window = [0.0, 10.0];
        let mut rect = egui::Rect::ZERO;
        let mut run = |events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 160.0),
                )),
                events,
                ..Default::default()
            };
            let _ = context.run(input, |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    let (bounds, response) = ui.allocate_exact_size(
                        egui::vec2(200.0, 30.0),
                        egui::Sense::click_and_drag(),
                    );
                    rect = bounds;
                    interact(ui, &response, bounds, &mut window, 10.0, &mut state, true);
                });
            });
        };
        run(vec![]);
        let start = egui::pos2(28.0, 18.0);
        let end = egui::pos2(148.0, 18.0);
        run(vec![
            egui::Event::PointerMoved(start),
            egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        run(vec![egui::Event::PointerMoved(end)]);
        run(vec![egui::Event::PointerButton {
            pos: end,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert!(rect.contains(start) && rect.contains(end));
        let [start, end] = state.selection.expect("drag did not create measurement");
        assert!((end - start - 6.0).abs() < 1e-6);
        assert_eq!(window, [0.0, 10.0]);
        assert!(state.anchor.is_none());
    }

    #[test]
    fn event_catalog_virtualizes_rows_and_clicks_preserve_the_exact_key() {
        let analysis = analysis();
        let entries: Vec<_> = (0..50_000)
            .map(|context| CatalogEntry {
                key: ExactKey {
                    zone: 1,
                    event: 2,
                    thread: 7,
                    context,
                },
                first_start: Some(context),
                complete_spans: 1,
                open_spans: 0,
                unmatched_stops: 0,
                metadata_ambiguous: false,
            })
            .collect();
        let rows: Vec<_> = (0..entries.len()).collect();
        let context = egui::Context::default();
        let mut selected = Vec::new();
        let mut run = |events, can_plot| {
            context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 300.0),
                    )),
                    events,
                    ..Default::default()
                },
                |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        if let Some(key) =
                            event_rows(ui, &entries, &rows, &analysis, Language::English, can_plot)
                        {
                            selected.push((can_plot, key));
                        }
                    });
                },
            )
        };
        let output = run(vec![], true);
        assert!(
            output.shapes.len() < 150,
            "the catalog rendered hidden rows"
        );
        let button = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == "Plot" => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
            .unwrap();
        for enabled in [false, true] {
            run(vec![], enabled);
            for pressed in [true, false] {
                run(
                    vec![
                        egui::Event::PointerMoved(button),
                        egui::Event::PointerButton {
                            pos: button,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    enabled,
                );
            }
        }
        assert_eq!(selected, vec![(true, entries[0].key)]);
    }

    #[test]
    fn navigating_catalog_pages_cancels_the_actual_reader_and_clears_prepared_rows() {
        let (source, recording, started, release) =
            crate::full_profile::Source::gated_test_capture();
        let context = egui::Context::default();
        let jobs = crate::background_jobs::BackgroundJobs::default();
        let mut view = EventView::default();
        view.cache.update_with(
            refresh::Request {
                recording: &recording,
                frame: 0,
                epoch: 1,
                following: false,
                now: Instant::now(),
                context: &context,
                jobs: &jobs,
            },
            Some(0),
            move |_, cancel| {
                source
                    .catalog_page(None, pages::PAGE_SIZE, cancel, |_| true)
                    .map(|(page, analysis)| {
                        (
                            CatalogPage {
                                entries: page.entries,
                                next: page.next.map(pages::Cursor::Key),
                            },
                            Some(analysis),
                        )
                    })
            },
        );
        started
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let cancelled = view.cache.job.as_ref().unwrap().cancel.clone();
        view.rows = Arc::new(vec![1, 2]);
        view.filter = Some("stale page".into());
        let old_progress = view.progress.clone();
        assert!(view.pages.set_order(pages::Order::FirstStart));
        view.restart_page();
        assert!(!Arc::ptr_eq(&old_progress, &view.progress));
        assert!(cancelled.load(Ordering::Relaxed));
        assert!(view.cache.job.is_none() && view.cache.prepared.is_none());
        assert!(view.rows.is_empty() && view.filter.is_none());
        assert_eq!(jobs.active_count(), 1);
        release.send(()).unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while jobs.active_count() != 0 {
            assert!(Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(view.cache.prepared.is_none());
    }

    #[test]
    fn ambiguous_metadata_is_not_treated_as_the_event_name_when_filtering() {
        let analysis = analysis();
        let mut entry = CatalogEntry {
            key: ExactKey {
                zone: 1,
                event: 2,
                thread: 7,
                context: 0xabcd,
            },
            first_start: Some(0),
            complete_spans: 1,
            open_spans: 0,
            unmatched_stops: 0,
            metadata_ambiguous: false,
        };
        assert!(event_matches(&entry, &analysis, "solver"));
        entry.metadata_ambiguous = true;
        assert!(!event_matches(&entry, &analysis, "solver"));
        assert!(event_matches(&entry, &analysis, "c0xabcd"));
        assert!(event_matches(&entry, &analysis, "歧义"));
    }

    #[test]
    #[ignore = "requires official SDK profile event capture"]
    fn closing_a_capture_cancels_catalog_even_when_the_panel_is_hidden() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../captures/physx-3.4-profile-events.pvdstream");
        let recording = Arc::new(Recording::from_path(path).unwrap());
        let context = egui::Context::default();
        let mut cache = refresh::Cache::default();
        cache.update(
            refresh::Request {
                recording: &recording,
                frame: 0,
                epoch: 0,
                following: false,
                now: Instant::now(),
                context: &context,
                jobs: &crate::background_jobs::BackgroundJobs::default(),
            },
            build_catalog,
        );
        let cancel = cache.job.as_ref().unwrap().cancel.clone();
        let layout_job = refresh::Job::start(
            recording.clone(),
            0,
            context.clone(),
            build_layout,
            &crate::background_jobs::BackgroundJobs::default(),
        )
        .unwrap();
        let layout_cancel = layout_job.cancel.clone();
        let id = egui::Id::new("hidden-profile-catalog");
        context.data_mut(|data| {
            data.insert_temp(
                id,
                EventView {
                    cache,
                    ..Default::default()
                },
            )
        });
        let mut studio = Studio::default();
        studio.recording = Some(recording);
        studio.profile_catalog_cancel = Some(cancel.clone());
        studio.profile_layout_cancel = Some(layout_cancel.clone());
        studio.close();
        assert!(cancel.load(Ordering::Relaxed));
        assert!(layout_cancel.load(Ordering::Relaxed));
        assert!(studio.profile_catalog_cancel.is_none());
        assert!(studio.profile_layout_cancel.is_none());
        assert!(
            context
                .data_mut(|data| data.get_temp::<EventView>(id))
                .unwrap()
                .cache
                .job
                .is_some()
        );
    }
}
