use super::Studio;

impl Studio {
    pub(crate) fn prepare_action_probe_live_fixture(&mut self) -> Result<bool, String> {
        if self.display_trace.is_none() || self.server.is_some() || self.job.is_some() {
            return Err("action fixture requires an isolated loaded display probe".into());
        }
        if self.live_session.is_some() {
            return Err("action fixture cannot replace a live session".into());
        }
        if self.seeking.is_some() {
            return Ok(false);
        }
        if !self.analysis_ready() {
            self.request_current_analysis();
            return Ok(false);
        }
        let recording = self
            .recording
            .as_ref()
            .ok_or("action fixture has no recording")?;
        if self.frame.checked_add(1) != Some(recording.frame_count()) {
            return Err("action fixture must use the complete recording's final frame".into());
        }
        let source_frame = recording
            .frame_sources
            .frame(self.frame)
            .map(|(source, frame)| (source, *frame))
            .ok_or("action fixture has no complete frame source")?;
        self.live_recording = Some(recording.clone());
        self.live_state = self.state.clone();
        self.live_frame = self.frame;
        self.live_frame_source = Some(source_frame);
        self.live_analysis = recording.analysis.clone();
        self.live_session = Some(u64::MAX);
        self.live_connected = true;
        self.follow_live = false;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finite_live_fixture_requires_probe_and_latest_complete_state() {
        let bytes = crate::full_profile::tests::timeline_capture(2, "Action fixture");
        let mut studio = Studio::default();
        studio.set_recording(pvd_core::Recording::from_bytes(&bytes).unwrap());
        studio.seek(1);
        assert!(studio.prepare_action_probe_live_fixture().is_err());
        studio.display_trace = Some(crate::display_probe::Trace::default());
        studio.seek(0);
        assert!(studio.prepare_action_probe_live_fixture().is_err());
        studio.seek(1);
        let expected = studio.state.clone();
        let expected_source = studio.displayed_frame_source;
        assert!(studio.prepare_action_probe_live_fixture().unwrap());
        assert!(expected_source.is_some());
        assert_eq!(studio.live_frame_source, expected_source);
        assert!(studio.analysis_ready());
        assert!(studio.prepare_action_probe_live_fixture().is_err());
        studio.seek(0);
        studio.return_to_live();
        assert_eq!(studio.frame, 1);
        assert_eq!(studio.state, expected);
        assert_eq!(studio.displayed_frame_source, expected_source);
        assert!(studio.follow_live);
        assert!(studio.analysis_ready());
    }
}
