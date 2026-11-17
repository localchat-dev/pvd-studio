use super::*;

#[test]
fn source_controls_from_a_previous_session_do_not_affect_the_replacement() {
    let environment = TestEnvironment::new();
    let (session, output, mut previous) = fixture(&environment);
    feed(
        &output,
        &mut previous,
        frame(SOURCE_A, 10, &[(4, 1), (5, 2)], false),
    );
    session.take_snapshot().unwrap();
    output.begin_frame_session(2);
    assert!(!session.set_frame_source(1, Some(source(SOURCE_B))));
    let selected = *output.frame_source.lock().unwrap();
    assert_eq!(selected.session, Some(2));
    assert_eq!(selected.source, None);
    assert_eq!(selected.revision, 0);

    let mut live = LiveRecording::with_history(HistoryConfig {
        directory: environment.history.clone(),
        min_free_disk_bytes: 0,
        ..Default::default()
    })
    .unwrap();
    live.feed(&initialization()).unwrap();
    let mut replacement = LiveClient {
        session: 2,
        peer: "replacement".into(),
        live: Some(live),
        failure: None,
        raw: None,
        raw_attempted: false,
        received_bytes: 0,
        published_frames: 0,
        published_rooms_revision: 0,
        selection_revision: 0,
        last_publish: Instant::now(),
        profile: None,
    };
    feed(
        &output,
        &mut replacement,
        frame(SOURCE_A, 20, &[(4, 20), (5, 21)], false),
    );
    assert!(!session.set_frame_source(1, Some(source(SOURCE_B))));
    feed(
        &output,
        &mut previous,
        frame(SOURCE_A, 25, &[(4, 99)], false),
    );
    let snapshot = session
        .take_snapshot_for(2)
        .expect("stale control and publication must preserve the replacement snapshot");
    assert!(snapshot.display_updated);
    assert_eq!(snapshot.source_frame.unwrap().0, source(SOURCE_A));
    let mut displayed = SceneState::default();
    snapshot.update.apply(&mut displayed);
    assert_eq!((value(&displayed, "X"), value(&displayed, "Y")), (20, 21));

    assert!(session.set_frame_source(2, Some(source(SOURCE_B))));
    output.sync_frame_source(&mut previous);
    assert_eq!(
        previous.live.as_ref().unwrap().selected_frame_source(),
        Some(source(SOURCE_A))
    );
    feed(
        &output,
        &mut replacement,
        frame(SOURCE_B, 30, &[(4, 30)], true),
    );
    let snapshot = session.take_snapshot_for(2).unwrap();
    assert!(snapshot.display_updated);
    assert!(snapshot.update.replace_all);
    assert_eq!(snapshot.source_frame.unwrap().0, source(SOURCE_B));
    snapshot.update.apply(&mut displayed);
    assert_eq!((value(&displayed, "X"), value(&displayed, "Y")), (30, 21));
    assert!(!session.set_frame_source(1, Some(source(SOURCE_A))));
    assert_eq!(
        output.frame_source.lock().unwrap().source,
        Some(source(SOURCE_B))
    );
}
