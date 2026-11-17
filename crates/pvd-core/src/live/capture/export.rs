use super::*;

pub fn write_retained<W: Read + Write + Seek>(
    source: &crate::Recording,
    output: &mut W,
    range: CaptureRange,
    limits: &CaptureLimits,
    callback: impl FnMut(CaptureProgress) -> bool,
) -> Result<CaptureReport, Error> {
    write(source, output, range, limits, callback, false)
}

/// Internal banks preserve scene history and its original analysis-failure
/// diagnostic. They must not imply that a permanent complete-analysis export
/// is possible after the analysis decoder stopped.
pub(in crate::live) fn write_bank<W: Read + Write + Seek>(
    source: &crate::Recording,
    output: &mut W,
    range: CaptureRange,
    limits: &CaptureLimits,
    callback: impl FnMut(CaptureProgress) -> bool,
) -> Result<CaptureReport, Error> {
    write(source, output, range, limits, callback, true)
}

fn write<W: Read + Write + Seek>(
    source: &crate::Recording,
    output: &mut W,
    range: CaptureRange,
    limits: &CaptureLimits,
    mut callback: impl FnMut(CaptureProgress) -> bool,
    temporary_bank: bool,
) -> Result<CaptureReport, Error> {
    limits.validate()?;
    if output.stream_position()? != 0 || output.seek(std::io::SeekFrom::End(0))? != 0 {
        return Err(error("project capture output must be a new empty stream"));
    }
    output.seek(std::io::SeekFrom::Start(0))?;
    let mut report = copy(source, output, range, limits, &mut callback, temporary_bank)?;
    output.flush()?;
    output.seek(std::io::SeekFrom::Start(0))?;
    let directory = read::directory(output, report.bytes, limits, &mut callback)?;
    if directory.report.digest != report.digest || directory.report.frames != report.frames {
        return Err(error(
            "project capture verification differs from its written contents",
        ));
    }
    report.index_bytes = report.index_bytes.max(directory.report.index_bytes);
    progress(
        &mut callback,
        CapturePhase::Validate,
        report.bytes,
        report.bytes,
    )?;
    output.seek(std::io::SeekFrom::End(0))?;
    Ok(report)
}

fn copy<W: Write>(
    source: &crate::Recording,
    output: &mut W,
    range: CaptureRange,
    limits: &CaptureLimits,
    callback: &mut impl FnMut(CaptureProgress) -> bool,
    temporary_bank: bool,
) -> Result<CaptureReport, Error> {
    if !matches!(
        source.source,
        crate::RecordingSource::SdkStream | crate::RecordingSource::ProjectCapture
    ) {
        return Err(error(
            "retained export requires disk-backed SDK history or a project capture",
        ));
    }
    let disk = source
        .disk
        .as_ref()
        .ok_or_else(|| error("retained export requires disk history"))?;
    if !temporary_bank
        && source
            .diagnostics
            .iter()
            .any(|text| text.starts_with("Analysis decoding stopped at frame"))
    {
        return Err(error(
            "cannot export complete analysis after its decoder stopped",
        ));
    }
    let count = range
        .last
        .checked_sub(range.first)
        .and_then(|n| n.checked_add(1))
        .filter(|n| *n <= limits.frames)
        .ok_or_else(|| error("invalid retained capture frame range"))?;
    for (number, frame) in (range.first..=range.last).enumerate() {
        progress(callback, CapturePhase::Inspect, number as u64, count as u64)?;
        if !source.is_frame_retained(frame) {
            return Err(error(
                "capture range includes an expired or incomplete frame",
            ));
        }
    }
    let mut budget = Budget::new(limits.index_bytes);
    let bytes = metadata::encode(source, range, &mut budget, &mut || {
        !callback(CaptureProgress {
            phase: CapturePhase::Inspect,
            completed: count as u64,
            total: count as u64,
        })
    })?;
    let mut writer = wire::Writer::new(output, limits.output_bytes)?;
    writer.entry(wire::METADATA, &[&bytes], &mut || {
        !callback(CaptureProgress {
            phase: CapturePhase::Copy,
            completed: 0,
            total: count as u64,
        })
    })?;
    let metadata_bytes = bytes.capacity();
    drop(bytes);
    budget.release(metadata_bytes);
    let mut copied = Copied {
        resources: BTreeSet::new(),
        blocks: BTreeSet::new(),
        budget,
        limits,
    };
    let mut copied_frames = 0;
    let mut next_frame = Some(range.first);
    while let Some(first) = next_frame {
        let _transition = disk.history_guard()?;
        progress(callback, CapturePhase::Copy, copied_frames, count as u64)?;
        let mut cancelled = || {
            !callback(CaptureProgress {
                phase: CapturePhase::Copy,
                completed: copied_frames,
                total: count as u64,
            })
        };
        // Resolve one bank at a time. The returned segment keeps its resources
        // alive while the archive's bounded reader cache moves to another bank.
        let owned = disk.owned_segment(first, &mut cancelled)?;
        let segment = owned.as_ref();
        let last = segment
            .frames
            .last()
            .map(|frame| frame.info.index.min(range.last))
            .filter(|&last| last >= first)
            .ok_or_else(|| error("capture segment does not advance its frame range"))?;
        next_frame = if last == range.last {
            None
        } else {
            Some(
                last.checked_add(1)
                    .ok_or_else(|| error("capture frame index overflow"))?,
            )
        };
        copied.resource(
            segment,
            segment.analysis_base,
            &mut writer,
            0,
            &mut cancelled,
        )?;
        let mut saved = Segment {
            analysis_base: segment.analysis_base,
            previous_frame: segment.previous_frame,
            continuity: segment.analysis_continuity,
            frames: Vec::new(),
            checkpoints: BTreeMap::new(),
        };
        for location in segment
            .frames
            .iter()
            .take_while(|frame| frame.info.index <= range.last)
        {
            check_replay_cancelled(&mut cancelled)?;
            if !segment.lease.available() {
                return Err(error("history expired during capture export"));
            }
            let (bytes, record) = source_record(segment, location.offset, &mut cancelled)?;
            let Record::Frame(frame) = record else {
                return Err(error("capture frame record type mismatch"));
            };
            if frame.info.index != location.info.index
                || frame.info.timestamp != location.info.timestamp
                || frame.info.scene_id != location.info.scene_id
            {
                return Err(error(
                    "capture frame metadata differs from its source index",
                ));
            }
            for id in frame
                .update
                .objects
                .iter()
                .chain(&frame.update.debug)
                .chain(&frame.update.contacts)
                .filter_map(|(_, id)| *id)
            {
                copied.resource(segment, id, &mut writer, 0, &mut cancelled)?;
            }
            let offset = copy_record(&bytes, &mut writer, &mut cancelled)?;
            copied.budget.push(
                &mut saved.frames,
                FrameLocation {
                    undo: None,
                    offset,
                    ..location.clone()
                },
            )?;
            if let Some(&checkpoint) = segment.checkpoints.get(&location.info.index) {
                let (bytes, record) = source_record(segment, checkpoint, &mut cancelled)?;
                let Record::Checkpoint(index) = record else {
                    return Err(error("capture checkpoint record type mismatch"));
                };
                for id in index
                    .objects
                    .values()
                    .chain(index.debug.values())
                    .chain(index.contacts.values())
                {
                    copied.resource(segment, *id, &mut writer, 0, &mut cancelled)?;
                }
                let offset = copy_record(&bytes, &mut writer, &mut cancelled)?;
                copied.budget.charge(160)?;
                saved.checkpoints.insert(location.info.index, offset);
            }
        }
        let bytes = wire::encode_segment(
            &saved,
            limits.index_bytes.saturating_sub(copied.budget.used),
        )?;
        copied.budget.charge(bytes.capacity())?;
        writer.entry(wire::SEGMENT, &[&bytes], &mut cancelled)?;
        let encoded_bytes = bytes.capacity();
        drop(bytes);
        copied.budget.release(encoded_bytes);
        copied_frames += saved
            .frames
            .iter()
            .filter(|frame| frame.info.index >= range.first)
            .count() as u64;
        let saved_bytes =
            saved.frames.capacity() * size_of::<FrameLocation>() + saved.checkpoints.len() * 160;
        drop(saved);
        copied.budget.release(saved_bytes);
    }
    if copied_frames != count as u64 {
        return Err(error("capture range changed or contains a history gap"));
    }
    let mut cancelled = || {
        !callback(CaptureProgress {
            phase: CapturePhase::Copy,
            completed: copied_frames,
            total: count as u64,
        })
    };
    copied.budget.charge(
        source
            .frame_sources
            .clipped_index_bytes(range.first, range.last)?,
    )?;
    let directory =
        source
            .frame_sources
            .clipped_with_cancel(range.first, range.last, &mut cancelled)?;
    if directory.retained_frames() != count {
        return Err(error(
            "capture frame source directory does not cover its visible range",
        ));
    }
    let source_bytes = crate::frame_sources::wire::encode_with_cancel(
        &directory,
        limits.index_bytes.saturating_sub(copied.budget.used),
        &mut cancelled,
    )?;
    copied.budget.charge(source_bytes.capacity())?;
    writer.entry(wire::FRAME_SOURCES, &[&source_bytes], &mut cancelled)?;
    let temporary = directory.index_bytes() + source_bytes.capacity();
    drop(source_bytes);
    drop(directory);
    copied.budget.release(temporary);
    let digest = *writer.hash.finalize().as_bytes();
    let mut footer = wire::u64_bytes([writer.bytes, count as u64]);
    footer.extend(digest);
    writer.entry(wire::FOOTER, &[&footer], &mut || {
        !callback(CaptureProgress {
            phase: CapturePhase::Copy,
            completed: copied_frames,
            total: count as u64,
        })
    })?;
    Ok(CaptureReport {
        format: Format::V3,
        range,
        frames: count,
        bytes: writer.bytes,
        digest: *writer.hash.finalize().as_bytes(),
        resource_count: copied.resources.len(),
        block_count: copied.blocks.len(),
        index_bytes: copied.budget.peak,
    })
}

fn copy_record<W: Write>(
    bytes: &[u8],
    writer: &mut wire::Writer<'_, W>,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<u64, Error> {
    let length = u32::try_from(bytes.len())?.to_le_bytes();
    let hash = blake3::hash(bytes);
    writer.entry(wire::RECORD, &[&length, hash.as_bytes(), bytes], cancelled)
}

struct Copied<'a> {
    resources: BTreeSet<u64>,
    blocks: BTreeSet<u64>,
    budget: Budget,
    limits: &'a CaptureLimits,
}
impl Copied<'_> {
    fn resource<W: Write>(
        &mut self,
        segment: &SegmentView,
        id: u64,
        writer: &mut wire::Writer<'_, W>,
        depth: u8,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), Error> {
        check_replay_cancelled(cancelled)?;
        if self.resources.contains(&id) {
            return Ok(());
        }
        if depth >= 64 || !segment.lease.available() {
            return Err(error(
                "history expired or resource dependency depth exceeded during export",
            ));
        }
        if self
            .resources
            .len()
            .checked_add(self.blocks.len())
            .is_none_or(|n| n >= self.limits.resources)
        {
            return Err(error("capture resource count exceeds its limit"));
        }
        let description = segment
            .resources
            .describe_blob(id, self.budget.limit.saturating_sub(self.budget.used))?;
        let temporary_bytes = description
            .blocks
            .capacity()
            .checked_add(description.dependencies.capacity())
            .and_then(|n| n.checked_mul(16))
            .and_then(|n| n.checked_add(128))
            .ok_or_else(|| error("capture descriptor size overflow"))?;
        self.budget.charge(temporary_bytes)?;
        for &dependency in &description.dependencies {
            if dependency >= id {
                return Err(error("invalid capture resource dependency ordering"));
            }
            self.resource(segment, dependency, writer, depth + 1, cancelled)?;
        }
        for &block in &description.blocks {
            if self.blocks.contains(&block) {
                continue;
            }
            if self
                .resources
                .len()
                .checked_add(self.blocks.len())
                .is_none_or(|n| n >= self.limits.resources)
            {
                return Err(error("capture resource count exceeds its limit"));
            }
            if !segment.lease.available() {
                return Err(error("history expired during capture block export"));
            }
            let (description, bytes) = segment.resources.export_block(block, cancelled)?;
            self.budget.charge(192)?;
            let id = block.to_le_bytes();
            let len = u32::try_from(description.len)?.to_le_bytes();
            writer.entry(
                wire::BLOCK,
                &[&id, &len, &description.hash, &bytes],
                cancelled,
            )?;
            self.blocks.insert(block);
        }
        if self
            .resources
            .len()
            .checked_add(self.blocks.len())
            .is_none_or(|n| n >= self.limits.resources)
        {
            return Err(error("capture resource count exceeds its limit"));
        }
        let mut encoded = Vec::new();
        let encoded_size = 64usize
            .checked_add(
                description
                    .blocks
                    .len()
                    .checked_add(description.dependencies.len())
                    .and_then(|n| n.checked_mul(8))
                    .ok_or_else(|| error("capture descriptor size overflow"))?,
            )
            .ok_or_else(|| error("capture descriptor size overflow"))?;
        encoded.try_reserve_exact(encoded_size)?;
        if encoded.capacity() > encoded_size {
            self.budget.charge(encoded.capacity() - encoded_size)?;
        }
        encoded.extend(wire::u64_bytes([id, description.len as u64]));
        encoded.extend(description.hash);
        encoded.extend((description.blocks.len() as u64).to_le_bytes());
        for block in &description.blocks {
            encoded.extend(block.to_le_bytes());
        }
        encoded.extend((description.dependencies.len() as u64).to_le_bytes());
        for dependency in &description.dependencies {
            encoded.extend(dependency.to_le_bytes());
        }
        writer.entry(wire::BLOB, &[&encoded], cancelled)?;
        self.budget.charge(128)?;
        self.resources.insert(id);
        let extra_capacity = encoded.capacity() - encoded_size;
        drop(encoded);
        drop(description);
        self.budget.release(temporary_bytes + extra_capacity);
        Ok(())
    }
}

fn source_record(
    segment: &SegmentView,
    offset: u64,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<(Vec<u8>, Record), Error> {
    let mut reader = DiskRecording::open(segment)?;
    let bytes =
        read_record_bytes_with_cancel(&mut reader, offset, segment.committed_end, cancelled)?;
    let record = reader.decode(&bytes, cancelled)?;
    Ok((bytes, record))
}
