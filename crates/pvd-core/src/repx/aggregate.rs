use super::*;

#[cfg(test)]
mod tests;

pub(super) struct SceneMemberships(BTreeSet<u64>);

impl SceneMemberships {
    pub fn new(state: &SceneState) -> Self {
        let invalid = state
            .objects
            .values()
            .filter(|object| kind(object) == "PxScene")
            .filter_map(|scene| {
                let valid = [("Aggregates", "PxAggregate"), ("Articulations", "PxArticulation"), ("ParticleSystems", "PxParticleSystem"), ("ParticleFluids", "PxParticleFluid")].into_iter().all(|(field, expected)| {
                    let mut seen = BTreeSet::new();
                    match scene.properties.get(field) {
                        None => true,
                        Some(Value::Array(values)) => values.iter().all(|value| {
                            let Value::Ref(id) = value else { return false; };
                            seen.insert(*id) && state.resolve_reference(scene.id, field, *id).is_some_and(|target| {
                                kind(target) == expected
                                    && matches!(target.properties.get("Scene"), Some(Value::Ref(id)) if *id == scene.id)
                                    && state.resolve_reference(target.id, "Scene", scene.id).is_some()
                            })
                        }),
                        _ => false,
                    }
                });
                (!valid).then_some(scene.id)
            })
            .collect();
        Self(invalid)
    }

    pub fn validate_all(&self) -> Result<(), Error> {
        if !self.0.is_empty() {
            return Err(error(
                "RepX Scene has invalid, missing or stale group or Particle references",
            ));
        }
        Ok(())
    }

    pub fn validate_actor(&self, state: &SceneState, actor: &Object) -> Result<(), Error> {
        if self.0.is_empty() {
            return Ok(());
        }
        // Missing groups leave membership unknown for every Actor in their Scene.
        let scene = match actor.properties.get("Scene") {
            Some(Value::Ref(id)) => state
                .resolve_reference(actor.id, "Scene", *id)
                .filter(|object| kind(object) == "PxScene"),
            _ => None,
        }
        .ok_or_else(|| {
            error(
                "RepX cannot establish Actor Scene while group or Particle references are invalid",
            )
        })?;
        if self.0.contains(&scene.id) {
            return Err(error(
                "RepX Actor Scene has invalid, missing or stale group or Particle references",
            ));
        }
        Ok(())
    }
}

pub(super) struct Owners<'a>(BTreeMap<u64, Vec<&'a Object>>);

impl<'a> Owners<'a> {
    pub fn new(state: &'a SceneState) -> Self {
        let mut owners: BTreeMap<u64, Vec<&Object>> = BTreeMap::new();
        // SDK 3.4 filters PxAggregate pointers from Actor properties; membership comes from Actors.
        for aggregate in state
            .objects
            .values()
            .filter(|object| kind(object) == "PxAggregate")
        {
            let mut pending: Vec<_> = ["Actors", "Articulations"]
                .into_iter()
                .filter_map(|name| aggregate.properties.get(name))
                .collect();
            let mut ids = BTreeSet::new();
            while let Some(value) = pending.pop() {
                match value {
                    Value::Ref(id) => {
                        ids.insert(*id);
                    }
                    Value::Array(values) => pending.extend(values.iter()),
                    _ => {}
                }
            }
            for id in ids {
                owners.entry(id).or_default().push(aggregate);
            }
        }
        Self(owners)
    }

    pub fn get(&self, state: &SceneState, actor: &Object) -> Result<Option<&'a Object>, Error> {
        let owner = match self.0.get(&actor.id).map(Vec::as_slice).unwrap_or_default() {
            [] => None,
            [owner] => Some(*owner),
            _ => return Err(error("RepX Actor belongs to multiple Aggregates")),
        };
        validate_inverse(state, actor, owner.map(|object| object.id))?;
        Ok(owner)
    }
}

fn validate_inverse(state: &SceneState, actor: &Object, owner: Option<u64>) -> Result<(), Error> {
    let expected = match actor.properties.get("Aggregate") {
        None => return Ok(()),
        Some(Value::Ref(0)) => None,
        Some(Value::Ref(id)) => {
            let target = state
                .resolve_reference(actor.id, "Aggregate", *id)
                .ok_or_else(|| error("RepX Actor has missing or stale Aggregate membership"))?;
            if kind(target) != "PxAggregate" {
                return Err(error("RepX Actor Aggregate must refer to PxAggregate"));
            }
            Some(*id)
        }
        _ => return Err(error("RepX Actor Aggregate must be an object reference")),
    };
    if expected != owner {
        return Err(error("RepX Aggregate and Actor membership disagree"));
    }
    Ok(())
}

fn array<'a>(
    state: &'a SceneState,
    aggregate: &Object,
    field: &str,
) -> Result<Vec<&'a Object>, Error> {
    // SDK collection properties remain absent until the first membership update.
    match aggregate.properties.get(field) {
        None => Ok(Vec::new()),
        Some(Value::Array(values)) if values.len() <= 128 => references(state, aggregate, field),
        Some(Value::Array(_)) => Err(error("RepX Aggregate membership exceeds 128 entries")),
        _ => Err(error(format!(
            "RepX Aggregate {field} must be a reference array"
        ))),
    }
}

pub(super) fn members<'a>(
    state: &'a SceneState,
    aggregate: &Object,
) -> Result<Vec<&'a Object>, Error> {
    validate_fields(
        aggregate,
        &[
            "MaxNbActors",
            "SelfCollision",
            "ConcreteTypeName",
            "Scene",
            "Actors",
            "Articulations",
        ],
        &[],
    )?;
    if !matches!(value(aggregate, "ConcreteTypeName")?, Value::String(name) if name == "PxAggregate")
    {
        return Err(error("RepX Aggregate concrete type is inconsistent"));
    }
    if !matches!(value(aggregate, "SelfCollision")?, Value::Bool(_)) {
        return Err(error("RepX Aggregate SelfCollision must be boolean"));
    }
    let Value::Ref(scene_id) = value(aggregate, "Scene")? else {
        return Err(error("RepX Aggregate Scene must be an object reference"));
    };
    let scene = state
        .resolve_reference(aggregate.id, "Scene", *scene_id)
        .ok_or_else(|| error("RepX Aggregate has missing or stale Scene membership"))?;
    if kind(scene) != "PxScene"
        || !matches!(scene.properties.get("Aggregates"), Some(Value::Array(values))
            if values.iter().filter(|value| **value == Value::Ref(aggregate.id)).count() == 1)
        || state
            .resolve_reference(scene.id, "Aggregates", aggregate.id)
            .is_none()
    {
        return Err(error("RepX Aggregate and Scene membership disagree"));
    }
    let capacity = integer(aggregate, "MaxNbActors")?;
    if capacity > 128 {
        return Err(error(
            "RepX Aggregate capacity exceeds the PhysX 3.4 limit of 128",
        ));
    }
    let mut actors = array(state, aggregate, "Actors")?;
    if actors
        .iter()
        .any(|actor| !matches!(kind(actor), "PxRigidDynamic" | "PxRigidStatic"))
    {
        return Err(error("RepX Aggregate Actors requires rigid Actor members"));
    }
    let articulations = array(state, aggregate, "Articulations")?;
    let mut count = actors.len();
    for articulation in &articulations {
        if kind(articulation) != "PxArticulation"
            || articulation.properties.get("Scene") != aggregate.properties.get("Scene")
        {
            return Err(error(
                "RepX Aggregate Articulation type or Scene is inconsistent",
            ));
        }
        count += articulation::links(state, articulation)?.len();
    }
    if count as u64 > capacity {
        return Err(error("RepX Aggregate membership exceeds its capacity"));
    }
    actors.extend(articulations);
    let mut seen = BTreeSet::new();
    for actor in &actors {
        if !seen.insert(actor.id) {
            return Err(error(
                "RepX Aggregate requires distinct Actor and Articulation members",
            ));
        }
        validate_inverse(state, actor, Some(aggregate.id))?;
    }
    Ok(actors)
}

pub(super) fn validate_selection(
    state: &SceneState,
    actors: &[&Object],
    articulations: &[&Object],
    aggregates: &[&Object],
) -> Result<(), Error> {
    let ids: BTreeSet<_> = actors
        .iter()
        .chain(articulations)
        .map(|actor| actor.id)
        .collect();
    let mut memberships = BTreeMap::new();
    for aggregate in aggregates {
        for actor in members(state, aggregate)? {
            if !ids.contains(&actor.id) || memberships.insert(actor.id, aggregate.id).is_some() {
                return Err(error(
                    "RepX Aggregate dependency selection is incomplete or duplicated",
                ));
            }
        }
    }
    for actor in actors.iter().chain(articulations) {
        validate_inverse(state, actor, memberships.get(&actor.id).copied())?;
    }
    Ok(())
}

pub(super) fn write(xml: &mut String, state: &SceneState, aggregate: &Object) -> Result<(), Error> {
    let actors = members(state, aggregate)?;
    let Value::Bool(self_collision) = value(aggregate, "SelfCollision")? else {
        return Err(error("RepX Aggregate SelfCollision must be boolean"));
    };
    xml.push_str("<PxAggregate>\n");
    tag(xml, "Id", &aggregate.id.to_string())?;
    xml.push_str("<Actors>\n");
    let mut count = 0;
    for actor in &actors {
        let is_articulation = kind(actor) == "PxArticulation";
        tag(
            xml,
            if is_articulation {
                "PxArticulationRef"
            } else {
                "PxActorRef"
            },
            &actor.id.to_string(),
        )?;
        count += if is_articulation {
            articulation::links(state, actor)?.len()
        } else {
            1
        };
    }
    xml.push_str("</Actors>\n");
    tag(xml, "NumActors", &count.to_string())?;
    property(xml, aggregate, "MaxNbActors")?;
    tag(
        xml,
        "SelfCollision",
        if *self_collision { "true" } else { "false" },
    )?;
    xml.push_str("</PxAggregate>\n");
    Ok(())
}
