use super::Sessions;
use std::{
    fs,
    io::Write,
    net::TcpStream,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

mod replay;

#[test]
fn live_requests_keep_only_the_latest_complete_frame_and_discard_it_on_disconnect() {
    use serde_json::{Value, json};

    fn request(value: Value) -> Result<Value, String> {
        crate::dispatch(serde_json::from_value(value).unwrap())
    }
    fn status() -> Value {
        request(json!({"op":"status"})).unwrap()["status"].clone()
    }
    fn wait_status(test: impl Fn(&Value) -> bool) -> Value {
        let start = Instant::now();
        loop {
            let value = status();
            if test(&value) {
                return value;
            }
            assert!(start.elapsed() < Duration::from_secs(10), "{value}");
            thread::sleep(Duration::from_millis(10));
        }
    }
    fn frame(id: u64, index: u64, room: u64, base: u64) -> Option<Vec<u8>> {
        let mut length = 0;
        let data = unsafe {
            crate::pvd_frame_queries(
                id,
                index,
                room,
                base,
                1,
                0,
                0,
                0.,
                0.,
                0.,
                10.,
                1,
                &mut length,
            )
        };
        if data.is_null() {
            assert_eq!(length, 0);
            return None;
        }
        let bytes = unsafe { std::slice::from_raw_parts(data, length as usize).to_vec() };
        unsafe { crate::pvd_frame_free(data, length) };
        Some(bytes)
    }

    struct ResetBridge;
    impl Drop for ResetBridge {
        fn drop(&mut self) {
            request(json!({"op":"shutdown"})).unwrap();
        }
    }
    let _reset = ResetBridge;
    let parent = std::env::temp_dir().join(format!(
        "pvd-unity-latest-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let root = parent.join("PvdStudio");
    request(json!({"op":"start", "root":root, "address":"127.0.0.1:0", "recordHistory":false}))
        .unwrap();
    let address = status()["address"].as_str().unwrap().to_string();
    let mut sender = TcpStream::connect(&address).unwrap();
    let mut events = Vec::new();
    for (handle, name) in [(1u32, "frame"), (2, "physx3"), (3, "PxScene")] {
        events.push(1);
        events.extend((name.len() as u32 + 1).to_le_bytes());
        events.extend(name.as_bytes());
        events.push(0);
        events.extend(handle.to_le_bytes());
    }
    events.push(6);
    events.extend(2u32.to_le_bytes());
    events.extend(3u32.to_le_bytes());
    events.extend(7u64.to_le_bytes());
    sender.write_all(&stream_header()).unwrap();
    sender.write_all(&frame_group(0, events, 4)).unwrap();
    for stamp in 1..3 {
        sender
            .write_all(&frame_group(stamp, Vec::new(), 0))
            .unwrap();
    }
    let state = wait_status(|s| s["sessions"][0]["frames"] == 3);
    assert_eq!(state["sessions"][0]["liveOnly"], true);
    assert_eq!(state["sessions"][0]["rooms"], json!([{"number":1}]));
    assert!(!root.join("receive").exists());
    assert!(!root.join("history").exists());
    assert_eq!(state["totalBytes"], 0);
    assert_eq!(state["clearableBytes"], 0);
    let id = state["active"].as_u64().unwrap();
    let data = frame(id, u64::MAX, 1, 0).unwrap();
    assert!(data.starts_with(b"PVS5"));
    let token = u64::from_le_bytes(data[4..12].try_into().unwrap());
    let next = frame(id, 2, 1, token).unwrap();
    assert_eq!(&next[12..20], &token.to_le_bytes());
    assert!(frame(id, 1, 0, 0).is_none());
    assert!(status()["frameError"].as_str().unwrap().contains("latest"));
    assert_eq!(crate::pvd_frame_count(id), 3);

    let mut groups = Vec::new();
    for stamp in 3..131u64 {
        groups.extend(frame_group(stamp, Vec::new(), 0));
    }
    let mut destroy_scene = vec![15];
    destroy_scene.extend(7u64.to_le_bytes());
    groups.extend(frame_group(131, destroy_scene, 1));
    sender.write_all(&groups[..31]).unwrap();
    assert_eq!(status()["sessions"][0]["frames"], 3);
    assert!(frame(id, u64::MAX, 1, 0).is_some());
    sender.write_all(&groups[31..]).unwrap();
    let state = wait_status(|s| s["sessions"][0]["frames"] == 132);
    assert_eq!(state["sessions"][0]["rooms"], json!([]));
    assert!(frame(id, 2, 0, 0).is_none());
    assert!(frame(id, u64::MAX, 0, 0).is_some());
    assert_eq!(request(json!({"op":"clear"})).unwrap()["released"], 0);
    let export_path = parent.join("live.pvdcapture");
    assert!(
        request(json!({"op":"export", "session":id, "path":export_path}))
            .unwrap_err()
            .contains("live")
    );
    assert!(!export_path.exists());

    let mut pending = crate::BRIDGE
        .lock()
        .unwrap()
        .sessions
        .prepare_frame(id)
        .unwrap();
    drop(sender);
    wait_status(|s| s["active"] == 0);
    assert!(
        pending
            .read(u64::MAX, 0, super::FrameFormat::Inline)
            .is_err()
    );
    assert!(
        !crate::BRIDGE
            .lock()
            .unwrap()
            .sessions
            .finish_frame(&mut pending, true)
    );
    assert_eq!(status()["sessions"], json!([]));
    assert_eq!(crate::pvd_frame_count(id), -1);
    assert!(frame(id, u64::MAX, 0, 0).is_none());
    for op in ["stop", "shutdown"] {
        let mut sender = TcpStream::connect(&address).unwrap();
        sender.write_all(&frames()).unwrap();
        let state = wait_status(|s| s["sessions"][0]["frames"] == 3);
        assert!(state["active"].as_u64().unwrap() > id);
        request(json!({"op":op})).unwrap();
        assert_eq!(status()["sessions"], json!([]));
        assert!(TcpStream::connect(&address).is_err());
        drop(sender);
        if op == "stop" {
            request(json!({"op":"start", "root":root, "address":address, "recordHistory":false}))
                .unwrap();
        }
    }

    request(json!({"op":"start", "root":root, "address":address})).unwrap();
    let mut sender = TcpStream::connect(&address).unwrap();
    sender.write_all(&frames()).unwrap();
    drop(sender);
    let state = wait_status(|s| s["active"] == 0 && s["sessions"][0]["frames"] == 3);
    assert_ne!(state["sessions"][0]["liveOnly"], true);
    assert!(state["totalBytes"].as_u64().unwrap() > 0);
    let id = state["sessions"][0]["id"].as_u64().unwrap();
    assert!(frame(id, 0, 0, 0).is_some());
    request(json!({"op":"shutdown"})).unwrap();
    fs::remove_dir_all(parent).unwrap();
}

fn file_sizes(path: &std::path::Path) -> u64 {
    fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();
            if metadata.is_dir() {
                file_sizes(&entry.path())
            } else {
                metadata.len()
            }
        })
        .sum()
}

fn stream_header() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(837_150_850u32.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(10u64.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    bytes
}

fn frame_group(stamp: u64, mut events: Vec<u8>, event_count: u32) -> Vec<u8> {
    events.push(19);
    events.extend(7u64.to_le_bytes());
    events.extend(1u32.to_le_bytes());
    events.extend(stamp.to_le_bytes());
    let mut bytes = Vec::new();
    bytes.extend((events.len() as u32).to_le_bytes());
    bytes.extend((event_count + 1).to_le_bytes());
    bytes.extend(3u64.to_le_bytes());
    bytes.extend(stamp.to_le_bytes());
    bytes.extend(events);
    bytes
}

fn frames() -> Vec<u8> {
    let mut bytes = stream_header();
    for stamp in 0..3u64 {
        let mut events = vec![1];
        events.extend(6u32.to_le_bytes());
        events.extend(b"frame\0");
        events.extend(1u32.to_le_bytes());
        bytes.extend(frame_group(stamp, events, 1));
    }
    bytes
}

fn wait(sessions: &Sessions, test: impl Fn(&serde_json::Value) -> bool) {
    let start = Instant::now();
    loop {
        let status = sessions.status();
        if test(&status) {
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "{status}");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn superseded_resource_job_cannot_replace_the_acknowledged_frame() {
    use super::{Entry, FocusRequest, FrameFormat};
    let mut sessions = Sessions::default();
    sessions.catalog.lock().unwrap().entries.insert(
        1,
        Entry {
            leases: Default::default(),
            snapshot: Some(pvd_core::Recording::from_bytes(&frames()).unwrap().into()),
            directory: Default::default(),
            bytes: 0,
        },
    );
    let mut first = sessions.prepare_frame(1).unwrap();
    let first_bytes = first.read(0, 0, FrameFormat::Resources(0, None)).unwrap();
    assert!(sessions.finish_frame(&mut first, true));
    let base = u64::from_le_bytes(first_bytes[4..12].try_into().unwrap());
    let focused = |x| {
        FrameFormat::Resources(
            base,
            Some(FocusRequest {
                mode: 1,
                actor: 0,
                generation: 0,
                center: [x, 0., 0.],
                radius: 1.,
            }),
        )
    };
    let mut older = sessions.prepare_frame(1).unwrap();
    let mut newer = sessions.prepare_frame(1).unwrap();
    let accepted = newer.read(2, 0, focused(20.)).unwrap();
    assert!(sessions.finish_frame(&mut newer, true));
    assert_eq!(&accepted[12..20], &base.to_le_bytes());
    assert!(older.read(1, 0, focused(10.)).is_err());
    assert!(!sessions.finish_frame(&mut older, true));
    let latest = u64::from_le_bytes(accepted[4..12].try_into().unwrap());
    let mut next = sessions.prepare_frame(1).unwrap();
    let retry = next
        .read(0, 0, FrameFormat::Resources(latest, None))
        .unwrap();
    assert_eq!(&retry[12..20], &latest.to_le_bytes());
    sessions.release_view();
    assert!(!sessions.finish_frame(&mut next, true));
    assert!(next.read(0, 0, FrameFormat::Resources(0, None)).is_err());
    let mut reset = sessions.prepare_frame(1).unwrap();
    let full = reset
        .read(0, 0, FrameFormat::Resources(latest, None))
        .unwrap();
    assert_eq!(&full[12..20], &[0; 8]);
}

#[test]
fn export_keeps_the_selected_session_snapshot_after_release() {
    use pvd_core::Recording;
    let root = std::env::temp_dir().join(format!(
        "pvd-unity-export-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut sessions = Sessions::default();
    let mut incoming = super::Incoming::begin(
        &root.join("history"),
        &sessions.catalog,
        &Default::default(),
    )
    .unwrap();
    incoming.received_data = true;
    incoming.live.as_mut().unwrap().feed(&frames()).unwrap();
    incoming.publish(&sessions.catalog, true);
    let id = incoming.id;
    let mut snapshot = sessions.snapshot(id).unwrap();
    // The receiver may count a trailing frame that could not enter the retained directory.
    let recording = std::sync::Arc::make_mut(&mut snapshot.recording);
    recording.frames = recording.frames.iter().take(2).cloned().collect();
    assert_eq!(recording.frame_count(), 3);
    incoming.finish(&sessions.catalog);
    sessions.release_view();
    sessions.clear().unwrap();
    assert_eq!(sessions.status()["sessions"][0]["frames"], 3);
    let path = root.join("会话.pvdcapture");
    let mut export = crate::export::SessionExport::default();
    export.start(id, path.clone(), snapshot).unwrap();
    let started = Instant::now();
    let result = loop {
        let status = export.status();
        if status["running"] == false {
            break status;
        }
        assert!(started.elapsed() < Duration::from_secs(10));
        thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(result["error"], "", "{result}");
    assert_eq!(result["frames"], 2);
    sessions.clear().unwrap();
    assert!(sessions.status()["sessions"].as_array().unwrap().is_empty());
    assert!(fs::read(&path).unwrap().starts_with(b"PVDCAP03"));
    let recording = Recording::from_path(&path).unwrap();
    assert_eq!(recording.frames.len(), 2);
    for index in [1, 0] {
        recording.state_at(index).unwrap();
    }
    drop(recording);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn reconnect_replays_protected_history_and_clears_measured_files() {
    let root = std::env::temp_dir().join(format!(
        "pvd-unity-session-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let unrelated = root.join("keep.txt");
    fs::write(&unrelated, b"keep").unwrap();
    let mut sessions = Sessions::default();
    sessions.start(&root, "127.0.0.1:0", true, false).unwrap();
    let address = sessions.status()["address"].as_str().unwrap().to_owned();
    let mut first = TcpStream::connect(&address).unwrap();
    first.write_all(&frames()).unwrap();
    wait(&sessions, |s| s["sessions"][0]["frames"] == 3);
    let first_id = sessions.status()["active"].as_u64().unwrap();
    assert_ne!(first_id, 0);
    sessions.frame(first_id, 2, 0).unwrap();
    assert!(sessions.frame(first_id, 2, 99).is_err());
    drop(first);
    wait(&sessions, |s| s["active"] == 0);
    let mut next = TcpStream::connect(&address).unwrap();
    next.write_all(&frames()).unwrap();
    wait(&sessions, |s| s["sessions"][1]["frames"] == 3);
    assert_eq!(sessions.clear().unwrap(), 0);
    sessions.frame(first_id, 0, 0).unwrap();
    sessions.stop().unwrap();
    drop(next);
    assert_eq!(sessions.status()["listening"], false);
    assert!(TcpStream::connect(&address).is_err());
    assert!(sessions.status()["clearableBytes"].as_u64().unwrap() > 0);
    let before = file_sizes(&root);
    let cleared_next = sessions.clear().unwrap();
    assert_eq!(cleared_next, before - file_sizes(&root));
    assert_eq!(sessions.status()["sessions"].as_array().unwrap().len(), 1);
    sessions.frame(first_id, 1, 0).unwrap();
    let mut job = sessions.prepare_frame(first_id).unwrap();
    assert_eq!(sessions.frame_count(first_id), Some(3));
    sessions.release_view();
    assert_eq!(sessions.clear().unwrap(), 0);
    assert!(!sessions.finish_frame(&mut job, true));
    drop(job);
    let expected = sessions.status()["clearableBytes"].as_u64().unwrap();
    assert!(expected > 0);
    let before = file_sizes(&root);
    let released = sessions.clear().unwrap();
    assert_eq!(released, expected);
    assert_eq!(released, before - file_sizes(&root));
    assert!(sessions.status()["sessions"].as_array().unwrap().is_empty());
    assert_eq!(fs::read(&unrelated).unwrap(), b"keep");
    sessions.start(&root, &address, true, false).unwrap();
    let mut invalid = TcpStream::connect(&address).unwrap();
    invalid.write_all(b"bad").unwrap();
    drop(invalid);
    wait(&sessions, |s| {
        !s["error"].as_str().unwrap().is_empty()
            && s["active"] == 0
            && s["sessions"].as_array().unwrap().is_empty()
    });
    let mut restarted = TcpStream::connect(&address).unwrap();
    restarted.write_all(&frames()).unwrap();
    drop(restarted);
    wait(&sessions, |s| {
        s["sessions"][0]["frames"] == 3 && s["active"] == 0
    });
    assert!(sessions.status()["sessions"][0]["id"].as_u64().unwrap() > first_id + 1);
    let mut active = TcpStream::connect(&address).unwrap();
    active.write_all(&frames()).unwrap();
    wait(&sessions, |s| s["sessions"][1]["frames"] == 3);
    drop(sessions);
    assert!(TcpStream::connect(&address).is_err());
    assert_eq!(file_sizes(&root), 4);
    fs::remove_file(unrelated).unwrap();
    fs::remove_dir_all(root).unwrap();
}
