use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const ROOT_LOCK: &str = ".pvd-studio-profile-lock";
const ROOT_MAGIC: &[u8] = b"pvd-studio derived CPU profile root v1\n";
const OWNER_FILE: &str = ".pvd-studio-profile-owner";
const OWNER_MAGIC: &str = "pvd-studio derived CPU profile session v1\n";
const INDEX_DIRECTORY: &str = "index";
const CATALOG_DIRECTORY: &str = "catalog";
const DISK_BUDGET: u64 = 1 << 30;
const FREE_RESERVE: u64 = 512 << 20;
const MAX_ROOT_ENTRIES: usize = 256;
const MAX_SESSION_FILES: usize = 16_384;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

type Space = Arc<dyn Fn(&Path) -> io::Result<Option<u64>> + Send + Sync>;

pub(crate) struct Lease {
    root: PathBuf,
    session: PathBuf,
    workspace: PathBuf,
    disk_budget: u64,
    reserve: u64,
    space: Space,
    marker: Option<File>,
    session_guard: Option<File>,
    root_lock: Option<File>,
    root_guard: Option<File>,
}

impl Lease {
    pub fn acquire(root: &Path, cancelled: impl Fn() -> bool) -> io::Result<Arc<Self>> {
        if !cfg!(windows) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "managed profile cache requires Windows handle deletion",
            ));
        }
        Self::acquire_with(
            root,
            &cancelled,
            DISK_BUDGET,
            FREE_RESERVE,
            Arc::new(|path| pvd_core::live::available_disk_space(path).map_err(io::Error::other)),
        )
    }

    fn acquire_with(
        root: &Path,
        cancelled: &impl Fn() -> bool,
        budget: u64,
        reserve: u64,
        space: Space,
    ) -> io::Result<Arc<Self>> {
        check_cancelled(cancelled)?;
        check_existing_ancestors(root)?;
        fs::create_dir_all(root)?;
        check_existing_ancestors(root)?;
        // Cache parents are trusted; this guard pins and checks the final directory entry.
        let root_guard = directory_guard(root, false)?;
        let root = fs::canonicalize(root)?;
        let root_lock = acquire_root_lock(&root, cancelled, &|| {
            check_space(&root, &space, 0, ROOT_MAGIC.len() as u64)
        })?;
        let retained = reclaim_root(&root, cancelled)?;
        check_cancelled(cancelled)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let session = root.join(format!(
            "session-{stamp}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let marker_bytes = owner_bytes(&session)?;
        let disk_budget = budget
            .checked_sub(retained)
            .and_then(|remaining| remaining.checked_sub(marker_bytes.len() as u64))
            .filter(|&remaining| remaining >= 80)
            .ok_or_else(|| {
                io::Error::other(format!(
                    "profile cache disk budget is exhausted by {retained} retained bytes"
                ))
            })?;
        check_space(&root, &space, reserve, marker_bytes.len() as u64)?;
        fs::create_dir(&session)?;
        let session_guard = directory_guard(&session, true)?;
        let marker = session.join(OWNER_FILE);
        let mut owner = match open_owner(&marker, true) {
            Ok(owner) => owner,
            Err(error) => {
                let _ = delete_by_handle(session_guard);
                return Err(error);
            }
        };
        if let Err(error) = owner
            .write_all(&marker_bytes)
            .and_then(|()| owner.sync_all())
        {
            let _ = delete_by_handle(owner);
            let _ = delete_by_handle(session_guard);
            return Err(error);
        }
        let lease = Arc::new(Self {
            workspace: session.join(INDEX_DIRECTORY),
            root,
            session,
            disk_budget,
            reserve,
            space,
            marker: Some(owner),
            session_guard: Some(session_guard),
            root_lock: Some(root_lock),
            root_guard: Some(root_guard),
        });
        check_cancelled(cancelled)?;
        Ok(lease)
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub fn catalog_workspace(&self) -> PathBuf {
        self.session.join(CATALOG_DIRECTORY)
    }

    pub fn disk_budget(&self) -> u64 {
        self.disk_budget
    }

    pub fn check_write(&self, additional_bytes: u64) -> io::Result<()> {
        if additional_bytes > self.disk_budget {
            return Err(io::Error::other(
                "profile cache write exceeds its disk budget",
            ));
        }
        check_space(&self.root, &self.space, self.reserve, additional_bytes)
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let (Some(marker), Some(guard)) = (self.marker.take(), self.session_guard.take()) {
            let _ = inspect_session_owned(
                &self.root,
                &self.session,
                &|| false,
                Some(guard),
                Some(marker),
            )
            .and_then(|contents| reclaim_contents(&self.session, contents, &|| false));
        }
        // The permanent root lock serializes cleanup with the last in-flight index reader.
        drop(self.root_lock.take());
        drop(self.root_guard.take());
    }
}

fn check_cancelled(cancelled: &impl Fn() -> bool) -> io::Result<()> {
    if cancelled() {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "profile cache operation cancelled",
        ))
    } else {
        Ok(())
    }
}

fn checked_add(a: u64, b: u64) -> io::Result<u64> {
    a.checked_add(b)
        .ok_or_else(|| io::Error::other("profile cache byte count overflow"))
}

fn check_space(root: &Path, space: &Space, reserve: u64, additional: u64) -> io::Result<()> {
    let required = checked_add(reserve, additional)?;
    let available = space(root)?
        .ok_or_else(|| io::Error::other("profile cache free disk space is unavailable"))?;
    if available < required {
        return Err(io::Error::other(format!(
            "profile cache needs to preserve {reserve} free bytes before writing {additional} bytes"
        )));
    }
    Ok(())
}

fn plain_metadata(path: &Path) -> io::Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    check_plain(&metadata)?;
    Ok(metadata)
}

fn check_plain(metadata: &fs::Metadata) -> io::Result<()> {
    if metadata.file_type().is_symlink() {
        return Err(io::Error::other("profile cache refuses symbolic links"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(io::Error::other("profile cache refuses reparse points"));
        }
    }
    Ok(())
}

fn check_existing_ancestors(path: &Path) -> io::Result<()> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    for ancestor in absolute.ancestors() {
        match plain_metadata(ancestor) {
            Ok(metadata) if metadata.is_dir() => (),
            Ok(_) => return Err(io::Error::other("profile cache parent is not a directory")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn directory_guard(path: &Path, delete: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options
            .share_mode(3)
            .custom_flags(0x0200_0000 | 0x0020_0000);
        if delete {
            options.access_mode(0x8000_0000 | 0x0001_0000);
        }
    }
    #[cfg(not(windows))]
    let _ = delete;
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    check_plain(&metadata)?;
    if !metadata.is_dir() {
        return Err(io::Error::other("profile cache path is not a directory"));
    }
    Ok(file)
}

fn open_lock(path: &Path, create: bool) -> io::Result<File> {
    open_control(path, create, false)
}

fn open_owner(path: &Path, create: bool) -> io::Result<File> {
    open_control(path, create, true)
}

fn open_control(path: &Path, create: bool, delete: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(create);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(3).custom_flags(0x0020_0000);
        if delete {
            options.access_mode(0xc000_0000 | 0x0001_0000);
        }
    }
    #[cfg(not(windows))]
    let _ = delete;
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    check_plain(&metadata)?;
    if !metadata.is_file() {
        return Err(io::Error::other(
            "profile cache control is not a plain file",
        ));
    }
    file.try_lock().map_err(io::Error::from)?;
    Ok(file)
}

fn delete_by_handle(file: File) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{
            Foundation::HANDLE,
            Storage::FileSystem::{
                FILE_DISPOSITION_INFO, FileDispositionInfo, SetFileInformationByHandle,
            },
        };
        let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
        // DELETE access and denied delete sharing keep validation and deletion on one object.
        unsafe {
            SetFileInformationByHandle(
                HANDLE(file.as_raw_handle()),
                FileDispositionInfo,
                (&disposition as *const FILE_DISPOSITION_INFO).cast(),
                size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        }
        .map_err(io::Error::other)
    }
    #[cfg(not(windows))]
    {
        drop(file);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "managed profile cache deletion requires Windows",
        ))
    }
}

fn acquire_root_lock(
    root: &Path,
    cancelled: &impl Fn() -> bool,
    before_create: &impl Fn() -> io::Result<()>,
) -> io::Result<File> {
    let path = root.join(ROOT_LOCK);
    loop {
        check_cancelled(cancelled)?;
        let (file, created) = match open_lock(&path, false) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // The small permanent control enables reclamation below the payload reserve.
                before_create()?;
                match open_lock(&path, true) {
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    file => (file, true),
                }
            }
            file => (file, false),
        };
        match file {
            Ok(mut file) => {
                if created {
                    file.write_all(ROOT_MAGIC)?;
                    file.sync_all()?;
                } else {
                    if file.metadata()?.len() != ROOT_MAGIC.len() as u64 {
                        return Err(io::Error::other("unrecognized profile cache root control"));
                    }
                    let mut contents = vec![0; ROOT_MAGIC.len()];
                    file.read_exact(&mut contents)?;
                    if contents != ROOT_MAGIC {
                        return Err(io::Error::other("unrecognized profile cache root control"));
                    }
                }
                return Ok(file);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::park_timeout(Duration::from_millis(10));
            }
            Err(error) => return Err(error),
        }
    }
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

fn owner_bytes(session: &Path) -> io::Result<Vec<u8>> {
    let name = session
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| session_name(name))
        .ok_or_else(|| io::Error::other("invalid profile cache session name"))?;
    Ok(format!("{OWNER_MAGIC}{name}\n").into_bytes())
}

fn index_id(name: &str) -> Option<u32> {
    let number = name.strip_suffix(".idx")?;
    if number.len() != 8
        || !number
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return None;
    }
    u32::from_str_radix(number, 16)
        .ok()
        .filter(|&id| id < MAX_SESSION_FILES as u32)
}

fn open_index(path: &Path, delete: bool) -> io::Result<(File, bool)> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1).custom_flags(0x0020_0000);
        if delete {
            options.access_mode(0x8000_0000 | 0x0001_0000);
        }
    }
    #[cfg(not(windows))]
    let _ = delete;
    let mut file = options.open(path)?;
    let metadata = file.metadata()?;
    check_plain(&metadata)?;
    if !metadata.is_file() {
        return Err(io::Error::other("profile index is not a plain file"));
    }
    if metadata.len() < 8 {
        return Ok((file, false));
    }
    let mut header = [0; 8];
    file.read_exact(&mut header)?;
    Ok((
        file,
        matches!(&header, b"PVDSIDX1" | b"PVDNAME1" | b"PVDCAT01"),
    ))
}

struct SessionContents {
    bytes: u64,
    recognized: bool,
    indexes: Vec<IndexContents>,
    marker: Option<File>,
    guard: File,
}

struct IndexContents {
    name: &'static str,
    ids: Vec<u32>,
    guard: File,
}

fn inspect_session(
    root: &Path,
    session: &Path,
    cancelled: &impl Fn() -> bool,
) -> io::Result<SessionContents> {
    inspect_session_owned(root, session, cancelled, None, None)
}

fn inspect_session_owned(
    root: &Path,
    session: &Path,
    cancelled: &impl Fn() -> bool,
    guard: Option<File>,
    mut marker: Option<File>,
) -> io::Result<SessionContents> {
    if session.parent() != Some(root) || fs::canonicalize(session)? != session {
        return Err(io::Error::other(
            "profile session is outside its cache root",
        ));
    }
    let guard = match guard {
        Some(guard) => guard,
        None => directory_guard(session, true)?,
    };
    let expected = owner_bytes(session)?;
    let mut result = SessionContents {
        bytes: 0,
        recognized: true,
        indexes: Vec::new(),
        marker: None,
        guard,
    };
    for (position, entry) in fs::read_dir(session)?.enumerate() {
        check_cancelled(cancelled)?;
        if position >= MAX_SESSION_FILES + 3 {
            return Err(io::Error::other("profile session entry limit exceeded"));
        }
        let entry = entry?;
        let path = entry.path();
        let metadata = plain_metadata(&path)?;
        let index_name = [INDEX_DIRECTORY, CATALOG_DIRECTORY]
            .into_iter()
            .find(|name| entry.file_name() == *name);
        if let Some(name) = index_name.filter(|_| metadata.is_dir()) {
            let index_guard = directory_guard(&path, true)?;
            let mut ids = Vec::new();
            if fs::canonicalize(&path)? != path {
                return Err(io::Error::other("profile index directory path changed"));
            }
            for (position, entry) in fs::read_dir(&path)?.enumerate() {
                check_cancelled(cancelled)?;
                if position >= MAX_SESSION_FILES {
                    return Err(io::Error::other("profile index file limit exceeded"));
                }
                let entry = entry?;
                let metadata = plain_metadata(&entry.path())?;
                if !metadata.is_file() {
                    return Err(io::Error::other(
                        "profile index contains an unknown directory",
                    ));
                }
                result.bytes = checked_add(result.bytes, metadata.len())?;
                let id = entry.file_name().to_str().and_then(index_id);
                if let Some(id) = id
                    && open_index(&entry.path(), false)?.1
                {
                    ids.try_reserve(1).map_err(io::Error::other)?;
                    ids.push(id);
                } else {
                    result.recognized = false;
                }
            }
            result.indexes.push(IndexContents {
                name,
                ids,
                guard: index_guard,
            });
        } else if metadata.is_file() {
            result.bytes = checked_add(result.bytes, metadata.len())?;
            if entry.file_name() == OWNER_FILE && metadata.len() == expected.len() as u64 {
                let mut marker = match marker.take() {
                    Some(marker) => marker,
                    None => open_owner(&path, false)?,
                };
                let metadata = marker.metadata()?;
                check_plain(&metadata)?;
                if !metadata.is_file() || metadata.len() != expected.len() as u64 {
                    return Err(io::Error::other("profile ownership record changed"));
                }
                marker.rewind()?;
                let mut bytes = vec![0; expected.len()];
                marker.read_exact(&mut bytes)?;
                if bytes == expected {
                    result.marker = Some(marker);
                } else {
                    result.recognized = false;
                }
            } else {
                result.recognized = false;
            }
        } else {
            return Err(io::Error::other(
                "profile session contains an unknown directory",
            ));
        }
    }
    result.recognized &= result.marker.is_some();
    Ok(result)
}

fn reclaim_session(root: &Path, session: &Path, cancelled: &impl Fn() -> bool) -> io::Result<u64> {
    reclaim_contents(
        session,
        inspect_session(root, session, cancelled)?,
        cancelled,
    )
}

fn reclaim_contents(
    session: &Path,
    mut contents: SessionContents,
    cancelled: &impl Fn() -> bool,
) -> io::Result<u64> {
    if !contents.recognized {
        return Ok(contents.bytes);
    }
    for index in contents.indexes {
        for id in index.ids {
            check_cancelled(cancelled)?;
            let path = session.join(index.name).join(format!("{id:08x}.idx"));
            let (file, recognized) = open_index(&path, true)?;
            if !recognized {
                return Err(io::Error::other(
                    "profile index file changed during cleanup",
                ));
            }
            delete_by_handle(file)?;
        }
        delete_by_handle(index.guard)?;
    }
    delete_by_handle(contents.marker.take().unwrap())?;
    delete_by_handle(contents.guard)?;
    Ok(0)
}

fn reclaim_root(root: &Path, cancelled: &impl Fn() -> bool) -> io::Result<u64> {
    let mut retained = 0;
    for (position, entry) in fs::read_dir(root)?.enumerate() {
        check_cancelled(cancelled)?;
        if position >= MAX_ROOT_ENTRIES {
            return Err(io::Error::other("profile cache root entry limit exceeded"));
        }
        let entry = entry?;
        let metadata = plain_metadata(&entry.path())?;
        let bytes = if metadata.is_file() {
            metadata.len()
        } else if metadata.is_dir() && entry.file_name().to_str().is_some_and(session_name) {
            reclaim_session(root, &entry.path(), cancelled)?
        } else {
            return Err(io::Error::other(
                "profile cache contains an unknown directory",
            ));
        };
        retained = checked_add(retained, bytes)?;
    }
    Ok(retained)
}

#[cfg(all(test, windows))]
mod tests;
