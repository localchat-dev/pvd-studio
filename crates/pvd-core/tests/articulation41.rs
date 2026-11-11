use pvd_core::{Geometry, LiveRecording, Object, Recording, SceneState, Value};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};

#[derive(Deserialize)]
struct Fixture {
    sdk: String,
    scene: u64,
    destroy_frame: usize,
    initial: Articulation,
    frames: Vec<Frame>,
}

#[derive(Deserialize)]
struct Frame {
    frame: usize,
    scene_articulations: Vec<u64>,
    before_simulate: Option<Articulation>,
    after_fetch: Option<Articulation>,
}

#[derive(Deserialize)]
struct Articulation {
    id: u64,
    concrete_type: String,
    name: String,
    sleeping: bool,
    flags: u32,
    dofs: u32,
    sleep_threshold: f32,
    links: Vec<Link>,
}

#[derive(Deserialize)]
struct Link {
    id: u64,
    name: String,
    parent: u64,
    articulation: u64,
    pose: Pose,
    linear_velocity: [f32; 3],
    angular_velocity: [f32; 3],
    children: Vec<u64>,
    joint: Option<Joint>,
    shapes: Vec<Shape>,
}

#[derive(Deserialize)]
struct Joint {
    id: u64,
    concrete_type: String,
    joint_type: u32,
    twist_motion: u32,
    parent_pose: Pose,
    child_pose: Pose,
}

#[derive(Deserialize)]
struct Shape {
    id: u64,
    local_pose: Pose,
    world_pose: Pose,
    geometry_type: u32,
    dimensions: Vec<f32>,
}

#[derive(Deserialize)]
struct Pose {
    position: [f32; 3],
    rotation: [f32; 4],
}

fn input() -> (PathBuf, Fixture) {
    let capture = std::env::var_os("PVD_ARTICULATION41_CAPTURE")
        .map(PathBuf::from)
        .expect("set PVD_ARTICULATION41_CAPTURE to the same-run PhysX 4.1.2 fixture");
    let truth = fs::read(capture.with_extension("articulation.json")).unwrap();
    let fixture: Fixture = serde_json::from_slice(&truth).unwrap();
    assert_eq!(fixture.sdk, "4.1.2");
    assert_eq!(fixture.destroy_frame, 10);
    assert_eq!(fixture.frames.len(), 12);
    for (index, frame) in fixture.frames.iter().enumerate() {
        assert_eq!(frame.frame, index);
        assert_eq!(
            frame.before_simulate.is_some(),
            index < fixture.destroy_frame
        );
        assert_eq!(frame.after_fetch.is_some(), index < fixture.destroy_frame);
        if let Some(articulation) = &frame.after_fetch {
            assert_eq!(
                articulation.concrete_type,
                "PxArticulationReducedCoordinate"
            );
            assert_eq!(articulation.flags & 1, 1);
            assert_eq!(articulation.dofs, 1);
            assert_eq!(articulation.links.len(), 3);
            let joints: Vec<_> = articulation
                .links
                .iter()
                .filter_map(|link| link.joint.as_ref())
                .collect();
            assert_eq!(joints.len(), 2);
            assert_eq!((joints[0].joint_type, joints[0].twist_motion), (2, 1));
            assert_eq!((joints[1].joint_type, joints[1].twist_motion), (0, 0));
        }
    }
    let sleeping: BTreeSet<_> = fixture
        .frames
        .iter()
        .filter_map(|frame| {
            frame
                .after_fetch
                .as_ref()
                .map(|articulation| articulation.sleeping)
        })
        .collect();
    assert_eq!(sleeping, BTreeSet::from([false, true]));
    assert!(fixture.initial.sleeping);
    let before = &fixture.frames[1].after_fetch.as_ref().unwrap().links[2].pose;
    let moving = &fixture.frames[4].after_fetch.as_ref().unwrap().links[2].pose;
    assert!(
        before
            .position
            .iter()
            .zip(moving.position)
            .any(|(a, b)| (a - b).abs() > 0.00001)
    );
    (capture, fixture)
}

fn root_at_end_section(fixture: &Fixture, index: usize) -> Option<&Articulation> {
    fixture.frames[index].after_fetch.as_ref()
}

fn links_at_end_section(fixture: &Fixture, index: usize) -> Option<&Articulation> {
    // Link blocks follow EndSection; root properties also refresh during fetch synchronization.
    fixture.frames[index].before_simulate.as_ref()?;
    Some(if index == 0 {
        &fixture.initial
    } else {
        fixture.frames[index - 1].after_fetch.as_ref().unwrap()
    })
}

fn close(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert!(actual.is_finite() && expected.is_finite());
        assert!(
            (actual - expected).abs() < 0.00003,
            "{actual} != {expected}"
        );
    }
}

fn rotation(actual: &[f32; 4], expected: &[f32; 4]) {
    let equivalent = actual
        .iter()
        .zip(expected)
        .all(|(a, b)| (a - b).abs() < 0.00003)
        || actual
            .iter()
            .zip(expected)
            .all(|(a, b)| (a + b).abs() < 0.00003);
    assert!(equivalent, "{actual:?} != {expected:?}");
}

fn pose(object: &Object, property: &str, expected: &Pose) {
    let Value::Transform {
        position,
        rotation: orientation,
    } = &object.properties[property]
    else {
        panic!("missing {}.{property}", object.type_name);
    };
    close(position, &expected.position);
    rotation(orientation, &expected.rotation);
}

fn references(state: &SceneState, object: &Object, property: &str) -> BTreeSet<u64> {
    let ids = match object.properties.get(property) {
        Some(Value::Ref(0)) | None => Vec::new(),
        Some(Value::Ref(id)) => vec![*id],
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| match value {
                Value::Ref(id) if *id != 0 => *id,
                _ => panic!("invalid {}.{property} reference", object.type_name),
            })
            .collect(),
        _ => panic!("invalid {}.{property} value", object.type_name),
    };
    let mut result = BTreeSet::new();
    for id in ids {
        assert!(
            result.insert(id),
            "duplicate {}.{property} reference",
            object.type_name
        );
        assert!(
            state.resolve_reference(object.id, property, id).is_some(),
            "stale {}.{property} -> {id}",
            object.type_name
        );
    }
    result
}

fn check_wire_phases(bytes: &[u8], fixture: &Fixture) {
    use pvd_protocol::{Decoder, Endian, Event, Limits, MessageArg, Name};
    fn fields<const N: usize>(data: &[u8], arg: &MessageArg) -> [f32; N] {
        assert_eq!(arg.size as usize, N * 4);
        let start = arg.offset as usize;
        let raw = &data[start..start + N * 4];
        std::array::from_fn(|index| {
            f32::from_le_bytes(raw[index * 4..index * 4 + 4].try_into().unwrap())
        })
    }
    fn check_link_fields(
        strings: &BTreeMap<u32, String>,
        args: &[MessageArg],
        data: &[u8],
        link: &Link,
    ) {
        let mut seen = BTreeSet::new();
        for arg in args {
            let property = strings[&arg.property].as_str();
            match property {
                "GlobalPose" => {
                    assert_eq!(strings[&arg.datatype.name], "PxTransform");
                    let raw = fields::<7>(data, arg);
                    close(&raw[4..], &link.pose.position);
                    rotation(raw[..4].try_into().unwrap(), &link.pose.rotation);
                }
                "LinearVelocity" => close(&fields::<3>(data, arg), &link.linear_velocity),
                "AngularVelocity" => close(&fields::<3>(data, arg), &link.angular_velocity),
                _ => continue,
            }
            assert!(seen.insert(property));
        }
        assert_eq!(
            seen,
            BTreeSet::from(["GlobalPose", "LinearVelocity", "AngularVelocity"])
        );
    }
    let mut decoder = Decoder::new(Limits::default());
    let groups = decoder.feed(bytes).unwrap();
    decoder.finish().unwrap();
    assert_eq!(decoder.header().unwrap().endian, Endian::Little);
    let mut strings = BTreeMap::new();
    let mut messages = BTreeMap::<Name, Vec<MessageArg>>::new();
    let mut active = BTreeMap::<u64, Name>::new();
    let mut completed = 0;
    let mut open_frame = None;
    let mut root_message = None;
    let mut initial_messages = 0;
    let mut root_sync_messages = Vec::new();
    let mut initial_link_messages = BTreeSet::new();
    let mut sleep_messages = Vec::new();
    let mut link_updates = BTreeMap::<usize, BTreeSet<u64>>::new();
    let mut root_destroys = 0;
    for group in groups {
        for event in group.events {
            match event {
                Event::StringHandle { handle, value } => {
                    strings.insert(handle, value);
                }
                Event::CreatePropertyMessage { name, args, .. } => {
                    messages.insert(name, args);
                }
                Event::BeginSection { id, name, .. }
                    if id == fixture.scene
                        && strings.get(&name).map(String::as_str) == Some("frame") =>
                {
                    assert_eq!(open_frame.replace(completed), None);
                }
                Event::EndSection { id, name, .. }
                    if id == fixture.scene
                        && strings.get(&name).map(String::as_str) == Some("frame") =>
                {
                    assert_eq!(open_frame.take(), Some(completed));
                    completed += 1;
                }
                Event::SetPropertyMessage { id, message, data } if id == fixture.initial.id => {
                    if let Some(expected) = root_message {
                        assert_eq!(message, expected);
                    } else {
                        root_message = Some(message);
                    }
                    let sample = if let Some(frame) = open_frame {
                        assert_eq!(frame, completed);
                        root_sync_messages.push(frame);
                        fixture.frames[frame].after_fetch.as_ref().unwrap()
                    } else {
                        assert_eq!(
                            completed, 0,
                            "unexpected root full message outside fetch synchronization"
                        );
                        initial_messages += 1;
                        &fixture.initial
                    };
                    let arg = messages[&message]
                        .iter()
                        .find(|arg| strings[&arg.property] == "IsSleeping")
                        .unwrap();
                    assert_eq!(arg.size, 1);
                    assert_eq!(data[arg.offset as usize], u8::from(sample.sleeping));
                }
                Event::SetPropertyMessage { id, message, data }
                    if fixture.initial.links.iter().any(|link| link.id == id) =>
                {
                    assert_eq!(completed, 0);
                    assert!(
                        open_frame.is_none(),
                        "unexpected Link full message during fetch synchronization"
                    );
                    assert!(
                        initial_link_messages.insert(id),
                        "duplicate initial Link full message"
                    );
                    let link = fixture
                        .initial
                        .links
                        .iter()
                        .find(|link| link.id == id)
                        .unwrap();
                    check_link_fields(&strings, &messages[&message], &data, link);
                }
                Event::SetPropertyValue {
                    id,
                    property,
                    items,
                    data,
                    ..
                } if id == fixture.initial.id
                    && strings.get(&property).map(String::as_str) == Some("IsSleeping") =>
                {
                    assert!(open_frame.is_none());
                    assert!(completed > 0);
                    let sample = fixture.frames[completed - 1].after_fetch.as_ref().unwrap();
                    assert_eq!(items, 1);
                    assert_eq!(data.as_ref(), &[u8::from(sample.sleeping)]);
                    sleep_messages.push(completed - 1);
                }
                Event::BeginPropertyMessageGroup { message } => {
                    assert!(active.insert(group.stream_id, message).is_none());
                }
                Event::EndPropertyMessageGroup => {
                    assert!(active.remove(&group.stream_id).is_some());
                }
                Event::SendPropertyMessageFromGroup { id, data }
                    if fixture.initial.links.iter().any(|link| link.id == id) =>
                {
                    assert!(open_frame.is_none());
                    assert!(completed > 0);
                    let frame = completed - 1;
                    let sample = fixture.frames[frame].after_fetch.as_ref().unwrap();
                    let link = sample.links.iter().find(|link| link.id == id).unwrap();
                    let message = active[&group.stream_id];
                    let args = &messages[&message];
                    assert_eq!(args.len(), 3);
                    check_link_fields(&strings, args, &data, link);
                    assert!(
                        link_updates.entry(frame).or_default().insert(id),
                        "duplicate link update after EndSection {frame}"
                    );
                }
                Event::SetPropertyValue { id, property, .. }
                    if fixture.initial.links.iter().any(|link| link.id == id)
                        && matches!(
                            strings.get(&property).map(String::as_str),
                            Some("GlobalPose" | "LinearVelocity" | "AngularVelocity")
                        ) =>
                {
                    panic!("unexpected direct link state update obscures the capture phase");
                }
                Event::DestroyInstance { id } if id == fixture.initial.id => {
                    assert!(open_frame.is_none());
                    assert_eq!(completed, fixture.destroy_frame);
                    root_destroys += 1;
                }
                _ => {}
            }
        }
    }
    assert!(active.is_empty());
    assert!(open_frame.is_none());
    assert_eq!(completed, fixture.frames.len());
    assert_eq!(initial_messages, 1);
    assert_eq!(
        root_sync_messages,
        (0..fixture.destroy_frame).collect::<Vec<_>>()
    );
    assert_eq!(
        initial_link_messages,
        fixture.initial.links.iter().map(|link| link.id).collect()
    );
    assert_eq!(
        sleep_messages,
        (0..fixture.destroy_frame).collect::<Vec<_>>()
    );
    assert_eq!(root_destroys, 1);
    let mut was_sleeping = false;
    for (frame, sample) in fixture
        .frames
        .iter()
        .enumerate()
        .take(fixture.destroy_frame)
    {
        let sample = sample.after_fetch.as_ref().unwrap();
        let expected = if !sample.sleeping || sample.sleeping != was_sleeping {
            3
        } else {
            0
        };
        assert_eq!(
            link_updates.get(&frame).map_or(0, BTreeSet::len),
            expected,
            "SDK update suppression after EndSection {frame}"
        );
        was_sleeping = sample.sleeping;
    }
}

fn check_state(recording: &Recording, state: &SceneState, fixture: &Fixture, index: usize) {
    let expected = &fixture.frames[index];
    assert!(state.errors.is_empty(), "SDK error at frame {index}");
    assert_eq!(recording.frame_info(index).unwrap().scene_id, fixture.scene);
    let scene = &state.objects[&fixture.scene];
    assert_eq!(
        references(state, scene, "Articulations"),
        expected.scene_articulations.iter().copied().collect()
    );
    let rendered = state.render_scene();
    assert!(
        rendered.diagnostics.is_empty(),
        "{:?}",
        rendered.diagnostics
    );
    let Some(articulation) = root_at_end_section(fixture, index) else {
        assert!(rendered.objects.is_empty());
        let initial = &fixture.initial;
        let mut destroyed = vec![initial.id];
        for link in &initial.links {
            destroyed.push(link.id);
            destroyed.extend(link.shapes.iter().map(|shape| shape.id));
            destroyed.extend(link.joint.iter().map(|joint| joint.id));
        }
        for id in destroyed {
            assert!(
                !state.objects.contains_key(&id),
                "destroyed SDK instance {id} remains at frame {index}"
            );
        }
        return;
    };
    let root = &state.objects[&articulation.id];
    assert_eq!(root.type_name, "physx3::PxArticulationBase");
    assert_eq!(
        root.properties["Name"],
        Value::String(articulation.name.clone())
    );
    assert_eq!(
        root.properties["IsSleeping"],
        Value::Bool(articulation.sleeping)
    );
    assert_eq!(
        references(state, root, "Scene"),
        BTreeSet::from([fixture.scene])
    );
    assert_eq!(
        root.properties["SleepThreshold"],
        Value::F32(articulation.sleep_threshold)
    );
    for property in [
        "ConcreteTypeName",
        "ArticulationFlags",
        "Dofs",
        "CacheDataSize",
        "CoefficientMatrixSize",
    ] {
        assert!(
            !root.properties.contains_key(property),
            "uncaptured RC field {property}"
        );
        assert!(
            !recording.classes[&root.type_name]
                .properties
                .contains_key(property)
        );
    }
    let roots: BTreeSet<_> = articulation
        .links
        .iter()
        .filter(|link| link.parent == articulation.id)
        .map(|link| link.id)
        .collect();
    assert_eq!(references(state, root, "Links"), roots);
    assert_eq!(rendered.objects.len(), 3);
    let link_sample = links_at_end_section(fixture, index).unwrap();
    for link in &link_sample.links {
        assert_eq!(link.articulation, articulation.id);
        let object = &state.objects[&link.id];
        assert_eq!(object.type_name, "physx3::PxArticulationLink");
        assert_eq!(object.properties["Name"], Value::String(link.name.clone()));
        assert!(!object.properties.contains_key("IsSleeping"));
        assert_eq!(
            references(state, object, "Parent"),
            BTreeSet::from([link.parent])
        );
        assert_eq!(
            references(state, object, "Links"),
            link.children.iter().copied().collect()
        );
        assert_eq!(
            references(state, object, "Shapes"),
            link.shapes.iter().map(|shape| shape.id).collect()
        );
        pose(object, "GlobalPose", &link.pose);
        for (property, expected) in [
            ("LinearVelocity", &link.linear_velocity),
            ("AngularVelocity", &link.angular_velocity),
        ] {
            let Value::Vec3(actual) = &object.properties[property] else {
                panic!("missing {property}")
            };
            close(actual, expected);
        }
        let joint_ids = references(state, object, "InboundJoint");
        if let Some(joint) = &link.joint {
            assert_eq!(joint.concrete_type, "PxArticulationJointReducedCoordinate");
            assert_eq!(joint_ids, BTreeSet::from([joint.id]));
            let recorded = &state.objects[&joint.id];
            assert_eq!(recorded.type_name, "physx3::PxArticulationJointBase");
            assert_eq!(
                references(state, recorded, "Link"),
                BTreeSet::from([link.id])
            );
            pose(recorded, "ParentPose", &joint.parent_pose);
            pose(recorded, "ChildPose", &joint.child_pose);
            for property in [
                "ConcreteTypeName",
                "JointType",
                "Motion",
                "Drive",
                "FrictionCoefficient",
                "MaxJointVelocity",
            ] {
                assert!(
                    !recorded.properties.contains_key(property),
                    "uncaptured RC joint field {property}"
                );
                assert!(
                    !recording.classes[&recorded.type_name]
                        .properties
                        .contains_key(property)
                );
            }
        } else {
            assert!(joint_ids.is_empty());
        }
        for shape in &link.shapes {
            let object = &state.objects[&shape.id];
            pose(object, "LocalPose", &shape.local_pose);
            let displayed = rendered
                .objects
                .iter()
                .find(|displayed| displayed.id == link.id && displayed.shape_id == shape.id)
                .unwrap();
            assert!(displayed.dynamic);
            close(&displayed.position, &shape.world_pose.position);
            rotation(&displayed.rotation, &shape.world_pose.rotation);
            match &displayed.geometry {
                Geometry::Box { half_extents } => {
                    assert_eq!(shape.geometry_type, 3);
                    close(half_extents, &shape.dimensions);
                }
                Geometry::Sphere { radius } => {
                    assert_eq!(shape.geometry_type, 0);
                    close(&[*radius], &shape.dimensions);
                }
                Geometry::Capsule {
                    radius,
                    half_height,
                } => {
                    assert_eq!(shape.geometry_type, 2);
                    close(&[*radius, *half_height], &shape.dimensions);
                }
                other => panic!("unexpected SDK geometry {other:?}"),
            }
        }
    }
}

#[test]
#[ignore = "requires tools/physx41-articulation capture and same-run SDK getter truth"]
fn sdk41_reduced_coordinate_capture_matches_getters_and_replay() {
    let (capture, fixture) = input();
    let bytes = fs::read(&capture).unwrap();
    check_wire_phases(&bytes, &fixture);
    let recording = Recording::from_path(&capture).unwrap();
    assert_eq!(recording.frame_count(), fixture.frames.len());
    for class in [
        "physx3::PxArticulationReducedCoordinate",
        "physx3::PxArticulationJointReducedCoordinate",
    ] {
        assert!(
            !recording.classes.contains_key(class),
            "unregistered concrete class {class}"
        );
    }
    for index in (0..fixture.frames.len())
        .chain((0..fixture.frames.len()).rev())
        .chain([11, 0, 9, 2, 6, 10, 4, 1, 8, 5])
    {
        check_state(
            &recording,
            &recording.state_at(index).unwrap(),
            &fixture,
            index,
        );
    }
    recording.verify_replay().unwrap();
    let mut live = LiveRecording::default();
    for chunk in bytes.chunks(997) {
        live.feed(chunk).unwrap();
    }
    let streamed = live.finish().unwrap();
    assert_eq!(streamed.frame_count(), fixture.frames.len());
    for index in [11, 0, 9, 2, 6, 10, 4, 1, 8, 5] {
        assert_eq!(
            streamed.state_at(index).unwrap(),
            recording.state_at(index).unwrap(),
            "chunked input frame {index}"
        );
    }
}

#[test]
#[ignore = "requires tools/physx41-articulation capture and same-run SDK getter truth"]
fn sdk41_captured_base_owns_links_and_drives_sleep_display() {
    let (capture, fixture) = input();
    let recording = Recording::from_path(capture).unwrap();
    for frame in &fixture.frames {
        let state = recording.state_at(frame.frame).unwrap();
        let Some(expected) = root_at_end_section(&fixture, frame.frame) else {
            continue;
        };
        let rendered = state.render_scene();
        for link in &expected.links {
            let owner = state
                .articulation_for_link(link.id)
                .expect("captured PxArticulationBase must own its links");
            assert_eq!(owner.id, expected.id);
            assert_eq!(owner.type_name, "physx3::PxArticulationBase");
            for shape in &link.shapes {
                let displayed = rendered
                    .objects
                    .iter()
                    .find(|object| object.id == link.id && object.shape_id == shape.id)
                    .unwrap();
                assert_eq!(
                    displayed.sleeping, expected.sleeping,
                    "frame {}, SDK link {}",
                    frame.frame, link.id
                );
            }
        }
        assert!(state.articulation_for_link(expected.id).is_none());
    }
}
