use super::*;
use crate::native::{Class, Property, container_tests};
use pvd_protocol::Name;

mod multi_scene;

fn metadata() -> Metadata {
    let strings = [
        "",
        "physx3",
        "ObjectRef",
        "PxTransform",
        "PxBounds3",
        "PxScene",
        "PxActor",
        "PxRigidStatic",
        "PxRigidDynamic",
        "_debugger_",
        "SectionEventData",
        "frame",
        "RigidStatics",
        "RigidDynamics",
        "Articulations",
        "Cloths",
        "ParticleSystems",
        "ParticleFluids",
        "Scene",
        "GlobalPose",
        "WorldBounds",
        "KinematicTarget",
        "PxVec4",
        "PvdU32",
        "Counter",
    ]
    .into_iter()
    .enumerate()
    .map(|(id, value)| (id as u32, value.into()))
    .collect();
    let name = |name| Name { namespace: 1, name };
    let class = |name, base, size, properties, dynamic, has_pointers| {
        Some(Class {
            name,
            base,
            packing: crate::native::ClassPacking::default(),
            size,
            properties,
            dynamic,
            has_pointers,
            string_offsets: vec![],
        })
    };
    let mut properties = vec![None];
    for (owner, label, datatype_id, datatype, kind, offset32) in [
        (5, 12, 1, 2, 2, 0),
        (5, 13, 1, 2, 2, 12),
        (5, 14, 1, 2, 2, 24),
        (5, 15, 1, 2, 2, 36),
        (5, 16, 1, 2, 2, 48),
        (5, 17, 1, 2, 2, 60),
        (6, 18, 1, 2, 1, 0),
        (7, 19, 2, 3, 1, 4),
        (7, 20, 3, 4, 1, 32),
        (8, 19, 2, 3, 1, 4),
        (8, 20, 3, 4, 1, 32),
        (8, 21, 2, 3, 1, 56),
        (8, 24, 10, 23, 1, 84),
    ] {
        properties.push(Some(Property {
            owner: name(owner),
            name: label,
            datatype: name(datatype),
            datatype_id,
            kind,
            semantic: 0,
            offset32,
            values: vec![],
        }));
    }
    Metadata {
        strings,
        classes: vec![
            None,
            class(name(2), u32::MAX, 4, vec![], false, true),
            class(name(3), u32::MAX, 28, vec![], false, false),
            class(name(4), u32::MAX, 24, vec![], false, false),
            class(name(5), u32::MAX, 72, vec![1, 2, 3, 4, 5, 6], true, false),
            class(name(6), u32::MAX, 4, vec![7], false, false),
            class(name(7), 5, 56, vec![8, 9], false, false),
            class(name(8), 5, 88, vec![10, 11, 12, 13], false, false),
            class(
                Name {
                    namespace: 9,
                    name: 10,
                },
                u32::MAX,
                24,
                vec![],
                false,
                false,
            ),
            class(name(22), u32::MAX, 16, vec![], false, false),
            class(name(23), u32::MAX, 4, vec![], false, false),
        ],
        properties,
        messages: vec![],
    }
}

fn metadata_bytes(metadata: &Metadata) -> Vec<u8> {
    let mut bytes = container_tests::metadata_bytes(metadata);
    let insertion = {
        let mut reader = Reader::new(&bytes);
        reader.take(16).unwrap();
        for _ in 0..metadata.strings.len() - 1 {
            reader.u32().unwrap();
            reader.blob().unwrap();
        }
        assert_eq!(reader.u32().unwrap() as usize, metadata.properties.len());
        for property in &metadata.properties {
            assert_eq!(reader.marker().unwrap(), property.is_some());
            if property.is_some() {
                reader.take(13 * 4).unwrap();
            }
        }
        assert_eq!(reader.u32().unwrap() as usize, metadata.classes.len());
        assert!(!reader.marker().unwrap());
        assert!(reader.marker().unwrap());
        reader.take(6 * 4 + 2 + 6 * 4).unwrap();
        assert_eq!(reader.u32().unwrap(), 0);
        reader.pos
    };
    bytes[insertion..insertion + 4].copy_from_slice(&1u32.to_le_bytes());
    bytes.splice(
        insertion + 4..insertion + 4,
        [1u32, 0].into_iter().flat_map(u32::to_le_bytes),
    );
    bytes.truncate(bytes.len() - 4);
    w32(&mut bytes, metadata.messages.len() as u32);
    for (index, message) in metadata.messages.iter().enumerate() {
        w32(&mut bytes, u32::from(message.is_some()));
        let Some(message) = message else { continue };
        for word in [
            message.class_name.namespace,
            message.class_name.name,
            message.class,
            1,
            24,
            index as u32,
            message.size as u32,
            message.entries.len() as u32,
        ] {
            w32(&mut bytes, word);
        }
        for entry in &message.entries {
            for word in [
                entry.datatype.namespace,
                entry.datatype.name,
                entry.datatype_id,
                entry.offset as u32,
                entry.size as u32,
                entry.destination_size as u32,
                entry.property,
            ] {
                w32(&mut bytes, word);
            }
        }
        w32(&mut bytes, 0);
    }
    bytes
}

fn words(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|word| word.to_le_bytes()).collect()
}

fn floats(values: &[f32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

fn pose(position: [f32; 3]) -> Vec<u8> {
    floats(&[0.0, -0.0, 0.0, 1.0, position[0], position[1], position[2]])
}

fn set(id: u32, property: u32, data: &[u8], undo: &[u32]) -> Vec<u8> {
    let mut bytes = words(&[3, id, property, data.len() as u32]);
    bytes.extend(data);
    bytes.extend(words(&[undo.len() as u32]));
    bytes.extend(words(undo));
    bytes
}

fn array(kind: u32, id: u32, property: u32, index: u32, value: Option<u32>) -> Vec<u8> {
    let mut bytes = words(&[kind, id, property, index]);
    if let Some(value) = value {
        bytes.extend(words(&[4, value]));
    }
    if kind != 5 {
        bytes.extend(words(&[0]));
    }
    bytes
}

fn shift(scene: u32, values: [f32; 3], pairs: &[[u32; 2]]) -> Vec<u8> {
    let mut bytes = words(&[11, scene]);
    bytes.extend(floats(&values));
    bytes.extend(words(&[pairs.len() as u32]));
    for pair in pairs {
        bytes.extend(words(pair));
    }
    bytes
}

fn section(scene: u32, ticks: u64, begin: bool) -> Vec<u8> {
    let mut bytes = words(&[8, 8, 24]);
    bytes.extend((u64::from(scene) * 0x111).to_le_bytes());
    bytes.extend(ticks.to_le_bytes());
    bytes.extend(words(&[11]));
    bytes.extend([u8::from(begin), 0, 0, 0]);
    bytes
}

fn raw_events<'a>(layout: &Layout<'a>) -> Vec<&'a [u8]> {
    let mut result = Vec::new();
    for page in &layout.pages {
        let bytes =
            &layout.backing[page.position as usize..page.position as usize + page.len as usize];
        let count = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
        let payload = &bytes[4 + count * 4..];
        let offsets = bytes[4..4 + count * 4]
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()) as usize)
            .chain(std::iter::once(payload.len()))
            .collect::<Vec<_>>();
        for range in offsets.windows(2) {
            result.push(&payload[range[0]..range[1]]);
        }
    }
    result
}

struct Fixture {
    bytes: Vec<u8>,
    events: Vec<Vec<u8>>,
    shifts: Vec<usize>,
    counter: usize,
}

fn fixture(message: bool) -> Fixture {
    fixture_with_layout(message, false)
}

fn fixture_with_layout(message: bool, canonical: bool) -> Fixture {
    let mut metadata = metadata();
    if canonical {
        origin_shift::layout_tests::install(&mut metadata);
    }
    if message {
        metadata.messages.push(Some(crate::native::Message {
            class_name: metadata.class(7).unwrap().name,
            class: 7,
            size: 4,
            entries: vec![crate::native::MessageEntry {
                datatype: metadata.class(10).unwrap().name,
                datatype_id: 10,
                property: 13,
                offset: 0,
                size: 4,
                destination_size: 4,
            }],
        }));
    }
    let object_bytes = metadata_bytes(&metadata);
    let mut encoded = vec![words(&[9])];
    for (id, class) in [(1, 4), (10, 6), (20, 7)] {
        encoded.push(words(&[1, id, class]));
    }
    encoded.extend([
        set(1, 1, &words(&[10]), &[]),
        array(5, 1, 2, 0, Some(20)),
        set(10, 7, &words(&[1]), &[]),
        set(20, 7, &words(&[1]), &[]),
        set(10, 8, &pose([100.0, 200.0, 300.0]), &[]),
        set(
            10,
            9,
            &floats(&[90.0, 180.0, 270.0, 110.0, 220.0, 330.0]),
            &[],
        ),
        set(20, 10, &pose([10.0, 20.0, 30.0]), &[]),
        set(20, 11, &floats(&[8.0, 16.0, 24.0, 12.0, 24.0, 36.0]), &[]),
        set(20, 12, &pose([12.0, 24.0, 36.0]), &[]),
    ]);
    let counter_initial = encoded.len();
    encoded.push(set(20, 13, &words(&[7]), &[]));
    let pairs = [[20, 12], [10, 9], [20, 10], [10, 8], [20, 11]];
    let shifts_by_frame = [
        [2.0, 4.0, 0.0],
        [1.0, -2.0, 4.0],
        [4.0, 0.0, -8.0],
        [-2.0, 8.0, 2.0],
    ];
    let mut rows = Vec::new();
    let mut shifts = Vec::new();
    let mut counter = 0;
    for frame in 0..5 {
        let ticks = 100 + frame as u64 * 10;
        let begin = encoded.len() as u64;
        encoded.push(section(1, ticks, true));
        if frame == 2 {
            counter = encoded.len();
            let mut bytes = set(20, 13, &words(&[9]), &[(counter - counter_initial) as u32]);
            if message {
                bytes[..4].copy_from_slice(&4u32.to_le_bytes());
                bytes[8..12].copy_from_slice(&0u32.to_le_bytes());
            }
            encoded.push(bytes);
        }
        if let Some(&displacement) = shifts_by_frame.get(frame) {
            shifts.push(encoded.len());
            encoded.push(shift(1, displacement, &pairs));
        } else {
            encoded.push(set(
                20,
                13,
                &words(&[11]),
                &[(encoded.len() - counter) as u32],
            ));
        }
        let end = encoded.len() as u64;
        encoded.push(section(1, ticks + 1, false));
        rows.push([ticks, begin, ticks + 1, end]);
    }
    let mut parse_budget = 0;
    let events = encoded
        .iter()
        .enumerate()
        .map(|(ordinal, bytes)| {
            HistoryEvent::parse(bytes, ordinal as u64, &metadata, &mut parse_budget).unwrap()
        })
        .collect::<Vec<_>>();
    let mut pages = Vec::new();
    for (index, events) in events.chunks(5).enumerate() {
        pages.push(Page {
            first: (index * 5) as u64,
            position: ((index + 1) * 4096) as u64,
            count: events.len() as u32,
            len: encode_page(events, &mut || true).unwrap().len() as u32,
        });
    }
    let pointer_layouts = (0..metadata.classes.len())
        .map(|index| if index == 1 { vec![(1, 0)] } else { vec![] })
        .collect();
    let layout = Layout {
        source: &[],
        object_bytes: &object_bytes,
        metadata,
        pointer_layouts,
        communication: Communication {
            clock: [1, 1],
            instances: [1, 10, 20]
                .into_iter()
                .map(|id| crate::native::export::Instance {
                    id,
                    source: u64::from(id) * 0x111,
                    flags: [0; 10],
                })
                .collect(),
            reverse: [1, 10, 20]
                .into_iter()
                .map(|id| (u64::from(id) * 0x111, id))
                .collect(),
            cameras: vec![],
            sections: vec![Section {
                source: 0x111,
                name: 11,
                rows,
            }],
            zones: vec![],
            memory_instances: [u32::MAX; 2],
            memory_names: vec![],
        },
        metadata_padding: &[],
        history_padding: &[],
        pages,
        snapshots: vec![Snapshot {
            event: 0,
            position: 0,
            len: 4,
            reserved: 0,
        }],
        snapshot_bytes: vec![Cow::Borrowed(&[0; 4])],
        saved: vec![],
        events,
        backing: &[],
        gaps: vec![],
    };
    Fixture {
        bytes: layout.encode(false, &mut || true).unwrap(),
        events: encoded,
        shifts,
        counter,
    }
}

fn retain(name: &str, bytes: &[u8]) {
    if let Some(directory) = std::env::var_os("PVD_NATIVE_CONSTRUCTED_EXPORT_DIRECTORY") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join(name), bytes).unwrap();
    }
}

fn prepare_raw(layout: &Layout<'_>, stop: usize) -> (RawState, usize, Option<Scope>) {
    let mut scope = Scope::prepare(layout, layout.events.len(), &mut || true).unwrap();
    let mut state = RawState::new();
    let mut allocated = 0;
    for event in &layout.events[..stop] {
        raw_apply_with_origin(
            &layout.metadata,
            &mut state,
            event,
            &mut allocated,
            &mut scope,
            &mut || true,
        )
        .unwrap();
    }
    (state, allocated, scope)
}

fn value_array(values: &[f32]) -> crate::Value {
    crate::Value::Array(
        values
            .iter()
            .copied()
            .map(crate::Value::F32)
            .collect::<Vec<_>>()
            .into(),
    )
}

fn assert_frame(state: &crate::SceneState, frame: usize) {
    let static_positions = [
        [98.0, 196.0, 300.0],
        [97.0, 198.0, 296.0],
        [93.0, 198.0, 304.0],
        [95.0, 190.0, 302.0],
    ];
    let dynamic_positions = [
        [8.0, 16.0, 30.0],
        [7.0, 18.0, 26.0],
        [3.0, 18.0, 34.0],
        [5.0, 10.0, 32.0],
    ];
    let targets = [
        [10.0, 20.0, 36.0],
        [9.0, 22.0, 32.0],
        [5.0, 22.0, 40.0],
        [7.0, 14.0, 38.0],
    ];
    let static_bounds = [
        [88.0, 176.0, 270.0, 108.0, 216.0, 330.0],
        [87.0, 178.0, 266.0, 107.0, 218.0, 326.0],
        [83.0, 178.0, 274.0, 103.0, 218.0, 334.0],
        [85.0, 170.0, 272.0, 105.0, 210.0, 332.0],
    ];
    let dynamic_bounds = [
        [6.0, 12.0, 24.0, 10.0, 20.0, 36.0],
        [5.0, 14.0, 20.0, 9.0, 22.0, 32.0],
        [1.0, 14.0, 28.0, 5.0, 22.0, 40.0],
        [3.0, 6.0, 26.0, 7.0, 14.0, 38.0],
    ];
    let index = frame.min(3);
    for (id, property, expected) in [
        (10, "GlobalPose", static_positions[index]),
        (20, "GlobalPose", dynamic_positions[index]),
        (20, "KinematicTarget", targets[index]),
    ] {
        let crate::Value::Transform { position, rotation } =
            state.objects[&id].properties[property]
        else {
            panic!("transform")
        };
        assert_eq!(position, expected, "frame {frame} object {id}");
        assert_eq!(
            rotation.map(f32::to_bits),
            [0, 0x8000_0000, 0, 1.0f32.to_bits()]
        );
    }
    assert_eq!(
        state.objects[&10].properties["WorldBounds"],
        value_array(&static_bounds[index])
    );
    assert_eq!(
        state.objects[&20].properties["WorldBounds"],
        value_array(&dynamic_bounds[index])
    );
    assert_eq!(
        state.objects[&20].properties["Counter"],
        crate::Value::U64(match frame {
            0 | 1 => 7,
            2 | 3 => 9,
            _ => 11,
        })
    );
}

fn check_middle(canonical: bool) {
    for message in [false, true] {
        let fixture = fixture_with_layout(message, canonical);
        let kind = if canonical { "canonical-" } else { "" };
        retain(
            &format!(
                "origin-middle-{kind}{}source.pxd2",
                if message { "message-" } else { "" }
            ),
            &fixture.bytes,
        );
        let source_layout = Layout::parse(&fixture.bytes, &mut || true).unwrap();
        let (source, _) = crate::native::read_with_progress(&fixture.bytes, |_, _| true).unwrap();
        for (first, last) in [(1, 1), (1, 3), (2, 3), (3, 4)] {
            let start = source_layout.communication.sections[0].rows[first][1] as usize;
            let end = source_layout.communication.sections[0].rows[last][3] as usize;
            let mut bytes = Vec::new();
            let report =
                trim_scene_frames(&fixture.bytes, first, last, &mut bytes, || true).unwrap();
            assert_eq!((report.source_frames, report.frames), (5, last - first + 1));
            let output_layout = Layout::parse(&bytes, &mut || true).unwrap();
            if canonical {
                assert_initial_fixed_bytes(&output_layout, first);
            }
            assert_eq!(output_layout.snapshots[0].event, 0);
            assert_eq!(
                u32::from_le_bytes(output_layout.snapshot_bytes[0][..4].try_into().unwrap()),
                3
            );
            let actual = raw_events(&output_layout);
            let shifts = fixture
                .shifts
                .iter()
                .copied()
                .filter(|&ordinal| (start..=end).contains(&ordinal))
                .collect::<Vec<_>>();
            for &ordinal in &shifts {
                assert_eq!(actual[ordinal - start + 1], fixture.events[ordinal]);
            }
            let mut seen = 0;
            for (ordinal, raw) in actual.iter().enumerate() {
                let kind = u32::from_le_bytes(raw[..4].try_into().unwrap());
                if kind == 11 {
                    seen += 1;
                }
                if matches!(kind, 3 | 4) && raw.len() == 28 {
                    assert_eq!(u32::from_le_bytes(raw[20..24].try_into().unwrap()), 1);
                    assert_eq!(
                        u32::from_le_bytes(raw[24..28].try_into().unwrap()) as usize,
                        ordinal
                    );
                }
            }
            assert_eq!(seen, shifts.len());
            let (restored, _) = crate::native::read_with_progress(&bytes, |_, _| true).unwrap();
            for frame in (0..restored.frame_count())
                .chain((0..restored.frame_count()).rev())
                .chain([0, last - first, 0])
            {
                let actual = restored.state_at(frame).unwrap();
                assert_frame(&actual, first + frame);
                assert_eq!(actual, source.state_at(first + frame).unwrap());
            }
            retain(
                &format!(
                    "origin-middle-{kind}{}-{first}-{last}.pxd2",
                    if message { "message" } else { "scalar" }
                ),
                &bytes,
            );
        }
    }
}

#[test]
fn middle_preserves_handwritten_rigid_values_raw_rows_and_cross_start_scalar_or_message_undo() {
    check_middle(false);
}

#[test]
fn canonical_rigid_middle_preserves_handwritten_values_raw_rows_and_undo() {
    check_middle(true);
}

fn assert_initial_fixed_bytes(layout: &Layout<'_>, first: usize) {
    let (position, bounds, target, counter) = match first {
        1 => (
            [8.0, 16.0, 30.0],
            [6.0, 12.0, 24.0, 10.0, 20.0, 36.0],
            [10.0, 20.0, 36.0],
            7,
        ),
        2 => (
            [7.0, 18.0, 26.0],
            [5.0, 14.0, 20.0, 9.0, 22.0, 32.0],
            [9.0, 22.0, 32.0],
            7,
        ),
        3 => (
            [3.0, 18.0, 34.0],
            [1.0, 14.0, 28.0, 5.0, 22.0, 40.0],
            [5.0, 22.0, 40.0],
            9,
        ),
        _ => panic!("unexpected first frame"),
    };
    let snapshot = &layout.snapshot_bytes[0];
    assert_eq!(snapshot.len(), 40);
    assert_eq!(&snapshot[..4], &3u32.to_le_bytes());
    let row = snapshot[4..]
        .chunks_exact(12)
        .find(|row| row[..4] == 20u32.to_le_bytes())
        .unwrap();
    let position_in_file = u64::from_le_bytes(row[4..].try_into().unwrap());
    let record = layout
        .saved
        .iter()
        .find(|record| record.position == position_in_file)
        .unwrap();
    assert_eq!((record.id, record.class, record.deleted), (20, 7, false));
    let mut expected = words(&[88, 1]);
    expected.extend(pose(position));
    expected.extend(floats(&bounds));
    expected.extend(pose(target));
    expected.extend(words(&[counter]));
    assert_eq!(record.body.as_ref(), expected);
}

#[test]
fn canonical_middle_updates_remain_atomic_at_work_memory_and_cancel_boundaries() {
    let fixture = fixture_with_layout(false, true);
    let mut layout = Layout::parse(&fixture.bytes, &mut || true).unwrap();
    let current = fixture.shifts[1];
    let (before, allocated, _) = prepare_raw(&layout, current);
    let HistoryEvent::OriginShift(event) = &layout.events[current] else {
        panic!("shift")
    };
    for work in [6831, 6832] {
        let mut state = before.clone();
        let mut scope = Scope { work };
        let result = scope.apply(
            &layout.metadata,
            &mut state,
            event,
            allocated,
            true,
            &mut || true,
        );
        if work == 6832 {
            result.unwrap();
            assert_eq!(scope.work, 0);
            assert_eq!(state[&10].values[&8], pose([97.0, 198.0, 296.0]));
            assert_eq!(state[&20].values[&10], pose([7.0, 18.0, 26.0]));
            assert_eq!(
                state[&20].values[&11],
                floats(&[5.0, 14.0, 20.0, 9.0, 22.0, 32.0])
            );
            assert_eq!(state[&20].values[&12], pose([9.0, 22.0, 32.0]));
        } else {
            assert!(result.unwrap_err().to_string().contains("work budget"));
            assert_eq!(state, before);
        }
    }
    let mut calls = 0;
    Scope { work: WORK_LIMIT }
        .apply(
            &layout.metadata,
            &mut before.clone(),
            event,
            allocated,
            true,
            &mut || {
                calls += 1;
                true
            },
        )
        .unwrap();
    for stop in 1..=calls {
        let mut state = before.clone();
        let mut current = 0;
        let failure = Scope { work: WORK_LIMIT }
            .apply(
                &layout.metadata,
                &mut state,
                event,
                allocated,
                true,
                &mut || {
                    current += 1;
                    current != stop
                },
            )
            .unwrap_err();
        assert_eq!(
            failure.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
        assert_eq!(current, stop);
        assert_eq!(state, before);
    }
    let mut state = before.clone();
    let failure = Scope { work: WORK_LIMIT }
        .apply(
            &layout.metadata,
            &mut state,
            event,
            MAX_METADATA,
            true,
            &mut || true,
        )
        .unwrap_err();
    assert!(failure.to_string().contains("budget"));
    assert_eq!(state, before);
    let bounds_member = layout.metadata.class(84).unwrap().properties[1] as usize;
    layout.metadata.properties[bounds_member]
        .as_mut()
        .unwrap()
        .offset32 = 8;
    let failure = Scope { work: WORK_LIMIT }
        .apply(
            &layout.metadata,
            &mut state,
            event,
            allocated,
            true,
            &mut || true,
        )
        .unwrap_err();
    assert!(failure.to_string().contains("fixed layout"));
    assert_eq!(state, before);
}

#[test]
fn canonical_middle_keeps_previous_shift_and_nonempty_initial_scopes_closed() {
    let fixture = fixture_with_layout(false, true);
    let mut layout = Layout::parse(&fixture.bytes, &mut || true).unwrap();
    let HistoryEvent::Set { undo, .. } = &mut layout.events[fixture.counter] else {
        panic!("counter")
    };
    *undo = vec![(fixture.counter - fixture.shifts[0]) as u32];
    let source = layout.encode(false, &mut || true).unwrap();
    let mut output = Vec::new();
    let failure = trim_scene_frames(&source, 1, 3, &mut output, || true).unwrap_err();
    assert!(
        failure
            .to_string()
            .contains("OriginShift previous-event undo is not verified"),
        "{failure}"
    );
    assert!(output.is_empty());
    let mut middle = Vec::new();
    trim_scene_frames(&fixture.bytes, 1, 3, &mut middle, || true).unwrap();
    let failure = trim_scene_frames(&middle, 1, 2, &mut output, || true).unwrap_err();
    assert!(
        failure
            .to_string()
            .contains("initial middle OriginShift is not verified"),
        "{failure}"
    );
    assert!(output.is_empty());
    let failure = trim_scene_frames(&fixture.bytes, 4, 4, &mut output, || true).unwrap_err();
    assert!(
        failure
            .to_string()
            .contains("first frame must contain a captured Scene shift"),
        "{failure}"
    );
    assert!(output.is_empty());
}

#[test]
fn middle_rejects_previous_origin_references_before_they_can_collapse_into_the_snapshot() {
    for message in [false, true] {
        let fixture = fixture(message);
        for target in [fixture.shifts[0], fixture.shifts[1]] {
            let mut layout = Layout::parse(&fixture.bytes, &mut || true).unwrap();
            let HistoryEvent::Set { undo, .. } = &mut layout.events[fixture.counter] else {
                panic!("counter")
            };
            *undo = vec![(fixture.counter - target) as u32];
            let source = layout.encode(false, &mut || true).unwrap();
            let mut output = Vec::new();
            let failure = trim_scene_frames(&source, 1, 3, &mut output, || true).unwrap_err();
            assert!(
                failure
                    .to_string()
                    .contains("OriginShift previous-event undo is not verified"),
                "{failure}"
            );
            assert!(output.is_empty());
        }
    }
}

#[test]
fn middle_rejects_lossy_inverse_and_signed_zero_loss_without_publishing_an_output() {
    let fixture = fixture(false);
    for signed_zero in [false, true] {
        let mut layout = Layout::parse(&fixture.bytes, &mut || true).unwrap();
        let source_shift = &fixture.events[fixture.shifts[1]];
        let mut changed = source_shift.clone();
        if signed_zero {
            let HistoryEvent::Set { data, .. } = &mut layout.events[8] else {
                panic!("pose")
            };
            data.to_mut()[24..28].copy_from_slice(&(-0.0f32).to_le_bytes());
            changed[16..20].copy_from_slice(&(-0.0f32).to_le_bytes());
        } else {
            changed[8..12].copy_from_slice(&1.0e30f32.to_le_bytes());
        }
        layout.events[fixture.shifts[1]] =
            HistoryEvent::parse(&changed, fixture.shifts[1] as u64, &layout.metadata, &mut 0)
                .unwrap();
        let source = layout.encode(false, &mut || true).unwrap();
        let mut output = Vec::new();
        let failure = trim_scene_frames(&source, 1, 3, &mut output, || true).unwrap_err();
        assert!(
            failure
                .to_string()
                .contains("inverse does not preserve scalar bits"),
            "{failure}"
        );
        assert!(output.is_empty());
    }
}

#[test]
fn fixed_updates_validate_every_field_before_commit_and_check_memory_work_and_cancellation() {
    let fixture = fixture(false);
    let layout = Layout::parse(&fixture.bytes, &mut || true).unwrap();
    let current = fixture.shifts[1];
    let (before, allocated, _) = prepare_raw(&layout, current);
    let HistoryEvent::OriginShift(event) = &layout.events[current] else {
        panic!("shift")
    };
    let mut calls = 0;
    Scope { work: WORK_LIMIT }
        .apply(
            &layout.metadata,
            &mut before.clone(),
            event,
            allocated,
            true,
            &mut || {
                calls += 1;
                true
            },
        )
        .unwrap();
    for stop in 1..=calls {
        let mut state = before.clone();
        let mut current = 0;
        let failure = Scope { work: WORK_LIMIT }
            .apply(
                &layout.metadata,
                &mut state,
                event,
                allocated,
                true,
                &mut || {
                    current += 1;
                    current != stop
                },
            )
            .unwrap_err();
        assert_eq!(
            failure.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
        assert_eq!(state, before);
    }
    for (work, memory) in [(0, allocated), (WORK_LIMIT, MAX_METADATA)] {
        let mut state = before.clone();
        let failure = Scope { work }
            .apply(
                &layout.metadata,
                &mut state,
                event,
                memory,
                true,
                &mut || true,
            )
            .unwrap_err();
        assert!(failure.to_string().contains("budget"), "{failure}");
        assert_eq!(state, before);
    }
    let mut malformed = before.clone();
    malformed
        .get_mut(&10)
        .unwrap()
        .values
        .get_mut(&8)
        .unwrap()
        .truncate(24);
    let expected = malformed.clone();
    let failure = Scope { work: WORK_LIMIT }
        .apply(
            &layout.metadata,
            &mut malformed,
            event,
            allocated,
            true,
            &mut || true,
        )
        .unwrap_err();
    assert!(
        failure.to_string().contains("defined fixed property"),
        "{failure}"
    );
    assert_eq!(malformed, expected);
}

#[test]
fn raw_shift_requires_a_validated_scope_and_rejects_non_finite_values() {
    let fixture = fixture(false);
    let layout = Layout::parse(&fixture.bytes, &mut || true).unwrap();
    let current = fixture.shifts[1];
    let (before, allocated, _) = prepare_raw(&layout, current);
    let failure = raw_apply(
        &layout.metadata,
        &mut before.clone(),
        &layout.events[current],
        &mut 0,
        &mut || true,
    )
    .unwrap_err();
    assert!(
        failure
            .to_string()
            .contains("requires a verified middle scope")
    );
    let HistoryEvent::OriginShift(event) = &layout.events[current] else {
        panic!("shift")
    };
    for invalid in [f32::INFINITY, f32::NAN] {
        let mut state = before.clone();
        state.get_mut(&10).unwrap().values.get_mut(&8).unwrap()[16..20]
            .copy_from_slice(&invalid.to_le_bytes());
        let expected = state.clone();
        let failure = Scope { work: WORK_LIMIT }
            .apply(
                &layout.metadata,
                &mut state,
                event,
                allocated,
                true,
                &mut || true,
            )
            .unwrap_err();
        assert!(
            failure
                .to_string()
                .contains("requires finite fixed scalars")
        );
        assert_eq!(state, expected);
    }
}

#[test]
fn middle_keeps_unknown_layout_lifecycle_snapshot_and_saved_scopes_closed() {
    let fixture = fixture(false);
    for case in 0..6 {
        let mut layout = Layout::parse(&fixture.bytes, &mut || true).unwrap();
        let expected = match case {
            0 => {
                layout.metadata.classes[3].as_mut().unwrap().size = 23;
                "field layout"
            }
            1 => {
                layout.metadata.classes[2].as_mut().unwrap().has_pointers = true;
                "field layout"
            }
            2 => {
                layout.metadata.properties[9].as_mut().unwrap().kind = 2;
                "field layout"
            }
            3 => {
                layout.snapshot_bytes[0] = Cow::Borrowed(&[1, 0, 0, 0]);
                "empty event-zero snapshot"
            }
            4 => {
                layout.events[8] = HistoryEvent::Destroy(10);
                "writes missing instance"
            }
            _ => {
                layout.events[8] = HistoryEvent::Saved {
                    id: 10,
                    position: 0,
                };
                "retained saved instances"
            }
        };
        let failure = match Scope::prepare(&layout, layout.events.len(), &mut || true) {
            Ok(_) => panic!("scope {case}"),
            Err(failure) => failure,
        };
        assert!(
            failure.to_string().contains(expected),
            "case {case}: {failure}"
        );
    }
}

#[test]
fn middle_requires_a_captured_shift_in_the_first_frame_after_shifted_prehistory() {
    let fixture = fixture(false);
    let mut output = Vec::new();
    let failure = trim_scene_frames(&fixture.bytes, 4, 4, &mut output, || true).unwrap_err();
    assert!(
        failure
            .to_string()
            .contains("first frame must contain a captured Scene shift"),
        "{failure}"
    );
    assert!(output.is_empty());
    let mut first = Vec::new();
    trim_scene_frames(&fixture.bytes, 1, 3, &mut first, || true).unwrap();
    let failure = trim_scene_frames(&first, 1, 2, &mut output, || true).unwrap_err();
    assert!(
        failure
            .to_string()
            .contains("initial middle OriginShift is not verified"),
        "{failure}"
    );
    assert!(output.is_empty());
}

#[test]
fn retained_shift_proof_binds_raw_words_rows_ordinals_and_event_count() {
    let fixture = fixture(false);
    for case in 0..4 {
        let mut layout = Layout::parse(&fixture.bytes, &mut || true).unwrap();
        let proof = layout.trim_middle(1, 3, &mut || true).unwrap();
        let bytes = layout.encode(false, &mut || true).unwrap();
        let mut output = Layout::parse(&bytes, &mut || true).unwrap();
        proof.verify_layout(&output, &mut || true).unwrap();
        let at = output
            .events
            .iter()
            .enumerate()
            .filter(|(_, event)| matches!(event, HistoryEvent::OriginShift(_)))
            .nth(usize::from(case == 1))
            .unwrap()
            .0;
        let mut changed = raw_events(&output)[at].to_vec();
        if case == 0 {
            changed[24..40].rotate_left(8);
        } else if case == 1 {
            changed[12..16].copy_from_slice(&(-0.0f32).to_le_bytes());
        }
        if case < 2 {
            output.events[at] =
                HistoryEvent::parse(&changed, at as u64, &output.metadata, &mut 0).unwrap();
        } else if case == 2 {
            output.events[at] = HistoryEvent::Marker;
        } else {
            output.events.swap(at, at + 1);
        }
        assert!(proof.verify_layout(&output, &mut || true).is_err());
    }
}
