use std::{
    fs::File,
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    path::Path,
    slice,
};

use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE},
        Storage::FileSystem::{FILE_SHARE_DELETE, FILE_SHARE_READ},
        System::Memory::{
            CreateFileMappingW, FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
            PAGE_READONLY, UnmapViewOfFile,
        },
    },
    core::PCWSTR,
};

pub(super) struct MappedFont {
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    len: usize,
    _file: File,
}

// The file handle excludes writers for the lifetime of the shared read-only view.
unsafe impl Send for MappedFont {}
unsafe impl Sync for MappedFont {}

impl MappedFont {
    pub(super) fn open(path: &Path) -> Option<Self> {
        let file = File::options()
            .read(true)
            .share_mode((FILE_SHARE_READ | FILE_SHARE_DELETE).0)
            .open(path)
            .ok()?;
        let metadata = file.metadata().ok()?;
        let len = usize::try_from(metadata.len()).ok()?;
        if !metadata.is_file() || len == 0 || len > isize::MAX as usize {
            return None;
        }

        // SAFETY: The unnamed mapping uses the live file handle and exposes a read-only view.
        let view = unsafe {
            let mapping = CreateFileMappingW(
                HANDLE(file.as_raw_handle()),
                None,
                PAGE_READONLY,
                0,
                0,
                PCWSTR::null(),
            )
            .ok()?;
            let view = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, len);
            let _ = CloseHandle(mapping);
            view
        };
        if view.Value.is_null() {
            return None;
        }
        Some(Self {
            view,
            len,
            _file: file,
        })
    }

    pub(super) fn as_slice(&self) -> &[u8] {
        // SAFETY: The complete view is mapped and immutable until it is dropped.
        unsafe { slice::from_raw_parts(self.view.Value.cast(), self.len) }
    }
}

impl Drop for MappedFont {
    fn drop(&mut self) {
        // SAFETY: This instance owns the view and borrows cannot outlive it.
        let _ = unsafe { UnmapViewOfFile(self.view) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::ERROR_SHARING_VIOLATION;

    #[test]
    fn mapped_font_excludes_writers_and_survives_rename() {
        let directory = std::env::temp_dir().join(format!(
            "pvd-font-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("font.ttf");
        let previous = directory.join("replaced.ttf");
        std::fs::write(&path, b"original font").unwrap();

        let mapped = MappedFont::open(&path).unwrap();
        let error = File::options().write(true).open(&path).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(ERROR_SHARING_VIOLATION.0 as i32));
        std::fs::rename(&path, &previous).unwrap();
        std::fs::write(&path, b"replacement font").unwrap();
        assert_eq!(mapped.as_slice(), b"original font");

        drop(mapped);
        File::options().write(true).open(&previous).unwrap();
        std::fs::remove_file(path).unwrap();
        std::fs::remove_file(previous).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
