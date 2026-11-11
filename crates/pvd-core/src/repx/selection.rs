use super::*;

type Objects<'a> = BTreeMap<u64, &'a Object>;

pub(super) struct Plan<'a> {
    pub actors: Vec<&'a Object>,
    pub joints: Vec<&'a Object>,
    pub aggregates: Vec<&'a Object>,
    pub articulations: Vec<&'a Object>,
    pub particles: Vec<&'a Object>,
}

#[derive(Default)]
struct Graph<'a> {
    actors: Objects<'a>,
    joints: Objects<'a>,
    aggregates: Objects<'a>,
    articulations: Objects<'a>,
    particles: Objects<'a>,
}

impl<'a> Plan<'a> {
    pub fn new(state: &'a SceneState, selection: Selection) -> Result<Self, Error> {
        let scene_memberships = aggregate::SceneMemberships::new(state);
        let whole_frame = matches!(selection, Selection::Frame);
        if whole_frame {
            scene_memberships.validate_all()?;
        }
        let Graph {
            actors,
            joints,
            aggregates,
            articulations,
            particles,
        } = match selection {
            Selection::Frame => Graph {
                particles: state
                    .objects
                    .values()
                    .filter(|object| particle::supported(kind(object)))
                    .map(|object| (object.id, object))
                    .collect(),
                articulations: state
                    .objects
                    .values()
                    .filter(|object| kind(object) == "PxArticulation")
                    .map(|object| (object.id, object))
                    .collect(),
                actors: state
                    .objects
                    .values()
                    .filter(|object| {
                        matches!(
                            kind(object),
                            "PxRigidDynamic" | "PxRigidStatic" | "PxArticulationLink"
                        )
                    })
                    .map(|object| (object.id, object))
                    .collect(),
                joints: state
                    .objects
                    .values()
                    .filter(|object| supported_joint(kind(object)))
                    .map(|object| (object.id, object))
                    .collect(),
                aggregates: state
                    .objects
                    .values()
                    .filter(|object| kind(object) == "PxAggregate")
                    .map(|object| (object.id, object))
                    .collect(),
            },
            Selection::Actor(id) => Self::connected(state, &[id], &scene_memberships)?,
            Selection::Actors(keys) => {
                if keys.is_empty() || keys.len() > MAX_SELECTED_ACTORS {
                    return Err(error(format!(
                        "RepX selection requires 1 to {MAX_SELECTED_ACTORS} actors"
                    )));
                }
                let mut ids = BTreeSet::new();
                for key in keys {
                    let object = state.objects.get(&key.id).ok_or_else(|| {
                        error(format!("Actor 0x{:X} is absent in this frame", key.id))
                    })?;
                    if object.generation != key.generation {
                        return Err(error(format!(
                            "Actor 0x{:X} belongs to another generation",
                            key.id
                        )));
                    }
                    ids.insert(key.id);
                }
                Self::connected(
                    state,
                    &ids.into_iter().collect::<Vec<_>>(),
                    &scene_memberships,
                )?
            }
        };
        let dependencies = actors
            .keys()
            .chain(aggregates.keys())
            .chain(articulations.keys())
            .chain(particles.keys())
            .copied()
            .collect();
        let mut inbound_joints = BTreeSet::new();
        let mut articulated_links = BTreeSet::new();
        for articulation in articulations.values() {
            for link in articulation::links(state, articulation)? {
                if !actors.contains_key(&link.id) || !articulated_links.insert(link.id) {
                    return Err(error(
                        "RepX Articulation selection is incomplete or duplicated",
                    ));
                }
                if let Some(joint) = articulation::inbound(state, link)? {
                    inbound_joints.insert(joint.id);
                }
            }
        }
        if actors.values().any(|actor| {
            kind(actor) == "PxArticulationLink" && !articulated_links.contains(&actor.id)
        }) {
            return Err(error(
                "RepX Articulation Link is absent from its owner tree",
            ));
        }
        for object in state.objects.values() {
            let name = kind(object);
            let unsupported = (name.contains("Joint")
                && !joints.contains_key(&object.id)
                && !inbound_joints.contains(&object.id))
                || (name == "PxArticulation" && !articulations.contains_key(&object.id))
                || (name == "PxArticulationLink" && !actors.contains_key(&object.id))
                || (name.contains("Articulation")
                    && !matches!(
                        name,
                        "PxArticulation" | "PxArticulationLink" | "PxArticulationJoint"
                    ))
                || name.contains("Cloth")
                || (name.contains("Particle") && !particles.contains_key(&object.id))
                || (name == "PxAggregate" && !aggregates.contains_key(&object.id))
                || name == "PxConstraint";
            if unsupported
                && (whole_frame
                    || object
                        .properties
                        .values()
                        .any(|value| references_any(value, &dependencies)))
            {
                return Err(error(format!(
                    "RepX cannot export dependency {} (0x{:X}); nothing was exported",
                    object.type_name, object.id
                )));
            }
        }
        let plan = Self {
            actors: actors.into_values().collect(),
            joints: joints.into_values().collect(),
            aggregates: aggregates.into_values().collect(),
            articulations: articulations.into_values().collect(),
            particles: particles.into_values().collect(),
        };
        let actors: Vec<_> = plan.actors.iter().chain(&plan.particles).copied().collect();
        aggregate::validate_selection(state, &actors, &plan.articulations, &plan.aggregates)?;
        Ok(plan)
    }

    fn connected(
        state: &'a SceneState,
        ids: &[u64],
        scene_memberships: &aggregate::SceneMemberships,
    ) -> Result<Graph<'a>, Error> {
        let mut graph = Graph::default();
        let mut pending = Vec::with_capacity(ids.len());
        for id in ids {
            let actor = state
                .objects
                .get(id)
                .ok_or_else(|| error(format!("Actor 0x{id:X} is absent in this frame")))?;
            if !matches!(
                kind(actor),
                "PxRigidDynamic" | "PxRigidStatic" | "PxArticulationLink" | "PxArticulation"
            ) && !particle::supported(kind(actor))
            {
                return Err(error(format!(
                    "{} is not a supported actor",
                    actor.type_name
                )));
            }
            if particle::supported(kind(actor)) {
                graph.particles.insert(*id, actor);
            } else if kind(actor) != "PxArticulation" {
                graph.actors.insert(*id, actor);
            }
            pending.push(actor);
        }
        let owners = aggregate::Owners::new(state);
        while let Some(actor) = pending.pop() {
            if kind(actor) == "PxArticulationLink" {
                let root = articulation::owner(state, actor)?;
                if !graph.articulations.contains_key(&root.id) {
                    pending.push(root);
                }
            } else {
                scene_memberships.validate_actor(state, actor)?;
            }
            if kind(actor) == "PxArticulation" {
                if graph.articulations.insert(actor.id, actor).is_some() {
                    continue;
                }
                for link in articulation::links(state, actor)? {
                    if graph.actors.insert(link.id, link).is_none() {
                        pending.push(link);
                    }
                }
            }
            if let Some(aggregate) = owners.get(state, actor)?
                && graph.aggregates.insert(aggregate.id, aggregate).is_none()
            {
                for member in aggregate::members(state, aggregate)? {
                    if kind(member) == "PxArticulation" {
                        if !graph.articulations.contains_key(&member.id) {
                            pending.push(member);
                        }
                    } else if graph.actors.insert(member.id, member).is_none() {
                        pending.push(member);
                    }
                }
            }
            if !actor.properties.contains_key("Joints") {
                continue;
            }
            if !matches!(value(actor, "Joints")?, Value::Array(_)) {
                return Err(error("RepX Actor Joints must be a reference array"));
            }
            let mut seen = BTreeSet::new();
            for joint in references(state, actor, "Joints")? {
                if !supported_joint(kind(joint)) {
                    return Err(error(format!(
                        "RepX joint type {} is unsupported",
                        joint.type_name
                    )));
                }
                let endpoints = [
                    joint_actor(state, joint, "Actors.actor0")?,
                    joint_actor(state, joint, "Actors.actor1")?,
                ];
                if !seen.insert(joint.id)
                    || !endpoints
                        .iter()
                        .flatten()
                        .any(|endpoint| endpoint.id == actor.id)
                {
                    return Err(error("RepX Actor Joints relationships are inconsistent"));
                }
                if graph.joints.insert(joint.id, joint).is_some() {
                    continue;
                }
                for endpoint in endpoints.into_iter().flatten() {
                    if graph.actors.insert(endpoint.id, endpoint).is_none() {
                        pending.push(endpoint);
                    }
                }
            }
        }
        Ok(graph)
    }
}

#[cfg(test)]
mod tests;
