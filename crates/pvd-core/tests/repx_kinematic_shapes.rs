use pvd_core::{
    Object, Recording, SceneState, Value,
    repx::{self, Selection},
};
use std::path::PathBuf;

fn named<'a>(state: &'a SceneState, name: &str) -> &'a Object {
    state
        .objects
        .values()
        .find(|object| object.properties.get("Name") == Some(&Value::String(name.into())))
        .unwrap()
}

fn reference(object: &Object, field: &str) -> u64 {
    match &object.properties[field] {
        Value::Ref(id) => *id,
        Value::Array(values) if values.len() == 1 => {
            let Value::Ref(id) = values[0] else {
                panic!("expected reference")
            };
            id
        }
        _ => panic!("expected reference"),
    }
}

#[test]
#[ignore = "requires the same-run tools/repx-kinematic-shapes SDK capture"]
fn kinematic_geometry_snapshots_preserve_shared_dependencies_and_reject_invalid_owners() {
    let directory = PathBuf::from(
        std::env::var_os("PVD_REPX_KINEMATIC_SHAPES_DIRECTORY")
            .expect("set PVD_REPX_KINEMATIC_SHAPES_DIRECTORY"),
    );
    let fixture: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.join("fixture.json")).unwrap()).unwrap();
    assert_eq!(fixture["sdk_version"], serde_json::json!([3, 4, 2]));
    assert_eq!(fixture["schema"], 1);
    let recording = Recording::from_path(directory.join("capture.pvdstream")).unwrap();
    assert_eq!(recording.frame_count(), 1);
    let state = recording.state_at(0).unwrap();
    assert!(state.errors.is_empty());
    let exported = repx::export(&state, Selection::Frame).unwrap();
    assert_eq!(
        (
            exported.actors,
            exported.kinematic_actors,
            exported.shapes,
            exported.materials
        ),
        (8, 5, 6, 2)
    );
    assert_eq!(exported.xml.matches("<PxBVH33TriangleMesh>").count(), 1);
    assert_eq!(exported.xml.matches("<PxHeightField>").count(), 1);
    for name in [
        "kin-plane",
        "kin-triangle-exclusive",
        "kin-heightfield",
        "query-plane",
        "kin-shared-a",
        "kin-shared-b",
        "static-shared",
        "zero-flags-box",
    ] {
        let actor = named(&state, name);
        let selected = repx::export(&state, Selection::Actor(actor.id)).unwrap();
        assert_eq!((selected.actors, selected.shapes), (1, 1));
        assert_eq!(
            selected.kinematic_actors,
            usize::from(name.starts_with("kin-"))
        );
        let mut zero = state.clone();
        let shape = reference(actor, "Shapes");
        zero.objects
            .get_mut(&shape)
            .unwrap()
            .properties
            .insert("Flags".into(), Value::U64(0));
        zero.objects
            .get_mut(&actor.id)
            .unwrap()
            .properties
            .insert("ActorFlags".into(), Value::U64(0));
        let zero_export = repx::export(&zero, Selection::Actor(actor.id)).unwrap();
        assert!(zero_export.xml.contains("<ActorFlags>0</ActorFlags>"));
        assert!(zero_export.xml.contains("<Flags>0</Flags>"));
        if name.starts_with("kin-") {
            let mut invalid = state.clone();
            invalid
                .objects
                .get_mut(&actor.id)
                .unwrap()
                .properties
                .insert("RigidBodyFlags".into(), Value::U64(2));
            let error = repx::export(&invalid, Selection::Actor(actor.id))
                .err()
                .unwrap()
                .to_string();
            assert!(
                error.contains("non-kinematic dynamic actors cannot simulate"),
                "{error}"
            );
        }
    }
    let first = named(&state, "kin-shared-a");
    let second = named(&state, "kin-shared-b");
    let shared_shape = reference(first, "Shapes");
    assert_eq!(shared_shape, reference(second, "Shapes"));
    assert_eq!(
        shared_shape,
        reference(named(&state, "static-shared"), "Shapes")
    );
    assert_eq!(
        state.objects[&shared_shape].properties["IsExclusive"],
        Value::Bool(false)
    );
    let mut mixed = state.clone();
    mixed
        .objects
        .get_mut(&second.id)
        .unwrap()
        .properties
        .insert("RigidBodyFlags".into(), Value::U64(2));
    assert!(repx::export(&mixed, Selection::Frame).is_err());
    assert!(repx::export(&mixed, Selection::Actor(second.id)).is_err());
    assert!(repx::export(&mixed, Selection::Actor(first.id)).is_ok());
    for name in ["kin-triangle-exclusive", "kin-heightfield", "kin-shared-a"] {
        let actor = named(&state, name);
        let shape = reference(actor, "Shapes");
        for flags in [4, 6, 12, 14] {
            let mut invalid = state.clone();
            invalid
                .objects
                .get_mut(&shape)
                .unwrap()
                .properties
                .insert("Flags".into(), Value::U64(flags));
            let error = repx::export(&invalid, Selection::Actor(actor.id))
                .err()
                .unwrap()
                .to_string();
            assert!(
                error.contains("triangle mesh and height field triggers"),
                "{error}"
            );
        }
        let mut stale = state.clone();
        let geometry = reference(&state.objects[&shape], "Geometry");
        let resource_field = if name == "kin-heightfield" {
            "HeightField"
        } else {
            "TriangleMesh"
        };
        let resource = reference(&state.objects[&geometry], resource_field);
        stale.objects.get_mut(&resource).unwrap().generation += 1;
        assert!(repx::export(&stale, Selection::Actor(actor.id)).is_err());
        let mut missing = state.clone();
        missing.objects.remove(&resource);
        assert!(repx::export(&missing, Selection::Actor(actor.id)).is_err());
    }
    let query = named(&state, "query-plane");
    let mut simulation = state.clone();
    let shape = reference(query, "Shapes");
    simulation
        .objects
        .get_mut(&shape)
        .unwrap()
        .properties
        .insert("Flags".into(), Value::U64(11));
    assert!(repx::export(&simulation, Selection::Actor(query.id)).is_err());
}
