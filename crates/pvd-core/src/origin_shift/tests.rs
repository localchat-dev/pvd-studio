use super::*;
use pvd_protocol::{Endian, Header};

fn builder() -> RecordingBuilder {
    let mut builder = RecordingBuilder::new(Header {
        version: 1,
        timestamp_numerator: 1,
        timestamp_denominator: 1,
        flags: 1,
        endian: Endian::Little,
    });
    for (id, kind) in [
        (1, "PxScene"),
        (2, "PxRigidStatic"),
        (3, "PxRigidDynamic"),
        (4, "PxScene"),
        (5, "PxCloth"),
        (6, "PxParticleFluid"),
        (7, "PxArticulation"),
        (8, "PxArticulationLink"),
        (9, "PxParticleSystem"),
        (10, "PxArticulationLink"),
    ] {
        builder
            .action(Action::Create(Object {
                id,
                generation: 0,
                type_name: format!("physx3::{kind}"),
                properties: Default::default(),
            }))
            .unwrap();
    }
    for (owner, property, target) in [
        (1, "RigidStatics", 2),
        (4, "RigidDynamics", 3),
        (1, "Cloths", 5),
        (1, "ParticleFluids", 6),
        (1, "Articulations", 7),
        (7, "Links", 8),
        (8, "Links", 10),
        (1, "ParticleSystems", 9),
    ] {
        builder
            .action(Action::Push(owner, property.into(), target))
            .unwrap();
    }
    for id in [2, 3, 5, 8, 10] {
        for key in ["GlobalPose", "LocalPose"] {
            builder
                .set_value(
                    id,
                    key,
                    Value::Transform {
                        position: [0.125, -0.0, 8.25],
                        rotation: [0.0, 0.5, 0.0, 0.5],
                    },
                )
                .unwrap();
        }
    }
    for id in [6, 9] {
        builder
            .set_value(
                id,
                "Positions",
                Value::Array(Arc::new(vec![Value::Vec3([1.25, 2.5, -3.75])])),
            )
            .unwrap();
        builder
            .set_value(
                id,
                "Velocities",
                Value::Array(Arc::new(vec![Value::Vec3([0.0, 1.0, 0.0])])),
            )
            .unwrap();
    }
    builder
}

#[test]
fn shift_changes_only_current_scene_world_positions() {
    let mut builder = builder();
    let before = builder.state.clone();
    builder
        .shift_origin(1, [10000.25, -8.5, 4.25], &mut || true)
        .unwrap();
    for id in [2, 5, 8, 10] {
        let Value::Transform { position, rotation } =
            builder.state.objects[&id].properties["GlobalPose"]
        else {
            panic!()
        };
        assert_eq!(position, [-10000.125, 8.5, 4.0]);
        assert_eq!(rotation, [0.0, 0.5, 0.0, 0.5]);
        assert_eq!(
            builder.state.objects[&id].properties["LocalPose"],
            before.objects[&id].properties["LocalPose"]
        );
    }
    assert_eq!(builder.state.objects[&3], before.objects[&3]);
    for id in [6, 9] {
        assert_eq!(
            builder.state.objects[&id].properties["Positions"],
            Value::Array(Arc::new(vec![Value::Vec3([-9999.0, 11.0, -8.0])]))
        );
        assert_eq!(
            builder.state.objects[&id].properties["Velocities"],
            before.objects[&id].properties["Velocities"]
        );
    }
}

#[test]
fn stale_scene_membership_does_not_shift_a_reused_id() {
    let mut builder = builder();
    builder.action(Action::Destroy(2)).unwrap();
    builder
        .action(Action::Create(Object {
            id: 2,
            generation: 1,
            type_name: "physx3::PxRigidStatic".into(),
            properties: Default::default(),
        }))
        .unwrap();
    builder
        .set_value(
            2,
            "GlobalPose",
            Value::Transform {
                position: [1.0; 3],
                rotation: [0.0, 0.0, 0.0, 1.0],
            },
        )
        .unwrap();
    let expected = builder.state.objects[&2].clone();
    builder.shift_origin(1, [2.0; 3], &mut || true).unwrap();
    assert_eq!(builder.state.objects[&2], expected);
    builder
        .action(Action::Push(1, "RigidStatics".into(), 2))
        .unwrap();
    builder.shift_origin(1, [2.0; 3], &mut || true).unwrap();
    assert!(matches!(
        builder.state.objects[&2].properties["GlobalPose"],
        Value::Transform {
            position: [-1.0, -1.0, -1.0],
            ..
        }
    ));
}

#[test]
fn malformed_layout_budget_and_cancellation_leave_the_scene_unmodified() {
    for mode in 0..4 {
        let mut builder = builder();
        if mode == 0 {
            builder
                .set_value(9, "Positions", Value::Array(Arc::new(vec![Value::U64(1)])))
                .unwrap();
        }
        if mode == 1 {
            builder.memory_limit = builder.retained_bytes + 1024;
        }
        let before = builder.state.clone();
        let actions = builder.recording.actions.len();
        let mut calls = 0;
        let shift = if mode == 3 {
            [f32::NAN, 0.0, 0.0]
        } else {
            [1.0; 3]
        };
        let result = builder.shift_origin(1, shift, &mut || {
            calls += 1;
            mode != 2 || calls < 4
        });
        assert!(result.is_err());
        assert_eq!(builder.state, before);
        assert_eq!(builder.recording.actions.len(), actions);
    }
}

fn scalar_array(values: &[f32]) -> Value {
    Value::Array(
        values
            .iter()
            .copied()
            .map(Value::F32)
            .collect::<Vec<_>>()
            .into(),
    )
}

fn captured_world_fields() -> RecordingBuilder {
    let mut builder = builder();
    builder
        .action(Action::Create(Object {
            id: 11,
            generation: 0,
            type_name: "physx3::PxRigidDynamic".into(),
            properties: Default::default(),
        }))
        .unwrap();
    builder
        .action(Action::Push(1, "RigidDynamics".into(), 11))
        .unwrap();
    for id in [2, 3, 5, 6, 8, 9, 10, 11] {
        builder
            .set_value(
                id,
                "WorldBounds",
                scalar_array(&[-1.0, -2.0, -3.0, 4.0, 5.0, 6.0]),
            )
            .unwrap();
    }
    builder
        .set_value(
            11,
            "KinematicTarget",
            Value::Transform {
                position: [6.25, -2.5, 3.75],
                rotation: [-0.0, 0.5, 0.0, 0.5],
            },
        )
        .unwrap();
    for id in [6, 9] {
        builder
            .set_value(
                id,
                "ProjectionPlane",
                scalar_array(&[0.5, -0.25, 0.75, 3.25]),
            )
            .unwrap();
    }
    builder
}

#[test]
fn shift_updates_captured_bounds_targets_and_planes_without_requiring_a_pose() {
    let mut builder = captured_world_fields();
    let before = builder.state.clone();
    assert!(!before.objects[&11].properties.contains_key("GlobalPose"));
    builder
        .shift_origin(1, [2.0, -4.0, 8.0], &mut || true)
        .unwrap();
    for id in [2, 5, 6, 8, 9, 10, 11] {
        assert_eq!(
            builder.state.objects[&id].properties["WorldBounds"],
            scalar_array(&[-3.0, 2.0, -11.0, 2.0, 9.0, -2.0])
        );
    }
    for id in [6, 9] {
        assert_eq!(
            builder.state.objects[&id].properties["ProjectionPlane"],
            scalar_array(&[0.5, -0.25, 0.75, 11.25])
        );
    }
    let Value::Transform { position, rotation } =
        builder.state.objects[&11].properties["KinematicTarget"]
    else {
        panic!("target");
    };
    assert_eq!(position, [4.25, 1.5, -4.25]);
    assert_eq!(
        rotation.map(f32::to_bits),
        [-0.0f32, 0.5, 0.0, 0.5].map(f32::to_bits)
    );
    assert_eq!(builder.state.objects[&3], before.objects[&3]);
}

#[test]
fn world_fields_are_shifted_after_decoding_either_stream_endianness() {
    for endian in [Endian::Little, Endian::Big] {
        let header = Header {
            version: 1,
            timestamp_numerator: 1,
            timestamp_denominator: 1,
            flags: 1,
            endian,
        };
        let mut builder = captured_world_fields();
        for (id, property, datatype, scalars) in [
            (
                2,
                "WorldBounds",
                "PxBounds3",
                &[-1.0, -2.0, -3.0, 4.0, 5.0, 6.0][..],
            ),
            (
                11,
                "KinematicTarget",
                "PxTransform",
                &[-0.0, 0.5, 0.0, 0.5, 6.25, -2.5, 3.75][..],
            ),
            (
                6,
                "ProjectionPlane",
                "PxVec4",
                &[0.5, -0.25, 0.75, 3.25][..],
            ),
        ] {
            let bytes = scalars
                .iter()
                .flat_map(|&value: &f32| match endian {
                    Endian::Little => value.to_le_bytes(),
                    Endian::Big => value.to_be_bytes(),
                })
                .collect::<Vec<_>>();
            let value =
                crate::decode_value(datatype, &bytes, 1, false, &header, &Default::default())
                    .unwrap();
            builder.set_value(id, property, value).unwrap();
        }
        builder
            .shift_origin(1, [2.0, -4.0, 8.0], &mut || true)
            .unwrap();
        assert_eq!(
            builder.state.objects[&2].properties["WorldBounds"],
            scalar_array(&[-3.0, 2.0, -11.0, 2.0, 9.0, -2.0])
        );
        assert_eq!(
            builder.state.objects[&6].properties["ProjectionPlane"],
            scalar_array(&[0.5, -0.25, 0.75, 11.25])
        );
        assert_eq!(
            builder.state.objects[&11].properties["KinematicTarget"],
            Value::Transform {
                position: [4.25, 1.5, -4.25],
                rotation: [-0.0, 0.5, 0.0, 0.5],
            }
        );
    }
}

#[test]
fn malformed_captured_world_fields_fail_before_any_shift_is_published() {
    for (id, property, value) in [
        (2, "WorldBounds", scalar_array(&[0.0; 5])),
        (2, "WorldBounds", Value::Bytes(vec![0; 24].into())),
        (
            5,
            "WorldBounds",
            Value::Array(vec![Value::U64(0); 6].into()),
        ),
        (11, "KinematicTarget", Value::Vec3([0.0; 3])),
        (6, "ProjectionPlane", scalar_array(&[0.0; 3])),
        (6, "ProjectionPlane", Value::Bytes(vec![0; 16].into())),
        (
            9,
            "ProjectionPlane",
            Value::Array(vec![Value::F64(0.0); 4].into()),
        ),
    ] {
        let mut builder = captured_world_fields();
        builder.set_value(id, property, value).unwrap();
        let before = builder.state.clone();
        let actions = builder.recording.actions.len();
        assert!(
            builder
                .shift_origin(1, [2.0, -4.0, 8.0], &mut || true)
                .is_err()
        );
        assert_eq!(builder.state, before, "{property}");
        assert_eq!(builder.recording.actions.len(), actions, "{property}");
    }
}

#[test]
fn captured_world_fields_share_the_scene_budget_and_cancellation_boundary() {
    for stop_after in [0, 12, 24] {
        let mut builder = captured_world_fields();
        if stop_after == 0 {
            builder.memory_limit = builder.retained_bytes + 1024;
        }
        let before = builder.state.clone();
        let actions = builder.recording.actions.len();
        let mut checks = 0;
        let result = builder.shift_origin(1, [2.0, -4.0, 8.0], &mut || {
            checks += 1;
            stop_after == 0 || checks < stop_after
        });
        assert!(result.is_err());
        assert_eq!(builder.state, before);
        assert_eq!(builder.recording.actions.len(), actions);
    }
}

#[test]
fn stale_membership_cannot_shift_captured_world_fields_of_a_reused_actor() {
    let mut builder = captured_world_fields();
    builder.action(Action::Destroy(11)).unwrap();
    builder
        .action(Action::Create(Object {
            id: 11,
            generation: 1,
            type_name: "physx3::PxRigidDynamic".into(),
            properties: Default::default(),
        }))
        .unwrap();
    builder
        .set_value(
            11,
            "WorldBounds",
            scalar_array(&[-1.0, -2.0, -3.0, 4.0, 5.0, 6.0]),
        )
        .unwrap();
    builder
        .set_value(
            11,
            "KinematicTarget",
            Value::Transform {
                position: [6.25, -2.5, 3.75],
                rotation: [0.0, 0.0, 0.0, 1.0],
            },
        )
        .unwrap();
    let expected = builder.state.objects[&11].clone();
    builder
        .shift_origin(1, [2.0, -4.0, 8.0], &mut || true)
        .unwrap();
    assert_eq!(builder.state.objects[&11], expected);
}
