use pvd_core::{Object, RenderObject, SceneState, Value};

use crate::studio::Studio;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Style {
    pub color: Option<[u8; 4]>,
    pub pickable: bool,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            color: None,
            pickable: true,
        }
    }
}

impl Style {
    pub fn instance(object: Option<&Object>) -> Self {
        let Some(object) = object else {
            return Self::default();
        };
        Self {
            color: match object.properties.get("$color") {
                Some(Value::Bytes(bytes)) => bytes.as_ref().try_into().ok(),
                _ => None,
            },
            pickable: !matches!(object.properties.get("$pickable"), Some(Value::Bool(false))),
        }
    }

    pub fn shape(state: &SceneState, object: &RenderObject) -> Self {
        let mut display =
            Self::instance(state.resolve_reference(object.id, "Shapes", object.shape_id));
        // The ray hits a Shape but selection returns its Actor. Both instances must allow picking.
        display.pickable &= Self::instance(state.objects.get(&object.id)).pickable;
        display
    }

    pub fn rgba(self, fallback: [f32; 4], selected: bool, opacity: f32) -> [f32; 4] {
        let mut rgba = if selected {
            fallback
        } else {
            self.color.map_or(fallback, |color| {
                color.map(|channel| f32::from(channel) / 255.0)
            })
        };
        rgba[3] *= opacity;
        rgba
    }
}

pub(super) fn opacity(studio: &Studio, selected: bool) -> f32 {
    if studio.prefs.opacity_scope.includes(selected) && studio.prefs.opacity.is_finite() {
        studio.prefs.opacity.clamp(0.1, 1.0)
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests;
