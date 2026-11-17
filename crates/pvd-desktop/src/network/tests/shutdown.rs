use super::*;

fn wait_until(mut ready: impl FnMut() -> bool, timeout: Duration, message: &str) {
    let deadline = Instant::now() + timeout;
    while !ready() {
        assert!(Instant::now() < deadline, "{message}");
        std::thread::park_timeout(Duration::from_millis(1));
    }
}

fn committed_backlog(environment: &TestEnvironment) -> (NetworkSession, TcpStream, usize) {
    let session = environment
        .start_with_options(
            ListenConfig {
                address: "127.0.0.1:0".parse().unwrap(),
                queue_capacity: 1,
                ..Default::default()
            },
            NetworkOptions {
                progress_only: true,
                ..Default::default()
            },
            egui::Context::default(),
        )
        .unwrap();
    // The empty first connection fills the four-event UI channel, blocking the
    // indexer while the independent receive thread commits the second stream.
    drop(TcpStream::connect(session.address()).unwrap());
    wait_until(
        || {
            session
                .profile_snapshot()
                .is_some_and(|profile| profile.completed)
        },
        Duration::from_secs(5),
        "empty connection did not complete",
    );
    let mut bytes = empty_stream_header();
    for timestamp in 0..256 {
        bytes.extend(frame_group(timestamp));
    }
    let mut client = TcpStream::connect(session.address()).unwrap();
    client.write_all(&bytes).unwrap();
    wait_until(
        || {
            session.receive_status().is_some_and(|receive| {
                receive.session == 2
                    && receive.received_bytes == bytes.len() as u64
                    && receive.queued_bytes > 0
            })
        },
        Duration::from_secs(5),
        "second stream was not committed to the receive spool",
    );
    assert!(session.receive_status().unwrap().queued_bytes > 0);
    assert_eq!(session.profile_snapshot().unwrap().session, 1);
    (session, client, bytes.len())
}

#[test]
fn closing_a_recording_drains_committed_tcp_backlog_without_cancelling_decoding() {
    let environment = TestEnvironment::new();
    let (session, client, length) = committed_backlog(&environment);
    session.request_stop();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut completed = false;
    loop {
        match next_event(&session, deadline) {
            LiveEvent::Complete {
                session: id,
                received_bytes,
                result,
                ..
            } => {
                assert_eq!(id, 2);
                assert_eq!(received_bytes, length as u64);
                assert_eq!(result.unwrap().frame_count(), 256);
                completed = true;
            }
            LiveEvent::Error(message) => {
                panic!("{}", message.text(crate::i18n::Language::English));
            }
            LiveEvent::Stopped => break,
            _ => {}
        }
    }
    assert!(completed, "closing must retain the committed recording");
    assert_eq!(session.take_snapshot_for(2).unwrap().frame, 255);
    assert_eq!(session.receive_status().unwrap().queued_bytes, 0);
    drop(client);
    drop(session);
}

#[test]
fn exiting_with_committed_tcp_backlog_does_not_wait_for_replay_or_emit_a_decode_error() {
    let environment = TestEnvironment::new();
    let (session, client, _) = committed_backlog(&environment);
    wait_until(
        || environment.jobs.cancel_all_and_check_finished(),
        Duration::from_secs(5),
        "app exit waited for an undrained UI channel or receive backlog",
    );
    for event in session.receiver.lock().unwrap().try_iter() {
        assert!(
            !matches!(event, LiveEvent::Complete { .. } | LiveEvent::Error(_)),
            "app cancellation must not finalize the unread recording or report decoding failure"
        );
    }
    let profile = session.profile_snapshot().unwrap();
    assert_eq!(
        profile.session, 1,
        "cancelled backlog must not start a new decoder"
    );
    assert!(!profile.capture_failed);
    assert_eq!(session.receive_status().unwrap().queued_bytes, 0);
    drop(client);
    drop(session);
}

#[test]
#[ignore = "set PVD_EXIT_SEAL_CAPTURE to a real PVD stream containing a full persistent bank"]
fn real_tcp_exit_interrupts_an_in_flight_history_seal() {
    let capture = std::env::var_os("PVD_EXIT_SEAL_CAPTURE")
        .expect("set PVD_EXIT_SEAL_CAPTURE to the real capture fixture path");
    let input = fs::read(&capture).unwrap();
    let environment = TestEnvironment::new();
    let session = environment
        .start_with_options(
            ListenConfig {
                address: "127.0.0.1:0".parse().unwrap(),
                ..Default::default()
            },
            NetworkOptions {
                progress_only: true,
                ..Default::default()
            },
            egui::Context::default(),
        )
        .unwrap();
    let address = session.address();
    let producer = std::thread::spawn(move || {
        let mut client = TcpStream::connect(address).unwrap();
        // An exit may stop the socket before all fixture bytes have arrived.
        client.write_all(&input)
    });
    let deadline = Instant::now() + Duration::from_secs(180);
    let sealing = loop {
        assert!(
            Instant::now() < deadline,
            "capture never entered a history seal"
        );
        for event in session.receiver.lock().unwrap().try_iter() {
            match event {
                LiveEvent::Error(message) => {
                    panic!("{}", message.text(crate::i18n::Language::English));
                }
                LiveEvent::Complete { .. } => {
                    panic!("capture ended before observing a history seal")
                }
                _ => {}
            }
        }
        session.take_snapshot();
        if let Some(profile) = session.profile_snapshot()
            && profile.metrics.stages.iter().any(|stage| {
                stage.stage == pvd_core::metrics::Stage::HistorySeal && stage.in_flight > 0
            })
        {
            break profile;
        }
        std::thread::park_timeout(Duration::from_millis(1));
    };
    let stopped_at = Instant::now();
    wait_until(
        || environment.jobs.cancel_all_and_check_finished(),
        Duration::from_secs(5),
        "history seal did not respond to app exit within five seconds",
    );
    let elapsed = stopped_at.elapsed();
    let profile = session.profile_snapshot().unwrap();
    assert!(
        !profile.capture_failed,
        "requested cancellation is not decoding failure"
    );
    assert!(
        !profile.completed,
        "cancelled recording must not report normal completion"
    );
    assert!(
        profile
            .metrics
            .stages
            .iter()
            .all(|stage| stage.in_flight == 0)
    );
    assert_eq!(
        profile.metrics.last_committed_frame, sealing.metrics.last_committed_frame,
        "app exit must interrupt this seal before another frame is committed"
    );
    for event in session.receiver.lock().unwrap().try_iter() {
        assert!(!matches!(
            event,
            LiveEvent::Complete { .. } | LiveEvent::Error(_)
        ));
    }
    eprintln!(
        "TCP seal cancellation: committed_frame={:?}, decoder_bytes={}, queued_bytes={:?}, worker_stop_ms={:.3}",
        sealing.metrics.last_committed_frame,
        sealing.decoder_input_bytes,
        sealing.queued_bytes,
        elapsed.as_secs_f64() * 1000.0,
    );
    let _ = producer.join().unwrap();
    drop(session);
}
