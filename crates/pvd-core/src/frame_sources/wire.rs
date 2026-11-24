//! Versioned directory payload for project capture source entries.

use std::{cmp::Reverse, collections::BinaryHeap, mem::size_of};

use super::{
    FrameDirectory, FrameDirectoryLimits, FrameObject, FrameSection, FrameSourceKey,
    FrameSourceTimeline, SourceFrame,
};
use crate::{Error, FrameIndex, check_replay_cancelled, error};

const VERSION: u32 = 1;
const HEADER_BYTES: usize = 25;
const SOURCE_BYTES: usize = 33;
const FRAME_BYTES: usize = 21;
const OBJECT_BYTES: usize = 12;

pub fn encode(directory: &FrameDirectory, max_bytes: usize) -> Result<Vec<u8>, Error> {
    encode_with_cancel(directory, max_bytes, || false)
}

pub fn encode_with_cancel(
    directory: &FrameDirectory,
    max_bytes: usize,
    mut cancelled: impl FnMut() -> bool,
) -> Result<Vec<u8>, Error> {
    check_replay_cancelled(&mut cancelled)?;
    let source_count = u32::try_from(directory.sources.len())
        .map_err(|_| error("frame source count exceeds directory format"))?;
    let mut bytes = HEADER_BYTES
        .checked_add(
            directory
                .sources
                .len()
                .checked_mul(SOURCE_BYTES)
                .ok_or_else(|| error("frame source encoded size overflow"))?,
        )
        .and_then(|bytes| bytes.checked_add(directory.retained_frames.checked_mul(FRAME_BYTES)?))
        .ok_or_else(|| error("frame source encoded size overflow"))?;
    for source in directory.sources.values() {
        for frame in source.frames.iter() {
            check_replay_cancelled(&mut cancelled)?;
            let objects =
                usize::from(frame.source_object.is_some()) + usize::from(frame.scene.is_some());
            bytes = bytes
                .checked_add(objects * OBJECT_BYTES)
                .filter(|bytes| *bytes <= max_bytes)
                .ok_or_else(|| {
                    crate::CapacityLimit::exceeded("frame source encoded byte limit exceeded")
                })?;
        }
    }
    if bytes > max_bytes {
        return Err(crate::CapacityLimit::exceeded(
            "frame source encoded byte limit exceeded",
        ));
    }
    let temporary_bytes = directory
        .sources
        .len()
        .checked_mul(
            size_of::<&FrameSourceTimeline>() + size_of::<Reverse<(usize, usize, usize)>>(),
        )
        .ok_or_else(|| error("frame source encoding workspace overflow"))?;
    if bytes
        .checked_add(temporary_bytes)
        .is_none_or(|total| total > max_bytes)
    {
        return Err(crate::CapacityLimit::exceeded(
            "frame source encoding workspace limit exceeded",
        ));
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(bytes)
        .map_err(|_| error("frame source encoded allocation failed"))?;
    output.extend_from_slice(&VERSION.to_le_bytes());
    output.extend_from_slice(&source_count.to_le_bytes());
    write_usize(&mut output, directory.retained_frames)?;
    output.push(u8::from(directory.last_index.is_some()));
    write_usize(&mut output, directory.last_index.unwrap_or(0))?;
    let mut sources = Vec::new();
    sources
        .try_reserve_exact(directory.sources.len())
        .map_err(|_| error("frame source encoding workspace allocation failed"))?;
    let mut heap = BinaryHeap::new();
    heap.try_reserve_exact(directory.sources.len())
        .map_err(|_| error("frame source encoding workspace allocation failed"))?;
    for (source_index, source) in directory.sources.values().enumerate() {
        check_replay_cancelled(&mut cancelled)?;
        let (kind, raw_source) = match source.source {
            FrameSourceKey::Legacy => (0, 0),
            FrameSourceKey::Captured {
                section: FrameSection::PhysX,
                source,
            } => (1, source),
            FrameSourceKey::Captured {
                section: FrameSection::Apex,
                source,
            } => (2, source),
        };
        output.push(kind);
        output.extend_from_slice(&raw_source.to_le_bytes());
        write_usize(&mut output, source.next_ordinal)?;
        write_usize(&mut output, source.len())?;
        write_usize(&mut output, source.first_index)?;
        if let Some(frame) = source.first() {
            heap.push(Reverse((frame.index, source_index, 0)));
        }
        sources.push(source);
    }
    while let Some(Reverse((_, source_index, position))) = heap.pop() {
        check_replay_cancelled(&mut cancelled)?;
        let source = sources[source_index];
        let frame = source
            .frames
            .get(position)
            .expect("queued source frame exists");
        output.extend_from_slice(&(source_index as u32).to_le_bytes());
        write_usize(&mut output, frame.index)?;
        write_usize(&mut output, frame.ordinal)?;
        output
            .push(u8::from(frame.source_object.is_some()) | (u8::from(frame.scene.is_some()) << 1));
        if let Some(object) = frame.source_object {
            write_object(&mut output, object);
        }
        if let Some(object) = frame.scene {
            write_object(&mut output, object);
        }
        if let Some(next) = source.frames.get(position + 1) {
            heap.push(Reverse((next.index, source_index, position + 1)));
        }
    }
    check_replay_cancelled(&mut cancelled)?;
    debug_assert_eq!(output.len(), bytes);
    Ok(output)
}

pub fn decode(
    bytes: &[u8],
    frames: &FrameIndex,
    limits: FrameDirectoryLimits,
) -> Result<FrameDirectory, Error> {
    decode_with_cancel(bytes, frames, limits, || false)
}

pub fn decode_with_cancel(
    bytes: &[u8],
    frames: &FrameIndex,
    limits: FrameDirectoryLimits,
    mut cancelled: impl FnMut() -> bool,
) -> Result<FrameDirectory, Error> {
    check_replay_cancelled(&mut cancelled)?;
    let mut reader = Reader { bytes, offset: 0 };
    if reader.u32()? != VERSION {
        return Err(error("unsupported frame source directory version"));
    }
    let source_count =
        usize::try_from(reader.u32()?).map_err(|_| error("frame source count overflow"))?;
    let frame_count = reader.usize()?;
    let last_index = match reader.byte()? {
        0 => {
            if reader.usize()? != 0 {
                return Err(error("empty frame source end has a value"));
            }
            None
        }
        1 => Some(reader.usize()?),
        _ => return Err(error("invalid frame source end flag")),
    };
    if source_count > limits.sources || frame_count > limits.frames || frame_count != frames.len() {
        return Err(error(
            "frame source directory count limit or frame count mismatch",
        ));
    }
    let minimum_bytes = source_count
        .checked_mul(SOURCE_BYTES)
        .and_then(|bytes| bytes.checked_add(frame_count.checked_mul(FRAME_BYTES)?))
        .ok_or_else(|| error("frame source directory size overflow"))?;
    if minimum_bytes > reader.remaining() {
        return Err(error("truncated frame source directory"));
    }
    let temporary_bytes = source_count
        .checked_mul(size_of::<DecodedSource>())
        .ok_or_else(|| error("frame source decoding workspace overflow"))?;
    let index_bytes = limits
        .index_bytes
        .checked_sub(temporary_bytes)
        .filter(|bytes| *bytes >= size_of::<FrameDirectory>())
        .ok_or_else(|| {
            crate::CapacityLimit::exceeded("frame source decoding workspace limit exceeded")
        })?;
    let mut directory = FrameDirectory::new(FrameDirectoryLimits {
        index_bytes,
        ..limits
    });
    let mut sources = Vec::new();
    sources
        .try_reserve_exact(source_count)
        .map_err(|_| error("frame source decoding workspace allocation failed"))?;
    let mut declared_frames = 0usize;
    for _ in 0..source_count {
        check_replay_cancelled(&mut cancelled)?;
        let kind = reader.byte()?;
        let raw_source = reader.u64()?;
        let source = match kind {
            0 if raw_source == 0 => FrameSourceKey::Legacy,
            1 => FrameSourceKey::Captured {
                section: FrameSection::PhysX,
                source: raw_source,
            },
            2 => FrameSourceKey::Captured {
                section: FrameSection::Apex,
                source: raw_source,
            },
            _ => return Err(error("invalid frame source kind")),
        };
        let next_ordinal = reader.usize()?;
        let count = reader.usize()?;
        let first_index = reader.usize()?;
        if count > next_ordinal || next_ordinal == 0 {
            return Err(error("frame source retained count exceeds its ordinal"));
        }
        declared_frames = declared_frames
            .checked_add(count)
            .filter(|count| *count <= frame_count)
            .ok_or_else(|| error("frame source counts exceed directory frame count"))?;
        if last_index.is_none() && next_ordinal != 0 {
            return Err(error("frame source ordinals require a recorded boundary"));
        }
        if last_index.is_none_or(|last| first_index > last) {
            return Err(error(
                "frame source first observation exceeds the capture boundary",
            ));
        }
        directory.insert_empty(source, next_ordinal, first_index)?;
        sources.push(DecodedSource {
            source,
            count,
            seen: 0,
        });
    }
    if declared_frames != frame_count || (source_count == 0 && last_index.is_some()) {
        return Err(error("frame source directory counts are inconsistent"));
    }
    let mut previous_index = None;
    for position in 0..frame_count {
        check_replay_cancelled(&mut cancelled)?;
        let source_index =
            usize::try_from(reader.u32()?).map_err(|_| error("frame source index overflow"))?;
        let source = sources
            .get_mut(source_index)
            .ok_or_else(|| error("frame references an undefined source"))?;
        let index = reader.usize()?;
        let ordinal = reader.usize()?;
        let flags = reader.byte()?;
        if flags & !3 != 0 {
            return Err(error("invalid frame source object flags"));
        }
        let source_object = (flags & 1 != 0).then(|| reader.object()).transpose()?;
        let scene = (flags & 2 != 0).then(|| reader.object()).transpose()?;
        if source.seen >= source.count
            || previous_index.is_some_and(|previous| previous >= index)
            || last_index.is_none_or(|last| index > last)
            || frames[position].index != index
        {
            return Err(error("frame source ordinal or count is inconsistent"));
        }
        directory.insert_retained(
            source.source,
            SourceFrame {
                index,
                ordinal,
                source_object,
                scene,
            },
        )?;
        previous_index = Some(index);
        source.seen += 1;
    }
    if reader.remaining() != 0 {
        return Err(error("trailing frame source directory data"));
    }
    for source in &sources {
        if source.seen != source.count {
            return Err(error(
                "frame source directory contains missing source frames",
            ));
        }
    }
    directory.last_index = last_index;
    drop(sources);
    directory.limits = limits;
    check_replay_cancelled(&mut cancelled)?;
    Ok(directory)
}

struct DecodedSource {
    source: FrameSourceKey,
    count: usize,
    seen: usize,
}

fn write_usize(output: &mut Vec<u8>, value: usize) -> Result<(), Error> {
    let value =
        u64::try_from(value).map_err(|_| error("frame source value exceeds directory format"))?;
    output.extend_from_slice(&value.to_le_bytes());
    Ok(())
}

fn write_object(output: &mut Vec<u8>, object: FrameObject) {
    output.extend_from_slice(&object.id.to_le_bytes());
    output.extend_from_slice(&object.generation.to_le_bytes());
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Reader<'_> {
    fn remaining(&self) -> usize {
        self.bytes.len() - self.offset
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let end = self
            .offset
            .checked_add(N)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| error("truncated frame source directory"))?;
        let value = self.bytes[self.offset..end]
            .try_into()
            .expect("checked frame source field width");
        self.offset = end;
        Ok(value)
    }

    fn byte(&mut self) -> Result<u8, Error> {
        Ok(self.take::<1>()?[0])
    }

    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.take()?))
    }

    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.take()?))
    }

    fn usize(&mut self) -> Result<usize, Error> {
        usize::try_from(self.u64()?).map_err(|_| error("frame source value exceeds platform range"))
    }

    fn object(&mut self) -> Result<FrameObject, Error> {
        Ok(FrameObject {
            id: self.u64()?,
            generation: self.u32()?,
        })
    }
}
