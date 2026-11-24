use super::*;

pub const SET_MAGIC: &[u8; 8] = b"PVDSET01";
const ENTRY_BYTES: usize = 56;

pub(super) struct Part {
    pub(super) path: PathBuf,
    pub(super) range: CaptureRange,
    pub(super) bytes: u64,
    pub(super) digest: [u8; 32],
}

fn manifest_bytes(count: usize) -> Result<u64, Error> {
    (count as u64)
        .checked_mul(ENTRY_BYTES as u64)
        .and_then(|bytes| bytes.checked_add(24 + 32))
        .ok_or_else(|| error("capture set manifest size overflow"))
}

fn check_location(path: &Path, directory: bool) -> Result<(), Error> {
    let info = std::fs::symlink_metadata(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if info.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
            return Err(error("capture set members must not use reparse points"));
        }
    }
    if (directory && !info.is_dir()) || (!directory && !info.is_file()) {
        return Err(error("invalid capture set member location"));
    }
    Ok(())
}

pub(super) fn check_part_location(path: &Path) -> Result<(), Error> {
    let dir = path
        .parent()
        .ok_or_else(|| error("invalid capture set part path"))?;
    check_location(dir, true)?;
    check_location(path, false)
}

pub(super) fn read_manifest_with_progress(
    path: &Path,
    limits: &CaptureLimits,
    callback: &mut impl FnMut(CaptureProgress) -> bool,
) -> Result<Vec<Part>, Error> {
    limits.validate()?;
    let path = std::fs::canonicalize(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| error("invalid capture set path"))?;
    let file = File::open(&path)?;
    let bytes = file.metadata()?.len();
    let mut reader = std::io::BufReader::new(file);
    let mut header = [0; 24];
    reader.read_exact(&mut header)?;
    if &header[..8] != SET_MAGIC {
        return Err(error("unsupported capture set version"));
    }
    let group = u64::from_le_bytes(header[8..16].try_into()?);
    let count = usize::try_from(u64::from_le_bytes(header[16..].try_into()?))?;
    if count == 0 || count > MAX_INDEX_FRAMES || bytes != manifest_bytes(count)? {
        return Err(error("invalid capture set manifest length or part count"));
    }
    let mut budget = Budget::new(limits.index_bytes);
    let mut parts = Vec::new();
    budget.reserve(&mut parts, count)?;
    let dir = parent.join(format!("pvdset-{group:016x}.parts"));
    check_location(&dir, true).map_err(|failure| read::part_error(&dir, failure))?;
    let mut hash = blake3::Hasher::new();
    hash.update(&header);
    let mut next = None;
    let mut frames = 0usize;
    for index in 0..count {
        progress(callback, CapturePhase::Inspect, index as u64, count as u64)?;
        let mut entry = [0; ENTRY_BYTES];
        reader.read_exact(&mut entry)?;
        hash.update(&entry);
        let mut input = wire::Input::new(&entry);
        let first = input.usize()?;
        let last = input.usize()?;
        let bytes = input.u64()?;
        let visible = last
            .checked_sub(first)
            .and_then(|n| n.checked_add(1))
            .filter(|&n| n <= limits.frames)
            .ok_or_else(|| error("invalid capture set frame range"))?;
        frames = frames
            .checked_add(visible)
            .filter(|&n| n <= MAX_INDEX_FRAMES)
            .ok_or_else(|| error("capture set exceeds its frame index limit"))?;
        if next.is_some_and(|expected| first != expected)
            || bytes == 0
            || bytes > limits.output_bytes
        {
            return Err(error("invalid capture set part range or byte count"));
        }
        next = Some(
            last.checked_add(1)
                .ok_or_else(|| error("capture set frame index overflow"))?,
        );
        let path = dir.join(format!("{:08}.pvdcapture", index + 1));
        budget.charge(path.capacity())?;
        check_location(&path, false).map_err(|failure| read::part_error(&path, failure))?;
        parts.push(Part {
            path,
            range: CaptureRange { first, last },
            bytes,
            digest: input.take(32)?.try_into()?,
        });
    }
    let mut digest = [0; 32];
    reader.read_exact(&mut digest)?;
    if digest != *hash.finalize().as_bytes() {
        return Err(error("capture set manifest checksum mismatch"));
    }
    Ok(parts)
}
