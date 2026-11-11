use pvd_core::{
    Object, Recording, Value,
    repx::{self, Selection},
};
use std::{collections::BTreeSet, path::PathBuf};

fn fixture() -> (Recording, PathBuf) {
    let directory = std::env::var_os("PVD_REPX_AGGREGATE_DIRECTORY")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../captures/repx-aggregates")
        });
    (
        Recording::from_path(directory.join("capture.pvdstream")).unwrap(),
        directory,
    )
}

fn aggregate(object: &Object) -> bool {
    object.type_name == "physx3::PxAggregate"
}

#[test]
#[ignore = "requires tools/repx-aggregates capture and same-run getter truth"]
fn rigid_aggregate_snapshots_and_selected_components_preserve_membership() {
    let (recording, directory) = fixture();
    assert_eq!(recording.frame_count(), 4);
    for frame in [3, 0, 2, 1] {
        let truth: serde_json::Value = serde_json::from_slice(
            &std::fs::read(directory.join(format!("frame-{frame}-truth.json"))).unwrap(),
        )
        .unwrap();
        let state = recording.state_at(frame).unwrap();
        assert!(state.errors.is_empty());
        let exported = repx::export(&state, Selection::Frame).unwrap();
        assert_eq!(
            (exported.actors, exported.aggregates, exported.joints),
            (5, 3, usize::from(frame == 0 || frame == 2))
        );
        for expected in truth["aggregates"].as_array().unwrap() {
            let id = expected["id"].as_u64().unwrap();
            let object = &state.objects[&id];
            assert!(aggregate(object));
            assert_eq!(
                object.properties["MaxNbActors"],
                Value::U64(expected["capacity"].as_u64().unwrap())
            );
            assert_eq!(
                object.properties["SelfCollision"],
                Value::Bool(expected["self_collision"].as_bool().unwrap())
            );
            let members = match object.properties.get("Actors") {
                Some(Value::Array(values)) => values
                    .iter()
                    .map(|value| match value {
                        Value::Ref(id) => *id,
                        _ => panic!(),
                    })
                    .collect::<BTreeSet<_>>(),
                None => BTreeSet::new(),
                _ => panic!(),
            };
            assert_eq!(
                members,
                expected["members"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|value| value.as_u64().unwrap())
                    .collect()
            );
            for member in members {
                let actor = state.resolve_reference(id, "Actors", member).unwrap();
                assert!(!actor.properties.contains_key("Aggregate"));
                let selected = repx::export(&state, Selection::Actor(actor.id)).unwrap();
                assert!(
                    selected
                        .xml
                        .contains(&format!("<PxAggregate>\n<Id>{id}</Id>"))
                );
                assert!(selected.aggregates > 0);
            }
        }
        let unrelated = truth["actors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|actor| actor["name"] == "unrelated")
            .unwrap()["id"]
            .as_u64()
            .unwrap();
        let selected = repx::export(&state, Selection::Actor(unrelated)).unwrap();
        assert_eq!(
            (selected.actors, selected.aggregates, selected.joints),
            (1, 0, 0)
        );
    }
}

#[test]
#[ignore = "requires tools/repx-aggregates capture"]
fn malformed_aggregate_fields_membership_and_generations_are_rejected() {
    let (recording, _) = fixture();
    let state = recording.state_at(0).unwrap();
    let owner = state.objects.values().find(|object| aggregate(object) && matches!(object.properties.get("Actors"), Some(Value::Array(values)) if !values.is_empty())).unwrap();
    let Value::Array(members) = &owner.properties["Actors"] else {
        panic!()
    };
    let Value::Ref(actor) = members[0] else {
        panic!()
    };
    for (field, replacement) in [
        ("MaxNbActors", Value::U64(129)),
        ("MaxNbActors", Value::U64(0)),
        ("MaxNbActors", Value::Bool(true)),
        ("SelfCollision", Value::U64(0)),
        (
            "ConcreteTypeName",
            Value::String("PxCustomAggregate".into()),
        ),
        ("Actors", Value::Ref(actor)),
        (
            "Actors",
            Value::Array(vec![Value::Ref(actor), Value::Ref(actor)].into()),
        ),
        ("Actors", Value::Array(vec![Value::Ref(u64::MAX)].into())),
        ("Articulations", Value::Ref(0)),
        (
            "Articulations",
            Value::Array(vec![Value::Ref(actor)].into()),
        ),
        ("UnknownProperty", Value::U64(1)),
    ] {
        let mut invalid = state.clone();
        let incident = field != "Actors"
            || match &replacement {
                Value::Ref(id) => *id == actor,
                Value::Array(values) => values.contains(&Value::Ref(actor)),
                _ => false,
            };
        invalid
            .objects
            .get_mut(&owner.id)
            .unwrap()
            .properties
            .insert(field.into(), replacement);
        assert!(
            repx::export(&invalid, Selection::Frame).is_err(),
            "accepted invalid {field}"
        );
        if incident {
            assert!(
                repx::export(&invalid, Selection::Actor(actor)).is_err(),
                "selected closure accepted invalid {field}"
            );
        }
    }
    for field in ["MaxNbActors", "SelfCollision", "ConcreteTypeName", "Scene"] {
        let mut invalid = state.clone();
        invalid
            .objects
            .get_mut(&owner.id)
            .unwrap()
            .properties
            .remove(field);
        assert!(
            repx::export(&invalid, Selection::Frame).is_err(),
            "accepted missing {field}"
        );
        assert!(repx::export(&invalid, Selection::Actor(actor)).is_err());
    }
    for id in [owner.id, actor] {
        let mut invalid = state.clone();
        invalid.objects.get_mut(&id).unwrap().generation += 1;
        assert!(repx::export(&invalid, Selection::Frame).is_err());
        assert!(repx::export(&invalid, Selection::Actor(actor)).is_err());
    }
    let mut invalid = state.clone();
    invalid.objects.remove(&actor);
    assert!(repx::export(&invalid, Selection::Frame).is_err());
    let mut invalid = state.clone();
    invalid.objects.remove(&owner.id);
    assert!(repx::export(&invalid, Selection::Frame).is_err());
    assert!(repx::export(&invalid, Selection::Actor(actor)).is_err());
    for replacement in [Value::Ref(0), Value::U64(owner.id)] {
        let mut invalid = state.clone();
        invalid
            .objects
            .get_mut(&actor)
            .unwrap()
            .properties
            .insert("Aggregate".into(), replacement);
        assert!(repx::export(&invalid, Selection::Frame).is_err());
        assert!(repx::export(&invalid, Selection::Actor(actor)).is_err());
    }
}
