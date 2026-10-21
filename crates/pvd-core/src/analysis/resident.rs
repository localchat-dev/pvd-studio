use crate::{Error, analysis::Allocation, check_replay_cancelled, error};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    mem::size_of,
    ops::Deref,
    sync::{
        Arc, Weak,
        atomic::{AtomicUsize, Ordering},
    },
};

mod allocations;
mod continuation;
mod identity;
mod identity_table;
mod owner;
mod raw;
mod read_maps;
mod wire;
mod write_maps;

pub(crate) use allocations::ResidentMap;
pub(crate) use continuation::pages::{
    FullAnalysisReader, FullAnalysisWriter, FullLimits, PreparedAnalysis,
};
#[cfg(test)]
pub(crate) use continuation::pages::{full_fixture_bytes, full_fixture_inputs};
pub(crate) use continuation::{EmergencyReset, ResidentAnalysis, ResidentMemoryCursor};
use owner::{AllocationData, Cache, Owned, Text};
pub(crate) use owner::{Domain, ResidentValue};
use raw::{DataRows, PageReader, Traversal};
use read_maps::ResidentReader;
use wire::*;
type Key = (u64, u64);

type ResidentLink = Option<Owned<ResidentNode>>;

#[derive(Clone)]
struct ResidentNode {
    key: Key,
    value: ResidentValue,
    left: ResidentLink,
    right: ResidentLink,
    subtree_len: u32,
    height: u8,
}

#[derive(Clone, Debug)]
pub(crate) struct ResidentLimits {
    pub tree_slots: usize,
    pub value_slots: usize,
    pub string_slots: usize,
    pub page_cache_bytes: usize,
}

impl Default for ResidentLimits {
    fn default() -> Self {
        Self {
            tree_slots: 4096,
            value_slots: 1 << 18,
            string_slots: 1 << 16,
            page_cache_bytes: 1 << 20,
        }
    }
}
const HEIGHTS_PER_PAGE: u8 = 7;
const MAX_MAP_ROWS: usize = (1 << HEIGHTS_PER_PAGE) - 1;
const MAX_PAGE_BYTES: usize = 64 << 10;
const MAX_STRING_BYTES: usize = (64 << 20) - 14;
const MAX_VALUE_ROWS: usize = 1024;
const MAX_STRING_ROWS: usize = 4096;
const STRING_PAGE_TARGET: usize = 16 << 10;
const TREE_ARC_BYTES: usize = size_of::<ResidentNode>() + 2 * size_of::<usize>();
const VALUE_ARC_BYTES: usize = size_of::<Allocation>() + 2 * size_of::<usize>();
const STR_ARC_BYTES: usize = 2 * size_of::<usize>();
const TREE_MEMO_ALLOWANCE: usize = 256;
const BUILD_SCRATCH_BYTES: usize = 4 << 20;
const BUILD_PIN_BYTES: usize = 1 << 20;
const BUILD_STRING_BYTES: usize = 2 << 20;
const BUILD_TREE_BYTES: usize = 1 << 20;
pub(crate) const MAX_WRITER_STRING_BYTES: usize = BUILD_STRING_BYTES - MAX_PAGE_BYTES;
const MAX_AVL_HEIGHT: u8 = 96;

pub(crate) struct LoadedPage {
    pub bytes: Vec<u8>,
    pub dependencies: Vec<u64>,
    #[cfg(test)]
    pub _release_probe: Option<ReadReleaseProbe>,
}

#[cfg(test)]
pub(crate) struct ReadReleaseProbe(pub Option<Box<dyn FnOnce() + Send>>);
#[cfg(test)]
impl Drop for ReadReleaseProbe {
    fn drop(&mut self) {
        if let Some(probe) = self.0.take() {
            probe();
        }
    }
}

pub(crate) trait PageIo {
    type Handle: Clone;
    type Store: Clone;
    type Reservation;
    fn store(&self) -> Self::Store;
    fn same(&self, store: &Self::Store) -> bool;
    fn pin(&self, id: u64) -> Result<Self::Handle, Error>;
    fn id(&self, handle: &Self::Handle) -> u64;
    fn committed(&self, handle: &Self::Handle) -> bool;
    fn begin(&mut self, transaction: &Arc<()>) -> Result<(), Error>;
    fn plan(&mut self, bytes: &[u8], dependencies: &[Self::Handle]) -> Result<Self::Handle, Error>;
    #[cfg(test)]
    fn commit(&mut self) -> Result<(), Error>;
    #[cfg(test)]
    fn rollback(&mut self);
    fn invalidate_reader(&mut self);
    fn info(&self, id: u64) -> Result<(usize, usize), Error>;
    fn read(
        &self,
        id: u64,
        maximum: usize,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<LoadedPage, Error>;
    fn reserve_metadata(&self, bytes: usize) -> Result<Self::Reservation, Error>;
}

#[derive(Clone, Debug)]
pub(crate) struct Limits {
    pub page_cache_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            page_cache_bytes: 1 << 20,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct Statistics {
    pub visited_nodes: usize,
    pub new_tree_pages: usize,
    pub reused_tree_pages: usize,
    pub new_values: usize,
    pub new_strings: usize,
    pub logical_bytes: u64,
    pub read_pages: usize,
    pub cached_tree_pages: usize,
    pub cached_values: usize,
    pub cached_strings: usize,
    pub memo_bytes: usize,
    pub memory_peak: usize,
    pub writer_scratch_reserved_bytes: usize,
    pub writer_tree_scratch_bound_bytes: usize,
    pub writer_pin_bytes_peak: usize,
    pub writer_pin_capacity_misses: usize,
    pub writer_string_buffer_bytes_peak: usize,
}

pub(crate) struct Descriptor<S> {
    pub root: u64,
    store: S,
}

impl<S: Clone> Clone for Descriptor<S> {
    fn clone(&self) -> Self {
        Self {
            root: self.root,
            store: self.store.clone(),
        }
    }
}

impl<S> Descriptor<S> {
    pub(crate) fn new(root: u64, store: S) -> Self {
        Self { root, store }
    }
}

pub(crate) struct Snapshot<I: PageIo> {
    root: I::Handle,
    store: I::Store,
}

impl<I: PageIo> Snapshot<I> {
    pub(crate) fn handle(&self) -> I::Handle {
        self.root.clone()
    }
    pub(crate) fn descriptor(&self, io: &I) -> Descriptor<I::Store> {
        Descriptor {
            root: io.id(&self.root),
            store: self.store.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct RowRef {
    page: u64,
    row: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MapRef {
    page: u64,
    len: u64,
    height: u8,
    minimum: Key,
    maximum: Key,
    charge: u64,
}

impl MapRef {
    fn empty() -> Self {
        Self {
            page: 0,
            len: 0,
            height: 0,
            minimum: (0, 0),
            maximum: (0, 0),
            charge: 0,
        }
    }
}

fn add(a: usize, b: usize) -> Result<usize, Error> {
    a.checked_add(b)
        .ok_or_else(|| error("allocation page size overflow"))
}

fn string_charge(value: &str) -> Result<usize, Error> {
    add(value.len(), STR_ARC_BYTES)
}

fn dependency_ids(ids: impl IntoIterator<Item = u64>) -> Vec<u64> {
    ids.into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
fn dependencies_match(expected: &[u64], actual: &[u64]) -> Result<(), Error> {
    if expected != actual {
        return Err(error("allocation page resource dependencies disagree"));
    }
    Ok(())
}

struct MemoryBudget {
    limit: usize,
    identities: identity::Identities,
    used: AtomicUsize,
    peak: AtomicUsize,
    resident_allocations: AtomicUsize,
    resident_fixed_allocated: AtomicUsize,
    resident_payload_allocated: AtomicUsize,
    resident_alive: AtomicUsize,
    resident_strong_bytes: AtomicUsize,
    resident_weak_bytes: AtomicUsize,
    buffer_allocated_nodes: AtomicUsize,
    buffer_copied_elements: AtomicUsize,
    buffer_copied_layout_bytes: AtomicUsize,
    buffer_moved_elements: AtomicUsize,
    buffer_moved_layout_bytes: AtomicUsize,
    tree_copied_entries: AtomicUsize,
    tree_copied_layout_bytes: AtomicUsize,
}
struct MemoryLease {
    budget: Arc<MemoryBudget>,
    bytes: usize,
}

#[derive(Debug)]
pub(crate) struct ResidentBudgetExceeded {
    pub used: usize,
    pub requested: usize,
    pub limit: usize,
    #[cfg(test)]
    pub strong_layout_bytes: usize,
    #[cfg(test)]
    pub weak_fixed_bytes: usize,
    #[cfg(test)]
    pub strong_cells: usize,
}
impl std::fmt::Display for ResidentBudgetExceeded {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            output,
            "allocation page reader exceeds its memory budget (used {}, requested {}, limit {})",
            self.used, self.requested, self.limit
        )?;
        #[cfg(test)]
        write!(
            output,
            " [strong-layout {}, weak-fixed {}, strong-cells {}]",
            self.strong_layout_bytes, self.weak_fixed_bytes, self.strong_cells
        )?;
        Ok(())
    }
}
impl std::error::Error for ResidentBudgetExceeded {}

impl MemoryBudget {
    fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit,
            identities: identity::Identities::new(),
            used: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            resident_allocations: AtomicUsize::new(0),
            resident_fixed_allocated: AtomicUsize::new(0),
            resident_payload_allocated: AtomicUsize::new(0),
            resident_alive: AtomicUsize::new(0),
            resident_strong_bytes: AtomicUsize::new(0),
            resident_weak_bytes: AtomicUsize::new(0),
            buffer_allocated_nodes: AtomicUsize::new(0),
            buffer_copied_elements: AtomicUsize::new(0),
            buffer_copied_layout_bytes: AtomicUsize::new(0),
            buffer_moved_elements: AtomicUsize::new(0),
            buffer_moved_layout_bytes: AtomicUsize::new(0),
            tree_copied_entries: AtomicUsize::new(0),
            tree_copied_layout_bytes: AtomicUsize::new(0),
        })
    }
    fn reserve(self: &Arc<Self>, bytes: usize) -> Result<MemoryLease, Error> {
        let used = self
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|used| *used <= self.limit)
            })
            .map_err(|used| -> Error {
                Box::new(ResidentBudgetExceeded {
                    used,
                    requested: bytes,
                    limit: self.limit,
                    #[cfg(test)]
                    strong_layout_bytes: self.resident_strong_bytes.load(Ordering::Acquire),
                    #[cfg(test)]
                    weak_fixed_bytes: self.resident_weak_bytes.load(Ordering::Acquire),
                    #[cfg(test)]
                    strong_cells: self.resident_alive.load(Ordering::Acquire),
                })
            })?;
        self.peak.fetch_max(used + bytes, Ordering::Relaxed);
        Ok(MemoryLease {
            budget: self.clone(),
            bytes,
        })
    }
}
impl Drop for MemoryLease {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

impl MemoryLease {
    fn shrink(&mut self, bytes: usize) -> Result<(), Error> {
        if bytes > self.bytes {
            return Err(error("allocation page reservation is too small"));
        }
        self.budget
            .used
            .fetch_sub(self.bytes - bytes, Ordering::AcqRel);
        self.bytes = bytes;
        Ok(())
    }
}
