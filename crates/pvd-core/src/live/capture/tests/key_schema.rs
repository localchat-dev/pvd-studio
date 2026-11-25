use super::*;

pub(super) fn keyed_fixture(paged: bool) -> Fixture {
    Fixture::with_keys(
        paged,
        32 << 20,
        FrameClock {
            origin: 8_000_000_000,
            step: 1_000_003,
            scene_id: 101,
            ratio: (7, 3),
        },
        true,
    )
}

fn frame_object(segment: &SegmentView, frame: usize) -> (HistoryReader, u64) {
    let location = &segment.frames[segment
        .frames
        .binary_search_by_key(&frame, |entry| entry.info.index)
        .unwrap()];
    let mut reader = DiskRecording::open(segment).unwrap();
    let Record::Frame(stored) =
        read_record(&mut reader, location.offset, segment.committed_end).unwrap()
    else {
        panic!("frame variant")
    };
    let id = stored
        .update
        .objects
        .iter()
        .find(|(id, _)| *id == 71)
        .and_then(|(_, id)| *id)
        .unwrap();
    (reader, id)
}

fn assert_range(recording: &crate::Recording, fixture: &Fixture, range: CaptureRange) {
    assert_eq!(recording.capture_format(), Some(Format::V3));
    assert_eq!(recording.first_frame(), range.first);
    assert_eq!(recording.frame_count(), range.last + 1);
    let mut cursor = ReplayCursor::default();
    let mut state = SceneState::default();
    for frame in (range.first..=range.last)
        .rev()
        .chain(range.first..=range.last)
    {
        let replay = cursor.seek(recording, frame, || false).unwrap();
        replay.update.apply_owned(&mut state);
        assert_eq!(state, fixture.states[frame]);
        assert_eq!(recording.state_at(frame).unwrap(), fixture.states[frame]);
        assert_eq!(
            postcard::to_stdvec(&replay.analysis).unwrap(),
            fixture.analysis[frame]
        );
        assert_eq!(
            postcard::to_stdvec(&recording.analysis_at(frame).unwrap()).unwrap(),
            fixture.analysis[frame]
        );
        let info = recording.frame_info(frame).unwrap();
        assert_eq!(
            (info.index, info.timestamp, info.scene_id),
            (frame, 8_000_000_000 + frame as u64 * 1_000_003, 101)
        );
    }
    for frame in (0..range.first).chain(range.last + 1..fixture.states.len()) {
        assert!(!recording.is_frame_retained(frame));
        assert!(recording.state_at(frame).is_err());
        assert!(recording.analysis_at(frame).is_err());
        assert!(cursor.seek(recording, frame, || false).is_err());
    }
}

#[test]
fn keyed_capture_reopens_and_recrops_with_independent_complete_scene_and_analysis() {
    for paged in [false, true] {
        let mut fixture = keyed_fixture(paged);
        let source = fixture.recording.as_ref().unwrap();
        let disk = source.disk.as_ref().unwrap();
        let mut schema_ids = BTreeSet::new();
        for frame in 0..8 {
            let segment = disk.segment(frame).unwrap();
            let (mut reader, id) = frame_object(segment, frame);
            let Record::KeyedObject(object) =
                read_record(&mut reader, id, segment.committed_end).unwrap()
            else {
                panic!("schema was not selected")
            };
            assert!(
                segment
                    .resources
                    .pin(id)
                    .unwrap()
                    .dependency_ids()
                    .contains(&object.keys)
            );
            schema_ids.insert(object.keys);
        }
        assert_eq!(schema_ids.len(), 2);
        let (bytes, report) = fixture.export(CaptureRange { first: 3, last: 6 });
        assert_eq!(report.format, Format::V3);
        assert!(bytes.starts_with(MAGIC));
        let path = fixture.root.join("keyed.pvdcapture");
        std::fs::write(&path, &bytes).unwrap();
        fixture.recording.take();
        fixture.history.take();
        let mut recording = crate::Recording::from_path(&path).unwrap();
        assert_range(&recording, &fixture, CaptureRange { first: 3, last: 6 });
        for (number, range) in [
            CaptureRange { first: 4, last: 6 },
            CaptureRange { first: 5, last: 5 },
        ]
        .into_iter()
        .enumerate()
        {
            let output = fixture.root.join(format!("keyed-crop-{number}.pvdcapture"));
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
            recording = crate::Recording::from_path(&output).unwrap();
            assert_range(&recording, &fixture, range);
        }
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        drop(recording);
    }
}

#[test]
fn keyed_capture_rejects_damage_version_mismatch_and_budget_and_cancellation() {
    let fixture = keyed_fixture(true);
    let source = fixture.recording.as_ref().unwrap();
    let (bytes, _) = fixture.export(CaptureRange { first: 3, last: 6 });
    let path = fixture.root.join("keyed-valid.pvdcapture");
    std::fs::write(&path, &bytes).unwrap();
    let recording = crate::Recording::from_path(&path).unwrap();
    for (name, limits) in [
        (
            "bytes",
            CaptureLimits {
                output_bytes: 4096,
                ..Default::default()
            },
        ),
        (
            "index",
            CaptureLimits {
                index_bytes: 4096,
                ..Default::default()
            },
        ),
        (
            "ids",
            CaptureLimits {
                resources: 1,
                ..Default::default()
            },
        ),
    ] {
        let output = fixture.root.join(format!("rejected-{name}.pvdcapture"));
        assert!(
            write_retained_new(
                &recording,
                &output,
                CaptureRange { first: 4, last: 6 },
                &limits,
                0,
                |_| true
            )
            .is_err()
        );
        assert!(!output.exists());
    }
    let output = fixture.root.join("cancelled.pvdcapture");
    assert!(
        write_retained_new(
            &recording,
            &output,
            CaptureRange { first: 4, last: 6 },
            &CaptureLimits::default(),
            0,
            |progress| progress.phase != CapturePhase::Copy
        )
        .is_err()
    );
    assert!(!output.exists());
    assert!(
        write_retained_new(
            &recording,
            &path,
            CaptureRange { first: 4, last: 6 },
            &CaptureLimits::default(),
            0,
            |_| true
        )
        .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let disk = source.disk.as_ref().unwrap();
    let segment = disk.segment(3).unwrap();
    let (mut reader, id) = frame_object(segment, 3);
    let record =
        read_record_bytes_with_cancel(&mut reader, id, segment.committed_end, &mut || false)
            .unwrap();
    reader.capture = Some(Arc::new(CaptureBacking::new(
        Format::V1,
        File::open(&path).unwrap(),
        path.clone(),
        0,
        bytes.len() as u64,
    )));
    assert!(reader.decode(&record, &mut || false).is_err());
    reader.capture = None;
    assert!(reader.decode(&record, &mut || false).is_ok());
    for (number, cut) in [7, 49, bytes.len() - 1].into_iter().enumerate() {
        let output = fixture.root.join(format!("truncated-{number}.pvdcapture"));
        std::fs::write(&output, &bytes[..cut]).unwrap();
        assert!(crate::Recording::from_path(&output).is_err());
    }
    let mut damaged = bytes.clone();
    damaged[70] ^= 1;
    let corrupt = fixture.root.join("corrupt.pvdcapture");
    std::fs::write(&corrupt, damaged).unwrap();
    assert!(crate::Recording::from_path(&corrupt).is_err());
    assert_eq!(Format::from_magic(b"PVDCAP04").unwrap(), Some(Format::V4));
    assert_eq!(Format::from_magic(b"PVDSET01").unwrap(), Some(Format::Set));
    assert!(Format::from_magic(b"PVDCAP05").is_err());
    assert_eq!(Format::from_magic(b"notcapture").unwrap(), None);
    assert!(std::fs::read_dir(&fixture.root).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".pvd-capture-")
    }));
    drop(reader);
    drop(recording);
}

const EMPTY_ANALYSIS: &[u8] = &[
    5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 128, 128, 128, 64,
];
const EMPTY_CHECKPOINT: &[u8] = &[3, 0, 0, 0, 0, 0, 0];
const FRAME_20: &[u8] = &[4, 20, 232, 7, 42, 0, 0, 0, 0, 0, 0, 0];

pub(super) fn small_v1_fixture() -> Vec<u8> {
    let mut output = Cursor::new(LEGACY_MAGIC.to_vec());
    output.set_position(8);
    let mut hash = blake3::Hasher::new();
    hash.update(LEGACY_MAGIC);
    let mut writer = wire::Writer {
        output: &mut output,
        bytes: 8,
        hash,
        limit: 1 << 20,
    };
    let metadata = wire::u64_bytes([1, 20, 20, 1, 1000, 1, 1, 1, 0, 0]);
    writer
        .entry(wire::METADATA, &[&metadata], &mut || false)
        .unwrap();
    let checksum = *blake3::hash(EMPTY_ANALYSIS).as_bytes();
    writer
        .entry(
            wire::BLOCK,
            &[
                &1u64.to_le_bytes(),
                &(EMPTY_ANALYSIS.len() as u32).to_le_bytes(),
                &checksum,
                EMPTY_ANALYSIS,
            ],
            &mut || false,
        )
        .unwrap();
    let id = (1 << 63) | 1;
    let mut blob = wire::u64_bytes([id, EMPTY_ANALYSIS.len() as u64]);
    blob.extend(checksum);
    blob.extend(wire::u64_bytes([1, 1, 0]));
    writer.entry(wire::BLOB, &[&blob], &mut || false).unwrap();
    let mut record = |bytes: &[u8]| {
        writer
            .entry(
                wire::RECORD,
                &[
                    &(bytes.len() as u32).to_le_bytes(),
                    blake3::hash(bytes).as_bytes(),
                    bytes,
                ],
                &mut || false,
            )
            .unwrap()
    };
    let checkpoint = record(EMPTY_CHECKPOINT);
    let frame = record(FRAME_20);
    let directory = wire::u64_bytes([
        id,
        u64::MAX,
        0,
        1,
        20,
        1000,
        42,
        frame,
        u64::MAX,
        1,
        20,
        checkpoint,
    ]);
    writer
        .entry(wire::SEGMENT, &[&directory], &mut || false)
        .unwrap();
    let mut footer = wire::u64_bytes([writer.bytes, 1]);
    footer.extend(writer.hash.finalize().as_bytes());
    writer
        .entry(wire::FOOTER, &[&footer], &mut || false)
        .unwrap();
    output.into_inner()
}

#[test]
fn fixed_v1_records_open_under_legacy_magic_and_export_as_explicit_v3() {
    let fixture = Fixture::new(false);
    let bytes = small_v1_fixture();
    let Record::Analysis(analysis) = decode_record(EMPTY_ANALYSIS, &mut || false).unwrap() else {
        panic!("fixed analysis variant")
    };
    assert_eq!(
        postcard::to_stdvec(&*analysis).unwrap(),
        postcard::to_stdvec(&Analysis::default()).unwrap()
    );
    let path = fixture.root.join("fixed-v1.pvdcapture");
    std::fs::write(&path, &bytes).unwrap();
    let recording = crate::Recording::from_path(&path).unwrap();
    assert_eq!(recording.capture_format(), Some(Format::V1));
    assert_eq!(recording.first_frame(), 20);
    assert_eq!(recording.frame_count(), 21);
    assert_eq!(recording.state_at(20).unwrap(), SceneState::default());
    assert_eq!(
        postcard::to_stdvec(&recording.analysis_at(20).unwrap()).unwrap(),
        postcard::to_stdvec(&Analysis::default()).unwrap()
    );
    let output = fixture.root.join("fixed-v1-cropped.pvdcapture");
    assert_eq!(
        write_retained_new(
            &recording,
            &output,
            CaptureRange {
                first: 20,
                last: 20
            },
            &CaptureLimits::default(),
            0,
            |_| true
        )
        .unwrap()
        .format,
        Format::V3
    );
    let reopened = crate::Recording::from_path(&output).unwrap();
    assert_eq!(reopened.capture_format(), Some(Format::V3));
    assert_eq!(reopened.state_at(20).unwrap(), SceneState::default());
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    drop(reopened);
    drop(recording);
}

#[test]
#[ignore = "requires PVD_CAPTURE_V1_FIXTURES with the unchanged 311-generated rich/all 20..60 captures"]
fn actual_311_v1_fixtures_remain_readable_and_can_be_recropped() {
    let directory = PathBuf::from(
        std::env::var_os("PVD_CAPTURE_V1_FIXTURES").expect("PVD_CAPTURE_V1_FIXTURES"),
    );
    let fixture = Fixture::new(false);
    for (name, length, expected) in [
        (
            "rich-20-60.pvdcapture",
            2_402_879,
            "b91cb56ae322d208685a80ed2f9790d9e20913f4f1da8ae044911a096a73e13d",
        ),
        (
            "all-20-60.pvdcapture",
            2_383_177,
            "f110472fc09ff6e92001ef2dd2c226afc4952db0f536ec78b8f1e3968ad097aa",
        ),
    ] {
        let path = directory.join(name);
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), length);
        assert!(bytes.starts_with(LEGACY_MAGIC));
        assert_eq!(blake3::hash(&bytes).to_hex().as_str(), expected);
        let source = crate::Recording::from_path(&path).unwrap();
        assert_eq!(source.capture_format(), Some(Format::V1));
        assert_eq!((source.first_frame(), source.frame_count()), (20, 61));
        let states: Vec<_> = (25..=55)
            .map(|frame| source.state_at(frame).unwrap())
            .collect();
        let analysis: Vec<_> = (25..=55)
            .map(|frame| postcard::to_stdvec(&source.analysis_at(frame).unwrap()).unwrap())
            .collect();
        let output = fixture.root.join(name);
        write_retained_new(
            &source,
            &output,
            CaptureRange {
                first: 25,
                last: 55,
            },
            &CaptureLimits::default(),
            0,
            |_| true,
        )
        .unwrap();
        drop(source);
        let cropped = crate::Recording::from_path(output).unwrap();
        assert_eq!(cropped.capture_format(), Some(Format::V3));
        for frame in (25..=55).rev().chain(25..=55) {
            assert_eq!(cropped.state_at(frame).unwrap(), states[frame - 25]);
            assert_eq!(
                postcard::to_stdvec(&cropped.analysis_at(frame).unwrap()).unwrap(),
                analysis[frame - 25]
            );
        }
        assert!(cropped.state_at(24).is_err());
        assert!(cropped.state_at(56).is_err());
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        drop(cropped);
    }
}
