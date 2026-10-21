use super::*;
use std::sync::{Mutex, MutexGuard, TryLockError};

#[cfg(test)]
mod tests;

fn lock_with_cancel<'a, T>(
    state: &'a Mutex<T>,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<MutexGuard<'a, T>, Error> {
    loop {
        check_replay_cancelled(cancelled)?;
        match state.try_lock() {
            Ok(state) => return Ok(state),
            Err(TryLockError::WouldBlock) => {
                std::thread::sleep(std::time::Duration::from_millis(5))
            }
            Err(TryLockError::Poisoned(_)) => {
                return Err(error("analysis registry lock poisoned"));
            }
        }
    }
}

struct ReadState<I: PageIo> {
    maps: ResidentReader<I>,
    registry: FullRegistry<I::Store>,
}
pub(super) struct ReadSession<I: PageIo> {
    store: I::Store,
    domain: Domain,
    state: Arc<Mutex<ReadState<I>>>,
}
impl<I: PageIo> Clone for ReadSession<I> {
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            domain: self.domain.clone(),
            state: self.state.clone(),
        }
    }
}
impl<I: PageIo> ReadSession<I> {
    pub(super) fn matches(&self, io: &I, domain: &Domain) -> bool {
        io.same(&self.store) && domain.same(&self.domain)
    }
    pub(super) fn register(
        &self,
        source: Option<&Owned<Continuation>>,
        root: u64,
        memos: &FullMemos<I::Store>,
        map_memos: &ResidentMemos,
        io: &I,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), Error> {
        check_replay_cancelled(cancelled)?;
        if !io.same(&self.store) {
            return Err(error("analysis registration belongs to another source"));
        }
        let mut state = match self.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::WouldBlock) => return Ok(()),
            Err(TryLockError::Poisoned(_)) => return Err(error("analysis registry lock poisoned")),
        };
        let ReadState { maps, registry } = &mut *state;
        if let Some(source) = source {
            if !source.domain().same(&self.domain) {
                return Err(error("analysis registration belongs to another domain"));
            }
            registry.roots.put(
                root,
                source,
                crate::analysis::ANALYSIS_REVISION.fetch_add(1, Ordering::Relaxed),
            );
        }
        memos.register(registry, io, cancelled)?;
        map_memos.trees.visit(|value, reference| {
            check_replay_cancelled(cancelled)?;
            if io
                .pin(reference.page)
                .is_ok_and(|handle| io.committed(&handle))
            {
                maps.trees.put(reference.page, value, reference);
            }
            Ok(())
        })?;
        check_replay_cancelled(cancelled)
    }
}

impl<I: PageIo> crate::analysis::resident::write_maps::ImmutableRegistry for ReadSession<I> {
    fn texts(&self, values: &[Text], cancelled: &mut dyn FnMut() -> bool) -> Result<(), Error> {
        check_replay_cancelled(&mut || cancelled())?;
        let mut state = match self.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::WouldBlock) => return Ok(()),
            Err(TryLockError::Poisoned(_)) => return Err(error("analysis registry lock poisoned")),
        };
        for value in values {
            state.maps.strings.put(&value.0, &mut || cancelled())?;
        }
        Ok(())
    }
    fn allocations(
        &self,
        values: &[ResidentValue],
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        check_replay_cancelled(&mut || cancelled())?;
        let mut state = match self.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::WouldBlock) => return Ok(()),
            Err(TryLockError::Poisoned(_)) => return Err(error("analysis registry lock poisoned")),
        };
        for value in values {
            state.maps.values.put(&value.0, &mut || cancelled())?;
        }
        Ok(())
    }
}

pub(crate) struct FullAnalysisReader<I: PageIo> {
    pub(super) session: ReadSession<I>,
    pub statistics: FullStatistics,
}
impl<I: PageIo> FullAnalysisReader<I> {
    #[cfg(test)]
    pub(crate) fn domain(&self) -> &Domain {
        &self.session.domain
    }
    #[cfg(test)]
    pub(crate) fn with_registry_locked_for_test<R>(
        &self,
        operation: impl FnOnce() -> R,
    ) -> Result<R, Error> {
        let _held = self
            .session
            .state
            .lock()
            .map_err(|_| error("analysis registry lock poisoned"))?;
        Ok(operation())
    }
    pub(crate) fn new(io: I, domain: Domain, limits: FullLimits) -> Result<Self, Error> {
        let store = io.store();
        let mut maps = ResidentReader::dynamic(io, domain.clone(), limits.maps.clone())?;
        let mut cancelled = || false;
        let cx = ReadContext {
            maps: &mut maps,
            domain: &domain,
            cancelled: &mut cancelled,
        };
        let registry = FullRegistry::new(&domain, cx.store(), &limits)?;
        Ok(Self {
            session: ReadSession {
                store,
                domain,
                state: Arc::new(Mutex::new(ReadState { maps, registry })),
            },
            statistics: FullStatistics::default(),
        })
    }
    pub(crate) fn fork(&self) -> Self {
        Self {
            session: self.session.clone(),
            statistics: FullStatistics::default(),
        }
    }
    pub(crate) fn reclaim(&mut self, cancelled: &mut impl FnMut() -> bool) -> Result<(), Error> {
        check_replay_cancelled(cancelled)?;
        let mut state = lock_with_cancel(&self.session.state, cancelled)?;
        state.registry.reclaim(cancelled)?;
        state.maps.reclaim(cancelled)
    }
    pub(crate) fn invalidate(&mut self) -> Result<(), Error> {
        let mut state = self
            .session
            .state
            .lock()
            .map_err(|_| error("analysis registry lock poisoned"))?;
        state.maps.invalidate();
        state.registry.clear();
        Ok(())
    }
    pub(crate) fn read(
        &mut self,
        descriptor: &Descriptor<I::Store>,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<ResidentAnalysis, Error> {
        self.statistics = FullStatistics::default();
        check_replay_cancelled(cancelled)?;
        let mut state = lock_with_cancel(&self.session.state, cancelled)?;
        let ReadState { maps, registry } = &mut *state;
        maps.statistics = Statistics::default();
        maps.raw.statistics = Statistics::default();
        registry.reset_statistics();
        #[cfg(test)]
        {
            maps.reset_small_statistics();
            maps.trace = std::env::var_os("PVD_SHARED_READ_DIAGNOSTICS").map(|_| {
                crate::analysis::resident::read_maps::MapReadTrace {
                    initial: maps.trees.live_keys().collect(),
                    requested: BTreeSet::new(),
                    overflow: false,
                }
            });
        }
        if !maps.raw.io.same(&self.session.store) || !maps.raw.io.same(&descriptor.store) {
            return Err(error("analysis reader belongs to another store"));
        }
        let handle = maps.raw.io.pin(descriptor.root)?;
        if !maps.raw.io.committed(&handle) {
            return Err(error("analysis root is not committed"));
        }
        let analysis = if let Some((data, revision)) = registry.roots.get(descriptor.root) {
            if !data.domain().same(&self.session.domain) {
                return Err(error("analysis cached root belongs to another domain"));
            }
            self.statistics.cached_roots = 1;
            Ok(ResidentAnalysis {
                domain: self.session.domain.clone(),
                data,
                revision,
            })
        } else {
            let mut cx = ReadContext {
                maps,
                domain: &self.session.domain,
                cancelled,
            };
            root::read(descriptor.root, registry, &mut cx)
        };
        self.statistics.map_nodes = maps.statistics.visited_nodes;
        self.statistics.map_new_values = maps.statistics.new_values;
        self.statistics.map_new_strings = maps.statistics.new_strings;
        self.statistics.read_pages = maps.raw.statistics.read_pages;
        self.statistics.cache_capacity_bytes =
            maps.cache_capacity_bytes() + registry.capacity_bytes();
        #[cfg(test)]
        if std::env::var_os("PVD_SHARED_READ_DIAGNOSTICS").is_some() {
            maps.report_small_statistics();
            eprintln!(
                "analysis_page_read root={} failed={} nodes={} new_values={} new_strings={} cached_maps={} cached_values={} cached_strings={} read_pages={} directory_bytes={}",
                descriptor.root,
                analysis.is_err(),
                maps.statistics.visited_nodes,
                maps.statistics.new_values,
                maps.statistics.new_strings,
                maps.statistics.cached_tree_pages,
                maps.statistics.cached_values,
                maps.statistics.cached_strings,
                maps.raw.statistics.read_pages,
                self.statistics.cache_capacity_bytes,
            );
            if let Some(trace) = &maps.trace {
                eprintln!(
                    "analysis_map_page_keys root={} initial_live={} requested={} initial_overlap={} complete={}",
                    descriptor.root,
                    trace.initial.len(),
                    trace.requested.len(),
                    trace.initial.intersection(&trace.requested).count(),
                    !trace.overflow,
                );
            }
        }
        let analysis = analysis?;
        check_replay_cancelled(cancelled)?;
        Ok(analysis)
    }
}
