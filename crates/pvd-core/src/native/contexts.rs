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
