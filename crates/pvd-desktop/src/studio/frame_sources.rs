use super::{DisplayReason, Studio};
use pvd_core::frame_sources::{FrameSourceKey, FrameSourceTimeline};

pub(super) type FrameIdentity = (FrameSourceKey, pvd_core::frame_sources::SourceFrame);

impl Studio {
    pub fn frame_source(&self) -> Option<FrameSourceKey> {
        self.selected_frame_source
            .or_else(|| self.timeline_recording()?.frame_sources.default_source())
    }

    pub fn source_timeline(&self) -> Option<&FrameSourceTimeline> {
        self.timeline_recording()?
            .frame_sources
            .source(self.frame_source()?)
    }

    pub fn playback_bounds(&self) -> Option<(usize, usize)> {
        if let Some(source) = self.source_timeline() {
            return Some((source.first()?.index, source.last()?.index));
        }
        let recording = self.timeline_recording()?;
        Some((
            recording.frames.first()?.index,
            recording.frames.last()?.index,
        ))
    }

    pub fn playback_ordinal_bounds(&self) -> Option<(usize, usize)> {
        if let Some(source) = self.source_timeline() {
            return Some((source.first()?.ordinal, source.last()?.ordinal));
        }
        self.playback_bounds()
    }

    pub fn frame_ordinal(&self, index: usize) -> usize {
        if let Some((_, frame)) = self
            .displayed_frame_source
            .filter(|(_, frame)| frame.index == index)
        {
            return frame.ordinal;
        }
        if let Some((_, frame)) = self
            .live_frame_source
            .filter(|(_, frame)| frame.index == index)
        {
            return frame.ordinal;
        }
        self.timeline_recording()
            .and_then(|recording| recording.frame_sources.frame(index))
            .map_or(index, |(_, frame)| frame.ordinal)
    }

    pub fn frame_at_ordinal(&self, ordinal: usize) -> Option<usize> {
        if let Some(source) = self.source_timeline() {
            return source.nearest_ordinal(ordinal).map(|frame| frame.index);
        }
        self.timeline_recording()?
            .frame_info(ordinal)
            .map(|frame| frame.index)
    }

    pub fn next_playback_frame(&self, direction: i32) -> Option<usize> {
        if let Some(source) = self.source_timeline() {
            return if direction < 0 {
                source.previous(self.frame)
            } else {
                source.next(self.frame)
            }
            .map(|frame| frame.index);
        }
        let recording = self.timeline_recording()?;
        if direction < 0 {
            recording
                .frames
                .iter()
                .rev()
                .find(|frame| frame.index < self.frame)
        } else {
            recording
                .frames
                .iter()
                .find(|frame| frame.index > self.frame)
        }
        .map(|frame| frame.index)
    }

    pub(super) fn retained_playback_bounds(&self) -> Option<(usize, usize)> {
        let recording = self.timeline_recording()?;
        if let Some(source) = self.source_timeline() {
            let first = source
                .frames()
                .find(|frame| recording.is_frame_retained(frame.index))?
                .index;
            let last = source
                .frames()
                .rev()
                .find(|frame| recording.is_frame_retained(frame.index))?
                .index;
            return Some((first, last));
        }
        let first = recording
            .frames
            .iter()
            .find(|frame| recording.is_frame_retained(frame.index))?
            .index;
        let last = recording
            .frames
            .iter()
            .rev()
            .find(|frame| recording.is_frame_retained(frame.index))?
            .index;
        Some((first, last))
    }

    pub(super) fn retained_playback_at_or_after(&self, target: usize) -> Option<usize> {
        let recording = self.timeline_recording()?;
        if let Some(source) = self.source_timeline() {
            return source
                .frames()
                .find(|frame| frame.index >= target && recording.is_frame_retained(frame.index))
                .map(|frame| frame.index);
        }
        recording
            .frames
            .iter()
            .find(|frame| frame.index >= target && recording.is_frame_retained(frame.index))
            .map(|frame| frame.index)
    }

    pub(super) fn remember_display_frame(&mut self, frame: usize) {
        self.frame = frame;
        self.displayed_frame_source = self
            .recording
            .as_ref()
            .and_then(|recording| recording.frame_sources.frame(frame))
            .or_else(|| self.timeline_recording()?.frame_sources.frame(frame))
            .map(|(source, frame)| (source, *frame));
    }

    pub(super) fn source_selection_pending(&self) -> bool {
        self.follow_live
            && self.frame_source().is_some_and(|source| {
                self.displayed_frame_source.map(|(displayed, _)| displayed) != Some(source)
            })
    }

    pub(super) fn restore_display_frame_source(&mut self) {
        let Some((source, _)) = self.displayed_frame_source else {
            return;
        };
        if self.frame_source() != Some(source) {
            self.selected_frame_source = Some(source);
            if let (Some(server), Some(session)) = (&self.server, self.live_session) {
                server.set_frame_source(session, Some(source));
            }
        }
    }

    pub fn select_frame_source(&mut self, source: FrameSourceKey) {
        if self.frame_source() == Some(source) {
            return;
        }
        let Some(timeline) = self
            .timeline_recording()
            .and_then(|recording| recording.frame_sources.source(source))
        else {
            return;
        };
        let target = timeline
            .at_or_before(self.frame)
            .or_else(|| timeline.first())
            .map(|frame| frame.index);
        if target.is_none() && !(self.follow_live && self.live_connected) {
            self.notice(crate::i18n::Message::new(
                "This frame source has no retained frames.",
                "该帧源没有可回放的保留帧。",
            ));
            return;
        }
        self.cancel_seek();
        self.playing = 0;
        self.reset_profile_views();
        self.selected_frame_source = Some(source);
        if let (Some(server), Some(session)) = (&self.server, self.live_session) {
            server.set_frame_source(session, Some(source));
        }
        if self.follow_live && self.live_connected {
            return;
        }
        if let Some(target) = target {
            self.seek_paused(target, DisplayReason::Frame);
        }
    }

    pub(super) fn select_source_of_frame(&mut self, target: usize) {
        let Some((source, _)) = self
            .timeline_recording()
            .and_then(|recording| recording.frame_sources.frame(target))
        else {
            return;
        };
        if self.frame_source() != Some(source) {
            self.selected_frame_source = Some(source);
            if let (Some(server), Some(session)) = (&self.server, self.live_session) {
                server.set_frame_source(session, Some(source));
            }
        }
    }
}

#[cfg(test)]
mod tests;
