use crate::{background_jobs::BackgroundJobs, i18n::Message, profile_cache};
use pvd_core::{
    profile_catalog_index::{
        self, CatalogIndex, FirstStartCursor, FirstStartPageReport, PageLimits,
    },
    profile_curves::SourceIdentity,
    profile_disk_index::{self, CpuSpanIndex, IndexLimits, IndexOptions, Progress},
    profile_timeline::{WindowReport, WindowRequest},
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[cfg(all(test, windows))]
mod tests;

struct OwnedIndex {
    // The root lock must outlive removal of the core index's registered files.
    index: Option<CpuSpanIndex>,
    catalog: Option<CatalogIndex>,
    _lease: Arc<profile_cache::Lease>,
}

#[derive(Clone, Copy, Default)]
struct Capabilities {
    cpu: bool,
    catalog: bool,
}

impl Capabilities {
    fn include(&mut self, requested: Self) {
        self.cpu |= requested.cpu;
        self.catalog |= requested.catalog;
    }
}

impl OwnedIndex {
    fn supports(&self, requested: Capabilities) -> bool {
        (!requested.cpu || self.index.is_some()) && (!requested.catalog || self.catalog.is_some())
    }
}

#[derive(Default)]
struct Retirement {
    requested: AtomicBool,
    mutex: Mutex<()>,
    changed: Condvar,
}

impl Retirement {
    fn request(&self) {
        self.requested.store(true, Ordering::Release);
        self.changed.notify_all();
    }

    fn wait(&self, owner: &Arc<OwnedIndex>) {
        let mut guard = self.mutex.lock().unwrap_or_else(|error| error.into_inner());
        while !self.requested.load(Ordering::Acquire) || Arc::strong_count(owner) > 1 {
            guard = self
                .changed
                .wait_timeout(guard, Duration::from_millis(25))
                .unwrap_or_else(|error| error.into_inner())
                .0;
        }
    }
}

struct ManagedIndex {
    owner: Arc<OwnedIndex>,
    retirement: Arc<Retirement>,
}

impl Drop for ManagedIndex {
    fn drop(&mut self) {
        self.retirement.request();
    }
}

struct Building {
    generation: u64,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
struct State {
    generation: u64,
    binding: Option<(PathBuf, SourceIdentity)>,
    building: Option<Building>,
    ready: Option<Arc<ManagedIndex>>,
    capabilities: Capabilities,
}

pub(crate) struct IndexSlot {
    root: PathBuf,
    limits: IndexLimits,
    state: Mutex<State>,
    changed: Condvar,
    closed: AtomicBool,
    jobs: BackgroundJobs,
    #[cfg(test)]
    builds: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    cleanup_gate: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl IndexSlot {
    pub(crate) fn tracked(jobs: &BackgroundJobs) -> Self {
        Self::new(
            PathBuf::from("pvd-cache/profile"),
            IndexLimits::default(),
            jobs.clone(),
        )
    }

    #[cfg(all(test, windows))]
    pub(crate) fn for_tests(root: PathBuf, jobs: &BackgroundJobs) -> Self {
        Self::new(root, IndexLimits::default(), jobs.clone())
    }

    #[cfg(all(test, windows))]
    pub(crate) fn with_test_limits(
        root: PathBuf,
        limits: IndexLimits,
        jobs: &BackgroundJobs,
    ) -> Self {
        Self::new(root, limits, jobs.clone())
    }

    fn new(root: PathBuf, limits: IndexLimits, jobs: BackgroundJobs) -> Self {
        let root = std::env::current_dir().map_or_else(|_| root.clone(), |cwd| cwd.join(&root));
        Self {
            root,
            limits,
            state: Mutex::default(),
            changed: Condvar::new(),
            closed: AtomicBool::new(false),
            jobs,
            #[cfg(test)]
            builds: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            cleanup_gate: Mutex::default(),
        }
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.retire();
    }

    pub fn rebuild(&self) {
        self.retire();
    }

    fn retire(&self) {
        let ready = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(building) = state.building.take() {
                building.cancel.store(true, Ordering::Release);
            }
            state.ready.take()
        };
        if let Some(ready) = &ready {
            ready.retirement.request();
        }
        self.changed.notify_all();
        drop(ready);
    }

    pub(super) fn check(&self, cancelled: &AtomicBool) -> Result<(), Message> {
        if cancelled.load(Ordering::Relaxed) || self.closed.load(Ordering::Acquire) {
            Err(cancelled_message())
        } else {
            Ok(())
        }
    }

    fn get(
        &self,
        path: &Path,
        identity: &SourceIdentity,
        requested: Capabilities,
        cancel: &AtomicBool,
        progress: &mut impl FnMut(Progress) -> bool,
    ) -> Result<Arc<ManagedIndex>, Message> {
        let (generation, build_cancel, capabilities) = loop {
            self.check(cancel)?;
            let mut state = self.state.lock().map_err(|_| lock_error())?;
            self.check(cancel)?;
            if let Some((bound_path, bound_identity)) = &state.binding {
                if bound_path != path || bound_identity != identity {
                    return Err(Message::new(
                        "The profile index belongs to another source; reopen the recording.",
                        "性能索引属于其他来源，请重新打开录制。",
                    ));
                }
            } else {
                state.binding = Some((path.to_owned(), identity.clone()));
            }
            if let Some(index) = &state.ready {
                if index.owner.supports(requested) {
                    return Ok(index.clone());
                }
                let index = state.ready.take().unwrap();
                index.retirement.request();
                drop(state);
                drop(index);
                continue;
            }
            if state.building.is_some() {
                drop(
                    self.changed
                        .wait_timeout(state, Duration::from_millis(25))
                        .map_err(|_| lock_error())?,
                );
                continue;
            }
            state.generation = state.generation.checked_add(1).ok_or_else(lock_error)?;
            let generation = state.generation;
            let cancel = Arc::new(AtomicBool::new(false));
            state.building = Some(Building {
                generation,
                cancel: cancel.clone(),
            });
            let mut capabilities = state.capabilities;
            capabilities.include(requested);
            break (generation, cancel, capabilities);
        };
        let ticket = BuildTicket {
            slot: self,
            generation,
        };
        let stopped = || {
            cancel.load(Ordering::Relaxed)
                || build_cancel.load(Ordering::Acquire)
                || self.closed.load(Ordering::Acquire)
        };
        let lease = profile_cache::Lease::acquire(&self.root, stopped).map_err(index_error)?;
        if stopped() {
            return Err(cancelled_message());
        }
        let limits = IndexLimits {
            disk_bytes: self.limits.disk_bytes.min(lease.disk_budget()),
            ..self.limits
        };
        #[cfg(test)]
        self.builds.fetch_add(1, Ordering::Relaxed);
        let index = if capabilities.cpu {
            let space = lease.clone();
            Some(
                profile_disk_index::build_index(
                    path,
                    lease.workspace(),
                    identity,
                    IndexOptions {
                        limits,
                        before_write: Some(Arc::new(move |bytes| space.check_write(bytes))),
                    },
                    &build_cancel,
                    |status| !stopped() && progress(status),
                )
                .map_err(index_error)?,
            )
        } else {
            None
        };
        let catalog = if capabilities.catalog {
            // The completed CPU index is immutable, so its final size can reserve a fixed share.
            let disk_bytes = limits
                .disk_bytes
                .checked_sub(index.as_ref().map_or(0, |index| index.info().disk_bytes))
                .ok_or_else(|| index_error("combined profile index exceeds its disk budget"))?;
            let space = lease.clone();
            Some(
                profile_catalog_index::build_index(
                    path,
                    &lease.catalog_workspace(),
                    identity,
                    IndexOptions {
                        limits: IndexLimits {
                            disk_bytes,
                            ..limits
                        },
                        before_write: Some(Arc::new(move |bytes| space.check_write(bytes))),
                    },
                    &build_cancel,
                    |status| !stopped() && progress(status),
                )
                .map_err(index_error)?,
            )
        } else {
            None
        };
        if stopped() {
            return Err(cancelled_message());
        }
        let owner = Arc::new(OwnedIndex {
            index,
            catalog,
            _lease: lease,
        });
        let retirement = Arc::new(Retirement::default());
        let index = Arc::new(ManagedIndex {
            owner: owner.clone(),
            retirement: retirement.clone(),
        });
        let stop = retirement.clone();
        #[cfg(test)]
        let cleanup_gate = self.cleanup_gate.lock().map_err(|_| lock_error())?.take();
        drop(
            self.jobs
                .spawn_with_cancel(
                    "cpu-index-cleanup",
                    move || stop.request(),
                    move || {
                        retirement.wait(&owner);
                        #[cfg(test)]
                        if let Some(gate) = cleanup_gate {
                            gate();
                        }
                        drop(owner);
                    },
                )
                .map_err(index_error)?,
        );
        let mut state = self.state.lock().map_err(|_| lock_error())?;
        if stopped()
            || state
                .building
                .as_ref()
                .is_none_or(|building| building.generation != generation)
        {
            return Err(cancelled_message());
        }
        state.ready = Some(index.clone());
        state.capabilities = capabilities;
        state.building = None;
        drop(state);
        drop(ticket);
        Ok(index)
    }

    pub fn window(
        &self,
        path: &Path,
        identity: &SourceIdentity,
        request: WindowRequest,
        cancel: &AtomicBool,
        mut progress: impl FnMut(Progress) -> bool,
    ) -> Result<WindowReport, Message> {
        let index = self.get(
            path,
            identity,
            Capabilities {
                cpu: true,
                catalog: false,
            },
            cancel,
            &mut progress,
        )?;
        self.check(cancel)?;
        let report = index
            .owner
            .index
            .as_ref()
            .ok_or_else(lock_error)?
            .window(request, cancel, |status| {
                !self.closed.load(Ordering::Acquire) && progress(status)
            })
            .map_err(index_error)?;
        self.check(cancel)?;
        Ok(report)
    }

    pub fn catalog_page(
        &self,
        path: &Path,
        identity: &SourceIdentity,
        cursor: Option<&FirstStartCursor>,
        limit: usize,
        cancel: &AtomicBool,
        mut progress: impl FnMut(Progress) -> bool,
    ) -> Result<FirstStartPageReport, Message> {
        let index = self.get(
            path,
            identity,
            Capabilities {
                cpu: false,
                catalog: true,
            },
            cancel,
            &mut progress,
        )?;
        self.check(cancel)?;
        let report = index
            .owner
            .catalog
            .as_ref()
            .ok_or_else(lock_error)?
            .page(
                cursor,
                PageLimits {
                    entries: limit,
                    ..Default::default()
                },
                cancel,
                |status| !self.closed.load(Ordering::Acquire) && progress(status),
            )
            .map_err(index_error)?;
        self.check(cancel)?;
        Ok(report)
    }
}

struct BuildTicket<'a> {
    slot: &'a IndexSlot,
    generation: u64,
}

impl Drop for BuildTicket<'_> {
    fn drop(&mut self) {
        let mut state = self
            .slot
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state
            .building
            .as_ref()
            .is_some_and(|building| building.generation == self.generation)
        {
            state.building = None;
        }
        drop(state);
        self.slot.changed.notify_all();
    }
}

fn cancelled_message() -> Message {
    Message::new(
        "Profile indexing cancelled or its source was closed",
        "性能索引已取消或来源已关闭",
    )
}
fn index_error(error: impl std::fmt::Display) -> Message {
    Message::detail("Profile index failed", "性能索引失败", error)
}
fn lock_error() -> Message {
    Message::new("Profile index state is unavailable", "性能索引状态不可用")
}
