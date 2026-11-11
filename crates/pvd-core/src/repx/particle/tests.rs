use super::*;

fn validate_with_budget(object: &Object) -> Result<(), Error> {
    let mut budget = 128 * 1024 * 1024;
    validate(object, &mut budget).map(|_| ())
}

fn fixture(fluid: bool) -> Object {
    let name = if fluid {
        "PxParticleFluid"
    } else {
        "PxParticleSystem"
    };
    let mut object = Object {
        id: 1,
        generation: 0,
        type_name: format!("physx3::{name}"),
        properties: BTreeMap::new(),
    };
    for (field, number) in [
        ("ActorFlags", 1),
        ("DominanceGroup", 0),
        ("OwnerClient", 0),
        ("ClientBehaviorFlags", 0),
        ("ParticleBaseFlags", 38),
        ("ParticleReadDataFlags", 11),
        ("MaxParticles", 64),
        ("NbParticles", 32),
        ("ValidParticleRange", 32),
    ] {
        object.properties.insert(field.into(), Value::U64(number));
    }
    for (field, number) in [
        ("Damping", 0.2),
        ("ParticleMass", 0.7),
        ("Restitution", 0.3),
        ("DynamicFriction", 0.2),
        ("StaticFriction", 0.4),
        ("MaxMotionDistance", 0.25),
        ("RestOffset", 0.04),
        ("ContactOffset", 0.1),
        ("GridSize", if fluid { 1.28 } else { 1.92 }),
    ] {
        object.properties.insert(field.into(), Value::F32(number));
    }
    object
        .properties
        .insert("Name".into(), Value::String("particles".into()));
    object
        .properties
        .insert("ConcreteTypeName".into(), Value::String(name.into()));
    object
        .properties
        .insert("ExternalAcceleration".into(), Value::Vec3([0.0; 3]));
    object.properties.insert(
        "ProjectionPlane".into(),
        Value::Array(
            vec![
                Value::F32(0.0),
                Value::F32(1.0),
                Value::F32(0.0),
                Value::F32(3.0),
            ]
            .into(),
        ),
    );
    object.properties.insert(
        "SimulationFilterData".into(),
        Value::Array(vec![Value::U64(0); 4].into()),
    );
    object.properties.insert(
        "ValidParticleBitmap".into(),
        Value::Array(vec![Value::U64(u32::MAX.into()), Value::U64(0)].into()),
    );
    object
        .properties
        .insert("Flags".into(), Value::Array(vec![Value::U64(1); 32].into()));
    for field in ["Positions", "Velocities"] {
        object.properties.insert(
            field.into(),
            Value::Array(vec![Value::Vec3([0.0; 3]); 32].into()),
        );
    }
    if fluid {
        for (field, number) in [
            ("Stiffness", 13.0),
            ("Viscosity", 1.3),
            ("RestParticleDistance", 0.04),
        ] {
            object.properties.insert(field.into(), Value::F32(number));
        }
    }
    object
}

#[test]
fn particle_dense_bitmap_boundary_and_rest_offsets_are_validated() {
    let mut object = fixture(false);
    assert!(validate_with_budget(&object).is_ok());
    object.properties.insert(
        "ValidParticleBitmap".into(),
        Value::Array(vec![Value::U64(u32::MAX.into())].into()),
    );
    assert!(validate_with_budget(&object).is_ok());
    object
        .properties
        .insert("ParticleBaseFlags".into(), Value::U64(54));
    object
        .properties
        .insert("ParticleReadDataFlags".into(), Value::U64(15));
    assert!(validate_with_budget(&object).is_err());
    object.properties.insert(
        "RestOffsets".into(),
        Value::Array(vec![Value::F32(0.02); 32].into()),
    );
    assert!(validate_with_budget(&object).is_ok());
    assert!(validate(&object, &mut 0).is_err());
}

#[test]
fn particle_incomplete_nonfinite_and_unrestorable_state_is_rejected() {
    for (field, replacement) in [
        ("NbParticles", Value::U64(0)),
        ("ValidParticleRange", Value::U64(33)),
        ("MaxParticles", Value::U64(u64::MAX)),
        ("ParticleBaseFlags", Value::U64(36)),
        ("ParticleBaseFlags", Value::U64(102)),
        ("ParticleReadDataFlags", Value::U64(9)),
        ("ParticleReadDataFlags", Value::U64(27)),
        ("Damping", Value::F32(f32::NAN)),
        ("ParticleMass", Value::F32(0.0)),
        ("Flags", Value::Array(vec![Value::U64(3); 32].into())),
        (
            "ValidParticleBitmap",
            Value::Array(vec![Value::U64(7), Value::U64(0)].into()),
        ),
        (
            "Positions",
            Value::Array(vec![Value::Vec3([f32::NAN; 3]); 32].into()),
        ),
        (
            "Velocities",
            Value::Array(vec![Value::Vec3([0.0; 3]); 31].into()),
        ),
        (
            "RestOffsets",
            Value::Array(vec![Value::F32(0.02); 32].into()),
        ),
    ] {
        let mut object = fixture(false);
        object.properties.insert(field.into(), replacement);
        assert!(validate_with_budget(&object).is_err(), "accepted {field}");
    }
    for field in [
        "Positions",
        "Velocities",
        "Flags",
        "ParticleBaseFlags",
        "SimulationFilterData",
        "ProjectionPlane",
    ] {
        let mut object = fixture(false);
        object.properties.remove(field);
        assert!(
            validate_with_budget(&object).is_err(),
            "accepted missing {field}"
        );
    }
}

#[test]
fn sparse_particle_buffers_preserve_indices_and_charge_expanded_range() {
    let mut object = fixture(false);
    for (field, number) in [
        ("NbParticles", 3),
        ("ValidParticleRange", 64),
        ("ParticleBaseFlags", 54),
        ("ParticleReadDataFlags", 15),
    ] {
        object.properties.insert(field.into(), Value::U64(number));
    }
    object.properties.insert(
        "ValidParticleBitmap".into(),
        Value::Array(
            vec![
                Value::U64(1 << 31),
                Value::U64((1 << 31) | 2),
                Value::U64(0),
            ]
            .into(),
        ),
    );
    object
        .properties
        .insert("Flags".into(), Value::Array(vec![Value::U64(1); 3].into()));
    object.properties.insert(
        "RestOffsets".into(),
        Value::Array(vec![Value::F32(0.01), Value::F32(0.02), Value::F32(0.03)].into()),
    );
    for field in ["Positions", "Velocities"] {
        object.properties.insert(
            field.into(),
            Value::Array(
                vec![
                    Value::Vec3([1.0; 3]),
                    Value::Vec3([2.0; 3]),
                    Value::Vec3([3.0; 3]),
                ]
                .into(),
            ),
        );
    }
    let mut budget = 128 * 1024 * 1024;
    let indices = validate(&object, &mut budget).unwrap();
    assert_eq!(indices, [31, 33, 63]);
    assert_eq!(budget, 128 * 1024 * 1024 - 64 * (64 + 256));
    let mut xml = String::new();
    write_buffer(&mut xml, &object, "Positions", &indices, 64, "0 0 0").unwrap();
    let values: Vec<_> = xml
        .split_once('>')
        .unwrap()
        .1
        .split('<')
        .next()
        .unwrap()
        .split_whitespace()
        .map(|value| value.parse::<f32>().unwrap())
        .collect();
    assert_eq!(values.len(), 64 * 3);
    for index in 0..64 {
        let expected = match index {
            31 => 1.0,
            33 => 2.0,
            63 => 3.0,
            _ => 0.0,
        };
        assert_eq!(&values[index * 3..index * 3 + 3], &[expected; 3]);
    }
    object
        .properties
        .insert("MaxParticles".into(), Value::U64(96));
    object.properties.insert(
        "ValidParticleBitmap".into(),
        Value::Array(
            vec![
                Value::U64(1 << 31),
                Value::U64((1 << 31) | 2),
                Value::U64(1),
            ]
            .into(),
        ),
    );
    assert!(validate_with_budget(&object).is_err());
    object.properties.insert(
        "ValidParticleBitmap".into(),
        Value::Array(vec![Value::U64(1 << 31), Value::U64(2)].into()),
    );
    assert!(validate_with_budget(&object).is_err());
}

#[test]
fn particle_capacity_boundary_excludes_only_the_sdk_extra_word() {
    for fluid in [false, true] {
        let mut state = particle_state(fluid);
        let object = state.objects.get_mut(&1).unwrap();
        for (field, number) in [("NbParticles", 3), ("ValidParticleRange", 64)] {
            object.properties.insert(field.into(), Value::U64(number));
        }
        for field in ["Positions", "Velocities"] {
            object.properties.insert(
                field.into(),
                Value::Array(vec![Value::Vec3([0.0; 3]); 3].into()),
            );
        }
        object
            .properties
            .insert("Flags".into(), Value::Array(vec![Value::U64(1); 3].into()));
        let valid = [Value::U64(0x8000_0001), Value::U64(0x8000_0000)];
        for extra in [None, Some(0), Some(1), Some(0xcdcd_cdcd), Some(u32::MAX)] {
            let mut bitmap = valid.to_vec();
            bitmap.extend(extra.map(|number| Value::U64(u64::from(number))));
            state
                .objects
                .get_mut(&1)
                .unwrap()
                .properties
                .insert("ValidParticleBitmap".into(), Value::Array(bitmap.into()));
            let object = &state.objects[&1];
            assert_eq!(
                validate(object, &mut (128 * 1024 * 1024)).unwrap(),
                [0, 31, 63]
            );
            let mut xml = String::new();
            write(&mut xml, &state, object, &mut (128 * 1024 * 1024)).unwrap();
            assert!(
                xml.contains("<ValidParticleBitmap>2147483649 2147483648</ValidParticleBitmap>")
            );
        }
        for bitmap in [
            vec![valid[0].clone(), valid[1].clone(), Value::I64(0)],
            vec![valid[0].clone(), valid[1].clone(), Value::F32(0.0)],
            vec![valid[0].clone(), valid[1].clone(), Value::U64(u64::MAX)],
            vec![
                valid[0].clone(),
                valid[1].clone(),
                Value::U64(0),
                Value::U64(0),
            ],
            vec![Value::U64(1), valid[1].clone(), Value::U64(0)],
            vec![valid[0].clone(), Value::U64(1), Value::U64(0)],
        ] {
            state
                .objects
                .get_mut(&1)
                .unwrap()
                .properties
                .insert("ValidParticleBitmap".into(), Value::Array(bitmap.into()));
            assert!(validate_with_budget(&state.objects[&1]).is_err());
        }
        let object = state.objects.get_mut(&1).unwrap();
        object.properties.insert(
            "ValidParticleBitmap".into(),
            Value::Array(vec![valid[0].clone(), valid[1].clone(), Value::U64(1)].into()),
        );
        object
            .properties
            .insert("MaxParticles".into(), Value::U64(96));
        assert!(validate_with_budget(object).is_err());
        object
            .properties
            .insert("MaxParticles".into(), Value::U64(63));
        object
            .properties
            .insert("ValidParticleRange".into(), Value::U64(63));
        object.properties.insert(
            "ValidParticleBitmap".into(),
            Value::Array(valid.to_vec().into()),
        );
        assert!(validate_with_budget(object).is_err());
    }
}

#[test]
fn fluid_grid_requires_exact_official_import_roundtrip() {
    let mut object = fixture(true);
    assert!(validate_with_budget(&object).is_ok());
    object.properties.insert("GridSize".into(), Value::F32(3.2));
    object
        .properties
        .insert("RestParticleDistance".into(), Value::F32(0.2));
    assert!(validate_with_budget(&object).is_err());
    assert_eq!(
        quantized_grid(quantized_grid(3.2, 0.04).unwrap(), 0.4).unwrap(),
        6.4
    );
}

#[test]
fn particle_scene_membership_requires_current_reciprocal_references() {
    use crate::Action;
    let mut state = SceneState::default();
    Action::Create(fixture(false)).apply(&mut state);
    Action::Create(Object {
        id: 2,
        generation: 0,
        type_name: "physx3::PxScene".into(),
        properties: BTreeMap::new(),
    })
    .apply(&mut state);
    Action::Set(1, "Scene".into(), Value::Ref(2)).apply(&mut state);
    Action::Set(
        2,
        "ParticleSystems".into(),
        Value::Array(vec![Value::Ref(1)].into()),
    )
    .apply(&mut state);
    assert!(selection::Plan::new(&state, Selection::Frame).is_ok());
    let mut budget = 128 * 1024 * 1024;
    assert!(write(&mut String::new(), &state, &state.objects[&1], &mut budget).is_ok());
    let mut invalid = state.clone();
    invalid.objects.get_mut(&1).unwrap().generation += 1;
    assert!(selection::Plan::new(&invalid, Selection::Frame).is_err());
    assert!(selection::Plan::new(&invalid, Selection::Actor(1)).is_err());
    let mut invalid = state.clone();
    invalid.objects.remove(&1);
    assert!(selection::Plan::new(&invalid, Selection::Frame).is_err());
    let mut invalid = state.clone();
    Action::Set(
        2,
        "ParticleSystems".into(),
        Value::Array(vec![Value::Ref(1), Value::Ref(1)].into()),
    )
    .apply(&mut invalid);
    assert!(selection::Plan::new(&invalid, Selection::Frame).is_err());
}

fn particle_state(fluid: bool) -> SceneState {
    use crate::Action;
    let mut state = SceneState::default();
    Action::Create(fixture(fluid)).apply(&mut state);
    for (id, name) in [(2, "PxScene"), (3, "PxPhysics")] {
        Action::Create(Object {
            id,
            generation: 0,
            type_name: format!("physx3::{name}"),
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
        Action::Set(3, field.into(), value).apply(&mut state);
    }
    Action::Set(1, "Scene".into(), Value::Ref(2)).apply(&mut state);
    Action::Set(
        2,
        if fluid {
            "ParticleFluids"
        } else {
            "ParticleSystems"
        }
        .into(),
        Value::Array(vec![Value::Ref(1)].into()),
    )
    .apply(&mut state);
    state
}

#[test]
fn particle_client_behavior_preserves_all_flag_combinations_and_clear() {
    let names = [
        "eREPORT_TO_FOREIGN_CLIENTS_TRIGGER_NOTIFY",
        "eREPORT_TO_FOREIGN_CLIENTS_CONTACT_NOTIFY",
        "eREPORT_TO_FOREIGN_CLIENTS_CONSTRAINT_BREAK_NOTIFY",
        "eREPORT_TO_FOREIGN_CLIENTS_SCENE_QUERY",
    ];
    for fluid in [false, true] {
        let mut state = particle_state(fluid);
        for flags in (0..=15).chain([0]) {
            crate::Action::Set(1, "ClientBehaviorFlags".into(), Value::U64(flags))
                .apply(&mut state);
            let expected = names
                .iter()
                .enumerate()
                .filter_map(|(bit, name)| (flags & (1 << bit) != 0).then_some(*name))
                .collect::<Vec<_>>()
                .join("|");
            let expected = if flags == 0 { "0" } else { &expected };
            let tag = format!("<ClientBehaviorFlags>{expected}</ClientBehaviorFlags>");
            for selection in [Selection::Frame, Selection::Actor(1)] {
                let exported = crate::repx::export(&state, selection).unwrap();
                assert_eq!(exported.xml.matches("<ClientBehaviorFlags>").count(), 1);
                assert!(
                    exported.xml.contains(&tag),
                    "{fluid} flags={flags}: {}",
                    exported.xml
                );
                assert_eq!(exported.particle_systems, 1);
            }
        }
    }
}

#[test]
fn particle_client_behavior_rejects_unknown_missing_and_mistyped_flags() {
    for fluid in [false, true] {
        let state = particle_state(fluid);
        for replacement in [
            None,
            Some(Value::U64(16)),
            Some(Value::U64(17)),
            Some(Value::U64(u64::MAX)),
            Some(Value::I64(-1)),
            Some(Value::F32(1.0)),
            Some(Value::Bool(true)),
            Some(Value::String(
                "eREPORT_TO_FOREIGN_CLIENTS_CONTACT_NOTIFY".into(),
            )),
            Some(Value::Array(Vec::new().into())),
        ] {
            let mut invalid = state.clone();
            let properties = &mut invalid.objects.get_mut(&1).unwrap().properties;
            match replacement {
                Some(value) => properties.insert("ClientBehaviorFlags".into(), value),
                None => properties.remove("ClientBehaviorFlags"),
            };
            for selection in [Selection::Frame, Selection::Actor(1)] {
                assert!(crate::repx::export(&invalid, selection).is_err());
            }
        }
        let mut budget = 0;
        assert!(validate(&state.objects[&1], &mut budget).is_err());
        assert_eq!(budget, 0);
    }
}

fn empty_particle_state(fluid: bool, per_rest: bool) -> SceneState {
    let mut state = particle_state(fluid);
    for (field, value) in [
        ("NbParticles", 0),
        ("ValidParticleRange", 0),
        ("ActorFlags", 0),
        ("ParticleBaseFlags", if per_rest { 54 } else { 38 }),
        ("ParticleReadDataFlags", 9),
    ] {
        crate::Action::Set(1, field.into(), Value::U64(value)).apply(&mut state);
    }
    state
}

#[test]
fn empty_particle_export_preserves_parameters_without_active_slots() {
    for fluid in [false, true] {
        for per_rest in [false, true] {
            let state = empty_particle_state(fluid, per_rest);
            for stale in [false, true] {
                let mut state = state.clone();
                if !stale {
                    let properties = &mut state.objects.get_mut(&1).unwrap().properties;
                    for field in [
                        "ValidParticleBitmap",
                        "Positions",
                        "Velocities",
                        "RestOffsets",
                        "Flags",
                    ] {
                        properties.remove(field);
                    }
                }
                let mut budget = 64 * 64 + 256;
                assert!(
                    validate(&state.objects[&1], &mut budget)
                        .unwrap()
                        .is_empty()
                );
                assert_eq!(budget, 0);
                for selection in [Selection::Frame, Selection::Actor(1)] {
                    let exported = crate::repx::export(&state, selection).unwrap();
                    assert_eq!(exported.particle_systems, 1);
                    assert!(exported.xml.contains("<NbParticles>0</NbParticles>"));
                    assert!(
                        exported
                            .xml
                            .contains("<ValidParticleRange>1</ValidParticleRange>")
                    );
                    assert!(
                        exported
                            .xml
                            .contains("<ValidParticleBitmap>0</ValidParticleBitmap>")
                    );
                    assert!(exported.xml.contains("<ActorFlags>0</ActorFlags>"));
                    for field in ["Positions", "Velocities", "Flags"] {
                        assert!(!exported.xml.contains(&format!("<{field}>")));
                    }
                    assert_eq!(
                        exported.xml.contains("<RestOffsets>0</RestOffsets>"),
                        per_rest
                    );
                    assert_eq!(
                        state.objects[&1].properties["ValidParticleRange"],
                        Value::U64(0)
                    );
                }
            }
        }
    }
}

#[test]
fn empty_particle_read_flags_require_representable_constructor_defaults() {
    for fluid in [false, true] {
        for per_rest in [false, true] {
            let state = empty_particle_state(fluid, per_rest);
            for flags in 0..=255 {
                let mut state = state.clone();
                crate::Action::Set(1, "ParticleReadDataFlags".into(), Value::U64(flags))
                    .apply(&mut state);
                let expected = flags & 9 == 9
                    && flags & if fluid { 128 } else { 192 } == 0
                    && (per_rest || flags & 4 == 0);
                let result = crate::repx::export(&state, Selection::Actor(1));
                assert_eq!(
                    result.is_ok(),
                    expected,
                    "fluid={fluid} rest={per_rest} flags={flags}"
                );
                if let Ok(exported) = result {
                    for (bit, name) in [
                        (16, "eCOLLISION_NORMAL_BUFFER"),
                        (32, "eCOLLISION_VELOCITY_BUFFER"),
                        (64, "eDENSITY_BUFFER"),
                    ] {
                        assert_eq!(exported.xml.contains(name), flags & bit != 0);
                    }
                }
            }
        }
    }
}

#[test]
fn empty_particle_export_rejects_inconsistent_counts_and_parameters() {
    for fluid in [false, true] {
        let state = empty_particle_state(fluid, false);
        for (field, replacement) in [
            ("NbParticles", Value::U64(1)),
            ("NbParticles", Value::I64(-1)),
            ("NbParticles", Value::F32(0.0)),
            ("ValidParticleRange", Value::U64(1)),
            ("MaxParticles", Value::U64(0)),
            ("MaxParticles", Value::U64(1_000_001)),
            ("ParticleBaseFlags", Value::U64(36)),
            ("ParticleBaseFlags", Value::U64(102)),
            ("ParticleReadDataFlags", Value::U64(8)),
            ("ParticleReadDataFlags", Value::U64(13)),
            ("ParticleMass", Value::F32(0.0)),
            ("Damping", Value::F32(f32::NAN)),
            ("ClientBehaviorFlags", Value::U64(16)),
            ("UnknownBuffer", Value::Array(Vec::new().into())),
        ] {
            let mut invalid = state.clone();
            invalid
                .objects
                .get_mut(&1)
                .unwrap()
                .properties
                .insert(field.into(), replacement);
            assert!(
                crate::repx::export(&invalid, Selection::Actor(1)).is_err(),
                "accepted empty {field}"
            );
        }
        for field in [
            "NbParticles",
            "ValidParticleRange",
            "MaxParticles",
            "ParticleReadDataFlags",
        ] {
            let mut invalid = state.clone();
            invalid
                .objects
                .get_mut(&1)
                .unwrap()
                .properties
                .remove(field);
            assert!(crate::repx::export(&invalid, Selection::Actor(1)).is_err());
        }
        let mut budget = 64 * 64 + 255;
        assert!(validate(&state.objects[&1], &mut budget).is_err());
        assert_eq!(budget, 64 * 64 + 255);
    }
}

#[test]
fn empty_particle_auxiliary_buffers_are_not_active_state() {
    for fluid in [false, true] {
        let mut state = empty_particle_state(fluid, false);
        for field in ["CollisionNormals", "Densities"] {
            let properties = &mut state.objects.get_mut(&1).unwrap().properties;
            properties.insert(
                field.into(),
                Value::Array(vec![Value::F32(f32::NAN)].into()),
            );
            let result = crate::repx::export(&state, Selection::Actor(1));
            assert_eq!(result.is_ok(), field != "Densities" || fluid);
            if let Ok(exported) = result {
                assert!(!exported.xml.contains(&format!("<{field}>")));
            }
            state.objects.get_mut(&1).unwrap().properties.remove(field);
        }
    }
    let mut active = particle_state(false);
    active.objects.get_mut(&1).unwrap().properties.insert(
        "CollisionNormals".into(),
        Value::Array(vec![Value::Vec3([0.0; 3]); 32].into()),
    );
    assert!(crate::repx::export(&active, Selection::Actor(1)).is_err());
}
