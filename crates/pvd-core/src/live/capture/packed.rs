use super::*;
use std::io::SeekFrom;

pub const PACKED_MAGIC: &[u8; 8] = b"PVDCAP04";
const PART_FRAMES: usize = 4096;
const PARTS_BEFORE_GROWTH: usize = 4;
const ENTRY_BYTES: usize = 64;
const FOOTER_BYTES: u64 = 48;

#[derive(Debug, Clone)]
pub struct PortableReport {
    pub frames: usize,
    pub bytes: u64,
    pub parts: usize,
}

#[derive(Debug)]
pub(super) struct PackedPart {
    pub(super) range: CaptureRange,
    pub(super) offset: u64,
    pub(super) bytes: u64,
    pub(super) digest: [u8; 32],
}

impl PackedPart {
    fn encode(&self) -> [u8; ENTRY_BYTES] {
        let mut entry = [0; ENTRY_BYTES];
        entry[..8].copy_from_slice(&(self.range.first as u64).to_le_bytes());
        entry[8..16].copy_from_slice(&(self.range.last as u64).to_le_bytes());
        entry[16..24].copy_from_slice(&self.offset.to_le_bytes());
        entry[24..32].copy_from_slice(&self.bytes.to_le_bytes());
        entry[32..].copy_from_slice(&self.digest);
        entry
    }

    fn decode(entry: &[u8; ENTRY_BYTES]) -> Result<Self, Error> {
        let mut reader = wire::Input::new(entry);
        Ok(Self {
            range: CaptureRange {
                first: reader.usize()?,
                last: reader.usize()?,
            },
            offset: reader.u64()?,
            bytes: reader.u64()?,
            digest: reader.take(32)?.try_into()?,
        })
    }
}

pub(super) fn read_directory(
    file: &mut (impl Read + Seek),
    limits: &CaptureLimits,
    callback: &mut impl FnMut(CaptureProgress) -> bool,
) -> Result<Vec<PackedPart>, Error> {
    limits.validate()?;
    file.rewind()?;
    let mut magic = [0; 8];
    file.read_exact(&mut magic)?;
    if &magic != PACKED_MAGIC {
        return Err(error("invalid packed capture header"));
    }
    let footer_offset = file.seek(SeekFrom::End(-(FOOTER_BYTES as i64)))?;
    let mut footer = [0; FOOTER_BYTES as usize];
    file.read_exact(&mut footer)?;
    let mut fields = wire::Input::new(&footer);
    let count = fields.usize()?;
    let directory_offset = fields.u64()?;
    if count == 0
        || count > MAX_INDEX_FRAMES
        || directory_offset < 8
        || directory_offset.checked_add(count as u64 * ENTRY_BYTES as u64) != Some(footer_offset)
    {
        return Err(error("invalid packed capture directory extent"));
    }
    let mut parts = Vec::new();
    Budget::new(limits.index_bytes).reserve(&mut parts, count)?;
    file.seek(SeekFrom::Start(directory_offset))?;
    let mut reader = std::io::BufReader::new(file);
    let mut digest = blake3::Hasher::new();
    digest.update(PACKED_MAGIC);
    let mut offset = 8u64;
    let mut next_frame = None;
    let mut visible_frames = 0usize;
    for index in 0..count {
        progress(callback, CapturePhase::Inspect, index as u64, count as u64)?;
        let mut entry = [0; ENTRY_BYTES];
        reader.read_exact(&mut entry)?;
        digest.update(&entry);
        let part = PackedPart::decode(&entry)?;
        if part.offset != offset || part.bytes <= 8 || part.bytes > limits.output_bytes {
            return Err(error("invalid packed capture part extent"));
        }
        offset = offset
            .checked_add(part.bytes)
            .filter(|&n| n <= directory_offset)
            .ok_or_else(|| error("packed capture part extends outside the payload"))?;
        let frames = part
            .range
            .last
            .checked_sub(part.range.first)
            .and_then(|n| n.checked_add(1))
            .filter(|&n| n <= limits.frames)
            .ok_or_else(|| error("invalid packed capture part frame count"))?;
        if next_frame.is_some_and(|first| first != part.range.first) {
            return Err(error("packed capture parts have a frame gap or overlap"));
        }
        next_frame = Some(
            part.range
                .last
                .checked_add(1)
                .ok_or_else(|| error("packed capture frame index overflow"))?,
        );
        visible_frames = visible_frames
            .checked_add(frames)
            .filter(|&n| n <= MAX_INDEX_FRAMES)
            .ok_or_else(|| error("packed capture exceeds the global frame index limit"))?;
        parts.push(part);
    }
    digest.update(&footer[..16]);
    if offset != directory_offset || digest.finalize().as_bytes() != &footer[16..] {
        return Err(error(
            "packed capture directory extent or checksum mismatch",
        ));
    }
    Ok(parts)
}

pub fn write_portable_new(
    source: &crate::Recording,
    path: &Path,
    range: CaptureRange,
    limits: &CaptureLimits,
    minimum_free_bytes: u64,
    mut callback: impl FnMut(CaptureProgress) -> bool,
) -> Result<PortableReport, Error> {
    limits.validate()?;
    let frames = range
        .last
        .checked_sub(range.first)
        .and_then(|n| n.checked_add(1))
        .filter(|&n| n <= MAX_INDEX_FRAMES && range.last < usize::MAX)
        .ok_or_else(|| error("invalid portable capture frame range"))?;
    let maximum = limits.frames.min(PART_FRAMES);
    let mut target = maximum;
    if frames <= target {
        match write_retained_new(
            source,
            path,
            range,
            limits,
            minimum_free_bytes,
            &mut callback,
        ) {
            Ok(report) => {
                return Ok(PortableReport {
                    frames,
                    bytes: report.bytes,
                    parts: 1,
                });
            }
            Err(failure) if frames > 1 && crate::CapacityLimit::caused(&failure) => {
                target = frames.div_ceil(2);
            }
            Err(failure) => return Err(failure),
        }
    }
    progress(&mut callback, CapturePhase::Inspect, 0, frames as u64)?;
    let destination = atomic::destination_for_new(source, path)?;
    let parent = destination.parent().unwrap();
    atomic::space(parent, minimum_free_bytes, 0)?;
    let mut temporary = atomic::Temporary::create(parent)?;
    let mut output = temporary.checked_writer(parent, minimum_free_bytes);
    output.write_all(PACKED_MAGIC)?;
    let mut entries = Vec::new();
    let mut budget = Budget::new(limits.index_bytes);
    let mut completed = 0;
    let mut successful_parts = 0;
    while completed < frames {
        let width = target.min(frames - completed);
        let first = range.first + completed;
        let slice = CaptureRange {
            first,
            last: first + (width - 1),
        };
        let result = atomic::admitted_temporary(
            source,
            parent,
            slice,
            limits,
            minimum_free_bytes,
            |state| {
                callback(CaptureProgress {
                    phase: state.phase,
                    completed: completed as u64,
                    total: frames as u64,
                })
            },
        );
        let (part, report) = match result {
            Ok(part) => part,
            Err(failure) if width > 1 && crate::CapacityLimit::caused(&failure) => {
                target = width.div_ceil(2);
                successful_parts = 0;
                continue;
            }
            Err(failure) => return Err(failure),
        };
        let offset = output.stream_position()?;
        let mut file = File::open(&part.path)?;
        if file.metadata()?.len() != report.bytes {
            return Err(error("capture part changed before packing"));
        }
        let mut remaining = report.bytes;
        let mut buffer = [0; 64 << 10];
        while remaining > 0 {
            progress(
                &mut callback,
                CapturePhase::Copy,
                completed as u64,
                frames as u64,
            )?;
            let n = remaining.min(buffer.len() as u64) as usize;
            file.read_exact(&mut buffer[..n])?;
            output.write_all(&buffer[..n])?;
            remaining -= n as u64;
        }
        budget.push(
            &mut entries,
            PackedPart {
                range: slice,
                offset,
                bytes: report.bytes,
                digest: report.digest,
            },
        )?;
        completed += width;
        successful_parts += 1;
        if successful_parts == PARTS_BEFORE_GROWTH {
            target = (2 * target).min(maximum);
            successful_parts = 0;
        }
        progress(
            &mut callback,
            CapturePhase::Copy,
            completed as u64,
            frames as u64,
        )?;
    }
    let directory_offset = output.stream_position()?;
    let parts = entries.len();
    let mut digest = blake3::Hasher::new();
    digest.update(PACKED_MAGIC);
    for (index, part) in entries.into_iter().enumerate() {
        progress(
            &mut callback,
            CapturePhase::Copy,
            index as u64,
            parts as u64,
        )?;
        let entry = part.encode();
        output.write_all(&entry)?;
        digest.update(&entry);
    }
    let footer = wire::u64_bytes([parts as u64, directory_offset]);
    output.write_all(&footer)?;
    digest.update(&footer);
    output.write_all(digest.finalize().as_bytes())?;
    let bytes = output.stream_position()?;
    drop(output);
    temporary.close_synced()?;
    drop(super::open_packed_with_progress(
        &temporary.path,
        limits,
        &mut callback,
    )?);
    progress(&mut callback, CapturePhase::Validate, bytes, bytes)?;
    temporary.publish(&destination)?;
    Ok(PortableReport {
        frames,
        bytes,
        parts,
    })
}
