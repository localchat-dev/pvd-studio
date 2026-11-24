use super::*;
use crate::analysis::resident::full_fixture_inputs;
use std::io::Cursor;
use std::sync::atomic::Ordering;

mod admission_tests;
mod key_schema;
mod portable;
mod reexport_tests;
mod source_tests;

struct FrameClock {
    origin: u64,
    step: u64,
    scene_id: u64,
    ratio: (u64, u64),
}

struct Fixture {
    root: PathBuf,
    history: Option<HistoryWriter>,
    recording: Option<crate::Recording>,
    states: Vec<SceneState>,
    analysis: Vec<Vec<u8>>,
}
impl Fixture {
    fn new(paged: bool) -> Self {
        Self::with_budget(paged, 32 << 20)
    }
    fn with_budget(paged: bool, disk_budget_bytes: u64) -> Self {
        Self::with_frame_clock(
            paged,
            disk_budget_bytes,
            FrameClock {
                origin: 0,
                step: 1,
                scene_id: 0,
                ratio: (1, 1),
            },
        )
    }
    fn with_frame_clock(paged: bool, disk_budget_bytes: u64, clock: FrameClock) -> Self {
        Self::with_keys(paged, disk_budget_bytes, clock, false)
    }

    fn with_keys(paged: bool, disk_budget_bytes: u64, clock: FrameClock, keyed: bool) -> Self {
        let root = std::env::temp_dir().join(format!(
            "pvd-capture-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let config = HistoryConfig {
            directory: root.join("history"),
            disk_budget_bytes,
            segment_target_bytes: 64 << 20,
            segment_frames: 2,
            min_free_disk_bytes: 0,
        };
        let mut history = if paged {
            HistoryWriter::new_paged(config).unwrap()
        } else {
            HistoryWriter::new(config).unwrap()
        };
        let mut expected = Analysis::default();
        let mut states = Vec::new();
        let mut analysis = Vec::new();
        for (frame, (id, kind, bytes)) in full_fixture_inputs(pvd_protocol::Endian::Little)
            .into_iter()
            .enumerate()
        {
            let mut inputs = Vec::new();
            if frame == 0 {
                inputs.extend([
                    AnalysisInput::Zone(3, "Physics".into()),
                    AnalysisInput::Event(3, 4, "simulate".into()),
                ]);
            }
            inputs.push(AnalysisInput::Buffer {
                id,
                kind: kind.into(),
                bytes: bytes.into(),
                little_endian: true,
                frame,
            });
            for input in &inputs {
                input.apply(&mut expected).unwrap();
            }
            let mut state = SceneState::default();
            state.objects.insert(
                71,
                Object {
                    id: 71,
                    generation: 3,
                    type_name: "Captured".into(),
                    properties: [
                        ("mesh".into(), Value::Bytes(vec![41; 8192].into())),
                        ("frame".into(), Value::U64(frame as u64)),
                    ]
                    .into(),
                },
            );
            if keyed {
                let object = state.objects.get_mut(&71).unwrap();
                for index in 0..48 {
                    object.properties.insert(
                        format!("Property_{index:03}_with_a_stable_name"),
                        Value::U64((frame + index) as u64),
                    );
                }
                object
                    .properties
                    .insert("未知属性".into(), Value::String(format!("frame {frame}")));
                if frame >= 4 {
                    object.properties.remove("Property_000_with_a_stable_name");
                    object
                        .properties
                        .insert("$synthetic".into(), Value::Bool(true));
                }
            }
            state.cameras.insert(
                "SDK".into(),
                SourceCamera {
                    position: [frame as f32, 2., 3.],
                    up: [0., 1., 0.],
                    target: [0., 0., 0.],
                },
            );
            let mut patch = SceneUpdate::capture(&state, &[71].into(), false);
            patch.cameras = state
                .cameras
                .iter()
                .map(|(name, camera)| (name.clone(), Some(*camera)))
                .collect();
            history.append(
                &FrameInfo {
                    index: frame,
                    timestamp: clock.origin + frame as u64 * clock.step,
                    scene_id: clock.scene_id,
                },
                &state,
                &patch,
                &expected,
                inputs,
            );
            assert!(
                history.snapshot().stats.warning.is_none(),
                "{:?}",
                history.snapshot().stats.warning
            );
            assert_eq!(history.snapshot().state_at(frame).unwrap(), state);
            states.push(state);
            analysis.push(postcard::to_stdvec(&expected).unwrap());
        }
        let disk = history.snapshot();
        assert_eq!(disk.frames.len(), 8);
        let mut recording = crate::RecordingBuilder::new(pvd_protocol::Header {
            version: 1,
            timestamp_numerator: clock.ratio.0,
            timestamp_denominator: clock.ratio.1,
            flags: 1,
            endian: pvd_protocol::Endian::Little,
        })
        .snapshot()
        .unwrap();
        recording.frames = disk.frames.clone();
        recording.frame_sources = crate::frame_sources::FrameDirectory::from_legacy(
            &recording.frames,
            Default::default(),
        )
        .unwrap();
        recording.disk = Some(disk);
        recording.streamed_frame_end = Some(8);
        Self {
            root,
            history: Some(history),
            recording: Some(recording),
            states,
            analysis,
        }
    }
    fn export(&self, range: CaptureRange) -> (Vec<u8>, CaptureReport) {
        let mut output = Cursor::new(Vec::new());
        let report = write_retained(
            self.recording.as_ref().unwrap(),
            &mut output,
            range,
            &CaptureLimits::default(),
            |_| true,
        )
        .unwrap();
        (output.into_inner(), report)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.recording.take();
        self.history.take();
        let history = self.root.join("history");
        if history.exists() {
            assert_eq!(
                std::fs::read_dir(&history).unwrap().count(),
                0,
                "history payload survived its last owner"
            );
            std::fs::remove_dir(history).unwrap();
        }
        for entry in std::fs::read_dir(&self.root).unwrap() {
            let path = entry.unwrap().path();
            assert!(path.is_file());
            std::fs::remove_file(path).unwrap();
        }
        std::fs::remove_dir(&self.root).unwrap();
    }
}

#[test]
fn retained_capture_reopens_legacy_and_resident_after_source_drops() {
    for paged in [false, true] {
        let mut fixture = Fixture::new(paged);
        let disk = fixture.recording.as_ref().unwrap().disk.as_ref().unwrap();
        for segment in disk
            .segments
            .iter()
            .map(AsRef::as_ref)
            .chain(disk.active.iter())
        {
            for &id in segment.checkpoints.values() {
                assert_ne!(id & (1 << 63), 0);
                assert!(
                    segment
                        .resources
                        .pin(id)
                        .unwrap()
                        .dependency_ids()
                        .is_empty()
                );
            }
        }
        assert!(
            fixture
                .recording
                .as_ref()
                .unwrap()
                .disk
                .as_ref()
                .unwrap()
                .segments
                .iter()
                .map(AsRef::as_ref)
                .chain(
                    fixture
                        .recording
                        .as_ref()
                        .unwrap()
                        .disk
                        .as_ref()
                        .unwrap()
                        .active
                        .iter()
                )
                .flat_map(|segment| segment.frames.iter())
                .any(|location| location.undo.is_some())
        );
        let (bytes, report) = fixture.export(CaptureRange { first: 3, last: 6 });
        assert_eq!(report.frames, 4);
        assert_eq!(report.digest, *blake3::hash(&bytes).as_bytes());
        assert!(report.resource_count > 0);
        assert!(report.block_count > 0);
        let path = fixture.root.join("saved.pvdcapture");
        std::fs::write(&path, &bytes).unwrap();
        fixture.recording.take();
        fixture.history.take();
        let reopened = crate::Recording::from_path(&path).unwrap();
        let disk = reopened.disk.as_ref().unwrap();
        for segment in disk
            .segments
            .iter()
            .map(AsRef::as_ref)
            .chain(disk.active.iter())
        {
            assert!(
                segment
                    .frames
                    .iter()
                    .all(|location| location.undo.is_none())
            );
            assert!(!segment.checkpoints.is_empty());
            assert!(
                segment
                    .checkpoints
                    .values()
                    .all(|offset| offset & (1 << 63) == 0)
            );
        }
        assert_eq!(reopened.source, crate::RecordingSource::ProjectCapture);
        assert_eq!(reopened.first_frame(), 3);
        assert_eq!(reopened.frame_count(), 7);
        assert_eq!(reopened.frames.len(), 4);
        let mut cursor = ReplayCursor::default();
        let mut scene = SceneState::default();
        for frame in [6, 3, 4, 5, 4, 3, 6] {
            assert_eq!(reopened.state_at(frame).unwrap(), fixture.states[frame]);
            let actual = reopened.analysis_at(frame).unwrap();
            assert_eq!(actual.resident().is_some(), paged);
            assert_eq!(
                postcard::to_stdvec(&actual).unwrap(),
                fixture.analysis[frame]
            );
            let replay = cursor.seek(&reopened, frame, || false).unwrap();
            replay.update.apply_owned(&mut scene);
            assert_eq!(scene, fixture.states[frame]);
            assert_eq!(
                postcard::to_stdvec(&replay.analysis).unwrap(),
                fixture.analysis[frame]
            );
        }
        assert_eq!(cursor.scene_route_counts().persistent_undo, 0);
        for hidden in [0, 1, 2, 7] {
            assert!(reopened.state_at(hidden).is_err());
            assert!(reopened.analysis_at(hidden).is_err());
            assert!(cursor.seek(&reopened, hidden, || false).is_err());
        }
        assert!(crate::Recording::from_bytes(&bytes).is_err());
    }
}

#[test]
fn capture_rejects_truncation_corruption_limits_and_nonempty_output() {
    let fixture = Fixture::new(true);
    let (bytes, _) = fixture.export(CaptureRange { first: 3, last: 6 });
    for cut in [0, 7, 8, 49, bytes.len() - 1] {
        assert!(
            read::directory(
                &mut Cursor::new(&bytes[..cut]),
                cut as u64,
                &CaptureLimits::default(),
                &mut |_| true
            )
            .is_err()
        );
    }
    let mut damaged = bytes.clone();
    damaged[70] ^= 1;
    assert!(
        read::directory(
            &mut Cursor::new(&damaged),
            damaged.len() as u64,
            &CaptureLimits::default(),
            &mut |_| true
        )
        .is_err()
    );
    for limits in [
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
    ] {
        assert!(
            write_retained(
                fixture.recording.as_ref().unwrap(),
                &mut Cursor::new(Vec::new()),
                CaptureRange { first: 3, last: 6 },
                &limits,
                |_| true
            )
            .is_err()
        );
    }
    let mut occupied = Cursor::new(vec![1, 2, 3]);
    assert!(
        write_retained(
            fixture.recording.as_ref().unwrap(),
            &mut occupied,
            CaptureRange { first: 3, last: 6 },
            &CaptureLimits::default(),
            |_| true
        )
        .is_err()
    );
    assert_eq!(occupied.into_inner(), vec![1, 2, 3]);
}

#[test]
fn cancellation_is_observed_during_copy_verification_and_open() {
    let fixture = Fixture::new(true);
    for phase in [
        CapturePhase::Inspect,
        CapturePhase::Copy,
        CapturePhase::Validate,
    ] {
        let result = write_retained(
            fixture.recording.as_ref().unwrap(),
            &mut Cursor::new(Vec::new()),
            CaptureRange { first: 3, last: 6 },
            &CaptureLimits::default(),
            |state| state.phase != phase,
        );
        assert_eq!(
            result
                .unwrap_err()
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .kind(),
            std::io::ErrorKind::Interrupted
        );
    }
    let (bytes, _) = fixture.export(CaptureRange { first: 3, last: 6 });
    let path = fixture.root.join("saved.pvdcapture");
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(
        open_with_progress(&path, &CaptureLimits::default(), |_| false)
            .unwrap_err()
            .downcast_ref::<std::io::Error>()
            .unwrap()
            .kind(),
        std::io::ErrorKind::Interrupted
    );
}

#[test]
fn atomic_capture_never_overwrites_and_cleans_cancelled_or_full_destinations() {
    let fixture = Fixture::new(true);
    let path = fixture.root.join("atomic.pvdcapture");
    let range = CaptureRange { first: 3, last: 6 };
    for phase in [CapturePhase::Copy, CapturePhase::Validate] {
        assert!(
            write_retained_new(
                fixture.recording.as_ref().unwrap(),
                &path,
                range,
                &CaptureLimits::default(),
                0,
                |state| state.phase != phase
            )
            .is_err()
        );
        assert!(!path.exists());
        assert!(!std::fs::read_dir(&fixture.root).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }
    assert!(
        write_retained_new(
            fixture.recording.as_ref().unwrap(),
            &path,
            range,
            &CaptureLimits::default(),
            u64::MAX,
            |_| true
        )
        .is_err()
    );
    assert!(!path.exists());
    write_retained_new(
        fixture.recording.as_ref().unwrap(),
        &path,
        range,
        &CaptureLimits::default(),
        0,
        |_| true,
    )
    .unwrap();
    let saved = std::fs::read(&path).unwrap();
    assert!(
        write_retained_new(
            fixture.recording.as_ref().unwrap(),
            &path,
            range,
            &CaptureLimits::default(),
            0,
            |_| true
        )
        .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), saved);
    let reopened = crate::Recording::from_path(path).unwrap();
    assert_eq!(reopened.state_at(6).unwrap(), fixture.states[6]);
}

#[test]
fn expiry_during_copy_fails_without_publishing_or_pinning_the_source_lease() {
    let fixture = Fixture::new(true);
    let source = fixture.recording.as_ref().unwrap();
    let segment = source.disk.as_ref().unwrap().segments[1].clone();
    let path = fixture.root.join("expired.pvdcapture");
    let mut expired = false;
    let result = write_retained_new(
        source,
        &path,
        CaptureRange { first: 3, last: 6 },
        &CaptureLimits::default(),
        0,
        |state| {
            if !expired && state.phase == CapturePhase::Copy {
                segment.lease.invalidate().unwrap();
                expired = true;
            }
            true
        },
    );
    assert!(expired);
    assert!(result.is_err());
    assert!(!path.exists());
    assert!(!segment.lease.available());
    assert!(source.state_at(3).is_err());
    assert_eq!(source.state_at(7).unwrap(), fixture.states[7]);
}

#[test]
fn source_metadata_and_diagnostics_roundtrip_without_dense_analysis() {
    let mut fixture = Fixture::new(false);
    let source = fixture.recording.as_mut().unwrap();
    source.classes.insert(
        "Derived".into(),
        crate::ClassDefinition {
            parent: Some("Base".into()),
            properties: [(
                "flags".into(),
                crate::PropertyDefinition {
                    datatype: "u32".into(),
                    semantic: "bitmask".into(),
                    array: false,
                    named_values: vec![("On".into(), 3)],
                },
            )]
            .into(),
        },
    );
    source.diagnostics.push("captured source diagnostic".into());
    let (bytes, _) = fixture.export(CaptureRange { first: 3, last: 6 });
    let path = fixture.root.join("metadata.pvdcapture");
    std::fs::write(&path, bytes).unwrap();
    let reopened = crate::Recording::from_path(path).unwrap();
    assert_eq!(reopened.classes["Derived"].parent.as_deref(), Some("Base"));
    assert_eq!(
        reopened.classes["Derived"].properties["flags"].named_values,
        vec![("On".into(), 3)]
    );
    assert!(
        reopened
            .diagnostics
            .iter()
            .any(|text| text == "captured source diagnostic")
    );
    assert!(
        crate::analysis_export::write_analysis_csv(
            &reopened,
            &mut Vec::new(),
            crate::analysis_export::AnalysisCsvKind::Memory,
            |_| true
        )
        .is_err()
    );
}

#[test]
fn a_live_writer_can_roll_past_export_without_waiting_for_a_whole_range_pin() {
    let mut fixture = Fixture::with_budget(false, 256 << 10);
    let source = fixture.recording.as_ref().unwrap();
    let history = fixture.history.as_mut().unwrap();
    let mut advanced = false;
    let mut expected_last = SceneState::default();
    let output = fixture.root.join("rolled.pvdcapture");
    let result = write_retained_new(
        source,
        &output,
        CaptureRange { first: 3, last: 6 },
        &CaptureLimits::default(),
        0,
        |progress| {
            if progress.phase == CapturePhase::Copy && !advanced {
                advanced = true;
                let mut generator = 0x1234_5678_9abc_def0u64;
                for frame in 8..28 {
                    let bytes: Vec<u8> = (0..16 * 1024)
                        .map(|_| {
                            generator ^= generator << 13;
                            generator ^= generator >> 7;
                            generator ^= generator << 17;
                            generator as u8
                        })
                        .collect();
                    let mut state = SceneState::default();
                    state.objects.insert(
                        71,
                        Object {
                            id: 71,
                            generation: 3,
                            type_name: "Captured".into(),
                            properties: [("mesh".into(), Value::Bytes(bytes.into()))].into(),
                        },
                    );
                    let mut patch = SceneUpdate::full(&state);
                    patch.cameras.insert("SDK".into(), None);
                    history.append(
                        &FrameInfo {
                            index: frame,
                            timestamp: frame as u64,
                            scene_id: 0,
                        },
                        &state,
                        &patch,
                        &Analysis::default(),
                        Vec::new(),
                    );
                    assert!(
                        history.snapshot().stats.warning.is_none(),
                        "{:?}",
                        history.snapshot().stats.warning
                    );
                    expected_last = state;
                }
            }
            true
        },
    );
    assert!(advanced);
    assert!(result.is_err());
    assert!(!output.exists());
    let current = history.snapshot();
    assert!(current.stats.first_frame > 6);
    assert_eq!(current.state_at(27).unwrap(), expected_last);
}

#[test]
fn writer_failure_is_propagated_and_does_not_mark_the_capture_complete() {
    struct Failing {
        bytes: Cursor<Vec<u8>>,
        remaining: usize,
    }
    impl Read for Failing {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            self.bytes.read(b)
        }
    }
    impl Seek for Failing {
        fn seek(&mut self, p: SeekFrom) -> std::io::Result<u64> {
            self.bytes.seek(p)
        }
    }
    impl Write for Failing {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            if self.remaining == 0 {
                return Err(std::io::Error::other("injected destination failure"));
            }
            let n = b.len().min(self.remaining);
            self.remaining -= n;
            self.bytes.write(&b[..n])
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let fixture = Fixture::new(false);
    let mut output = Failing {
        bytes: Cursor::new(Vec::new()),
        remaining: 8192,
    };
    let result = write_retained(
        fixture.recording.as_ref().unwrap(),
        &mut output,
        CaptureRange { first: 3, last: 6 },
        &CaptureLimits::default(),
        |_| true,
    );
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("injected destination failure")
    );
    let length = output.bytes.get_ref().len() as u64;
    assert!(
        read::directory(
            &mut output.bytes,
            length,
            &CaptureLimits::default(),
            &mut |_| true
        )
        .is_err()
    );
}

fn rewrite_entry(bytes: &mut [u8], kind: u8, change: impl FnOnce(&mut [u8])) {
    let mut offset = 8;
    let mut change = Some(change);
    loop {
        let length = u64::from_le_bytes(bytes[offset + 1..offset + 9].try_into().unwrap()) as usize;
        let payload = offset + wire::ENTRY_HEADER;
        if bytes[offset] == kind
            && let Some(change) = change.take()
        {
            change(&mut bytes[payload..payload + length]);
            let mut hash = blake3::Hasher::new();
            hash.update(&bytes[offset..offset + 9]);
            hash.update(&bytes[payload..payload + length]);
            bytes[offset + 9..payload].copy_from_slice(hash.finalize().as_bytes());
        }
        if bytes[offset] == wire::FOOTER {
            let hash = blake3::hash(&bytes[..offset]);
            bytes[payload + 16..payload + 48].copy_from_slice(hash.as_bytes());
            let mut hash = blake3::Hasher::new();
            hash.update(&bytes[offset..offset + 9]);
            hash.update(&bytes[payload..payload + length]);
            bytes[offset + 9..payload].copy_from_slice(hash.finalize().as_bytes());
            break;
        }
        offset = payload + length;
    }
    assert!(change.is_none());
}

#[test]
fn valid_checksums_cannot_hide_missing_dependencies_or_unbounded_directory_counts() {
    let fixture = Fixture::new(true);
    let (bytes, _) = fixture.export(CaptureRange { first: 3, last: 6 });
    let mut missing = bytes.clone();
    rewrite_entry(&mut missing, wire::BLOB, |payload| {
        payload[56..64].copy_from_slice(&u64::MAX.to_le_bytes())
    });
    let failure = read::directory(
        &mut Cursor::new(&missing),
        missing.len() as u64,
        &CaptureLimits::default(),
        &mut |_| true,
    )
    .err()
    .unwrap();
    assert!(failure.to_string().contains("missing block"));
    let mut huge = bytes;
    rewrite_entry(&mut huge, wire::SEGMENT, |payload| {
        payload[24..32].copy_from_slice(&u64::MAX.to_le_bytes())
    });
    let failure = read::directory(
        &mut Cursor::new(&huge),
        huge.len() as u64,
        &CaptureLimits::default(),
        &mut |_| true,
    )
    .err()
    .unwrap();
    assert!(failure.to_string().contains("count exceeds"));
}

#[test]
fn index_capacity_growth_counts_old_and_new_storage_without_charging_freed_payloads() {
    let mut budget = Budget::new(128);
    let mut entries = Vec::<u64>::new();
    budget.reserve(&mut entries, 8).unwrap();
    assert_eq!(budget.used, entries.capacity() * 8);
    entries.resize(8, 1);
    assert!(budget.reserve(&mut entries, 16).is_err());
    assert_eq!(entries, vec![1; 8]);
    assert_eq!(budget.used, entries.capacity() * 8);
    budget.limit = 256;
    budget.reserve(&mut entries, 16).unwrap();
    assert_eq!(budget.peak, 192);
    assert_eq!(budget.used, entries.capacity() * 8);
    let charged = entries.capacity() * 8;
    drop(entries);
    budget.release(charged);
    assert_eq!(budget.used, 0);
}

#[test]
fn capture_destination_cannot_be_owned_by_the_live_cache() {
    let fixture = Fixture::new(false);
    let path = fixture.root.join("history").join("capture.pvdcapture");
    assert!(
        write_retained_new(
            fixture.recording.as_ref().unwrap(),
            &path,
            CaptureRange { first: 3, last: 6 },
            &CaptureLimits::default(),
            0,
            |_| true
        )
        .is_err()
    );
    assert!(!path.exists());
}

#[test]
fn destination_writes_do_not_hold_source_segment_read_guards() {
    struct Output {
        bytes: Cursor<Vec<u8>>,
        readers: Vec<SegmentReaders>,
        writes: usize,
    }
    impl Read for Output {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            self.bytes.read(out)
        }
    }
    impl Seek for Output {
        fn seek(&mut self, at: std::io::SeekFrom) -> std::io::Result<u64> {
            self.bytes.seek(at)
        }
    }
    impl Write for Output {
        fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
            assert!(
                self.readers.iter().all(|readers| !readers.active()),
                "slow destination retained a source segment reader"
            );
            self.writes += 1;
            self.bytes.write(input)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let fixture = Fixture::new(true);
    let source = fixture.recording.as_ref().unwrap();
    let disk = source.disk.as_ref().unwrap();
    let mut output = Output {
        bytes: Cursor::new(Vec::new()),
        readers: disk
            .segments
            .iter()
            .map(Arc::as_ref)
            .chain(&disk.active)
            .map(|s| s.readers.clone())
            .collect(),
        writes: 0,
    };
    let report = write_retained(
        source,
        &mut output,
        CaptureRange { first: 3, last: 6 },
        &CaptureLimits::default(),
        |_| true,
    )
    .unwrap();
    assert!(output.writes > report.frames * 10);
    let path = fixture.root.join("slow.pvdcapture");
    std::fs::write(&path, output.bytes.into_inner()).unwrap();
    let capture = crate::Recording::from_path(path).unwrap();
    assert_eq!(capture.state_at(6).unwrap(), fixture.states[6]);
}
#[test]
fn public_open_rejects_forged_record_counts_even_with_valid_archive_checksums() {
    let fixture = Fixture::new(true);
    let (mut bytes, _) = fixture.export(CaptureRange { first: 3, last: 6 });
    rewrite_entry(&mut bytes, wire::RECORD, |payload| {
        let record = &mut payload[36..];
        assert!(record.len() >= 11);
        record[0] = 3;
        record[1..11].copy_from_slice(&postcard::to_stdvec(&u64::MAX).unwrap());
        let digest = blake3::hash(record);
        payload[4..36].copy_from_slice(digest.as_bytes());
    });
    let path = fixture.root.join("record-count.pvdcapture");
    std::fs::write(&path, bytes).unwrap();
    let failure = crate::Recording::from_path(path).err().unwrap();
    assert!(
        failure.to_string().contains("collection count exceeds"),
        "{failure}"
    );
}
