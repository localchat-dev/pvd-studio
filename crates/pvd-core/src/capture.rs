//! Project capture files preserve retained history; they are not SDK communication or PVD3 files.
pub use crate::live::capture::{
    CaptureLimits, CapturePhase, CaptureProgress, CaptureRange, CaptureReport, Format, MAGIC,
    PACKED_MAGIC, PortableReport, SET_MAGIC, open_packed_with_progress,
    open_segmented_with_progress, write_portable_new, write_retained, write_retained_new,
};

use crate::{Error, Recording};
use std::{io::Read, path::Path};

impl Format {
    pub fn open(
        self,
        path: &Path,
        limits: &CaptureLimits,
        progress: impl FnMut(CaptureProgress) -> bool,
    ) -> Result<Recording, Error> {
        match self {
            Self::V1 | Self::V2 | Self::V3 => {
                crate::live::capture::open_with_progress(path, limits, progress)
            }
            Self::V4 => open_packed_with_progress(path, limits, progress),
            Self::Set => open_segmented_with_progress(path, limits, progress),
        }
    }
}

pub fn open_with_progress(
    path: &Path,
    limits: &CaptureLimits,
    progress: impl FnMut(CaptureProgress) -> bool,
) -> Result<Recording, Error> {
    let mut magic = [0; 8];
    std::fs::File::open(path)?.read_exact(&mut magic)?;
    Format::from_magic(&magic)?
        .ok_or_else(|| crate::error("invalid project capture header"))?
        .open(path, limits, progress)
}
