use std::collections::BTreeSet;

use crate::{Object, SceneState, Value, frame_sources::FrameObject, live::SceneUpdate};

const DEPENDENCIES: &[&str] = &[
    "Aggregates",
    "Articulations",
    "RigidStatics",
    "RigidDynamics",
    "Actors",
    "ParticleSystems",
    "ParticleFluids",
    "Cloths",
    "Joints",
    "Constraints",
    "Links",
    "InboundJoint",
    "Shapes",
    "Geometry",
    "Materials",
    "ConvexMesh",
    "TriangleMesh",
    "HeightField",
    "Fabric",
];

pub(crate) fn affects_membership(property: &str) -> bool {
    matches!(property, "Scene" | "Parent") || DEPENDENCIES.contains(&property)
}

#[derive(Default)]
pub struct RoomScope {
    room: Option<FrameObject>,
    revision: Option<u64>,
    version: u64,
    objects: BTreeSet<u64>,
    actors: BTreeSet<u64>,
    scenes: BTreeSet<u64>,
    cloths: BTreeSet<u64>,
    particles: BTreeSet<u64>,
}

impl RoomScope {
    pub fn room(&self) -> Option<FrameObject> {
        self.room
    }

    pub fn contains(&self, id: u64) -> bool {
        self.room.is_none() || self.objects.contains(&id)
    }

    pub fn revision(&self) -> Option<u64> {
        self.revision
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn objects(&self) -> &BTreeSet<u64> {
        &self.objects
    }

    pub fn actors(&self) -> &BTreeSet<u64> {
        &self.actors
    }

    pub fn scenes(&self) -> &BTreeSet<u64> {
        &self.scenes
    }

    pub fn cloths(&self) -> &BTreeSet<u64> {
        &self.cloths
    }

    pub fn particles(&self) -> &BTreeSet<u64> {
        &self.particles
    }

    pub fn retain_for_update(&mut self, update: &SceneUpdate, state: &SceneState, revision: u64) {
        if self.revision != Some(revision) || update.replace_all {
            return;
        }
        for (id, next) in update.changed_objects() {
            let (Some(previous), Some(next)) = (state.objects.get(&id), next) else {
                return;
            };
            if previous.generation != next.generation
                || previous.type_name != next.type_name
                || update.changes_object_properties(state, id, DEPENDENCIES)
                || update.changes_object_properties(state, id, &["Scene", "Parent"])
            {
                return;
            }
        }
        self.revision = Some(revision.wrapping_add(1));
    }

    pub fn update(&mut self, state: &SceneState, room: Option<FrameObject>, revision: u64) {
        if self.room == room && self.revision == Some(revision) {
            return;
        }
        let previous = std::mem::take(&mut self.objects);
        let previous_room = self.room;
        self.room = room;
        self.revision = Some(revision);
        self.rebuild(state);
        if self.room != previous_room || self.objects != previous {
            self.version = self.version.wrapping_add(1);
        }
    }

    fn rebuild(&mut self, state: &SceneState) {
        self.actors.clear();
        self.scenes.clear();
        self.cloths.clear();
        self.particles.clear();
        let Some(room) = self.room else {
            return;
        };
        let Some(scene) = state.objects.get(&room.id).filter(|scene| {
            scene.generation == room.generation && scene.type_name.ends_with("::PxScene")
        }) else {
            return;
        };
        let mut pending = BTreeSet::from([scene.id]);
        for object in state.objects.values() {
            if scene_for_object(state, object) == Some(room) {
                pending.insert(object.id);
            }
        }
        while let Some(id) = pending.pop_first() {
            if self.objects.contains(&id) {
                continue;
            }
            let Some(object) = state.objects.get(&id) else {
                continue;
            };
            if object.type_name.ends_with("::PxScene") && id != room.id {
                continue;
            }
            if object.properties.contains_key("Scene")
                && scene_for_object(state, object) != Some(room)
            {
                continue;
            }
            if object.type_name.ends_with("::PxArticulationLink")
                && scene_for_object(state, object) != Some(room)
            {
                continue;
            }
            self.objects.insert(id);
            match object.type_name.rsplit("::").next() {
                Some("PxScene") => {
                    self.scenes.insert(id);
                }
                Some("PxRigidStatic" | "PxRigidDynamic" | "PxArticulationLink") => {
                    self.actors.insert(id);
                }
                Some("PxCloth") => {
                    self.cloths.insert(id);
                }
                Some("PxParticleSystem" | "PxParticleFluid") => {
                    self.particles.insert(id);
                }
                _ => {}
            }
            for property in DEPENDENCIES {
                for target in references(object.properties.get(*property)) {
                    if state.resolve_reference(id, property, target).is_some()
                        && !self.objects.contains(&target)
                    {
                        pending.insert(target);
                    }
                }
            }
        }
    }
}

fn references(value: Option<&Value>) -> impl Iterator<Item = u64> + '_ {
    let (single, array) = match value {
        Some(Value::Ref(id)) => (Some(*id), None),
        Some(Value::Array(values)) => (None, Some(values)),
        _ => (None, None),
    };
    single
        .into_iter()
        .chain(array.into_iter().flat_map(|values| {
            values.iter().filter_map(|value| match value {
                Value::Ref(id) => Some(*id),
                _ => None,
            })
        }))
}

pub fn scene_for_object(state: &SceneState, object: &Object) -> Option<FrameObject> {
    let owner = if object.type_name.ends_with("::PxArticulationLink") {
        state.articulation_for_link(object.id)?
    } else {
        object
    };
    let mut scenes = references(owner.properties.get("Scene"));
    let target = scenes.next()?;
    if scenes.next().is_some() {
        return None;
    }
    let scene = state.resolve_reference(owner.id, "Scene", target)?;
    scene
        .type_name
        .ends_with("::PxScene")
        .then_some(FrameObject {
            id: scene.id,
            generation: scene.generation,
        })
}
