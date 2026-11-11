use super::*;
use crate::Action;

fn set(state: &mut SceneState, id: u64, field: &str, value: Value) {
    Action::Set(id, field.into(), value).apply(state);
}

fn fixture() -> SceneState {
    let mut state = SceneState::default();
    for (id, name) in [
        (1, "PxArticulation"),
        (2, "PxArticulationLink"),
        (3, "PxArticulationLink"),
        (4, "PxArticulationLink"),
        (5, "PxArticulationJoint"),
        (6, "PxArticulationJoint"),
        (10, "PxScene"),
    ] {
        Action::Create(Object {
            id,
            generation: 0,
            type_name: format!("physx3::{name}"),
            properties: BTreeMap::new(),
        })
        .apply(&mut state);
    }
    for (id, field, target) in [
        (1, "Scene", 10),
        (2, "Parent", 1),
        (3, "Parent", 2),
        (4, "Parent", 2),
        (3, "InboundJoint", 5),
        (4, "InboundJoint", 6),
        (5, "Link", 3),
        (6, "Link", 4),
    ] {
        set(&mut state, id, field, Value::Ref(target));
    }
    for (id, field, targets) in [
        (1, "Links", vec![2]),
        (2, "Links", vec![3, 4]),
        (10, "Articulations", vec![1]),
    ] {
        set(
            &mut state,
            id,
            field,
            Value::Array(
                targets
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
fn articulation_selection_from_any_link_or_root_preserves_complete_tree() {
    let state = fixture();
    for id in [1, 2, 3, 4] {
        let plan = selection::Plan::new(&state, Selection::Actor(id)).unwrap();
        assert_eq!(
            plan.actors
                .iter()
                .map(|object| object.id)
                .collect::<Vec<_>>(),
            [2, 3, 4]
        );
        assert_eq!(plan.articulations.len(), 1);
        assert!(plan.joints.is_empty());
    }
}

#[test]
fn articulation_cycles_missing_objects_and_stale_generations_are_rejected() {
    let state = fixture();
    for id in [1, 2, 3, 4, 5, 6, 10] {
        let mut invalid = state.clone();
        invalid.objects.get_mut(&id).unwrap().generation += 1;
        assert!(selection::Plan::new(&invalid, Selection::Actor(3)).is_err());
        let mut invalid = state.clone();
        invalid.objects.remove(&id);
        assert!(selection::Plan::new(&invalid, Selection::Frame).is_err());
    }
    for (id, field, replacement) in [
        (1, "Links", Value::Ref(2)),
        (
            2,
            "Links",
            Value::Array(vec![Value::Ref(3), Value::Ref(3)].into()),
        ),
        (2, "Parent", Value::Ref(3)),
        (3, "Parent", Value::Ref(4)),
        (2, "InboundJoint", Value::Ref(5)),
        (5, "Link", Value::Ref(4)),
    ] {
        let mut invalid = state.clone();
        set(&mut invalid, id, field, replacement);
        assert!(selection::Plan::new(&invalid, Selection::Actor(3)).is_err());
    }
}

fn drive_joint(drive: Value, target: Value) -> Object {
    let mut joint = Object {
        id: 5,
        generation: 0,
        type_name: "physx3::PxArticulationJoint".into(),
        properties: BTreeMap::new(),
    };
    for field in ["ParentPose", "ChildPose"] {
        joint.properties.insert(
            field.into(),
            Value::Transform {
                position: [0.0; 3],
                rotation: [0.0, 0.0, 0.0, 1.0],
            },
        );
    }
    for field in [
        "Stiffness",
        "Damping",
        "TangentialStiffness",
        "TangentialDamping",
        "SwingLimitContactDistance",
        "TwistLimitContactDistance",
    ] {
        joint.properties.insert(field.into(), Value::F32(0.0));
    }
    for field in [
        "InternalCompliance",
        "ExternalCompliance",
        "SwingLimit.zLimit",
        "SwingLimit.yLimit",
    ] {
        joint.properties.insert(field.into(), Value::F32(0.5));
    }
    for (field, value) in [
        (
            "ConcreteTypeName",
            Value::String("PxArticulationJoint".into()),
        ),
        ("Link", Value::Ref(3)),
        ("TargetOrientation", target),
        ("TargetVelocity", Value::Vec3([0.0; 3])),
        ("DriveType", drive),
        ("SwingLimitEnabled", Value::Bool(false)),
        ("TwistLimitEnabled", Value::Bool(false)),
        ("TwistLimit.lower", Value::F32(-0.5)),
        ("TwistLimit.upper", Value::F32(0.5)),
    ] {
        joint.properties.insert(field.into(), value);
    }
    joint
}

#[test]
fn error_drive_writes_original_signed_target_without_normalizing() {
    for target in [
        [1.0, 0.0, 0.0, 0.0],
        [0.36, 0.48, 0.8, 0.0],
        [1.00005, 0.0, 0.0, 0.0],
        [0.99995, 0.0, 0.0, 0.0],
        [-0.0, 0.0, -1.0, -0.0],
        [1.0000899, 0.0, 0.0, 0.0],
        [0.9999101, 0.0, 0.0, 0.0],
    ] {
        let joint = drive_joint(Value::U64(1), Value::Quat(target));
        let mut xml = String::new();
        write_joint(&mut xml, &joint).unwrap();
        assert!(xml.contains("<DriveType>eERROR</DriveType>"));
        let text = xml
            .split_once("<TargetOrientation>")
            .unwrap()
            .1
            .split_once("</TargetOrientation>")
            .unwrap()
            .0;
        let actual: Vec<_> = text
            .split_whitespace()
            .map(|part| part.parse::<f32>().unwrap().to_bits())
            .collect();
        assert_eq!(actual, target.map(f32::to_bits));
    }
}

#[test]
fn error_drive_rejects_unrestorable_vectors_and_margin_boundaries() {
    for target in [
        [0.0; 4],
        [0.5, 0.0, 0.0, 0.0],
        [2.0, 0.0, 0.0, 0.0],
        [1.0000901, 0.0, 0.0, 0.0],
        [0.9999099, 0.0, 0.0, 0.0],
        [1.0000998, 0.0, 0.0, 0.0],
        [1.0001002, 0.0, 0.0, 0.0],
        [0.9999002, 0.0, 0.0, 0.0],
        [0.9998998, 0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0, f32::from_bits(1)],
        [1.0, 0.0, 0.0, -f32::from_bits(1)],
        [1.0, 0.0, 0.0, 0.01],
        [f32::NAN, 0.0, 0.0, 0.0],
        [f32::INFINITY, 0.0, 0.0, 0.0],
    ] {
        let joint = drive_joint(Value::U64(1), Value::Quat(target));
        let error = write_joint(&mut String::new(), &joint)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("eERROR target"), "{error}");
    }
    for drive in [
        Value::U64(2),
        Value::U64(256),
        Value::I64(-1),
        Value::Bool(true),
        Value::F32(1.0),
    ] {
        assert!(
            write_joint(
                &mut String::new(),
                &drive_joint(drive, Value::Quat([1.0, 0.0, 0.0, 0.0]))
            )
            .is_err()
        );
    }
    assert!(
        write_joint(
            &mut String::new(),
            &drive_joint(Value::U64(1), Value::Vec3([1.0, 0.0, 0.0]))
        )
        .is_err()
    );
    let target = drive_joint(Value::U64(0), Value::Quat([1.000095, 0.0, 0.0, 0.0]));
    assert!(write_joint(&mut String::new(), &target).is_ok());
}
