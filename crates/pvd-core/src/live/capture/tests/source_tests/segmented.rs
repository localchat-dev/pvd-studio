use super::*;

struct CaptureSet {
    manifest: PathBuf,
    parts: PathBuf,
}

impl CaptureSet {
    fn create(fixture: &Fixture) -> Self {
        Self::with_ranges(fixture, &[(3, 4), (5, 6), (7, 10)])
    }

    fn with_ranges(fixture: &Fixture, ranges: &[(usize, usize)]) -> Self {
        let parts = fixture.root.join("pvdset-0000000000000007.parts");
        std::fs::create_dir(&parts).unwrap();
        let set = Self {
            manifest: fixture.root.join("clocks.pvdset"),
            parts,
        };
        let mut bytes = b"PVDSET01".to_vec();
        bytes.extend(7u64.to_le_bytes());
        bytes.extend((ranges.len() as u64).to_le_bytes());
        for (index, &(first, last)) in ranges.iter().enumerate() {
            let (part, report) = fixture.export(CaptureRange { first, last });
            std::fs::write(set.parts.join(format!("{:08}.pvdcapture", index + 1)), part).unwrap();
            bytes.extend((first as u64).to_le_bytes());
            bytes.extend((last as u64).to_le_bytes());
            bytes.extend(report.bytes.to_le_bytes());
            bytes.extend(report.digest);
        }
        bytes.extend(*blake3::hash(&bytes).as_bytes());
        std::fs::write(&set.manifest, bytes).unwrap();
        set
    }
}

impl Drop for CaptureSet {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.parts).unwrap();
    }
}

#[test]
fn capture_set_source_index_and_general_navigation_have_independent_limits() {
    let mut fixture = source_fixture(false);
    let mut sources = FrameDirectory::default();
    let range: Vec<_> = (0..FRAME_COUNT).map(|index| (index, index)).collect();
    for &(index, _) in &range {
        sources
            .append(
                index,
                FrameBinding {
                    source: FrameSourceKey::Captured {
                        section: FrameSection::Apex,
                        source: 100 + index as u64,
                    },
                    scene: None,
                    source_object: None,
                },
            )
            .unwrap();
    }
    let limits = CaptureLimits {
        index_bytes: sources.index_bytes(),
        ..Default::default()
    };
    fixture.recording.as_mut().unwrap().frame_sources = sources;
    let set = CaptureSet::with_ranges(&fixture, &range);
    let reopened = open_segmented_with_progress(&set.manifest, &limits, |_| true).unwrap();
    assert_eq!(reopened.frame_sources.index_bytes(), limits.index_bytes);
    assert_eq!(reopened.frames.len(), FRAME_COUNT);
    assert_eq!(
        reopened.state_at(FRAME_COUNT - 1).unwrap(),
        fixture.states[FRAME_COUNT - 1]
    );
    let smaller = CaptureLimits {
        index_bytes: limits.index_bytes - 1,
        ..limits
    };
    assert!(open_segmented_with_progress(&set.manifest, &smaller, |_| true).is_err());
}

#[test]
fn capture_set_reexport_rejects_substituted_hidden_scene_history() {
    let fixture = source_fixture(false);
    let other = source_fixture_with_step_offset(false, 1);
    let set = CaptureSet::create(&fixture);
    let (part, report) = other.export(CaptureRange { first: 5, last: 6 });
    std::fs::write(set.parts.join("00000002.pvdcapture"), part).unwrap();
    let mut manifest = std::fs::read(&set.manifest).unwrap();
    let entry = 24 + 56;
    manifest[entry + 16..entry + 24].copy_from_slice(&report.bytes.to_le_bytes());
    manifest[entry + 24..entry + 56].copy_from_slice(&report.digest);
    let end = manifest.len() - 32;
    let checksum = blake3::hash(&manifest[..end]);
    manifest[end..].copy_from_slice(checksum.as_bytes());
    std::fs::write(&set.manifest, manifest).unwrap();
    let recording = crate::Recording::from_path(&set.manifest).unwrap();
    assert_eq!(recording.state_at(4).unwrap(), fixture.states[4]);
    assert_eq!(recording.state_at(5).unwrap(), other.states[5]);
    let output = fixture.root.join("conflict.pvdcapture");
    let failure = write_retained_new(
        &recording,
        &output,
        CaptureRange { first: 4, last: 5 },
        &Default::default(),
        0,
        |_| true,
    )
    .unwrap_err()
    .to_string();
    assert!(failure.contains("restoration prefix"), "{failure}");
    assert!(!output.exists());
}

#[test]
fn capture_set_reopens_source_clocks_rooms_and_reexports_across_hidden_prefixes() {
    let mut fixture = source_fixture(true);
    let range = CaptureRange { first: 3, last: 10 };
    let room = FrameObject {
        id: PHYSX,
        generation: 4,
    };
    let source = fixture.recording.as_mut().unwrap();
    source.rooms.observe(room, 0);
    source.rooms.destroy(room, 9);
    let set = CaptureSet::create(&fixture);
    fixture.recording.take();
    fixture.history.take();
    let imported = crate::Recording::from_path(&set.manifest).unwrap();
    assert_eq!(imported.capture_format(), Some(Format::Set));
    assert_source_range(&imported, &fixture, range);
    assert_eq!(
        imported.rooms.get(room).unwrap().destroyed_at_frame,
        Some(9)
    );
    imported.verify_replay().unwrap();
    let output = fixture.root.join("recrop.pvdcapture");
    write_retained_new(
        &imported,
        &output,
        range,
        &CaptureLimits::default(),
        0,
        |_| true,
    )
    .unwrap();
    drop(imported);
    let cropped = crate::Recording::from_path(&output).unwrap();
    assert_eq!(cropped.capture_format(), Some(Format::V3));
    assert_source_range(&cropped, &fixture, range);
    assert_eq!(std::fs::read_dir(&set.parts).unwrap().count(), 3);
}

#[test]
fn capture_set_reports_part_replacement_during_admission_and_later_seek() {
    let mut fixture = source_fixture(false);
    let set = CaptureSet::create(&fixture);
    let recording = crate::Recording::from_path(&set.manifest).unwrap();
    recording.state_at(3).unwrap();
    fixture.recording.as_mut().unwrap().header.flags ^= 1;
    let (replacement, report) = fixture.export(CaptureRange { first: 7, last: 10 });
    let last = set.parts.join("00000003.pvdcapture");
    std::fs::write(&last, replacement).unwrap();
    let failure = recording.state_at(10).unwrap_err().to_string();
    assert!(
        failure.contains("00000003.pvdcapture") && failure.contains("changed"),
        "{failure}"
    );
    assert_eq!(recording.state_at(4).unwrap(), fixture.states[4]);
    let failure = crate::Recording::from_path(&set.manifest)
        .unwrap_err()
        .to_string();
    assert!(
        failure.contains("00000003.pvdcapture") && failure.contains("checksum"),
        "{failure}"
    );

    let mut manifest = std::fs::read(&set.manifest).unwrap();
    let end = manifest.len() - 32;
    manifest[end - 32..end].copy_from_slice(&report.digest);
    let checksum = blake3::hash(&manifest[..end]);
    manifest[end..].copy_from_slice(checksum.as_bytes());
    std::fs::write(&set.manifest, manifest).unwrap();
    let failure = crate::Recording::from_path(&set.manifest)
        .unwrap_err()
        .to_string();
    assert!(
        failure.contains("00000003.pvdcapture") && failure.contains("metadata"),
        "{failure}"
    );
}

#[test]
fn packed_members_preserve_relative_offsets_source_clocks_and_recrop() {
    let fixture = source_fixture(true);
    let path = fixture.root.join("nested.pvdcapture");
    let mut bytes = b"PVDCAP04".to_vec();
    let mut entries = Vec::new();
    for (first, last) in [(3, 4), (5, 6), (7, 10)] {
        let (payload, report) = fixture.export(CaptureRange { first, last });
        for field in [first as u64, last as u64, bytes.len() as u64, report.bytes] {
            entries.extend(field.to_le_bytes());
        }
        entries.extend(report.digest);
        bytes.extend(payload);
    }
    let offset = bytes.len() as u64;
    let mut footer = 3u64.to_le_bytes().to_vec();
    footer.extend(offset.to_le_bytes());
    let mut hash = blake3::Hasher::new();
    hash.update(&bytes[..8]);
    hash.update(&entries);
    hash.update(&footer);
    bytes.extend(entries);
    bytes.extend(footer);
    bytes.extend(hash.finalize().as_bytes());
    std::fs::write(&path, &bytes).unwrap();
    let recording = crate::Recording::from_path(&path).unwrap();
    assert_eq!(recording.capture_format(), Some(Format::V4));
    assert_eq!(
        recording.disk_usage().unwrap().disk_bytes,
        bytes.len() as u64
    );
    let range = CaptureRange { first: 3, last: 10 };
    assert_source_range(&recording, &fixture, range);
    recording.verify_replay().unwrap();
    let recrop = fixture.root.join("inner-crop.pvdcapture");
    write_retained_new(&recording, &recrop, range, &Default::default(), 0, |_| true).unwrap();
    drop(recording);
    assert_source_range(
        &crate::Recording::from_path(recrop).unwrap(),
        &fixture,
        range,
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn embedded_file_views_reject_outside_seeks_and_end_at_their_own_boundary() {
    use std::io::SeekFrom;
    let fixture = source_fixture(false);
    let path = fixture.root.join("windows.bin");
    std::fs::write(&path, b"abcd1234wxyz").unwrap();
    let file = File::open(path).unwrap();
    let mut first = packed_read::FileView::new(&file, 0, 4).unwrap();
    let mut middle = packed_read::FileView::new(&file, 4, 4).unwrap();
    let mut output = [0; 8];
    assert_eq!(first.seek(SeekFrom::End(-1)).unwrap(), 3);
    assert!(middle.seek(SeekFrom::Current(-1)).is_err());
    assert_eq!(middle.read(&mut output).unwrap(), 4);
    assert_eq!(&output[..4], b"1234");
    assert_eq!(middle.read(&mut output).unwrap(), 0);
    assert_eq!(first.read(&mut output).unwrap(), 1);
    assert_eq!(output[0], b'd');
    assert!(middle.seek(SeekFrom::Start(5)).is_err());
    assert!(packed_read::FileView::new(&file, u64::MAX, 4).is_err());
}
