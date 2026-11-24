use super::*;
use std::io::SeekFrom;

pub(in crate::live) fn part_error(path: &Path, failure: Error) -> Error {
    let kind = failure
        .downcast_ref::<std::io::Error>()
        .map_or(std::io::ErrorKind::InvalidData, std::io::Error::kind);
    std::io::Error::new(kind, format!("capture part {}: {failure}", path.display())).into()
}

pub fn open_with_progress(
    path: &Path,
    limits: &CaptureLimits,
    callback: impl FnMut(CaptureProgress) -> bool,
) -> Result<crate::Recording, Error> {
    let opened = open_report_with_progress(path, limits, callback)?;
    let mut recording = opened.recording;
    note_visible_range(&mut recording, opened.report.range);
    Ok(recording)
}

pub(super) fn note_visible_range(recording: &mut crate::Recording, range: CaptureRange) {
    recording.diagnostics.push(format!("Project capture retains source frames {}..={}; hidden restoration history and analysis retention boundaries are preserved. Type metadata belongs to the export snapshot.", range.first, range.last));
}

pub(in crate::live) struct OpenedCapture {
    pub(in crate::live) recording: crate::Recording,
    pub(in crate::live) report: CaptureReport,
    pub(super) has_room_directory: bool,
}

pub(in crate::live) fn open_set_part(
    path: &Path,
    limits: &CaptureLimits,
    callback: &mut dyn FnMut(CaptureProgress) -> bool,
) -> Result<OpenedCapture, Error> {
    segmented::check_part_location(path)?;
    open_report_with_progress(path, limits, callback)
}

pub(in crate::live) fn open_report_with_progress(
    path: &Path,
    limits: &CaptureLimits,
    mut callback: impl FnMut(CaptureProgress) -> bool,
) -> Result<OpenedCapture, Error> {
    limits.validate()?;
    let path = std::fs::canonicalize(path)?;
    let mut file = open_file(&path)?;
    let bytes = file.metadata()?.len();
    let directory = directory(&mut file, bytes, limits, &mut callback)?;
    restore(path, file, 0, directory, limits, callback)
}

pub(super) fn open_file(path: &Path) -> Result<File, Error> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1);
    }
    Ok(options.open(path)?)
}

pub(in crate::live) fn open_embedded(
    path: &Path,
    file: &File,
    base: u64,
    bytes: u64,
    limits: &CaptureLimits,
    mut callback: &mut dyn FnMut(CaptureProgress) -> bool,
) -> Result<OpenedCapture, Error> {
    let view = packed_read::FileView::new(file, base, bytes)?;
    let directory = directory(&mut BufReader::new(view), bytes, limits, &mut callback)?;
    restore(
        path.to_owned(),
        file.try_clone()?,
        base,
        directory,
        limits,
        callback,
    )
}

fn restore(
    path: PathBuf,
    file: File,
    base: u64,
    directory: Directory,
    limits: &CaptureLimits,
    mut callback: impl FnMut(CaptureProgress) -> bool,
) -> Result<OpenedCapture, Error> {
    let report = directory.report;
    let bytes = report.bytes;
    let backing = Arc::new(CaptureBacking {
        format: directory.format,
        file,
        path: path.clone(),
        base,
        bytes,
        owner: std::sync::Mutex::new(None),
    });
    let mut budget = Budget::new(limits.index_bytes);
    budget.charge(directory.retained_bytes)?;
    let frame_count = directory
        .segments
        .iter()
        .try_fold(0usize, |n, segment| n.checked_add(segment.frames.len()))
        .ok_or_else(|| error("capture frame allocation overflow"))?;
    budget.entries(
        frame_count,
        2 * size_of::<FrameLocation>() + 2 * size_of::<FrameInfo>() + 32,
    )?;
    budget.entries(directory.segments.len(), 1024 + 2 * path.as_os_str().len())?;
    let remaining = limits.index_bytes - budget.used;
    let resources = Resources::from_capture(
        backing,
        directory.blocks,
        directory.blobs,
        remaining,
        &mut || {
            !callback(CaptureProgress {
                phase: CapturePhase::Validate,
                completed: bytes,
                total: bytes,
            })
        },
    )?;
    let mut segments = VecDeque::new();
    let mut frames = crate::FrameIndex::default();
    for segment in directory.segments {
        let handles = vec![resources.pin(segment.analysis_base)?];
        for frame in &segment.frames {
            if (report.range.first..=report.range.last).contains(&frame.info.index) {
                frames.push(frame.info.clone());
            }
        }
        segments.push_back(Arc::new(SegmentView {
            committed_end: bytes,
            path: path.clone(),
            frames: segment.frames.into_iter().collect(),
            checkpoints: Arc::new(segment.checkpoints),
            analysis_base: segment.analysis_base,
            bytes,
            resources: resources.clone(),
            lease: ResourceLease::new(handles),
            readers: SegmentReaders::default(),
            previous_frame: segment.previous_frame,
            analysis_continuity: segment.continuity,
        }));
    }
    let mut disk = DiskRecording {
        _owner: None,
        archive: None,
        segments: Arc::new(segments),
        active: None,
        frames: frames.clone(),
        stats: HistoryStats {
            disk_bytes: bytes,
            cache_disk_bytes: bytes,
            persistent: false,
            budget_bytes: bytes,
            first_frame: report.range.first,
            retained_frames: report.frames,
            segments: 0,
            warning: None,
        },
        analysis_cache: AnalysisCache::default(),
        analysis_pages: None,
    };
    disk.stats.segments = disk.segments.len();
    if validate_records(&disk, &directory.records, report.range, &mut callback)? {
        disk.analysis_pages = Some(analysis_pages::AnalysisPageReader::for_capture(&resources)?);
    }
    let metadata = directory.metadata;
    let mut recording = crate::RecordingBuilder::new(pvd_protocol::Header {
        version: metadata.sdk_version,
        timestamp_numerator: metadata.timestamp_numerator,
        timestamp_denominator: metadata.timestamp_denominator,
        flags: metadata.flags,
        endian: if metadata.little_endian {
            pvd_protocol::Endian::Little
        } else {
            pvd_protocol::Endian::Big
        },
    })
    .snapshot()?;
    recording.source = crate::RecordingSource::ProjectCapture;
    recording.frames = frames;
    recording.frame_sources = directory.frame_sources;
    recording.classes = metadata.classes;
    let has_room_directory = metadata.rooms.is_some();
    recording.rooms = metadata.rooms.unwrap_or_default();
    recording.diagnostics = metadata.diagnostics;
    recording.streamed_frame_end = Some(
        report
            .range
            .last
            .checked_add(1)
            .ok_or_else(|| error("capture frame index overflow"))?,
    );
    recording.disk = Some(disk);
    let mut cancelled = || {
        !callback(CaptureProgress {
            phase: CapturePhase::Validate,
            completed: bytes,
            total: bytes,
        })
    };
    recording.initial =
        Arc::new(recording.state_at_with_cancel(report.range.first, &mut cancelled)?);
    if !has_room_directory {
        recording
            .rooms
            .observe_state(&recording.initial, report.range.first);
        let mut first_observed = BTreeMap::new();
        for source in recording.frame_sources.sources() {
            let mut previous = None;
            for frame in source.frames() {
                check_replay_cancelled(&mut cancelled)?;
                if let Some(scene) = frame.scene
                    && previous != Some(scene)
                {
                    previous = Some(scene);
                    if recording.rooms.len() + first_observed.len() < crate::rooms::ROOM_LIMIT
                        && recording.rooms.get(scene).is_none()
                    {
                        first_observed.entry(frame.index).or_insert(scene);
                    }
                }
            }
        }
        for (frame, scene) in first_observed {
            recording.rooms.observe(scene, frame);
        }
    }
    recording.analysis = recording.analysis_at_with_cancel(report.range.first, &mut cancelled)?;
    check_replay_cancelled(&mut cancelled)?;
    Ok(OpenedCapture {
        recording,
        report,
        has_room_directory,
    })
}

fn validate_records(
    disk: &DiskRecording,
    records: &BTreeMap<u64, u64>,
    range: CaptureRange,
    callback: &mut impl FnMut(CaptureProgress) -> bool,
) -> Result<bool, Error> {
    let mut paged = None;
    for segment in disk.segments.iter() {
        let mut reader = DiskRecording::open(segment)?;
        let mut cancelled = || {
            !callback(CaptureProgress {
                phase: CapturePhase::Validate,
                completed: 0,
                total: disk.stats.disk_bytes,
            })
        };
        let baseline = read_record_with_cancel(
            &mut reader,
            segment.analysis_base,
            segment.committed_end,
            &mut cancelled,
        )?;
        let current = match baseline {
            Record::Analysis(_) => false,
            Record::AnalysisPages(root) => {
                segment.resources.pin(root)?;
                true
            }
            _ => return Err(error("invalid capture analysis baseline")),
        };
        if paged.is_some_and(|previous| previous != current) {
            return Err(error("capture mixes analysis baseline families"));
        }
        paged = Some(current);
        for (&frame, &offset) in segment.checkpoints.iter() {
            if !records.contains_key(&offset) {
                return Err(error("capture checkpoint is not an inline record"));
            }
            let Record::Checkpoint(index) = read_record_with_cancel(
                &mut reader,
                offset,
                segment.committed_end,
                &mut cancelled,
            )?
            else {
                return Err(error("invalid capture checkpoint record"));
            };
            for id in index
                .objects
                .values()
                .chain(index.debug.values())
                .chain(index.contacts.values())
            {
                segment.resources.pin(*id)?;
            }
            if segment
                .frames
                .binary_search_by_key(&frame, |frame| frame.info.index)
                .is_err()
            {
                return Err(error("capture checkpoint has no frame"));
            }
        }
        for frame in &segment.frames {
            if !records.contains_key(&frame.offset) {
                return Err(error("capture frame is not an inline record"));
            }
            let Record::Frame(stored) = read_record_with_cancel(
                &mut reader,
                frame.offset,
                segment.committed_end,
                &mut cancelled,
            )?
            else {
                return Err(error("invalid capture frame record"));
            };
            if stored.info.index != frame.info.index
                || stored.info.timestamp != frame.info.timestamp
                || stored.info.scene_id != frame.info.scene_id
            {
                return Err(error("capture frame and directory disagree"));
            }
            for id in stored
                .update
                .objects
                .iter()
                .chain(&stored.update.debug)
                .chain(&stored.update.contacts)
                .filter_map(|(_, id)| *id)
            {
                segment.resources.pin(id)?;
            }
        }
    }
    if disk.frames.first().map(|frame| frame.index) != Some(range.first)
        || disk.frames.last().map(|frame| frame.index) != Some(range.last)
    {
        return Err(error("capture visible frame bounds disagree"));
    }
    Ok(paged.unwrap_or(false))
}

pub(super) fn directory<R: Read + Seek>(
    input: &mut R,
    total: u64,
    limits: &CaptureLimits,
    callback: &mut impl FnMut(CaptureProgress) -> bool,
) -> Result<Directory, Error> {
    limits.validate()?;
    if total > limits.output_bytes || total < 8 {
        return Err(error("project capture size exceeds its limits"));
    }
    input.seek(SeekFrom::Start(0))?;
    let mut magic = [0; 8];
    input.read_exact(&mut magic)?;
    let format =
        Format::from_magic(&magic)?.ok_or_else(|| error("invalid project capture header"))?;
    if !matches!(format, Format::V1 | Format::V2 | Format::V3) {
        return Err(error("capture member must be a single PVDCAP01/02/03 file"));
    }
    let mut hash = blake3::Hasher::new();
    hash.update(&magic);
    let mut position = 8u64;
    let mut budget = Budget::new(limits.index_bytes);
    let mut metadata = None;
    let mut source_directory = None;
    let mut segments: Vec<Segment> = Vec::new();
    let mut blocks = Vec::new();
    let mut blobs = Vec::new();
    let mut records = BTreeMap::new();
    let mut block_ids = BTreeSet::new();
    let mut blob_ids = BTreeSet::new();
    let mut frame_total = 0usize;
    loop {
        progress(callback, CapturePhase::Validate, position, total)?;
        let entry_start = position;
        let mut header = [0; wire::ENTRY_HEADER];
        input.read_exact(&mut header)?;
        let kind = header[0];
        let length = usize::try_from(u64::from_le_bytes(header[1..9].try_into()?))?;
        let end = position
            .checked_add(header.len() as u64)
            .and_then(|n| n.checked_add(length as u64))
            .filter(|end| *end <= total)
            .ok_or_else(|| error("capture entry lies outside the file"))?;
        if length > wire::MAX_ENTRY {
            return Err(error("capture entry exceeds its limit"));
        }
        if !matches!(
            kind,
            wire::BLOCK
                | wire::BLOB
                | wire::RECORD
                | wire::SEGMENT
                | wire::METADATA
                | wire::FRAME_SOURCES
                | wire::FOOTER
        ) {
            return Err(error("unknown project capture entry"));
        }
        if source_directory.is_some() && kind != wire::FOOTER {
            return Err(error(
                "capture frame source directory must immediately precede the footer",
            ));
        }
        if kind == wire::FRAME_SOURCES && format != Format::V3 {
            return Err(error("frame source directory requires capture version 3"));
        }
        if kind == wire::BLOCK && length > (64 << 10) + 44 {
            return Err(error("capture block exceeds 64 KiB"));
        }
        if kind == wire::FOOTER && length != 48 {
            return Err(error("invalid capture footer length"));
        }
        if kind == wire::METADATA && length > 8 << 20 {
            return Err(error("capture metadata exceeds 8 MiB"));
        }
        if kind != wire::FOOTER {
            hash.update(&header);
        }
        let mut entry_hash = blake3::Hasher::new();
        entry_hash.update(&header[..9]);
        position += header.len() as u64;
        let payload_offset = position;
        let mut payload = Vec::new();
        if kind != wire::RECORD {
            budget.reserve(&mut payload, length)?;
        }
        let mut remaining = length;
        let mut scratch = [0u8; 64 << 10];
        let mut inner_header = [0u8; 36];
        let mut inner_header_bytes = 0;
        let mut inner_hash = blake3::Hasher::new();
        while remaining != 0 {
            progress(callback, CapturePhase::Validate, position, total)?;
            let bytes = &mut scratch[..remaining.min(64 << 10)];
            input.read_exact(bytes)?;
            entry_hash.update(bytes);
            if kind != wire::FOOTER {
                hash.update(bytes);
            }
            if kind == wire::RECORD {
                let header_part = bytes.len().min(36 - inner_header_bytes);
                inner_header[inner_header_bytes..inner_header_bytes + header_part]
                    .copy_from_slice(&bytes[..header_part]);
                inner_header_bytes += header_part;
                inner_hash.update(&bytes[header_part..]);
            } else {
                payload.extend_from_slice(bytes);
            }
            remaining -= bytes.len();
            position += bytes.len() as u64;
        }
        if entry_hash.finalize().as_bytes() != &header[9..] {
            return Err(error("capture entry checksum mismatch"));
        }
        match kind {
            wire::BLOCK => {
                let mut data = wire::Input::new(&payload);
                let id = data.u64()?;
                let length = u32::from_le_bytes(data.take(4)?.try_into()?) as usize;
                let digest: [u8; 32] = data.take(32)?.try_into()?;
                let contents = data.take(length)?;
                data.finish()?;
                if blake3::hash(contents).as_bytes() != &digest
                    || block_ids.contains(&id)
                    || id == 0
                    || id >= 1 << 63
                {
                    return Err(error("invalid capture block checksum or ID"));
                }
                budget.charge(128)?;
                block_ids.insert(id);
                budget.push(
                    &mut blocks,
                    BlockDescription {
                        id,
                        len: length,
                        hash: digest,
                        offset: payload_offset + 8,
                    },
                )?;
            }
            wire::BLOB => {
                let mut data = wire::Input::new(&payload);
                let id = data.u64()?;
                let len = data.usize()?;
                let hash = data.take(32)?.try_into()?;
                if id < 1 << 63 || len > MAX_RECORD || blob_ids.contains(&id) {
                    return Err(error("invalid capture resource ID or length"));
                }
                let count = data.count(limits.resources, 8)?;
                let mut references = Vec::new();
                budget.reserve(&mut references, count)?;
                for item in 0..count {
                    if item.is_multiple_of(256) {
                        progress(callback, CapturePhase::Validate, position, total)?;
                    }
                    let block = data.u64()?;
                    if !block_ids.contains(&block) {
                        return Err(error("capture resource references a missing block"));
                    }
                    references.push(block);
                }
                let count = data.count(limits.resources, 8)?;
                let mut dependencies = Vec::new();
                budget.reserve(&mut dependencies, count)?;
                for item in 0..count {
                    if item.is_multiple_of(256) {
                        progress(callback, CapturePhase::Validate, position, total)?;
                    }
                    let dependency = data.u64()?;
                    if dependency >= id
                        || !blob_ids.contains(&dependency)
                        || dependencies.last().is_some_and(|last| *last >= dependency)
                    {
                        return Err(error("invalid capture dependency closure"));
                    }
                    dependencies.push(dependency);
                }
                data.finish()?;
                budget.charge(128)?;
                blob_ids.insert(id);
                budget.push(
                    &mut blobs,
                    BlobDescription {
                        id,
                        len,
                        hash,
                        blocks: references,
                        dependencies,
                    },
                )?;
            }
            wire::RECORD => {
                if inner_header_bytes != 36
                    || u32::from_le_bytes(inner_header[..4].try_into()?) as usize != length - 36
                    || inner_hash.finalize().as_bytes() != &inner_header[4..]
                {
                    return Err(error("capture inline record checksum mismatch"));
                }
                budget.charge(160)?;
                records.insert(payload_offset, (length - 36) as u64);
            }
            wire::SEGMENT => {
                let remaining = CaptureLimits {
                    frames: limits.frames - frame_total,
                    ..limits.clone()
                };
                let segment = wire::decode_segment(&payload, &remaining, &mut budget)?;
                if !blob_ids.contains(&segment.analysis_base) {
                    return Err(error("capture analysis baseline is missing"));
                }
                if segments.last().is_some_and(|previous| {
                    previous.frames.last().unwrap().info.index.checked_add(1)
                        != Some(segment.frames[0].info.index)
                }) {
                    return Err(error("capture segment directory has a gap or overlap"));
                }
                frame_total = frame_total
                    .checked_add(segment.frames.len())
                    .filter(|count| *count <= limits.frames)
                    .ok_or_else(|| {
                        crate::CapacityLimit::exceeded("capture frame count exceeds its limit")
                    })?;
                budget.push(&mut segments, segment)?;
            }
            wire::METADATA => {
                if metadata.is_some() || entry_start != 8 {
                    return Err(error(
                        "capture metadata must appear exactly once at the start",
                    ));
                }
                let value = metadata::decode(&payload, &mut budget, &mut || {
                    !callback(CaptureProgress {
                        phase: CapturePhase::Validate,
                        completed: position,
                        total,
                    })
                })?;
                metadata = Some(value);
            }
            wire::FRAME_SOURCES => {
                source_directory = Some(std::mem::take(&mut payload));
            }
            wire::FOOTER => {
                if end != total {
                    return Err(error("trailing bytes after project capture footer"));
                }
                let mut data = wire::Input::new(&payload);
                let before = data.u64()?;
                let frames = data.usize()?;
                let digest: [u8; 32] = data.take(32)?.try_into()?;
                if before != entry_start || hash.finalize().as_bytes() != &digest {
                    return Err(error("project capture file checksum mismatch"));
                }
                let metadata = metadata.ok_or_else(|| error("capture metadata is missing"))?;
                let first = usize::try_from(metadata.first)?;
                let last = usize::try_from(metadata.last)?;
                if last.checked_sub(first).and_then(|n| n.checked_add(1)) != Some(frames)
                    || frames > limits.frames
                    || frames == 0
                    || segments
                        .first()
                        .is_none_or(|segment| segment.frames[0].info.index > first)
                    || segments
                        .last()
                        .is_none_or(|segment| segment.frames.last().unwrap().info.index != last)
                {
                    return Err(error(
                        "capture visible range is not covered by its segment directory",
                    ));
                }
                let visible = segments
                    .iter()
                    .flat_map(|segment| &segment.frames)
                    .filter(|frame| frame.info.index >= first)
                    .count();
                if visible != frames {
                    return Err(error("capture visible frame count differs"));
                }
                let frame_index_bytes = frames
                    .max(4)
                    .checked_mul(2 * size_of::<FrameInfo>() + 32)
                    .and_then(|bytes| bytes.checked_add(128))
                    .ok_or_else(|| error("capture source frame index size overflow"))?;
                budget.charge(frame_index_bytes)?;
                let mut visible_frames = crate::FrameIndex::default();
                for frame in segments.iter().flat_map(|segment| &segment.frames) {
                    if (first..=last).contains(&frame.info.index) {
                        progress(callback, CapturePhase::Validate, position, total)?;
                        visible_frames.push(frame.info.clone());
                    }
                }
                let source_limits = crate::frame_sources::FrameDirectoryLimits {
                    index_bytes: limits.index_bytes.saturating_sub(budget.used),
                    frames: limits.frames,
                    ..Default::default()
                };
                let frame_sources = match (format, source_directory) {
                    (Format::V3, Some(source_bytes)) => {
                        let directory = crate::frame_sources::wire::decode_with_cancel(
                            &source_bytes,
                            &visible_frames,
                            source_limits,
                            || {
                                !callback(CaptureProgress {
                                    phase: CapturePhase::Validate,
                                    completed: position,
                                    total,
                                })
                            },
                        )?;
                        budget.charge(directory.index_bytes())?;
                        let temporary = source_bytes.capacity();
                        drop(source_bytes);
                        budget.release(temporary);
                        directory
                    }
                    (Format::V3, None) => {
                        return Err(error("capture frame source directory is missing"));
                    }
                    (_, None) => {
                        let directory =
                            crate::frame_sources::FrameDirectory::from_legacy_with_cancel(
                                &visible_frames,
                                source_limits,
                                || {
                                    !callback(CaptureProgress {
                                        phase: CapturePhase::Validate,
                                        completed: position,
                                        total,
                                    })
                                },
                            )?;
                        budget.charge(directory.index_bytes())?;
                        directory
                    }
                    (_, Some(_)) => {
                        return Err(error("legacy capture contains a frame source directory"));
                    }
                };
                drop(visible_frames);
                budget.release(frame_index_bytes);
                hash.update(&header);
                hash.update(&payload);
                let digest = *hash.finalize().as_bytes();
                let temporary = payload.capacity();
                drop(payload);
                budget.release(temporary);
                let ids = block_ids.len() + blob_ids.len();
                drop(block_ids);
                drop(blob_ids);
                budget.release(ids * 128);
                let report = CaptureReport {
                    format,
                    range: CaptureRange { first, last },
                    frames,
                    bytes: total,
                    digest,
                    resource_count: blobs.len(),
                    block_count: blocks.len(),
                    index_bytes: budget.peak,
                };
                return Ok(Directory {
                    format,
                    retained_bytes: budget.used,
                    frame_sources,
                    metadata,
                    segments,
                    blocks,
                    blobs,
                    records,
                    report,
                });
            }
            _ => unreachable!(),
        }
        let temporary = payload.capacity();
        drop(payload);
        budget.release(temporary);
        if block_ids
            .len()
            .checked_add(blob_ids.len())
            .is_none_or(|n| n > limits.resources)
        {
            return Err(crate::CapacityLimit::exceeded(
                "capture resource count exceeds its limit",
            ));
        }
    }
}
