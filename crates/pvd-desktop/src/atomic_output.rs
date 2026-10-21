use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) fn create_parent_directory(path: &Path) -> io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

struct TemporaryOutput {
    path: PathBuf,
    file: Option<File>,
    remove_on_drop: bool,
}

impl Drop for TemporaryOutput {
    fn drop(&mut self) {
        self.file = None;
        if self.remove_on_drop {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_with(path, |file| {
        file.write_all(bytes)?;
        file.sync_all()
    })
}

#[cfg(test)]
fn copy_new(source: &Path, destination: &Path, byte_limit: Option<u64>) -> io::Result<u64> {
    let input = File::open(source)?;
    copy_reader_new(input, destination, byte_limit, || true)
}

pub fn copy_reader_new(
    source: impl Read,
    destination: &Path,
    byte_limit: Option<u64>,
    mut continue_copy: impl FnMut() -> bool,
) -> io::Result<u64> {
    let mut input = source.take(byte_limit.unwrap_or(u64::MAX));
    let mut copied = 0;
    write_with(destination, |output| {
        let mut buffer = vec![0; 64 * 1024];
        loop {
            if !continue_copy() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let count = input.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            output.write_all(&buffer[..count])?;
            copied += count as u64;
        }
        if byte_limit.is_some_and(|limit| copied != limit) {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "source ended before the requested capture boundary",
            ));
        }
        output.sync_all()?;
        if !continue_copy() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        Ok(())
    })?;
    Ok(copied)
}

pub fn write_with(path: &Path, writer: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "output needs a file name"))?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = fs::canonicalize(parent)?;
    let destination = parent.join(name);
    match fs::symlink_metadata(&destination) {
        Ok(_) => return Err(io::ErrorKind::AlreadyExists.into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut temporary = None;
    for _ in 0..32 {
        let path = parent.join(format!(
            ".pvdstudio-output-{}-{}.tmp",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                temporary = Some(TemporaryOutput {
                    path,
                    file: Some(file),
                    remove_on_drop: true,
                });
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    let mut temporary = temporary.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "temporary output names are in use",
        )
    })?;
    writer(temporary.file.as_mut().unwrap())?;
    temporary.file = None;
    publish_new(&temporary.path, &destination)?;
    #[cfg(target_os = "windows")]
    {
        temporary.remove_on_drop = false;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn publish_new(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        Win32::Storage::FileSystem::{MOVE_FILE_FLAGS, MoveFileExW},
        core::PCWSTR,
    };
    let wide = |path: &Path| -> io::Result<Vec<u16>> {
        let mut value: Vec<_> = path.as_os_str().encode_wide().collect();
        if value.contains(&0) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        value.push(0);
        Ok(value)
    };
    let source = wide(source)?;
    let destination = wide(destination)?;
    // Zero flags forbid replacement; same-directory staging keeps publication on one volume.
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVE_FILE_FLAGS(0),
        )
    }
    .map_err(|error| io::Error::from_raw_os_error(error.code().0 & 0xffff))
}

#[cfg(not(target_os = "windows"))]
fn publish_new(source: &Path, destination: &Path) -> io::Result<()> {
    fs::hard_link(source, destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../output/atomic-output-tests")
                .join(format!(
                    "{}-{}",
                    std::process::id(),
                    TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
                ));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::create_dir(&path).unwrap();
            Self(fs::canonicalize(path).unwrap())
        }

        fn files(&self) -> Vec<PathBuf> {
            fs::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            for path in self.files() {
                fs::remove_file(path).unwrap();
            }
            fs::remove_dir(&self.0).unwrap();
        }
    }

    #[test]
    fn interrupted_write_removes_its_partial_temp_and_preserves_other_files() {
        let scratch = Scratch::new();
        let destination = scratch.0.join("片段.pvdstream");
        let source = scratch.0.join("original.pvdstream");
        let unrelated = scratch.0.join("unrelated.tmp");
        fs::write(&source, b"original bytes").unwrap();
        fs::write(&unrelated, b"unrelated bytes").unwrap();
        let result = write_with(&destination, |file| {
            file.write_all(b"partial output")?;
            assert!(!destination.exists());
            assert_eq!(scratch.files().len(), 3);
            Err(io::Error::other("injected failure after partial write"))
        });
        assert!(result.is_err());
        assert!(!destination.exists());
        assert_eq!(scratch.files().len(), 2);
        assert_eq!(fs::read(source).unwrap(), b"original bytes");
        assert_eq!(fs::read(unrelated).unwrap(), b"unrelated bytes");
        write_new(&destination, b"complete output").unwrap();
        assert_eq!(fs::read(destination).unwrap(), b"complete output");
        assert_eq!(scratch.files().len(), 3);
    }

    #[test]
    fn copying_can_cancel_before_during_and_after_writing_without_publication() {
        let scratch = Scratch::new();
        let destination = scratch.0.join("cancelled.pvdstream");
        let payload = vec![17; 128 * 1024];
        for stop_at in [1, 2, 4] {
            let mut checks = 0;
            let error = copy_reader_new(payload.as_slice(), &destination, None, || {
                checks += 1;
                checks != stop_at
            })
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Interrupted);
            assert_eq!(checks, stop_at);
            assert!(scratch.files().is_empty());
        }
    }

    #[test]
    fn streamed_copy_preserves_boundaries_and_does_not_publish_truncated_input() {
        let scratch = Scratch::new();
        let source = scratch.0.join("source.pvdstream");
        let payload: Vec<_> = (0..300_000).map(|index| (index % 251) as u8).collect();
        fs::write(&source, &payload).unwrap();
        let complete = scratch.0.join("complete.pvdstream");
        assert_eq!(
            copy_new(&source, &complete, None).unwrap(),
            payload.len() as u64
        );
        assert_eq!(fs::read(&complete).unwrap(), payload);
        let prefix = scratch.0.join("prefix.pvdstream");
        assert_eq!(copy_new(&source, &prefix, Some(100_000)).unwrap(), 100_000);
        assert_eq!(fs::read(&prefix).unwrap(), &payload[..100_000]);
        let incomplete = scratch.0.join("incomplete.pvdstream");
        assert_eq!(
            copy_new(&source, &incomplete, Some(300_001))
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert!(!incomplete.exists());
        assert_eq!(scratch.files().len(), 3);
        assert_eq!(
            copy_new(&source, &source, None).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(source).unwrap(), payload);
    }

    #[test]
    fn publication_race_preserves_the_destination_created_by_another_writer() {
        let scratch = Scratch::new();
        let destination = scratch.0.join("collision.pvdstream");
        let result = write_with(&destination, |file| {
            file.write_all(b"our completed output")?;
            file.sync_all()?;
            fs::write(&destination, b"other writer")
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&destination).unwrap(), b"other writer");
        assert_eq!(scratch.files(), vec![destination.clone()]);
        assert_eq!(
            write_new(&destination, b"replacement").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(destination).unwrap(), b"other writer");
    }

    #[test]
    fn successful_publish_has_no_temp_and_source_aliases_cannot_be_overwritten() {
        let scratch = Scratch::new();
        let destination = scratch.0.join("完整录制.pvdstream");
        let payload = vec![17; 256 * 1024];
        write_new(&destination, &payload).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), payload);
        assert_eq!(scratch.files(), vec![destination.clone()]);
        let alias = scratch.0.join("alias.pvdstream");
        fs::hard_link(&destination, &alias).unwrap();
        assert_eq!(
            write_new(&alias, b"replacement").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(destination).unwrap(), payload);
        assert_eq!(scratch.files().len(), 2);
    }
}
