use super::*;
use bevy::asset::uuid::Uuid;
use std::{
    fs,
    io::{SeekFrom, Write},
};

fn fingerprint(path: &Path) -> (blake3::Hash, u64) {
    let mut file = File::open(path).unwrap();
    let mut hash = blake3::Hasher::new();
    let mut buffer = [0; 64 * 1024];
    let mut bytes = 0;
    loop {
        let count = file.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
        bytes += count as u64;
    }
    assert_eq!(file.metadata().unwrap().len(), bytes);
    (hash.finalize(), bytes)
}

fn sample(path: &Path, offset: u64) -> [u8; 64] {
    let mut file = File::open(path).unwrap();
    file.seek(SeekFrom::Start(offset)).unwrap();
    let mut bytes = [0; 64];
    file.read_exact(&mut bytes).unwrap();
    bytes
}

fn write_byte(path: &Path, offset: u64, value: u8) {
    let mut file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(offset)).unwrap();
    file.write_all(&[value]).unwrap();
    file.sync_all().unwrap();
}

fn poll_until_idle(studio: &mut Studio, context: &egui::Context) {
    let deadline = Instant::now() + Duration::from_secs(180);
    while studio.job.is_some()
        || studio.seeking.is_some()
        || studio.recording_export.is_some()
        || studio.clip_persistence.busy()
    {
        studio.poll(context);
        assert!(Instant::now() < deadline, "{:?}", studio.notices);
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn owned_sessions(history: &Path) -> usize {
    let sessions: Vec<_> = fs::read_dir(history).unwrap().map(Result::unwrap).collect();
    assert!(!sessions.is_empty());
    for session in &sessions {
        assert!(session.file_type().unwrap().is_dir());
        assert!(session.path().join(".pvd-studio-owner").is_file());
    }
    sessions.len()
}

fn close_and_wait_for_history(studio: &mut Studio, context: &egui::Context, history: &Path) {
    studio.close();
    assert!(studio.recording.is_none());
    assert!(studio.bytes.is_none());
    assert!(studio.source_identity.is_none());
    assert!(studio.capture_path.is_none());
    assert!(!studio.can_export_native(Selection::Whole));
    let deadline = Instant::now() + Duration::from_secs(30);
    while fs::read_dir(history).unwrap().next().is_some() {
        studio.poll(context);
        assert!(
            Instant::now() < deadline,
            "history owner did not release its files"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn assert_no_temporary_output(directory: &Path) {
    assert!(fs::read_dir(directory).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".pvdstudio-output-")
    }));
}

#[test]
#[ignore = "requires PVD_NATIVE_LARGE_FILE: baseline PVD3 container padded to 513 MiB + 19 bytes; this is a constructed size fixture"]
fn padded_native_file_above_512_mib_keeps_identity_and_whole_save_workflow() {
    let output_root =
        fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../output")).unwrap();
    let source = fs::canonicalize(
        std::env::var_os("PVD_NATIVE_LARGE_FILE").expect("PVD_NATIVE_LARGE_FILE is required"),
    )
    .unwrap();
    assert!(source.starts_with(&output_root));
    let expected = fingerprint(&source);
    assert_eq!(expected.1, (513 << 20) + 19);
    assert!(expected.1 > MAX_CAPTURE_BYTES);
    let boundary_offsets = [0, MAX_CAPTURE_BYTES - 32, expected.1 - 64];
    let boundary_bytes = boundary_offsets.map(|offset| sample(&source, offset));
    assert_eq!(
        &boundary_bytes[0][..4],
        &pvd_core::native::MAGIC.to_le_bytes()
    );

    let parent = output_root.join("native-large-desktop-tests");
    fs::create_dir_all(&parent).unwrap();
    let directory = parent.join(Uuid::new_v4().to_string());
    fs::create_dir(&directory).unwrap();
    let history = directory.join("history");
    fs::create_dir(&history).unwrap();
    let saved = directory.join("saved-whole.pxd2");
    let rejected = directory.join("changed-source.pxd2");
    let clip = directory.join("unavailable-clip.pxd2");
    let mut studio = Studio::default();
    studio.prefs.history_directory = history.to_string_lossy().into_owned();
    let context = egui::Context::default();

    studio.open(source.clone(), context.clone());
    assert!(studio.job.is_some());
    poll_until_idle(&mut studio, &context);
    assert!(studio.is_native_recording(), "{:?}", studio.notices);
    assert!(studio.bytes.is_none());
    assert_eq!(studio.capture_path.as_deref(), Some(source.as_path()));
    let frames = studio.count();
    assert!(frames > 0);
    assert_eq!(studio.first_frame(), 0);
    assert_eq!(
        studio.source_identity.as_ref().unwrap().fingerprint(),
        Some((expected.0, expected.1, frames))
    );
    let recording = studio.recording.as_ref().unwrap();
    assert!(recording.is_disk_backed());
    let usage = recording.disk_usage().unwrap();
    assert_eq!(usage.first_frame, 0);
    assert_eq!(usage.retained_frames, frames);
    assert!(usage.warning.is_none());
    let first_sessions = owned_sessions(&history);

    let selection = Selection::Frames { first: 0, last: 0 };
    assert!(studio.has_native_export_source(Selection::Whole));
    assert!(studio.can_export_native(Selection::Whole));
    assert!(!studio.has_native_export_source(selection));
    assert!(!studio.can_export_native(selection));
    studio.native_export_path = clip.to_string_lossy().into_owned();
    studio.export_native(selection, context.clone());
    assert!(studio.recording_export.is_none());
    assert!(!clip.exists());

    studio.native_export_path = saved.to_string_lossy().into_owned();
    studio.export_native(Selection::Whole, context.clone());
    assert!(studio.recording_export.is_some());
    assert!(!studio.can_export_native(Selection::Whole));
    poll_until_idle(&mut studio, &context);
    assert!(saved.is_file(), "{:?}", studio.notices);
    assert_eq!(fingerprint(&saved), expected);
    for (offset, bytes) in boundary_offsets.into_iter().zip(boundary_bytes) {
        assert_eq!(sample(&saved, offset), bytes);
    }
    assert!(studio.notices.iter().any(|notice| {
        notice
            .text(Language::English)
            .starts_with("Saved PVD3 recording:")
    }));
    assert_no_temporary_output(&directory);
    close_and_wait_for_history(&mut studio, &context, &history);

    studio.open(saved.clone(), context.clone());
    poll_until_idle(&mut studio, &context);
    assert!(studio.is_native_recording(), "{:?}", studio.notices);
    assert_eq!(studio.capture_path.as_deref(), Some(saved.as_path()));
    assert_eq!(
        studio.source_identity.as_ref().unwrap().fingerprint(),
        Some((expected.0, expected.1, frames))
    );
    let second_sessions = owned_sessions(&history);
    let changed_offset = MAX_CAPTURE_BYTES + 1;
    assert_eq!(sample(&saved, changed_offset)[0], 0);
    write_byte(&saved, changed_offset, 1);
    assert_eq!(fs::metadata(&saved).unwrap().len(), expected.1);
    studio.notices.clear();
    studio.native_export_path = rejected.to_string_lossy().into_owned();
    studio.export_native(Selection::Whole, context.clone());
    assert!(studio.recording_export.is_some());
    poll_until_idle(&mut studio, &context);
    assert!(!rejected.exists());
    assert!(
        studio.notices.iter().any(|notice| {
            notice
                .text(Language::English)
                .contains("changed since it was opened")
        }),
        "{:?}",
        studio.notices
    );
    assert_no_temporary_output(&directory);
    close_and_wait_for_history(&mut studio, &context, &history);
    write_byte(&saved, changed_offset, 0);
    assert_eq!(fingerprint(&saved), expected);
    assert_eq!(fingerprint(&source), expected);

    let deadline = Instant::now() + Duration::from_secs(30);
    while !studio.poll_shutdown() {
        assert!(Instant::now() < deadline, "desktop workers did not stop");
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(studio);
    assert!(fs::read_dir(&history).unwrap().next().is_none());
    fs::remove_dir(&history).unwrap();
    fs::remove_file(&saved).unwrap();
    assert!(fs::read_dir(&directory).unwrap().next().is_none());
    let report = serde_json::json!({
        "schema": 1,
        "input_kind": "PVD3 baseline content with constructed trailing padding; not a real large recording",
        "source": source,
        "source_bytes": expected.1,
        "source_blake3": expected.0.to_hex().to_string(),
        "frame_count": frames,
        "first_frame": 0,
        "last_frame": frames - 1,
        "boundary_offsets": boundary_offsets,
        "mutation_offset": changed_offset,
        "history_sessions_while_open": [first_sessions, second_sessions],
        "history_sessions_after_close": [0, 0],
        "source_identity_complete": true,
        "whole_save_available": true,
        "clip_save_available": false,
        "background_whole_save_digest_equal": true,
        "changed_source_rejected_before_publication": true,
        "temporary_outputs_remaining": 0,
        "saved_copy_removed": true,
        "scope": "Desktop load/export workers and the production button predicates; no native window or GUI automation"
    });
    fs::write(
        directory.join("result.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    println!(
        "large native desktop result: {}",
        directory.join("result.json").display()
    );
}
