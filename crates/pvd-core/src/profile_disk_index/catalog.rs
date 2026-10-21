//! Source-bound catalog pages ordered by the minimum recorded start of each exact CPU event key.
use super::storage::{Blob, Lease, META_MAGIC, open_metadata, write_metadata};
use super::{IO_MEMORY, add, check, memory, mul, progress, sum, verify_source};
pub use super::{IndexLimits, IndexOptions, Phase, Progress};
use crate::{
    Error,
    analysis::{Analysis, ProfileZone},
    analysis_export::{ProfileObserver, scan_profile},
    error,
    profile_curves::{CatalogEntry, ExactKey, MAX_CATALOG_PAGE, SourceIdentity},
    profile_timeline::GlobalPairingStats,
};
use pvd_protocol::Header;
use std::{
    collections::{BTreeMap, BTreeSet, BinaryHeap},
    fs::File,
    hash::BuildHasher,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};
mod storage;
use storage::{HEADER, Order, Reader, Row, Run, WIDTH, Writer};

pub const SORT_VERSION: u32 = 1;
pub const MAX_PAGE_BYTES: usize = 32 << 20;
const ENTRY_BYTES: usize = size_of::<CatalogEntry>();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BuildIdentity {
    source_hash: [u8; 32],
    session: [u8; 16],
}
impl BuildIdentity {
    fn new(source_hash: [u8; 32]) -> Result<Self, Error> {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let sequence = SEQUENCE
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| error("catalog build identity sequence exhausted"))?;
        let time = std::time::SystemTime::now();
        let mut session = [0; 16];
        for (part, bytes) in session.chunks_exact_mut(8).enumerate() {
            let random = std::collections::hash_map::RandomState::new();
            let word = random.hash_one((time, std::process::id(), sequence, part));
            bytes.copy_from_slice(&word.to_le_bytes());
        }
        Ok(Self {
            source_hash,
            session,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FirstStartPosition {
    pub first_start: Option<u64>,
    pub key: ExactKey,
}
impl Ord for FirstStartPosition {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.first_start.is_none(), self.first_start, self.key).cmp(&(
            other.first_start.is_none(),
            other.first_start,
            other.key,
        ))
    }
}
impl PartialOrd for FirstStartPosition {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirstStartCursor {
    pub identity: SourceIdentity,
    pub sort_version: u32,
    pub after: FirstStartPosition,
}
#[derive(Clone, Copy, Debug)]
pub struct PageLimits {
    pub entries: usize,
    pub bytes: usize,
}
impl Default for PageLimits {
    fn default() -> Self {
        Self {
            entries: 1024,
            bytes: MAX_PAGE_BYTES,
        }
    }
}
#[derive(Clone, Debug)]
pub struct FirstStartPageReport {
    pub identity: SourceIdentity,
    pub entries: Vec<CatalogEntry>,
    pub zones: BTreeMap<u64, ProfileZone>,
    pub next: Option<FirstStartCursor>,
    /// Includes owned capacities and estimated map nodes; excludes decoder state and process overhead.
    pub estimated_index_bytes: usize,
}
#[derive(Clone, Debug)]
pub struct CatalogIndexInfo {
    pub identity: SourceIdentity,
    pub series: u64,
    pub global_pairing: GlobalPairingStats,
    pub legacy_cuda_buffers: u64,
    pub disk_bytes: u64,
    pub peak_disk_bytes: u64,
    pub contribution_runs: u64,
    pub key_merge_passes: usize,
    pub start_merge_passes: usize,
}
pub struct CatalogIndex {
    source: PathBuf,
    lease: Lease,
    data: Run,
    metadata: Blob,
    limits: IndexLimits,
    info: CatalogIndexInfo,
}

pub fn build_index(
    source: &Path,
    workspace: &Path,
    expected: &SourceIdentity,
    options: impl Into<IndexOptions>,
    cancelled: &AtomicBool,
    mut callback: impl FnMut(Progress) -> bool,
) -> Result<CatalogIndex, Error> {
    check(cancelled)?;
    let IndexOptions {
        limits,
        before_write,
    } = options.into();
    if limits.run_bytes < size_of::<Row>()
        || !(1..=1024).contains(&limits.page_records)
        || !(2..=16).contains(&limits.merge_fan_in)
        || limits.disk_bytes < HEADER as u64
    {
        return Err(error("invalid first-start catalog limits"));
    }
    let page_memory = mul(limits.page_records, WIDTH)?;
    memory(
        sum(limits.run_bytes, sum(mul(2, page_memory)?, IO_MEMORY)?)?,
        limits.memory_bytes,
    )?;
    memory(
        sum(
            mul(limits.merge_fan_in + 1, page_memory)?,
            sum(mul(limits.merge_fan_in, 1024)?, IO_MEMORY)?,
        )?,
        limits.memory_bytes,
    )?;
    let source = source.canonicalize()?;
    let mut input = File::open(&source)?;
    if !input.metadata()?.is_file() {
        return Err(error("catalog source must be a regular file"));
    }
    let mut lease = Lease::create(workspace, &source, limits.disk_bytes, before_write)?;
    let identity = BuildIdentity::new(expected.blake3)?;
    let mut builder = Builder::new(&mut lease, limits, identity, cancelled)?;
    let report = scan_profile(&mut input, &mut builder, &mut |p| {
        !cancelled.load(Ordering::Relaxed)
            && callback(Progress {
                phase: Phase::Decode,
                completed: p.bytes_read,
                total: p.total_bytes,
            })
    })?;
    if SourceIdentity::from(&report) != *expected {
        return Err(error("first-start catalog source identity changed"));
    }
    builder.flush(Order::Key)?;
    if builder.runs == 0 {
        builder.empty(Order::Key)?;
    }
    let runs = builder.runs;
    let pairing = builder.pairing;
    drop(builder);
    if pairing.complete_spans != report.summary.profile_spans
        || pairing.open_starts != report.summary.open_spans as u64
        || pairing.unmatched_stops != report.summary.unmatched_stops
    {
        return Err(corrupt_catalog());
    }
    let metadata = write_metadata(&mut lease, &report.summary, cancelled, &mut callback)?;
    let cuda = report.summary.legacy_cuda_buffers;
    drop(report);
    let (key_run, key_passes) = merge_all(
        &mut lease,
        0,
        runs,
        Order::Key,
        limits,
        identity,
        cancelled,
        &mut callback,
    )?;
    let series = key_run.records;
    let start_first = lease.next_id();
    let mut reader = Reader::new(&lease, key_run.clone())?;
    let mut builder = Builder::new(&mut lease, limits, identity, cancelled)?;
    while let Some(row) = reader.next()? {
        progress(
            cancelled,
            &mut callback,
            Phase::Merge,
            builder.accepted,
            series,
        )?;
        builder.push(row, Order::Start)?;
    }
    builder.flush(Order::Start)?;
    if builder.runs == 0 {
        builder.empty(Order::Start)?;
    }
    let start_runs = builder.runs;
    drop(builder);
    drop(reader);
    lease.remove(key_run.id)?;
    let (data, start_passes) = merge_all(
        &mut lease,
        start_first,
        start_runs,
        Order::Start,
        limits,
        identity,
        cancelled,
        &mut callback,
    )?;
    if data.records != series {
        return Err(corrupt_catalog());
    }
    progress(cancelled, &mut callback, Phase::Merge, series, series)?;
    let info = CatalogIndexInfo {
        identity: expected.clone(),
        series,
        global_pairing: pairing,
        legacy_cuda_buffers: cuda,
        disk_bytes: lease.used,
        peak_disk_bytes: lease.peak,
        contribution_runs: runs,
        key_merge_passes: key_passes,
        start_merge_passes: start_passes,
    };
    Ok(CatalogIndex {
        source,
        lease,
        data,
        metadata,
        limits,
        info,
    })
}

struct Builder<'a> {
    lease: &'a mut Lease,
    rows: Vec<Row>,
    capacity: usize,
    page_records: usize,
    runs: u64,
    accepted: u64,
    pairing: GlobalPairingStats,
    identity: BuildIdentity,
    cancelled: &'a AtomicBool,
}
impl<'a> Builder<'a> {
    fn new(
        lease: &'a mut Lease,
        limits: IndexLimits,
        identity: BuildIdentity,
        cancelled: &'a AtomicBool,
    ) -> Result<Self, Error> {
        let capacity = limits.run_bytes / size_of::<Row>();
        let mut rows = Vec::new();
        rows.try_reserve_exact(capacity)?;
        memory(
            sum(
                mul(rows.capacity(), size_of::<Row>())?,
                sum(mul(2 * limits.page_records, WIDTH)?, IO_MEMORY)?,
            )?,
            limits.memory_bytes,
        )?;
        Ok(Self {
            lease,
            rows,
            capacity,
            page_records: limits.page_records,
            runs: 0,
            accepted: 0,
            pairing: GlobalPairingStats::default(),
            identity,
            cancelled,
        })
    }
    fn push(&mut self, row: Row, order: Order) -> Result<(), Error> {
        check(self.cancelled)?;
        self.rows.push(row);
        add(&mut self.accepted, 1)?;
        if self.rows.len() == self.capacity {
            self.flush(order)?;
        }
        Ok(())
    }
    fn empty(&mut self, order: Order) -> Result<(), Error> {
        check(self.cancelled)?;
        Writer::new(self.lease, self.page_records, order, self.identity)?.finish(self.lease)?;
        add(&mut self.runs, 1)
    }
    fn flush(&mut self, order: Order) -> Result<(), Error> {
        if self.rows.is_empty() {
            return Ok(());
        }
        check(self.cancelled)?;
        self.rows.sort_unstable_by(|a, b| a.compare(b, order));
        let mut writer = Writer::new(self.lease, self.page_records, order, self.identity)?;
        let mut pending: Option<Row> = None;
        for row in &self.rows {
            check(self.cancelled)?;
            if let Some(old) = &mut pending {
                if order == Order::Key && old.key == row.key {
                    old.add(row)?;
                    continue;
                }
                writer.push(self.lease, old)?;
            }
            pending = Some(row.clone());
        }
        if let Some(row) = pending {
            writer.push(self.lease, &row)?;
        }
        writer.finish(self.lease)?;
        self.rows.clear();
        add(&mut self.runs, 1)
    }
}
impl ProfileObserver for Builder<'_> {
    fn batch(&mut self, analysis: &Analysis) -> Result<(), Error> {
        for span in &analysis.spans {
            self.push(
                Row {
                    key: (span.zone, span.event, span.thread, span.context).into(),
                    first: Some(span.start),
                    complete: 1,
                    open: 0,
                    unmatched: 0,
                },
                Order::Key,
            )?;
            add(&mut self.pairing.complete_spans, 1)?;
        }
        Ok(())
    }
    fn finish(&mut self, analysis: &Analysis, _: &Header) -> Result<(), Error> {
        for (key, starts) in analysis.open_event_starts() {
            let count = u64::try_from(starts.len())?;
            if count != 0 {
                self.push(
                    Row {
                        key: (*key).into(),
                        first: starts.iter().copied().min(),
                        complete: 0,
                        open: count,
                        unmatched: 0,
                    },
                    Order::Key,
                )?;
                add(&mut self.pairing.open_starts, count)?;
            }
        }
        for (key, &count) in &analysis.unmatched_stop_events {
            if count != 0 {
                self.push(
                    Row {
                        key: (*key).into(),
                        first: None,
                        complete: 0,
                        open: 0,
                        unmatched: count,
                    },
                    Order::Key,
                )?;
                add(&mut self.pairing.unmatched_stops, count)?;
            }
        }
        Ok(())
    }
}

#[derive(Eq)]
struct HeapRow {
    row: Row,
    index: usize,
    order: Order,
}
impl PartialEq for HeapRow {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}
impl PartialOrd for HeapRow {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for HeapRow {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .row
            .compare(&self.row, self.order)
            .then_with(|| other.index.cmp(&self.index))
    }
}
#[allow(clippy::too_many_arguments)]
fn merge_all(
    lease: &mut Lease,
    mut first: u64,
    mut count: u64,
    order: Order,
    limits: IndexLimits,
    identity: BuildIdentity,
    cancelled: &AtomicBool,
    callback: &mut impl FnMut(Progress) -> bool,
) -> Result<(Run, usize), Error> {
    let mut passes = 0;
    while count > 1 {
        let next_first = lease.next_id();
        let mut next_count = 0;
        for start in (0..count).step_by(limits.merge_fan_in) {
            check(cancelled)?;
            let end = (start + limits.merge_fan_in as u64).min(count);
            let mut readers = Vec::new();
            readers.try_reserve_exact((end - start) as usize)?;
            let mut heap = BinaryHeap::new();
            heap.try_reserve_exact((end - start) as usize)?;
            memory(
                sum(
                    sum(
                        mul(readers.capacity(), size_of::<Reader>())?,
                        mul(heap.capacity(), size_of::<HeapRow>())?,
                    )?,
                    sum(
                        mul(limits.merge_fan_in + 1, mul(limits.page_records, WIDTH)?)?,
                        IO_MEMORY,
                    )?,
                )?,
                limits.memory_bytes,
            )?;
            let mut total = 0;
            for id in first + start..first + end {
                let run = Run::open(lease, id, order, identity)?;
                add(&mut total, run.records)?;
                let mut reader = Reader::new(lease, run)?;
                if let Some(row) = reader.next()? {
                    heap.push(HeapRow {
                        row,
                        index: readers.len(),
                        order,
                    });
                }
                readers.push(reader);
            }
            let mut writer = Writer::new(lease, limits.page_records, order, identity)?;
            let mut completed = 0;
            let mut pending: Option<Row> = None;
            while let Some(HeapRow { row, index, .. }) = heap.pop() {
                progress(cancelled, callback, Phase::Merge, completed, total)?;
                add(&mut completed, 1)?;
                if let Some(old) = &mut pending {
                    if order == Order::Key && old.key == row.key {
                        old.add(&row)?;
                    } else {
                        writer.push(lease, old)?;
                        *old = row;
                    }
                } else {
                    pending = Some(row);
                }
                if let Some(row) = readers[index].next()? {
                    heap.push(HeapRow { row, index, order });
                }
            }
            if completed != total {
                return Err(corrupt_catalog());
            }
            if let Some(row) = pending {
                writer.push(lease, &row)?;
            }
            writer.finish(lease)?;
            drop(readers);
            for id in first + start..first + end {
                lease.remove(id)?;
            }
            add(&mut next_count, 1)?;
        }
        first = next_first;
        count = next_count;
        passes += 1;
    }
    Ok((Run::open(lease, first, order, identity)?, passes))
}

impl CatalogIndex {
    pub fn info(&self) -> &CatalogIndexInfo {
        &self.info
    }
    pub fn page(
        &self,
        cursor: Option<&FirstStartCursor>,
        limits: PageLimits,
        cancelled: &AtomicBool,
        mut callback: impl FnMut(Progress) -> bool,
    ) -> Result<FirstStartPageReport, Error> {
        check(cancelled)?;
        if !(1..=MAX_CATALOG_PAGE).contains(&limits.entries)
            || !(1..=MAX_PAGE_BYTES).contains(&limits.bytes)
        {
            return Err(error("invalid first-start catalog page limits"));
        }
        if cursor
            .is_some_and(|c| c.sort_version != SORT_VERSION || c.identity != self.info.identity)
        {
            return Err(error(
                "first-start catalog cursor has a different source or sort version",
            ));
        }
        verify_source(&self.source, &self.info.identity, cancelled, &mut callback)?;
        let mut reader = Reader::new(&self.lease, self.data.clone())?;
        let mut low = 0;
        let mut high = self.data.records;
        if let Some(cursor) = cursor {
            while low < high {
                progress(
                    cancelled,
                    &mut callback,
                    Phase::Query,
                    low,
                    self.data.records,
                )?;
                let mid = low + (high - low) / 2;
                if reader.at(mid)?.position() <= cursor.after {
                    low = mid + 1;
                } else {
                    high = mid;
                }
            }
        }
        page_memory(
            mul(limits.entries, ENTRY_BYTES)?,
            limits,
            self.limits,
            self.data.page_records,
        )?;
        let mut entries = Vec::new();
        entries.try_reserve_exact(limits.entries)?;
        let mut bytes = mul(entries.capacity(), ENTRY_BYTES)?;
        page_memory(bytes, limits, self.limits, self.data.page_records)?;
        let end = low
            .saturating_add(limits.entries as u64)
            .min(self.data.records);
        let mut previous = cursor.map(|c| c.after);
        for index in low..end {
            progress(
                cancelled,
                &mut callback,
                Phase::Query,
                index,
                self.data.records,
            )?;
            let row = reader.at(index)?;
            if previous.is_some_and(|old| old >= row.position()) {
                return Err(corrupt_catalog());
            }
            previous = Some(row.position());
            entries.push(CatalogEntry {
                key: row.key,
                first_start: row.first,
                complete_spans: usize::try_from(row.complete)?,
                open_spans: usize::try_from(row.open)?,
                unmatched_stops: row.unmatched,
                metadata_ambiguous: false,
            });
        }
        let next = (end < self.data.records).then(|| FirstStartCursor {
            identity: self.info.identity.clone(),
            sort_version: SORT_VERSION,
            after: previous.unwrap(),
        });
        let zones = names(
            &self.lease,
            self.metadata,
            &mut entries,
            &mut bytes,
            limits,
            self.limits,
            self.data.page_records,
            cancelled,
            &mut callback,
        )?;
        progress(
            cancelled,
            &mut callback,
            Phase::Query,
            end,
            self.data.records,
        )?;
        Ok(FirstStartPageReport {
            identity: self.info.identity.clone(),
            entries,
            zones,
            next,
            estimated_index_bytes: bytes,
        })
    }
}

fn page_memory(
    bytes: usize,
    limits: PageLimits,
    index: IndexLimits,
    page_records: usize,
) -> Result<(), Error> {
    if bytes > limits.bytes {
        return Err(error(
            "first-start catalog page exceeds its byte budget; reduce page size",
        ));
    }
    memory(
        sum(bytes, sum(mul(page_records, WIDTH)?, IO_MEMORY)?)?,
        index.memory_bytes,
    )
}
#[allow(clippy::too_many_arguments)]
fn names(
    lease: &Lease,
    blob: Blob,
    entries: &mut [CatalogEntry],
    bytes: &mut usize,
    limits: PageLimits,
    index: IndexLimits,
    page_records: usize,
    cancelled: &AtomicBool,
    callback: &mut impl FnMut(Progress) -> bool,
) -> Result<BTreeMap<u64, ProfileZone>, Error> {
    let mut keys = BTreeSet::new();
    for entry in entries.iter() {
        if !keys.contains(&(entry.key.zone, entry.key.event)) {
            *bytes = sum(*bytes, 128)?;
            page_memory(*bytes, limits, index, page_records)?;
            keys.insert((entry.key.zone, entry.key.event));
        }
    }
    let mut reader = open_metadata(lease, blob, cancelled, callback)?;
    let mut magic = [0; 8];
    reader.read(&mut magic)?;
    if &magic != META_MAGIC {
        return Err(corrupt_catalog());
    }
    let mut zones = BTreeMap::new();
    let mut previous = None;
    let count = reader.u64()?;
    for _ in 0..count {
        let zone = reader.u64()?;
        if previous.is_some_and(|old| old >= zone) {
            return Err(corrupt_catalog());
        }
        previous = Some(zone);
        let selected = keys.range((zone, 0)..=(zone, u16::MAX)).next().is_some();
        let ambiguous = reader.flag()?;
        if selected {
            *bytes = sum(*bytes, size_of::<ProfileZone>() + 128)?;
            page_memory(*bytes, limits, index, page_records)?;
        }
        let available = limits.bytes.saturating_sub(*bytes).min(
            index
                .memory_bytes
                .saturating_sub(sum(*bytes, sum(mul(page_records, WIDTH)?, IO_MEMORY)?)?),
        );
        let name = reader.string(selected, available)?;
        if let Some(name) = name {
            *bytes = sum(*bytes, name.capacity())?;
            page_memory(*bytes, limits, index, page_records)?;
            zones.insert(
                zone,
                ProfileZone {
                    name,
                    events: BTreeMap::new(),
                },
            );
        }
        let events = reader.u64()?;
        let mut previous_event = None;
        for _ in 0..events {
            let event = reader.u16()?;
            if previous_event.is_some_and(|old| old >= event) {
                return Err(corrupt_catalog());
            }
            previous_event = Some(event);
            let event_ambiguous = reader.flag()?;
            let selected = keys.contains(&(zone, event));
            if selected {
                *bytes = sum(*bytes, 96)?;
                page_memory(*bytes, limits, index, page_records)?;
            }
            let available = limits.bytes.saturating_sub(*bytes).min(
                index
                    .memory_bytes
                    .saturating_sub(sum(*bytes, sum(mul(page_records, WIDTH)?, IO_MEMORY)?)?),
            );
            let name = reader.string(selected, available)?;
            if let Some(name) = name {
                *bytes = sum(*bytes, name.capacity())?;
                page_memory(*bytes, limits, index, page_records)?;
                zones
                    .get_mut(&zone)
                    .ok_or_else(corrupt_catalog)?
                    .events
                    .insert(event, name);
            }
            if selected && (ambiguous || event_ambiguous) {
                for entry in entries
                    .iter_mut()
                    .filter(|e| e.key.zone == zone && e.key.event == event)
                {
                    entry.metadata_ambiguous = true;
                }
            }
        }
        if selected && ambiguous {
            for entry in entries.iter_mut().filter(|e| e.key.zone == zone) {
                entry.metadata_ambiguous = true;
            }
        }
    }
    if reader.read != blob.bytes || reader.hash.finalize().as_bytes() != &blob.hash {
        return Err(corrupt_catalog());
    }
    check(cancelled)?;
    Ok(zones)
}

fn corrupt_catalog() -> Error {
    error("first-start catalog index is corrupt; rebuild it")
}
