use pvd_core::{
    Recording, SceneState, Value,
    debug_draw::Primitive,
    frame_sources::{FrameObject, FrameSection, FrameSourceKey, SourceFrame},
    live::ReplayCursor,
};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

const SDK_COMMIT: &str = "5e42a5f112351a223c19c17bb331e6c55037b8eb";
const RENDERER: &str = "_debugger_::PvdUserRenderer";
const ACTOR: &str = "Apex::ClothingActor";
const ASSET: &str = "Apex::ClothingAsset";
const PARAMS: &str = "Apex::ClothingActorParam";

#[derive(Deserialize)]
struct Truth {
    sdk: String,
    apex_commit: String,
    instrumentation: String,
    async_fetch: bool,
    #[serde(default = "one_cycle")]
    cycles: usize,
    physx_scene_id: u64,
    apex_scene_api_id: u64,
    asset_params_id: u64,
    physical_vertices: usize,
    physical_indices: Vec<usize>,
    steps: Vec<Step>,
    before_apex_release_bytes: usize,
    after_apex_release_bytes: usize,
}

#[derive(Deserialize)]
struct Step {
    step: usize,
    before_simulate_bytes: usize,
    after_simulate_bytes: usize,
    after_fetch_bytes: usize,
    after_getters_bytes: usize,
    actors: Vec<ActorTruth>,
}

#[derive(Deserialize)]
struct ActorTruth {
    serial: usize,
    api_id: u64,
    actor_params_id: u64,
    local_space: bool,
    simulation_vertices: usize,
    global_pose: [f32; 16],
    render_pose: [f32; 16],
    simulation_positions: Vec<[f32; 3]>,
    world_positions: Vec<[f32; 3]>,
}

#[derive(Deserialize)]
struct Probe {
    event: usize,
    group_offset: usize,
    group_timestamp: u64,
    stream: u64,
    ordinal_in_group: usize,
    kind: String,
    #[serde(default)]
    id: u64,
    #[serde(default)]
    class: String,
    #[serde(default)]
    property: String,
    #[serde(default)]
    target: u64,
    #[serde(default)]
    source: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    timestamp: u64,
    #[serde(default)]
    payload: String,
    #[serde(default)]
    bytes: usize,
    #[serde(default)]
    items: usize,
}

#[derive(Clone, Debug)]
struct Identity {
    generation: u32,
    class: String,
}

#[derive(Clone)]
struct Boundary {
    event: usize,
    offset_end: usize,
    key: FrameSourceKey,
    frame: SourceFrame,
    timestamp: u64,
    objects: BTreeMap<u64, Identity>,
    references: BTreeMap<(u64, String), u64>,
    poses: BTreeMap<u64, [f32; 16]>,
    members: BTreeMap<u64, BTreeSet<u64>>,
    draws: Vec<usize>,
}

struct Draw {
    event: usize,
    owner: FrameObject,
    subject: FrameObject,
    step: usize,
    actor: usize,
}

struct Oracle {
    boundaries: Vec<Boundary>,
    draws: Vec<Draw>,
    final_objects: BTreeMap<u64, Identity>,
    renderer_bytes: usize,
}

struct Bytes<'a>(&'a [u8]);

impl<'a> Bytes<'a> {
    fn take(&mut self, count: usize) -> &'a [u8] {
        let (head, tail) = self.0.split_at(count);
        self.0 = tail;
        head
    }

    fn u8(&mut self) -> u8 {
        self.take(1)[0]
    }

    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take(4).try_into().unwrap())
    }

    fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.take(8).try_into().unwrap())
    }

    fn finish(self) {
        assert!(self.0.is_empty(), "unsupported fixture event layout");
    }
}

fn near(actual: f32, expected: f32) {
    assert!(
        actual.is_finite() && expected.is_finite() && (actual - expected).abs() <= 0.0001,
        "actual {actual}, SDK getter {expected}"
    );
}

fn one_cycle() -> usize {
    1
}

fn check_truth(truth: &Truth, bytes: usize) {
    assert_eq!(truth.sdk, "3.4.2");
    assert_eq!(truth.apex_commit, SDK_COMMIT);
    assert_eq!(truth.instrumentation, "DEBUG");
    assert_eq!(truth.physical_vertices, 1024);
    assert_eq!(truth.physical_indices.len(), 5766);
    assert!(truth.physical_indices.iter().all(|i| *i < 1024));
    assert!((1..=32).contains(&truth.cycles));
    assert_eq!(truth.steps.len(), 36 * truth.cycles);
    assert!(truth.before_apex_release_bytes < truth.after_apex_release_bytes);
    assert!(truth.after_apex_release_bytes <= bytes);
    let identity: [f32; 16] = std::array::from_fn(|i| if i % 5 == 0 { 1.0 } else { 0.0 });
    let mut observations = 0;
    for (index, step) in truth.steps.iter().enumerate() {
        assert_eq!(step.step, index);
        assert!(step.before_simulate_bytes < step.after_simulate_bytes);
        assert!(step.after_simulate_bytes <= step.after_fetch_bytes);
        assert!(step.after_fetch_bytes <= step.after_getters_bytes);
        assert!(step.after_getters_bytes <= truth.before_apex_release_bytes);
        if let Some(previous) = index.checked_sub(1) {
            assert!(truth.steps[previous].after_getters_bytes <= step.before_simulate_bytes);
        }
        let expected_serials: BTreeSet<_> = [(0, 0..16), (1, 4..30), (2, 20..36)]
            .into_iter()
            .filter_map(|(serial, range)| {
                range
                    .contains(&(index % 36))
                    .then_some(index / 36 * 3 + serial)
            })
            .collect();
        assert_eq!(
            step.actors
                .iter()
                .map(|actor| actor.serial)
                .collect::<BTreeSet<_>>(),
            expected_serials
        );
        for actor in &step.actors {
            observations += 1;
            assert_ne!(actor.api_id, 0);
            assert_ne!(actor.actor_params_id, 0);
            assert_eq!(actor.local_space, actor.serial % 3 == 1);
            assert_eq!(actor.simulation_vertices, truth.physical_vertices);
            assert_eq!(actor.simulation_positions.len(), truth.physical_vertices);
            assert_eq!(actor.world_positions.len(), truth.physical_vertices);
            let expected_pose = if actor.local_space {
                actor.global_pose
            } else {
                identity
            };
            for (actual, expected) in actor.render_pose.into_iter().zip(expected_pose) {
                near(actual, expected);
            }
            for (local, world) in actor
                .simulation_positions
                .iter()
                .zip(&actor.world_positions)
            {
                for (axis, expected) in world.iter().enumerate() {
                    let transformed = actor.render_pose[axis] * local[0]
                        + actor.render_pose[4 + axis] * local[1]
                        + actor.render_pose[8 + axis] * local[2]
                        + actor.render_pose[12 + axis];
                    near(transformed, *expected);
                }
            }
        }
    }
    assert_eq!(observations, 58 * truth.cycles);
    for step in (0..truth.cycles).flat_map(|cycle| [cycle * 36 + 8, cycle * 36 + 24]) {
        for actor in &truth.steps[step].actors {
            let previous = truth.steps[step - 1]
                .actors
                .iter()
                .find(|previous| previous.serial == actor.serial)
                .unwrap();
            for (axis, shift) in [3.0, 2.0, -1.0].into_iter().enumerate() {
                near(
                    actor.global_pose[12 + axis] - previous.global_pose[12 + axis],
                    shift,
                );
            }
        }
    }
}

fn source_keys(truth: &Truth) -> [FrameSourceKey; 2] {
    [
        FrameSourceKey::Captured {
            section: FrameSection::PhysX,
            source: truth.physx_scene_id,
        },
        FrameSourceKey::Captured {
            section: FrameSection::Apex,
            source: truth.apex_scene_api_id,
        },
    ]
}

fn read_oracle(directory: &Path, data: &[u8], truth: &Truth) -> Oracle {
    let probes: BTreeMap<usize, Probe> =
        fs::read_to_string(directory.join("event-probe/events.jsonl"))
            .unwrap()
            .lines()
            .map(|line| {
                let row: Probe = serde_json::from_str(line).unwrap();
                (row.event, row)
            })
            .collect();
    let mut header = Bytes(&data[..28]);
    assert_eq!(header.u32(), 0x31e5e882);
    assert_eq!(header.u32(), 1);
    assert_ne!(header.u64(), 0);
    assert_ne!(header.u64(), 0);
    assert_eq!(header.u32(), 1);
    header.finish();
    let mut strings = BTreeMap::<u32, String>::new();
    let mut objects = BTreeMap::<u64, Identity>::new();
    let mut generations = BTreeMap::<u64, u32>::new();
    let mut references = BTreeMap::<(u64, String), u64>::new();
    let mut poses = BTreeMap::<u64, [f32; 16]>::new();
    let mut members = BTreeMap::<u64, BTreeSet<u64>>::new();
    let mut previous_ends = BTreeMap::<FrameSourceKey, usize>::new();
    let mut ordinals = BTreeMap::<FrameSourceKey, usize>::new();
    let mut open_sections = BTreeSet::<FrameSourceKey>::new();
    let mut boundaries = Vec::new();
    let mut draws = Vec::<Draw>::new();
    let mut draw_observations = BTreeSet::new();
    let mut actor_generations = BTreeMap::new();
    let mut probe_count = 0;
    let mut actor_ref_count = 0;
    let mut asset_ref_count = 0;
    let mut list_counts = [0, 0];
    let mut renderer_flushes = 0;
    let mut renderer_bytes = 0usize;
    let mut offset = 28;
    let mut event = 0;
    while offset < data.len() {
        let mut group = Bytes(&data[offset..]);
        let size = group.u32() as usize;
        assert_eq!(
            group.u32(),
            1,
            "oracle requires single-event fixture groups"
        );
        let stream = group.u64();
        let timestamp = group.u64();
        let offset_end = offset + 24 + size;
        let mut input = Bytes(group.take(size));
        let tag = input.u8();
        let probe = probes.get(&event);
        if let Some(row) = probe {
            probe_count += 1;
            assert_eq!(row.group_offset, offset);
            assert_eq!(row.ordinal_in_group, 0);
            assert_eq!(row.stream, stream);
            assert_eq!(row.group_timestamp, timestamp);
        }
        match tag {
            1 => {
                let length = input.u32() as usize;
                let text = input.take(length);
                assert_eq!(text.last(), Some(&0));
                let text = std::str::from_utf8(&text[..length - 1]).unwrap().to_owned();
                let handle = input.u32();
                if let Some(previous) = strings.insert(handle, text.clone()) {
                    assert_eq!(previous, text);
                }
                input.finish();
            }
            6 => {
                let namespace = input.u32();
                let name = input.u32();
                let id = input.u64();
                input.finish();
                let class = format!("{}::{}", strings[&namespace], strings[&name]);
                let row = probe.unwrap();
                assert_eq!(row.kind, "create");
                assert_eq!(row.id, id);
                assert_eq!(row.class, class);
                let generation = *generations.entry(id).and_modify(|g| *g += 1).or_insert(0);
                assert!(objects.insert(id, Identity { generation, class }).is_none());
            }
            15 => {
                let id = input.u64();
                input.finish();
                let object = objects.remove(&id).unwrap();
                let row = probe.unwrap();
                assert_eq!(row.kind, "destroy");
                assert_eq!(row.id, id);
                assert_eq!(row.class, object.class);
                references.retain(|(owner, _), _| *owner != id);
                poses.remove(&id);
                members.remove(&id);
            }
            7 => {
                let id = input.u64();
                let property = strings[&input.u32()].clone();
                let namespace = input.u32();
                let name = input.u32();
                let datatype = format!("{}::{}", strings[&namespace], strings[&name]);
                let items = input.u32() as usize;
                let length = input.u32() as usize;
                let value = input.take(length);
                input.finish();
                let object = &objects[&id];
                if property == "ActorParams" || property == "AssetParams" {
                    assert_eq!(datatype, "physx3::ObjectRef");
                    assert_eq!((items, length), (1, 8));
                    let target = u64::from_le_bytes(value.try_into().unwrap());
                    let row = probe.unwrap();
                    assert_eq!(row.kind, "reference");
                    assert_eq!((row.id, row.target), (id, target));
                    assert_eq!(row.property, property);
                    if property == "ActorParams" {
                        assert_eq!(object.class, ACTOR);
                        assert_eq!(objects[&target].class, PARAMS);
                        actor_ref_count += 1;
                    } else {
                        assert_eq!(object.class, ASSET);
                        assert_eq!(target, truth.asset_params_id);
                        asset_ref_count += 1;
                    }
                    references.insert((id, property), target);
                } else if object.class == PARAMS && property == "globalPose" {
                    assert_eq!(datatype, "physx3::PxMat44");
                    assert_eq!((items, length), (1, 64));
                    let getter = truth
                        .steps
                        .iter()
                        .filter(|step| step.after_getters_bytes > offset)
                        .flat_map(|step| &step.actors)
                        .find(|actor| actor.actor_params_id == id)
                        .expect("pose setter has a following getter in the same actor lifetime");
                    for (raw, expected) in value.chunks_exact(4).zip(getter.global_pose) {
                        near(f32::from_le_bytes(raw.try_into().unwrap()), expected);
                    }
                    poses.insert(id, getter.global_pose);
                } else if object.class == RENDERER && property == "events" {
                    renderer_flushes += 1;
                    renderer_bytes = renderer_bytes.checked_add(value.len()).unwrap();
                    let row = probe.unwrap();
                    assert_eq!(row.kind, "renderer_flush");
                    assert_eq!(row.id, id);
                    assert_eq!((row.bytes, row.items), (length, items));
                    assert_eq!(row.payload, format!("renderer-{event}.bin"));
                    assert_eq!(
                        fs::read(directory.join("event-probe").join(&row.payload)).unwrap(),
                        value
                    );
                    if !value.is_empty() {
                        let mut packet = Bytes(value);
                        assert_eq!(packet.u8(), 1);
                        let subject = packet.u64();
                        assert_eq!(packet.u8(), 4);
                        assert_eq!(packet.u32() as usize * 3, truth.physical_indices.len());
                        assert_eq!(packet.0.len(), truth.physical_indices.len() * 16);
                        for vertex in packet.0.chunks_exact(16) {
                            assert_eq!(u32::from_le_bytes(vertex[12..].try_into().unwrap()), 27);
                        }
                        let step = truth
                            .steps
                            .iter()
                            .position(|step| {
                                step.before_simulate_bytes <= offset
                                    && offset < step.after_getters_bytes
                            })
                            .expect("draw belongs to one SDK step");
                        let params = references[&(subject, "ActorParams".into())];
                        let actor = truth.steps[step]
                            .actors
                            .iter()
                            .position(|actor| actor.actor_params_id == params)
                            .expect("captured ActorParams identifies an independent getter");
                        assert_eq!(objects[&subject].class, ACTOR);
                        let subject = FrameObject {
                            id: subject,
                            generation: objects[&subject].generation,
                        };
                        let getter = &truth.steps[step].actors[actor];
                        if let Some(previous) = actor_generations.insert(getter.serial, subject) {
                            assert_eq!(previous, subject);
                        }
                        assert!(draw_observations.insert((step, getter.serial)));
                        draws.push(Draw {
                            event,
                            owner: FrameObject {
                                id,
                                generation: object.generation,
                            },
                            subject,
                            step,
                            actor,
                        });
                    }
                }
            }
            16 | 17 => {
                let id = input.u64();
                let property = &strings[&input.u32()];
                let target = input.u64();
                input.finish();
                if property == "ClothingActors" {
                    assert_eq!(objects[&id].class, ASSET);
                    assert_eq!(objects[&target].class, ACTOR);
                    assert_eq!(
                        references.get(&(id, "AssetParams".into())),
                        Some(&truth.asset_params_id)
                    );
                    let list = members.entry(id).or_default();
                    if tag == 16 {
                        assert!(list.insert(target));
                        list_counts[0] += 1;
                    } else {
                        assert!(list.remove(&target));
                        list_counts[1] += 1;
                    }
                }
            }
            18 | 19 => {
                let source = input.u64();
                let name = &strings[&input.u32()];
                let timestamp = input.u64();
                input.finish();
                let section = match name.as_str() {
                    "frame" => FrameSection::PhysX,
                    "ApexFrame" => FrameSection::Apex,
                    _ => panic!("unexpected fixture section {name}"),
                };
                let key = FrameSourceKey::Captured { section, source };
                assert!(source_keys(truth).contains(&key));
                let row = probe.unwrap();
                assert_eq!(
                    row.kind,
                    if tag == 18 {
                        "section_begin"
                    } else {
                        "section_end"
                    }
                );
                assert_eq!((row.source, row.timestamp), (source, timestamp));
                assert_eq!(row.name, *name);
                if tag == 18 {
                    assert!(open_sections.insert(key));
                } else {
                    assert!(open_sections.remove(&key));
                    let source_object = FrameObject {
                        id: source,
                        generation: objects[&source].generation,
                    };
                    let ordinal = ordinals.entry(key).or_default();
                    let previous = previous_ends.insert(key, event);
                    let current_draws = draws
                        .iter()
                        .enumerate()
                        .filter_map(|(index, draw)| {
                            (previous.is_none_or(|start| start < draw.event)
                                && objects
                                    .get(&draw.owner.id)
                                    .is_some_and(|owner| owner.generation == draw.owner.generation))
                            .then_some(index)
                        })
                        .collect();
                    boundaries.push(Boundary {
                        event,
                        offset_end,
                        key,
                        timestamp,
                        frame: SourceFrame {
                            index: boundaries.len(),
                            ordinal: *ordinal,
                            source_object: Some(source_object),
                            scene: (section == FrameSection::PhysX).then_some(source_object),
                        },
                        objects: objects.clone(),
                        references: references.clone(),
                        poses: poses.clone(),
                        members: members.clone(),
                        draws: current_draws,
                    });
                    *ordinal += 1;
                }
            }
            2..=5 | 8..=14 | 20..=22 => {}
            _ => panic!("unsupported fixture event tag {tag}"),
        }
        offset = offset_end;
        event += 1;
    }
    assert_eq!(offset, data.len());
    assert_eq!(probe_count, probes.len());
    assert!(open_sections.is_empty());
    assert_eq!(boundaries.len(), 72 * truth.cycles);
    assert_eq!(draws.len(), 58 * truth.cycles);
    assert_eq!(renderer_flushes, 130 * truth.cycles);
    assert_eq!((actor_ref_count, asset_ref_count), (3 * truth.cycles, 1));
    assert_eq!(list_counts, [3 * truth.cycles; 2]);
    assert_eq!(actor_generations.len(), 3 * truth.cycles);
    assert_eq!(
        actor_generations
            .values()
            .map(|actor| (actor.id, actor.generation))
            .collect::<BTreeSet<_>>()
            .len(),
        3 * truth.cycles
    );
    assert_ne!(actor_generations[&0], actor_generations[&2]);
    if actor_generations[&0].id == actor_generations[&2].id {
        assert_eq!(
            actor_generations[&2].generation,
            actor_generations[&0].generation + 1
        );
    }
    for step in &truth.steps {
        for actor in &step.actors {
            assert!(draw_observations.contains(&(step.step, actor.serial)));
        }
    }
    for key in source_keys(truth) {
        assert_eq!(ordinals[&key], truth.steps.len());
        let count: usize = boundaries
            .iter()
            .filter(|b| b.key == key)
            .map(|b| b.draws.len())
            .sum();
        let expected = if !truth.async_fetch && key == source_keys(truth)[1] {
            58 * truth.cycles
        } else {
            58 * truth.cycles - 1
        };
        assert_eq!(count, expected, "completed source windows {key:?}");
        let last = boundaries
            .iter()
            .rev()
            .find(|boundary| boundary.key == key)
            .unwrap();
        let tail: Vec<_> = draws
            .iter()
            .enumerate()
            .filter(|(_, draw)| draw.event > last.event)
            .collect();
        assert_eq!(tail.len(), 58 * truth.cycles - expected);
        for (index, draw) in tail {
            assert_eq!(draw.step, truth.steps.len() - 1);
            assert!(!last.draws.contains(&index));
        }
    }
    let last = boundaries.last().unwrap();
    assert_eq!(
        last.objects
            .values()
            .filter(|object| object.class == RENDERER)
            .count(),
        2
    );
    assert!(
        objects
            .values()
            .all(|object| object.class != RENDERER && object.class != ACTOR)
    );
    Oracle {
        boundaries,
        draws,
        final_objects: objects,
        renderer_bytes,
    }
}

fn check_state(state: &SceneState, boundary: &Boundary, oracle: &Oracle, truth: &Truth) {
    assert_eq!(
        state.objects.keys().collect::<Vec<_>>(),
        boundary.objects.keys().collect::<Vec<_>>(),
        "object lifecycle at global frame {}",
        boundary.frame.index
    );
    for (id, expected) in &boundary.objects {
        let actual = &state.objects[id];
        assert_eq!(actual.generation, expected.generation, "generation {id:#x}");
        assert_eq!(actual.type_name, expected.class, "class {id:#x}");
    }
    for ((id, property), target) in &boundary.references {
        assert_eq!(state.objects[id].properties[property], Value::Ref(*target));
        let resolved = state.resolve_reference(*id, property, *target).unwrap();
        assert_eq!(resolved.generation, boundary.objects[target].generation);
    }
    for (id, expected) in &boundary.poses {
        let Value::Array(actual) = &state.objects[id].properties["globalPose"] else {
            panic!("ClothingActorParam.globalPose must retain its matrix");
        };
        assert_eq!(actual.len(), 16);
        for (actual, expected) in actual.iter().zip(expected) {
            let Value::F32(actual) = actual else {
                panic!("matrix component is not f32")
            };
            near(*actual, *expected);
        }
    }
    for (asset, expected) in &boundary.members {
        let Value::Array(actual) = &state.objects[asset].properties["ClothingActors"] else {
            panic!("shared ClothingAsset actor list is missing");
        };
        let actual: BTreeSet<_> = actual
            .iter()
            .map(|value| match value {
                Value::Ref(id) => *id,
                _ => panic!("ClothingActors element is not a reference"),
            })
            .collect();
        assert_eq!(&actual, expected);
        for actor in expected {
            let object = state
                .resolve_reference(*asset, "ClothingActors", *actor)
                .unwrap();
            assert_eq!(object.generation, boundary.objects[actor].generation);
        }
    }
    let mut expected_draws = BTreeMap::<u64, Vec<&Draw>>::new();
    for index in &boundary.draws {
        let draw = &oracle.draws[*index];
        expected_draws.entry(draw.owner.id).or_default().push(draw);
    }
    let actual_owners: BTreeSet<_> = state
        .debug_draw
        .iter()
        .filter_map(|(owner, primitives)| (!primitives.is_empty()).then_some(*owner))
        .collect();
    assert_eq!(
        actual_owners,
        expected_draws.keys().copied().collect(),
        "renderer window at global frame {}",
        boundary.frame.index
    );
    let triangles_per_draw = truth.physical_indices.len() / 3;
    for (owner, draws) in expected_draws {
        let actual = &state.debug_draw[&owner];
        assert_eq!(
            actual.len(),
            draws.len() * triangles_per_draw,
            "triangle count at global frame {}",
            boundary.frame.index
        );
        for (draw, triangles) in draws
            .into_iter()
            .zip(actual.chunks_exact(triangles_per_draw))
        {
            let getter = &truth.steps[draw.step].actors[draw.actor];
            for (triangle, indices) in triangles.iter().zip(truth.physical_indices.chunks_exact(3))
            {
                let Primitive::Triangle {
                    instance,
                    positions,
                    colors,
                } = triangle
                else {
                    panic!("unexpected Clothing renderer primitive");
                };
                assert_eq!(*instance, draw.subject.id);
                assert_eq!(*colors, [27; 3]);
                for (position, vertex) in positions.iter().zip(indices) {
                    for (actual, expected) in position.iter().zip(getter.world_positions[*vertex]) {
                        near(*actual, expected);
                    }
                }
            }
        }
    }
}

fn check_directory(recording: &Recording, oracle: &Oracle, truth: &Truth) {
    assert_eq!(recording.first_frame(), 0);
    assert_eq!(recording.frame_count(), 2 * truth.steps.len());
    assert_eq!(recording.frame_sources.sources().count(), 2);
    assert_eq!(
        recording.frame_sources.default_source(),
        Some(source_keys(truth)[0])
    );
    for key in source_keys(truth) {
        let source = recording.frame_sources.source(key).unwrap();
        assert_eq!(
            (source.observed_frames(), source.len()),
            (truth.steps.len(), truth.steps.len())
        );
        for boundary in oracle
            .boundaries
            .iter()
            .filter(|boundary| boundary.key == key)
        {
            assert_eq!(
                source.at_ordinal(boundary.frame.ordinal),
                Some(&boundary.frame)
            );
            assert_eq!(source.at_index(boundary.frame.index), Some(&boundary.frame));
            let actual = recording.frame_info(boundary.frame.index).unwrap();
            assert_eq!(actual.index, boundary.frame.index);
            assert_eq!(actual.timestamp, boundary.timestamp);
            let FrameSourceKey::Captured { source, .. } = key else {
                unreachable!()
            };
            assert_eq!(actual.scene_id, source);
        }
    }
    for class in [ACTOR, PARAMS, ASSET, "Apex::scene", RENDERER] {
        assert!(
            recording.classes.contains_key(class),
            "captured class {class}"
        );
    }
    assert_eq!(
        recording.classes[PARAMS].properties["globalPose"].datatype,
        "physx3::PxMat44"
    );
    assert_eq!(
        recording.classes[ACTOR].properties["ActorParams"].datatype,
        "physx3::ObjectRef"
    );
    assert!(recording.classes[ASSET].properties["ClothingActors"].array);
    assert!(
        !recording
            .diagnostics
            .iter()
            .any(|message| message.starts_with("Analysis decoding stopped"))
    );
}

fn check_replay(recording: &Recording, oracle: &Oracle, truth: &Truth) {
    check_directory(recording, oracle, truth);
    let mut cursor = ReplayCursor::default();
    let mut state = SceneState::default();
    for key in source_keys(truth) {
        let frames: Vec<_> = oracle.boundaries.iter().filter(|b| b.key == key).collect();
        let count = truth.steps.len();
        for ordinal in (0..count)
            .chain((0..count).rev())
            .chain((0..count).map(|i| (i * 13 + 7) % count))
        {
            let expected = frames[ordinal];
            if recording.is_disk_backed() {
                cursor
                    .seek(recording, expected.frame.index, || false)
                    .unwrap()
                    .update
                    .apply_owned(&mut state);
            } else {
                state = recording.state_at(expected.frame.index).unwrap();
            }
            check_state(&state, expected, oracle, truth);
        }
    }
    for expected in &oracle.boundaries {
        check_state(
            &recording.state_at(expected.frame.index).unwrap(),
            expected,
            oracle,
            truth,
        );
    }
}

pub(crate) struct Fixture {
    truth: Truth,
    oracle: Oracle,
}

impl Fixture {
    pub(crate) fn load(directory: &Path, bytes: &[u8]) -> Self {
        let truth =
            serde_json::from_slice(&fs::read(directory.join("truth.json")).unwrap()).unwrap();
        check_truth(&truth, bytes.len());
        let oracle = read_oracle(directory, bytes, &truth);
        Self { truth, oracle }
    }

    pub(crate) fn sources(&self) -> [FrameSourceKey; 2] {
        source_keys(&self.truth)
    }

    pub(crate) fn frames(
        &self,
    ) -> impl DoubleEndedIterator<Item = (FrameSourceKey, SourceFrame, usize)> + '_ {
        self.oracle
            .boundaries
            .iter()
            .map(|boundary| (boundary.key, boundary.frame, boundary.offset_end))
    }

    pub(crate) fn async_fetch(&self) -> bool {
        self.truth.async_fetch
    }

    pub(crate) fn final_object_count(&self) -> usize {
        self.oracle.final_objects.len()
    }

    pub(crate) fn check_state(&self, index: usize, state: &SceneState) {
        check_state(
            state,
            &self.oracle.boundaries[index],
            &self.oracle,
            &self.truth,
        );
    }

    pub(crate) fn check_directory(&self, recording: &Recording) {
        check_directory(recording, &self.oracle, &self.truth);
    }

    pub(crate) fn check_recording(&self, recording: &Recording) {
        check_replay(recording, &self.oracle, &self.truth);
    }

    pub(crate) fn debug_payload_bytes(&self) -> usize {
        let owners: BTreeSet<_> = self
            .oracle
            .draws
            .iter()
            .map(|draw| (draw.owner.id, draw.owner.generation))
            .collect();
        assert!(!owners.is_empty());
        for (id, generation) in owners {
            let first = self
                .oracle
                .draws
                .iter()
                .find(|draw| draw.owner.id == id && draw.owner.generation == generation)
                .unwrap()
                .event;
            for boundary in self.oracle.boundaries.iter().filter(|b| b.event >= first) {
                let owner = boundary.objects.get(&id).unwrap();
                assert_eq!(owner.generation, generation);
                assert_eq!(owner.class, RENDERER);
            }
        }
        self.oracle.draws.len()
            * (self.truth.physical_indices.len() / 3)
            * std::mem::size_of::<Primitive>()
    }

    pub(crate) fn renderer_wire_bytes(&self) -> usize {
        self.oracle.renderer_bytes
    }

    pub(crate) fn check_retained_recording(&self, recording: &Recording) {
        assert!(recording.first_frame() > 0);
        assert_eq!(recording.frame_count(), 2 * self.truth.steps.len());
        recording
            .frame_sources
            .validate_frames(&recording.frames)
            .unwrap();
        assert_eq!(recording.frame_sources.sources().count(), 2);
        let mut cursor = ReplayCursor::default();
        let mut state = SceneState::default();
        for key in self.sources() {
            let source = recording.frame_sources.source(key).unwrap();
            assert_eq!(source.observed_frames(), self.truth.steps.len());
            let frames: Vec<_> = self
                .oracle
                .boundaries
                .iter()
                .filter(|boundary| {
                    boundary.key == key && recording.is_frame_retained(boundary.frame.index)
                })
                .collect();
            assert!(frames.len() >= 3 && frames.len() < self.truth.steps.len());
            assert_eq!(source.len(), frames.len());
            for boundary in &frames {
                assert_eq!(
                    source.at_ordinal(boundary.frame.ordinal),
                    Some(&boundary.frame)
                );
                assert_eq!(source.at_index(boundary.frame.index), Some(&boundary.frame));
                assert_eq!(
                    recording
                        .frame_info(boundary.frame.index)
                        .unwrap()
                        .timestamp,
                    boundary.timestamp
                );
            }
            let count = frames.len();
            for position in (0..count)
                .chain((0..count).rev())
                .chain((0..count).map(|i| (i * 13 + 7) % count))
            {
                let expected = frames[position];
                if recording.is_disk_backed() {
                    cursor
                        .seek(recording, expected.frame.index, || false)
                        .unwrap()
                        .update
                        .apply_owned(&mut state);
                } else {
                    state = recording.state_at(expected.frame.index).unwrap();
                }
                check_state(&state, expected, &self.oracle, &self.truth);
            }
        }
    }
}
