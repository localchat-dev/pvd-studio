use super::*;
use crate::analysis::MemoryEventKind;
use crate::analysis_export::{AnalysisCsvKind, write_analysis_csv};
use crate::{MemoryHistoryScope, Recording, Value};

const OLD_ALLOCATION: &[u8] = &[2, 0, 64, 20, 21, 7, 42];
const FIRST_ACTIVITY: &[u8] = &[3, 0, 42, 2, 0, 16, 20, 21, 7, 43];
const FREE_B: &[u8] = &[3, 0, 43];
const TAIL_ACTIVITY: &[u8] = &[2, 0, 8, 20, 21, 7, 44];

#[derive(Clone, Copy)]
enum Step<'a> {
    Buffer(&'a [u8]),
    Totals(&'a [i32]),
    Begin(u64),
    End(u64),
}

fn metadata() -> Metadata {
    let mut metadata = fixture_metadata();
    for (id, text) in [
        (13, "_debugger"),
        (14, "PvdU8"),
        (15, "PvdI32"),
        (16, "PxProfileMemoryEventBuffer"),
        (17, "MemoryEventTotals"),
        (18, "events"),
        (19, "totals"),
        (20, "Allocation"),
        (21, "source.cpp"),
    ] {
        metadata.strings.insert(id, text.into());
    }
    for (label, size, properties, dynamic) in [
        (name(1, 14), 1, vec![], false),
        (name(1, 15), 4, vec![], false),
        (name(10, 16), 24, vec![4], true),
        (name(13, 17), 24, vec![5], true),
    ] {
        metadata.classes.push(Some(Class {
            name: label,
            base: u32::MAX,
            packing: crate::native::ClassPacking::default(),
            size,
            properties,
            dynamic,
            has_pointers: false,
            string_offsets: vec![],
        }));
    }
    for (owner, label, datatype, datatype_id) in [
        (name(10, 16), 18, name(1, 14), 5),
        (name(13, 17), 19, name(1, 15), 6),
    ] {
        metadata.properties.push(Some(Property {
            owner,
            name: label,
            datatype,
            datatype_id,
            kind: 2,
            semantic: 0,
            offset32: 0,
            values: vec![],
        }));
    }
    metadata
}

fn append(encoded: &mut Vec<Vec<u8>>, event: HistoryEvent<'_>) {
    let mut bytes = Vec::new();
    event.encode(&mut bytes).unwrap();
    encoded.push(bytes);
}

fn fixture(steps: &[Step<'_>]) -> Vec<u8> {
    let source = super::fixture();
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    layout.metadata = metadata();
    let object_bytes = metadata_bytes(&layout.metadata);
    layout.object_bytes = &object_bytes;
    layout
        .pointer_layouts
        .resize(layout.metadata.classes.len(), vec![]);
    let mut encoded = Vec::new();
    append(&mut encoded, HistoryEvent::Marker);
    append(&mut encoded, HistoryEvent::Create(1, 0));
    append(&mut encoded, HistoryEvent::Create(2, 7));
    append(&mut encoded, HistoryEvent::Create(3, 8));
    let mut previous = [2usize, 3];
    let mut begin = None;
    let mut rows = Vec::new();
    for step in steps {
        match *step {
            Step::Buffer(data) => {
                let current = encoded.len();
                append(
                    &mut encoded,
                    HistoryEvent::Set {
                        kind: 3,
                        id: 2,
                        property: 4,
                        data: data.into(),
                        undo: vec![(current - previous[0]) as u32],
                    },
                );
                previous[0] = current;
            }
            Step::Totals(values) => {
                let current = encoded.len();
                let bytes: Vec<_> = values
                    .iter()
                    .flat_map(|value| value.to_le_bytes())
                    .collect();
                append(
                    &mut encoded,
                    HistoryEvent::Set {
                        kind: 3,
                        id: 3,
                        property: 5,
                        data: bytes.as_slice().into(),
                        undo: vec![(current - previous[1]) as u32],
                    },
                );
                previous[1] = current;
            }
            Step::Begin(ticks) => {
                assert!(begin.replace((ticks, encoded.len() as u64)).is_none());
                encoded.push(section(ticks, true));
            }
            Step::End(ticks) => {
                let (ticks_before, event_before) = begin.take().unwrap();
                rows.push([ticks_before, event_before, ticks, encoded.len() as u64]);
                encoded.push(section(ticks, false));
            }
        }
    }
    assert!(begin.is_none());
    layout.events = encoded
        .iter()
        .enumerate()
        .map(|(ordinal, data)| {
            HistoryEvent::parse(data, ordinal as u64, &layout.metadata, &mut 0).unwrap()
        })
        .collect();
    layout.communication.instances = vec![
        Instance {
            id: 1,
            source: 0x111,
            flags: [0; 10],
        },
        Instance {
            id: 2,
            source: 0x222,
            flags: [0; 10],
        },
    ];
    layout.communication.reverse = vec![(0x111, 1), (0x222, 2)];
    layout.communication.sections[0].rows = rows;
    layout.communication.zones.clear();
    layout.communication.memory_instances = [2, 3];
    layout.communication.memory_names = vec![
        (b"Future\0", b"future.cpp\0", 9, 2),
        (b"Allocation\0", b"source.cpp\0", 7, 0),
        (b"Other\0", b"other.cpp\0", 8, 1),
    ];
    layout.saved.clear();
    encode_modified(&mut layout)
}

fn source() -> Vec<u8> {
    fixture(&[
        Step::Buffer(OLD_ALLOCATION),
        Step::Totals(&[64]),
        Step::Begin(100),
        Step::End(101),
        Step::Begin(200),
        Step::Buffer(FIRST_ACTIVITY),
        Step::Totals(&[16]),
        Step::Buffer(&[]),
        Step::End(201),
        Step::Begin(300),
        Step::Buffer(FREE_B),
        Step::Totals(&[0]),
        Step::End(301),
        Step::Buffer(TAIL_ACTIVITY),
        Step::Totals(&[8]),
    ])
}

fn trim(source: &[u8], first: usize, last: usize) -> (Vec<u8>, Recording) {
    let mut output = Vec::new();
    trim_scene_frames(source, first, last, &mut output, || true).unwrap();
    let restored = Recording::from_bytes(&output).unwrap();
    let original = Recording::from_bytes(source).unwrap();
    assert_eq!(restored.frame_count(), last - first + 1);
    for frame in (0..restored.frame_count())
        .chain((0..restored.frame_count()).rev())
        .chain([0, last - first, 0])
    {
        assert_eq!(
            restored.state_at(frame).unwrap(),
            original.state_at(frame + first).unwrap()
        );
    }
    restored.verify_replay().unwrap();
    let mut rewritten = Vec::new();
    rewrite(&output, &mut rewritten, || true).unwrap();
    assert_eq!(rewritten, output);
    (output, restored)
}

fn rows(totals: &crate::NativeMemoryTotals) -> Vec<i32> {
    (0..totals.len())
        .map(|index| totals.row(index).unwrap().bytes)
        .collect()
}

fn persist(label: &str, source: &[u8], output: &[u8]) {
    if let Some(directory) = std::env::var_os("PVD_NATIVE_CONSTRUCTED_EXPORT_DIRECTORY") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join(format!("memory-middle-{label}-source.pxd2")),
            source,
        )
        .unwrap();
        std::fs::write(
            directory.join(format!("memory-middle-{label}-trimmed.pxd2")),
            output,
        )
        .unwrap();
    }
}

#[test]
fn middle_memory_preserves_source_totals_and_only_real_interval_activity() {
    let source = source();
    let (output, restored) = trim(&source, 1, 2);
    let original = Recording::from_bytes(&source).unwrap();
    assert_eq!(
        original.memory_history_scope(),
        MemoryHistoryScope::CapturedEvents
    );
    assert_eq!(
        restored.memory_history_scope(),
        MemoryHistoryScope::AfterInitialSnapshot
    );
    let layout = Layout::parse(&output, &mut || true).unwrap();
    let initial = initial::State::read(&layout, &mut || true).unwrap();
    let entry = initial.entries.iter().find(|entry| entry.id == 2).unwrap();
    let mut body = Reader::new(&layout.saved[entry.record].body);
    assert_eq!(body.blob().unwrap(), [0; 24]);
    assert_eq!(body.blob().unwrap(), OLD_ALLOCATION);
    body.end().unwrap();
    assert!(
        matches!(&restored.state_at(0).unwrap().objects[&2].properties["events"], Value::Bytes(bytes) if bytes.is_empty())
    );
    assert_eq!(
        rows(&restored.native_initial_memory_totals().unwrap().unwrap()),
        vec![64]
    );
    for (frame, expected) in [(0, vec![16]), (1, vec![0])] {
        let state = restored.state_at(frame).unwrap();
        assert_eq!(
            rows(&restored.native_memory_totals_at(&state).unwrap().unwrap()),
            expected
        );
        assert_eq!(
            rows(
                &original
                    .native_memory_totals_at(&original.state_at(frame + 1).unwrap())
                    .unwrap()
                    .unwrap()
            ),
            expected
        );
    }
    assert_eq!(
        rows(&restored.native_end_memory_totals().unwrap().unwrap()),
        vec![0]
    );
    assert_eq!(
        rows(&original.native_end_memory_totals().unwrap().unwrap()),
        vec![8]
    );
    let memory = &restored.analysis.legacy().unwrap().memory;
    assert_eq!(
        (
            memory.allocations,
            memory.frees,
            memory.unmatched_frees,
            memory.peak_bytes,
            memory.live_bytes
        ),
        (1, 2, 1, 16, 0)
    );
    assert_eq!(
        memory
            .events()
            .iter()
            .map(|event| (event.address, event.received_frame))
            .collect::<Vec<_>>(),
        vec![(42, 0), (43, 0), (43, 1)]
    );
    assert!(matches!(
        &memory.events()[0].kind,
        MemoryEventKind::Free { allocation: None }
    ));
    assert!(
        matches!(&memory.events()[1].kind, MemoryEventKind::Allocate { allocation, replaced: None } if allocation.size == 16 && allocation.type_name.as_ref() == "Allocation" && allocation.received_frame == 0)
    );
    assert!(
        matches!(&memory.events()[2].kind, MemoryEventKind::Free { allocation: Some(allocation) } if allocation.size == 16 && allocation.received_frame == 0)
    );
    assert_eq!(
        memory
            .samples
            .iter()
            .map(|sample| (
                sample.received_frame,
                sample.live_bytes,
                sample.live_allocations,
                sample.allocations,
                sample.frees
            ))
            .collect::<Vec<_>>(),
        vec![(0, 16, 1, 1, 1), (0, 16, 1, 1, 1), (1, 0, 0, 1, 2)]
    );
    let mut csv = Vec::new();
    assert!(
        write_analysis_csv(&restored, &mut csv, AnalysisCsvKind::Memory, |_| true)
            .unwrap_err()
            .to_string()
            .contains("before the initial snapshot")
    );
    assert!(csv.is_empty());
    assert_eq!(
        layout
            .communication
            .memory_names
            .iter()
            .map(|row| row.3)
            .collect::<Vec<_>>(),
        vec![2, 0, 1]
    );
    let first_undo = layout
        .events
        .iter()
        .enumerate()
        .filter_map(|(ordinal, event)| match event {
            HistoryEvent::Set {
                id: 2 | 3, undo, ..
            } => Some((ordinal, undo.clone())),
            _ => None,
        })
        .take(2)
        .collect::<Vec<_>>();
    assert_eq!(first_undo, vec![(2, vec![2]), (3, vec![3])]);
    persist("partial", &source, &output);
}

#[test]
fn middle_memory_zero_net_prehistory_and_totals_only_changes_stay_partial() {
    let source = fixture(&[
        Step::Buffer(OLD_ALLOCATION),
        Step::Buffer(&[3, 0, 42]),
        Step::Totals(&[0]),
        Step::Begin(100),
        Step::End(101),
        Step::Totals(&[-1, 4]),
        Step::Begin(200),
        Step::End(201),
    ]);
    let (output, restored) = trim(&source, 1, 1);
    let memory = &restored.analysis.legacy().unwrap().memory;
    assert!(memory.events().is_empty() && memory.samples.is_empty());
    assert_eq!(
        restored.memory_history_scope(),
        MemoryHistoryScope::AfterInitialSnapshot
    );
    let totals = restored.native_initial_memory_totals().unwrap().unwrap();
    assert_eq!(rows(&totals), vec![-1, 4]);
    assert_eq!(totals.nonnegative_bytes(), None);
    assert_eq!(totals.len(), 2);
    assert!(totals.row(2).is_none());
    persist("no-events", &source, &output);
}

#[test]
fn middle_memory_first_frame_gap_rejects_nonempty_and_empty_flushes_without_writing() {
    for gap in [&[3, 0, 42][..], &[]] {
        let source = fixture(&[
            Step::Buffer(OLD_ALLOCATION),
            Step::Totals(&[64]),
            Step::Begin(100),
            Step::End(101),
            Step::Buffer(gap),
            Step::Begin(200),
            Step::End(201),
        ]);
        let mut output = Vec::new();
        let failure = trim_scene_frames(&source, 1, 1, &mut output, || true).unwrap_err();
        assert!(failure.to_string().contains("first-frame gap"), "{failure}");
        assert!(output.is_empty());
    }
}

fn modified(source: &[u8], change: impl FnOnce(&mut Layout<'_>)) -> Vec<u8> {
    let mut layout = Layout::parse(source, &mut || true).unwrap();
    change(&mut layout);
    let object_bytes = metadata_bytes(&layout.metadata);
    layout.object_bytes = &object_bytes;
    encode_modified(&mut layout)
}

#[test]
fn middle_memory_unverified_pair_catalog_schema_and_lifetimes_refuse_before_output() {
    let source = source();
    for bytes in [
        modified(&source, |layout| {
            layout.communication.memory_instances[1] = u32::MAX
        }),
        modified(&source, |layout| layout.communication.memory_names[0].3 = 0),
        modified(&source, |layout| {
            let field = layout.metadata.properties[5].as_mut().unwrap();
            field.datatype = name(1, 3);
            field.datatype_id = 1;
        }),
        modified(&source, |layout| {
            layout.events.push(HistoryEvent::Create(4, 7))
        }),
        modified(&source, |layout| {
            if let HistoryEvent::Set { undo, .. } = &mut layout.events[9] {
                *undo = vec![4];
            }
        }),
    ] {
        let mut output = Vec::new();
        assert!(trim_scene_frames(&bytes, 1, 2, &mut output, || true).is_err());
        assert!(output.is_empty());
    }
}

#[test]
fn middle_memory_proof_rejects_initial_payload_sources_history_and_undo_tampering() {
    let source = source();
    let (output, _) = trim(&source, 1, 2);
    let layout = Layout::parse(&source, &mut || true).unwrap();
    let proof = history::prepare(&layout, 1, 2, &mut || true).unwrap();
    for variant in 0..6 {
        let mut changed = Layout::parse(&output, &mut || true).unwrap();
        match variant {
            0 => {
                let record = changed
                    .saved
                    .iter_mut()
                    .find(|record| record.id == 2 && !record.deleted)
                    .unwrap();
                let mut body = record.body.to_vec();
                *body.last_mut().unwrap() ^= 1;
                record.body = body.into();
            }
            1 => changed.communication.memory_names[1].1 = b"changed.cpp\0",
            2 => {
                let event = changed
                    .events
                    .iter_mut()
                    .find(|event| matches!(event, HistoryEvent::Set { id: 2, .. }))
                    .unwrap();
                if let HistoryEvent::Set { data, .. } = event {
                    let mut bytes = data.to_vec();
                    bytes[2] ^= 1;
                    *data = bytes.into();
                }
            }
            3 => {
                let event = changed
                    .events
                    .iter_mut()
                    .find(|event| matches!(event, HistoryEvent::Set { id: 2, .. }))
                    .unwrap();
                if let HistoryEvent::Set { undo, .. } = event {
                    undo[0] = 1;
                }
            }
            4 => {
                changed
                    .communication
                    .instances
                    .iter_mut()
                    .find(|instance| instance.id == 2)
                    .unwrap()
                    .source ^= 1
            }
            5 => {
                let empty = changed.events.iter().position(|event| matches!(event, HistoryEvent::Set { id: 2, data, .. } if data.is_empty())).unwrap();
                changed.events[empty] = HistoryEvent::Marker;
            }
            _ => unreachable!(),
        }
        assert!(
            proof.verify_layout(&changed, &mut || true).is_err(),
            "variant {variant}"
        );
    }
}

#[test]
fn middle_memory_cancellation_does_not_publish_output() {
    let source = source();
    let mut output = Vec::new();
    let mut checks = 0;
    let failure = trim_scene_frames(&source, 1, 2, &mut output, || {
        checks += 1;
        checks < 100
    })
    .unwrap_err();
    assert_eq!(
        failure.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
    assert!(output.is_empty());
}
