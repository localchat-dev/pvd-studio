use pvd_core::{
    Object, Recording, Value,
    repx::{self, Selection},
};
use std::path::PathBuf;

fn fixture() -> Recording {
    let path = PathBuf::from(
        std::env::var_os("PVD_REPX_ARTICULATION_DIRECTORY")
            .expect("Set PVD_REPX_ARTICULATION_DIRECTORY to the same-run capture directory"),
    );
    Recording::from_path(path.join("capture.pvdstream")).unwrap()
}
fn kind(object: &Object) -> &str {
    object.type_name.rsplit("::").next().unwrap()
}

#[test]
#[ignore = "requires tools/repx-articulations capture"]
fn articulation_trees_updates_destruction_and_selected_closures_are_exportable() {
    let recording = fixture();
    assert_eq!(recording.frame_count(), 4);
    for frame in [3, 0, 2, 1] {
        let state = recording.state_at(frame).unwrap();
        assert!(state.errors.is_empty());
        let exported = repx::export(&state, Selection::Frame).unwrap();
        let count = if frame == 2 { 1 } else { 2 };
        assert_eq!(
            (exported.articulations, exported.actors),
            (count, count * 4 + 2)
        );
        for object in state.objects.values().filter(|object| {
            matches!(
                kind(object),
                "PxArticulation" | "PxArticulationLink" | "PxRigidStatic" | "PxRigidDynamic"
            )
        }) {
            let selected = repx::export(&state, Selection::Actor(object.id)).unwrap();
            assert!(selected.actors > 0);
            if matches!(kind(object), "PxArticulation" | "PxArticulationLink") {
                assert!(selected.articulations > 0);
            }
        }
    }
}

#[test]
#[ignore = "requires tools/repx-articulations capture"]
fn malformed_articulation_fields_and_generations_fail_without_exporting_defaults() {
    let state = fixture().state_at(0).unwrap();
    let root = state
        .objects
        .values()
        .find(|object| kind(object) == "PxArticulation")
        .unwrap();
    let link = state
        .objects
        .values()
        .find(|object| kind(object) == "PxArticulationLink")
        .unwrap();
    let joint = state
        .objects
        .values()
        .find(|object| kind(object) == "PxArticulationJoint")
        .unwrap();
    for (object, field, value) in [
        (
            root,
            "SolverIterationCounts.minVelocityIters",
            Value::U64(0),
        ),
        (root, "ExternalDriveIterations", Value::U64(u64::MAX)),
        (root, "WakeCounter", Value::F32(f32::NAN)),
        (
            root,
            "Links",
            Value::Array(vec![Value::Ref(u64::MAX)].into()),
        ),
        (link, "Mass", Value::F32(0.0)),
        (link, "RigidBodyFlags", Value::U64(1)),
        (link, "RigidBodyFlags", Value::U64(2)),
        (link, "RigidBodyFlags", Value::U64(36)),
        (link, "RigidBodyFlags", Value::U64(128)),
        (link, "Parent", Value::Ref(link.id)),
        (joint, "DriveType", Value::U64(1)),
        (
            joint,
            "TargetOrientation",
            Value::Quat([0.0, 0.0, 0.0, 1.0002]),
        ),
        (joint, "InternalCompliance", Value::F32(0.0)),
        (joint, "SwingLimit.yLimit", Value::F32(std::f32::consts::PI)),
        (joint, "TwistLimit.lower", Value::F32(std::f32::consts::PI)),
        (joint, "SwingLimitEnabled", Value::U64(1)),
        (joint, "Link", Value::Ref(u64::MAX)),
    ] {
        let mut invalid = state.clone();
        invalid
            .objects
            .get_mut(&object.id)
            .unwrap()
            .properties
            .insert(field.into(), value);
        assert!(
            repx::export(&invalid, Selection::Frame).is_err(),
            "accepted {field}"
        );
    }
    for object in [root, link, joint] {
        let mut invalid = state.clone();
        invalid.objects.get_mut(&object.id).unwrap().generation += 1;
        assert!(repx::export(&invalid, Selection::Frame).is_err());
    }
    for (object, field) in [
        (root, "WakeCounter"),
        (link, "Mass"),
        (joint, "TargetOrientation"),
    ] {
        let mut invalid = state.clone();
        invalid
            .objects
            .get_mut(&object.id)
            .unwrap()
            .properties
            .remove(field);
        assert!(repx::export(&invalid, Selection::Frame).is_err());
    }
    for actor_kind in ["PxRigidStatic", "PxRigidDynamic", "PxArticulationLink"] {
        let actor = state
            .objects
            .values()
            .find(|object| kind(object) == actor_kind)
            .unwrap();
        for value in [
            None,
            Some(Value::U64(16)),
            Some(Value::U64(u64::MAX)),
            Some(Value::I64(-1)),
            Some(Value::F32(1.0)),
            Some(Value::Bool(true)),
        ] {
            let mut invalid = state.clone();
            let properties = &mut invalid.objects.get_mut(&actor.id).unwrap().properties;
            match value {
                Some(value) => properties.insert("ClientBehaviorFlags".into(), value),
                None => properties.remove("ClientBehaviorFlags"),
            };
            let error = repx::export(&invalid, Selection::Actor(actor.id))
                .err()
                .expect("invalid client behavior must be rejected");
            assert!(error.to_string().contains("ClientBehaviorFlags"), "{error}");
        }
    }
}
