use super::*;

pub fn write_retained_new(
    source: &crate::Recording,
    path: &Path,
    range: CaptureRange,
    limits: &CaptureLimits,
    minimum_free_bytes: u64,
    mut callback: impl FnMut(CaptureProgress) -> bool,
) -> Result<CaptureReport, Error> {
    limits.validate()?;
    progress(&mut callback, CapturePhase::Inspect, 0, 0)?;
    let destination = destination_for_new(source, path)?;
    let parent = destination.parent().unwrap();
    space(parent, minimum_free_bytes, 0)?;
    let (temporary, report) = admitted_temporary(
        source,
        parent,
        range,
        limits,
        minimum_free_bytes,
        &mut callback,
    )?;
    temporary.publish(&destination)?;
    Ok(report)
}

pub(super) fn admitted_temporary(
    source: &crate::Recording,
    parent: &Path,
    range: CaptureRange,
    limits: &CaptureLimits,
    minimum_free_bytes: u64,
    mut callback: impl FnMut(CaptureProgress) -> bool,
) -> Result<(Temporary, CaptureReport), Error> {
    let mut temporary = Temporary::create(parent)?;
    let report = {
        let mut checked = temporary.checked_writer(parent, minimum_free_bytes);
        write_retained(source, &mut checked, range, limits, &mut callback)?
    };
    temporary.close_synced()?;
    // The full reader also budgets the resident resource directory and restoration state.
    drop(read::open_with_progress(
        &temporary.path,
        limits,
        &mut callback,
    )?);
    progress(
        &mut callback,
        CapturePhase::Validate,
        report.bytes,
        report.bytes,
    )?;
    Ok((temporary, report))
}

pub(super) fn destination_for_new(
    source: &crate::Recording,
    path: &Path,
) -> Result<PathBuf, Error> {
    let name = path
        .file_name()
        .ok_or_else(|| error("capture output requires a file name"))?;
    let parent = std::fs::canonicalize(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new(".")),
    )?;
    if source
        .disk
        .as_ref()
        .and_then(|disk| disk._owner.as_ref())
        .is_some_and(|owner| {
            let cache = owner.path.parent().unwrap_or(&owner.path);
            parent.starts_with(cache)
        })
    {
        return Err(error(
            "capture output must be outside its active history cache",
        ));
    }
    let destination = parent.join(name);
    match std::fs::symlink_metadata(&destination) {
        Ok(_) => return Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists).into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(destination)
}

pub(super) struct Temporary {
    pub(super) path: PathBuf,
    file: Option<File>,
}

impl Temporary {
    pub(super) fn checked_writer<'a>(
        &'a mut self,
        parent: &'a Path,
        minimum_free_bytes: u64,
    ) -> impl Read + Write + Seek + 'a {
        SpaceChecked {
            file: self.file.as_mut().unwrap(),
            parent,
            minimum_free_bytes,
        }
    }

    pub(super) fn create(parent: &Path) -> Result<Self, Error> {
        for _ in 0..32 {
            let path = parent.join(format!(
                ".pvd-capture-{}-{}.tmp",
                std::process::id(),
                SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file: Some(file),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(error("capture temporary names are in use"))
    }

    pub(super) fn close_synced(&mut self) -> Result<(), Error> {
        self.file.take().unwrap().sync_all()?;
        Ok(())
    }

    pub(super) fn publish(self, destination: &Path) -> Result<(), Error> {
        publish_new(&self.path, destination)
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        self.file.take();
        let _ = std::fs::remove_file(&self.path);
    }
}
pub(super) fn space(parent: &Path, minimum: u64, additional: u64) -> Result<(), Error> {
    if minimum != 0 {
        let available=available_disk_space(parent)?.ok_or_else(|| error("capture free-space reserve is unsupported on this platform; specify zero explicitly"))?;
        if available
            .checked_sub(additional)
            .is_none_or(|remaining| remaining < minimum)
        {
            return Err(error("capture output would consume its free-space reserve"));
        }
    }
    Ok(())
}
struct SpaceChecked<'a> {
    file: &'a mut File,
    parent: &'a Path,
    minimum_free_bytes: u64,
}
impl Read for SpaceChecked<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.file.read(bytes)
    }
}
impl Seek for SpaceChecked<'_> {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        self.file.seek(position)
    }
}
impl Write for SpaceChecked<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        space(self.parent, self.minimum_free_bytes, bytes.len() as u64)
            .map_err(std::io::Error::other)?;
        self.file.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}
#[cfg(windows)]
fn publish_new(source: &Path, destination: &Path) -> Result<(), Error> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW},
        core::PCWSTR,
    };
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_WRITE_THROUGH,
        )?;
    }
    Ok(())
}
#[cfg(not(windows))]
fn publish_new(source: &Path, destination: &Path) -> Result<(), Error> {
    std::fs::hard_link(source, destination)?;
    Ok(())
}
