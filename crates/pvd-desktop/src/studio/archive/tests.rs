use super::*;
#[path = "../../../../pvd-core/tests/support/pvd_archive.rs"]
mod fixture;

fn poll(studio: &mut Studio, context: &egui::Context) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while studio.job.is_some()
        || studio.seeking.is_some()
        || studio.recording_export.is_some()
        || studio.save_job.is_some()
    {
        studio.poll(context);
        assert!(Instant::now() < deadline, "{:?}", studio.notices);
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn close(studio: &mut Studio, context: &egui::Context) {
    studio.close();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !studio.poll_shutdown() {
        studio.poll(context);
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn pvd_archive_load_binds_inner_bytes_and_rejects_corruption_and_cancellation() {
    let scratch = fixture::Scratch::new("desktop-load");
    for (name, inner, source) in [
        (
            "sdk.pvd",
            fixture::sdk(),
            pvd_core::RecordingSource::SdkStream,
        ),
        (
            "native.bin",
            fixture::native(),
            pvd_core::RecordingSource::Pvd3Database,
        ),
    ] {
        let path = scratch.write(name, &fixture::stored("nested/recording.pxd2", &inner));
        let loaded = read_recording(
            &path,
            &AtomicBool::new(false),
            &AtomicU64::new(0),
            scratch.history(),
        )
        .unwrap();
        assert_eq!(loaded.recording.source, source);
        assert!(loaded.recording.is_disk_backed());
        assert_eq!(loaded.bytes.as_ref().unwrap().as_slice(), inner);
        assert!(loaded.source_path.is_none());
        assert!(loaded.clips.is_none());
        assert!(loaded.source_identity.unwrap().matches(
            *blake3::hash(&inner).as_bytes(),
            inner.len() as u64,
            3
        ));
        for index in [0, 1, 2, 1, 0] {
            assert_eq!(
                loaded.recording.state_at(index).unwrap().objects[&1].properties["Value"],
                pvd_core::Value::U64([11, 22, 33][index])
            );
        }
        drop(loaded.recording);
        assert!(
            read_recording(
                &path,
                &AtomicBool::new(true),
                &AtomicU64::new(0),
                scratch.history()
            )
            .is_err()
        );
    }
    let mut corrupt = fixture::stored("recording.pxd2", &fixture::sdk());
    corrupt[30 + "recording.pxd2".len() + 20] ^= 1;
    let path = scratch.write("corrupt.pvd", &corrupt);
    assert!(
        read_recording(
            &path,
            &AtomicBool::new(false),
            &AtomicU64::new(0),
            scratch.history()
        )
        .is_err()
    );
    assert!(
        std::fs::read_dir(scratch.path("history"))
            .unwrap()
            .next()
            .is_none()
    );
    assert!(!scratch.path("nested").exists());
}

#[test]
fn pvd_archive_sdk_desktop_exports_and_profile_queries_use_memory_source() {
    let scratch = fixture::Scratch::new("desktop-sdk");
    let inner = fixture::sdk();
    let path = scratch.write("source.pvd", &fixture::stored("recording.pxd2", &inner));
    let context = egui::Context::default();
    let mut studio = Studio::default();
    studio.persist_layout = false;
    studio.prefs.history_directory = scratch.path("history").display().to_string();
    studio.prefs.min_free_disk_mib = 0;
    studio.open(path.clone(), context.clone());
    poll(&mut studio, &context);
    assert!(studio.capture_path.is_none());
    assert_eq!(studio.bytes.as_ref().unwrap().as_slice(), inner);
    assert!(studio.can_save_capture());
    assert!(studio.can_trim_capture());
    assert!(studio.can_export_analysis());
    assert!(studio.complete_timeline_source().is_some());
    assert!(studio.clip_persistence.binding.is_none());
    assert!(studio.clip_persistence.issue.is_some());
    let profile = studio.complete_profile_source().unwrap();
    assert!(!profile.can_rebuild_cpu_index());
    let (catalog, _) = profile.catalog(&AtomicBool::new(false), |_| true).unwrap();
    assert!(catalog.is_empty());
    match studio.export_source() {
        crate::recording_export::Source::Memory(bytes) => assert_eq!(bytes.as_slice(), inner),
        _ => panic!("archive export must hold its inner bytes"),
    }
    std::fs::write(&path, b"changed outside the loaded recording").unwrap();
    let saved = scratch.path("saved.pvdstream");
    studio.export_path = saved.display().to_string();
    studio.save_capture(context.clone());
    poll(&mut studio, &context);
    assert_eq!(std::fs::read(saved).unwrap(), inner);
    for (kind, file) in [
        (
            pvd_core::analysis_export::AnalysisCsvKind::Profile,
            "profile.csv",
        ),
        (
            pvd_core::analysis_export::AnalysisCsvKind::Memory,
            "memory.csv",
        ),
    ] {
        let path = scratch.path(file);
        studio.analysis_export_path = path.display().to_string();
        studio.export_analysis(kind, context.clone());
        assert!(studio.recording_export.is_some());
        poll(&mut studio, &context);
        assert!(path.is_file(), "{:?}", studio.notices);
    }
    let clip = scratch.path("clip.pvdstream");
    studio.export_path = clip.display().to_string();
    studio.trim_first = 1;
    studio.trim_last = 2;
    studio.trim_capture(context.clone());
    poll(&mut studio, &context);
    let result = Recording::from_path(clip).unwrap();
    assert_eq!(result.frame_count(), 2);
    assert_eq!(
        result.state_at(0).unwrap().objects[&1].properties["Value"],
        pvd_core::Value::U64(22)
    );
    drop(result);
    drop(profile);
    close(&mut studio, &context);
}

#[test]
fn pvd_archive_native_desktop_exports_use_inner_database_after_outer_path_changes() {
    let scratch = fixture::Scratch::new("desktop-native");
    let inner = fixture::native();
    let path = scratch.write("source.pvd", &fixture::stored("recording.pxd2", &inner));
    let context = egui::Context::default();
    let mut studio = Studio::default();
    studio.persist_layout = false;
    studio.prefs.history_directory = scratch.path("history").display().to_string();
    studio.prefs.min_free_disk_mib = 0;
    studio.open(path.clone(), context.clone());
    poll(&mut studio, &context);
    assert!(studio.is_native_recording());
    assert!(!studio.can_save_capture());
    assert!(!studio.can_trim_capture());
    assert!(studio.can_export_analysis());
    assert!(studio.complete_profile_source().is_none());
    assert!(studio.complete_timeline_source().is_some());
    assert!(studio.capture_path.is_none());
    std::fs::write(path, b"changed outside the loaded recording").unwrap();
    for (file, selection) in [
        ("whole.pxd2", pvd_core::native::export::Selection::Whole),
        (
            "clip.pxd2",
            pvd_core::native::export::Selection::Frames { first: 1, last: 2 },
        ),
    ] {
        let destination = scratch.path(file);
        studio.recording_export = Some(
            crate::recording_export::Job::start(
                studio.export_source(),
                destination.clone(),
                crate::recording_export::Kind::Native(selection),
                context.clone(),
                &studio.export_workers,
            )
            .unwrap(),
        );
        poll(&mut studio, &context);
        assert!(destination.is_file(), "{:?}", studio.notices);
        if matches!(selection, pvd_core::native::export::Selection::Whole) {
            assert_eq!(std::fs::read(destination).unwrap(), inner);
        } else {
            let recording = Recording::from_path(destination).unwrap();
            assert_eq!(recording.frame_count(), 2);
            assert_eq!(
                recording.state_at(0).unwrap().objects[&1].properties["Value"],
                pvd_core::Value::U64(22)
            );
        }
    }
    for (kind, file) in [
        (
            pvd_core::analysis_export::AnalysisCsvKind::Profile,
            "profile.csv",
        ),
        (
            pvd_core::analysis_export::AnalysisCsvKind::Memory,
            "memory.csv",
        ),
    ] {
        let path = scratch.path(file);
        studio.analysis_export_path = path.display().to_string();
        studio.export_analysis(kind, context.clone());
        poll(&mut studio, &context);
        assert!(path.is_file(), "{:?}", studio.notices);
    }
    close(&mut studio, &context);
}
