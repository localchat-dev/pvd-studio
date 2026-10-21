use super::*;

fn check_scene(
    fixture: &Fixture,
    frame: usize,
    replay: SceneReplayFrame,
    displayed: &mut SceneState,
) {
    assert_eq!(replay.frame, frame);
    replay.update.apply(displayed);
    assert_eq!(*displayed, fixture.scenes[frame]);
}

#[test]
fn scene_only_and_full_seeks_share_the_scene_but_track_analysis_independently() {
    let fixture = Fixture::new(14, 4, true, false);
    let mut cursor = ReplayCursor::default();
    let mut displayed = SceneState::default();
    for (full, frame) in [
        (false, 0),
        (false, 1),
        (true, 1),
        (false, 2),
        (false, 3),
        (true, 3),
        (false, 8),
        (false, 7),
        (true, 7),
        (false, 13),
        (false, 4),
        (true, 4),
        (false, 5),
        (true, 5),
        (false, 9),
        (false, 10),
        (false, 0),
        (true, 0),
    ] {
        if full {
            let replay = cursor.seek(&fixture.recording, frame, || false).unwrap();
            fixture.check(frame, &replay, &mut displayed);
            assert_eq!(
                cursor
                    .current
                    .as_ref()
                    .unwrap()
                    .analysis
                    .as_ref()
                    .unwrap()
                    .frame,
                frame
            );
        } else {
            let analysis_frame = cursor
                .current
                .as_ref()
                .and_then(|cursor| cursor.analysis.as_ref())
                .map(|analysis| analysis.frame);
            let replay = cursor
                .seek_scene(&fixture.recording, frame, || false)
                .unwrap();
            check_scene(&fixture, frame, replay, &mut displayed);
            assert_eq!(
                cursor
                    .current
                    .as_ref()
                    .unwrap()
                    .analysis
                    .as_ref()
                    .map(|analysis| analysis.frame),
                analysis_frame
            );
        }
    }
}

#[test]
fn scene_only_does_not_read_or_cache_analysis_baselines_and_full_seek_still_validates_them() {
    let mut fixture = Fixture::new(14, 4, true, false);
    let disk = fixture.recording.disk.as_mut().unwrap();
    let original = disk.segments.clone();
    for segment in Arc::make_mut(&mut disk.segments) {
        Arc::make_mut(segment).analysis_base = 0;
    }
    let mut cursor = ReplayCursor::default();
    let mut displayed = SceneState::default();
    let empty_cache = fixture
        .recording
        .disk
        .as_ref()
        .unwrap()
        .analysis_cache
        .stats();
    assert_eq!(empty_cache.0, 0);
    for frame in (0..14).chain((0..14).rev()) {
        let replay = cursor
            .seek_scene(&fixture.recording, frame, || false)
            .unwrap();
        check_scene(&fixture, frame, replay, &mut displayed);
        assert!(cursor.current.as_ref().unwrap().analysis.is_none());
        assert_eq!(
            fixture
                .recording
                .disk
                .as_ref()
                .unwrap()
                .analysis_cache
                .stats(),
            empty_cache
        );
    }
    assert!(cursor.seek(&fixture.recording, 0, || false).is_err());
    assert!(cursor.current.is_none());
    fixture.recording.disk.as_mut().unwrap().segments = original;
    let (replay, metrics) = cursor
        .seek_profiled(&fixture.recording, 0, || false)
        .unwrap();
    assert_eq!(metrics.baseline_loads, 1);
    assert!(replay.update.replace_all);
    fixture.check(0, &replay, &mut displayed);
}

#[test]
fn same_scene_frame_can_request_missing_analysis_and_then_reuse_the_complete_result() {
    let fixture = Fixture::new(14, 4, true, false);
    let mut cursor = ReplayCursor::default();
    let mut displayed = SceneState::default();
    let replay = cursor.seek_scene(&fixture.recording, 9, || false).unwrap();
    check_scene(&fixture, 9, replay, &mut displayed);
    let (replay, metrics) = cursor
        .seek_profiled(&fixture.recording, 9, || false)
        .unwrap();
    assert!(!replay.update.replace_all);
    assert!(replay.update.objects.is_empty());
    assert_eq!(metrics.baseline_loads, 1);
    fixture.check(9, &replay, &mut displayed);
    let (replay, metrics) = cursor
        .seek_profiled(&fixture.recording, 9, || false)
        .unwrap();
    assert_eq!(metrics.baseline_loads, 0);
    assert_eq!(metrics.analysis_frames, 0);
    fixture.check(9, &replay, &mut displayed);
}

#[test]
fn scene_only_final_cancellation_discards_cursor_and_retry_replaces_the_scene() {
    let fixture = Fixture::new(6, 2, true, false);
    let mut cursor = ReplayCursor::default();
    let mut displayed = SceneState::default();
    let replay = cursor.seek_scene(&fixture.recording, 3, || false).unwrap();
    check_scene(&fixture, 3, replay, &mut displayed);
    let mut calls = 0;
    assert!(
        cursor
            .seek_scene(&fixture.recording, 3, || {
                calls += 1;
                calls == 2
            })
            .is_err()
    );
    assert_eq!(calls, 2);
    assert!(cursor.current.is_none());
    let replay = cursor.seek_scene(&fixture.recording, 4, || false).unwrap();
    assert!(replay.update.replace_all);
    check_scene(&fixture, 4, replay, &mut displayed);
    let replay = cursor.seek(&fixture.recording, 4, || false).unwrap();
    fixture.check(4, &replay, &mut displayed);
}

#[test]
fn scene_only_expiry_and_record_metadata_checks_remain_in_force() {
    let mut fixture = Fixture::new(6, 2, true, false);
    let mut cursor = ReplayCursor::default();
    let mut displayed = SceneState::default();
    let replay = cursor.seek_scene(&fixture.recording, 0, || false).unwrap();
    check_scene(&fixture, 0, replay, &mut displayed);
    assert!(fixture.history.evict_one().unwrap());
    assert!(cursor.seek_scene(&fixture.recording, 1, || false).is_err());
    assert_eq!(cursor.current.as_ref().unwrap().frame, Some(0));
    let replay = cursor.seek_scene(&fixture.recording, 2, || false).unwrap();
    check_scene(&fixture, 2, replay, &mut displayed);
    let original = fixture.recording.disk.as_ref().unwrap().segments.clone();
    let disk = fixture.recording.disk.as_mut().unwrap();
    Arc::make_mut(&mut Arc::make_mut(&mut disk.segments)[1]).frames[1]
        .info
        .timestamp += 1;
    assert!(cursor.seek_scene(&fixture.recording, 3, || false).is_err());
    assert!(cursor.current.is_none());
    fixture.recording.disk.as_mut().unwrap().segments = original;
    let replay = cursor.seek(&fixture.recording, 3, || false).unwrap();
    assert!(replay.update.replace_all);
    fixture.check(3, &replay, &mut displayed);
}

#[test]
fn switching_recordings_replaces_scene_and_does_not_transfer_analysis() {
    let first = Fixture::new(6, 3, true, false);
    let second = Fixture::new(6, 3, true, false);
    let mut cursor = ReplayCursor::default();
    let mut displayed = SceneState::default();
    let replay = cursor.seek(&first.recording, 3, || false).unwrap();
    first.check(3, &replay, &mut displayed);
    let replay = cursor.seek_scene(&second.recording, 3, || false).unwrap();
    assert!(replay.update.replace_all);
    check_scene(&second, 3, replay, &mut displayed);
    assert!(cursor.current.as_ref().unwrap().analysis.is_none());
    let replay = cursor.seek(&second.recording, 3, || false).unwrap();
    second.check(3, &replay, &mut displayed);
    let replay = cursor.seek_scene(&first.recording, 3, || false).unwrap();
    assert!(replay.update.replace_all);
    check_scene(&first, 3, replay, &mut displayed);
}

#[test]
fn rejected_requests_before_take_preserve_the_unmodified_cursor() {
    let mut fixture = Fixture::new(6, 2, true, false);
    let mut cursor = ReplayCursor::default();
    let mut displayed = SceneState::default();
    let replay = cursor.seek(&fixture.recording, 0, || false).unwrap();
    fixture.check(0, &replay, &mut displayed);
    assert!(cursor.seek(&fixture.recording, 1, || true).is_err());
    assert_eq!(cursor.current.as_ref().unwrap().frame, Some(0));
    assert!(cursor.seek_scene(&fixture.recording, 1, || true).is_err());
    assert_eq!(cursor.current.as_ref().unwrap().frame, Some(0));
    assert!(
        cursor
            .seek_scene(&fixture.recording, 100, || false)
            .is_err()
    );
    assert_eq!(cursor.current.as_ref().unwrap().frame, Some(0));
    assert!(fixture.history.evict_one().unwrap());
    assert!(cursor.seek(&fixture.recording, 1, || false).is_err());
    assert_eq!(cursor.current.as_ref().unwrap().frame, Some(0));
    let replay = cursor.seek_scene(&fixture.recording, 2, || false).unwrap();
    check_scene(&fixture, 2, replay, &mut displayed);
    let replay = cursor.seek(&fixture.recording, 2, || false).unwrap();
    fixture.check(2, &replay, &mut displayed);
}
