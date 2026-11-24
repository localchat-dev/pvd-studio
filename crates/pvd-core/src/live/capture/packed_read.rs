use super::*;
use persistent::BankLocation;
use segmented_read::Member;
use std::io::{self, SeekFrom};

pub fn open_packed_with_progress(
    path: &Path,
    limits: &CaptureLimits,
    mut callback: impl FnMut(CaptureProgress) -> bool,
) -> Result<crate::Recording, Error> {
    let path: Arc<Path> = std::fs::canonicalize(path)?.into();
    let mut file = read::open_file(&path)?;
    let size = file.metadata()?.len();
    let parts = packed::read_directory(&mut file, limits, &mut callback)?;
    let file = Arc::new(file);
    let range = CaptureRange {
        first: parts[0].range.first,
        last: parts.last().unwrap().range.last,
    };
    let members = parts.into_iter().map(|part| Member {
        location: BankLocation::Embedded {
            path: path.clone(),
            file: file.clone(),
            offset: part.offset,
        },
        range: part.range,
        bytes: part.bytes,
        digest: part.digest,
    });
    segmented_read::open_members(members, range, size, Format::V4, limits, callback)
}

// Separate logical positions allow concurrent readers to use one open file.
pub(super) struct FileView<'a> {
    file: &'a File,
    base: u64,
    len: u64,
    position: u64,
}

impl<'a> FileView<'a> {
    pub(super) fn new(file: &'a File, base: u64, len: u64) -> Result<Self, Error> {
        let end = file.metadata()?.len();
        if base.checked_add(len).is_none_or(|boundary| boundary > end) {
            return Err(error("capture member extends past the file"));
        }
        Ok(Self {
            file,
            base,
            len,
            position: 0,
        })
    }
}

impl Read for FileView<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let remaining = usize::try_from(self.len - self.position).unwrap_or(usize::MAX);
        let count = bytes.len().min(remaining);
        if count == 0 {
            return Ok(0);
        }
        let n =
            resources::capture::read_at(self.file, self.base + self.position, &mut bytes[..count])?;
        self.position += n as u64;
        Ok(n)
    }
}

impl Seek for FileView<'_> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let position = match from {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::Current(offset) => self.position.checked_add_signed(offset),
            SeekFrom::End(offset) => self.len.checked_add_signed(offset),
        };
        self.position = position
            .filter(|position| *position <= self.len)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "seek outside capture member")
            })?;
        Ok(self.position)
    }
}
