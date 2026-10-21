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
        let property = match object.type_name.as_str() {
            "physx3::PxRigidStatic"
            | "physx3::PxRigidDynamic"
            | "physx3::PxArticulationLink"
            | "physx3::PxCloth" => "GlobalPose",
            "physx3::PxParticleSystem" | "physx3::PxParticleFluid" => "Positions",
            _ => {
                return Err(error(format!(
                    "unsupported origin-shift actor {}",
                    object.type_name
                )));
            }
        };
        let Some(value) = object.properties.get(property) else {
            return Ok(());
        };
        self.charge(256 + property.len())?;
        let value = match (property, value) {
            ("GlobalPose", Value::Transform { position, rotation }) => Value::Transform {
                position: std::array::from_fn(|axis| position[axis] - shift[axis]),
                rotation: *rotation,
            },
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
