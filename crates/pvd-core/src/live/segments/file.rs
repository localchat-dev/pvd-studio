use super::*;

#[cfg(not(all(test, feature = "resource-test-write-faults")))]
pub(in crate::live) type SegmentFile = File;

#[cfg(not(all(test, feature = "resource-test-write-faults")))]
pub(super) fn segment_file(file: File) -> SegmentFile {
    file
}

#[cfg(all(test, feature = "resource-test-write-faults"))]
pub(in crate::live) struct SegmentFile {
    pub file: File,
    pub remaining: Option<usize>,
    pub fail_flush: bool,
}

#[cfg(all(test, feature = "resource-test-write-faults"))]
pub(super) fn segment_file(file: File) -> SegmentFile {
    SegmentFile {
        file,
        remaining: None,
        fail_flush: false,
    }
}

#[cfg(all(test, feature = "resource-test-write-faults"))]
impl std::ops::Deref for SegmentFile {
    type Target = File;
    fn deref(&self) -> &File {
        &self.file
    }
}

#[cfg(all(test, feature = "resource-test-write-faults"))]
impl Write for SegmentFile {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.remaining == Some(0) {
            return Err(std::io::Error::other(
                "injected persistent segment write failure",
            ));
        }
        let limit = self.remaining.unwrap_or(bytes.len()).min(bytes.len());
        let written = self.file.write(&bytes[..limit])?;
        if let Some(remaining) = &mut self.remaining {
            *remaining -= written;
        }
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if self.fail_flush {
            return Err(std::io::Error::other("injected segment flush failure"));
        }
        self.file.flush()
    }
}
