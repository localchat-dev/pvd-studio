use super::*;
pub(super) const BLOCK: u8 = 1;
pub(super) const BLOB: u8 = 2;
pub(super) const RECORD: u8 = 3;
pub(super) const SEGMENT: u8 = 4;
pub(super) const METADATA: u8 = 5;
pub(super) const FRAME_SOURCES: u8 = 6;
pub(super) const FOOTER: u8 = 255;
pub(super) const ENTRY_HEADER: usize = 41;
pub(super) const MAX_ENTRY: usize = MAX_RECORD + 128;

pub(super) struct Writer<'a, W> {
    pub output: &'a mut W,
    pub bytes: u64,
    pub hash: blake3::Hasher,
    pub limit: u64,
}
impl<'a, W: Write> Writer<'a, W> {
    pub fn new(output: &'a mut W, limit: u64) -> Result<Self, Error> {
        let mut writer = Self {
            output,
            bytes: 0,
            hash: blake3::Hasher::new(),
            limit,
        };
        writer.put(MAGIC)?;
        Ok(writer)
    }
    fn put(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let next = self
            .bytes
            .checked_add(bytes.len() as u64)
            .filter(|next| *next <= self.limit)
            .ok_or_else(|| {
                crate::CapacityLimit::exceeded("project capture exceeds its output byte limit")
            })?;
        self.output.write_all(bytes)?;
        self.hash.update(bytes);
        self.bytes = next;
        Ok(())
    }
    pub fn entry(
        &mut self,
        kind: u8,
        parts: &[&[u8]],
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<u64, Error> {
        check_replay_cancelled(cancelled)?;
        let length = parts
            .iter()
            .try_fold(0usize, |n, bytes| n.checked_add(bytes.len()))
            .filter(|length| *length <= MAX_ENTRY)
            .ok_or_else(|| error("capture entry exceeds its record limit"))?;
        if self
            .bytes
            .checked_add(ENTRY_HEADER as u64)
            .and_then(|n| n.checked_add(length as u64))
            .is_none_or(|end| end > self.limit)
        {
            return Err(crate::CapacityLimit::exceeded(
                "project capture exceeds its output byte limit",
            ));
        }
        let mut hash = blake3::Hasher::new();
        hash.update(&[kind]);
        hash.update(&(length as u64).to_le_bytes());
        for part in parts {
            hash.update(part);
        }
        self.put(&[kind])?;
        self.put(&(length as u64).to_le_bytes())?;
        self.put(hash.finalize().as_bytes())?;
        let offset = self.bytes;
        for part in parts {
            for bytes in part.chunks(64 << 10) {
                check_replay_cancelled(cancelled)?;
                self.put(bytes)?;
            }
        }
        Ok(offset)
    }
}

pub(super) fn u64_bytes(values: impl IntoIterator<Item = u64>) -> Vec<u8> {
    values.into_iter().flat_map(u64::to_le_bytes).collect()
}

pub(super) struct Input<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> Input<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self
            .offset
            .checked_add(n)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| error("truncated project capture directory"))?;
        let value = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(value)
    }
    pub fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into()?))
    }
    pub fn usize(&mut self) -> Result<usize, Error> {
        Ok(usize::try_from(self.u64()?)?)
    }
    pub fn count(&mut self, limit: usize, minimum_width: usize) -> Result<usize, Error> {
        let count = self.usize()?;
        if count
            .checked_mul(minimum_width)
            .is_none_or(|size| size > self.bytes.len() - self.offset)
        {
            return Err(error("project capture directory count exceeds its bounds"));
        }
        if count > limit {
            return Err(crate::CapacityLimit::exceeded(
                "project capture directory count exceeds its bounds",
            ));
        }
        Ok(count)
    }
    pub fn finish(self) -> Result<(), Error> {
        if self.offset != self.bytes.len() {
            return Err(error("trailing project capture directory bytes"));
        }
        Ok(())
    }
}

pub(super) fn encode_segment(segment: &Segment, limit: usize) -> Result<Vec<u8>, Error> {
    let size = segment
        .frames
        .len()
        .checked_mul(40)
        .and_then(|n| {
            segment
                .checkpoints
                .len()
                .checked_mul(16)
                .and_then(|checkpoints| n.checked_add(checkpoints))
        })
        .and_then(|n| n.checked_add(40))
        .filter(|n| *n <= limit)
        .ok_or_else(|| {
            crate::CapacityLimit::exceeded("capture segment directory exceeds its temporary budget")
        })?;
    let mut out = u64_bytes([
        segment.analysis_base,
        segment
            .previous_frame
            .map_or(u64::MAX, |frame| frame as u64),
        u64::from(segment.continuity),
        segment.frames.len() as u64,
    ]);
    out.try_reserve_exact(size - out.len())?;
    if out.capacity() > limit {
        return Err(crate::CapacityLimit::exceeded(
            "capture segment capacity exceeds its temporary budget",
        ));
    }
    for frame in &segment.frames {
        out.extend(u64_bytes([
            frame.info.index as u64,
            frame.info.timestamp,
            frame.info.scene_id,
            frame.offset,
            frame.scene_generation.map_or(u64::MAX, u64::from),
        ]));
    }
    out.extend((segment.checkpoints.len() as u64).to_le_bytes());
    for (&frame, &offset) in &segment.checkpoints {
        out.extend(u64_bytes([frame as u64, offset]));
    }
    Ok(out)
}

pub(super) fn decode_segment(
    bytes: &[u8],
    limits: &CaptureLimits,
    budget: &mut Budget,
) -> Result<Segment, Error> {
    let mut input = Input::new(bytes);
    let analysis_base = input.u64()?;
    let previous = input.u64()?;
    let continuity = match input.u64()? {
        0 => false,
        1 => true,
        _ => return Err(error("invalid capture continuity flag")),
    };
    let count = input.count(limits.frames, 40)?;
    if count == 0 {
        return Err(error("empty capture segment"));
    }
    budget.entries(count, size_of::<FrameLocation>() + 16)?;
    let mut frames = Vec::new();
    frames.try_reserve_exact(count)?;
    budget.entries(frames.capacity() - count, size_of::<FrameLocation>())?;
    for _ in 0..count {
        let index = input.usize()?;
        let timestamp = input.u64()?;
        let scene_id = input.u64()?;
        let offset = input.u64()?;
        let generation = input.u64()?;
        if frames
            .last()
            .is_some_and(|frame: &FrameLocation| frame.info.index.checked_add(1) != Some(index))
        {
            return Err(error("capture segment frames are not contiguous"));
        }
        frames.push(FrameLocation {
            undo: None,
            info: FrameInfo {
                index,
                timestamp,
                scene_id,
            },
            offset,
            scene_generation: if generation == u64::MAX {
                None
            } else {
                Some(u32::try_from(generation)?)
            },
        });
    }
    let count = input.count(limits.frames, 16)?;
    budget.entries(count, 160)?;
    let mut checkpoints = BTreeMap::new();
    for _ in 0..count {
        let frame = input.usize()?;
        let offset = input.u64()?;
        if frame < frames[0].info.index
            || frame > frames.last().unwrap().info.index
            || checkpoints.insert(frame, offset).is_some()
        {
            return Err(error("invalid capture checkpoint directory"));
        }
    }
    if !checkpoints.contains_key(&frames[0].info.index) {
        return Err(error("capture segment lacks its initial checkpoint"));
    }
    input.finish()?;
    Ok(Segment {
        analysis_base,
        previous_frame: if previous == u64::MAX {
            None
        } else {
            Some(usize::try_from(previous)?)
        },
        continuity,
        frames,
        checkpoints,
    })
}
