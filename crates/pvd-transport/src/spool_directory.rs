use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const OWNER_FILE: &str = ".pvd-studio-owner";
const OWNER_MAGIC: &str = "pvd-studio temporary receive v1\n";
const SEGMENT_MAGIC: &str = "pvd-studio receive segment v1\n";
const REGISTRATION_SUFFIX: &str = ".pvdspool";
const MAX_SCAN_DIRECTORIES: usize = 256;
const MAX_CLEANUP_FILES: usize = 8320;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) struct SpoolDirectory {
    pub(super) path: PathBuf,
    lease: Option<File>,
    guard: Option<File>,
}

#[derive(Debug, Default)]
pub struct SpoolCleanupReport {
    pub removed_sessions: usize,
    /// Valid sessions with a reclaimed batch and more files left for a later pass.
    pub partial_sessions: usize,
    /// Raw segment lengths, excluding ownership records and filesystem allocation overhead.
    pub removed_bytes: u64,
    pub skipped_sessions: usize,
}

fn plain_metadata(path: &Path) -> io::Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::other("receive cleanup refuses symbolic links"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(io::Error::other("receive cleanup refuses reparse points"));
        }
    }
    Ok(metadata)
}

fn no_delete_sharing(options: &mut OpenOptions) {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // A validated owner or segment cannot be replaced while its handle is held.
        options.share_mode(3);
    }
    #[cfg(not(windows))]
    let _ = options;
}

fn directory_guard(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    no_delete_sharing(&mut options);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0200_0000);
    }
    options.open(path)
}

fn open_lease(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    no_delete_sharing(&mut options);
    let file = options.open(path)?;
    file.try_lock().map_err(io::Error::from)?;
    Ok(file)
}

fn session_name(name: &str) -> bool {
    let Some(parts) = name.strip_prefix("spool-") else {
        return false;
    };
    let mut parts = parts.split('-');
    (0..3).all(|_| {
        parts.next().is_some_and(|part| {
            !part.is_empty() && part.len() <= 39 && part.bytes().all(|c| c.is_ascii_digit())
        })
    }) && parts.next().is_none()
}

fn segment_name(name: &str) -> bool {
    let Some((session, index)) = name
        .strip_suffix(".bin")
        .and_then(|name| name.split_once('-'))
    else {
        return false;
    };
    let (Ok(session), Ok(index)) = (session.parse::<u64>(), index.parse::<u64>()) else {
        return false;
    };
    session != 0 && name == format!("{session:016}-{index:016}.bin")
}

fn owner_bytes(path: &Path) -> io::Result<Vec<u8>> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| session_name(name))
        .ok_or_else(|| io::Error::other("invalid receive session name"))?;
    Ok(format!("{OWNER_MAGIC}{name}\n").into_bytes())
}

fn registration_bytes(path: &Path, name: &str) -> io::Result<Vec<u8>> {
    if !segment_name(name) {
        return Err(io::Error::other("invalid receive segment name"));
    }
    let directory = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| session_name(name))
        .ok_or_else(|| io::Error::other("invalid receive session name"))?;
    Ok(format!("{SEGMENT_MAGIC}{directory}\n{name}\n").into_bytes())
}

fn registration_path(path: &Path, name: &str) -> PathBuf {
    path.join(format!("{name}{REGISTRATION_SUFFIX}"))
}

fn verify_bytes(path: &Path, expected: &[u8]) -> io::Result<()> {
    let metadata = plain_metadata(path)?;
    if !metadata.is_file() || metadata.len() != expected.len() as u64 {
        return Err(io::Error::other("invalid receive ownership record"));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    no_delete_sharing(&mut options);
    let mut file = options.open(path)?;
    let mut contents = vec![0; expected.len()];
    file.read_exact(&mut contents)?;
    if contents != expected {
        return Err(io::Error::other("receive ownership record does not match"));
    }
    Ok(())
}

fn remove_stale(root: &Path, path: &Path, max_files: usize) -> io::Result<(u64, bool)> {
    if !plain_metadata(path)?.is_dir() || path.parent() != Some(root) {
        return Err(io::Error::other(
            "receive session is outside the spool root",
        ));
    }
    let guard = directory_guard(path)?;
    if fs::canonicalize(path)? != path {
        return Err(io::Error::other("receive session path changed"));
    }
    let marker = path.join(OWNER_FILE);
    let expected = owner_bytes(path)?;
    verify_bytes(&marker, &expected)?;
    let mut lease = open_lease(&marker)?;
    let mut contents = vec![0; expected.len()];
    lease.read_exact(&mut contents)?;
    if contents != expected {
        return Err(io::Error::other("receive ownership record changed"));
    }
    let mut segments = BTreeSet::new();
    let mut bytes = 0u64;
    // An interrupted large receive may leave more files than one cleanup pass
    // can inspect. Validate a bounded batch before removing any of its pairs.
    for entry in fs::read_dir(path)?.take(max_files.max(1)) {
        let entry = entry?;
        if entry.file_name() == OWNER_FILE {
            continue;
        }
        let metadata = plain_metadata(&entry.path())?;
        if !metadata.is_file() {
            return Err(io::Error::other("receive directory contains a non-file"));
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| io::Error::other("unrecognized receive filename"))?;
        if segment_name(&name) {
            segments.insert(name);
        } else if let Some(name) = name
            .strip_suffix(REGISTRATION_SUFFIX)
            .filter(|name| segment_name(name))
        {
            verify_bytes(&entry.path(), &registration_bytes(path, name)?)?;
            segments.insert(name.to_owned());
        } else {
            return Err(io::Error::other("unrecognized receive directory contents"));
        }
    }
    for name in &segments {
        // A pair can straddle the scan boundary; verify its registration
        // directly instead of rejecting or deleting an unregistered segment.
        verify_bytes(
            &registration_path(path, name),
            &registration_bytes(path, name)?,
        )?;
        match plain_metadata(&path.join(name)) {
            Ok(metadata) if metadata.is_file() => {
                bytes = bytes.saturating_add(metadata.len());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            _ => return Err(io::Error::other("receive segment is not a plain file")),
        }
    }
    for name in &segments {
        let file = path.join(name);
        match plain_metadata(&file) {
            Ok(metadata) if metadata.is_file() => fs::remove_file(file)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            _ => return Err(io::Error::other("receive segment changed during cleanup")),
        }
    }
    for name in &segments {
        let registration = registration_path(path, name);
        verify_bytes(&registration, &registration_bytes(path, name)?)?;
        fs::remove_file(registration)?;
    }
    if fs::read_dir(path)?
        .any(|entry| entry.is_err() || entry.is_ok_and(|entry| entry.file_name() != OWNER_FILE))
    {
        // Keep the ownership marker so another bind can reclaim the next batch.
        return Ok((bytes, false));
    }
    drop(lease);
    fs::remove_file(marker)?;
    drop(guard);
    fs::remove_dir(path)?;
    Ok((bytes, true))
}

/// Reclaims bounded batches from marked, unlocked receive spools.
/// Every selected pair is validated before its batch is deleted; larger sessions
/// retain their ownership marker so later calls can continue reclaiming them.
pub fn cleanup_stale_spools(root: &Path) -> io::Result<SpoolCleanupReport> {
    cleanup_stale_spools_with_limit(root, MAX_CLEANUP_FILES)
}

fn cleanup_stale_spools_with_limit(
    root: &Path,
    max_files: usize,
) -> io::Result<SpoolCleanupReport> {
    if !plain_metadata(root)?.is_dir() {
        return Err(io::Error::other("receive cleanup requires a directory"));
    }
    let root = fs::canonicalize(root)?;
    let _guard = directory_guard(&root)?;
    let mut report = SpoolCleanupReport::default();
    for entry in fs::read_dir(&root)?.take(MAX_SCAN_DIRECTORIES) {
        let entry = entry?;
        if !entry.file_name().to_str().is_some_and(session_name) {
            continue;
        }
        match remove_stale(&root, &entry.path(), max_files) {
            Ok((bytes, removed)) => {
                if removed {
                    report.removed_sessions += 1;
                } else {
                    report.partial_sessions += 1;
                }
                report.removed_bytes = report.removed_bytes.saturating_add(bytes);
            }
            Err(_) => report.skipped_sessions += 1,
        }
    }
    Ok(report)
}

impl SpoolDirectory {
    pub(super) fn new(root: &Path) -> io::Result<Self> {
        fs::create_dir_all(root)?;
        if !plain_metadata(root)?.is_dir() {
            return Err(io::Error::other("receive spool requires a directory"));
        }
        let root = fs::canonicalize(root)?;
        let _ = cleanup_stale_spools(&root);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = root.join(format!(
            "spool-{stamp}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        let guard = match directory_guard(&path) {
            Ok(guard) => guard,
            Err(error) => {
                let _ = fs::remove_dir(&path);
                return Err(error);
            }
        };
        let marker = path.join(OWNER_FILE);
        let mut created = false;
        let lease = (|| -> io::Result<File> {
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            no_delete_sharing(&mut options);
            let mut lease = options.open(&marker)?;
            created = true;
            lease.try_lock().map_err(io::Error::from)?;
            lease.write_all(&owner_bytes(&path)?)?;
            lease.sync_all()?;
            Ok(lease)
        })();
        let lease = match lease {
            Ok(lease) => lease,
            Err(error) => {
                if created {
                    let _ = fs::remove_file(marker);
                }
                drop(guard);
                let _ = fs::remove_dir(path);
                return Err(error);
            }
        };
        Ok(Self {
            path,
            lease: Some(lease),
            guard: Some(guard),
        })
    }

    pub(super) fn create_segment(&self, name: &str) -> io::Result<File> {
        let contents = registration_bytes(&self.path, name)?;
        let path = self.path.join(name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        no_delete_sharing(&mut options);
        let file = options.open(&path)?;
        let marker = registration_path(&self.path, name);
        let mut created = false;
        let registration = (|| -> io::Result<()> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            no_delete_sharing(&mut options);
            let mut record = options.open(&marker)?;
            created = true;
            // Kernel-cached registration survives process exit; incomplete power-loss records stay untouched.
            record.write_all(&contents)
        })();
        if let Err(error) = registration {
            drop(file);
            let _ = fs::remove_file(path);
            if created {
                let _ = fs::remove_file(marker);
            }
            return Err(error);
        }
        Ok(file)
    }

    pub(super) fn remove_segment(&self, name: &str) -> io::Result<()> {
        let expected = registration_bytes(&self.path, name)?;
        let marker = registration_path(&self.path, name);
        verify_bytes(&marker, &expected)?;
        let path = self.path.join(name);
        match plain_metadata(&path) {
            Ok(metadata) if metadata.is_file() => fs::remove_file(path)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            _ => {
                return Err(io::Error::other(
                    "owned receive segment is not a plain file",
                ));
            }
        }
        fs::remove_file(marker)
    }
}

impl Drop for SpoolDirectory {
    fn drop(&mut self) {
        let empty = fs::read_dir(&self.path).is_ok_and(|entries| {
            entries
                .into_iter()
                .all(|entry| entry.is_ok_and(|entry| entry.file_name() == OWNER_FILE))
        });
        drop(self.lease.take());
        if empty {
            let _ = fs::remove_file(self.path.join(OWNER_FILE));
        }
        drop(self.guard.take());
        let _ = fs::remove_dir(&self.path);
    }
}

#[cfg(test)]
mod tests;
