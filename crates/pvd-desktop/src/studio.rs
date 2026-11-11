use std::{
    collections::VecDeque,
    fs::File,
    io::{Read, Seek},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use crate::i18n::{Language, Message};
use crate::network::{
    LiveEvent, LiveProfileSnapshot, LiveSnapshot, NetworkOptions, NetworkSession,
};
use bevy::prelude::Resource;
use egui_dock::{DockState, NodeIndex};
use pvd_core::{LiveRecording, Recording, SceneState, analysis::view::ProfileSource};
use pvd_transport::{ListenConfig, SpoolSnapshot};
use serde::{Deserialize, Serialize};

const MAX_CAPTURE_BYTES: u64 = 512 * 1024 * 1024;
const IN_MEMORY_SDK_BYTES: u64 = 32 * 1024 * 1024;

mod action_probe;
mod analysis_demand;
mod archive;
mod display_status;
mod frame_sources;
pub use display_status::{DisplayMode, DisplayReason};
#[cfg(test)]
mod analysis_demand_tests;
#[cfg(test)]
mod analysis_file_tests;
#[cfg(all(test, windows))]
mod apex_tcp_tests;
#[cfg(test)]
mod clip_tests;
mod coordinates;
mod layout;
mod native_export;
mod playback_recovery;
#[cfg(test)]
mod playback_tests;
#[cfg(all(test, windows))]
mod profile_index_tests;
mod retained_capture;
mod seeking;
mod selection;
mod ui_actions;
pub use ui_actions::UiAction;
#[cfg(test)]
mod shutdown_tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Panel {
    Objects,
    Viewport,
    Inspector,
    Timeline,
    Clips,
    Performance,
    Memory,
    Curves,
    Diagnostics,
    Display,
}

impl Panel {
    pub const ALL: [Self; 10] = [
        Self::Objects,
        Self::Viewport,
        Self::Inspector,
        Self::Timeline,
        Self::Clips,
        Self::Performance,
        Self::Memory,
        Self::Curves,
        Self::Diagnostics,
        Self::Display,
    ];

    pub fn label(self, language: Language) -> &'static str {
        match self {
            Self::Objects => language.text("Objects", "对象"),
            Self::Viewport => language.text("Scene", "场景"),
            Self::Inspector => language.text("Properties", "属性"),
            Self::Timeline => language.text("Timeline", "时间轴"),
            Self::Clips => language.text("Clips", "片段"),
            Self::Performance => language.text("Performance", "性能"),
            Self::Memory => language.text("Memory", "内存"),
            Self::Curves => language.text("Curves", "曲线"),
            Self::Diagnostics => language.text("Diagnostics", "诊断"),
            Self::Display => language.text("Display", "显示"),
        }
    }

    pub fn is_closeable(self) -> bool {
        self != Self::Viewport
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObjectScope {
    #[default]
    All,
    Selected,
    Unselected,
}

impl ObjectScope {
    pub fn includes(self, selected: bool) -> bool {
        match self {
            Self::All => true,
            Self::Selected => selected,
            Self::Unselected => !selected,
        }
    }

    pub fn label(self, language: Language) -> &'static str {
        match self {
            Self::All => language.text("All", "全部"),
            Self::Selected => language.text("Selected", "已选"),
            Self::Unselected => language.text("Unselected", "未选"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayColors {
    pub static_body: [f32; 3],
    pub dynamic_body: [f32; 3],
    pub sleeping_body: [f32; 3],
    pub kinematic_body: [f32; 3],
    pub cloth: [f32; 3],
}

impl Default for DisplayColors {
    fn default() -> Self {
        Self {
            static_body: [0.28, 0.34, 0.42],
            dynamic_body: [0.56, 0.67, 0.79],
            sleeping_body: [0.36, 0.43, 0.52],
            kinematic_body: [0.68, 0.47, 0.81],
            cloth: [0.85, 0.74, 0.5],
        }
    }
}

impl DisplayColors {
    pub(crate) fn sanitize(&mut self) {
        let defaults = Self::default();
        for (color, fallback) in [
            (&mut self.static_body, defaults.static_body),
            (&mut self.dynamic_body, defaults.dynamic_body),
            (&mut self.sleeping_body, defaults.sleeping_body),
            (&mut self.kinematic_body, defaults.kinematic_body),
            (&mut self.cloth, defaults.cloth),
        ] {
            for (channel, fallback) in color.iter_mut().zip(fallback) {
                *channel = if channel.is_finite() {
                    channel.clamp(0.0, 1.0)
                } else {
                    fallback
                };
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub language: Language,
    #[serde(serialize_with = "serialize_dock")]
    pub dock: DockState<Panel>,
    #[serde(flatten)]
    pub listener: crate::listen_settings::ListenSettings,
    pub auto_capture: bool,
    pub capture_directory: String,
    pub history_directory: String,
    pub history_budget_gib: u32,
    pub receive_budget_gib: u32,
    pub min_free_disk_mib: u32,
    pub live_refresh_hz: u32,
    pub raw_capture_limit_gib: u32,
    pub show_grid: bool,
    pub show_axes: bool,
    pub show_viewport_help: bool,
    pub show_debug_draw: bool,
    pub show_contacts: bool,
    pub show_query_rays: bool,
    pub show_query_sweeps: bool,
    pub show_query_overlaps: bool,
    pub show_query_hits: bool,
    pub show_query_normals: bool,
    pub show_bounds: bool,
    pub show_com: bool,
    pub show_velocity: bool,
    pub show_local_axes: bool,
    pub show_cloth_points: bool,
    pub show_particles: bool,
    pub show_cloth_constraints: bool,
    pub point_radius: f32,
    pub bounds_scope: ObjectScope,
    pub com_scope: ObjectScope,
    pub local_axes_scope: ObjectScope,
    pub velocity_scope: ObjectScope,
    pub opacity_scope: ObjectScope,
    pub sleeping_scope: ObjectScope,
    pub show_triggers: bool,
    pub trigger_scope: ObjectScope,
    pub display_colors: DisplayColors,
    pub gizmo_scale: f32,
    pub gizmo_line_width: f32,
    pub velocity_scale: f32,
    pub joint_frame_scale: f32,
    pub wireframe: bool,
    pub show_kinematic: bool,
    pub show_static: bool,
    pub show_dynamic: bool,
    pub show_sleeping: bool,
    pub opacity: f32,
    pub camera_near: f32,
    pub camera_far: f32,
    pub camera_speed: f32,
    pub camera_mode: crate::camera_controller::CameraMode,
    pub fly_speed: f32,
    pub up_axis: crate::coordinates::UpAxis,
    pub up_mode: crate::coordinates::UpMode,
    pub custom_up: [f32; 3],
    pub handedness: crate::coordinates::Handedness,
    pub meters_per_unit: f32,
}

fn serialize_dock<S: serde::Serializer>(
    dock: &DockState<Panel>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    layout::validate(dock).map_err(serde::ser::Error::custom)?;
    let mut dock = dock.clone();
    // Paint rectangles are rebuilt during layout; their initial infinities cannot round-trip through JSON.
    for (_, node) in dock.iter_all_nodes_mut() {
        node.set_rect(egui::Rect::ZERO);
        if let Some(leaf) = node.get_leaf_mut() {
            leaf.viewport = egui::Rect::ZERO;
        }
    }
    dock.serialize(serializer)
}

impl Default for Preferences {
    fn default() -> Self {
        let mut dock = DockState::new(vec![Panel::Viewport]);
        let [main, _] =
            dock.main_surface_mut()
                .split_left(NodeIndex::root(), 0.19, vec![Panel::Objects]);
        let [main, _] =
            dock.main_surface_mut()
                .split_right(main, 0.73, vec![Panel::Inspector, Panel::Display]);
        dock.main_surface_mut().split_below(
            main,
            0.70,
            vec![
                Panel::Timeline,
                Panel::Clips,
                Panel::Performance,
                Panel::Memory,
                Panel::Curves,
                Panel::Diagnostics,
            ],
        );
        Self {
            language: Language::default(),
            dock,
            listener: crate::listen_settings::ListenSettings::default(),
            auto_capture: false,
            capture_directory: "captures".into(),
            history_directory: pvd_core::live::HistoryConfig::default()
                .directory
                .to_string_lossy()
                .into_owned(),
            history_budget_gib: 8,
            receive_budget_gib: 0,
            min_free_disk_mib: 512,
            live_refresh_hz: 60,
            raw_capture_limit_gib: 0,
            show_grid: true,
            show_axes: true,
            show_viewport_help: true,
            show_debug_draw: true,
            show_contacts: true,
            show_query_rays: true,
            show_query_sweeps: true,
            show_query_overlaps: true,
            show_query_hits: true,
            show_query_normals: true,
            show_bounds: false,
            show_com: false,
            show_velocity: false,
            show_local_axes: false,
            show_cloth_points: true,
            show_particles: true,
            show_cloth_constraints: true,
            point_radius: 0.025,
            bounds_scope: ObjectScope::All,
            com_scope: ObjectScope::All,
            local_axes_scope: ObjectScope::All,
            velocity_scope: ObjectScope::All,
            opacity_scope: ObjectScope::All,
            sleeping_scope: ObjectScope::All,
            show_triggers: true,
            trigger_scope: ObjectScope::All,
            display_colors: DisplayColors::default(),
            gizmo_scale: 1.0,
            gizmo_line_width: crate::line_render::DEFAULT_LINE_WIDTH,
            velocity_scale: 1.0,
            joint_frame_scale: 1.0,
            wireframe: false,
            show_kinematic: true,
            show_static: true,
            show_dynamic: true,
            show_sleeping: true,
            opacity: 1.0,
            camera_near: 0.01,
            camera_far: 1_000_000.0,
            camera_speed: 1.0,
            camera_mode: crate::camera_controller::CameraMode::default(),
            fly_speed: 10.0,
            up_axis: crate::coordinates::UpAxis::default(),
            up_mode: crate::coordinates::UpMode::default(),
            custom_up: [0.0, 1.0, 0.0],
            handedness: crate::coordinates::Handedness::default(),
            meters_per_unit: 1.0,
        }
    }
}

impl Preferences {
    fn path() -> PathBuf {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("pvd-studio")
            .join("layout.json")
    }
    pub fn load() -> Self {
        std::fs::read(Self::path())
            .ok()
            .map(|bytes| Self::from_bytes(&bytes))
            .unwrap_or_default()
    }
    fn from_bytes(bytes: &[u8]) -> Self {
        let mut preferences: Self = serde_json::from_slice(bytes).unwrap_or_else(|_| {
            let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
                return Self::default();
            };
            if let Some(fields) = value.as_object_mut() {
                fields.remove("dock");
            }
            serde_json::from_value(value).unwrap_or_default()
        });
        layout::repair(&mut preferences.dock);
        preferences.min_free_disk_mib = preferences.min_free_disk_mib.max(512);
        preferences.display_colors.sanitize();
        if Path::new(&preferences.history_directory) == std::env::temp_dir().join("pvd-studio-live")
        {
            preferences.history_directory = pvd_core::live::HistoryConfig::default()
                .directory
                .to_string_lossy()
                .into_owned();
        }
        preferences
    }
    pub(crate) fn validate_dock(dock: &DockState<Panel>) -> Result<(), &'static str> {
        layout::validate(dock)
    }
    pub fn show_panel(&mut self, panel: Panel) {
        layout::show_panel(&mut self.dock, panel);
    }
    pub fn hide_panel(&mut self, panel: Panel) {
        if panel.is_closeable()
            && let Some(location) = self.dock.find_tab(&panel)
        {
            self.dock.remove_tab(location);
        }
    }
    pub fn save(&self) -> Result<(), String> {
        self.save_to(&Self::path())
    }
    fn save_to(&self, path: &Path) -> Result<(), String> {
        let data = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, data).map_err(|e| e.to_string())
    }
}

type LoadResult = Result<LoadedCapture, Message>;

struct LoadedCapture {
    recording: Recording,
    bytes: Option<Arc<Vec<u8>>>,
    source_path: Option<PathBuf>,
    source_identity: Option<crate::clip_store::Identity>,
    clips: Option<Result<crate::clip_store::Loaded, crate::clip_store::Error>>,
}

fn playback_interval(
    recording: Option<&Recording>,
    current: usize,
    next: usize,
    recorded: bool,
    fps: f32,
) -> Duration {
    let fps = if fps.is_finite() {
        fps.clamp(1.0, 240.0)
    } else {
        30.0
    };
    let fallback = Duration::from_secs_f64(1.0 / f64::from(fps));
    if !recorded {
        return fallback;
    }
    let Some(recording) = recording else {
        return fallback;
    };
    let Some(first) = recording.frame_info(current.min(next)) else {
        return fallback;
    };
    let Some(last) = recording.frame_info(current.max(next)) else {
        return fallback;
    };
    let Some(ticks) = last.timestamp.checked_sub(first.timestamp) else {
        return fallback;
    };
    let seconds = ticks as f64 * recording.header.timestamp_numerator as f64
        / recording.header.timestamp_denominator as f64
        / 1_000_000_000.0;
    Duration::try_from_secs_f64(seconds.max(0.000_001)).unwrap_or(fallback)
}

pub struct LoadJob {
    result: Mutex<mpsc::Receiver<LoadResult>>,
    pub cancelled: Arc<AtomicBool>,
    pub progress: Arc<AtomicU64>,
    pub label: String,
    pub started: Instant,
    pub live: bool,
    pub final_snapshot: bool,
}

#[derive(Resource)]
pub struct Studio {
    pub(crate) display_trace: Option<crate::display_probe::Trace>,
    pub(crate) display_request: u64,
    pub prefs: Preferences,
    pub recording: Option<Arc<Recording>>,
    pub state: SceneState,
    pub scene_changes: crate::scene::SceneChanges,
    up_resolver: crate::coordinates::UpResolver,
    pub activity: crate::activity::ActivityCache,
    pub bytes: Option<Arc<Vec<u8>>>,
    pub source: String,
    pub path_input: String,
    pub export_path: String,
    pub native_export_path: String,
    pub filter: String,
    pub selected: Option<u64>,
    selection: selection::Selection,
    pub frame: usize,
    selected_frame_source: Option<pvd_core::frame_sources::FrameSourceKey>,
    displayed_frame_source: Option<frame_sources::FrameIdentity>,
    live_frame_source: Option<frame_sources::FrameIdentity>,
    live_analysis: pvd_core::analysis::SharedAnalysis,
    pub playing: i32,
    pub playback_fps: f32,
    pub recorded_timing: bool,
    pub last_step: Instant,
    pub revision: u64,
    pub job: Option<LoadJob>,
    pub notices: Vec<Message>,
    pub initialized: bool,
    pub focus_selected: bool,
    pub follow_selected: bool,
    pub last_layout_save: Instant,
    pub last_layout: String,
    pub persist_layout: bool,
    pub server: Option<NetworkSession>,
    pub receive_status: Option<SpoolSnapshot>,
    pub live_profile_enabled: bool,
    pub last_live_profile: Option<LiveProfileSnapshot>,
    pub live_session: Option<u64>,
    pub live_connected: bool,
    pub follow_live: bool,
    display_reason: Option<DisplayReason>,
    live_state: SceneState,
    live_recording: Option<Arc<Recording>>,
    live_frame: usize,
    live_update_times: VecDeque<Instant>,
    seek_worker: Option<seeking::Worker>,
    pub seeking: Option<usize>,
    analysis_demand: bool,
    #[cfg(test)]
    force_full_playback_analysis: bool,
    #[cfg(test)]
    pending_playback_rebase: Option<playback_tests::RebaseWitness>,
    #[cfg(test)]
    accepted_playback_rebase: Option<playback_tests::RebaseWitness>,
    displayed_analysis: analysis_demand::DisplayedAnalysis,
    playback_recovery: playback_recovery::Recovery,
    pub background_jobs: crate::background_jobs::BackgroundJobs,
    shutdown_started: bool,
    ui_actions: ui_actions::UiActions,
    pub capture_path: Option<PathBuf>,
    source_identity: Option<crate::clip_store::Identity>,
    pub connection: Message,
    pub profile_filter: String,
    pub profile_events_view: bool,
    pub profile_catalog_cancel: Option<Arc<AtomicBool>>,
    pub profile_layout_cancel: Option<Arc<AtomicBool>>,
    pub profile_view_epoch: u64,
    profile_cpu_index: Arc<crate::full_profile::IndexSlot>,
    pub profile_window: Option<[f64; 2]>,
    pub memory_panel: crate::memory::PanelState,
    pub error_rows: Arc<Vec<(Option<usize>, pvd_core::ErrorEvent)>>,
    pub file_picker: crate::file_picker::FilePicker,
    pub save_job: Option<Mutex<mpsc::Receiver<Message>>>,
    pub recording_export: Option<crate::recording_export::Job>,
    pub captured_camera: Option<String>,
    pub export_workers: crate::recording_export::Workers,
    pub curves: crate::curve::Workspace,
    pub curve_job: Option<crate::curve::Job>,
    pub curve_export_path: String,
    pub analysis_export_path: String,
    pub requested_panel: Option<Panel>,
    pub retained_path: String,
    pub retained_first: usize,
    pub retained_last: usize,
    capture_epoch: Arc<AtomicU64>,
    pub trim_first: usize,
    pub trim_last: usize,
    pub clip_set: crate::clips::ClipSet,
    pub clip_persistence: crate::clips::Persistence,
    pub pending_clip_action: Option<crate::clips::PendingAction>,
    pub exit_requested: bool,
    pub repx_path: String,
    pub tree: crate::tree::TreeCache,
}

impl Default for Studio {
    fn default() -> Self {
        let path_input = String::new();
        let persist_layout = !cfg!(test)
            && !std::env::args().any(|arg| {
                matches!(
                    arg.as_str(),
                    "--render-check" | "--render-playback-check" | "--render-ui-actions-check"
                )
            });
        let background_jobs = crate::background_jobs::BackgroundJobs::default();
        Self {
            prefs: if persist_layout {
                Preferences::load()
            } else {
                Preferences::default()
            },
            recording: None,
            state: SceneState::default(),
            scene_changes: crate::scene::SceneChanges::default(),
            up_resolver: crate::coordinates::UpResolver::default(),
            activity: crate::activity::ActivityCache::default(),
            bytes: None,
            source: String::new(),
            path_input,
            export_path: "captures/capture.pvdstream".into(),
            native_export_path: "captures/capture.pxd2".into(),
            filter: String::new(),
            selected: None,
            selection: selection::Selection::default(),
            frame: 0,
            selected_frame_source: None,
            displayed_frame_source: None,
            live_frame_source: None,
            live_analysis: Default::default(),
            display_trace: None,
            display_request: 0,
            playing: 0,
            playback_fps: 30.0,
            recorded_timing: false,
            last_step: Instant::now(),
            revision: 0,
            job: None,
            notices: vec![Message::new(
                "Ready. Open a capture or start a configurable TCP listener.",
                "就绪。打开录制文件，或设置地址并开始 TCP 监听。",
            )],
            initialized: false,
            focus_selected: false,
            follow_selected: false,
            last_layout_save: Instant::now(),
            last_layout: String::new(),
            persist_layout,
            server: None,
            receive_status: None,
            live_profile_enabled: false,
            last_live_profile: None,
            live_session: None,
            live_connected: false,
            follow_live: false,
            display_reason: None,
            live_state: SceneState::default(),
            live_recording: None,
            live_frame: 0,
            live_update_times: VecDeque::new(),
            seek_worker: None,
            seeking: None,
            analysis_demand: true,
            #[cfg(test)]
            force_full_playback_analysis: false,
            #[cfg(test)]
            pending_playback_rebase: None,
            #[cfg(test)]
            accepted_playback_rebase: None,
            displayed_analysis: analysis_demand::DisplayedAnalysis::Complete,
            playback_recovery: playback_recovery::Recovery::default(),
            background_jobs: background_jobs.clone(),
            shutdown_started: false,
            ui_actions: ui_actions::UiActions::default(),
            capture_path: None,
            source_identity: None,
            connection: Message::new("Offline", "离线"),
            profile_filter: String::new(),
            profile_events_view: false,
            profile_catalog_cancel: None,
            profile_layout_cancel: None,
            profile_view_epoch: 0,
            profile_cpu_index: Arc::new(crate::full_profile::IndexSlot::tracked(&background_jobs)),
            profile_window: None,
            memory_panel: crate::memory::PanelState::default(),
            error_rows: Arc::default(),
            file_picker: crate::file_picker::FilePicker::default(),
            save_job: None,
            recording_export: None,
            captured_camera: None,
            export_workers: crate::recording_export::Workers::default(),
            curves: crate::curve::Workspace::default(),
            curve_job: None,
            curve_export_path: "property.csv".into(),
            analysis_export_path: "analysis.csv".into(),
            requested_panel: None,
            retained_path: "captures/capture.pvdcapture".into(),
            retained_first: 0,
            retained_last: 0,
            capture_epoch: Arc::new(AtomicU64::new(0)),
            trim_first: 0,
            trim_last: 0,
            clip_set: crate::clips::ClipSet::default(),
            clip_persistence: crate::clips::Persistence::default(),
            pending_clip_action: None,
            exit_requested: false,
            repx_path: "frame.repx".into(),
            tree: crate::tree::TreeCache::default(),
        }
    }
}

impl Studio {
    pub fn pick_file(&mut self, kind: crate::file_picker::PickerKind, context: egui::Context) {
        use crate::file_picker::PickerKind;
        let initial = match kind {
            PickerKind::OpenCapture => &self.path_input,
            PickerKind::SaveCapture | PickerKind::SaveClip => &self.export_path,
            PickerKind::SaveNativeCapture => &self.native_export_path,
            PickerKind::SaveRepx => &self.repx_path,
            PickerKind::SaveCsv => &self.curve_export_path,
            PickerKind::SaveAnalysisCsv => &self.analysis_export_path,
            PickerKind::SaveRetainedCapture => &self.retained_path,
            PickerKind::RecordingDirectory => &self.prefs.capture_directory,
        };
        if let Err(error) = self
            .file_picker
            .request(kind, initial, self.prefs.language, context)
        {
            self.notice(error);
        }
    }
    fn set_recording(&mut self, recording: Recording) {
        self.displayed_analysis = if recording.is_disk_backed() && recording.frame_count() > 0 {
            analysis_demand::DisplayedAnalysis::Deferred
        } else {
            analysis_demand::DisplayedAnalysis::Complete
        };
        self.reset_profile_views();
        self.error_rows = Arc::new(
            recording
                .error_events()
                .map(|(frame, error)| (frame, error.clone()))
                .collect(),
        );
        self.recording = Some(Arc::new(recording));
    }
    fn reset_profile_views(&mut self) {
        self.profile_cpu_index.close();
        self.profile_cpu_index = Arc::new(crate::full_profile::IndexSlot::tracked(
            &self.background_jobs,
        ));
        self.profile_view_epoch = self.profile_view_epoch.wrapping_add(1);
        for cancel in [
            self.profile_catalog_cancel.take(),
            self.profile_layout_cancel.take(),
        ]
        .into_iter()
        .flatten()
        {
            cancel.store(true, Ordering::Relaxed);
        }
    }
    fn refresh_live_errors(&mut self) {
        let Some(recording) = self
            .recording
            .as_ref()
            .filter(|recording| recording.is_disk_backed())
        else {
            return;
        };
        if !self
            .error_rows
            .iter()
            .map(|(frame, error)| (*frame, error))
            .eq(recording.error_events())
        {
            self.error_rows = Arc::new(
                recording
                    .error_events()
                    .map(|(frame, error)| (frame, error.clone()))
                    .collect(),
            );
        }
    }
    pub fn notice(&mut self, text: Message) {
        self.notices.push(text);
        if self.notices.len() > 500 {
            self.notices.remove(0);
        }
    }
    pub fn source_text(&self, language: Language) -> &str {
        if self.live_session.is_some() {
            language.text("TCP capture", "TCP 捕获")
        } else if self.source.is_empty() {
            language.text("No capture loaded", "未加载录制")
        } else {
            &self.source
        }
    }
    pub fn count(&self) -> usize {
        let count = self.timeline_recording().map_or(0, Recording::frame_count);
        if self.live_recording.is_some() {
            count.max(self.live_frame.saturating_add(1))
        } else {
            count
        }
    }
    pub fn first_frame(&self) -> usize {
        self.timeline_recording().map_or(0, Recording::first_frame)
    }
    pub fn timeline_recording(&self) -> Option<&Recording> {
        self.live_recording.as_deref().or(self.recording.as_deref())
    }
    pub fn has_history(&self) -> bool {
        self.timeline_recording()
            .is_some_and(|recording| !recording.frames.is_empty())
    }
    pub fn history_unavailable_detail(&self, language: Language) -> String {
        if let Some(warning) = self
            .timeline_recording()
            .and_then(Recording::disk_usage)
            .and_then(|stats| stats.warning)
        {
            return format!(
                "{}: {warning}",
                language.text("Playback history is unavailable", "回放历史不可用")
            );
        }
        language.text(
            "Playback requires completed frames in the history cache. Check the history directory, disk budget and free-space reserve in Capture setup.",
            "回放需要历史缓存中有已完成的帧。请检查录制设置中的历史目录、磁盘预算和磁盘保留空间。",
        ).into()
    }
    pub fn live_updates_per_second(&self) -> usize {
        self.live_update_times
            .iter()
            .filter(|time| time.elapsed() <= Duration::from_secs(1))
            .count()
    }
    pub fn can_save_capture(&self) -> bool {
        if self.is_project_capture()
            || self.timeline_recording().is_some_and(|recording| {
                recording.source == pvd_core::RecordingSource::Pvd3Database
            })
        {
            return false;
        }
        self.bytes.is_some()
            || (!self.live_connected
                && self.capture_path.as_ref().is_some_and(|path| {
                    path.extension()
                        .is_none_or(|extension| extension != "partial")
                }))
    }
    pub fn pause_display(&mut self) {
        self.pause_display_for(DisplayReason::Pause);
    }
    pub fn toggle_playback(&mut self) {
        if self.follow_live || self.playing != 0 {
            self.pause_display();
        } else {
            self.start_playback(1);
        }
    }
    pub fn toggle_reverse(&mut self) {
        if self.playing == -1 {
            self.pause_display();
        } else {
            self.start_playback(-1);
        }
    }
    pub fn step_frame(&mut self, direction: i32) {
        let Some(target) = self.next_playback_frame(direction) else {
            return;
        };
        self.seek_paused(
            target,
            if direction < 0 {
                DisplayReason::PreviousFrame
            } else {
                DisplayReason::NextFrame
            },
        );
    }
    fn start_playback(&mut self, direction: i32) {
        let Some((first, last)) = self.playback_bounds() else {
            if direction > 0 && self.live_connected {
                self.return_to_live();
            }
            return;
        };
        if direction > 0 && self.frame >= last && self.live_connected {
            self.return_to_live();
            return;
        }
        self.leave_live(if direction < 0 {
            DisplayReason::Reverse
        } else {
            DisplayReason::Forward
        });
        self.display_reason = None;
        self.playback_recovery.clear();
        if self.seeking.is_some() {
            self.cancel_seek();
        }
        self.playing = direction;
        self.last_step = Instant::now();
        if self.frame < first
            || self.frame > last
            || (direction < 0 && self.frame == first)
            || (direction > 0 && self.frame == last)
        {
            if direction > 0 {
                self.continue_from_retained_start();
            } else {
                self.seek_with_intent(last, playback_recovery::Intent::Playback);
            }
        }
    }
    fn continue_from_retained_start(&mut self) {
        use playback_recovery::Decision;
        let range = self.retained_playback_bounds();
        let decision = match range {
            Some((first, last)) => self
                .playback_recovery
                .target(first, last, self.live_connected),
            None => self
                .playback_recovery
                .unavailable(self.live_connected, Instant::now()),
        };
        let decision = if let Decision::Seek(target) = decision {
            self.retained_playback_at_or_after(target)
                .map(Decision::Seek)
                .unwrap_or_else(|| {
                    self.playback_recovery
                        .unavailable(self.live_connected, Instant::now())
                })
        } else {
            decision
        };
        let target = match decision {
            Decision::Seek(target) => target,
            Decision::Wait => return,
            Decision::Stop => {
                self.pause_display_for(DisplayReason::HistoryExpired);
                self.notice(Message::new(
                    "Playback stopped because retained history expires faster than it can be loaded. Increase the history budget or return to Live.",
                    "历史保留窗口短于加载所需时间，播放已停止。请增加历史预算，或手动返回实时。",
                ));
                return;
            }
        };
        self.seek_with_intent(target, playback_recovery::Intent::Recovery);
        self.last_step = Instant::now();
        self.notice(Message::new(
            format!("Earlier history expired. Continuing playback at frame {target}."),
            format!("较早历史已淘汰，继续从第 {target} 帧回放。"),
        ));
    }
    pub fn return_to_live(&mut self) {
        let Some(recording) = self.live_recording.clone() else {
            return;
        };
        self.cancel_seek();
        self.reset_profile_views();
        self.playing = 0;
        self.follow_live = self.live_connected;
        self.display_reason = None;
        self.refresh_selection();
        self.state = self.live_state.clone();
        self.refresh_selection();
        self.activity.rebuild(&self.state);
        self.frame = self.live_frame;
        self.displayed_frame_source = self.live_frame_source;
        if !self.live_connected {
            self.selected_frame_source = self.live_frame_source.map(|(source, _)| source);
        }
        let mut displayed = (*recording).clone();
        displayed.analysis = self.live_analysis.clone();
        self.recording = Some(Arc::new(displayed));
        self.displayed_analysis = analysis_demand::DisplayedAnalysis::Complete;
        self.refresh_live_errors();
        self.revision += 1;
    }
    fn cancel_seek(&mut self) {
        #[cfg(test)]
        {
            self.pending_playback_rebase = None;
            self.accepted_playback_rebase = None;
        }
        if let Some(worker) = &self.seek_worker {
            worker.cancel();
        }
        self.seeking = None;
        self.playback_recovery.clear();
    }
    pub fn seek(&mut self, target: usize) {
        self.playback_recovery.make_manual();
        self.seek_with_intent(target, playback_recovery::Intent::Manual);
    }
    fn seek_with_intent(&mut self, target: usize, intent: playback_recovery::Intent) {
        let with_analysis = self.analysis_demand && self.playing == 0;
        #[cfg(test)]
        let with_analysis = with_analysis || self.force_full_playback_analysis;
        self.seek_with_analysis(
            target,
            intent,
            with_analysis,
            (intent == playback_recovery::Intent::Manual).then_some(DisplayReason::Frame),
        );
    }

    fn seek_with_analysis(
        &mut self,
        target: usize,
        intent: playback_recovery::Intent,
        with_analysis: bool,
        reason: Option<DisplayReason>,
    ) -> bool {
        if self.shutdown_started || self.seeking == Some(target) {
            return false;
        }
        let Some(recording) = self
            .live_recording
            .as_ref()
            .or(self.recording.as_ref())
            .cloned()
        else {
            return false;
        };
        if recording.frame_count() == 0 {
            if self.follow_live {
                return false;
            }
            self.refresh_selection();
            self.state = recording.initial_state().clone();
            self.refresh_selection();
            self.activity.rebuild(&self.state);
            self.frame = 0;
            self.revision += 1;
            return true;
        }
        if target < recording.first_frame()
            || target >= recording.frame_count()
            || (self.follow_live && target == self.frame)
        {
            return false;
        }
        if recording.frame_info(target).is_none()
            || (self.follow_live && !recording.is_frame_retained(target))
        {
            self.notice(Message::new(format!("Frame {target} has no retained disk history. Use Live to view the current scene."), format!("第 {target} 帧没有可读取的磁盘历史。可点击返回实时查看当前场景。")));
            return false;
        }
        if recording.is_disk_backed() {
            self.playback_recovery.begin(
                intent,
                target,
                (
                    recording.first_frame(),
                    recording.frame_count().saturating_sub(1),
                ),
                intent == playback_recovery::Intent::Recovery
                    || recording.is_frame_retained(target),
                Instant::now(),
            );
            if self.seek_worker.is_none() {
                match seeking::Worker::tracked(&self.background_jobs) {
                    Ok(worker) => self.seek_worker = Some(worker),
                    Err(error) => {
                        self.stop_seek_failure(target, error.to_string());
                        return false;
                    }
                }
            }
            let worker = self.seek_worker.as_ref().unwrap();
            #[cfg(test)]
            let rebase_witness = (intent == playback_recovery::Intent::Recovery)
                .then(|| playback_tests::RebaseWitness::observe(&recording, self.frame, target))
                .flatten();
            if let Some(trace) = &self.display_trace {
                worker.request_profiled(recording.clone(), target, with_analysis, trace.clone());
            } else if with_analysis {
                worker.request(recording.clone(), target);
            } else {
                worker.request_scene(recording.clone(), target);
            }
            #[cfg(test)]
            {
                self.pending_playback_rebase =
                    rebase_witness.map(|witness| witness.dispatched(worker.generation()));
            }
            if let Some(reason) = reason.or(self.follow_live.then_some(DisplayReason::Frame)) {
                self.leave_live(reason);
            }
            self.select_source_of_frame(target);
            self.seeking = Some(target);
            return true;
        }
        match recording.state_at(target) {
            Ok(state) => {
                if let Some(reason) = reason.or(self.follow_live.then_some(DisplayReason::Frame)) {
                    self.leave_live(reason);
                }
                self.refresh_selection();
                self.state = state;
                self.refresh_selection();
                self.activity.rebuild(&self.state);
                self.select_source_of_frame(target);
                self.remember_display_frame(target);
                self.revision += 1;
                true
            }
            Err(error) => {
                self.stop_seek_failure(target, error.to_string());
                false
            }
        }
    }
    pub fn close(&mut self) {
        if self.allow_clip_transition(crate::clips::PendingAction::Close) {
            self.clear_recording();
        }
    }
    pub fn allow_clip_transition(&mut self, action: crate::clips::PendingAction) -> bool {
        if self.shutdown_started {
            return false;
        }
        if self.pending_clip_action.is_some() {
            return false;
        }
        if self.clip_persistence.needs_confirmation(&self.clip_set) {
            self.pending_clip_action = Some(action);
            self.playing = 0;
            return false;
        }
        true
    }
    fn clear_recording(&mut self) {
        self.capture_epoch.fetch_add(1, Ordering::Relaxed);
        self.cancel_seek();
        self.recording_export = None;
        self.captured_camera = None;
        self.source_identity = None;
        self.live_connected = false;
        self.follow_live = false;
        self.display_reason = None;
        self.live_recording = None;
        self.live_state = SceneState::default();
        self.live_frame = 0;
        self.live_update_times.clear();
        self.clip_set = crate::clips::ClipSet::default();
        self.clip_persistence = crate::clips::Persistence::default();
        self.profile_window = None;
        self.memory_panel = crate::memory::PanelState::default();
        self.error_rows = Arc::default();
        self.curve_job = None;
        self.curves.clear();
        self.requested_panel = None;
        self.profile_events_view = false;
        self.reset_profile_views();
        if let Some(job) = self.job.take() {
            job.cancelled.store(true, Ordering::Relaxed);
        }
        self.recording = None;
        self.selected_frame_source = None;
        self.displayed_frame_source = None;
        self.live_frame_source = None;
        self.live_analysis = Default::default();
        self.displayed_analysis = analysis_demand::DisplayedAnalysis::Complete;
        self.state = SceneState::default();
        self.activity.clear();
        self.bytes = None;
        self.source.clear();
        self.clear_selection();
        self.live_session = None;
        self.capture_path = None;
        self.playing = 0;
        self.frame = 0;
        self.revision += 1;
    }
    pub fn poll_shutdown(&mut self) -> bool {
        if !self.shutdown_started {
            self.shutdown_started = true;
            self.ui_actions = ui_actions::UiActions::default();
            self.background_jobs.cancel_all_and_check_finished();
            self.server = None;
            self.seek_worker = None;
            self.clear_recording();
        }
        let background_finished = self.background_jobs.cancel_all_and_check_finished();
        let exports_finished = self.export_workers.cancel_all_and_check_finished();
        background_finished && exports_finished
    }
    pub fn open(&mut self, path: PathBuf, context: egui::Context) {
        if !self.allow_clip_transition(crate::clips::PendingAction::Open(path.clone())) {
            return;
        }
        if let Some(profile) = self
            .server
            .as_ref()
            .and_then(NetworkSession::profile_snapshot)
        {
            self.last_live_profile = Some(profile);
        }
        self.server = None;
        self.receive_status = None;
        self.cancel_seek();
        self.follow_live = false;
        self.live_connected = false;
        self.connection = Message::new("Offline", "离线");
        if let Some(job) = self.job.take() {
            job.cancelled.store(true, Ordering::Relaxed);
        }
        let (tx, rx) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicU64::new(0));
        let job_cancelled = cancelled.clone();
        let job_progress = progress.clone();
        let label = path.display().to_string();
        let history = self.history_config();
        let spawn = self
            .background_jobs
            .spawn("pvd-file-loader", cancelled.clone(), move || {
                let result = read_recording(&path, &job_cancelled, &job_progress, history);
                if !job_cancelled.load(Ordering::Relaxed) {
                    let _ = tx.send(result);
                }
                context.request_repaint();
            });
        if let Err(error) = spawn {
            self.notice(Message::detail(
                "Could not start file loading",
                "无法启动文件加载",
                error,
            ));
            return;
        }
        self.playing = 0;
        self.job = Some(LoadJob {
            result: Mutex::new(rx),
            cancelled,
            progress,
            label,
            started: Instant::now(),
            live: false,
            final_snapshot: true,
        });
    }
    pub fn poll(&mut self, context: &egui::Context) {
        let wake = context.clone();
        self.background_jobs
            .set_wake(move || wake.request_repaint());
        if self.shutdown_started {
            context.request_repaint_after(Duration::from_millis(16));
            return;
        }
        if let Some(result) = self.file_picker.poll() {
            use crate::file_picker::PickerKind;
            match result.outcome {
                Ok(Some(path)) => {
                    if let Some(text) = path.to_str().map(str::to_owned) {
                        match result.kind {
                            PickerKind::OpenCapture => {
                                self.path_input = text;
                                self.open(path, context.clone());
                            }
                            PickerKind::SaveCapture | PickerKind::SaveClip => {
                                self.export_path = text
                            }
                            PickerKind::SaveNativeCapture => self.native_export_path = text,
                            PickerKind::SaveRepx => self.repx_path = text,
                            PickerKind::SaveCsv => self.curve_export_path = text,
                            PickerKind::SaveAnalysisCsv => self.analysis_export_path = text,
                            PickerKind::SaveRetainedCapture => self.retained_path = text,
                            PickerKind::RecordingDirectory => self.prefs.capture_directory = text,
                        }
                    } else {
                        self.notice(Message::new("This path contains invalid Unicode and cannot be represented by the path editor.", "路径包含无效 Unicode，无法由路径编辑框无损表示。"));
                    }
                }
                Ok(None) => (),
                Err(error) => self.notice(error),
            }
        }
        self.poll_network(context);
        self.poll_seek(context);
        if let Some(message) = self.clip_persistence.poll(&mut self.clip_set) {
            self.notice(message);
        }
        if self.clip_persistence.busy() {
            context.request_repaint_after(Duration::from_millis(100));
        }
        let saved = self
            .save_job
            .as_ref()
            .and_then(|job| job.lock().ok()?.try_recv().ok());
        if let Some(result) = saved {
            self.save_job = None;
            self.notice(result);
        }
        if let Some(message) = self.recording_export.as_ref().and_then(|job| job.result()) {
            self.recording_export = None;
            self.notice(message);
        }
        if self.recording_export.is_some() {
            context.request_repaint_after(Duration::from_millis(100));
        }
        let curve = self.curve_job.as_ref().and_then(|job| job.result());
        if let Some(result) = curve {
            let id = self.curve_job.take().unwrap().id;
            let entry = self.curves.curves.iter_mut().find(|curve| curve.id == id);
            match result {
                Ok(data) => {
                    let count = data.samples.len();
                    if let Some(entry) = entry {
                        entry.data = Some(data);
                        entry.pending = false;
                        entry.error = None;
                    }
                    self.notice(Message::new(
                        format!("Curve ready · {count} samples"),
                        format!("曲线已就绪 · {count} 个采样点"),
                    ));
                }
                Err(error) => {
                    if let Some(entry) = entry {
                        entry.pending = false;
                        entry.error = Some(error.clone());
                    }
                    self.notice(error);
                }
            }
        }
        self.start_curve_job(context.clone());
        if self.curve_job.is_some() {
            context.request_repaint_after(Duration::from_millis(100));
        }
        let loaded = self
            .job
            .as_ref()
            .and_then(|job| job.result.lock().ok()?.try_recv().ok());
        if let Some(result) = loaded {
            let job = self.job.take().unwrap();
            match result {
                Ok(LoadedCapture {
                    recording,
                    bytes,
                    source_path,
                    source_identity,
                    clips,
                }) => {
                    let native_container =
                        recording.source == pvd_core::RecordingSource::Pvd3Database;
                    self.profile_window = None;
                    if !job.live {
                        self.clear_recording();
                    }
                    self.source = job.label;
                    self.bytes = bytes;
                    self.capture_path = source_path;
                    self.source_identity = source_identity;
                    self.set_recording(recording);
                    if let Some(clips) = clips
                        && let Some(message) =
                            self.clip_persistence.accept_load(clips, &mut self.clip_set)
                    {
                        self.notice(message);
                    }
                    if !job.live
                        && !native_container
                        && self.capture_path.is_none()
                        && self.bytes.is_some()
                    {
                        self.clip_persistence.issue = Some(Message::new(
                            "Save the SDK stream to a file to keep clip definitions. Range export is available in the toolbar.",
                            "先另存 SDK 数据流后可保存片段定义；当前可在工具栏导出帧区间。",
                        ));
                    }
                    self.trim_first = self.first_frame();
                    self.trim_last = self.count().saturating_sub(1);
                    self.use_retained_range();
                    if let Some((first, last)) = self.playback_bounds() {
                        self.seek(if job.live { last } else { first });
                    }
                    if !job.live {
                        self.focus_selected = true;
                        let count = self.count().saturating_sub(self.first_frame());
                        let seconds = job.started.elapsed().as_secs_f64();
                        self.notice(Message::new(
                            format!("Loaded {count} frames in {seconds:.3} s"),
                            format!("已加载 {count} 帧，用时 {seconds:.3} 秒"),
                        ));
                        if self.is_project_capture() {
                            self.clip_persistence.issue = Some(Message::new(
                                "Project capture: original frame numbers retained; full raw export, full-file analysis and clip definitions are unavailable.",
                                "本项目捕获保留原帧号；不支持完整原始流导出、全文件分析和片段定义。",
                            ));
                            self.notice(Message::new(
                                format!(
                                    "Project capture · visible frames {}..={}",
                                    self.first_frame(),
                                    self.count().saturating_sub(1)
                                ),
                                format!(
                                    "本项目捕获 · 可见帧 {}..={}",
                                    self.first_frame(),
                                    self.count().saturating_sub(1)
                                ),
                            ));
                        }
                        if native_container {
                            self.clip_persistence.issue = Some(Message::new(
                                "Clip definitions and SDK stream export are not supported for native PVD3 containers.",
                                "原版 PVD3 容器暂不支持编辑片段定义或导出 SDK 流片段。",
                            ));
                            self.notice(Message::new(
                                "PVD3 database opened with limited support. Object IDs are container-local; see Diagnostics for unsupported data and export limits.",
                                "已打开原版 PVD3 数据库，当前为部分支持。对象 ID 属于容器内部编号；未导入数据和导出限制请查看诊断。",
                            ));
                            self.requested_panel = Some(Panel::Diagnostics);
                        }
                    }
                }
                Err(error) if !job.live || job.final_snapshot => {
                    self.notice(error.context("Decode failed", "解码失败"))
                }
                Err(_) => (),
            }
        }
        if self.job.is_some() {
            context.request_repaint_after(Duration::from_millis(100));
        }
        if self.playing > 0 && self.seeking.is_none() && self.playback_recovery.waiting() {
            self.continue_from_retained_start();
        }
        if self.playing != 0
            && self.count() > self.first_frame()
            && self.seeking.is_none()
            && !self.playback_recovery.waiting()
        {
            let now = Instant::now();
            for _ in 0..16 {
                let next = self.next_playback_frame(self.playing);
                if next.is_none() {
                    if self.playing > 0 && self.frame < self.first_frame() {
                        self.continue_from_retained_start();
                    } else if self.playing > 0 && self.live_connected {
                        self.return_to_live();
                    } else {
                        self.pause_display_for(if self.playing < 0 {
                            DisplayReason::FirstFrame
                        } else {
                            DisplayReason::LastFrame
                        });
                    }
                    break;
                }
                let next = next.unwrap();
                let interval = playback_interval(
                    self.recording.as_deref(),
                    self.frame,
                    next,
                    self.recorded_timing,
                    self.playback_fps,
                );
                if now.duration_since(self.last_step) < interval {
                    break;
                }
                self.seek_with_intent(next, playback_recovery::Intent::Playback);
                self.last_step += interval;
                if self.playing == 0 || self.seeking.is_some() {
                    break;
                }
            }
            if self.playing != 0 {
                let next = self.next_playback_frame(self.playing).unwrap_or(self.frame);
                let interval = playback_interval(
                    self.recording.as_deref(),
                    self.frame,
                    next,
                    self.recorded_timing,
                    self.playback_fps,
                );
                if self.last_step.elapsed() > interval.saturating_mul(16) {
                    self.last_step = now;
                }
                context.request_repaint_after(
                    interval
                        .saturating_sub(self.last_step.elapsed())
                        .min(Duration::from_millis(100)),
                );
            }
        }
        if self.persist_layout && self.last_layout_save.elapsed() >= Duration::from_secs(2) {
            if let Ok(serialized) = serde_json::to_string(&self.prefs)
                && serialized != self.last_layout
            {
                if let Err(error) = self.prefs.save() {
                    self.notice(Message::detail(
                        "Preferences save failed",
                        "偏好设置保存失败",
                        error,
                    ));
                } else {
                    self.last_layout = serialized;
                }
            }
            self.last_layout_save = Instant::now();
        }
    }
    pub fn save_capture(&mut self, context: egui::Context) {
        if self.exit_requested || self.save_job.is_some() {
            return;
        }
        if !self.can_save_capture() {
            self.notice(Message::new("A completed raw recording is required for Save capture. Interrupted .partial files remain at their reported paths for diagnosis.", "保存录制需要完整的原始录制文件。中断的 .partial 文件保留在所示路径，供诊断使用。"));
            return;
        }
        let bytes = self.bytes.clone();
        let capture = (!self.live_connected)
            .then(|| self.capture_path.clone())
            .flatten();
        if bytes.is_none() && capture.is_none() {
            return;
        }
        if bytes
            .as_ref()
            .is_some_and(|bytes| bytes.starts_with(&pvd_core::native::MAGIC.to_le_bytes()))
        {
            self.notice(Message::new(
                "Saving a native PVD3 container is not supported by the SDK stream export. The .pxd2 container must not be written as .pvdstream; the original file is preserved.",
                "SDK 数据流保存入口暂不支持另存原版 PVD3 容器，不能将 .pxd2 内容伪装为 .pvdstream；源文件已保留。",
            ));
            return;
        }
        let path = PathBuf::from(&self.export_path);
        if !path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("pvdstream"))
        {
            self.notice(Message::new("Use .pvdstream for SDK stream bytes. Native .pxd/.pxd2 containers and OmniPVD .ovd are different formats.", "SDK 原始数据流请保存为 .pvdstream。原版 .pxd/.pxd2 容器和 OmniPVD .ovd 属于不同格式。"));
            return;
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let permit = match self.export_workers.acquire(cancelled.clone()) {
            Ok(permit) => permit,
            Err(message) => {
                self.notice(message);
                return;
            }
        };
        let (sender, receiver) = mpsc::channel();
        self.save_job = Some(Mutex::new(receiver));
        self.notice(Message::new(
            format!("Saving capture to {}", path.display()),
            format!("正在保存录制到 {}", path.display()),
        ));
        let spawn = std::thread::Builder::new()
            .name("save-capture".into())
            .spawn(move || {
                let _permit = permit;
                let continue_copy = || !cancelled.load(Ordering::Relaxed);
                let saved = crate::atomic_output::create_parent_directory(&path).and_then(|()| {
                    if let Some(bytes) = bytes {
                        crate::atomic_output::copy_reader_new(
                            bytes.as_slice(),
                            &path,
                            None,
                            continue_copy,
                        )
                    } else {
                        File::open(capture.as_deref().unwrap()).and_then(|input| {
                            crate::atomic_output::copy_reader_new(input, &path, None, continue_copy)
                        })
                    }
                });
                let result = match saved {
                    Ok(length) => Message::new(
                        format!("Saved {length} bytes to {}", path.display()),
                        format!("已保存 {length} 字节到 {}", path.display()),
                    ),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                        Message::new("Capture save cancelled", "保存录制已取消")
                    }
                    Err(error) => Message::new(
                        format!("Save failed: {error} (existing files are protected)"),
                        format!("保存失败：{error}（不会覆盖已有文件）"),
                    ),
                };
                let _ = sender.send(result);
                context.request_repaint();
            });
        if let Err(error) = spawn {
            self.save_job = None;
            self.notice(Message::detail(
                "Cannot start saving capture",
                "无法开始保存录制",
                error,
            ));
        }
    }
    pub fn start_listener(&mut self, context: egui::Context) {
        if self.shutdown_started {
            return;
        }
        if !self.allow_clip_transition(crate::clips::PendingAction::Listen) {
            return;
        }
        if self.file_picker.is_busy() {
            self.notice(Message::new(
                "Finish or cancel the file dialog before starting the listener.",
                "请先完成或取消文件对话框，再开始监听。",
            ));
            return;
        }
        if self.prefs.auto_capture && self.prefs.capture_directory.trim().is_empty() {
            self.notice(Message::new(
                "Choose a directory before enabling automatic capture.",
                "请先设置保存目录，再启用自动录制。",
            ));
            return;
        }
        if self.prefs.history_directory.trim().is_empty() {
            self.notice(Message::new(
                "Choose a directory for the live history cache before listening.",
                "请先设置实时历史缓存目录，再开始监听。",
            ));
            return;
        }
        let address = match self.prefs.listener.address() {
            Ok(address) => address,
            Err(error) => {
                self.notice(error);
                return;
            }
        };
        let config = ListenConfig {
            address,
            capture_directory: self
                .prefs
                .auto_capture
                .then(|| PathBuf::from(&self.prefs.capture_directory)),
            ..Default::default()
        };
        let options = NetworkOptions {
            history: self.history_config(),
            receive_budget_bytes: u64::from(self.prefs.receive_budget_gib) * 1024 * 1024 * 1024,
            refresh_hz: self.prefs.live_refresh_hz.clamp(15, 120),
            raw_limit_bytes: u64::from(self.prefs.raw_capture_limit_gib) * 1024 * 1024 * 1024,
            live_profile: self.live_profile_enabled,
            #[cfg(test)]
            progress_only: false,
        };
        match NetworkSession::start_tracked(config, options, context, &self.background_jobs) {
            Ok(server) => {
                let address = server.address();
                self.receive_status = None;
                self.last_live_profile = None;
                self.server = Some(server);
                self.connection = Message::new(
                    format!("Listening · {address}"),
                    format!("监听中 · {address}"),
                );
            }
            Err(error) => self.notice(Message::detail("Listener failed", "监听失败", error)),
        }
    }
    fn history_config(&self) -> pvd_core::live::HistoryConfig {
        pvd_core::live::HistoryConfig {
            directory: PathBuf::from(&self.prefs.history_directory),
            disk_budget_bytes: u64::from(self.prefs.history_budget_gib.max(1)) * 1024 * 1024 * 1024,
            min_free_disk_bytes: u64::from(self.prefs.min_free_disk_mib.max(512)) * 1024 * 1024,
            ..Default::default()
        }
    }
    pub fn plot(&mut self, property: String, component: usize, context: egui::Context) {
        self.plot_property(property, None, component, context);
    }
    pub fn plot_element(
        &mut self,
        property: String,
        index: usize,
        component: usize,
        context: egui::Context,
    ) {
        self.plot_property(property, Some(index), component, context);
    }
    fn plot_property(
        &mut self,
        property: String,
        index: Option<usize>,
        component: usize,
        context: egui::Context,
    ) {
        if self.recording.is_none() {
            return;
        }
        let Some(object) = self.selected.and_then(|id| self.state.objects.get(&id)) else {
            return;
        };
        let Some(value) = object.properties.get(&property) else {
            return;
        };
        let (source, value) = if let Some(index) = index {
            let pvd_core::Value::Array(values) = value else {
                return;
            };
            let Some(value) = values.get(index) else {
                return;
            };
            let Some(value_type) = crate::curve::ElementType::of(value) else {
                return;
            };
            (
                crate::curve::Source::PropertyElement { index, value_type },
                value,
            )
        } else {
            (crate::curve::Source::Property, value)
        };
        let Some(component_name) = crate::curve::components(value).get(component).copied() else {
            return;
        };
        let spec = crate::curve::Spec {
            source,
            object: object.id,
            generation: object.generation,
            property,
            component,
            component_name: component_name.into(),
        };
        match self.curves.add(spec) {
            Ok(id) => {
                self.select_curve(id);
                self.start_curve_job(context);
            }
            Err(error) => self.notice(error),
        }
    }
    pub fn plot_performance(&mut self, context: egui::Context) {
        use pvd_core::frame_metrics::Metric;
        let Some(recording) = self.recording.as_ref() else {
            return;
        };
        let Some(frame) = recording.frame_info(self.frame) else {
            return;
        };
        let Some(scene) = self.state.objects.get(&frame.scene_id) else {
            return;
        };
        let fixed_stages =
            pvd_core::frame_metrics::supports_stages(recording, scene.id, scene.generation);
        let specs: Vec<_> = [
            Metric::FrameInterval,
            Metric::Simulate,
            Metric::Collision,
            Metric::Solver,
        ]
        .into_iter()
        .filter(|metric| fixed_stages || *metric == Metric::FrameInterval)
        .map(|metric| crate::curve::Spec {
            source: crate::curve::Source::Performance(metric),
            object: scene.id,
            generation: scene.generation,
            property: metric.name().into(),
            component: 0,
            component_name: "ms".into(),
        })
        .collect();
        let additional = specs
            .iter()
            .filter(|spec| !self.curves.curves.iter().any(|curve| &curve.spec == *spec))
            .count();
        if self.curves.curves.len() + additional > crate::curve::MAX_CURVES
            || (!self.curves.pages[self.curves.active_page].curves.is_empty()
                && !self.curves.new_page())
        {
            self.notice(Message::new("Remove curves or a graph before adding the frame graph (16 curves / 8 graphs maximum).", "请先移除曲线或图页，再添加帧性能图（最多 16 条曲线 / 8 个图页）。"));
            return;
        }
        for spec in specs {
            match self.curves.add(spec) {
                Ok(id) => self.select_curve(id),
                Err(error) => {
                    self.notice(error);
                    return;
                }
            }
        }
        self.start_curve_job(context);
        self.notice(if !fixed_stages {
            Message::new(
                "Frame interval curve added. This Scene has no unique recorded source context; use exact event curves with an explicit reference Scene.",
                "已添加帧间隔曲线。此 Scene 没有唯一的录制源 Context；可选择明确的性能事件和参考 Scene 绘制曲线。",
            )
        } else {
            Message::new(
                "Frame and phase curves added. Open Curves to inspect elapsed times in milliseconds.",
                "已添加帧与阶段曲线。请打开“曲线”查看毫秒经过时间。",
            )
        });
    }
    pub fn plot_profile_event(
        &mut self,
        key: pvd_core::profile_curves::ExactKey,
        context: egui::Context,
    ) {
        let Some(recording) = &self.recording else {
            return;
        };
        let Some(frame) = recording.frame_info(self.frame) else {
            return;
        };
        let Some(scene) = self
            .state
            .objects
            .get(&frame.scene_id)
            .filter(|scene| scene.type_name == "physx3::PxScene")
        else {
            self.notice(Message::new(
                "The current frame has no valid reference Scene for event curves.",
                "当前帧没有可用于事件曲线的有效参考 Scene。",
            ));
            return;
        };
        let spec = crate::curve::Spec {
            source: crate::curve::Source::ProfileEvent(key),
            object: scene.id,
            generation: scene.generation,
            property: "profile_event_inclusive_sum".into(),
            component: 0,
            component_name: "ms".into(),
        };
        let analysis = recording
            .native_end_analysis()
            .unwrap_or(&recording.analysis);
        let label = if analysis.profile_metadata_ambiguous(key.zone, key.event) {
            spec.label()
                .context("Metadata identity is ambiguous", "元数据身份有歧义")
        } else if let Some(zone) = analysis.profile_zone_name(key.zone)
            && let Some(event) = analysis.profile_event_name(key.zone, key.event)
        {
            let name = format!("{zone} / {event}");
            spec.label().context(&name, &name)
        } else {
            spec.label()
        };
        match self.curves.add(spec) {
            Ok(id) => {
                if let Some(curve) = self.curves.curves.iter_mut().find(|curve| curve.id == id) {
                    curve.label = label;
                }
                self.select_curve(id);
                self.requested_panel = Some(Panel::Curves);
                self.start_curve_job(context);
                self.notice(Message::new("Event curve added: aligned to the reference Scene's time windows; inclusive sums count overlaps and do not measure CPU utilization.", "已添加事件曲线：按参考 Scene 时间窗口对齐；包含耗时总和会重复累加重叠区间，不代表 CPU 占用率。"));
            }
            Err(error) => self.notice(error),
        }
    }
    fn start_curve_job(&mut self, context: egui::Context) {
        if self.shutdown_started
            || self.curve_job.is_some()
            || self.job.as_ref().is_some_and(|job| !job.live)
        {
            return;
        }
        let Some(recording) = self.recording.clone() else {
            return;
        };
        let source = self.complete_profile_source();
        for _ in 0..self.curves.pending.len() {
            let Some(id) = self.curves.pending.pop_front() else {
                break;
            };
            if let Some(curve) = self.curves.curves.iter().find(|curve| curve.id == id) {
                if curve.spec.needs_analysis()
                    && source.is_none()
                    && recording.native_end_analysis().is_none()
                    && !self.analysis_ready()
                {
                    self.request_current_analysis();
                    if let analysis_demand::DisplayedAnalysis::Failed { message, .. } =
                        &self.displayed_analysis
                    {
                        if let Some(curve) =
                            self.curves.curves.iter_mut().find(|curve| curve.id == id)
                        {
                            curve.pending = false;
                            curve.error = Some(message.clone());
                        }
                    } else {
                        self.curves.pending.push_back(id);
                    }
                    continue;
                }
                match crate::curve::Job::start(
                    id,
                    curve.spec.clone(),
                    recording,
                    source,
                    context,
                    &self.background_jobs,
                ) {
                    Ok(job) => self.curve_job = Some(job),
                    Err(error) => {
                        if let Some(curve) =
                            self.curves.curves.iter_mut().find(|curve| curve.id == id)
                        {
                            curve.pending = false;
                            curve.error = Some(error.clone());
                        }
                        self.notice(error);
                    }
                }
                break;
            }
        }
    }
    pub fn select_curve(&mut self, id: u64) {
        if let Some(previous) = self.curves.selected
            && let Some(curve) = self
                .curves
                .curves
                .iter_mut()
                .find(|curve| curve.id == previous)
        {
            curve.export_path = self.curve_export_path.clone();
        }
        if let Some(curve) = self.curves.curves.iter().find(|curve| curve.id == id) {
            self.curve_export_path = curve.export_path.clone();
            self.curves.selected = Some(id);
        }
    }
    pub fn refresh_curve(&mut self, id: u64, context: egui::Context) {
        if self.curve_job.as_ref().is_some_and(|job| job.id == id)
            || self.curves.pending.contains(&id)
        {
            return;
        }
        if let Some(curve) = self.curves.curves.iter_mut().find(|curve| curve.id == id) {
            curve.pending = true;
            curve.error = None;
            self.curves.pending.push_back(id);
            self.start_curve_job(context);
        }
    }
    pub fn cancel_curves(&mut self) {
        self.curve_job = None;
        self.curves.cancel_pending();
    }
    pub fn remove_curve(&mut self, id: u64) {
        if self.curve_job.as_ref().is_some_and(|job| job.id == id) {
            self.curve_job = None;
        }
        self.curves.remove_curve(id);
    }
    pub fn can_trim_capture(&self) -> bool {
        self.count() > 0 && self.can_export_recording()
    }

    pub fn can_export_recording(&self) -> bool {
        !self.is_project_capture()
            && !self.exit_requested
            && self.job.is_none()
            && self.server.is_none()
            && self.save_job.is_none()
            && self.recording_export.is_none()
            && self
                .recording
                .as_ref()
                .is_some_and(|recording| recording.source == pvd_core::RecordingSource::SdkStream)
            && (self
                .bytes
                .as_ref()
                .is_some_and(|bytes| !bytes.starts_with(&pvd_core::native::MAGIC.to_le_bytes()))
                || (self.capture_path.is_some() && self.source_identity.is_some()))
    }

    pub fn can_export_analysis(&self) -> bool {
        self.can_export_recording()
            || (!self.exit_requested
                && self.job.is_none()
                && self.server.is_none()
                && self.save_job.is_none()
                && self.recording_export.is_none()
                && self.recording.as_ref().is_some_and(|recording| {
                    recording.source == pvd_core::RecordingSource::Pvd3Database
                }))
    }

    pub(crate) fn complete_profile_source(&self) -> Option<crate::full_profile::Source> {
        if self.exit_requested
            || self.job.is_some()
            || self.server.is_some()
            || self.recording.as_ref()?.source != pvd_core::RecordingSource::SdkStream
        {
            return None;
        }
        let identity = self.source_identity.clone()?;
        let source = if let Some(bytes) = &self.bytes {
            crate::full_profile::Source::memory(bytes.clone(), identity)
        } else {
            crate::full_profile::Source::file(self.capture_path.clone()?, identity)
        };
        Some(
            if cfg!(windows)
                && let Some(path) = &self.capture_path
            {
                source.with_cpu_index(path.clone(), self.profile_cpu_index.clone())
            } else {
                source
            },
        )
    }

    pub(crate) fn complete_timeline_source(&self) -> Option<crate::full_profile::timeline::Source> {
        if self.exit_requested || self.job.is_some() || self.server.is_some() {
            return None;
        }
        if self.recording.as_ref()?.source == pvd_core::RecordingSource::Pvd3Database {
            crate::full_profile::timeline::Source::native(self.recording.as_ref()?.clone())
        } else {
            self.complete_profile_source().map(Into::into)
        }
    }

    fn export_source(&self) -> crate::recording_export::Source {
        if let Some(bytes) = self.bytes.clone() {
            crate::recording_export::Source::Memory(bytes)
        } else {
            crate::recording_export::Source::File(
                self.capture_path.clone().unwrap(),
                self.source_identity.clone().unwrap(),
            )
        }
    }

    pub fn export_analysis(
        &mut self,
        kind: pvd_core::analysis_export::AnalysisCsvKind,
        context: egui::Context,
    ) {
        if !self.can_export_analysis() {
            return;
        }
        if !Path::new(&self.analysis_export_path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("csv"))
        {
            self.notice(Message::new(
                "Use .csv for analysis export.",
                "分析导出请使用 .csv 扩展名。",
            ));
            return;
        }
        let source = if let Some(recording) = self
            .recording
            .as_ref()
            .filter(|recording| recording.source == pvd_core::RecordingSource::Pvd3Database)
        {
            crate::recording_export::Source::Loaded(recording.clone())
        } else {
            self.export_source()
        };
        match crate::recording_export::Job::start(
            source,
            self.analysis_export_path.clone().into(),
            crate::recording_export::Kind::Analysis(kind),
            context,
            &self.export_workers,
        ) {
            Ok(job) => {
                self.recording_export = Some(job);
                self.notice(Message::new(
                    "Exporting analysis from the complete recording",
                    "正在导出整份录制的分析数据",
                ));
            }
            Err(error) => self.notice(error),
        }
    }

    pub fn trim_capture(&mut self, context: egui::Context) {
        if !self.can_trim_capture()
            || self.trim_first > self.trim_last
            || self.trim_last >= self.count()
        {
            return;
        }
        if !Path::new(&self.export_path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pvdstream"))
        {
            self.notice(Message::new(
                "Use .pvdstream for a scene-frame clip.",
                "场景帧片段请保存为 .pvdstream。",
            ));
            return;
        }
        let source = self.export_source();
        let (first, last) = (self.trim_first, self.trim_last);
        match crate::recording_export::Job::start(
            source,
            self.export_path.clone().into(),
            crate::recording_export::Kind::Clip { first, last },
            context,
            &self.export_workers,
        ) {
            Ok(job) => {
                self.recording_export = Some(job);
                self.notice(Message::new(
                    format!("Trimming scene frames {first}..={last}"),
                    format!("正在裁剪场景帧 {first}..={last}"),
                ));
            }
            Err(message) => self.notice(message),
        }
    }
    pub fn export_curve(&mut self, context: egui::Context) {
        if self.exit_requested || self.save_job.is_some() {
            return;
        }
        let Some(curve) = self
            .curves
            .curves
            .iter_mut()
            .find(|curve| Some(curve.id) == self.curves.selected)
        else {
            return;
        };
        let Some(csv) = crate::curve::csv(curve) else {
            return;
        };
        curve.export_path = self.curve_export_path.clone();
        let path = self.curve_export_path.clone();
        let (sender, receiver) = mpsc::channel();
        self.save_job = Some(Mutex::new(receiver));
        let cancelled = Arc::new(AtomicBool::new(false));
        let result = self
            .background_jobs
            .spawn("curve-export", cancelled.clone(), move || {
                let result = crate::atomic_output::copy_reader_new(
                    csv.as_bytes(),
                    Path::new(&path),
                    None,
                    || !cancelled.load(Ordering::Acquire),
                );
                let _ = sender.send(match result {
                    Ok(_) => Message::new(
                        format!("Exported sampled curve CSV to {path}"),
                        format!("已将采样曲线导出为 CSV：{path}"),
                    ),
                    Err(error) => Message::detail("CSV export failed", "CSV 导出失败", error),
                });
                context.request_repaint();
            });
        if let Err(error) = result {
            self.save_job = None;
            self.notice(Message::detail("CSV export failed", "CSV 导出失败", error));
        }
    }
    pub fn export_repx(&mut self, selection: pvd_core::repx::Selection, context: egui::Context) {
        if self.exit_requested || self.save_job.is_some() {
            return;
        }
        let state = self.state.clone();
        let path = self.repx_path.clone();
        let (sender, receiver) = mpsc::channel();
        self.save_job = Some(Mutex::new(receiver));
        self.notice(Message::new(
            format!("Exporting RepX to {path}"),
            format!("正在导出 RepX 到 {path}"),
        ));
        let cancelled = Arc::new(AtomicBool::new(false));
        let result = self
            .background_jobs
            .spawn("repx-export", cancelled.clone(), move || {
                let result = (|| {
                    if cancelled.load(Ordering::Acquire) {
                        return Err("export cancelled".to_owned());
                    }
                    let export =
                        pvd_core::repx::export(&state, selection).map_err(|e| e.to_string())?;
                    crate::atomic_output::copy_reader_new(
                        export.xml.as_bytes(),
                        Path::new(&path),
                        None,
                        || !cancelled.load(Ordering::Acquire),
                    )
                    .map_err(|e| e.to_string())?;
                    let message = if export.kinematic_actors == 0 {
                        Message::detail("RepX saved", "RepX 已保存", &path)
                    } else {
                        Message::new(
                            format!(
                                "RepX saved: {path}\n{} kinematic actors: targets, velocities and wake counters are not restored.",
                                export.kinematic_actors
                            ),
                            format!(
                                "RepX 已保存：{path}\n含 {} 个运动学刚体；导入后不恢复目标、速度和唤醒计时。",
                                export.kinematic_actors
                            ),
                        )
                    };
                    Ok::<_, String>(message)
                })();
                let _ = sender.send(result.unwrap_or_else(|error| {
                    Message::detail("RepX export failed", "RepX 导出失败", error)
                }));
                context.request_repaint();
            });
        if let Err(error) = result {
            self.save_job = None;
            self.notice(Message::detail(
                "RepX export failed",
                "RepX 导出失败",
                error,
            ));
        }
    }
    pub fn stop_listener(&mut self) {
        if let Some(server) = &self.server {
            server.request_stop();
        }
        self.connection = Message::new("Stopping listener…", "正在停止监听…");
    }
    fn poll_seek(&mut self, context: &egui::Context) {
        if let Some(completed) = self.seek_worker.as_ref().and_then(seeking::Worker::poll) {
            #[cfg(test)]
            let rebase_witness = self.pending_playback_rebase.take().and_then(|witness| {
                witness.accepted(
                    completed.frame,
                    self.seek_worker.as_ref().unwrap().generation(),
                )
            });
            if let Some(trace) = &completed.trace {
                trace.event("accept_begin", 0);
            }
            self.seeking = None;
            let retained = self
                .timeline_recording()
                .is_some_and(|recording| recording.is_frame_retained(completed.frame));
            let intent = self.playback_recovery.finish(
                completed.frame,
                (self.first_frame(), self.count().saturating_sub(1)),
                retained,
                Instant::now(),
            );
            match completed.result {
                Ok((recording, update)) => {
                    #[cfg(test)]
                    {
                        self.accepted_playback_rebase = rebase_witness;
                    }
                    if self.complete_profile_source().is_some() {
                        self.recording = Some(Arc::new(recording));
                    } else {
                        self.set_recording(recording);
                    }
                    self.displayed_analysis = if completed.analysis_requested {
                        analysis_demand::DisplayedAnalysis::Frame(completed.frame)
                    } else {
                        analysis_demand::DisplayedAnalysis::Deferred
                    };
                    if let Some(trace) = &completed.trace {
                        trace.event("tree_begin", 0);
                    }
                    self.tree
                        .retain_for_update(&update, &self.state, self.revision);
                    if let Some(trace) = &completed.trace {
                        trace.event("tree_end", 0);
                    }
                    self.scene_changes.record(&update, self.revision);
                    if let Some(trace) = &completed.trace {
                        trace.event("apply_begin", 0);
                    }
                    self.refresh_selection();
                    self.activity.apply_update(update, &mut self.state);
                    self.refresh_selection();
                    if let Some(trace) = &completed.trace {
                        trace.event("apply_end", 0);
                    }
                    self.refresh_live_errors();
                    self.remember_display_frame(completed.frame);
                    self.revision += 1;
                    if let Some(trace) = &completed.trace {
                        self.display_request = trace.id;
                        trace.event("accepted", self.revision);
                    }
                    if self.playing != 0
                        && self
                            .display_trace
                            .as_ref()
                            .is_some_and(|trace| trace.stop_at(self.frame))
                    {
                        self.pause_display();
                    }
                    if self.playing == 0 {
                        self.last_step = Instant::now();
                    }
                    if intent != playback_recovery::Intent::Manual && self.playing > 0 && !retained
                    {
                        self.continue_from_retained_start();
                    }
                }
                Err(error) if intent != playback_recovery::Intent::Manual => {
                    self.handle_seek_failure(completed.frame, completed.expired, error);
                }
                Err(error) => {
                    if completed.analysis_requested
                        && self.frame == completed.frame
                        && !self.analysis_ready()
                    {
                        self.displayed_analysis = analysis_demand::DisplayedAnalysis::Failed {
                            frame: completed.frame,
                            message: Message::detail(
                                "Analysis loading failed",
                                "分析加载失败",
                                error.clone(),
                            ),
                        };
                    }
                    self.stop_seek_failure(completed.frame, error);
                }
            }
        }
        if self.seeking.is_some() || self.playback_recovery.waiting() {
            context.request_repaint_after(Duration::from_millis(16));
        }
    }
    fn handle_seek_failure(&mut self, frame: usize, expired: bool, error: String) {
        if self.playing > 0
            && self.has_history()
            && (frame < self.first_frame() || expired && self.live_connected)
        {
            self.continue_from_retained_start();
        } else {
            self.stop_seek_failure(frame, error);
        }
    }
    fn stop_seek_failure(&mut self, frame: usize, error: String) {
        self.playing = 0;
        if !self.follow_live {
            self.restore_display_frame_source();
            self.display_reason = Some(DisplayReason::SeekFailed);
        }
        self.playback_recovery.clear();
        self.notice(Message::new(
            format!("Frame {frame}: {error}"),
            format!("第 {frame} 帧：{error}"),
        ));
    }
    fn accept_live_snapshot(&mut self, snapshot: LiveSnapshot) {
        if self.live_session != Some(snapshot.session) {
            return;
        }
        if !snapshot.display_updated {
            self.live_recording = Some(Arc::new(snapshot.recording));
            return;
        }
        self.live_frame_source = snapshot.source_frame.or_else(|| {
            snapshot
                .recording
                .frame_sources
                .frame(snapshot.frame)
                .map(|(source, frame)| (source, *frame))
        });
        self.live_analysis = snapshot.recording.analysis.clone();
        if self.follow_live {
            snapshot.update.apply(&mut self.live_state);
        }
        let now = Instant::now();
        while self
            .live_update_times
            .front()
            .is_some_and(|time| now.duration_since(*time) > Duration::from_secs(1))
        {
            self.live_update_times.pop_front();
        }
        self.live_update_times.push_back(now);
        self.live_frame = snapshot.frame;
        let recording = Arc::new(snapshot.recording);
        self.live_recording = Some(recording.clone());
        self.source = "Live TCP capture".into();
        if self.follow_live {
            self.tree
                .retain_for_update(&snapshot.update, &self.state, self.revision);
            self.scene_changes.record(&snapshot.update, self.revision);
            self.refresh_selection();
            self.activity.apply_update(snapshot.update, &mut self.state);
            self.refresh_selection();
            self.recording = Some(recording);
            self.frame = snapshot.frame;
            self.displayed_frame_source = self.live_frame_source;
            self.displayed_analysis = analysis_demand::DisplayedAnalysis::Complete;
            self.refresh_live_errors();
            self.revision += 1;
        } else {
            snapshot.update.apply_owned(&mut self.live_state);
        }
    }
    fn connect_live_session(&mut self, session: u64) {
        self.clear_recording();
        self.live_session = Some(session);
        self.live_connected = true;
        self.follow_live = true;
    }
    fn poll_network(&mut self, context: &egui::Context) {
        if let Some(status) = self
            .server
            .as_ref()
            .and_then(NetworkSession::receive_status)
        {
            self.receive_status = Some(status);
        }
        let events: Vec<_> = self
            .server
            .as_ref()
            .and_then(|server| server.receiver.lock().ok())
            .map(|receiver| receiver.try_iter().take(16).collect())
            .unwrap_or_default();
        for event in events {
            match event {
                LiveEvent::Status(status) => {
                    if self
                        .server
                        .as_ref()
                        .is_some_and(|server| !server.is_stopping())
                    {
                        self.connection = status;
                    }
                }
                LiveEvent::Connected(session) => {
                    self.connect_live_session(session);
                    self.notice(Message::new(
                        format!("Connected session {session}"),
                        format!("会话 {session} 已连接"),
                    ));
                }
                LiveEvent::CaptureStarted { session, path } => {
                    if self.live_session == Some(session) {
                        self.notice(Message::new(
                            format!("Recording incoming bytes to {}", path.display()),
                            format!("正在将接收数据录制到 {}", path.display()),
                        ));
                        self.capture_path = Some(path);
                    }
                }
                LiveEvent::Empty { session } => {
                    if self.live_session == Some(session) {
                        self.live_connected = false;
                        self.notice(Message::new(
                            format!("Session {session} ended without receiving any data; no PVD recording was created"),
                            format!("会话 {session} 未收到数据便已断开，尚未形成 PVD 录制"),
                        ));
                    }
                }
                LiveEvent::Complete {
                    session,
                    received_bytes,
                    result,
                    capture,
                } => {
                    if self.live_session == Some(session) {
                        self.source = "Live TCP capture".into();
                        if let Some(snapshot) = self
                            .server
                            .as_ref()
                            .and_then(|server| server.take_snapshot_for(session))
                        {
                            self.accept_live_snapshot(snapshot);
                        }
                        self.live_connected = false;
                        self.notice(Message::new(
                            format!("Session {session} ended · {received_bytes} bytes"),
                            format!("会话 {session} 已结束 · {received_bytes} 字节"),
                        ));
                        if let Some(path) = capture {
                            self.notice(Message::new(
                                format!("Captured stream retained at {}", path.display()),
                                format!("捕获数据流已保存在 {}", path.display()),
                            ));
                            self.capture_path = Some(path);
                        }
                        match *result {
                            Ok(recording) => {
                                let recording = Arc::new(recording);
                                self.live_recording = Some(recording.clone());
                                if self.follow_live {
                                    let mut displayed = (*recording).clone();
                                    displayed.analysis = self.live_analysis.clone();
                                    self.recording = Some(Arc::new(displayed));
                                    if let Some((_, last)) = self.playback_bounds() {
                                        if last != self.frame {
                                            self.seek_paused(last, DisplayReason::Disconnected);
                                        }
                                    } else {
                                        self.selected_frame_source =
                                            self.displayed_frame_source.map(|(source, _)| source);
                                    }
                                }
                                self.clip_persistence.bind_complete_capture(
                                    None,
                                    Arc::default(),
                                    self.count(),
                                    context.clone(),
                                    &self.background_jobs,
                                );
                                self.clip_persistence.issue = Some(Message::new(
                                    "Clip editing and clip export are not yet supported for live disk history. Complete raw recordings can be saved independently.",
                                    "片段编辑和片段导出暂不支持实时磁盘历史。完整的原始录制仍可单独保存。",
                                ));
                            }
                            Err(error) => {
                                if self.follow_live {
                                    self.restore_display_frame_source();
                                }
                                self.notice(Message::detail(
                                    "Capture decode failed",
                                    "捕获数据解码失败",
                                    error,
                                ));
                            }
                        }
                    }
                }
                LiveEvent::Error(message) => {
                    self.notice(message.context("Capture error", "捕获错误"))
                }
                LiveEvent::Stopped => {
                    if self.follow_live {
                        self.display_reason = Some(DisplayReason::ListenerStopped);
                    }
                    self.last_live_profile = self
                        .server
                        .as_ref()
                        .and_then(NetworkSession::profile_snapshot);
                    if let Some(status) = self
                        .server
                        .as_ref()
                        .and_then(NetworkSession::receive_status)
                    {
                        self.receive_status = Some(status);
                    }
                    if let Some(status) = &mut self.receive_status {
                        status.connected = false;
                    }
                    if let Some(snapshot) = self.live_session.and_then(|session| {
                        self.server
                            .as_ref()
                            .and_then(|server| server.take_snapshot_for(session))
                    }) {
                        self.accept_live_snapshot(snapshot);
                    }
                    self.server = None;
                    self.live_connected = false;
                    self.connection = Message::new("Offline", "离线");
                }
            }
        }
        if let Some(snapshot) = self.live_session.and_then(|session| {
            self.server
                .as_ref()
                .and_then(|server| server.take_snapshot_for(session))
        }) {
            self.accept_live_snapshot(snapshot);
        }
        if !self.live_connected && self.follow_live {
            self.pause_display_for(self.display_reason.unwrap_or(DisplayReason::Disconnected));
        }
    }

    pub fn receive_summary(&self, language: Language) -> Option<String> {
        let status = self.receive_status.as_ref()?;
        if status.session == 0 && status.last_failure.is_none() {
            return None;
        }
        let phase = if status.last_failure.is_some() {
            if status.connected {
                language.text("Receive error", "接收故障")
            } else {
                language.text("Receive error · TCP disconnected", "接收故障 · TCP 已断开")
            }
        } else if !status.connected {
            if self.live_connected || status.queued_bytes > 0 {
                language.text(
                    "TCP disconnected · decoding buffered data",
                    "TCP 已断开 · 正在处理已接收数据",
                )
            } else {
                language.text("TCP disconnected", "TCP 已断开")
            }
        } else if self.receive_backlog_warning() {
            language.text("Large backlog · continuing reception", "积压较大，继续接收")
        } else if status.queued_bytes > 0 && self.follow_live {
            language.text("Display catching up", "显示追赶中")
        } else {
            language.text("Receiving", "接收中")
        };
        let received = status.received_bytes as f64 / (1024.0 * 1024.0);
        let queued = status.queued_bytes as f64 / (1024.0 * 1024.0);
        let mut summary = match language {
            Language::English => {
                format!("{phase} · TCP received {received:.1} MiB · disk backlog {queued:.1} MiB")
            }
            Language::Chinese => {
                format!("{phase} · TCP 已接收 {received:.1} MiB · 磁盘接收积压 {queued:.1} MiB")
            }
        };
        if let Some(decoding) = self
            .live_session
            .filter(|session| *session != status.session)
        {
            summary.push_str(&match language {
                Language::English => format!(
                    " · receive session {} / decoding session {decoding}",
                    status.session
                ),
                Language::Chinese => {
                    format!(" · 接收会话 {} / 解码会话 {decoding}", status.session)
                }
            });
        }
        Some(summary)
    }

    pub fn receive_backlog_warning(&self) -> bool {
        self.receive_status.as_ref().is_some_and(|status| {
            status.connected && status.last_failure.is_none() && status.queued_bytes >= 256 << 20
        })
    }

    pub fn receive_disk_summary(&self, language: Language) -> Option<String> {
        let status = self.receive_status.as_ref()?;
        let used = status.retained_disk_bytes as f64 / (1024.0 * 1024.0);
        let queued = status.queued_bytes as f64 / (1024.0 * 1024.0);
        let usage = if status.limit_bytes == 0 {
            let limit = language.text("limited by available disk space", "受可用磁盘空间限制");
            format!("{used:.1} MiB · {limit}")
        } else {
            let limit = status.limit_bytes as f64 / (1024.0 * 1024.0);
            format!("{used:.1} / {limit:.0} MiB")
        };
        Some(match language {
            Language::English => {
                format!("Receive buffer: {usage} on disk · {queued:.1} MiB waiting for decoding")
            }
            Language::Chinese => format!("接收缓冲：磁盘占用 {usage} · 磁盘待解码 {queued:.1} MiB"),
        })
    }

    pub fn receive_detail(&self, language: Language) -> String {
        let Some(status) = &self.receive_status else {
            return String::new();
        };
        let mut detail = self.receive_disk_summary(language).unwrap_or_default();
        detail.push_str(language.text(
            "\nReceive storage is separate from retained playback history. Disk backlog excludes chunks already handed to the decoder and the chunk being parsed. Displayed frames may lag behind TCP reception.\nA growing backlog increases delay and disk usage. Reaching an optional receive quota, reserved free-space limit or a disk write failure stops the connection; protocol events cannot be discarded safely.",
            "\n接收存储与回放历史容量独立。磁盘积压不含已交给解码器的内存块和当前解析块。显示帧可能落后于 TCP 接收进度。\n积压持续增长会增加延迟与磁盘占用。达到可选接收配额、磁盘保留空间或无法写盘时停止连接；协议事件不能安全丢弃。",
        ));
        if let Some(error) = &status.last_failure {
            let scope = match (language, status.failure_session) {
                (Language::English, Some(session)) => format!("Receive session {session}"),
                (Language::Chinese, Some(session)) => format!("接收会话 {session}"),
                (Language::English, None) => "Receive worker".into(),
                (Language::Chinese, None) => "接收工作线程".into(),
            };
            detail.push_str(&format!("\n{scope}: {error}"));
        }
        detail
    }

    pub fn live_follow_label(&self, language: Language) -> &'static str {
        if self.follow_live {
            language.text("● Following live", "● 跟随实时")
        } else if self.live_connected {
            language.text("Return to live", "返回实时")
        } else {
            language.text("Latest decoded frame", "最后解码帧")
        }
    }
}

impl Drop for Studio {
    fn drop(&mut self) {
        self.background_jobs.cancel_all_and_check_finished();
        self.reset_profile_views();
        self.curve_job = None;
        if let Some(job) = &self.job {
            job.cancelled.store(true, Ordering::Relaxed);
        }
        if self.persist_layout
            && !std::thread::panicking()
            && let Err(error) = self.prefs.save()
        {
            eprintln!(
                "{}",
                Message::detail("Preferences save failed", "偏好设置保存失败", error)
                    .text(self.prefs.language)
            );
        }
    }
}

fn read_recording(
    path: &Path,
    cancelled: &AtomicBool,
    progress: &AtomicU64,
    history: pvd_core::live::HistoryConfig,
) -> LoadResult {
    let mut file = File::open(path)
        .map_err(|e| Message::detail("Cannot open capture", "无法打开录制文件", e))?;
    let length = file
        .metadata()
        .map_err(|e| Message::detail("Cannot read file metadata", "无法读取文件信息", e))?
        .len();
    let mut signature = [0; 8];
    file.read_exact(&mut signature)
        .and_then(|_| file.rewind())
        .map_err(|e| Message::detail("Cannot read capture header", "无法读取录制文件头", e))?;
    if pvd_core::capture::Format::from_magic(&signature)
        .map_err(|error| {
            Message::detail("Cannot open project capture", "无法打开本项目捕获", error)
        })?
        .is_some()
    {
        drop(file);
        let recording = pvd_core::capture::open_with_progress(path, &Default::default(), |value| {
            progress.store(value.completed.min(length), Ordering::Relaxed);
            !cancelled.load(Ordering::Relaxed)
        })
        .map_err(|error| {
            Message::detail("Cannot open project capture", "无法打开本项目捕获", error)
        })?;
        return Ok(LoadedCapture {
            recording,
            bytes: None,
            source_path: Some(path.to_owned()),
            source_identity: None,
            clips: None,
        });
    }
    if pvd_core::pvd_archive::has_zip_signature(&signature) {
        return archive::read(&mut file, length, cancelled, progress, history);
    }
    let native_container = signature[..4] == pvd_core::native::MAGIC.to_le_bytes();
    if native_container {
        drop(file);
        let (recording, info) =
            pvd_core::native::read_path_with_progress(path, history, |completed, total| {
                let indexed = if total == 0 {
                    0
                } else {
                    (u128::from(completed) * u128::from(length) / u128::from(total))
                        .min(u128::from(length)) as u64
                };
                progress.store(indexed, Ordering::Relaxed);
                !cancelled.load(Ordering::Relaxed)
            })
            .map_err(|error| {
                Message::detail("Cannot import native capture", "无法导入原版录制", error)
            })?;
        validate_complete_disk_history(&recording)?;
        let source_identity = info.source_blake3.map(|digest| {
            crate::clip_store::Identity::from_digest(
                blake3::Hash::from_bytes(digest),
                info.source_bytes,
                recording.frame_count(),
            )
        });
        return Ok(LoadedCapture {
            recording,
            bytes: None,
            source_path: Some(path.to_owned()),
            source_identity,
            clips: None,
        });
    }
    if length > IN_MEMORY_SDK_BYTES {
        return read_recording_on_disk(path, file, cancelled, progress, history);
    }
    let mut bytes = Vec::with_capacity(length as usize);
    let mut decoder = LiveRecording::default();
    let mut chunk = [0u8; 65536];
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err(Message::new("Cancelled", "已取消"));
        }
        let count = file
            .read(&mut chunk)
            .map_err(|e| Message::detail("Cannot read capture", "无法读取录制文件", e))?;
        if count == 0 {
            break;
        }
        if bytes.len() as u64 + count as u64 > IN_MEMORY_SDK_BYTES {
            drop(decoder);
            drop(bytes);
            return read_recording_on_disk(path, file, cancelled, progress, history);
        }
        if bytes.len() as u64 + count as u64 > MAX_CAPTURE_BYTES {
            return Err(Message::new(
                "Capture grew beyond size limit",
                "录制文件增长后超出大小限制",
            ));
        }
        bytes.extend_from_slice(&chunk[..count]);
        if let Err(error) = decoder.feed(&chunk[..count]) {
            if requires_disk_history(&error) {
                drop(decoder);
                drop(bytes);
                return read_recording_on_disk(path, file, cancelled, progress, history);
            }
            return Err(Message::detail(
                "Capture data is invalid or unsupported",
                "捕获数据无效或暂不支持",
                error,
            ));
        }
        progress.store(bytes.len() as u64, Ordering::Relaxed);
    }
    let recording = decoder.finish().map_err(|e| {
        Message::detail(
            "Capture is incomplete or invalid",
            "捕获数据不完整或无效",
            e,
        )
    })?;
    let source_identity = Some(
        crate::clip_store::identity(&bytes, recording.frame_count(), cancelled)
            .map_err(|error| error.message())?,
    );
    let clips = source_identity
        .clone()
        .map(|identity| crate::clip_store::load_identified(path, identity, cancelled));
    Ok(LoadedCapture {
        recording,
        bytes: Some(Arc::new(bytes)),
        source_path: Some(path.to_owned()),
        source_identity,
        clips,
    })
}

fn read_recording_on_disk(
    path: &Path,
    file: File,
    cancelled: &AtomicBool,
    progress: &AtomicU64,
    history: pvd_core::live::HistoryConfig,
) -> LoadResult {
    let mut loaded = read_sdk_reader(file, cancelled, progress, history)?;
    loaded.clips = loaded
        .source_identity
        .clone()
        .map(|identity| crate::clip_store::load_identified(path, identity, cancelled));
    loaded.source_path = Some(path.to_owned());
    Ok(loaded)
}

fn read_sdk_reader(
    mut file: impl Read + Seek,
    cancelled: &AtomicBool,
    progress: &AtomicU64,
    history: pvd_core::live::HistoryConfig,
) -> LoadResult {
    file.rewind().map_err(|error| {
        Message::detail("Cannot restart capture read", "无法重新读取录制", error)
    })?;
    progress.store(0, Ordering::Relaxed);
    let mut decoder = LiveRecording::with_history(history).map_err(|error| {
        Message::detail("Cannot create disk history", "无法创建磁盘历史", error)
    })?;
    let mut chunk = [0u8; 65536];
    let mut received = 0u64;
    let mut checked_at = 0u64;
    let mut source_hash = blake3::Hasher::new();
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err(Message::new("Cancelled", "已取消"));
        }
        let count = file
            .read(&mut chunk)
            .map_err(|error| Message::detail("Cannot read capture", "无法读取录制文件", error))?;
        if count == 0 {
            break;
        }
        decoder.feed(&chunk[..count]).map_err(|error| {
            Message::detail(
                "Capture data is invalid or unsupported",
                "捕获数据无效或暂不支持",
                error,
            )
        })?;
        let _ = decoder.take_update();
        source_hash.update(&chunk[..count]);
        received += count as u64;
        progress.store(received, Ordering::Relaxed);
        if decoder.frame_count() > 0 && received.saturating_sub(checked_at) >= 4 * 1024 * 1024 {
            validate_complete_disk_history(&decoder.snapshot().map_err(|error| {
                Message::detail("Cannot index capture", "无法索引录制", error)
            })?)?;
            checked_at = received;
        }
    }
    let recording = decoder.finish().map_err(|error| {
        Message::detail(
            "Capture is incomplete or invalid",
            "捕获数据不完整或无效",
            error,
        )
    })?;
    validate_complete_disk_history(&recording)?;
    let identity = crate::clip_store::Identity::from_digest(
        source_hash.finalize(),
        received,
        recording.frame_count(),
    );
    Ok(LoadedCapture {
        recording,
        bytes: None,
        source_path: None,
        source_identity: Some(identity),
        clips: None,
    })
}

fn requires_disk_history(error: &pvd_core::Error) -> bool {
    if error
        .downcast_ref::<pvd_core::analysis::DecodeError>()
        .is_some()
    {
        return true;
    }
    let detail = error.to_string();
    detail.contains("history memory budget")
        || detail.contains("512 MiB raw stream limit")
        || detail.contains("512 MiB indexing limit")
        || (detail.starts_with("analysis exceeds its ")
            && detail.ends_with(" byte allocation budget"))
        || detail == "profile analysis exceeds two million events"
        || detail == "memory history exceeds two million allocation/free events"
        || detail == "recording exceeds 20 million state changes"
}

fn validate_complete_disk_history(recording: &Recording) -> Result<(), Message> {
    if let Some(stats) = recording.disk_usage()
        && (stats.first_frame != 0
            || stats.retained_frames != recording.frame_count()
            || stats.warning.is_some())
    {
        let detail = stats.warning.unwrap_or_default();
        return Err(Message::new(
            format!(
                "The disk history cache cannot retain this entire file. Increase its quota or choose a drive with more free space. The source file is unchanged. {detail}"
            ),
            format!(
                "磁盘历史缓存无法容纳完整文件。请提高缓存预算或选择剩余空间更多的磁盘；源文件保持完整。{detail}"
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod captured_camera_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_live_profile_survives_open_and_resets_with_new_listener() {
        use std::io::Write;
        use std::net::{TcpListener, TcpStream};

        let directory = std::env::temp_dir().join(format!(
            "pvd-studio-profile-history-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let context = egui::Context::default();
        let mut studio = Studio::default();
        studio.live_profile_enabled = true;
        studio.prefs.history_directory = directory.to_string_lossy().into_owned();
        studio.server = Some(
            NetworkSession::start_tracked(
                ListenConfig {
                    address: "127.0.0.1:0".parse().unwrap(),
                    ..Default::default()
                },
                NetworkOptions {
                    live_profile: true,
                    history: pvd_core::live::HistoryConfig {
                        directory: directory.clone(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                context.clone(),
                &studio.background_jobs,
            )
            .unwrap(),
        );
        let address = studio.server.as_ref().unwrap().address();
        let mut header = Vec::new();
        header.extend(pvd_protocol::STREAM_MAGIC.to_le_bytes());
        header.extend(1u32.to_le_bytes());
        header.extend(1u64.to_le_bytes());
        header.extend(10u64.to_le_bytes());
        header.extend(1u32.to_le_bytes());
        TcpStream::connect(address)
            .unwrap()
            .write_all(&header)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let completed = loop {
            studio.poll_network(&context);
            if let Some(profile) = studio
                .server
                .as_ref()
                .and_then(NetworkSession::profile_snapshot)
                && profile.completed
            {
                break profile;
            }
            assert!(
                Instant::now() < deadline,
                "profiled connection did not finish"
            );
            std::thread::park_timeout(Duration::from_millis(5));
        };
        assert!(studio.last_live_profile.is_none());
        let missing = std::env::temp_dir().join(format!(
            "pvd-profile-open-{}-{}.missing",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        studio.open(missing.clone(), context.clone());
        assert!(studio.server.is_none());
        for _ in 0..3 {
            studio.poll_network(&context);
            let retained = studio.last_live_profile.as_ref().unwrap();
            assert_eq!(retained.session, completed.session);
            assert!(retained.completed);
            assert_eq!(retained.elapsed_seconds, completed.elapsed_seconds);
        }
        studio.open(missing, context.clone());
        assert_eq!(
            studio.last_live_profile.as_ref().unwrap().session,
            completed.session
        );
        let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
        studio.prefs.listener.host = "127.0.0.1".into();
        studio.prefs.listener.port = reservation.local_addr().unwrap().port();
        drop(reservation);
        studio.start_listener(context);
        assert!(studio.server.is_some());
        assert!(studio.last_live_profile.is_none());
        let deadline = Instant::now() + Duration::from_secs(5);
        while !studio.poll_shutdown() {
            assert!(
                Instant::now() < deadline,
                "test profile jobs did not finish"
            );
            std::thread::park_timeout(Duration::from_millis(1));
        }
        assert_eq!(studio.background_jobs.active_count(), 0);
        let mut directories = vec![directory];
        let mut files = Vec::new();
        let mut position = 0;
        while position < directories.len() {
            let path = &directories[position];
            if path.exists() {
                for entry in std::fs::read_dir(path).unwrap() {
                    let entry = entry.unwrap();
                    if entry.file_type().unwrap().is_dir() {
                        directories.push(entry.path());
                    } else {
                        files.push(entry.path());
                    }
                }
            }
            position += 1;
        }
        assert!(
            files.is_empty(),
            "test history cache files remain: {files:?}"
        );
        for path in directories.into_iter().rev() {
            if path.exists() {
                std::fs::remove_dir(path).unwrap();
            }
        }
    }

    fn live_snapshot(session: u64, frame: usize) -> LiveSnapshot {
        use pvd_protocol::{Endian, Event, EventGroup, Header};
        let mut builder = pvd_core::RecordingBuilder::new(Header {
            version: 1,
            timestamp_numerator: 1,
            timestamp_denominator: 1,
            flags: 1,
            endian: Endian::Little,
        });
        let mut events = vec![Event::StringHandle {
            handle: 1,
            value: "frame".into(),
        }];
        events.extend((0..=frame).map(|index| Event::EndSection {
            id: 1,
            name: 1,
            timestamp: index as u64,
        }));
        builder
            .push(EventGroup {
                stream_id: 1,
                timestamp: 0,
                offset: 0,
                events,
            })
            .unwrap();
        let mut state = SceneState::default();
        state.objects.insert(
            9,
            pvd_core::Object {
                id: 9,
                generation: 0,
                type_name: "FrameMarker".into(),
                properties: [("frame".into(), pvd_core::Value::U64(frame as u64))].into(),
            },
        );
        LiveSnapshot {
            source_frame: None,
            display_updated: true,
            session,
            frame,
            recording: builder.finish().unwrap(),
            update: pvd_core::live::SceneUpdate::full(&state),
        }
    }

    fn receive_status(session: u64, connected: bool) -> SpoolSnapshot {
        SpoolSnapshot {
            session,
            received_bytes: 80 << 20,
            queued_bytes: 8 << 20,
            retained_disk_bytes: 16 << 20,
            limit_bytes: 256 << 20,
            connected,
            last_failure: None,
            failure_session: None,
        }
    }

    #[test]
    fn socket_disconnect_does_not_stop_following_buffered_snapshots() {
        let mut studio = Studio::default();
        studio.connect_live_session(4);
        studio.accept_live_snapshot(live_snapshot(4, 0));
        studio.receive_status = Some(receive_status(4, false));
        studio.poll_network(&egui::Context::default());
        studio.accept_live_snapshot(live_snapshot(4, 3));
        assert!(studio.follow_live);
        assert_eq!(studio.frame, 3);
        assert!(
            studio
                .receive_summary(Language::English)
                .unwrap()
                .contains("decoding buffered data")
        );
        assert!(
            studio
                .receive_summary(Language::Chinese)
                .unwrap()
                .contains("正在处理已接收数据")
        );
        studio.toggle_playback();
        studio.accept_live_snapshot(live_snapshot(4, 6));
        assert_eq!(studio.frame, 3);
        assert_eq!(studio.live_follow_label(Language::Chinese), "返回实时");
        studio.return_to_live();
        assert_eq!(studio.frame, 6);
        assert_eq!(studio.live_follow_label(Language::Chinese), "● 跟随实时");
    }

    #[test]
    fn activity_counts_follow_displayed_state_through_pause_seek_resume_and_close() {
        use pvd_core::{Object, Value};
        let mut state = SceneState::default();
        state.objects.insert(
            9,
            Object {
                id: 9,
                generation: 0,
                type_name: "PxRigidDynamic".into(),
                properties: [
                    ("RigidBodyFlags".into(), Value::U64(0)),
                    ("IsSleeping".into(), Value::Bool(false)),
                ]
                .into(),
            },
        );
        let mut studio = Studio::default();
        studio.connect_live_session(4);
        let mut snapshot = live_snapshot(4, 0);
        snapshot.update = pvd_core::live::SceneUpdate::full(&state);
        studio.accept_live_snapshot(snapshot);
        assert_eq!(studio.activity.summary().awake_dynamic, 1);
        assert_eq!(studio.activity.summary().sdk_active_total(), None);
        studio.toggle_playback();
        state
            .objects
            .get_mut(&9)
            .unwrap()
            .properties
            .insert("IsSleeping".into(), Value::Bool(true));
        state.objects.insert(
            11,
            Object {
                id: 11,
                generation: 0,
                type_name: "PxRigidDynamic".into(),
                properties: [("RigidBodyFlags".into(), Value::U64(1))].into(),
            },
        );
        state.objects.insert(
            12,
            Object {
                id: 12,
                generation: 0,
                type_name: "PxRigidStatic".into(),
                properties: Default::default(),
            },
        );
        let mut snapshot = live_snapshot(4, 3);
        snapshot.update = pvd_core::live::SceneUpdate::full(&state);
        studio.accept_live_snapshot(snapshot);
        assert_eq!(studio.activity.summary().total_actors, 1);
        assert_eq!(studio.activity.summary().awake_dynamic, 1);
        studio.return_to_live();
        let counts = studio.activity.summary();
        assert_eq!(counts.total_actors, 3);
        assert_eq!(counts.awake_dynamic, 0);
        assert_eq!(counts.sleeping_dynamic, 1);
        assert_eq!(counts.kinematic, 1);
        assert_eq!(counts.static_actors, 1);
        studio.seek(0);
        assert!(studio.state.objects.is_empty());
        assert_eq!(studio.activity.summary().total_actors, 0);
        studio.return_to_live();
        assert_eq!(studio.activity.summary().total_actors, 3);
        studio.close();
        assert_eq!(studio.activity.summary().total_actors, 0);
        assert_eq!(studio.activity.summary().sdk_active_total(), None);
    }

    #[test]
    fn receive_error_identifies_new_socket_session_without_changing_decoding_session() {
        let mut studio = Studio::default();
        studio.connect_live_session(4);
        let mut status = receive_status(5, false);
        status.failure_session = Some(5);
        status.last_failure = Some("receive backlog capacity reached".into());
        studio.receive_status = Some(status);
        studio.poll_network(&egui::Context::default());
        assert_eq!(studio.live_session, Some(4));
        assert!(studio.follow_live);
        let english = studio.receive_summary(Language::English).unwrap();
        let chinese = studio.receive_summary(Language::Chinese).unwrap();
        assert!(english.contains("TCP disconnected"));
        assert!(english.contains("receive session 5 / decoding session 4"));
        assert!(chinese.contains("接收会话 5 / 解码会话 4"));
        assert!(
            studio
                .receive_detail(Language::English)
                .contains("Receive session 5:")
        );
        assert!(
            studio
                .receive_detail(Language::Chinese)
                .contains("接收会话 5:")
        );
        studio.accept_live_snapshot(live_snapshot(4, 7));
        assert_eq!(studio.frame, 7);
    }

    #[test]
    fn large_unbounded_backlog_is_a_warning_and_keeps_following_live() {
        let mut studio = Studio::default();
        studio.connect_live_session(4);
        let mut status = receive_status(4, true);
        status.limit_bytes = 0;
        status.queued_bytes = 1024 << 20;
        status.retained_disk_bytes = 1040 << 20;
        studio.receive_status = Some(status);
        studio.poll_network(&egui::Context::default());
        assert!(studio.receive_backlog_warning());
        for (language, warning, limit) in [
            (
                Language::English,
                "Large backlog · continuing reception",
                "limited by available disk space",
            ),
            (
                Language::Chinese,
                "积压较大，继续接收",
                "受可用磁盘空间限制",
            ),
        ] {
            assert!(studio.receive_summary(language).unwrap().contains(warning));
            for text in [
                studio.receive_disk_summary(language).unwrap(),
                studio.receive_detail(language),
            ] {
                assert!(text.contains(limit));
                assert!(!text.contains("/ 0"));
            }
        }
        studio.accept_live_snapshot(live_snapshot(4, 7));
        assert!(studio.follow_live);
        assert_eq!(studio.frame, 7);
        let status = studio.receive_status.as_mut().unwrap();
        status.limit_bytes = 2048 << 20;
        status.queued_bytes = 1 << 20;
        assert!(!studio.receive_backlog_warning());
        for language in [Language::English, Language::Chinese] {
            assert!(
                studio
                    .receive_disk_summary(language)
                    .unwrap()
                    .contains("1040.0 / 2048 MiB")
            );
        }
    }

    #[test]
    fn paused_live_view_receives_history_without_moving_display_and_resumes_latest() {
        let mut studio = Studio::default();
        studio.connect_live_session(4);
        studio.accept_live_snapshot(live_snapshot(4, 0));
        let first = studio.state.clone();
        studio.toggle_playback();
        assert!(!studio.follow_live);
        assert_eq!(studio.playing, 0);
        studio.accept_live_snapshot(live_snapshot(4, 3));
        assert_eq!(studio.count(), 4);
        assert_eq!(studio.frame, 0);
        assert_eq!(studio.state, first);
        studio.return_to_live();
        assert!(studio.follow_live);
        assert_eq!(studio.frame, 3);
        assert_eq!(
            studio.state.objects[&9].properties["frame"],
            pvd_core::Value::U64(3)
        );
        studio.seek(1);
        assert!(!studio.follow_live);
        studio.accept_live_snapshot(live_snapshot(4, 6));
        assert_eq!(studio.frame, 1);
        assert_eq!(studio.count(), 7);
        studio.return_to_live();
        assert_eq!(studio.frame, 6);
    }

    #[test]
    fn invalid_and_same_frame_navigation_keep_live_following_new_snapshots() {
        let mut studio = Studio::default();
        studio.connect_live_session(4);
        studio.seek(0);
        studio.step_frame(1);
        assert_eq!(studio.display_mode(), DisplayMode::Waiting);
        assert!(studio.follow_live);
        studio.accept_live_snapshot(live_snapshot(4, 3));
        let revision = studio.revision;
        let state = studio.state.clone();
        studio.seek(usize::MAX);
        studio.seek(3);
        studio.step_frame(1);
        studio.seek_paused(3, DisplayReason::Timeline);
        assert!(studio.follow_live);
        assert_eq!(studio.display_mode(), DisplayMode::Live);
        assert!(studio.display_reason.is_none());
        assert!(studio.seeking.is_none());
        assert_eq!(studio.revision, revision);
        assert_eq!(studio.state, state);
        studio.accept_live_snapshot(live_snapshot(4, 6));
        assert_eq!(studio.frame, 6);
        let mut snapshot = live_snapshot(4, 9);
        let mut frames: Vec<_> = snapshot.recording.frames.iter().cloned().collect();
        frames.retain(|frame| frame.index != 7);
        snapshot.recording.frames.clear();
        for frame in frames {
            snapshot.recording.frames.push(frame);
        }
        studio.accept_live_snapshot(snapshot);
        studio.seek(7);
        assert!(studio.follow_live);
        studio.accept_live_snapshot(live_snapshot(4, 12));
        assert_eq!(studio.frame, 12);
        assert!(
            studio
                .notices
                .iter()
                .all(|notice| !notice.text(Language::English).contains("Left live view"))
        );
        assert!(studio.seek_with_analysis(10, playback_recovery::Intent::Manual, true, None));
        assert!(!studio.follow_live);
        assert_eq!(studio.display_mode(), DisplayMode::Paused);
        assert_eq!(studio.frame, 10);
    }

    #[test]
    fn navigation_reason_and_display_lag_survive_receiving_until_live_resumes() {
        let context = egui::Context::default();
        let mut studio = Studio::default();
        studio.connect_live_session(4);
        studio.accept_live_snapshot(live_snapshot(4, 6));
        studio.request_ui_action(
            &context,
            "timeline",
            UiAction::Locate(2, DisplayReason::Timeline),
        );
        studio.apply_ui_actions(&context);
        assert_eq!(studio.display_mode(), DisplayMode::Paused);
        assert_eq!(studio.display_reason, Some(DisplayReason::Timeline));
        assert_eq!(studio.display_detail(Language::Chinese), "时间轴定位");
        studio.accept_live_snapshot(live_snapshot(4, 9));
        assert_eq!(studio.frame, 2);
        assert_eq!(
            studio.display_position(Language::Chinese),
            "帧 2 · 落后 7 帧 · 最新 9"
        );
        assert_eq!(studio.source_text(Language::Chinese), "TCP 捕获");
        assert!(
            studio
                .notices
                .iter()
                .any(|notice| notice.text(Language::English)
                    == "Left live view: Timeline navigation")
        );
        studio.return_to_live();
        assert_eq!(studio.display_mode(), DisplayMode::Live);
        assert!(studio.display_reason.is_none());
        assert_eq!(studio.frame, 9);
        studio.step_frame(-1);
        assert_eq!(studio.display_reason, Some(DisplayReason::PreviousFrame));
        studio.return_to_live();
        studio.pause_display();
        assert_eq!(studio.display_reason, Some(DisplayReason::Pause));
    }

    #[test]
    fn stopping_listener_preserves_last_frame_and_explains_the_live_exit() {
        let context = egui::Context::default();
        let (server, events) = NetworkSession::test_session(live_snapshot(4, 3));
        let mut studio = Studio::default();
        studio.connect_live_session(4);
        studio.server = Some(server);
        studio.poll_network(&context);
        let state = studio.state.clone();
        events.send(LiveEvent::Stopped).unwrap();
        studio.poll_network(&context);
        assert!(!studio.live_connected);
        assert!(!studio.follow_live);
        assert_eq!(studio.display_mode(), DisplayMode::Paused);
        assert_eq!(studio.display_detail(Language::Chinese), "已停止监听");
        assert_eq!(studio.state, state);
        assert_eq!(studio.frame, 3);
    }

    #[test]
    fn stale_live_session_cannot_replace_current_or_closed_display() {
        let mut studio = Studio::default();
        studio.connect_live_session(7);
        studio.accept_live_snapshot(live_snapshot(7, 2));
        studio.accept_live_snapshot(live_snapshot(6, 9));
        assert_eq!(studio.frame, 2);
        assert_eq!(studio.count(), 3);
        studio.close();
        studio.accept_live_snapshot(live_snapshot(7, 4));
        assert!(studio.state.objects.is_empty());
        assert_eq!(studio.count(), 0);
        assert!(!studio.follow_live);
    }

    #[test]
    fn next_session_snapshot_waits_for_its_queued_connection_event() {
        let context = egui::Context::default();
        for prior_session in [None, Some(1)] {
            let (server, events) = NetworkSession::test_session(live_snapshot(2, 7));
            let mut studio = Studio::default();
            if let Some(session) = prior_session {
                studio.connect_live_session(session);
                studio.accept_live_snapshot(live_snapshot(session, 1));
            }
            studio.server = Some(server);
            let prior_state = studio.state.clone();
            studio.poll_network(&context);
            assert_eq!(studio.state, prior_state);
            events.send(LiveEvent::Connected(2)).unwrap();
            studio.poll_network(&context);
            assert_eq!(studio.live_session, Some(2));
            assert_eq!(studio.frame, 7);
            assert_eq!(
                studio.state.objects[&9].properties["frame"],
                pvd_core::Value::U64(7)
            );
            assert!(studio.follow_live);
        }
    }

    #[test]
    fn empty_tcp_session_ends_without_a_decode_error_or_recording() {
        let context = egui::Context::default();
        let (server, events) = NetworkSession::test_session(live_snapshot(1, 0));
        server.take_snapshot();
        let mut studio = Studio::default();
        studio.server = Some(server);
        events.send(LiveEvent::Connected(1)).unwrap();
        events.send(LiveEvent::Empty { session: 1 }).unwrap();
        studio.poll_network(&context);
        assert!(studio.server.is_some());
        assert!(!studio.live_connected);
        assert!(!studio.follow_live);
        assert!(studio.recording.is_none());
        assert!(studio.live_recording.is_none());
        assert!(studio.capture_path.is_none());
        assert_eq!(studio.count(), 0);
        assert!(studio.notices.iter().any(|notice| {
            notice.text(Language::Chinese).contains("未收到数据")
                && notice
                    .text(Language::English)
                    .contains("without receiving any data")
        }));
        assert!(studio.notices.iter().all(|notice| {
            !notice.text(Language::Chinese).contains("捕获数据解码失败")
                && !notice
                    .text(Language::English)
                    .contains("Capture decode failed")
        }));
    }

    #[test]
    fn stale_empty_tcp_session_cannot_end_the_current_capture() {
        let context = egui::Context::default();
        let (server, events) = NetworkSession::test_session(live_snapshot(2, 3));
        let mut studio = Studio::default();
        studio.connect_live_session(2);
        studio.server = Some(server);
        studio.poll_network(&context);
        let state = studio.state.clone();
        let revision = studio.revision;
        events.send(LiveEvent::Empty { session: 1 }).unwrap();
        studio.poll_network(&context);
        assert_eq!(studio.live_session, Some(2));
        assert!(studio.live_connected);
        assert!(studio.follow_live);
        assert_eq!(studio.state, state);
        assert_eq!(studio.revision, revision);
        assert_eq!(studio.frame, 3);
    }

    #[test]
    fn live_pause_and_resume_remain_available_without_disk_history() {
        let mut studio = Studio::default();
        studio.connect_live_session(5);
        let mut snapshot = live_snapshot(5, 9);
        snapshot.recording.frames.clear();
        studio.accept_live_snapshot(snapshot);
        assert_eq!(studio.count(), 10);
        assert!(!studio.has_history());
        studio.toggle_playback();
        assert!(!studio.follow_live);
        studio.toggle_playback();
        assert!(studio.follow_live);
        assert_eq!(studio.frame, 9);
    }

    #[test]
    fn history_worker_replaces_pending_seeks_and_discards_cancelled_results() {
        let worker = seeking::Worker::new();
        let recording = Arc::new(live_snapshot(1, 20).recording);
        for frame in 0..20 {
            worker.request(recording.clone(), frame);
        }
        worker.cancel();
        std::thread::sleep(Duration::from_millis(20));
        assert!(worker.poll().is_none());
        worker.request(recording, 7);
        let deadline = Instant::now() + Duration::from_secs(5);
        let result = loop {
            if let Some(result) = worker.poll() {
                break result;
            }
            assert!(Instant::now() < deadline, "seek worker timed out");
            std::thread::sleep(Duration::from_millis(2));
        };
        assert_eq!(result.frame, 7);
        assert!(result.result.is_ok());
        assert!(worker.poll().is_none());
    }

    fn small_disk_recording() -> Recording {
        disk_recording(
            8,
            pvd_core::live::HistoryConfig {
                disk_budget_bytes: 4 << 20,
                min_free_disk_bytes: 0,
                ..Default::default()
            },
        )
    }

    fn disk_recording(frames: usize, history: pvd_core::live::HistoryConfig) -> Recording {
        let mut live = LiveRecording::with_history(history).unwrap();
        live.feed(&disk_frame_stream(0, frames)).unwrap();
        live.finish().unwrap()
    }

    fn disk_frame_stream(first: usize, frames: usize) -> Vec<u8> {
        let mut bytes = pvd_protocol::STREAM_MAGIC.to_le_bytes().to_vec();
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(1u64.to_le_bytes());
        bytes.extend(1_000_000u64.to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        let mut events = vec![1u8];
        events.extend(6u32.to_le_bytes());
        events.extend(b"frame\0");
        events.extend(1u32.to_le_bytes());
        for frame in first as u64..(first + frames) as u64 {
            events.push(19);
            events.extend(1u64.to_le_bytes());
            events.extend(1u32.to_le_bytes());
            events.extend((frame * 16_667).to_le_bytes());
        }
        bytes.extend((events.len() as u32).to_le_bytes());
        bytes.extend((frames as u32 + 1).to_le_bytes());
        bytes.extend(1u64.to_le_bytes());
        bytes.extend(0u64.to_le_bytes());
        bytes.extend(events);
        bytes
    }

    fn assert_repeated_disk_seek_finishes(recording: Recording) {
        assert!(recording.is_disk_backed());
        let context = egui::Context::default();
        let last = recording.frame_count() - 1;
        let target = last / 2;
        let expected = recording.state_at(target).unwrap();
        let mut studio = Studio::default();
        studio.connect_live_session(1);
        studio.accept_live_snapshot(LiveSnapshot {
            source_frame: None,
            display_updated: true,
            session: 1,
            update: pvd_core::live::SceneUpdate::full(&recording.state_at(last).unwrap()),
            recording: recording.clone(),
            frame: last,
        });
        studio.seek(target);
        let generation = studio.seek_worker.as_ref().unwrap().generation();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            for _ in 0..32 {
                studio.accept_live_snapshot(LiveSnapshot {
                    source_frame: None,
                    display_updated: true,
                    session: 1,
                    update: pvd_core::live::SceneUpdate::default(),
                    recording: recording.clone(),
                    frame: last,
                });
                studio.seek(target);
                assert_eq!(
                    studio.seek_worker.as_ref().unwrap().generation(),
                    generation
                );
            }
            studio.poll_seek(&context);
            if studio.seeking.is_none() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "repeated requests starved the disk seek"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(studio.frame, target);
        assert_eq!(studio.state, expected);

        studio.seek(target);
        let requested = studio.seek_worker.as_ref().unwrap().generation();
        assert!(requested > generation);
        studio.pause_display();
        studio.seek(target);
        let resumed = studio.seek_worker.as_ref().unwrap().generation();
        assert!(resumed > requested);
        studio.return_to_live();
        studio.seek(target);
        let after_live = studio.seek_worker.as_ref().unwrap().generation();
        assert!(after_live > resumed);
        studio.connect_live_session(2);
        studio.accept_live_snapshot(LiveSnapshot {
            source_frame: None,
            display_updated: true,
            session: 2,
            update: pvd_core::live::SceneUpdate::full(&recording.state_at(last).unwrap()),
            recording: recording.clone(),
            frame: last,
        });
        studio.seek(target);
        let reconnected = studio.seek_worker.as_ref().unwrap().generation();
        assert!(reconnected > after_live);
        studio.clear_recording();
        studio.set_recording(recording);
        studio.seek(target);
        assert!(studio.seek_worker.as_ref().unwrap().generation() > reconnected);
        let deadline = Instant::now() + Duration::from_secs(15);
        while studio.seeking.is_some() {
            assert!(
                Instant::now() < deadline,
                "replacement source seek timed out"
            );
            studio.poll_seek(&context);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(studio.frame, target);
        assert_eq!(studio.state, expected);
    }

    #[test]
    fn repeated_disk_seek_survives_live_snapshots_and_source_transitions() {
        assert_repeated_disk_seek_finishes(small_disk_recording());
    }

    #[test]
    fn disk_deltas_require_a_displayed_base_after_superseding_or_cancelling() {
        let recording = Arc::new(small_disk_recording());
        let worker = seeking::Worker::new();
        let wait = || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !worker.has_completed() {
                assert!(Instant::now() < deadline, "disk worker timed out");
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        worker.request(recording.clone(), 2);
        wait();
        worker.request(recording.clone(), 3);
        wait();
        let (_, update) = worker.poll().unwrap().result.unwrap();
        assert!(update.replace_all);
        let mut display = SceneState::default();
        update.apply(&mut display);
        assert_eq!(display, recording.state_at(3).unwrap());
        worker.request(recording.clone(), 4);
        wait();
        let (_, update) = worker.poll().unwrap().result.unwrap();
        assert!(!update.replace_all);
        update.apply(&mut display);
        assert_eq!(display, recording.state_at(4).unwrap());
        worker.request(recording.clone(), 5);
        wait();
        worker.cancel();
        worker.request(recording.clone(), 6);
        wait();
        let (_, update) = worker.poll().unwrap().result.unwrap();
        assert!(update.replace_all);
        update.apply(&mut display);
        assert_eq!(display, recording.state_at(6).unwrap());
    }

    #[test]
    fn disk_seek_latency_consumes_the_playback_interval_instead_of_resetting_it() {
        let mut studio = Studio::default();
        studio.set_recording(small_disk_recording());
        let context = egui::Context::default();
        studio.playing = 1;
        studio.last_step = Instant::now() - Duration::from_millis(100);
        let scheduled = studio.last_step;
        studio.seek(1);
        let deadline = Instant::now() + Duration::from_secs(5);
        while studio.seeking.is_some() {
            assert!(Instant::now() < deadline);
            studio.poll_seek(&context);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(studio.frame, 1);
        assert_eq!(studio.last_step, scheduled);
        studio.pause_display();
        studio.seek(2);
        while studio.seeking.is_some() {
            assert!(Instant::now() < deadline);
            studio.poll_seek(&context);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(studio.last_step > scheduled);
    }

    #[test]
    fn expired_seek_retries_before_new_history_metadata_arrives_but_corruption_stops_playback() {
        let mut studio = Studio::default();
        studio.set_recording(small_disk_recording());
        studio.live_connected = true;
        studio.playing = 1;
        studio.frame = 0;
        assert_eq!(studio.first_frame(), 0);
        studio.handle_seek_failure(0, true, "history resource is no longer retained".into());
        assert_eq!(studio.playing, 1);
        assert_eq!(studio.seeking, Some(0));
        studio.pause_display();
        studio.playing = 1;
        studio.handle_seek_failure(0, false, "history record checksum mismatch".into());
        assert_eq!(studio.playing, 0);
        assert_eq!(studio.seeking, None);
    }

    #[test]
    fn tcp_probe_accepts_expired_forward_rebase_before_metadata_refresh() {
        let mut live = LiveRecording::with_history(pvd_core::live::HistoryConfig {
            disk_budget_bytes: 12 << 10,
            segment_frames: 4,
            min_free_disk_bytes: 0,
            ..Default::default()
        })
        .unwrap();
        live.feed(&disk_frame_stream(0, 32)).unwrap();
        let old = live.snapshot().unwrap();
        let first = old.first_frame();
        let expected = first + 1;
        assert!(old.is_frame_retained(expected));
        let last = old.frame_count() - 1;
        let retained_at_dispatch = playback_tests::RebaseWitness::observe(&old, first, last)
            .unwrap()
            .dispatched(7);
        for frame in 32..512 {
            if !old.is_frame_retained(expected) {
                break;
            }
            live.feed(&disk_frame_stream(frame, 1)[28..]).unwrap();
        }
        assert_eq!(old.first_frame(), first);
        assert!(old.frame_info(expected).is_some());
        assert!(!old.is_frame_retained(expected));
        assert!(old.is_frame_retained(last));
        assert!(
            !retained_at_dispatch
                .accepted(last, 7)
                .unwrap()
                .allows(first, last, 1)
        );
        let next_retained = old
            .frames
            .iter()
            .find(|frame| old.is_frame_retained(frame.index))
            .unwrap()
            .index;
        assert!(next_retained > expected);
        let witness = playback_tests::RebaseWitness::observe(&old, first, next_retained)
            .unwrap()
            .dispatched(8);
        let accepted = witness.accepted(next_retained, 8).unwrap();
        assert!(accepted.allows(first, next_retained, 1));
        assert!(!accepted.allows(first, next_retained, -1));
        assert!(witness.accepted(next_retained, 9).is_none());
        assert!(witness.accepted(next_retained + 1, 8).is_none());
        assert!(playback_tests::RebaseWitness::observe(&old, first, old.frame_count()).is_none());
    }

    #[test]
    fn tcp_probe_rejects_retained_forward_and_reverse_jumps() {
        let recording = small_disk_recording();
        assert!(recording.is_frame_retained(2));
        let witness = playback_tests::RebaseWitness::observe(&recording, 1, 4)
            .unwrap()
            .dispatched(1)
            .accepted(4, 1)
            .unwrap();
        assert!(!witness.allows(1, 4, 1));
        assert!(!witness.allows(4, 1, -1));
        assert!(playback_tests::RebaseWitness::observe(&recording, 1, 2).is_none());
    }

    #[test]
    fn recovery_margin_does_not_change_manual_targets_or_survive_direction_and_live_changes() {
        let recording = small_disk_recording();
        let mut studio = Studio::default();
        studio.connect_live_session(1);
        studio.accept_live_snapshot(LiveSnapshot {
            source_frame: None,
            display_updated: true,
            session: 1,
            update: pvd_core::live::SceneUpdate::full(&recording.state_at(7).unwrap()),
            recording,
            frame: 7,
        });
        studio.pause_display();
        studio.frame = 0;
        studio.playing = 1;
        let now = Instant::now();
        studio
            .playback_recovery
            .begin(playback_recovery::Intent::Recovery, 0, (0, 7), true, now);
        studio
            .playback_recovery
            .finish(0, (3, 10), false, now + Duration::from_secs(1));
        studio.continue_from_retained_start();
        assert_eq!(studio.seeking, Some(6));
        studio.seek(2);
        assert_eq!(studio.seeking, Some(2));
        let context = egui::Context::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        while studio.seeking.is_some() {
            assert!(Instant::now() < deadline);
            studio.poll_seek(&context);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(studio.frame, 2);
        assert_eq!(
            studio.playback_recovery.target(0, 7, true),
            playback_recovery::Decision::Seek(0)
        );
        studio.frame = 0;
        studio.continue_from_retained_start();
        studio.toggle_reverse();
        assert_eq!(studio.playing, -1);
        assert_eq!(studio.seeking, Some(7));
        studio.return_to_live();
        assert!(studio.follow_live);
        assert_eq!(studio.seeking, None);
        assert_eq!(studio.frame, 7);
        assert!(!studio.playback_recovery.waiting());
    }

    #[test]
    fn repeated_expired_loads_stop_without_forcing_live_and_disconnect_removes_margin() {
        let mut studio = Studio::default();
        studio.set_recording(small_disk_recording());
        studio.live_connected = true;
        studio.playing = 1;
        studio.frame = 3;
        let displayed = studio.state.clone();
        let now = Instant::now();
        for _ in 0..3 {
            studio.playback_recovery.begin(
                playback_recovery::Intent::Recovery,
                0,
                (0, 7),
                true,
                now,
            );
            studio
                .playback_recovery
                .finish(0, (3, 10), false, now + Duration::from_secs(1));
        }
        studio.continue_from_retained_start();
        assert_eq!(studio.playing, 0);
        assert!(!studio.follow_live);
        assert_eq!(studio.seeking, None);
        assert_eq!(studio.frame, 3);
        assert_eq!(studio.state, displayed);
        assert!(studio.notices.iter().any(|notice| {
            notice
                .text(Language::English)
                .contains("Increase the history budget")
        }));
        studio
            .playback_recovery
            .begin(playback_recovery::Intent::Recovery, 0, (0, 7), true, now);
        studio
            .playback_recovery
            .finish(0, (3, 10), false, now + Duration::from_secs(1));
        studio.live_connected = false;
        studio.playing = 1;
        studio.continue_from_retained_start();
        assert_eq!(studio.seeking, Some(0));
        studio.pause_display();
        assert_eq!(studio.seeking, None);
        assert_eq!(
            studio.playback_recovery.target(0, 7, true),
            playback_recovery::Decision::Seek(0)
        );
    }

    #[test]
    fn manual_expired_target_stops_exactly_and_unavailable_disconnected_history_does_not_retry() {
        let mut live = LiveRecording::with_history(pvd_core::live::HistoryConfig {
            disk_budget_bytes: 4096,
            segment_frames: 4,
            min_free_disk_bytes: 0,
            ..Default::default()
        })
        .unwrap();
        live.feed(&disk_frame_stream(0, 16)).unwrap();
        let old = live.snapshot().unwrap();
        let target = old.first_frame();
        assert!(old.is_frame_retained(target));
        live.feed(&disk_frame_stream(16, 128)[28..]).unwrap();
        assert!(!old.is_frame_retained(target));
        assert!(
            old.frames
                .iter()
                .all(|frame| !old.is_frame_retained(frame.index))
        );
        let mut studio = Studio::default();
        studio.set_recording(old);
        studio.live_connected = true;
        studio.playing = 1;
        let displayed = studio.frame;
        studio.seek(target);
        let generation = studio.seek_worker.as_ref().unwrap().generation();
        let deadline = Instant::now() + Duration::from_secs(5);
        let context = egui::Context::default();
        while studio.seeking.is_some() {
            assert!(Instant::now() < deadline);
            studio.poll_seek(&context);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(studio.frame, displayed);
        assert_eq!(studio.playing, 0);
        assert_eq!(
            studio.seek_worker.as_ref().unwrap().generation(),
            generation
        );
        studio.playing = 1;
        studio.continue_from_retained_start();
        assert!(studio.playback_recovery.waiting());
        studio.live_connected = false;
        studio.continue_from_retained_start();
        assert_eq!(studio.playing, 0);
        assert!(!studio.follow_live);
        assert!(!studio.playback_recovery.waiting());
    }

    #[test]
    fn final_disk_playback_step_returns_to_live_in_the_completion_poll() {
        let recording = disk_recording(
            3,
            pvd_core::live::HistoryConfig {
                disk_budget_bytes: 4 << 20,
                min_free_disk_bytes: 0,
                ..Default::default()
            },
        );
        let mut studio = Studio::default();
        studio.persist_layout = false;
        studio.connect_live_session(1);
        studio.accept_live_snapshot(LiveSnapshot {
            source_frame: None,
            display_updated: true,
            session: 1,
            update: pvd_core::live::SceneUpdate::full(&recording.state_at(2).unwrap()),
            recording: recording.clone(),
            frame: 2,
        });
        let context = egui::Context::default();
        studio.seek(1);
        let deadline = Instant::now() + Duration::from_secs(5);
        while studio.seeking.is_some() {
            assert!(Instant::now() < deadline);
            studio.poll_seek(&context);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!studio.follow_live);
        assert_eq!(studio.frame, 1);
        let saved_state = studio.state.clone();
        let saved_analysis = studio.recording.as_ref().unwrap().analysis.clone();
        studio.playing = 1;
        studio.last_step = Instant::now() - Duration::from_secs(1);
        studio.poll(&context);
        assert_eq!(studio.frame, 1);
        assert_eq!(studio.seeking, Some(2));
        while !studio.follow_live {
            assert!(Instant::now() < deadline);
            assert_eq!(studio.frame, 1);
            assert_eq!(studio.seeking, Some(2));
            studio.poll(&context);
            std::thread::sleep(Duration::from_millis(1));
        }
        let terminal = |studio: &Studio, pending, count| {
            playback_tests::completed_history_step_at_live_edge(studio, 1, pending, count)
        };
        assert!(terminal(&studio, Some(2), 3));
        assert_eq!(saved_state, recording.state_at(1).unwrap());
        assert_eq!(
            postcard::to_stdvec(&saved_analysis).unwrap(),
            postcard::to_stdvec(&recording.analysis_at(1).unwrap()).unwrap()
        );
        assert!(!terminal(&studio, Some(1), 3));
        assert!(!terminal(&studio, Some(3), 3));
        assert!(!terminal(&studio, None, 3));
        assert!(!terminal(&studio, Some(2), 4));
        studio.follow_live = false;
        assert!(!terminal(&studio, Some(2), 3));
        studio.follow_live = true;
        studio.playing = -1;
        assert!(!terminal(&studio, Some(2), 3));
        studio.playing = 0;
        studio.live_connected = false;
        assert!(!terminal(&studio, Some(2), 3));
        studio.live_connected = true;
        studio.seeking = Some(2);
        assert!(!terminal(&studio, Some(2), 3));
        studio.seeking = None;
        studio.live_frame = 3;
        studio.frame = 3;
        assert!(!terminal(&studio, Some(2), 3));
    }

    #[test]
    fn playback_resumes_at_live_edge_and_restarts_inside_retained_history() {
        let recording = disk_recording(
            80,
            pvd_core::live::HistoryConfig {
                disk_budget_bytes: 4096,
                segment_frames: 4,
                min_free_disk_bytes: 0,
                ..Default::default()
            },
        );
        let first = recording.first_frame();
        assert!(
            first > 0,
            "the paused frame must have expired from disk history"
        );
        let last = recording.frame_count() - 1;
        let mut studio = Studio::default();
        studio.connect_live_session(1);
        studio.accept_live_snapshot(LiveSnapshot {
            source_frame: None,
            display_updated: true,
            session: 1,
            update: pvd_core::live::SceneUpdate::full(&recording.state_at(last).unwrap()),
            recording: recording.clone(),
            frame: last,
        });
        studio.pause_display();
        studio.toggle_playback();
        assert!(
            studio.follow_live,
            "Play at the live edge must resume following"
        );
        studio.pause_display();
        studio.frame = 0;
        studio.toggle_playback();
        assert_eq!(studio.seeking, Some(first));
        assert_eq!(studio.playing, 1);
        studio.pause_display();
        studio.toggle_reverse();
        assert_eq!(studio.seeking, Some(last));
        assert_eq!(studio.playing, -1);
        studio.toggle_reverse();
        assert_eq!(studio.seeking, None);
        assert_eq!(studio.playing, 0);
        studio.frame = first;
        studio.toggle_reverse();
        assert_eq!(studio.seeking, Some(last));
        studio.pause_display();
        studio.live_connected = false;
        studio.frame = last;
        studio.toggle_playback();
        assert_eq!(studio.seeking, Some(first));
        assert_eq!(studio.playing, 1);
        studio.pause_display();
        studio.frame = 0;
        studio.playing = 1;
        studio.last_step = Instant::now() - Duration::from_secs(1);
        studio.poll(&egui::Context::default());
        assert_eq!(studio.playing, 1);
        assert_eq!(studio.seeking, Some(first));
    }

    #[test]
    #[ignore = "Requires official PhysX 3.4.2 output"]
    fn repeated_disk_seek_matches_real_sdk_capture() {
        let path = sdk_baseline_path();
        let loaded = read_recording_on_disk(
            &path,
            File::open(&path).unwrap(),
            &AtomicBool::new(false),
            &AtomicU64::new(0),
            Default::default(),
        )
        .unwrap();
        assert_repeated_disk_seek_finishes(loaded.recording);
    }

    #[test]
    #[ignore = "Requires official PhysX 3.4.2 output"]
    fn disk_file_loader_matches_sdk_replay_and_rejects_incomplete_cache() {
        let path = sdk_baseline_path();
        let cancel = AtomicBool::new(false);
        let progress = AtomicU64::new(0);
        let expected = Recording::from_bytes(&std::fs::read(&path).unwrap()).unwrap();
        let config = pvd_core::live::HistoryConfig {
            segment_frames: 15,
            ..Default::default()
        };
        let loaded = read_recording_on_disk(
            &path,
            File::open(&path).unwrap(),
            &cancel,
            &progress,
            config,
        )
        .unwrap();
        assert!(loaded.bytes.is_none());
        assert_eq!(loaded.source_path.as_deref(), Some(path.as_path()));
        assert!(loaded.clips.as_ref().is_some_and(Result::is_ok));
        assert_eq!(
            progress.load(Ordering::Relaxed),
            std::fs::metadata(&path).unwrap().len()
        );
        for frame in [0, 32, 119] {
            assert_eq!(
                loaded.recording.state_at(frame).unwrap(),
                expected.state_at(frame).unwrap()
            );
        }
        let config = pvd_core::live::HistoryConfig {
            disk_budget_bytes: 4096,
            segment_frames: 1,
            ..Default::default()
        };
        let failure = read_recording_on_disk(
            &path,
            File::open(&path).unwrap(),
            &cancel,
            &progress,
            config,
        );
        assert!(failure.is_err());
        cancel.store(true, Ordering::Relaxed);
        assert!(
            read_recording_on_disk(
                &path,
                File::open(&path).unwrap(),
                &cancel,
                &progress,
                Default::default()
            )
            .is_err()
        );
    }

    #[test]
    #[ignore = "Requires official PhysX 3.4.2 output"]
    fn real_tcp_capture_continues_while_display_is_paused() {
        use std::{io::Write, net::TcpStream};
        let path = sdk_baseline_path();
        let bytes = std::fs::read(path).unwrap();
        let mut decoder = pvd_protocol::Decoder::default();
        let groups = decoder.feed(&bytes).unwrap();
        let first_end = groups
            .iter()
            .position(|group| {
                group
                    .events
                    .iter()
                    .any(|event| matches!(event, pvd_protocol::Event::EndSection { .. }))
            })
            .unwrap();
        let split = groups[first_end + 1].offset as usize;
        let context = egui::Context::default();
        let server = NetworkSession::start(
            ListenConfig {
                address: "127.0.0.1:0".parse().unwrap(),
                ..Default::default()
            },
            context.clone(),
        )
        .unwrap();
        let address = server.address();
        let (continue_tx, continue_rx) = mpsc::channel();
        let producer = std::thread::spawn(move || {
            let mut socket = TcpStream::connect(address).unwrap();
            socket.write_all(&bytes[..split]).unwrap();
            continue_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            socket.write_all(&bytes[split..]).unwrap();
        });
        let mut studio = Studio::default();
        studio.server = Some(server);
        let deadline = Instant::now() + Duration::from_secs(10);
        while studio.count() == 0 {
            studio.poll(&context);
            assert!(Instant::now() < deadline, "first live frame timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
        studio.pause_display();
        let displayed = studio.state.clone();
        let displayed_frame = studio.frame;
        continue_tx.send(()).unwrap();
        while studio.live_connected || studio.count() < 120 {
            studio.poll(&context);
            assert!(Instant::now() < deadline, "live capture did not complete");
            std::thread::sleep(Duration::from_millis(2));
        }
        producer.join().unwrap();
        assert_eq!(studio.frame, displayed_frame);
        assert_eq!(studio.state, displayed);
        assert!(studio.bytes.is_none());
        assert_eq!(studio.count(), 120);
        studio.return_to_live();
        assert_eq!(studio.frame, 119);
        assert_eq!(
            studio.state,
            studio.recording.as_ref().unwrap().state_at(119).unwrap()
        );
        studio.stop_listener();
    }

    #[test]
    #[ignore = "requires an original PVD3 rich container saved from the paired SDK sample"]
    fn native_container_opens_through_desktop_worker_with_visible_limits() {
        let path = std::env::var_os("PVD_NATIVE_RICH_CAPTURE")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../captures/native/rich-paired-native.pxd2")
            });
        let mut studio = Studio::default();
        let context = egui::Context::default();
        studio.open(path.clone(), context.clone());
        let deadline = Instant::now() + Duration::from_secs(10);
        while (studio.job.is_some() || studio.seeking.is_some()) && Instant::now() < deadline {
            studio.poll(&context);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(studio.job.is_none(), "desktop load did not finish");
        assert!(
            studio.seeking.is_none(),
            "first frame did not finish loading"
        );
        assert_eq!(studio.count(), 120, "{:?}", studio.notices);
        assert_eq!(studio.state.render_scene().objects.len(), 6);
        assert_eq!(studio.requested_panel, Some(Panel::Diagnostics));
        assert!(studio.bytes.is_none());
        assert_eq!(studio.capture_path.as_deref(), Some(path.as_path()));
        let bytes = std::fs::read(&path).unwrap();
        assert!(studio.source_identity.as_ref().unwrap().matches(
            *blake3::hash(&bytes).as_bytes(),
            bytes.len() as u64,
            120
        ));
        assert!(studio.recording.as_ref().unwrap().is_disk_backed());
        assert!(!studio.can_save_capture());
        assert!(!studio.can_trim_capture());
        assert!(studio.can_export_analysis());
        assert!(!studio.recording.as_ref().unwrap().diagnostics.is_empty());
        let errors = studio.recording.as_ref().unwrap().untimed_error_events();
        assert_eq!(errors.len(), 1);
        assert_eq!((errors[0].code, errors[0].received_frame), (4, 30));
        assert!(studio.error_rows.is_empty());
        assert!(studio.notices.iter().any(|notice| {
            notice.text(Language::English).contains("limited support")
                && notice.text(Language::Chinese).contains("部分支持")
        }));
        studio.seek(60);
        let deadline = Instant::now() + Duration::from_secs(10);
        while studio.seeking.is_some() && Instant::now() < deadline {
            studio.poll(&context);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(studio.frame, 60);
        assert!(studio.seeking.is_none());
        assert!(
            studio
                .recording
                .as_ref()
                .unwrap()
                .native_end_analysis()
                .is_some()
        );
        studio.plot_performance(context.clone());
        let deadline = Instant::now() + Duration::from_secs(30);
        while studio.curve_job.is_some() && Instant::now() < deadline {
            studio.poll(&context);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(studio.curve_job.is_none());
        assert_eq!(studio.curves.curves.len(), 4);
        for curve in &studio.curves.curves {
            let data = curve.data.as_ref().expect("native full-history curve");
            assert_eq!(data.samples.len(), 120);
            assert!(data.samples.iter().any(|(_, value)| value.is_some()));
        }
        let interval = &studio.curves.curves[0];
        assert_eq!(
            interval.spec.source,
            crate::curve::Source::Performance(pvd_core::frame_metrics::Metric::FrameInterval)
        );
        let samples = &interval
            .data
            .as_ref()
            .expect("native frame interval")
            .samples;
        assert_eq!(samples.len(), 120);
        assert!(samples[0].1.is_none());
        assert!(samples[1..].iter().all(|(_, value)| value.is_some()));
        studio.close();
        assert!(studio.recording.is_none());
        assert!(studio.state.objects.is_empty());
        let cancel = AtomicBool::new(true);
        assert!(read_recording(&path, &cancel, &AtomicU64::new(0), Default::default()).is_err());
    }

    #[test]
    fn recorded_playback_respects_irregular_intervals_in_both_directions() {
        use pvd_protocol::{Endian, Event, EventGroup, Header};
        let mut builder = pvd_core::RecordingBuilder::new(Header {
            version: 1,
            timestamp_numerator: 1_000_000,
            timestamp_denominator: 1,
            flags: 1,
            endian: Endian::Little,
        });
        builder
            .push(EventGroup {
                stream_id: 1,
                timestamp: 0,
                offset: 0,
                events: vec![
                    Event::StringHandle {
                        handle: 1,
                        value: "frame".into(),
                    },
                    Event::EndSection {
                        id: 1,
                        name: 1,
                        timestamp: 100,
                    },
                    Event::EndSection {
                        id: 1,
                        name: 1,
                        timestamp: 116,
                    },
                    Event::EndSection {
                        id: 1,
                        name: 1,
                        timestamp: 166,
                    },
                    Event::EndSection {
                        id: 1,
                        name: 1,
                        timestamp: 165,
                    },
                ],
            })
            .unwrap();
        let recording = builder.finish().unwrap();
        assert_eq!(
            playback_interval(Some(&recording), 0, 1, true, 20.0),
            Duration::from_millis(16)
        );
        assert_eq!(
            playback_interval(Some(&recording), 2, 1, true, 20.0),
            Duration::from_millis(50)
        );
        assert_eq!(
            playback_interval(Some(&recording), 0, 1, false, 20.0),
            Duration::from_millis(50)
        );
        assert_eq!(
            playback_interval(Some(&recording), 2, 3, true, 10.0),
            Duration::from_millis(100)
        );
        assert_eq!(
            playback_interval(Some(&recording), 4, 5, true, 10.0),
            Duration::from_millis(100)
        );
        assert!(playback_interval(None, 0, 1, true, f32::NAN) > Duration::ZERO);
    }

    #[test]
    fn history_directory_defaults_to_working_directory_and_migrates_only_legacy_default() {
        let default = Preferences::default();
        assert_eq!(
            Path::new(&default.history_directory),
            Path::new("pvd-cache").join("history")
        );
        let mut stored = serde_json::to_value(&default).unwrap();
        stored["history_directory"] = std::env::temp_dir()
            .join("pvd-studio-live")
            .to_string_lossy()
            .into_owned()
            .into();
        stored["history_budget_gib"] = 3.into();
        let migrated = Preferences::from_bytes(&serde_json::to_vec(&stored).unwrap());
        assert_eq!(migrated.history_directory, default.history_directory);
        assert_eq!(migrated.history_budget_gib, 3);
        stored["history_directory"] = "D:\\MyCaptures\\cache".into();
        let custom = Preferences::from_bytes(&serde_json::to_vec(&stored).unwrap());
        assert_eq!(custom.history_directory, "D:\\MyCaptures\\cache");
    }

    #[test]
    fn language_preferences_migrate_without_resetting_layout_or_capture_settings() {
        let prefs = Preferences {
            listener: crate::listen_settings::ListenSettings {
                host: "127.0.0.1".into(),
                port: 15425,
            },
            auto_capture: true,
            language: Language::English,
            ..Preferences::default()
        };
        let mut stored = serde_json::to_value(&prefs).unwrap();
        let original_dock = stored["dock"].clone();
        let restored: Preferences = serde_json::from_value(stored.clone()).unwrap();
        assert_eq!(restored.language, Language::English);
        stored.as_object_mut().unwrap().remove("language");
        for value in [None, Some("unsupported-locale")] {
            if let Some(value) = value {
                stored["language"] = value.into();
            }
            let restored: Preferences = serde_json::from_value(stored.clone()).unwrap();
            assert_eq!(restored.language, Language::Chinese);
            assert_eq!(
                restored.listener.address().unwrap(),
                "127.0.0.1:15425".parse().unwrap()
            );
            assert!(restored.auto_capture);
            assert_eq!(serde_json::to_value(restored.dock).unwrap(), original_dock);
        }
    }

    #[test]
    fn receive_storage_preferences_migrate_and_enforce_free_space_reserve() {
        let preferences = Preferences {
            history_budget_gib: 24,
            live_refresh_hz: 90,
            ..Default::default()
        };
        let mut stored = serde_json::to_value(&preferences).unwrap();
        let original_dock = stored["dock"].clone();
        stored.as_object_mut().unwrap().remove("receive_budget_gib");
        stored.as_object_mut().unwrap().remove("min_free_disk_mib");
        let restored = Preferences::from_bytes(&serde_json::to_vec(&stored).unwrap());
        assert_eq!(restored.receive_budget_gib, 0);
        assert_eq!(restored.min_free_disk_mib, 512);
        assert_eq!(restored.history_budget_gib, 24);
        assert_eq!(restored.live_refresh_hz, 90);
        assert_eq!(
            serde_json::to_value(&restored).unwrap()["dock"],
            original_dock
        );
        for (stored_reserve, expected_reserve) in [(0, 512), (128, 512), (2048, 2048)] {
            stored["receive_budget_gib"] = 16.into();
            stored["min_free_disk_mib"] = stored_reserve.into();
            let restored = Preferences::from_bytes(&serde_json::to_vec(&stored).unwrap());
            assert_eq!(restored.receive_budget_gib, 16);
            assert_eq!(restored.min_free_disk_mib, expected_reserve);
            let mut studio = Studio::default();
            studio.prefs = restored;
            assert_eq!(
                studio.history_config().min_free_disk_bytes,
                u64::from(expected_reserve) << 20
            );
        }
    }

    #[test]
    fn sleeping_scope_preferences_preserve_visibility_and_old_settings() {
        let preferences = Preferences {
            live_refresh_hz: 90,
            ..Default::default()
        };
        let mut stored = serde_json::to_value(&preferences).unwrap();
        let original_dock = stored["dock"].clone();
        stored.as_object_mut().unwrap().remove("sleeping_scope");
        for enabled in [false, true] {
            stored["show_sleeping"] = enabled.into();
            let restored = Preferences::from_bytes(&serde_json::to_vec(&stored).unwrap());
            assert_eq!(restored.show_sleeping, enabled);
            assert!(restored.sleeping_scope == ObjectScope::All);
            assert_eq!(restored.live_refresh_hz, 90);
            assert_eq!(
                serde_json::to_value(&restored).unwrap()["dock"],
                original_dock
            );
        }
        for scope in [
            ObjectScope::All,
            ObjectScope::Selected,
            ObjectScope::Unselected,
        ] {
            stored["sleeping_scope"] = serde_json::to_value(scope).unwrap();
            let restored = Preferences::from_bytes(&serde_json::to_vec(&stored).unwrap());
            assert!(restored.sleeping_scope == scope);
            assert!(restored.show_sleeping);
        }
    }

    #[test]
    fn display_color_preferences_restore_defaults_and_custom_values() {
        let mut preferences = Preferences {
            live_refresh_hz: 90,
            show_sleeping: false,
            ..Default::default()
        };
        preferences.display_colors = DisplayColors {
            static_body: [0.1, 0.2, 0.3],
            dynamic_body: [0.2, 0.3, 0.4],
            sleeping_body: [0.3, 0.4, 0.5],
            kinematic_body: [0.4, 0.5, 0.6],
            cloth: [0.5, 0.6, 0.7],
        };
        let mut stored = serde_json::to_value(&preferences).unwrap();
        let original_dock = stored["dock"].clone();
        let restored = Preferences::from_bytes(&serde_json::to_vec(&stored).unwrap());
        assert_eq!(restored.display_colors, preferences.display_colors);
        stored.as_object_mut().unwrap().remove("display_colors");
        let restored = Preferences::from_bytes(&serde_json::to_vec(&stored).unwrap());
        assert_eq!(restored.display_colors, DisplayColors::default());
        assert!(!restored.show_sleeping);
        assert_eq!(restored.live_refresh_hz, 90);
        assert_eq!(
            serde_json::to_value(&restored).unwrap()["dock"],
            original_dock
        );
        stored["display_colors"] = serde_json::json!({"static_body": [1.5, -0.2, 0.25]});
        let restored = Preferences::from_bytes(&serde_json::to_vec(&stored).unwrap());
        assert_eq!(restored.display_colors.static_body, [1.0, 0.0, 0.25]);
        assert_eq!(
            restored.display_colors.dynamic_body,
            DisplayColors::default().dynamic_body
        );
        assert!(!restored.show_sleeping);
        assert_eq!(restored.live_refresh_hz, 90);
    }

    #[test]
    fn display_colors_replace_non_finite_channels_independently() {
        let mut colors = DisplayColors {
            dynamic_body: [f32::NAN, -0.5, 2.0],
            cloth: [0.25, f32::INFINITY, 0.75],
            ..Default::default()
        };
        colors.sanitize();
        assert_eq!(colors.dynamic_body, [0.56, 0.0, 1.0]);
        assert_eq!(colors.cloth, [0.25, 0.74, 0.75]);
    }

    #[test]
    fn trigger_preferences_restore_old_defaults_and_each_scope() {
        let preferences = Preferences {
            show_dynamic: false,
            live_refresh_hz: 90,
            ..Default::default()
        };
        let mut stored = serde_json::to_value(preferences).unwrap();
        let original_dock = stored["dock"].clone();
        stored.as_object_mut().unwrap().remove("show_triggers");
        stored.as_object_mut().unwrap().remove("trigger_scope");
        let restored = Preferences::from_bytes(&serde_json::to_vec(&stored).unwrap());
        assert!(restored.show_triggers);
        assert!(restored.trigger_scope == ObjectScope::All);
        assert!(!restored.show_dynamic);
        assert_eq!(restored.live_refresh_hz, 90);
        assert_eq!(
            serde_json::to_value(&restored).unwrap()["dock"],
            original_dock
        );
        for enabled in [false, true] {
            for scope in [
                ObjectScope::All,
                ObjectScope::Selected,
                ObjectScope::Unselected,
            ] {
                stored["show_triggers"] = enabled.into();
                stored["trigger_scope"] = serde_json::to_value(scope).unwrap();
                let restored = Preferences::from_bytes(&serde_json::to_vec(&stored).unwrap());
                assert_eq!(restored.show_triggers, enabled);
                assert!(restored.trigger_scope == scope);
                assert!(!restored.show_dynamic);
            }
        }
    }

    #[test]
    fn raw_export_rejects_other_formats_before_creating_files() {
        let mut studio = Studio::default();
        studio.bytes.replace(Arc::new(Vec::new()));
        for extension in ["pxd", "pxd2", "ovd", ""] {
            studio.export_path = format!("rejected-capture.{extension}");
            studio.save_capture(egui::Context::default());
            assert!(studio.save_job.is_none());
            let notice = studio.notices.last().unwrap();
            assert!(notice.text(Language::English).contains("Use .pvdstream"));
            assert!(notice.text(Language::Chinese).contains(".pvdstream"));
        }
    }

    #[test]
    fn native_export_is_rejected_before_sdk_stream_file_creation() {
        let mut studio = Studio::default();
        let bytes = pvd_core::native::MAGIC.to_le_bytes().to_vec();
        studio.bytes = Some(Arc::new(bytes.clone()));
        for extension in ["pvdstream", "pxd2", "PXD2"] {
            studio.export_path = format!("rejected-native.{extension}");
            studio.save_capture(egui::Context::default());
            assert!(studio.save_job.is_none());
            assert_eq!(studio.bytes.as_deref().unwrap(), &bytes);
            let notice = studio.notices.last().unwrap();
            assert!(
                notice
                    .text(Language::English)
                    .contains("native PVD3 container")
            );
            assert!(notice.text(Language::Chinese).contains("原版 PVD3 容器"));
        }
    }

    fn sdk_baseline_path() -> PathBuf {
        std::env::var_os("PVD_SDK34_CAPTURE")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../captures/physx-3.4.pvdstream")
            })
    }

    #[test]
    #[ignore = "Requires official PhysX 3.4.2 output"]
    fn repeated_open_close_does_not_restore_cancelled_or_previous_state() {
        let path = sdk_baseline_path();
        let mut studio = Studio::default();
        let context = egui::Context::default();
        for _ in 0..3 {
            studio.open(path.clone(), context.clone());
            let deadline = Instant::now() + Duration::from_secs(5);
            while studio.job.is_some() {
                assert!(Instant::now() < deadline, "recording worker timed out");
                studio.poll(&context);
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(studio.count() > 60);
            studio.seek(60);
            assert!(!studio.state.objects.is_empty());
            studio.close();
            assert!(studio.recording.is_none());
            assert!(studio.bytes.is_none());
            assert!(studio.state.objects.is_empty());
            assert!(studio.live_session.is_none());
        }
        studio.open(path, context.clone());
        studio.close();
        std::thread::sleep(Duration::from_millis(100));
        studio.poll(&context);
        assert!(studio.recording.is_none());
        assert!(studio.job.is_none());
    }
}
