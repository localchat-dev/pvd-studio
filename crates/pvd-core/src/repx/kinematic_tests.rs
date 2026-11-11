use super::*;

fn fixture(flags: u64, target: Option<Value>) -> SceneState {
    let mut state = SceneState::default();
    for (id, kind) in [(1, "PxPhysics"), (2, "PxScene"), (3, "PxRigidDynamic")] {
        crate::Action::Create(Object {
            id,
            generation: 0,
            type_name: format!("physx3::{kind}"),
            properties: BTreeMap::new(),
        })
        .apply(&mut state);
    }
    for (field, value) in [
        ("Version.Major", Value::U64(3)),
        ("Version.Minor", Value::U64(4)),
        ("Version.Bugfix", Value::U64(2)),
        ("TolerancesScale.Length", Value::F32(1.0)),
        ("TolerancesScale.Mass", Value::F32(1.0)),
        ("TolerancesScale.Speed", Value::F32(10.0)),
    ] {
        crate::Action::Set(1, field.into(), value).apply(&mut state);
    }
    crate::Action::Set(3, "Scene".into(), Value::Ref(2)).apply(&mut state);
    for field in ["GlobalPose", "CMassLocalPose"] {
        crate::Action::Set(3, field.into(), transform()).apply(&mut state);
    }
    for (field, value) in [
        ("Name", Value::String("ordinary-body".into())),
        ("ConcreteTypeName", Value::String("PxRigidDynamic".into())),
        ("ActorFlags", Value::U64(0)),
        ("ClientBehaviorFlags", Value::U64(0)),
        ("DominanceGroup", Value::U64(0)),
        ("OwnerClient", Value::U64(0)),
        ("Shapes", Value::Array(Vec::new().into())),
        ("Joints", Value::Array(Vec::new().into())),
        ("RigidBodyFlags", Value::U64(flags)),
        ("RigidDynamicLockFlags", Value::U64(0)),
        ("SolverIterationCounts.minPositionIters", Value::U64(4)),
        ("SolverIterationCounts.minVelocityIters", Value::U64(1)),
        ("MassSpaceInertiaTensor", Value::Vec3([1.0; 3])),
        ("LinearVelocity", Value::Vec3([0.0; 3])),
        ("AngularVelocity", Value::Vec3([0.0; 3])),
    ] {
        crate::Action::Set(3, field.into(), value).apply(&mut state);
    }
    for field in [
        "Mass",
        "MinCCDAdvanceCoefficient",
        "MaxDepenetrationVelocity",
        "MaxContactImpulse",
        "LinearDamping",
        "AngularDamping",
        "MaxAngularVelocity",
        "SleepThreshold",
        "StabilizationThreshold",
        "WakeCounter",
        "ContactReportThreshold",
    ] {
        crate::Action::Set(3, field.into(), Value::F32(0.5)).apply(&mut state);
    }
    if let Some(target) = target {
        crate::Action::Set(3, "KinematicTarget".into(), target).apply(&mut state);
    }
    state
}

fn transform() -> Value {
    Value::Transform {
        position: [4.0, 5.0, 6.0],
        rotation: [0.0, 0.0, 1.0, 0.0],
    }
}

#[test]
fn non_kinematic_snapshots_keep_query_flag_and_omit_inactive_target() {
    for flags in [0, 2, 4, 6, 18] {
        for target in [None, Some(transform())] {
            let state = fixture(flags, target);
            for selection in [Selection::Frame, Selection::Actor(3)] {
                let result = export(&state, selection).unwrap();
                assert_eq!((result.actors, result.shapes, result.materials), (1, 0, 0));
                assert_eq!(result.kinematic_actors, 0);
                assert!(!result.xml.contains("KinematicTarget"));
                assert_eq!(
                    result
                        .xml
                        .contains("eUSE_KINEMATIC_TARGET_FOR_SCENE_QUERIES"),
                    flags & 2 != 0
                );
            }
        }
    }
}

#[test]
fn kinematic_snapshots_preserve_parameters_without_target_or_sleep_state() {
    for flags in [1, 3, 9, 17, 33, 65] {
        for target in [None, Some(transform())] {
            let mut state = fixture(flags, target);
            crate::Action::Set(3, "IsSleeping".into(), Value::Bool(false)).apply(&mut state);
            crate::Action::Set(3, "LinearVelocity".into(), Value::Vec3([1.0, 2.0, 3.0]))
                .apply(&mut state);
            crate::Action::Set(3, "AngularVelocity".into(), Value::Vec3([4.0, 5.0, 6.0]))
                .apply(&mut state);
            for selection in [Selection::Frame, Selection::Actor(3)] {
                let result = export(&state, selection).unwrap();
                assert_eq!(result.kinematic_actors, 1);
                assert!(result.xml.contains("<RigidBodyFlags>eKINEMATIC"));
                assert!(
                    result
                        .xml
                        .contains("<LinearVelocity>1 2 3</LinearVelocity>")
                );
                assert!(
                    result
                        .xml
                        .contains("<AngularVelocity>4 5 6</AngularVelocity>")
                );
                assert!(result.xml.contains("<WakeCounter>0.5</WakeCounter>"));
                assert!(!result.xml.contains("KinematicTarget"));
                assert!(!result.xml.contains("IsSleeping"));
            }
        }
    }
}

#[test]
fn kinematic_snapshots_reject_the_sdk_filtered_ccd_combination() {
    for flags in [5, 7, 37, 69] {
        let state = fixture(flags, Some(transform()));
        for selection in [Selection::Frame, Selection::Actor(3)] {
            let error = export(&state, selection).err().unwrap().to_string();
            assert!(
                error.contains("kinematic actors cannot enable CCD"),
                "{error}"
            );
        }
    }
}

#[test]
fn target_observations_do_not_bypass_type_field_flag_or_numeric_guards() {
    for target in [
        Value::Vec3([1.0, 2.0, 3.0]),
        Value::String("0 0 0 1 4 5 6".into()),
        Value::Array(vec![Value::F32(0.0); 100_000].into()),
        Value::Transform {
            position: [f32::INFINITY, 0.0, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
        },
        Value::Transform {
            position: [0.0; 3],
            rotation: [0.0; 4],
        },
        Value::Transform {
            position: [0.0; 3],
            rotation: [0.0, 0.0, f32::NAN, 1.0],
        },
    ] {
        for flags in [1, 2] {
            let state = fixture(flags, Some(target.clone()));
            let error = export(&state, Selection::Frame).err().unwrap().to_string();
            assert!(error.contains("KinematicTarget"), "{error}");
        }
    }
    for (field, replacement, expected) in [
        (
            "FutureTargetState",
            Value::Bool(false),
            "no validated serializer",
        ),
        ("RigidBodyFlags", Value::U64(130), "unknown flag bits"),
        (
            "RigidBodyFlags",
            Value::U64(38),
            "CCD and speculative CCD together",
        ),
        ("Mass", Value::F32(f32::INFINITY), "finite"),
    ] {
        let mut state = fixture(2, Some(transform()));
        crate::Action::Set(3, field.into(), replacement).apply(&mut state);
        let error = export(&state, Selection::Frame).err().unwrap().to_string();
        assert!(error.contains(expected), "{error}");
    }
}

fn plane_fixture(body_flags: u64, shape_flags: u64) -> SceneState {
    let mut state = fixture(body_flags, None);
    for (id, name) in [(4, "PxShape"), (5, "PxMaterial"), (6, "PxPlaneGeometry")] {
        crate::Action::Create(Object {
            id,
            generation: 0,
            type_name: format!("physx3::{name}"),
            properties: BTreeMap::new(),
        })
        .apply(&mut state);
    }
    for (field, value) in [
        ("Actor", Value::Ref(3)),
        ("IsExclusive", Value::Bool(true)),
        ("Geometry", Value::Ref(6)),
        ("GeometryType", Value::U64(1)),
        ("LocalPose", transform()),
        ("Materials", Value::Array(vec![Value::Ref(5)].into())),
        ("Name", Value::String("plane".into())),
        ("Flags", Value::U64(shape_flags)),
        ("ContactOffset", Value::F32(0.02)),
        ("RestOffset", Value::F32(0.0)),
        (
            "SimulationFilterData",
            Value::Array(vec![Value::U64(0); 4].into()),
        ),
        (
            "QueryFilterData",
            Value::Array(vec![Value::U64(0); 4].into()),
        ),
    ] {
        crate::Action::Set(4, field.into(), value).apply(&mut state);
    }
    for (field, value) in [
        ("StaticFriction", Value::F32(0.4)),
        ("DynamicFriction", Value::F32(0.3)),
        ("Restitution", Value::F32(0.2)),
        ("Flags", Value::U64(0)),
        ("FrictionCombineMode", Value::U64(0)),
        ("RestitutionCombineMode", Value::U64(0)),
    ] {
        crate::Action::Set(5, field.into(), value).apply(&mut state);
    }
    crate::Action::Set(6, "Shape".into(), Value::Ref(4)).apply(&mut state);
    crate::Action::Set(3, "Shapes".into(), Value::Array(vec![Value::Ref(4)].into()))
        .apply(&mut state);
    state
}

#[test]
fn plane_snapshots_follow_simulation_and_kinematic_attachment_rules() {
    for body_flags in [0, 2, 1, 3] {
        for shape_flags in 0..=15 {
            let state = plane_fixture(body_flags, shape_flags);
            for selection in [Selection::Frame, Selection::Actor(3)] {
                let result = export(&state, selection);
                let valid = shape_flags & 5 != 5 && (shape_flags & 1 == 0 || body_flags & 1 != 0);
                assert_eq!(
                    result.is_ok(),
                    valid,
                    "body={body_flags} shape={shape_flags}"
                );
                if let Ok(exported) = result {
                    assert_eq!(exported.kinematic_actors, usize::from(body_flags & 1 != 0));
                    assert!(exported.xml.contains("<PxPlaneGeometry>"));
                    assert_eq!(
                        exported.xml.contains("eTRIGGER_SHAPE"),
                        shape_flags & 4 != 0
                    );
                }
            }
        }
    }
}

#[test]
fn shared_plane_checks_every_selected_owner_and_requires_typed_body_flags() {
    let mut state = plane_fixture(3, 11);
    crate::Action::Set(4, "IsExclusive".into(), Value::Bool(false)).apply(&mut state);
    crate::Action::Set(
        1,
        "SharedShapes".into(),
        Value::Array(vec![Value::Ref(4)].into()),
    )
    .apply(&mut state);
    let mut other = state.objects[&3].clone();
    other.id = 7;
    let properties = std::mem::take(&mut other.properties);
    crate::Action::Create(other).apply(&mut state);
    for (name, value) in properties {
        crate::Action::Set(7, name, value).apply(&mut state);
    }
    export(&state, Selection::Frame).unwrap();
    crate::Action::Set(7, "RigidBodyFlags".into(), Value::U64(2)).apply(&mut state);
    let error = export(&state, Selection::Frame).err().unwrap().to_string();
    assert!(
        error.contains("non-kinematic dynamic actors cannot simulate plane"),
        "{error}"
    );
    assert!(export(&state, Selection::Actor(7)).is_err());
    assert!(export(&state, Selection::Actor(3)).is_ok());
    for bad in [Value::Bool(true), Value::U64(129)] {
        let mut invalid = state.clone();
        crate::Action::Set(7, "RigidBodyFlags".into(), bad).apply(&mut invalid);
        assert!(export(&invalid, Selection::Frame).is_err());
    }
    state.objects.get_mut(&7).unwrap().type_name = "physx3::PxArticulationLink".into();
    for flags in [None, Some(Value::U64(1))] {
        match flags {
            Some(value) => {
                state
                    .objects
                    .get_mut(&7)
                    .unwrap()
                    .properties
                    .insert("RigidBodyFlags".into(), value);
            }
            None => {
                state
                    .objects
                    .get_mut(&7)
                    .unwrap()
                    .properties
                    .remove("RigidBodyFlags");
            }
        }
        let owners = [&state.objects[&3], &state.objects[&7]];
        let plan = ShapePlan::new(&state, &state.objects[&1], &owners).unwrap();
        assert!(plan.shapes[&4].non_kinematic_dynamic);
        assert!(
            validate_geometry(
                &state.objects[&6],
                &state.objects[&4],
                plan.shapes[&4].non_kinematic_dynamic
            )
            .is_err()
        );
    }
}

#[test]
fn mesh_and_height_field_trigger_snapshots_are_rejected_before_resource_writes() {
    for (geometry, kind_id) in [("PxTriangleMeshGeometry", 5), ("PxHeightFieldGeometry", 6)] {
        for flags in [4, 6, 12, 14] {
            let mut state = plane_fixture(3, flags);
            state.objects.get_mut(&6).unwrap().type_name = format!("physx3::{geometry}");
            crate::Action::Set(4, "GeometryType".into(), Value::U64(kind_id)).apply(&mut state);
            for selection in [Selection::Frame, Selection::Actor(3)] {
                let error = export(&state, selection).err().unwrap().to_string();
                assert!(
                    error.contains("triangle mesh and height field triggers"),
                    "{error}"
                );
            }
        }
    }
}

#[test]
fn zero_flags_write_explicit_values_for_sdk_constructor_defaults() {
    for flags in [0, 3] {
        let state = plane_fixture(flags, 0);
        for selection in [Selection::Frame, Selection::Actor(3)] {
            let exported = export(&state, selection).unwrap();
            assert!(exported.xml.contains("<ActorFlags>0</ActorFlags>"));
            assert!(exported.xml.contains("<Flags>0</Flags>"));
            assert!(
                exported
                    .xml
                    .contains("<ClientBehaviorFlags>0</ClientBehaviorFlags>")
            );
            assert!(!exported.xml.contains("<Flags></Flags>"));
        }
    }
}
