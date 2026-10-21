use super::{Layout, TrackScale, ViewState, refresh};
use crate::{
    full_profile::{Source, TimelineProgress},
    i18n::{Language, Message},
    studio::Studio,
};
use bevy_egui::egui;
use pvd_core::{
    analysis::{Analysis, view::ProfileSource},
    profile_timeline::{
        ClockRatio, GlobalPairingStats, ThreadCursor, ThreadPageReport, TickRange, WindowLimits,
        WindowReport, WindowRequest,
    },
};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

const THREAD_PAGE: usize = 512;
const MAX_BACK_PAGES: usize = 256;

#[derive(Clone)]
enum RenderPhase {
    Requested,
    Loading,
    Loaded { displayed: bool },
    Failed(Message),
}

#[derive(Clone)]
struct RenderQuery {
    epoch: u64,
    phase: RenderPhase,
}

fn render_id() -> egui::Id {
    egui::Id::new("complete-thread-render-request")
}

pub(super) fn prepare_render(context: &egui::Context, epoch: u64) -> Result<bool, Message> {
    let mut request = context
        .data_mut(|data| data.get_temp::<RenderQuery>(render_id()))
        .filter(|request| request.epoch == epoch)
        .unwrap_or(RenderQuery {
            epoch,
            phase: RenderPhase::Requested,
        });
    let result = match &mut request.phase {
        RenderPhase::Loaded { displayed } => {
            let ready = *displayed;
            *displayed = true;
            Ok(ready)
        }
        RenderPhase::Failed(error) => Err(error.clone()),
        _ => Ok(false),
    };
    context.data_mut(|data| data.insert_temp(render_id(), request));
    context.request_repaint();
    result
}

#[derive(Clone)]
struct Overview {
    identity: pvd_core::profile_curves::SourceIdentity,
    clock: ClockRatio,
    range: Option<TickRange>,
    stats: GlobalPairingStats,
    cuda_buffers: u64,
}

impl Overview {
    fn duration_ms(&self) -> f64 {
        self.range.map_or(0.0, |range| {
            (range.end - range.start) as f64 * self.scale().ms_per_tick
        })
    }

    fn scale(&self) -> TrackScale {
        let ms_per_tick = self.clock.numerator as f64 / self.clock.denominator as f64 / 1e6;
        let total = self
            .range
            .map_or(0.0, |range| (range.end - range.start) as f64 * ms_per_tick);
        TrackScale {
            ms_per_tick,
            total: total.max(1e-6),
        }
    }

    fn milliseconds(&self, ticks: TickRange) -> [f64; 2] {
        let first = self.range.map_or(0, |range| range.start);
        [ticks.start, ticks.end]
            .map(|tick| tick.saturating_sub(first) as f64 * self.scale().ms_per_tick)
    }
}

#[derive(Clone, Default)]
struct Form {
    milliseconds: [f64; 2],
    thread: Option<u32>,
    context: String,
}

impl Form {
    fn around(&mut self, overview: &Overview, timestamp: Option<u64>) {
        let Some(range) = overview.range else { return };
        let scale = overview.scale();
        let center =
            timestamp.unwrap_or(range.start).saturating_sub(range.start) as f64 * scale.ms_per_tick;
        let width = overview.duration_ms().min(100.0);
        let start = (center - width * 0.5).clamp(0.0, (overview.duration_ms() - width).max(0.0));
        self.milliseconds = [start, (start + width).min(overview.duration_ms())];
    }

    fn request(&self, overview: &Overview) -> Result<WindowRequest, Message> {
        let range = overview.range.ok_or_else(|| {
            Message::new(
                "The complete source has no finished CPU spans.",
                "完整源没有已结束的 CPU 区间。",
            )
        })?;
        let [start, end] = self.milliseconds;
        let scale = overview.scale();
        if !start.is_finite()
            || !end.is_finite()
            || start < 0.0
            || end < start
            || end > overview.duration_ms()
        {
            return Err(Message::new(
                "Enter a finite, ordered time range inside the complete source.",
                "请输入完整源范围内有序、有限的时间区间。",
            ));
        }
        let context = self.context.trim();
        let context = if context.is_empty() {
            None
        } else {
            let parsed = if let Some(hex) = context
                .strip_prefix("0x")
                .or_else(|| context.strip_prefix("0X"))
            {
                u64::from_str_radix(hex, 16)
            } else {
                context.parse::<u64>()
            };
            Some(parsed.map_err(|_| {
                Message::new(
                    "Context must be blank, an unsigned decimal ID or 0x hexadecimal ID.",
                    "Context 应留空，或填写无符号十进制、0x 十六进制标识。",
                )
            })?)
        };
        Ok(WindowRequest {
            ticks: TickRange {
                start: checked_tick(
                    start,
                    false,
                    range,
                    overview.duration_ms(),
                    scale.ms_per_tick,
                )?,
                end: checked_tick(end, true, range, overview.duration_ms(), scale.ms_per_tick)?,
            },
            thread: self.thread,
            context,
            limits: WindowLimits::default(),
        })
    }
}

fn checked_tick(
    milliseconds: f64,
    end: bool,
    range: TickRange,
    duration: f64,
    ms_per_tick: f64,
) -> Result<u64, Message> {
    if milliseconds == 0.0 {
        return Ok(range.start);
    }
    if milliseconds == duration {
        return Ok(range.end);
    }
    let offset = milliseconds / ms_per_tick;
    let offset = if end { offset.ceil() } else { offset.floor() };
    let error = || {
        Message::new(
            "The time coordinate cannot be represented as an input tick.",
            "该时间坐标无法表示为输入 tick。",
        )
    };
    if !offset.is_finite() || offset < 0.0 || offset >= u64::MAX as f64 {
        return Err(error());
    }
    range
        .start
        .checked_add(offset as u64)
        .map(|tick| tick.min(range.end))
        .ok_or_else(error)
}

struct Loaded {
    request: WindowRequest,
    layout: Arc<Layout>,
    index_bytes: usize,
    spans: usize,
    stats: GlobalPairingStats,
    cuda_buffers: u64,
}

fn build_window(
    report: WindowReport,
    overview: &Overview,
    cancelled: &AtomicBool,
) -> Result<(Loaded, Analysis), Message> {
    let mut analysis = Analysis::default();
    analysis.spans = report.spans;
    for ((zone, event), names) in report.names {
        if cancelled.load(Ordering::Relaxed) {
            return Err(Message::new("Thread layout cancelled", "线程布局已取消"));
        }
        if let Some(zone_name) = names.zone {
            let zone = analysis.zones.entry(zone).or_default();
            zone.name = zone_name;
            if let Some(name) = names.event {
                zone.events.insert(event, name);
            }
        }
    }
    let mut layout = Layout::build_cancellable(&analysis, cancelled)?;
    if let Some(range) = overview.range {
        layout.first = range.start;
        layout.last = range.end;
    }
    Ok((
        Loaded {
            request: report.request,
            layout: Arc::new(layout),
            index_bytes: report.estimated_index_bytes,
            spans: analysis.spans.len(),
            stats: report.global_pairing,
            cuda_buffers: report.legacy_cuda_buffers,
        },
        analysis,
    ))
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ReadMode {
    #[default]
    Indexed,
    Streaming,
}

#[derive(Clone, Default)]
struct State {
    epoch: Option<u64>,
    overview: Option<Overview>,
    form: Form,
    catalog: refresh::Cache<ThreadPageReport>,
    catalog_pending: bool,
    cursor: Option<ThreadCursor>,
    previous: VecDeque<Option<ThreadCursor>>,
    page: usize,
    window: refresh::Cache<Loaded>,
    requested: Option<WindowRequest>,
    read_mode: ReadMode,
    plot: ViewState,
    displayed: Option<[f64; 2]>,
    progress: Arc<Mutex<Option<TimelineProgress>>>,
}

pub(super) fn clear_session(data: &mut egui::util::IdTypeMap) {
    data.remove_by_type::<State>();
}

impl State {
    fn sync(&mut self, epoch: u64) {
        if self.epoch != Some(epoch) {
            self.catalog.cancel();
            self.window.cancel();
            *self = Self {
                epoch: Some(epoch),
                page: 1,
                catalog_pending: true,
                ..Default::default()
            };
        }
    }

    fn cancel(&mut self) {
        self.catalog.cancel();
        self.window.cancel();
        self.catalog.job = None;
        self.window.job = None;
        self.catalog_pending = false;
        self.requested = None;
    }

    fn request_page(&mut self, cursor: Option<ThreadCursor>) {
        self.cancel();
        self.cursor = cursor;
        self.catalog = refresh::Cache::default();
        self.catalog_pending = true;
        self.progress = Arc::default();
    }

    fn request_window(&mut self, request: WindowRequest) {
        self.cancel();
        self.window = refresh::Cache::default();
        self.plot = ViewState::default();
        self.displayed = None;
        self.requested = Some(request);
        self.progress = Arc::default();
    }

    fn set_read_mode(&mut self, mode: ReadMode) {
        if self.read_mode != mode {
            self.read_mode = mode;
            self.window.cancel();
            self.window = refresh::Cache::default();
            self.requested = None;
            self.plot = ViewState::default();
            self.displayed = None;
            if !self.catalog_pending && self.catalog.job.is_none() {
                self.progress = Arc::default();
            }
        }
    }

    fn busy(&self) -> bool {
        self.catalog.job.is_some() || self.window.job.is_some()
    }

    fn poll(&mut self, source: &Source, request: refresh::Request<'_>) {
        let timestamp = request
            .recording
            .frame_info(request.frame)
            .map(|frame| frame.timestamp);
        if self.catalog_pending {
            let source = source.clone();
            let cursor = self.cursor.clone();
            let progress = self.progress.clone();
            let changed = self
                .catalog
                .update_with(request, Some(0), move |_, cancel| {
                    let report =
                        source.thread_page(cursor.as_ref(), THREAD_PAGE, cancel, |status| {
                            update_progress(&progress, TimelineProgress::Streaming(status), cancel)
                        })?;
                    Ok((report, Some(Analysis::default())))
                });
            if changed && let Some(prepared) = &self.catalog.prepared {
                let report = prepared.value.as_ref();
                let overview = Overview {
                    identity: report.identity.clone(),
                    clock: report.clock,
                    range: report.completed_tick_range,
                    stats: report.global_pairing,
                    cuda_buffers: report.legacy_cuda_buffers,
                };
                if self.overview.is_none() {
                    self.form.around(&overview, timestamp);
                }
                self.overview = Some(overview);
            }
            if self.catalog.job.is_none()
                && (self.catalog.prepared.is_some() || self.catalog.error.is_some())
            {
                self.catalog_pending = false;
            }
            if self.catalog.error.is_some() {
                self.window = refresh::Cache::default();
                self.plot = ViewState::default();
                self.displayed = None;
            }
        } else if let (Some(query), Some(overview)) = (self.requested, self.overview.clone()) {
            let source = source.clone();
            let progress = self.progress.clone();
            let mode = self.read_mode;
            let changed = self.window.update_with(request, Some(0), move |_, cancel| {
                let report = match mode {
                    ReadMode::Indexed => {
                        source.thread_window(&overview.identity, query, cancel, |status| {
                            update_progress(&progress, status, cancel)
                        })
                    }
                    ReadMode::Streaming => source.thread_window_streaming(
                        &overview.identity,
                        query,
                        cancel,
                        |status| update_progress(&progress, status, cancel),
                    ),
                }?;
                build_window(report, &overview, cancel)
                    .map(|(loaded, analysis)| (loaded, Some(analysis)))
            });
            if changed && let Some(prepared) = &self.window.prepared {
                self.plot.layout = prepared.value.layout.clone();
                self.plot.rows = self.plot.layout.rows.clone();
                self.plot.filter.clear();
                self.displayed = self
                    .overview
                    .as_ref()
                    .map(|overview| overview.milliseconds(prepared.value.request.ticks));
            }
        }
    }
}

fn update_progress(
    progress: &Mutex<Option<TimelineProgress>>,
    status: TimelineProgress,
    cancelled: &AtomicBool,
) -> bool {
    if let Ok(mut progress) = progress.lock() {
        *progress = Some(status);
    }
    !cancelled.load(Ordering::Relaxed)
}

pub(super) fn show(ui: &mut egui::Ui, studio: &mut Studio, source: Source, compact: bool) {
    let Some(recording) = studio.recording.clone() else {
        return;
    };
    let language = studio.prefs.language;
    let id = ui.id().with("complete-thread-profile");
    let mut state = ui
        .ctx()
        .data_mut(|data| data.get_temp::<State>(id))
        .unwrap_or_default();
    state.sync(studio.profile_view_epoch);
    state.poll(
        &source,
        refresh::Request {
            recording: &recording,
            frame: studio.frame,
            epoch: studio.profile_view_epoch,
            following: false,
            now: Instant::now(),
            context: ui.ctx(),
            jobs: &studio.background_jobs,
        },
    );
    advance_render(ui.ctx(), studio, &mut state);
    controls(
        ui,
        studio,
        &mut state,
        compact,
        source.can_rebuild_cpu_index(),
    );
    studio.profile_layout_cancel = state
        .catalog
        .job
        .as_ref()
        .map(|job| job.cancel.clone())
        .or_else(|| state.window.job.as_ref().map(|job| job.cancel.clone()));
    if state.busy() {
        ui.horizontal(|ui| {
            let progress = state.progress.lock().ok().and_then(|value| *value);
            ui.spinner();
            ui.label(if state.catalog.job.is_some() {
                language.text(
                    "Scanning the complete thread catalog…",
                    "正在扫描完整线程目录…",
                )
            } else {
                use pvd_core::profile_disk_index::Phase;
                match progress {
                    Some(TimelineProgress::Index(progress)) => match progress.phase {
                        Phase::Decode | Phase::Metadata => {
                            language.text("Building the timeline index…", "正在建立时间线索引…")
                        }
                        Phase::Merge => {
                            language.text("Organizing the timeline index…", "正在整理时间线索引…")
                        }
                        Phase::VerifySource => {
                            language.text("Verifying the recording file…", "正在核验录制文件…")
                        }
                        Phase::Query => {
                            language.text("Reading the time window…", "正在读取时间窗…")
                        }
                    },
                    _ => language.text("Loading the selected time window…", "正在载入所选时间窗…"),
                }
            });
            if let Some(progress) = progress
                && !matches!(progress, TimelineProgress::Index(p) if p.phase == pvd_core::profile_disk_index::Phase::Merge)
            {
                let fraction = match progress {
                    TimelineProgress::Streaming(progress) => {
                        progress.bytes_read as f64 / progress.total_bytes.max(1) as f64
                    }
                    TimelineProgress::Index(progress) => {
                        progress.completed as f64 / progress.total.max(1) as f64
                    }
                };
                ui.add(egui::ProgressBar::new(fraction as f32).desired_width(80.0));
            }
            if ui.small_button(language.text("Cancel", "取消")).clicked() {
                let catalog = state.catalog.job.is_some();
                state.cancel();
                let error = Message::new(
                    "Complete-file timeline query cancelled; adjust the range and load again.",
                    "完整文件时间线查询已取消；可调整范围后重新载入。",
                );
                if catalog {
                    state.catalog.error = Some(error);
                } else {
                    state.window.error = Some(error);
                }
            }
        });
    }
    if let Some(error) = state.catalog.error.clone().or(state.window.error.clone()) {
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(
                egui::Color32::LIGHT_RED,
                if compact {
                    language.text("Timeline query failed", "时间线查询失败")
                } else {
                    error.text(language)
                },
            )
            .on_hover_text(error.text(language));
            if state.catalog.error.is_some()
                && ui
                    .small_button(language.text("Retry catalog", "重试目录"))
                    .clicked()
            {
                state.request_page(state.cursor.clone());
            }
            if state.window.error.is_some()
                && source.can_rebuild_cpu_index()
                && state.read_mode == ReadMode::Indexed
                && let Some(request) = state.requested
                && ui
                    .small_button(language.text("Scan file", "直接扫描"))
                    .clicked()
            {
                state.set_read_mode(ReadMode::Streaming);
                state.request_window(request);
            }
            if state.window.error.is_some()
                && source.can_rebuild_cpu_index()
                && state.read_mode == ReadMode::Indexed
                && let Some(request) = state.requested
                && ui
                    .small_button(language.text("Rebuild index", "重建索引"))
                    .clicked()
            {
                source.rebuild_cpu_index();
                state.request_window(request);
            }
        });
    }
    if let Some(overview) = &state.overview
        && overview.range.is_none()
        && !state.busy()
    {
        ui.label(language.text(
            "No finished CPU spans define a time range in this source.",
            "源中没有构成时间范围的已结束 CPU 区间。",
        ));
        diagnostics(ui, overview.stats, overview.cuda_buffers, language);
    }
    if let (Some(prepared), Some(overview)) =
        (state.window.prepared.clone(), state.overview.clone())
    {
        let analysis = &prepared.analysis;
        if analysis.profile_spans().len() == 0 {
            ui.label(language.text(
                "No complete CPU spans intersect the loaded window and filters.",
                "载入的时间窗和过滤条件内没有完整 CPU 区间。",
            ));
        } else {
            let filter = studio.profile_filter.to_lowercase();
            if state.plot.filter != filter {
                state.plot.rows = state.plot.layout.filtered(analysis, &filter);
                state.plot.filter = filter;
            }
            if !ui.input(|input| input.pointer.primary_down()) {
                state.plot.anchor = None;
            }
            let loaded = overview.milliseconds(prepared.value.request.ticks);
            let window =
                super::time_window(state.displayed.unwrap_or(loaded), overview.scale().total);
            let window = super::show_tracks(
                ui,
                studio,
                analysis,
                &mut state.plot,
                id.with("tracks"),
                window,
                overview.scale(),
            );
            state.displayed = Some(clamp_loaded(window, loaded, overview.scale().total));
        }
    } else if !state.busy()
        && state.catalog.error.is_none()
        && state.window.error.is_none()
        && state
            .overview
            .as_ref()
            .is_none_or(|overview| overview.range.is_some())
    {
        ui.label(language.text("Choose Load window to inspect the complete SDK file. Scrolling never scans the source.", "点击“载入时间窗”查看完整 SDK 文件；滚动操作不会扫描源文件。"));
    }
    ui.ctx().data_mut(|data| data.insert_temp(id, state));
}

fn advance_render(context: &egui::Context, studio: &Studio, state: &mut State) {
    let Some(mut request) = context
        .data_mut(|data| data.get_temp::<RenderQuery>(render_id()))
        .filter(|request| request.epoch == studio.profile_view_epoch)
    else {
        return;
    };
    if let Some(error) = state.catalog.error.as_ref().or(state.window.error.as_ref()) {
        request.phase = RenderPhase::Failed(error.clone());
    } else if matches!(request.phase, RenderPhase::Requested)
        && let Some(overview) = &state.overview
    {
        let mut form = Form::default();
        form.around(
            overview,
            studio
                .recording
                .as_ref()
                .and_then(|recording| recording.frame_info(studio.frame))
                .map(|frame| frame.timestamp),
        );
        match form.request(overview) {
            Ok(query) => {
                state.form = form;
                state.request_window(query);
                request.phase = RenderPhase::Loading;
            }
            Err(error) => request.phase = RenderPhase::Failed(error),
        }
    } else if matches!(request.phase, RenderPhase::Loading)
        && let Some(prepared) = &state.window.prepared
    {
        if prepared.analysis.profile_spans().len() == 0 {
            request.phase = RenderPhase::Failed(Message::new(
                "The render-check window has no complete CPU spans.",
                "渲染检查窗口中没有完整 CPU 区间。",
            ));
        } else {
            println!(
                "Thread render window loaded: {}",
                serde_json::json!({"epoch":request.epoch,"frame":studio.frame,"start_tick":prepared.value.request.ticks.start,"end_tick":prepared.value.request.ticks.end,"complete_spans":prepared.analysis.profile_spans().len(),"source_bytes":state.overview.as_ref().unwrap().identity.bytes,"source_frames":state.overview.as_ref().unwrap().identity.frames,"source_blake3":blake3::Hash::from(state.overview.as_ref().unwrap().identity.blake3).to_hex().to_string()})
            );
            request.phase = RenderPhase::Loaded { displayed: false };
        }
    }
    context.data_mut(|data| data.insert_temp(render_id(), request));
}

fn clamp_loaded(window: [f64; 2], loaded: [f64; 2], total: f64) -> [f64; 2] {
    let loaded = super::time_window(loaded, total);
    let width = (window[1] - window[0]).min(loaded[1] - loaded[0]);
    let start = window[0].clamp(loaded[0], (loaded[1] - width).max(loaded[0]));
    [start, start + width]
}

fn controls(
    ui: &mut egui::Ui,
    studio: &mut Studio,
    state: &mut State,
    compact: bool,
    indexed: bool,
) {
    let language = studio.prefs.language;
    let narrow_filter = compact && ui.available_width() < 520.0;
    let query = state
        .overview
        .as_ref()
        .map(|overview| state.form.request(overview));
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !state.busy() && query.as_ref().is_some_and(Result::is_ok),
                egui::Button::new(language.text("Load window", "载入时间窗")),
            )
            .clicked()
            && let Some(Ok(query)) = &query
        {
            state.request_window(*query);
        }
        crate::ui::settings_menu(
            ui,
            if compact {
                language.text("Window…", "时间窗…")
            } else {
                language.text("Window settings", "时间窗设置")
            },
            |ui| {
                ui.set_max_width(480.0);
                query_settings(ui, studio, state, narrow_filter);
                if indexed {
                    ui.separator();
                    let mut mode = state.read_mode;
                    ui.radio_value(&mut mode, ReadMode::Indexed, language.text("Use disk index", "使用磁盘索引"));
                    ui.radio_value(&mut mode, ReadMode::Streaming, language.text("Scan file without index", "直接扫描（不建索引）"));
                    state.set_read_mode(mode);
                    ui.label(language.text(
                        "The disk index uses pvd-cache/profile (shared limit 1 GiB, 512 MiB free reserve). Streaming reads the full file for each window without creating or waiting for an index. Both modes verify the source and apply the same window limits; the thread catalog scans separately.",
                        "磁盘索引使用 pvd-cache/profile（共享上限 1 GiB，保留 512 MiB 可用空间）。流式查询每次读取完整文件，不建立或等待索引。两种方式均核验源并遵守相同窗口上限；线程目录仍单独扫描。",
                    ));
                }
            },
        );
        if compact && !narrow_filter {
            ui.add(
                egui::TextEdit::singleline(&mut studio.profile_filter)
                    .desired_width(110.0_f32.min(ui.available_width()))
                    .hint_text(language.text("Filter loaded tracks", "筛选已载入轨道")),
            );
        }
        if let (Some(prepared), Some(overview)) = (&state.window.prepared, &state.overview) {
            let loaded = overview.milliseconds(prepared.value.request.ticks);
            let draft = query.as_ref().is_none_or(|query| {
                !query
                    .as_ref()
                    .is_ok_and(|query| same_request(*query, prepared.value.request))
            });
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!(
                        "{} {:.3}–{:.3} ms{}",
                        language.text("Loaded CPU", "已载入 CPU"),
                        loaded[0],
                        loaded[1],
                        if draft {
                            language.text(" · draft differs", " · 参数待载入")
                        } else {
                            ""
                        }
                    ))
                    .small(),
                )
                .truncate(),
            )
            .on_hover_ui(|ui| loaded_detail(ui, prepared.value.as_ref(), overview, language));
        } else if let Some(Err(error)) = &query {
            ui.label(
                egui::RichText::new(
                    language.text("Adjust range or Context", "请调整范围或 Context"),
                )
                .small(),
            )
            .on_hover_text(error.text(language));
        }
    });
}

fn same_request(a: WindowRequest, b: WindowRequest) -> bool {
    a.ticks == b.ticks && a.thread == b.thread && a.context == b.context
}

fn query_settings(ui: &mut egui::Ui, studio: &mut Studio, state: &mut State, show_filter: bool) {
    let language = studio.prefs.language;
    if show_filter {
        ui.add(
            egui::TextEdit::singleline(&mut studio.profile_filter)
                .desired_width(180.0)
                .hint_text(language.text("Filter loaded tracks", "筛选已载入轨道")),
        );
    }
    if let Some(overview) = state.overview.clone() {
        if let Some(range) = overview.range {
            ui.label(format!(
                "{} 0–{:.6} ms",
                language.text("Complete source", "完整源范围"),
                overview.milliseconds(range)[1]
            ));
            ui.monospace(format!(
                "ticks {}–{} · {}/{} ns/tick",
                range.start, range.end, overview.clock.numerator, overview.clock.denominator
            ));
            ui.horizontal_wrapped(|ui| {
                ui.label(language.text("From", "起点"));
                ui.add(
                    egui::DragValue::new(&mut state.form.milliseconds[0])
                        .speed(0.1)
                        .range(0.0..=overview.duration_ms())
                        .max_decimals(9)
                        .suffix(" ms"),
                );
                ui.label(language.text("to", "至"));
                ui.add(
                    egui::DragValue::new(&mut state.form.milliseconds[1])
                        .speed(0.1)
                        .range(0.0..=overview.duration_ms())
                        .max_decimals(9)
                        .suffix(" ms"),
                );
            });
            if ui
                .button(language.text("Around current frame (100 ms)", "当前帧附近（100 ms）"))
                .clicked()
            {
                state.form.around(
                    &overview,
                    studio
                        .recording
                        .as_ref()
                        .and_then(|recording| recording.frame_info(studio.frame))
                        .map(|frame| frame.timestamp),
                );
            }
        } else {
            ui.label(language.text("No complete CPU span range; pending starts and unmatched stops do not define time bounds.", "没有完整 CPU 区间范围；未结束 start 和未匹配 stop 不构成时间边界。"));
        }
        diagnostics(ui, overview.stats, overview.cuda_buffers, language);
    }
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("complete-thread-filter")
            .width(180.0)
            .selected_text(state.form.thread.map_or_else(
                || language.text("All threads", "所有线程").to_owned(),
                |thread| format!("{} {thread}", language.text("Thread", "线程")),
            ))
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut state.form.thread,
                    None,
                    language.text("All threads", "所有线程"),
                );
                if let Some(prepared) = &state.catalog.prepared {
                    egui::ScrollArea::vertical()
                        .max_height(180.0)
                        .show(ui, |ui| {
                            for thread in &prepared.value.threads {
                                ui.selectable_value(
                                    &mut state.form.thread,
                                    Some(thread.thread),
                                    format!(
                                        "{} {} · {}",
                                        language.text("Thread", "线程"),
                                        thread.thread,
                                        thread.complete_spans
                                    ),
                                )
                                .on_hover_text(format!(
                                    "{} {} · {} {}",
                                    thread.open_starts,
                                    language.text("open starts", "个未结束 start"),
                                    thread.unmatched_stops,
                                    language.text("unmatched stops", "个未匹配 stop")
                                ));
                            }
                        });
                }
            });
        ui.label("Context");
        ui.add(
            egui::TextEdit::singleline(&mut state.form.context)
                .desired_width(150.0)
                .char_limit(20)
                .hint_text(language.text("All / decimal / 0x…", "全部 / 十进制 / 0x…")),
        );
    });
    let next = state
        .catalog
        .prepared
        .as_ref()
        .and_then(|prepared| prepared.value.next.clone());
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(
                !state.busy() && state.cursor.is_some(),
                egui::Button::new(language.text("First threads", "线程首页")),
            )
            .clicked()
        {
            state.previous.clear();
            state.page = 1;
            state.request_page(None);
        }
        if ui
            .add_enabled(
                !state.busy() && !state.previous.is_empty(),
                egui::Button::new(language.text("Previous threads", "上一页线程")),
            )
            .clicked()
            && let Some(cursor) = state.previous.pop_back()
        {
            state.page -= 1;
            state.request_page(cursor);
        }
        ui.label(format!(
            "{} {}",
            language.text("Thread page", "线程页"),
            state.page
        ));
        if ui
            .add_enabled(
                !state.busy() && next.is_some(),
                egui::Button::new(language.text("Next threads", "下一页线程")),
            )
            .clicked()
            && let Some(next) = next
        {
            if state.previous.len() == MAX_BACK_PAGES {
                state.previous.pop_front();
            }
            state.previous.push_back(state.cursor.clone());
            state.page = state.page.saturating_add(1);
            state.request_page(Some(next));
        }
    });
    ui.label(language.text("Thread IDs are paged in numeric order. Each page and Load window scans the complete SDK file; panning and zooming only inspect the loaded window.", "线程标识按数值分页。每次翻页或载入时间窗都扫描完整 SDK 文件；平移、缩放只查看已载入窗口。"));
    ui.label(language.text("A query retains at most 200,000 spans and a 32 MiB estimated index; layout and scanner memory are separate. Narrow the range or filter thread/Context if a query exceeds its limits.", "一次查询最多保留 200,000 个区间与 32 MiB 估算索引；轨道和扫描器内存另计。超限时可缩小范围或筛选线程、Context。"));
    ui.separator();
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                state.window.prepared.is_some(),
                egui::Button::new(language.text("Fit loaded window", "适配已载入窗口")),
            )
            .clicked()
            && let (Some(prepared), Some(overview)) = (&state.window.prepared, &state.overview)
        {
            state.displayed = Some(overview.milliseconds(prepared.value.request.ticks));
        }
        ui.toggle_value(
            &mut state.plot.measure,
            language.text("Measure interval", "测量区间"),
        );
        if ui
            .button(language.text("Clear measurement", "清除测量"))
            .clicked()
        {
            state.plot.selection = None;
        }
    });
    if let Some([start, end]) = state.plot.selection {
        ui.label(format!(
            "{start:.6} → {end:.6} ms · Δ {:.6} ms",
            end - start
        ));
    }
    if let (Some(prepared), Some(overview)) = (&state.window.prepared, &state.overview) {
        loaded_detail(ui, prepared.value.as_ref(), overview, language);
    }
    ui.label(language.text("Time is relative to the complete source's first finished CPU span. Tracks are arranged for this window, not stable lanes across the whole capture. Original endpoints and flush-frame attribution are retained.", "时间相对完整源首个已结束 CPU 区间；轨道仅按当前窗口排布，不表示全录制稳定轨道。保留原始端点和数据刷新帧归属。"));
}

fn diagnostics(
    ui: &mut egui::Ui,
    stats: GlobalPairingStats,
    cuda_buffers: u64,
    language: Language,
) {
    ui.label(format!(
        "{}: {} {} · {} {} · {} {}",
        language.text("Whole-source pairing", "全源配对统计"),
        stats.complete_spans,
        language.text("complete", "完整"),
        stats.open_starts,
        language.text("open starts", "未结束 start"),
        stats.unmatched_stops,
        language.text("unmatched stops", "未匹配 stop")
    ));
    if cuda_buffers != 0 {
        ui.colored_label(egui::Color32::YELLOW, format!("{} {cuda_buffers}", language.text("Legacy CUDA buffers skipped; this is a CPU timeline, not a complete GPU timeline:", "已跳过旧 CUDA 缓冲；此处为 CPU 时间线，不是完整 GPU 时间线：")));
    }
}

fn loaded_detail(ui: &mut egui::Ui, loaded: &Loaded, overview: &Overview, language: Language) {
    let bounds = overview.milliseconds(loaded.request.ticks);
    ui.label(format!(
        "{} {:.9}–{:.9} ms · ticks {}–{}",
        language.text("Actual loaded range", "实际载入范围"),
        bounds[0],
        bounds[1],
        loaded.request.ticks.start,
        loaded.request.ticks.end
    ));
    ui.label(format!(
        "{} {:?} · Context {:?} · {} {}",
        language.text("Thread", "线程"),
        loaded.request.thread,
        loaded.request.context,
        loaded.spans,
        language.text("complete intersecting spans", "个相交完整区间")
    ));
    ui.label(format!(
        "{} {} B",
        language.text(
            "Estimated query index (layout and scanner memory separate):",
            "查询索引估算（轨道和扫描器另计）："
        ),
        loaded.index_bytes
    ));
    diagnostics(ui, loaded.stats, loaded.cuda_buffers, language);
}

#[cfg(test)]
mod tests;
