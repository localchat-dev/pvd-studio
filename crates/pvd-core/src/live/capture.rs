use super::*;
use resources::capture::{BlobDescription, BlockDescription, CaptureBacking};
mod atomic;
mod export;
mod metadata;
mod read;
#[cfg(test)]
mod real_tests;
pub(super) mod record;
#[cfg(test)]
mod tests;
mod wire;

pub use atomic::write_retained_new;
pub use export::write_retained;
pub use read::open_with_progress;
pub const MAGIC: &[u8; 8] = b"PVDCAP02";
pub const LEGACY_MAGIC: &[u8; 8] = b"PVDCAP01";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    V1,
    V2,
}

impl Format {
    pub fn from_magic(bytes: &[u8]) -> Result<Option<Self>, Error> {
        if bytes.starts_with(LEGACY_MAGIC) {
            Ok(Some(Self::V1))
        } else if bytes.starts_with(MAGIC) {
            Ok(Some(Self::V2))
        } else if bytes.starts_with(b"PVDCAP") {
            Err(error("unsupported project capture version"))
        } else {
            Ok(None)
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::V1 => "PVDCAP01",
            Self::V2 => "PVDCAP02",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CaptureRange {
    pub first: usize,
    pub last: usize,
}

#[derive(Clone, Debug)]
pub struct CaptureLimits {
    pub index_bytes: usize,
    pub output_bytes: u64,
    pub frames: usize,
    pub resources: usize,
}
impl Default for CaptureLimits {
    fn default() -> Self {
        Self {
            index_bytes: 64 << 20,
            output_bytes: 8 << 30,
            frames: 1_000_000,
            resources: 1_048_576,
        }
    }
}
impl CaptureLimits {
    fn validate(&self) -> Result<(), Error> {
        if self.index_bytes < 4096
            || self.output_bytes < 4096
            || self.output_bytes >= 1 << 63
            || self.frames == 0
            || self.frames > 1_000_000
            || self.resources == 0
            || self.resources > 1_048_576
        {
            return Err(error("invalid project capture limits"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapturePhase {
    Inspect,
    Copy,
    Validate,
}
#[derive(Clone, Copy, Debug)]
pub struct CaptureProgress {
    pub phase: CapturePhase,
    pub completed: u64,
    pub total: u64,
}
#[derive(Clone, Debug)]
pub struct CaptureReport {
    pub format: Format,
    pub range: CaptureRange,
    pub frames: usize,
    pub bytes: u64,
    pub digest: [u8; 32],
    pub resource_count: usize,
    pub block_count: usize,
    pub index_bytes: usize,
}

struct Budget {
    used: usize,
    peak: usize,
    limit: usize,
}
impl Budget {
    fn new(limit: usize) -> Self {
        Self {
            used: 0,
            peak: 0,
            limit,
        }
    }
    fn charge(&mut self, bytes: usize) -> Result<(), Error> {
        self.used = self
            .used
            .checked_add(bytes)
            .filter(|used| *used <= self.limit)
            .ok_or_else(|| error("project capture index exceeds its metadata budget"))?;
        self.peak = self.peak.max(self.used);
        Ok(())
    }
    fn release(&mut self, bytes: usize) {
        self.used = self
            .used
            .checked_sub(bytes)
            .expect("capture budget imbalance");
    }
    fn reserve<T>(&mut self, values: &mut Vec<T>, capacity: usize) -> Result<(), Error> {
        if capacity <= values.capacity() {
            return Ok(());
        }
        self.entries(capacity, size_of::<T>())?;
        let old = values.capacity();
        values.try_reserve_exact(capacity - values.len())?;
        self.entries(values.capacity() - capacity, size_of::<T>())?;
        self.release(old * size_of::<T>());
        Ok(())
    }
    fn push<T>(&mut self, values: &mut Vec<T>, value: T) -> Result<(), Error> {
        if values.len() == values.capacity() {
            let capacity = values
                .capacity()
                .checked_mul(2)
                .map(|n| n.max(4))
                .ok_or_else(|| error("capture capacity overflow"))?;
            self.reserve(values, capacity)?;
        }
        values.push(value);
        Ok(())
    }
    fn entries(&mut self, count: usize, width: usize) -> Result<(), Error> {
        self.charge(
            count
                .checked_mul(width)
                .ok_or_else(|| error("capture metadata size overflow"))?,
        )
    }
}

fn progress(
    callback: &mut impl FnMut(CaptureProgress) -> bool,
    phase: CapturePhase,
    completed: u64,
    total: u64,
) -> Result<(), Error> {
    if !callback(CaptureProgress {
        phase,
        completed,
        total,
    }) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "project capture operation cancelled",
        )
        .into());
    }
    Ok(())
}

struct Metadata {
    first: u64,
    last: u64,
    sdk_version: u32,
    timestamp_numerator: u64,
    timestamp_denominator: u64,
    flags: u32,
    little_endian: bool,
    classes: BTreeMap<String, crate::ClassDefinition>,
    diagnostics: Vec<String>,
}

struct Segment {
    analysis_base: u64,
    previous_frame: Option<usize>,
    continuity: bool,
    frames: Vec<FrameLocation>,
    checkpoints: BTreeMap<usize, u64>,
}

struct Directory {
    format: Format,
    retained_bytes: usize,
    metadata: Metadata,
    segments: Vec<Segment>,
    blocks: Vec<BlockDescription>,
    blobs: Vec<BlobDescription>,
    records: BTreeMap<u64, u64>,
    report: CaptureReport,
}
