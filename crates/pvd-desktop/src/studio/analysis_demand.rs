use super::{Panel, Studio, playback_recovery};
use crate::i18n::Message;

#[derive(Default)]
pub(super) enum DisplayedAnalysis {
    #[default]
    Complete,
    Frame(usize),
    Deferred,
    Failed {
        frame: usize,
        message: Message,
    },
}

impl Studio {
    pub(crate) fn analysis_ready(&self) -> bool {
        match self.displayed_analysis {
            DisplayedAnalysis::Complete => true,
            DisplayedAnalysis::Frame(frame) => frame == self.frame,
            DisplayedAnalysis::Deferred | DisplayedAnalysis::Failed { .. } => false,
        }
    }

    pub(crate) fn update_analysis_demand(&mut self) {
        let complete_source = self.complete_profile_source().is_some();
        self.analysis_demand = self.playing == 0
            && (self.prefs.dock.iter_all_nodes().any(|(_, node)| {
                if let egui_dock::Node::Leaf(leaf) = node {
                    leaf.tabs
                        .get(leaf.active.0)
                        .is_some_and(|panel| needs_analysis(panel, complete_source))
                } else {
                    false
                }
            }) || self
                .requested_panel
                .as_ref()
                .is_some_and(|panel| needs_analysis(panel, complete_source))
                || self.curves_need_analysis());
        if self.analysis_demand {
            self.request_current_analysis();
        }
    }

    pub(super) fn request_current_analysis(&mut self) {
        if self.analysis_ready()
            || self.shutdown_started
            || self.playing != 0
            || self.seeking.is_some()
            || self.follow_live
            || self.recording.is_none()
            || matches!(self.displayed_analysis, DisplayedAnalysis::Failed { .. })
        {
            return;
        }
        if !self
            .timeline_recording()
            .is_some_and(|recording| recording.is_frame_retained(self.frame))
        {
            self.displayed_analysis = DisplayedAnalysis::Failed {
                frame: self.frame,
                message: Message::new(
                    "This frame has expired from disk history. Select a retained frame to load its analysis.",
                    "该帧已从磁盘历史淘汰。请选择仍保留的帧以加载分析。",
                ),
            };
            return;
        }
        self.seek_with_analysis(self.frame, playback_recovery::Intent::Manual, true, None);
        if self.seeking.is_none() && !self.analysis_ready() {
            self.displayed_analysis = DisplayedAnalysis::Failed {
                frame: self.frame,
                message: Message::new(
                    "Analysis could not be requested. Retry or select a retained frame.",
                    "无法请求该帧分析。请重试或选择仍保留的帧。",
                ),
            };
        }
    }

    pub(crate) fn curves_need_analysis(&self) -> bool {
        self.complete_profile_source().is_none()
            && self.curves.pending.iter().any(|id| {
                self.curves
                    .curves
                    .iter()
                    .any(|curve| curve.id == *id && curve.spec.needs_analysis())
            })
    }

    pub(crate) fn ensure_current_analysis(&mut self, ui: &mut egui::Ui) -> bool {
        if self.analysis_ready() {
            return true;
        }
        if self.playing != 0 {
            ui.label(self.prefs.language.text(
                "Pause playback to load analysis for the displayed frame.",
                "暂停回放后加载当前显示帧的分析。",
            ));
            return false;
        }
        self.analysis_demand = true;
        self.request_current_analysis();
        let language = self.prefs.language;
        ui.label(format!(
            "{} {}",
            language.text("Analysis frame", "分析帧"),
            self.frame
        ));
        if let DisplayedAnalysis::Failed { frame, message } = &self.displayed_analysis {
            ui.label(format!("{frame}: {}", message.text(language)));
            let retained = self
                .timeline_recording()
                .is_some_and(|recording| recording.is_frame_retained(*frame));
            let target = if retained {
                Some(*frame)
            } else {
                self.timeline_recording().and_then(|recording| {
                    recording
                        .frames
                        .iter()
                        .rev()
                        .find(|frame| recording.is_frame_retained(frame.index))
                        .map(|frame| frame.index)
                })
            };
            let label = if retained {
                language.text("Retry", "重试")
            } else {
                language.text("Load latest retained frame", "加载最新可用帧")
            };
            if ui
                .add_enabled(target.is_some(), egui::Button::new(label))
                .clicked()
            {
                self.displayed_analysis = DisplayedAnalysis::Deferred;
                self.seek_with_analysis(
                    target.unwrap(),
                    playback_recovery::Intent::Manual,
                    true,
                    None,
                );
            }
        } else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(language.text(
                    "Loading analysis for the displayed frame…",
                    "正在加载当前显示帧的分析…",
                ));
            });
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(16));
        }
        false
    }
}

fn needs_analysis(panel: &Panel, complete_source: bool) -> bool {
    matches!(panel, Panel::Memory) || matches!(panel, Panel::Performance) && !complete_source
}
