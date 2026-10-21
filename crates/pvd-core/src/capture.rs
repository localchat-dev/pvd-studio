//! Project capture files preserve retained history; they are not SDK communication or PVD3 files.
pub use crate::live::capture::{
    CaptureLimits, CapturePhase, CaptureProgress, CaptureRange, CaptureReport, Format, MAGIC,
    open_with_progress, write_retained, write_retained_new,
};
