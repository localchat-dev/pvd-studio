use super::*;
use pvd_core::capture::CaptureRange;

impl Studio {
    pub fn retained_range(&self) -> Option<CaptureRange> {
        self.timeline_recording_arc()
            .as_deref()
            .and_then(CaptureRange::retained)
    }

    pub fn recording_saves_history(&self) -> bool {
        self.timeline_recording().is_some_and(|recording| {
            recording.is_disk_backed()
                && matches!(
                    recording.source,
                    pvd_core::RecordingSource::SdkStream
                        | pvd_core::RecordingSource::ProjectCapture
                )
        })
    }

    pub fn can_save_recording(&self) -> bool {
        if self.recording_saves_history() {
            self.can_export_retained()
        } else {
            !self.exit_requested
                && self.job.is_none()
                && self.save_job.is_none()
                && self.recording_export.is_none()
                && self.can_save_capture()
        }
    }

    pub fn save_recording(&mut self, context: egui::Context) {
        if !self.can_save_recording() {
            return;
        }
        if self.recording_saves_history() {
            self.use_retained_range();
            self.export_retained(context);
        } else {
            self.save_capture(context);
        }
    }

    pub fn is_project_capture(&self) -> bool {
        self.timeline_recording()
            .is_some_and(|recording| recording.source == pvd_core::RecordingSource::ProjectCapture)
    }

    pub fn can_export_retained(&self) -> bool {
        !self.exit_requested
            && self.job.is_none()
            && self.save_job.is_none()
            && self.recording_export.is_none()
            && self.timeline_recording_arc().is_some_and(|recording| {
                matches!(
                    recording.source,
                    pvd_core::RecordingSource::SdkStream
                        | pvd_core::RecordingSource::ProjectCapture
                ) && recording.is_disk_backed()
                    && !recording.frames.is_empty()
            })
    }

    pub fn use_retained_range(&mut self) {
        if let Some(range) = self.retained_range() {
            self.retained_first = range.first;
            self.retained_last = range.last;
        }
    }

    pub fn export_retained(&mut self, context: egui::Context) {
        if !self.can_export_retained() {
            return;
        }
        let (first, last) = (self.retained_first, self.retained_last);
        let Some(recording) = self.timeline_recording_arc() else {
            return;
        };
        if !CaptureRange::retained(&recording).is_some_and(|available| {
            available.first <= first && first <= last && last <= available.last
        }) {
            self.notice(Message::new(
                "The requested range is no longer retained. Select an available range and retry.",
                "所选范围已不在保留历史中，请选择可用范围后重试。",
            ));
            return;
        }
        if !Path::new(&self.retained_path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pvdcapture"))
        {
            self.notice(Message::new(
                "Choose a .pvdcapture filename for the recording.",
                "录制文件请使用 .pvdcapture 扩展名。",
            ));
            return;
        }
        let source = crate::recording_export::Source::Retained {
            recording,
            epoch: self.capture_epoch.clone(),
            expected_epoch: self.capture_epoch.load(Ordering::Relaxed),
        };
        match crate::recording_export::Job::start(
            source,
            self.retained_path.clone().into(),
            crate::recording_export::Kind::Retained { first, last },
            context,
            &self.export_workers,
        ) {
            Ok(job) => self.recording_export = Some(job),
            Err(error) => self.notice(error),
        }
    }
}

#[cfg(test)]
mod tests;
