use crate::{Action, Error, Object, RecordingBuilder, SceneState, Value, error};
use std::{collections::BTreeSet, sync::Arc};

const MAX_PLAN_BYTES: usize = 64 << 20;

fn check_progress(progress: &mut dyn FnMut() -> bool) -> Result<(), Error> {
    if progress() {
        Ok(())
    } else {
        Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "origin shift cancelled").into())
    }
}

struct Plan {
    actions: Vec<Action>,
    visited: BTreeSet<u64>,
    remaining: usize,
}

impl Plan {
    fn charge(&mut self, bytes: usize) -> Result<(), Error> {
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .ok_or_else(|| error("origin shift exceeds its bounded update budget"))?;
        Ok(())
    }

    fn actor(
        &mut self,
        object: &Object,
        shift: [f32; 3],
        progress: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        if self.visited.contains(&object.id) {
            return Ok(());
        }
        self.charge(128)?;
        self.visited.insert(object.id);
        let properties: &[&str] = match object.type_name.as_str() {
            "physx3::PxRigidStatic" | "physx3::PxArticulationLink" | "physx3::PxCloth" => {
                &["GlobalPose", "WorldBounds"]
            }
            "physx3::PxRigidDynamic" => &["GlobalPose", "WorldBounds", "KinematicTarget"],
            "physx3::PxParticleSystem" | "physx3::PxParticleFluid" => {
                &["Positions", "WorldBounds", "ProjectionPlane"]
            }
            _ => {
                return Err(error(format!(
                    "unsupported origin-shift actor {}",
                    object.type_name
                )));
            }
        };
        for property in properties {
            self.property(object, property, shift, progress)?;
        }
        Ok(())
    }

    fn property(
        &mut self,
        object: &Object,
        property: &str,
        shift: [f32; 3],
        progress: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let Some(value) = object.properties.get(property) else {
            return Ok(());
        };
        check_progress(progress)?;
        self.charge(256 + property.len())?;
        let value = match (property, value) {
            ("GlobalPose" | "KinematicTarget", Value::Transform { position, rotation }) => {
                Value::Transform {
                    position: std::array::from_fn(|axis| position[axis] - shift[axis]),
                    rotation: *rotation,
                }
            }
            ("WorldBounds", _) => {
                let values = scalars::<6>(value, property)?;
                self.charge(6 * std::mem::size_of::<Value>())?;
                Value::Array(
                    values
                        .into_iter()
                        .enumerate()
                        .map(|(index, value)| Value::F32(value - shift[index % 3]))
                        .collect::<Vec<_>>()
                        .into(),
                )
            }
            ("ProjectionPlane", _) => {
                let [x, y, z, distance] = scalars::<4>(value, property)?;
                self.charge(4 * std::mem::size_of::<Value>())?;
                Value::Array(
                    [
                        x,
                        y,
                        z,
                        distance + (x * shift[0] + y * shift[1] + z * shift[2]),
                    ]
                    .into_iter()
                    .map(Value::F32)
                    .collect::<Vec<_>>()
                    .into(),
                )
            }
            ("Positions", Value::Array(values)) => {
                self.charge(
                    values
                        .len()
                        .checked_mul(std::mem::size_of::<Value>())
                        .ok_or_else(|| error("origin-shift particle size overflow"))?,
                )?;
                let mut positions = Vec::new();
                positions.try_reserve_exact(values.len())?;
                for value in values.iter() {
                    check_progress(progress)?;
                    let Value::Vec3(position) = value else {
                        return Err(error("origin-shift Positions has a non-vector element"));
                    };
                    positions.push(Value::Vec3(std::array::from_fn(|axis| {
                        position[axis] - shift[axis]
                    })));
                }
                Value::Array(Arc::new(positions))
            }
            _ => return Err(error(format!("unsupported origin-shift {property} layout"))),
        };
        self.actions
            .push(Action::Set(object.id, property.into(), value));
        Ok(())
    }

    fn links(
        &mut self,
        state: &SceneState,
        owner: &Object,
        shift: [f32; 3],
        depth: usize,
        progress: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        if depth > 64 {
            return Err(error("origin-shift articulation exceeds 64 link levels"));
        }
        for link in children(state, owner, "Links")? {
            check_progress(progress)?;
            let link = link?;
            if link.type_name != "physx3::PxArticulationLink" {
                return Err(error("origin-shift Links contains an unsupported object"));
            }
            if !self.visited.contains(&link.id) {
                self.actor(link, shift, progress)?;
                self.links(state, link, shift, depth + 1, progress)?;
            }
        }
        Ok(())
    }
}

fn scalars<const N: usize>(value: &Value, property: &str) -> Result<[f32; N], Error> {
    let invalid = || {
        error(format!(
            "origin-shift {property} requires {N} PhysX scalars"
        ))
    };
    let Value::Array(values) = value else {
        return Err(invalid());
    };
    if values.len() != N {
        return Err(invalid());
    }
    let mut result = [0.0; N];
    for (target, value) in result.iter_mut().zip(values.iter()) {
        let Value::F32(value) = value else {
            return Err(invalid());
        };
        *target = *value;
    }
    Ok(result)
}

fn children<'a>(
    state: &'a SceneState,
    owner: &'a Object,
    property: &'a str,
) -> Result<impl Iterator<Item = Result<&'a Object, Error>>, Error> {
    let values = match owner.properties.get(property) {
        None => &[][..],
        Some(Value::Array(values)) => values.as_slice(),
        _ => {
            return Err(error(format!(
                "origin-shift {property} is not a reference array"
            )));
        }
    };
    Ok(values.iter().filter_map(move |value| match value {
        Value::Ref(id) => state.resolve_reference(owner.id, property, *id).map(Ok),
        _ => Some(Err(error(format!(
            "origin-shift {property} contains a non-reference"
        )))),
    }))
}

impl RecordingBuilder {
    pub(crate) fn shift_origin(
        &mut self,
        id: u64,
        shift: [f32; 3],
        progress: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        check_progress(progress)?;
        if !shift.iter().all(|value| value.is_finite()) {
            return Err(error("origin shift contains a non-finite displacement"));
        }
        let scene = self
            .state
            .objects
            .get(&id)
            .filter(|object| object.type_name == "physx3::PxScene")
            .ok_or_else(|| error("origin shift targets a missing or unsupported scene"))?;
        let state_bytes = self.streaming.as_ref().map_or_else(
            || {
                self.validation
                    .as_ref()
                    .map_or(0, |validation| validation.state_bytes)
            },
            |live| live.state_bytes,
        );
        // Planning, retained actions and replacement values share the existing scene budget.
        // Validate every layout before publishing any part of the world-space update.
        let budget = self
            .memory_limit
            .saturating_sub(self.retained_bytes)
            .saturating_sub(self.recording.frame_sources.index_bytes())
            .saturating_sub(self.draw_history_bytes())
            .saturating_sub(state_bytes)
            / 4;
        let mut plan = Plan {
            actions: Vec::new(),
            visited: BTreeSet::new(),
            remaining: budget.min(MAX_PLAN_BYTES),
        };
        plan.charge(512)?;
        for property in [
            "RigidStatics",
            "RigidDynamics",
            "Cloths",
            "ParticleSystems",
            "ParticleFluids",
        ] {
            for object in children(&self.state, scene, property)? {
                check_progress(progress)?;
                plan.actor(object?, shift, progress)?;
            }
        }
        for articulation in children(&self.state, scene, "Articulations")? {
            check_progress(progress)?;
            let articulation = articulation?;
            if articulation.type_name != "physx3::PxArticulation" {
                return Err(error("unsupported origin-shift articulation type"));
            }
            plan.links(&self.state, articulation, shift, 0, progress)?;
        }
        if self.streaming.is_none()
            && self.validation.is_none()
            && self
                .recording
                .actions
                .len()
                .saturating_add(plan.actions.len() + 1)
                > 20_000_000
        {
            return Err(error("origin shift exceeds the recording action limit"));
        }
        check_progress(progress)?;
        for action in plan.actions {
            self.action(action)?;
        }
        self.action(Action::Set(id, "$origin_shift".into(), Value::Vec3(shift)))
    }
}

#[cfg(test)]
mod tests;
