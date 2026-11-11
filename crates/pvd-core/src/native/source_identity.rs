use super::{Error, cancelled, error};
use std::io::{self, Read, Seek, SeekFrom};

const BLOCK_BYTES: usize = 64 << 10;
const INDEX_BYTES: usize = 64 << 20;

pub(super) struct CheckedSource<'a, R> {
    reader: &'a mut R,
    length: u64,
    position: u64,
    digest: blake3::Hash,
    blocks: Vec<blake3::Hash>,
    cached_block: Option<usize>,
    cache: Box<[u8; BLOCK_BYTES]>,
}

impl<'a, R: Read + Seek> CheckedSource<'a, R> {
    pub(super) fn new(
        reader: &'a mut R,
        mut progress: impl FnMut(u64, u64) -> bool,
    ) -> Result<Self, Error> {
        if !progress(0, 0) {
            return Err(cancelled());
        }
        let length = reader.seek(SeekFrom::End(0))?;
        let count = usize::try_from(length.div_ceil(BLOCK_BYTES as u64))?;
        if count > INDEX_BYTES / std::mem::size_of::<blake3::Hash>() {
            return Err(error("PVD3 source fingerprint index exceeds 64 MiB"));
        }
        reader.rewind()?;
        let mut blocks = Vec::new();
        blocks.try_reserve_exact(count)?;
        let mut cache = Box::new([0; BLOCK_BYTES]);
        let mut hash = blake3::Hasher::new();
        let mut completed = 0;
        while completed < length {
            if !progress(completed, length) {
                return Err(cancelled());
            }
            let size = (length - completed).min(BLOCK_BYTES as u64) as usize;
            reader.read_exact(&mut cache[..size])?;
            hash.update(&cache[..size]);
            blocks.push(blake3::hash(&cache[..size]));
            completed += size as u64;
        }
        if reader.seek(SeekFrom::End(0))? != length {
            return Err(changed().into());
        }
        if !progress(length, length) {
            return Err(cancelled());
        }
        Ok(Self {
            reader,
            length,
            position: 0,
            digest: hash.finalize(),
            blocks,
            cached_block: None,
            cache,
        })
    }

    pub(super) fn len(&self) -> u64 {
        self.length
    }

    pub(super) fn digest(&self) -> blake3::Hash {
        self.digest
    }

    pub(super) fn recheck(
        &mut self,
        mut progress: impl FnMut(u64, u64) -> bool,
    ) -> Result<(), Error> {
        if !progress(0, self.length) {
            return Err(cancelled());
        }
        self.cached_block = None;
        for index in 0..self.blocks.len() {
            let completed = index as u64 * BLOCK_BYTES as u64;
            if !progress(completed, self.length) {
                return Err(cancelled());
            }
            self.load_block(index)?;
        }
        if self.reader.seek(SeekFrom::End(0))? != self.length {
            return Err(changed().into());
        }
        if !progress(self.length, self.length) {
            return Err(cancelled());
        }
        Ok(())
    }

    fn load_block(&mut self, index: usize) -> io::Result<()> {
        if self.cached_block == Some(index) {
            return Ok(());
        }
        let expected = self.blocks.get(index).ok_or_else(changed)?;
        let begin = index as u64 * BLOCK_BYTES as u64;
        let size = (self.length - begin).min(BLOCK_BYTES as u64) as usize;
        self.cached_block = None;
        self.reader.seek(SeekFrom::Start(begin))?;
        self.reader.read_exact(&mut self.cache[..size])?;
        if &blake3::hash(&self.cache[..size]) != expected {
            return Err(changed());
        }
        self.cached_block = Some(index);
        Ok(())
    }
}

impl<R: Read + Seek> Read for CheckedSource<'_, R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() || self.position >= self.length {
            return Ok(0);
        }
        let index = (self.position / BLOCK_BYTES as u64) as usize;
        self.load_block(index)?;
        let offset = (self.position % BLOCK_BYTES as u64) as usize;
        let size = output
            .len()
            .min(BLOCK_BYTES - offset)
            .min((self.length - self.position).min(usize::MAX as u64) as usize);
        output[..size].copy_from_slice(&self.cache[offset..offset + size]);
        self.position += size as u64;
        Ok(size)
    }
}

impl<R: Read + Seek> Seek for CheckedSource<'_, R> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let position = match position {
            SeekFrom::Start(position) => i128::from(position),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
            SeekFrom::End(offset) => i128::from(self.length) + i128::from(offset),
        };
        self.position =
            u64::try_from(position).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        Ok(self.position)
    }
}

fn changed() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "PVD3 source changed during export or import",
    )
}

#[cfg(test)]
mod tests;
