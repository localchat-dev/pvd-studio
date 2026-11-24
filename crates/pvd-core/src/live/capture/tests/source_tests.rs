use super::*;
use crate::frame_sources::{
    FrameBinding, FrameDirectory, FrameObject, FrameSection, FrameSourceKey,
};

const PHYSX: u64 = 11;
const APEX: u64 = 22;
const RENDERER: u64 = 33;
const FRAME_COUNT: usize = 12;

fn source_key(section: FrameSection) -> FrameSourceKey {
    FrameSourceKey::Captured {
        section,
        source: if section == FrameSection::PhysX {
            PHYSX
        } else {
            APEX
        },
    }
}

fn source_fixture(paged: bool) -> Fixture {
    source_fixture_with_step_offset(paged, 0)
}

fn source_fixture_with_step_offset(paged: bool, step_offset: u64) -> Fixture {
    let root = std::env::temp_dir().join(format!(
        "pvd-source-capture-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir(&root).unwrap();
    let config = HistoryConfig {
        directory: root.join("history"),
        disk_budget_bytes: 32 << 20,
        segment_target_bytes: 64 << 20,
        segment_frames: 4,
        min_free_disk_bytes: 0,
    };
    let mut history = if paged {
        HistoryWriter::new_paged(config).unwrap()
    } else {
        HistoryWriter::new(config).unwrap()
    };
    let mut directory = FrameDirectory::default();
    let mut state = SceneState::default();
    for (id, generation, type_name) in [(PHYSX, 4, "physx3::PxScene"), (APEX, 1, "Apex::scene")] {
        state.objects.insert(
            id,
            Object {
                id,
                generation,
                type_name: type_name.into(),
                properties: BTreeMap::new(),
            },
        );
    }
    let mut states = Vec::new();
    let analysis = Analysis::default();
    for index in 0..FRAME_COUNT {
        let section = if index % 2 == 0 {
            FrameSection::PhysX
        } else {
            FrameSection::Apex
        };
        let source = if section == FrameSection::PhysX {
            PHYSX
        } else {
            APEX
        };
        let object = state.objects.get_mut(&source).unwrap();
        object
            .properties
            .insert("step".into(), Value::U64((index / 2) as u64 + step_offset));
        if source == APEX && index >= 9 {
            object.generation = 2;
        }
        state.debug_draw.clear();
        if source == APEX {
            state.debug_draw.insert(
                RENDERER,
                Arc::new(vec![Primitive::Line {
                    instance: APEX,
                    positions: [[index as f32, 0., 0.], [index as f32, 1., 0.]],
                    colors: [0xff00ffff; 2],
                }]),
            );
        }
        let timestamp = if source == PHYSX {
            1_000_000 + (index / 2) as u64 * 1000
        } else {
            9_000_000 + (index / 2) as u64 * 1500
        };
        let info = FrameInfo {
            index,
            timestamp,
            scene_id: source,
        };
        let dirty = [PHYSX, APEX, RENDERER].into_iter().collect();
        let patch = SceneUpdate::capture(&state, &dirty, false);
        history.append(&info, &state, &patch, &analysis, Vec::new());
        assert!(
            history.snapshot().stats.warning.is_none(),
            "{:?}",
            history.snapshot().stats.warning
        );
        assert_eq!(history.snapshot().state_at(index).unwrap(), state);
        directory
            .append(
                index,
                FrameBinding::sdk(section.name(), source, &state).unwrap(),
            )
            .unwrap();
        states.push(state.clone());
    }
    let disk = history.snapshot();
    directory.validate_frames(&disk.frames).unwrap();
    let mut recording = crate::RecordingBuilder::new(pvd_protocol::Header {
        version: 1,
        timestamp_numerator: 1000,
        timestamp_denominator: 1,
        flags: 1,
        endian: pvd_protocol::Endian::Little,
    })
    .snapshot()
    .unwrap();
    recording.frames = disk.frames.clone();
    recording.frame_sources = directory;
    recording.disk = Some(disk);
    recording.streamed_frame_end = Some(FRAME_COUNT);
    Fixture {
        root,
        history: Some(history),
        recording: Some(recording),
        states,
        analysis: vec![postcard::to_stdvec(&analysis).unwrap(); FRAME_COUNT],
    }
}

fn assert_source_range(recording: &crate::Recording, fixture: &Fixture, range: CaptureRange) {
    recording
        .frame_sources
        .validate_frames(&recording.frames)
        .unwrap();
    assert_eq!(recording.frames.len(), range.last - range.first + 1);
    for section in [FrameSection::PhysX, FrameSection::Apex] {
        let expected: Vec<_> = (range.first..=range.last)
            .filter(|index| (*index % 2 == 0) == (section == FrameSection::PhysX))
            .collect();
        let timeline = recording.frame_sources.source(source_key(section));
        if expected.is_empty() {
            assert!(timeline.is_none());
            continue;
        }
        let timeline = timeline.unwrap();
        assert_eq!(timeline.observed_frames(), FRAME_COUNT / 2);
        assert_eq!(
            timeline
                .frames()
                .map(|frame| (frame.index, frame.ordinal))
                .collect::<Vec<_>>(),
            expected
                .iter()
                .map(|index| (*index, index / 2))
                .collect::<Vec<_>>()
        );
        let mut cursor = ReplayCursor::default();
        let mut displayed = SceneState::default();
        let random = [
            expected[expected.len() - 1],
            expected[0],
            expected[expected.len() / 2],
            expected[0],
        ];
        for index in expected
            .iter()
            .rev()
            .copied()
            .chain(expected.iter().copied())
            .chain(random)
        {
            let replay = cursor.seek(recording, index, || false).unwrap();
            replay.update.apply_owned(&mut displayed);
            assert_eq!(displayed, fixture.states[index]);
            assert_eq!(recording.state_at(index).unwrap(), fixture.states[index]);
            assert_eq!(
                postcard::to_stdvec(&replay.analysis).unwrap(),
                fixture.analysis[index]
            );
            let frame = timeline.at_ordinal(index / 2).unwrap();
            let source = if section == FrameSection::PhysX {
                PHYSX
            } else {
                APEX
            };
            let identity = FrameObject {
                id: source,
                generation: fixture.states[index].objects[&source].generation,
            };
            assert_eq!(frame.source_object, Some(identity));
            assert_eq!(
                frame.scene,
                (section == FrameSection::PhysX).then_some(identity)
            );
            let expected_timestamp = if source == PHYSX {
                1_000_000 + (index / 2) as u64 * 1000
            } else {
                9_000_000 + (index / 2) as u64 * 1500
            };
            assert_eq!(
                recording.frame_info(index).unwrap().timestamp,
                expected_timestamp
            );
            let step_seconds = if section == FrameSection::PhysX {
                0.001
            } else {
                0.0015
            };
            let expected_seconds = ((index - expected[0]) / 2) as f64 * step_seconds;
            assert!(
                (recording.seconds_at(index).unwrap() - expected_seconds).abs() < 1e-12,
                "source {section:?}, visible range {range:?}, frame {index}"
            );
        }
    }
    for index in (0..range.first).chain(range.last + 1..FRAME_COUNT) {
        assert!(recording.frame_sources.frame(index).is_none());
        assert_eq!(recording.seconds_at(index), None);
        assert!(!recording.is_frame_retained(index));
        assert!(recording.state_at(index).is_err());
    }
}

#[test]
fn two_captured_clocks_reopen_and_recrop_with_independent_endpoints_and_replay() {
    for paged in [false, true] {
        let mut fixture = source_fixture(paged);
        let range = CaptureRange { first: 5, last: 10 };
        let (bytes, report) = fixture.export(range);
        assert_eq!(report.format, Format::V3);
        assert!(bytes.starts_with(MAGIC));
        assert_eq!(report.digest, *blake3::hash(&bytes).as_bytes());
        let directory = read::directory(
            &mut Cursor::new(&bytes),
            bytes.len() as u64,
            &CaptureLimits::default(),
            &mut |_| true,
        )
        .unwrap();
        let hidden = directory.segments[0].frames[0].info.index;
        assert!(hidden < range.first);
        assert!(directory.frame_sources.frame(hidden).is_none());
        let mut path = fixture.root.join("source-clocks.pvdcapture");
        std::fs::write(&path, bytes).unwrap();
        fixture.recording.take();
        fixture.history.take();
        let mut recording = crate::Recording::from_path(&path).unwrap();
        assert_source_range(&recording, &fixture, range);
        for (number, range) in [
            CaptureRange { first: 5, last: 9 },
            CaptureRange { first: 7, last: 7 },
        ]
        .into_iter()
        .enumerate()
        {
            let next = fixture
                .root
                .join(format!("source-crop-{number}.pvdcapture"));
            write_retained_new(
                &recording,
                &next,
                range,
                &CaptureLimits::default(),
                0,
                |_| true,
            )
            .unwrap();
            drop(recording);
            std::fs::remove_file(&path).unwrap();
            recording = crate::Recording::from_path(&next).unwrap();
            path = next;
            assert_source_range(&recording, &fixture, range);
        }
        assert_eq!(
            recording.frame_sources.default_source(),
            Some(source_key(FrameSection::Apex))
        );
        drop(recording);
    }
}

struct Entry {
    kind: u8,
    start: usize,
    payload: std::ops::Range<usize>,
}

fn entries(bytes: &[u8]) -> Vec<Entry> {
    let mut offset = 8;
    let mut result = Vec::new();
    while offset < bytes.len() {
        let length = u64::from_le_bytes(bytes[offset + 1..offset + 9].try_into().unwrap()) as usize;
        let payload = offset + wire::ENTRY_HEADER;
        result.push(Entry {
            kind: bytes[offset],
            start: offset,
            payload: payload..payload + length,
        });
        offset = payload + length;
    }
    assert_eq!(offset, bytes.len());
    result
}

fn assert_archive_checksums(bytes: &[u8]) {
    let all = entries(bytes);
    for entry in &all {
        let mut hash = blake3::Hasher::new();
        hash.update(&bytes[entry.start..entry.start + 9]);
        hash.update(&bytes[entry.payload.clone()]);
        assert_eq!(
            &bytes[entry.start + 9..entry.payload.start],
            hash.finalize().as_bytes()
        );
        if entry.kind == wire::FOOTER {
            assert_eq!(entry.payload.end, bytes.len());
            let payload = &bytes[entry.payload.clone()];
            assert_eq!(
                u64::from_le_bytes(payload[..8].try_into().unwrap()),
                entry.start as u64
            );
            assert_eq!(
                &payload[16..48],
                blake3::hash(&bytes[..entry.start]).as_bytes()
            );
        }
    }
    assert_eq!(all.last().unwrap().kind, wire::FOOTER);
}

fn with_source_entries(bytes: &[u8], magic: &[u8; 8], payloads: &[&[u8]]) -> Vec<u8> {
    let all = entries(bytes);
    let source = all
        .iter()
        .find(|entry| entry.kind == wire::FRAME_SOURCES)
        .unwrap();
    let footer = all.last().unwrap();
    assert_eq!(source.payload.end, footer.start);
    let frames = u64::from_le_bytes(
        bytes[footer.payload.start + 8..footer.payload.start + 16]
            .try_into()
            .unwrap(),
    );
    let mut output = bytes[..source.start].to_vec();
    output[..8].copy_from_slice(magic);
    let mut hash = blake3::Hasher::new();
    hash.update(&output);
    let written = output.len() as u64;
    let mut writer = wire::Writer {
        output: &mut output,
        bytes: written,
        hash,
        limit: 32 << 20,
    };
    for payload in payloads {
        writer
            .entry(wire::FRAME_SOURCES, &[*payload], &mut || false)
            .unwrap();
    }
    let mut footer = wire::u64_bytes([writer.bytes, frames]);
    footer.extend(writer.hash.finalize().as_bytes());
    writer
        .entry(wire::FOOTER, &[&footer], &mut || false)
        .unwrap();
    assert_archive_checksums(&output);
    output
}

#[test]
fn v1_and_v2_without_source_catalogues_reopen_as_legacy_and_resave_as_v3() {
    let mut fixture = key_schema::keyed_fixture(true);
    let (v3, _) = fixture.export(CaptureRange { first: 3, last: 6 });
    let v2 = with_source_entries(&v3, V2_MAGIC, &[]);
    let v1 = key_schema::small_v1_fixture();
    for (number, bytes, expected_format, range) in [
        (
            1,
            v1,
            Format::V1,
            CaptureRange {
                first: 20,
                last: 20,
            },
        ),
        (2, v2, Format::V2, CaptureRange { first: 3, last: 6 }),
    ] {
        assert_archive_checksums(&bytes);
        let path = fixture.root.join(format!("legacy-{number}.pvdcapture"));
        std::fs::write(&path, &bytes).unwrap();
        let recording = crate::Recording::from_path(&path).unwrap();
        assert_eq!(recording.capture_format(), Some(expected_format));
        assert_eq!(
            recording.frame_sources.default_source(),
            Some(FrameSourceKey::Legacy)
        );
        let legacy = recording
            .frame_sources
            .source(FrameSourceKey::Legacy)
            .unwrap();
        assert_eq!(
            legacy
                .frames()
                .map(|frame| (frame.index, frame.ordinal))
                .collect::<Vec<_>>(),
            (range.first..=range.last)
                .map(|index| (index, index))
                .collect::<Vec<_>>()
        );
        for index in range.first..=range.last {
            let expected = (index - range.first) as f64 * 1_000_003.0 * 7.0 / 3.0 / 1e9;
            assert!((recording.seconds_at(index).unwrap() - expected).abs() < 1e-12);
        }
        let expected_states: Vec<_> = (range.first..=range.last)
            .map(|index| recording.state_at(index).unwrap())
            .collect();
        let output = fixture
            .root
            .join(format!("legacy-{number}-saved.pvdcapture"));
        let report = write_retained_new(
            &recording,
            &output,
            range,
            &CaptureLimits::default(),
            0,
            |_| true,
        )
        .unwrap();
        assert_eq!(report.format, Format::V3);
        drop(recording);
        let reopened = crate::Recording::from_path(&output).unwrap();
        assert_eq!(reopened.capture_format(), Some(Format::V3));
        assert_eq!(
            reopened.frame_sources.default_source(),
            Some(FrameSourceKey::Legacy)
        );
        for index in (range.first..=range.last).rev() {
            assert_eq!(
                reopened.state_at(index).unwrap(),
                expected_states[index - range.first]
            );
            let expected = (index - range.first) as f64 * 1_000_003.0 * 7.0 / 3.0 / 1e9;
            assert!((reopened.seconds_at(index).unwrap() - expected).abs() < 1e-12);
        }
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        drop(reopened);
    }
    fixture.recording.take();
    fixture.history.take();
}

fn source_payload(bytes: &[u8]) -> Vec<u8> {
    let source = entries(bytes)
        .into_iter()
        .find(|entry| entry.kind == wire::FRAME_SOURCES)
        .unwrap();
    bytes[source.payload].to_vec()
}

fn source_rows(payload: &[u8]) -> Vec<usize> {
    let source_count = u32::from_le_bytes(payload[4..8].try_into().unwrap()) as usize;
    let frame_count = u64::from_le_bytes(payload[8..16].try_into().unwrap()) as usize;
    let mut offset = 25 + source_count * 33;
    let mut rows = Vec::new();
    for _ in 0..frame_count {
        rows.push(offset);
        let flags = payload[offset + 20];
        offset += 21 + 12 * (usize::from(flags & 1 != 0) + usize::from(flags & 2 != 0));
    }
    assert_eq!(offset, payload.len());
    rows
}

#[test]
fn source_directory_damage_is_rejected_even_with_valid_entry_and_file_checksums() {
    let fixture = source_fixture(false);
    let (bytes, _) = fixture.export(CaptureRange { first: 3, last: 10 });
    let payload = source_payload(&bytes);
    let mut cases = vec![
        ("missing", with_source_entries(&bytes, MAGIC, &[])),
        (
            "duplicate",
            with_source_entries(&bytes, MAGIC, &[&payload, &payload]),
        ),
        (
            "v2-entry",
            with_source_entries(&bytes, V2_MAGIC, &[&payload]),
        ),
    ];
    let mut version = payload.clone();
    version[..4].copy_from_slice(&2u32.to_le_bytes());
    cases.push(("version", with_source_entries(&bytes, MAGIC, &[&version])));
    let mut duplicate_key = payload.clone();
    duplicate_key[58..67].copy_from_slice(&payload[25..34]);
    cases.push((
        "duplicate-key",
        with_source_entries(&bytes, MAGIC, &[&duplicate_key]),
    ));
    let rows = source_rows(&payload);
    let mut duplicate_frame = payload.clone();
    duplicate_frame[rows[1] + 4..rows[1] + 12].copy_from_slice(&payload[rows[0] + 4..rows[0] + 12]);
    cases.push((
        "duplicate-frame",
        with_source_entries(&bytes, MAGIC, &[&duplicate_frame]),
    ));
    let mut hidden_frame = payload.clone();
    hidden_frame[rows[0] + 4..rows[0] + 12].copy_from_slice(&2u64.to_le_bytes());
    cases.push((
        "hidden-frame",
        with_source_entries(&bytes, MAGIC, &[&hidden_frame]),
    ));
    let mut ordinal = payload.clone();
    ordinal[rows[0] + 12..rows[0] + 20].copy_from_slice(&u64::MAX.to_le_bytes());
    cases.push(("ordinal", with_source_entries(&bytes, MAGIC, &[&ordinal])));
    let mut missing_count = payload.clone();
    missing_count[8..16].copy_from_slice(&7u64.to_le_bytes());
    cases.push((
        "missing-row",
        with_source_entries(&bytes, MAGIC, &[&missing_count]),
    ));
    for (name, bytes) in cases {
        assert_archive_checksums(&bytes);
        let path = fixture.root.join(format!("damaged-{name}.pvdcapture"));
        std::fs::write(&path, bytes).unwrap();
        let failure = crate::Recording::from_path(&path).err().unwrap();
        assert!(
            !failure.to_string().contains("checksum"),
            "{name}: {failure}"
        );
        assert!(failure.to_string().contains("source"), "{name}: {failure}");
    }
}

#[test]
fn source_directory_budget_and_late_cancellation_reject_incomplete_archives() {
    let fixture = source_fixture(true);
    let source = fixture.recording.as_ref().unwrap();
    let range = CaptureRange { first: 3, last: 10 };
    let visible_count = range.last - range.first + 1;
    let (bytes, _) = fixture.export(range);
    let source_index_bytes = source
        .frame_sources
        .clipped_index_bytes(range.first, range.last)
        .unwrap();
    let limits = CaptureLimits {
        index_bytes: source_index_bytes - 1,
        ..Default::default()
    };
    assert!(
        read::directory(
            &mut Cursor::new(&bytes),
            bytes.len() as u64,
            &limits,
            &mut |_| true
        )
        .is_err()
    );
    let limited = fixture.root.join("limited-sources.pvdcapture");
    assert!(write_retained_new(source, &limited, range, &limits, 0, |_| true).is_err());
    assert!(!limited.exists());
    let cancelled = fixture.root.join("cancelled-sources.pvdcapture");
    let mut final_copy_checks = 0;
    let failure = write_retained_new(
        source,
        &cancelled,
        range,
        &CaptureLimits::default(),
        0,
        |progress| {
            if progress.phase == CapturePhase::Copy && progress.completed == visible_count as u64 {
                final_copy_checks += 1;
                return final_copy_checks <= visible_count + 2;
            }
            true
        },
    )
    .unwrap_err();
    assert_eq!(
        failure.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
    assert!(final_copy_checks > visible_count);
    assert!(!cancelled.exists());
    let path = fixture.root.join("open-cancelled-sources.pvdcapture");
    std::fs::write(&path, &bytes).unwrap();
    let mut final_read_checks = 0;
    let failure = open_with_progress(&path, &CaptureLimits::default(), |progress| {
        if progress.phase == CapturePhase::Validate && progress.completed == bytes.len() as u64 {
            final_read_checks += 1;
            return final_read_checks <= visible_count + 1;
        }
        true
    })
    .err()
    .unwrap();
    assert_eq!(
        failure.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
    assert!(final_read_checks > visible_count);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(std::fs::read_dir(&fixture.root).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".pvd-capture-")
    }));
}

mod segmented;
