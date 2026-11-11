use super::*;
use crate::{
    i18n::Language,
    network::{LiveEvent, LiveSnapshot, NetworkSession},
};
use pvd_core::{
    Recording, RecordingBuilder, SceneState, Value,
    frame_sources::{FrameSection, SourceFrame},
    live::SceneUpdate,
};
use pvd_protocol::{Endian, Event, EventGroup, Header, Name};

mod failure_tests;

const SOURCE_A: u64 = 10;
const SOURCE_B: u64 = 20;

fn source(id: u64) -> FrameSourceKey {
    FrameSourceKey::Captured {
        section: FrameSection::PhysX,
        source: id,
    }
}

fn recording_for_sources(sources: &[u64]) -> Recording {
    let mut builder = RecordingBuilder::new(Header {
        version: 1,
        timestamp_numerator: 1,
        timestamp_denominator: 1000,
        flags: 1,
        endian: Endian::Little,
    });
    let marker = Name {
        namespace: 1,
        name: 2,
    };
    let integer = Name {
        namespace: 1,
        name: 5,
    };
    let scene = Name {
        namespace: 1,
        name: 6,
    };
    let mut events = [
        (1, "physx3"),
        (2, "Marker"),
        (3, "frame"),
        (4, "marker"),
        (5, "PvdU32"),
        (6, "PxScene"),
        (7, ""),
    ]
    .into_iter()
    .map(|(handle, value)| Event::StringHandle {
        handle,
        value: value.into(),
    })
    .collect::<Vec<_>>();
    events.extend([
        Event::CreateClass { name: marker },
        Event::CreateClass { name: scene },
        Event::CreateProperty {
            class: marker,
            name: 4,
            semantic: 7,
            datatype: integer,
            kind: 1,
            named_values: Vec::new(),
        },
        Event::CreateInstance {
            class: marker,
            id: 99,
        },
        Event::CreateInstance {
            class: scene,
            id: SOURCE_A,
        },
        Event::CreateInstance {
            class: scene,
            id: SOURCE_B,
        },
    ]);
    for (index, &id) in sources.iter().enumerate() {
        let index = index as u32;
        events.extend([
            Event::BeginSection {
                id,
                name: 3,
                timestamp: u64::from(index) * 100,
            },
            Event::SetPropertyValue {
                id: 99,
                property: 4,
                datatype: integer,
                items: 1,
                data: index.to_le_bytes().to_vec().into(),
            },
            Event::EndSection {
                id,
                name: 3,
                timestamp: u64::from(index) * 100 + 1,
            },
        ]);
    }
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

fn snapshot(recording: Recording, index: usize) -> LiveSnapshot {
    let state = recording.state_at(index).unwrap();
    let source_frame = recording
        .frame_sources
        .frame(index)
        .map(|(key, frame)| (key, *frame));
    LiveSnapshot {
        session: 1,
        frame: index,
        recording,
        update: SceneUpdate::full(&state),
        display_updated: true,
        source_frame,
    }
}

fn metadata(recording: Recording, displayed: (FrameSourceKey, SourceFrame)) -> LiveSnapshot {
    LiveSnapshot {
        session: 1,
        frame: displayed.1.index,
        recording,
        update: SceneUpdate::default(),
        display_updated: false,
        source_frame: Some(displayed),
    }
}

fn marker(state: &SceneState) -> u64 {
    match state.objects[&99].properties["marker"] {
        Value::U64(value) => value,
        ref value => panic!("unexpected marker: {value:?}"),
    }
}

#[test]
fn selected_source_steps_through_its_ordinals_and_real_global_endpoints() {
    let mut studio = Studio::default();
    studio.set_recording(recording_for_sources(&[
        SOURCE_A, SOURCE_B, SOURCE_A, SOURCE_B, SOURCE_A, SOURCE_B, SOURCE_A, SOURCE_B,
    ]));
    studio.seek(4);
    assert_eq!(studio.frame_source(), Some(source(SOURCE_A)));
    assert_eq!(studio.frame_ordinal(studio.frame), 2);
    assert_eq!(marker(&studio.state), 4);

    studio.select_frame_source(source(SOURCE_B));
    assert_eq!(studio.frame, 3);
    assert_eq!(studio.frame_ordinal(studio.frame), 1);
    assert_eq!(marker(&studio.state), 3);
    assert_eq!(studio.playback_bounds(), Some((1, 7)));
    assert_eq!(studio.playback_ordinal_bounds(), Some((0, 3)));
    studio.step_frame(1);
    assert_eq!(studio.frame, 5);
    assert_eq!(studio.frame_ordinal(studio.frame), 2);
    assert_eq!(marker(&studio.state), 5);
    studio.step_frame(-1);
    assert_eq!(studio.frame, 3);
    assert_eq!(studio.frame_source(), Some(source(SOURCE_B)));
}

#[test]
fn rolling_metadata_keeps_paused_source_ordinal_and_display_state() {
    let recording = recording_for_sources(&[SOURCE_A, SOURCE_B, SOURCE_A, SOURCE_B, SOURCE_A]);
    let displayed = recording
        .frame_sources
        .frame(4)
        .map(|(key, frame)| (key, *frame))
        .unwrap();
    let mut studio = Studio::default();
    studio.prefs.language = Language::English;
    studio.connect_live_session(1);
    studio.accept_live_snapshot(snapshot(recording.clone(), 4));
    studio.pause_display();
    let prior_state = studio.state.clone();
    let prior_revision = studio.revision;
    let prior_recording = studio.recording.clone().unwrap();

    let mut rolled = recording_for_sources(&[
        SOURCE_A, SOURCE_B, SOURCE_A, SOURCE_B, SOURCE_A, SOURCE_B, SOURCE_B, SOURCE_B,
    ]);
    rolled.frames = rolled
        .frames
        .iter()
        .filter(|frame| frame.index >= 5)
        .cloned()
        .collect();
    rolled.frame_sources.discard_before(5);
    assert_eq!(
        rolled.frame_sources.default_source(),
        Some(source(SOURCE_A))
    );
    assert!(rolled.frame_sources.frame(4).is_none());
    studio.accept_live_snapshot(metadata(rolled, displayed));

    assert_eq!(studio.frame, 4);
    assert_eq!(studio.live_frame, 4);
    assert_eq!(studio.frame_source(), Some(source(SOURCE_A)));
    assert_eq!(studio.frame_ordinal(studio.frame), 2);
    assert_eq!(studio.playback_bounds(), None);
    assert_eq!(studio.playback_ordinal_bounds(), None);
    assert_eq!(studio.state, prior_state);
    assert_eq!(studio.revision, prior_revision);
    assert!(std::sync::Arc::ptr_eq(
        &studio.recording.clone().unwrap(),
        &prior_recording
    ));
    assert!(
        studio
            .display_position(Language::English)
            .starts_with("Frame 2")
    );

    studio.return_to_live();
    assert!(studio.follow_live);
    assert_eq!(studio.frame, 4);
    assert_eq!(studio.frame_ordinal(studio.frame), 2);
    assert_eq!(studio.state, prior_state);
}

#[test]
fn paused_ordinal_and_lag_use_the_displayed_source_after_eviction() {
    let mut studio = Studio::default();
    studio.connect_live_session(1);
    studio.accept_live_snapshot(snapshot(
        recording_for_sources(&[SOURCE_A, SOURCE_B, SOURCE_A]),
        2,
    ));
    studio.pause_display();
    let mut latest = recording_for_sources(&[
        SOURCE_A, SOURCE_B, SOURCE_A, SOURCE_B, SOURCE_A, SOURCE_B, SOURCE_A, SOURCE_B,
    ]);
    studio.accept_live_snapshot(snapshot(latest.clone(), 6));
    let live_endpoint = latest
        .frame_sources
        .frame(6)
        .map(|(key, frame)| (key, *frame))
        .unwrap();
    latest.frames = latest
        .frames
        .iter()
        .filter(|frame| frame.index >= 5)
        .cloned()
        .collect();
    latest.frame_sources.discard_before(5);
    studio.accept_live_snapshot(metadata(latest, live_endpoint));

    assert_eq!(studio.frame, 2);
    assert_eq!(marker(&studio.state), 2);
    assert_eq!(studio.live_frame, 6);
    assert_eq!(studio.frame_source(), Some(source(SOURCE_A)));
    assert_eq!(studio.frame_ordinal(2), 1);
    assert_eq!(studio.frame_ordinal(6), 3);
    assert_eq!(
        studio.display_position(Language::English),
        "Frame 1 · 2 behind live (3)"
    );
}

#[test]
fn metadata_only_adds_source_without_replacing_display_or_analysis() {
    let mut initial = recording_for_sources(&[SOURCE_A]);
    initial
        .analysis
        .add_zone(101, "selected boundary".into())
        .unwrap();
    let displayed = initial
        .frame_sources
        .frame(0)
        .map(|(key, frame)| (key, *frame))
        .unwrap();
    let selected_analysis = initial.analysis.revision();
    let mut studio = Studio::default();
    studio.connect_live_session(1);
    studio.accept_live_snapshot(snapshot(initial, 0));
    let prior_state = studio.state.clone();
    let prior_revision = studio.revision;
    assert!(studio.source_timeline().is_some());
    assert!(
        studio
            .timeline_recording()
            .unwrap()
            .frame_sources
            .source(source(SOURCE_B))
            .is_none()
    );

    let mut later = recording_for_sources(&[SOURCE_A, SOURCE_B]);
    later
        .analysis
        .add_zone(202, "later metadata".into())
        .unwrap();
    let metadata_analysis = later.analysis.revision();
    assert_ne!(selected_analysis, metadata_analysis);
    studio.accept_live_snapshot(metadata(later, displayed));

    assert!(
        studio
            .timeline_recording()
            .unwrap()
            .frame_sources
            .source(source(SOURCE_B))
            .is_some()
    );
    assert_eq!(studio.frame, 0);
    assert_eq!(studio.state, prior_state);
    assert_eq!(studio.revision, prior_revision);
    assert_eq!(
        studio.recording.as_ref().unwrap().analysis.revision(),
        selected_analysis
    );
    assert_eq!(
        studio.live_recording.as_ref().unwrap().analysis.revision(),
        metadata_analysis
    );
    studio.select_frame_source(source(SOURCE_B));
    assert!(studio.follow_live);
    assert_eq!(studio.display_mode(), crate::studio::DisplayMode::Waiting);
    assert_eq!(studio.state, prior_state);
}

#[test]
fn completion_and_return_live_keep_last_display_analysis_separate_from_tail() {
    let mut initial = recording_for_sources(&[SOURCE_A]);
    initial
        .analysis
        .add_zone(101, "selected boundary".into())
        .unwrap();
    let selected_analysis = initial.analysis.revision();
    let (session, events) = NetworkSession::test_session(snapshot(initial, 0));
    let context = egui::Context::default();
    let mut studio = Studio::default();
    studio.connect_live_session(1);
    studio.server = Some(session);
    studio.poll_network(&context);
    let prior_state = studio.state.clone();

    let mut complete = recording_for_sources(&[SOURCE_A, SOURCE_B]);
    complete
        .analysis
        .add_zone(202, "completed tail".into())
        .unwrap();
    let tail_analysis = complete.analysis.revision();
    assert_ne!(selected_analysis, tail_analysis);
    events
        .send(LiveEvent::Complete {
            session: 1,
            received_bytes: 1,
            result: Box::new(Ok(complete)),
            capture: None,
        })
        .unwrap();
    studio.poll_network(&context);

    assert!(!studio.live_connected);
    assert!(!studio.follow_live);
    assert_eq!(studio.frame, 0);
    assert_eq!(studio.state, prior_state);
    assert_eq!(
        studio.recording.as_ref().unwrap().analysis.revision(),
        selected_analysis
    );
    assert_eq!(
        studio.live_recording.as_ref().unwrap().analysis.revision(),
        tail_analysis
    );
    studio.return_to_live();
    assert_eq!(studio.frame, 0);
    assert_eq!(studio.state, prior_state);
    assert_eq!(
        studio.recording.as_ref().unwrap().analysis.revision(),
        selected_analysis
    );
}

#[test]
fn an_expired_source_cannot_relabel_a_paused_display() {
    let mut recording = recording_for_sources(&[SOURCE_B, SOURCE_A]);
    recording.frame_sources.discard_before(1);
    let mut studio = Studio::default();
    studio.set_recording(recording);
    studio.seek(1);
    studio.select_frame_source(source(SOURCE_B));
    assert_eq!(studio.frame_source(), Some(source(SOURCE_A)));
    assert_eq!(studio.frame, 1);
    assert_eq!(marker(&studio.state), 1);
}

#[test]
fn disconnect_during_source_wait_keeps_the_cached_display_identity() {
    let mut recording = recording_for_sources(&[SOURCE_B, SOURCE_A]);
    recording.frame_sources.discard_before(1);
    let (session, events) = NetworkSession::test_session(snapshot(recording.clone(), 1));
    let context = egui::Context::default();
    let mut studio = Studio::default();
    studio.connect_live_session(1);
    studio.server = Some(session);
    studio.poll_network(&context);
    studio.select_frame_source(source(SOURCE_A));
    studio.select_frame_source(source(SOURCE_B));
    assert_eq!(studio.display_mode(), crate::studio::DisplayMode::Waiting);
    events
        .send(LiveEvent::Complete {
            session: 1,
            received_bytes: 1,
            result: Box::new(Ok(recording)),
            capture: None,
        })
        .unwrap();
    studio.poll_network(&context);
    assert!(!studio.follow_live);
    assert_eq!(studio.frame_source(), Some(source(SOURCE_A)));
    assert_eq!(studio.frame_ordinal(studio.frame), 0);
    assert_eq!(marker(&studio.state), 1);
}
