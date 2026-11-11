use super::*;

const CYCLES: usize = 10;
const STEPS: usize = 36 * CYCLES;
const HISTORY_BYTES: u64 = 4 << 20;
const RESUME_STEP: usize = 240;

fn check_window(case: &mut Case, selected: &mut FrameSourceKey, sources: [FrameSourceKey; 2]) {
    case.studio.pause_display();
    let (first, last) = case.studio.playback_ordinal_bounds().unwrap();
    assert!(first > 0 && last >= first + 2);
    case.seek_ordinal(first);
    assert!(case.studio.next_playback_frame(-1).is_none());
    case.timed_step(1);
    case.timed_step(-1);
    case.seek_ordinal(last);
    assert!(case.studio.next_playback_frame(1).is_none());
    case.single_step(-1);
    case.single_step(1);
    *selected = if *selected == sources[0] {
        sources[1]
    } else {
        sources[0]
    };
    case.studio.select_frame_source(*selected);
    case.wait("retained paused source selection", |studio| {
        studio.seeking.is_none()
    });
    assert_eq!(case.studio.displayed_frame_source.unwrap().0, *selected);
    case.observe("select_retained_source");
    case.resume();
}

#[test]
#[ignore = "requires official APEX producer, plane32 asset and event probe; runs 360 real SDK TCP steps with 4 MiB history"]
fn official_apex_tcp_rolls_both_sources_and_reopens_the_retained_window() {
    let executable = input("PVD_APEX_CLOTHING_PRODUCER");
    let asset = input("PVD_APEX_CLOTHING_ASSET");
    let probe = input("PVD_APEX_EVENT_PROBE");
    let input_hashes = [digest(&executable), digest(&asset), digest(&probe)];
    let mode = std::env::var("PVD_APEX_TCP_MODE").expect("PVD_APEX_TCP_MODE must be sync or async");
    assert!(matches!(mode.as_str(), "sync" | "async"));
    let asynchronous = mode == "async";
    let mut case = Case::with_history(&format!("rolling-{mode}"), STEPS, HISTORY_BYTES, 4);
    let port = case.studio.server.as_ref().unwrap().address().port();
    case.producer = Some(producer::Producer::start_cycles(
        &executable,
        &asset,
        &case.directory,
        port,
        asynchronous,
        CYCLES,
    ));
    let ready = case.control("PVD_APEX_READY");
    assert_eq!(ready.len(), 3);
    assert!(ready.iter().all(|value| *value > 0));
    let sources = [
        FrameSourceKey::Captured {
            section: FrameSection::PhysX,
            source: ready[0],
        },
        FrameSourceKey::Captured {
            section: FrameSection::Apex,
            source: ready[1],
        },
    ];
    let mut selected = sources[0];
    let mut paused = None;
    let mut paused_evicted = false;
    let mut first_frames = std::collections::BTreeSet::new();
    let mut peak_history_bytes = 0;
    let mut peak_segments = 0;
    let mut retained = Vec::new();
    let mut offsets = Vec::new();
    for step in 0..STEPS {
        if step == 6 || (step > RESUME_STEP && step.is_multiple_of(18)) {
            selected = if selected == sources[0] {
                sources[1]
            } else {
                sources[0]
            };
            case.studio.select_frame_source(selected);
        }
        if step == 10 {
            case.studio.pause_display();
            paused = Some((
                case.studio.displayed_frame_source.unwrap(),
                case.studio.state.clone(),
            ));
        }
        offsets.push(case.step(step, selected));
        let recording = case.studio.timeline_recording().unwrap();
        let stats = recording.disk_usage().unwrap();
        assert!(
            stats.warning.is_none(),
            "history warning: {:?}",
            stats.warning
        );
        assert!(stats.disk_bytes <= HISTORY_BYTES);
        peak_history_bytes = peak_history_bytes.max(stats.disk_bytes);
        peak_segments = peak_segments.max(stats.segments);
        first_frames.insert(stats.first_frame);
        retained.push(serde_json::json!({
            "step": step, "first": stats.first_frame, "frames": stats.retained_frames,
            "segments": stats.segments, "bytes": stats.disk_bytes,
        }));
        if step.is_multiple_of(36) {
            fs::write(
                case.directory.join("retention.json"),
                serde_json::to_vec_pretty(&retained).unwrap(),
            )
            .unwrap();
        }
        if let Some((identity, state)) = &paused {
            assert_eq!(case.studio.displayed_frame_source, Some(*identity));
            assert!(
                case.studio.state == *state,
                "eviction changed the paused display"
            );
            paused_evicted |= !recording.is_frame_retained(identity.1.index);
        }
        if step == RESUME_STEP {
            assert!(
                paused_evicted,
                "the paused endpoint did not cross physical eviction: first={}, frames={}, bytes={}, segments={}",
                stats.first_frame, stats.retained_frames, stats.disk_bytes, stats.segments
            );
            case.resume();
            paused = None;
        }
        if step > RESUME_STEP && step % 36 == 12 {
            check_window(&mut case, &mut selected, sources);
        }
    }
    assert!(first_frames.len() > 4, "history did not roll repeatedly");
    assert!(case.studio.follow_live);
    let final_state = case.studio.state.clone();
    let final_identity = case.studio.displayed_frame_source.unwrap();
    case.finish_producer();
    assert_eq!(case.studio.displayed_frame_source, Some(final_identity));
    assert!(
        case.studio.state == final_state,
        "disconnect changed the displayed endpoint"
    );
    let received = case.studio.capture_path.clone().unwrap();
    let capture = case.directory.join("capture.pvdstream");
    assert!(fs::metadata(&received).unwrap().len() < 96 << 20);
    case.studio.export_path = capture.display().to_string();
    assert!(case.studio.can_save_capture());
    case.studio.save_capture(case.context.clone());
    case.wait("rolling raw save", |studio| studio.save_job.is_none());
    assert_eq!(digest(&received), digest(&capture));
    let profile = case
        .studio
        .server
        .as_ref()
        .unwrap()
        .profile_snapshot()
        .unwrap();
    assert!(profile.completed && !profile.capture_failed);
    assert_eq!(
        profile.decoder_input_bytes,
        fs::metadata(&capture).unwrap().len()
    );
    assert_eq!(profile.received_bytes, Some(profile.decoder_input_bytes));
    case.run_probe(&probe, &capture);
    let bytes = fs::read(&capture).unwrap();
    let fixture = oracle::Fixture::load(&case.directory, &bytes);
    assert_eq!(fixture.sources(), sources);
    assert_eq!(fixture.async_fetch(), asynchronous);
    let debug_bytes = fixture.debug_payload_bytes();
    assert!(
        debug_bytes > 64 << 20,
        "source log reclamation threshold was not crossed"
    );
    let renderer_bytes = fixture.renderer_wire_bytes();
    assert!(renderer_bytes < debug_bytes && renderer_bytes < bytes.len());
    let truth: serde_json::Value =
        serde_json::from_slice(&fs::read(case.directory.join("truth.json")).unwrap()).unwrap();
    assert_eq!(truth["cycles"], CYCLES);
    assert_eq!(truth["transport"], "socket");
    assert_eq!(truth["controlled_steps"], true);
    for (step, written) in offsets.iter().enumerate() {
        assert_eq!(
            truth["steps"][step]["after_getters_bytes"].as_u64(),
            Some(*written)
        );
    }
    for (index, observation) in &case.observed {
        let (source, frame, _) = fixture
            .frames()
            .find(|(_, frame, _)| frame.index == *index)
            .unwrap();
        assert_eq!((observation.source, observation.frame), (source, frame));
        fixture.check_state(*index, &observation.state);
    }
    let recording = case.studio.timeline_recording().unwrap();
    fixture.check_retained_recording(recording);
    let visible: Vec<_> = recording.frames.iter().map(|frame| frame.index).collect();
    let final_history = recording.disk_usage().unwrap();
    assert_eq!(visible.last().copied(), Some(2 * STEPS - 1));
    let archive = case.directory.join("retained.pvdcapture");
    case.studio.retained_path = archive.display().to_string();
    assert!(case.studio.can_save_recording());
    case.studio.save_recording(case.context.clone());
    case.wait("rolling project save", |studio| {
        studio.recording_export.is_none()
    });
    assert!(archive.is_file(), "{:?}", case.studio.notices);
    case.studio.stop_listener();
    case.wait("rolling listener stop", |studio| studio.server.is_none());
    case.studio.close();
    let history = case.directory.join("history");
    case.wait("rolling history release", |_| !has_files(&history));
    case.studio.open(archive.clone(), case.context.clone());
    case.wait("rolling project reopen", |studio| {
        studio.job.is_none() && studio.seeking.is_none()
    });
    assert!(case.studio.is_project_capture());
    let reopened = case.studio.timeline_recording().unwrap();
    assert_eq!(
        reopened
            .frames
            .iter()
            .map(|frame| frame.index)
            .collect::<Vec<_>>(),
        visible
    );
    fixture.check_retained_recording(reopened);
    for source in sources {
        case.studio.select_frame_source(source);
        case.wait("rolling reopened source selection", |studio| {
            studio.seeking.is_none()
        });
        let (first, last) = case.studio.playback_ordinal_bounds().unwrap();
        for ordinal in [first, last, first + 1, last - 1, first] {
            case.seek_ordinal(ordinal);
            assert_eq!(case.studio.displayed_frame_source.unwrap().0, source);
            fixture.check_state(case.studio.frame, &case.studio.state);
        }
    }
    case.studio.close();
    case.wait("rolling reopened history release", |_| !has_files(&history));
    assert_eq!(
        input_hashes,
        [digest(&executable), digest(&asset), digest(&probe)]
    );
    fs::write(
        case.directory.join("retention.json"),
        serde_json::to_vec_pretty(&retained).unwrap(),
    )
    .unwrap();
    fs::write(case.directory.join("result.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "mode": mode, "cycles": CYCLES, "source_frames": [STEPS, STEPS],
        "history_budget_bytes": HISTORY_BYTES, "retained_global_frames": visible,
                "history_sampling": "after each flushed SDK step is decoded",
                "sampled_peak_history_bytes": peak_history_bytes, "sampled_peak_segments": peak_segments,
                "final_history_bytes": final_history.disk_bytes, "final_history_segments": final_history.segments,
                "final_history_first_frame": final_history.first_frame, "final_retained_frames": final_history.retained_frames,
        "capture_bytes": bytes.len(), "capture_blake3": blake3::hash(&bytes).to_hex().to_string(),
                "received_bytes": profile.received_bytes, "decoder_input_bytes": profile.decoder_input_bytes,
                "renderer_wire_bytes": renderer_bytes,
        "captured_debug_payload_bytes": debug_bytes, "sampled_endpoints": case.observed.len(),
        "pause_crossed_eviction": paused_evicted, "history_released": true,
        "producer_blake3": input_hashes[0], "asset_blake3": input_hashes[1], "probe_blake3": input_hashes[2],
        "truth_blake3": digest(&case.directory.join("truth.json")), "archive_blake3": digest(&archive),
        "passed": true,
    })).unwrap()).unwrap();
    println!(
        "APEX {mode} rolling TCP, both source windows, eviction, playback and retained PVDCAP03 verified; artifacts={}",
        case.directory.display()
    );
}
