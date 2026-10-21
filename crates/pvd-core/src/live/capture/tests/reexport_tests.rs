use super::*;

fn saved_source(fixture: &mut Fixture) -> (PathBuf, crate::Recording, Vec<u8>) {
    let (bytes, _) = fixture.export(CaptureRange { first: 3, last: 6 });
    let path = fixture.root.join("source.pvdcapture");
    std::fs::write(&path, &bytes).unwrap();
    fixture.recording.take();
    fixture.history.take();
    let recording = crate::Recording::from_path(&path).unwrap();
    (path, recording, bytes)
}

fn assert_visible(
    recording: &crate::Recording,
    fixture: &Fixture,
    range: CaptureRange,
    paged: bool,
) {
    assert_eq!(recording.source, crate::RecordingSource::ProjectCapture);
    assert_eq!(recording.first_frame(), range.first);
    assert_eq!(recording.frame_count(), range.last + 1);
    assert_eq!(recording.frames.len(), range.last - range.first + 1);
    let mut cursor = ReplayCursor::default();
    let mut scene = SceneState::default();
    for frame in (range.first..=range.last)
        .rev()
        .chain(range.first..=range.last)
    {
        assert_eq!(recording.state_at(frame).unwrap(), fixture.states[frame]);
        let actual = recording.analysis_at(frame).unwrap();
        assert_eq!(actual.resident().is_some(), paged);
        assert_eq!(
            postcard::to_stdvec(&actual).unwrap(),
            fixture.analysis[frame]
        );
        let replay = cursor.seek(recording, frame, || false).unwrap();
        replay.update.apply_owned(&mut scene);
        assert_eq!(scene, fixture.states[frame]);
        assert_eq!(
            postcard::to_stdvec(&replay.analysis).unwrap(),
            fixture.analysis[frame]
        );
    }
    for frame in (0..range.first).chain(range.last + 1..fixture.states.len()) {
        assert!(!recording.is_frame_retained(frame));
        assert!(recording.state_at(frame).is_err());
        assert!(recording.analysis_at(frame).is_err());
        assert!(cursor.seek(recording, frame, || false).is_err());
    }
}

fn assert_no_temporary(root: &Path) {
    assert!(std::fs::read_dir(root).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".pvd-capture-")
    }));
}

#[test]
fn project_captures_can_be_trimmed_repeatedly_without_exposing_preroll() {
    for paged in [false, true] {
        let mut fixture = Fixture::with_frame_clock(
            paged,
            32 << 20,
            FrameClock {
                origin: 8_000_000_000,
                step: 1_000_003,
                scene_id: 101,
                ratio: (7, 3),
            },
        );
        let source = fixture.recording.as_ref().unwrap();
        let source_frames: Vec<_> = source.frames.iter().cloned().collect();
        let source_seconds: Vec<_> = (0..source.frame_count())
            .map(|frame| source.seconds_at(frame).unwrap())
            .collect();
        assert_eq!(source_seconds[0], 0.0);
        assert_eq!(source_seconds[3], 0.007_000_021);
        let ratio = (
            source.header.timestamp_numerator,
            source.header.timestamp_denominator,
        );
        let assert_timing = |recording: &crate::Recording, range: CaptureRange| {
            assert_eq!(
                (
                    recording.header.timestamp_numerator,
                    recording.header.timestamp_denominator,
                ),
                ratio
            );
            assert_eq!(recording.seconds_at(range.first), Some(0.0));
            for frame in range.first..=range.last {
                let expected = &source_frames[frame];
                let actual = recording.frame_info(frame).unwrap();
                assert_eq!(actual.index, expected.index);
                assert_eq!(actual.timestamp, expected.timestamp);
                assert_eq!(actual.scene_id, expected.scene_id);
                let relative = source_seconds[frame] - source_seconds[range.first];
                assert!(
                    (recording.seconds_at(frame).unwrap() - relative).abs() <= 4.0 * f64::EPSILON
                );
            }
        };
        let (mut path, mut recording, _) = saved_source(&mut fixture);
        assert_timing(&recording, CaptureRange { first: 3, last: 6 });
        for (number, range) in [
            CaptureRange { first: 4, last: 6 },
            CaptureRange { first: 5, last: 6 },
            CaptureRange { first: 5, last: 5 },
            CaptureRange { first: 5, last: 5 },
        ]
        .into_iter()
        .enumerate()
        {
            let parent_bytes = std::fs::read(&path).unwrap();
            let next = fixture.root.join(format!("crop-{number}.pvdcapture"));
            let report = write_retained_new(
                &recording,
                &next,
                range,
                &CaptureLimits::default(),
                0,
                |_| true,
            )
            .unwrap();
            let saved = std::fs::read(&next).unwrap();
            assert_eq!(report.bytes, saved.len() as u64);
            assert_eq!(report.digest, *blake3::hash(&saved).as_bytes());
            assert_eq!(report.frames, range.last - range.first + 1);
            assert_eq!(std::fs::read(&path).unwrap(), parent_bytes);
            let reopened = crate::Recording::from_path(&next).unwrap();
            assert!(
                !recording.disk.as_ref().unwrap().segments[0]
                    .resources
                    .same(&reopened.disk.as_ref().unwrap().segments[0].resources)
            );
            drop(recording);
            std::fs::remove_file(&path).unwrap();
            recording = reopened;
            path = next;
            assert_visible(&recording, &fixture, range, paged);
            assert_timing(&recording, range);
            if range.first == 5 {
                let directory = read::directory(
                    &mut Cursor::new(&saved),
                    saved.len() as u64,
                    &CaptureLimits::default(),
                    &mut |_| true,
                )
                .unwrap();
                assert_eq!(directory.segments[0].frames[0].info.index, 4);
                let hidden = fixture.root.join(format!("hidden-{number}.pvdcapture"));
                assert!(
                    write_retained_new(
                        &recording,
                        &hidden,
                        CaptureRange { first: 4, last: 5 },
                        &CaptureLimits::default(),
                        0,
                        |_| true
                    )
                    .is_err()
                );
                assert!(!hidden.exists());
            }
        }
        drop(recording);
        assert_no_temporary(&fixture.root);
    }
}

#[test]
fn project_reexport_preserves_source_and_cleans_cancelled_or_limited_outputs() {
    let mut fixture = Fixture::new(true);
    let (path, recording, source_bytes) = saved_source(&mut fixture);
    let range = CaptureRange { first: 3, last: 6 };
    let failure = write_retained_new(
        &recording,
        &path,
        range,
        &CaptureLimits::default(),
        0,
        |_| true,
    )
    .unwrap_err();
    assert_eq!(
        failure.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::AlreadyExists
    );
    for phase in [
        CapturePhase::Inspect,
        CapturePhase::Copy,
        CapturePhase::Validate,
    ] {
        let output = fixture.root.join(format!("cancel-{phase:?}.pvdcapture"));
        let failure = write_retained_new(
            &recording,
            &output,
            range,
            &CaptureLimits::default(),
            0,
            |progress| progress.phase != phase,
        )
        .unwrap_err();
        assert_eq!(
            failure.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
        assert!(!output.exists());
        assert_no_temporary(&fixture.root);
    }
    for (number, limits) in [
        CaptureLimits {
            output_bytes: 4096,
            ..Default::default()
        },
        CaptureLimits {
            index_bytes: 4096,
            ..Default::default()
        },
        CaptureLimits {
            resources: 1,
            ..Default::default()
        },
        CaptureLimits {
            frames: 3,
            ..Default::default()
        },
    ]
    .into_iter()
    .enumerate()
    {
        let output = fixture.root.join(format!("limited-{number}.pvdcapture"));
        assert!(write_retained_new(&recording, &output, range, &limits, 0, |_| true).is_err());
        assert!(!output.exists());
        assert_no_temporary(&fixture.root);
    }
    let output = fixture.root.join("preroll-limit.pvdcapture");
    assert!(
        write_retained_new(
            &recording,
            &output,
            CaptureRange { first: 5, last: 5 },
            &CaptureLimits {
                frames: 1,
                ..Default::default()
            },
            0,
            |_| true
        )
        .is_err()
    );
    assert!(!output.exists());
    assert_no_temporary(&fixture.root);
    assert_eq!(std::fs::read(path).unwrap(), source_bytes);
    assert_visible(&recording, &fixture, range, true);
}

fn source_with_replaced_backing(
    source: &crate::Recording,
    original: &[u8],
    path: &Path,
) -> crate::Recording {
    let directory = read::directory(
        &mut Cursor::new(original),
        original.len() as u64,
        &CaptureLimits::default(),
        &mut |_| true,
    )
    .unwrap();
    let backing = Arc::new(CaptureBacking {
        format: directory.format,
        file: File::open(path).unwrap(),
        path: path.to_owned(),
        bytes: original.len() as u64,
    });
    let resources = Resources::from_capture(
        backing,
        directory.blocks,
        directory.blobs,
        CaptureLimits::default().index_bytes,
        &mut || false,
    )
    .unwrap();
    let mut result = source.clone();
    let disk = result.disk.as_mut().unwrap();
    assert!(disk.active.is_none());
    let mut segments = disk.segments.as_ref().clone();
    for segment in &mut segments {
        let segment = Arc::make_mut(segment);
        segment.path = path.to_owned();
        segment.resources = resources.clone();
        segment.lease = ResourceLease::new(vec![resources.pin(segment.analysis_base).unwrap()]);
        segment.readers = SegmentReaders::default();
    }
    disk.segments = Arc::new(segments);
    result
}

#[test]
fn reexport_checks_record_decoding_and_resource_checksums_again() {
    let mut fixture = Fixture::new(true);
    let (_, recording, original) = saved_source(&mut fixture);
    for record in [true, false] {
        let mut damaged = original.clone();
        if record {
            rewrite_entry(&mut damaged, wire::RECORD, |payload| {
                let bytes = &mut payload[36..];
                bytes[0] = 3;
                bytes[1..11].copy_from_slice(&postcard::to_stdvec(&u64::MAX).unwrap());
                let digest = blake3::hash(bytes);
                payload[4..36].copy_from_slice(digest.as_bytes());
            });
        } else {
            rewrite_entry(&mut damaged, wire::BLOCK, |payload| {
                *payload.last_mut().unwrap() ^= 1;
            });
        }
        let path = fixture.root.join(format!("damaged-{record}.pvdcapture"));
        std::fs::write(&path, &damaged).unwrap();
        let source = source_with_replaced_backing(&recording, &original, &path);
        let output = fixture.root.join(format!("rejected-{record}.pvdcapture"));
        let failure = write_retained_new(
            &source,
            &output,
            CaptureRange { first: 3, last: 6 },
            &CaptureLimits::default(),
            0,
            |_| true,
        )
        .unwrap_err();
        assert!(
            failure.to_string().contains(if record {
                "collection count exceeds"
            } else {
                "checksum mismatch"
            }),
            "{failure}"
        );
        assert!(!output.exists());
        assert_eq!(std::fs::read(path).unwrap(), damaged);
        assert_no_temporary(&fixture.root);
    }
}
