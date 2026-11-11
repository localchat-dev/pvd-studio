use super::*;
use crate::Action;

fn fixture() -> SceneState {
    let mut state = SceneState::default();
    for (id, name) in [
        (1, "PxRigidDynamic"),
        (2, "PxRigidStatic"),
        (3, "PxRigidDynamic"),
        (10, "PxAggregate"),
        (20, "PxScene"),
    ] {
        Action::Create(Object {
            id,
            generation: 0,
            type_name: format!("physx3::{name}"),
            properties: BTreeMap::new(),
        })
        .apply(&mut state);
    }
    for (field, value) in [
        ("MaxNbActors", Value::U64(2)),
        ("SelfCollision", Value::Bool(true)),
        ("ConcreteTypeName", Value::String("PxAggregate".into())),
        ("Scene", Value::Ref(20)),
        (
            "Actors",
            Value::Array(vec![Value::Ref(1), Value::Ref(2)].into()),
        ),
    ] {
        Action::Set(10, field.into(), value).apply(&mut state);
    }
    Action::Set(
        20,
        "Aggregates".into(),
        Value::Array(vec![Value::Ref(10)].into()),
    )
    .apply(&mut state);
    for id in [1, 2, 3] {
        Action::Set(id, "Scene".into(), Value::Ref(20)).apply(&mut state);
    }
    state
}

#[test]
fn selected_member_includes_complete_aggregate_and_rejects_stale_membership() {
    let state = fixture();
    for id in [1, 2] {
        let selected = selection::Plan::new(&state, Selection::Actor(id)).unwrap();
        assert_eq!(
            selected
                .actors
                .iter()
                .map(|actor| actor.id)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(selected.aggregates.len(), 1);
    }
    assert!(
        selection::Plan::new(&state, Selection::Actor(3))
            .unwrap()
            .aggregates
            .is_empty()
    );
    for id in [1, 2, 10, 20] {
        let mut invalid = state.clone();
        invalid.objects.get_mut(&id).unwrap().generation += 1;
        assert!(selection::Plan::new(&invalid, Selection::Actor(1)).is_err());
    }
    for id in [1, 2] {
        let mut invalid = state.clone();
        Action::Set(id, "Aggregate".into(), Value::Ref(0)).apply(&mut invalid);
        assert!(selection::Plan::new(&invalid, Selection::Actor(1)).is_err());
    }
}

#[test]
fn aggregate_capacity_and_missing_scalar_fields_never_fall_back_to_defaults() {
    let mut state = fixture();
    for field in ["MaxNbActors", "SelfCollision", "ConcreteTypeName", "Scene"] {
        let mut invalid = state.clone();
        invalid
            .objects
            .get_mut(&10)
            .unwrap()
            .properties
            .remove(field);
        assert!(members(&invalid, &invalid.objects[&10]).is_err());
    }
    for capacity in [0, 1, 129, u64::MAX] {
        let mut invalid = state.clone();
        invalid
            .objects
            .get_mut(&10)
            .unwrap()
            .properties
            .insert("MaxNbActors".into(), Value::U64(capacity));
        assert!(members(&invalid, &invalid.objects[&10]).is_err());
    }
    state
        .objects
        .get_mut(&10)
        .unwrap()
        .properties
        .remove("Actors");
    state
        .objects
        .get_mut(&10)
        .unwrap()
        .properties
        .insert("MaxNbActors".into(), Value::U64(0));
    assert!(members(&state, &state.objects[&10]).unwrap().is_empty());
    assert!(
        selection::Plan::new(&state, Selection::Actor(1))
            .unwrap()
            .aggregates
            .is_empty()
    );
    Action::Set(1, "Aggregate".into(), Value::Ref(10)).apply(&mut state);
    assert!(selection::Plan::new(&state, Selection::Actor(1)).is_err());
}

#[test]
fn duplicate_ownership_is_rejected_without_blocking_unrelated_selection() {
    let mut state = fixture();
    let mut other = state.objects[&10].clone();
    other.id = 11;
    Action::Create(other).apply(&mut state);
    assert!(selection::Plan::new(&state, Selection::Actor(1)).is_err());
    assert!(selection::Plan::new(&state, Selection::Actor(3)).is_ok());
    Action::Set(11, "Actors".into(), Value::Ref(1)).apply(&mut state);
    assert!(selection::Plan::new(&state, Selection::Actor(1)).is_err());
    assert!(selection::Plan::new(&state, Selection::Actor(3)).is_ok());
}

#[test]
fn aggregate_xml_preserves_both_self_collision_modes() {
    let mut state = fixture();
    for enabled in [false, true] {
        Action::Set(10, "SelfCollision".into(), Value::Bool(enabled)).apply(&mut state);
        let mut xml = String::new();
        write(&mut xml, &state, &state.objects[&10]).unwrap();
        assert!(xml.contains(&format!("<SelfCollision>{enabled}</SelfCollision>")));
        assert!(xml.contains("<NumActors>2</NumActors>"));
    }
}

#[test]
fn missing_or_misattributed_aggregate_blocks_only_uncertain_scenes() {
    let mut state = fixture();
    state.objects.remove(&10);
    assert!(selection::Plan::new(&state, Selection::Frame).is_err());
    for actor in [1, 2, 3] {
        assert!(selection::Plan::new(&state, Selection::Actor(actor)).is_err());
    }
    Action::Create(Object {
        id: 30,
        generation: 0,
        type_name: "physx3::PxScene".into(),
        properties: BTreeMap::new(),
    })
    .apply(&mut state);
    Action::Set(3, "Scene".into(), Value::Ref(30)).apply(&mut state);
    let selected = selection::Plan::new(&state, Selection::Actor(3)).unwrap();
    assert_eq!(selected.actors.len(), 1);
    assert!(selected.aggregates.is_empty());

    let mut misattributed = fixture();
    Action::Create(state.objects[&30].clone()).apply(&mut misattributed);
    Action::Set(3, "Scene".into(), Value::Ref(30)).apply(&mut misattributed);
    Action::Set(
        30,
        "Aggregates".into(),
        Value::Array(vec![Value::Ref(10)].into()),
    )
    .apply(&mut misattributed);
    assert!(selection::Plan::new(&misattributed, Selection::Frame).is_err());
    assert!(selection::Plan::new(&misattributed, Selection::Actor(3)).is_err());
    let selected = selection::Plan::new(&misattributed, Selection::Actor(1)).unwrap();
    assert_eq!(selected.actors.len(), 2);
    assert_eq!(selected.aggregates.len(), 1);
}
