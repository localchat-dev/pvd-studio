use super::*;
use crate::{Recording, Value, native::container_tests};

struct Fixture {
    metadata: Metadata,
    events: Vec<HistoryEvent<'static>>,
    initial: Vec<Saved<'static>>,
    retired: Vec<Saved<'static>>,
    sections: BTreeMap<u32, Section>,
    starts: BTreeMap<u32, (u64, u64)>,
    writes: BTreeMap<u32, usize>,
}

fn saved(id: u32, class: u32, value: u32) -> Saved<'static> {
    let mut body = Vec::new();
    blob(&mut body, &value.to_le_bytes()).unwrap();
    Saved {
        id,
        position: 0,
        class,
        body: body.into(),
        deleted: false,
    }
}

fn metadata_bytes(metadata: &Metadata) -> Vec<u8> {
    let mut bytes = Vec::new();
    for value in [
        1,
        metadata.classes.len() as u32,
        metadata.strings.len() as u32 - 1,
        metadata.strings.len() as u32,
    ] {
        w32(&mut bytes, value);
    }
    for (&id, value) in &metadata.strings {
        if id != 0 {
            w32(&mut bytes, id);
            blob(&mut bytes, &[value.as_bytes(), &[0]].concat()).unwrap();
        }
    }
    w32(&mut bytes, metadata.properties.len() as u32);
    for (index, property) in metadata.properties.iter().enumerate() {
        w32(&mut bytes, u32::from(property.is_some()));
        let Some(property) = property else { continue };
        for value in [
            property.owner.namespace,
            property.owner.name,
            0,
            property.semantic,
            property.datatype_id,
            property.datatype.namespace,
            property.datatype.name,
            u32::from(property.kind),
            index as u32,
            property.offset32 as u32,
            property.offset32 as u32,
            0,
            property.name,
        ] {
            w32(&mut bytes, value);
        }
    }
    w32(&mut bytes, metadata.classes.len() as u32);
    for (index, class) in metadata.classes.iter().enumerate() {
        w32(&mut bytes, u32::from(class.is_some()));
        let Some(class) = class else { continue };
        for value in [
            class.name.namespace,
            class.name.name,
            index as u32,
            class.base,
            class.packing.uniform_width as u32,
            class.packing.class_type as u32,
        ] {
            w32(&mut bytes, value);
        }
        bytes.extend([0, u8::from(class.dynamic)]);
        for value in [
            class.size as u32,
            class.size as u32,
            4,
            class.size as u32,
            class.size as u32,
            4,
        ] {
            w32(&mut bytes, value);
        }
        w32(&mut bytes, class.properties.len() as u32);
        for &property in &class.properties {
            w32(&mut bytes, property);
        }
        for _ in 0..2 {
            w32(&mut bytes, u32::from(class.has_pointers));
            if class.has_pointers {
                assert!(matches!(index, 2 | 4));
                w32(&mut bytes, 1);
                w32(&mut bytes, if index == 2 { 0 } else { 4 });
            }
        }
    }
    w32(&mut bytes, 0);
    bytes
}

impl Fixture {
    fn new(initial: bool) -> Self {
        Self::build(initial, false, false)
    }

    fn build(initial: bool, shared: bool, gap: bool) -> Self {
        let mut metadata = container_tests::fixture();
        let scene = metadata.classes[4].as_mut().unwrap();
        scene.size = if shared { 8 } else { 4 };
        scene.properties.clear();
        scene.dynamic = false;
        metadata.properties.truncate(1);
        if shared {
            metadata.strings.insert(14, "Shared".into());
            metadata.classes[2].as_mut().unwrap().has_pointers = true;
            let scene = metadata.classes[4].as_mut().unwrap();
            scene.has_pointers = true;
            scene.properties.push(1);
            metadata.properties.push(Some(crate::native::Property {
                owner: scene.name,
                name: 14,
                datatype: pvd_protocol::Name {
                    namespace: 1,
                    name: 4,
                },
                datatype_id: 2,
                kind: 1,
                semantic: 0,
                offset32: 4,
                values: vec![],
            }));
        }
        let mut result = Self {
            metadata,
            events: vec![HistoryEvent::Marker],
            initial: Vec::new(),
            retired: Vec::new(),
            sections: BTreeMap::new(),
            starts: BTreeMap::new(),
            writes: BTreeMap::new(),
        };
        for (id, class, value) in [(1, 4, 10), (2, 4, 20), (3, 3, 77)] {
            if initial {
                let mut record = saved(id, class, value);
                if shared && class == 4 {
                    let mut body = Vec::new();
                    blob(
                        &mut body,
                        &[value.to_le_bytes(), 3u32.to_le_bytes()].concat(),
                    )
                    .unwrap();
                    record.body = body.into();
                }
                result.initial.push(record);
                result.writes.insert(id, 0);
            } else {
                result.writes.insert(id, result.events.len());
                result.events.push(HistoryEvent::Create(id, class));
                result.set(id, value);
            }
        }
        if shared && !initial {
            for id in [1, 2] {
                let created = result
                    .events
                    .iter()
                    .position(
                        |event| matches!(event, HistoryEvent::Create(value, _) if *value == id),
                    )
                    .unwrap();
                result.events.push(HistoryEvent::Set {
                    kind: 3,
                    id,
                    property: 1,
                    data: 3u32.to_le_bytes().to_vec().into(),
                    undo: vec![(result.events.len() - created) as u32],
                });
            }
        }
        for (step, (id, begin, value)) in [
            (1, true, 101),
            (1, false, 0),
            (2, true, 201),
            (2, false, 0),
            (1, true, 102),
            (2, true, 202),
            (2, false, 0),
            (1, false, 0),
            (2, true, 203),
            (2, false, 0),
            (1, true, 103),
            (1, false, 0),
            (2, true, 204),
            (1, true, 104),
            (1, false, 0),
            (2, false, 0),
        ]
        .into_iter()
        .enumerate()
        {
            result.section(id, 100 + step as u64, begin);
            if begin {
                result.set(id, value);
            }
            if gap && step == 3 {
                result.set(1, 150);
                result.set(3, 88);
            }
        }
        result
    }

    fn set(&mut self, id: u32, value: u32) {
        let current = self.events.len();
        let previous = self.writes.insert(id, current).unwrap();
        self.events.push(HistoryEvent::Set {
            kind: 3,
            id,
            property: 0,
            data: value.to_le_bytes().to_vec().into(),
            undo: vec![(current - previous) as u32],
        });
    }

    fn section(&mut self, id: u32, ticks: u64, begin: bool) {
        let ordinal = self.events.len() as u64;
        self.events.push(HistoryEvent::Section {
            class: 5,
            source: u64::from(id) * 0x111,
            ticks,
            name: 13,
            begin: u8::from(begin),
            padding: [0; 3],
        });
        if begin {
            assert!(self.starts.insert(id, (ticks, ordinal)).is_none());
        } else {
            let (start_ticks, start) = self.starts.remove(&id).unwrap();
            self.sections
                .entry(id)
                .or_insert_with(|| Section {
                    source: u64::from(id) * 0x111,
                    name: 13,
                    rows: Vec::new(),
                })
                .rows
                .push([start_ticks, start, ticks, ordinal]);
        }
    }

    fn bytes(mut self) -> Vec<u8> {
        let object_bytes = metadata_bytes(&self.metadata);
        let mut directory = Vec::new();
        w32(&mut directory, self.initial.len() as u32);
        let mut position = 4 + self.initial.len() as u64 * 12;
        for record in &mut self.initial {
            record.position = position;
            w32(&mut directory, record.id);
            w64(&mut directory, position);
            position += 12 + record.body.len() as u64;
        }
        for record in &mut self.retired {
            record.position = position;
            position += 12 + record.body.len() as u64;
            for event in &mut self.events {
                if let HistoryEvent::Saved { id, position } = event
                    && *id == record.id
                {
                    *position = record.position;
                }
            }
        }
        let destroyed = self
            .retired
            .iter()
            .map(|record| record.id)
            .collect::<BTreeSet<_>>();
        self.initial.append(&mut self.retired);
        let page = encode_page(&self.events, &mut || true).unwrap();
        let ids = (1..=3).chain(self.sections.keys().copied().filter(|id| *id > 3));
        let layout = Layout {
            source: &[],
            object_bytes: &object_bytes,
            metadata: self.metadata,
            pointer_layouts: vec![vec![]; 6],
            communication: Communication {
                clock: [1, 1],
                instances: ids
                    .clone()
                    .map(|id| Instance {
                        id,
                        source: u64::from(id) * 0x111,
                        flags: [0; 10],
                    })
                    .collect(),
                reverse: ids
                    .filter(|id| !destroyed.contains(id))
                    .map(|id| (u64::from(id) * 0x111, id))
                    .collect(),
                cameras: vec![],
                sections: self.sections.into_values().rev().collect(),
                zones: vec![],
                memory_instances: [u32::MAX; 2],
                memory_names: vec![],
            },
            metadata_padding: &[],
            history_padding: &[],
            pages: vec![Page {
                first: 0,
                position: 4096,
                count: self.events.len() as u32,
                len: page.len() as u32,
            }],
            snapshots: vec![Snapshot {
                event: 0,
                position: 0,
                len: directory.len() as u32,
                reserved: 0,
            }],
            snapshot_bytes: vec![directory.into()],
            saved: self.initial,
            events: self.events,
            backing: &[],
            gaps: vec![],
        };
        layout.encode(false, &mut || true).unwrap()
    }
}

const VALUES: [[u64; 2]; 8] = [
    [101, 20],
    [101, 201],
    [102, 202],
    [102, 202],
    [102, 203],
    [103, 203],
    [104, 204],
    [104, 204],
];
const SCENES: [u64; 8] = [1, 2, 2, 1, 2, 1, 1, 2];

fn trim(source: &[u8], first: usize, last: usize) -> Vec<u8> {
    let mut output = Vec::new();
    trim_scene_frames(source, first, last, &mut output, || true).unwrap();
    let original = Recording::from_bytes(source).unwrap();
    let restored = Recording::from_bytes(&output).unwrap();
    assert_eq!(restored.frame_count(), last - first + 1);
    for frame in (0..restored.frame_count())
        .chain((0..restored.frame_count()).rev())
        .chain([last - first, 0, last - first])
    {
        let actual = restored.state_at(frame).unwrap();
        let expected = original.state_at(frame + first).unwrap();
        assert_eq!(actual.objects, expected.objects);
        assert_eq!(actual.errors, expected.errors);
        assert_eq!(actual.debug_draw, expected.debug_draw);
        assert_eq!(
            restored.frames[frame].scene_id,
            original.frames[frame + first].scene_id
        );
        assert_eq!(
            restored.frames[frame].timestamp,
            original.frames[frame + first].timestamp
        );
    }
    restored.verify_replay().unwrap();
    let mut rewritten = Vec::new();
    rewrite(&output, &mut rewritten, || true).unwrap();
    assert_eq!(rewritten, output);
    output
}

fn assert_values(bytes: &[u8], first: usize, last: usize) {
    let recording = Recording::from_bytes(bytes).unwrap();
    for (frame, &expected) in VALUES[first..=last].iter().enumerate() {
        let state = recording.state_at(frame).unwrap();
        assert_eq!(
            state.objects[&1].properties["Value"],
            Value::U64(expected[0])
        );
        assert_eq!(
            state.objects[&2].properties["Value"],
            Value::U64(expected[1])
        );
        assert_eq!(state.objects[&3].properties["Value"], Value::U64(77));
        assert_eq!(recording.frames[frame].scene_id, SCENES[first + frame]);
    }
}

fn initial_fixed(bytes: &[u8], id: u32) -> Option<Vec<u8>> {
    let layout = Layout::parse(bytes, &mut || true).unwrap();
    let initial = initial::State::read(&layout, &mut || true).unwrap();
    let entry = initial.entries.iter().find(|entry| entry.id == id)?;
    Some(
        Reader::new(&layout.saved[entry.record].body)
            .blob()
            .unwrap()
            .to_vec(),
    )
}

#[test]
fn multi_scene_intervals_preserve_global_state_and_each_completed_frame() {
    for initial in [false, true] {
        let source = Fixture::new(initial).bytes();
        assert_values(&source, 0, 7);
        for first in [1, 2, 4, 5, 6] {
            for last in [1, 3, 4, 5, 7].into_iter().filter(|last| *last >= first) {
                let output = trim(&source, first, last);
                assert_values(&output, first, last);
                let state = Recording::from_bytes(&output).unwrap();
                assert_eq!(state.initial_state().objects.len(), 3);
            }
        }
    }
}

#[test]
fn multi_scene_initial_prefix_and_repeated_middle_keep_all_sections() {
    for initial in [false, true] {
        let source = Fixture::new(initial).bytes();
        for last in [0, 1, 3, 4, 5, 7] {
            assert_values(&trim(&source, 0, last), 0, last);
        }
        let middle = trim(&source, 2, 7);
        assert_values(&trim(&middle, 0, 1), 2, 3);
        let repeated = trim(&middle, 2, 5);
        assert_values(&repeated, 4, 7);
        let direct = trim(&source, 4, 7);
        let repeated = Recording::from_bytes(&repeated).unwrap();
        let direct = Recording::from_bytes(&direct).unwrap();
        assert_eq!(
            repeated.initial_state().objects,
            direct.initial_state().objects
        );
    }
}

#[test]
fn multi_scene_middle_rejects_both_kinds_of_split_boundary_without_output() {
    for initial in [false, true] {
        let source = Fixture::new(initial).bytes();
        for (first, last, expected) in [
            (3, 3, "starts inside"),
            (3, 7, "starts inside"),
            (7, 7, "starts inside"),
            (1, 2, "ends inside"),
            (2, 2, "ends inside"),
            (4, 6, "ends inside"),
        ] {
            let mut output = Vec::new();
            let failure =
                trim_scene_frames(&source, first, last, &mut output, || true).unwrap_err();
            assert!(failure.to_string().contains(expected), "{failure}");
            assert!(output.is_empty());
        }
    }
}

#[test]
fn multi_scene_middle_validates_global_sources_and_scene_lifetimes() {
    let source = Fixture::new(true).bytes();
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    layout.communication.instances[1].source = layout.communication.instances[0].source;
    let failure = layout.trim_middle(2, 3, &mut || true).unwrap_err();
    assert!(
        failure.to_string().contains("Scene lifecycles share"),
        "{failure}"
    );
    let source = Fixture::new(false).bytes();
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    let second_scene = layout
        .events
        .iter_mut()
        .find(|event| matches!(event, HistoryEvent::Create(2, _)))
        .unwrap();
    *second_scene = HistoryEvent::Create(2, 3);
    let failure = layout.trim_middle(2, 3, &mut || true).unwrap_err();
    assert!(
        failure
            .to_string()
            .contains("does not identify a captured Scene"),
        "{failure}"
    );
}

#[test]
fn multi_scene_middle_accepts_scene_creation_after_a_nonempty_snapshot() {
    let mut fixture = Fixture::new(true);
    let created = fixture.events.len();
    fixture.events.push(HistoryEvent::Create(4, 4));
    fixture.writes.insert(4, created);
    fixture.set(4, 40);
    fixture.section(4, 200, true);
    fixture.set(4, 401);
    fixture.section(4, 201, false);
    fixture.section(1, 202, true);
    fixture.set(1, 105);
    fixture.section(1, 203, false);
    let source = fixture.bytes();
    let output = trim(&source, 6, 9);
    let restored = Recording::from_bytes(&output).unwrap();
    assert!(!restored.initial_state().objects.contains_key(&4));
    assert!(!restored.state_at(1).unwrap().objects.contains_key(&4));
    assert_eq!(
        restored.state_at(2).unwrap().objects[&4].properties["Value"],
        Value::U64(401)
    );
    let output = trim(&output, 2, 3);
    assert_eq!(initial_fixed(&output, 4).unwrap(), 40u32.to_le_bytes());
    let restored = Recording::from_bytes(&output).unwrap();
    assert_eq!(
        restored.initial_state().objects[&4].properties["Value"],
        Value::U64(401)
    );
}

#[test]
fn multi_scene_middle_keeps_interframe_values_in_the_raw_snapshot() {
    for initial in [false, true] {
        let source = Fixture::build(initial, false, true).bytes();
        let output = trim(&source, 2, 3);
        assert_eq!(initial_fixed(&output, 1).unwrap(), 150u32.to_le_bytes());
        assert_eq!(initial_fixed(&output, 2).unwrap(), 201u32.to_le_bytes());
        assert_eq!(initial_fixed(&output, 3).unwrap(), 88u32.to_le_bytes());
        let restored = Recording::from_bytes(&output).unwrap();
        for frame in [0, 1, 0] {
            let state = restored.state_at(frame).unwrap();
            assert_eq!(state.objects[&1].properties["Value"], Value::U64(102));
            assert_eq!(state.objects[&2].properties["Value"], Value::U64(202));
            assert_eq!(state.objects[&3].properties["Value"], Value::U64(88));
        }
    }
}

#[test]
fn multi_scene_middle_preserves_shared_global_references_and_rejects_dangling_ones() {
    for initial in [false, true] {
        let source = Fixture::build(initial, true, false).bytes();
        let middle = trim(&source, 2, 7);
        let output = trim(&middle, 2, 3);
        for (id, value) in [(1, 102u32), (2, 202u32)] {
            assert_eq!(
                initial_fixed(&output, id).unwrap(),
                [value.to_le_bytes(), 3u32.to_le_bytes()].concat()
            );
        }
        let restored = Recording::from_bytes(&output).unwrap();
        for frame in [0, 1, 0] {
            let state = restored.state_at(frame).unwrap();
            assert_eq!(state.objects[&1].properties["Shared"], Value::Ref(3));
            assert_eq!(state.objects[&2].properties["Shared"], Value::Ref(3));
            assert_eq!(state.objects[&3].properties["Value"], Value::U64(77));
        }
        let mut layout = Layout::parse(&output, &mut || true).unwrap();
        let record = layout
            .saved
            .iter_mut()
            .find(|record| record.id == 1)
            .unwrap();
        record.body.to_mut()[8..12].copy_from_slice(&999u32.to_le_bytes());
        let malformed = layout.encode(false, &mut || true).unwrap();
        let mut bytes = Vec::new();
        assert!(trim_scene_frames(&malformed, 0, 0, &mut bytes, || true).is_err());
        assert!(bytes.is_empty());
    }
}

#[test]
fn multi_scene_middle_preserves_a_retired_scene_and_its_saved_body() {
    for initial in [false, true] {
        let mut fixture = Fixture::new(initial);
        let mut record = saved(2, 4, 204);
        record.deleted = true;
        fixture.retired.push(record);
        fixture
            .events
            .push(HistoryEvent::Saved { id: 2, position: 0 });
        fixture.events.push(HistoryEvent::Destroy(2));
        fixture.section(1, 200, true);
        fixture.set(1, 105);
        fixture.section(1, 201, false);
        let source = fixture.bytes();
        let middle = trim(&source, 6, 8);
        let layout = Layout::parse(&middle, &mut || true).unwrap();
        let retired = layout
            .saved
            .iter()
            .find(|record| record.id == 2 && record.deleted)
            .unwrap();
        assert_eq!(
            Reader::new(&retired.body).blob().unwrap(),
            204u32.to_le_bytes()
        );
        let restored = Recording::from_bytes(&middle).unwrap();
        assert!(restored.state_at(1).unwrap().objects.contains_key(&2));
        assert!(!restored.state_at(2).unwrap().objects.contains_key(&2));
        let output = trim(&middle, 2, 2);
        assert!(initial_fixed(&output, 2).is_none());
        assert_eq!(initial_fixed(&output, 1).unwrap(), 104u32.to_le_bytes());
        assert_eq!(initial_fixed(&output, 3).unwrap(), 77u32.to_le_bytes());
        let layout = Layout::parse(&output, &mut || true).unwrap();
        assert_eq!(layout.communication.sections.len(), 1);
        assert_eq!(layout.communication.sections[0].source, 0x111);
        assert!(layout.saved.iter().all(|record| record.id != 2));
    }
}

#[test]
fn multi_scene_frame_interval_observes_cancellation() {
    let source = Fixture::new(true).bytes();
    let layout = Layout::parse(&source, &mut || true).unwrap();
    let mut calls = 0;
    let failure = layout
        .middle_frame_interval(2, 3, &mut || {
            calls += 1;
            calls < 5
        })
        .unwrap_err();
    assert_eq!(calls, 5);
    assert_eq!(
        failure.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
}

#[test]
#[ignore = "writes constructed multi-Scene captures to PVD_NATIVE_MULTI_SCENE_DIRECTORY"]
fn retain_multi_scene_trim_fixtures() {
    let directory = std::path::PathBuf::from(
        std::env::var_os("PVD_NATIVE_MULTI_SCENE_DIRECTORY")
            .expect("set PVD_NATIVE_MULTI_SCENE_DIRECTORY"),
    );
    std::fs::create_dir(&directory).unwrap();
    for (name, fixture) in [
        ("empty", Fixture::new(false)),
        ("initial", Fixture::new(true)),
        ("shared", Fixture::build(true, true, false)),
        ("interframe", Fixture::build(true, false, true)),
    ] {
        let source = fixture.bytes();
        std::fs::write(directory.join(format!("{name}-source.pxd2")), &source).unwrap();
        let middle = trim(&source, 2, 7);
        std::fs::write(directory.join(format!("{name}-2-7.pxd2")), &middle).unwrap();
        std::fs::write(
            directory.join(format!("{name}-2-3.pxd2")),
            trim(&middle, 0, 1),
        )
        .unwrap();
        std::fs::write(
            directory.join(format!("{name}-4-5.pxd2")),
            trim(&middle, 2, 3),
        )
        .unwrap();
    }
}
