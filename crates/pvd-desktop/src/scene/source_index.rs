use pvd_core::{Object, SceneState};
use std::collections::{BTreeSet, HashSet};

const MAX_SOURCE_IDS: usize = 100_000;

#[derive(Default)]
pub(super) struct SourceIndex {
    pub scenes: BTreeSet<u64>,
    pub cloths: BTreeSet<u64>,
    pub particles: BTreeSet<u64>,
    pub complete: bool,
}

impl SourceIndex {
    pub fn update(&mut self, state: &SceneState, changed: Option<&HashSet<u64>>) {
        self.update_with_limit(state, changed, MAX_SOURCE_IDS);
    }

    fn update_with_limit(
        &mut self,
        state: &SceneState,
        changed: Option<&HashSet<u64>>,
        limit: usize,
    ) {
        if self.complete
            && let Some(changed) = changed
        {
            for id in changed {
                self.scenes.remove(id);
                self.cloths.remove(id);
                self.particles.remove(id);
            }
            for object in changed.iter().filter_map(|id| state.objects.get(id)) {
                if !self.insert(object, limit) {
                    self.clear();
                    return;
                }
            }
        } else {
            self.clear();
            for object in state.objects.values() {
                if !self.insert(object, limit) {
                    self.clear();
                    return;
                }
            }
        }
        self.complete = true;
    }

    fn insert(&mut self, object: &Object, limit: usize) -> bool {
        let count = self.scenes.len() + self.cloths.len() + self.particles.len();
        let ids = if object.type_name.ends_with("::PxScene") {
            &mut self.scenes
        } else if object.type_name.ends_with("::PxCloth") {
            &mut self.cloths
        } else if matches!(
            object.type_name.as_str(),
            "physx3::PxParticleSystem" | "physx3::PxParticleFluid"
        ) {
            &mut self.particles
        } else {
            return true;
        };
        if count >= limit {
            return false;
        }
        ids.insert(object.id);
        true
    }

    fn clear(&mut self) {
        self.scenes.clear();
        self.cloths.clear();
        self.particles.clear();
        self.complete = false;
    }
}

#[cfg(test)]
#[path = "source_index/tests.rs"]
mod tests;
