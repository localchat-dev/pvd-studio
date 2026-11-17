use super::*;

#[test]
fn finishing_a_connected_spool_drains_queued_bytes_before_disconnect() {
    let directory = std::env::temp_dir().join(format!(
        "pvd-finish-connected-{}-{}",
        std::process::id(),
        DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut server = CaptureServer::bind_spooled(
        ListenConfig {
            address: "127.0.0.1:0".parse().unwrap(),
            queue_capacity: 1,
            ..Default::default()
        },
        SpoolOptions {
            directory: directory.clone(),
            segment_bytes: 1024,
            max_backlog_bytes: 64 << 10,
            space_check: None,
        },
        || {},
    )
    .unwrap();
    let mut client = TcpStream::connect(server.address()).unwrap();
    let expected: Vec<_> = (0..12345).map(|index| (index % 251) as u8).collect();
    client.write_all(&expected).unwrap();
    let stats = server.spool_stats().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while stats.snapshot().received_bytes != expected.len() as u64 {
        assert!(
            std::time::Instant::now() < deadline,
            "intake did not commit the prefix"
        );
        thread::sleep(Duration::from_millis(2));
    }
    server.control().request_finish();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !server.control().intake_quiesced() {
        assert!(
            std::time::Instant::now() < deadline,
            "intake did not quiesce"
        );
        thread::sleep(Duration::from_millis(1));
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let attempt = TcpStream::connect_timeout(&server.address(), Duration::from_millis(250));
        match &attempt {
            // Windows may silently time out a loopback SYN to a closed port.
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionRefused | io::ErrorKind::TimedOut
                ) =>
            {
                break;
            }
            _ => {}
        }
        assert!(
            std::time::Instant::now() < deadline,
            "listener still accepts connections while delivery is blocked: {attempt:?}"
        );
        drop(attempt);
        thread::sleep(Duration::from_millis(2));
    }
    let mut actual = Vec::new();
    loop {
        match server
            .receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
        {
            ReceiverEvent::Listening(_) | ReceiverEvent::Connected { .. } => {}
            ReceiverEvent::Data { bytes, .. } => actual.extend(bytes),
            ReceiverEvent::Disconnected { bytes, .. } => {
                assert_eq!(bytes, actual.len() as u64);
                break;
            }
            event => panic!("unexpected finish event: {event:?}"),
        }
    }
    assert_eq!(actual, expected);
    assert!(matches!(
        server.receiver.recv_timeout(Duration::from_secs(5)),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
    ));
    server.stop();
    assert_eq!(stats.snapshot().retained_disk_bytes, 0);
    assert!(fs::read_dir(&directory).unwrap().next().is_none());
    fs::remove_dir(directory).unwrap();
}

#[test]
fn finishing_intake_preserves_committed_backlog_and_terminal_event() {
    let directory = std::env::temp_dir().join(format!(
        "pvd-finish-drain-{}-{}",
        std::process::id(),
        DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let options = SpoolOptions {
        directory: directory.clone(),
        segment_bytes: 1024,
        max_backlog_bytes: 64 << 10,
        space_check: None,
    };
    let stats = Arc::new(SpoolStats {
        connected: AtomicBool::new(true),
        session: AtomicU64::new(1),
        received: AtomicU64::new(0),
        queued: AtomicU64::new(0),
        disk: AtomicU64::new(0),
        limit: options.max_backlog_bytes,
        failure: Mutex::new(None),
        wake: Arc::new(|| {}),
    });
    let owner = Directory::new(&options, stats.clone()).unwrap();
    let queue = Arc::new(Queue::default());
    let cancel = Arc::new(AtomicBool::new(false));
    let finish = Arc::new(AtomicBool::new(false));
    let (output, receiver) = Output::channel(1, cancel.clone(), || {});
    let control = CaptureControl {
        cancel: cancel.clone(),
        finish: finish.clone(),
        intake_quiesced: Arc::new(AtomicBool::new(false)),
        socket: queue.socket.clone(),
        stats: Some(stats.clone()),
        delivery_signal: output.signal.clone(),
    };
    let expected: Vec<_> = (0..12345).map(|index| (index % 251) as u8).collect();
    let mut writer = Writer::new(&owner, &queue, options.segment_bytes, 1).unwrap();
    writer.append(&expected).unwrap();
    stats
        .received
        .store(expected.len() as u64, Ordering::Release);

    control.request_finish();
    control.request_finish();
    assert!(finish.load(Ordering::Acquire));
    assert!(!cancel.load(Ordering::Acquire));
    assert!(!stats.snapshot().connected);
    assert_eq!(stats.snapshot().queued_bytes, expected.len() as u64);
    drop(writer);
    queue
        .push(Item::Event(ReceiverEvent::Disconnected {
            session: 1,
            bytes: expected.len() as u64,
            capture: None,
        }))
        .unwrap();
    queue.state.lock().unwrap().done = true;
    let worker_queue = queue.clone();
    let worker_stats = stats.clone();
    let worker = thread::spawn(move || relay(worker_queue, output, worker_stats));
    let mut actual = Vec::new();
    loop {
        match receiver.recv_timeout(Duration::from_secs(5)).unwrap() {
            ReceiverEvent::Data { session, bytes } => {
                assert_eq!(session, 1);
                actual.extend(bytes);
            }
            ReceiverEvent::Disconnected { session, bytes, .. } => {
                assert_eq!(session, 1);
                assert_eq!(bytes, actual.len() as u64);
                break;
            }
            other => panic!("unexpected finish event: {other:?}"),
        }
    }
    worker.join().unwrap();
    assert_eq!(actual, expected);
    assert_eq!(stats.snapshot().queued_bytes, 0);
    assert_eq!(stats.snapshot().retained_disk_bytes, 0);
    drop(control);
    drop(queue);
    drop(owner);
    assert!(fs::read_dir(&directory).unwrap().next().is_none());
    fs::remove_dir(directory).unwrap();
}

#[test]
fn cancelling_after_finish_still_wakes_a_blocked_delivery() {
    let cancel = Arc::new(AtomicBool::new(false));
    let (output, receiver) = Output::channel(1, cancel.clone(), || {});
    let control = CaptureControl {
        cancel: cancel.clone(),
        finish: Arc::new(AtomicBool::new(false)),
        intake_quiesced: Arc::new(AtomicBool::new(false)),
        socket: Arc::new(Mutex::new(None)),
        stats: None,
        delivery_signal: output.signal.clone(),
    };
    assert!(output.send(ReceiverEvent::Listening("127.0.0.1:0".parse().unwrap())));
    control.request_finish();
    assert!(!cancel.load(Ordering::Acquire));
    let (done, completed) = std::sync::mpsc::channel();
    let worker = thread::spawn(move || {
        done.send(output.send(ReceiverEvent::Listening("127.0.0.1:0".parse().unwrap())))
            .unwrap();
    });
    control.request_stop();
    assert!(!completed.recv_timeout(Duration::from_secs(5)).unwrap());
    worker.join().unwrap();
    drop(receiver);
}

#[test]
fn quiescence_waits_for_active_spool_writes_and_closes_partial_raw_capture() {
    let directory = std::env::temp_dir().join(format!(
        "pvd-quiescence-{}-{}",
        std::process::id(),
        DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let raw_directory = directory.join("raw");
    let receive_directory = directory.join("receive");
    let (entered, writing) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let released = Mutex::new(released);
    let mut server = CaptureServer::bind_spooled(
        ListenConfig {
            address: "127.0.0.1:0".parse().unwrap(),
            capture_directory: Some(raw_directory.clone()),
            queue_capacity: 1,
            ..Default::default()
        },
        SpoolOptions {
            directory: receive_directory.clone(),
            segment_bytes: 4096,
            max_backlog_bytes: 64 << 10,
            space_check: Some(Arc::new(move |_| {
                entered.send(()).unwrap();
                released
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
                Ok(())
            })),
        },
        || {},
    )
    .unwrap();
    let mut client = TcpStream::connect(server.address()).unwrap();
    client.write_all(b"partial raw capture").unwrap();
    writing.recv_timeout(Duration::from_secs(5)).unwrap();
    let control = server.control();
    control.request_stop();
    assert!(
        !control.intake_quiesced(),
        "an active write is still real work"
    );
    release.send(()).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !control.intake_quiesced() {
        assert!(
            std::time::Instant::now() < deadline,
            "writers did not close"
        );
        thread::sleep(Duration::from_millis(1));
    }
    let raw = fs::read_dir(&raw_directory)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(raw.extension().unwrap(), "partial");
    assert_eq!(fs::read(&raw).unwrap(), b"partial raw capture");
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Sharing zero fails if the receiver still owns its read/write handle.
        drop(
            OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&raw)
                .unwrap(),
        );
    }
    server.stop();
    assert!(fs::read_dir(&receive_directory).unwrap().next().is_none());
    fs::remove_file(raw).unwrap();
    fs::remove_dir(raw_directory).unwrap();
    fs::remove_dir(receive_directory).unwrap();
    fs::remove_dir(directory).unwrap();
}

#[test]
fn normal_eof_publishes_idle_before_cleanup_and_reconnect_clears_it_before_writing() {
    let directory = std::env::temp_dir().join(format!(
        "pvd-quiescence-reconnect-{}-{}",
        std::process::id(),
        DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let (entered, writing) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let released = Mutex::new(released);
    let writes = AtomicU64::new(0);
    let mut server = CaptureServer::bind_spooled(
        ListenConfig {
            address: "127.0.0.1:0".parse().unwrap(),
            queue_capacity: 1,
            ..Default::default()
        },
        SpoolOptions {
            directory: directory.clone(),
            segment_bytes: 4096,
            max_backlog_bytes: 64 << 10,
            space_check: Some(Arc::new(move |_| {
                if writes.fetch_add(1, Ordering::Relaxed) == 1 {
                    entered.send(()).unwrap();
                    released
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))
                        .unwrap();
                }
                Ok(())
            })),
        },
        || {},
    )
    .unwrap();
    let control = server.control();
    let mut first = TcpStream::connect(server.address()).unwrap();
    first.write_all(b"first").unwrap();
    drop(first);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !control.intake_quiesced() {
        assert!(
            std::time::Instant::now() < deadline,
            "normal EOF did not publish idle"
        );
        thread::sleep(Duration::from_millis(1));
    }
    assert!(server.spool_stats().unwrap().snapshot().queued_bytes > 0);
    // The first stream is still owned by the blocked relay. A subsequent stop
    // can recognize idle without waiting for that backlog's eventual deletion.
    let mut second = TcpStream::connect(server.address()).unwrap();
    second.write_all(b"second").unwrap();
    writing.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(
        !control.intake_quiesced(),
        "reconnect must revoke idle before writing"
    );
    control.request_stop();
    assert!(!control.intake_quiesced());
    release.send(()).unwrap();
    server.stop();
    assert!(control.intake_quiesced());
    assert!(fs::read_dir(&directory).unwrap().next().is_none());
    fs::remove_dir(directory).unwrap();
}
