use super::*;
use crate::native::{Class, ClassPacking, Message, MessageEntry, Property, read_with_progress};
use crate::{Recording, SourceCamera};
use pvd_protocol::Name;

const MAIN_A: SourceCamera = SourceCamera {
    position: [1.0, 2.0, 3.0],
    up: [0.0, 1.0, 0.0],
    target: [4.0, 5.0, 6.0],
};
const MAIN_B: SourceCamera = SourceCamera {
    position: [10.0, 20.0, 30.0],
    ..MAIN_A
};
const MAIN_C: SourceCamera = SourceCamera {
    position: [-1.0, -2.0, -3.0],
    up: [0.0, 0.0, 1.0],
    target: [-4.0, -5.0, -6.0],
};
const LATE_A: SourceCamera = SourceCamera {
    position: [7.0, 8.0, 9.0],
    up: [1.0, 0.0, 0.0],
    target: [10.0, 11.0, 12.0],
};
const LATE_B: SourceCamera = SourceCamera {
    position: [70.0, 80.0, 90.0],
    ..LATE_A
};
const AFTER_LAST: SourceCamera = SourceCamera {
    position: [700.0, 800.0, 900.0],
    ..LATE_A
};
const ROWS: [[u64; 4]; 5] = [
    [100, 5, 101, 6],
    [200, 7, 201, 9],
    [300, 10, 301, 15],
    [400, 16, 401, 19],
    [500, 20, 501, 22],
];

fn name(namespace: u32, name: u32) -> Name {
    Name { namespace, name }
}

fn metadata() -> Metadata {
    let strings = [
        "",
        "physx3",
        "PxScene",
        "StringHandle",
        "ObjectRef",
        "PxVec3",
        "Camera",
        "Name",
        "Origin",
        "Up",
        "Target",
        "_debugger_",
        "SectionEventData",
        "frame",
        "main",
        "late",
        "renamed",
        "after last",
        "x",
        "y",
        "z",
        "PvdF32",
    ]
    .into_iter()
    .enumerate()
    .map(|(id, value)| (id as u32, value.to_owned()))
    .collect();
    let class = |name, size, properties, has_pointers, string_offsets| {
        Some(Class {
            name,
            base: u32::MAX,
            packing: ClassPacking::default(),
            size,
            properties,
            dynamic: false,
            has_pointers,
            string_offsets,
        })
    };
    let camera = name(0, 6);
    let vector = name(1, 5);
    let mut metadata = Metadata {
        strings,
        classes: vec![
            class(name(1, 2), 0, vec![], false, vec![]),
            class(name(1, 3), 4, vec![], true, vec![0]),
            class(name(1, 4), 4, vec![], true, vec![]),
            class(vector, 12, vec![], false, vec![]),
            class(camera, 40, vec![0, 1, 2, 3], true, vec![0]),
            class(name(11, 12), 24, vec![], false, vec![]),
        ],
        properties: [
            (7, name(1, 3), 1, 0),
            (8, vector, 3, 4),
            (9, vector, 3, 16),
            (10, vector, 3, 28),
        ]
        .into_iter()
        .map(|(label, datatype, datatype_id, offset32)| {
            Some(Property {
                owner: camera,
                name: label,
                datatype,
                datatype_id,
                kind: 1,
                semantic: 0,
                offset32,
                values: vec![],
            })
        })
        .collect(),
        messages: vec![Some(Message {
            class_name: camera,
            class: 4,
            size: 36,
            entries: (1..=3)
                .map(|property| MessageEntry {
                    datatype: vector,
                    datatype_id: 3,
                    property,
                    offset: (property as usize - 1) * 12,
                    size: 12,
                    destination_size: 12,
                })
                .collect(),
        })],
    };
    metadata.classes.resize_with(74, || None);
    metadata.classes[73] = class(name(1, 21), 4, vec![], false, vec![]);
    metadata.classes[73].as_mut().unwrap().packing = ClassPacking {
        uniform_width: 4,
        class_type: 73,
    };
    let vector_class = metadata.classes[3].as_mut().unwrap();
    vector_class.properties = vec![4, 5, 6];
    vector_class.packing = ClassPacking {
        uniform_width: 4,
        class_type: 73,
    };
    for (index, label) in [18, 19, 20].into_iter().enumerate() {
        metadata.properties.push(Some(Property {
            owner: vector,
            name: label,
            datatype: name(1, 21),
            datatype_id: 73,
            kind: 1,
            semantic: 0,
            offset32: index * 4,
            values: vec![],
        }));
    }
    metadata
}

fn raw_words(values: &[u32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

fn vectors(value: SourceCamera) -> Vec<u8> {
    value
        .position
        .into_iter()
        .chain(value.up)
        .chain(value.target)
        .flat_map(f32::to_le_bytes)
        .collect()
}

fn set(kind: u32, id: u32, property: u32, bytes: &[u8], undo: &[u32]) -> Vec<u8> {
    let mut event = raw_words(&[kind, id, property]);
    blob(&mut event, bytes).unwrap();
    w32(&mut event, undo.len() as u32);
    event.extend(raw_words(undo));
    event
}

fn section(ticks: u64, begin: bool) -> Vec<u8> {
    let mut data = 0x111u64.to_le_bytes().to_vec();
    w64(&mut data, ticks);
    w32(&mut data, 13);
    data.extend([u8::from(begin), 0, 0, 0]);
    let mut event = raw_words(&[8, 5]);
    blob(&mut event, &data).unwrap();
    event
}

fn saved_event(id: u32, position: u64) -> Vec<u8> {
    let mut event = raw_words(&[10, id]);
    w64(&mut event, position);
    event
}

struct SavedInput {
    id: u32,
    position: usize,
    bytes: Vec<u8>,
    deleted: bool,
}

struct Fixture {
    metadata: Metadata,
    cameras: Vec<(u32, u32)>,
    initial: Vec<u8>,
    saved: Vec<SavedInput>,
    events: Vec<Vec<u8>>,
    rows: Vec<[u64; 4]>,
}

impl Fixture {
    fn new() -> Self {
        let mut fixed = 14u32.to_le_bytes().to_vec();
        fixed.extend(vectors(MAIN_C));
        let mut body = Vec::new();
        blob(&mut body, &fixed).unwrap();
        let mut saved = raw_words(&[4, 7]);
        blob(&mut saved, &body).unwrap();
        Self {
            metadata: metadata(),
            cameras: vec![(17, 9), (14, 7), (15, 8)],
            initial: vec![0; 4],
            rows: ROWS.to_vec(),
            saved: vec![SavedInput {
                id: 7,
                position: 128,
                bytes: saved,
                deleted: true,
            }],
            events: vec![
                raw_words(&[9]),
                raw_words(&[1, 1, 0]),
                raw_words(&[1, 7, 4]),
                set(3, 7, 0, &14u32.to_le_bytes(), &[1]),
                set(4, 7, 0, &vectors(MAIN_A), &[2]),
                section(100, true),
                section(101, false),
                section(200, true),
                set(3, 7, 1, &vectors(MAIN_B)[..12], &[4]),
                section(201, false),
                section(300, true),
                set(4, 7, 0, &vectors(MAIN_C), &[7, 3]),
                raw_words(&[1, 8, 4]),
                set(3, 8, 0, &15u32.to_le_bytes(), &[1]),
                set(4, 8, 0, &vectors(LATE_A), &[2]),
                section(301, false),
                section(400, true),
                saved_event(7, 128),
                raw_words(&[2, 7]),
                section(401, false),
                section(500, true),
                set(4, 8, 0, &vectors(LATE_B), &[7]),
                section(501, false),
                set(4, 8, 0, &vectors(AFTER_LAST), &[2]),
                raw_words(&[1, 9, 4]),
                set(3, 9, 0, &17u32.to_le_bytes(), &[1]),
                set(4, 9, 0, &vectors(AFTER_LAST), &[2]),
            ],
        }
    }

    fn bytes(&self) -> Vec<u8> {
        let metadata = middle::tests::metadata_bytes(&self.metadata);
        let mut comm = raw_words(&[1]);
        w64(&mut comm, 1);
        w64(&mut comm, 1);
        comm.extend(raw_words(&[1, 1]));
        w64(&mut comm, 0x111);
        w32(&mut comm, 1);
        comm.extend([0; 10]);
        w32(&mut comm, 1);
        w64(&mut comm, 0x111);
        w32(&mut comm, 1);
        w32(&mut comm, self.cameras.len() as u32);
        for &(name, id) in &self.cameras {
            comm.extend(raw_words(&[name, id]));
        }
        w32(&mut comm, 1);
        w64(&mut comm, 0x111);
        comm.extend(raw_words(&[13, self.rows.len() as u32]));
        for row in &self.rows {
            for &word in row {
                w64(&mut comm, word);
            }
        }
        comm.extend(raw_words(&[0, u32::MAX, u32::MAX, 0]));

        let mut page = raw_words(&[self.events.len() as u32]);
        let mut offset = 0;
        for event in &self.events {
            w32(&mut page, offset);
            offset += event.len() as u32;
        }
        for event in &self.events {
            page.extend(event);
        }
        let mut bytes = raw_words(&[MAGIC, 1, metadata.len() as u32, comm.len() as u32]);
        bytes.extend(metadata);
        bytes.extend(comm);
        bytes.resize(bytes.len().next_multiple_of(4096), 0);
        w64(&mut bytes, self.events.len() as u64);
        w32(&mut bytes, 1);
        w64(&mut bytes, 0);
        w64(&mut bytes, 4096);
        w32(&mut bytes, self.events.len() as u32);
        w32(&mut bytes, page.len() as u32);
        w32(&mut bytes, 1);
        bytes.extend([0; 16]);
        w32(&mut bytes, self.initial.len() as u32);
        w32(&mut bytes, 0);
        w32(
            &mut bytes,
            self.saved.iter().filter(|saved| saved.deleted).count() as u32,
        );
        for saved in self.saved.iter().filter(|saved| saved.deleted) {
            w32(&mut bytes, saved.id);
            w64(&mut bytes, saved.position as u64);
            w32(&mut bytes, saved.bytes.len() as u32);
        }
        bytes.resize(bytes.len().next_multiple_of(4096), 0);
        let mut backing = vec![0; 4096];
        backing[..self.initial.len()].copy_from_slice(&self.initial);
        for saved in &self.saved {
            backing[saved.position..saved.position + saved.bytes.len()]
                .copy_from_slice(&saved.bytes);
        }
        bytes.extend(backing);
        bytes.extend(page);
        bytes
    }
}

fn expected(frame: usize) -> BTreeMap<String, SourceCamera> {
    let cameras: &[(&str, SourceCamera)] = match frame {
        0 => &[("main", MAIN_A)],
        1 => &[("main", MAIN_B)],
        2 => &[("main", MAIN_C), ("late", LATE_A)],
        3 => &[("late", LATE_A)],
        4 => &[("late", LATE_B)],
        _ => panic!("unexpected frame"),
    };
    cameras
        .iter()
        .map(|&(name, value)| (name.into(), value))
        .collect()
}

fn assert_frames(recording: &Recording, last: usize) {
    assert_eq!(recording.frame_count(), last + 1);
    for frame in (0..=last)
        .chain((0..=last).rev())
        .chain([last, 0, last / 2])
    {
        let actual = recording.state_at(frame).unwrap();
        assert_eq!(actual.cameras, expected(frame), "frame {frame}");
        assert_eq!(recording.frames[frame].timestamp, ROWS[frame][2]);
        assert_eq!(recording.frames[frame].scene_id, 1);
    }
    recording.verify_replay().unwrap();
}

fn assert_disk_frames(bytes: &[u8], memory: &Recording, last: usize) {
    let mut config = crate::native::container_tests::file_history_config("camera-prefix");
    config.segment_frames = 2;
    let directory = config.directory.clone();
    let temporary_root = std::fs::canonicalize(std::env::temp_dir()).unwrap();
    let expected_directory = temporary_root.join(directory.file_name().unwrap());
    assert_eq!(
        directory.parent().unwrap().canonicalize().unwrap(),
        temporary_root
    );
    assert!(!directory.exists());
    let (disk, _) = crate::native::import::read_source_with_progress(
        std::io::Cursor::new(bytes),
        Some(config),
        |_, _| true,
    )
    .unwrap();
    assert!(disk.is_disk_backed());
    let stats = disk.disk_usage().unwrap();
    assert_eq!(stats.first_frame, 0);
    assert_eq!(stats.retained_frames, last + 1);
    assert!(stats.segments >= (last + 1).div_ceil(2));
    assert_frames(&disk, last);
    let mut cursor = crate::live::ReplayCursor::default();
    let mut state = crate::SceneState::default();
    for frame in (0..=last)
        .chain((0..=last).rev())
        .chain([last, 0, last / 2])
    {
        cursor
            .seek(&disk, frame, || false)
            .unwrap()
            .update
            .apply(&mut state);
        assert_eq!(state.cameras, expected(frame), "cursor frame {frame}");
        assert_eq!(state, memory.state_at(frame).unwrap());
        assert_eq!(state, disk.state_at(frame).unwrap());
    }
    drop(state);
    drop(cursor);
    drop(disk);
    let resolved = directory.canonicalize().unwrap();
    assert_eq!(resolved, expected_directory);
    assert_eq!(resolved.parent(), Some(temporary_root.as_path()));
    assert_ne!(resolved, temporary_root);
    std::fs::remove_dir_all(&resolved).unwrap();
    assert!(!directory.exists());
}

fn retain(name: &str, bytes: &[u8]) {
    if let Some(directory) = std::env::var_os("PVD_NATIVE_CONSTRUCTED_EXPORT_DIRECTORY") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join(format!("camera-prefix-{name}.pxd2")), bytes).unwrap();
    }
}

#[test]
fn camera_prefix_preserves_raw_events_directory_lifetimes_and_frame_vectors() {
    let fixture = Fixture::new();
    let source = fixture.bytes();
    retain("source", &source);
    let (original, info) = read_with_progress(&source, |_, _| true).unwrap();
    assert_frames(&original, 4);
    assert_disk_frames(&source, &original, 4);
    for id in [7, 8, 9] {
        assert!(!info.source_ids.contains_key(&id));
    }
    let original_layout = Layout::parse(&source, &mut || true).unwrap();
    assert_eq!(original_layout.pointer_layouts[1], [(2, 0)]);
    assert_eq!(original_layout.pointer_layouts[4], [(2, 0)]);
    for last in 0..5 {
        let mut output = Vec::new();
        let report = trim_scene_frames(&source, 0, last, &mut output, || true).unwrap();
        assert_eq!((report.source_frames, report.frames), (5, last + 1));
        retain(&last.to_string(), &output);
        let restored = read_with_progress(&output, |_, _| true).unwrap().0;
        assert_frames(&restored, last);
        assert_disk_frames(&output, &restored, last);
        for frame in 0..=last {
            assert_eq!(
                restored.state_at(frame).unwrap(),
                original.state_at(frame).unwrap()
            );
        }
        let layout = Layout::parse(&output, &mut || true).unwrap();
        let directory: &[(u32, u32)] = if last < 2 {
            &[(14, 7)]
        } else {
            &[(14, 7), (15, 8)]
        };
        assert_eq!(layout.communication.cameras, directory);
        assert_eq!(layout.object_bytes, original_layout.object_bytes);
        assert_eq!(layout.communication.sections[0].rows, ROWS[..=last]);
        let cut = [7, 10, 16, 20, 23][last];
        assert_eq!(layout.events.len(), cut);
        for (ordinal, event) in layout.events.iter().enumerate() {
            let mut bytes = Vec::new();
            event.encode(&mut bytes).unwrap();
            let expected = if ordinal == 17 {
                saved_event(7, 4)
            } else {
                fixture.events[ordinal].clone()
            };
            assert_eq!(bytes, expected, "event {ordinal}");
        }
        if last >= 3 {
            assert_eq!(layout.saved.len(), 1);
            assert_eq!(layout.saved[0].position, 4);
            assert_eq!(layout.saved[0].body.as_ref(), &fixture.saved[0].bytes[12..]);
            assert!(layout.saved[0].deleted);
        } else {
            assert!(layout.saved.is_empty());
        }
        let mut rewritten = Vec::new();
        rewrite(&output, &mut rewritten, || true).unwrap();
        assert_eq!(rewritten, output);
        let mut repeated = Vec::new();
        trim_scene_frames(&output, 0, 0, &mut repeated, || true).unwrap();
        let repeated_recording = read_with_progress(&repeated, |_, _| true).unwrap().0;
        assert_frames(&repeated_recording, 0);
        assert_disk_frames(&repeated, &repeated_recording, 0);
        assert_eq!(
            Layout::parse(&repeated, &mut || true)
                .unwrap()
                .communication
                .cameras,
            [(14, 7)]
        );
    }
}

fn rejected(source: &[u8], first: usize, last: usize, expected: &str) {
    let mut output = Vec::new();
    let error = trim_scene_frames(source, first, last, &mut output, || true).unwrap_err();
    assert!(
        error.to_string().contains(expected),
        "expected {expected:?}, got {error}"
    );
    assert!(output.is_empty());
}

#[test]
fn camera_prefix_keeps_unnamed_instances_and_captured_vector_bits() {
    let mut fixture = Fixture::new();
    fixture.events[3] = set(3, 7, 0, &0u32.to_le_bytes(), &[1]);
    fixture.events[8] = set(3, 7, 0, &14u32.to_le_bytes(), &[5]);
    fixture.events[11] = set(4, 7, 0, &vectors(MAIN_C), &[7]);
    let source = fixture.bytes();
    let mut output = Vec::new();
    trim_scene_frames(&source, 0, 1, &mut output, || true).unwrap();
    let recording = read_with_progress(&output, |_, _| true).unwrap().0;
    let first = recording.state_at(0).unwrap();
    assert!(first.cameras.is_empty());
    assert_eq!(first.objects[&7].type_name, "Camera");
    assert_eq!(recording.state_at(1).unwrap().cameras, expected(0));
    assert_eq!(
        Layout::parse(&output, &mut || true)
            .unwrap()
            .communication
            .cameras,
        [(14, 7)]
    );
    let mut prefix = Vec::new();
    trim_scene_frames(&output, 0, 0, &mut prefix, || true).unwrap();
    assert!(
        read_with_progress(&prefix, |_, _| true)
            .unwrap()
            .0
            .state_at(0)
            .unwrap()
            .cameras
            .is_empty()
    );
    assert_eq!(
        Layout::parse(&prefix, &mut || true)
            .unwrap()
            .communication
            .cameras,
        [(14, 7)]
    );

    let value = SourceCamera {
        position: [f32::from_bits(0x7fc0_5678), f32::INFINITY, -0.0],
        up: [0.0; 3],
        ..MAIN_A
    };
    let mut fixture = Fixture::new();
    fixture.events[4] = set(4, 7, 0, &vectors(value), &[2]);
    let mut output = Vec::new();
    trim_scene_frames(&fixture.bytes(), 0, 0, &mut output, || true).unwrap();
    let state = read_with_progress(&output, |_, _| true)
        .unwrap()
        .0
        .state_at(0)
        .unwrap();
    assert_eq!(state.cameras, BTreeMap::from([("main".into(), value)]));
    let layout = Layout::parse(&output, &mut || true).unwrap();
    let mut raw = Vec::new();
    layout.events[4].encode(&mut raw).unwrap();
    assert_eq!(raw, fixture.events[4]);
}

#[test]
fn camera_prefix_checks_the_full_source_before_dropping_future_directory_rows() {
    let mut fixture = Fixture::new();
    fixture.cameras[0].1 = 99;
    rejected(
        &fixture.bytes(),
        0,
        0,
        "camera directory targets a missing instance",
    );
    let mut fixture = Fixture::new();
    fixture.cameras[0].1 = 1;
    rejected(
        &fixture.bytes(),
        0,
        0,
        "camera directory targets a non-camera instance",
    );
    let mut fixture = Fixture::new();
    fixture.events[25] = set(3, 9, 0, &16u32.to_le_bytes(), &[1]);
    rejected(
        &fixture.bytes(),
        0,
        0,
        "camera name differs from its directory",
    );
    let mut fixture = Fixture::new();
    fixture.cameras[0].0 = 14;
    rejected(
        &fixture.bytes(),
        0,
        0,
        "duplicate PVD3 camera name or instance",
    );
    let mut fixture = Fixture::new();
    fixture.cameras[0].1 = 7;
    rejected(
        &fixture.bytes(),
        0,
        0,
        "duplicate PVD3 camera name or instance",
    );
    let mut fixture = Fixture::new();
    fixture.metadata.strings.insert(22, "main".into());
    fixture.cameras[0].0 = 22;
    rejected(
        &fixture.bytes(),
        0,
        0,
        "duplicate PVD3 camera name or instance",
    );
    let mut fixture = Fixture::new();
    fixture.metadata.properties[1].as_mut().unwrap().offset32 = 8;
    rejected(&fixture.bytes(), 0, 0, "Camera property layout");
    let mut fixture = Fixture::new();
    let mut fixed = 17u32.to_le_bytes().to_vec();
    fixed.extend(vectors(AFTER_LAST));
    let mut body = Vec::new();
    blob(&mut body, &fixed).unwrap();
    let mut record = raw_words(&[4, 9]);
    blob(&mut record, &body).unwrap();
    fixture.saved.push(SavedInput {
        id: 9,
        position: 256,
        bytes: record,
        deleted: true,
    });
    fixture.events.push(saved_event(9, 256));
    fixture.events.push(raw_words(&[2, 9]));
    fixture.events.push(raw_words(&[1, 9, 4]));
    let source = fixture.bytes();
    assert!(read_with_progress(&source, |_, _| true).is_ok());
    rejected(&source, 0, 0, "container local ID reuse");
}

#[test]
fn camera_prefix_preserves_an_initial_scene_and_rejects_origin_shift() {
    let mut fixture = Fixture::new();
    fixture.initial = raw_words(&[1, 1]);
    w64(&mut fixture.initial, 32);
    fixture.events[1] = raw_words(&[9]);
    fixture.saved.push(SavedInput {
        id: 1,
        position: 32,
        bytes: raw_words(&[0, 1, 4, 0]),
        deleted: false,
    });
    let source = fixture.bytes();
    let mut output = Vec::new();
    trim_scene_frames(&source, 0, 0, &mut output, || true).unwrap();
    assert_frames(&read_with_progress(&output, |_, _| true).unwrap().0, 0);

    let mut fixture = Fixture::new();
    fixture.events[8] = raw_words(&[11, 1, 1.0f32.to_bits(), 0, 0, 0]);
    let source = fixture.bytes();
    assert!(read_with_progress(&source, |_, _| true).is_ok());
    rejected(
        &source,
        0,
        1,
        "camera trim does not support OriginShift history",
    );
    rejected(
        &source,
        1,
        2,
        "camera trim does not support OriginShift history",
    );
}

#[test]
fn camera_prefix_cancellation_never_publishes_an_unchecked_prefix() {
    let source = Fixture::new().bytes();
    let mut total = 0;
    let mut complete = Vec::new();
    trim_scene_frames(&source, 0, 4, &mut complete, || {
        total += 1;
        true
    })
    .unwrap();
    assert!(complete.len() < 64 << 10);
    for limit in [0, 1, 8, 32, 128, total / 2, total - 2] {
        let mut calls = 0;
        let mut output = Vec::new();
        let error = trim_scene_frames(&source, 0, 4, &mut output, || {
            let proceed = calls < limit;
            calls += 1;
            proceed
        })
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
        assert!(output.is_empty());
    }
}

#[path = "camera_tests/trim.rs"]
mod trim_tests;

mod multi_scene;
