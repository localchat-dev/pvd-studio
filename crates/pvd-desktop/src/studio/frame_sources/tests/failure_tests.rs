use super::*;
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[test]
fn pausing_a_source_wait_keeps_the_displayed_source_and_frame() {
    let recording = recording_for_sources(&[SOURCE_A, SOURCE_B, SOURCE_A]);
    let (session, _events) = NetworkSession::test_session(snapshot(recording, 2));
    let context = egui::Context::default();
    let mut studio = Studio::default();
    studio.connect_live_session(1);
    studio.server = Some(session);
    studio.poll_network(&context);
    let state = studio.state.clone();
    let identity = studio.displayed_frame_source;
    studio.select_frame_source(source(SOURCE_B));
    assert_eq!(studio.display_mode(), crate::studio::DisplayMode::Waiting);

    studio.pause_display();

    assert!(studio.live_connected);
    assert!(!studio.follow_live);
    assert_eq!(studio.display_mode(), crate::studio::DisplayMode::Paused);
    assert_eq!(studio.frame_source(), Some(source(SOURCE_A)));
    assert_eq!(studio.displayed_frame_source, identity);
    assert_eq!(studio.frame, 2);
    assert_eq!(studio.state, state);
}

#[test]
fn failed_capture_during_source_wait_preserves_the_displayed_identity() {
    for expired in [false, true] {
        let mut recording = recording_for_sources(&[SOURCE_A, SOURCE_B, SOURCE_A]);
        if expired {
            recording.frame_sources.discard_before(2);
        }
        let (session, events) = NetworkSession::test_session(snapshot(recording, 2));
        let context = egui::Context::default();
        let mut studio = Studio::default();
        studio.connect_live_session(1);
        studio.server = Some(session);
        studio.poll_network(&context);
        let displayed = studio.state.clone();
        let identity = studio.displayed_frame_source;
        studio.select_frame_source(source(SOURCE_B));
        assert_eq!(studio.display_mode(), crate::studio::DisplayMode::Waiting);
        events
            .send(LiveEvent::Complete {
                session: 1,
                received_bytes: 1,
                result: Box::new(Err("stream ended in a partial event group".into())),
                capture: None,
            })
            .unwrap();
        studio.poll_network(&context);
        assert!(!studio.live_connected);
        assert!(!studio.follow_live);
        assert_eq!(studio.display_mode(), crate::studio::DisplayMode::Paused);
        assert_eq!(studio.frame_source(), Some(source(SOURCE_A)));
        assert_eq!(studio.displayed_frame_source, identity);
        assert_eq!(studio.frame, 2);
        assert_eq!(studio.state, displayed);
        assert!(studio.notices.iter().any(|notice| {
            notice
                .text(Language::English)
                .contains("stream ended in a partial event group")
        }));
    }
}

fn stream(first: usize, sources: &[u64], initialize: bool) -> Vec<u8> {
    let mut events = Vec::new();
    let mut count = 0u32;
    if initialize {
        for (handle, text) in [
            (1u32, "physx3"),
            (2, "Marker"),
            (3, "frame"),
            (4, "marker"),
            (5, "PvdU32"),
            (6, "PxScene"),
            (7, ""),
        ] {
            events.push(1);
            events.extend((text.len() as u32 + 1).to_le_bytes());
            events.extend(text.as_bytes());
            events.push(0);
            events.extend(handle.to_le_bytes());
            count += 1;
        }
        for class in [2u32, 6] {
            events.push(2);
            events.extend(1u32.to_le_bytes());
            events.extend(class.to_le_bytes());
            count += 1;
        }
        events.push(4);
        for handle in [1u32, 2, 4, 7, 1, 5] {
            events.extend(handle.to_le_bytes());
        }
        events.push(1);
        events.extend(0u32.to_le_bytes());
        count += 1;
        for (class, id) in [(2u32, 99u64), (6, SOURCE_A), (6, SOURCE_B)] {
            events.push(6);
            events.extend(1u32.to_le_bytes());
            events.extend(class.to_le_bytes());
            events.extend(id.to_le_bytes());
            count += 1;
        }
    }
    for (index, source) in sources.iter().enumerate() {
        let index = first + index;
        events.push(18);
        events.extend(source.to_le_bytes());
        events.extend(3u32.to_le_bytes());
        events.extend((index as u64 * 100).to_le_bytes());
        events.push(7);
        events.extend(99u64.to_le_bytes());
        for value in [4u32, 1, 5, 1, 4, index as u32] {
            events.extend(value.to_le_bytes());
        }
        events.push(19);
        events.extend(source.to_le_bytes());
        events.extend(3u32.to_le_bytes());
        events.extend((index as u64 * 100 + 1).to_le_bytes());
        count += 3;
    }
    let mut bytes = Vec::new();
    if initialize {
        bytes.extend(pvd_protocol::STREAM_MAGIC.to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(1u64.to_le_bytes());
        bytes.extend(1000u64.to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
    }
    bytes.extend((events.len() as u32).to_le_bytes());
    bytes.extend(count.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    bytes.extend(events);
    bytes
}

fn cache_path() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../output")
        .join(format!(
            "source-seek-failure-{}-{nonce}",
            std::process::id()
        ))
}

#[test]
fn expired_cross_source_disk_seek_restores_the_displayed_source() {
    let cache = cache_path();
    let mut live = pvd_core::LiveRecording::with_history(pvd_core::live::HistoryConfig {
        directory: cache.clone(),
        disk_budget_bytes: 64 << 10,
        segment_frames: 1,
        min_free_disk_bytes: 0,
        ..Default::default()
    })
    .unwrap();
    live.feed(&stream(0, &[SOURCE_A, SOURCE_B, SOURCE_A, SOURCE_B], true))
        .unwrap();
    let recording = live.snapshot().unwrap();
    assert!(recording.is_frame_retained(1));
    let displayed = recording.state_at(2).unwrap();
    let mut studio = Studio::default();
    studio.set_recording(recording.clone());
    studio.state = displayed.clone();
    studio.remember_display_frame(2);
    studio.commit_room_display();
    let identity = studio.displayed_frame_source;
    let displayed_room = studio.displayed_room();
    assert_eq!(
        displayed_room,
        Some(pvd_core::frame_sources::FrameObject {
            id: SOURCE_A,
            generation: 0
        })
    );
    for index in 4..1024 {
        if !recording.is_frame_retained(1) {
            break;
        }
        let source = if index % 2 == 0 { SOURCE_A } else { SOURCE_B };
        live.feed(&stream(index, &[source], false)).unwrap();
    }
    assert!(!recording.is_frame_retained(1));
    assert!(recording.frame_info(1).is_some());
    assert!(
        recording.is_frame_retained(3),
        "a later frame must not replace the paused alignment target"
    );
    studio.select_frame_source(source(SOURCE_B));
    assert_eq!(studio.seeking, Some(1));
    assert_eq!(studio.displayed_room(), displayed_room);
    assert_eq!(studio.frame, 2);
    assert_eq!(studio.room_scope.room(), displayed_room);
    let context = egui::Context::default();
    let deadline = Instant::now() + Duration::from_secs(5);
    while studio.seeking.is_some() {
        assert!(Instant::now() < deadline, "expired source seek timed out");
        studio.poll_seek(&context);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(studio.frame_source(), Some(source(SOURCE_A)));
    assert_eq!(studio.displayed_frame_source, identity);
    assert_eq!(studio.frame, 2);
    assert_eq!(studio.state, displayed);
    assert_eq!(studio.room_view.requested, displayed_room);
    assert_eq!(studio.displayed_room(), displayed_room);
    assert_eq!(studio.room_scope.room(), displayed_room);
    assert_eq!(studio.display_reason, Some(DisplayReason::SeekFailed));
    assert_eq!(studio.playing, 0);
    assert!(
        studio
            .notices
            .iter()
            .any(|notice| notice.text(Language::English).contains("Frame 1:"))
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !studio.poll_shutdown() {
        assert!(Instant::now() < deadline, "seek worker release timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
    drop(studio);
    drop(recording);
    drop(live);
    assert!(fs::read_dir(&cache).unwrap().next().is_none());
    fs::remove_dir(cache).unwrap();
}

#[test]
fn paused_room_switch_uses_first_frame_only_when_the_room_has_no_earlier_boundary() {
    let recording = recording_for_sources(&[SOURCE_A, SOURCE_A, SOURCE_B, SOURCE_A, SOURCE_B]);
    let mut studio = Studio::default();
    studio.set_recording(recording);
    studio.seek(1);
    assert!(!studio.follow_live);
    assert_eq!(studio.displayed_room().unwrap().id, SOURCE_A);
    studio.select_frame_source(source(SOURCE_B));
    assert_eq!(studio.frame, 2);
    assert_eq!(studio.displayed_room().unwrap().id, SOURCE_B);
    studio.select_frame_source(source(SOURCE_A));
    assert_eq!(studio.frame, 1);
    assert_eq!(studio.displayed_room().unwrap().id, SOURCE_A);
    assert!(!studio.follow_live);
}
