//! Append-only complete-session banks, independent of the rolling writer cache.
use super::*;

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
    path: PathBuf,
    bytes: u64,
}

#[derive(Debug, Default)]
struct Directory {
    banks: Vec<Bank>,
    bytes: u64,
    // A single open bank bounds its resource directory and analysis caches.
    cached: Option<(usize, Arc<crate::Recording>)>,
}

#[derive(Clone, Debug)]
pub(super) struct ArchiveReader {
    // Snapshots keep the entire temporary session alive, including subsequently sealed banks.
    owner: Arc<CacheDirectory>,
    directory: Arc<Mutex<Directory>>,
    transition: Arc<std::sync::RwLock<()>>,
}

impl ArchiveReader {
    fn new(owner: Arc<CacheDirectory>) -> Self {
        Self {
            owner,
            directory: Arc::new(Mutex::new(Directory::default())),
            transition: Arc::new(std::sync::RwLock::new(())),
        }
    }

    fn bank(&self, frame: usize) -> Option<(usize, Bank)> {
        let directory = self.directory.lock().ok()?;
        let index = directory.banks.partition_point(|bank| bank.last < frame);
        let bank = directory.banks.get(index)?;
        (bank.first <= frame).then(|| (index, bank.clone()))
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
        {
            let directory = self
                .directory
                .lock()
                .map_err(|_| error("complete-session bank lock poisoned"))?;
            if let Some((cached_index, recording)) = &directory.cached
                && *cached_index == index
            {
                return Ok(Some(recording.clone()));
            }
        }
        let limits = capture::CaptureLimits {
            index_bytes: BANK_INDEX_BYTES,
            output_bytes: bank.bytes.max(4096),
            ..Default::default()
        };
        let mut progress = |_| !cancelled();
        let progress: &mut dyn FnMut(capture::CaptureProgress) -> bool = &mut progress;
        let mut recording = capture::open_with_progress(&bank.path, &limits, progress)?;
        // The open capture's file handle alone must not outlive ownership of the directory.
        let disk = recording.disk.as_mut().unwrap();
        disk.segments
            .front()
            .unwrap()
            .resources
            .capture_backing()
            .unwrap()
            .keep_owner(self.owner.clone())?;
        disk._owner = Some(self.owner.clone());
        let recording = Arc::new(recording);
        let mut directory = self
            .directory
            .lock()
            .map_err(|_| error("complete-session bank lock poisoned"))?;
        directory.cached = Some((index, recording.clone()));
        Ok(Some(recording))
    }

    fn publish(&self, bank: Bank) -> Result<(), Error> {
        let mut directory = self
            .directory
            .lock()
            .map_err(|_| error("complete-session bank lock poisoned"))?;
        if directory
            .banks
            .last()
            .is_some_and(|last| last.last.checked_add(1) != Some(bank.first))
        {
            return Err(error("complete-session banks must be contiguous"));
        }
        directory.bytes = directory.bytes.saturating_add(bank.bytes);
        directory.banks.push(bank);
        Ok(())
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

    fn seal_bank(&mut self, source: &crate::Recording) -> Result<(), Error> {
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
            index_bytes: BANK_INDEX_BYTES,
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
        if let Err(failure) = capture::open_with_progress(&path, &limits, |value| {
            progress(SealPhase::Admission, Some(value));
            !cancellation.requested()
        }) {
            return Err(self.remove_incomplete_bank(&owner, &path, failure));
        }
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
        if let Err(failure) = archive.publish(Bank {
            first,
            last,
            path: path.clone(),
            bytes: report.bytes,
        }) {
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
