use super::*;
use crate::sessions::{Entry, FocusRequest, FrameFormat, publish_recording};
use pvd_core::{LiveRecording, Recording, live::HistoryConfig};
use std::path::PathBuf;

fn write_string(events: &mut Vec<u8>, handle: u32, value: &str) {
    events.push(1);
    events.extend((value.len() as u32 + 1).to_le_bytes());
    events.extend(value.as_bytes());
    events.push(0);
    events.extend(handle.to_le_bytes());
}

fn create(events: &mut Vec<u8>, id: u64) {
    events.push(6);
    events.extend(0u32.to_le_bytes());
    events.extend(2u32.to_le_bytes());
    events.extend(id.to_le_bytes());
}

fn capture(name: &str) -> (Vec<u8>, Vec<Vec<u8>>) {
    let groups = (0..22u64)
        .map(|stamp| {
            let mut events = Vec::new();
            let event_count = match stamp {
                0 => {
                    write_string(&mut events, 1, "frame");
                    write_string(&mut events, 2, name);
                    events.push(2);
                    events.extend(0u32.to_le_bytes());
                    events.extend(2u32.to_le_bytes());
                    create(&mut events, 8);
                    create(&mut events, 9);
                    5
                }
                1 => {
                    events.push(15);
                    events.extend(9u64.to_le_bytes());
                    1
                }
                2 => {
                    create(&mut events, 9);
                    1
                }
                _ => 0,
            };
            frame_group(stamp, events, event_count)
        })
        .collect();
    (stream_header(), groups)
}

fn install(sessions: &Sessions, id: u64, recording: Recording, directory: PathBuf) {
    sessions.catalog.lock().unwrap().entries.insert(
        id,
        Entry {
            leases: Default::default(),
            snapshot: Some(recording.into()),
            directory,
            bytes: 0,
        },
    );
}

fn view(sessions: &mut Sessions, id: u64, index: usize, format: FrameFormat) {
    let mut job = sessions.prepare_frame(id).unwrap();
    job.read(index as u64, 0, format).unwrap();
    assert!(sessions.finish_frame(&mut job, true));
}

fn check_scene(sessions: &Sessions, id: u64, index: usize) -> usize {
    let snapshot = sessions.snapshot(id).unwrap();
    let expected = snapshot.recording.state_at(index).unwrap();
    let workspace = sessions.frame_cache.workspace.lock().unwrap();
    assert_eq!(workspace.scene, expected);
    workspace.scene.objects[&8].type_name.as_ptr() as usize
}

#[test]
fn live_playback_reuses_the_scene_across_publications_and_history_banks() {
    let directory = std::env::temp_dir().join(format!(
        "pvd-unity-replay-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut live = LiveRecording::with_persistent_history(HistoryConfig {
        directory: directory.clone(),
        segment_frames: 1,
        min_free_disk_bytes: 0,
        ..Default::default()
    })
    .unwrap();
    let (header, groups) = capture("PersistentObject");
    live.feed(&header).unwrap();
    live.feed(&groups[0]).unwrap();
    let mut sessions = Sessions::default();
    let history = live.history_directory().unwrap().to_path_buf();
    install(&sessions, 1, live.snapshot().unwrap(), history);
    view(&mut sessions, 1, 0, FrameFormat::Resources(0, None));
    let initial = check_scene(&sessions, 1, 0);

    live.feed(&groups[1]).unwrap();
    publish_recording(&sessions.catalog, 1, live.snapshot().unwrap());
    let focus = FocusRequest {
        mode: 1,
        actor: 0,
        generation: 0,
        center: [1., 0., 0.],
        radius: 2.,
    };
    view(&mut sessions, 1, 0, FrameFormat::View(0, Some(focus)));
    assert_eq!(check_scene(&sessions, 1, 0), initial);
    view(&mut sessions, 1, 1, FrameFormat::View(0, Some(focus)));
    assert_eq!(check_scene(&sessions, 1, 1), initial);

    // An older cursor may still point into the hot copy when ingestion seals a bank.
    for group in &groups[2..] {
        live.feed(group).unwrap();
    }
    let final_recording = live.finish().unwrap();
    let disk = final_recording.disk_usage().unwrap();
    assert!(disk.disk_bytes > disk.cache_disk_bytes);
    publish_recording(&sessions.catalog, 1, final_recording);
    for index in [1, 0, 3, 2, 20, 21, 19, 21] {
        view(&mut sessions, 1, index, FrameFormat::Resources(0, None));
        check_scene(&sessions, 1, index);
    }
    assert!(sessions.frame(1, groups.len(), 0).is_err());
    view(&mut sessions, 1, 1, FrameFormat::Resources(0, None));
    check_scene(&sessions, 1, 1);
    sessions.release_view();
    assert!(sessions.frame(1, 0, 99).is_err());
    assert!(sessions.clear().unwrap() > 0);
    assert!(sessions.status()["sessions"].as_array().unwrap().is_empty());
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn memory_frames_reuse_the_exact_snapshot_and_replace_it_on_session_changes() {
    let recording = |name| {
        let (mut bytes, groups) = capture(name);
        bytes.extend_from_slice(&groups[0]);
        Recording::from_bytes(&bytes).unwrap()
    };
    let mut sessions = Sessions::default();
    install(&sessions, 1, recording("First"), PathBuf::new());
    install(&sessions, 2, recording("Second"), PathBuf::new());
    view(&mut sessions, 1, 0, FrameFormat::Inline);
    let initial = check_scene(&sessions, 1, 0);
    view(&mut sessions, 1, 0, FrameFormat::Resources(0, None));
    assert_eq!(check_scene(&sessions, 1, 0), initial);
    publish_recording(&sessions.catalog, 1, recording("Replacement"));
    view(&mut sessions, 1, 0, FrameFormat::Cached(0));
    check_scene(&sessions, 1, 0);
    view(&mut sessions, 2, 0, FrameFormat::Resources(0, None));
    check_scene(&sessions, 2, 0);
    view(&mut sessions, 1, 0, FrameFormat::Resources(0, None));
    check_scene(&sessions, 1, 0);
}
