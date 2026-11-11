use super::*;
use crate::native::{Class, Property, container_tests, read_with_progress};
use pvd_protocol::Name;

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
            class(name(8), 5, 84, vec![10, 11, 12], false, false),
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

struct Fixture {
    bytes: Vec<u8>,
    events: Vec<Vec<u8>>,
    first_shift: usize,
}

fn fixture() -> Fixture {
    fixture_with_layout(false)
}

fn fixture_with_layout(canonical: bool) -> Fixture {
    let mut metadata = metadata();
    if canonical {
        origin_shift::layout_tests::install(&mut metadata);
    }
    let object_bytes = metadata_bytes(&metadata);
    let mut encoded = vec![words(&[9])];
    for (id, class) in [(1, 4), (2, 4), (10, 6), (20, 7), (30, 6)] {
        encoded.push(words(&[1, id, class]));
    }
    encoded.extend([
        set(1, 1, &words(&[10]), &[]),
        array(5, 1, 2, 0, Some(20)),
        array(5, 2, 1, 0, Some(30)),
        set(10, 7, &words(&[1]), &[]),
        set(20, 7, &words(&[1]), &[]),
        set(30, 7, &words(&[2]), &[]),
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
        set(30, 8, &pose([300.0, 400.0, 500.0]), &[]),
        set(
            30,
            9,
            &floats(&[290.0, 390.0, 490.0, 310.0, 410.0, 510.0]),
            &[],
        ),
    ]);
    let pairs = [[20, 12], [10, 9], [20, 10], [10, 8], [20, 11]];
    let mut sections = BTreeMap::<u32, Section>::new();
    let mut first_shift = 0;
    for frame in 0..4 {
        let scene = if frame == 1 { 2 } else { 1 };
        let ticks = 100 + frame * 10;
        let begin = encoded.len() as u64;
        encoded.push(section(scene, ticks, true));
        if frame == 0 {
            first_shift = encoded.len();
            encoded.push(shift(1, [2.0, 4.0, -0.0], &pairs));
        } else if frame == 1 {
            encoded.push(shift(2, [-8.0, 16.0, 32.0], &[[30, 9], [30, 8]]));
        } else if frame == 2 {
            let ordinal = encoded.len();
            encoded.push(set(
                20,
                10,
                &pose([50.0, 60.0, 70.0]),
                &[(ordinal - 14) as u32, (ordinal - first_shift) as u32],
            ));
            encoded.push(shift(1, [1.0, -2.0, 4.0], &pairs));
        } else {
            encoded.push(set(20, 10, &pose([999.0; 3]), &[]));
        }
        let end = encoded.len() as u64;
        encoded.push(section(scene, ticks + 1, false));
        sections
            .entry(scene)
            .or_insert_with(|| Section {
                source: u64::from(scene) * 0x111,
                name: 11,
                rows: vec![],
            })
            .rows
            .push([ticks, begin, ticks + 1, end]);
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
    let layout = Layout {
        source: &[],
        object_bytes: &object_bytes,
        pointer_layouts: (0..metadata.classes.len())
            .map(|index| if index == 1 { vec![(1, 0)] } else { vec![] })
            .collect(),
        metadata,
        communication: Communication {
            clock: [1, 1],
            instances: [1, 2, 10, 20, 30]
                .into_iter()
                .map(|id| super::super::Instance {
                    id,
                    source: u64::from(id) * 0x111,
                    flags: [0; 10],
                })
                .collect(),
            reverse: [1, 2, 10, 20, 30]
                .into_iter()
                .map(|id| (u64::from(id) * 0x111, id))
                .collect(),
            cameras: vec![],
            sections: sections.into_values().rev().collect(),
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
        first_shift,
    }
}

fn reject(result: Result<bool, Error>, expected: &str) {
    let failure = match result {
        Ok(_) => panic!("expected {expected}"),
        Err(failure) => failure,
    };
    assert!(failure.to_string().contains(expected), "{failure}");
}

fn verify(layout: &Layout<'_>) -> Result<bool, Error> {
    verify_prefix_events(layout, layout.events.len(), &mut || true)
}

fn array_value(values: &[f32]) -> crate::Value {
    crate::Value::Array(
        values
            .iter()
            .copied()
            .map(crate::Value::F32)
            .collect::<Vec<_>>()
            .into(),
    )
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

fn check_rigid_prefix(canonical: bool) {
    let fixture = fixture_with_layout(canonical);
    let retained = std::env::var_os("PVD_NATIVE_CONSTRUCTED_EXPORT_DIRECTORY")
        .filter(|_| canonical)
        .map(std::path::PathBuf::from);
    if let Some(directory) = &retained {
        std::fs::create_dir_all(directory).unwrap();
        std::fs::write(
            directory.join("origin-prefix-canonical-source.pxd2"),
            &fixture.bytes,
        )
        .unwrap();
    }
    let layout = Layout::parse(&fixture.bytes, &mut || true).unwrap();
    assert!(verify(&layout).unwrap());
    let (source, _) = read_with_progress(&fixture.bytes, |_, _| true).unwrap();
    for last in [0, 1, 2] {
        let mut bytes = Vec::new();
        let report = trim_scene_frames(&fixture.bytes, 0, last, &mut bytes, || true).unwrap();
        if let Some(directory) = &retained {
            std::fs::write(
                directory.join(format!("origin-prefix-canonical-0-{last}.pxd2")),
                &bytes,
            )
            .unwrap();
        }
        assert_eq!((report.source_frames, report.frames), (4, last + 1));
        let trimmed = Layout::parse(&bytes, &mut || true).unwrap();
        assert!(trimmed.pages.len() > 1);
        assert_eq!(
            raw_events(&trimmed),
            fixture.events[..trimmed.events.len()]
                .iter()
                .map(Vec::as_slice)
                .collect::<Vec<_>>()
        );
        let (restored, _) = read_with_progress(&bytes, |_, _| true).unwrap();
        for frame in [last, 0, last, last / 2] {
            assert_eq!(
                restored.state_at(frame).unwrap(),
                source.state_at(frame).unwrap()
            );
        }
        let state = restored.state_at(last).unwrap();
        let position = if last == 2 {
            [49.0, 62.0, 66.0]
        } else {
            [8.0, 16.0, 30.0]
        };
        let target = if last == 2 {
            [9.0, 22.0, 32.0]
        } else {
            [10.0, 20.0, 36.0]
        };
        for (property, position) in [("GlobalPose", position), ("KinematicTarget", target)] {
            let crate::Value::Transform {
                position: actual,
                rotation,
            } = state.objects[&20].properties[property]
            else {
                panic!("transform")
            };
            assert_eq!(actual, position);
            assert_eq!(
                rotation.map(f32::to_bits),
                [0, 0x8000_0000, 0, 1.0f32.to_bits()]
            );
        }
        assert_eq!(
            state.objects[&10].properties["WorldBounds"],
            array_value(if last == 2 {
                &[87.0, 178.0, 266.0, 107.0, 218.0, 326.0]
            } else {
                &[88.0, 176.0, 270.0, 108.0, 216.0, 330.0]
            })
        );
        let crate::Value::Transform { position, .. } = state.objects[&30].properties["GlobalPose"]
        else {
            panic!("transform")
        };
        assert_eq!(
            position,
            if last == 0 {
                [300.0, 400.0, 500.0]
            } else {
                [308.0, 384.0, 468.0]
            }
        );
    }
    let mut output = Vec::new();
    assert!(trim_scene_frames(&fixture.bytes, 1, 2, &mut output, || true).is_err());
    assert!(output.is_empty());
}

#[test]
fn rigid_prefix_retains_raw_rows_ordinals_undo_words_and_handwritten_frame_values() {
    check_rigid_prefix(false);
}

#[test]
fn canonical_rigid_prefix_preserves_handwritten_values_and_raw_history() {
    check_rigid_prefix(true);
}

#[test]
fn canonical_prefix_schema_uses_the_shared_work_and_cancellation_budget() {
    let fixture = fixture_with_layout(true);
    let layout = Layout::parse(&fixture.bytes, &mut || true).unwrap();
    let cut = fixture.first_shift + 1;
    let mut complete = Budget::default();
    assert!(verify_with_budget(&layout, cut, &mut complete, &mut || true).unwrap());
    let required = WORK_LIMIT - complete.work;
    assert!(required > 19 * 64);
    for work in [required - 1, required] {
        let mut budget = Budget {
            work,
            ..Budget::default()
        };
        let result = verify_with_budget(&layout, cut, &mut budget, &mut || true);
        if work == required {
            assert!(result.unwrap());
            assert_eq!(budget.work, 0);
        } else {
            reject(result, "work budget");
        }
    }
    let mut calls = 0;
    assert!(
        verify_prefix_events(&layout, cut, &mut || {
            calls += 1;
            true
        })
        .unwrap()
    );
    for stop in 1..=calls {
        let mut current = 0;
        let failure = verify_prefix_events(&layout, cut, &mut || {
            current += 1;
            current != stop
        })
        .unwrap_err();
        assert_eq!(current, stop);
        assert_eq!(
            failure.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
    }
}

#[test]
fn prefix_validates_scene_and_actor_lifetimes_membership_and_auxiliary_completeness() {
    let fixture = fixture();
    for (ordinal, replacement, expected) in [
        (
            fixture.first_shift,
            shift(99, [1.0; 3], &[[10, 8]]),
            "missing or unsupported Scene",
        ),
        (
            fixture.first_shift,
            shift(10, [1.0; 3], &[[10, 8]]),
            "missing or unsupported Scene",
        ),
        (17, words(&[2, 10]), "contains missing Actor"),
        (17, words(&[1, 10, 6]), "instance ID reuse"),
        (9, set(10, 7, &words(&[2]), &[]), "membership disagrees"),
        (6, set(1, 1, &[], &[]), "absent from its Scene"),
        (7, array(5, 1, 1, 1, Some(20)), "membership disagrees"),
        (8, array(5, 2, 1, 0, Some(10)), "membership disagrees"),
        (
            17,
            set(1, 3, &words(&[30]), &[]),
            "only supports rigid Scene members",
        ),
        (
            fixture.first_shift,
            shift(1, [1.0; 3], &[[10, 8]]),
            "is incomplete",
        ),
        (
            fixture.first_shift,
            shift(1, [1.0; 3], &[[10, 8], [10, 8]]),
            "set disagrees",
        ),
        (
            fixture.first_shift,
            shift(1, [1.0; 3], &[[30, 8]]),
            "set disagrees",
        ),
        (3, words(&[9]), "writes missing instance"),
        (17, set(10, 8, &[0; 24], &[]), "invalid width"),
        (17, array(7, 1, 1, 1, Some(10)), "index exceeds membership"),
        (17, words(&[10, 1, 0, 0]), "retained saved instances"),
    ] {
        let mut layout = Layout::parse(&fixture.bytes, &mut || true).unwrap();
        layout.events[ordinal] =
            HistoryEvent::parse(&replacement, ordinal as u64, &layout.metadata, &mut 0).unwrap();
        reject(verify(&layout), expected);
    }
}

#[test]
fn prefix_accepts_reference_removal_replacement_and_reinsertion_before_shift() {
    let fixture = fixture();
    let mut layout = Layout::parse(&fixture.bytes, &mut || true).unwrap();
    let changes = [
        array(7, 1, 1, 0, Some(10)),
        array(6, 1, 2, 0, None),
        array(5, 1, 2, 0, Some(20)),
    ];
    for (index, bytes) in changes.iter().enumerate() {
        layout.events[16 + index] =
            HistoryEvent::parse(bytes, (16 + index) as u64, &layout.metadata, &mut 0).unwrap();
    }
    assert!(verify(&layout).unwrap());
}

#[test]
fn scalar_messages_preserve_membership_and_require_matching_owner_and_fixed_target() {
    let fixture = fixture();
    for case in 0..3 {
        let mut layout = Layout::parse(&fixture.bytes, &mut || true).unwrap();
        let owner = if case == 1 { 7 } else { 6 };
        layout.metadata.messages.push(Some(crate::native::Message {
            class_name: layout.metadata.class(owner).unwrap().name,
            class: owner,
            size: 4,
            entries: vec![crate::native::MessageEntry {
                datatype: layout.metadata.class(1).unwrap().name,
                datatype_id: 1,
                property: if case == 2 { 1 } else { 7 },
                offset: 0,
                size: 4,
                destination_size: 4,
            }],
        }));
        layout.events[17] = HistoryEvent::Set {
            kind: 4,
            id: 10,
            property: 0,
            data: Cow::Owned(words(&[1])),
            undo: vec![],
        };
        match case {
            0 => assert!(verify(&layout).unwrap()),
            1 => reject(verify(&layout), "does not belong to instance class"),
            _ => reject(verify(&layout), "array-target property messages"),
        }
    }
}

#[test]
fn prefix_proof_rejects_shift_row_reordering_float_bit_changes_and_undo_changes() {
    let fixture = fixture();
    for case in 0..3 {
        let mut layout = Layout::parse(&fixture.bytes, &mut || true).unwrap();
        let proof =
            super::super::initial::Proof::prepare(&layout, layout.events.len(), &mut || true)
                .unwrap();
        let mut bytes = fixture.events[fixture.first_shift].clone();
        if case == 0 {
            bytes[24..40].rotate_left(8);
        } else if case == 1 {
            bytes[16..20].copy_from_slice(&0u32.to_le_bytes());
        }
        if case < 2 {
            layout.events[fixture.first_shift] =
                HistoryEvent::parse(&bytes, fixture.first_shift as u64, &layout.metadata, &mut 0)
                    .unwrap();
        } else {
            let undo = layout
                .events
                .iter_mut()
                .find_map(|event| match event {
                    HistoryEvent::Set { undo, .. } if !undo.is_empty() => Some(undo),
                    _ => None,
                })
                .unwrap();
            undo[0] -= 1;
        }
        let failure = proof.verify_layout(&layout, &mut || true).unwrap_err();
        assert!(
            failure
                .to_string()
                .contains("changed retained capture inputs"),
            "{failure}"
        );
    }
}

#[test]
fn prefix_refuses_ambiguous_fixed_metadata_nonempty_snapshots_and_unverified_actor_fields() {
    let fixture = fixture();
    for case in 0..10 {
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
                layout.metadata.classes[3].as_mut().unwrap().dynamic = true;
                "property layout"
            }
            3 => {
                layout.metadata.properties[8].as_mut().unwrap().owner.name = 6;
                "identity is inconsistent"
            }
            4 => {
                layout.metadata.classes[6].as_mut().unwrap().base = 6;
                "class inheritance"
            }
            5 => {
                layout.metadata.properties[9].as_mut().unwrap().offset32 = 31;
                "property layout"
            }
            6 => {
                layout.metadata.classes[2].as_mut().unwrap().name.namespace = 9;
                "identity is inconsistent"
            }
            7 => {
                layout.metadata.properties[9].as_mut().unwrap().name = 21;
                "unsupported OriginShift prefix actor property"
            }
            8 => {
                layout.snapshot_bytes[0] = Cow::Borrowed(&[1, 0, 0, 0]);
                "empty event-zero snapshot"
            }
            _ => {
                layout.pointer_layouts[1].clear();
                "field layout"
            }
        };
        reject(verify(&layout), expected);
    }
}

#[test]
fn auxiliary_rows_check_cancellation_and_bounded_memory_and_work() {
    let fixture = fixture();
    let layout = Layout::parse(&fixture.bytes, &mut || true).unwrap();
    let cut = fixture.first_shift + 1;
    let mut calls = 0;
    assert!(
        verify_prefix_events(&layout, cut, &mut || {
            calls += 1;
            true
        })
        .unwrap()
    );
    for cancelled_at in calls - 4..=calls {
        let mut current = 0;
        let failure = verify_prefix_events(&layout, cut, &mut || {
            current += 1;
            current != cancelled_at
        })
        .unwrap_err();
        assert_eq!(
            failure.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
        assert_eq!(current, cancelled_at);
    }
    reject(
        verify_with_budget(
            &layout,
            cut,
            &mut Budget {
                limit: 128,
                ..Budget::default()
            },
            &mut || true,
        ),
        "metadata budget",
    );
    reject(
        verify_with_budget(
            &layout,
            cut,
            &mut Budget {
                work: 4,
                ..Budget::default()
            },
            &mut || true,
        ),
        "work budget",
    );
    let mut layout = Layout::parse(&fixture.bytes, &mut || true).unwrap();
    layout.events.truncate(fixture.first_shift);
    layout.snapshot_bytes[0] = Cow::Borrowed(&[1, 0, 0, 0]);
    assert!(!verify(&layout).unwrap());
}
