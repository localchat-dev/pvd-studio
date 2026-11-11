use pvd_core::{
    Object, Recording, SceneState, Value,
    repx::{self, Selection},
};
use std::{collections::BTreeMap, path::PathBuf};

fn path(variable: &str, default: &str) -> PathBuf {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(default)
        })
}

fn spherical(state: &SceneState) -> &Object {
    state
        .objects
        .values()
        .find(|object| object.type_name.ends_with("::PxSphericalJoint"))
        .unwrap()
}

fn assert_joint_failures(state: &SceneState) {
    let joint = spherical(state);
    let joint_id = joint.id;
    let actor = ["Actors.actor0", "Actors.actor1"]
        .iter()
        .find_map(|name| match joint.properties[*name] {
            Value::Ref(id) if id != 0 => Some(id),
            _ => None,
        })
        .unwrap();
    let corruptions = [
        ("Actors.actor0", Value::Ref(u64::MAX)),
        ("Actors.actor1", Value::U64(actor)),
        ("Parent", Value::Ref(0)),
        (
            "LocalPose[eACTOR0]",
            Value::Transform {
                position: [f32::INFINITY, 0.0, 0.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
            },
        ),
        (
            "LocalPose[eACTOR1]",
            Value::Transform {
                position: [0.0; 3],
                rotation: [0.0; 4],
            },
        ),
        ("LimitCone.YAngle", Value::F32(0.0)),
        ("LimitCone.ZAngle", Value::F32(std::f32::consts::PI)),
        ("LimitCone.Restitution", Value::F32(1.01)),
        ("LimitCone.ContactDistance", Value::F32(-0.01)),
        ("LimitCone.Stiffness", Value::F32(f32::NAN)),
        ("LimitCone.Damping", Value::String("0".into())),
        ("LimitCone.BounceThreshold", Value::F32(-1.0)),
        ("BreakForce.force", Value::F32(-1.0)),
        ("BreakForce.torque", Value::F32(f32::INFINITY)),
        ("InvMassScale0", Value::F32(-1.0)),
        ("InvInertiaScale1", Value::F32(f32::NAN)),
        ("ProjectionLinearTolerance", Value::F32(-1.0)),
        ("ConstraintFlags", Value::U64(1)),
        ("ConstraintFlags", Value::U64(1 << 6)),
        ("ConstraintFlags", Value::U64(1 << 9)),
        ("SphericalJointFlags", Value::U64(1)),
        ("Name", Value::U64(0)),
        ("UnknownProperty", Value::F32(0.0)),
    ];
    for (field, replacement) in corruptions {
        let mut invalid = state.clone();
        invalid
            .objects
            .get_mut(&joint_id)
            .unwrap()
            .properties
            .insert(field.into(), replacement);
        assert!(
            repx::export(&invalid, Selection::Frame).is_err(),
            "accepted corrupt {field}"
        );
        assert!(
            repx::export(&invalid, Selection::Actor(actor)).is_err(),
            "selected closure accepted corrupt {field}"
        );
    }
    for id in [actor, joint_id] {
        let mut stale = state.clone();
        stale.objects.get_mut(&id).unwrap().generation += 1;
        assert!(
            repx::export(&stale, Selection::Frame).is_err(),
            "accepted stale reference to {id}"
        );
        assert!(repx::export(&stale, Selection::Actor(actor)).is_err());
        let mut missing = state.clone();
        missing.objects.remove(&id);
        assert!(
            repx::export(&missing, Selection::Frame).is_err(),
            "accepted missing dependency {id}"
        );
        assert!(repx::export(&missing, Selection::Actor(actor)).is_err());
    }
    let mut orphan = state.clone();
    orphan
        .objects
        .get_mut(&actor)
        .unwrap()
        .properties
        .remove("Joints");
    assert!(repx::export(&orphan, Selection::Frame).is_err());
    assert!(repx::export(&orphan, Selection::Actor(actor)).is_err());
    let mut missing_limit = state.clone();
    missing_limit
        .objects
        .get_mut(&joint_id)
        .unwrap()
        .properties
        .remove("LimitCone.Damping");
    assert!(repx::export(&missing_limit, Selection::Frame).is_err());
    let mut unsupported = state.clone();
    unsupported.objects.get_mut(&joint_id).unwrap().type_name = "physx3::PxCustomJoint".into();
    assert!(repx::export(&unsupported, Selection::Frame).is_err());
    unsupported.objects.insert(
        u64::MAX,
        Object {
            id: u64::MAX,
            generation: 0,
            type_name: "physx3::PxConstraint".into(),
            properties: BTreeMap::new(),
        },
    );
    unsupported.objects.get_mut(&joint_id).unwrap().type_name = "physx3::PxSphericalJoint".into();
    assert!(repx::export(&unsupported, Selection::Frame).is_err());
    let selected = repx::export(state, Selection::Actor(actor)).unwrap();
    assert!(selected.joints > 0);
    assert!(selected.xml.contains(&format!("<Id>{joint_id}</Id>")));
}

#[test]
#[ignore = "requires official PhysX 3.4 tools/repx-joints capture"]
fn spherical_dependencies_limits_and_lifetimes_are_preserved_or_rejected() {
    let recording = Recording::from_path(path(
        "PVD_REPX_JOINT_CAPTURE",
        "captures/physx-3.4-repx-joints.pvdstream",
    ))
    .unwrap();
    assert_eq!(recording.frame_count(), 3);
    let mut counts = Vec::new();
    for frame in 0..recording.frame_count() {
        let state = recording.state_at(frame).unwrap();
        let exported = repx::export(&state, Selection::Frame).unwrap();
        let expected = state
            .objects
            .values()
            .filter(|object| object.type_name.ends_with("::PxSphericalJoint"))
            .count();
        assert!(expected > 0);
        assert_eq!(exported.joints, expected);
        assert_eq!(exported.xml.matches("<PxSphericalJoint>").count(), expected);
        assert!(!exported.xml.contains("<PxConstraint>"));
        assert!(
            exported.xml.find("<PxSphericalJoint>").unwrap()
                > exported.xml.rfind("</PxRigidDynamic>").unwrap()
        );
        counts.push(expected);
        assert_joint_failures(&state);
    }
    assert_eq!(counts, [3, 3, 2]);
}

#[test]
#[ignore = "requires official PhysX 3.4 rich capture"]
fn rich_whole_frame_retains_spherical_joint_and_unrelated_actor_can_export() {
    let recording = Recording::from_path(path(
        "PVD_SDK34_RICH_CAPTURE",
        "captures/physx-3.4-rich.pvdstream",
    ))
    .unwrap();
    for frame in [0, recording.frame_count() / 2, recording.frame_count() - 1] {
        let state = recording.state_at(frame).unwrap();
        let exported = repx::export(&state, Selection::Frame).unwrap();
        assert_eq!(exported.joints, 1);
        assert_eq!(exported.xml.matches("<PxSphericalJoint>").count(), 1);
        assert!(
            exported
                .xml
                .contains("<Name>PvdStudioSphericalJoint</Name>")
        );
        let actor = state.objects.values().find(|object| matches!(object.properties.get("Name"), Some(Value::String(name)) if name.starts_with("PvdStudioBox"))).unwrap();
        let selected = repx::export(&state, Selection::Actor(actor.id)).unwrap();
        assert_eq!(selected.actors, 1);
        assert_eq!(selected.joints, 0);
        assert_joint_failures(&state);
    }
}

fn expanded_recording() -> Recording {
    Recording::from_path(path(
        "PVD_REPX_EXPANDED_JOINT_CAPTURE",
        "captures/physx-3.4-repx-joints-expanded.pvdstream",
    ))
    .unwrap()
}

#[test]
#[ignore = "requires official tools/repx-joints --expanded capture"]
fn all_sdk_extension_joint_snapshots_preserve_types_changes_and_destruction() {
    let recording = expanded_recording();
    assert_eq!(recording.frame_count(), 3);
    let mut previous = BTreeMap::new();
    for (frame, count) in [8, 8, 2].into_iter().enumerate() {
        let state = recording.state_at(frame).unwrap();
        let exported = repx::export(&state, Selection::Frame).unwrap();
        assert_eq!(exported.joints, count);
        for name in [
            "PxFixedJoint",
            "PxDistanceJoint",
            "PxRevoluteJoint",
            "PxPrismaticJoint",
            "PxD6Joint",
        ] {
            assert_eq!(
                exported.xml.matches(&format!("<{name}>")).count(),
                usize::from(frame < 2)
            );
            if frame == 2 {
                continue;
            }
            let joint = state
                .objects
                .values()
                .find(|object| object.type_name == format!("physx3::{name}"))
                .unwrap();
            let local_pose = joint.properties["LocalPose[eACTOR1]"].clone();
            if let Some((id, before)) = previous.insert(name, (joint.id, local_pose.clone())) {
                assert_eq!(id, joint.id);
                assert_ne!(before, local_pose);
            }
            assert!(
                exported.xml.find(&format!("<{name}>")).unwrap()
                    > exported.xml.rfind("</PxRigidDynamic>").unwrap()
            );
            let actor = ["Actors.actor0", "Actors.actor1"]
                .iter()
                .find_map(|field| match joint.properties[*field] {
                    Value::Ref(id) if id != 0 => Some(id),
                    _ => None,
                })
                .unwrap();
            let selected = repx::export(&state, Selection::Actor(actor)).unwrap();
            assert!(selected.xml.contains(&format!("<Id>{}</Id>", joint.id)));
            assert!(selected.joints > 0);
        }
        assert!(!exported.xml.contains("<PxConstraint>"));
        if frame < 2 {
            assert!(exported.xml.contains("<Motion>") && exported.xml.contains("<Drive>"));
            assert!(exported.xml.contains("<eSLERP>") && exported.xml.contains("<eSWING2>"));
        }
    }
}

#[test]
#[ignore = "requires official tools/repx-joints --expanded capture"]
fn extension_joint_invalid_values_missing_fields_and_stale_dependencies_are_rejected() {
    let state = expanded_recording().state_at(0).unwrap();
    let types = [
        "PxFixedJoint",
        "PxDistanceJoint",
        "PxRevoluteJoint",
        "PxPrismaticJoint",
        "PxD6Joint",
    ];
    for name in types {
        let joint = state
            .objects
            .values()
            .find(|object| object.type_name == format!("physx3::{name}"))
            .unwrap();
        let actor = ["Actors.actor0", "Actors.actor1"]
            .iter()
            .find_map(|field| match joint.properties[*field] {
                Value::Ref(id) if id != 0 => Some(id),
                _ => None,
            })
            .unwrap();
        for id in [actor, joint.id] {
            let mut stale = state.clone();
            stale.objects.get_mut(&id).unwrap().generation += 1;
            assert!(
                repx::export(&stale, Selection::Frame).is_err(),
                "accepted stale {name} dependency"
            );
        }
        for field in joint.properties.keys() {
            let mut missing = state.clone();
            missing
                .objects
                .get_mut(&joint.id)
                .unwrap()
                .properties
                .remove(field);
            assert!(
                repx::export(&missing, Selection::Frame).is_err(),
                "accepted missing {name}.{field}"
            );
        }
        let mut corruptions = vec![
            ("BreakForce.force", Value::F32(f32::NAN)),
            ("InvMassScale0", Value::F32(-1.0)),
            ("ConstraintFlags", Value::U64(1)),
            ("ConstraintFlags", Value::U64(64)),
            ("Actors.actor1", Value::Ref(u64::MAX)),
            ("Name", Value::String("bad\0name".into())),
            ("UnknownField", Value::U64(1)),
            ("ConcreteTypeName", Value::String("PxCustomJoint".into())),
        ];
        if name != "PxDistanceJoint" {
            corruptions.extend([
                ("ProjectionLinearTolerance", Value::F32(-0.01)),
                ("ProjectionAngularTolerance", Value::F32(3.2)),
            ]);
        }
        match name {
            "PxDistanceJoint" => corruptions.extend([
                ("MinDistance", Value::F32(-0.1)),
                ("MinDistance", Value::F32(5.0)),
                ("MaxDistance", Value::F32(f32::INFINITY)),
                ("Tolerance", Value::F32(-0.01)),
                ("Stiffness", Value::F32(-1.0)),
                ("Damping", Value::U64(1)),
                ("DistanceJointFlags", Value::U64(1)),
            ]),
            "PxPrismaticJoint" | "PxRevoluteJoint" => {
                corruptions.extend([
                    ("Limit.Lower", Value::F32(4.0)),
                    ("Limit.ContactDistance", Value::F32(10.0)),
                    ("Limit.Restitution", Value::F32(1.1)),
                    ("Limit.Damping", Value::F32(-1.0)),
                    ("Velocity", Value::F32(f32::NAN)),
                ]);
                if name == "PxRevoluteJoint" {
                    corruptions.extend([
                        ("Limit.Lower", Value::F32(-std::f32::consts::PI)),
                        ("Limit.Upper", Value::F32(std::f32::consts::PI)),
                        ("DriveVelocity", Value::F32(f32::INFINITY)),
                        ("DriveForceLimit", Value::F32(-1.0)),
                        ("DriveGearRatio", Value::F32(0.0)),
                        ("RevoluteJointFlags", Value::U64(8)),
                    ]);
                } else {
                    corruptions.extend([
                        ("Limit.Upper", Value::F32(-3.0)),
                        ("PrismaticJointFlags", Value::U64(1)),
                    ]);
                }
            }
            "PxD6Joint" => corruptions.extend([
                ("Motion[eX]", Value::U64(3)),
                ("Motion[eSWING2]", Value::F32(1.0)),
                ("Drive[eSLERP].Stiffness", Value::F32(-1.0)),
                ("Drive[eX].ForceLimit", Value::F32(-1.0)),
                ("Drive[eSWING].Flags", Value::U64(2)),
                ("LinearLimit.Value", Value::F32(0.0)),
                ("TwistLimit.Upper", Value::F32(std::f32::consts::TAU)),
                ("TwistLimit.Lower", Value::F32(-6.0)),
                ("SwingLimit.YAngle", Value::F32(0.0)),
                (
                    "DrivePosition",
                    Value::Transform {
                        position: [0.0; 3],
                        rotation: [0.0; 4],
                    },
                ),
                (
                    "DriveVelocity.linear",
                    Value::Vec3([f32::INFINITY, 0.0, 0.0]),
                ),
                ("DriveVelocity.angular", Value::U64(1)),
            ]),
            _ => {}
        }
        for (field, replacement) in corruptions {
            let mut invalid = state.clone();
            invalid
                .objects
                .get_mut(&joint.id)
                .unwrap()
                .properties
                .insert(field.into(), replacement);
            assert!(
                repx::export(&invalid, Selection::Frame).is_err(),
                "accepted corrupt {name}.{field}"
            );
        }
        if name == "PxPrismaticJoint" {
            let mut overflow = state.clone();
            let properties = &mut overflow.objects.get_mut(&joint.id).unwrap().properties;
            properties.insert("Limit.Lower".into(), Value::F32(-f32::MAX));
            properties.insert("Limit.Upper".into(), Value::F32(f32::MAX));
            assert!(repx::export(&overflow, Selection::Frame).is_err());
        }
    }
}
