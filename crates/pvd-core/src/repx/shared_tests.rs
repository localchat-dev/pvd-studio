use crate::{
    Object, Recording, SceneState, Value,
    repx::{self, Selection},
};

fn capture() -> Recording {
    let path = std::env::var_os("PVD_REPX_SHARED_CAPTURE")
        .expect("PVD_REPX_SHARED_CAPTURE must name the tools/repx-shared-shapes capture");
    Recording::from_path(path).unwrap()
}

fn named<'a>(state: &'a SceneState, name: &str) -> &'a Object {
    state
        .objects
        .values()
        .find(|object| object.properties.get("Name") == Some(&Value::String(name.into())))
        .unwrap()
}

fn referenced(state: &SceneState, owner: u64, field: &str) -> u64 {
    match &state.objects[&owner].properties[field] {
        Value::Ref(id) => *id,
        Value::Array(values) => match &values[0] {
            Value::Ref(id) => *id,
            _ => panic!("expected reference"),
        },
        _ => panic!("expected references"),
    }
}

#[test]
#[ignore = "requires real PhysX 3.4 tools/repx-shared-shapes five-stage capture"]
fn shared_shapes_keep_unique_resources_and_selected_attachment_closure() {
    let recording = capture();
    assert_eq!(recording.frame_count(), 5);
    let counts = [
        (2, 2, 1, 2),
        (1, 1, 1, 1),
        (2, 1, 1, 2),
        (2, 1, 1, 2),
        (2, 2, 2, 2),
    ];
    let first = recording.state_at(0).unwrap();
    let first_shared = named(&first, "SharedFirst");
    let first_owner = named(&first, "OwnerA").id;
    let old_generation = first_shared.generation;
    for (frame, (actors, shapes, materials, attachments)) in counts.into_iter().enumerate() {
        let state = recording.state_at(frame).unwrap();
        let shared = named(
            &state,
            if frame < 3 {
                "SharedFirst"
            } else {
                "SharedAfterDetach"
            },
        );
        assert_eq!(shared.id, first_shared.id);
        if frame < 3 {
            assert_eq!(shared.generation, old_generation);
        } else {
            assert!(shared.generation > old_generation);
        }
        let exported = repx::export(&state, Selection::Frame).unwrap();
        assert_eq!(
            (exported.actors, exported.shapes, exported.materials),
            (actors, shapes, materials)
        );
        assert_eq!(exported.xml.matches("<PxShapeRef>").count(), attachments);
        assert_eq!(
            exported
                .xml
                .matches(&format!("<Id>{}</Id>", shared.id))
                .count(),
            1
        );
        for actor in state.objects.values().filter(|o| {
            o.type_name.ends_with("::PxRigidStatic") || o.type_name.ends_with("::PxRigidDynamic")
        }) {
            let selected = repx::export(&state, Selection::Actor(actor.id)).unwrap();
            let Value::Array(shapes) = &actor.properties["Shapes"] else {
                panic!()
            };
            assert_eq!(selected.actors, 1);
            assert_eq!(selected.shapes, shapes.len());
            assert_eq!(selected.xml.matches("<PxShapeRef>").count(), 1);
        }
        println!(
            "frame={frame} shared={} generation={} actor_hint={:?}",
            shared.id, shared.generation, shared.properties["Actor"]
        );
    }
    let detached = recording.state_at(1).unwrap();
    let shared = named(&detached, "SharedFirst");
    assert_eq!(shared.properties["Actor"], Value::Ref(first_owner));
    assert!(
        detached
            .resolve_reference(shared.id, "Actor", first_owner)
            .is_none()
    );
    assert!(!detached.objects.contains_key(&first_owner));
    assert!(repx::export(&detached, Selection::Frame).is_ok());
    let mut reused = detached.clone();
    let mut new_actor = first.objects[&first_owner].clone();
    new_actor.generation += 1;
    new_actor
        .properties
        .insert("Shapes".into(), Value::Array(Vec::new().into()));
    reused.objects.insert(first_owner, new_actor);
    let selected = repx::export(&reused, Selection::Actor(named(&detached, "OwnerB").id)).unwrap();
    assert!(!selected.xml.contains("OwnerA"));
    assert_eq!(selected.actors, 1);
}

#[test]
#[ignore = "requires real PhysX 3.4 tools/repx-shared-shapes capture"]
fn shared_authoritative_references_and_shape_properties_are_validated() {
    let recording = capture();
    let original = recording.state_at(0).unwrap();
    let shape = named(&original, "SharedFirst").id;
    let actor = named(&original, "OwnerB").id;
    let physics = original
        .objects
        .values()
        .find(|o| o.type_name.ends_with("::PxPhysics"))
        .unwrap()
        .id;
    let geometry = referenced(&original, shape, "Geometry");
    let material = referenced(&original, shape, "Materials");
    for (id, field, replacement) in [
        (shape, "IsExclusive", Value::U64(0)),
        (shape, "IsExclusive", Value::Bool(true)),
        (shape, "Actor", Value::Array(vec![Value::Ref(actor)].into())),
        (shape, "Flags", Value::U64(u64::MAX)),
        (shape, "ContactOffset", Value::F32(-1.0)),
        (shape, "RestOffset", Value::F32(f32::INFINITY)),
        (shape, "GeometryType", Value::U64(0)),
        (shape, "Name", Value::String("bad\0name".into())),
        (shape, "Materials", Value::Array(Vec::new().into())),
        (geometry, "Shape", Value::Ref(actor)),
        (geometry, "HalfExtents", Value::Vec3([0.0, 1.0, 1.0])),
        (material, "StaticFriction", Value::F32(-1.0)),
        (physics, "SharedShapes", Value::Array(Vec::new().into())),
        (
            physics,
            "SharedShapes",
            Value::Array(vec![Value::Ref(shape); 2].into()),
        ),
        (
            physics,
            "SharedShapes",
            Value::Array(vec![Value::U64(shape)].into()),
        ),
        (
            actor,
            "Shapes",
            Value::Array(vec![Value::Ref(shape); 2].into()),
        ),
    ] {
        let mut state = original.clone();
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .properties
            .insert(field.into(), replacement);
        assert!(
            repx::export(&state, Selection::Frame).is_err(),
            "accepted {field}"
        );
    }
    for id in [shape, geometry, material] {
        let mut state = original.clone();
        state.objects.get_mut(&id).unwrap().generation += 1;
        assert!(
            repx::export(&state, Selection::Frame).is_err(),
            "accepted stale object {id}"
        );
    }
    for field in ["IsExclusive", "Actor", "Geometry", "Materials", "LocalPose"] {
        let mut state = original.clone();
        state
            .objects
            .get_mut(&shape)
            .unwrap()
            .properties
            .remove(field);
        assert!(
            repx::export(&state, Selection::Frame).is_err(),
            "accepted absent {field}"
        );
    }
    let mut stale_registration = original.clone();
    *std::sync::Arc::make_mut(
        stale_registration
            .reference_lifetimes
            .get_mut(&(physics, "SharedShapes".into()))
            .unwrap(),
    )
    .get_mut(&shape)
    .unwrap() += 1;
    assert!(repx::export(&stale_registration, Selection::Frame).is_err());
    let mut extension = original.clone();
    extension
        .objects
        .get_mut(&shape)
        .unwrap()
        .properties
        .insert("FutureShapeField".into(), Value::U64(1));
    assert!(repx::export(&extension, Selection::Frame).is_err());
}

#[test]
#[ignore = "requires real PhysX 3.4 tools/repx-shared-shapes capture"]
fn shared_geometry_restrictions_include_every_selected_owner() {
    let recording = capture();
    let mut state = recording.state_at(0).unwrap();
    let shape = named(&state, "SharedFirst").id;
    let static_actor = named(&state, "OwnerA").id;
    let geometry = referenced(&state, shape, "Geometry");
    state
        .objects
        .get_mut(&shape)
        .unwrap()
        .properties
        .insert("GeometryType".into(), Value::U64(1));
    let geometry = state.objects.get_mut(&geometry).unwrap();
    geometry.type_name = "physx3::PxPlaneGeometry".into();
    geometry.properties.remove("HalfExtents");
    assert!(repx::export(&state, Selection::Actor(static_actor)).is_ok());
    let error = repx::export(&state, Selection::Frame)
        .err()
        .unwrap()
        .to_string();
    assert!(
        error.contains("non-kinematic dynamic actors cannot simulate plane"),
        "{error}"
    );
}
