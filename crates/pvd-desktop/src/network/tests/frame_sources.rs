use super::*;
use pvd_core::{
    SceneState, Value,
    frame_sources::{FrameSection, FrameSourceKey},
};

mod session_tests;

const SOURCE_A: u64 = 10;
const SOURCE_B: u64 = 20;
const MARKER: u64 = 99;

fn source(id: u64) -> FrameSourceKey {
    FrameSourceKey::Captured {
        section: FrameSection::PhysX,
        source: id,
    }
}

fn group(events: Vec<u8>, count: u32, timestamp: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend((events.len() as u32).to_le_bytes());
    bytes.extend(count.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(timestamp.to_le_bytes());
    bytes.extend(events);
    bytes
}

fn initialization() -> Vec<u8> {
    let mut events = Vec::new();
    for (handle, text) in [
        (1u32, "physx3"),
        (2, "Marker"),
        (3, "frame"),
        (4, "X"),
        (5, "Y"),
        (6, "PvdU32"),
        (7, "PxScene"),
        (8, ""),
    ] {
        events.push(1);
        events.extend((text.len() as u32 + 1).to_le_bytes());
        events.extend(text.as_bytes());
        events.push(0);
        events.extend(handle.to_le_bytes());
    }
    for name in [2u32, 7] {
        events.push(2);
        events.extend(1u32.to_le_bytes());
        events.extend(name.to_le_bytes());
    }
    for property in [4u32, 5] {
        events.push(4);
        for value in [1u32, 2, property, 8, 1, 6] {
            events.extend(value.to_le_bytes());
        }
        events.push(1);
        events.extend(0u32.to_le_bytes());
    }
    for (class, id) in [(2u32, MARKER), (7, SOURCE_A)] {
        events.push(6);
        events.extend(1u32.to_le_bytes());
        events.extend(class.to_le_bytes());
        events.extend(id.to_le_bytes());
    }
    let mut bytes = empty_stream_header();
    bytes.extend(group(events, 14, 0));
    bytes
}

fn frame(source: u64, timestamp: u64, properties: &[(u32, u32)], create_scene: bool) -> Vec<u8> {
    let mut events = Vec::new();
    if create_scene {
        events.push(6);
        events.extend(1u32.to_le_bytes());
        events.extend(7u32.to_le_bytes());
        events.extend(source.to_le_bytes());
    }
    events.push(18);
    events.extend(source.to_le_bytes());
    events.extend(3u32.to_le_bytes());
    events.extend(timestamp.to_le_bytes());
    for &(property, value) in properties {
        events.push(7);
        events.extend(MARKER.to_le_bytes());
        for item in [property, 1, 6, 1, 4, value] {
            events.extend(item.to_le_bytes());
        }
    }
    events.push(19);
    events.extend(source.to_le_bytes());
    events.extend(3u32.to_le_bytes());
    events.extend(timestamp.saturating_add(1).to_le_bytes());
    group(
        events,
        2 + properties.len() as u32 + u32::from(create_scene),
        timestamp,
    )
}

fn fixture(environment: &TestEnvironment) -> (NetworkSession, WorkerOutput, LiveClient) {
    let (session, sender) = NetworkSession::test_session(LiveSnapshot {
        session: 1,
        frame: 0,
        recording: Recording::from_bytes(&empty_stream_header()).unwrap(),
        update: SceneUpdate::default(),
        display_updated: false,
        source_frame: None,
    });
    session.take_snapshot();
    let output = WorkerOutput {
        sender,
        cancel: session.cancel.clone(),
        stop: session.stop.clone(),
        context: egui::Context::default(),
        snapshot: session.snapshot.clone(),
        frame_source: session.frame_source.clone(),
    };
    let mut live = LiveRecording::with_history(HistoryConfig {
        directory: environment.history.clone(),
        min_free_disk_bytes: 0,
        ..Default::default()
    })
    .unwrap();
    live.feed(&initialization()).unwrap();
    let client = LiveClient {
        session: 1,
        peer: "fixture".into(),
        live: Some(live),
        failure: None,
        raw: None,
        raw_attempted: false,
        received_bytes: 0,
        published_frames: 0,
        selection_revision: 0,
        last_publish: Instant::now(),
        profile: None,
    };
    (session, output, client)
}

fn feed(output: &WorkerOutput, client: &mut LiveClient, bytes: Vec<u8>) {
    output.sync_frame_source(client);
    client.live.as_mut().unwrap().feed(&bytes).unwrap();
    output.publish(client);
}

fn value(state: &SceneState, name: &str) -> u64 {
    match state.objects[&MARKER].properties[name] {
        Value::U64(value) => value,
        ref value => panic!("unexpected marker value: {value:?}"),
    }
}

#[test]
fn round_trip_source_request_replaces_dropped_unconsumed_delta() {
    let environment = TestEnvironment::new();
    let (session, output, mut client) = fixture(&environment);
    session.set_frame_source(1, Some(source(SOURCE_A)));
    feed(
        &output,
        &mut client,
        frame(SOURCE_A, 10, &[(4, 1), (5, 2)], false),
    );
    let initial = session.take_snapshot().unwrap();
    assert!(initial.display_updated);
    let mut displayed = SceneState::default();
    initial.update.apply(&mut displayed);
    assert_eq!((value(&displayed, "X"), value(&displayed, "Y")), (1, 2));

    feed(&output, &mut client, frame(SOURCE_A, 20, &[(4, 21)], false));
    assert!(session.snapshot.lock().unwrap().is_some());
    session.set_frame_source(1, Some(source(SOURCE_B)));
    session.set_frame_source(1, Some(source(SOURCE_A)));
    feed(&output, &mut client, frame(SOURCE_A, 30, &[(5, 31)], false));

    let resumed = session.take_snapshot().unwrap();
    assert!(resumed.display_updated);
    assert!(resumed.update.replace_all);
    let (selected, endpoint) = resumed.source_frame.unwrap();
    assert_eq!(selected, source(SOURCE_A));
    assert_eq!((endpoint.index, endpoint.ordinal), (2, 2));
    resumed.update.apply(&mut displayed);
    assert_eq!((value(&displayed, "X"), value(&displayed, "Y")), (21, 31));
}

#[test]
fn new_source_becomes_selectable_while_display_source_has_no_more_ends() {
    let environment = TestEnvironment::new();
    let (session, output, mut client) = fixture(&environment);
    feed(
        &output,
        &mut client,
        frame(SOURCE_A, 10, &[(4, 1), (5, 2)], false),
    );
    let initial = session.take_snapshot().unwrap();
    let mut displayed = SceneState::default();
    initial.update.apply(&mut displayed);

    feed(&output, &mut client, frame(SOURCE_B, 20, &[(4, 40)], true));
    let metadata = session
        .take_snapshot()
        .expect("new source directory was not published");
    assert!(!metadata.display_updated);
    assert_eq!(metadata.recording.frame_count(), 2);
    let timeline = metadata
        .recording
        .frame_sources
        .source(source(SOURCE_B))
        .unwrap();
    assert_eq!(
        (
            timeline.first().unwrap().index,
            timeline.first().unwrap().ordinal
        ),
        (1, 0)
    );
    let prior = displayed.clone();
    metadata.update.apply(&mut displayed);
    assert_eq!(displayed, prior);

    session.set_frame_source(1, Some(source(SOURCE_B)));
    feed(&output, &mut client, frame(SOURCE_B, 30, &[(5, 41)], false));
    let selected = session.take_snapshot().unwrap();
    assert!(selected.display_updated);
    assert!(selected.update.replace_all);
    let (key, endpoint) = selected.source_frame.unwrap();
    assert_eq!(key, source(SOURCE_B));
    assert_eq!((endpoint.index, endpoint.ordinal), (2, 1));
    selected.update.apply(&mut displayed);
    assert_eq!((value(&displayed, "X"), value(&displayed, "Y")), (40, 41));
    assert!(displayed.objects.contains_key(&SOURCE_B));
}

#[test]
fn metadata_publication_preserves_an_unconsumed_display_endpoint() {
    let environment = TestEnvironment::new();
    let (session, output, mut client) = fixture(&environment);
    feed(
        &output,
        &mut client,
        frame(SOURCE_A, 10, &[(4, 1), (5, 2)], false),
    );
    feed(&output, &mut client, frame(SOURCE_B, 20, &[(4, 40)], true));

    let combined = session.take_snapshot().unwrap();
    assert!(combined.display_updated);
    assert_eq!(combined.frame, 0);
    assert_eq!(combined.recording.frame_count(), 2);
    assert_eq!(combined.source_frame.unwrap().0, source(SOURCE_A));
    assert!(
        combined
            .recording
            .frame_sources
            .source(source(SOURCE_B))
            .is_some()
    );
    let mut displayed = SceneState::default();
    combined.update.apply(&mut displayed);
    assert_eq!((value(&displayed, "X"), value(&displayed, "Y")), (1, 2));
    assert!(!displayed.objects.contains_key(&SOURCE_B));
}

fn await_snapshot(
    session: &NetworkSession,
    accept: impl Fn(&LiveSnapshot) -> bool,
) -> LiveSnapshot {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(snapshot) = session.take_snapshot()
            && accept(&snapshot)
        {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "frame source publication timed out"
        );
        std::thread::park_timeout(Duration::from_millis(2));
    }
}

#[test]
fn tcp_worker_switches_sources_and_keeps_updates_between_selected_ends() {
    let environment = TestEnvironment::new();
    let session = environment
        .start(
            ListenConfig {
                address: "127.0.0.1:0".parse().unwrap(),
                ..Default::default()
            },
            egui::Context::default(),
        )
        .unwrap();
    let mut client = TcpStream::connect(session.address()).unwrap();
    client.write_all(&initialization()).unwrap();
    client
        .write_all(&frame(SOURCE_A, 10, &[(4, 1), (5, 2)], false))
        .unwrap();
    let initial = await_snapshot(&session, |snapshot| snapshot.display_updated);
    let mut displayed = SceneState::default();
    initial.update.apply(&mut displayed);
    assert_eq!((value(&displayed, "X"), value(&displayed, "Y")), (1, 2));

    client
        .write_all(&frame(SOURCE_B, 20, &[(4, 40)], true))
        .unwrap();
    let metadata = await_snapshot(&session, |snapshot| {
        snapshot
            .recording
            .frame_sources
            .source(source(SOURCE_B))
            .is_some()
    });
    assert!(!metadata.display_updated);
    session.set_frame_source(initial.session, Some(source(SOURCE_B)));
    client
        .write_all(&frame(SOURCE_B, 30, &[(5, 41)], false))
        .unwrap();
    let selected = await_snapshot(&session, |snapshot| {
        snapshot.display_updated
            && snapshot
                .source_frame
                .is_some_and(|(key, _)| key == source(SOURCE_B))
    });
    assert!(selected.update.replace_all);
    selected.update.apply(&mut displayed);
    assert_eq!((value(&displayed, "X"), value(&displayed, "Y")), (40, 41));
    assert_eq!(selected.source_frame.unwrap().1.ordinal, 1);

    session.set_frame_source(initial.session, Some(source(SOURCE_A)));
    session.set_frame_source(initial.session, Some(source(SOURCE_B)));
    client
        .write_all(&frame(SOURCE_A, 40, &[(4, 51)], false))
        .unwrap();
    client
        .write_all(&frame(SOURCE_B, 50, &[(5, 52)], false))
        .unwrap();
    let selected = await_snapshot(&session, |snapshot| {
        snapshot.display_updated && snapshot.frame == 4
    });
    assert!(selected.update.replace_all);
    assert_eq!(selected.source_frame.unwrap().1.ordinal, 2);
    selected.update.apply(&mut displayed);
    assert_eq!((value(&displayed, "X"), value(&displayed, "Y")), (51, 52));

    drop(client);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let LiveEvent::Complete { result, .. } = next_event(&session, deadline) {
            let recording = result.unwrap();
            assert_eq!(recording.frame_count(), 5);
            assert_eq!(recording.frame_sources.sources().count(), 2);
            assert_eq!(recording.state_at(4).unwrap(), displayed);
            break;
        }
    }
}
