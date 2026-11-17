use super::*;
use crate::network::{LiveEvent, LiveSnapshot, NetworkOptions, NetworkSession};
use pvd_core::{
    LiveRecording,
    live::{HistoryConfig, SceneUpdate},
};
use pvd_protocol::{Endian, Event, EventGroup, Header};
use std::{
    io::Write,
    net::TcpStream,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn recording(count: usize) -> Recording {
    let mut builder = pvd_core::RecordingBuilder::new(Header {
        version: 1,
        timestamp_numerator: 1,
        timestamp_denominator: 1,
        flags: 1,
        endian: Endian::Little,
    });
    let mut events = vec![Event::StringHandle {
        handle: 1,
        value: "frame".into(),
    }];
    events.extend((0..count).map(|frame| Event::EndSection {
        id: 1,
        name: 1,
        timestamp: frame as u64,
    }));
    builder
        .push(EventGroup {
            stream_id: 1,
            timestamp: 0,
            offset: 0,
            events,
        })
        .unwrap();
    builder.finish().unwrap()
}

fn snapshot(session: u64, count: usize) -> LiveSnapshot {
    let recording = recording(count);
    LiveSnapshot {
        session,
        frame: count - 1,
        update: SceneUpdate::full(&recording.state_at(count - 1).unwrap()),
        recording,
        source_frame: None,
        display_updated: true,
    }
}

fn stream(count: usize) -> Vec<u8> {
    let mut bytes = pvd_protocol::STREAM_MAGIC.to_le_bytes().to_vec();
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(1_000_000u64.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    let mut events = vec![1u8];
    events.extend(6u32.to_le_bytes());
    events.extend(b"frame\0");
    events.extend(1u32.to_le_bytes());
    for frame in 0..count as u64 {
        events.push(19);
        events.extend(1u64.to_le_bytes());
        events.extend(1u32.to_le_bytes());
        events.extend((frame * 16_667).to_le_bytes());
    }
    bytes.extend((events.len() as u32).to_le_bytes());
    bytes.extend((count as u32 + 1).to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    bytes.extend(events);
    bytes
}

fn directory(label: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../output/temporary-recordings-validation")
        .join(format!(
            "pvd-temporary-recording-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
}

#[test]
fn closed_live_capture_waits_for_completion_and_retains_final_frames() {
    let context = egui::Context::default();
    let (server, events) = NetworkSession::test_session(snapshot(1, 2));
    let mut studio = Studio::default();
    studio.server = Some(server);
    studio.connect_live_session(1);
    studio.poll_network(&context);
    studio.close();
    assert!(studio.close_after_capture);
    assert!(studio.server.as_ref().unwrap().is_stopping());
    assert_eq!(studio.count(), 2);
    events
        .send(LiveEvent::Complete {
            session: 1,
            received_bytes: 50,
            result: Box::new(Ok(recording(5))),
            capture: None,
        })
        .unwrap();
    events.send(LiveEvent::Stopped).unwrap();
    studio.poll_network(&context);
    assert!(studio.server.is_none());
    assert!(studio.recording.is_none());
    assert_eq!(studio.count(), 0);
    let entry = studio.temporary_recordings.entries().next().unwrap();
    assert_eq!(entry.status, Status::Closed);
    assert_eq!(entry.frame_count(), 5);
    let id = entry.id;
    studio.select_temporary_recording(id);
    assert_eq!(studio.count(), 5);
    assert_eq!(studio.frame, 0);
    studio.close();
    assert!(studio.can_delete_temporary_recording(id));
}

#[test]
fn finished_recording_survives_reconnect_and_viewing_it_keeps_live_updates_separate() {
    let context = egui::Context::default();
    let (server, events) = NetworkSession::test_session(snapshot(1, 3));
    let mut studio = Studio::default();
    studio.server = Some(server);
    studio.connect_live_session(1);
    studio.poll_network(&context);
    events
        .send(LiveEvent::Complete {
            session: 1,
            received_bytes: 50,
            result: Box::new(Ok(recording(3))),
            capture: None,
        })
        .unwrap();
    studio.poll_network(&context);
    let id = studio.temporary_recordings.entries().next().unwrap().id;
    studio.connect_live_session(2);
    studio.accept_live_snapshot(snapshot(2, 8));
    studio.select_temporary_recording(id);
    assert_eq!(studio.frame, 0);
    assert_eq!(studio.count(), 3);
    studio.accept_live_snapshot(snapshot(2, 12));
    assert_eq!(studio.frame, 0);
    assert_eq!(studio.count(), 3);
    assert_eq!(studio.live_recording.as_ref().unwrap().frame_count(), 12);
    assert!(studio.live_connected);
    assert!(!studio.can_delete_temporary_recording(id));
    studio.close();
    assert!(studio.live_connected);
    assert_eq!(studio.count(), 12);
    assert_eq!(studio.frame, 11);
    assert!(!studio.server.as_ref().unwrap().is_stopping());
}

#[test]
fn connecting_while_viewing_a_temporary_recording_does_not_replace_display() {
    let mut studio = Studio::default();
    let id = studio
        .temporary_recordings
        .insert(1, Arc::new(recording(4)), None, Status::Completed);
    studio.select_temporary_recording(id);
    studio.connect_live_session(1);
    studio.accept_live_snapshot(snapshot(1, 9));
    assert_eq!(studio.temporary_recordings.selected, Some(id));
    assert_eq!(studio.count(), 4);
    assert_eq!(studio.frame, 0);
    studio.return_to_live();
    assert_eq!(studio.count(), 9);
    assert_eq!(studio.frame, 8);
}

#[test]
fn empty_closed_connection_does_not_create_a_temporary_recording() {
    let context = egui::Context::default();
    let (server, events) = NetworkSession::test_session(snapshot(1, 1));
    server.take_snapshot();
    let mut studio = Studio::default();
    studio.server = Some(server);
    studio.connect_live_session(1);
    studio.close();
    events.send(LiveEvent::Empty { session: 1 }).unwrap();
    events.send(LiveEvent::Stopped).unwrap();
    studio.poll_network(&context);
    assert!(studio.temporary_recordings.entries().next().is_none());
    assert!(studio.server.is_none());
    assert!(studio.live_session.is_none());
}

#[test]
fn cleanup_skips_viewing_and_reader_leases_and_removes_oldest_unused_entry() {
    let mut temporary = TemporaryRecordings::default();
    let reader = Arc::new(recording(2));
    let first = temporary.insert(1, reader.clone(), None, Status::Completed);
    let unused = temporary.insert(2, Arc::new(recording(4)), None, Status::Completed);
    let selected = temporary.insert(3, Arc::new(recording(3)), None, Status::Completed);
    temporary.selected = Some(selected);
    assert_eq!(temporary.reclaim(1, u64::MAX), 1);
    assert!(temporary.get(unused).is_none());
    assert!(temporary.get(first).is_some());
    assert!(!temporary.remove(selected));
    drop(reader);
    assert_eq!(temporary.reclaim(1, u64::MAX), 1);
    assert!(temporary.get(first).is_none());
    assert!(temporary.get(selected).is_some());
}

#[test]
fn disk_cleanup_drops_only_owned_history_and_keeps_saved_recording() {
    let directory = directory("saved");
    std::fs::create_dir_all(&directory).unwrap();
    let mut live = LiveRecording::with_persistent_history(HistoryConfig {
        directory: directory.join("history"),
        min_free_disk_bytes: 0,
        disk_budget_bytes: 4 << 20,
        ..Default::default()
    })
    .unwrap();
    live.feed(&stream(10)).unwrap();
    let recording = Arc::new(live.finish().unwrap());
    let newest_empty = Arc::new(super::tests::recording(0));
    let saved = directory.join("saved.pvdcapture");
    pvd_core::capture::write_retained_new(
        &recording,
        &saved,
        pvd_core::capture::CaptureRange { first: 0, last: 9 },
        &Default::default(),
        0,
        |_| true,
    )
    .unwrap();
    let mut temporary = TemporaryRecordings::default();
    temporary.insert(1, recording, Some(saved.clone()), Status::Completed);
    assert!(temporary.entries().next().unwrap().disk_bytes() > 0);
    assert_eq!(
        temporary.reclaim(16, 0),
        0,
        "newest recording must survive even over budget"
    );
    temporary.insert(2, newest_empty, None, Status::Completed);
    assert_eq!(temporary.reclaim(16, 0), 1);
    assert!(
        std::fs::read_dir(directory.join("history"))
            .unwrap()
            .next()
            .is_none(),
        "last recording lease must release the owned history directory"
    );
    assert!(saved.exists());
    let reopened = Recording::from_path(&saved).unwrap();
    assert_eq!(reopened.first_frame(), 0);
    assert_eq!(reopened.frame_count(), 10);
    reopened.verify_replay().unwrap();
    drop(reopened);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn real_tcp_close_finishes_accepted_frames_and_preserves_replay() {
    let directory = directory("tcp");
    let context = egui::Context::default();
    let mut studio = Studio::default();
    let server = NetworkSession::start_tracked(
        pvd_transport::ListenConfig {
            address: "127.0.0.1:0".parse().unwrap(),
            ..Default::default()
        },
        NetworkOptions {
            history: HistoryConfig {
                directory: directory.clone(),
                min_free_disk_bytes: 0,
                ..Default::default()
            },
            ..Default::default()
        },
        context.clone(),
        &studio.background_jobs,
    )
    .unwrap();
    let mut client = TcpStream::connect(server.address()).unwrap();
    studio.server = Some(server);
    client.write_all(&stream(100)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while studio.count() < 99 {
        studio.poll_network(&context);
        assert!(
            Instant::now() < deadline,
            "TCP frames were not published ({}): {:?}; {:?}",
            studio.count(),
            studio.receive_status,
            studio.notices,
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    studio.close();
    while studio.server.is_some() {
        studio.poll_network(&context);
        assert!(Instant::now() < deadline, "closed capture did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!studio.live_connected);
    let entry = studio.temporary_recordings.entries().next().unwrap();
    assert_eq!(entry.status, Status::Closed);
    assert_eq!(entry.frame_count(), 100);
    assert_eq!(entry.recording.first_frame(), 0);
    entry.recording.state_at(0).unwrap();
    entry.recording.state_at(99).unwrap();
    drop(client);
    drop(studio);
    let _ = std::fs::remove_dir(directory);
}

#[test]
fn temporary_playback_does_not_join_another_live_connection_at_the_end() {
    let context = egui::Context::default();
    let mut studio = Studio::default();
    studio.persist_layout = false;
    let id = studio
        .temporary_recordings
        .insert(1, Arc::new(recording(3)), None, Status::Completed);
    studio.connect_live_session(2);
    studio.accept_live_snapshot(snapshot(2, 12));
    studio.select_temporary_recording(id);
    studio.seek(2);
    studio.start_playback(1);
    assert_eq!(studio.temporary_recordings.selected, Some(id));
    assert!(!studio.follow_live);
    studio.seek(2);
    studio.playing = 1;
    studio.poll(&context);
    assert_eq!(studio.frame, 2);
    assert_eq!(studio.playing, 0);
    assert_eq!(studio.temporary_recordings.selected, Some(id));
    assert_eq!(studio.display_reason, Some(DisplayReason::LastFrame));
    assert!(studio.live_connected);
}

#[test]
fn export_source_pins_the_list_entry_even_when_display_uses_a_frame_clone() {
    let mut studio = Studio::default();
    let recording = Arc::new(recording(3));
    let displayed = Arc::new((*recording).clone());
    let id = studio
        .temporary_recordings
        .insert(1, recording, None, Status::Completed);
    studio.select_temporary_recording(id);
    studio.recording = Some(displayed);
    let export_reader = studio.timeline_recording_arc().unwrap();
    studio.clear_recording();
    studio.temporary_recordings.insert(
        2,
        Arc::new(super::tests::recording(0)),
        None,
        Status::Completed,
    );
    assert_eq!(studio.temporary_recordings.reclaim(1, 0), 0);
    assert!(studio.temporary_recordings.get(id).is_some());
    drop(export_reader);
    assert_eq!(studio.temporary_recordings.reclaim(1, 0), 1);
}

#[test]
fn opening_a_file_waits_for_the_current_capture_and_retains_its_final_frames() {
    let context = egui::Context::default();
    let (server, events) = NetworkSession::test_session(snapshot(1, 2));
    let mut studio = Studio::default();
    studio.server = Some(server);
    studio.connect_live_session(1);
    studio.poll_network(&context);
    let path = directory("open-after-close").join("not-created.pvdstream");
    studio.open(path.clone(), context.clone());
    assert!(studio.close_after_capture);
    assert!(studio.job.is_none());
    assert_eq!(studio.pending_open_after_capture.as_ref(), Some(&path));
    events
        .send(LiveEvent::Complete {
            session: 1,
            received_bytes: 50,
            result: Box::new(Ok(recording(5))),
            capture: None,
        })
        .unwrap();
    events.send(LiveEvent::Stopped).unwrap();
    studio.poll_network(&context);
    assert!(studio.server.is_none());
    assert!(studio.pending_open_after_capture.is_none());
    assert!(studio.job.is_some());
    let entry = studio.temporary_recordings.entries().next().unwrap();
    assert_eq!(entry.frame_count(), 5);
    assert_eq!(entry.status, Status::Closed);
    studio.poll_shutdown();
}

#[test]
fn opening_before_connected_is_polled_drains_and_retains_the_pending_capture() {
    let context = egui::Context::default();
    let (server, events) = NetworkSession::test_session(snapshot(1, 2));
    let mut studio = Studio::default();
    studio.server = Some(server);
    events.send(LiveEvent::Connected(1)).unwrap();
    assert!(!studio.live_connected);
    studio.open(
        directory("pending-open").join("not-created.pvdstream"),
        context.clone(),
    );
    assert!(studio.server.as_ref().unwrap().is_stopping());
    assert!(studio.job.is_none());
    events
        .send(LiveEvent::Complete {
            session: 1,
            received_bytes: 50,
            result: Box::new(Ok(recording(5))),
            capture: None,
        })
        .unwrap();
    events.send(LiveEvent::Stopped).unwrap();
    studio.poll_network(&context);
    assert!(studio.server.is_none());
    assert!(studio.job.is_some());
    let entry = studio.temporary_recordings.entries().next().unwrap();
    assert_eq!(entry.frame_count(), 5);
    assert_eq!(entry.status, Status::Closed);
    studio.poll_shutdown();
}
