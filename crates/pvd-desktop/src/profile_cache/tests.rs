use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let parent = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../output");
        fs::create_dir_all(&parent).unwrap();
        let root = parent.join(format!(
            "profile-cache-test-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        assert!(!root.exists());
        Self(root)
    }

    fn acquire(&self) -> Arc<Lease> {
        acquire(
            &self.0,
            &|| false,
            DISK_BUDGET,
            Arc::new(|_| Ok(Some(u64::MAX))),
        )
        .unwrap()
    }

    fn finish(self) {
        let files: Vec<_> = fs::read_dir(&self.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(files, [std::ffi::OsString::from(ROOT_LOCK)]);
        assert_eq!(fs::read(self.0.join(ROOT_LOCK)).unwrap(), ROOT_MAGIC);
        fs::remove_file(self.0.join(ROOT_LOCK)).unwrap();
        fs::remove_dir(self.0).unwrap();
    }
}

fn acquire(
    root: &Path,
    cancelled: &impl Fn() -> bool,
    budget: u64,
    space: Space,
) -> io::Result<Arc<Lease>> {
    Lease::acquire_with(root, cancelled, budget, FREE_RESERVE, space)
}

fn payload(lease: &Lease, id: u32, bytes: &[u8]) -> PathBuf {
    fs::create_dir_all(lease.workspace()).unwrap();
    let path = lease.workspace().join(format!("{id:08x}.idx"));
    fs::write(&path, bytes).unwrap();
    path
}

fn abandon(lease: Arc<Lease>) -> PathBuf {
    let mut lease = Arc::try_unwrap(lease).ok().unwrap();
    let path = lease.session.clone();
    drop(lease.marker.take());
    drop(lease.session_guard.take());
    drop(lease.root_lock.take());
    drop(lease.root_guard.take());
    // Process loss releases OS leases without invoking resource cleanup.
    std::mem::forget(lease);
    path
}

#[test]
fn workspace_is_new_and_budget_includes_control_and_session_ownership() {
    let scratch = Scratch::new();
    let lease = scratch.acquire();
    assert!(!lease.workspace().exists());
    assert_eq!(lease.workspace().file_name().unwrap(), INDEX_DIRECTORY);
    assert_eq!(lease.session.parent(), Some(lease.root.as_path()));
    assert_eq!(
        lease.disk_budget(),
        DISK_BUDGET - ROOT_MAGIC.len() as u64 - owner_bytes(&lease.session).unwrap().len() as u64
    );
    payload(&lease, 0, b"PVDSIDX1complete index bytes");
    payload(&lease, 1, b"PVDNAME1complete names");
    drop(lease);
    scratch.finish();
}

#[test]
fn both_index_workspaces_are_reclaimed_under_the_same_root_lease() {
    let scratch = Scratch::new();
    let old = scratch.acquire();
    payload(&old, 0, b"PVDSIDX1cpu index");
    fs::create_dir(old.catalog_workspace()).unwrap();
    fs::write(
        old.catalog_workspace().join("00000000.idx"),
        b"PVDCAT01catalog index",
    )
    .unwrap();
    fs::write(
        old.catalog_workspace().join("00000001.idx"),
        b"PVDNAME1catalog names",
    )
    .unwrap();
    let old = abandon(old);
    drop(scratch.acquire());
    assert!(!old.exists());
    scratch.finish();
}

#[test]
fn unknown_catalog_payload_preserves_the_complete_session_and_both_quotas() {
    let scratch = Scratch::new();
    let old = scratch.acquire();
    let cpu = payload(&old, 0, b"PVDSIDX1cpu retained");
    fs::create_dir(old.catalog_workspace()).unwrap();
    let catalog = old.catalog_workspace().join("00000000.idx");
    let unknown = old.catalog_workspace().join("keep.txt");
    fs::write(&catalog, b"PVDCAT01catalog retained").unwrap();
    fs::write(&unknown, b"user data").unwrap();
    let bytes = fs::metadata(&cpu).unwrap().len()
        + fs::metadata(&catalog).unwrap().len()
        + fs::metadata(&unknown).unwrap().len()
        + owner_bytes(&old.session).unwrap().len() as u64;
    let old = abandon(old);
    let fresh = scratch.acquire();
    assert!(cpu.exists() && catalog.exists() && unknown.exists());
    assert_eq!(
        fresh.disk_budget(),
        DISK_BUDGET
            - ROOT_MAGIC.len() as u64
            - bytes
            - owner_bytes(&fresh.session).unwrap().len() as u64
    );
    drop(fresh);
    fs::remove_file(unknown).unwrap();
    drop(scratch.acquire());
    assert!(!old.exists());
    scratch.finish();
}

#[test]
fn root_waiter_cancels_and_last_arc_keeps_the_root_exclusive() {
    let scratch = Scratch::new();
    let first = scratch.acquire();
    let held = first.clone();
    drop(first);
    assert_eq!(
        open_lock(&held.root.join(ROOT_LOCK), false)
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    let cancelled = Arc::new(AtomicBool::new(false));
    let stop = cancelled.clone();
    let path = scratch.0.clone();
    let (started, waiting) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        started.send(()).unwrap();
        acquire(
            &path,
            &|| stop.load(Ordering::Relaxed),
            DISK_BUDGET,
            Arc::new(|_| Ok(Some(u64::MAX))),
        )
        .err()
        .unwrap()
        .kind()
    });
    waiting.recv_timeout(Duration::from_secs(3)).unwrap();
    cancelled.store(true, Ordering::Relaxed);
    assert_eq!(waiter.join().unwrap(), io::ErrorKind::Interrupted);
    assert!(held.session.exists());
    drop(held);
    drop(scratch.acquire());
    scratch.finish();
}

#[test]
fn recognized_abandoned_sessions_are_reclaimed_before_new_budget_is_granted() {
    let scratch = Scratch::new();
    let old = scratch.acquire();
    payload(&old, 0, b"PVDSIDX1old sorted span run");
    payload(&old, 16_383, b"PVDNAME1old metadata");
    let old = abandon(old);
    let fresh = scratch.acquire();
    assert!(!old.exists());
    assert_eq!(
        fresh.disk_budget(),
        DISK_BUDGET - ROOT_MAGIC.len() as u64 - owner_bytes(&fresh.session).unwrap().len() as u64
    );
    drop(fresh);
    scratch.finish();
}

#[test]
fn truncated_or_unknown_files_preserve_the_whole_session_and_consume_budget() {
    let scratch = Scratch::new();
    let old = scratch.acquire();
    let known = payload(&old, 0, b"PVDSIDX1must stay intact");
    let partial = payload(&old, 1, b"partial");
    let unknown = old.workspace().join("unrelated.txt");
    fs::write(&unknown, b"unrelated data").unwrap();
    let retained = owner_bytes(&old.session).unwrap().len() as u64
        + fs::metadata(&known).unwrap().len()
        + fs::metadata(&partial).unwrap().len()
        + fs::metadata(&unknown).unwrap().len();
    let old = abandon(old);
    let fresh = scratch.acquire();
    assert_eq!(fs::read(&known).unwrap(), b"PVDSIDX1must stay intact");
    assert_eq!(fs::read(&partial).unwrap(), b"partial");
    assert_eq!(fs::read(&unknown).unwrap(), b"unrelated data");
    assert_eq!(
        fresh.disk_budget(),
        DISK_BUDGET
            - ROOT_MAGIC.len() as u64
            - retained
            - owner_bytes(&fresh.session).unwrap().len() as u64
    );
    drop(fresh);
    fs::remove_file(partial).unwrap();
    fs::remove_file(unknown).unwrap();
    drop(scratch.acquire());
    assert!(!old.exists());
    scratch.finish();
}

#[test]
fn foreign_root_files_are_not_deleted_and_unmeasurable_directories_are_rejected() {
    let scratch = Scratch::new();
    fs::create_dir(&scratch.0).unwrap();
    let unrelated = scratch.0.join("keep.txt");
    fs::write(&unrelated, b"user-owned").unwrap();
    let lease = scratch.acquire();
    assert_eq!(
        lease.disk_budget(),
        DISK_BUDGET
            - ROOT_MAGIC.len() as u64
            - 10
            - owner_bytes(&lease.session).unwrap().len() as u64
    );
    drop(lease);
    let foreign = scratch.0.join("unknown");
    fs::create_dir(&foreign).unwrap();
    fs::write(foreign.join("source.pvdstream"), b"untouched").unwrap();
    let error = acquire(
        &scratch.0,
        &|| false,
        DISK_BUDGET,
        Arc::new(|_| Ok(Some(u64::MAX))),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("unknown directory"));
    assert_eq!(fs::read(&unrelated).unwrap(), b"user-owned");
    assert_eq!(
        fs::read(foreign.join("source.pvdstream")).unwrap(),
        b"untouched"
    );
    fs::remove_file(foreign.join("source.pvdstream")).unwrap();
    fs::remove_dir(foreign).unwrap();
    fs::remove_file(unrelated).unwrap();
    scratch.finish();
}

#[test]
fn reserve_is_rechecked_for_every_write_and_unknown_free_space_is_rejected() {
    let scratch = Scratch::new();
    let available = Arc::new(AtomicU64::new(u64::MAX));
    let current = available.clone();
    let lease = acquire(
        &scratch.0,
        &|| false,
        DISK_BUDGET,
        Arc::new(move |_| Ok(Some(current.load(Ordering::Relaxed)))),
    )
    .unwrap();
    lease.check_write(1024).unwrap();
    available.store(FREE_RESERVE + 7, Ordering::Relaxed);
    lease.check_write(7).unwrap();
    assert!(
        lease
            .check_write(8)
            .unwrap_err()
            .to_string()
            .contains("preserve")
    );
    available.store(FREE_RESERVE - 1, Ordering::Relaxed);
    assert!(lease.check_write(0).is_err());
    assert!(lease.check_write(u64::MAX).is_err());
    drop(lease);
    assert!(
        acquire(&scratch.0, &|| false, DISK_BUDGET, Arc::new(|_| Ok(None)))
            .err()
            .unwrap()
            .to_string()
            .contains("unavailable")
    );
    scratch.finish();
}

#[test]
fn retained_bytes_and_directory_entry_limits_cannot_be_bypassed() {
    let scratch = Scratch::new();
    fs::create_dir(&scratch.0).unwrap();
    let foreign = scratch.0.join("occupied");
    fs::write(&foreign, [0; 1024]).unwrap();
    assert!(
        acquire(
            &scratch.0,
            &|| false,
            1024,
            Arc::new(|_| Ok(Some(u64::MAX)))
        )
        .err()
        .unwrap()
        .to_string()
        .contains("budget")
    );
    assert_eq!(fs::metadata(&foreign).unwrap().len(), 1024);
    fs::remove_file(foreign).unwrap();
    for id in 0..MAX_ROOT_ENTRIES {
        fs::write(scratch.0.join(format!("foreign-{id}")), []).unwrap();
    }
    assert!(
        acquire(
            &scratch.0,
            &|| false,
            DISK_BUDGET,
            Arc::new(|_| Ok(Some(u64::MAX)))
        )
        .err()
        .unwrap()
        .to_string()
        .contains("entry limit")
    );
    for id in 0..MAX_ROOT_ENTRIES {
        fs::remove_file(scratch.0.join(format!("foreign-{id}"))).unwrap();
    }
    scratch.finish();
}

#[test]
fn cancellation_during_validation_leaves_payload_and_ownership_recoverable() {
    let scratch = Scratch::new();
    let old = scratch.acquire();
    let data = payload(&old, 0, b"PVDSIDX1data");
    let old = abandon(old);
    let calls = AtomicUsize::new(0);
    let error = acquire(
        &scratch.0,
        &|| calls.fetch_add(1, Ordering::Relaxed) >= 4,
        DISK_BUDGET,
        Arc::new(|_| Ok(Some(u64::MAX))),
    )
    .err()
    .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert_eq!(fs::read(data).unwrap(), b"PVDSIDX1data");
    assert!(old.join(OWNER_FILE).exists());
    drop(scratch.acquire());
    assert!(!old.exists());
    scratch.finish();
}

#[test]
fn session_and_payload_names_are_bounded_and_control_files_are_never_adopted() {
    assert!(session_name("session-1-2-3"));
    for name in [
        "session-1-2",
        "session-1-2-3-4",
        "session--2-3",
        "session-a-2-3",
    ] {
        assert!(!session_name(name));
    }
    assert_eq!(index_id("00000000.idx"), Some(0));
    assert_eq!(index_id("00003fff.idx"), Some(16_383));
    for name in [
        "00004000.idx",
        "00003FFF.idx",
        "0000000.idx",
        "../00000000.idx",
    ] {
        assert_eq!(index_id(name), None);
    }
    let scratch = Scratch::new();
    fs::create_dir(&scratch.0).unwrap();
    let control = scratch.0.join(ROOT_LOCK);
    fs::write(&control, b"foreign ownership").unwrap();
    assert!(
        acquire(
            &scratch.0,
            &|| false,
            DISK_BUDGET,
            Arc::new(|_| Ok(Some(u64::MAX)))
        )
        .is_err()
    );
    assert_eq!(fs::read(&control).unwrap(), b"foreign ownership");
    fs::remove_file(control).unwrap();
    fs::remove_dir(scratch.0).unwrap();
}

#[cfg(windows)]
#[test]
fn active_directory_guards_block_path_replacement_and_failed_delete_keeps_ownership() {
    use std::os::windows::fs::OpenOptionsExt;
    let scratch = Scratch::new();
    let lease = scratch.acquire();
    let data = payload(&lease, 0, b"PVDSIDX1held file");
    assert!(fs::rename(&lease.session, lease.root.join("moved")).is_err());
    assert!(fs::rename(&lease.root, lease.root.with_extension("moved")).is_err());
    let held = OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(&data)
        .unwrap();
    let session = lease.session.clone();
    drop(lease);
    assert!(data.exists());
    assert!(session.join(OWNER_FILE).exists());
    assert!(
        acquire(
            &scratch.0,
            &|| false,
            DISK_BUDGET,
            Arc::new(|_| Ok(Some(u64::MAX)))
        )
        .is_err()
    );
    drop(held);
    drop(scratch.acquire());
    assert!(!session.exists());
    scratch.finish();
}

#[test]
fn final_verified_handle_blocks_payload_replacement_and_new_writers_until_deletion() {
    let scratch = Scratch::new();
    let lease = scratch.acquire();
    let path = payload(&lease, 0, b"PVDSIDX1verified object");
    let replacement = lease.workspace().join("unknown-replacement");
    fs::write(&replacement, b"unrelated replacement").unwrap();
    let (file, recognized) = open_index(&path, true).unwrap();
    assert!(recognized);
    assert!(file.metadata().unwrap().is_file());
    assert!(fs::rename(&path, lease.workspace().join("moved.idx")).is_err());
    assert!(fs::rename(&replacement, &path).is_err());
    assert!(OpenOptions::new().write(true).open(&path).is_err());
    assert!(fs::write(&path, b"must not overwrite").is_err());
    assert_eq!(fs::read(&replacement).unwrap(), b"unrelated replacement");
    delete_by_handle(file).unwrap();
    assert!(!path.exists());
    assert_eq!(fs::read(&replacement).unwrap(), b"unrelated replacement");
    fs::remove_file(replacement).unwrap();
    drop(lease);
    scratch.finish();
}

#[test]
fn replacement_after_session_preflight_is_rejected_and_unknown_bytes_are_preserved() {
    let scratch = Scratch::new();
    let lease = scratch.acquire();
    let path = payload(&lease, 0, b"PVDSIDX1original object");
    let session = abandon(lease);
    let root = fs::canonicalize(&scratch.0).unwrap();
    let guard = directory_guard(&root, false).unwrap();
    let lock = acquire_root_lock(&root, &|| false, &|| Ok(())).unwrap();
    let contents = inspect_session(&root, &session, &|| false).unwrap();
    assert!(contents.recognized);
    let previous = scratch.0.join("previous");
    fs::rename(&path, &previous).unwrap();
    fs::write(&path, b"unknown bytes inserted after validation").unwrap();
    assert!(reclaim_contents(&session, contents, &|| false).is_err());
    assert_eq!(
        fs::read(&path).unwrap(),
        b"unknown bytes inserted after validation"
    );
    assert!(session.join(OWNER_FILE).exists());
    fs::remove_file(&path).unwrap();
    fs::rename(previous, path).unwrap();
    drop(lock);
    drop(guard);
    drop(scratch.acquire());
    assert!(!session.exists());
    scratch.finish();
}

#[test]
fn low_space_reclaims_owned_payload_before_enforcing_the_new_write_reserve() {
    let scratch = Scratch::new();
    let lease = scratch.acquire();
    let data = payload(&lease, 0, b"PVDSIDX1reclaimable old bytes");
    let session = abandon(lease);
    let reclaimed = data.clone();
    let fresh = acquire(
        &scratch.0,
        &|| false,
        DISK_BUDGET,
        Arc::new(move |_| Ok(Some(if reclaimed.exists() { 0 } else { u64::MAX }))),
    )
    .unwrap();
    assert!(!data.exists());
    assert!(!session.exists());
    drop(fresh);
    scratch.finish();
}

#[test]
fn new_low_space_root_creates_only_the_permanent_control_and_can_later_retry() {
    let scratch = Scratch::new();
    let error = acquire(
        &scratch.0,
        &|| false,
        DISK_BUDGET,
        Arc::new(|_| Ok(Some(ROOT_MAGIC.len() as u64))),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("preserve"));
    assert_eq!(fs::read(scratch.0.join(ROOT_LOCK)).unwrap(), ROOT_MAGIC);
    assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 1);
    drop(scratch.acquire());
    scratch.finish();
}

#[test]
fn public_acquisition_observes_cancellation_before_creating_any_directory() {
    let scratch = Scratch::new();
    let error = Lease::acquire(&scratch.0, || true).err().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert!(!scratch.0.exists());
}

#[test]
fn handle_disposition_failure_preserves_the_readonly_payload_without_path_fallback() {
    let scratch = Scratch::new();
    let lease = scratch.acquire();
    let path = payload(&lease, 0, b"PVDSIDX1readonly payload");
    let original = fs::metadata(&path).unwrap().permissions();
    let mut readonly = original.clone();
    readonly.set_readonly(true);
    fs::set_permissions(&path, readonly).unwrap();
    let (file, recognized) = open_index(&path, true).unwrap();
    assert!(recognized);
    assert!(delete_by_handle(file).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"PVDSIDX1readonly payload");
    assert!(lease.session.join(OWNER_FILE).exists());
    fs::set_permissions(path, original).unwrap();
    drop(lease);
    scratch.finish();
}
