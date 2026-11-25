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

    fn retained_destination(&self, range: CaptureRange) -> PathBuf {
        if !self.retained_path.is_empty() {
            return PathBuf::from(&self.retained_path);
        }
        let session = if let Some(selected) = self.temporary_recordings.selected {
            self.temporary_recordings
                .get(selected)
                .map(|entry| entry.session)
        } else {
            self.live_session
        };
        let source = if let Some(session) = session {
            format!("desktop-session-{session}")
        } else if let Some(stem) = self.capture_path.as_deref().and_then(Path::file_stem) {
            stem.to_string_lossy().chars().take(80).collect()
        } else {
            "pvd-recording".to_owned()
        };
        let stamp = capture_save_timestamp();
        let suffix = if self
            .retained_range()
            .is_some_and(|available| range.first != available.first || range.last != available.last)
        {
            format!("-frames-{}-{}", range.first, range.last)
        } else {
            String::new()
        };
        self.retained_directory
            .join(format!("{source}-{stamp}{suffix}.pvdcapture"))
    }

    pub(crate) fn pick_retained_path(&mut self, range: CaptureRange, context: egui::Context) {
        let destination = self.retained_destination(range);
        if let Err(error) = self.file_picker.request(
            crate::file_picker::PickerKind::SaveRetainedCapture,
            destination,
            self.prefs.language,
            context,
        ) {
            self.notice(error);
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
        let destination = self.retained_destination(CaptureRange { first, last });
        if !destination
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
            destination,
            crate::recording_export::Kind::Retained { first, last },
            context,
            &self.export_workers,
        ) {
            Ok(job) => self.recording_export = Some(job),
            Err(error) => self.notice(error),
        }
    }
}

#[cfg(windows)]
fn capture_save_timestamp() -> String {
    let time = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}-{:03}",
        time.wYear,
        time.wMonth,
        time.wDay,
        time.wHour,
        time.wMinute,
        time.wSecond,
        time.wMilliseconds
    )
}

#[cfg(not(windows))]
fn capture_save_timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string()
}

#[cfg(test)]
mod tests;
