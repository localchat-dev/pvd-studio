use super::*;

type SceneLifetime = (u64, u32);

const ENTRY_BYTES: usize =
    2 * 96 + size_of::<(SceneLifetime, u64)>() + size_of::<(u64, Option<SceneLifetime>)>();

#[derive(Debug, Default)]
pub(crate) struct SceneContexts {
    scenes: BTreeMap<SceneLifetime, u64>,
    owners: BTreeMap<u64, Option<SceneLifetime>>,
}

impl SceneContexts {
    fn insert(&mut self, lifetime: SceneLifetime, source: u64) {
        self.scenes.insert(lifetime, source);
        self.owners
            .entry(source)
            .and_modify(|owner| {
                if *owner != Some(lifetime) {
                    // Creation events have no clock that proves the boundary between reused contexts.
                    *owner = None;
                }
            })
            .or_insert(Some(lifetime));
    }

    pub(crate) fn source(&self, scene: u64, generation: u32) -> Option<u64> {
        let lifetime = (scene, generation);
        let source = *self.scenes.get(&lifetime)?;
        (self.owners.get(&source) == Some(&Some(lifetime))).then_some(source)
    }
}

pub(super) struct Collector(SceneContexts);

impl Collector {
    pub(super) fn from_state(
        builder: &mut RecordingBuilder,
        sources: &BTreeMap<u64, u64>,
        allocation: &mut usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Self, Error> {
        let count = builder
            .state
            .objects
            .values()
            .filter(|object| {
                object.type_name == "physx3::PxScene" && sources.contains_key(&object.id)
            })
            .count();
        let bytes = count
            .checked_mul(ENTRY_BYTES)
            .and_then(|bytes| {
                bytes.checked_add(size_of::<SceneContexts>() + 2 * size_of::<usize>())
            })
            .ok_or_else(|| error("PVD3 Scene context allocation overflow"))?;
        metadata_budget(allocation, 1, bytes)?;
        builder.reserve_history(bytes)?;
        let mut contexts = SceneContexts::default();
        for object in builder.state.objects.values() {
            if !proceed() {
                return Err(cancelled());
            }
            if object.type_name == "physx3::PxScene"
                && let Some(source) = sources.get(&object.id)
            {
                contexts.insert((object.id, object.generation), *source);
            }
        }
        Ok(Self(contexts))
    }

    pub(super) fn observe(
        &mut self,
        builder: &mut RecordingBuilder,
        sources: &BTreeMap<u64, u64>,
        id: u64,
        allocation: &mut usize,
    ) -> Result<(), Error> {
        let Some(object) = builder.state.objects.get(&id) else {
            return Ok(());
        };
        let Some(source) = sources.get(&id) else {
            return Ok(());
        };
        if object.type_name != "physx3::PxScene" {
            return Ok(());
        }
        let lifetime = (id, object.generation);
        if !self.0.scenes.contains_key(&lifetime) {
            metadata_budget(allocation, 1, ENTRY_BYTES)?;
            builder.reserve_history(ENTRY_BYTES)?;
            self.0.insert(lifetime, *source);
        }
        Ok(())
    }

    pub(super) fn install(self, builder: &mut RecordingBuilder) {
        builder.recording.native_scene_contexts = Some(Arc::new(self.0));
    }
}

pub(super) fn install(
    builder: &mut RecordingBuilder,
    sources: &BTreeMap<u64, u64>,
    allocation: &mut usize,
    progress: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    let mut count = 0usize;
    for action in builder.recording.actions.iter() {
        if !progress() {
            return Err(cancelled());
        }
        if matches!(action, Action::Create(object) if object.type_name == "physx3::PxScene") {
            count = count
                .checked_add(1)
                .ok_or_else(|| error("PVD3 Scene context count overflow"))?;
        }
    }
    let bytes = count
        .checked_mul(ENTRY_BYTES)
        .and_then(|bytes| bytes.checked_add(size_of::<SceneContexts>() + 2 * size_of::<usize>()))
        .ok_or_else(|| error("PVD3 Scene context allocation overflow"))?;
    metadata_budget(allocation, 1, bytes)?;
    builder.reserve_history(bytes)?;
    let mut contexts = SceneContexts::default();
    for action in builder.recording.actions.iter() {
        if !progress() {
            return Err(cancelled());
        }
        if let Action::Create(object) = action
            && object.type_name == "physx3::PxScene"
            && let Some(source) = sources.get(&object.id)
        {
            contexts.insert((object.id, object.generation), *source);
        }
    }
    if !progress() {
        return Err(cancelled());
    }
    builder.recording.native_scene_contexts = Some(Arc::new(contexts));
    Ok(())
}

#[cfg(test)]
mod tests;
