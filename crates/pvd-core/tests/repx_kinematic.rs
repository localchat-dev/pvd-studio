use pvd_core::{
    Recording, Value,
    repx::{self, Selection},
};
use pvd_protocol::{Decoder, Endian, Event, Limits, MessageArg, Name};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Default)]
struct WireState {
    pose: Option<[f32; 7]>,
    linear: Option<[f32; 3]>,
    angular: Option<[f32; 3]>,
    written: [usize; 3],
}

fn wire_floats<const N: usize>(data: &[u8]) -> [f32; N] {
    assert_eq!(data.len(), N * 4);
    std::array::from_fn(|index| {
        f32::from_le_bytes(data[index * 4..index * 4 + 4].try_into().unwrap())
    })
}

fn wire_field(state: &mut WireState, name: &str, data: &[u8], ordinal: usize) {
    match name {
        "GlobalPose" => {
            let raw = wire_floats::<7>(data);
            state.pose = Some([raw[4], raw[5], raw[6], raw[0], raw[1], raw[2], raw[3]]);
            state.written[0] = ordinal;
        }
        "LinearVelocity" => {
            state.linear = Some(wire_floats(data));
            state.written[1] = ordinal;
        }
        "AngularVelocity" => {
            state.angular = Some(wire_floats(data));
            state.written[2] = ordinal;
        }
        _ => {}
    }
}

fn wire_message(
    state: &mut WireState,
    arguments: &[MessageArg],
    strings: &BTreeMap<u32, String>,
    data: &[u8],
    ordinal: usize,
) {
    for argument in arguments {
        let start = argument.offset as usize;
        let end = start + argument.size as usize;
        assert!(end <= data.len());
        wire_field(
            state,
            &strings[&argument.property],
            &data[start..end],
            ordinal,
        );
    }
}

fn wire_frames(bytes: &[u8], actor: u64, scene: u64) -> (Vec<(WireState, usize)>, usize) {
    let mut decoder = Decoder::new(Limits::default());
    let groups = decoder.feed(bytes).unwrap();
    decoder.finish().unwrap();
    assert_eq!(decoder.header().unwrap().endian, Endian::Little);
    let mut strings = BTreeMap::new();
    let mut messages = BTreeMap::<Name, Vec<MessageArg>>::new();
    let mut active = BTreeMap::new();
    let mut state = WireState::default();
    let mut frames = Vec::new();
    let mut ordinal = 0;
    for group in groups {
        for event in group.events {
            ordinal += 1;
            match event {
                Event::StringHandle { handle, value } => {
                    strings.insert(handle, value);
                }
                Event::CreatePropertyMessage { name, args, .. } => {
                    messages.insert(name, args);
                }
                Event::SetPropertyValue {
                    id, property, data, ..
                } if id == actor => {
                    wire_field(&mut state, &strings[&property], &data, ordinal);
                }
                Event::SetPropertyMessage { id, message, data } if id == actor => {
                    wire_message(&mut state, &messages[&message], &strings, &data, ordinal);
                }
                Event::BeginPropertyMessageGroup { message } => {
                    active.insert(group.stream_id, message);
                }
                Event::EndPropertyMessageGroup => {
                    active.remove(&group.stream_id);
                }
                Event::SendPropertyMessageFromGroup { id, data } if id == actor => {
                    wire_message(
                        &mut state,
                        &messages[&active[&group.stream_id]],
                        &strings,
                        &data,
                        ordinal,
                    );
                }
                Event::EndSection { id, name, .. }
                    if id == scene && strings.get(&name).map(String::as_str) == Some("frame") =>
                {
                    frames.push((state.clone(), ordinal));
                }
                _ => {}
            }
        }
    }
    (frames, ordinal)
}

fn close<const N: usize>(actual: &[f32; N], expected: &serde_json::Value) {
    let expected = expected.as_array().unwrap();
    assert_eq!(expected.len(), N);
    for (actual, expected) in actual.iter().zip(expected) {
        let expected = expected.as_f64().unwrap();
        let actual = f64::from(*actual);
        assert!(actual.is_finite() && expected.is_finite());
        assert!(
            (actual - expected).abs() <= 1e-6_f64.max(1e-5 * actual.abs().max(expected.abs())),
            "{actual} != {expected}"
        );
    }
}

#[test]
#[ignore = "requires the same-run eight-frame tools/repx-kinematics capture"]
fn kinematic_parameter_snapshots_match_wire_boundaries_without_reviving_targets() {
    let directory = PathBuf::from(
        std::env::var_os("PVD_REPX_KINEMATIC_DIRECTORY")
            .expect("Set PVD_REPX_KINEMATIC_DIRECTORY to the tools/repx-kinematics output"),
    );
    let bytes = std::fs::read(directory.join("capture.pvdstream")).unwrap();
    let recording = Recording::from_bytes(&bytes).unwrap();
    assert_eq!(recording.frame_count(), 8);
    let initial = recording.state_at(0).unwrap();
    let initial_actor = initial
        .objects
        .values()
        .find(|object| object.type_name == "physx3::PxRigidDynamic")
        .unwrap();
    let identity = (initial_actor.id, initial_actor.generation);
    let Value::Ref(scene) = initial_actor.properties["Scene"] else {
        panic!("captured Scene reference missing")
    };
    let (wire, _) = wire_frames(&bytes, identity.0, scene);
    assert_eq!(wire.len(), 8);
    let phases = [
        "prestep", "prestep", "prestep", "prestep", "prestep", "prestep", "poststep", "prestep",
    ];
    for frame in [5, 0, 6, 4, 7, 2, 1, 3, 4] {
        let state = recording.state_at(frame).unwrap();
        assert!(state.errors.is_empty());
        let actor = &state.objects[&identity.0];
        assert_eq!((actor.id, actor.generation), identity);
        let flags = [3, 3, 3, 3, 2, 0, 1, 1][frame];
        assert_eq!(actor.properties["RigidBodyFlags"], Value::U64(flags));
        assert_eq!(actor.properties.contains_key("KinematicTarget"), frame != 0);
        let load = |name: &str| -> serde_json::Value {
            serde_json::from_slice(&std::fs::read(directory.join(name)).unwrap()).unwrap()
        };
        let truth = load(&format!("frame-{frame}-truth.json"));
        let phase = load(&format!("frame-{frame}-{}-truth.json", phases[frame]));
        assert_eq!(truth["schema"], 2);
        assert_eq!(truth, phase);
        assert_eq!(truth["flags"].as_u64().unwrap(), flags);
        let (wire_state, end_ordinal) = &wire[frame];
        close(&wire_state.pose.unwrap(), &truth["pose"]);
        close(&wire_state.linear.unwrap(), &truth["linear"]);
        close(&wire_state.angular.unwrap(), &truth["angular"]);
        let Value::Transform { position, rotation } = actor.properties["GlobalPose"] else {
            panic!("captured GlobalPose missing")
        };
        assert_eq!(
            wire_state.pose.unwrap().map(f32::to_bits),
            [
                position[0],
                position[1],
                position[2],
                rotation[0],
                rotation[1],
                rotation[2],
                rotation[3]
            ]
            .map(f32::to_bits)
        );
        let boundary = load(&format!("frame-{frame}-boundary.json"));
        let before = usize::try_from(boundary["prestep_bytes"].as_u64().unwrap()).unwrap();
        let after = usize::try_from(boundary["poststep_bytes"].as_u64().unwrap()).unwrap();
        assert!(before < after && after <= bytes.len());
        let (previous, before_ordinal) = wire_frames(&bytes[..before], identity.0, scene);
        let (completed, _) = wire_frames(&bytes[..after], identity.0, scene);
        assert_eq!(previous.len(), frame);
        assert_eq!(completed.len(), frame + 1);
        if frame == 6 {
            for written in wire_state.written {
                assert!(before_ordinal < written && written < *end_ordinal);
            }
            assert_eq!(
                completed[6].0.pose.unwrap().map(f32::to_bits),
                wire_state.pose.unwrap().map(f32::to_bits)
            );
            assert_ne!(load("frame-6-prestep-truth.json")["pose"], truth["pose"]);
        }
        for selection in [Selection::Frame, Selection::Actor(actor.id)] {
            let exported = repx::export(&state, selection).unwrap();
            assert_eq!(
                (exported.actors, exported.shapes, exported.materials),
                (1, 1, 1)
            );
            assert_eq!(exported.kinematic_actors, usize::from(flags & 1 != 0));
            assert!(
                !exported.xml.contains("KinematicTarget") && !exported.xml.contains("IsSleeping")
            );
            assert_eq!(
                exported
                    .xml
                    .contains("eUSE_KINEMATIC_TARGET_FOR_SCENE_QUERIES"),
                flags & 2 != 0
            );
            assert_eq!(
                exported.xml.contains("<RigidBodyFlags>eKINEMATIC"),
                flags & 1 != 0
            );
        }
    }
    let first_dynamic = recording.state_at(4).unwrap();
    let second_dynamic = recording.state_at(5).unwrap();
    assert_eq!(
        first_dynamic.objects[&identity.0].properties["KinematicTarget"],
        second_dynamic.objects[&identity.0].properties["KinematicTarget"]
    );
    let last_kinematic = recording.state_at(6).unwrap();
    assert_ne!(
        first_dynamic.objects[&identity.0].properties["KinematicTarget"],
        last_kinematic.objects[&identity.0].properties["KinematicTarget"]
    );
}
