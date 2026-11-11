use pvd_core::{
    Object, Recording, Value,
    repx::{self, Selection},
};
use std::path::PathBuf;

fn fixture() -> Recording {
    let path = PathBuf::from(
        std::env::var_os("PVD_REPX_PARTICLE_DIRECTORY")
            .expect("Set PVD_REPX_PARTICLE_DIRECTORY to the same-run capture directory"),
    );
    Recording::from_path(path.join("capture.pvdstream")).unwrap()
}
fn kind(object: &Object) -> &str {
    object.type_name.rsplit("::").next().unwrap()
}

#[test]
#[ignore = "requires tools/repx-particles capture"]
fn particle_snapshots_updates_destruction_and_selection_are_exportable() {
    let recording = fixture();
    assert_eq!(recording.frame_count(), 18);
    for frame in [17, 0, 16, 7, 5, 6, 4, 2, 3, 1, 8, 9, 10, 11, 12, 13, 14, 15] {
        let state = recording.state_at(frame).unwrap();
        assert!(state.errors.is_empty());
        let exported = repx::export(&state, Selection::Frame).unwrap();
        assert_eq!(exported.xml.matches("<PxParticleSystem>").count(), 1);
        assert_eq!(exported.xml.matches("<PxParticleFluid>").count(), 1);
        assert_eq!(exported.actors, 0);
        for object in state
            .objects
            .values()
            .filter(|object| matches!(kind(object), "PxParticleSystem" | "PxParticleFluid"))
        {
            let expected = [0, 1, 2, 3, 4, 5, 6, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 0][frame];
            assert_eq!(
                object.properties.get("ClientBehaviorFlags"),
                Some(&Value::U64(expected)),
                "frame {frame} {}",
                object.type_name,
            );
            let selected = repx::export(&state, Selection::Actor(object.id)).unwrap();
            assert_eq!(
                selected.xml.matches(&format!("<{}>", kind(object))).count(),
                1
            );
        }
    }
    let empty = recording.state_at(6).unwrap();
    assert_eq!(
        repx::export(&empty, Selection::Frame)
            .unwrap()
            .particle_systems,
        2
    );
    for object in empty
        .objects
        .values()
        .filter(|object| matches!(kind(object), "PxParticleSystem" | "PxParticleFluid"))
    {
        assert_eq!(object.properties["NbParticles"], Value::U64(0));
        assert_eq!(object.properties["ValidParticleRange"], Value::U64(0));
        assert!(
            matches!(object.properties["Positions"], Value::Array(ref values) if !values.is_empty())
        );
        let selected = repx::export(&empty, Selection::Actor(object.id)).unwrap();
        assert_eq!(selected.particle_systems, 1);
        assert!(!selected.xml.contains("<Positions>"));
    }
}

#[test]
#[ignore = "requires tools/repx-particles capture"]
fn particle_missing_data_and_stale_scene_membership_fail_explicitly() {
    let state = fixture().state_at(0).unwrap();
    let object = state
        .objects
        .values()
        .find(|object| kind(object) == "PxParticleSystem")
        .unwrap();
    for field in ["Velocities", "Flags", "ProjectionPlane", "Scene"] {
        let mut invalid = state.clone();
        invalid
            .objects
            .get_mut(&object.id)
            .unwrap()
            .properties
            .remove(field);
        assert!(repx::export(&invalid, Selection::Actor(object.id)).is_err());
    }
    for (field, replacement) in [
        ("ParticleReadDataFlags", Value::U64(9)),
        ("ParticleBaseFlags", Value::U64(0)),
        ("ValidParticleRange", Value::U64(33)),
    ] {
        let mut invalid = state.clone();
        invalid
            .objects
            .get_mut(&object.id)
            .unwrap()
            .properties
            .insert(field.into(), replacement);
        assert!(repx::export(&invalid, Selection::Frame).is_err());
    }
    for object in state
        .objects
        .values()
        .filter(|object| matches!(kind(object), "PxParticleSystem" | "PxParticleFluid"))
    {
        for replacement in [
            None,
            Some(Value::U64(16)),
            Some(Value::U64(17)),
            Some(Value::I64(-1)),
            Some(Value::F32(1.0)),
            Some(Value::Bool(true)),
        ] {
            let mut invalid = state.clone();
            let properties = &mut invalid.objects.get_mut(&object.id).unwrap().properties;
            match replacement {
                Some(value) => properties.insert("ClientBehaviorFlags".into(), value),
                None => properties.remove("ClientBehaviorFlags"),
            };
            assert!(repx::export(&invalid, Selection::Frame).is_err());
            assert!(repx::export(&invalid, Selection::Actor(object.id)).is_err());
        }
    }
    let mut invalid = state.clone();
    invalid.objects.get_mut(&object.id).unwrap().generation += 1;
    assert!(repx::export(&invalid, Selection::Frame).is_err());
    assert!(repx::export(&invalid, Selection::Actor(object.id)).is_err());
    let mut invalid = state.clone();
    invalid.objects.remove(&object.id);
    assert!(repx::export(&invalid, Selection::Frame).is_err());
}

#[test]
#[ignore = "requires tools/repx-particles --empty capture and same-run getter truth"]
fn empty_particle_creation_clear_refill_and_read_flags_match_sdk() {
    let directory = PathBuf::from(
        std::env::var_os("PVD_REPX_EMPTY_PARTICLE_DIRECTORY").expect(
            "Set PVD_REPX_EMPTY_PARTICLE_DIRECTORY to the same-run empty capture directory",
        ),
    );
    let recording = Recording::from_path(directory.join("capture.pvdstream")).unwrap();
    assert_eq!(recording.frame_count(), 4);
    for frame in [2, 0, 3, 1, 0, 2] {
        let truth: serde_json::Value = serde_json::from_slice(
            &std::fs::read(directory.join(format!("frame-{frame}-truth.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(truth["schema"], 2);
        assert_eq!(truth["sdk_version"], serde_json::json!([3, 4, 2]));
        let state = recording.state_at(frame).unwrap();
        assert!(state.errors.is_empty());
        let exported = repx::export(&state, Selection::Frame).unwrap();
        assert_eq!(exported.particle_systems, 8);
        let expected = truth["systems"].as_array().unwrap();
        assert_eq!(expected.len(), 8);
        for system in expected {
            let id = system["id"].as_u64().unwrap();
            let object = &state.objects[&id];
            assert_eq!(kind(object), system["type"].as_str().unwrap());
            for (field, key) in [
                ("NbParticles", "count"),
                ("ValidParticleRange", "range"),
                ("MaxParticles", "capacity"),
                ("ActorFlags", "actor_flags"),
                ("ClientBehaviorFlags", "client_flags"),
                ("ParticleBaseFlags", "base_flags"),
                ("ParticleReadDataFlags", "read_flags"),
            ] {
                assert_eq!(
                    object.properties[field],
                    Value::U64(system[key].as_u64().unwrap()),
                    "frame {frame} {id} {field}"
                );
            }
            let selected = repx::export(&state, Selection::Actor(id)).unwrap();
            assert_eq!(selected.particle_systems, 1);
            if system["count"] == 0 {
                assert!(!selected.xml.contains("<Positions>"));
                assert!(!selected.xml.contains("<Velocities>"));
                assert!(!selected.xml.contains("<Flags>"));
                assert!(
                    selected
                        .xml
                        .contains("<ValidParticleBitmap>0</ValidParticleBitmap>")
                );
                if frame == 0 {
                    assert!(object.properties.get("Positions").is_none_or(
                        |value| matches!(value, Value::Array(values) if values.is_empty())
                    ));
                } else if frame == 2 && system["name"].as_str().unwrap().starts_with("cycle-") {
                    assert!(
                        matches!(object.properties.get("Positions"), Some(Value::Array(values)) if values.len() == 3)
                    );
                }
            } else {
                let mut expected_bitmap = [0u64; 2];
                for particle in system["particles"].as_array().unwrap() {
                    let index = particle["index"].as_u64().unwrap() as usize;
                    expected_bitmap[index / 32] |= 1 << (index % 32);
                }
                let Value::Array(bitmap) = &object.properties["ValidParticleBitmap"] else {
                    panic!("missing captured particle bitmap");
                };
                assert_eq!(bitmap.len(), 3);
                assert_eq!(
                    bitmap[..2],
                    expected_bitmap.map(Value::U64),
                    "frame {frame} {id} bitmap disagrees with SDK getter indices"
                );
                assert!(selected.xml.contains(&format!(
                    "<ValidParticleBitmap>{} {}</ValidParticleBitmap>",
                    expected_bitmap[0], expected_bitmap[1]
                )));
                assert!(
                    selected
                        .xml
                        .contains("<ValidParticleRange>64</ValidParticleRange>")
                );
                assert!(selected.xml.contains("<Positions>"));
            }
        }
    }
}
