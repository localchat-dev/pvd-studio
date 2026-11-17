use std::{path::PathBuf, sync::atomic::Ordering};

use bevy::{prelude::*, render::render_resource::Extent3d};
use bevy_egui::{EguiContexts, egui};
use egui_dock::{DockArea, TabViewer};
use pvd_core::{Recording, Value};

use crate::{
    file_picker::PickerKind,
    i18n::Language,
    scene::Viewport,
    studio::{ObjectScope, Panel, Preferences, Studio, UiAction},
};

#[cfg(test)]
mod array_format_tests;
mod capture_save;
mod frame_sources;
mod help;
#[cfg(test)]
mod object_search_tests;
mod playback_status;
#[cfg(test)]
mod property_context_tests;
mod settings_menu;
mod source_coordinates;
mod toolbar;
mod typography;
mod viewport_help;
mod viewport_size;
pub(crate) use capture_save::show as recording_save_controls;
pub(crate) use settings_menu::settings_menu;

const ACCENT: egui::Color32 = egui::Color32::from_rgb(82, 200, 213);
const MUTED: egui::Color32 = egui::Color32::from_rgb(166, 177, 191);

macro_rules! localized_format {
    ($language:expr, $english:literal, $chinese:literal $(, $argument:expr)* $(,)?) => {
        match $language {
            Language::English => format!($english, $($argument),*),
            Language::Chinese => format!($chinese, $($argument),*),
        }
    };
}

mod rooms;
mod temporary_recordings;

fn shortcut(context: &egui::Context, key: egui::Key) -> bool {
    !crate::camera_controller::keyboard_input_blocked(context)
        && context.input(|input| {
            !input.modifiers.ctrl
                && !input.modifiers.alt
                && !input.modifiers.command
                && input.key_pressed(key)
        })
}

fn request_scene_sync(context: &egui::Context, previous_revision: u64, revision: u64) {
    // Scene synchronization runs before the UI pass and needs another update to apply its changes.
    if revision != previous_revision {
        context.request_repaint();
    }
}

fn untimed_error_detail(error: &pvd_core::UntimedErrorEvent, language: Language) -> String {
    localized_format!(
        language,
        "Time unknown · Code {} · {} · {}:{} · Index {} · Received after {} completed frames · History event {} · Container 0x{:X} · Generation {}",
        "时间未知 · 错误码 {} · {} · {}:{} · 索引 {} · 收到时已完成 {} 帧 · 历史事件 {} · 容器 0x{:X} · 代际 {}",
        error.code,
        error.message,
        error.file,
        error.line,
        error.index,
        error.received_frame,
        error.history_event,
        error.container_instance,
        error.container_generation,
    )
}

fn initial_untimed_error_detail(
    error: &pvd_core::InitialUntimedError,
    language: Language,
) -> String {
    localized_format!(
        language,
        "Present at start · Code {} · {} · {}:{} · Index {} · Container 0x{:X} · Generation {}",
        "起点已有错误 · 错误码 {} · {} · {}:{} · 索引 {} · 容器 0x{:X} · 代际 {}",
        error.code,
        error.message,
        error.file,
        error.line,
        error.index,
        error.container_instance,
        error.container_generation,
    )
}

fn error_summary(code: u32, message: &str, file: &str, line: u32, language: Language) -> String {
    localized_format!(
        language,
        "Code {} · {} · {}:{}",
        "错误码 {} · {} · {}:{}",
        code,
        message,
        file,
        line,
    )
}

fn initial_errors(ui: &mut egui::Ui, errors: &[pvd_core::InitialUntimedError], language: Language) {
    if errors.is_empty() {
        return;
    }
    ui.separator();
    ui.label(language.text("Errors present at start", "起点已有错误"))
        .on_hover_text(language.text(
            "These records are in the initial snapshot. Their time and receive positions are unknown. Records retain each container's array order; container IDs do not identify Actors.",
            "这些记录来自起点快照，时间和接收位置未知。记录保留各容器内的数组顺序；容器 ID 不代表 Actor。",
        ));
    egui::ScrollArea::vertical()
        .id_salt("initial-errors")
        .max_height(240.0)
        .show_rows(ui, 24.0, errors.len(), |ui, range| {
            for row in range {
                let error = &errors[row];
                let detail = initial_untimed_error_detail(error, language);
                let summary = error_summary(
                    error.code,
                    &error.message,
                    &error.file,
                    error.line,
                    language,
                );
                ui.horizontal(|ui| {
                    if ui.small_button(language.text("Copy", "复制")).clicked() {
                        ui.ctx().copy_text(detail.clone());
                    }
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(summary)
                                .color(egui::Color32::from_rgb(239, 148, 137)),
                        )
                        .truncate(),
                    )
                    .on_hover_text(detail);
                });
            }
        });
}

fn unframed_error_position(frame_count: usize, language: Language) -> &'static str {
    if frame_count == 0 {
        language.text("No completed frames", "尚无完成帧")
    } else {
        language.text("After final frame", "末帧之后")
    }
}

fn object_scope(ui: &mut egui::Ui, id: &str, value: &mut ObjectScope, language: Language) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(value.label(language))
        .width(100.0)
        .show_ui(ui, |ui| {
            for scope in [
                ObjectScope::All,
                ObjectScope::Selected,
                ObjectScope::Unselected,
            ] {
                ui.selectable_value(value, scope, scope.label(language));
            }
        });
}

fn display_color(ui: &mut egui::Ui, label: &str, color: &mut [f32; 3]) {
    ui.horizontal(|ui| {
        let mut srgb = color.map(|channel| (channel * 255.0).round() as u8);
        if ui.color_edit_button_srgb(&mut srgb).changed() {
            *color = srgb.map(|channel| f32::from(channel) / 255.0);
        }
        ui.label(label);
    });
}

fn dock_translations(language: Language) -> egui_dock::Translations {
    use egui_dock::{LeafTranslations, TabContextMenuTranslations, Translations};

    Translations {
        tab_context_menu: TabContextMenuTranslations {
            close_button: language.text("Close", "关闭").into(),
            eject_button: language.text("Float panel", "浮动面板").into(),
        },
        leaf: LeafTranslations {
            close_button_disabled_tooltip: language
                .text(
                    "This group contains panels that cannot be closed.",
                    "此分组包含不可关闭的面板。",
                )
                .into(),
            close_all_button: language.text("Close floating group", "关闭浮动分组").into(),
            close_all_button_menu_hint: language
                .text(
                    "Right-click to close this floating group.",
                    "右键单击可关闭此浮动分组。",
                )
                .into(),
            close_all_button_modifier_hint: language
                .text(
                    "Hold Shift to close this floating group.",
                    "按住 Shift 可关闭此浮动分组。",
                )
                .into(),
            close_all_button_modifier_menu_hint: language
                .text(
                    "Hold Shift or right-click to close this floating group.",
                    "按住 Shift 或右键单击可关闭此浮动分组。",
                )
                .into(),
            close_all_button_disabled_tooltip: language
                .text(
                    "This floating group contains panels that cannot be closed.",
                    "此浮动分组包含不可关闭的面板。",
                )
                .into(),
            minimize_button: language
                .text("Collapse floating group", "折叠浮动分组")
                .into(),
            minimize_button_menu_hint: language
                .text(
                    "Right-click to collapse this floating group.",
                    "右键单击可折叠此浮动分组。",
                )
                .into(),
            minimize_button_modifier_hint: language
                .text(
                    "Hold Shift to collapse this floating group.",
                    "按住 Shift 可折叠此浮动分组。",
                )
                .into(),
            minimize_button_modifier_menu_hint: language
                .text(
                    "Hold Shift or right-click to collapse this floating group.",
                    "按住 Shift 或右键单击可折叠此浮动分组。",
                )
                .into(),
        },
    }
}

#[derive(Clone)]
struct PropertyMenuTarget {
    source_epoch: u64,
    object: u64,
    generation: u32,
    property: String,
    index: Option<usize>,
    value_type: std::mem::Discriminant<Value>,
}

impl PropertyMenuTarget {
    fn new(
        source_epoch: u64,
        object: &pvd_core::Object,
        property: &str,
        index: Option<usize>,
        value: &Value,
    ) -> Self {
        Self {
            source_epoch,
            object: object.id,
            generation: object.generation,
            property: property.into(),
            index,
            value_type: std::mem::discriminant(value),
        }
    }

    fn value<'a>(&self, source_epoch: u64, object: &'a pvd_core::Object) -> Option<&'a Value> {
        if self.source_epoch != source_epoch
            || self.object != object.id
            || self.generation != object.generation
        {
            return None;
        }
        let value = object.properties.get(&self.property)?;
        let value = if let Some(index) = self.index {
            let Value::Array(values) = value else {
                return None;
            };
            values.get(index)?
        } else {
            value
        };
        (std::mem::discriminant(value) == self.value_type).then_some(value)
    }

    fn label(&self) -> String {
        match self.index {
            Some(index) => format!("{}[{index}]", self.property),
            None => self.property.clone(),
        }
    }
}

fn property_context_requested(ui: &egui::Ui, rect: egui::Rect) -> bool {
    ui.input(|input| input.focused && input.pointer.button_clicked(egui::PointerButton::Secondary))
        && ui.is_enabled()
        && ui.rect_contains_pointer(rect)
}

fn property_value_row(
    ui: &mut egui::Ui,
    controls: impl FnOnce(&mut egui::Ui),
    value: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let height = ui.spacing().interact_size.y;
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), height),
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            controls(ui);
            let remaining = ui.available_width().max(0.0);
            if remaining > 0.0 {
                ui.allocate_ui_with_layout(
                    egui::vec2(remaining, height),
                    egui::Layout::left_to_right(egui::Align::Center),
                    value,
                );
            }
        },
    )
    .response
}

fn property_f32(value: f32) -> String {
    if value.abs() >= 1e6 || (value.abs() > 0.0 && value.abs() < 1e-4) {
        format!("{value:.6e}")
    } else {
        format!("{value:.6}")
    }
}

fn property_components(values: &[f32]) -> String {
    let values: Vec<_> = values.iter().map(|value| property_f32(*value)).collect();
    format!("[{}]", values.join(", "))
}

fn property_text(
    recording: Option<&pvd_core::Recording>,
    kind: &str,
    property: &str,
    value: &Value,
) -> String {
    let scalar = match value {
        Value::U64(value) => u32::try_from(*value).ok(),
        Value::I64(value) => u32::try_from(*value).ok(),
        _ => None,
    };
    if let (Some(recording), Some(scalar)) = (recording, scalar) {
        let mut kind = kind;
        for _ in 0..32 {
            let Some(class) = recording.classes.get(kind) else {
                break;
            };
            if let Some(definition) = class.properties.get(property) {
                if let Some((name, _)) = definition
                    .named_values
                    .iter()
                    .find(|(_, value)| *value == scalar)
                {
                    return format!("{name} ({scalar})");
                }
                if definition.semantic == "Bitflag" {
                    let names: Vec<_> = definition
                        .named_values
                        .iter()
                        .filter(|(_, flag)| *flag != 0 && scalar & *flag == *flag)
                        .map(|(name, _)| name.as_str())
                        .collect();
                    if !names.is_empty() {
                        return format!("{} (0x{scalar:x})", names.join(" | "));
                    }
                }
                break;
            }
            let Some(parent) = &class.parent else {
                break;
            };
            kind = parent;
        }
    }
    match value {
        Value::F32(value) => property_f32(*value),
        Value::F64(value) if value.abs() >= 1e6 || (value.abs() > 0.0 && value.abs() < 1e-4) => {
            format!("{value:.6e}")
        }
        Value::Vec3(values) => property_components(values),
        Value::Quat(values) => property_components(values),
        Value::Transform { position, rotation } => format!(
            "p {} · q {}",
            property_components(position),
            property_components(rotation)
        ),
        _ => value.to_string(),
    }
}

fn copy_property(context: &egui::Context, value: &Value) -> Result<(), crate::i18n::Message> {
    let text = crate::property_copy::text(value).map_err(|error| {
        let chinese = match error {
            crate::property_copy::CopyError::Bytes => "属性超过 1 MiB 复制上限，未写入剪贴板。",
            crate::property_copy::CopyError::Depth => "属性超过 64 层嵌套上限，未写入剪贴板。",
            crate::property_copy::CopyError::Nodes => "属性超过 100000 项复制上限，未写入剪贴板。",
        };
        crate::i18n::Message::new(format!("{error}; clipboard was not changed."), chinese)
    })?;
    context.copy_text(text);
    Ok(())
}

pub fn prepare(mut contexts: EguiContexts, mut studio: ResMut<Studio>) -> Result {
    let context = match contexts.ctx_mut() {
        Ok(context) => context,
        Err(bevy::ecs::query::QuerySingleError::NoEntities(_)) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !studio.initialized {
        configure(context);
        studio.initialized = true;
        if !studio.path_input.is_empty() {
            let path = PathBuf::from(&studio.path_input);
            studio.open(path, context.clone());
        }
    }
    studio.apply_ui_actions(context);
    studio.update_analysis_demand();
    studio.poll(context);
    if crate::profile::sync_session(context, studio.profile_view_epoch) {
        studio.memory_panel.reset_source();
    }
    Ok(())
}

pub fn draw(
    mut contexts: EguiContexts,
    mut studio: ResMut<Studio>,
    mut navmesh: ResMut<crate::navmesh::NavMeshOverlay>,
    mut viewport: ResMut<Viewport>,
    mut images: ResMut<Assets<Image>>,
) -> Result {
    let context = contexts.ctx_mut()?;
    let previous_revision = studio.revision;
    crate::camera_controller::begin_frame(context);
    let clip_modal = crate::clips::show_pending_action(context, &mut studio);
    let modal = clip_modal || help::show(context, &studio);
    let language = studio.prefs.language;
    if !modal
        && let Some(path) = context.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .find_map(|file| file.path.clone())
        })
    {
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("bin"))
        {
            navmesh.open = true;
            navmesh.request_import(path, &studio, Some(context.clone()));
        } else {
            studio.request_ui_action(context, "dropped-capture", UiAction::Open(path));
        }
    }
    if !modal && !crate::camera_controller::keyboard_input_blocked(context) {
        if shortcut(context, egui::Key::Space) && studio.count() > 0 {
            studio.request_ui_action(context, "keyboard-playback", UiAction::TogglePlayback);
        }
        if shortcut(context, egui::Key::ArrowRight) {
            studio.request_ui_action(context, "keyboard-next", UiAction::Step(1));
        }
        if shortcut(context, egui::Key::ArrowLeft) {
            studio.request_ui_action(context, "keyboard-previous", UiAction::Step(-1));
        }
        if shortcut(context, egui::Key::F) {
            studio.focus_selected = true;
        }
    }
    egui::TopBottomPanel::top("main-toolbar").show(context, |ui| {
        if modal {
            ui.disable();
        }
        toolbar::show(ui, &mut studio, &mut navmesh);
    });
    if !modal {
        crate::navmesh::window(context, &mut navmesh, &studio);
    }
    egui::TopBottomPanel::bottom("status").show(context, |ui| {
        if modal {
            ui.disable();
        }
        ui.horizontal(|ui| {
            if let Some(job) = &studio.job {
                ui.spinner();
                ui.label(localized_format!(
                    language,
                    "Reading / indexing · {:.1} MiB",
                    "读取 / 索引中 · {:.1} MiB",
                    job.progress.load(Ordering::Relaxed) as f64 / 1048576.0
                ));
                if ui.button(language.text("Cancel", "取消")).clicked()
                    && let Some(job) = studio.job.take()
                {
                    job.cancelled.store(true, Ordering::Relaxed);
                }
                context.request_repaint();
            } else {
                let notice = studio
                    .notices
                    .last()
                    .map(|notice| notice.text(language))
                    .unwrap_or(language.text("Ready", "就绪"));
                ui.add_sized(
                    [(ui.available_width() - 215.0).max(20.0), 16.0],
                    egui::Label::new(egui::RichText::new(notice).small().color(MUTED)).truncate(),
                )
                .on_hover_text(notice);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(localized_format!(
                    language,
                    "{} objects   ·   {} frames",
                    "{} 个对象   ·   {} 帧",
                    studio.state.objects.len(),
                    studio.count().saturating_sub(studio.first_frame())
                ));
            });
        });
    });
    let mut dock = studio.prefs.dock.clone();
    dock.translations = dock_translations(studio.prefs.language);
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show(context, |ui| {
            if modal {
                ui.disable();
            }
            let mut viewer = Viewer {
                studio: &mut studio,
                viewport: &mut viewport,
                images: &mut images,
            };
            DockArea::new(&mut dock)
                .show_close_buttons(false)
                .show_leaf_close_all_buttons(false)
                .style(egui_dock::Style::from_egui(ui.style().as_ref()))
                .show_inside(ui, &mut viewer);
        });
    match Preferences::validate_dock(&dock) {
        Ok(()) => studio.prefs.dock = dock,
        Err(error) => studio.notice(crate::i18n::Message::detail(
            "Dock layout update rejected",
            "停靠布局更新无效，已保留有效布局",
            error,
        )),
    }
    if let Some(panel) = studio.requested_panel.take() {
        studio.prefs.show_panel(panel);
        context.request_repaint();
    }
    request_scene_sync(context, previous_revision, studio.revision);
    Ok(())
}

pub(crate) fn configure(context: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    let text = egui::Color32::from_rgb(214, 220, 228);
    let border = egui::Stroke::new(1.0, egui::Color32::from_rgb(43, 51, 62));
    visuals.panel_fill = egui::Color32::from_rgb(23, 28, 35);
    visuals.window_fill = egui::Color32::from_rgb(28, 34, 42);
    visuals.extreme_bg_color = egui::Color32::from_rgb(17, 21, 27);
    visuals.faint_bg_color = egui::Color32::from_rgb(31, 37, 46);
    visuals.weak_text_color = Some(MUTED);
    visuals.selection.bg_fill = egui::Color32::from_rgb(32, 70, 80);
    visuals.selection.stroke = egui::Stroke::new(1.0, ACCENT);
    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
    ] {
        widget.fg_stroke = egui::Stroke::new(1.0, text);
        widget.bg_stroke = border;
    }
    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(34, 42, 53);
    visuals.widgets.inactive.weak_bg_fill = egui::Color32::from_rgb(34, 42, 53);
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(46, 57, 70);
    visuals.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(46, 57, 70);
    visuals.widgets.active.bg_fill = egui::Color32::from_rgb(38, 77, 88);
    visuals.widgets.active.weak_bg_fill = egui::Color32::from_rgb(38, 77, 88);
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, ACCENT);
    context.set_visuals(visuals);
    context.style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 4.0);
        style.spacing.button_padding = egui::vec2(8.0, 4.0);
        style.spacing.interact_size = egui::vec2(28.0, 24.0);
    });
    typography::configure(context);
}

fn receive_status(ui: &mut egui::Ui, studio: &Studio) -> Option<egui::Response> {
    let language = studio.prefs.language;
    let summary = studio.receive_summary(language)?;
    Some(
        ui.add(
            egui::Label::new(egui::RichText::new(summary).color(receive_status_color(studio)))
                .truncate(),
        )
        .on_hover_text(studio.receive_detail(language)),
    )
}

fn receive_status_color(studio: &Studio) -> egui::Color32 {
    let failed = studio
        .receive_status
        .as_ref()
        .is_some_and(|status| status.last_failure.is_some());
    if failed {
        egui::Color32::from_rgb(239, 126, 126)
    } else if studio.receive_backlog_warning() {
        egui::Color32::from_rgb(230, 187, 98)
    } else {
        MUTED
    }
}

fn receive_buffer_status(ui: &mut egui::Ui, studio: &Studio) -> Option<egui::Response> {
    let language = studio.prefs.language;
    let summary = studio.receive_disk_summary(language)?;
    Some(
        ui.add(
            egui::Label::new(egui::RichText::new(summary).color(receive_status_color(studio)))
                .truncate(),
        )
        .on_hover_text(studio.receive_detail(language)),
    )
}

fn receive_storage_settings(ui: &mut egui::Ui, preferences: &mut Preferences) {
    let language = preferences.language;
    ui.separator();
    ui.strong(language.text("Receive buffer", "接收缓存"));
    ui.horizontal_wrapped(|ui| {
        ui.label(language.text("Capacity", "容量上限"));
        ui.add(
            egui::DragValue::new(&mut preferences.receive_budget_gib)
                .range(0..=16384)
                .suffix(" GiB"),
        ).on_hover_text(language.text(
            "0 means unlimited, subject to the free-space reserve. Reaching a quota or a disk write failure stops the connection.",
            "0 表示不设上限，仍受磁盘保留空间限制。达到接收上限或无法写盘时会断开连接。",
        ));
    });
    ui.horizontal_wrapped(|ui| {
        ui.label(language.text("Free-space reserve", "保留空间"));
        ui.add(
            egui::DragValue::new(&mut preferences.min_free_disk_mib)
                .range(512..=1_048_576)
                .suffix(" MiB"),
        ).on_hover_text(language.text(
            "Minimum free disk space for reception and playback history. Reception stops when the reserve is reached.",
            "接收缓存和回放历史共用的磁盘剩余空间底线。剩余空间不足时停止接收。",
        ));
    });
}

fn live_diagnostic_json(
    profile: &crate::network::LiveProfileSnapshot,
    studio: &Studio,
    shapes: usize,
) -> Result<String, serde_json::Error> {
    let display = serde_json::json!({
        "session": studio.live_session,
        "frame": studio.frame,
        "recording_frame_count": studio.count(),
        "live_updates_per_second": studio.live_updates_per_second(),
        "follow_live": studio.follow_live,
        "objects": studio.state.objects.len(),
        "shapes": shapes,
    });
    let history = studio
        .timeline_recording()
        .and_then(Recording::disk_usage)
        .map(|stats| {
            serde_json::json!({
                "disk_bytes": stats.disk_bytes,
                "budget_bytes": stats.budget_bytes,
                "first_frame": stats.first_frame,
                "retained_frames": stats.retained_frames,
                "segments": stats.segments,
                "warning": stats.warning,
            })
        });
    serde_json::to_string_pretty(
        &serde_json::json!({ "profile": profile, "display": display, "history": history }),
    )
}

fn live_profile_diagnostics(
    ui: &mut egui::Ui,
    studio: &mut Studio,
    shapes: usize,
) -> egui::Response {
    let language = studio.prefs.language;
    ui.vertical(|ui| {
        ui.label(egui::RichText::new(language.text("LIVE DECODER PROFILING", "实时解码耗时诊断")).small().color(MUTED));
        if !studio.live_profile_enabled {
            ui.label(language.text(
                "Disabled. Start with --live-profile to record decoder timings.",
                "未启用。使用 --live-profile 启动可记录解码耗时。",
            ));
            return;
        }
        ui.label(language.text(
            "Enabled for this launch. Restart without --live-profile to disable. Timing collection adds overhead.",
            "本次启动已开启。重新启动时移除 --live-profile 可关闭；采样本身会增加开销。",
        ));
        let snapshot = studio.server.as_ref().and_then(crate::network::NetworkSession::profile_snapshot)
            .or_else(|| studio.last_live_profile.clone());
        let Some(snapshot) = snapshot else {
            ui.label(language.text("Waiting for a connection to decode.", "等待连接进入解码。"));
            return;
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(localized_format!(
                language,
                "Session {} · {:.1} s · Decoder input {:.1} MiB · {}",
                "会话 {} · {:.1} 秒 · 解码器输入 {:.1} MiB · {}",
                snapshot.session,
                snapshot.elapsed_seconds,
                snapshot.decoder_input_bytes as f64 / (1024.0 * 1024.0),
                if snapshot.capture_failed { language.text("Capture failed", "捕获失败") }
                else if snapshot.completed { language.text("Completed", "已结束") }
                else { language.text("In progress", "进行中") },
            ));
            if ui.button(language.text("Copy diagnostic JSON", "复制诊断 JSON")).clicked() {
                match live_diagnostic_json(&snapshot, studio, shapes) {
                    Ok(json) => ui.ctx().copy_text(json),
                    Err(error) => studio.notice(crate::i18n::Message::detail("Diagnostic export failed", "诊断导出失败", error)),
                }
            }
        });
        ui.label(language.text(
            "JSON contains session counters, stage timings and display counts, without captured objects or property values. Elapsed time includes waiting; stage times are nanoseconds, and inclusive stages overlap. Live counters are sampled independently. Receive backlog belongs to the listener; received_bytes is unknown when its session differs from the decoder.",
            "JSON 仅包含会话计数、各阶段耗时和显示数量，不包含捕获对象或属性值。总时长包含等待；阶段耗时单位为纳秒，包含子阶段的时间不能相加。运行中的计数分别采样。接收积压属于监听器；接收会话与解码会话不同时，received_bytes 为未知。",
        ));
        ui.label(language.text(
            "These timings cover the core decoder, not Bevy rendering or GPU work. Display frames are zero-based; update counts measure consumed scene snapshots, not rendered FPS. Objects and shapes describe the displayed state and cached viewport, which may differ by one UI pass.",
            "耗时覆盖核心解码，不包含 Bevy 渲染或 GPU 工作。显示帧号从 0 开始；更新次数统计已消费的场景快照，不是渲染 FPS。对象与形状数分别来自显示状态和视口缓存，可能相差一次界面更新。",
        ));
    }).response
}

fn activity_counts(ui: &mut egui::Ui, studio: &Studio) -> egui::Response {
    let language = studio.prefs.language;
    let counts = studio.activity.summary_scoped(&studio.room_scope);
    let sdk = |value: Option<u64>| value.map_or_else(|| "—".to_owned(), |value| value.to_string());
    let mut summary = localized_format!(
        language,
        "Bodies {} · Awake {} · Sleeping {} · Kinematic {} · Static {}",
        "刚体 {} · 唤醒 {} · 休眠 {} · 运动学 {} · 静态 {}",
        counts.total_actors,
        counts.awake_dynamic,
        counts.sleeping_dynamic,
        counts.kinematic,
        counts.static_actors,
    );
    if counts.unknown > 0 {
        summary.push_str(&localized_format!(
            language,
            " · Unknown {}",
            " · 未知 {}",
            counts.unknown
        ));
    }
    let detail = localized_format!(
        language,
        "Displayed frame {}. Counts follow the selected scene, independent of category visibility. Articulation links: {}. Cloth and particles are excluded.\nAwake means a non-kinematic dynamic body with IsSleeping=false. It is separate from SDK activity statistics.\nSDK active bodies: dynamic {}, kinematic {}, total {}. Reporting scenes: {}/{}. A dash means unknown. Each scene uses its last reported value, which may refer to a different simulation time.",
        "当前显示帧 {}，统计所选场景的刚体，不受类别显示开关影响；包含 {} 个关节体 Link，不含布料和粒子。\n唤醒表示 IsSleeping=false 的非运动学动态刚体，与 SDK 活动数分别统计。\nSDK 活动数：动态 {}，运动学 {}，合计 {}。已上报场景 {}/{}；— 表示未知。采用各场景最后上报的值，不保证来自同一仿真时刻。",
        studio.frame,
        counts.articulation_links,
        sdk(counts.sdk_active_dynamic),
        sdk(counts.sdk_active_kinematic),
        sdk(counts.sdk_active_total()),
        counts.sdk_reporting_scenes,
        counts.sdk_scene_count,
    );
    ui.add(egui::Label::new(egui::RichText::new(summary).small().color(MUTED)).truncate())
        .on_hover_text(detail)
}

fn playback(ui: &mut egui::Ui, studio: &mut Studio) {
    let language = studio.prefs.language;
    let (first, last) = studio.playback_ordinal_bounds().unwrap_or((0, 0));
    let (first_index, last_index) = studio.playback_bounds().unwrap_or((0, 0));
    let count = last.saturating_add(1);
    let history = studio.has_history() && studio.playback_bounds().is_some();
    ui.add_enabled_ui(studio.count() > 0 || (studio.viewing_temporary_recording() && studio.live_connected), |ui| {
        if studio.live_session.is_some() && playback_status::live_button(ui, studio)
            .on_hover_text(language.text("Follow the newest decoded frame; it may lag behind TCP reception. Pausing or seeking only pauses this view; reception and history continue.", "跟随最新解码帧，可能落后于 TCP 接收进度。暂停或跳帧只暂停显示，数据接收和历史存储继续。"))
            .clicked() {
            studio.request_ui_action(ui.ctx(), "playback-live", UiAction::ReturnLive);
        }
        if ui
            .add_enabled(history, egui::Button::new("|<"))
            .on_hover_text(language.text("First frame", "首帧"))
            .clicked()
        {
            studio.request_ui_action(ui.ctx(), "playback-first", UiAction::PauseAndSeek(first_index));
        }
        if ui
            .add_enabled(history, egui::Button::new("<"))
            .on_hover_text(language.text("Previous frame · Left", "上一帧 · 左方向键"))
            .clicked()
        {
            studio.request_ui_action(ui.ctx(), "playback-previous", UiAction::Step(-1));
        }
        if ui
            .add_enabled_ui(history, |ui| ui.selectable_label(studio.playing == -1, language.text("◀ Reverse", "◀ 倒放"))).inner
            .on_disabled_hover_text(studio.history_unavailable_detail(language))
            .clicked()
        {
            studio.request_ui_action(ui.ctx(), "playback-reverse", UiAction::ToggleReverse);
        }
        if ui
            .selectable_label(
                studio.playing == 1 || studio.follow_live,
                if studio.playing == 0 && !studio.follow_live {
                    language.text("▶ Play", "▶ 播放")
                } else {
                    language.text("Ⅱ Pause", "Ⅱ 暂停")
                },
            )
            .on_hover_text(language.text("Space", "空格键"))
            .clicked()
        {
            studio.request_ui_action(ui.ctx(), "playback-toggle", UiAction::TogglePlayback);
        }
        if ui
            .add_enabled(history, egui::Button::new(">"))
            .on_hover_text(language.text("Next frame · Right", "下一帧 · 右方向键"))
            .clicked()
        {
            studio.request_ui_action(ui.ctx(), "playback-next", UiAction::Step(1));
        }
        if ui
            .add_enabled(history, egui::Button::new(">|"))
            .on_hover_text(language.text("Last frame", "末帧"))
            .clicked()
        {
            studio.request_ui_action(ui.ctx(), "playback-last", UiAction::PauseAndSeek(last_index));
        }
        let mut frame = studio.frame_ordinal(studio.frame);
        ui.label(language.text("Frame", "帧"));
        if ui
            .add_enabled(history, egui::DragValue::new(&mut frame).range(first..=count.saturating_sub(1)).clamp_existing_to_range(false))
            .on_hover_text(localized_format!(language, "Displayed frame; retained history: {first}–{}", "实际显示帧；可回放历史：{first}–{}", count.saturating_sub(1)))
            .changed()
            && let Some(target) = studio.frame_at_ordinal(frame)
        {
            studio.request_ui_action(ui.ctx(), "playback-frame", UiAction::PauseAndSeek(target));
        }
        ui.label(format!("/ {}", count.saturating_sub(1)));
        if let Some(index) = studio.seeking {
            let frame = studio.frame_ordinal(index);
            ui.spinner();
            ui.label(localized_format!(language, "Loading frame {frame}", "正在读取第 {frame} 帧"));
            if ui.small_button(language.text("Cancel", "取消")).clicked() { studio.request_ui_action(ui.ctx(), "playback-cancel", UiAction::Pause); }
        }
        if ui.checkbox(&mut studio.recorded_timing, language.text("Recorded timing", "录制速度"))
            .on_hover_text(language.text("Use recorded SDK frame timestamps in either direction. This is capture time, not a fixed simulation timestep.", "正放和倒放均使用录制的 SDK 帧时间戳间隔；这是捕获时序，不是固定仿真步长。"))
            .changed() { studio.last_step = std::time::Instant::now(); }
        if ui.add_enabled(!studio.recorded_timing,
            egui::DragValue::new(&mut studio.playback_fps)
                .range(1.0..=240.0)
                .suffix(language.text(" fps", " 帧/秒")),
        )
            .on_hover_text(language.text("Lower the frame rate for slow playback. At 1 fps, playback advances one frame per second; Left/Right steps manually.", "调低帧率可慢放。1 帧/秒表示每秒前进一帧；左右方向键可手动逐帧。"))
            .on_disabled_hover_text(language.text("Turn off Recorded timing to set a playback frame rate.", "取消“录制速度”后可设置播放帧率。"))
            .changed() { studio.last_step = std::time::Instant::now(); }
    });
    if studio.live_session.is_some() && !history {
        ui.colored_label(
            egui::Color32::from_rgb(230, 187, 98),
            language.text("History unavailable", "历史回放不可用"),
        )
        .on_hover_text(studio.history_unavailable_detail(language));
    }
}

struct Viewer<'a> {
    studio: &'a mut Studio,
    viewport: &'a mut Viewport,
    images: &'a mut Assets<Image>,
}

impl TabViewer for Viewer<'_> {
    type Tab = Panel;
    fn id(&mut self, tab: &mut Panel) -> egui::Id {
        let key = match tab {
            Panel::Objects => "objects",
            Panel::Viewport => "viewport",
            Panel::Inspector => "inspector",
            Panel::Timeline => "timeline",
            Panel::Clips => "clips",
            Panel::Performance => "performance",
            Panel::Memory => "memory",
            Panel::Curves => "curves",
            Panel::Diagnostics => "diagnostics",
            Panel::Display => "display",
        };
        egui::Id::new(("studio-panel", key))
    }
    fn title(&mut self, tab: &mut Panel) -> egui::WidgetText {
        tab.label(self.studio.prefs.language).into()
    }
    fn is_closeable(&self, tab: &Panel) -> bool {
        tab.is_closeable()
    }
    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Panel) {
        match tab {
            Panel::Objects => self.objects(ui),
            Panel::Viewport => self.viewport(ui),
            Panel::Inspector => self.inspector(ui),
            Panel::Timeline => {
                egui::ScrollArea::vertical()
                    .id_salt("timeline-panel")
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.timeline(ui));
            }
            Panel::Clips => crate::clips::show(ui, self.studio),
            Panel::Diagnostics => self.diagnostics(ui),
            Panel::Display => self.display(ui),
            Panel::Performance => self.performance(ui),
            Panel::Memory => self.memory(ui),
            Panel::Curves => self.curves(ui),
        }
    }
}

impl Viewer<'_> {
    fn objects(&mut self, ui: &mut egui::Ui) {
        let language = self.studio.prefs.language;
        temporary_recordings::show(ui, self.studio);
        rooms::show(ui, self.studio);
        ui.add_space(7.0);
        let search = ui.add(
            egui::TextEdit::singleline(&mut self.studio.filter)
                .hint_text(language.text("Search name, type or ID", "搜索名称、类型或 ID"))
                .desired_width(f32::INFINITY),
        );
        if search.lost_focus()
            && ui.input(|input| {
                input.focused
                    && input.modifiers == egui::Modifiers::NONE
                    && input.key_pressed(egui::Key::Enter)
                    && !input
                        .events
                        .iter()
                        .any(|event| matches!(event, egui::Event::Ime(_)))
            })
            && let Some(id) = crate::tree::exact_object_id_scoped(
                &self.studio.state,
                &self.studio.filter,
                &self.studio.room_scope,
            )
        {
            self.studio.select_object(Some(id), false);
        }
        ui.add_space(5.0);
        self.studio.tree.update_scoped(
            &self.studio.state,
            self.studio.revision,
            &self.studio.filter,
            language,
            &self.studio.room_scope,
        );
        if self.studio.state.objects.is_empty() {
            ui.add_space(18.0);
            ui.label(egui::RichText::new(language.text("No objects", "暂无对象")).strong());
            ui.label(
                egui::RichText::new(language.text(
                    "Open a capture or connect a PhysX application.",
                    "打开录制或连接 PhysX 应用程序。",
                ))
                .color(MUTED),
            );
        }
        let selected = self.studio.selected_ids();
        let tree = &mut self.studio.tree;
        let mut selection = None;
        let mut toggle = None;
        egui::ScrollArea::vertical()
            .id_salt("object-tree")
            .show_rows(ui, 40.0, tree.rows.len(), |ui, range| {
                for index in range {
                    let row = &tree.rows[index];
                    ui.horizontal(|ui| {
                        ui.add_space(row.depth.min(8) as f32 * 13.0);
                        if row.children {
                            let closed =
                                tree.collapsed.contains(&(row.id.unwrap(), row.generation));
                            let (rect, response) = ui
                                .allocate_exact_size(egui::vec2(15.0, 30.0), egui::Sense::click());
                            let center = rect.center();
                            let points = if closed {
                                vec![
                                    center + egui::vec2(-3.0, -4.0),
                                    center + egui::vec2(3.0, 0.0),
                                    center + egui::vec2(-3.0, 4.0),
                                ]
                            } else {
                                vec![
                                    center + egui::vec2(-4.0, -2.0),
                                    center + egui::vec2(4.0, -2.0),
                                    center + egui::vec2(0.0, 4.0),
                                ]
                            };
                            ui.painter().add(egui::Shape::convex_polygon(
                                points,
                                MUTED,
                                egui::Stroke::NONE,
                            ));
                            if response.clicked() {
                                toggle = Some((row.id.unwrap(), row.generation));
                            }
                        } else {
                            ui.add_space(23.0);
                        }
                        let (rect, response) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), 36.0),
                            egui::Sense::click(),
                        );
                        let painter = ui.painter().with_clip_rect(rect);
                        if row.id.is_some_and(|id| selected.contains(&id)) {
                            painter.rect_filled(rect, 3.0, ui.visuals().selection.bg_fill);
                        } else if response.hovered() {
                            painter.rect_filled(rect, 3.0, ui.visuals().widgets.hovered.bg_fill);
                        }
                        painter.text(
                            rect.left_top() + egui::vec2(4.0, 1.0),
                            egui::Align2::LEFT_TOP,
                            &row.label,
                            egui::TextStyle::Body.resolve(ui.style()),
                            if row.id.is_some_and(|id| selected.contains(&id)) {
                                ACCENT
                            } else {
                                ui.visuals().text_color()
                            },
                        );
                        painter.text(
                            rect.left_bottom() + egui::vec2(4.0, -1.0),
                            egui::Align2::LEFT_BOTTOM,
                            &row.detail,
                            egui::FontId::monospace(12.0),
                            MUTED,
                        );
                        if response.on_hover_text(&row.detail).clicked() {
                            selection = row.id.map(|id| {
                                (
                                    id,
                                    ui.input(|input| input.modifiers == egui::Modifiers::SHIFT),
                                )
                            });
                        }
                    });
                }
            });
        if let Some(key) = toggle {
            if !tree.collapsed.remove(&key) {
                tree.collapsed.insert(key);
            }
            tree.dirty = true;
        }
        if let Some((id, toggle)) = selection {
            self.studio.select_object(Some(id), toggle);
        }
    }
    fn inspector(&mut self, ui: &mut egui::Ui) {
        fn plot_component_menu(
            ui: &mut egui::Ui,
            value: &Value,
            language: Language,
        ) -> Option<usize> {
            let components = crate::curve::components(value);
            if components.is_empty() {
                return None;
            }
            let mut selected = None;
            ui.menu_button(language.text("Plot", "曲线"), |ui| {
                for (index, component) in components.iter().enumerate() {
                    if ui
                        .button(crate::curve::component_label(component, language))
                        .clicked()
                    {
                        selected = Some(index);
                        ui.close();
                    }
                }
            });
            selected
        }

        let menu_id = ui.id().with("property-context");
        let language = self.studio.prefs.language;
        ui.add_space(7.0);
        let Some(id) = self.studio.selected else {
            egui::Popup::close_id(ui.ctx(), menu_id);
            ui.label(language.text(
                "Select an object in the scene or object list.",
                "在场景或对象列表中选择对象。",
            ));
            return;
        };
        let Some(object) = self.studio.state.objects.get(&id) else {
            egui::Popup::close_id(ui.ctx(), menu_id);
            ui.label(format!("0x{id:016x}"));
            ui.colored_label(
                MUTED,
                language.text("Object is absent at this frame.", "此帧中不存在该对象。"),
            );
            return;
        };
        ui.add(
            egui::Label::new(
                egui::RichText::new(
                    object
                        .type_name
                        .rsplit("::")
                        .next()
                        .unwrap_or(&object.type_name),
                )
                .size(18.0)
                .strong(),
            )
            .truncate(),
        )
        .on_hover_text(&object.type_name);
        ui.horizontal(|ui| {
            ui.monospace(format!("0x{id:016x}"));
            if ui
                .small_button(language.text("Copy ID", "复制 ID"))
                .clicked()
            {
                ui.ctx().copy_text(format!("0x{id:016x}"));
            }
        });
        ui.label(
            egui::RichText::new(localized_format!(
                language,
                "{} properties",
                "{} 项属性",
                object.properties.len()
            ))
            .small()
            .color(MUTED),
        )
        .on_hover_text(localized_format!(
            language,
            "Generation {}",
            "对象代际 {}",
            object.generation
        ));
        let selection_count = self.studio.selected_ids().len();
        if selection_count > 1 {
            ui.label(
                egui::RichText::new(localized_format!(
                    language,
                    "{selection_count} selected · inspecting this object",
                    "已选 {selection_count} 个 · 查看当前对象",
                ))
                .small()
                .color(MUTED),
            );
        }
        let mut export_actor = false;
        let mut choose_repx_path = false;
        ui.horizontal(|ui| {
            if ui.button(language.text("Focus · F", "聚焦 · F")).clicked() {
                self.studio.focus_selected = true;
            }
            ui.checkbox(
                &mut self.studio.follow_selected,
                language.text("Follow", "跟随"),
            );
            if selection_count > 1 || matches!(
                object.type_name.rsplit("::").next(),
                Some("PxRigidDynamic" | "PxRigidStatic" | "PxArticulation" | "PxArticulationLink" | "PxParticleSystem" | "PxParticleFluid")
            ) {
                settings_menu(ui, "RepX", |ui| {
                    ui.text_edit_singleline(&mut self.studio.repx_path);
                    choose_repx_path = ui
                        .add_enabled(
                            !self.studio.file_picker.is_busy(),
                            egui::Button::new(language.text("Choose path…", "选择路径…")),
                        )
                        .clicked();
                    export_actor = ui
                        .add_enabled(
                            self.studio.save_job.is_none(),
                            egui::Button::new(
                                language.text("Export selection", "导出所选对象"),
                            ),
                        )
                        .on_hover_text(language.text(
                            "Includes connected Actors, joints, complete Articulation trees, Aggregate membership and resources.",
                            "包含相连 Actor、关节、完整 Articulation 树、Aggregate 成员及资源。",
                        ))
                        .clicked();
                    if choose_repx_path || export_actor {
                        ui.close();
                    }
                });
            }
        });
        ui.separator();
        let properties: Vec<_> = object.properties.iter().collect();
        let mut jump = None;
        let mut plot = None;
        let mut element_plot = None;
        let mut context_plot = None;
        let mut context_target = None;
        // 捕获切换递增曲线代际，实时快照更新保留该值。
        let source_epoch = self.studio.curves.epoch;
        let mut copy_error = None;
        let mut array_copy_error = None;
        egui::ScrollArea::vertical()
            .id_salt("properties")
            .show_rows(ui, 50.0, properties.len(), |ui, range| {
                for row in range {
                    let (name, value) = properties[row];
                    let title = ui
                        .add(
                            egui::Label::new(egui::RichText::new(name).small().color(MUTED))
                                .truncate(),
                        )
                        .on_hover_text(name);
                    if property_context_requested(ui, title.rect) {
                        context_target = Some(PropertyMenuTarget::new(
                            source_epoch,
                            object,
                            name,
                            None,
                            value,
                        ));
                    }
                    let property_row = property_value_row(
                        ui,
                        |ui| {
                            if ui.small_button(language.text("Copy", "复制")).clicked()
                                && let Err(error) = copy_property(ui.ctx(), value)
                            {
                                copy_error = Some(error);
                            }
                            if let Some(component) = plot_component_menu(ui, value, language) {
                                plot = Some((name.clone(), component));
                            }
                        },
                        |ui| match value {
                            Value::Ref(target) => {
                                let resolved = self
                                    .studio
                                    .state
                                    .resolve_reference(id, name, *target)
                                    .is_some();
                                let in_room = self.studio.room_scope.contains(*target);
                                if ui
                                    .add_enabled(
                                        resolved && in_room,
                                        egui::Link::new(format!("→ 0x{target:016x}")),
                                    )
                                    .on_hover_text(if resolved && !in_room {
                                        language.text("Outside this scene; available in All scenes", "不属于当前场景，可在“全部场景”中查看")
                                    } else if resolved {
                                        language.text("Inspect referenced object", "查看引用对象")
                                    } else {
                                        language.text(
                                            "Referenced generation is absent at this frame",
                                            "此帧中不存在该引用所指向的对象代际",
                                        )
                                    })
                                    .clicked()
                                {
                                    jump = Some(*target);
                                }
                            }
                            Value::Array(values) => {
                                ui.menu_button(
                                    localized_format!(
                                        language,
                                        "Array · {} values",
                                        "数组 · {} 项",
                                        values.len()
                                    ),
                                    |ui| {
                                        ui.set_min_width(280.0);
                                        egui::ScrollArea::vertical().max_height(360.0).show_rows(
                                            ui,
                                            24.0,
                                            values.len(),
                                            |ui, range| {
                                                for index in range {
                                                    let array_row = property_value_row(
                                                        ui,
                                                        |ui| {
                                                            if ui
                                                                .small_button(
                                                                    language.text("Copy", "复制"),
                                                                )
                                                                .clicked()
                                                                && let Err(error) = copy_property(
                                                                    ui.ctx(),
                                                                    &values[index],
                                                                )
                                                            {
                                                                array_copy_error = Some(error);
                                                            }
                                                            if let Some(component) = ui
                                                                .push_id(
                                                                    (
                                                                        id,
                                                                        object.generation,
                                                                        name,
                                                                        index,
                                                                    ),
                                                                    |ui| {
                                                                        plot_component_menu(
                                                                            ui,
                                                                            &values[index],
                                                                            language,
                                                                        )
                                                                    },
                                                                )
                                                                .inner
                                                            {
                                                                element_plot = Some((
                                                                    name.clone(),
                                                                    index,
                                                                    component,
                                                                ));
                                                            }
                                                        },
                                                        |ui| {
                                                            ui.label(format!("[{index}]"));
                                                            if let Value::Ref(target) =
                                                                &values[index]
                                                            {
                                                                let valid = self
                                                                    .studio
                                                                    .state
                                                                    .resolve_reference(
                                                                        id, name, *target,
                                                                    )
                                                                    .is_some();
                                                                if ui
                                                                    .add_enabled(
                                                                        valid && self.studio.room_scope.contains(*target),
                                                                        egui::Link::new(format!(
                                                                            "0x{target:x}"
                                                                        )),
                                                                    )
                                                                    .on_disabled_hover_text(language.text("Unavailable in this scene or frame", "不属于当前场景，或此帧中不存在"))
                                                                    .clicked()
                                                                {
                                                                    jump = Some(*target);
                                                                    ui.close();
                                                                }
                                                            } else {
                                                                let text = property_text(
                                                                    self.studio
                                                                        .recording
                                                                        .as_deref(),
                                                                    &object.type_name,
                                                                    name,
                                                                    &values[index],
                                                                );
                                                                ui.add(
                                                                    egui::Label::new(text)
                                                                        .truncate(),
                                                                );
                                                            }
                                                        },
                                                    );
                                                    if property_context_requested(
                                                        ui,
                                                        array_row.rect,
                                                    ) {
                                                        context_target =
                                                            Some(PropertyMenuTarget::new(
                                                                source_epoch,
                                                                object,
                                                                name,
                                                                Some(index),
                                                                &values[index],
                                                            ));
                                                    }
                                                }
                                            },
                                        );
                                    },
                                );
                            }
                            Value::Bytes(values) => {
                                ui.add(
                                    egui::Label::new(localized_format!(
                                        language,
                                        "{} raw bytes",
                                        "{} 个原始字节",
                                        values.len()
                                    ))
                                    .truncate(),
                                );
                            }
                            _ => {
                                let text = property_text(
                                    self.studio.recording.as_deref(),
                                    &object.type_name,
                                    name,
                                    value,
                                );
                                ui.add_sized(
                                    [ui.available_width(), 20.0],
                                    egui::Label::new(text).truncate(),
                                );
                            }
                        },
                    );
                    if property_context_requested(ui, property_row.rect) {
                        context_target = Some(PropertyMenuTarget::new(
                            source_epoch,
                            object,
                            name,
                            None,
                            value,
                        ));
                    }
                }
            });
        let target_id = menu_id.with("target");
        let open_menu = context_target.is_some();
        if let Some(target) = context_target {
            ui.ctx()
                .data_mut(|data| data.insert_temp(target_id, target));
        }
        let target = ui
            .ctx()
            .data(|data| data.get_temp::<PropertyMenuTarget>(target_id));
        egui::Popup::context_menu(&ui.response())
            .id(menu_id)
            .open_memory(open_menu.then_some(egui::SetOpenCommand::Bool(true)))
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                let Some(target) = target else {
                    ui.close();
                    return;
                };
                ui.set_max_width(360.0);
                ui.add(egui::Label::new(target.label()).wrap());
                let value = target.value(source_epoch, object);
                if value.is_none() {
                    ui.label(language.text(
                        "Property is unavailable at this frame.",
                        "此帧中该属性不可用。",
                    ));
                }
                if ui
                    .add_enabled(
                        value.is_some(),
                        egui::Button::new(language.text("Copy", "复制")),
                    )
                    .clicked()
                    && let Some(value) = value
                {
                    if let Err(error) = copy_property(ui.ctx(), value) {
                        copy_error = Some(error);
                    }
                    ui.close();
                }
                if let Some(value) =
                    value.filter(|value| !crate::curve::components(value).is_empty())
                {
                    if let Some(component) = plot_component_menu(ui, value, language) {
                        context_plot = Some((target, component));
                        ui.close();
                    }
                } else {
                    ui.add_enabled(false, egui::Button::new(language.text("Plot", "曲线")));
                }
            });
        if let Some(id) = jump {
            self.studio.select_object(Some(id), false);
        }
        if let Some(error) = copy_error.or(array_copy_error) {
            self.studio.notice(error);
        }
        if let Some((target, component)) = context_plot {
            if let Some(object) = self
                .studio
                .selected
                .and_then(|id| self.studio.state.objects.get(&id))
                && target.value(self.studio.curves.epoch, object).is_some()
            {
                if let Some(index) = target.index {
                    self.studio
                        .plot_element(target.property, index, component, ui.ctx().clone());
                } else {
                    self.studio
                        .plot(target.property, component, ui.ctx().clone());
                }
            }
        } else if let Some((property, index, component)) = element_plot {
            self.studio
                .plot_element(property, index, component, ui.ctx().clone());
        } else if let Some((property, component)) = plot {
            self.studio.plot(property, component, ui.ctx().clone());
        }
        if choose_repx_path {
            self.studio
                .pick_file(PickerKind::SaveRepx, ui.ctx().clone());
        }
        if export_actor {
            self.studio
                .export_repx(self.studio.repx_selection(), ui.ctx().clone());
        }
    }
    fn viewport(&mut self, ui: &mut egui::Ui) {
        let language = self.studio.prefs.language;
        playback_status::show(ui, self.studio);
        ui.add_space(4.0);
        let up = self.studio.up_resolution();
        let up_description = up.description(language);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(language.text("Perspective", "透视")).small().color(MUTED))
                .on_hover_text(format!("{} · {}", up_description, self.studio.prefs.handedness.label(language)));
            if up.is_fallback() {
                ui.colored_label(egui::Color32::YELLOW, language.text("Up direction unknown", "上方向未知"))
                    .on_hover_text(&up_description);
            }
            if ui.small_button(language.text("Frame all", "查看全部")).clicked() {
                self.studio.captured_camera = None;
                self.viewport.captured_pose = None;
                self.viewport.focus(None);
                self.studio.follow_selected = false;
            }
            if self.viewport.invalid_shapes > 0 { ui.colored_label(egui::Color32::LIGHT_RED, localized_format!(language, "{} invalid shapes omitted", "已略过 {} 个无效形状", self.viewport.invalid_shapes)).on_hover_text(language.text("Non-finite poses, invalid quaternions, negative dimensions or invalid mesh data. Raw object properties remain available.", "姿态含非有限数值、四元数无效、尺寸为负或网格数据无效。仍可查看原始对象属性。")); }
            if !self.viewport.diagnostics.is_empty() { ui.colored_label(egui::Color32::LIGHT_RED, language.text("Display incomplete", "显示不完整")).on_hover_text(self.viewport.diagnostics.iter().map(|message| message.text(language)).collect::<Vec<_>>().join("\n")); }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(localized_format!(language, "{} shapes", "{} 个形状", self.viewport.objects.len()))
                        .small()
                        .color(MUTED),
                );
                if self.viewport.point_clouds.point_count > 0 {
                    ui.label(egui::RichText::new(localized_format!(language, "{} particle points", "{} 个粒子点", self.viewport.point_clouds.point_count)).small().color(MUTED));
                }
            });
        });
        if self.studio.count() > 0 || !self.studio.state.objects.is_empty() {
            activity_counts(ui, self.studio);
        }
        let available = ui.available_size().max(egui::vec2(1.0, 1.0));
        let pixels = viewport_size::texture_size(available, ui.ctx().pixels_per_point());
        if self.viewport.pixel_size != pixels
            && let Some(image) = self.images.get_mut(&self.viewport.image)
        {
            image.resize(Extent3d {
                width: pixels.x,
                height: pixels.y,
                depth_or_array_layers: 1,
            });
            self.viewport.pixel_size = pixels;
        }
        let image_response = ui.add(egui::Image::new((self.viewport.texture, available)));
        let response = ui.interact(
            image_response.rect,
            crate::camera_controller::viewport_id(),
            egui::Sense::click_and_drag(),
        );
        let rect = response.rect;
        if self.studio.prefs.show_debug_draw {
            for primitive in crate::scene::display_debug_primitives(self.studio) {
                if !crate::scene::drawable_debug(primitive) {
                    continue;
                }
                if let pvd_core::debug_draw::Primitive::Text {
                    position,
                    size,
                    color,
                    text,
                    ..
                } = primitive
                    && let Some((point, scale)) = self.viewport.project(Vec3::from(*position), rect)
                    && rect.contains(point)
                {
                    ui.painter().text(
                        point,
                        egui::Align2::LEFT_BOTTOM,
                        text,
                        egui::FontId::proportional(
                            (size * scale * self.viewport.coordinates.scale).clamp(8.0, 40.0),
                        ),
                        egui::Color32::from_rgba_unmultiplied(
                            (color >> 16) as u8,
                            (color >> 8) as u8,
                            *color as u8,
                            (color >> 24) as u8,
                        ),
                    );
                }
            }
        }
        if self.studio.count() == 0 && self.studio.job.is_none() {
            ui.painter().text(
                rect.center() - egui::vec2(0.0, 15.0),
                egui::Align2::CENTER_CENTER,
                if self.studio.live_connected {
                    language.text("Waiting for frames", "等待帧数据")
                } else if self.studio.recording.is_some() {
                    language.text("No frames in this capture", "录制中没有帧")
                } else {
                    language.text("No capture open", "尚未打开录制")
                },
                egui::FontId::proportional(18.0),
                egui::Color32::from_rgb(207, 220, 235),
            );
            ui.painter().text(
                rect.center() + egui::vec2(0.0, 20.0),
                egui::Align2::CENTER_CENTER,
                language.text(
                    "Open a file, drop a capture, or start listening.",
                    "打开或拖入文件，或开始监听。",
                ),
                egui::TextStyle::Body.resolve(ui.style()),
                MUTED,
            );
        }
        let help_button = viewport_help::show(ui, rect, self.studio);
        if !help_button.contains_pointer()
            && self.studio.captured_camera.is_none()
            && crate::camera_controller::update(
                self.viewport,
                &response,
                self.studio.prefs.camera_mode,
                self.studio.prefs.camera_speed,
                &mut self.studio.prefs.fly_speed,
            )
            && self.studio.prefs.camera_mode == crate::camera_controller::CameraMode::Fly
        {
            self.studio.follow_selected = false;
        }
        if !help_button.contains_pointer()
            && !crate::camera_controller::keyboard_input_blocked(ui.ctx())
        {
            if response.clicked()
                && let Some(point) = response.interact_pointer_pos()
            {
                let toggle = ui.input(|input| input.modifiers == egui::Modifiers::SHIFT);
                self.studio
                    .select_object(self.viewport.select_at(point, rect), toggle);
            }
            if response.double_clicked() {
                self.studio.focus_selected = true;
            }
        }
    }
    fn timeline(&mut self, ui: &mut egui::Ui) {
        let language = self.studio.prefs.language;
        ui.add_space(6.0);
        let count = self.studio.count();
        let (first, last) = self.studio.playback_ordinal_bounds().unwrap_or((0, 0));
        if count == 0 || self.studio.playback_bounds().is_none() {
            ui.label(language.text("No frames", "暂无帧数据"));
        } else {
            let position = ui.label(localized_format!(
                language,
                "Frame {} / {}",
                "帧 {} / {}",
                self.studio.frame_ordinal(self.studio.frame),
                last
            ));
            if let Some(info) = self
                .studio
                .timeline_recording()
                .and_then(|recording| recording.frame_info(self.studio.frame))
            {
                position.on_hover_text(localized_format!(
                    language,
                    "Scene 0x{:016x} · SDK timestamp {}",
                    "场景 0x{:016x} · SDK 时间戳 {}",
                    info.scene_id,
                    info.timestamp
                ));
            }
            let mut frame = self
                .studio
                .frame_ordinal(self.studio.frame)
                .clamp(first, last);
            ui.spacing_mut().slider_width = (ui.available_width() - 12.0).max(50.0);
            if ui
                .add(
                    egui::Slider::new(&mut frame, first..=last)
                        .show_value(false)
                        .trailing_fill(true),
                )
                .changed()
                && let Some(target) = self.studio.frame_at_ordinal(frame)
            {
                self.studio.request_ui_action(
                    ui.ctx(),
                    ui.id().with("timeline-frame"),
                    UiAction::Locate(target, crate::studio::DisplayReason::Timeline),
                );
            }
            if self.studio.frame < self.studio.first_frame() {
                ui.label(language.text(
                    "Displayed frame has expired from history",
                    "当前帧已从历史中淘汰",
                ));
            }
        }
        if let Some(recording) = self.studio.timeline_recording()
            && let Some(stats) = recording.disk_usage()
        {
            if let (Some(first), Some(last)) = (recording.frames.first(), recording.frames.last()) {
                ui.label(localized_format!(
                    language,
                    "History {}–{} · {} frames",
                    "历史 {}–{} · {} 帧",
                    first.index,
                    last.index,
                    stats.retained_frames,
                ));
                if last.index.saturating_sub(first.index).saturating_add(1) != stats.retained_frames
                {
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        language.text(
                            "History contains gaps; missing frames cannot be replayed.",
                            "历史存在缺口；缺失帧无法回放。",
                        ),
                    );
                }
            } else {
                ui.label(language.text("No retained history frames", "尚无留存历史帧"));
            }
            let usage = if recording.capture_format().is_some() {
                localized_format!(
                    language,
                    "History file {:.1} MiB",
                    "历史文件 {:.1} MiB",
                    stats.disk_bytes as f64 / 1_048_576.0,
                )
            } else if stats.persistent {
                localized_format!(
                    language,
                    "Temporary recording on disk {:.2} GiB",
                    "临时录制磁盘占用 {:.2} GiB",
                    stats.disk_bytes as f64 / 1_073_741_824.0,
                )
            } else {
                localized_format!(
                    language,
                    "Playback disk cache {:.2} / {:.2} GiB",
                    "回放磁盘缓存 {:.2} / {:.2} GiB",
                    stats.disk_bytes as f64 / 1_073_741_824.0,
                    stats.budget_bytes as f64 / 1_073_741_824.0,
                )
            };
            let mut detail = localized_format!(
                language,
                "{} disk segments",
                "{} 个磁盘分段",
                stats.segments
            );
            if stats.persistent {
                detail.push('\n');
                detail.push_str(&localized_format!(
                    language,
                    "Hot history {:.2} / {:.2} GiB; sealed history stays available.",
                    "热历史 {:.2} / {:.2} GiB；已封存的历史仍可回放。",
                    stats.cache_disk_bytes as f64 / 1_073_741_824.0,
                    stats.budget_bytes as f64 / 1_073_741_824.0,
                ));
                detail.push('\n');
                detail.push_str(language.text(
                    "The active recording retains its beginning on disk. Ended temporary recordings are cleaned up separately according to Capture setup.",
                    "活动录制在磁盘保留起点。已结束的临时录制按录制设置单独清理。",
                ));
            }
            ui.label(usage).on_hover_text(detail);
            if let Some(warning) = stats.warning {
                ui.colored_label(egui::Color32::YELLOW, warning);
            }
        }
        ui.add_space(5.0);
        recording_save_controls(ui, self.studio);
        if count == 0 {
            return;
        }
        ui.horizontal_wrapped(|ui| {
            if !self.studio.is_native_recording() {
                ui.label(language.text("Clip capture frames", "裁剪录制帧"));
                ui.add(
                    egui::DragValue::new(&mut self.studio.trim_first)
                        .range(0..=count.saturating_sub(1)),
                );
                ui.label(language.text("through", "至"));
                ui.add(
                    egui::DragValue::new(&mut self.studio.trim_last)
                        .range(0..=count.saturating_sub(1)),
                );
                if ui
                    .add_enabled(!self.studio.file_picker.is_busy(), egui::Button::new("…"))
                    .on_hover_text(language.text("Choose clip path", "选择片段路径"))
                    .clicked()
                {
                    self.studio
                        .pick_file(PickerKind::SaveClip, ui.ctx().clone());
                }
                if ui
                .add_enabled(
                    self.studio.can_trim_capture()
                        && self.studio.trim_first <= self.studio.trim_last,
                    egui::Button::new(language.text("Save clip", "保存片段")),
                )
                .on_hover_text(localized_format!(
                    language,
                    "Save SDK clip to {}. Retains resource initialization and analysis preroll.",
                    "将 SDK 片段保存到 {}，保留资源初始化和片段前的分析事件。",
                    self.studio.export_path
                ))
                .clicked()
            {
                self.studio.trim_capture(ui.ctx().clone());
            }
            }
            settings_menu(
                ui,
                language.text("Export frame RepX", "导出帧 RepX"),
                |ui| {
                    ui.weak(language.text("All scenes in this frame", "范围：当前帧的全部场景"));
                    ui.text_edit_singleline(&mut self.studio.repx_path);
                    if ui
                        .add_enabled(
                            !self.studio.file_picker.is_busy(),
                            egui::Button::new(language.text("Choose path…", "选择路径…")),
                        )
                        .clicked()
                    {
                        self.studio
                            .pick_file(PickerKind::SaveRepx, ui.ctx().clone());
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            self.studio.save_job.is_none(),
                            egui::Button::new(language.text("Export current frame", "导出当前帧")),
                        )
                        .clicked()
                    {
                        self.studio
                            .export_repx(pvd_core::repx::Selection::Frame, ui.ctx().clone());
                        ui.close();
                    }
                },
            );
        });
        crate::recording_export::show_progress(ui, self.studio);
    }
    fn diagnostics(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .id_salt("diagnostics-panel")
            .show(ui, |ui| self.diagnostics_content(ui));
    }

    fn diagnostics_content(&mut self, ui: &mut egui::Ui) {
        let language = self.studio.prefs.language;
        ui.add_space(5.0);
        self.diagnostic_errors(ui);
        for message in &self.viewport.diagnostics {
            ui.colored_label(egui::Color32::LIGHT_RED, message.text(language));
        }
        receive_status(ui, self.studio);
        receive_buffer_status(ui, self.studio);
        if self.studio.live_connected {
            ui.label(localized_format!(
                language,
                "Decoded view updates: {}/s · target {} Hz",
                "解码画面更新 {} 次/秒 · 目标 {} Hz",
                self.studio.live_updates_per_second(),
                self.studio.prefs.live_refresh_hz,
            ));
        }
        egui::CollapsingHeader::new(language.text("Decoder profiling", "解码性能")).show(
            ui,
            |ui| {
                live_profile_diagnostics(ui, self.studio, self.viewport.objects.len());
            },
        );
        if let Some(recording) = &self.studio.recording {
            egui::CollapsingHeader::new(language.text("Memory usage", "内存用量"))
                .id_salt("diagnostic-memory")
                .show(ui, |ui| {
                    let usage = recording.history_memory_usage();
                    let mib = 1024.0 * 1024.0;
                    ui.label(localized_format!(
                        language,
                        "Core memory budget estimate: {:.1} / {:.0} MiB · checkpoint/cache {:.1} MiB ({} checkpoints)",
                        "核心内存预算估算：{:.1} / {:.0} MiB · 检查点/缓存 {:.1} MiB（{} 个）",
                        (usage.estimated_history_bytes + usage.estimated_checkpoint_bytes) as f64 / mib,
                        usage.limit_bytes as f64 / mib,
                        usage.estimated_checkpoint_bytes as f64 / mib,
                        usage.checkpoint_count,
                    ))
                    .on_hover_text(language.text(
                        "This is a core memory estimate, separate from the rolling disk quota shown in Timeline. It is not process memory usage: analysis, UI snapshots and graphics resources also consume memory.",
                        "这是核心内存估算，与时间轴显示的滚动磁盘容量分开计算。它不等于进程内存占用；分析数据、界面快照和图形资源也会占用内存。",
                    ));
                    ui.label(localized_format!(
                        language,
                        "Retained state changes: {} · unchanged updates omitted: {}",
                        "保留状态变化：{} · 已省略无变化更新：{}",
                        usage.retained_actions,
                        usage.skipped_redundant_actions,
                    ));
                });
        }
        if let Some(recording) = &self.studio.recording
            && !recording.diagnostics.is_empty()
        {
            egui::CollapsingHeader::new(language.text("Capture diagnostics", "录制诊断与导入限制"))
                .id_salt("diagnostic-capture")
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(100.0)
                        .id_salt("capture-diagnostics")
                        .show_rows(ui, 22.0, recording.diagnostics.len(), |ui, range| {
                            for row in range {
                                let detail = &recording.diagnostics[row];
                                ui.add(egui::Label::new(detail).truncate())
                                    .on_hover_text(detail);
                            }
                        });
                });
        }
        egui::CollapsingHeader::new(language.text("Application diagnostics", "应用诊断"))
            .id_salt("diagnostic-application")
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(90.0)
                    .id_salt("notices")
                    .show_rows(ui, 22.0, self.studio.notices.len(), |ui, range| {
                        for row in range {
                            ui.label(self.studio.notices[row].text(language));
                        }
                    });
            });
    }

    fn diagnostic_errors(&mut self, ui: &mut egui::Ui) {
        let language = self.studio.prefs.language;
        let errors = self.studio.error_rows.clone();
        if !errors.is_empty() {
            ui.separator();
            ui.label(language.text("SDK errors · click a frame to locate", "SDK 错误 · 点击帧号定位"))
                .on_hover_text(language.text("Frame association follows stream order. SDK error events do not include an object ID.", "帧关联按数据流顺序确定。SDK 错误事件不包含对象 ID。"));
            egui::ScrollArea::vertical()
                .id_salt("sdk-errors")
                .max_height(240.0)
                .show_rows(ui, 24.0, errors.len(), |ui, range| {
                    for row in range {
                        let (frame, error) = &errors[row];
                        let detail = localized_format!(
                            language,
                            "Code {} · {} · {}:{} · Timestamp {}",
                            "错误码 {} · {} · {}:{} · 时间戳 {}",
                            error.code,
                            error.message,
                            error.file,
                            error.line,
                            error.timestamp,
                        );
                        let summary = error_summary(
                            error.code,
                            &error.message,
                            &error.file,
                            error.line,
                            language,
                        );
                        ui.horizontal(|ui| {
                            if let Some(frame) = frame {
                                if ui
                                    .button(format!("{} {frame}", language.text("Frame", "帧")))
                                    .clicked()
                                {
                                    self.studio.request_ui_action(
                                        ui.ctx(),
                                        ui.id().with(("timed-error", row)),
                                        UiAction::Locate(
                                            *frame,
                                            crate::studio::DisplayReason::Error,
                                        ),
                                    );
                                }
                            } else {
                                ui.label(unframed_error_position(self.studio.count(), language));
                            }
                            if ui.small_button(language.text("Copy", "复制")).clicked() {
                                ui.ctx().copy_text(detail.clone());
                            }
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(summary)
                                        .color(egui::Color32::from_rgb(239, 148, 137)),
                                )
                                .truncate(),
                            )
                            .on_hover_text(detail);
                        });
                    }
                });
        }
        if let Some(recording) = &self.studio.recording {
            initial_errors(ui, recording.initial_untimed_errors(), language);
        }
        if let Some(recording) = self.studio.recording.clone()
            && !recording.untimed_error_events().is_empty()
        {
            ui.separator();
            ui.label(language.text("Recorded errors · time unknown", "录制错误 · 时间未知"))
                .on_hover_text(language.text(
                    "The database preserves receive order, without an error timestamp or affected Actor ID. Frame buttons show the next scene frame in that order, not the time of the error.",
                    "数据库保留接收顺序，没有错误时间戳或受影响的 Actor ID。帧按钮显示接收位置之后的场景帧，不代表错误发生时间。",
                ));
            let errors = recording.untimed_error_events();
            egui::ScrollArea::vertical()
                .id_salt("untimed-errors")
                .max_height(240.0)
                .show_rows(ui, 24.0, errors.len(), |ui, range| {
                    for row in range {
                        let error = &errors[row];
                        let detail = untimed_error_detail(error, language);
                        let summary = error_summary(
                            error.code,
                            &error.message,
                            &error.file,
                            error.line,
                            language,
                        );
                        ui.horizontal(|ui| {
                            if error.received_frame < recording.frame_count() {
                                if ui
                                    .button(format!(
                                        "{} {}",
                                        language.text("View frame", "查看帧"),
                                        error.received_frame
                                    ))
                                    .clicked()
                                {
                                    self.studio.request_ui_action(
                                        ui.ctx(),
                                        ui.id().with(("untimed-error", row)),
                                        UiAction::Locate(
                                            error.received_frame,
                                            crate::studio::DisplayReason::Error,
                                        ),
                                    );
                                }
                            } else {
                                ui.label(unframed_error_position(
                                    recording.frame_count(),
                                    language,
                                ));
                            }
                            if ui.small_button(language.text("Copy", "复制")).clicked() {
                                ui.ctx().copy_text(detail.clone());
                            }
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(summary)
                                        .color(egui::Color32::from_rgb(239, 148, 137)),
                                )
                                .truncate(),
                            )
                            .on_hover_text(detail);
                        });
                    }
                });
        }
    }
    fn display(&mut self, ui: &mut egui::Ui) {
        let language = self.studio.prefs.language;
        egui::ScrollArea::vertical().id_salt("display-settings").show(ui, |ui| {
        ui.add_space(8.0);
        ui.checkbox(&mut self.studio.prefs.show_viewport_help, language.text("Controls hint", "操作提示"));
        egui::CollapsingHeader::new(language.text("Camera", "相机"))
            .id_salt("camera-settings")
            .default_open(true)
            .show(ui, |ui| {
            crate::captured_camera::show(ui, self.studio, self.viewport);
            let manual = self.studio.captured_camera.is_none();
            ui.add_enabled_ui(manual, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(language.text("Controller", "控制模式"));
                for mode in [crate::camera_controller::CameraMode::Orbit, crate::camera_controller::CameraMode::Fly] {
                    ui.selectable_value(&mut self.studio.prefs.camera_mode, mode, mode.label(language));
                }
            });
            });
            if manual && self.studio.prefs.camera_mode == crate::camera_controller::CameraMode::Fly {
                self.studio.prefs.fly_speed = crate::camera_controller::fly_speed(self.studio.prefs.fly_speed);
                ui.horizontal_wrapped(|ui| {
                    ui.label(language.text("Flight speed", "飞行速度"));
                    ui.add_enabled(manual, egui::DragValue::new(&mut self.studio.prefs.fly_speed).range(0.001..=1e9).speed(0.1).suffix(language.text(" units/s", " 单位/秒")));
                });
                ui.label(egui::RichText::new(language.text("Hold the right button in the viewport: WASD moves, Q/E moves down/up along the selected up axis, Shift doubles speed. Speed uses recorded units per second; wheel adjusts speed. Text editing and IME suspend navigation.", "在视口按住右键：WASD 移动，Q/E 沿所选上轴下降/上升，Shift 加速2倍。速度按录制单位/秒计，滚轮调速。文本编辑及输入法组字时暂停导航。")).small());
            }
            let scale = self.viewport.coordinates.scale;
            ui.label(language.text("Target coordinates", "目标坐标"));
            let mut source_target = self.viewport.coordinates.source_point(self.viewport.view_target());
            for (index, axis) in ["X", "Y", "Z"].into_iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(axis);
                    if ui.add_enabled(manual, egui::DragValue::new(&mut source_target[index]).range(-1e12..=1e12).speed(0.1)).changed() {
                        self.viewport.target = self.viewport.coordinates.point(source_target);
                        self.studio.follow_selected = false;
                    }
                });
            }
            ui.horizontal(|ui| {
                ui.label(language.text("Distance", "距离"));
                let mut distance = self.viewport.view_distance();
                if ui.add_enabled(manual, egui::DragValue::new(&mut distance).range(0.01 * scale..=1e12 * scale).speed(0.1 * scale).max_decimals(12).suffix(" m")).changed() {
                    self.viewport.distance = distance;
                }
            });
            ui.horizontal(|ui| {
                ui.label(language.text("Near clip", "近裁剪面"));
                ui.add(egui::DragValue::new(&mut self.studio.prefs.camera_near).range(1e-7 * scale..=1e6 * scale).speed(0.001 * scale).max_decimals(12).suffix(" m"));
            });
            ui.horizontal(|ui| {
                ui.label(language.text("Far clip", "远裁剪面"));
                ui.add(egui::DragValue::new(&mut self.studio.prefs.camera_far).range(1e-6 * scale..=1e12 * scale).speed(scale).max_decimals(12).suffix(" m"));
            });
            ui.horizontal(|ui| {
                ui.label(language.text("Control speed", "操作速度"));
                ui.add_enabled(manual, egui::DragValue::new(&mut self.studio.prefs.camera_speed).range(0.05..=20.0).speed(0.05).suffix("×"));
            });
            ui.horizontal_wrapped(|ui| {
                if ui.button(language.text("Reset camera", "重置相机"))
                    .on_hover_text(language.text("Point at the scene center without changing camera distance.", "朝向场景中心，保持相机距离。"))
                    .clicked()
                {
                    self.studio.captured_camera = None;
                    self.studio.focus_selected = false;
                    self.studio.follow_selected = false;
                    self.viewport.captured_pose = None;
                    crate::camera_controller::cancel_gesture(ui.ctx());
                    self.viewport.reset_camera();
                }
                if ui.button(language.text("Frame all", "查看全部")).clicked() { self.studio.captured_camera = None; self.viewport.captured_pose = None; self.viewport.focus(None); self.studio.follow_selected = false; }
            });
        });
        ui.collapsing(language.text("Source coordinates", "源坐标系"), |ui| {
            source_coordinates::show(ui, self.studio);
        });
        ui.label(
            egui::RichText::new(language.text("OVERLAYS", "叠加显示"))
                .small()
                .color(MUTED),
        );
        ui.horizontal(|ui| {
            ui.label(language.text("Line width", "线宽"));
            ui.add(
                egui::DragValue::new(&mut self.studio.prefs.gizmo_line_width)
                    .range(0.5..=4.0)
                    .speed(0.05)
                    .suffix(" px"),
            );
        }).response.on_hover_text(language.text(
            "Applies to bounds, wireframes and debug lines.",
            "用于包围盒、线框和调试线条。",
        ));
        ui.checkbox(
            &mut self.studio.prefs.show_grid,
            language.text("Coordinate grid", "坐标网格"),
        );
        ui.checkbox(
            &mut self.studio.prefs.show_axes,
            language.text("Origin axes", "原点坐标轴"),
        );
        ui.checkbox(
            &mut self.studio.prefs.show_debug_draw,
            language.text("Debug drawing", "调试绘制"),
        ).on_hover_text(language.text("Drawing without verified scene ownership is available in All scenes.", "无法确定场景归属的绘制可在“全部场景”中查看。"));
        ui.checkbox(
            &mut self.studio.prefs.show_contacts,
            language.text("Contacts and normals", "接触点与法线"),
        );
        ui.checkbox(&mut self.studio.prefs.show_particles, language.text("Particles / fluid points", "粒子 / 流体点"));
        ui.checkbox(&mut self.studio.prefs.show_cloth_points, language.text("Cloth particles", "布料粒子"));
        ui.checkbox(&mut self.studio.prefs.show_cloth_constraints, language.text("Fabric constraints", "Fabric 约束"));
        ui.horizontal(|ui| {
            ui.label(language.text("Point radius", "点半径"));
            ui.add(egui::DragValue::new(&mut self.studio.prefs.point_radius).range(0.0001..=1000.0).speed(0.005));
        }).response.on_hover_text(language.text("Display marker size in recorded units; it is not the SDK particle rest offset or a reconstructed fluid surface.", "以录制单位设置标记大小；它不是 SDK 粒子静止间距，也不代表重建的流体表面。"));
        if self.viewport.point_clouds.clouds.iter().any(|cloud| cloud.kind == crate::point_clouds::Kind::Cloth) {
            ui.label(egui::RichText::new(language.text("Cloth shows current particles and Fabric constraints. The SDK stream has no surface topology or previous-particle buffer.", "Cloth 显示当前粒子和 Fabric 约束。SDK 流不含表面拓扑或前一粒子缓冲。")).small());
        }
        ui.checkbox(
            &mut self.studio.prefs.show_query_rays,
            language.text("Raycasts", "射线查询"),
        );
        ui.checkbox(
            &mut self.studio.prefs.show_query_sweeps,
            language.text("Sweeps", "扫掠查询"),
        );
        ui.checkbox(
            &mut self.studio.prefs.show_query_overlaps,
            language.text("Overlaps", "重叠查询"),
        );
        ui.checkbox(
            &mut self.studio.prefs.show_query_hits,
            language.text("Hit positions", "命中位置"),
        );
        ui.checkbox(
            &mut self.studio.prefs.show_query_normals,
            language.text("Hit normals", "命中法线"),
        );
        let queries = &self.viewport.queries;
        if !queries.raycasts.is_empty() || !queries.sweeps.is_empty() || !queries.overlaps.is_empty() {
            ui.label(localized_format!(
                language,
                "This frame: {} rays · {} sweeps · {} overlaps",
                "此帧：{} 条射线 · {} 次扫掠 · {} 次重叠查询",
                queries.raycasts.len(), queries.sweeps.len(), queries.overlaps.len()
            ));
            ui.label(egui::RichText::new(language.text("Gold: recorded hit · blue/purple: no recorded hit", "金色：有命中记录 · 蓝/紫：无命中记录")).small().color(MUTED))
                .on_hover_text(language.text("Batch completion and sweep inflation are absent from this stream. Overlaps carry references, no contact points.", "捕获中不含批量执行状态和扫掠膨胀量。重叠查询只有引用，没有接触点。"));
        }
        ui.add_space(12.0);
        ui.label(
            egui::RichText::new(language.text("VISIBILITY", "可见性"))
                .small()
                .color(MUTED),
        );
        ui.checkbox(
            &mut self.studio.prefs.show_static,
            language.text("Static bodies", "静态刚体"),
        );
        ui.checkbox(
            &mut self.studio.prefs.show_dynamic,
            language.text("Dynamic bodies", "动态刚体"),
        );
        ui.checkbox(
            &mut self.studio.prefs.show_kinematic,
            language.text("Kinematic bodies", "运动学刚体"),
        );
        ui.checkbox(
            &mut self.studio.prefs.show_bounds,
            language.text("Computed geometry bounds", "计算的几何包围盒"),
        );
        if self.studio.prefs.show_bounds { object_scope(ui, "bounds-scope", &mut self.studio.prefs.bounds_scope, language); }
        ui.checkbox(&mut self.studio.prefs.show_local_axes, language.text("Local axes", "局部坐标轴"));
        if self.studio.prefs.show_local_axes { object_scope(ui, "local-axes-scope", &mut self.studio.prefs.local_axes_scope, language); }
        ui.checkbox(
            &mut self.studio.prefs.show_com,
            language.text("Centers of mass", "质心"),
        );
        if self.studio.prefs.show_com { object_scope(ui, "com-scope", &mut self.studio.prefs.com_scope, language); }
        ui.checkbox(
            &mut self.studio.prefs.show_velocity,
            language.text("Velocity vectors", "速度向量"),
        )
        .on_hover_text(language.text(
            "Green: linear velocity · purple: angular velocity. Arrow lengths use the display multiplier below; property values remain in SDK units.",
            "绿色：线速度 · 紫色：角速度。箭头长度使用下方显示倍率；属性数值保留 SDK 单位。",
        ));
        if self.studio.prefs.show_velocity { object_scope(ui, "velocity-scope", &mut self.studio.prefs.velocity_scope, language); }
        ui.checkbox(
            &mut self.studio.prefs.wireframe,
            language.text("Wireframe geometry", "几何线框"),
        );
        ui.checkbox(
            &mut self.studio.prefs.show_sleeping,
            language.text("Sleeping bodies", "休眠刚体"),
        );
        if self.studio.prefs.show_sleeping { object_scope(ui, "sleeping-scope", &mut self.studio.prefs.sleeping_scope, language); }
        ui.checkbox(&mut self.studio.prefs.show_triggers, language.text("Trigger shapes", "触发器"));
        if self.studio.prefs.show_triggers { object_scope(ui, "trigger-scope", &mut self.studio.prefs.trigger_scope, language); }
        ui.label(language.text("Opacity", "不透明度"));
        object_scope(ui, "opacity-scope", &mut self.studio.prefs.opacity_scope, language);
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut self.studio.prefs.opacity).range(0.1..=1.0).speed(0.01));
            ui.spacing_mut().slider_width = ui.available_width().max(1.0);
            ui.add(egui::Slider::new(&mut self.studio.prefs.opacity, 0.1..=1.0).show_value(false));
        });
        ui.collapsing(language.text("Colors", "颜色"), |ui| {
            let colors = &mut self.studio.prefs.display_colors;
            display_color(ui, language.text("Static bodies", "静态刚体"), &mut colors.static_body);
            display_color(ui, language.text("Awake bodies", "唤醒刚体"), &mut colors.dynamic_body);
            display_color(ui, language.text("Sleeping bodies", "休眠刚体"), &mut colors.sleeping_body);
            display_color(ui, language.text("Kinematic bodies", "运动学刚体"), &mut colors.kinematic_body);
            display_color(ui, language.text("Cloth", "布料"), &mut colors.cloth);
            if ui.button(language.text("Reset colors", "重置颜色")).clicked() {
                *colors = crate::studio::DisplayColors::default();
            }
        });
        ui.collapsing(language.text("Overlay sizes", "叠加显示尺寸"), |ui| {
            ui.horizontal(|ui| {
                ui.label(language.text("Markers / axes", "标记 / 坐标轴"));
                ui.add(egui::DragValue::new(&mut self.studio.prefs.gizmo_scale).range(0.01..=100.0).speed(0.05).suffix("×"));
            });
            ui.horizontal(|ui| {
                ui.label(language.text("Velocity arrows", "速度箭头"));
                ui.add(egui::DragValue::new(&mut self.studio.prefs.velocity_scale).range(0.01..=100.0).speed(0.05).suffix("×"));
            });
            ui.horizontal(|ui| {
                ui.label(language.text("Joint frame axes", "关节坐标轴"));
                ui.add(egui::DragValue::new(&mut self.studio.prefs.joint_frame_scale).range(0.01..=100.0).speed(0.05).suffix("×"));
            });
            ui.label(egui::RichText::new(language.text("Display sizes do not change recorded geometry, joint limits or properties.", "显示尺寸不改变录制几何、关节限位或属性。")).small());
        });
        });
    }
    fn performance(&mut self, ui: &mut egui::Ui) {
        if self.studio.displayed_room().is_some() {
            ui.weak(
                self.studio
                    .prefs
                    .language
                    .text("Capture-wide analysis", "整个录制的分析"),
            );
        }
        crate::profile::show(ui, self.studio);
    }
    fn memory(&mut self, ui: &mut egui::Ui) {
        if self.studio.displayed_room().is_some() {
            ui.weak(
                self.studio
                    .prefs
                    .language
                    .text("Capture-wide analysis", "整个录制的分析"),
            );
        }
        crate::memory::show(ui, self.studio);
    }
    fn curves(&mut self, ui: &mut egui::Ui) {
        crate::curve_panel::show(ui, self.studio);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn initial_error() -> pvd_core::InitialUntimedError {
        pvd_core::InitialUntimedError {
            code: 4,
            message: "captured before the first retained frame".repeat(12),
            file: "source/example.cpp".into(),
            line: 248,
            index: 9,
            container_instance: 29,
            container_generation: 3,
        }
    }

    #[test]
    fn initial_error_rows_keep_duplicates_and_copy_full_details_without_frame_actions() {
        for language in [Language::Chinese, Language::English] {
            let context = egui::Context::default();
            configure(&context);
            let errors = [initial_error(), initial_error()];
            let draw = |events| {
                context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(360.0, 300.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |context| {
                        egui::CentralPanel::default().show(context, |ui| {
                            let right = ui.max_rect().right();
                            initial_errors(ui, &errors, language);
                            assert!(ui.min_rect().right() <= right + 0.1);
                        });
                    },
                )
            };
            let _ = draw(vec![]);
            let output = draw(vec![]);
            let text: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) => Some(text),
                    _ => None,
                })
                .collect();
            let copy = language.text("Copy", "复制");
            assert_eq!(
                text.iter()
                    .filter(|text| text.galley.text() == copy)
                    .count(),
                2
            );
            assert!(text.iter().any(|text| {
                text.galley.text() == language.text("Errors present at start", "起点已有错误")
            }));
            assert!(text.iter().any(|text| {
                text.galley
                    .text()
                    .starts_with(language.text("Code 4 · ", "错误码 4 · "))
            }));
            for absent in [
                "View frame",
                "查看帧",
                "After final frame",
                "末帧之后",
                "Index 9",
                "索引 9",
                "Container",
                "容器",
                "Generation",
                "代际",
            ] {
                assert!(!text.iter().any(|text| text.galley.text().contains(absent)));
            }
            let copy_text = text.iter().find(|text| text.galley.text() == copy).unwrap();
            let position = copy_text
                .galley
                .rect
                .translate(copy_text.pos.to_vec2())
                .center();
            let _ = draw(vec![egui::Event::PointerMoved(position)]);
            let _ = draw(vec![egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }]);
            let output = draw(vec![egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }]);
            let copied = output
                .platform_output
                .commands
                .iter()
                .find_map(|command| match command {
                    egui::OutputCommand::CopyText(text) => Some(text),
                    _ => None,
                })
                .expect("copy command");
            assert!(copied.contains(&errors[0].message));
            assert!(copied.contains("source/example.cpp:248"));
            assert!(copied.contains(language.text("Index 9", "索引 9")));
            assert!(copied.contains(language.text("Container 0x1D", "容器 0x1D")));
            assert!(copied.contains(language.text("Generation 3", "代际 3")));
            for absent in ["Received", "收到", "History event", "历史事件"] {
                assert!(!copied.contains(absent));
            }
        }
    }

    #[test]
    fn initial_error_list_draws_only_visible_rows() {
        let context = egui::Context::default();
        configure(&context);
        let errors = vec![initial_error(); 1000];
        for _ in 0..2 {
            let output = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(480.0, 320.0),
                    )),
                    ..Default::default()
                },
                |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        initial_errors(ui, &errors, Language::English);
                    });
                },
            );
            let drawn_rows = output
                .shapes
                .iter()
                .filter(|shape| {
                    matches!(&shape.shape,
                        egui::epaint::Shape::Text(text) if text.galley.text() == "Copy"
                    )
                })
                .count();
            assert!((1..=12).contains(&drawn_rows), "{drawn_rows}");
        }
    }

    #[test]
    fn diagnostics_prioritize_errors_in_short_panels() {
        for language in [Language::Chinese, Language::English] {
            let context = egui::Context::default();
            configure(&context);
            let mut studio = Studio::default();
            studio.prefs.language = language;
            let mut recording = playback_recording();
            recording.diagnostics = vec!["long capture import restriction".repeat(30); 20];
            studio.recording = Some(Arc::new(recording));
            studio.error_rows = Arc::new(vec![(
                Some(3),
                pvd_core::ErrorEvent {
                    code: 4,
                    message: "invalid mass".into(),
                    file: "source.cpp".into(),
                    line: 248,
                    timestamp: 100,
                },
            )]);
            let mut viewport = Viewport::default();
            let mut images = Assets::<Image>::default();
            let mut output = None;
            for _ in 0..2 {
                output = Some(context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(840.0, 200.0),
                        )),
                        ..Default::default()
                    },
                    |context| {
                        egui::CentralPanel::default().show(context, |ui| {
                            Viewer {
                                studio: &mut studio,
                                viewport: &mut viewport,
                                images: &mut images,
                            }
                            .diagnostics(ui);
                        });
                    },
                ));
            }
            let output = output.unwrap();
            let visible: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text)
                        if shape
                            .clip_rect
                            .contains(text.galley.rect.translate(text.pos.to_vec2()).center()) =>
                    {
                        Some(text.galley.text())
                    }
                    _ => None,
                })
                .collect();
            assert!(visible.iter().any(|text| text.contains("invalid mass")));
            assert!(visible.iter().any(|text| {
                *text
                    == language.text(
                        "SDK errors · click a frame to locate",
                        "SDK 错误 · 点击帧号定位",
                    )
            }));
            assert!(
                !visible
                    .iter()
                    .any(|text| text.contains("long capture import restriction"))
            );
            assert!(!visible.iter().any(|text| {
                text.contains(language.text("Core memory budget estimate", "核心内存预算估算"))
            }));
        }
    }

    fn playback_recording() -> Recording {
        use pvd_protocol::{Endian, Event, EventGroup, Header};
        let mut builder = pvd_core::RecordingBuilder::new(Header {
            version: 1,
            timestamp_numerator: 1,
            timestamp_denominator: 1,
            flags: 1,
            endian: Endian::Little,
        });
        builder
            .push(EventGroup {
                stream_id: 1,
                timestamp: 0,
                offset: 0,
                events: std::iter::once(Event::StringHandle {
                    handle: 1,
                    value: "frame".into(),
                })
                .chain((0..20).map(|timestamp| Event::EndSection {
                    id: 1,
                    name: 1,
                    timestamp,
                }))
                .collect(),
            })
            .unwrap();
        builder.snapshot().unwrap()
    }

    #[test]
    fn toolbar_fits_minimum_window_in_both_languages_and_while_seeking() {
        for language in [Language::Chinese, Language::English] {
            for width in [1024.0, 1280.0, 1440.0] {
                for live in [false, true] {
                    for seeking in [None, Some(12)] {
                        let context = egui::Context::default();
                        configure(&context);
                        let mut studio = Studio::default();
                        studio.prefs.language = language;
                        studio.recording = Some(Arc::new(playback_recording()));
                        studio.path_input =
                            "C:/recordings/非常长的目录名称/scene.pvdstream".repeat(8);
                        studio.source = studio.path_input.clone();
                        studio.frame = 10;
                        studio.seeking = seeking;
                        if live {
                            studio.live_session = Some(7);
                            studio.live_connected = true;
                            studio.follow_live = true;
                        }
                        for pixels_per_point in [1.0, 1.25, 1.5, 2.0, 1.0] {
                            for _ in 0..2 {
                                let mut input = egui::RawInput {
                                    screen_rect: Some(egui::Rect::from_min_size(
                                        egui::Pos2::ZERO,
                                        egui::vec2(width, 720.0),
                                    )),
                                    ..Default::default()
                                };
                                input
                                    .viewports
                                    .get_mut(&egui::ViewportId::ROOT)
                                    .unwrap()
                                    .native_pixels_per_point = Some(pixels_per_point);
                                let _ =
                                    context.run(input, |context| {
                                        assert_eq!(context.pixels_per_point(), pixels_per_point);
                                        let response = egui::TopBottomPanel::top("toolbar-test")
                                            .show(context, |ui| {
                                                let right = ui.max_rect().right();
                                                toolbar::show(
                                                    ui,
                                                    &mut studio,
                                                    &mut crate::navmesh::NavMeshOverlay::default(),
                                                );
                                                assert!(ui.min_rect().right() <= right + 0.1);
                                            });
                                        assert!(response.response.rect.right() <= width + 0.1);
                                        assert!(response.response.rect.height() <= 116.0);
                                    });
                            }
                        }
                        assert_eq!(studio.frame, 10);
                        assert_eq!(studio.seeking, seeking);
                        assert_eq!(studio.follow_live, live);
                    }
                }
            }
        }
    }

    #[test]
    fn display_status_is_visible_in_both_languages_and_fits_narrow_viewports() {
        use crate::studio::DisplayMode;
        for language in [Language::Chinese, Language::English] {
            for width in [240.0, 480.0, 1200.0] {
                let context = egui::Context::default();
                configure(&context);
                let mut studio = Studio::default();
                studio.prefs.language = language;
                studio.recording = Some(Arc::new(playback_recording()));
                studio.seek(10);
                studio.live_session = Some(1);
                studio.live_connected = true;
                for (follow, playing, seeking, mode) in [
                    (true, 0, None, DisplayMode::Live),
                    (false, 0, None, DisplayMode::Paused),
                    (false, 1, None, DisplayMode::Forward),
                    (false, -1, None, DisplayMode::Reverse),
                    (false, 1, Some(12), DisplayMode::Forward),
                    (false, -1, Some(12), DisplayMode::Reverse),
                    (false, 0, Some(12), DisplayMode::Seeking),
                ] {
                    studio.follow_live = follow;
                    studio.playing = playing;
                    studio.seeking = seeking;
                    let mut output = None;
                    for _ in 0..2 {
                        output = Some(context.run(
                            egui::RawInput {
                                screen_rect: Some(egui::Rect::from_min_size(
                                    egui::Pos2::ZERO,
                                    egui::vec2(width, 300.0),
                                )),
                                ..Default::default()
                            },
                            |context| {
                                egui::CentralPanel::default().show(context, |ui| {
                                    let right = ui.max_rect().right();
                                    let response = playback_status::show(ui, &studio);
                                    assert!(response.rect.right() <= right + 0.1);
                                });
                            },
                        ));
                    }
                    assert!(output.unwrap().shapes.iter().any(|shape| matches!(&shape.shape,
                        egui::epaint::Shape::Text(text) if text.galley.text() == mode.label(language)
                    )));
                    assert_eq!(studio.follow_live, follow);
                    assert_eq!(studio.playing, playing);
                    assert_eq!(studio.seeking, seeking);
                }
            }
        }
    }

    #[test]
    fn expired_display_frame_is_visible_without_an_implicit_seek() {
        let mut recording = playback_recording();
        let retained: Vec<_> = recording.frames.iter().skip(10).cloned().collect();
        recording.frames.clear();
        for frame in retained {
            recording.frames.push(frame);
        }
        for language in [Language::English, Language::Chinese] {
            let context = egui::Context::default();
            configure(&context);
            let mut studio = Studio::default();
            studio.prefs.language = language;
            studio.live_session = Some(1);
            studio.live_connected = true;
            studio.frame = 3;
            studio.recording = Some(Arc::new(recording.clone()));
            let mut output = None;
            for _ in 0..2 {
                output = Some(context.run(egui::RawInput::default(), |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        ui.horizontal(|ui| playback(ui, &mut studio));
                    });
                }));
            }
            assert_eq!(studio.frame, 3);
            assert!(studio.seeking.is_none());
            assert!(
                output
                    .unwrap()
                    .shapes
                    .iter()
                    .any(|shape| matches!(&shape.shape,
                        egui::epaint::Shape::Text(text) if text.galley.text() == "3"
                    ))
            );
        }
    }

    #[test]
    fn tcp_playback_buttons_receive_pointer_clicks_while_live_frames_grow() {
        for language in [Language::English, Language::Chinese] {
            let context = egui::Context::default();
            configure(&context);
            let recording = Arc::new(playback_recording());
            let mut studio = Studio::default();
            studio.prefs.language = language;
            studio.live_session = Some(1);
            studio.live_connected = true;
            studio.follow_live = true;
            studio.frame = 10;
            studio.recording = Some(recording);
            let draw = |studio: &mut Studio, events| {
                context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(1024.0, 768.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |context| {
                        egui::CentralPanel::default().show(context, |ui| {
                            ui.horizontal(|ui| playback(ui, studio));
                        });
                    },
                )
            };
            for (label, direction) in [
                (language.text("◀ Reverse", "◀ 倒放"), -1),
                (language.text("▶ Play", "▶ 播放"), 1),
            ] {
                if direction == 1 {
                    studio.pause_display();
                }
                let _ = draw(&mut studio, vec![]);
                let output = draw(&mut studio, vec![]);
                let position = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                            Some(text.galley.rect.translate(text.pos.to_vec2()).center())
                        }
                        _ => None,
                    })
                    .expect("playback button was not drawn");
                let _ = draw(&mut studio, vec![egui::Event::PointerMoved(position)]);
                let _ = draw(
                    &mut studio,
                    vec![egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    }],
                );
                let mut latest = (**studio.recording.as_ref().unwrap()).clone();
                latest.frames.push(pvd_core::FrameInfo {
                    index: latest.frame_count(),
                    timestamp: 20,
                    scene_id: 1,
                });
                studio.recording = Some(Arc::new(latest));
                let _ = draw(
                    &mut studio,
                    vec![egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                );
                studio.apply_ui_actions(&context);
                assert_eq!(studio.playing, direction);
                assert!(!studio.follow_live);
                assert!(studio.live_connected);
            }
        }
    }

    #[test]
    fn live_profile_diagnostics_fit_both_languages_and_preserve_default_off() {
        let context = egui::Context::default();
        configure(&context);
        let mut studio = Studio::default();
        assert!(!studio.live_profile_enabled);
        for language in [Language::English, Language::Chinese] {
            studio.prefs.language = language;
            for enabled in [false, true] {
                studio.live_profile_enabled = enabled;
                for available in [false, true] {
                    studio.last_live_profile =
                        available.then(|| crate::network::LiveProfileSnapshot {
                            session: 123,
                            elapsed_seconds: 234.5,
                            completed: true,
                            capture_failed: false,
                            decoder_input_bytes: 1024 * 1024,
                            receive_session: Some(123),
                            received_bytes: Some(1024 * 1024),
                            queued_bytes: Some(0),
                            retained_disk_bytes: Some(0),
                            metrics: pvd_core::metrics::Metrics::default().snapshot(),
                        });
                    for width in [240.0, 480.0, 1200.0] {
                        let input = egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(width, 1000.0),
                            )),
                            ..Default::default()
                        };
                        let _ = context.run(input, |context| {
                            egui::CentralPanel::default().show(context, |ui| {
                                let right = ui.max_rect().right();
                                let response = live_profile_diagnostics(ui, &mut studio, 12);
                                assert!(response.rect.right() <= right + 0.1);
                            });
                        });
                        assert_eq!(studio.live_profile_enabled, enabled);
                    }
                }
            }
        }
    }

    #[test]
    fn live_diagnostic_json_keeps_paused_display_separate_from_decoder_session() {
        let mut studio = Studio::default();
        studio.live_session = Some(7);
        studio.frame = 17;
        studio.follow_live = false;
        let profile = crate::network::LiveProfileSnapshot {
            session: 8,
            elapsed_seconds: 1.5,
            completed: false,
            capture_failed: false,
            decoder_input_bytes: 1000,
            receive_session: Some(9),
            received_bytes: None,
            queued_bytes: Some(500),
            retained_disk_bytes: Some(700),
            metrics: pvd_core::metrics::Metrics::default().snapshot(),
        };
        let json: serde_json::Value =
            serde_json::from_str(&live_diagnostic_json(&profile, &studio, 12).unwrap()).unwrap();
        assert_eq!(json["display"]["frame"], 17);
        assert_eq!(json["display"]["session"], 7);
        assert_eq!(json["display"]["follow_live"], false);
        assert_eq!(json["display"]["objects"], 0);
        assert_eq!(json["display"]["shapes"], 12);
        assert_eq!(json["display"]["live_updates_per_second"], 0);
        assert_eq!(json["profile"]["session"], 8);
        assert_eq!(json["profile"]["receive_session"], 9);
        assert!(json["profile"]["received_bytes"].is_null());
        assert_eq!(studio.frame, 17);
        assert!(!studio.follow_live);
    }

    #[test]
    fn scene_revision_requests_followup_repaint_without_refreshing_stable_idle() {
        use std::sync::atomic::AtomicUsize;
        use std::time::Duration;

        let context = egui::Context::default();
        let immediate_wakes = Arc::new(AtomicUsize::new(0));
        let wakes = immediate_wakes.clone();
        context.set_request_repaint_callback(move |request| {
            if request.delay.is_zero() {
                wakes.fetch_add(1, Ordering::Relaxed);
            }
        });
        let pass = |previous_revision, revision| {
            context.run(egui::RawInput::default(), |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    ui.label("Captured scene");
                });
                request_scene_sync(context, previous_revision, revision);
            })
        };
        for _ in 0..8 {
            let _ = pass(7, 7);
        }
        assert!(!context.has_requested_repaint());
        let before_change = immediate_wakes.load(Ordering::Relaxed);
        let changed = pass(7, 8);
        assert_eq!(
            changed.viewport_output[&egui::ViewportId::ROOT].repaint_delay,
            Duration::ZERO
        );
        assert!(context.has_requested_repaint());
        assert!(immediate_wakes.load(Ordering::Relaxed) > before_change);
        let followup = pass(8, 8);
        assert_eq!(
            followup.viewport_output[&egui::ViewportId::ROOT].repaint_delay,
            Duration::ZERO
        );
        for _ in 0..8 {
            let _ = pass(8, 8);
        }
        assert!(!context.has_requested_repaint());
        let idle_wakes = immediate_wakes.load(Ordering::Relaxed);
        for _ in 0..8 {
            let idle = pass(8, 8);
            assert_eq!(
                idle.viewport_output[&egui::ViewportId::ROOT].repaint_delay,
                Duration::MAX
            );
        }
        assert_eq!(immediate_wakes.load(Ordering::Relaxed), idle_wakes);
    }

    #[test]
    fn activity_counts_fit_both_languages_without_expanding_narrow_viewports() {
        let context = egui::Context::default();
        configure(&context);
        let mut studio = Studio::default();
        for language in [Language::English, Language::Chinese] {
            studio.prefs.language = language;
            for width in [240.0, 480.0, 1200.0] {
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 300.0),
                    )),
                    ..Default::default()
                };
                let _ = context.run(input, |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        let right = ui.max_rect().right();
                        let response = activity_counts(ui, &studio);
                        assert!(response.rect.right() <= right + 0.1);
                        assert!(response.rect.height() < 60.0);
                    });
                });
            }
        }
    }

    #[test]
    fn receive_backlog_and_failure_labels_fit_both_languages_without_changing_follow() {
        let context = egui::Context::default();
        configure(&context);
        let mut studio = Studio::default();
        studio.live_session = Some(123);
        studio.follow_live = true;
        for language in [Language::English, Language::Chinese] {
            studio.prefs.language = language;
            for (limit_bytes, queued_bytes, failed) in [
                (0, 512 << 20, false),
                (0, 8 << 20, false),
                (2048 << 20, 512 << 20, false),
                (2048 << 20, 2048 << 20, true),
            ] {
                studio.receive_status = Some(pvd_transport::SpoolSnapshot {
                    session: 124,
                    received_bytes: 99_999 << 20,
                    queued_bytes,
                    retained_disk_bytes: queued_bytes + (16 << 20),
                    limit_bytes,
                    connected: !failed,
                    last_failure: failed.then(|| "receive backlog capacity reached".into()),
                    failure_session: failed.then_some(124),
                });
                for width in [240.0, 480.0, 1200.0] {
                    let input = egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 300.0),
                        )),
                        ..Default::default()
                    };
                    let _ = context.run(input, |context| {
                        egui::CentralPanel::default().show(context, |ui| {
                            let right = ui.max_rect().right();
                            for response in [
                                receive_status(ui, &studio).unwrap(),
                                receive_buffer_status(ui, &studio).unwrap(),
                            ] {
                                assert!(response.rect.right() <= right + 0.1);
                                assert!(response.rect.height() < 40.0);
                            }
                        });
                    });
                    assert!(studio.follow_live);
                    assert_eq!(studio.live_session, Some(123));
                }
                let color = receive_status_color(&studio);
                if failed {
                    assert_eq!(color, egui::Color32::from_rgb(239, 126, 126));
                } else if queued_bytes >= 256 << 20 {
                    assert_ne!(color, MUTED);
                    assert_ne!(color, egui::Color32::from_rgb(239, 126, 126));
                } else {
                    assert_eq!(color, MUTED);
                }
            }
        }
    }

    #[test]
    fn receive_storage_controls_fit_both_languages_in_narrow_panels() {
        let context = egui::Context::default();
        configure(&context);
        for language in [Language::English, Language::Chinese] {
            let mut preferences = Preferences {
                language,
                ..Default::default()
            };
            for width in [240.0, 480.0] {
                for enabled in [false, true] {
                    let input = egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 600.0),
                        )),
                        ..Default::default()
                    };
                    let _ = context.run(input, |context| {
                        egui::CentralPanel::default().show(context, |ui| {
                            let right = ui.max_rect().right();
                            let controls = ui.add_enabled_ui(enabled, |ui| {
                                receive_storage_settings(ui, &mut preferences);
                            });
                            assert!(controls.response.rect.right() <= right + 0.1);
                        });
                    });
                    assert_eq!(preferences.receive_budget_gib, 0);
                    assert_eq!(preferences.min_free_disk_mib, 512);
                }
            }
        }
    }

    #[test]
    fn property_rows_reserve_actual_action_width_without_accumulating_overflow() {
        let context = egui::Context::default();
        configure(&context);
        for language in [Language::English, Language::Chinese] {
            for width in [150.0, 260.0, 400.0] {
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(600.0, 2400.0),
                    )),
                    ..Default::default()
                };
                let _ = context.run(input, |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        ui.set_width(width);
                        let right = ui.max_rect().right();
                        for row in 0..60 {
                            let response = property_value_row(
                                ui,
                                |ui| {
                                    let _ = ui.small_button(language.text("Copy", "复制"));
                                    ui.menu_button(language.text("Plot", "曲线"), |_| {});
                                },
                                |ui| match row % 3 {
                                    0 => {
                                        ui.add(
                                            egui::Label::new("1234567890 ".repeat(40)).truncate(),
                                        );
                                    }
                                    1 => {
                                        ui.add(egui::Link::new("→ 0xffffffffffffffff"));
                                    }
                                    _ => {
                                        ui.menu_button(
                                            language
                                                .text("Array · 100000 values", "数组 · 100000 项"),
                                            |_| {},
                                        );
                                    }
                                },
                            );
                            assert!(
                                response.rect.right() <= right + 0.1,
                                "row {row}: {:?}",
                                response.rect
                            );
                            assert!(ui.min_rect().right() <= right + 0.1);
                        }
                    });
                });
            }
        }
    }

    #[test]
    fn translated_tabs_keep_widget_state_and_distinct_identifiers() {
        let context = egui::Context::default();
        let mut studio = Studio::default();
        let mut viewport = Viewport::default();
        let mut images = Assets::<Image>::default();
        let mut viewer = Viewer {
            studio: &mut studio,
            viewport: &mut viewport,
            images: &mut images,
        };
        let mut identifiers = std::collections::HashSet::new();
        for (index, mut panel) in [
            Panel::Objects,
            Panel::Viewport,
            Panel::Inspector,
            Panel::Timeline,
            Panel::Clips,
            Panel::Performance,
            Panel::Memory,
            Panel::Curves,
            Panel::Diagnostics,
            Panel::Display,
        ]
        .into_iter()
        .enumerate()
        {
            viewer.studio.prefs.language = Language::English;
            let identifier = viewer.id(&mut panel);
            let english = viewer.title(&mut panel).text().to_owned();
            assert!(identifiers.insert(identifier));
            context.data_mut(|data| data.insert_temp(identifier, index));

            viewer.studio.prefs.language = Language::Chinese;
            assert_ne!(viewer.title(&mut panel).text(), english);
            assert_eq!(viewer.id(&mut panel), identifier);
            assert_eq!(
                context.data(|data| data.get_temp::<usize>(viewer.id(&mut panel))),
                Some(index)
            );
        }
    }

    #[test]
    fn focused_text_edit_owns_navigation_keys() {
        let context = egui::Context::default();
        let id = egui::Id::new("search-field");
        let mut text = String::from("物理场景");
        let _ = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                ui.add(egui::TextEdit::singleline(&mut text).id(id))
                    .request_focus();
            });
        });
        let input = egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::F,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        let _ = context.run(input.clone(), |context| {
            assert!(context.wants_keyboard_input());
            assert!(!shortcut(context, egui::Key::F));
            assert!(!shortcut(context, egui::Key::ArrowLeft));
        });
        context.memory_mut(|memory| memory.surrender_focus(id));
        let _ = context.run(egui::RawInput::default(), |_| {});
        let _ = context.run(input, |context| {
            assert!(shortcut(context, egui::Key::F));
        });
    }

    #[test]
    fn navigation_modifiers_and_same_frame_text_commit_do_not_trigger_shortcuts() {
        let context = egui::Context::default();
        let keys = [
            egui::Key::F,
            egui::Key::Space,
            egui::Key::ArrowLeft,
            egui::Key::ArrowRight,
        ];
        let events = |modifiers| {
            keys.into_iter()
                .map(|key| egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                })
                .collect()
        };
        for modifiers in [
            egui::Modifiers {
                ctrl: true,
                ..Default::default()
            },
            egui::Modifiers {
                alt: true,
                ..Default::default()
            },
            egui::Modifiers {
                command: true,
                ..Default::default()
            },
        ] {
            let _ = context.run(
                egui::RawInput {
                    modifiers,
                    events: events(modifiers),
                    ..Default::default()
                },
                |context| {
                    crate::camera_controller::begin_frame(context);
                    for key in keys {
                        assert!(!shortcut(context, key));
                    }
                },
            );
        }
        let id = egui::Id::new("commit-field");
        let mut text = String::from("场景");
        let _ = context.run(egui::RawInput::default(), |context| {
            crate::camera_controller::begin_frame(context);
            egui::CentralPanel::default().show(context, |ui| {
                ui.add(egui::TextEdit::singleline(&mut text).id(id))
                    .request_focus();
            });
        });
        let _ = context.run(
            egui::RawInput {
                events: events(egui::Modifiers::NONE),
                ..Default::default()
            },
            |context| {
                crate::camera_controller::begin_frame(context);
                context.memory_mut(|memory| memory.surrender_focus(id));
                assert!(!context.wants_keyboard_input());
                for key in keys {
                    assert!(!shortcut(context, key));
                }
            },
        );
        let mut committed = events(egui::Modifiers::NONE);
        committed.push(egui::Event::Ime(egui::ImeEvent::Commit("场景".into())));
        let _ = context.run(
            egui::RawInput {
                events: committed,
                ..Default::default()
            },
            |context| {
                crate::camera_controller::begin_frame(context);
                for key in keys {
                    assert!(!shortcut(context, key));
                }
            },
        );
        let _ = context.run(
            egui::RawInput {
                events: events(egui::Modifiers::NONE),
                ..Default::default()
            },
            |context| {
                crate::camera_controller::begin_frame(context);
                for key in keys {
                    assert!(shortcut(context, key));
                }
            },
        );
    }
}
