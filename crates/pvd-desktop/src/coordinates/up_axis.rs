use super::*;
use pvd_core::{Object, SceneState, Value};
use std::collections::{BTreeSet, HashSet};

const MAX_SCENES: usize = 100_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpMode {
    #[default]
    Preset,
    Custom,
    Auto,
}

impl UpMode {
    pub const ALL: [Self; 3] = [Self::Preset, Self::Custom, Self::Auto];

    pub fn label(self, language: Language) -> &'static str {
        match self {
            Self::Preset => language.text("Preset", "预设"),
            Self::Custom => language.text("Custom", "自定义"),
            Self::Auto => language.text("Auto (gravity)", "自动（重力）"),
        }
    }
}

#[derive(Clone, Copy)]
pub struct UpSettings {
    pub mode: UpMode,
    pub preset: UpAxis,
    pub custom: [f32; 3],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failure {
    CustomVector,
    NoScene,
    MissingGravity,
    InvalidGravity,
    ConflictingGravity,
    SceneLimit,
}

impl Failure {
    fn label(self, language: Language) -> &'static str {
        match self {
            Self::CustomVector => language.text("Invalid up vector", "上方向向量无效"),
            Self::NoScene => language.text("No scene in the displayed frame", "当前显示帧没有场景"),
            Self::MissingGravity => language.text("Scene gravity is missing", "场景重力缺失"),
            Self::InvalidGravity => {
                language.text("Scene gravity is zero or invalid", "场景重力为零或无效")
            }
            Self::ConflictingGravity => {
                language.text("Scene gravity directions differ", "多个场景的重力方向不同")
            }
            Self::SceneLimit => language.text(
                "Too many scenes for automatic up",
                "场景过多，无法自动判断上方向",
            ),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct UpResolution {
    pub vector: Vec3,
    mode: UpMode,
    preset: UpAxis,
    failure: Option<Failure>,
}

impl UpResolution {
    pub fn is_fallback(self) -> bool {
        self.failure.is_some()
    }

    pub fn description(self, language: Language) -> String {
        if let Some(failure) = self.failure {
            return format!(
                "{} · {} {}",
                failure.label(language),
                language.text("using preset", "使用预设"),
                self.preset.label(),
            );
        }
        match self.mode {
            UpMode::Preset => format!("{} {}", self.preset.label(), language.text("up", "向上")),
            _ => format!(
                "{} · [{:.3}, {:.3}, {:.3}] {}",
                self.mode.label(language),
                self.vector.x,
                self.vector.y,
                self.vector.z,
                language.text("up", "向上"),
            ),
        }
    }
}

pub(super) fn unit_vector(value: Vec3) -> Option<Vec3> {
    if !value.is_finite() {
        return None;
    }
    let maximum = value.abs().max_element();
    (maximum > 0.0).then(|| (value / maximum).normalize())
}

fn is_scene(object: &Object) -> bool {
    object.type_name.ends_with("::PxScene")
}

fn gravity_up(object: &Object) -> Result<Vec3, Failure> {
    let gravity = match object.properties.get("Gravity") {
        Some(Value::Vec3(gravity)) => gravity,
        None => return Err(Failure::MissingGravity),
        Some(_) => return Err(Failure::InvalidGravity),
    };
    unit_vector(-Vec3::from(*gravity)).ok_or(Failure::InvalidGravity)
}

#[derive(Default)]
pub(crate) struct UpResolver {
    revision: Option<u64>,
    scenes: BTreeSet<u64>,
    overflow: bool,
    auto_key: Option<(u64, Option<u64>)>,
    automatic: Option<Result<Vec3, Failure>>,
    #[cfg(test)]
    full_scans: usize,
}

impl UpResolver {
    pub fn revision(&self) -> Option<u64> {
        self.revision
    }

    pub fn resolve(
        &mut self,
        state: &SceneState,
        revision: u64,
        changed: Option<&HashSet<u64>>,
        selected: Option<u64>,
        settings: UpSettings,
    ) -> UpResolution {
        let result = match settings.mode {
            UpMode::Preset => Ok(settings.preset.vector()),
            UpMode::Custom => unit_vector(Vec3::from(settings.custom)).ok_or(Failure::CustomVector),
            UpMode::Auto => {
                let selected_scene =
                    selected.filter(|id| state.objects.get(id).is_some_and(is_scene));
                if selected_scene.is_none() {
                    self.update_scenes(state, revision, changed);
                }
                let key = (revision, selected_scene);
                if self.auto_key != Some(key) {
                    self.auto_key = Some(key);
                    self.automatic = Some(self.detect(state, selected_scene));
                }
                self.automatic.unwrap()
            }
        };
        UpResolution {
            vector: result.unwrap_or_else(|_| settings.preset.vector()),
            mode: settings.mode,
            preset: settings.preset,
            failure: result.err(),
        }
    }

    fn update_scenes(&mut self, state: &SceneState, revision: u64, changed: Option<&HashSet<u64>>) {
        if self.revision == Some(revision) {
            return;
        }
        if self.revision.is_some()
            && !self.overflow
            && let Some(changed) = changed
        {
            for id in changed {
                self.scenes.remove(id);
                if state.objects.get(id).is_some_and(is_scene) {
                    self.scenes.insert(*id);
                    if self.scenes.len() > MAX_SCENES {
                        break;
                    }
                }
            }
        } else {
            self.scenes.clear();
            self.overflow = false;
            #[cfg(test)]
            {
                self.full_scans += 1;
            }
            for object in state.objects.values().filter(|object| is_scene(object)) {
                self.scenes.insert(object.id);
                if self.scenes.len() > MAX_SCENES {
                    break;
                }
            }
        }
        self.revision = Some(revision);
        if self.scenes.len() > MAX_SCENES {
            self.scenes.clear();
            self.overflow = true;
        }
    }

    fn detect(&self, state: &SceneState, selected: Option<u64>) -> Result<Vec3, Failure> {
        if let Some(id) = selected {
            return gravity_up(&state.objects[&id]);
        }
        if self.overflow {
            return Err(Failure::SceneLimit);
        }
        let mut direction: Option<Vec3> = None;
        for id in &self.scenes {
            let next = gravity_up(&state.objects[id])?;
            if let Some(current) = direction {
                if !current.abs_diff_eq(next, 1e-4) {
                    return Err(Failure::ConflictingGravity);
                }
            } else {
                direction = Some(next);
            }
        }
        direction.ok_or(Failure::NoScene)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> UpSettings {
        UpSettings {
            mode: UpMode::Auto,
            preset: UpAxis::Y,
            custom: [0.0, 1.0, 0.0],
        }
    }

    fn scene(id: u64, gravity: [f32; 3]) -> Object {
        Object {
            id,
            generation: 0,
            type_name: "physx3::PxScene".into(),
            properties: [("Gravity".into(), Value::Vec3(gravity))].into(),
        }
    }

    #[test]
    fn automatic_up_uses_displayed_gravity_and_selected_scene_without_guessing() {
        let mut state = SceneState::default();
        let mut resolver = UpResolver::default();
        assert_eq!(
            resolver.resolve(&state, 0, None, None, settings()).failure,
            Some(Failure::NoScene)
        );
        state.objects.insert(1, scene(1, [0.0, 0.0, -9.81]));
        let resolution = resolver.resolve(&state, 1, Some(&[1].into()), None, settings());
        assert_eq!(resolution.vector, Vec3::Z);
        assert!(!resolution.is_fallback());
        state.objects.insert(2, scene(2, [-2.0, 0.0, 0.0]));
        assert_eq!(
            resolver
                .resolve(&state, 2, Some(&[2].into()), None, settings())
                .failure,
            Some(Failure::ConflictingGravity)
        );
        assert_eq!(
            resolver
                .resolve(&state, 2, None, Some(2), settings())
                .vector,
            Vec3::X
        );
        assert_eq!(
            resolver
                .resolve(&state, 2, None, Some(1), settings())
                .vector,
            Vec3::Z
        );
        state.objects.insert(1, scene(1, [0.0; 3]));
        assert_eq!(
            resolver
                .resolve(&state, 3, Some(&[1].into()), Some(1), settings())
                .failure,
            Some(Failure::InvalidGravity)
        );
        state.objects.get_mut(&1).unwrap().properties.clear();
        assert_eq!(
            resolver
                .resolve(&state, 4, Some(&[1].into()), Some(1), settings())
                .failure,
            Some(Failure::MissingGravity)
        );
        state
            .objects
            .get_mut(&1)
            .unwrap()
            .properties
            .insert("Gravity".into(), Value::F32(9.81));
        assert_eq!(
            resolver
                .resolve(&state, 5, Some(&[1].into()), Some(1), settings())
                .failure,
            Some(Failure::InvalidGravity)
        );
    }

    #[test]
    fn gravity_index_updates_incrementally_and_ignores_unpublished_state_changes() {
        let mut state = SceneState::default();
        state.objects.insert(1, scene(1, [0.0, -1.0, 0.0]));
        let mut resolver = UpResolver::default();
        assert_eq!(
            resolver.resolve(&state, 0, None, None, settings()).vector,
            Vec3::Y
        );
        state.objects.insert(1, scene(1, [0.0, 0.0, -1.0]));
        assert_eq!(
            resolver.resolve(&state, 0, None, None, settings()).vector,
            Vec3::Y
        );
        assert_eq!(
            resolver
                .resolve(&state, 1, Some(&[1].into()), None, settings())
                .vector,
            Vec3::Z
        );
        state.objects.insert(2, scene(2, [0.0, 0.0, -3.0]));
        assert_eq!(
            resolver
                .resolve(&state, 2, Some(&[2].into()), None, settings())
                .vector,
            Vec3::Z
        );
        state.objects.remove(&1);
        assert_eq!(
            resolver
                .resolve(&state, 3, Some(&[1].into()), None, settings())
                .vector,
            Vec3::Z
        );
        assert_eq!(resolver.full_scans, 1);
        state.objects.clear();
        assert!(
            resolver
                .resolve(&state, 4, None, None, settings())
                .is_fallback()
        );
        assert_eq!(resolver.full_scans, 2);
    }

    #[test]
    fn invalid_custom_and_gravity_vectors_use_the_selected_preset() {
        let mut state = SceneState::default();
        let mut resolver = UpResolver::default();
        for (revision, vector) in [[0.0; 3], [f32::NAN, 1.0, 0.0], [f32::INFINITY, 0.0, 0.0]]
            .into_iter()
            .enumerate()
        {
            let custom = UpSettings {
                mode: UpMode::Custom,
                preset: UpAxis::NegativeX,
                custom: vector,
            };
            let resolution = resolver.resolve(&state, revision as u64, None, None, custom);
            assert_eq!(resolution.vector, Vec3::NEG_X);
            assert!(resolution.is_fallback());
            state.objects.insert(1, scene(1, vector));
            let automatic = UpSettings {
                mode: UpMode::Auto,
                ..custom
            };
            assert!(
                resolver
                    .resolve(&state, revision as u64, None, None, automatic)
                    .is_fallback()
            );
        }
        for vector in [
            Vec3::new(1.0, 2.0, -3.0),
            Vec3::splat(f32::MAX),
            Vec3::new(f32::MIN_POSITIVE, 0.0, 0.0),
        ] {
            assert!(unit_vector(vector).unwrap().is_finite());
            assert!((unit_vector(vector).unwrap().length() - 1.0).abs() < 1e-6);
        }
    }
}
