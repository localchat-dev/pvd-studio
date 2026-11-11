//! Captured section clocks and their retained replay boundaries.

use std::{collections::BTreeMap, mem::size_of};

use crate::{Error, FrameIndex, SceneState, error, history::SharedIndex};

pub mod wire;

#[cfg(test)]
mod tests;

const INDEX_BLOCK_ITEMS: usize = 256;
const SOURCE_ALLOCATION_OVERHEAD: usize = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FrameSection {
    PhysX,
    Apex,
}

impl FrameSection {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "frame" => Some(Self::PhysX),
            "ApexFrame" => Some(Self::Apex),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::PhysX => "frame",
            Self::Apex => "ApexFrame",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FrameSourceKey {
    Captured {
        section: FrameSection,
        source: u64,
    },
    /// PVDCAP01/02 preserve a frame axis but not the original section keys.
    Legacy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameObject {
    pub id: u64,
    pub generation: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameBinding {
    pub source: FrameSourceKey,
    pub source_object: Option<FrameObject>,
    pub scene: Option<FrameObject>,
}

impl FrameBinding {
    pub fn sdk(name: &str, source: u64, state: &SceneState) -> Option<Self> {
        let section = FrameSection::from_name(name)?;
        let object = state.objects.get(&source);
        let source_object = object.map(|object| FrameObject {
            id: object.id,
            generation: object.generation,
        });
        let scene = object
            .filter(|object| object.type_name == "physx3::PxScene")
            .and(source_object);
        Some(Self {
            source: FrameSourceKey::Captured { section, source },
            source_object,
            scene,
        })
    }

    pub fn legacy() -> Self {
        Self {
            source: FrameSourceKey::Legacy,
            source_object: None,
            scene: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceFrame {
    pub index: usize,
    pub ordinal: usize,
    pub source_object: Option<FrameObject>,
    pub scene: Option<FrameObject>,
}

#[derive(Clone, Debug)]
pub struct FrameSourceTimeline {
    source: FrameSourceKey,
    first_index: usize,
    frames: SharedIndex<SourceFrame>,
    next_ordinal: usize,
    peak_blocks: usize,
}

impl FrameSourceTimeline {
    fn empty(source: FrameSourceKey, next_ordinal: usize, first_index: usize) -> Self {
        Self {
            source,
            first_index,
            frames: SharedIndex::default(),
            next_ordinal,
            peak_blocks: 0,
        }
    }

    pub fn source(&self) -> FrameSourceKey {
        self.source
    }

    pub fn observed_frames(&self) -> usize {
        self.next_ordinal
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn first(&self) -> Option<&SourceFrame> {
        self.frames.first()
    }

    pub fn last(&self) -> Option<&SourceFrame> {
        self.frames.last()
    }

    pub fn at_ordinal(&self, ordinal: usize) -> Option<&SourceFrame> {
        let position = self
            .frames
            .binary_search_by_key(&ordinal, |frame| frame.ordinal)
            .ok()?;
        self.frames.get(position)
    }

    pub fn nearest_ordinal(&self, ordinal: usize) -> Option<&SourceFrame> {
        let position = self.frames.partition_point(|frame| frame.ordinal < ordinal);
        let before = position
            .checked_sub(1)
            .and_then(|position| self.frames.get(position));
        let after = self.frames.get(position);
        match (before, after) {
            (Some(before), Some(after)) if ordinal - before.ordinal <= after.ordinal - ordinal => {
                Some(before)
            }
            (_, Some(after)) => Some(after),
            (before, None) => before,
        }
    }

    pub fn at_index(&self, index: usize) -> Option<&SourceFrame> {
        let position = self
            .frames
            .binary_search_by_key(&index, |frame| frame.index)
            .ok()?;
        self.frames.get(position)
    }

    pub fn at_or_before(&self, index: usize) -> Option<&SourceFrame> {
        let position = self.frames.partition_point(|frame| frame.index <= index);
        self.frames.get(position.checked_sub(1)?)
    }

    pub fn at_or_after(&self, index: usize) -> Option<&SourceFrame> {
        let position = self.frames.partition_point(|frame| frame.index < index);
        self.frames.get(position)
    }

    pub fn next(&self, index: usize) -> Option<&SourceFrame> {
        let position = self.frames.partition_point(|frame| frame.index <= index);
        self.frames.get(position)
    }

    pub fn previous(&self, index: usize) -> Option<&SourceFrame> {
        let position = self.frames.partition_point(|frame| frame.index < index);
        self.frames.get(position.checked_sub(1)?)
    }

    pub fn frames(&self) -> impl DoubleEndedIterator<Item = &SourceFrame> {
        self.frames.iter()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FrameDirectoryLimits {
    pub index_bytes: usize,
    pub sources: usize,
    pub frames: usize,
}

impl Default for FrameDirectoryLimits {
    fn default() -> Self {
        Self {
            index_bytes: 64 * 1024 * 1024,
            sources: 4096,
            frames: 1_000_000,
        }
    }
}

#[derive(Clone, Debug)]
pub struct FrameDirectory {
    limits: FrameDirectoryLimits,
    sources: BTreeMap<FrameSourceKey, FrameSourceTimeline>,
    last_index: Option<usize>,
    retained_frames: usize,
    index_bytes: usize,
}

impl Default for FrameDirectory {
    fn default() -> Self {
        Self::new(FrameDirectoryLimits::default())
    }
}

impl FrameDirectory {
    pub fn new(limits: FrameDirectoryLimits) -> Self {
        Self {
            limits,
            sources: BTreeMap::new(),
            last_index: None,
            retained_frames: 0,
            index_bytes: size_of::<Self>(),
        }
    }

    pub fn from_legacy(frames: &FrameIndex, limits: FrameDirectoryLimits) -> Result<Self, Error> {
        Self::from_legacy_with_cancel(frames, limits, || false)
    }

    pub fn from_legacy_with_cancel(
        frames: &FrameIndex,
        limits: FrameDirectoryLimits,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Self, Error> {
        crate::check_replay_cancelled(&mut cancelled)?;
        let mut directory = Self::new(limits);
        let Some(last) = frames.last() else {
            return Ok(directory);
        };
        let next_ordinal = last
            .index
            .checked_add(1)
            .ok_or_else(|| error("legacy frame ordinal overflow"))?;
        directory.insert_empty(
            FrameSourceKey::Legacy,
            next_ordinal,
            frames.first().unwrap().index,
        )?;
        directory.last_index = Some(last.index);
        for frame in frames.iter() {
            crate::check_replay_cancelled(&mut cancelled)?;
            directory.insert_retained(
                FrameSourceKey::Legacy,
                SourceFrame {
                    index: frame.index,
                    ordinal: frame.index,
                    source_object: None,
                    scene: None,
                },
            )?;
        }
        crate::check_replay_cancelled(&mut cancelled)?;
        Ok(directory)
    }

    pub fn append(&mut self, index: usize, binding: FrameBinding) -> Result<(), Error> {
        self.observe(index, binding, true)
    }

    pub fn index_bytes_after_observe(
        &self,
        index: usize,
        binding: FrameBinding,
        retained: bool,
    ) -> Result<usize, Error> {
        Ok(self
            .prepare_observation(index, binding, retained)?
            .index_bytes)
    }

    pub fn observe(
        &mut self,
        index: usize,
        binding: FrameBinding,
        retained: bool,
    ) -> Result<(), Error> {
        let prepared = self.prepare_observation(index, binding, retained)?;
        let source = self
            .sources
            .entry(binding.source)
            .or_insert_with(|| FrameSourceTimeline::empty(binding.source, 0, index));
        if retained {
            source.frames.push(SourceFrame {
                index,
                ordinal: source.next_ordinal,
                source_object: binding.source_object,
                scene: binding.scene,
            });
            self.retained_frames += 1;
        }
        source.next_ordinal += 1;
        source.peak_blocks = prepared.peak_blocks;
        self.last_index = Some(index);
        self.index_bytes = prepared.index_bytes;
        Ok(())
    }

    fn prepare_observation(
        &self,
        index: usize,
        binding: FrameBinding,
        retained: bool,
    ) -> Result<PreparedObservation, Error> {
        if let Some(last) = self.last_index
            && last.checked_add(1) != Some(index)
        {
            return Err(error("frame source boundaries must be contiguous"));
        }
        if retained && self.retained_frames >= self.limits.frames {
            return Err(error("frame source frame limit exceeded"));
        }
        self.check_source_kind(binding.source)?;
        let existing = self.sources.get(&binding.source);
        if existing.is_none() && self.sources.len() >= self.limits.sources {
            return Err(error("frame source count limit exceeded"));
        }
        existing
            .map_or(0, |source| source.next_ordinal)
            .checked_add(1)
            .ok_or_else(|| error("frame source ordinal overflow"))?;
        let old_bytes = existing
            .map(|source| source_bytes(source.len(), source.peak_blocks))
            .transpose()?
            .unwrap_or(0);
        let len = existing.map_or(0, FrameSourceTimeline::len) + usize::from(retained);
        let peak_blocks = existing
            .map_or(0, |source| source.peak_blocks)
            .max(blocks_for(len)?);
        let index_bytes = self
            .index_bytes
            .checked_sub(old_bytes)
            .and_then(|bytes| bytes.checked_add(source_bytes(len, peak_blocks).ok()?))
            .filter(|bytes| *bytes <= self.limits.index_bytes)
            .ok_or_else(|| error("frame source index byte limit exceeded"))?;

        Ok(PreparedObservation {
            peak_blocks,
            index_bytes,
        })
    }

    pub fn discard_before(&mut self, first_index: usize) {
        for source in self.sources.values_mut() {
            let count = source
                .frames
                .partition_point(|frame| frame.index < first_index);
            if count == 0 {
                continue;
            }
            let old_bytes = source_bytes(source.len(), source.peak_blocks)
                .expect("retained frame source size was checked on append");
            source.frames.discard_front(count);
            if source.is_empty() {
                source.peak_blocks = 0;
            }
            let new_bytes = source_bytes(source.len(), source.peak_blocks)
                .expect("discarding frames cannot increase the frame source size");
            self.retained_frames -= count;
            self.index_bytes -= old_bytes - new_bytes;
        }
    }

    pub fn clipped(&self, first: usize, last: usize) -> Result<Self, Error> {
        self.clipped_with_cancel(first, last, || false)
    }

    pub fn clipped_index_bytes(&self, first: usize, last: usize) -> Result<usize, Error> {
        if first > last {
            return Err(error("frame source clip range is reversed"));
        }
        let mut bytes = size_of::<Self>();
        for source in self.sources.values() {
            let start = source.frames.partition_point(|frame| frame.index < first);
            let end = source.frames.partition_point(|frame| frame.index <= last);
            if start != end {
                bytes = bytes
                    .checked_add(source_bytes(end - start, blocks_for(end - start)?)?)
                    .ok_or_else(|| error("frame source clipped index size overflow"))?;
            }
        }
        Ok(bytes)
    }

    pub fn clipped_with_cancel(
        &self,
        first: usize,
        last: usize,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Self, Error> {
        crate::check_replay_cancelled(&mut cancelled)?;
        if first > last {
            return Err(error("frame source clip range is reversed"));
        }
        let mut directory = Self::new(self.limits);
        for source in self.sources.values() {
            let start = source.frames.partition_point(|frame| frame.index < first);
            let end = source.frames.partition_point(|frame| frame.index <= last);
            if start == end {
                continue;
            }
            directory.insert_empty(source.source, source.next_ordinal, source.first_index)?;
            for position in start..end {
                crate::check_replay_cancelled(&mut cancelled)?;
                directory.insert_retained(source.source, source.frames[position])?;
            }
        }
        if directory.retained_frames != 0 {
            directory.last_index = self.last_index;
        }
        crate::check_replay_cancelled(&mut cancelled)?;
        Ok(directory)
    }

    pub fn source(&self, source: FrameSourceKey) -> Option<&FrameSourceTimeline> {
        self.sources.get(&source)
    }

    pub fn sources(&self) -> impl Iterator<Item = &FrameSourceTimeline> {
        self.sources.values()
    }

    pub fn frame(&self, index: usize) -> Option<(FrameSourceKey, &SourceFrame)> {
        self.sources
            .values()
            .find_map(|source| source.at_index(index).map(|frame| (source.source, frame)))
    }

    pub fn default_source(&self) -> Option<FrameSourceKey> {
        self.sources
            .values()
            .map(|source| {
                let priority = match source.source {
                    FrameSourceKey::Captured {
                        section: FrameSection::PhysX,
                        ..
                    }
                    | FrameSourceKey::Legacy => 0,
                    FrameSourceKey::Captured {
                        section: FrameSection::Apex,
                        ..
                    } => 1,
                };
                ((priority, source.first_index), source.source)
            })
            .min_by_key(|(order, _)| *order)
            .map(|(_, source)| source)
    }

    pub fn retained_frames(&self) -> usize {
        self.retained_frames
    }

    pub fn index_bytes(&self) -> usize {
        self.index_bytes
    }

    pub fn validate_frames(&self, frames: &FrameIndex) -> Result<(), Error> {
        if frames.len() != self.retained_frames
            || frames.windows(2).any(|pair| pair[0].index >= pair[1].index)
        {
            return Err(error(
                "frame source directory does not match retained frames",
            ));
        }
        for source in self.sources.values() {
            for frame in source.frames.iter() {
                if frames
                    .binary_search_by_key(&frame.index, |frame| frame.index)
                    .is_err()
                {
                    return Err(error(
                        "frame source directory contains an unretained boundary",
                    ));
                }
            }
        }
        Ok(())
    }

    fn check_source_kind(&self, source: FrameSourceKey) -> Result<(), Error> {
        let legacy = matches!(source, FrameSourceKey::Legacy);
        if self
            .sources
            .keys()
            .next()
            .is_some_and(|existing| matches!(existing, FrameSourceKey::Legacy) != legacy)
        {
            return Err(error("legacy and captured frame sources cannot be mixed"));
        }
        Ok(())
    }

    fn insert_empty(
        &mut self,
        source: FrameSourceKey,
        ordinal: usize,
        first_index: usize,
    ) -> Result<(), Error> {
        self.check_source_kind(source)?;
        if self.sources.contains_key(&source) {
            return Err(error("duplicate frame source key"));
        }
        if self.sources.len() >= self.limits.sources {
            return Err(error("frame source count limit exceeded"));
        }
        let index_bytes = self
            .index_bytes
            .checked_add(source_bytes(0, 0)?)
            .filter(|bytes| *bytes <= self.limits.index_bytes)
            .ok_or_else(|| error("frame source index byte limit exceeded"))?;
        self.sources.insert(
            source,
            FrameSourceTimeline::empty(source, ordinal, first_index),
        );
        self.index_bytes = index_bytes;
        Ok(())
    }

    fn insert_retained(&mut self, key: FrameSourceKey, frame: SourceFrame) -> Result<(), Error> {
        let source = self
            .sources
            .get(&key)
            .ok_or_else(|| error("frame references an undefined source"))?;
        if self.retained_frames >= self.limits.frames {
            return Err(error("frame source frame limit exceeded"));
        }
        if frame.ordinal >= source.next_ordinal
            || frame.index < source.first_index
            || source.last().is_some_and(|previous| {
                previous.ordinal >= frame.ordinal || previous.index >= frame.index
            })
        {
            return Err(error(
                "retained source frames must have increasing indices and ordinals",
            ));
        }
        let old_bytes = source_bytes(source.len(), source.peak_blocks)?;
        let len = source.len() + 1;
        let peak_blocks = source.peak_blocks.max(blocks_for(len)?);
        let index_bytes = self
            .index_bytes
            .checked_sub(old_bytes)
            .and_then(|bytes| bytes.checked_add(source_bytes(len, peak_blocks).ok()?))
            .filter(|bytes| *bytes <= self.limits.index_bytes)
            .ok_or_else(|| error("frame source index byte limit exceeded"))?;
        let source = self
            .sources
            .get_mut(&key)
            .expect("retained source was validated");
        source.frames.push(frame);
        source.peak_blocks = peak_blocks;
        self.index_bytes = index_bytes;
        self.retained_frames += 1;
        Ok(())
    }
}

struct PreparedObservation {
    peak_blocks: usize,
    index_bytes: usize,
}

fn blocks_for(len: usize) -> Result<usize, Error> {
    if len == 0 {
        return Ok(0);
    }
    len.div_ceil(INDEX_BLOCK_ITEMS)
        .checked_add(2)
        .ok_or_else(|| error("frame source block count overflow"))
}

fn source_bytes(len: usize, peak_blocks: usize) -> Result<usize, Error> {
    let base =
        size_of::<FrameSourceTimeline>() + size_of::<FrameSourceKey>() + SOURCE_ALLOCATION_OVERHEAD;
    if len == 0 {
        return Ok(base);
    }
    // Copy-on-write growth can double each block's capacity; partial head/tail blocks also stay owned.
    let frame_bytes = len
        .checked_add(2 * INDEX_BLOCK_ITEMS)
        .and_then(|count| count.checked_mul(2))
        .and_then(|count| count.checked_mul(size_of::<SourceFrame>()));
    let pointer_bytes = peak_blocks.checked_mul(size_of::<[usize; 8]>());
    frame_bytes
        .zip(pointer_bytes)
        .and_then(|(frames, pointers)| base.checked_add(frames)?.checked_add(pointers))
        .ok_or_else(|| error("frame source index size overflow"))
}
