use super::*;
use crate::coordinates::{Coordinates, UpResolution, UpSettings};

impl Studio {
    pub(crate) fn up_resolution(&mut self) -> UpResolution {
        let changed = self.scene_changes.changed(
            self.up_resolver.revision().unwrap_or(u64::MAX),
            self.revision,
        );
        self.up_resolver.resolve(
            &self.state,
            self.revision,
            changed,
            self.room_scope.room().map(|room| room.id).or(self.selected),
            UpSettings {
                mode: self.prefs.up_mode,
                preset: self.prefs.up_axis,
                custom: self.prefs.custom_up,
            },
        )
    }

    pub(crate) fn display_coordinates(&mut self) -> Coordinates {
        let direction = self.up_resolution().vector;
        Coordinates::from_up_vector(direction, self.prefs.handedness, self.prefs.meters_per_unit)
            .unwrap_or_else(|| {
                Coordinates::new(
                    self.prefs.up_axis,
                    self.prefs.handedness,
                    self.prefs.meters_per_unit,
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinates::{UpAxis, UpMode};
    use bevy::prelude::Vec3;
    use pvd_core::{Object, Value};

    #[test]
    fn source_up_preferences_preserve_legacy_axes_and_custom_values() {
        for up in UpAxis::ALL {
            let legacy = format!("{{\"up_axis\":{}}}", serde_json::to_string(&up).unwrap());
            let preferences = Preferences::from_bytes(legacy.as_bytes());
            assert_eq!(preferences.up_axis, up);
            assert_eq!(preferences.up_mode, UpMode::Preset);
            assert_eq!(preferences.custom_up, [0.0, 1.0, 0.0]);
        }
        let preferences = Preferences {
            up_axis: UpAxis::NegativeY,
            up_mode: UpMode::Custom,
            custom_up: [1.0, -2.0, 3.0],
            ..Default::default()
        };
        let decoded = Preferences::from_bytes(&serde_json::to_vec(&preferences).unwrap());
        assert_eq!(decoded.up_axis, UpAxis::NegativeY);
        assert_eq!(decoded.up_mode, UpMode::Custom);
        assert_eq!(decoded.custom_up, preferences.custom_up);
    }

    #[test]
    fn automatic_up_reads_the_displayed_scene_and_not_the_receiving_state() {
        let mut studio = Studio::default();
        studio.prefs.up_mode = UpMode::Auto;
        let scene = |gravity| Object {
            id: 1,
            generation: 0,
            type_name: "physx3::PxScene".into(),
            properties: [("Gravity".into(), Value::Vec3(gravity))].into(),
        };
        studio.state.objects.insert(1, scene([0.0, -9.81, 0.0]));
        studio
            .live_state
            .objects
            .insert(1, scene([-9.81, 0.0, 0.0]));
        studio.live_connected = true;
        studio.follow_live = false;
        assert!(
            studio
                .display_coordinates()
                .direction(Vec3::Y)
                .abs_diff_eq(Vec3::Y, 1e-6)
        );
        studio
            .live_state
            .objects
            .insert(1, scene([0.0, 0.0, -9.81]));
        studio.live_frame += 1;
        assert!(
            studio
                .display_coordinates()
                .direction(Vec3::Y)
                .abs_diff_eq(Vec3::Y, 1e-6)
        );
        studio.state.objects.insert(1, scene([0.0, 0.0, -9.81]));
        studio.revision += 1;
        assert!(
            studio
                .display_coordinates()
                .direction(Vec3::Z)
                .abs_diff_eq(Vec3::Y, 1e-6)
        );
        assert!(!studio.follow_live);
    }
}
