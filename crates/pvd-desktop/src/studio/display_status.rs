use super::{Studio, playback_recovery};
use crate::i18n::{Language, Message};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayMode {
    Empty,
    Waiting,
    Live,
    Paused,
    Forward,
    Reverse,
    Seeking,
}

impl DisplayMode {
    pub fn label(self, language: Language) -> &'static str {
        match self {
            Self::Empty => language.text("No capture", "未载入"),
            Self::Waiting => language.text("Waiting", "等待数据"),
            Self::Live => language.text("LIVE", "实时"),
            Self::Paused => language.text("PAUSED", "已暂停"),
            Self::Forward => language.text("PLAYING", "正放"),
            Self::Reverse => language.text("REVERSE", "倒放"),
            Self::Seeking => language.text("SEEKING", "正在定位"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayReason {
    Pause,
    PreviousFrame,
    NextFrame,
    Frame,
    Timeline,
    Performance,
    Curve,
    Memory,
    Clip,
    Error,
    Forward,
    Reverse,
    Disconnected,
    ListenerStopped,
    HistoryExpired,
    FirstFrame,
    LastFrame,
    SeekFailed,
}

impl DisplayReason {
    pub fn label(self, language: Language) -> &'static str {
        match self {
            Self::Pause => language.text("Pause command", "暂停操作"),
            Self::PreviousFrame => language.text("Previous-frame command", "上一帧操作"),
            Self::NextFrame => language.text("Next-frame command", "下一帧操作"),
            Self::Frame => language.text("Frame navigation", "帧号定位"),
            Self::Timeline => language.text("Timeline navigation", "时间轴定位"),
            Self::Performance => language.text("Performance event navigation", "性能事件定位"),
            Self::Curve => language.text("Curve navigation", "曲线定位"),
            Self::Memory => language.text("Memory event navigation", "内存事件定位"),
            Self::Clip => language.text("Clip navigation", "片段定位"),
            Self::Error => language.text("Error navigation", "错误定位"),
            Self::Forward => language.text("Forward playback", "开始正放"),
            Self::Reverse => language.text("Reverse playback", "开始倒放"),
            Self::Disconnected => language.text("Capture session ended", "捕获连接已结束"),
            Self::ListenerStopped => language.text("Listener stopped", "已停止监听"),
            Self::HistoryExpired => {
                language.text("History expired during loading", "加载期间历史已过期")
            }
            Self::FirstFrame => language.text("Reached first retained frame", "已到最早保留帧"),
            Self::LastFrame => language.text("Reached last frame", "已到末帧"),
            Self::SeekFailed => language.text("Frame loading failed", "帧加载失败"),
        }
    }
}

impl Studio {
    pub fn display_mode(&self) -> DisplayMode {
        if self.follow_live {
            if self.count() == 0 || self.source_selection_pending() {
                DisplayMode::Waiting
            } else {
                DisplayMode::Live
            }
        } else if self.playing > 0 {
            DisplayMode::Forward
        } else if self.playing < 0 {
            DisplayMode::Reverse
        } else if self.seeking.is_some() {
            DisplayMode::Seeking
        } else if self.count() > 0 || !self.state.objects.is_empty() {
            DisplayMode::Paused
        } else {
            DisplayMode::Empty
        }
    }

    pub fn display_detail(&self, language: Language) -> &'static str {
        match self.display_mode() {
            DisplayMode::Live => language.text("Following latest decoded frame", "跟随最新解码帧"),
            DisplayMode::Waiting => {
                language.text("Waiting for the selected frame source", "等待所选帧源")
            }
            DisplayMode::Forward | DisplayMode::Reverse => {
                language.text("Recorded history", "历史回放")
            }
            _ => self
                .display_reason
                .map_or("", |reason| reason.label(language)),
        }
    }

    pub fn display_position(&self, language: Language) -> String {
        if self.count() == 0 {
            return String::new();
        }
        let frame = self.frame_ordinal(self.frame);
        if let Some(index) = self.seeking {
            let target = self.frame_ordinal(index);
            return match language {
                Language::English => format!("Frame {frame} → {target}"),
                Language::Chinese => format!("帧 {frame} → {target}"),
            };
        }
        if self.live_connected
            && !self.follow_live
            && self
                .displayed_frame_source
                .zip(self.live_frame_source)
                .is_none_or(|((displayed, _), (live, _))| displayed == live)
        {
            let latest = self.frame_ordinal(self.live_frame);
            let behind = latest.saturating_sub(frame);
            return match language {
                Language::English => format!("Frame {frame} · {behind} behind live ({latest})"),
                Language::Chinese => format!("帧 {frame} · 落后 {behind} 帧 · 最新 {latest}"),
            };
        }
        match language {
            Language::English => format!("Frame {frame}"),
            Language::Chinese => format!("帧 {frame}"),
        }
    }

    pub(super) fn leave_live(&mut self, reason: DisplayReason) {
        if self.follow_live {
            self.reset_profile_views();
            self.notice(Message::new(
                format!("Left live view: {}", reason.label(Language::English)),
                format!("已退出实时：{}", reason.label(Language::Chinese)),
            ));
        }
        self.follow_live = false;
        self.display_reason = Some(reason);
    }

    pub(super) fn pause_display_for(&mut self, reason: DisplayReason) {
        self.leave_live(reason);
        self.playing = 0;
        self.cancel_seek();
        self.restore_display_frame_source();
    }

    pub(super) fn seek_paused(&mut self, target: usize, reason: DisplayReason) {
        if self.seeking == Some(target) {
            self.playing = 0;
            self.display_reason = Some(reason);
            self.playback_recovery.make_manual();
            return;
        }
        let with_analysis = self.analysis_demand;
        #[cfg(test)]
        let with_analysis = with_analysis || self.force_full_playback_analysis;
        if self.seek_with_analysis(
            target,
            playback_recovery::Intent::Manual,
            with_analysis,
            Some(reason),
        ) {
            self.playing = 0;
        }
    }
}
