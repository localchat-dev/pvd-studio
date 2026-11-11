use pvd_core::{
    LiveRecording, Recording, SceneState,
    capture::{self, CaptureRange, Format},
    frame_sources::FrameSourceKey,
    live::HistoryConfig,
};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[path = "../../../test-support/apex_clothing.rs"]
mod support;
use support::Fixture;

fn record_live(bytes: &[u8], cache: &Path, key: FrameSourceKey, fixture: &Fixture) -> Recording {
    let mut live = LiveRecording::with_history(HistoryConfig {
        directory: cache.to_owned(),
        disk_budget_bytes: 256 << 20,
        segment_target_bytes: 16 << 20,
        segment_frames: 12,
        min_free_disk_bytes: 0,
    })
    .unwrap();
    live.set_frame_source(Some(key));
    let mut offset = 0;
    let mut displayed = SceneState::default();
    let mut displayed_frame = None;
    for (source, frame, offset_end) in fixture.frames() {
        for chunk in bytes[offset..offset_end].chunks(997) {
            live.feed(chunk).unwrap();
        }
        offset = offset_end;
        assert_eq!(live.frame_count(), frame.index + 1);
        if source == key {
            let (index, update) = live
                .take_update()
                .expect("selected End publishes a complete frame");
            assert_eq!(index, frame.index);
            update.apply_owned(&mut displayed);
            fixture.check_state(frame.index, &displayed);
            displayed_frame = Some((key, frame));
        } else {
            assert!(
                live.take_update().is_none(),
                "another source must not replace the display"
            );
        }
        assert_eq!(live.display_frame_source(), displayed_frame);
    }
    for chunk in bytes[offset..].chunks(997) {
        live.feed(chunk).unwrap();
    }
    assert_eq!(live.frame_count(), 72);
    assert!(
        live.take_update().is_none(),
        "uncommitted tail cannot replace a completed display"
    );
    assert_eq!(live.display_frame_source(), displayed_frame);
    let snapshot = live.snapshot().unwrap();
    fixture.check_directory(&snapshot);
    let (_, last, _) = fixture
        .frames()
        .rev()
        .find(|(source, _, _)| *source == key)
        .unwrap();
    fixture.check_state(last.index, &snapshot.state_at(last.index).unwrap());
    drop(snapshot);
    let recording = live.finish().unwrap();
    assert!(recording.disk_usage().unwrap().segments > 1);
    fixture.check_directory(&recording);
    fixture.check_state(last.index, &recording.state_at(last.index).unwrap());
    assert_ne!(
        recording.state_at(71).unwrap().objects.len(),
        fixture.final_object_count()
    );
    recording
}

#[test]
#[ignore = "requires the official APEX CPU Clothing producer, getter truth and independent event probe"]
fn official_apex_clothing_matches_getters_and_source_windows() {
    let directory = std::env::var_os("PVD_APEX_CLOTHING_DIRECTORY")
        .map(PathBuf::from)
        .expect("PVD_APEX_CLOTHING_DIRECTORY must name one generated DEBUG capture directory");
    let bytes = fs::read(directory.join("capture.pvdstream")).unwrap();
    let fixture = Fixture::load(&directory, &bytes);
    let memory = Recording::from_bytes(&bytes).unwrap();
    fixture.check_recording(&memory);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let scratch = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../output")
        .join(format!("apex-clothing-core-{}-{nonce}", std::process::id()));
    fs::create_dir_all(&scratch).unwrap();
    let mut retained = None;
    for (index, key) in fixture.sources().into_iter().enumerate() {
        let cache = scratch.join(format!("history-{index}"));
        let recording = record_live(&bytes, &cache, key, &fixture);
        fixture.check_recording(&recording);
        if index == 0 {
            retained = Some(recording);
        } else {
            drop(recording);
            assert!(fs::read_dir(&cache).unwrap().next().is_none());
            fs::remove_dir(cache).unwrap();
        }
    }
    let recording = retained.unwrap();
    let archive = scratch.join("retained.pvdcapture");
    let report = capture::write_retained_new(
        &recording,
        &archive,
        CaptureRange { first: 0, last: 71 },
        &Default::default(),
        0,
        |_| true,
    )
    .unwrap();
    assert_eq!((report.format, report.frames), (Format::V3, 72));
    let reopened = capture::open_with_progress(&archive, &Default::default(), |_| true).unwrap();
    fixture.check_recording(&reopened);
    for (_, frame, _) in fixture.frames() {
        let index = frame.index;
        assert_eq!(
            reopened.state_at(index).unwrap(),
            recording.state_at(index).unwrap(),
            "saved frame {index}"
        );
        assert_eq!(
            reopened.state_at(index).unwrap(),
            memory.state_at(index).unwrap(),
            "file/live frame {index}"
        );
    }
    drop(reopened);
    drop(recording);
    fs::remove_file(archive).unwrap();
    let cache = scratch.join("history-0");
    assert!(fs::read_dir(&cache).unwrap().next().is_none());
    fs::remove_dir(cache).unwrap();
    fs::remove_dir(scratch).unwrap();
    println!(
        "APEX DEBUG async_fetch={}: 36 PhysX + 36 Apex endpoints; 58 SDK getter draws / 111476 triangles; source windows, local-space rendering, shared asset, actor lifetimes, forward/reverse/random replay and PVDCAP03 verified; raw blake3={}",
        fixture.async_fetch(),
        blake3::hash(&bytes)
    );
}

#[test]
#[ignore = "requires an official APEX 10-cycle capture with same-run getter truth and event probe"]
fn official_apex_clothing_retained_windows_match_getters_after_log_reclamation() {
    let directory = std::env::var_os("PVD_APEX_CLOTHING_DIRECTORY")
        .map(PathBuf::from)
        .expect("PVD_APEX_CLOTHING_DIRECTORY must name one generated 10-cycle DEBUG capture");
    let capture = directory.join("capture.pvdstream");
    assert!(fs::metadata(&capture).unwrap().len() < 96 << 20);
    let bytes = fs::read(capture).unwrap();
    let fixture = Fixture::load(&directory, &bytes);
    assert_eq!(fixture.frames().count(), 720);
    assert!(fixture.debug_payload_bytes() > 64 << 20);
    assert!(fixture.renderer_wire_bytes() < fixture.debug_payload_bytes());
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let scratch = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../output")
        .join(format!(
            "apex-retention-core-{}-{nonce}",
            std::process::id()
        ));
    fs::create_dir_all(&scratch).unwrap();
    for (index, key) in fixture.sources().into_iter().enumerate() {
        let cache = scratch.join(format!("history-{index}"));
        let mut live = LiveRecording::with_history(HistoryConfig {
            directory: cache.clone(),
            disk_budget_bytes: 4 << 20,
            segment_target_bytes: 1 << 20,
            segment_frames: 4,
            min_free_disk_bytes: 0,
        })
        .unwrap();
        live.set_frame_source(Some(key));
        let mut offset = 0;
        let mut displayed = SceneState::default();
        for (source, frame, offset_end) in fixture.frames() {
            for chunk in bytes[offset..offset_end].chunks(997) {
                live.feed(chunk).unwrap();
            }
            offset = offset_end;
            if source == key {
                let (actual, update) = live.take_update().unwrap();
                assert_eq!(actual, frame.index);
                update.apply_owned(&mut displayed);
                if frame.ordinal.is_multiple_of(12) || frame.ordinal == 359 {
                    fixture.check_state(frame.index, &displayed);
                }
            } else {
                assert!(live.take_update().is_none());
            }
        }
        for chunk in bytes[offset..].chunks(997) {
            live.feed(chunk).unwrap();
        }
        assert!(live.take_update().is_none());
        let recording = live.finish().unwrap();
        let stats = recording.disk_usage().unwrap();
        assert!(stats.warning.is_none(), "{:?}", stats.warning);
        assert!(stats.disk_bytes <= 4 << 20);
        fixture.check_retained_recording(&recording);
        drop(recording);
        assert!(fs::read_dir(&cache).unwrap().next().is_none());
        fs::remove_dir(cache).unwrap();
    }
    fs::remove_dir(scratch).unwrap();
}
