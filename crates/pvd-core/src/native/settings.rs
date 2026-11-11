use super::*;
use crate::SourceCamera;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Default)]
struct DisplaySettings {
    pickable: Option<bool>,
    color: Option<[u8; 4]>,
    top_level: Option<bool>,
}

struct CameraEntry {
    name: String,
    seen: bool,
    generation: Option<u32>,
}

#[derive(Clone, Copy)]
struct CameraOwner {
    id: u64,
    generation: u32,
    value: SourceCamera,
}

#[derive(Default)]
pub(super) struct Settings {
    display: BTreeMap<u64, DisplaySettings>,
    cameras: BTreeMap<u64, CameraEntry>,
    owners: BTreeMap<String, CameraOwner>,
}

impl Settings {
    pub(super) fn read_display(
        &mut self,
        id: u64,
        reader: &mut Reader<'_>,
        allocation: &mut usize,
    ) -> Result<(), Error> {
        let flags = reader.u32()?;
        let color = reader.take(4)?.try_into()?;
        let boolean = |value| match value {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(error("invalid PVD3 instance display boolean")),
        };
        let pickable = boolean(reader.take(1)?[0])?;
        let top_level = boolean(reader.take(1)?[0])?;
        if flags & !7 != 0 {
            return Err(error("unsupported PVD3 instance display flags"));
        }
        if flags != 0 {
            metadata_budget(allocation, 1, 160)?;
            self.display.insert(
                id,
                DisplaySettings {
                    pickable: (flags & 1 != 0).then_some(pickable),
                    color: (flags & 2 != 0).then_some(color),
                    top_level: (flags & 4 != 0).then_some(top_level),
                },
            );
        }
        Ok(())
    }

    pub(super) fn read_cameras(
        &mut self,
        reader: &mut Reader<'_>,
        metadata: &Metadata,
        sources: &BTreeMap<u64, u64>,
        allocation: &mut usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let count = reader.count()?;
        metadata_budget(allocation, count, 64)?;
        let mut reverse = BTreeSet::new();
        for _ in 0..count {
            if !proceed() {
                return Err(cancelled());
            }
            let source = reader.u64()?;
            let id = u64::from(reader.u32()?);
            if sources.get(&id) != Some(&source) || !reverse.insert(source) {
                return Err(error("invalid PVD3 source reverse map"));
            }
        }
        let count = reader.count()?;
        metadata_budget(allocation, count, 512)?;
        if count != 0 {
            validate_camera_schema(metadata)?;
        }
        let mut names = BTreeSet::new();
        for _ in 0..count {
            if !proceed() {
                return Err(cancelled());
            }
            let handle = reader.u32()?;
            let id = u64::from(reader.u32()?);
            let name = metadata
                .strings
                .get(&handle)
                .ok_or_else(|| error("undefined PVD3 camera name handle"))?;
            metadata_budget(allocation, name.len(), 3)?;
            if !names.insert(name.as_str())
                || self
                    .cameras
                    .insert(
                        id,
                        CameraEntry {
                            name: name.clone(),
                            seen: false,
                            generation: None,
                        },
                    )
                    .is_some()
            {
                return Err(error("duplicate PVD3 camera name or instance"));
            }
        }
        Ok(())
    }

    pub(super) fn initialize(
        &mut self,
        builder: &mut RecordingBuilder,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        for &id in self.display.keys() {
            if !proceed() {
                return Err(cancelled());
            }
            self.apply_display(builder, id)?;
        }
        let ids = self.cameras.keys().copied().collect::<Vec<_>>();
        for id in ids {
            if !proceed() {
                return Err(cancelled());
            }
            self.sync_camera(builder, id)?;
        }
        Ok(())
    }

    pub(super) fn apply_display(
        &self,
        builder: &mut RecordingBuilder,
        id: u64,
    ) -> Result<(), Error> {
        let Some(settings) = self.display.get(&id) else {
            return Ok(());
        };
        if !builder.state.objects.contains_key(&id) {
            return Ok(());
        }
        // The communication directory stores saved preferences, without change timestamps.
        if let Some(value) = settings.pickable {
            builder.action(Action::Set(id, "$pickable".into(), Value::Bool(value)))?;
        }
        if let Some(value) = settings.color {
            builder.action(Action::Set(
                id,
                "$color".into(),
                Value::Bytes(Arc::from(value)),
            ))?;
        }
        if let Some(value) = settings.top_level {
            builder.action(Action::Set(id, "$top_level".into(), Value::Bool(value)))?;
        }
        Ok(())
    }

    pub(super) fn sync_camera(
        &mut self,
        builder: &mut RecordingBuilder,
        id: u64,
    ) -> Result<(), Error> {
        let Some(entry) = self.cameras.get_mut(&id) else {
            return Ok(());
        };
        let candidate = if let Some(object) = builder.state.objects.get(&id) {
            entry.seen = true;
            if object.type_name != "Camera" {
                return Err(error("PVD3 camera directory targets a non-camera instance"));
            }
            let value = camera_value(&entry.name, object)?;
            value.map(|value| CameraOwner {
                id,
                generation: object.generation,
                value,
            })
        } else {
            None
        };
        if let Some(generation) = entry.generation
            && candidate.is_none_or(|camera| camera.generation != generation)
            && let Some(owner) = self.owners.get(&entry.name).copied()
            && owner.id == id
            && owner.generation == generation
        {
            // Retiring a lifetime must not remove a camera published by another owner.
            if builder.state.cameras.get(&entry.name) == Some(&owner.value) {
                builder.action(Action::RemoveCamera(entry.name.clone()))?;
            }
            self.owners.remove(&entry.name);
        }
        entry.generation = candidate.map(|camera| camera.generation);
        if let Some(owner) = candidate {
            builder.action(Action::Camera(entry.name.clone(), owner.value))?;
            self.owners.insert(entry.name.clone(), owner);
        }
        Ok(())
    }

    pub(super) fn validate_references(&self) -> Result<(), Error> {
        if self.cameras.values().any(|camera| !camera.seen) {
            return Err(error("PVD3 camera directory targets a missing instance"));
        }
        Ok(())
    }
}

fn camera_value(name: &str, object: &crate::Object) -> Result<Option<SourceCamera>, Error> {
    match object.properties.get("Name") {
        None => return Ok(None),
        Some(Value::String(value)) if value == name => {}
        Some(Value::String(value)) if value.is_empty() => return Ok(None),
        _ => return Err(error("PVD3 camera name differs from its directory")),
    }
    let vector = |key| match object.properties.get(key) {
        None => Ok(None),
        Some(Value::Vec3(value)) => Ok(Some(*value)),
        _ => Err(error("unsupported PVD3 camera vector value")),
    };
    Ok(
        match (vector("Origin")?, vector("Up")?, vector("Target")?) {
            (Some(position), Some(up), Some(target)) => Some(SourceCamera {
                position,
                up,
                target,
            }),
            _ => None,
        },
    )
}

fn validate_camera_schema(metadata: &Metadata) -> Result<(), Error> {
    let label = |handle| metadata.strings.get(&handle).map(String::as_str);
    let mut classes = metadata.classes.iter().flatten().filter(|class| {
        label(class.name.namespace) == Some("") && label(class.name.name) == Some("Camera")
    });
    let class = classes
        .next()
        .ok_or_else(|| error("missing PVD3 Camera class"))?;
    if classes.next().is_some()
        || class.base != u32::MAX
        || class.size != 40
        || class.dynamic
        || class.properties.len() != 4
    {
        return Err(error("unsupported PVD3 Camera class layout"));
    }
    let mut found = BTreeSet::new();
    for &id in &class.properties {
        let property = metadata.property(id)?;
        let (datatype, size, offset) = match label(property.name) {
            Some("Name") => ("StringHandle", 4, 0),
            Some("Origin") => ("PxVec3", 12, 4),
            Some("Up") => ("PxVec3", 12, 16),
            Some("Target") => ("PxVec3", 12, 28),
            _ => return Err(error("unsupported PVD3 Camera property")),
        };
        let value_class = metadata.class(property.datatype_id)?;
        if property.owner != class.name
            || property.kind != 1
            || property.offset32 != offset
            || label(property.datatype.namespace) != Some("physx3")
            || label(property.datatype.name) != Some(datatype)
            || value_class.name != property.datatype
            || value_class.size != size
            || !found.insert(offset)
        {
            return Err(error("unsupported PVD3 Camera property layout"));
        }
    }
    Ok(())
}
