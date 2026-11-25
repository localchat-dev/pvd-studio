use super::*;
use crate::{CaptureServer, ListenConfig, ReceiverEvent};
use std::fs;
use std::io::Write;
use std::net::{Shutdown, TcpStream};

const WAIT: Duration = Duration::from_secs(5);

fn server(
    address: SocketAddr,
    viewer: Viewer,
    late_join: Option<LateJoinStorage>,
) -> CaptureServer {
    CaptureServer::bind_with_sharing(
        ListenConfig {
            address,
            ..Default::default()
        },
        Some(ShareOptions { viewer, late_join }),
        || {},
    )
    .unwrap()
}

fn event(server: &CaptureServer) -> ReceiverEvent {
    server.receiver.recv_timeout(WAIT).unwrap()
}

fn drain(server: &CaptureServer) -> (u64, Vec<u8>) {
    let mut data = Vec::new();
    loop {
        match event(server) {
            ReceiverEvent::Data { bytes, .. } => data.extend(bytes),
            ReceiverEvent::Disconnected { bytes, session, .. } => {
                assert_eq!(bytes, data.len() as u64);
                return (session, data);
            }
            ReceiverEvent::Failed { message, .. } => panic!("{message}"),
            _ => (),
        }
    }
}

fn role(server: &CaptureServer, expected: ListenerRole) {
    let deadline = Instant::now() + WAIT;
    while server.control().role() != expected {
        assert!(
            Instant::now() < deadline,
            "role {:?}",
            server.control().role()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn shared_order_and_active_takeover_with_both_startup_orders() {
    for (first, second) in [
        (Viewer::Desktop, Viewer::Unity),
        (Viewer::Unity, Viewer::Desktop),
    ] {
        let mut owner = server("127.0.0.1:0".parse().unwrap(), first, None);
        let address = owner.address();
        let mut subscriber = server(address, second, None);
        role(&subscriber, ListenerRole::Shared(first));
        let mut client = TcpStream::connect(address).unwrap();
        let peer = client.local_addr().unwrap();
        for endpoint in [&owner, &subscriber] {
            assert!(matches!(event(endpoint), ReceiverEvent::Listening(_)));
            assert!(
                matches!(event(endpoint), ReceiverEvent::Connected { session: 1, peer: p } if p == peer)
            );
        }
        client.write_all(b"first").unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        assert_eq!(drain(&owner), (1, b"first".to_vec()));
        assert_eq!(drain(&subscriber), (1, b"first".to_vec()));

        let mut client = TcpStream::connect(address).unwrap();
        client.write_all(b"interrupted").unwrap();
        assert!(matches!(
            event(&subscriber),
            ReceiverEvent::Connected { session: 2, .. }
        ));
        assert!(
            matches!(event(&subscriber), ReceiverEvent::Data { bytes, .. } if bytes == b"interrupted")
        );
        owner.stop();
        assert!(matches!(
            event(&subscriber),
            ReceiverEvent::Failed {
                session: Some(2),
                ..
            }
        ));
        role(&subscriber, ListenerRole::Listening);
        let mut client = TcpStream::connect(address).unwrap();
        client.write_all(b"new connection").unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        assert_eq!(drain(&subscriber), (3, b"new connection".to_vec()));
        subscriber.stop();
    }
}

#[test]
fn late_join_storage_replays_the_prefix_or_waits_for_a_new_connection() {
    let temp = std::env::temp_dir().join(format!(
        "pvd-share-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    for storage in [
        None,
        Some(LateJoinStorage::Memory),
        Some(LateJoinStorage::Disk(temp.clone())),
    ] {
        let late = storage.is_some();
        let disk = matches!(storage, Some(LateJoinStorage::Disk(_)));
        let mut owner = server("127.0.0.1:0".parse().unwrap(), Viewer::Unity, storage);
        let address = owner.address();
        let mut source = TcpStream::connect(address).unwrap();
        assert!(matches!(event(&owner), ReceiverEvent::Listening(_)));
        assert!(matches!(event(&owner), ReceiverEvent::Connected { .. }));
        source.write_all(b"prefix").unwrap();
        assert!(matches!(event(&owner), ReceiverEvent::Data { bytes, .. } if bytes == b"prefix"));
        let mut subscriber = server(address, Viewer::Desktop, None);
        role(
            &subscriber,
            if late {
                ListenerRole::Shared(Viewer::Unity)
            } else {
                ListenerRole::WaitingForSession(Viewer::Unity)
            },
        );
        source.write_all(b"-tail").unwrap();
        source.shutdown(Shutdown::Write).unwrap();
        assert!(matches!(event(&owner), ReceiverEvent::Data { bytes, .. } if bytes == b"-tail"));
        assert!(matches!(
            event(&owner),
            ReceiverEvent::Disconnected { bytes: 11, .. }
        ));
        if late {
            assert_eq!(drain(&subscriber), (1, b"prefix-tail".to_vec()));
        } else {
            assert!(matches!(event(&subscriber), ReceiverEvent::Listening(_)));
            assert!(matches!(
                subscriber.receiver.recv_timeout(Duration::from_millis(100)),
                Err(RecvTimeoutError::Timeout)
            ));
        }
        let mut source = TcpStream::connect(address).unwrap();
        source.write_all(b"next").unwrap();
        source.shutdown(Shutdown::Write).unwrap();
        assert_eq!(drain(&owner), (2, b"next".to_vec()));
        assert_eq!(
            drain(&subscriber),
            (if late { 2 } else { 1 }, b"next".to_vec())
        );
        subscriber.stop();
        owner.stop();
        if disk {
            let deadline = Instant::now() + WAIT;
            while fs::read_dir(&temp).unwrap().next().is_some() {
                assert!(
                    Instant::now() < deadline,
                    "prefix files survived both listeners"
                );
                thread::sleep(Duration::from_millis(10));
            }
            fs::remove_dir(&temp).unwrap();
        } else {
            assert!(!temp.exists());
        }
    }
}

#[test]
fn stalled_shared_consumer_cannot_stop_intake_or_rejoin_a_partial_stream() {
    let owner = server("127.0.0.1:0".parse().unwrap(), Viewer::Desktop, None);
    let address = owner.address();
    let subscriber = server(address, Viewer::Unity, None);
    let send = thread::spawn(move || {
        let mut socket = TcpStream::connect(address).unwrap();
        socket.set_write_timeout(Some(WAIT)).unwrap();
        let bytes = [17; CHUNK_SIZE];
        for _ in 0..QUEUE_BYTES / CHUNK_SIZE + 128 {
            socket.write_all(&bytes).unwrap();
        }
        socket
    });
    let mut received = 0;
    let total = QUEUE_BYTES + 128 * CHUNK_SIZE;
    while received < total {
        match event(&owner) {
            ReceiverEvent::Data { bytes, .. } => received += bytes.len(),
            ReceiverEvent::Failed { message, .. } => panic!("{message}"),
            _ => (),
        }
    }
    let socket = send.join().unwrap();
    loop {
        match event(&subscriber) {
            ReceiverEvent::Failed {
                session: Some(1), ..
            } => break,
            ReceiverEvent::Disconnected { .. } => {
                panic!("incomplete follower stream ended normally")
            }
            _ => (),
        }
    }
    role(
        &subscriber,
        ListenerRole::WaitingForSession(Viewer::Desktop),
    );
    socket.shutdown(Shutdown::Write).unwrap();
    assert!(
        matches!(event(&owner), ReceiverEvent::Disconnected { bytes, .. } if bytes == total as u64)
    );

    let mut socket = TcpStream::connect(address).unwrap();
    socket.write_all(b"fresh").unwrap();
    socket.shutdown(Shutdown::Write).unwrap();
    assert_eq!(drain(&owner), (2, b"fresh".to_vec()));
    assert_eq!(drain(&subscriber), (2, b"fresh".to_vec()));
}

#[test]
fn unrelated_tcp_listener_is_a_port_conflict() {
    let outsider = TcpListener::bind("127.0.0.1:0").unwrap();
    let result = CaptureServer::bind_with_sharing(
        ListenConfig {
            address: outsider.local_addr().unwrap(),
            ..Default::default()
        },
        Some(ShareOptions {
            viewer: Viewer::Desktop,
            late_join: None,
        }),
        || {},
    );
    assert!(result.err().is_some_and(|error| busy(&error)));
}
