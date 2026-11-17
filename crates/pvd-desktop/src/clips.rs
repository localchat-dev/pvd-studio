use crate::{file_picker::PickerKind, studio::Studio};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

use crate::clip_store::{MAX_CLIPS, MAX_NAME_CHARS};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clip {
    pub name: String,
    pub first: usize,
    pub last: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipSet {
    pub clips: Vec<Clip>,
    pub selected: usize,
}

impl ClipSet {
    pub fn add(&mut self, first: usize, last: usize) -> bool {
        if first > last || self.clips.len() >= MAX_CLIPS {
            return false;
        }
        self.selected = self.clips.len();
        self.clips.push(Clip {
            name: format!("clip-{:02}", self.clips.len() + 1),
            first,
            last,
        });
        true
    }

    pub fn split(&mut self, frame: usize) -> bool {
        let Some(clip) = self.clips.get(self.selected) else {
            return false;
        };
        if frame < clip.first || frame >= clip.last || self.clips.len() >= MAX_CLIPS {
            return false;
        }
        let right = Clip {
            name: split_name(&clip.name, "-b"),
            first: frame + 1,
            last: clip.last,
        };
        let left = &mut self.clips[self.selected];
        left.name = split_name(&left.name, "-a");
        left.last = frame;
        self.clips.insert(self.selected + 1, right);
        true
    }
}

fn split_name(name: &str, suffix: &str) -> String {
    format!(
        "{}{suffix}",
        name.chars()
            .take(MAX_NAME_CHARS - suffix.len())
            .collect::<String>()
    )
}

pub enum PendingAction {
    Close,
    Open(PathBuf),
    Listen,
    Exit,
}

enum Completion {
    Load(Result<crate::clip_store::Loaded, crate::clip_store::Error>),
    Save(
        Result<crate::clip_store::Binding, crate::clip_store::Error>,
        ClipSet,
    ),
}

struct Job {
    receiver: Mutex<mpsc::Receiver<Completion>>,
    cancelled: Arc<AtomicBool>,
    saving: bool,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub struct Persistence {
    pub binding: Option<crate::clip_store::Binding>,
    pub issue: Option<crate::i18n::Message>,
    saved: ClipSet,
    job: Option<Job>,
}

impl Persistence {
    pub fn dirty(&self, definitions: &ClipSet) -> bool {
        self.saved.clips != definitions.clips
    }
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }
    pub fn needs_confirmation(&self, definitions: &ClipSet) -> bool {
        self.dirty(definitions) || self.job.as_ref().is_some_and(|job| job.saving)
    }
    pub fn discard(&mut self, definitions: &mut ClipSet) {
        self.job = None;
        *definitions = self.saved.clone();
    }
    pub fn cancel(&mut self) {
        self.job = None;
        self.issue = Some(crate::i18n::Message::new(
            "Cancellation requested. A save that already committed may remain on disk; reopen the recording to check. Unsaved edits are retained.",
            "已请求取消。已经提交的保存可能仍在磁盘上，请重开录制核对。尚未保存的编辑仍保留。",
        ));
    }
    pub fn accept_load(
        &mut self,
        result: Result<crate::clip_store::Loaded, crate::clip_store::Error>,
        definitions: &mut ClipSet,
    ) -> Option<crate::i18n::Message> {
        *self = Self::default();
        *definitions = ClipSet::default();
        match result {
            Ok(loaded) => {
                *definitions = loaded.definitions;
                self.saved = definitions.clone();
                let message = loaded.existed.then(|| {
                    crate::i18n::Message::new(
                        format!(
                            "Restored {} saved clip definitions",
                            definitions.clips.len()
                        ),
                        format!("已恢复 {} 个已保存片段定义", definitions.clips.len()),
                    )
                });
                self.binding = Some(loaded.binding);
                message
            }
            Err(error) => {
                let message = error.message();
                self.issue = Some(message.clone());
                Some(message)
            }
        }
    }
    pub fn bind_complete_capture(
        &mut self,
        path: Option<PathBuf>,
        bytes: Arc<Vec<u8>>,
        frame_count: usize,
        context: egui::Context,
        background: &crate::background_jobs::BackgroundJobs,
    ) {
        *self = Self::default();
        let Some(path) = path.filter(|path| {
            path.extension()
                .is_none_or(|extension| extension != "partial")
        }) else {
            self.issue = Some(crate::i18n::Message::new(
                "Save the completed recording and reopen it to persist clip definitions.",
                "请保存完整录制并重新打开，再保存片段定义。",
            ));
            return;
        };
        let (sender, receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancelled.clone();
        let spawn = background.spawn("load-clip-definitions", cancelled.clone(), move || {
            let result = crate::clip_store::load(&path, &bytes, frame_count, &worker_cancel);
            if !worker_cancel.load(Ordering::Relaxed) {
                let _ = sender.send(Completion::Load(result));
            }
            context.request_repaint();
        });
        if let Err(error) = spawn {
            self.issue = Some(crate::i18n::Message::detail(
                "Cannot load clip definitions",
                "无法加载片段定义",
                error,
            ));
            return;
        }
        self.job = Some(Job {
            receiver: Mutex::new(receiver),
            cancelled,
            saving: false,
        });
    }
    pub fn save(
        &mut self,
        definitions: &ClipSet,
        context: egui::Context,
        background: &crate::background_jobs::BackgroundJobs,
    ) {
        if self.busy() {
            return;
        }
        let Some(binding) = self.binding.clone() else {
            return;
        };
        let definitions = definitions.clone();
        let (sender, receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancelled.clone();
        let spawn = background.spawn("save-clip-definitions", cancelled.clone(), move || {
            let result = crate::clip_store::save(&binding, &definitions, &worker_cancel);
            let _ = sender.send(Completion::Save(result, definitions));
            context.request_repaint();
        });
        if let Err(error) = spawn {
            self.issue = Some(crate::i18n::Message::detail(
                "Cannot save clip definitions",
                "无法保存片段定义",
                error,
            ));
            return;
        }
        self.issue = None;
        self.job = Some(Job {
            receiver: Mutex::new(receiver),
            cancelled,
            saving: true,
        });
    }
    pub fn poll(&mut self, definitions: &mut ClipSet) -> Option<crate::i18n::Message> {
        let result = self.job.as_ref()?.receiver.lock().ok()?.try_recv();
        let result = match result {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.job = None;
                let message = crate::i18n::Message::new(
                    "The clip definition worker stopped before returning a result; reopen the capture before retrying.",
                    "片段定义工作线程未返回结果便已停止；请重新打开录制后重试。",
                );
                self.issue = Some(message.clone());
                return Some(message);
            }
        };
        self.job = None;
        match result {
            Completion::Load(result) => self.accept_load(result, definitions),
            Completion::Save(Ok(binding), saved) => {
                let path = crate::clip_store::sidecar_path(&binding.capture_path);
                self.binding = Some(binding);
                self.saved = saved;
                self.issue = None;
                Some(crate::i18n::Message::new(
                    format!("Saved clip definitions to {}", path.display()),
                    format!("片段定义已保存至 {}", path.display()),
                ))
            }
            Completion::Save(Err(error), _) => {
                let message = error.message();
                self.issue = Some(message.clone());
                Some(message)
            }
        }
    }
}

pub fn show_pending_action(context: &egui::Context, studio: &mut Studio) -> bool {
    if studio.exit_requested {
        crate::camera_controller::cancel_gesture(context);
        studio.playing = 0;
        context.request_repaint_after(std::time::Duration::from_millis(50));
        egui::Modal::new(egui::Id::new("stopping-recording-exports")).show(context, |ui| {
            let language = studio.prefs.language;
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!(
                    "{} · {:.1} {}",
                    language.text("Closing", "正在退出"),
                    studio.shutdown_elapsed().as_secs_f64(),
                    language.text("s", "秒"),
                ));
            });
            for phase in studio.shutdown_phases() {
                ui.label(phase);
            }
            if studio.shutdown_cleanup_pending() {
                ui.weak(language.text(
                    "Remaining temporary files will be cleaned up on the next startup or recording.",
                    "剩余临时文件将在下次启动或开始录制时继续清理。",
                ));
            }
        });
        return true;
    }
    if studio.pending_clip_action.is_none() {
        return false;
    }
    crate::camera_controller::cancel_gesture(context);
    studio.playing = 0;
    let language = studio.prefs.language;
    let modal = egui::Modal::new(egui::Id::new("unsaved-clip-definitions")).show(context, |ui| {
        ui.set_max_width(460.0);
        ui.heading(language.text("Unsaved clip definitions", "片段定义尚未保存"));
        ui.label(language.text("Continuing will discard the unsaved clip names and ranges. The recording file will remain unchanged. Keep editing to save definitions from the Clips panel first.", "继续将放弃尚未保存的片段名称和范围，录制文件不会改变。可选择继续编辑，先在片段面板保存定义。"));
        ui.horizontal(|ui| {
            let keep = ui.button(language.text("Keep editing", "继续编辑")).clicked();
            let discard = ui.button(language.text("Discard and continue", "放弃修改并继续")).clicked();
            (keep, discard)
        }).inner
    });
    if modal.inner.0 || modal.should_close() {
        studio.request_ui_action(
            context,
            "clip-confirmation",
            crate::studio::UiAction::ResolveClip { discard: false },
        );
    } else if modal.inner.1 {
        studio.request_ui_action(
            context,
            "clip-confirmation",
            crate::studio::UiAction::ResolveClip { discard: true },
        );
    }
    true
}

pub fn handle_window_close(
    mut requests: bevy::prelude::MessageReader<bevy::window::WindowCloseRequested>,
    primary: bevy::prelude::Query<
        bevy::prelude::Entity,
        bevy::prelude::With<bevy::window::PrimaryWindow>,
    >,
    mut studio: bevy::prelude::ResMut<Studio>,
    mut exit: bevy::prelude::MessageWriter<bevy::app::AppExit>,
    mut exit_sent: bevy::prelude::Local<bool>,
) {
    for request in requests.read() {
        if !studio.exit_requested
            && primary.get(request.window).is_ok()
            && studio.allow_clip_transition(PendingAction::Exit)
        {
            studio.exit_requested = true;
        }
    }
    if studio.exit_requested && !*exit_sent && studio.poll_shutdown() {
        *exit_sent = true;
        exit.write(bevy::app::AppExit::Success);
    }
}

pub fn show(ui: &mut egui::Ui, studio: &mut Studio) {
    ui.scope(|ui| {
        ui.style_mut().spacing.scroll.floating = false;
        egui::ScrollArea::vertical()
            .id_salt("clips-panel")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
            .auto_shrink([false, false])
            .show(ui, |ui| show_contents(ui, studio));
    });
}

fn show_contents(ui: &mut egui::Ui, studio: &mut Studio) {
    crate::recording_export::retained_controls(ui, studio);
    if studio.is_native_recording() {
        return;
    }
    let language = studio.prefs.language;
    let count = studio.count();
    let can_export = studio.can_trim_capture();
    let editable = studio.job.is_none()
        && studio.server.is_none()
        && studio.clip_persistence.binding.is_some()
        && !studio.clip_persistence.busy();
    ui.vertical(|ui| {
        ui.label(format!(
            "{} ({}/{MAX_CLIPS})",
            language.text("Clips", "片段"),
            studio.clip_set.clips.len()
        ));
        ui.horizontal_wrapped(|ui| {
            let dirty = studio.clip_persistence.dirty(&studio.clip_set);
            ui.label(if studio.clip_persistence.busy() {
                language.text("Reading / saving…", "正在读写…")
            } else if dirty {
                language.text("Unsaved changes", "有未保存修改")
            } else {
                language.text("No unsaved changes", "无未保存修改")
            });
            let mut save = ui.add_enabled(
                editable && dirty,
                egui::Button::new(language.text("Save definitions", "保存片段定义")),
            );
            if let Some(binding) = &studio.clip_persistence.binding {
                let path = crate::clip_store::sidecar_path(&binding.capture_path)
                    .display()
                    .to_string();
                save = save.on_hover_text(&path).on_disabled_hover_text(path);
            }
            if save.clicked() {
                studio.clip_persistence.save(
                    &studio.clip_set,
                    ui.ctx().clone(),
                    &studio.background_jobs,
                );
            }
            if studio.clip_persistence.busy()
                && ui.button(language.text("Cancel", "取消")).clicked()
            {
                studio.clip_persistence.cancel();
            }
        });
        if let Some(issue) = &studio.clip_persistence.issue {
            ui.colored_label(egui::Color32::LIGHT_RED, issue.text(language));
        }
        if !editable && !studio.clip_persistence.busy() {
            ui.label(language.text(
                "Definition editing requires a complete file capture and a stopped listener.",
                "编辑片段定义需要已完整打开的录制文件，且监听已停止。",
            ));
        }
        ui.add_enabled_ui(editable, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(language.text("Output", "输出"));
                ui.add(egui::TextEdit::singleline(&mut studio.export_path).desired_width(260.0));
                if ui
                    .add_enabled(
                        !studio.file_picker.is_busy(),
                        egui::Button::new(language.text("Choose path…", "选择路径…")),
                    )
                    .clicked()
                {
                    studio.pick_file(PickerKind::SaveClip, ui.ctx().clone());
                }
            });
        });
        ui.add_enabled_ui(editable, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::DragValue::new(&mut studio.trim_first)
                        .range(0..=studio.trim_last.min(count.saturating_sub(1))),
                );
                ui.label("→");
                ui.add(
                    egui::DragValue::new(&mut studio.trim_last)
                        .range(studio.trim_first..=count.saturating_sub(1)),
                );
                if ui
                    .add_enabled(
                        studio.clip_set.clips.len() < MAX_CLIPS && studio.count() > 0,
                        egui::Button::new(language.text("Add range", "添加范围")),
                    )
                    .on_hover_text(
                        language.text("Includes both endpoint frames", "包含起始帧和结束帧"),
                    )
                    .clicked()
                {
                    studio.clip_set.add(studio.trim_first, studio.trim_last);
                }
                let can_split = studio
                    .clip_set
                    .clips
                    .get(studio.clip_set.selected)
                    .is_some_and(|clip| (clip.first..clip.last).contains(&studio.frame));
                if ui
                    .add_enabled(
                        can_split && studio.clip_set.clips.len() < MAX_CLIPS,
                        egui::Button::new(language.text("Split at playhead", "在播放头分割")),
                    )
                    .on_hover_text(language.text(
                        "The playhead frame stays in the left clip",
                        "当前帧保留在左侧片段",
                    ))
                    .clicked()
                {
                    studio.clip_set.split(studio.frame);
                }
            });
        });
        let mut jump = None;
        let mut remove = None;
        let mut save = None;
        ui.add_enabled_ui(editable, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("clip-ranges")
                .max_height(150.0)
                .show_rows(ui, 28.0, studio.clip_set.clips.len(), |ui, range| {
                    for index in range {
                        let clip = &mut studio.clip_set.clips[index];
                        ui.horizontal(|ui| {
                            if ui
                                .selectable_label(
                                    studio.clip_set.selected == index,
                                    format!("{}", index + 1),
                                )
                                .clicked()
                            {
                                studio.clip_set.selected = index;
                            }
                            ui.add(
                                egui::TextEdit::singleline(&mut clip.name)
                                    .desired_width(110.0)
                                    .char_limit(80),
                            );
                            ui.add(
                                egui::DragValue::new(&mut clip.first)
                                    .range(0..=clip.last.min(count.saturating_sub(1))),
                            );
                            ui.label("→");
                            ui.add(
                                egui::DragValue::new(&mut clip.last)
                                    .range(clip.first..=count.saturating_sub(1)),
                            );
                            ui.label(format!(
                                "{} {}",
                                clip.last - clip.first + 1,
                                language.text("frames", "帧")
                            ));
                            if ui.small_button(language.text("Go", "定位")).clicked() {
                                jump = Some(clip.first);
                            }
                            if ui
                                .add_enabled(
                                    can_export,
                                    egui::Button::new(language.text("Export clip", "另存片段")),
                                )
                                .on_hover_text(language.text(
                                    "Uses the output path above; existing files are preserved.",
                                    "使用上方输出路径；已有文件会保留。",
                                ))
                                .clicked()
                            {
                                save = Some((clip.first, clip.last));
                            }
                            if ui
                                .small_button("×")
                                .on_hover_text(language.text(
                                    "Remove this range; keep the recording file",
                                    "移除此范围，保留录制文件",
                                ))
                                .clicked()
                            {
                                remove = Some(index);
                            }
                        });
                    }
                });
        });
        if let Some(index) = remove {
            studio.clip_set.clips.remove(index);
            studio.clip_set.selected = studio
                .clip_set
                .selected
                .min(studio.clip_set.clips.len().saturating_sub(1));
        }
        if let Some(frame) = jump {
            studio.request_ui_action(
                ui.ctx(),
                "clip-jump",
                crate::studio::UiAction::Locate(frame, crate::studio::DisplayReason::Clip),
            );
        }
        if let Some((first, last)) = save {
            studio.trim_first = first;
            studio.trim_last = last;
            studio.trim_capture(ui.ctx().clone());
        }
        if let Some(clip) = studio
            .clip_set
            .clips
            .get_mut(studio.clip_set.selected)
            .filter(|_| editable)
        {
            ui.horizontal(|ui| {
                let contained = (clip.first..=clip.last).contains(&studio.frame);
                if ui
                    .add_enabled(
                        contained,
                        egui::Button::new(language.text("Trim before playhead", "裁去播放头左侧")),
                    )
                    .clicked()
                {
                    clip.first = studio.frame;
                }
                if ui
                    .add_enabled(
                        contained,
                        egui::Button::new(language.text("Trim after playhead", "裁去播放头右侧")),
                    )
                    .clicked()
                {
                    clip.last = studio.frame;
                }
            });
        }
        crate::recording_export::show_progress(ui, studio);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_names_keep_unicode_character_limit() {
        let mut set = ClipSet::default();
        set.add(0, 10);
        set.clips[0].name = "界".repeat(80);
        assert!(set.split(5));
        assert_eq!(set.clips[0].name.chars().count(), 80);
        assert_eq!(set.clips[1].name.chars().count(), 80);
        assert!(set.clips[0].name.ends_with("-a"));
        assert!(set.clips[1].name.ends_with("-b"));
    }

    #[test]
    fn dirty_definitions_guard_close_open_listener_and_preserve_first_action() {
        let mut studio = Studio::default();
        studio.source = "current recording".into();
        studio.clip_set.add(10, 30);
        studio.playing = 1;
        studio.close();
        assert!(matches!(
            studio.pending_clip_action,
            Some(PendingAction::Close)
        ));
        assert_eq!(studio.source, "current recording");
        assert_eq!(studio.clip_set.clips.len(), 1);
        assert_eq!(studio.playing, 0);
        studio.open(PathBuf::from("another.pvdstream"), egui::Context::default());
        studio.start_listener(egui::Context::default());
        assert!(matches!(
            studio.pending_clip_action,
            Some(PendingAction::Close)
        ));
        assert!(studio.job.is_none());
        assert!(studio.server.is_none());
        studio.pending_clip_action = None;
        studio.open(PathBuf::from("another.pvdstream"), egui::Context::default());
        assert!(
            matches!(&studio.pending_clip_action, Some(PendingAction::Open(path)) if path == &PathBuf::from("another.pvdstream"))
        );
        assert!(studio.job.is_none());
        studio.pending_clip_action = None;
        studio.start_listener(egui::Context::default());
        assert!(matches!(
            studio.pending_clip_action,
            Some(PendingAction::Listen)
        ));
        assert!(studio.server.is_none());
        studio.pending_clip_action = None;
        studio.clip_persistence.discard(&mut studio.clip_set);
        studio.close();
        assert!(studio.source.is_empty());
        assert!(studio.clip_set.clips.is_empty());
    }

    #[test]
    fn modal_escape_preserves_edits_and_blocks_keyboard_input() {
        let context = egui::Context::default();
        let mut studio = Studio::default();
        studio.clip_set.add(0, 5);
        studio.close();
        let frame = |studio: &mut Studio, events| {
            let _ = context.run(
                egui::RawInput {
                    events,
                    focused: true,
                    ..Default::default()
                },
                |context| {
                    assert!(show_pending_action(context, studio));
                    assert!(crate::camera_controller::keyboard_input_blocked(context));
                },
            );
        };
        frame(&mut studio, Vec::new());
        assert!(studio.pending_clip_action.is_some());
        frame(
            &mut studio,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(studio.pending_clip_action.is_some());
        studio.apply_ui_actions(&context);
        assert!(studio.pending_clip_action.is_none());
        assert_eq!(studio.clip_set.clips.len(), 1);
        assert!(studio.clip_persistence.dirty(&studio.clip_set));
    }

    #[test]
    fn native_close_only_targets_primary_and_cancel_keeps_window_alive() {
        use bevy::{
            app::AppExit,
            ecs::message::Messages,
            prelude::*,
            window::{PrimaryWindow, WindowCloseRequested},
        };
        let mut app = App::new();
        app.add_message::<WindowCloseRequested>()
            .add_message::<AppExit>()
            .insert_resource(Studio::default())
            .add_systems(Update, handle_window_close);
        let primary = app
            .world_mut()
            .spawn((Window::default(), PrimaryWindow))
            .id();
        let secondary = app.world_mut().spawn(Window::default()).id();
        app.world_mut().resource_mut::<Studio>().clip_set.add(0, 5);
        app.world_mut()
            .write_message(WindowCloseRequested { window: secondary });
        app.update();
        assert!(
            app.world()
                .resource::<Studio>()
                .pending_clip_action
                .is_none()
        );
        assert!(app.world().resource::<Messages<AppExit>>().is_empty());
        for _ in 0..3 {
            app.world_mut()
                .write_message(WindowCloseRequested { window: primary });
        }
        app.update();
        assert!(matches!(
            app.world().resource::<Studio>().pending_clip_action,
            Some(PendingAction::Exit)
        ));
        assert!(app.world().resource::<Messages<AppExit>>().is_empty());
        app.world_mut().resource_mut::<Studio>().pending_clip_action = None;
        app.update();
        assert!(app.world().get::<Window>(primary).is_some());
        assert!(app.world().resource::<Messages<AppExit>>().is_empty());
        assert_eq!(app.world().resource::<Studio>().clip_set.clips.len(), 1);
        app.world_mut()
            .write_message(WindowCloseRequested { window: primary });
        app.update();
        assert!(matches!(
            app.world().resource::<Studio>().pending_clip_action,
            Some(PendingAction::Exit)
        ));
        {
            let mut studio = app.world_mut().resource_mut::<Studio>();
            studio.pending_clip_action = None;
            studio.clip_set = ClipSet::default();
        }
        for _ in 0..3 {
            app.world_mut()
                .write_message(WindowCloseRequested { window: primary });
        }
        app.update();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.world().resource::<Messages<AppExit>>().is_empty() {
            assert!(std::time::Instant::now() < deadline);
            app.update();
            std::thread::yield_now();
        }
        assert_eq!(
            app.world_mut()
                .resource_mut::<Messages<AppExit>>()
                .drain()
                .count(),
            1
        );
        app.update();
        assert!(app.world().resource::<Messages<AppExit>>().is_empty());
    }

    #[test]
    fn persistence_cancel_drops_stale_results_and_live_partial_is_not_bound() {
        let mut persistence = Persistence::default();
        let mut definitions = ClipSet::default();
        definitions.add(0, 5);
        let (sender, receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        persistence.job = Some(Job {
            receiver: Mutex::new(receiver),
            cancelled: cancelled.clone(),
            saving: true,
        });
        assert!(persistence.needs_confirmation(&ClipSet::default()));
        persistence.discard(&mut definitions);
        assert!(cancelled.load(Ordering::Relaxed));
        assert!(
            sender
                .send(Completion::Load(Err(crate::clip_store::Error::Cancelled)))
                .is_err()
        );
        assert!(persistence.poll(&mut definitions).is_none());
        assert!(definitions.clips.is_empty());
        for path in [None, Some(PathBuf::from("capture.pvdstream.partial"))] {
            persistence.bind_complete_capture(
                path,
                Arc::default(),
                0,
                egui::Context::default(),
                &crate::background_jobs::BackgroundJobs::default(),
            );
            assert!(!persistence.busy());
            assert!(persistence.binding.is_none());
            assert!(persistence.issue.is_some());
        }
    }

    #[test]
    fn cancelling_a_definition_save_keeps_edits_and_worker_failure_is_reported() {
        let mut persistence = Persistence::default();
        let mut definitions = ClipSet::default();
        definitions.add(2, 6);
        let expected = definitions.clone();
        let (sender, receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        persistence.job = Some(Job {
            receiver: Mutex::new(receiver),
            cancelled: cancelled.clone(),
            saving: true,
        });
        persistence.cancel();
        assert!(cancelled.load(Ordering::Relaxed));
        assert!(persistence.poll(&mut definitions).is_none());
        assert_eq!(definitions, expected);
        assert!(persistence.dirty(&definitions));
        assert!(
            sender
                .send(Completion::Load(Err(crate::clip_store::Error::Cancelled)))
                .is_err()
        );
        let (sender, receiver) = mpsc::channel();
        persistence.job = Some(Job {
            receiver: Mutex::new(receiver),
            cancelled: Arc::new(AtomicBool::new(false)),
            saving: false,
        });
        drop(sender);
        let message = persistence.poll(&mut definitions).unwrap();
        assert!(
            message
                .text(crate::i18n::Language::English)
                .contains("worker stopped")
        );
        assert!(!persistence.busy());
        assert_eq!(definitions, expected);
    }

    #[test]
    fn splitting_partitions_inclusive_ranges_without_duplication_or_empty_clips() {
        let mut set = ClipSet::default();
        assert!(set.add(10, 20));
        assert!(!set.split(9));
        assert!(!set.split(20));
        assert!(set.split(15));
        assert_eq!((set.clips[0].first, set.clips[0].last), (10, 15));
        assert_eq!((set.clips[1].first, set.clips[1].last), (16, 20));
        assert_eq!(
            set.clips
                .iter()
                .map(|clip| clip.last - clip.first + 1)
                .sum::<usize>(),
            11
        );
        set.selected = 1;
        assert!(set.split(16));
        assert_eq!((set.clips[1].first, set.clips[1].last), (16, 16));
        assert!(!set.split(16));
        while set.clips.len() < MAX_CLIPS {
            assert!(set.add(0, 1));
        }
        assert!(!set.add(0, 1));
        assert!(!set.split(0));
    }
}
