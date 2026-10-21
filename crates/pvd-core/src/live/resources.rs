use super::{Error, MAGIC, MAX_RECORD, cache_directory::CacheDirectory, error};
use crate::check_replay_cancelled;
#[cfg(test)]
pub(super) mod accounting;
pub(super) mod capture;
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{BufReader, Read, Seek, SeekFrom, Write},
    mem::size_of,
    path::PathBuf,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};

const RESOURCE_ID: u64 = 1 << 63;
const FILE_ID: u64 = 1 << 62;
const FILE_HEADER: u64 = 8;
const BLOCK_HEADER: u64 = 36;
const MAX_BLOCK: usize = 64 << 10;
const MIN_BLOCK: usize = 4 << 10;
const MAX_TRANSACTION_BYTES: u64 = 640 << 20;
const MAX_ENTRIES: usize = 1_048_576;
const MAX_METADATA_BYTES: usize = 512 << 20;
const MAX_CHUNKS: usize = 4096;
const MAX_DEPENDENCY_DEPTH: u8 = 64;
const MAP_ENTRY_BYTES: usize = 128;
const READ_BUFFER_BYTES: usize = 256 << 10;
const WRITE_BUFFER_BYTES: usize = MAX_BLOCK + BLOCK_HEADER as usize;

#[derive(Debug)]
struct AdmissionLimit(&'static str);

impl std::fmt::Display for AdmissionLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for AdmissionLimit {}

pub(super) fn admission_failure(failure: &Error) -> bool {
    failure.is::<AdmissionLimit>() || failure.is::<std::collections::TryReserveError>()
}

#[derive(Clone, Debug)]
pub(super) struct Resources {
    shared: Arc<Shared>,
}

#[derive(Debug)]
struct Shared {
    owner: Option<Arc<CacheDirectory>>,
    capture: Option<Arc<capture::CaptureBacking>>,
    retained: Mutex<Vec<ResourceHandle>>,
    target: u64,
    read_buffer_bytes: usize,
    state: Mutex<Store>,
    blobs: Mutex<BTreeMap<u64, Weak<Blob>>>,
    reader: Mutex<Option<CachedFile>>,
}

#[derive(Debug)]
struct Store {
    next_blob: u64,
    next_block: u64,
    next_file: u64,
    blocks: BTreeMap<u64, Weak<Block>>,
    contents: BTreeMap<([u8; 32], usize), Vec<Weak<Block>>>,
    chunks: BTreeMap<u64, Arc<Chunk>>,
    plan_bytes: Arc<AtomicUsize>,
    metadata: usize,
    read_buffer_bytes: usize,
    active: Option<ActiveFile>,
    write_scratch: Vec<u8>,
    entry_limit: usize,
    metadata_limit: usize,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            next_blob: 1,
            next_block: 1,
            next_file: 1,
            blocks: BTreeMap::new(),
            contents: BTreeMap::new(),
            chunks: BTreeMap::new(),
            plan_bytes: Arc::new(AtomicUsize::new(0)),
            metadata: size_of::<Shared>() + size_of::<Store>() + 64 + READ_BUFFER_BYTES,
            read_buffer_bytes: READ_BUFFER_BYTES,
            active: None,
            write_scratch: Vec::new(),
            entry_limit: MAX_ENTRIES,
            metadata_limit: MAX_METADATA_BYTES,
        }
    }
}

#[derive(Clone)]
pub(super) struct ResourceHandle(Arc<Blob>);

impl std::fmt::Debug for ResourceHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceHandle")
            .field("id", &self.0.id)
            .field("bytes", &self.0.len)
            .field("blocks", &self.0.blocks.len())
            .field("dependencies", &self.0.dependencies.len())
            .finish()
    }
}

impl ResourceHandle {
    pub(super) fn id(&self) -> u64 {
        self.0.id
    }

    pub(super) fn info(&self) -> (usize, usize, bool) {
        (
            self.0.len,
            self.0.dependencies.len(),
            self.0.ready.load(Ordering::Acquire),
        )
    }

    pub(super) fn dependency_ids(&self) -> Vec<u64> {
        self.0.dependencies.iter().map(ResourceHandle::id).collect()
    }
}

pub(super) struct MetadataReservation {
    _accounting: Arc<PlanAccounting>,
}

#[derive(Debug)]
struct Blob {
    id: u64,
    len: usize,
    hash: [u8; 32],
    blocks: Vec<Arc<Block>>,
    dependencies: Vec<ResourceHandle>,
    depth: u8,
    ready: AtomicBool,
    store: Weak<Shared>,
}

#[derive(Debug)]
struct Block {
    id: u64,
    len: usize,
    hash: [u8; 32],
    location: Mutex<BlockLocation>,
}

#[derive(Debug)]
enum BlockLocation {
    Pending(Arc<[u8]>),
    Stored(Placement),
}

#[derive(Clone, Debug)]
struct Placement {
    chunk: Arc<Chunk>,
    offset: u64,
}

#[derive(Debug)]
struct Chunk {
    id: u64,
    path: PathBuf,
    bytes: AtomicU64,
    _owner: Option<Arc<CacheDirectory>>,
}

#[derive(Debug)]
struct ActiveFile {
    chunk: Arc<Chunk>,
    file: File,
}

#[derive(Debug)]
struct CachedFile {
    chunk: Arc<Chunk>,
    file: BufReader<File>,
    position: Option<u64>,
}

#[derive(Debug, Default)]
pub(super) struct ResourcePlan {
    store: Weak<Shared>,
    accounting: Option<Arc<PlanAccounting>>,
    blobs: BTreeMap<u64, Arc<Blob>>,
    blocks: BTreeMap<u64, Arc<Block>>,
    input_bytes: u64,
    pending_bytes: u64,
}

#[derive(Debug)]
struct PlanAccounting {
    total: Arc<AtomicUsize>,
    bytes: AtomicUsize,
}

impl Drop for PlanAccounting {
    fn drop(&mut self) {
        self.total
            .fetch_sub(self.bytes.load(Ordering::Relaxed), Ordering::Relaxed);
    }
}

impl ResourcePlan {
    fn account(&self) {
        if let Some(accounting) = &self.accounting {
            let bytes = (size_of::<Self>() + size_of::<PlanAccounting>() + 4 * size_of::<usize>())
                .saturating_add(self.blobs.len().saturating_mul(MAP_ENTRY_BYTES))
                .saturating_add(self.blocks.len().saturating_mul(MAP_ENTRY_BYTES));
            let previous = accounting.bytes.swap(bytes, Ordering::Relaxed);
            if bytes >= previous {
                accounting
                    .total
                    .fetch_add(bytes - previous, Ordering::Relaxed);
            } else {
                accounting
                    .total
                    .fetch_sub(previous - bytes, Ordering::Relaxed);
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct ResourceReader {
    shared: Arc<Shared>,
}

impl Resources {
    pub(super) fn try_reserve_metadata(
        &self,
        bytes: usize,
    ) -> Result<Option<MetadataReservation>, Error> {
        match self.reserve_metadata(bytes) {
            Ok(reservation) => Ok(Some(reservation)),
            Err(failure) if admission_failure(&failure) => Ok(None),
            Err(failure) => Err(failure),
        }
    }

    pub(super) fn reserve_metadata(&self, bytes: usize) -> Result<MetadataReservation, Error> {
        let mut state = self.shared.state.lock().map_err(|_| lock_error())?;
        check_metadata(&mut state, &self.shared, bytes)?;
        let accounting = Arc::new(PlanAccounting {
            total: state.plan_bytes.clone(),
            bytes: AtomicUsize::new(bytes),
        });
        accounting.total.fetch_add(bytes, Ordering::Relaxed);
        Ok(MetadataReservation {
            _accounting: accounting,
        })
    }
    pub(super) fn new(owner: Arc<CacheDirectory>, budget: u64) -> Self {
        let target = (budget / 32).clamp(256, 8 << 20);
        let read_buffer_bytes = (target as usize).min(READ_BUFFER_BYTES);
        Self {
            shared: Arc::new(Shared {
                owner: Some(owner),
                capture: None,
                retained: Mutex::new(Vec::new()),
                target,
                read_buffer_bytes,
                state: Mutex::new(Store {
                    metadata: size_of::<Shared>() + size_of::<Store>() + 64 + read_buffer_bytes,
                    read_buffer_bytes,
                    entry_limit: (budget / 8192).clamp(262_144, MAX_ENTRIES as u64) as usize,
                    metadata_limit: (budget / 16).clamp(128 << 20, MAX_METADATA_BYTES as u64)
                        as usize,
                    ..Default::default()
                }),
                blobs: Mutex::new(BTreeMap::new()),
                reader: Mutex::new(None),
            }),
        }
    }

    pub(super) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
    }

    pub(super) fn reader(&self) -> ResourceReader {
        if let Ok(mut reader) = self.shared.reader.lock() {
            reader.take();
        }
        ResourceReader {
            shared: self.shared.clone(),
        }
    }

    pub(super) fn pin(&self, id: u64) -> Result<ResourceHandle, Error> {
        self.try_pin(id)?
            .ok_or_else(|| error("history resource is no longer retained"))
    }

    pub(super) fn try_pin(&self, id: u64) -> Result<Option<ResourceHandle>, Error> {
        Ok(self.shared.lookup_blob(id)?.map(ResourceHandle))
    }

    pub(super) fn plan(
        &self,
        bytes: &[u8],
        dependencies: &[ResourceHandle],
        plan: &mut ResourcePlan,
    ) -> Result<ResourceHandle, Error> {
        if self.shared.capture.is_some() {
            return Err(error("capture resources are read-only"));
        }
        if bytes.len() > MAX_RECORD {
            return Err(error("history resource exceeds the record limit"));
        }
        let input_bytes = plan
            .input_bytes
            .checked_add(bytes.len() as u64)
            .filter(|bytes| *bytes <= MAX_TRANSACTION_BYTES)
            .ok_or_else(|| {
                Box::new(AdmissionLimit(
                    "history resource transaction exceeds its byte limit",
                )) as Error
            })?;
        let mut state = self.shared.state.lock().map_err(|_| lock_error())?;
        if plan.accounting.is_some() && !Weak::ptr_eq(&plan.store, &Arc::downgrade(&self.shared)) {
            return Err(error("history resource plan belongs to another session"));
        }
        if dependencies.len() > state.entry_limit {
            return Err(error("history resource has too many dependencies"));
        }
        let mut depth = 0u8;
        let mut distinct_dependencies = BTreeMap::new();
        for dependency in dependencies {
            if !Weak::ptr_eq(&dependency.0.store, &Arc::downgrade(&self.shared)) {
                return Err(error(
                    "history resource dependency belongs to another session",
                ));
            }
            depth = depth.max(dependency.0.depth);
            distinct_dependencies.insert(dependency.id(), dependency.clone());
        }
        let depth = depth
            .checked_add(1)
            .filter(|depth| *depth <= MAX_DEPENDENCY_DEPTH)
            .ok_or_else(|| error("history resource dependency depth exceeds its limit"))?;
        let id = state
            .next_blob
            .checked_add(RESOURCE_ID)
            .ok_or_else(|| error("history resource ID space exhausted"))?;
        state.next_blob = state
            .next_blob
            .checked_add(1)
            .ok_or_else(|| error("history resource ID space exhausted"))?;
        if distinct_dependencies
            .keys()
            .any(|dependency| *dependency >= id)
        {
            return Err(error(
                "history resource dependencies must precede their owner",
            ));
        }
        let maximum = (self.shared.target - FILE_HEADER - BLOCK_HEADER) as usize;
        let maximum = maximum.min(MAX_BLOCK);
        let mut blocks = Vec::new();
        for (start, end) in boundaries(bytes, maximum) {
            let contents = &bytes[start..end];
            let hash = *blake3::hash(contents).as_bytes();
            let key = (hash, contents.len());
            let candidates = state.contents.get(&key).cloned().unwrap_or_default();
            let mut found = None;
            for candidate in candidates.into_iter().filter_map(|block| block.upgrade()) {
                if self.shared.block_bytes(&candidate, &mut || false)? == contents {
                    found = Some(candidate);
                    break;
                }
            }
            let block = if let Some(block) = found {
                block
            } else {
                check_entries(&mut state, &self.shared, 1)?;
                let block_id = state.next_block;
                state.next_block = block_id
                    .checked_add(1)
                    .ok_or_else(|| error("history resource block ID space exhausted"))?;
                let block = Arc::new(Block {
                    id: block_id,
                    len: contents.len(),
                    hash,
                    location: Mutex::new(BlockLocation::Pending(contents.into())),
                });
                state.blocks.insert(block_id, Arc::downgrade(&block));
                let new_key = !state.contents.contains_key(&key);
                let candidates = state.contents.entry(key).or_default();
                let previous_capacity = candidates.capacity();
                candidates.push(Arc::downgrade(&block));
                let capacity = candidates.capacity();
                state.metadata = state.metadata.saturating_add(
                    MAP_ENTRY_BYTES
                        + size_of::<Block>()
                        + 4 * size_of::<usize>()
                        + if new_key { MAP_ENTRY_BYTES } else { 0 }
                        + (capacity - previous_capacity) * size_of::<Weak<Block>>(),
                );
                block
            };
            blocks.push(block);
            check_metadata(&mut state, &self.shared, 0)?;
        }
        check_entries(&mut state, &self.shared, 1)?;
        let blob = Arc::new(Blob {
            id,
            len: bytes.len(),
            hash: *blake3::hash(bytes).as_bytes(),
            blocks,
            dependencies: distinct_dependencies.into_values().collect(),
            depth,
            ready: AtomicBool::new(false),
            store: Arc::downgrade(&self.shared),
        });
        let mut pending_blobs = BTreeMap::new();
        let mut pending_blocks = BTreeMap::new();
        let mut visiting = vec![blob.clone()];
        while let Some(current) = visiting.pop() {
            #[cfg(test)]
            tests::record_pending_visit();
            // Ready dependencies have committed; this plan already owns its other dependency closures.
            if current.ready.load(Ordering::Acquire)
                || plan.blobs.contains_key(&current.id)
                || pending_blobs.insert(current.id, current.clone()).is_some()
            {
                continue;
            }
            for block in &current.blocks {
                pending_blocks.insert(block.id, block.clone());
            }
            visiting.extend(
                current
                    .dependencies
                    .iter()
                    .map(|dependency| dependency.0.clone()),
            );
        }
        let mut pending_bytes = plan.pending_bytes;
        for block in pending_blocks
            .values()
            .filter(|block| !plan.blocks.contains_key(&block.id))
        {
            if matches!(
                *block.location.lock().map_err(|_| lock_error())?,
                BlockLocation::Pending(_)
            ) {
                pending_bytes = pending_bytes
                    .checked_add(block.len as u64)
                    .ok_or_else(|| error("history resource transaction size overflow"))?;
            }
        }
        if pending_bytes > MAX_TRANSACTION_BYTES {
            return Err(Box::new(AdmissionLimit(
                "history resource transaction exceeds its byte limit",
            )));
        }
        let blob_metadata = size_of::<Blob>()
            + 2 * size_of::<usize>()
            + MAP_ENTRY_BYTES
            + blob.blocks.capacity() * size_of::<Arc<Block>>()
            + blob.dependencies.capacity() * size_of::<ResourceHandle>();
        let blob_entries = pending_blobs
            .keys()
            .filter(|id| !plan.blobs.contains_key(id))
            .count();
        let block_entries = pending_blocks
            .keys()
            .filter(|id| !plan.blocks.contains_key(id))
            .count();
        let plan_metadata = if plan.accounting.is_none() {
            size_of::<ResourcePlan>() + size_of::<PlanAccounting>() + 4 * size_of::<usize>()
        } else {
            0
        };
        let additional_metadata = blob_metadata
            .saturating_add(plan_metadata)
            .saturating_add(blob_entries.saturating_mul(MAP_ENTRY_BYTES))
            .saturating_add(block_entries.saturating_mul(MAP_ENTRY_BYTES));
        check_metadata(&mut state, &self.shared, additional_metadata)?;
        self.shared
            .blobs
            .lock()
            .map_err(|_| lock_error())?
            .insert(id, Arc::downgrade(&blob));
        state.metadata = state.metadata.saturating_add(blob_metadata);
        if plan.accounting.is_none() {
            let accounting = Arc::new(PlanAccounting {
                total: state.plan_bytes.clone(),
                bytes: AtomicUsize::new(0),
            });
            plan.store = Arc::downgrade(&self.shared);
            plan.accounting = Some(accounting);
        }
        plan.input_bytes = input_bytes;
        plan.pending_bytes = pending_bytes;
        plan.blobs.extend(pending_blobs);
        plan.blocks.extend(pending_blocks);
        plan.account();
        Ok(ResourceHandle(blob))
    }

    pub(super) fn planned_bytes(&self, plan: &ResourcePlan) -> u64 {
        if plan.accounting.is_some() && !Weak::ptr_eq(&plan.store, &Arc::downgrade(&self.shared)) {
            return u64::MAX;
        }
        let mut bytes = 0u64;
        let mut remaining = 0u64;
        for block in plan.blocks.values() {
            let Ok(location) = block.location.lock() else {
                return u64::MAX;
            };
            if !matches!(*location, BlockLocation::Pending(_)) {
                continue;
            }
            let record = BLOCK_HEADER + block.len as u64;
            if record > remaining {
                bytes = bytes.saturating_add(FILE_HEADER);
                remaining = self.shared.target - FILE_HEADER;
            }
            bytes = bytes.saturating_add(record);
            remaining -= record;
        }
        bytes
    }

    pub(super) fn commit(&self, plan: &ResourcePlan) -> Result<(), Error> {
        if self.shared.capture.is_some() {
            return Err(error("project capture resources are read-only"));
        }
        if plan.accounting.is_none() {
            return Ok(());
        }
        if !Weak::ptr_eq(&plan.store, &Arc::downgrade(&self.shared)) {
            return Err(error("history resource plan belongs to another session"));
        }
        let mut state = self.shared.state.lock().map_err(|_| lock_error())?;
        let result = (|| {
            let mut placements = Vec::new();
            for block in plan.blocks.values() {
                let contents = match &*block.location.lock().map_err(|_| lock_error())? {
                    BlockLocation::Pending(contents) => contents.clone(),
                    BlockLocation::Stored(_) => continue,
                };
                let placement = append_block(&self.shared, &mut state, block, &contents)?;
                placements.push((block.clone(), placement));
            }
            if let Some(active) = &mut state.active {
                #[cfg(all(test, feature = "resource-test-write-faults"))]
                raw_write_tests::check_flush_failure()?;
                active.file.flush()?;
            }
            let mut locations = placements
                .iter()
                .map(|(block, _)| block.location.lock().map_err(|_| lock_error()))
                .collect::<Result<Vec<_>, _>>()?;
            // Directory lookups cannot enter during location and ready-flag publication.
            let directory = self.shared.blobs.lock().map_err(|_| lock_error())?;
            for (location, (_, placement)) in locations.iter_mut().zip(&placements) {
                **location = BlockLocation::Stored(placement.clone());
            }
            for blob in plan.blobs.values() {
                blob.ready.store(true, Ordering::Release);
            }
            drop(directory);
            Ok(())
        })();
        if result.is_err() {
            state.active = None;
        }
        result
    }

    pub(super) fn collect(&self) -> Result<(), Error> {
        if self.shared.capture.is_some() {
            return Err(error("capture resources are read-only"));
        }
        let mut state = self.shared.state.lock().map_err(|_| lock_error())?;
        close_active(&mut state)?;
        self.shared.reader.lock().map_err(|_| lock_error())?.take();
        prune(&mut state, &self.shared)?;
        remove_unused(&self.shared, &mut state)
    }

    pub(super) fn compact_one(&self) -> Result<bool, Error> {
        if self.shared.capture.is_some() {
            return Err(error("capture resources are read-only"));
        }
        let mut state = self.shared.state.lock().map_err(|_| lock_error())?;
        close_active(&mut state)?;
        self.shared.reader.lock().map_err(|_| lock_error())?.take();
        prune(&mut state, &self.shared)?;
        remove_unused(&self.shared, &mut state)?;
        let mut live = BTreeMap::<u64, Vec<Arc<Block>>>::new();
        for block in state.blocks.values().filter_map(Weak::upgrade) {
            if let BlockLocation::Stored(placement) =
                &*block.location.lock().map_err(|_| lock_error())?
            {
                live.entry(placement.chunk.id)
                    .or_default()
                    .push(block.clone());
            }
        }
        let candidate = live
            .into_iter()
            .filter_map(|(id, blocks)| {
                let retained = FILE_HEADER
                    + blocks
                        .iter()
                        .map(|block| BLOCK_HEADER + block.len as u64)
                        .sum::<u64>();
                let bytes = state.chunks.get(&id)?.bytes.load(Ordering::Relaxed);
                let reclaimable = bytes.checked_sub(retained)?;
                (reclaimable != 0).then_some((reclaimable, id, blocks))
            })
            .max_by_key(|(reclaimable, id, _)| (*reclaimable, std::cmp::Reverse(*id)));
        let Some((_, _, blocks)) = candidate else {
            return Ok(false);
        };
        prepare_write_scratch(&mut state, &self.shared)?;
        let mut replacement = new_chunk(&self.shared, &mut state)?;
        let mut placements = Vec::new();
        for block in &blocks {
            let contents = self.shared.block_bytes(block, &mut || false)?;
            let placement =
                write_block(&mut replacement, block, &contents, &mut state.write_scratch)?;
            placements.push(placement);
        }
        #[cfg(all(test, feature = "resource-test-write-faults"))]
        raw_write_tests::check_flush_failure()?;
        replacement.file.flush()?;
        drop(replacement);
        let mut locations = blocks
            .iter()
            .map(|block| block.location.lock().map_err(|_| lock_error()))
            .collect::<Result<Vec<_>, _>>()?;
        for (location, placement) in locations.iter_mut().zip(placements) {
            **location = BlockLocation::Stored(placement);
        }
        drop(locations);
        drop(blocks);
        self.shared.reader.lock().map_err(|_| lock_error())?.take();
        remove_unused(&self.shared, &mut state)?;
        Ok(true)
    }

    pub(super) fn physical_bytes(&self) -> u64 {
        let Ok(state) = self.shared.state.lock() else {
            return u64::MAX;
        };
        state.chunks.values().fold(0u64, |bytes, chunk| {
            bytes.saturating_add(chunk.bytes.load(Ordering::Relaxed))
        })
    }

    pub(super) fn metadata_bytes(&self) -> usize {
        self.shared
            .state
            .lock()
            .map_or(usize::MAX, |state| metadata_bytes(&state))
    }

    pub(super) fn metadata_capacity(&self) -> usize {
        self.shared
            .state
            .lock()
            .map_or(0, |state| state.metadata_limit)
    }

    pub(super) fn reserve_bytes(&self) -> u64 {
        self.shared.target
    }

    pub(super) fn needs_reclaim(&self, additional_records: usize) -> bool {
        let Ok(state) = self.shared.state.lock() else {
            return true;
        };
        let Ok(entries) = entry_count(&state, &self.shared) else {
            return true;
        };
        let entry_target = state.entry_limit - state.entry_limit / 4;
        let metadata_target = state.metadata_limit - state.metadata_limit / 4;
        entries.saturating_add(additional_records.saturating_mul(4)) >= entry_target
            || metadata_bytes(&state).saturating_add(additional_records.saturating_mul(1024))
                >= metadata_target
    }
}

impl ResourceReader {
    pub(super) fn read(
        &self,
        id: u64,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Vec<u8>, Error> {
        self.read_limited(id, MAX_RECORD, cancelled)
    }

    pub(super) fn read_dependency(
        &self,
        owner: u64,
        id: u64,
        maximum: usize,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Vec<u8>, Error> {
        check_replay_cancelled(cancelled)?;
        let owner = self
            .shared
            .lookup_blob(owner)?
            .ok_or_else(|| error("history resource owner is no longer retained"))?;
        if !owner.ready.load(Ordering::Acquire)
            || id & RESOURCE_ID == 0
            || id >= owner.id
            || !owner
                .dependencies
                .iter()
                .any(|dependency| dependency.id() == id)
        {
            return Err(error(
                "object key schema is not a committed direct dependency",
            ));
        }
        self.read_limited(id, maximum, cancelled)
    }

    pub(super) fn read_limited(
        &self,
        id: u64,
        maximum: usize,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Vec<u8>, Error> {
        let result = self.read_inner(id, maximum, cancelled);
        if result.is_err()
            && let Ok(mut reader) = self.shared.reader.lock()
            && let Some(reader) = reader.as_mut()
        {
            reader.position = None;
        }
        result
    }

    fn read_inner(
        &self,
        id: u64,
        maximum: usize,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Vec<u8>, Error> {
        check_replay_cancelled(cancelled)?;
        let blob = self
            .shared
            .lookup_blob(id)?
            .ok_or_else(|| error("history resource is no longer retained"))?;
        if !blob.ready.load(Ordering::Acquire) {
            return Err(error("history resource has not been committed"));
        }
        if blob.len > maximum.min(MAX_RECORD) {
            return Err(error("history resource exceeds its read byte limit"));
        }
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(blob.len)?;
        let mut hash = blake3::Hasher::new();
        for block in &blob.blocks {
            check_replay_cancelled(cancelled)?;
            let contents = self.shared.block_bytes(block, cancelled)?;
            hash.update(&contents);
            bytes.extend_from_slice(&contents);
        }
        check_replay_cancelled(cancelled)?;
        if bytes.len() != blob.len || hash.finalize().as_bytes() != &blob.hash {
            return Err(error("history resource checksum mismatch"));
        }
        Ok(bytes)
    }
}

impl Shared {
    fn lookup_blob(&self, id: u64) -> Result<Option<Arc<Blob>>, Error> {
        let blobs = self.blobs.lock().map_err(|_| lock_error())?;
        Ok(blobs.get(&id).and_then(Weak::upgrade))
    }

    fn block_bytes(
        &self,
        block: &Block,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Vec<u8>, Error> {
        check_replay_cancelled(cancelled)?;
        let placement = {
            let location = block.location.lock().map_err(|_| lock_error())?;
            match &*location {
                BlockLocation::Pending(bytes) => return Ok(bytes.to_vec()),
                BlockLocation::Stored(placement) => placement.clone(),
            }
        };
        self.read_placement(block, &placement, cancelled)
    }

    fn read_placement(
        &self,
        block: &Block,
        placement: &Placement,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Vec<u8>, Error> {
        check_replay_cancelled(cancelled)?;
        let end = placement
            .offset
            .checked_add(BLOCK_HEADER)
            .and_then(|end| end.checked_add(block.len as u64))
            .filter(|end| *end <= placement.chunk.bytes.load(Ordering::Relaxed))
            .ok_or_else(|| error("history resource block lies outside its chunk"))?;
        if placement.offset < FILE_HEADER || block.len > MAX_BLOCK || end < placement.offset {
            return Err(error("invalid history resource block location"));
        }
        if let Some(capture) = &self.capture {
            let mut header = [0; BLOCK_HEADER as usize];
            capture.read_exact_at(placement.offset, &mut header)?;
            if u32::from_le_bytes(header[..4].try_into().unwrap()) as usize != block.len
                || header[4..] != block.hash
            {
                return Err(error("capture block header mismatch"));
            }
            check_replay_cancelled(cancelled)?;
            let mut bytes = vec![0; block.len];
            capture.read_exact_at(placement.offset + BLOCK_HEADER, &mut bytes)?;
            check_replay_cancelled(cancelled)?;
            if blake3::hash(&bytes).as_bytes() != &block.hash {
                return Err(error("capture block checksum mismatch"));
            }
            return Ok(bytes);
        }
        let mut cache = self.reader.lock().map_err(|_| lock_error())?;
        if cache
            .as_ref()
            .is_none_or(|cache| !Arc::ptr_eq(&cache.chunk, &placement.chunk))
        {
            cache.take();
            let mut file = BufReader::with_capacity(
                self.read_buffer_bytes,
                File::open(&placement.chunk.path)?,
            );
            let mut magic = [0u8; 8];
            file.read_exact(&mut magic)?;
            if &magic != MAGIC {
                return Err(error("history resource chunk signature mismatch"));
            }
            *cache = Some(CachedFile {
                chunk: placement.chunk.clone(),
                file,
                position: Some(FILE_HEADER),
            });
        }
        let cached = cache.as_mut().unwrap();
        let position = cached.position.take();
        if let Some(relative) = position.and_then(|position| {
            i64::try_from(i128::from(placement.offset) - i128::from(position)).ok()
        }) {
            cached.file.seek_relative(relative)?;
        } else {
            cached.file.seek(SeekFrom::Start(placement.offset))?;
        }
        let mut header = [0; BLOCK_HEADER as usize];
        cached.file.read_exact(&mut header)?;
        if u32::from_le_bytes(header[..4].try_into().unwrap()) as usize != block.len
            || header[4..] != block.hash
        {
            return Err(error("history resource block header mismatch"));
        }
        let mut bytes = vec![0; block.len];
        for part in bytes.chunks_mut(MAX_BLOCK) {
            check_replay_cancelled(cancelled)?;
            cached.file.read_exact(part)?;
        }
        check_replay_cancelled(cancelled)?;
        cached.position = Some(end);
        drop(cache);
        if blake3::hash(&bytes).as_bytes() != &block.hash {
            return Err(error("history resource block checksum mismatch"));
        }
        Ok(bytes)
    }
}

fn entry_count(state: &Store, shared: &Shared) -> Result<usize, Error> {
    let blobs = shared.blobs.lock().map_err(|_| lock_error())?;
    Ok(blobs.len().saturating_add(state.blocks.len()))
}

fn check_entries(state: &mut Store, shared: &Shared, additional: usize) -> Result<(), Error> {
    if entry_count(state, shared)?.saturating_add(additional) > state.entry_limit {
        prune(state, shared)?;
    }
    if entry_count(state, shared)?.saturating_add(additional) > state.entry_limit {
        return Err(Box::new(AdmissionLimit(
            "history resource directory exceeds its entry limit",
        )));
    }
    Ok(())
}

fn check_metadata(state: &mut Store, shared: &Shared, additional: usize) -> Result<(), Error> {
    if metadata_bytes(state).saturating_add(additional) > state.metadata_limit {
        prune(state, shared)?;
    }
    if metadata_bytes(state).saturating_add(additional) > state.metadata_limit {
        return Err(Box::new(AdmissionLimit(
            "history resource metadata exceeds its limit",
        )));
    }
    Ok(())
}

fn lock_error() -> Error {
    error("history resource lock poisoned")
}

fn metadata_bytes(state: &Store) -> usize {
    state
        .metadata
        .saturating_add(state.plan_bytes.load(Ordering::Relaxed))
}

fn recalculate_metadata(state: &Store, blobs: &BTreeMap<u64, Weak<Blob>>) -> usize {
    let mut bytes = size_of::<Shared>()
        + size_of::<Store>()
        + 64
        + state.read_buffer_bytes
        + state.write_scratch.capacity();
    for weak in blobs.values() {
        bytes = bytes.saturating_add(MAP_ENTRY_BYTES);
        if let Some(blob) = weak.upgrade() {
            bytes = bytes
                .saturating_add(size_of::<Blob>() + 2 * size_of::<usize>())
                .saturating_add(
                    blob.blocks
                        .capacity()
                        .saturating_mul(size_of::<Arc<Block>>()),
                )
                .saturating_add(
                    blob.dependencies
                        .capacity()
                        .saturating_mul(size_of::<ResourceHandle>()),
                );
        }
    }
    for weak in state.blocks.values() {
        bytes = bytes.saturating_add(MAP_ENTRY_BYTES);
        if weak.strong_count() != 0 {
            bytes = bytes.saturating_add(size_of::<Block>() + 4 * size_of::<usize>());
        }
    }
    for candidates in state.contents.values() {
        bytes = bytes.saturating_add(MAP_ENTRY_BYTES).saturating_add(
            candidates
                .capacity()
                .saturating_mul(size_of::<Weak<Block>>()),
        );
    }
    for chunk in state.chunks.values() {
        bytes = bytes.saturating_add(chunk_metadata(&chunk.path));
    }
    bytes
}

fn prune(state: &mut Store, shared: &Shared) -> Result<(), Error> {
    let mut blobs = shared.blobs.lock().map_err(|_| lock_error())?;
    blobs.retain(|_, blob| blob.strong_count() != 0);
    state.blocks.retain(|_, block| block.strong_count() != 0);
    state.contents.retain(|_, candidates| {
        candidates.retain(|block| block.strong_count() != 0);
        !candidates.is_empty()
    });
    state.metadata = recalculate_metadata(state, &blobs);
    Ok(())
}

fn close_active(state: &mut Store) -> Result<(), Error> {
    if let Some(mut active) = state.active.take()
        && let Err(error) = active.file.flush()
    {
        state.active = Some(active);
        return Err(error.into());
    }
    Ok(())
}

fn remove_unused(shared: &Shared, state: &mut Store) -> Result<(), Error> {
    let unused = state
        .chunks
        .iter()
        .filter_map(|(id, chunk)| (Arc::strong_count(chunk) == 1).then_some(*id))
        .collect::<Vec<_>>();
    for id in unused {
        let chunk = &state.chunks[&id];
        shared
            .owner
            .as_ref()
            .ok_or_else(|| error("capture resources are read-only"))?
            .remove(&chunk.path)?;
        state.chunks.remove(&id);
    }
    let blobs = shared.blobs.lock().map_err(|_| lock_error())?;
    state.metadata = recalculate_metadata(state, &blobs);
    Ok(())
}

fn new_chunk(shared: &Shared, state: &mut Store) -> Result<ActiveFile, Error> {
    let owner = shared
        .owner
        .as_ref()
        .ok_or_else(|| error("capture resources are read-only"))?;
    if state.chunks.len() >= MAX_CHUNKS || state.next_file >= FILE_ID {
        return Err(error("history resource chunk directory exceeds its limit"));
    }
    let id = state.next_file;
    state.next_file += 1;
    let path = owner.path.join(format!("{:020}.pvdhistory", FILE_ID + id));
    let metadata = chunk_metadata(&path);
    check_metadata(state, shared, metadata)?;
    let mut owned = owner
        .files
        .lock()
        .map_err(|_| error("history ownership lock poisoned"))?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)?;
    owned.insert(path.clone());
    drop(owned);
    let chunk = Arc::new(Chunk {
        id,
        path,
        bytes: AtomicU64::new(0),
        _owner: Some(owner.clone()),
    });
    state.chunks.insert(id, chunk.clone());
    state.metadata = state.metadata.saturating_add(metadata);
    let mut active = ActiveFile { chunk, file };
    write_counted(&mut active, MAGIC)?;
    Ok(active)
}

fn chunk_metadata(path: &std::path::Path) -> usize {
    (2 * MAP_ENTRY_BYTES + size_of::<Chunk>() + size_of::<PathBuf>() + 2 * size_of::<usize>())
        .saturating_add(path.as_os_str().len().saturating_mul(4))
}

fn write_counted(active: &mut ActiveFile, bytes: &[u8]) -> Result<(), Error> {
    let before = active.chunk.bytes.load(Ordering::Relaxed);
    #[cfg(all(test, feature = "resource-test-write-faults"))]
    let result = raw_write_tests::write_all(&mut active.file, bytes);
    #[cfg(not(all(test, feature = "resource-test-write-faults")))]
    let result = active.file.write_all(bytes);
    let accepted = active.file.metadata().map_or_else(
        |_| before.saturating_add(bytes.len() as u64),
        |metadata| metadata.len(),
    );
    active.chunk.bytes.fetch_max(accepted, Ordering::Relaxed);
    result?;
    Ok(())
}

fn prepare_write_scratch(state: &mut Store, shared: &Shared) -> Result<(), Error> {
    let capacity = shared
        .target
        .checked_sub(FILE_HEADER)
        .and_then(|bytes| usize::try_from(bytes).ok())
        .map(|bytes| bytes.min(WRITE_BUFFER_BYTES))
        .filter(|bytes| *bytes > BLOCK_HEADER as usize)
        .ok_or_else(|| error("history resource write buffer limit is invalid"))?;
    if !state.write_scratch.is_empty() {
        return Err(error("history resource write buffer is not empty"));
    }
    if state.write_scratch.capacity() != 0 {
        return if state.write_scratch.capacity() <= capacity {
            Ok(())
        } else {
            Err(error("history resource write buffer exceeds its limit"))
        };
    }
    check_metadata(state, shared, capacity)?;
    let mut scratch = Vec::new();
    scratch
        .try_reserve_exact(capacity)
        .map_err(|_| error("history resource write buffer allocation failed"))?;
    if scratch.capacity() > capacity {
        return Err(error("history resource write buffer exceeds its limit"));
    }
    state.metadata = state.metadata.saturating_add(scratch.capacity());
    state.write_scratch = scratch;
    Ok(())
}

fn write_block(
    active: &mut ActiveFile,
    block: &Block,
    contents: &[u8],
    scratch: &mut Vec<u8>,
) -> Result<Placement, Error> {
    if block.len == 0 || block.len > MAX_BLOCK || contents.len() != block.len {
        return Err(error("invalid history resource block length"));
    }
    let record_bytes = BLOCK_HEADER as usize + block.len;
    if scratch.capacity() < record_bytes || scratch.capacity() > WRITE_BUFFER_BYTES {
        return Err(error("history resource write buffer capacity is invalid"));
    }
    let offset = active.chunk.bytes.load(Ordering::Relaxed);
    scratch.clear();
    scratch.extend_from_slice(&(block.len as u32).to_le_bytes());
    scratch.extend_from_slice(&block.hash);
    scratch.extend_from_slice(contents);
    let result = write_counted(active, scratch);
    scratch.clear();
    result?;
    Ok(Placement {
        chunk: active.chunk.clone(),
        offset,
    })
}

fn append_block(
    shared: &Shared,
    state: &mut Store,
    block: &Block,
    contents: &[u8],
) -> Result<Placement, Error> {
    prepare_write_scratch(state, shared)?;
    let size = BLOCK_HEADER + block.len as u64;
    if state.active.as_ref().is_some_and(|active| {
        active
            .chunk
            .bytes
            .load(Ordering::Relaxed)
            .saturating_add(size)
            > shared.target
    }) {
        close_active(state)?;
    }
    if state.active.is_none() {
        state.active = Some(new_chunk(shared, state)?);
    }
    write_block(
        state.active.as_mut().unwrap(),
        block,
        contents,
        &mut state.write_scratch,
    )
}

fn boundaries(bytes: &[u8], maximum: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
    let minimum = MIN_BLOCK.min(maximum);
    let mut start = 0usize;
    std::iter::from_fn(move || {
        if start == bytes.len() {
            return None;
        }
        let mut hash = 0u64;
        let mut end = start;
        while end < bytes.len() {
            hash = hash.wrapping_shl(1).wrapping_add(GEAR[bytes[end] as usize]);
            end += 1;
            let len = end - start;
            if len >= maximum || (len >= minimum && hash & 0x3fff == 0) {
                break;
            }
        }
        let range = (start, end);
        start = end;
        Some(range)
    })
}

const GEAR: [u64; 256] = {
    let mut values = [0; 256];
    let mut index = 0;
    let mut seed = 0x9e3779b97f4a7c15u64;
    while index < values.len() {
        seed = seed.wrapping_add(0x9e3779b97f4a7c15);
        let mut value = seed;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
        values[index] = value ^ (value >> 31);
        index += 1;
    }
    values
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, fs, sync::atomic::AtomicU64};

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    thread_local! {
        static PENDING_VISITS: Cell<usize> = const { Cell::new(0) };
    }

    pub(super) fn record_pending_visit() {
        PENDING_VISITS.with(|visits| visits.set(visits.get() + 1));
    }

    fn take_pending_visits() -> usize {
        PENDING_VISITS.with(|visits| visits.replace(0))
    }

    fn store(budget: u64) -> Resources {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../output/shared-resource-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
        Resources::new(CacheDirectory::new(&root).unwrap(), budget)
    }

    fn bytes(len: usize, mut seed: u64) -> Vec<u8> {
        (0..len)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed as u8
            })
            .collect()
    }

    fn save(
        resources: &Resources,
        bytes: &[u8],
        dependencies: &[ResourceHandle],
    ) -> ResourceHandle {
        let mut plan = ResourcePlan::default();
        let handle = resources.plan(bytes, dependencies, &mut plan).unwrap();
        resources.commit(&plan).unwrap();
        handle
    }

    #[test]
    fn committed_reads_and_pins_complete_while_the_writer_store_is_locked() {
        use std::{sync::mpsc, time::Duration};

        let resources = store(8 << 20);
        let child = save(&resources, b"schema", &[]);
        let owner = save(&resources, b"object", std::slice::from_ref(&child));
        let mut pending_plan = ResourcePlan::default();
        let pending = resources.plan(b"pending", &[], &mut pending_plan).unwrap();
        let child_id = child.id();
        let owner_id = owner.id();
        let pending_id = pending.id();
        let reader = resources.reader();
        std::thread::scope(|scope| {
            let (locked_tx, locked_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let (done_tx, done_rx) = mpsc::channel();
            let shared = &resources.shared;
            let writer = scope.spawn(move || {
                let _state = shared.state.lock().unwrap();
                locked_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(30)).unwrap();
            });
            locked_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            let resources = &resources;
            let reader = &reader;
            let reading = scope.spawn(move || {
                assert_eq!(reader.read(owner_id, &mut || false).unwrap(), b"object");
                assert_eq!(
                    reader
                        .read_dependency(owner_id, child_id, 6, &mut || false)
                        .unwrap(),
                    b"schema"
                );
                assert_eq!(resources.try_pin(owner_id).unwrap().unwrap().id(), owner_id);
                assert!(reader.read(pending_id, &mut || false).is_err());
                assert!(reader.read_limited(owner_id, 5, &mut || false).is_err());
                assert!(reader.read(owner_id, &mut || true).is_err());
                done_tx.send(()).unwrap();
            });
            let completed = done_rx.recv_timeout(Duration::from_secs(10));
            release_tx.send(()).unwrap();
            writer.join().unwrap();
            reading.join().unwrap();
            completed.expect("reads and pins waited for the writer store lock");
        });
        drop((reader, pending_plan, pending, owner, child));
        resources.collect().unwrap();
        assert_eq!(resources.physical_bytes(), 0);
    }

    #[test]
    fn unchanged_and_shifted_contents_reuse_committed_blocks() {
        let resources = store(32 << 20);
        let original = bytes(2 << 20, 19);
        let first = save(&resources, &original, &[]);
        let before = resources.physical_bytes();
        let mut same_plan = ResourcePlan::default();
        let second = resources.plan(&original, &[], &mut same_plan).unwrap();
        assert_ne!(first.id(), second.id());
        assert_ne!(first.id() & RESOURCE_ID, 0);
        assert_eq!(resources.planned_bytes(&same_plan), 0);
        resources.commit(&same_plan).unwrap();
        assert_eq!(before, resources.physical_bytes());
        let mut shifted = bytes(73, 51);
        shifted.extend_from_slice(&original[137..]);
        let mut shifted_plan = ResourcePlan::default();
        let shifted_handle = resources.plan(&shifted, &[], &mut shifted_plan).unwrap();
        let planned = resources.planned_bytes(&shifted_plan);
        assert!(
            planned < original.len() as u64 / 5,
            "reused too little: {planned}"
        );
        resources.commit(&shifted_plan).unwrap();
        assert_eq!(
            resources
                .reader()
                .read(shifted_handle.id(), &mut || false)
                .unwrap(),
            shifted
        );
        assert_eq!(
            resources.reader().read(first.id(), &mut || false).unwrap(),
            original
        );
        drop((first, second, shifted_handle, same_plan, shifted_plan));
        resources.collect().unwrap();
        assert_eq!(resources.physical_bytes(), 0);
    }

    #[test]
    fn ready_dependency_subgraphs_are_pinned_without_replanning_their_contents() {
        let resources = store(8 << 20);
        let mut nodes: Vec<ResourceHandle> = Vec::new();
        for index in 0..1023u32 {
            let children = if index < 512 {
                Vec::new()
            } else {
                let first = (index as usize - 512) * 2;
                vec![nodes[first].clone(), nodes[first + 1].clone()]
            };
            nodes.push(save(&resources, &index.to_le_bytes(), &children));
        }
        let subtree = nodes.pop().unwrap();
        let leaf_id = nodes[0].id();
        drop(nodes);
        take_pending_visits();

        let mut plan = ResourcePlan::default();
        let parent = resources
            .plan(b"new parent", std::slice::from_ref(&subtree), &mut plan)
            .unwrap();
        assert_eq!(take_pending_visits(), 2);
        assert_eq!(plan.blobs.len(), 1);
        assert_eq!(plan.blocks.len(), 1);
        assert!(plan.blobs.contains_key(&parent.id()));
        drop(subtree);
        resources.collect().unwrap();
        assert!(resources.pin(leaf_id).is_ok());
        assert!(resources.reader().read(parent.id(), &mut || false).is_err());
        resources.commit(&plan).unwrap();
        assert_eq!(
            resources.reader().read(leaf_id, &mut || false).unwrap(),
            0u32.to_le_bytes()
        );
        drop((parent, plan));
        resources.collect().unwrap();
        assert!(resources.pin(leaf_id).is_err());
        assert_eq!(resources.physical_bytes(), 0);
    }

    #[test]
    fn current_plan_subgraphs_are_visited_only_at_their_boundary() {
        let resources = store(8 << 20);
        let mut plan = ResourcePlan::default();
        let mut parent = resources.plan(b"leaf", &[], &mut plan).unwrap();
        take_pending_visits();
        for index in 1..32u32 {
            parent = resources
                .plan(&index.to_le_bytes(), &[parent], &mut plan)
                .unwrap();
            assert_eq!(take_pending_visits(), 2);
        }
        assert_eq!(plan.blobs.len(), 32);
        assert_eq!(plan.blocks.len(), 32);
        assert!(
            plan.blobs
                .values()
                .all(|blob| !blob.ready.load(Ordering::Acquire))
        );
        resources.commit(&plan).unwrap();
        assert!(
            plan.blobs
                .values()
                .all(|blob| blob.ready.load(Ordering::Acquire))
        );
        assert_eq!(
            resources.reader().read(parent.id(), &mut || false).unwrap(),
            31u32.to_le_bytes()
        );
        drop((parent, plan));
        resources.collect().unwrap();
        assert_eq!(resources.physical_bytes(), 0);
    }

    #[test]
    fn unready_cross_plan_dependencies_commit_together_and_survive_a_retry() {
        let resources = store(8 << 20);
        let old = save(&resources, b"old committed", &[]);
        let mut child_plan = ResourcePlan::default();
        let child = resources
            .plan(b"pending child", &[], &mut child_plan)
            .unwrap();
        let child_id = child.id();
        let mut parent_plan = ResourcePlan::default();
        take_pending_visits();
        let parent = resources
            .plan(b"pending parent", &[old.clone(), child], &mut parent_plan)
            .unwrap();
        assert_eq!(take_pending_visits(), 3);
        assert_eq!(parent_plan.blobs.len(), 2);
        assert_eq!(parent_plan.blocks.len(), 2);
        assert!(!parent_plan.blobs.contains_key(&old.id()));
        assert!(parent_plan.blobs.contains_key(&child_id));
        drop(child_plan);
        {
            let mut state = resources.shared.state.lock().unwrap();
            let active = state.active.as_mut().unwrap();
            active.file = File::open(&active.chunk.path).unwrap();
        }
        assert!(resources.commit(&parent_plan).is_err());
        assert_eq!(
            resources.reader().read(old.id(), &mut || false).unwrap(),
            b"old committed"
        );
        assert!(resources.reader().read(child_id, &mut || false).is_err());
        assert!(resources.reader().read(parent.id(), &mut || false).is_err());
        assert!(
            parent_plan
                .blobs
                .values()
                .all(|blob| !blob.ready.load(Ordering::Acquire))
        );
        resources.commit(&parent_plan).unwrap();
        assert_eq!(
            resources.reader().read(child_id, &mut || false).unwrap(),
            b"pending child"
        );
        assert_eq!(
            resources.reader().read(parent.id(), &mut || false).unwrap(),
            b"pending parent"
        );
        drop((old, parent, parent_plan));
        resources.collect().unwrap();
        assert_eq!(resources.physical_bytes(), 0);
    }

    #[test]
    fn dependencies_pin_lifetimes_but_reader_directories_do_not() {
        let resources = store(8 << 20);
        let base = save(&resources, &[7; 20_000], &[]);
        let id = base.id();
        let derived = save(&resources, b"reference", std::slice::from_ref(&base));
        let reader = resources.reader();
        drop(base);
        resources.collect().unwrap();
        assert!(resources.pin(id).is_ok());
        assert_eq!(reader.read(id, &mut || false).unwrap(), vec![7; 20_000]);
        drop(derived);
        resources.collect().unwrap();
        assert!(resources.pin(id).is_err());
        assert!(reader.read(id, &mut || false).is_err());
        assert_eq!(resources.physical_bytes(), 0);
    }

    #[test]
    fn invalidation_retires_dependencies_while_an_inflight_placement_remains_readable() {
        use std::{sync::mpsc, time::Duration};

        let resources = store(8 << 20);
        let contents = bytes(3000, 71);
        let base = save(&resources, &contents, &[]);
        let base_id = base.id();
        let block = base.0.blocks[0].clone();
        assert_eq!(base.0.blocks.len(), 1);
        let placement = match &*block.location.lock().unwrap() {
            BlockLocation::Stored(placement) => placement.clone(),
            _ => unreachable!(),
        };
        let parent = save(&resources, b"parent", std::slice::from_ref(&base));
        let base_weak = Arc::downgrade(&base.0);
        let parent_weak = Arc::downgrade(&parent.0);
        let lease = super::super::ResourceLease::new(vec![parent]);
        let weak_lease = lease.downgrade();
        drop(base);
        let (ready_tx, ready_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            let shared = &resources.shared;
            let reader = scope.spawn(move || {
                let mut first_check = true;
                shared
                    .read_placement(&block, &placement, &mut || {
                        if first_check {
                            first_check = false;
                            ready_tx.send(()).unwrap();
                            resume_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                        }
                        false
                    })
                    .unwrap()
            });
            ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            lease.invalidate().unwrap();
            assert!(!lease.available() && !weak_lease.available());
            assert!(lease.extend(Vec::new()).is_err());
            assert!(base_weak.upgrade().is_none() && parent_weak.upgrade().is_none());
            assert!(resources.pin(base_id).is_err());
            resources.collect().unwrap();
            assert!(resources.physical_bytes() > 0);
            resume_tx.send(()).unwrap();
            assert_eq!(reader.join().unwrap(), contents);
        });
        lease.invalidate().unwrap();
        resources.collect().unwrap();
        assert_eq!(resources.physical_bytes(), 0);
    }

    #[test]
    fn checksum_failure_resets_the_reader_before_a_valid_following_block() {
        let resources = store(8 << 20);
        let invalid = save(&resources, &[3; 3000], &[]);
        let valid = save(&resources, &[9; 3000], &[]);
        let placement = match &*invalid.0.blocks[0].location.lock().unwrap() {
            BlockLocation::Stored(placement) => placement.clone(),
            _ => unreachable!(),
        };
        let mut file = OpenOptions::new()
            .write(true)
            .open(&placement.chunk.path)
            .unwrap();
        file.seek(SeekFrom::Start(placement.offset + BLOCK_HEADER))
            .unwrap();
        file.write_all(&[4]).unwrap();
        file.flush().unwrap();
        drop(file);
        let reader = resources.reader();
        for _ in 0..2 {
            assert!(
                reader
                    .read(invalid.id(), &mut || false)
                    .unwrap_err()
                    .to_string()
                    .contains("block checksum mismatch")
            );
            assert!(
                resources
                    .shared
                    .reader
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .position
                    .is_none()
            );
            assert_eq!(reader.read(valid.id(), &mut || false).unwrap(), [9; 3000]);
        }
        drop((invalid, valid, placement));
        resources.collect().unwrap();
        assert_eq!(resources.physical_bytes(), 0);
    }

    #[test]
    fn mixed_chunk_compaction_preserves_inflight_locations_and_accounts_retired_files() {
        let resources = store(8 << 20);
        let first = save(&resources, &[1; 5000], &[]);
        let second = save(&resources, &[2; 7000], &[]);
        let block = second.0.blocks[0].clone();
        let old = match &*block.location.lock().unwrap() {
            BlockLocation::Stored(placement) => placement.clone(),
            _ => panic!("uncommitted block"),
        };
        let before = resources.physical_bytes();
        assert_eq!(resources.shared.state.lock().unwrap().chunks.len(), 1);
        drop(first);
        resources.collect().unwrap();
        assert!(resources.compact_one().unwrap());
        assert!(resources.physical_bytes() > before);
        assert!(old.chunk.path.exists());
        assert_eq!(
            resources
                .shared
                .read_placement(&block, &old, &mut || false)
                .unwrap(),
            vec![2; 7000]
        );
        assert_eq!(
            resources.reader().read(second.id(), &mut || false).unwrap(),
            vec![2; 7000]
        );
        resources.collect().unwrap();
        assert!(old.chunk.path.exists());
        let old_path = old.chunk.path.clone();
        drop(old);
        resources.collect().unwrap();
        assert!(!old_path.exists());
        assert!(resources.physical_bytes() < before);
        drop((second, block));
        resources.collect().unwrap();
        assert_eq!(resources.physical_bytes(), 0);
    }

    #[test]
    fn cancellation_retries_and_checksums_reject_corrupt_bytes() {
        let resources = store(16 << 20);
        let contents = bytes(400_000, 41);
        let handle = save(&resources, &contents, &[]);
        let reader = resources.reader();
        let mut checks = 0;
        let failure = reader
            .read(handle.id(), &mut || {
                checks += 1;
                checks == 10
            })
            .unwrap_err();
        assert_eq!(
            failure.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
        assert!(
            resources
                .shared
                .reader
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .position
                .is_none()
        );
        assert_eq!(reader.read(handle.id(), &mut || false).unwrap(), contents);
        let block = &handle.0.blocks[0];
        let placement = match &*block.location.lock().unwrap() {
            BlockLocation::Stored(placement) => placement.clone(),
            _ => unreachable!(),
        };
        for _ in 0..2 {
            assert_eq!(
                resources
                    .shared
                    .read_placement(block, &placement, &mut || false)
                    .unwrap(),
                contents[..block.len]
            );
        }
        let mut file = OpenOptions::new()
            .write(true)
            .open(&placement.chunk.path)
            .unwrap();
        file.seek(SeekFrom::Start(placement.offset + BLOCK_HEADER + 5))
            .unwrap();
        file.write_all(&[contents[5] ^ 0xff]).unwrap();
        file.flush().unwrap();
        assert!(
            resources
                .reader()
                .read(handle.id(), &mut || false)
                .unwrap_err()
                .to_string()
                .contains("checksum")
        );
    }

    #[test]
    fn failed_writes_leave_old_resources_valid_and_new_resources_retryable() {
        let resources = store(8 << 20);
        let first = save(&resources, &[3; 1000], &[]);
        {
            let mut state = resources.shared.state.lock().unwrap();
            let active = state.active.as_mut().unwrap();
            active.file = File::open(&active.chunk.path).unwrap();
        }
        let mut plan = ResourcePlan::default();
        let second = resources.plan(&[4; 2000], &[], &mut plan).unwrap();
        assert!(resources.commit(&plan).is_err());
        assert_eq!(
            resources.reader().read(first.id(), &mut || false).unwrap(),
            vec![3; 1000]
        );
        assert!(resources.reader().read(second.id(), &mut || false).is_err());
        resources.commit(&plan).unwrap();
        assert_eq!(
            resources.reader().read(second.id(), &mut || false).unwrap(),
            vec![4; 2000]
        );
    }

    #[test]
    fn collision_candidates_require_matching_bytes_and_whole_blob_checksum() {
        let resources = store(8 << 20);
        let first = save(&resources, &[5; 5000], &[]);
        let contents = vec![6; 5000];
        resources.shared.state.lock().unwrap().contents.insert(
            (*blake3::hash(&contents).as_bytes(), contents.len()),
            vec![Arc::downgrade(&first.0.blocks[0])],
        );
        let second = save(&resources, &contents, &[]);
        assert_ne!(first.0.blocks[0].id, second.0.blocks[0].id);
        assert_eq!(
            resources.reader().read(second.id(), &mut || false).unwrap(),
            contents
        );
        let invalid_id = RESOURCE_ID + 500;
        let invalid = Arc::new(Blob {
            id: invalid_id,
            len: second.0.len,
            hash: [0; 32],
            blocks: second.0.blocks.clone(),
            dependencies: Vec::new(),
            depth: 1,
            ready: AtomicBool::new(true),
            store: Arc::downgrade(&resources.shared),
        });
        resources
            .shared
            .blobs
            .lock()
            .unwrap()
            .insert(invalid_id, Arc::downgrade(&invalid));
        assert!(
            resources
                .reader()
                .read(invalid_id, &mut || false)
                .unwrap_err()
                .to_string()
                .contains("resource checksum")
        );
    }

    #[test]
    fn budget_estimates_cover_headers_and_small_chunks_and_limits_fail_before_publish() {
        let resources = store(4096);
        assert_eq!(resources.reserve_bytes(), 256);
        let mut plan = ResourcePlan::default();
        let handle = resources.plan(&bytes(2000, 61), &[], &mut plan).unwrap();
        let planned = resources.planned_bytes(&plan);
        resources.commit(&plan).unwrap();
        assert_eq!(resources.physical_bytes(), planned);
        assert!(
            resources
                .shared
                .state
                .lock()
                .unwrap()
                .chunks
                .values()
                .all(|chunk| chunk.bytes.load(Ordering::Relaxed) <= 256)
        );
        assert_eq!(
            resources.reader().read(handle.id(), &mut || false).unwrap(),
            bytes(2000, 61)
        );
        let mut oversized = ResourcePlan {
            input_bytes: MAX_TRANSACTION_BYTES - 2,
            ..Default::default()
        };
        assert!(resources.plan(b"abc", &[], &mut oversized).is_err());
        assert!(
            resources
                .plan(&vec![0; MAX_RECORD + 1], &[], &mut ResourcePlan::default())
                .is_err()
        );
        assert!(resources.metadata_bytes() > 0);
        let limited = store(8 << 20);
        limited.shared.state.lock().unwrap().entry_limit = 2;
        let first = save(&limited, b"one", &[]);
        assert!(
            limited
                .plan(b"two", &[], &mut ResourcePlan::default())
                .is_err()
        );
        drop(first);
        limited.collect().unwrap();
        limited.shared.state.lock().unwrap().metadata_limit = 1;
        assert!(
            limited
                .plan(b"three", &[], &mut ResourcePlan::default())
                .is_err()
        );
    }

    #[test]
    fn foreign_dependencies_and_plans_are_rejected_and_dependency_depth_is_bounded() {
        let resources = store(8 << 20);
        let other = store(8 << 20);
        assert!(resources.same(&resources.clone()));
        assert!(!resources.same(&other));
        let first = save(&resources, b"one", &[]);
        assert!(
            other
                .plan(
                    b"two",
                    std::slice::from_ref(&first),
                    &mut ResourcePlan::default()
                )
                .is_err()
        );
        let mut plan = ResourcePlan::default();
        let _ = resources.plan(b"three", &[], &mut plan).unwrap();
        assert!(other.commit(&plan).is_err());
        assert_eq!(other.planned_bytes(&plan), u64::MAX);
        let mut parent = first;
        for _ in 1..MAX_DEPENDENCY_DEPTH {
            parent = save(&resources, b"", &[parent]);
        }
        assert!(
            resources
                .plan(b"", &[parent], &mut ResourcePlan::default())
                .is_err()
        );
    }

    #[test]
    fn failed_deletion_keeps_physical_accounting_until_actual_removal() {
        let resources = store(8 << 20);
        let handle = save(&resources, b"kept bytes", &[]);
        resources.collect().unwrap();
        let path = resources
            .shared
            .state
            .lock()
            .unwrap()
            .chunks
            .values()
            .next()
            .unwrap()
            .path
            .clone();
        let before = resources.physical_bytes();
        resources
            .shared
            .owner
            .as_ref()
            .unwrap()
            .files
            .lock()
            .unwrap()
            .remove(&path);
        drop(handle);
        assert!(resources.collect().is_err());
        assert_eq!(resources.physical_bytes(), before);
        assert!(fs::metadata(&path).unwrap().len() > 0);
        resources
            .shared
            .owner
            .as_ref()
            .unwrap()
            .files
            .lock()
            .unwrap()
            .insert(path.clone());
        resources.collect().unwrap();
        assert_eq!(resources.physical_bytes(), 0);
        assert!(!path.exists());
    }

    #[test]
    fn reclaim_preflight_reserves_directory_and_metadata_headroom() {
        let resources = store(8 << 20);
        {
            let mut state = resources.shared.state.lock().unwrap();
            state.entry_limit = 32;
            state.metadata_limit = 1 << 20;
        }
        let mut handles = (0..9)
            .map(|index| save(&resources, &[index; 100], &[]))
            .collect::<Vec<_>>();
        assert!(!resources.needs_reclaim(0));
        assert!(resources.needs_reclaim(2));
        handles.truncate(5);
        resources.collect().unwrap();
        assert!(!resources.needs_reclaim(2));
        assert!(resources.needs_reclaim(usize::MAX));
        {
            let mut state = resources.shared.state.lock().unwrap();
            let target = state.metadata_limit - state.metadata_limit / 4;
            state.metadata = target - 1024;
        }
        assert!(!resources.needs_reclaim(0));
        assert!(resources.needs_reclaim(1));
    }

    #[test]
    fn reclaim_preflight_treats_a_poisoned_directory_as_unavailable() {
        let resources = store(8 << 20);
        let shared = resources.shared.clone();
        assert!(
            std::thread::spawn(move || {
                let _state = shared.state.lock().unwrap();
                panic!("poison resource directory");
            })
            .join()
            .is_err()
        );
        assert!(resources.needs_reclaim(0));
    }
}

#[cfg(test)]
mod raw_write_tests;
