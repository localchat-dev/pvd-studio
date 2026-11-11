use super::*;
use crate::native::{container_tests, read_with_progress};

fn fixture(overlapping: bool) -> Vec<u8> {
    let metadata = container_tests::fixture();
    let object_bytes = container_tests::metadata_bytes(&metadata);
    let mut encoded = Vec::new();
    for event in [
        HistoryEvent::Marker,
        HistoryEvent::Create(1, 4),
        HistoryEvent::Create(2, 4),
    ] {
        let mut bytes = Vec::new();
        event.encode(&mut bytes).unwrap();
        encoded.push(bytes);
    }
    let order = if overlapping {
        vec![
            (1, true),
            (2, true),
            (1, false),
            (2, false),
            (1, true),
            (1, false),
            (2, true),
            (2, false),
        ]
    } else {
        vec![
            (1, true),
            (1, false),
            (2, true),
            (2, false),
            (1, true),
            (1, false),
            (2, true),
            (2, false),
        ]
    };
    let mut sections = BTreeMap::<u32, Section>::new();
    let mut starts = BTreeMap::new();
    let mut writes = BTreeMap::from([(1, 1usize), (2, 2usize)]);
    for (step, (id, begin)) in order.into_iter().enumerate() {
        let ticks = 100 + step as u64;
        let ordinal = encoded.len() as u64;
        let mut bytes = Vec::new();
        HistoryEvent::Section {
            class: 5,
            source: u64::from(id) * 0x111,
            ticks,
            name: 13,
            begin: u8::from(begin),
            padding: [0; 3],
        }
        .encode(&mut bytes)
        .unwrap();
        encoded.push(bytes);
        if begin {
            starts.insert(id, (ticks, ordinal));
            let value = (id * 10 + step as u32).to_le_bytes();
            let previous = writes.insert(id, encoded.len()).unwrap();
            let mut bytes = Vec::new();
            HistoryEvent::Set {
                kind: 3,
                id,
                property: 0,
                data: value.as_slice().into(),
                undo: vec![(encoded.len() - previous) as u32],
            }
            .encode(&mut bytes)
            .unwrap();
            encoded.push(bytes);
        } else {
            let (start_ticks, start) = starts.remove(&id).unwrap();
            sections
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
    let events = encoded
        .iter()
        .enumerate()
        .map(|(i, bytes)| HistoryEvent::parse(bytes, i as u64, &metadata, &mut 0).unwrap())
        .collect::<Vec<_>>();
    let page = encode_page(&events, &mut || true).unwrap();
    let layout = Layout {
        source: &[],
        object_bytes: &object_bytes,
        metadata,
        pointer_layouts: vec![vec![]; 6],
        communication: Communication {
            clock: [1, 1],
            instances: (1..=2)
                .map(|id| Instance {
                    id,
                    source: u64::from(id) * 0x111,
                    flags: [0; 10],
                })
                .collect(),
            reverse: vec![(0x111, 1), (0x222, 2)],
            cameras: vec![],
            sections: sections.into_values().rev().collect(),
            zones: vec![],
            memory_instances: [u32::MAX; 2],
            memory_names: vec![],
        },
        metadata_padding: &[],
        history_padding: &[],
        pages: vec![Page {
            first: 0,
            position: 4096,
            count: events.len() as u32,
            len: page.len() as u32,
        }],
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
    layout.encode(false, &mut || true).unwrap()
}

fn check_prefix(source: &[u8], last: usize) {
    let (original, _) = read_with_progress(source, |_, _| true).unwrap();
    let mut output = Vec::new();
    let report = trim_scene_frames(source, 0, last, &mut output, || true).unwrap();
    assert_eq!((report.source_frames, report.frames), (4, last + 1));
    let (restored, _) = read_with_progress(&output, |_, _| true).unwrap();
    assert_eq!(restored.frame_count(), last + 1);
    for frame in (0..=last).chain((0..=last).rev()) {
        assert_eq!(
            restored.state_at(frame).unwrap(),
            original.state_at(frame).unwrap()
        );
        assert_eq!(
            restored.frames[frame].scene_id,
            original.frames[frame].scene_id
        );
        assert_eq!(
            restored.frames[frame].timestamp,
            original.frames[frame].timestamp
        );
    }
    let layout = Layout::parse(&output, &mut || true).unwrap();
    let mut copy = Vec::new();
    rewrite(&output, &mut copy, || true).unwrap();
    assert_eq!(copy, output);
    assert_eq!(
        layout
            .communication
            .sections
            .iter()
            .map(|s| s.rows.len())
            .sum::<usize>(),
        last + 1
    );
}

#[test]
fn prefix_uses_completed_frame_order_across_scenes_and_section_tables() {
    let source = fixture(false);
    for last in 0..4 {
        check_prefix(&source, last);
    }
}

#[test]
fn prefix_rejects_open_scene_frames_without_truncating_other_scenes() {
    let source = fixture(true);
    let mut output = Vec::new();
    let failure = trim_scene_frames(&source, 0, 0, &mut output, || true).unwrap_err();
    assert!(failure.to_string().contains("inside another Scene frame"));
    assert!(output.is_empty());
    for last in 1..4 {
        check_prefix(&source, last);
    }
}

#[test]
fn prefix_requires_unique_scene_sources_and_indexed_frame_sections() {
    let source = fixture(false);
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    layout.communication.instances[1].source = layout.communication.instances[0].source;
    assert!(
        layout
            .trim_prefix(1, &mut || true)
            .unwrap_err()
            .to_string()
            .contains("Scene lifecycles share")
    );
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    layout
        .communication
        .sections
        .push(layout.communication.sections[0].clone());
    assert!(
        layout
            .trim_prefix(1, &mut || true)
            .unwrap_err()
            .to_string()
            .contains("referenced twice")
    );
}

#[test]
fn prefix_keeps_frame_boundaries_inside_the_scene_lifetime() {
    let source = fixture(false);
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    layout.events.swap(1, 4);
    assert!(
        layout
            .trim_prefix(0, &mut || true)
            .unwrap_err()
            .to_string()
            .contains("exceeds its Scene lifecycle")
    );
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    layout.events[4] = HistoryEvent::Destroy(1);
    assert!(
        layout
            .trim_prefix(0, &mut || true)
            .unwrap_err()
            .to_string()
            .contains("exceeds its Scene lifecycle")
    );
}
