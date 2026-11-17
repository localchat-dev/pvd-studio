//! Scene identities observed over a capture, independent of the replay position.

use std::{collections::BTreeMap, sync::Arc};

use crate::{Action, Object, SceneState, frame_sources::FrameObject};

pub(crate) const ROOM_LIMIT: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomInfo {
    pub scene: FrameObject,
    pub number: u64,
    /// Position of the next complete frame when this identity was first observed.
    pub first_observed_frame: usize,
    /// A lifecycle event position, not an additional simulation frame.
    pub destroyed_at_frame: Option<usize>,
}

#[derive(Clone, Debug, Default)]
struct Index {
    entries: BTreeMap<u64, RoomInfo>,
    identities: BTreeMap<FrameObject, u64>,
    observed: u64,
    revision: u64,
}

#[derive(Clone, Debug, Default)]
pub struct RoomDirectory {
    index: Arc<Index>,
}

impl RoomDirectory {
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &RoomInfo> {
        self.index.entries.values()
    }

    pub fn get(&self, scene: FrameObject) -> Option<&RoomInfo> {
        let number = self.index.identities.get(&scene)?;
        self.index.entries.get(number)
    }

    pub fn len(&self) -> usize {
        self.index.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.entries.is_empty()
    }

    pub fn revision(&self) -> u64 {
        self.index.revision
    }

    pub fn observed_count(&self) -> u64 {
        self.index.observed
    }

    pub fn omitted_count(&self) -> u64 {
        self.index.observed.saturating_sub(self.len() as u64)
    }

    pub fn memory_bytes(&self) -> usize {
        std::mem::size_of::<Index>() + self.len() * 256
    }

    pub(crate) fn observe(&mut self, scene: FrameObject, frame: usize) {
        if self.get(scene).is_some() {
            return;
        }
        let index = Arc::make_mut(&mut self.index);
        index.observed = index.observed.saturating_add(1);
        index.revision = index.revision.saturating_add(1);
        if index.entries.len() == ROOM_LIMIT {
            let evicted = index
                .entries
                .iter()
                .find(|(_, entry)| entry.destroyed_at_frame.is_some())
                .map(|(&number, _)| number);
            let Some(number) = evicted else {
                return;
            };
            let entry = index.entries.remove(&number).unwrap();
            index.identities.remove(&entry.scene);
        }
        index.identities.insert(scene, index.observed);
        index.entries.insert(
            index.observed,
            RoomInfo {
                scene,
                number: index.observed,
                first_observed_frame: frame,
                destroyed_at_frame: None,
            },
        );
    }

    pub(crate) fn destroy(&mut self, scene: FrameObject, frame: usize) {
        let Some(entry) = self.get(scene) else {
            return;
        };
        if entry.destroyed_at_frame.is_some() {
            return;
        }
        let number = entry.number;
        let index = Arc::make_mut(&mut self.index);
        index.entries.get_mut(&number).unwrap().destroyed_at_frame = Some(frame);
        index.revision = index.revision.saturating_add(1);
    }

    pub(crate) fn observe_state(&mut self, state: &SceneState, frame: usize) {
        for object in state.objects.values() {
            if let Some(scene) = scene_identity(object) {
                self.observe(scene, frame);
            }
        }
    }

    pub(crate) fn from_entries(
        entries: impl IntoIterator<Item = RoomInfo>,
        observed: u64,
    ) -> Result<Self, crate::Error> {
        let mut index = Index {
            observed,
            revision: 1,
            ..Default::default()
        };
        for entry in entries {
            if index.entries.len() >= ROOM_LIMIT
                || entry.number == 0
                || entry.number > observed
                || entry
                    .destroyed_at_frame
                    .is_some_and(|frame| frame < entry.first_observed_frame)
                || index.entries.contains_key(&entry.number)
                || index.identities.contains_key(&entry.scene)
            {
                return Err(crate::error("invalid capture room directory"));
            }
            index.identities.insert(entry.scene, entry.number);
            index.entries.insert(entry.number, entry);
        }
        Ok(Self {
            index: Arc::new(index),
        })
    }
}

fn scene_identity(object: &Object) -> Option<FrameObject> {
    (object.type_name == "physx3::PxScene").then_some(FrameObject {
        id: object.id,
        generation: object.generation,
    })
}

pub(crate) enum Change {
    Create(FrameObject),
    Destroy(FrameObject),
}

impl Change {
    pub(crate) fn from_action(action: &Action, state: &SceneState) -> Option<Self> {
        match action {
            Action::Create(object) => scene_identity(object).map(Self::Create),
            Action::Destroy(id) => state
                .objects
                .get(id)
                .and_then(scene_identity)
                .map(Self::Destroy),
            _ => None,
        }
    }

    pub(crate) fn apply(self, directory: &mut RoomDirectory, frame: usize) {
        match self {
            Self::Create(scene) => directory.observe(scene, frame),
            Self::Destroy(scene) => directory.destroy(scene, frame),
        }
    }
}

#[cfg(test)]
mod tests;
