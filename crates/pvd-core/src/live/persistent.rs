//! Append-only complete-session banks, independent of the rolling writer cache.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

const MAX_BANKS: usize = 4096;
const BANK_INDEX_BYTES: usize = 128 << 20;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SealPhase {
    Write,
    Admission,
    WaitingForReaders,
    Retire,
}

#[derive(Clone, Copy)]
pub(super) struct PressureBaseline {
    disk_bytes: u64,
    metadata_bytes: usize,
    lease_bytes: usize,
    resource_pressure: bool,
}

#[cfg(test)]
mod tests;

#[derive(Clone, Debug)]
struct Bank {
    first: usize,
    last: usize,
    restore_from: usize,
    location: BankLocation,
    bytes: u64,
    digest: [u8; 32],
    admission: Option<capture::BankAdmission>,
}

#[derive(Clone, Debug)]
pub(super) enum BankLocation {
    File(Arc<Path>),
    Embedded {
        path: Arc<Path>,
        file: Arc<File>,
        offset: u64,
    },
}

impl BankLocation {
    pub(super) fn open_member(
        &self,
        limits: &capture::CaptureLimits,
        bytes: u64,
        callback: &mut dyn FnMut(capture::CaptureProgress) -> bool,
    ) -> Result<capture::read::OpenedCapture, Error> {
        match self {
            Self::File(path) => capture::read::open_set_part(path, limits, callback),
            Self::Embedded { path, file, offset } => {
                capture::read::open_embedded(path, file, *offset, bytes, limits, callback)
            }
        }
    }

    pub(super) fn failure(&self, failure: Error) -> Error {
        match self {
            Self::File(path) => capture::read::part_error(path, failure),
            Self::Embedded { path, offset, .. } => {
                let kind = failure
                    .downcast_ref::<std::io::Error>()
                    .map_or(std::io::ErrorKind::InvalidData, std::io::Error::kind);
                std::io::Error::new(
                    kind,
                    format!(
                        "capture member at byte {offset} in {}: {failure}",
                        path.display()
                    ),
                )
                .into()
            }
        }
    }
}

#[derive(Debug, Default)]
struct Directory {
    banks: Vec<Bank>,
    bytes: u64,
    cached: Option<(usize, Arc<crate::Recording>)>,
    prepared: Option<PreparedBank>,
}

#[derive(Debug)]
struct PreparedBank {
    index: usize,
    recording: Arc<crate::Recording>,
    cancelled: Arc<AtomicBool>,
}

#[derive(Clone)]
pub struct ReplayLookahead {
    directory: Weak<Mutex<Directory>>,
    index: usize,
    frame: usize,
}

impl ReplayLookahead {
    pub fn frame(&self) -> usize {
        self.frame
    }

    pub fn same_bank(&self, other: &Self) -> bool {
        self.index == other.index && Weak::ptr_eq(&self.directory, &other.directory)
    }

    pub fn prepare(
        &self,
        recording: &crate::Recording,
        cancelled: Arc<AtomicBool>,
    ) -> Result<bool, Error> {
        let Some(archive) = recording
            .disk
            .as_ref()
            .and_then(|disk| disk.archive.as_ref())
        else {
            return Ok(false);
        };
        if !self.directory.ptr_eq(&Arc::downgrade(&archive.directory)) {
            return Ok(false);
        }
        let Ok(_opening) = archive.opening.try_lock() else {
            return Ok(false);
        };
        let (bank, budget, discarded) = {
            let mut directory = archive
                .directory
                .lock()
                .map_err(|_| error("complete-session bank lock poisoned"))?;
            if directory
                .cached
                .as_ref()
                .is_some_and(|(index, _)| *index == self.index)
            {
                return Ok(false);
            }
            let resident = directory
                .cached
                .as_ref()
                .and_then(|(index, _)| directory.banks[*index].admission)
                .map_or(0, |admission| admission.index_bytes);
            let budget = archive.index_limit.saturating_sub(resident);
            let bank = &directory.banks[self.index];
            if bank
                .admission
                .is_none_or(|admission| admission.index_bytes > budget)
            {
                return Ok(false);
            }
            (bank.clone(), budget, directory.prepared.take())
        };
        drop(discarded);
        let mut is_cancelled = || cancelled.load(Ordering::Acquire);
        check_replay_cancelled(&mut is_cancelled)?;
        let recording = archive.read_bank(&bank, budget, &mut is_cancelled)?;
        let mut directory = archive
            .directory
            .lock()
            .map_err(|_| error("complete-session bank lock poisoned"))?;
        check_replay_cancelled(&mut is_cancelled)?;
        directory.prepared = Some(PreparedBank {
            index: self.index,
            recording,
            cancelled,
        });
        Ok(true)
    }

    pub fn cancel(&self, cancelled: &Arc<AtomicBool>) -> Option<Arc<crate::Recording>> {
        cancelled.store(true, Ordering::Release);
        self.directory
            .upgrade()?
            .lock()
            .ok()?
            .prepared
            .take_if(|bank| bank.index == self.index && Arc::ptr_eq(&bank.cancelled, cancelled))
            .map(|bank| bank.recording)
    }
}

#[derive(Clone, Debug)]
pub(super) struct ArchiveReader {
    source: ArchiveSource,
    pub(super) index_limit: usize,
    directory: Arc<Mutex<Directory>>,
    transition: Arc<std::sync::RwLock<()>>,
    opening: Arc<Mutex<()>>,
}

#[derive(Clone, Debug)]
enum ArchiveSource {
    Temporary(Arc<CacheDirectory>),
    Capture(capture::CaptureLimits, capture::Format),
}

impl ArchiveReader {
    pub(super) fn bank_index_bytes() -> usize {
        2 * size_of::<Bank>()
    }

    fn new(owner: Arc<CacheDirectory>) -> Self {
        Self::with_source(ArchiveSource::Temporary(owner), BANK_INDEX_BYTES)
    }

    pub(super) fn capture(limits: &capture::CaptureLimits, format: capture::Format) -> Self {
        Self::with_source(
            ArchiveSource::Capture(limits.clone(), format),
            limits.index_bytes,
        )
    }

    fn with_source(source: ArchiveSource, index_limit: usize) -> Self {
        Self {
            source,
            index_limit,
            directory: Arc::new(Mutex::new(Directory::default())),
            transition: Arc::new(std::sync::RwLock::new(())),
            opening: Arc::new(Mutex::new(())),
        }
    }

    pub(super) fn capture_format(&self) -> Option<capture::Format> {
        match &self.source {
            ArchiveSource::Temporary(_) => None,
            ArchiveSource::Capture(_, format) => Some(*format),
        }
    }

    // A later part can contain the entire earlier prefix of its first replay
    // segment. Export this segment once, from the last applicable part.
    pub(super) fn export_location(&self, first: usize, last: usize) -> Result<usize, Error> {
        if self.capture_format().is_none() {
            return Ok(first);
        }
        let directory = self
            .directory
            .lock()
            .map_err(|_| error("capture set lock poisoned"))?;
        let end = directory
            .banks
            .partition_point(|bank| bank.first <= last && bank.restore_from <= first);
        Ok(end
            .checked_sub(1)
            .map_or(first, |index| first.max(directory.banks[index].first)))
    }

    fn bank(&self, frame: usize) -> Option<(usize, Bank)> {
        let directory = self.directory.lock().ok()?;
        let index = directory.banks.partition_point(|bank| bank.last < frame);
        let bank = directory.banks.get(index)?;
        (bank.first <= frame).then(|| (index, bank.clone()))
    }

    pub(super) fn shares_resource_ids(&self, other: &Self) -> bool {
        matches!(&self.source, ArchiveSource::Temporary(_))
            && Arc::ptr_eq(&self.directory, &other.directory)
    }

    pub(super) fn contains(&self, frame: usize) -> bool {
        let Ok(directory) = self.directory.lock() else {
            return false;
        };
        let index = directory.banks.partition_point(|bank| bank.last < frame);
        directory
            .banks
            .get(index)
            .is_some_and(|bank| bank.first <= frame)
    }
    pub(super) fn bytes(&self) -> u64 {
        self.directory.lock().map_or(0, |directory| directory.bytes)
    }
    pub(super) fn len(&self) -> usize {
        self.directory
            .lock()
            .map_or(0, |directory| directory.banks.len())
    }

    fn open(
        &self,
        frame: usize,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Option<Arc<crate::Recording>>, Error> {
        let Some((index, bank)) = self.bank(frame) else {
            return Ok(None);
        };
        check_replay_cancelled(cancelled)?;
        if let Some(cached) = self.cached(index)? {
            return Ok(Some(cached));
        }
        // Cached reads continue while another bank is opening.
        let _opening = self
            .opening
            .lock()
            .map_err(|_| error("bank open lock poisoned"))?;
        check_replay_cancelled(cancelled)?;
        if let Some(cached) = self.cached(index)? {
            return Ok(Some(cached));
        }
        let discarded = self
            .directory
            .lock()
            .map_err(|_| error("complete-session bank lock poisoned"))?
            .prepared
            .take();
        drop(discarded);
        let recording = self.read_bank(&bank, self.index_limit, cancelled)?;
        check_replay_cancelled(cancelled)?;
        self.directory
            .lock()
            .map_err(|_| error("complete-session bank lock poisoned"))?
            .cached = Some((index, recording.clone()));
        Ok(Some(recording))
    }

    fn cached(&self, index: usize) -> Result<Option<Arc<crate::Recording>>, Error> {
        let mut directory = self
            .directory
            .lock()
            .map_err(|_| error("complete-session bank lock poisoned"))?;
        let cancelled = directory
            .prepared
            .take_if(|bank| bank.cancelled.load(Ordering::Acquire));
        let recording = if let Some((cached_index, recording)) = &directory.cached
            && *cached_index == index
        {
            Some(recording.clone())
        } else if let Some(bank) = directory.prepared.take_if(|bank| bank.index == index) {
            directory.cached = Some((index, bank.recording.clone()));
            Some(bank.recording)
        } else {
            None
        };
        drop(directory);
        drop(cancelled);
        Ok(recording)
    }

    fn read_bank(
        &self,
        bank: &Bank,
        index_bytes: usize,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Arc<crate::Recording>, Error> {
        let limits = capture::CaptureLimits {
            index_bytes,
            output_bytes: bank.bytes.max(4096),
            ..match &self.source {
                ArchiveSource::Temporary(_) => capture::CaptureLimits::default(),
                ArchiveSource::Capture(limits, _) => limits.clone(),
            }
        };
        let mut progress = |_| !cancelled();
        let progress: &mut dyn FnMut(capture::CaptureProgress) -> bool = &mut progress;
        let mut recording =
            if let (BankLocation::File(path), Some(admission)) = (&bank.location, bank.admission) {
                capture::open_bank(path, &limits, admission, progress)?
            } else {
                let opened = bank
                    .location
                    .open_member(&limits, bank.bytes, progress)
                    .map_err(|failure| bank.location.failure(failure))?;
                if opened.report.digest != bank.digest {
                    return Err(bank
                        .location
                        .failure(error("contents changed after capture admission")));
                }
                opened.recording
            };
        if let ArchiveSource::Temporary(owner) = &self.source {
            // A replay cursor may outlive the recording that opened a temporary bank.
            let disk = recording.disk.as_mut().unwrap();
            disk.segments
                .front()
                .unwrap()
                .resources
                .capture_backing()
                .unwrap()
                .keep_owner(owner.clone())?;
            disk._owner = Some(owner.clone());
        }
        Ok(Arc::new(recording))
    }

    fn lookahead(&self, from: usize, ahead: usize) -> Option<ReplayLookahead> {
        if !matches!(&self.source, ArchiveSource::Temporary(_)) {
            return None;
        }
        let directory = self.directory.lock().ok()?;
        let current = directory.banks.partition_point(|bank| bank.last < from);
        let (index, frame) = if ahead > from {
            let index = current.checked_add(1)?;
            let bank = directory.banks.get(index)?;
            (index, bank.first)
        } else {
            let index = current.checked_sub(1)?;
            let bank = directory.banks.get(index)?;
            (index, bank.last)
        };
        if !(from.min(ahead)..=from.max(ahead)).contains(&frame) || frame == from {
            return None;
        }
        Some(ReplayLookahead {
            directory: Arc::downgrade(&self.directory),
            index,
            frame,
        })
    }

    pub(super) fn publish(
        &self,
        location: BankLocation,
        report: &capture::CaptureReport,
        restore_from: usize,
        admission: Option<capture::BankAdmission>,
    ) -> Result<(), Error> {
        let bank = Bank {
            first: report.range.first,
            last: report.range.last,
            bytes: report.bytes,
            digest: report.digest,
            admission,
            location,
            restore_from,
        };
        let mut directory = self
            .directory
            .lock()
            .map_err(|_| error("complete-session bank lock poisoned"))?;
        if directory.banks.last().is_some_and(|last| {
            last.last.checked_add(1) != Some(bank.first) || last.restore_from > bank.restore_from
        }) {
            return Err(error("complete-session banks must be contiguous"));
        }
        directory.bytes = directory
            .bytes
            .checked_add(bank.bytes)
            .ok_or_else(|| error("archive size overflow"))?;
        directory.banks.push(bank);
        Ok(())
    }
}

impl crate::Recording {
    pub fn replay_lookahead(&self, from: usize, ahead: usize) -> Option<ReplayLookahead> {
        self.disk.as_ref()?.archive.as_ref()?.lookahead(from, ahead)
    }
}

impl DiskRecording {
    pub(super) fn history_guard(
        &self,
    ) -> Result<Option<std::sync::RwLockReadGuard<'_, ()>>, Error> {
        self.archive
            .as_ref()
            .map(|archive| {
                archive
                    .transition
                    .read()
                    .map_err(|_| error("complete-session transition lock poisoned"))
            })
            .transpose()
    }
    pub(crate) fn archived(
        &self,
        frame: usize,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Option<Arc<crate::Recording>>, Error> {
        if self
            .frames
            .binary_search_by_key(&frame, |info| info.index)
            .is_err()
        {
            return Ok(None);
        }
        match &self.archive {
            Some(archive) => archive.open(frame, cancelled),
            None => Ok(None),
        }
    }

    pub(super) fn owned_segment(
        &self,
        frame: usize,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Arc<SegmentView>, Error> {
        check_replay_cancelled(cancelled)?;
        if let Some(recording) = self.archived(frame, cancelled)? {
            return Ok(Arc::new(
                recording.disk.as_ref().unwrap().segment(frame)?.clone(),
            ));
        }
        Ok(Arc::new(self.segment(frame)?.clone()))
    }
}

impl HistoryWriter {
    pub(crate) fn new_persistent(config: HistoryConfig) -> Result<Self, Error> {
        let mut writer = Self::new_paged(config)?;
        let owner = writer
            .owner
            .clone()
            .ok_or_else(|| error("complete-session history directory is unavailable"))?;
        writer.archive = Some(ArchiveReader::new(owner));
        Ok(writer)
    }

    pub(super) fn stop_retention(&mut self, reason: impl std::fmt::Display) {
        self.retention_stopped = true;
        self.warning = Some(format!(
            "Complete-session recording stopped; earlier frames remain available: {reason}"
        ));
    }

    pub(crate) fn accepting_frames(&self) -> bool {
        !self.retention_stopped
    }

    /// Global navigation remains a bounded in-memory paged index (one million frames).
    /// Crossing that boundary stops appending; it never evicts the session prefix.
    pub(crate) fn prepare_persistent(
        &mut self,
        source: &crate::Recording,
        planned_records: usize,
    ) -> Result<(), Error> {
        self.cancellation.check()?;
        if self.archive.is_none() || self.retention_stopped {
            return Ok(());
        }
        if self.frame_infos.len() >= MAX_INDEX_FRAMES {
            self.stop_retention(
                "the complete-session navigation limit of 1,000,000 frames was reached",
            );
            return Ok(());
        }
        let hot_frames = self
            .completed
            .iter()
            .map(|segment| segment.frames.len())
            .sum::<usize>()
            + self
                .active
                .as_ref()
                .map_or(0, |segment| segment.view.frames.len());
        // Group ordinary segments into a bank: static meshes are copied once per bank,
        // rather than once per 600-frame replay segment.
        let frame_limit = self
            .config
            .segment_frames
            .saturating_mul(20)
            .clamp(20, 12_000);
        if hot_frames == 0 {
            return Ok(());
        }
        let current = PressureBaseline {
            disk_bytes: self.bytes(),
            metadata_bytes: self.resources.as_ref().map_or(0, Resources::metadata_bytes),
            lease_bytes: self.lease_bytes(),
            resource_pressure: self
                .resources
                .as_ref()
                .is_some_and(|resources| resources.needs_reclaim(planned_records)),
        };
        // The first hot frame includes the live scene and the analysis writer's
        // fixed reservation. Sealing cannot reclaim either of those baselines.
        let baseline = *self.persistent_baseline.get_or_insert(current);
        let disk_growth_limit = (self.config.disk_budget_bytes / 2)
            .min(
                self.config
                    .disk_budget_bytes
                    .saturating_sub(baseline.disk_bytes)
                    / 2,
            )
            .max(1);
        let metadata_growth = current
            .metadata_bytes
            .saturating_sub(baseline.metadata_bytes);
        let metadata_growth_limit = self.resources.as_ref().map_or(32 << 20, |resources| {
            (resources
                .metadata_capacity()
                .saturating_sub(16 << 20)
                .saturating_sub(baseline.metadata_bytes)
                / 2)
            .clamp(1, 32 << 20)
        });
        let pressure = hot_frames >= frame_limit
            || current.disk_bytes >= self.config.disk_budget_bytes
            || current.disk_bytes.saturating_sub(baseline.disk_bytes) >= disk_growth_limit
            || current.lease_bytes.saturating_sub(baseline.lease_bytes) >= 32 << 20
            || metadata_growth >= metadata_growth_limit
            || (metadata_growth != 0 && current.resource_pressure && !baseline.resource_pressure);
        if pressure && let Err(failure) = self.seal_bank(source) {
            if failure
                .downcast_ref::<std::io::Error>()
                .is_some_and(|failure| failure.kind() == std::io::ErrorKind::Interrupted)
            {
                return Err(failure);
            }
            self.cancellation.check()?;
            self.stop_retention(failure);
        }
        Ok(())
    }

    pub(super) fn seal_bank(&mut self, source: &crate::Recording) -> Result<(), Error> {
        self.seal_bank_with_progress(source, |_, _| {})
    }

    fn seal_bank_with_progress(
        &mut self,
        source: &crate::Recording,
        mut progress: impl FnMut(SealPhase, Option<capture::CaptureProgress>),
    ) -> Result<(), Error> {
        let cancellation = self.cancellation.clone();
        cancellation.check()?;
        let _timer = self.metrics.measure(Stage::HistorySeal);
        let archive = self.archive.as_ref().unwrap().clone();
        if archive.len() >= MAX_BANKS {
            return Err(error(
                "complete-session bank directory reached its 4096-bank limit",
            ));
        }
        self.close_segment();
        let Some(first) = self
            .completed
            .front()
            .and_then(|segment| segment.frames.first())
            .map(|frame| frame.info.index)
        else {
            return Ok(());
        };
        let last = self
            .completed
            .back()
            .unwrap()
            .frames
            .last()
            .unwrap()
            .info
            .index;
        let owner = self.owner.as_ref().unwrap().clone();
        let available = match available_disk_space(&owner.path)? {
            Some(space) => space.saturating_sub(self.config.min_free_disk_bytes),
            None if self.config.min_free_disk_bytes == 0 => (1 << 63) - 1,
            None => {
                return Err(error(
                    "free disk space cannot verify the configured reserve on this platform",
                ));
            }
        };
        if available < 4096 {
            return Err(error(
                "free disk space cannot preserve the configured reserve",
            ));
        }
        let path = owner.path.join(format!(
            "{:020}.pvdhistory",
            (1u64 << 61) + archive.len() as u64
        ));
        let mut output = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        owner
            .files
            .lock()
            .map_err(|_| error("history ownership lock poisoned"))?
            .insert(path.clone());
        let mut recording = source.clone();
        let mut disk = self.snapshot();
        disk.archive = None;
        recording.frames = disk.frames.clone();
        recording.disk = Some(disk);
        let limits = capture::CaptureLimits {
            index_bytes: archive.index_limit,
            output_bytes: available.min((1 << 63) - 1),
            ..Default::default()
        };
        let result = capture::write_bank(
            &recording,
            &mut output,
            capture::CaptureRange { first, last },
            &limits,
            |value| {
                progress(SealPhase::Write, Some(value));
                !cancellation.requested()
            },
        )
        .and_then(|report| {
            cancellation.check()?;
            output.sync_all()?;
            cancellation.check()?;
            Ok(report)
        });
        drop(output);
        let report = match result {
            Ok(report) => report,
            Err(failure) => {
                return Err(self.remove_incomplete_bank(&owner, &path, failure));
            }
        };
        // Opening needs a larger resource directory than the export footer alone.
        // Verify the actual lazy-reader admission before giving up the hot copy.
        let admission = match capture::admit_bank(&path, &limits, report.digest, |value| {
            progress(SealPhase::Admission, Some(value));
            !cancellation.requested()
        }) {
            Ok(admission) => admission,
            Err(failure) => return Err(self.remove_incomplete_bank(&owner, &path, failure)),
        };
        // Readers choose hot/cold and consume one frame or export segment under a
        // read guard. Publishing and retiring the hot copy must be one transition.
        let _transition = loop {
            if let Err(failure) = cancellation.check() {
                return Err(self.remove_incomplete_bank(&owner, &path, failure));
            }
            match archive.transition.try_write() {
                Ok(guard) => break guard,
                Err(std::sync::TryLockError::WouldBlock) => {
                    progress(SealPhase::WaitingForReaders, None);
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    let failure = error("complete-session transition lock poisoned");
                    return Err(self.remove_incomplete_bank(&owner, &path, failure));
                }
            }
        };
        if let Err(failure) = cancellation.check() {
            return Err(self.remove_incomplete_bank(&owner, &path, failure));
        }
        if let Err(failure) = archive.publish(
            BankLocation::File(Arc::from(path.as_path())),
            &report,
            first,
            Some(admission),
        ) {
            return Err(self.remove_incomplete_bank(&owner, &path, failure));
        }
        // Publication precedes lease invalidation, so old snapshots can always route
        // the same visible frame to either its hot segment or the sealed bank.
        drop(recording);
        loop {
            progress(SealPhase::Retire, None);
            cancellation.check()?;
            if !self.evict_one()? {
                break;
            }
        }
        cancellation.check()?;
        self.reclaim_analysis()?;
        cancellation.check()?;
        if let Some(resources) = &self.resources {
            resources.collect()?;
        }
        cancellation.check()?;
        self.persistent_baseline = None;
        self.bank_index = Default::default();
        Ok(())
    }

    fn remove_incomplete_bank(
        &mut self,
        owner: &CacheDirectory,
        path: &Path,
        failure: Error,
    ) -> Error {
        match owner.remove(path) {
            Ok(()) => failure,
            Err(cleanup) => {
                self.pending_archive_bytes = self
                    .pending_archive_bytes
                    .saturating_add(std::fs::metadata(path).map_or(0, |metadata| metadata.len()));
                let kind = failure
                    .downcast_ref::<std::io::Error>()
                    .map_or(std::io::ErrorKind::InvalidData, std::io::Error::kind);
                std::io::Error::new(
                    kind,
                    format!("{failure}; incomplete bank is retained pending cleanup: {cleanup}"),
                )
                .into()
            }
        }
    }
}
