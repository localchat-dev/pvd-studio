use super::*;
use pvd_core::analysis::view::MemorySource;
use std::fs;

fn poll_until_idle(studio: &mut Studio, context: &egui::Context) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while studio.job.is_some()
        || studio.recording_export.is_some()
        || studio.clip_persistence.busy()
    {
        assert!(Instant::now() < deadline, "clip operation timed out");
        studio.poll(context);
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn open_on_disk(studio: &mut Studio, path: &Path, context: &egui::Context) {
    let loaded = read_recording_on_disk(
        path,
        File::open(path).unwrap(),
        &AtomicBool::new(false),
        &AtomicU64::new(0),
        pvd_core::live::HistoryConfig {
            segment_frames: 15,
            ..Default::default()
        },
    )
    .unwrap();
    let (sender, receiver) = mpsc::channel();
    sender.send(Ok(loaded)).unwrap();
    studio.job = Some(LoadJob {
        result: Mutex::new(receiver),
        cancelled: Arc::new(AtomicBool::new(false)),
        progress: Arc::new(AtomicU64::new(0)),
        label: path.display().to_string(),
        started: Instant::now(),
        live: false,
        final_snapshot: true,
    });
    studio.poll(context);
    assert!(studio.bytes.is_none());
    assert!(studio.can_trim_capture());
}

#[test]
#[ignore = "Requires official PhysX 3.4.2 rich capture"]
fn disk_clip_workflow_persists_ranges_exports_and_rejects_changed_source() {
    let fixture = std::env::var_os("PVD_SDK34_RICH_CAPTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../captures/physx-3.4-rich.pvdstream")
        });
    let bytes = fs::read(fixture).unwrap();
    let expected = Recording::from_bytes(&bytes).unwrap();
    assert_eq!(expected.frame_count(), 120);
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../output/clip-export-tests")
        .join(format!("disk-workflow-{}", std::process::id()));
    fs::create_dir_all(directory.parent().unwrap()).unwrap();
    fs::create_dir(&directory).unwrap();
    let source = directory.join("完整录制.pvdstream");
    let destination = directory.join("片段.pvdstream");
    fs::write(&source, &bytes).unwrap();
    let context = egui::Context::default();
    let mut studio = Studio::default();
    open_on_disk(&mut studio, &source, &context);
    assert!(studio.clip_persistence.binding.is_some());
    assert!(studio.clip_set.add(40, 79));
    studio.clip_set.clips[0].name = "中间运动".into();
    studio
        .clip_persistence
        .save(&studio.clip_set, context.clone(), &studio.background_jobs);
    poll_until_idle(&mut studio, &context);
    assert!(!studio.clip_persistence.dirty(&studio.clip_set));
    let definitions = studio.clip_set.clone();
    studio.close();
    open_on_disk(&mut studio, &source, &context);
    assert_eq!(studio.clip_set, definitions);
    studio.trim_first = 40;
    studio.trim_last = 79;
    studio.export_path = destination.display().to_string();
    studio.trim_capture(context.clone());
    assert!(studio.recording_export.is_some());
    assert!(!studio.can_trim_capture());
    poll_until_idle(&mut studio, &context);
    let actual = Recording::from_path(&destination).unwrap();
    assert_eq!(actual.frame_count(), 40);
    actual.verify_replay().unwrap();
    for frame in (0..40).rev() {
        assert_eq!(
            actual.state_at(frame).unwrap(),
            expected.state_at(frame + 40).unwrap()
        );
        assert_eq!(
            actual.frames[frame].timestamp,
            expected.frames[frame + 40].timestamp
        );
    }
    let rejected = directory.join("changed.pvdstream");
    let mut changed = bytes.clone();
    changed[12] ^= 1;
    fs::write(&source, &changed).unwrap();
    studio.export_path = rejected.display().to_string();
    studio.trim_capture(context.clone());
    poll_until_idle(&mut studio, &context);
    assert!(!rejected.exists());
    assert!(studio.notices.iter().any(|notice| {
        notice
            .text(Language::English)
            .contains("changed since it was opened")
    }));
    fs::write(&source, &bytes).unwrap();
    studio.close();
    assert!(studio.recording_export.is_none());
    assert!(studio.recording.is_none());
    studio.poll(&context);
    assert!(studio.recording.is_none());
    assert!(!rejected.exists());
    for entry in fs::read_dir(&directory).unwrap() {
        let path = entry.unwrap().path();
        assert!(
            !path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".pvdstudio-output")
        );
        fs::remove_file(path).unwrap();
    }
    fs::remove_dir(directory).unwrap();
}

#[test]
#[ignore = "Requires official PhysX 3.4.2 eALL capture"]
fn disk_analysis_exports_cover_all_recorded_spans_and_memory_samples() {
    use pvd_core::analysis_export::AnalysisCsvKind;
    let path = std::env::var_os("PVD_CLIP_ANALYSIS_CAPTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../captures/physx-3.4-all.pvdstream")
        });
    let expected = Recording::from_path(&path).unwrap();
    assert!(expected.analysis.profile_spans().next().is_some());
    assert!(expected.analysis.memory_snapshot().sample_count() > 0);
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../output/clip-export-tests")
        .join(format!("analysis-workflow-{}", std::process::id()));
    fs::create_dir_all(directory.parent().unwrap()).unwrap();
    fs::create_dir(&directory).unwrap();
    let context = egui::Context::default();
    let mut studio = Studio::default();
    open_on_disk(&mut studio, &path, &context);
    for (kind, rows, name) in [
        (
            AnalysisCsvKind::Profile,
            expected.analysis.profile_spans().len(),
            "profile.csv",
        ),
        (
            AnalysisCsvKind::Memory,
            expected.analysis.memory_snapshot().sample_count(),
            "memory.csv",
        ),
    ] {
        let destination = directory.join(name);
        studio.analysis_export_path = destination.display().to_string();
        studio.export_analysis(kind, context.clone());
        assert!(studio.recording_export.is_some());
        poll_until_idle(&mut studio, &context);
        let csv = fs::read_to_string(&destination).unwrap();
        assert_eq!(csv.lines().count(), rows + 1);
        fs::remove_file(destination).unwrap();
    }
    studio.close();
    assert!(!studio.can_export_recording());
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
    fs::remove_dir(directory).unwrap();
}

#[test]
#[ignore = "Requires paired native rich capture and reference CLI CSV files"]
fn native_analysis_exports_match_reference_cli_bytes_through_desktop_jobs() {
    use pvd_core::analysis_export::AnalysisCsvKind;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source = root.join("captures/native/rich-paired-native.pxd2");
    let reference = root.join("captures/native/reference");
    let directory = root
        .join("output/clip-export-tests")
        .join(format!("native-analysis-workflow-{}", std::process::id()));
    fs::create_dir_all(directory.parent().unwrap()).unwrap();
    fs::create_dir(&directory).unwrap();
    let context = egui::Context::default();
    let mut studio = Studio::default();
    studio.open(source, context.clone());
    poll_until_idle(&mut studio, &context);
    assert_eq!(
        studio.recording.as_ref().unwrap().source,
        pvd_core::RecordingSource::Pvd3Database
    );
    assert!(studio.can_export_analysis());
    assert!(!studio.can_trim_capture());
    for (kind, name) in [
        (AnalysisCsvKind::Profile, "profile"),
        (AnalysisCsvKind::Memory, "memory"),
    ] {
        let destination = directory.join(format!("{name}.csv"));
        studio.analysis_export_path = destination.display().to_string();
        studio.export_analysis(kind, context.clone());
        assert!(studio.recording_export.is_some());
        poll_until_idle(&mut studio, &context);
        assert_eq!(
            fs::read(&destination).unwrap(),
            fs::read(reference.join(format!("native-{name}.csv"))).unwrap()
        );
        fs::remove_file(destination).unwrap();
    }
    studio.close();
    assert!(!studio.can_export_analysis());
    fs::remove_dir(directory).unwrap();
}
