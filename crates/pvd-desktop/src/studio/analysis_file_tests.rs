use super::*;
use pvd_core::Value;
use std::io::Cursor;

#[cfg(windows)]
mod profile;

struct Fixture {
    directory: PathBuf,
    path: PathBuf,
    history: pvd_core::live::HistoryConfig,
}

impl Fixture {
    fn new(bytes: &[u8]) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "pvd-analysis-file-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("capture.pvdstream");
        std::fs::write(&path, bytes).unwrap();
        Self {
            history: pvd_core::live::HistoryConfig {
                directory: directory.join("history"),
                disk_budget_bytes: 16 << 20,
                segment_target_bytes: 1 << 20,
                segment_frames: 1,
                min_free_disk_bytes: 0,
            },
            directory,
            path,
        }
    }

    fn load(&self, progress: &AtomicU64) -> LoadResult {
        read_recording(
            &self.path,
            &AtomicBool::new(false),
            progress,
            self.history.clone(),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn group(events: &[Vec<u8>]) -> Vec<u8> {
    let payload: Vec<_> = events.iter().flatten().copied().collect();
    let mut bytes = Vec::new();
    bytes.extend((payload.len() as u32).to_le_bytes());
    bytes.extend((events.len() as u32).to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    bytes.extend(payload);
    bytes
}

fn string(handle: u32, text: &str) -> Vec<u8> {
    let mut bytes = vec![1];
    bytes.extend((text.len() as u32 + 1).to_le_bytes());
    bytes.extend(text.as_bytes());
    bytes.push(0);
    bytes.extend(handle.to_le_bytes());
    bytes
}

fn class(name: u32) -> Vec<u8> {
    let mut bytes = vec![2];
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(name.to_le_bytes());
    bytes
}

fn instance(name: u32, id: u64) -> Vec<u8> {
    let mut bytes = vec![6];
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(name.to_le_bytes());
    bytes.extend(id.to_le_bytes());
    bytes
}

fn property(class: u32, name: u32, datatype: u32, kind: u8) -> Vec<u8> {
    let mut bytes = vec![4];
    for value in [1u32, class, name, 8, 1, datatype] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.push(kind);
    bytes.extend(0u32.to_le_bytes());
    bytes
}

fn set(id: u64, property: u32, datatype: u32, items: u32, data: &[u8]) -> Vec<u8> {
    let mut bytes = vec![7];
    bytes.extend(id.to_le_bytes());
    for value in [property, 1, datatype, items, data.len() as u32] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.extend(data);
    bytes
}

fn section(kind: u8, timestamp: u64) -> Vec<u8> {
    let mut bytes = vec![kind];
    bytes.extend(10u64.to_le_bytes());
    bytes.extend(3u32.to_le_bytes());
    bytes.extend(timestamp.to_le_bytes());
    bytes
}

fn capture(failure_before_frame: Option<usize>) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(pvd_protocol::STREAM_MAGIC.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    let mut metadata = Vec::new();
    for (handle, text) in [
        (1u32, "physx3"),
        (2, "Marker"),
        (3, "frame"),
        (4, "Value"),
        (5, "PvdU32"),
        (6, "PxScene"),
        (7, "PxProfileMemoryEventBuffer"),
        (8, ""),
        (9, "events"),
        (10, "PvdU8"),
    ] {
        metadata.push(string(handle, text));
    }
    metadata.extend([class(2), class(6), class(7)]);
    metadata.extend([property(2, 4, 5, 1), property(7, 9, 10, 2)]);
    metadata.extend([instance(2, 99), instance(6, 10), instance(7, 90)]);
    bytes.extend(group(&metadata));
    for (frame, value) in [7u32, 9].into_iter().enumerate() {
        if failure_before_frame == Some(frame) {
            bytes.extend(group(&[set(90, 9, 10, 7, &[2, 0, 56, 13, 40, 22, 99])]));
        }
        bytes.extend(group(&[
            section(18, frame as u64 * 10),
            set(99, 4, 5, 1, &value.to_le_bytes()),
            section(19, frame as u64 * 10 + 1),
        ]));
    }
    bytes
}

fn assert_scene(recording: &Recording) {
    assert_eq!(recording.frame_count(), 2);
    assert_eq!(recording.first_frame(), 0);
    for (frame, value) in [(0, 7), (1, 9), (0, 7)] {
        assert_eq!(
            recording.state_at(frame).unwrap().objects[&99].properties["Value"],
            Value::U64(value)
        );
    }
}

fn assert_incomplete_analysis_is_not_exported(recording: &Recording) {
    for kind in [
        pvd_core::analysis_export::AnalysisCsvKind::Memory,
        pvd_core::analysis_export::AnalysisCsvKind::Profile,
    ] {
        let mut csv = Vec::new();
        let error =
            pvd_core::analysis_export::write_analysis_csv(recording, &mut csv, kind, |_| true)
                .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("undefined memory type handle 13")
        );
        assert!(csv.is_empty());
    }
    let mut output = Cursor::new(Vec::new());
    let error = pvd_core::capture::write_retained(
        recording,
        &mut output,
        pvd_core::capture::CaptureRange { first: 0, last: 1 },
        &Default::default(),
        |_| true,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("analysis after its decoder stopped")
    );
    assert!(output.into_inner().is_empty());
}

#[test]
fn small_file_analysis_failure_preserves_scene_history_and_source_diagnostic() {
    for failed_frame in [0, 1] {
        let bytes = capture(Some(failed_frame));
        let failure = Recording::from_bytes(&bytes).err().unwrap();
        assert!(
            failure
                .downcast_ref::<pvd_core::analysis::DecodeError>()
                .is_some()
        );
        assert_eq!(failure.to_string(), "undefined memory type handle 13");
        let fixture = Fixture::new(&bytes);
        let progress = AtomicU64::new(0);
        let loaded = fixture.load(&progress).unwrap();
        assert!(loaded.bytes.is_none());
        assert!(loaded.recording.is_disk_backed());
        assert_eq!(loaded.source_path.as_deref(), Some(fixture.path.as_path()));
        assert_eq!(progress.load(Ordering::Relaxed), bytes.len() as u64);
        assert!(loaded.source_identity.as_ref().unwrap().matches(
            *blake3::hash(&bytes).as_bytes(),
            bytes.len() as u64,
            2
        ));
        assert_scene(&loaded.recording);
        assert!(loaded.recording.diagnostics.iter().any(|text| {
            text.starts_with(&format!(
                "Analysis decoding stopped at frame {failed_frame}"
            )) && text.contains("undefined memory type handle 13")
        }));
        assert_incomplete_analysis_is_not_exported(&loaded.recording);
        assert_eq!(std::fs::read(&fixture.path).unwrap(), bytes);
    }
}

#[test]
fn healthy_small_file_keeps_the_memory_path() {
    let bytes = capture(None);
    let fixture = Fixture::new(&bytes);
    let loaded = fixture.load(&AtomicU64::new(0)).unwrap();
    assert_eq!(
        loaded.bytes.as_deref().map(Vec::as_slice),
        Some(bytes.as_slice())
    );
    assert!(!loaded.recording.is_disk_backed());
    assert!(!fixture.history.directory.exists());
    assert_scene(&loaded.recording);
}

#[test]
fn fallback_does_not_hide_a_later_scene_error() {
    let mut bytes = capture(Some(0));
    bytes.extend(group(&[set(0xDEAD, 4, 5, 1, &11u32.to_le_bytes())]));
    let fixture = Fixture::new(&bytes);
    let error = fixture.load(&AtomicU64::new(0)).err().unwrap();
    assert!(
        error
            .text(Language::English)
            .contains("property update for missing instance 0xDEAD")
    );
}

#[test]
fn fallback_does_not_hide_incomplete_protocol_or_bypass_history_limits() {
    let mut bytes = capture(Some(0));
    bytes.push(1);
    let fixture = Fixture::new(&bytes);
    let error = fixture.load(&AtomicU64::new(0)).err().unwrap();
    assert!(error.text(Language::English).contains("truncated"));

    let mut fixture = Fixture::new(&capture(Some(0)));
    fixture.history.disk_budget_bytes = 0;
    let error = fixture.load(&AtomicU64::new(0)).err().unwrap();
    assert!(error.text(Language::English).contains("history budget"));
    assert!(
        read_recording(
            &fixture.path,
            &AtomicBool::new(true),
            &AtomicU64::new(0),
            fixture.history.clone()
        )
        .err()
        .unwrap()
        .text(Language::English)
        .contains("Cancelled")
    );
}

#[test]
fn scene_and_protocol_errors_do_not_select_the_analysis_fallback() {
    let mut bytes = capture(None);
    bytes.extend(group(&[set(0xDEAD, 4, 5, 1, &11u32.to_le_bytes())]));
    let failure = Recording::from_bytes(&bytes).err().unwrap();
    assert!(!requires_disk_history(&failure));
    let fixture = Fixture::new(&bytes);
    assert!(fixture.load(&AtomicU64::new(0)).is_err());
    assert!(!fixture.history.directory.exists());

    let mut bytes = capture(None);
    bytes.push(1);
    let failure = Recording::from_bytes(&bytes).err().unwrap();
    assert!(!requires_disk_history(&failure));
}

#[test]
fn renderer_errors_remain_fatal_on_both_file_paths() {
    for failed_frame in [None, Some(0)] {
        let mut bytes = capture(failed_frame);
        bytes.extend(group(&[
            string(11, "PvdUserRenderer"),
            class(11),
            property(11, 9, 10, 2),
            instance(11, 91),
            set(91, 9, 10, 1, &[255]),
        ]));
        if failed_frame.is_none() {
            let failure = Recording::from_bytes(&bytes).err().unwrap();
            assert!(!requires_disk_history(&failure));
        }
        let fixture = Fixture::new(&bytes);
        let error = fixture.load(&AtomicU64::new(0)).err().unwrap();
        assert!(
            error
                .text(Language::English)
                .contains("unknown PVD renderer event 255")
        );
    }
}

#[test]
fn complete_streaming_memory_export_remains_strict() {
    let mut csv = Vec::new();
    let error = pvd_core::analysis_export::export_analysis_csv_streaming(
        &mut Cursor::new(capture(Some(0))),
        &mut csv,
        pvd_core::analysis_export::AnalysisCsvKind::Memory,
        |_| true,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("undefined memory type handle 13")
    );
    assert!(csv.is_empty());
}

#[test]
#[ignore = "requires the APEX eALL capture with missing memory dictionary entries"]
fn apex_all_file_opens_scene_without_claiming_complete_memory() {
    let path =
        PathBuf::from(std::env::var_os("PVD_APEX_ALL_CAPTURE").expect("PVD_APEX_ALL_CAPTURE"));
    let bytes = std::fs::read(path).unwrap();
    assert!(bytes.len() as u64 <= IN_MEMORY_SDK_BYTES);
    let failure = Recording::from_bytes(&bytes).err().unwrap();
    assert!(
        failure
            .downcast_ref::<pvd_core::analysis::DecodeError>()
            .is_some()
    );
    assert_eq!(failure.to_string(), "undefined memory type handle 13");
    let fixture = Fixture::new(&bytes);
    let loaded = fixture.load(&AtomicU64::new(0)).unwrap();
    assert!(loaded.recording.is_disk_backed());
    assert_eq!(loaded.recording.frame_count(), 72);
    for frame in [0, 71, 1] {
        assert!(!loaded.recording.state_at(frame).unwrap().objects.is_empty());
    }
    assert!(loaded.recording.diagnostics.iter().any(|text| {
        text.starts_with("Analysis decoding stopped at frame")
            && text.contains("undefined memory type handle 13")
    }));
    assert_incomplete_analysis_is_not_exported(&loaded.recording);
}
