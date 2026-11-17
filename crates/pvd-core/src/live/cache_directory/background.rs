use super::{OwnedCleanup, cleanup_stale_history};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
        mpsc::{SyncSender, TrySendError, sync_channel},
    },
};

const QUEUE_CAPACITY: usize = 32;
static CLEANER: OnceLock<Option<Cleaner>> = OnceLock::new();

/// Desktop opt-in: temporary history deletion and stale scans run off the UI
/// and decoder threads. Shutdown may leave marked files for a later scan.
pub fn enable_background_stale_history_cleanup() {
    CLEANER.get_or_init(|| Cleaner::start().ok());
}

pub(super) fn enabled() -> bool {
    CLEANER.get().is_some()
}

/// Queued and currently running cleanup jobs, for a bounded shutdown grace.
pub fn pending_history_cleanups() -> usize {
    CLEANER
        .get()
        .and_then(Option::as_ref)
        .map_or(0, |cleaner| cleaner.pending.load(Ordering::Acquire))
}

/// Schedules a safe scan of marked, unlocked sessions without blocking callers.
/// If the bounded queue is full, the next startup/session can retry the scan.
pub fn queue_stale_history_cleanup(root: &Path) {
    if let Some(Some(cleaner)) = CLEANER.get() {
        cleaner.scan(root.to_path_buf());
    }
}

pub(super) fn dispose(owned: OwnedCleanup) {
    if let Some(Some(cleaner)) = CLEANER.get() {
        cleaner.enqueue(Job::Owned(owned));
    }
    // A failed worker allocation leaves the session marker for a future run.
}

enum Job {
    Owned(OwnedCleanup),
    Scan(PathBuf),
}

struct Cleaner {
    sender: SyncSender<Job>,
    pending: Arc<AtomicUsize>,
    scans: Arc<Mutex<BTreeSet<PathBuf>>>,
}

impl Cleaner {
    fn start() -> std::io::Result<Self> {
        let (sender, receiver) = sync_channel(QUEUE_CAPACITY);
        let pending = Arc::new(AtomicUsize::new(0));
        let scans = Arc::new(Mutex::new(BTreeSet::new()));
        let worker_pending = pending.clone();
        let worker_scans = scans.clone();
        std::thread::Builder::new()
            .name("pvd-history-cleanup".into())
            .spawn(move || {
                while let Ok(job) = receiver.recv() {
                    match job {
                        Job::Owned(owned) => owned.run(),
                        Job::Scan(root) => {
                            let _ = cleanup_stale_history(&root);
                            worker_scans
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .remove(&root);
                        }
                    }
                    worker_pending.fetch_sub(1, Ordering::Release);
                }
            })?;
        Ok(Self {
            sender,
            pending,
            scans,
        })
    }

    fn scan(&self, root: PathBuf) {
        if self
            .scans
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(root.clone())
        {
            self.enqueue(Job::Scan(root));
        }
    }

    fn enqueue(&self, job: Job) {
        self.pending.fetch_add(1, Ordering::Release);
        if let Err(TrySendError::Full(job) | TrySendError::Disconnected(job)) =
            self.sender.try_send(job)
        {
            if let Job::Scan(root) = job {
                self.scans
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&root);
            }
            self.pending.fetch_sub(1, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::cache_directory::{CacheDirectory, MAGIC, OWNER_FILE, tests::root};
    use std::{
        fs,
        time::{Duration, Instant},
    };

    #[test]
    fn background_cleanup_holds_lease_and_reclaims_owned_files() {
        let root = root();
        let mut owner = Arc::try_unwrap(CacheDirectory::new(&root).unwrap()).unwrap();
        let path = owner.path.clone();
        let file = path.join("00000000.pvdhistory");
        fs::write(&file, MAGIC).unwrap();
        owner.files.get_mut().unwrap().insert(file);
        let owned = OwnedCleanup {
            path: owner.path.clone(),
            files: Some(std::mem::take(owner.files.get_mut().unwrap())),
            lease: owner.lease.take(),
            guard: owner.guard.take(),
        };
        // The moved lease protects the session even while waiting for a worker.
        assert_eq!(cleanup_stale_history(&root).unwrap().removed_sessions, 0);
        std::mem::forget(owner);
        let cleaner = Cleaner::start().unwrap();
        cleaner.enqueue(Job::Owned(owned));
        let deadline = Instant::now() + Duration::from_secs(5);
        while cleaner.pending.load(Ordering::Acquire) != 0 {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!path.exists());
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn full_queue_defers_owned_files_and_later_scan_recovers_them() {
        let root = root();
        let mut owner = Arc::try_unwrap(CacheDirectory::new(&root).unwrap()).unwrap();
        let path = owner.path.clone();
        let file = path.join("00000000.pvdhistory");
        fs::write(&file, MAGIC).unwrap();
        owner.files.get_mut().unwrap().insert(file.clone());
        let owned = OwnedCleanup {
            path: owner.path.clone(),
            files: Some(std::mem::take(owner.files.get_mut().unwrap())),
            lease: owner.lease.take(),
            guard: owner.guard.take(),
        };
        std::mem::forget(owner);
        let (sender, _receiver) = sync_channel(0);
        let cleaner = Cleaner {
            sender,
            pending: Arc::new(AtomicUsize::new(0)),
            scans: Arc::default(),
        };
        cleaner.enqueue(Job::Owned(owned));
        assert_eq!(cleaner.pending.load(Ordering::Acquire), 0);
        assert!(path.join(OWNER_FILE).exists());
        assert!(file.exists());
        assert_eq!(cleanup_stale_history(&root).unwrap().removed_sessions, 1);
        fs::remove_dir(root).unwrap();
    }
}
