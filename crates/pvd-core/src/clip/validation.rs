use crate::{Action, Error, ErrorEvent, SceneState, error};

#[derive(Default)]
pub(crate) struct ValidationState {
    pub frames: usize,
    pub state_bytes: usize,
    pub skip_analysis: bool,
}

impl ValidationState {
    pub fn apply(
        &mut self,
        action: Action,
        state: &mut SceneState,
        limit: usize,
    ) -> Result<(), Error> {
        let before = action.scene_bytes(state);
        action.apply(state);
        let after = action.scene_bytes(state);
        let error_bytes = match &action {
            Action::Error(event) => error_bytes(event),
            _ => 0,
        };
        self.state_bytes =
            crate::updated_state_bytes(self.state_bytes, before, after).saturating_add(error_bytes);
        if self.state_bytes > limit || state.generations.len() > 1_000_000 {
            return Err(error(
                "clip validation exceeds its current scene resource budget",
            ));
        }
        Ok(())
    }

    pub fn discard_errors(&mut self, state: &mut SceneState) {
        self.state_bytes -= state
            .errors
            .drain(..)
            .map(|event| error_bytes(&event))
            .sum::<usize>();
    }
}

fn error_bytes(event: &ErrorEvent) -> usize {
    2 * std::mem::size_of_val(event) + event.message.len() + event.file.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RecordingBuilder;
    use pvd_protocol::{Endian, Event, EventGroup, Header};

    #[test]
    fn validation_camera_replacements_keep_current_state_bounded() {
        let mut validation = ValidationState::default();
        let mut state = SceneState::default();
        let bytes = crate::camera_entry_bytes("camera");
        for value in 0..300 {
            validation
                .apply(
                    Action::Camera(
                        "camera".into(),
                        crate::SourceCamera {
                            position: [value as f32, 0.0, 0.0],
                            up: [0.0, 1.0, 0.0],
                            target: [0.0; 3],
                        },
                    ),
                    &mut state,
                    bytes,
                )
                .unwrap();
            assert_eq!(validation.state_bytes, bytes);
        }
        let error = validation
            .apply(
                Action::Camera("other camera".into(), state.cameras["camera"]),
                &mut state,
                bytes,
            )
            .unwrap_err();
        assert!(error.to_string().contains("resource budget"));
    }

    #[test]
    fn validation_keeps_complete_errors_without_accumulating_frame_or_action_history() {
        let mut builder = RecordingBuilder::new(Header {
            version: 1,
            timestamp_numerator: 1,
            timestamp_denominator: 1,
            flags: 1,
            endian: Endian::Little,
        });
        builder.validation = Some(ValidationState::default());
        builder
            .push(EventGroup {
                stream_id: 1,
                timestamp: 0,
                offset: 0,
                events: vec![Event::StringHandle {
                    handle: 1,
                    value: "frame".into(),
                }],
            })
            .unwrap();
        for frame in 0..20_000 {
            let mut events = Vec::new();
            if frame < 600 {
                events.push(Event::ErrorMessage {
                    code: frame,
                    message: "captured error".into(),
                    file: "sdk.cpp".into(),
                    line: 9,
                });
            }
            events.push(Event::EndSection {
                id: 17,
                name: 1,
                timestamp: u64::from(frame),
            });
            builder
                .push(EventGroup {
                    stream_id: 1,
                    timestamp: u64::from(frame),
                    offset: 0,
                    events,
                })
                .unwrap();
        }
        assert_eq!(builder.state.errors.len(), 600);
        assert_eq!(builder.validation.as_ref().unwrap().frames, 20_000);
        assert!(builder.recording.actions.is_empty());
        assert!(builder.recording.frames.is_empty());
        assert!(builder.recording.frame_ends.is_empty());
        assert!(builder.recording.checkpoints.is_empty());
        assert!(builder.validation.as_ref().unwrap().state_bytes < 128 << 10);
    }

    #[test]
    fn current_scene_limit_rejects_growth_without_silently_truncating_errors() {
        let mut validation = ValidationState::default();
        let mut scene = SceneState::default();
        let result = validation.apply(
            Action::Error(crate::ErrorEvent {
                code: 1,
                message: "x".repeat(4096),
                file: "sdk.cpp".into(),
                line: 1,
                timestamp: 0,
            }),
            &mut scene,
            1024,
        );
        assert!(result.unwrap_err().to_string().contains("resource budget"));
        assert_eq!(scene.errors.len(), 1);
    }

    #[test]
    fn reference_bindings_count_towards_the_scene_budget_without_a_value() {
        let mut validation = ValidationState::default();
        let mut state = SceneState::default();
        validation
            .apply(Action::Push(1, "actors".into(), 2), &mut state, 1024)
            .unwrap();
        let first = validation.state_bytes;
        assert!(first > 0);
        assert!(
            validation
                .apply(Action::Push(1, "actors".into(), 3), &mut state, first)
                .is_err()
        );
        validation
            .apply(Action::Destroy(1), &mut state, 1024)
            .unwrap();
        assert_eq!(validation.state_bytes, 0);
    }
}
