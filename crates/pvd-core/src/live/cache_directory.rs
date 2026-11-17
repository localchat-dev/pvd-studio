use super::{Error, MAGIC, SEQUENCE, error};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const OWNER_FILE: &str = ".pvd-studio-owner";
const OWNER_MAGIC: &str = "pvd-studio temporary history v1\n";
const MAX_SCAN_DIRECTORIES: usize = 256;
const MAX_CLEANUP_FILES: usize = 12416;

mod background;
pub use background::{
    enable_background_stale_history_cleanup, pending_history_cleanups, queue_stale_history_cleanup,
};

#[derive(Debug)]
pub(super) struct CacheDirectory {
    pub(super) path: PathBuf,
    pub(super) files: Mutex<BTreeSet<PathBuf>>,
    lease: Option<File>,
    guard: Option<File>,
}

#[derive(Debug, Default)]
pub struct CacheCleanupReport {
    pub removed_sessions: usize,
    pub removed_bytes: u64,
    pub skipped_sessions: usize,
}

fn plain_metadata(path: &Path) -> io::Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::other("history cleanup refuses symbolic links"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(io::Error::other("history cleanup refuses reparse points"));
        }
    }
    Ok(metadata)
}

fn open_lease(path: &Path, create: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(create);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // A lease must not be replaced while an owner or cleaner holds it.
        options.share_mode(3);
    }
    let file = options.open(path)?;
    file.try_lock().map_err(io::Error::from)?;
    Ok(file)
}

fn directory_guard(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Prevent path replacement during validation and deletion on Windows.
        options.share_mode(3).custom_flags(0x0200_0000);
    }
    options.open(path)
}

fn session_name(name: &str) -> bool {
    let Some(parts) = name.strip_prefix("session-") else {
        return false;
    };
    let mut parts = parts.split('-');
    (0..3).all(|_| {
        parts
            .next()
            .is_some_and(|part| !part.is_empty() && part.bytes().all(|c| c.is_ascii_digit()))
    }) && parts.next().is_none()
}

fn history_name(name: &str) -> bool {
    name.strip_suffix(".pvdhistory").is_some_and(|number| {
        number.len() >= 8 && number.len() <= 20 && number.bytes().all(|c| c.is_ascii_digit())
    })
}

fn owner_bytes(path: &Path) -> io::Result<Vec<u8>> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| session_name(name))
        .ok_or_else(|| io::Error::other("invalid history session name"))?;
    Ok(format!("{OWNER_MAGIC}{name}\n").into_bytes())
}

fn remove_stale(root: &Path, path: &Path) -> io::Result<u64> {
    if !plain_metadata(path)?.is_dir() || path.parent() != Some(root) {
        return Err(io::Error::other(
            "history session is outside the cache root",
        ));
    }
    let guard = directory_guard(path)?;
    if fs::canonicalize(path)? != path {
        return Err(io::Error::other("history session path changed"));
    }
    let marker = path.join(OWNER_FILE);
    let expected = owner_bytes(path)?;
    if !plain_metadata(&marker)?.is_file() || fs::metadata(&marker)?.len() != expected.len() as u64
    {
        return Err(io::Error::other("invalid history ownership marker"));
    }
    let mut lease = open_lease(&marker, false)?;
    let mut contents = vec![0; expected.len()];
    lease.read_exact(&mut contents)?;
    if contents != expected {
        return Err(io::Error::other("history ownership marker does not match"));
    }
    let mut files = Vec::new();
    let mut bytes = 0u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_name() == OWNER_FILE {
            continue;
        }
        if files.len() >= MAX_CLEANUP_FILES || !entry.file_name().to_str().is_some_and(history_name)
        {
            return Err(io::Error::other("unrecognized history directory contents"));
        }
        let metadata = plain_metadata(&entry.path())?;
        if !metadata.is_file() {
            return Err(io::Error::other("history directory contains a non-file"));
        }
        let mut header = [0; 8];
        File::open(entry.path())?.read_exact(&mut header)?;
        if &header != MAGIC
            && &header != b"PVDHIS01"
            && &header != b"PVDHIS02"
            && &header != b"PVDCAP03"
        {
            return Err(io::Error::other("history file has an unknown signature"));
        }
        bytes = bytes.saturating_add(metadata.len());
        files.push(entry.path());
    }
    for file in files {
        plain_metadata(&file)?;
        fs::remove_file(file)?;
    }
    drop(lease);
    fs::remove_file(marker)?;
    drop(guard);
    fs::remove_dir(path)?;
    Ok(bytes)
}

/// Reclaims marked, unlocked cache sessions. Unknown contents are left untouched.
pub fn cleanup_stale_history(root: &Path) -> io::Result<CacheCleanupReport> {
    let root = fs::canonicalize(root)?;
    let _guard = directory_guard(&root)?;
    let mut report = CacheCleanupReport::default();
    for entry in fs::read_dir(&root)?.take(MAX_SCAN_DIRECTORIES) {
        let entry = entry?;
        if !entry.file_name().to_str().is_some_and(session_name) {
            continue;
        }
        match remove_stale(&root, &entry.path()) {
            Ok(bytes) => {
                report.removed_sessions += 1;
                report.removed_bytes = report.removed_bytes.saturating_add(bytes);
            }
            Err(_) => report.skipped_sessions += 1,
        }
    }
    Ok(report)
}

impl CacheDirectory {
    pub(super) fn new(root: &Path) -> Result<Arc<Self>, Error> {
        fs::create_dir_all(root)?;
        let root = fs::canonicalize(root)?;
        if background::enabled() {
            queue_stale_history_cleanup(&root);
        } else {
            let _ = cleanup_stale_history(&root);
        }
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = root.join(format!(
            "session-{stamp}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        let guard = directory_guard(&path)?;
        let marker = path.join(OWNER_FILE);
        let lease = (|| -> io::Result<File> {
            let mut lease = open_lease(&marker, true)?;
            lease.write_all(&owner_bytes(&path)?)?;
            lease.sync_all()?;
            Ok(lease)
        })();
        let lease = match lease {
            Ok(lease) => lease,
            Err(error) => {
                let _ = fs::remove_file(marker);
                drop(guard);
                let _ = fs::remove_dir(path);
                return Err(error.into());
            }
        };
        Ok(Arc::new(Self {
            path,
            files: Mutex::new(BTreeSet::new()),
            lease: Some(lease),
            guard: Some(guard),
        }))
    }

    pub(super) fn remove(&self, path: &Path) -> Result<(), Error> {
        let mut files = self
            .files
            .lock()
            .map_err(|_| error("history ownership lock poisoned"))?;
        if !files.contains(path) || path.parent() != Some(self.path.as_path()) {
            return Err(error("unowned history path"));
        }
        plain_metadata(path)?;
        fs::remove_file(path)?;
        files.remove(path);
        Ok(())
    }
}

impl Drop for CacheDirectory {
    fn drop(&mut self) {
        let files = self.files.get_mut().ok().map(std::mem::take);
        let owned = OwnedCleanup {
            path: std::mem::take(&mut self.path),
            files,
            lease: self.lease.take(),
            guard: self.guard.take(),
        };
        if background::enabled() {
            background::dispose(owned);
        } else {
            owned.run();
        }
    }
}

// Keeps the ownership lease until the worker has finished. Dropping this value
// without running it deliberately leaves a marked, recoverable session behind.
struct OwnedCleanup {
    path: PathBuf,
    files: Option<BTreeSet<PathBuf>>,
    lease: Option<File>,
    guard: Option<File>,
}

impl OwnedCleanup {
    fn run(mut self) {
        let mut removed = true;
        if let Some(files) = &self.files {
            for path in files {
                if path.parent() == Some(self.path.as_path()) && plain_metadata(path).is_ok() {
                    removed &= fs::remove_file(path).is_ok();
                } else if path.exists() {
                    removed = false;
                }
            }
        } else {
            removed = false;
        }
        drop(self.lease.take());
        if removed {
            let _ = fs::remove_file(self.path.join(OWNER_FILE));
        }
        drop(self.guard.take());
        let _ = fs::remove_dir(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn root() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "pvd-cache-cleanup-test-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::canonicalize(root).unwrap()
    }

    fn abandon(root: &Path) -> PathBuf {
        let mut owner = Arc::try_unwrap(CacheDirectory::new(root).unwrap()).unwrap();
        let path = owner.path.clone();
        drop(owner.lease.take());
        drop(owner.guard.take());
        // Model process loss without running the owner's destructor.
        std::mem::forget(owner);
        path
    }

    #[test]
    fn active_snapshots_keep_the_lease_and_abandoned_history_is_reclaimed() {
        let root = root();
        let active = CacheDirectory::new(&root).unwrap();
        let snapshot = active.clone();
        drop(active);
        assert_eq!(cleanup_stale_history(&root).unwrap().removed_sessions, 0);
        assert!(snapshot.path.exists());
        let stale = abandon(&root);
        fs::write(
            stale.join("00000000.pvdhistory"),
            [MAGIC.as_slice(), &[7; 13]].concat(),
        )
        .unwrap();
        let report = cleanup_stale_history(&root).unwrap();
        assert_eq!(report.removed_sessions, 1);
        assert_eq!(report.removed_bytes, 21);
        assert!(snapshot.path.exists());
        assert!(!stale.exists());
        drop(snapshot);
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn unknown_files_markers_and_unmarked_legacy_sessions_are_preserved() {
        let root = root();
        let stale = abandon(&root);
        let raw = stale.join("capture.pvdstream");
        fs::write(&raw, b"permanent recording").unwrap();
        assert_eq!(cleanup_stale_history(&root).unwrap().removed_sessions, 0);
        assert_eq!(fs::read(&raw).unwrap(), b"permanent recording");
        fs::remove_file(raw).unwrap();
        let legacy = root.join("session-1-2-3");
        fs::create_dir(&legacy).unwrap();
        fs::write(legacy.join("00000000.pvdhistory"), MAGIC).unwrap();
        fs::write(stale.join(OWNER_FILE), b"foreign marker").unwrap();
        assert_eq!(cleanup_stale_history(&root).unwrap().removed_sessions, 0);
        assert!(legacy.exists());
        fs::remove_file(stale.join(OWNER_FILE)).unwrap();
        fs::remove_dir(stale).unwrap();
        fs::remove_file(legacy.join("00000000.pvdhistory")).unwrap();
        fs::remove_dir(legacy).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn unknown_or_truncated_history_prevents_partial_directory_cleanup() {
        let root = root();
        let stale = abandon(&root);
        let valid = stale.join("00000000.pvdhistory");
        let invalid = stale.join("00000001.pvdhistory");
        fs::write(&valid, MAGIC).unwrap();
        fs::write(&invalid, b"PVD").unwrap();
        assert_eq!(cleanup_stale_history(&root).unwrap().removed_sessions, 0);
        assert!(valid.exists());
        fs::remove_file(invalid).unwrap();
        assert_eq!(cleanup_stale_history(&root).unwrap().removed_sessions, 1);
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn crash_fixture_child() {
        let Some(root) = std::env::var_os("PVD_CACHE_CRASH_TEST_ROOT") else {
            return;
        };
        let owner = CacheDirectory::new(Path::new(&root)).unwrap();
        fs::write(owner.path.join("00000000.pvdhistory"), MAGIC).unwrap();
        std::process::exit(0);
    }

    #[test]
    fn process_exit_releases_the_lease_for_the_next_session() {
        let root = root();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "live::cache_directory::tests::crash_fixture_child",
            ])
            .env("PVD_CACHE_CRASH_TEST_ROOT", &root)
            .status()
            .unwrap();
        assert!(status.success());
        let report = cleanup_stale_history(&root).unwrap();
        assert_eq!(report.removed_sessions, 1);
        assert_eq!(report.removed_bytes, 8);
        fs::remove_dir(root).unwrap();
    }

    #[test]
    #[cfg(windows)]
    fn active_session_directory_cannot_be_replaced() {
        let root = root();
        let owner = CacheDirectory::new(&root).unwrap();
        assert!(fs::rename(&owner.path, root.join("moved")).is_err());
        assert!(owner.path.exists());
        drop(owner);
        fs::remove_dir(root).unwrap();
    }
}
