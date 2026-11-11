use pvd_core::{LiveRecording, Object, Recording, SceneState, Value, live::HistoryConfig, repx};
use pvd_protocol::{Decoder, Endian, Event, Limits, MessageArg, Name};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Pose {
    position: [f32; 3],
    rotation: [f32; 4],
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SdkActor {
    id: u64,
    scene: u64,
    flags: u64,
    actor_flags: u64,
    is_sleeping: Option<bool>,
    pose: Pose,
    cmass: Pose,
    linear: [f32; 3],
    angular: [f32; 3],
    wake: f32,
    target_valid: bool,
    target: Option<Pose>,
    body_target: Option<Pose>,
}

#[derive(Deserialize)]
struct Phase {
    name: String,
    completed_steps: usize,
    wire_bytes: usize,
    actor: Option<SdkActor>,
    reference: Option<String>,
}

#[derive(Deserialize)]
struct Fixture {
    sdk: String,
    initial_buffered: bool,
    scene: u64,
    frames: usize,
    phases: Vec<Phase>,
}

#[derive(Clone, Debug, Default, Serialize)]
struct Observation {
    present: bool,
    scene: Option<u64>,
    flags: Option<u64>,
    actor_flags: Option<u64>,
    is_sleeping: Option<bool>,
    linear: Option<[f32; 3]>,
    angular: Option<[f32; 3]>,
    wake: Option<f32>,
    target: Option<Pose>,
    last_sleep_event: Option<usize>,
    last_target_event: Option<usize>,
}

impl Observation {
    fn sleep_and_zero_fields(&self) -> bool {
        self.present
            && self.is_sleeping == Some(true)
            && self.linear == Some([0.0; 3])
            && self.angular == Some([0.0; 3])
            && self.wake == Some(0.0)
    }
}

fn from_object(object: Option<&Object>) -> Observation {
    let Some(object) = object else {
        return Observation::default();
    };
    let integer = |field| match object.properties.get(field) {
        Some(Value::U64(value)) => Some(*value),
        _ => None,
    };
    let vector = |field| match object.properties.get(field) {
        Some(Value::Vec3(value)) => Some(*value),
        _ => None,
    };
    Observation {
        present: true,
        scene: match object.properties.get("Scene") {
            Some(Value::Ref(id)) => Some(*id),
            _ => None,
        },
        flags: integer("RigidBodyFlags"),
        actor_flags: integer("ActorFlags"),
        is_sleeping: match object.properties.get("IsSleeping") {
            Some(Value::Bool(value)) => Some(*value),
            _ => None,
        },
        linear: vector("LinearVelocity"),
        angular: vector("AngularVelocity"),
        wake: match object.properties.get("WakeCounter") {
            Some(Value::F32(value)) => Some(*value),
            _ => None,
        },
        target: match object.properties.get("KinematicTarget") {
            Some(Value::Transform { position, rotation }) => Some(Pose {
                position: *position,
                rotation: *rotation,
            }),
            _ => None,
        },
        ..Observation::default()
    }
}

fn unsigned(data: &[u8]) -> u64 {
    assert!(data.len() <= 8);
    let mut bytes = [0; 8];
    bytes[..data.len()].copy_from_slice(data);
    u64::from_le_bytes(bytes)
}

fn floats<const N: usize>(data: &[u8]) -> [f32; N] {
    assert_eq!(data.len(), N * 4);
    std::array::from_fn(|index| {
        f32::from_le_bytes(data[index * 4..index * 4 + 4].try_into().unwrap())
    })
}

fn observe_field(observation: &mut Observation, name: &str, data: &[u8], ordinal: usize) {
    match name {
        "Scene" => observation.scene = Some(unsigned(data)),
        "RigidBodyFlags" => observation.flags = Some(unsigned(data)),
        "ActorFlags" => observation.actor_flags = Some(unsigned(data)),
        "IsSleeping" => {
            assert!(matches!(data, [0] | [1]));
            observation.is_sleeping = Some(data[0] == 1);
            observation.last_sleep_event = Some(ordinal);
        }
        "LinearVelocity" => observation.linear = Some(floats(data)),
        "AngularVelocity" => observation.angular = Some(floats(data)),
        "WakeCounter" => observation.wake = Some(floats::<1>(data)[0]),
        "KinematicTarget" => {
            let raw = floats::<7>(data);
            observation.target = Some(Pose {
                position: raw[4..].try_into().unwrap(),
                rotation: raw[..4].try_into().unwrap(),
            });
            observation.last_target_event = Some(ordinal);
        }
        _ => {}
    }
}

fn observe_message(
    observation: &mut Observation,
    definitions: &BTreeMap<Name, Vec<MessageArg>>,
    strings: &BTreeMap<u32, String>,
    message: Name,
    data: &[u8],
    ordinal: usize,
) {
    for arg in &definitions[&message] {
        let start = arg.offset as usize;
        let end = start + arg.size as usize;
        observe_field(
            observation,
            &strings[&arg.property],
            &data[start..end],
            ordinal,
        );
    }
}

fn raw_observation(bytes: &[u8], actor: u64, scene: u64) -> (Observation, usize, bool) {
    let mut decoder = Decoder::new(Limits::default());
    let groups = decoder.feed(bytes).unwrap();
    decoder.finish().unwrap();
    assert_eq!(decoder.header().unwrap().endian, Endian::Little);
    let mut strings = BTreeMap::new();
    let mut definitions = BTreeMap::new();
    let mut active = BTreeMap::new();
    let mut observation = Observation::default();
    let mut completed = 0;
    let mut inside_frame = false;
    let mut ordinal = 0;
    for group in groups {
        for event in group.events {
            ordinal += 1;
            match event {
                Event::StringHandle { handle, value } => {
                    strings.insert(handle, value);
                }
                Event::CreatePropertyMessage { name, args, .. } => {
                    definitions.insert(name, args);
                }
                Event::CreateInstance { id, .. } if id == actor => {
                    observation = Observation {
                        present: true,
                        ..Observation::default()
                    };
                }
                Event::DestroyInstance { id } if id == actor => {
                    observation = Observation::default();
                }
                Event::SetPropertyValue {
                    id, property, data, ..
                } if id == actor => {
                    observe_field(&mut observation, &strings[&property], &data, ordinal);
                }
                Event::SetPropertyMessage { id, message, data } if id == actor => {
                    observe_message(
                        &mut observation,
                        &definitions,
                        &strings,
                        message,
                        &data,
                        ordinal,
                    );
                }
                Event::BeginPropertyMessageGroup { message } => {
                    active.insert(group.stream_id, message);
                }
                Event::EndPropertyMessageGroup => {
                    active.remove(&group.stream_id);
                }
                Event::SendPropertyMessageFromGroup { id, data } if id == actor => {
                    observe_message(
                        &mut observation,
                        &definitions,
                        &strings,
                        active[&group.stream_id],
                        &data,
                        ordinal,
                    );
                }
                Event::BeginSection { id, name, .. }
                    if id == scene && strings.get(&name).map(String::as_str) == Some("frame") =>
                {
                    assert!(!inside_frame);
                    inside_frame = true;
                }
                Event::EndSection { id, name, .. }
                    if id == scene && strings.get(&name).map(String::as_str) == Some("frame") =>
                {
                    assert!(inside_frame);
                    inside_frame = false;
                    completed += 1;
                }
                _ => {}
            }
        }
    }
    (observation, completed, inside_frame)
}

fn snapshot_exports(state: &SceneState, actor: u64) -> (serde_json::Value, serde_json::Value) {
    let run = |selection| match repx::export(state, selection) {
        Ok(exported) => {
            assert!(!exported.xml.contains("KinematicTarget"));
            assert!(!exported.xml.contains("IsSleeping"));
            serde_json::json!({
                "exported": true,
                "kinematic_actors": exported.kinematic_actors,
                "kinematic_target_restored": false,
                "kinematic_motion_restored": false,
            })
        }
        Err(error) => serde_json::json!({"exported": false, "error": error.to_string()}),
    };
    (
        run(repx::Selection::Frame),
        run(repx::Selection::Actor(actor)),
    )
}

struct Scratch {
    root: PathBuf,
    path: PathBuf,
}
impl Scratch {
    fn new(root: &Path) -> Self {
        let root = fs::canonicalize(root).unwrap();
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = root.join(format!("oracle-history-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self { root, path }
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        if let Ok(path) = fs::canonicalize(&self.path) {
            assert!(path.starts_with(&self.root) && path != self.root);
            let _ = fs::remove_dir_all(path);
        }
    }
}

fn oracle(variant: &str, initial_buffered: bool) {
    let root = PathBuf::from(
        std::env::var_os("PVD_REPX_SLEEPING_DIRECTORY").expect("set PVD_REPX_SLEEPING_DIRECTORY"),
    );
    let directory = root.join(variant);
    let fixture: Fixture =
        serde_json::from_slice(&fs::read(directory.join("oracle.json")).unwrap()).unwrap();
    assert_eq!(fixture.sdk, "3.4.2");
    assert_eq!(fixture.initial_buffered, initial_buffered);
    assert_eq!(fixture.frames, 10);
    assert_eq!(fixture.phases.last().unwrap().completed_steps, 10);
    let bytes = fs::read(directory.join("capture.pvdstream")).unwrap();
    let actor = fixture.phases[0].actor.as_ref().unwrap().id;
    let scratch = Scratch::new(&directory);
    let mut live = LiveRecording::with_history(HistoryConfig {
        directory: scratch.path.clone(),
        min_free_disk_bytes: 0,
        ..Default::default()
    })
    .unwrap();
    let mut display = SceneState::default();
    let mut display_frame = None;
    let mut offset = 0;
    let mut rows = Vec::new();
    let mut mixed_raw_phases = Vec::new();
    let mut mixed_zero_frame_phases = Vec::new();
    let mut confirmed_sleep_phases = Vec::new();
    let mut idle_phases = Vec::new();
    let mut previous_completed = 0;
    for phase in &fixture.phases {
        let previous_display = display.clone();
        let previous_display_frame = display_frame;
        assert!(phase.wire_bytes >= offset && phase.wire_bytes <= bytes.len());
        live.feed(&bytes[offset..phase.wire_bytes]).unwrap();
        offset = phase.wire_bytes;
        let published = if let Some((frame, update)) = live.take_update() {
            update.apply(&mut display);
            display_frame = Some(frame);
            true
        } else {
            false
        };
        if phase.completed_steps == previous_completed {
            assert!(
                !published,
                "unclosed state must not publish at {}",
                phase.name
            );
            assert_eq!(display_frame, previous_display_frame);
            assert_eq!(display, previous_display);
        } else {
            assert_eq!(phase.completed_steps, previous_completed + 1);
            assert!(published, "completed frame must publish at {}", phase.name);
            assert_eq!(display_frame, Some(phase.completed_steps - 1));
        }
        previous_completed = phase.completed_steps;
        assert_eq!(live.frame_count(), phase.completed_steps);
        let (raw, completed, inside_frame) =
            raw_observation(&bytes[..offset], actor, fixture.scene);
        assert_eq!(completed, phase.completed_steps);
        let snapshot = live.snapshot().unwrap();
        let closed = if let Some(frame) = display_frame {
            let state = snapshot.state_at(frame).unwrap();
            assert_eq!(
                state, display,
                "display must represent its committed frame at {}",
                phase.name
            );
            Some(state)
        } else {
            None
        };
        let zero_initial = (phase.completed_steps == 0).then(|| snapshot.initial_state().clone());
        let memory_snapshot = if phase.completed_steps == 0 {
            let mut input = LiveRecording::default();
            input.feed(&bytes[..offset]).unwrap();
            Some(input.snapshot().unwrap().initial_state().clone())
        } else {
            None
        };
        if let Some(sdk) = &phase.actor {
            assert_eq!(sdk.id, actor);
            assert_eq!(sdk.target_valid, sdk.target.is_some());
            assert_eq!(sdk.target_valid, sdk.body_target.is_some());
            if phase.name.starts_with("first-idle-") || phase.name.starts_with("second-idle-") {
                idle_phases.push(phase.name.as_str());
            }
            if phase.name.ends_with("-sleep-confirmed") {
                assert_eq!(sdk.is_sleeping, Some(true));
                assert!(!sdk.target_valid);
                assert_eq!(sdk.scene, fixture.scene);
                assert_eq!(sdk.flags, 3);
                assert_eq!(sdk.actor_flags & 8, 0);
                assert_eq!(sdk.linear, [0.0; 3]);
                assert_eq!(sdk.angular, [0.0; 3]);
                assert_eq!(sdk.wake, 0.0);
                assert!(raw.sleep_and_zero_fields());
                assert!(
                    from_object(closed.as_ref().unwrap().objects.get(&actor))
                        .sleep_and_zero_fields()
                );
                confirmed_sleep_phases.push(phase.name.as_str());
            }
            if sdk.target_valid && raw.sleep_and_zero_fields() {
                mixed_raw_phases.push(phase.name.clone());
                assert!(raw.last_target_event > raw.last_sleep_event);
            }
            if sdk.target_valid
                && memory_snapshot.as_ref().is_some_and(|state| {
                    from_object(state.objects.get(&actor)).sleep_and_zero_fields()
                })
            {
                mixed_zero_frame_phases.push(phase.name.clone());
            }
        }
        if let Some(reference) = &phase.reference {
            let xml = fs::read_to_string(directory.join(reference)).unwrap();
            assert!(xml.contains("<PxRigidDynamic"));
            assert!(!xml.contains("KinematicTarget"));
        }
        let display_exports = closed.as_ref().map(|state| snapshot_exports(state, actor));
        if let Some(state) = &closed
            && state.objects.get(&actor).is_some_and(|object| matches!(object.properties.get("RigidBodyFlags"), Some(Value::U64(flags)) if flags & 1 != 0))
        {
            let (whole, selected) = snapshot_exports(state, actor);
            for result in [whole, selected] {
                assert_eq!(result["exported"], true, "{}: {result}", phase.name);
                assert_eq!(result["kinematic_actors"], 1);
                assert_eq!(result["kinematic_target_restored"], false);
                assert_eq!(result["kinematic_motion_restored"], false);
            }
        }
        let zero_exports = memory_snapshot
            .as_ref()
            .map(|state| snapshot_exports(state, actor));
        if phase.name == "initial-buffered-pending" {
            let (whole, selected) = zero_exports.as_ref().unwrap();
            for result in [whole, selected] {
                assert_eq!(result["exported"], true, "{result}");
                assert_eq!(result["kinematic_actors"], 1);
                assert_eq!(result["kinematic_target_restored"], false);
            }
        }
        rows.push(serde_json::json!({
            "phase": phase.name, "wire_bytes": offset, "completed_steps": completed, "inside_frame": inside_frame,
            "sdk_now": phase.actor, "raw_tail": raw, "published": published, "display_frame": display_frame,
            "display": from_object(display.objects.get(&actor)), "display_exports": display_exports,
            "disk_zero_initial": zero_initial.as_ref().map(|state| from_object(state.objects.get(&actor))),
            "memory_zero_initial": memory_snapshot.as_ref().map(|state| from_object(state.objects.get(&actor))), "memory_zero_exports": zero_exports,
        }));
    }
    assert_eq!(
        idle_phases,
        [
            "first-idle-1",
            "first-idle-2",
            "second-idle-1",
            "second-idle-2"
        ]
    );
    assert_eq!(
        confirmed_sleep_phases,
        ["first-sleep-confirmed", "second-sleep-confirmed"]
    );
    let expected_raw = if initial_buffered {
        vec!["initial-buffered-pending", "buffered-pending"]
    } else {
        vec!["buffered-pending"]
    };
    assert_eq!(mixed_raw_phases, expected_raw);
    let expected_zero = if initial_buffered {
        vec!["initial-buffered-pending"]
    } else {
        Vec::new()
    };
    assert_eq!(mixed_zero_frame_phases, expected_zero);
    let recording = Recording::from_path(directory.join("capture.pvdstream")).unwrap();
    assert_eq!(recording.frame_count(), 10);
    recording.verify_replay().unwrap();
    let report = serde_json::json!({ "variant": variant, "frames": recording.frame_count(), "confirmed_sleep_phases": confirmed_sleep_phases, "mixed_raw_phases": mixed_raw_phases, "mixed_zero_frame_phases": mixed_zero_frame_phases, "rows": rows });
    if let Some(destination) = std::env::var_os("PVD_REPX_SLEEPING_REPORT_DIRECTORY") {
        let destination = PathBuf::from(destination);
        fs::create_dir_all(&destination).unwrap();
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination.join(format!("{variant}.json")))
            .unwrap();
        serde_json::to_writer_pretty(file, &report).unwrap();
    }
    eprintln!(
        "sleeping oracle {variant}: raw mixed={:?}; zero-frame mixed={:?}",
        report["mixed_raw_phases"], report["mixed_zero_frame_phases"]
    );
}

#[test]
#[ignore = "requires the independent PhysX 3.4.2 sleeping producer normal mode"]
fn sdk_sleeping_kinematic_raw_order_and_committed_display() {
    oracle("normal", false);
}

#[test]
#[ignore = "requires the independent PhysX 3.4.2 sleeping producer --initial-buffered mode"]
fn sdk_initial_buffered_target_and_zero_frame_state() {
    oracle("initial-buffered", true);
}
