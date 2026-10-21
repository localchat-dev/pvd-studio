use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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

pub fn write_bytes_new(path: impl AsRef<Path>, bytes: &[u8]) -> crate::Result<()> {
    write_new(path, |file| {
        file.write_all(bytes)?;
        Ok(())
    })
}

pub fn write_new<T>(
    path: impl AsRef<Path>,
    writer: impl FnOnce(&mut dyn Write) -> crate::Result<T>,
) -> crate::Result<T> {
    write_with(path.as_ref(), writer, File::sync_all)
}

pub fn write_new_cancellable<T>(
    path: impl AsRef<Path>,
    cancel: &crate::cancellation::Cancellation,
    writer: impl FnOnce(&mut dyn Write) -> crate::Result<T>,
) -> crate::Result<T> {
    cancel.check()?;
    write_with(path.as_ref(), writer, |file| {
        file.sync_all()?;
        cancel.check()
    })
}

fn write_with<T>(
    path: &Path,
    writer: impl FnOnce(&mut dyn Write) -> crate::Result<T>,
    synchronize: impl FnOnce(&File) -> io::Result<()>,
) -> crate::Result<T> {
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
        Ok(_) => return Err(io::Error::from(io::ErrorKind::AlreadyExists).into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut temporary = None;
    for _ in 0..32 {
        let path = parent.join(format!(
            ".pvdstudio-cli-output-{}-{}.tmp",
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
            Err(error) => return Err(error.into()),
        }
    }
    let mut temporary = temporary.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "temporary output names are in use",
        )
    })?;
    let value = {
        let mut file = BufWriter::new(temporary.file.as_mut().unwrap());
        let value = writer(&mut file)?;
        file.flush()?;
        value
    };
    synchronize(temporary.file.as_ref().unwrap())?;
    temporary.file = None;
    publish_new(&temporary.path, &destination)?;
    #[cfg(target_os = "windows")]
    {
        temporary.remove_on_drop = false;
    }
    Ok(value)
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
    // Zero flags forbid replacement; staging in the destination directory keeps one volume.
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
                .join("../../output/cli-output-tests")
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

    fn error_kind(error: &crate::Result<()>) -> io::ErrorKind {
        error
            .as_ref()
            .unwrap_err()
            .downcast_ref::<io::Error>()
            .unwrap()
            .kind()
    }

    struct FailingWriter<'a> {
        file: &'a mut dyn Write,
        remaining: usize,
    }

    impl Write for FailingWriter<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Err(io::ErrorKind::StorageFull.into());
            }
            let written = self.file.write(&bytes[..bytes.len().min(self.remaining)])?;
            self.remaining -= written;
            Ok(written)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.file.flush()
        }
    }

    #[test]
    fn write_failure_cleans_only_its_temp_and_allows_retry() {
        let scratch = Scratch::new();
        let destination = scratch.0.join("部分写入.csv");
        let source = scratch.0.join("原始录制.pvdstream");
        let unrelated = scratch.0.join("unrelated.tmp");
        fs::write(&source, b"original capture").unwrap();
        fs::write(&unrelated, b"another task").unwrap();
        let result = write_new(&destination, |file| {
            assert!(!destination.exists());
            let mut failing = FailingWriter { file, remaining: 7 };
            failing.write_all(b"header\npartial row\n")?;
            Ok(())
        });
        assert_eq!(error_kind(&result), io::ErrorKind::StorageFull);
        assert!(!destination.exists());
        assert_eq!(scratch.files().len(), 2);
        assert_eq!(fs::read(source).unwrap(), b"original capture");
        assert_eq!(fs::read(unrelated).unwrap(), b"another task");
        write_bytes_new(&destination, b"header\ncomplete row\n").unwrap();
        assert_eq!(fs::read(destination).unwrap(), b"header\ncomplete row\n");
        assert_eq!(scratch.files().len(), 3);
    }

    #[test]
    fn synchronization_failure_never_publishes_buffered_bytes() {
        let scratch = Scratch::new();
        let destination = scratch.0.join("sync.repx");
        let result = write_with(
            &destination,
            |file| {
                file.write_all(b"<complete/>")?;
                Ok(())
            },
            |file| {
                assert_eq!(file.metadata()?.len(), 11);
                assert!(!destination.exists());
                Err(io::ErrorKind::StorageFull.into())
            },
        );
        assert_eq!(error_kind(&result), io::ErrorKind::StorageFull);
        assert!(scratch.files().is_empty());
    }

    #[test]
    fn interrupted_export_does_not_leave_a_formal_file() {
        let scratch = Scratch::new();
        let destination = scratch.0.join("cancelled.csv");
        let result = write_new(&destination, |file| {
            file.write_all(b"header\n")?;
            Err(io::Error::new(io::ErrorKind::Interrupted, "export cancelled").into())
        });
        assert_eq!(error_kind(&result), io::ErrorKind::Interrupted);
        assert!(scratch.files().is_empty());
    }

    #[test]
    fn cancellation_after_the_writer_returns_prevents_publication() {
        let scratch = Scratch::new();
        let destination = scratch.0.join("cancel-before-publication.csv");
        let cancel = crate::cancellation::Cancellation::install().unwrap();
        let result = write_new_cancellable(&destination, &cancel, |file| {
            file.write_all(b"header\ncomplete row\n")?;
            cancel.request_for_test();
            Ok(())
        });
        assert_eq!(error_kind(&result), io::ErrorKind::Interrupted);
        assert!(scratch.files().is_empty());
    }

    #[test]
    fn competing_publication_is_preserved_and_our_temp_is_removed() {
        let scratch = Scratch::new();
        let destination = scratch.0.join("competition.csv");
        let result = write_new(&destination, |file| {
            file.write_all(b"our output")?;
            let mut competitor = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)?;
            competitor.write_all(b"other output")?;
            Ok(())
        });
        assert_eq!(error_kind(&result), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&destination).unwrap(), b"other output");
        assert_eq!(scratch.files(), vec![destination]);
    }

    #[test]
    fn unicode_output_is_complete_and_source_aliases_are_never_replaced() {
        let scratch = Scratch::new();
        let source = scratch.0.join("捕获输出.pvdstream");
        let payload = vec![31; 256 * 1024];
        write_bytes_new(&source, &payload).unwrap();
        assert_eq!(fs::read(&source).unwrap(), payload);
        assert_eq!(scratch.files(), vec![source.clone()]);
        let alias = scratch.0.join("同一捕获的硬链接.pvdstream");
        fs::hard_link(&source, &alias).unwrap();
        for path in [&source, &alias] {
            let result = write_new(path, |_| {
                panic!("an existing destination must be rejected before writing")
            });
            assert_eq!(error_kind(&result), io::ErrorKind::AlreadyExists);
        }
        assert_eq!(fs::read(source).unwrap(), payload);
        assert_eq!(fs::read(alias).unwrap(), payload);
        assert_eq!(scratch.files().len(), 2);
    }
}
