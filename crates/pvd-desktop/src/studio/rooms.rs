use super::{DisplayReason, Studio};
use crate::i18n::{Language, Message};
use pvd_core::{
    frame_sources::{FrameObject, FrameSection, FrameSourceKey, FrameSourceTimeline, SourceFrame},
    rooms::RoomDirectory,
};
use std::{
    collections::BTreeMap,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

pub(crate) struct RoomView {
    pub requested: Option<FrameObject>,
    pub displayed: Option<FrameObject>,
    pub explicit: bool,
    pub expanded: bool,
    pub filter: String,
    pub show_destroyed: bool,
    pub counts: BTreeMap<FrameObject, usize>,
    counted_at: Option<Instant>,
    counted_revision: u64,
}

impl Default for RoomView {
    fn default() -> Self {
        Self {
            requested: None,
            displayed: None,
            explicit: false,
            expanded: true,
            filter: String::new(),
            show_destroyed: true,
            counts: BTreeMap::new(),
            counted_at: None,
            counted_revision: u64::MAX,
        }
    }
}

impl Studio {
    pub(crate) fn room_session_epoch(&self) -> u64 {
        self.capture_epoch.load(Ordering::Relaxed)
    }

    pub(crate) fn room_directory(&self) -> Option<&RoomDirectory> {
        Some(&self.timeline_recording()?.rooms)
    }

    pub(crate) fn displayed_room(&self) -> Option<FrameObject> {
        self.room_view.displayed
    }

    pub(crate) fn room_selection_pending(&self) -> bool {
        self.room_view.requested != self.room_view.displayed
    }

    pub(crate) fn room_label(&self, room: FrameObject) -> String {
        let number = self
            .room_directory()
            .and_then(|rooms| rooms.get(room))
            .map(|room| room.number);
        match (self.prefs.language, number) {
            (Language::English, Some(number)) => format!("Physics Scene {number}"),
            (_, Some(number)) => format!("物理场景 {number}"),
            _ => self.prefs.language.text("Physics Scene", "物理场景").into(),
        }
    }

    pub(crate) fn room_source(&self, room: FrameObject) -> Option<FrameSourceKey> {
        let directory = &self.timeline_recording()?.frame_sources;
        let preferred = FrameSourceKey::Captured {
            section: FrameSection::PhysX,
            source: room.id,
        };
        if directory
            .source(preferred)
            .is_some_and(|source| source.first_for_scene(room).is_some())
        {
            return Some(preferred);
        }
        directory
            .sources()
            .find(|source| source.first_for_scene(room).is_some())
            .map(|source| source.source())
            .or_else(|| {
                directory
                    .source(FrameSourceKey::Legacy)
                    .filter(|_| {
                        self.room_directory()
                            .is_some_and(|rooms| rooms.get(room).is_some())
                    })
                    .map(|source| source.source())
            })
    }

    pub(super) fn room_contains_frame(
        &self,
        room: FrameObject,
        source: FrameSourceKey,
        frame: &SourceFrame,
    ) -> bool {
        if source != FrameSourceKey::Legacy {
            return frame.scene == Some(room);
        }
        self.room_directory()
            .and_then(|rooms| rooms.get(room))
            .is_some_and(|room| {
                frame.index >= room.first_observed_frame
                    && room.destroyed_at_frame.is_none_or(|end| frame.index < end)
            })
    }

    pub(super) fn room_frame_bounds<'a>(
        &self,
        source: &'a FrameSourceTimeline,
        room: FrameObject,
    ) -> Option<(&'a SourceFrame, &'a SourceFrame)> {
        if source.source() != FrameSourceKey::Legacy {
            return Some((source.first_for_scene(room)?, source.last_for_scene(room)?));
        }
        // Legacy captures retain shared frame positions, without independent Scene clocks.
        let info = self.room_directory()?.get(room)?;
        let first = source.at_or_after(info.first_observed_frame)?;
        let last = if let Some(end) = info.destroyed_at_frame {
            source.at_or_before(end.checked_sub(1)?)?
        } else {
            source.last()?
        };
        (first.index <= last.index).then_some((first, last))
    }

    fn room_frames<'a>(
        &self,
        source: &'a FrameSourceTimeline,
        room: FrameObject,
    ) -> Box<dyn DoubleEndedIterator<Item = &'a SourceFrame> + 'a> {
        if source.source() != FrameSourceKey::Legacy {
            return Box::new(source.scene_frames(room));
        }
        let bounds = self
            .room_frame_bounds(source, room)
            .map(|(first, last)| (first.index, last.index));
        Box::new(source.frames().filter(move |frame| {
            bounds.is_some_and(|(first, last)| (first..=last).contains(&frame.index))
        }))
    }

    pub(crate) fn room_has_history(&self, room: FrameObject) -> bool {
        let Some(recording) = self.timeline_recording() else {
            return false;
        };
        self.room_source(room)
            .and_then(|source| recording.frame_sources.source(source))
            .is_some_and(|source| {
                let Some((first, last)) = self.room_frame_bounds(source, room) else {
                    return false;
                };
                let mut frame = Some(first);
                while let Some(current) = frame.filter(|frame| frame.index <= last.index) {
                    if recording.is_frame_retained(current.index) {
                        return true;
                    }
                    frame = source.next(current.index);
                }
                false
            })
    }

    pub(crate) fn requested_room_destroyed(&self) -> bool {
        self.room_view
            .requested
            .and_then(|room| self.room_directory()?.get(room))
            .is_some_and(|room| room.destroyed_at_frame.is_some())
    }

    pub(crate) fn select_room(&mut self, room: Option<FrameObject>) {
        if room.is_some_and(|room| {
            self.room_directory()
                .and_then(|directory| directory.get(room))
                .is_none()
        }) {
            return;
        }
        if self.room_view.explicit
            && self.room_view.requested == room
            && !self.room_selection_pending()
        {
            return;
        }
        self.cancel_seek();
        self.playing = 0;
        self.reset_profile_views();
        self.room_view.explicit = true;
        self.room_view.requested = room;
        let Some(room) = room else {
            self.room_view.displayed = None;
            self.restore_display_frame_source();
            self.revision = self.revision.wrapping_add(1);
            self.refresh_room_scope();
            return;
        };
        let source = self.room_source(room).unwrap_or(FrameSourceKey::Captured {
            section: FrameSection::PhysX,
            source: room.id,
        });
        self.selected_frame_source = Some(source);
        if !self.viewing_temporary_recording()
            && let (Some(server), Some(session)) = (&self.server, self.live_session)
        {
            server.set_frame_source(session, Some(source));
        }
        if self.follow_live && self.live_connected && !self.requested_room_destroyed() {
            if self
                .live_frame_source
                .is_some_and(|(_, frame)| frame.scene == Some(room))
            {
                self.return_to_live();
            }
            return;
        }
        let target = self.timeline_recording().and_then(|recording| {
            let source = recording.frame_sources.source(source)?;
            let frames = || self.room_frames(source, room);
            if self.follow_live {
                frames()
                    .rev()
                    .find(|frame| recording.is_frame_retained(frame.index))
            } else {
                frames()
                    .rev()
                    .find(|frame| frame.index <= self.frame)
                    .or_else(|| frames().next())
            }
            .map(|frame| frame.index)
        });
        if let Some(target) = target {
            self.seek_paused(target, DisplayReason::Frame);
        } else {
            self.pause_display();
            self.room_view.requested = self.room_view.displayed;
            self.restore_display_frame_source();
            self.notice(Message::new(
                "This scene has no retained complete frames.",
                "该场景没有可回放的完整帧。",
            ));
        }
    }

    pub(super) fn commit_room_display(&mut self) {
        if !self.room_view.explicit && self.count() != 0 {
            self.room_view.requested = self
                .displayed_frame_source
                .and_then(|(_, frame)| frame.scene)
                .or_else(|| {
                    if self
                        .displayed_frame_source
                        .is_none_or(|(source, _)| source != FrameSourceKey::Legacy)
                    {
                        return None;
                    }
                    self.room_directory()?
                        .iter()
                        .find(|room| {
                            self.state
                                .objects
                                .get(&room.scene.id)
                                .is_some_and(|object| object.generation == room.scene.generation)
                        })
                        .map(|room| room.scene)
                });
            self.room_view.explicit = self.room_view.requested.is_some();
        }
        if (self.room_view.requested.is_none()
            && self.displayed_frame_source.map(|(source, _)| source) == self.frame_source())
            || (self.room_view.requested.is_some()
                && self
                    .displayed_frame_source
                    .is_some_and(|(_, frame)| frame.scene == self.room_view.requested))
            || (self.seeking.is_none()
                && self.room_view.requested.is_some_and(|room| {
                    self.displayed_frame_source.is_some_and(|(source, frame)| {
                        source == FrameSourceKey::Legacy
                            && self.room_contains_frame(room, source, &frame)
                    }) && self.state.objects.get(&room.id).is_some_and(|object| {
                        object.generation == room.generation
                            && object.type_name.ends_with("::PxScene")
                    })
                }))
        {
            self.room_view.displayed = self.room_view.requested;
        }
        self.refresh_room_scope();
    }

    fn refresh_room_scope(&mut self) {
        self.room_scope
            .update(&self.state, self.room_view.displayed, self.revision);
        self.refresh_selection();
        if self.selected.is_none() {
            self.follow_selected = false;
        }
    }

    pub(super) fn refresh_room_directory(&mut self) {
        if self.follow_live && self.requested_room_destroyed() {
            let room = self.room_view.requested;
            if self.room_selection_pending() {
                self.select_room(room);
            } else {
                self.pause_display();
            }
        }
        let now = Instant::now();
        if (!self.live_connected && self.room_view.counted_revision == self.revision)
            || self
                .room_view
                .counted_at
                .is_some_and(|time| now.duration_since(time) < Duration::from_millis(500))
        {
            return;
        }
        self.room_view.counted_at = Some(now);
        self.room_view.counted_revision = self.revision;
        let state = if self.live_session.is_some() && !self.viewing_temporary_recording() {
            &self.live_state
        } else {
            &self.state
        };
        let mut counts = BTreeMap::new();
        for object in state.objects.values().filter(|object| {
            matches!(
                object.type_name.as_str(),
                "physx3::PxRigidDynamic" | "physx3::PxRigidStatic" | "physx3::PxArticulationLink"
            )
        }) {
            if let Some(scene) = crate::room_scope::scene_for_object(state, object) {
                *counts.entry(scene).or_insert(0) += 1;
            }
        }
        self.room_view.counts = counts;
    }
}

#[cfg(test)]
mod legacy_tests;
#[cfg(test)]
mod tests;
