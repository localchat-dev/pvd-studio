use super::*;

#[test]
fn portable_short_export_stays_byte_identical_to_v3() {
    let fixture = Fixture::new(true);
    let range = CaptureRange { first: 3, last: 7 };
    let target = fixture.root.join("short.pvdcapture");
    let report = write_portable_new(
        fixture.recording.as_ref().unwrap(),
        &target,
        range,
        &CaptureLimits::default(),
        0,
        |_| true,
    )
    .unwrap();
    assert_eq!(report.parts, 1);
    assert_eq!(report.frames, 5);
    assert_eq!(std::fs::read(target).unwrap(), fixture.export(range).0);
}

#[test]
fn portable_multi_part_file_moves_alone_and_rejects_bad_directory_offsets() {
    let mut fixture = Fixture::new(false);
    let range = CaptureRange { first: 0, last: 7 };
    let limits = CaptureLimits {
        frames: 2,
        ..Default::default()
    };
    let target = fixture.root.join("full.pvdcapture");
    let initial_files = std::fs::read_dir(&fixture.root).unwrap().count();
    let report = write_portable_new(
        fixture.recording.as_ref().unwrap(),
        &target,
        range,
        &limits,
        0,
        |_| true,
    )
    .unwrap();
    assert_eq!(report.parts, 4);
    assert_eq!(
        std::fs::read_dir(&fixture.root).unwrap().count(),
        initial_files + 1
    );
    let other_directory = Output::new(&fixture);
    let moved = other_directory.0.join("moved.pvdcapture");
    std::fs::rename(target, &moved).unwrap();
    fixture.recording.take();
    fixture.history.take();
    let mut bytes = std::fs::read(&moved).unwrap();
    assert_eq!(&bytes[..8], PACKED_MAGIC);
    assert_eq!(report.bytes, bytes.len() as u64);
    let rec = crate::Recording::from_path(&moved).unwrap();
    for index in [7, 0, 4, 3, 2] {
        assert_eq!(rec.state_at(index).unwrap(), fixture.states[index]);
    }
    drop(rec);
    let footer = bytes.len() - 48;
    bytes[footer + 16] ^= 1;
    assert!(packed::read_directory(&mut Cursor::new(&bytes), &limits, &mut |_| true).is_err());
    let dir = u64::from_le_bytes(bytes[footer + 8..footer + 16].try_into().unwrap()) as usize;
    bytes[dir + 16..dir + 24].copy_from_slice(&9u64.to_le_bytes());
    let mut hash = blake3::Hasher::new();
    hash.update(PACKED_MAGIC);
    hash.update(&bytes[dir..footer + 16]);
    bytes[footer + 16..].copy_from_slice(hash.finalize().as_bytes());
    std::fs::write(&moved, bytes).unwrap();
    assert!(crate::Recording::from_path(&moved).is_err());
}

#[test]
fn portable_failure_does_not_publish_or_leave_a_part() {
    let fixture = Fixture::new(false);
    let source = fixture.recording.as_ref().unwrap();
    let target = fixture.root.join("cancel.pvdcapture");
    let initial = std::fs::read_dir(&fixture.root).unwrap().count();
    let limits = CaptureLimits {
        frames: 2,
        ..Default::default()
    };
    let range = CaptureRange { first: 0, last: 7 };
    let failure = write_portable_new(source, &target, range, &limits, 0, |progress| {
        progress.total != 8 || progress.completed < 2
    })
    .unwrap_err();
    assert_eq!(
        failure.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
    assert_eq!(std::fs::read_dir(&fixture.root).unwrap().count(), initial);
    std::fs::write(&target, b"existing").unwrap();
    assert!(write_portable_new(source, &target, range, &limits, 0, |_| true).is_err());
    assert_eq!(std::fs::read(target).unwrap(), b"existing");
}

struct Output(PathBuf);
impl Output {
    fn new(fixture: &Fixture) -> Self {
        let path = fixture.root.join("exports");
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Output {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn contents(root: &Path) -> Vec<std::ffi::OsString> {
    let mut names: Vec<_> = std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    names
}

#[test]
fn portable_export_retries_a_resource_limited_part() {
    let fixture = Fixture::new(false);
    let output = Output::new(&fixture);
    let source = fixture.recording.as_ref().unwrap();
    let resources = |range| {
        let (_, report) = fixture.export(range);
        report.resource_count + report.block_count
    };
    let range = CaptureRange { first: 0, last: 7 };
    let per_part = (0..8)
        .step_by(2)
        .map(|first| {
            resources(CaptureRange {
                first,
                last: first + 1,
            })
        })
        .max()
        .unwrap();
    assert!(per_part < resources(range));
    let limits = CaptureLimits {
        resources: per_part,
        ..Default::default()
    };
    let report = write_portable_new(
        source,
        &output.0.join("saved.pvdcapture"),
        range,
        &limits,
        0,
        |_| true,
    )
    .unwrap();
    assert!(report.parts > 1);
    assert_eq!(report.frames, 8);
}

#[test]
fn portable_output_limit_applies_to_each_embedded_capture() {
    let fixture = Fixture::new(false);
    let target = fixture.root.join("limited.pvdcapture");
    let limit = (0..8)
        .step_by(2)
        .map(|first| {
            fixture
                .export(CaptureRange {
                    first,
                    last: first + 1,
                })
                .1
                .bytes
        })
        .max()
        .unwrap();
    let limits = CaptureLimits {
        output_bytes: limit,
        ..Default::default()
    };
    let report = write_portable_new(
        fixture.recording.as_ref().unwrap(),
        &target,
        CaptureRange { first: 0, last: 7 },
        &limits,
        0,
        |_| true,
    )
    .unwrap();
    let mut file = File::open(target).unwrap();
    let directory = packed::read_directory(&mut file, &limits, &mut |_| true).unwrap();
    assert!(directory.len() > 1);
    assert_eq!(directory.len(), report.parts);
    assert!(report.bytes > limit);
    assert!(directory.iter().all(|part| part.bytes <= limit));
}

#[test]
fn portable_export_grows_parts_again_after_two_dense_replay_segments() {
    let mut fixture = Fixture::new(false);
    let history = fixture.history.as_mut().unwrap();
    for index in 8..72 {
        let mut state = SceneState::default();
        if index < 12 {
            let mut seed = 1283u64 + index as u64 / 2;
            let bytes: Vec<u8> = (0..(48 << 10))
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    seed as u8
                })
                .collect();
            state.objects.insert(
                71,
                Object {
                    id: 71,
                    generation: 3,
                    type_name: "Captured".into(),
                    properties: [("mesh".into(), Value::Bytes(bytes.into()))].into(),
                },
            );
        }
        let patch = SceneUpdate::capture(&state, &[71].into(), false);
        history.append(
            &FrameInfo {
                index,
                timestamp: index as u64,
                scene_id: 0,
            },
            &state,
            &patch,
            &Analysis::default(),
            Vec::new(),
        );
    }
    let disk = history.snapshot();
    assert!(disk.stats.warning.is_none(), "{:?}", disk.stats.warning);
    let recording = fixture.recording.as_mut().unwrap();
    recording.frames = disk.frames.clone();
    recording.disk = Some(disk);
    recording.frame_sources =
        crate::frame_sources::FrameDirectory::from_legacy(&recording.frames, Default::default())
            .unwrap();
    recording.streamed_frame_end = Some(72);
    let limits = CaptureLimits {
        output_bytes: [8, 10]
            .into_iter()
            .map(|first| {
                fixture
                    .export(CaptureRange {
                        first,
                        last: first + 1,
                    })
                    .1
                    .bytes
            })
            .max()
            .unwrap()
            + 1024,
        ..Default::default()
    };
    let target = fixture.root.join("variable-cost.pvdcapture");
    write_portable_new(
        fixture.recording.as_ref().unwrap(),
        &target,
        CaptureRange { first: 8, last: 71 },
        &limits,
        0,
        |_| true,
    )
    .unwrap();
    let parts =
        packed::read_directory(&mut File::open(&target).unwrap(), &limits, &mut |_| true).unwrap();
    assert!(parts[0].range.last <= 9);
    assert!(
        parts
            .iter()
            .any(|part| part.range.last - part.range.first + 1 >= 8),
        "light tail stayed fragmented: {parts:?}"
    );
}

#[test]
fn portable_export_reports_incomplete_analysis_without_retrying_smaller_ranges() {
    let mut fixture = Fixture::new(false);
    let output = Output::new(&fixture);
    let source = fixture.recording.as_mut().unwrap();
    source
        .diagnostics
        .push("Analysis decoding stopped at frame 4".into());
    let mut inspected = 0;
    let result = write_portable_new(
        source,
        &output.0.join("saved.pvdcapture"),
        CaptureRange { first: 0, last: 7 },
        &CaptureLimits::default(),
        0,
        |progress| {
            if progress.phase == CapturePhase::Inspect {
                inspected += 1;
            }
            true
        },
    );
    assert!(result.unwrap_err().to_string().contains("analysis"));
    assert_eq!(inspected, 1, "one direct capture attempt");
    assert!(contents(&output.0).is_empty());
}

#[test]
fn portable_export_rejects_a_racing_destination_without_removing_it() {
    let fixture = Fixture::new(false);
    let output = Output::new(&fixture);
    let source = fixture.recording.as_ref().unwrap();
    let path = output.0.join("racing.pvdcapture");
    let result = write_portable_new(
        source,
        &path,
        CaptureRange { first: 0, last: 7 },
        &CaptureLimits::default(),
        0,
        |progress| {
            if progress.phase == CapturePhase::Copy && progress.completed == 8 {
                std::fs::write(&path, b"other export").unwrap();
            }
            true
        },
    );
    assert!(result.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"other export");
    assert_eq!(
        contents(&output.0),
        vec![path.file_name().unwrap().to_owned()]
    );
}

#[test]
fn portable_export_retries_reader_index_admission_and_keeps_the_limit_per_part() {
    let fixture = Fixture::new(false);
    let output = Output::new(&fixture);
    let path = output.0.join("reference.pvdcapture");
    let mut max_index = 0;
    for first in (0..8).step_by(2) {
        let (bytes, _) = fixture.export(CaptureRange {
            first,
            last: first + 1,
        });
        std::fs::write(&path, bytes).unwrap();
        let mut low = 4096;
        let mut high = CaptureLimits::default().index_bytes;
        while high - low > 32 {
            let middle = low + (high - low) / 2;
            let limits = CaptureLimits {
                index_bytes: middle,
                ..Default::default()
            };
            if open_with_progress(&path, &limits, |_| true).is_ok() {
                high = middle;
            } else {
                low = middle;
            }
        }
        max_index = max_index.max(high);
    }
    let range = CaptureRange { first: 0, last: 7 };
    let (bytes, _) = fixture.export(range);
    std::fs::write(&path, bytes).unwrap();
    // Admission charges the temporary and final path in each segment directory.
    let limits = CaptureLimits {
        index_bytes: max_index + 512,
        ..Default::default()
    };
    assert!(crate::CapacityLimit::caused(
        &open_with_progress(&path, &limits, |_| true).unwrap_err()
    ));
    let report = write_portable_new(
        fixture.recording.as_ref().unwrap(),
        &output.0.join("saved.pvdcapture"),
        range,
        &limits,
        0,
        |_| true,
    )
    .unwrap();
    assert!(report.parts > 1);
}
