use super::*;
use crate::Action;

fn set(state: &mut SceneState, id: u64, field: &str, value: Value) {
    Action::Set(id, field.into(), value).apply(state);
}

fn object(state: &mut SceneState, id: u64, name: &str) {
    Action::Create(Object {
        id,
        generation: 0,
        type_name: format!("physx3::{name}"),
        properties: BTreeMap::new(),
    })
    .apply(state);
}

fn graph() -> SceneState {
    let mut state = SceneState::default();
    for id in 1..=4 {
        object(&mut state, id, "PxRigidDynamic");
    }
    for (joint, a, b) in [(10, 0, 1), (11, 1, 2), (12, 2, 3), (13, 3, 1)] {
        object(&mut state, joint, "PxFixedJoint");
        for (field, target) in [("Actors.actor0", a), ("Actors.actor1", b)] {
            set(&mut state, joint, field, Value::Ref(target));
        }
    }
    for (id, joints) in [
        (1, vec![10, 11, 13]),
        (2, vec![11, 12]),
        (3, vec![12, 13]),
        (4, vec![]),
    ] {
        set(
            &mut state,
            id,
            "Joints",
            Value::Array(
                joints
                    .into_iter()
                    .map(Value::Ref)
                    .collect::<Vec<_>>()
                    .into(),
            ),
        );
    }
    state
}

#[test]
fn connected_selection_traverses_cycles_and_world_without_unrelated_actors() {
    let state = graph();
    for start in [1, 2, 3] {
        let plan = Plan::new(&state, Selection::Actor(start)).unwrap();
        assert_eq!(
            plan.actors.iter().map(|o| o.id).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(
            plan.joints.iter().map(|o| o.id).collect::<Vec<_>>(),
            [10, 11, 12, 13]
        );
    }
    let single = Plan::new(&state, Selection::Actor(4)).unwrap();
    assert_eq!(single.actors.len(), 1);
    assert!(single.joints.is_empty());
}

#[test]
fn selected_dependency_links_require_current_generations_and_rigid_endpoints() {
    for target in [2, 12] {
        let mut state = graph();
        state.objects.get_mut(&target).unwrap().generation += 1;
        assert!(Plan::new(&state, Selection::Actor(1)).is_err());
        state.objects.remove(&target);
        assert!(Plan::new(&state, Selection::Actor(1)).is_err());
    }
    let mut state = graph();
    state.objects.get_mut(&3).unwrap().type_name = "physx3::PxArticulationLink".into();
    assert!(Plan::new(&state, Selection::Actor(1)).is_err());
    assert!(Plan::new(&state, Selection::Actor(3)).is_err());
    assert!(Plan::new(&state, Selection::Actor(999)).is_err());
}

#[test]
fn unsupported_dependencies_on_transitively_selected_actors_are_not_dropped() {
    let mut state = graph();
    object(&mut state, 99, "PxConstraint");
    set(&mut state, 99, "Actors.actor0", Value::Ref(3));
    assert!(Plan::new(&state, Selection::Actor(1)).is_err());
    assert!(Plan::new(&state, Selection::Actor(4)).is_ok());
    assert!(Plan::new(&state, Selection::Frame).is_err());
}

#[test]
fn malformed_or_missing_actor_links_cannot_silently_omit_a_joint() {
    for links in [
        Value::Ref(11),
        Value::Array(vec![Value::Ref(11), Value::Ref(11)].into()),
        Value::Array(vec![Value::Ref(12)].into()),
    ] {
        let mut state = graph();
        set(&mut state, 1, "Joints", links);
        assert!(Plan::new(&state, Selection::Actor(1)).is_err());
    }
    let mut state = graph();
    state
        .objects
        .get_mut(&1)
        .unwrap()
        .properties
        .remove("Joints");
    assert!(Plan::new(&state, Selection::Actor(1)).is_err());
}

fn batch(state: &SceneState, ids: &[u64]) -> Selection {
    Selection::Actors(
        ids.iter()
            .map(|id| ActorKey {
                id: *id,
                generation: state.objects.get(id).map_or(0, |object| object.generation),
            })
            .collect(),
    )
}

#[test]
fn multiple_selected_actors_merge_connected_graphs_and_deduplicate() {
    let state = graph();
    for ids in [&[1, 4][..], &[4, 1, 2, 4][..]] {
        let plan = Plan::new(&state, batch(&state, ids)).unwrap();
        assert_eq!(
            plan.actors
                .iter()
                .map(|object| object.id)
                .collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
        assert_eq!(
            plan.joints
                .iter()
                .map(|object| object.id)
                .collect::<Vec<_>>(),
            [10, 11, 12, 13]
        );
    }
}

#[test]
fn batch_selection_rejects_empty_oversized_missing_unsupported_and_stale_items() {
    let mut state = graph();
    object(&mut state, 99, "PxShape");
    for ids in [&[][..], &[4, 999][..], &[4, 99][..]] {
        assert!(Plan::new(&state, batch(&state, ids)).is_err());
    }
    let key = ActorKey {
        id: 4,
        generation: state.objects[&4].generation,
    };
    assert!(Plan::new(&state, Selection::Actors(vec![key; MAX_SELECTED_ACTORS])).is_ok());
    assert!(
        Plan::new(
            &state,
            Selection::Actors(vec![key; MAX_SELECTED_ACTORS + 1])
        )
        .is_err()
    );
    let stale = ActorKey {
        generation: key.generation + 1,
        ..key
    };
    for keys in [vec![stale], vec![key, stale], vec![stale, key]] {
        assert!(Plan::new(&state, Selection::Actors(keys)).is_err());
    }
    object(&mut state, 100, "PxConstraint");
    set(&mut state, 100, "Actors.actor0", Value::Ref(3));
    assert!(Plan::new(&state, batch(&state, &[4])).is_ok());
    assert!(Plan::new(&state, batch(&state, &[4, 1])).is_err());
}
