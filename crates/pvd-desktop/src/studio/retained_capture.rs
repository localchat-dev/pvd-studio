use super::*;

impl Studio {
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
            && self.timeline_recording().is_some_and(|recording| {
                matches!(
                    recording.source,
                    pvd_core::RecordingSource::SdkStream
                        | pvd_core::RecordingSource::ProjectCapture
                ) && recording.is_disk_backed()
                    && recording.first_frame() < recording.frame_count()
            })
    }

    pub fn use_retained_range(&mut self) {
        self.retained_first = self.first_frame();
        self.retained_last = self.count().saturating_sub(1);
    }

    pub fn export_retained(&mut self, context: egui::Context) {
        if !self.can_export_retained() {
            return;
        }
        let (first, last) = (self.retained_first, self.retained_last);
        if first < self.first_frame() || first > last || last >= self.count() {
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
                "Use .pvdcapture for a retained project capture.",
                "保留范围的项目捕获请使用 .pvdcapture 扩展名。",
            ));
            return;
        }
        let source = crate::recording_export::Source::Retained {
            recording: self.timeline_recording_arc().unwrap(),
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
