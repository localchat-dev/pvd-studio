use super::*;
use crate::{CaptureServer, ListenConfig, SpoolOptions};
use std::{
    net::TcpStream,
    process::Command,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

fn root() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "pvd-receive-lease-test-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&path).unwrap();
    fs::canonicalize(path).unwrap()
}

fn abandon(mut owner: SpoolDirectory) -> PathBuf {
    let path = owner.path.clone();
    drop(owner.lease.take());
    drop(owner.guard.take());
    std::mem::forget(owner);
    path
}

fn raw(owner: &SpoolDirectory, index: u64, bytes: &[u8]) -> String {
    let name = format!("{:016}-{index:016}.bin", 1);
    owner
        .create_segment(&name)
        .unwrap()
        .write_all(bytes)
        .unwrap();
    name
}

#[test]
fn active_readers_keep_the_lease_and_abandoned_registered_bytes_are_reclaimed() {
    let root = root();
    let owner = Arc::new(SpoolDirectory::new(&root).unwrap());
    let retained = owner.clone();
    let active_name = raw(&owner, 0, b"active reader bytes");
    drop(owner);
    let stale = SpoolDirectory::new(&root).unwrap();
    raw(&stale, 0, &[9; 8192]);
    let stale_path = abandon(stale);
    let report = cleanup_stale_spools(&root).unwrap();
    assert_eq!(
        (
            report.removed_sessions,
            report.removed_bytes,
            report.skipped_sessions
        ),
        (1, 8192, 1)
    );
    assert!(!stale_path.exists());
    assert_eq!(
        fs::read(retained.path.join(&active_name)).unwrap(),
        b"active reader bytes"
    );
    retained.remove_segment(&active_name).unwrap();
    drop(retained);
    assert!(fs::read_dir(&root).unwrap().next().is_none());
    fs::remove_dir(root).unwrap();
}

#[test]
fn unknown_and_unregistered_files_block_the_entire_cleanup() {
    let root = root();
    let owner = SpoolDirectory::new(&root).unwrap();
    let name = raw(&owner, 0, b"owned");
    let path = abandon(owner);
    for unknown in [
        "capture.pvdstream",
        "0000000000000001-0000000000000001.bin",
        "foreign.txt",
    ] {
        let foreign = path.join(unknown);
        fs::write(&foreign, b"permanent data").unwrap();
        assert_eq!(cleanup_stale_spools(&root).unwrap().removed_sessions, 0);
        assert_eq!(fs::read(path.join(&name)).unwrap(), b"owned");
        assert_eq!(fs::read(&foreign).unwrap(), b"permanent data");
        fs::remove_file(foreign).unwrap();
    }
    assert_eq!(cleanup_stale_spools(&root).unwrap().removed_sessions, 1);
    fs::remove_dir(root).unwrap();
}

#[test]
fn legacy_and_foreign_markers_are_preserved() {
    let root = root();
    let legacy = root.join("spool-1-2-3");
    fs::create_dir(&legacy).unwrap();
    let file = legacy.join("0000000000000001-0000000000000000.bin");
    fs::write(&file, b"legacy bytes").unwrap();
    assert_eq!(cleanup_stale_spools(&root).unwrap().removed_sessions, 0);
    fs::write(legacy.join(OWNER_FILE), b"foreign marker").unwrap();
    assert_eq!(cleanup_stale_spools(&root).unwrap().removed_sessions, 0);
    assert_eq!(fs::read(&file).unwrap(), b"legacy bytes");
    fs::remove_file(file).unwrap();
    fs::remove_file(legacy.join(OWNER_FILE)).unwrap();
    fs::remove_dir(legacy).unwrap();
    fs::remove_dir(root).unwrap();
}

#[test]
fn invalid_registration_and_cross_directory_registration_preserve_all_bytes() {
    let root = root();
    let owner = SpoolDirectory::new(&root).unwrap();
    let name = raw(&owner, 0, b"owned");
    let valid = registration_bytes(&owner.path, &name).unwrap();
    let path = abandon(owner);
    let registration = registration_path(&path, &name);
    for invalid in [
        b"truncated".to_vec(),
        registration_bytes(&root.join("spool-1-2-3"), &name).unwrap(),
    ] {
        fs::write(&registration, invalid).unwrap();
        assert_eq!(cleanup_stale_spools(&root).unwrap().removed_sessions, 0);
        assert_eq!(fs::read(path.join(&name)).unwrap(), b"owned");
    }
    fs::write(registration, valid).unwrap();
    assert_eq!(cleanup_stale_spools(&root).unwrap().removed_sessions, 1);
    fs::remove_dir(root).unwrap();
}

#[test]
fn orphan_registration_and_empty_unregistered_creation_have_conservative_outcomes() {
    let root = root();
    let owner = SpoolDirectory::new(&root).unwrap();
    let name = raw(&owner, 0, b"consumed");
    fs::remove_file(owner.path.join(&name)).unwrap();
    let path = abandon(owner);
    assert_eq!(cleanup_stale_spools(&root).unwrap().removed_sessions, 1);
    assert!(!path.exists());
    let owner = SpoolDirectory::new(&root).unwrap();
    let unregistered = owner.path.join("0000000000000001-0000000000000000.bin");
    fs::write(&unregistered, []).unwrap();
    let path = abandon(owner);
    assert_eq!(cleanup_stale_spools(&root).unwrap().removed_sessions, 0);
    fs::remove_file(unregistered).unwrap();
    assert_eq!(cleanup_stale_spools(&root).unwrap().removed_sessions, 1);
    assert!(!path.exists());
    fs::remove_dir(root).unwrap();
}

#[test]
fn create_collision_never_removes_foreign_raw_or_registration() {
    let root = root();
    let owner = SpoolDirectory::new(&root).unwrap();
    let name = "0000000000000001-0000000000000000.bin";
    let path = owner.path.join(name);
    fs::write(&path, b"foreign raw").unwrap();
    assert!(owner.create_segment(name).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"foreign raw");
    fs::remove_file(&path).unwrap();
    let marker = registration_path(&owner.path, name);
    fs::write(&marker, b"foreign registration").unwrap();
    assert!(owner.create_segment(name).is_err());
    assert!(!path.exists());
    assert_eq!(fs::read(&marker).unwrap(), b"foreign registration");
    fs::remove_file(marker).unwrap();
    drop(owner);
    fs::remove_dir(root).unwrap();
}

#[test]
fn failed_bind_releases_its_directory_without_waiting_for_process_exit() {
    let root = root();
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    assert!(
        CaptureServer::bind_spooled(
            ListenConfig {
                address: occupied.local_addr().unwrap(),
                ..Default::default()
            },
            SpoolOptions {
                directory: root.clone(),
                ..Default::default()
            },
            || {}
        )
        .is_err()
    );
    assert!(fs::read_dir(&root).unwrap().next().is_none());
    fs::remove_dir(root).unwrap();
}

fn hidden(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    #[cfg(not(windows))]
    let _ = command;
}

#[test]
fn crash_fixture_child() {
    let Some(path) = std::env::var_os("PVD_RECEIVE_CRASH_TEST_ROOT") else {
        return;
    };
    let root = PathBuf::from(path);
    let server = CaptureServer::bind_spooled(
        ListenConfig {
            address: "127.0.0.1:0".parse().unwrap(),
            queue_capacity: 1,
            ..Default::default()
        },
        SpoolOptions {
            directory: root.join("receive"),
            segment_bytes: 64 << 10,
            ..Default::default()
        },
        || {},
    )
    .unwrap();
    let mut sender = TcpStream::connect(server.address()).unwrap();
    sender.write_all(&vec![71; 1 << 20]).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while server.spool_stats().unwrap().snapshot().queued_bytes < 1 << 20 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    fs::write(root.join("ready"), b"ready").unwrap();
    loop {
        thread::sleep(Duration::from_secs(1));
    }
}

#[test]
fn terminated_process_releases_lease_and_next_bind_reclaims_real_tcp_backlog() {
    let root = root();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "spool_directory::tests::crash_fixture_child",
            "--nocapture",
        ])
        .env("PVD_RECEIVE_CRASH_TEST_ROOT", &root);
    hidden(&mut command);
    let mut child = command.spawn().unwrap();
    let ready = root.join("ready");
    let deadline = Instant::now() + Duration::from_secs(15);
    while !ready.exists() {
        if Instant::now() >= deadline || child.try_wait().unwrap().is_some() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("receive crash child did not publish its ready marker");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let receive = root.join("receive");
    let report = cleanup_stale_spools(&receive).unwrap();
    assert_eq!((report.removed_sessions, report.skipped_sessions), (0, 1));
    child.kill().unwrap();
    child.wait().unwrap();
    let path = fs::read_dir(&receive)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let bytes: u64 = fs::read_dir(&path)
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.file_name().to_str().is_some_and(segment_name))
        .map(|e| {
            let contents = fs::read(e.path()).unwrap();
            assert!(contents.iter().all(|byte| *byte == 71));
            contents.len() as u64
        })
        .sum();
    assert_eq!(bytes, 1 << 20);
    let mut server = CaptureServer::bind_spooled(
        ListenConfig {
            address: "127.0.0.1:0".parse().unwrap(),
            ..Default::default()
        },
        SpoolOptions {
            directory: receive.clone(),
            ..Default::default()
        },
        || {},
    )
    .unwrap();
    assert!(!path.exists());
    server.stop();
    drop(server);
    assert!(fs::read_dir(&receive).unwrap().next().is_none());
    eprintln!(
        "terminated TCP child: {bytes} raw bytes verified; active lease skipped; next bind reclaimed old session; normal stop left zero files"
    );
    fs::remove_dir(receive).unwrap();
    fs::remove_file(ready).unwrap();
    fs::remove_dir(root).unwrap();
}

#[test]
#[cfg(windows)]
fn active_directory_and_segment_cannot_be_replaced() {
    let root = root();
    let owner = SpoolDirectory::new(&root).unwrap();
    let name = "0000000000000001-0000000000000000.bin";
    let file = owner.create_segment(name).unwrap();
    assert!(fs::rename(&owner.path, root.join("moved")).is_err());
    assert!(fs::rename(owner.path.join(name), owner.path.join("moved.bin")).is_err());
    drop(file);
    owner.remove_segment(name).unwrap();
    drop(owner);
    fs::remove_dir(root).unwrap();
}

#[test]
fn cleanup_bounds_leave_an_oversized_session_intact() {
    let root = root();
    let owner = SpoolDirectory::new(&root).unwrap();
    let path = abandon(owner);
    for index in 0..=MAX_CLEANUP_FILES / 2 {
        let name = format!("{:016}-{index:016}.bin", 1);
        fs::write(path.join(&name), b"x").unwrap();
        fs::write(
            registration_path(&path, &name),
            registration_bytes(&path, &name).unwrap(),
        )
        .unwrap();
    }
    assert_eq!(cleanup_stale_spools(&root).unwrap().removed_sessions, 0);
    assert_eq!(fs::read_dir(&path).unwrap().count(), MAX_CLEANUP_FILES + 3);
    let last = format!("{:016}-{:016}.bin", 1, MAX_CLEANUP_FILES / 2);
    fs::remove_file(path.join(&last)).unwrap();
    fs::remove_file(registration_path(&path, &last)).unwrap();
    let report = cleanup_stale_spools(&root).unwrap();
    assert_eq!(report.removed_sessions, 1);
    assert_eq!(report.removed_bytes, (MAX_CLEANUP_FILES / 2) as u64);
    fs::remove_dir(root).unwrap();
}

#[test]
fn cleanup_scans_a_bounded_number_of_root_entries() {
    let root = root();
    for index in 0..=MAX_SCAN_DIRECTORIES {
        let path = root.join(format!("spool-1-2-{index}"));
        fs::create_dir(&path).unwrap();
        fs::write(path.join(OWNER_FILE), owner_bytes(&path).unwrap()).unwrap();
    }
    assert_eq!(
        cleanup_stale_spools(&root).unwrap().removed_sessions,
        MAX_SCAN_DIRECTORIES
    );
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    assert_eq!(cleanup_stale_spools(&root).unwrap().removed_sessions, 1);
    fs::remove_dir(root).unwrap();
}

#[test]
#[cfg(windows)]
fn reparse_sessions_and_registered_entries_preserve_their_targets() {
    let root = root();
    let external = root.join("permanent-recordings");
    fs::create_dir(&external).unwrap();
    let permanent = external.join("capture.pvdstream");
    fs::write(&permanent, b"permanent bytes").unwrap();
    let junction = root.join("spool-1-2-3");
    let make_junction = |link: &Path| {
        let mut command = Command::new("cmd");
        command
            .args(["/c", "mklink", "/J"])
            .arg(link)
            .arg(&external);
        hidden(&mut command);
        let result = command.output().unwrap();
        assert!(result.status.success(), "{:?}", result);
    };
    make_junction(&junction);
    assert_eq!(cleanup_stale_spools(&root).unwrap().removed_sessions, 0);
    assert_eq!(fs::read(&permanent).unwrap(), b"permanent bytes");
    fs::remove_dir(&junction).unwrap();
    let owner = SpoolDirectory::new(&root).unwrap();
    let name = raw(&owner, 0, b"owned");
    let path = abandon(owner);
    fs::remove_file(path.join(&name)).unwrap();
    make_junction(&path.join(&name));
    assert_eq!(cleanup_stale_spools(&root).unwrap().removed_sessions, 0);
    assert_eq!(fs::read(&permanent).unwrap(), b"permanent bytes");
    fs::remove_dir(path.join(&name)).unwrap();
    assert_eq!(cleanup_stale_spools(&root).unwrap().removed_sessions, 1);
    fs::remove_file(permanent).unwrap();
    fs::remove_dir(external).unwrap();
    fs::remove_dir(root).unwrap();
}
