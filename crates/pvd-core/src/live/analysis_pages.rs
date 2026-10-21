use super::*;
use crate::analysis::resident::{
    Descriptor, Domain, EmergencyReset, FullAnalysisReader, FullAnalysisWriter, FullLimits,
    LoadedPage, PageIo, PreparedAnalysis, ResidentAnalysis, ResidentBudgetExceeded,
};
use resources::MetadataReservation;
#[cfg(test)]
mod tests;

pub(super) struct HistoryPageIo {
    resources: Resources,
    reader: ResourceReader,
    plan: ResourcePlan,
    transaction: Weak<()>,
}

impl HistoryPageIo {
    fn new(resources: &Resources, plan: ResourcePlan) -> Self {
        Self {
            resources: resources.clone(),
            reader: resources.reader(),
            plan,
            transaction: Weak::new(),
        }
    }
}

impl PageIo for HistoryPageIo {
    type Handle = ResourceHandle;
    type Store = Resources;
    type Reservation = MetadataReservation;

    fn store(&self) -> Resources {
        self.resources.clone()
    }
    fn same(&self, store: &Resources) -> bool {
        self.resources.same(store)
    }
    fn pin(&self, id: u64) -> Result<ResourceHandle, Error> {
        self.resources.pin(id)
    }
    fn id(&self, handle: &ResourceHandle) -> u64 {
        handle.id()
    }
    fn committed(&self, handle: &ResourceHandle) -> bool {
        handle.info().2
    }

    fn begin(&mut self, transaction: &Arc<()>) -> Result<(), Error> {
        if self.transaction.upgrade().is_some() {
            return Err(error("history analysis IO has an unpublished transaction"));
        }
        // The frame owns the plan; analysis adds dependencies to the same scene transaction.
        self.transaction = Arc::downgrade(transaction);
        Ok(())
    }

    fn plan(
        &mut self,
        bytes: &[u8],
        dependencies: &[ResourceHandle],
    ) -> Result<ResourceHandle, Error> {
        self.resources.plan(bytes, dependencies, &mut self.plan)
    }
    #[cfg(test)]
    fn commit(&mut self) -> Result<(), Error> {
        self.resources.commit(&self.plan)
    }
    #[cfg(test)]
    fn rollback(&mut self) {
        self.plan = ResourcePlan::default();
        self.transaction = Weak::new();
    }
    fn invalidate_reader(&mut self) {
        self.reader = self.resources.reader();
    }
    fn info(&self, id: u64) -> Result<(usize, usize), Error> {
        let (bytes, dependencies, _) = self.resources.pin(id)?.info();
        Ok((bytes, dependencies))
    }
    fn read(
        &self,
        id: u64,
        maximum: usize,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<LoadedPage, Error> {
        let handle = self.resources.pin(id)?;
        let (bytes, dependencies, ready) = handle.info();
        if !ready || bytes > maximum || dependencies > 512 {
            return Err(error(
                "history analysis resource exceeds its read limits or is uncommitted",
            ));
        }
        Ok(LoadedPage {
            bytes: self.reader.read(id, cancelled)?,
            dependencies: handle.dependency_ids(),
            #[cfg(test)]
            _release_probe: None,
        })
    }
    fn reserve_metadata(&self, bytes: usize) -> Result<MetadataReservation, Error> {
        self.resources.reserve_metadata(bytes)
    }
}

pub(super) struct PreparedPage {
    prepared: PreparedAnalysis<HistoryPageIo>,
    io: HistoryPageIo,
}
impl PreparedPage {
    pub(super) fn handle(&self) -> ResourceHandle {
        self.prepared.handle()
    }
    pub(super) fn id(&self) -> u64 {
        self.prepared.descriptor(&self.io).root
    }
}

pub(super) struct HistoryPages {
    resources: Resources,
    current: ResidentAnalysis,
    emergency_reset: Option<EmergencyReset>,
    frame: Option<usize>,
    writer: FullAnalysisWriter<HistoryPageIo>,
    reader: AnalysisPageReader,
}

impl HistoryPages {
    pub(super) fn new(resources: &Resources) -> Result<Self, Error> {
        let domain = Domain::new(192 << 20);
        let limits = FullLimits::default();
        let reader = FullAnalysisReader::new(
            HistoryPageIo::new(resources, ResourcePlan::default()),
            domain.clone(),
            limits.clone(),
        )?;
        let io = HistoryPageIo::new(resources, ResourcePlan::default());
        let writer = FullAnalysisWriter::with_reader(&io, domain.clone(), limits, &reader)?;
        Ok(Self {
            resources: resources.clone(),
            current: ResidentAnalysis::new(domain.clone())?,
            emergency_reset: Some(EmergencyReset::new(domain)?),
            frame: None,
            writer,
            reader: AnalysisPageReader(reader),
        })
    }
    pub(super) fn reader(&self) -> AnalysisPageReader {
        self.reader.clone()
    }
    pub(super) fn current(&self) -> &ResidentAnalysis {
        &self.current
    }

    pub(super) fn failure_analysis(&mut self, frame: usize) -> Result<SharedAnalysis, Error> {
        self.emergency_reset
            .take()
            .ok_or_else(|| error("live analysis reset has already been consumed"))?
            .at(frame)
            .map(Into::into)
    }

    pub(super) fn reclaim(&mut self) -> Result<(), Error> {
        self.writer.clear();
        self.reader.reclaim()
    }

    pub(super) fn bytes(&self) -> usize {
        self.current.domain().statistics().logical_used
    }

    pub(super) fn adopt(
        &mut self,
        frame: usize,
        analysis: &ResidentAnalysis,
        inputs: &[AnalysisInput],
    ) -> Result<(), Error> {
        if !self.current.domain().same(analysis.domain()) {
            return Err(error(
                "live analysis and history use different memory domains",
            ));
        }
        self.check_next(frame, inputs)?;
        self.current = analysis.clone();
        self.frame = Some(frame);
        Ok(())
    }

    fn check_next(&self, frame: usize, inputs: &[AnalysisInput]) -> Result<(), Error> {
        if self.frame.map_or(Some(0), |frame| frame.checked_add(1)) != Some(frame)
            && !matches!(inputs.first(), Some(AnalysisInput::Reset(_)))
        {
            return Err(error("history analysis inputs are discontinuous"));
        }
        Ok(())
    }

    pub(super) fn advance(&mut self, frame: usize, inputs: &[AnalysisInput]) -> Result<(), Error> {
        self.check_next(frame, inputs)?;
        let mut cancelled = || false;
        let stage = |current: &ResidentAnalysis, cancelled: &mut dyn FnMut() -> bool| {
            let mut draft = current.clone();
            for input in inputs {
                apply(input, &mut draft, cancelled)?;
            }
            check_replay_cancelled(&mut || cancelled())?;
            Ok::<_, Error>(draft)
        };
        let next = match stage(&self.current, &mut cancelled) {
            Ok(next) => next,
            Err(failure) if failure.downcast_ref::<ResidentBudgetExceeded>().is_some() => {
                self.writer.clear();
                self.reader.0.invalidate()?;
                stage(&self.current, &mut cancelled)?
            }
            Err(failure) => return Err(failure),
        };
        self.current = next;
        self.frame = Some(frame);
        Ok(())
    }

    pub(super) fn prepare(&mut self, plan: &mut ResourcePlan) -> Result<PreparedPage, Error> {
        let mut io = HistoryPageIo::new(&self.resources, std::mem::take(plan));
        let prepared = self.writer.prepare(&self.current, &mut io, &mut || false);
        *plan = std::mem::take(&mut io.plan);
        Ok(PreparedPage {
            prepared: prepared?,
            io,
        })
    }

    pub(super) fn publish(&mut self, page: PreparedPage) -> Result<(), Error> {
        self.writer.publish(page.prepared, &page.io)?;
        #[cfg(test)]
        if std::env::var_os("PVD_HISTORY_PAGE_DIAGNOSTICS").is_some() {
            eprintln!(
                "analysis_page_publish frame={:?} physical={} stats={:?}",
                self.frame,
                self.resources.physical_bytes(),
                self.writer.statistics
            );
        }
        Ok(())
    }
}

pub(super) struct AnalysisPageReader(FullAnalysisReader<HistoryPageIo>);
impl Clone for AnalysisPageReader {
    fn clone(&self) -> Self {
        Self(self.0.fork())
    }
}
impl std::fmt::Debug for AnalysisPageReader {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("AnalysisPageReader")
            .finish_non_exhaustive()
    }
}
impl AnalysisPageReader {
    pub(super) fn for_capture(resources: &Resources) -> Result<Self, Error> {
        Ok(Self(FullAnalysisReader::new(
            HistoryPageIo::new(resources, ResourcePlan::default()),
            Domain::new(192 << 20),
            FullLimits::default(),
        )?))
    }
    pub(super) fn reclaim(&mut self) -> Result<(), Error> {
        self.reclaim_with_cancel(&mut || false)
    }
    #[cfg(test)]
    pub(super) fn with_registry_locked_for_test<R>(
        &self,
        operation: impl FnOnce() -> R,
    ) -> Result<R, Error> {
        self.0.with_registry_locked_for_test(operation)
    }

    pub(super) fn reclaim_with_cancel(
        &mut self,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), Error> {
        self.0.reclaim(cancelled)
    }

    pub(super) fn read(
        &mut self,
        id: u64,
        resources: &Resources,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<ResidentAnalysis, Error> {
        self.0
            .read(&Descriptor::new(id, resources.clone()), cancelled)
    }
}

pub(super) fn apply(
    input: &AnalysisInput,
    analysis: &mut ResidentAnalysis,
    cancelled: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    match input {
        AnalysisInput::Reset(frame) => analysis.reset_at(*frame, cancelled),
        AnalysisInput::Zone(id, name) => analysis.add_zone(*id, name, cancelled),
        AnalysisInput::Event(id, event, name) => analysis.add_event(*id, *event, name, cancelled),
        AnalysisInput::Buffer {
            id,
            kind,
            bytes,
            little_endian,
            frame,
        } => analysis.ingest(
            *id,
            kind,
            bytes,
            if *little_endian {
                pvd_protocol::Endian::Little
            } else {
                pvd_protocol::Endian::Big
            },
            *frame,
            cancelled,
        ),
        AnalysisInput::Retain {
            first_frame,
            accounted_bytes,
        } => analysis.try_retain_captured(*first_frame, *accounted_bytes, cancelled),
    }
}
