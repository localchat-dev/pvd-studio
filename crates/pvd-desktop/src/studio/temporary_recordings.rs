use super::{DisplayReason, Studio};
use crate::i18n::Message;
use pvd_core::{Recording, SceneState};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, atomic::Ordering},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    Completed,
    Interrupted,
    Closed,
}

pub(crate) struct Entry {
    pub id: u64,
    pub session: u64,
    pub recording: Arc<Recording>,
    pub status: Status,
    capture_path: Option<PathBuf>,
}

impl Entry {
    pub fn disk_bytes(&self) -> u64 {
        self.recording
            .disk_usage()
            .map_or(0, |stats| stats.disk_bytes)
    }

    pub fn frame_count(&self) -> usize {
        self.recording
            .frame_count()
            .saturating_sub(self.recording.first_frame())
    }
}

#[derive(Default)]
pub(crate) struct TemporaryRecordings {
    entries: VecDeque<Entry>,
    pub selected: Option<u64>,
    next_id: u64,
}

impl TemporaryRecordings {
    pub fn entries(&self) -> impl DoubleEndedIterator<Item = &Entry> {
        self.entries.iter()
    }

    pub(super) fn get(&self, id: u64) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    fn insert(
        &mut self,
        session: u64,
        recording: Arc<Recording>,
        capture_path: Option<PathBuf>,
        status: Status,
    ) -> u64 {
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("temporary recording identity exhausted");
        let id = self.next_id;
        self.entries.push_back(Entry {
            id,
            session,
            recording,
            capture_path,
            status,
        });
        id
    }

    fn can_remove(&self, entry: &Entry) -> bool {
        self.selected != Some(entry.id) && Arc::strong_count(&entry.recording) == 1
    }

    fn remove(&mut self, id: u64) -> bool {
        let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.id == id && self.can_remove(entry))
        else {
            return false;
        };
        self.entries.remove(index);
        true
    }

    fn reclaim(&mut self, max_count: usize, budget: u64) -> usize {
        let mut removed = 0;
        // A newly finished recording must remain available long enough to save,
        // even when that single recording exceeds the cleanup target.
        let newest = self.entries.back().map(|entry| entry.id);
        let mut bytes = self.entries.iter().fold(0u64, |total, entry| {
            total.saturating_add(entry.disk_bytes())
        });
        while self.entries.len() > max_count || bytes > budget {
            let Some(index) = self
                .entries
                .iter()
                .position(|entry| Some(entry.id) != newest && self.can_remove(entry))
            else {
                break;
            };
            let entry = self.entries.remove(index).unwrap();
            bytes = bytes.saturating_sub(entry.disk_bytes());
            removed += 1;
        }
        removed
    }
}

impl Studio {
    pub(crate) fn viewing_temporary_recording(&self) -> bool {
        self.temporary_recordings.selected.is_some()
    }

    pub(super) fn timeline_is_live(&self) -> bool {
        self.live_connected && !self.viewing_temporary_recording()
    }

    pub(crate) fn can_delete_temporary_recording(&self, id: u64) -> bool {
        self.temporary_recordings
            .get(id)
            .is_some_and(|entry| self.temporary_recordings.can_remove(entry))
    }

    pub(crate) fn delete_temporary_recording(&mut self, id: u64) {
        if self.temporary_recordings.remove(id) {
            self.notice(Message::new(
                "Temporary recording removed from the list. Unused cache files are cleaned up; files still in use or locked may be released later. Saved files are unchanged.",
                "临时录制已移出列表。未被使用的缓存文件会清理；仍被读取或锁定的文件可能稍后释放，已保存文件不受影响。",
            ));
        }
    }

    pub(super) fn retain_current_capture(&mut self, status: Status) -> Option<u64> {
        if let Some(id) = self.retained_live_recording {
            return Some(id);
        }
        let session = self.live_session?;
        if self.live_connected && self.server.is_some() {
            return None;
        }
        let recording = self.live_recording.clone()?;
        let id = self.temporary_recordings.insert(
            session,
            recording,
            self.live_capture_path.clone(),
            status,
        );
        self.retained_live_recording = Some(id);
        Some(id)
    }

    pub(super) fn cleanup_temporary_recordings(&mut self) {
        let removed = self.temporary_recordings.reclaim(
            self.prefs.temporary_max_count.clamp(1, 256) as usize,
            u64::from(self.prefs.temporary_budget_gib.clamp(1, 1024)) << 30,
        );
        if removed != 0 {
            self.notice(Message::new(
                format!("Released {removed} ended temporary recordings from the list. The newest, active, opened and saving recordings are retained; locked cache files may be cleaned up later."),
                format!("已将 {removed} 条结束的临时录制移出列表。最新一条以及正在录制、查看和保存的内容继续保留；被锁定的缓存文件可能稍后清理。"),
            ));
        }
    }

    pub(super) fn reset_display_for_recording(&mut self) {
        self.capture_epoch.fetch_add(1, Ordering::Relaxed);
        self.cancel_seek();
        self.reset_profile_views();
        self.room_view = super::rooms::RoomView::default();
        self.room_scope = crate::room_scope::RoomScope::default();
        self.playing = 0;
        self.follow_live = false;
        self.display_reason = None;
        self.recording = None;
        self.clip_set = crate::clips::ClipSet::default();
        self.clip_persistence = crate::clips::Persistence::default();
        self.state = SceneState::default();
        self.activity.clear();
        self.clear_selection();
        self.selected_frame_source = None;
        self.displayed_frame_source = None;
        self.displayed_analysis = super::analysis_demand::DisplayedAnalysis::Complete;
        self.captured_camera = None;
        self.source_identity = None;
        self.bytes = None;
        self.source.clear();
        self.error_rows = Arc::default();
        self.curve_job = None;
        self.curves.clear();
        self.profile_window = None;
        self.frame = 0;
        self.revision = self.revision.wrapping_add(1);
    }

    pub(crate) fn select_temporary_recording(&mut self, id: u64) {
        if self.shutdown_started
            || self.exit_requested
            || self.close_after_capture
            || self.job.is_some()
            || self.save_job.is_some()
            || self.recording_export.is_some()
        {
            return;
        }
        if self.clip_persistence.needs_confirmation(&self.clip_set) {
            self.notice(Message::new(
                "Save or discard clip edits before changing recordings.",
                "请先保存或放弃片段编辑，再切换录制。",
            ));
            return;
        }
        let Some(entry) = self.temporary_recordings.get(id) else {
            return;
        };
        let recording = entry.recording.clone();
        let capture_path = entry.capture_path.clone();
        self.reset_display_for_recording();
        self.temporary_recordings.selected = Some(id);
        self.capture_path = capture_path;
        self.selected_frame_source = recording.frame_sources.default_source();
        self.recording = Some(recording);
        self.clip_set = crate::clips::ClipSet::default();
        self.clip_persistence = crate::clips::Persistence::default();
        let target = self
            .playback_bounds()
            .map_or(self.first_frame(), |(first, _)| first);
        self.seek_paused(target, DisplayReason::Frame);
        self.refresh_room_directory();
    }
}

#[cfg(test)]
mod tests;
