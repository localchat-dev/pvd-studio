//! Bounded reading of ZIP32 PVD bundles with one supported recording member.

mod decode;
mod directory;
#[cfg(test)]
mod tests;

use crate::{Error, error};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const BLOCK_BYTES: usize = 64 << 10;
const MAX_ARCHIVE_BYTES: u64 = 512 << 20;
const MAX_MEMBER_BYTES: u64 = 512 << 20;
const MAX_DIRECTORY_BYTES: u64 = 4 << 20;
const MAX_ENTRIES: usize = 4096;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_archive_bytes: u64,
    pub max_member_bytes: u64,
    pub max_directory_bytes: u64,
    pub max_entries: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_archive_bytes: MAX_ARCHIVE_BYTES,
            max_member_bytes: MAX_MEMBER_BYTES,
            max_directory_bytes: MAX_DIRECTORY_BYTES,
            max_entries: MAX_ENTRIES,
        }
    }
}

impl Limits {
    fn check(self) -> Result<(), Error> {
        if self.max_archive_bytes > MAX_ARCHIVE_BYTES
            || self.max_member_bytes > MAX_MEMBER_BYTES
            || self.max_directory_bytes > MAX_DIRECTORY_BYTES
            || self.max_entries > MAX_ENTRIES
        {
            return Err(error("PVD archive limits exceed the supported bounds"));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct Archive {
    pub member_name: String,
    pub bytes: Vec<u8>,
}

pub fn has_zip_signature(prefix: &[u8]) -> bool {
    [b"PK\x03\x04", b"PK\x05\x06", b"PK\x07\x08"]
        .iter()
        .any(|signature| prefix.starts_with(*signature))
}

/// Progress counts decoded member bytes; directory checks report `(0, 0)`.
/// The limits bound input and output separately, not their combined allocation.
pub fn read_with_progress<R: Read + Seek>(
    source: &mut R,
    limits: &Limits,
    mut progress: impl FnMut(u64, u64) -> bool,
) -> Result<Archive, Error> {
    check_progress(&mut progress, 0, 0)?;
    limits.check()?;
    let length = source.seek(SeekFrom::End(0))?;
    if length > limits.max_archive_bytes {
        return Err(error("PVD archive exceeds the compressed input limit"));
    }
    let mut reader = Reader { source, length };
    let mut prefix = [0; 4];
    reader.read_at(0, &mut prefix, &mut progress)?;
    if !has_zip_signature(&prefix) {
        return Err(error("PVD archive has no supported ZIP signature"));
    }
    let member = directory::select(&mut reader, limits, &mut progress)?;
    let bytes = decode::read(&mut reader, &member, limits, &mut progress)?;
    if reader.source.seek(SeekFrom::End(0))? != length {
        return Err(error("PVD archive length changed while reading"));
    }
    let minimum = if bytes.starts_with(&crate::native::MAGIC.to_le_bytes()) {
        16
    } else if bytes.starts_with(&pvd_protocol::STREAM_MAGIC.to_le_bytes())
        || bytes.starts_with(&pvd_protocol::STREAM_MAGIC.to_be_bytes())
    {
        28
    } else {
        return Err(unsupported(
            "PVD archive member is not a supported SDK stream or PVD3 database",
        ));
    };
    if bytes.len() < minimum {
        return Err(error("PVD archive recording header is truncated"));
    }
    check_progress(&mut progress, bytes.len() as u64, bytes.len() as u64)?;
    Ok(Archive {
        member_name: member.name,
        bytes,
    })
}

pub fn open_with_progress(
    path: impl AsRef<Path>,
    limits: &Limits,
    progress: impl FnMut(u64, u64) -> bool,
) -> Result<Archive, Error> {
    read_with_progress(&mut std::fs::File::open(path)?, limits, progress)
}

fn unsupported(message: &'static str) -> Error {
    std::io::Error::new(std::io::ErrorKind::Unsupported, message).into()
}

fn check_progress(
    progress: &mut dyn FnMut(u64, u64) -> bool,
    completed: u64,
    total: u64,
) -> Result<(), Error> {
    if progress(completed, total) {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "PVD archive reading cancelled",
        )
        .into())
    }
}

struct Reader<'a, R> {
    source: &'a mut R,
    length: u64,
}

impl<R: Read + Seek> Reader<'_, R> {
    fn read_at(
        &mut self,
        offset: u64,
        output: &mut [u8],
        progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<(), Error> {
        let end = offset
            .checked_add(output.len() as u64)
            .ok_or_else(|| error("PVD archive range overflows"))?;
        if end > self.length {
            return Err(error("PVD archive range exceeds the input"));
        }
        check_progress(progress, 0, 0)?;
        self.source.seek(SeekFrom::Start(offset))?;
        for chunk in output.chunks_mut(BLOCK_BYTES) {
            check_progress(progress, 0, 0)?;
            self.source.read_exact(chunk)?;
        }
        Ok(())
    }
}

struct Member {
    name: String,
    method: u16,
    crc: u32,
    compressed: u32,
    uncompressed: u32,
    offset: u64,
}
