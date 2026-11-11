use super::*;
use crate::native::{container_tests, read_with_progress};
use std::io::Cursor;

fn shift(id: u32, bits: [u32; 3], properties: &[[u32; 2]]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for value in [11, id, bits[0], bits[1], bits[2], properties.len() as u32] {
        bytes.extend(value.to_le_bytes());
    }
    for pair in properties {
        for value in pair {
            bytes.extend(value.to_le_bytes());
        }
    }
    bytes
}

fn section(id: u32, ticks: u64, begin: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    for value in [8u32, 5, 24] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.extend((u64::from(id) * 0x111).to_le_bytes());
    bytes.extend(ticks.to_le_bytes());
    bytes.extend(13u32.to_le_bytes());
    bytes.extend([u8::from(begin), 0, 0, 0]);
    bytes
}

fn fixture(initial_scene: bool) -> Vec<u8> {
    fixture_with_frames(
        initial_scene,
        &[(1, false), (1, true), (2, true), (1, true)],
    )
}

fn fixture_with_frames(initial_scene: bool, frames: &[(u32, bool)]) -> Vec<u8> {
    let metadata = container_tests::metadata_bytes(&container_tests::fixture());
    let mut events = vec![9u32.to_le_bytes().to_vec()];
    let ids = frames.iter().map(|(id, _)| *id).collect::<BTreeSet<_>>();
    for &id in &ids {
        if id == 1 && initial_scene {
            continue;
        }
        events.push(
            [1u32, id, 4]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect(),
        );
    }
    let mut sections = BTreeMap::<u32, Section>::new();
    for (frame, &(id, shifted)) in frames.iter().enumerate() {
        let ticks = 100 + frame as u64 * 10;
        let begin = events.len() as u64;
        events.push(section(id, ticks, true));
        if shifted {
            events.push(shift(
                id,
                [0x8000_0000, (frame as f32 * 1.25).to_bits(), 1],
                &[[id, 0], [id, 2]],
            ));
        }
        let end = events.len() as u64;
        events.push(section(id, ticks + 1, false));
        sections
            .entry(id)
            .or_insert_with(|| Section {
                source: u64::from(id) * 0x111,
                name: 13,
                rows: vec![],
            })
            .rows
            .push([ticks, begin, ticks + 1, end]);
    }
    let communication = Communication {
        clock: [1, 1],
        instances: ids
            .iter()
            .map(|&id| Instance {
                id,
                source: u64::from(id) * 0x111,
                flags: [0; 10],
            })
            .collect(),
        reverse: ids.iter().map(|&id| (u64::from(id) * 0x111, id)).collect(),
        cameras: vec![],
        sections: sections.into_values().rev().collect(),
        zones: vec![],
        memory_instances: [u32::MAX; 2],
        memory_names: vec![],
    }
    .encode(&mut || true)
    .unwrap();
    let mut backing = vec![0; 64];
    let mut snapshot = Vec::new();
    w32(&mut snapshot, u32::from(initial_scene));
    if initial_scene {
        w32(&mut snapshot, 1);
        w64(&mut snapshot, 64);
        backing.extend(container_tests::instance(1, 7, &[]));
        backing[63] = 0xA5;
    }
    backing[..snapshot.len()].copy_from_slice(&snapshot);
    let mut pages = Vec::new();
    let mut ordinal = 0u64;
    for events in events.chunks(7) {
        backing.resize(next_page(backing.len()).unwrap(), 0);
        let position = backing.len() as u64;
        let mut payload = Vec::new();
        w32(&mut payload, events.len() as u32);
        let mut offset = 0;
        for event in events {
            w32(&mut payload, offset);
            offset += event.len() as u32;
        }
        for event in events {
            payload.extend(event);
        }
        pages.push((ordinal, position, events.len() as u32, payload.len() as u32));
        ordinal += events.len() as u64;
        backing.extend(payload);
    }
    let mut bytes = Vec::new();
    for value in [MAGIC, 1, metadata.len() as u32, communication.len() as u32] {
        w32(&mut bytes, value);
    }
    bytes.extend(metadata);
    bytes.extend(communication);
    bytes.resize(next_page(bytes.len()).unwrap(), 0);
    w64(&mut bytes, ordinal);
    w32(&mut bytes, pages.len() as u32);
    for (first, position, count, len) in pages {
        w64(&mut bytes, first);
        w64(&mut bytes, position);
        w32(&mut bytes, count);
        w32(&mut bytes, len);
    }
    w32(&mut bytes, 1);
    w64(&mut bytes, 0);
    w64(&mut bytes, 0);
    w32(&mut bytes, snapshot.len() as u32);
    w32(&mut bytes, 0);
    w32(&mut bytes, 0);
    bytes.resize(next_page(bytes.len()).unwrap(), 0);
    bytes.extend(backing);
    bytes
}

fn parse(bytes: &[u8]) -> Result<HistoryEvent<'_>, Error> {
    HistoryEvent::parse(bytes, 0, &container_tests::fixture(), &mut 0)
}

#[test]
fn origin_shift_record_retains_float_bits_and_borrows_property_pairs() {
    for properties in [vec![], vec![[7, 11], [u32::MAX, 0x1234_5678]]] {
        let bytes = shift(1, [0x8000_0000, 1, 1.25f32.to_bits()], &properties);
        let parsed = parse(&bytes).unwrap();
        let HistoryEvent::OriginShift(event) = &parsed else {
            panic!("expected OriginShift");
        };
        assert_eq!(event.shift_bits, [0x8000_0000, 1, 1.25f32.to_bits()]);
        assert_eq!(event.shifted_properties, &bytes[24..]);
        assert_eq!(event.shifted_properties.as_ptr(), bytes[24..].as_ptr());
        let mut encoded = Vec::new();
        parsed.encode(&mut encoded).unwrap();
        assert_eq!(encoded, bytes);
    }
}

#[test]
fn origin_shift_records_reject_incomplete_rows_tails_counts_and_non_finite_shifts() {
    let bytes = shift(1, [1.25f32.to_bits(), 0, 0], &[[1, 0], [2, 2]]);
    for length in 0..bytes.len() {
        assert!(parse(&bytes[..length]).is_err(), "length {length}");
    }
    let mut tail = bytes.clone();
    tail.push(0);
    assert!(parse(&tail).is_err());
    for count in [0, 1, 3, 1_000_001, u32::MAX] {
        let mut bad = bytes.clone();
        bad[20..24].copy_from_slice(&count.to_le_bytes());
        assert!(parse(&bad).is_err(), "count {count}");
    }
    for axis in 0..3 {
        for bits in [
            f32::NAN.to_bits(),
            f32::INFINITY.to_bits(),
            f32::NEG_INFINITY.to_bits(),
        ] {
            let mut bad = bytes.clone();
            bad[8 + axis * 4..12 + axis * 4].copy_from_slice(&bits.to_le_bytes());
            assert!(parse(&bad).unwrap_err().to_string().contains("finite"));
        }
    }
}

#[test]
fn whole_origin_shift_save_preserves_multi_page_snapshots_and_frame_state() {
    for initial_scene in [false, true] {
        let source = fixture(initial_scene);
        let (original, _) = read_with_progress(&source, |_, _| true).unwrap();
        let mut output = Vec::new();
        let report = write_reader(
            &mut Cursor::new(&source),
            Selection::Whole,
            &mut output,
            |_| true,
        )
        .unwrap();
        assert_eq!(output, source);
        assert_eq!(report.source_blake3, blake3::hash(&source));
        assert_eq!((report.source_frames, report.frames), (4, 4));
        assert_eq!(report.saved_instances, usize::from(initial_scene));
        if let Some(directory) = std::env::var_os("PVD_NATIVE_CONSTRUCTED_EXPORT_DIRECTORY") {
            let directory = std::path::PathBuf::from(directory);
            std::fs::create_dir_all(&directory).unwrap();
            let name = if initial_scene {
                "origin-initial.pxd2"
            } else {
                "origin-empty.pxd2"
            };
            std::fs::write(directory.join(name), &source).unwrap();
        }
        let (restored, _) = read_with_progress(&output, |_, _| true).unwrap();
        for frame in [0, 1, 2, 3, 2, 1, 0, 3] {
            assert_eq!(
                restored.state_at(frame).unwrap(),
                original.state_at(frame).unwrap()
            );
        }
        let state = restored.state_at(2).unwrap();
        assert_eq!(
            state.objects[&2].properties["$origin_shift"],
            crate::Value::Vec3([-0.0, 2.5, f32::from_bits(1)])
        );
        let layout = Layout::parse(&output, &mut || true).unwrap();
        assert!(layout.pages.len() > 1);
        assert_eq!(layout.snapshots[0].len, if initial_scene { 16 } else { 4 });
    }
}

#[test]
fn origin_shift_trims_require_rigid_members_without_blocking_an_earlier_prefix() {
    let source = fixture(false);
    let mut prefix = Vec::new();
    trim_scene_frames(&source, 0, 0, &mut prefix, || true).unwrap();
    let (restored, _) = read_with_progress(&prefix, |_, _| true).unwrap();
    assert_eq!(restored.frame_count(), 1);
    for (first, last) in [(0, 1), (0, 3), (1, 3), (2, 3), (3, 3)] {
        let mut output = Vec::new();
        let failure = trim_scene_frames(&source, first, last, &mut output, || true).unwrap_err();
        assert!(
            failure
                .to_string()
                .contains("OriginShift prefix requires captured rigid properties"),
            "{failure}"
        );
        assert!(output.is_empty());
    }
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    layout
        .communication
        .sections
        .retain(|section| section.source == 0x111);
    for event in &mut layout.events {
        if let HistoryEvent::Create(2, class) = event {
            *class = 3;
        }
    }
    let failure = layout.trim_middle(1, 2, &mut || true).unwrap_err();
    assert!(
        failure
            .to_string()
            .contains("unpaired or unindexed section event"),
        "{failure}"
    );
}

#[test]
fn middle_origin_shift_without_rigid_members_is_refused_before_snapshot_rebuild() {
    let source = fixture_with_frames(false, &[(1, false), (1, true), (1, false), (1, false)]);
    let mut output = Vec::new();
    let failure = trim_scene_frames(&source, 2, 3, &mut output, || true).unwrap_err();
    assert!(
        failure
            .to_string()
            .contains("OriginShift prefix requires captured rigid properties"),
        "{failure}"
    );
    assert!(output.is_empty());
}

#[test]
fn whole_origin_shift_save_rejects_malformed_records_before_writing() {
    let source = fixture(false);
    let pattern = shift(1, [0x8000_0000, 1.25f32.to_bits(), 1], &[[1, 0], [1, 2]]);
    let start = source
        .windows(pattern.len())
        .position(|bytes| bytes == pattern)
        .unwrap();
    for (offset, value) in [(8, f32::INFINITY.to_bits()), (20, u32::MAX)] {
        let mut malformed = source.clone();
        malformed[start + offset..start + offset + 4].copy_from_slice(&value.to_le_bytes());
        let mut output = Vec::new();
        assert!(rewrite(&malformed, &mut output, || true).is_err());
        assert!(output.is_empty());
    }
}
