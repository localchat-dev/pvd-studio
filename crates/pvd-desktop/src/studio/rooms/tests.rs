use super::*;
use crate::network::{LiveEvent, LiveSnapshot, NetworkSession};
use pvd_core::{Recording, RecordingBuilder, live::SceneUpdate};
use pvd_protocol::{Endian, Event, EventGroup, Header, Name};

mod tcp;

fn room(id: u64, generation: u32) -> FrameObject {
    FrameObject { id, generation }
}

fn source(id: u64) -> FrameSourceKey {
    FrameSourceKey::Captured {
        section: FrameSection::PhysX,
        source: id,
    }
}

fn recordings() -> (Recording, Recording) {
    let mut builder = RecordingBuilder::new(Header {
        version: 1,
        timestamp_numerator: 1,
        timestamp_denominator: 1,
        flags: 1,
        endian: Endian::Little,
    });
    let class = Name {
        namespace: 1,
        name: 2,
    };
    let mut initial: Vec<_> = [(1, "physx3"), (2, "PxScene"), (3, "frame")]
        .into_iter()
        .map(|(handle, value)| Event::StringHandle {
            handle,
            value: value.into(),
        })
        .collect();
    initial.push(Event::CreateClass { name: class });
    initial.extend(
        [10, 20, 30]
            .into_iter()
            .map(|id| Event::CreateInstance { id, class }),
    );
    initial.extend(
        [10, 20, 10]
            .into_iter()
            .enumerate()
            .map(|(frame, id)| Event::EndSection {
                id,
                name: 3,
                timestamp: frame as u64,
            }),
    );
    builder
        .push(EventGroup {
            stream_id: 1,
            timestamp: 0,
            offset: 0,
            events: initial,
        })
        .unwrap();
    let initial = builder.snapshot().unwrap();
    let mut events = vec![
        Event::DestroyInstance { id: 30 },
        Event::DestroyInstance { id: 10 },
        Event::CreateInstance { id: 10, class },
    ];
    events.extend(
        [20, 10, 20, 10]
            .into_iter()
            .enumerate()
            .map(|(frame, id)| Event::EndSection {
                id,
                name: 3,
                timestamp: frame as u64 + 3,
            }),
    );
    events.push(Event::DestroyInstance { id: 20 });
    builder
        .push(EventGroup {
            stream_id: 1,
            timestamp: 0,
            offset: 0,
            events,
        })
        .unwrap();
    (initial, builder.snapshot().unwrap())
}

fn snapshot(recording: Recording, frame: usize) -> LiveSnapshot {
    let state = recording.state_at(frame).unwrap();
    let source_frame = recording
        .frame_sources
        .frame(frame)
        .map(|(source, frame)| (source, *frame));
    LiveSnapshot {
        session: 1,
        frame,
        recording,
        source_frame,
        update: SceneUpdate::full(&state),
        display_updated: true,
    }
}

fn metadata(recording: Recording) -> LiveSnapshot {
    LiveSnapshot {
        session: 1,
        frame: 0,
        recording,
        source_frame: None,
        update: SceneUpdate::default(),
        display_updated: false,
    }
}

#[test]
fn empty_room_selection_does_not_discard_display() {
    let mut studio = Studio::default();
    studio.select_room(Some(FrameObject {
        id: 123,
        generation: 0,
    }));
    assert_eq!(studio.room_view.requested, None);
    assert!(!studio.room_selection_pending());
}

#[test]
fn lifecycle_only_snapshot_has_no_synthetic_playback_frame() {
    let (mut recording, _) = recordings();
    recording.frames = Default::default();
    recording.frame_sources = Default::default();
    let mut studio = Studio::default();
    studio.connect_live_session(1);
    studio.accept_live_snapshot(metadata(recording));
    studio.commit_room_display();
    assert_eq!(studio.count(), 0);
    assert_eq!(studio.playback_bounds(), None);
    assert!(studio.state.objects.is_empty());
    assert!(!studio.room_selection_pending());
    assert_eq!(studio.room_directory().unwrap().len(), 3);
}

#[test]
fn returning_to_live_waits_for_the_selected_room_endpoint() {
    let (recording, _) = recordings();
    let mut studio = Studio::default();
    studio.connect_live_session(1);
    studio.accept_live_snapshot(snapshot(recording.clone(), 2));
    studio.pause_display();
    studio.select_room(Some(room(20, 0)));
    assert_eq!(studio.displayed_room(), Some(room(20, 0)));
    studio.return_to_live();
    assert!(studio.follow_live);
    assert_eq!(studio.display_mode(), crate::studio::DisplayMode::Waiting);
    studio.accept_live_snapshot(snapshot(recording, 1));
    assert_eq!(studio.display_mode(), crate::studio::DisplayMode::Live);
    assert_eq!(studio.displayed_room(), Some(room(20, 0)));
}

#[test]
fn reused_scene_address_has_separate_room_bounds_and_latest_lifecycle_during_rewind() {
    let (_, recording) = recordings();
    let mut studio = Studio::default();
    studio.set_recording(recording);
    studio.select_room(Some(room(10, 0)));
    assert_eq!(studio.displayed_room(), Some(room(10, 0)));
    assert_eq!(studio.playback_bounds(), Some((0, 2)));
    studio.step_frame(1);
    assert_eq!(studio.frame, 2);
    assert_eq!(studio.next_playback_frame(1), None);
    assert_eq!(studio.frame_at_ordinal(3), Some(2));
    assert!(
        studio
            .room_directory()
            .unwrap()
            .get(room(10, 0))
            .unwrap()
            .destroyed_at_frame
            .is_some()
    );
    assert!(
        studio
            .room_directory()
            .unwrap()
            .get(room(20, 0))
            .unwrap()
            .destroyed_at_frame
            .is_some()
    );
    studio.select_room(Some(room(10, 1)));
    assert_eq!(studio.frame_source(), Some(source(10)));
    assert_eq!(studio.displayed_room(), Some(room(10, 1)));
    assert_eq!(studio.frame, 4);
    assert_eq!(studio.playback_bounds(), Some((4, 6)));
    assert_eq!(studio.next_playback_frame(-1), None);
    studio.step_frame(1);
    assert_eq!(studio.frame, 6);
    studio.select_room(Some(room(10, 0)));
    assert_eq!(studio.frame, 2);
    assert_eq!(studio.state.objects[&10].generation, 0);
    assert_eq!(
        studio
            .room_directory()
            .unwrap()
            .get(room(10, 1))
            .unwrap()
            .number,
        4
    );
    assert_eq!(
        studio
            .room_directory()
            .unwrap()
            .get(room(10, 0))
            .unwrap()
            .number,
        1
    );
}

#[test]
fn pending_room_keeps_display_tree_scope_and_bounds_until_matching_snapshot() {
    let (recording, _) = recordings();
    let mut studio = Studio::default();
    studio.connect_live_session(1);
    studio.accept_live_snapshot(snapshot(recording.clone(), 2));
    let state = studio.state.clone();
    let revision = studio.revision;
    studio.select_room(Some(room(20, 0)));
    assert!(studio.room_selection_pending());
    assert_eq!(studio.displayed_room(), Some(room(10, 0)));
    assert_eq!(studio.playback_bounds(), Some((0, 2)));
    studio.accept_live_snapshot(snapshot(recording.clone(), 0));
    assert_eq!(studio.state, state);
    assert_eq!(studio.revision, revision);
    assert_eq!(studio.displayed_room(), Some(room(10, 0)));
    assert_eq!(studio.room_scope.room(), Some(room(10, 0)));
    studio.accept_live_snapshot(snapshot(recording, 1));
    assert!(!studio.room_selection_pending());
    assert_eq!(studio.displayed_room(), Some(room(20, 0)));
    assert_eq!(studio.room_scope.room(), Some(room(20, 0)));
    assert_eq!(studio.frame, 1);
    assert_eq!(studio.playback_bounds(), Some((1, 1)));
}

#[test]
fn lifecycle_metadata_pauses_destroyed_live_room_without_replacing_its_last_display() {
    let (initial, final_recording) = recordings();
    let mut studio = Studio::default();
    studio.connect_live_session(1);
    studio.accept_live_snapshot(snapshot(initial, 2));
    let state = studio.state.clone();
    let revision = studio.revision;
    studio.accept_live_snapshot(metadata(final_recording));
    assert!(!studio.follow_live);
    assert!(studio.live_connected);
    assert_eq!(studio.state, state);
    assert_eq!(studio.revision, revision);
    assert_eq!(studio.frame, 2);
    assert_eq!(studio.displayed_room(), Some(room(10, 0)));
    assert!(
        studio
            .room_directory()
            .unwrap()
            .get(room(30, 0))
            .unwrap()
            .destroyed_at_frame
            .is_some()
    );
    assert!(!studio.room_has_history(room(30, 0)));
    studio.step_frame(-1);
    assert_eq!(studio.frame, 0);
    assert!(
        studio
            .room_directory()
            .unwrap()
            .get(room(10, 0))
            .unwrap()
            .destroyed_at_frame
            .is_some()
    );
}

#[test]
fn no_history_and_failed_connection_restore_the_displayed_room() {
    let (initial, final_recording) = recordings();
    let mut studio = Studio::default();
    studio.set_recording(final_recording);
    studio.seek(0);
    studio.select_room(Some(room(30, 0)));
    assert_eq!(studio.room_view.requested, studio.displayed_room());
    assert_eq!(studio.frame_source(), Some(source(10)));
    let (session, events) = NetworkSession::test_session(snapshot(initial, 2));
    let context = egui::Context::default();
    studio.connect_live_session(1);
    studio.server = Some(session);
    studio.poll_network(&context);
    let state = studio.state.clone();
    studio.select_room(Some(room(20, 0)));
    assert!(studio.room_selection_pending());
    events
        .send(LiveEvent::Complete {
            session: 1,
            received_bytes: 1,
            result: Box::new(Err("incomplete stream".into())),
            capture: None,
        })
        .unwrap();
    studio.poll_network(&context);
    assert!(!studio.live_connected);
    assert!(!studio.follow_live);
    assert!(!studio.room_selection_pending());
    assert_eq!(studio.room_view.requested, Some(room(10, 0)));
    assert_eq!(studio.displayed_room(), Some(room(10, 0)));
    assert_eq!(studio.frame, 2);
    assert_eq!(studio.state, state);
}

#[test]
fn clean_disconnect_resolves_pending_room_without_waiting_for_another_end() {
    for (requested, expected, frame) in
        [(room(20, 0), room(20, 0), 1), (room(30, 0), room(10, 0), 2)]
    {
        let (recording, _) = recordings();
        let (session, events) = NetworkSession::test_session(snapshot(recording.clone(), 2));
        let mut studio = Studio::default();
        let context = egui::Context::default();
        studio.connect_live_session(1);
        studio.server = Some(session);
        studio.poll_network(&context);
        studio.select_room(Some(requested));
        assert!(studio.room_selection_pending());
        events
            .send(LiveEvent::Complete {
                session: 1,
                received_bytes: 1,
                result: Box::new(Ok(recording)),
                capture: None,
            })
            .unwrap();
        studio.poll_network(&context);
        assert!(!studio.room_selection_pending());
        assert!(!studio.follow_live);
        assert_eq!(studio.displayed_room(), Some(expected));
        assert_eq!(studio.frame, frame);
    }
}

#[test]
fn room_labels_use_physics_scene_numbers_and_keep_reused_addresses_distinct() {
    let (_, recording) = recordings();
    let mut studio = Studio::default();
    studio.recording = Some(std::sync::Arc::new(recording));
    for (language, expected) in [
        (
            Language::English,
            ["Physics Scene 1", "Physics Scene 2", "Physics Scene 4"],
        ),
        (
            Language::Chinese,
            ["物理场景 1", "物理场景 2", "物理场景 4"],
        ),
    ] {
        studio.prefs.language = language;
        for (scene, expected) in [room(10, 0), room(20, 0), room(10, 1)]
            .into_iter()
            .zip(expected)
        {
            assert_eq!(studio.room_label(scene), expected);
        }
        studio.seek(0);
        assert_eq!(studio.room_label(room(10, 1)), expected[2]);
        assert_eq!(
            studio.room_label(room(999, 0)),
            language.text("Physics Scene", "物理场景")
        );
    }
}
