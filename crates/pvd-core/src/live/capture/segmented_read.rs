use super::*;
use crate::frame_sources::{FrameDirectory, FrameDirectoryLimits};
use persistent::BankLocation;

pub(super) struct Member {
    pub location: BankLocation,
    pub range: CaptureRange,
    pub bytes: u64,
    pub digest: [u8; 32],
}

pub fn open_segmented_with_progress(
    path: &Path,
    limits: &CaptureLimits,
    mut callback: impl FnMut(CaptureProgress) -> bool,
) -> Result<crate::Recording, Error> {
    let parts = segmented::read_manifest_with_progress(path, limits, &mut callback)?;
    let range = CaptureRange {
        first: parts[0].range.first,
        last: parts.last().unwrap().range.last,
    };
    let total = parts
        .iter()
        .try_fold(std::fs::metadata(path)?.len(), |total, part| {
            total
                .checked_add(part.bytes)
                .ok_or_else(|| error("capture set size overflow"))
        })?;
    let members = parts.into_iter().map(|part| Member {
        location: BankLocation::File(Arc::from(part.path)),
        range: part.range,
        bytes: part.bytes,
        digest: part.digest,
    });
    open_members(members, range, total, Format::Set, limits, callback)
}

pub(super) fn open_members(
    parts: impl ExactSizeIterator<Item = Member>,
    range: CaptureRange,
    total: u64,
    format: Format,
    limits: &CaptureLimits,
    mut callback: impl FnMut(CaptureProgress) -> bool,
) -> Result<crate::Recording, Error> {
    let mut budget = Budget::new(limits.index_bytes);
    // The member table, archive directory and paged FrameIndex coexist during admission.
    budget.entries(
        parts.len(),
        size_of::<Member>() + persistent::ArchiveReader::bank_index_bytes(),
    )?;
    let frames = range.last - range.first + 1;
    budget.entries(frames, 2 * size_of::<FrameInfo>() + size_of::<usize>())?;
    // The live frame-source navigation table has its own bounded reservation.
    // Keep the same ceiling when restoring a complete multi-member session.
    let source_limits = FrameDirectoryLimits::default();
    let sources = FrameDirectory::new(FrameDirectoryLimits {
        index_bytes: limits.index_bytes.min(source_limits.index_bytes),
        ..source_limits
    });
    let mut set = Assembly {
        recording: None,
        frames: Default::default(),
        sources,
        archive: persistent::ArchiveReader::capture(limits, format),
        last_segment_start: None,
        has_room_directory: false,
    };
    let mut completed = 0;
    for part in parts {
        if let BankLocation::File(path) = &part.location {
            budget.charge(path.as_os_str().len() + 2 * size_of::<usize>())?;
        }
        let mut reported = 0;
        let opened = part
            .location
            .open_member(limits, part.bytes, &mut |entry| {
                reported = reported.max(entry.completed.min(part.bytes));
                callback(CaptureProgress {
                    phase: CapturePhase::Validate,
                    completed: completed + reported,
                    total,
                })
            })
            .map_err(|failure| part.location.failure(failure))?;
        completed += part.bytes;
        set.append(&part, opened, &mut || {
            !callback(CaptureProgress {
                phase: CapturePhase::Validate,
                completed,
                total,
            })
        })
        .map_err(|failure| part.location.failure(failure))?;
    }
    progress(&mut callback, CapturePhase::Validate, total, total)?;
    let mut recording = set.recording.unwrap();
    recording.streamed_frame_end = Some(range.last + 1);
    recording.frames = set.frames;
    recording.frame_sources = set.sources;
    recording.disk = Some(DiskRecording {
        _owner: None,
        segments: Arc::default(),
        active: None,
        frames: recording.frames.clone(),
        stats: HistoryStats {
            disk_bytes: total,
            cache_disk_bytes: 0,
            persistent: false,
            budget_bytes: total,
            first_frame: range.first,
            retained_frames: frames,
            segments: set.archive.len(),
            warning: None,
        },
        analysis_cache: Default::default(),
        analysis_pages: None,
        archive: Some(set.archive),
    });
    read::note_visible_range(&mut recording, range);
    Ok(recording)
}

struct Assembly {
    recording: Option<crate::Recording>,
    frames: crate::FrameIndex,
    sources: FrameDirectory,
    archive: persistent::ArchiveReader,
    last_segment_start: Option<usize>,
    has_room_directory: bool,
}

impl Assembly {
    fn append(
        &mut self,
        part: &Member,
        opened: read::OpenedCapture,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), Error> {
        let report = opened.report;
        if report.format != Format::V3
            || report.range.first != part.range.first
            || report.range.last != part.range.last
            || report.bytes != part.bytes
            || report.digest != part.digest
        {
            return Err(error(
                "capture member differs from its directory entry (format, range, size or checksum)",
            ));
        }
        let mut member = opened.recording;
        let disk = member.disk.take().unwrap();
        let first_segment = disk.segments.front().unwrap();
        let restore_from = first_segment.frames.first().unwrap().info.index;
        if self.recording.is_some() && restore_from < part.range.first {
            if Some(restore_from) != self.last_segment_start {
                return Err(error(
                    "capture part restoration prefix starts in a different replay segment",
                ));
            }
            for location in first_segment
                .frames
                .iter()
                .take_while(|entry| entry.info.index < part.range.first)
            {
                check_replay_cancelled(cancelled)?;
                let info = &location.info;
                if let Some(prior) = self
                    .frames
                    .first()
                    .and_then(|first| info.index.checked_sub(first.index))
                    .and_then(|index| self.frames.get(index))
                    && (prior.timestamp != info.timestamp || prior.scene_id != info.scene_id)
                {
                    return Err(error(
                        "capture part restoration prefix disagrees with the preceding frame index",
                    ));
                }
            }
        }
        self.last_segment_start = Some(
            disk.segments
                .back()
                .unwrap()
                .frames
                .first()
                .unwrap()
                .info
                .index,
        );
        drop(disk);
        for frame in member.frames.iter() {
            check_replay_cancelled(cancelled)?;
            self.frames.push(frame.clone());
        }
        self.sources
            .append_capture_part(&member.frame_sources, cancelled)?;
        self.archive
            .publish(part.location.clone(), &report, restore_from)?;
        if let Some(recording) = &mut self.recording {
            if recording.header != member.header
                || recording.classes != member.classes
                || recording.diagnostics != member.diagnostics
                || self.has_room_directory != opened.has_room_directory
            {
                return Err(error("capture parts disagree on their export metadata"));
            }
            if opened.has_room_directory {
                for previous in recording.rooms.iter() {
                    check_replay_cancelled(cancelled)?;
                    let consistent = member.rooms.get(previous.scene).is_some_and(|current| {
                        current.number == previous.number
                            && current.first_observed_frame == previous.first_observed_frame
                            && previous
                                .destroyed_at_frame
                                .is_none_or(|frame| Some(frame) == current.destroyed_at_frame)
                    });
                    if !consistent {
                        return Err(error("capture parts disagree on the room directory"));
                    }
                }
                recording.rooms = member.rooms;
            } else {
                for room in member.rooms.iter() {
                    recording
                        .rooms
                        .observe(room.scene, room.first_observed_frame);
                }
            }
        } else {
            // Keep the first visible scene and analysis; drop its private frame
            // indices and file-backed replay segments once admission is done.
            member.frames = Default::default();
            member.frame_sources = Default::default();
            self.has_room_directory = opened.has_room_directory;
            self.recording = Some(member);
        }
        Ok(())
    }
}
