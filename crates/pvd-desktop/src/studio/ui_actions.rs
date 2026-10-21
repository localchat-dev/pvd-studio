use super::{DisplayReason, Studio};
use crate::{clips::PendingAction, i18n::Message};
use std::{collections::VecDeque, hash::Hash, path::PathBuf};

const MAX_ACTIONS: usize = 32;
const MAX_PATH_BYTES: usize = 128 * 1024;

pub enum UiAction {
    Open(PathBuf),
    Close,
    PauseAndSeek(usize),
    Locate(usize, DisplayReason),
    Pause,
    Step(i32),
    TogglePlayback,
    ToggleReverse,
    ReturnLive,
    StartListener,
    StopListener,
    ResolveClip { discard: bool },
}

struct Request {
    frame: u64,
    origin: egui::Id,
    action: UiAction,
}

#[derive(Default)]
pub(super) struct UiActions(VecDeque<Request>);

impl Studio {
    pub fn request_ui_action(
        &mut self,
        context: &egui::Context,
        origin: impl Hash,
        action: UiAction,
    ) {
        if self.shutdown_started || self.exit_requested {
            return;
        }
        if self.pending_clip_action.is_some() && !matches!(action, UiAction::ResolveClip { .. }) {
            return;
        }
        if let UiAction::Open(path) = &action
            && path.capacity() > MAX_PATH_BYTES
        {
            self.notice(Message::new(
                "The capture path is too long.",
                "录制文件路径过长。",
            ));
            return;
        }
        let frame = context.cumulative_frame_nr();
        let origin = egui::Id::new(origin);
        // Layout retries belong to the same input frame; a control can refine its target once.
        if let Some(request) = self
            .ui_actions
            .0
            .iter_mut()
            .find(|request| request.frame == frame && request.origin == origin)
        {
            request.action = action;
        } else if self.ui_actions.0.len() < MAX_ACTIONS {
            self.ui_actions.0.push_back(Request {
                frame,
                origin,
                action,
            });
        } else {
            self.notice(Message::new(
                "Too many pending capture actions; try again after the next frame.",
                "待处理的录制操作过多，请在下一帧重试。",
            ));
        }
        context.request_repaint();
    }

    pub fn apply_ui_actions(&mut self, context: &egui::Context) {
        let requests = std::mem::take(&mut self.ui_actions.0);
        for request in requests {
            if self.shutdown_started || self.exit_requested {
                break;
            }
            if self.pending_clip_action.is_some()
                && !matches!(request.action, UiAction::ResolveClip { .. })
            {
                continue;
            }
            match request.action {
                UiAction::Open(path) => self.open(path, context.clone()),
                UiAction::Close => self.close(),
                UiAction::PauseAndSeek(frame) => self.seek_paused(frame, DisplayReason::Frame),
                UiAction::Locate(frame, reason) => self.seek_paused(frame, reason),
                UiAction::Pause => self.pause_display(),
                UiAction::Step(direction) => self.step_frame(direction),
                UiAction::TogglePlayback => self.toggle_playback(),
                UiAction::ToggleReverse => self.toggle_reverse(),
                UiAction::ReturnLive => self.return_to_live(),
                UiAction::StartListener => self.start_listener(context.clone()),
                UiAction::StopListener => self.stop_listener(),
                UiAction::ResolveClip { discard } => {
                    if let Some(action) = self.pending_clip_action.take()
                        && discard
                    {
                        self.clip_persistence.discard(&mut self.clip_set);
                        match action {
                            PendingAction::Close => self.close(),
                            PendingAction::Open(path) => self.open(path, context.clone()),
                            PendingAction::Listen => self.start_listener(context.clone()),
                            PendingAction::Exit => self.exit_requested = true,
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
