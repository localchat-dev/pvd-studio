use crate::{
    Error,
    analysis::{Analysis, ProfileSpan},
    analysis_export::{AnalysisExportSummary, ProfileObserver, scan_profile},
    error,
    profile_curves::SourceIdentity,
    profile_timeline::{
        ClockRatio, EventNames, GlobalPairingStats, MAX_WINDOW_BYTES, MAX_WINDOW_SPANS, TickRange,
        WindowReport, WindowRequest,
    },
};
use pvd_protocol::Header;
use std::{
    collections::{BTreeMap, BinaryHeap},
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub mod catalog;
mod storage;
use storage::*;
#[cfg(test)]
mod tests;

const NAME_BYTES: usize = size_of::<((u64, u16), EventNames)>() + 128;
const IO_MEMORY: usize = (192 << 10) + 4096;

#[derive(Clone, Copy, Debug)]
pub struct IndexLimits {
    /// Working allocations owned by the index, separate from the shared SDK decoder's budget.
    pub memory_bytes: usize,
    pub disk_bytes: u64,
    pub run_bytes: usize,
    pub page_records: usize,
    pub merge_fan_in: usize,
}

impl Default for IndexLimits {
    fn default() -> Self {
        Self {
            memory_bytes: 64 << 20,
            disk_bytes: 1 << 30,
            run_bytes: 8 << 20,
            page_records: 1024,
            merge_fan_in: 8,
        }
    }
}

pub type WritePolicy = Arc<dyn Fn(u64) -> std::io::Result<()> + Send + Sync>;

#[derive(Clone, Default)]
pub struct IndexOptions {
    pub limits: IndexLimits,
    /// Allows a shared disk reserve in addition to the index quota. Header rewrites pass zero.
    pub before_write: Option<WritePolicy>,
}

impl From<IndexLimits> for IndexOptions {
    fn from(limits: IndexLimits) -> Self {
        Self {
            limits,
            before_write: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Decode,
    Metadata,
    Merge,
    VerifySource,
    Query,
}
#[derive(Clone, Copy, Debug)]
pub struct Progress {
    pub phase: Phase,
    pub completed: u64,
    pub total: u64,
}

#[derive(Clone, Debug)]
pub struct IndexInfo {
    pub identity: SourceIdentity,
    pub clock: ClockRatio,
    pub completed_tick_range: Option<TickRange>,
    pub global_pairing: GlobalPairingStats,
    pub legacy_cuda_buffers: u64,
    pub disk_bytes: u64,
    pub peak_disk_bytes: u64,
    pub initial_runs: u64,
    pub merge_passes: usize,
}

pub struct CpuSpanIndex {
    source: PathBuf,
    lease: Lease,
    data: Run,
    metadata: Blob,
    limits: IndexLimits,
    info: IndexInfo,
}

fn check(cancelled: &AtomicBool) -> Result<(), Error> {
    if cancelled.load(Ordering::Relaxed) {
        Err(error("CPU span index operation cancelled"))
    } else {
        Ok(())
    }
}
fn progress(
    cancelled: &AtomicBool,
    callback: &mut impl FnMut(Progress) -> bool,
    phase: Phase,
    completed: u64,
    total: u64,
) -> Result<(), Error> {
    check(cancelled)?;
    if !callback(Progress {
        phase,
        completed,
        total,
    }) {
        return Err(error("CPU span index operation cancelled"));
    }
    check(cancelled)
}
fn add(value: &mut u64, extra: u64) -> Result<(), Error> {
    *value = value
        .checked_add(extra)
        .ok_or_else(|| error("CPU span index count overflow"))?;
    Ok(())
}
fn memory(bytes: usize, limit: usize) -> Result<(), Error> {
    if bytes > limit {
        Err(error("CPU span index working memory budget exceeded"))
    } else {
        Ok(())
    }
}
fn sum(a: usize, b: usize) -> Result<usize, Error> {
    a.checked_add(b)
        .ok_or_else(|| error("CPU span index size overflow"))
}
fn mul(a: usize, b: usize) -> Result<usize, Error> {
    a.checked_mul(b)
        .ok_or_else(|| error("CPU span index size overflow"))
}

/// The new workspace belongs exclusively to this handle; no existing directory is adopted.
pub fn build_index(
    source: &Path,
    workspace: &Path,
    expected: &SourceIdentity,
    options: impl Into<IndexOptions>,
    cancelled: &AtomicBool,
    mut callback: impl FnMut(Progress) -> bool,
) -> Result<CpuSpanIndex, Error> {
    check(cancelled)?;
    let IndexOptions {
        limits,
        before_write,
    } = options.into();
    if limits.run_bytes < size_of::<Row>()
        || limits.page_records == 0
        || limits.page_records > 1024
        || !(2..=16).contains(&limits.merge_fan_in)
        || limits.disk_bytes < HEADER_BYTES as u64
    {
        return Err(error("invalid CPU span index limits"));
    }
    let page_memory = mul(limits.page_records, RECORD_BYTES)?;
    memory(sum(page_memory, IO_MEMORY)?, limits.memory_bytes)?;
    memory(sum(limits.run_bytes, page_memory)?, limits.memory_bytes)?;
    memory(
        sum(
            mul(limits.merge_fan_in + 1, page_memory)?,
            mul(limits.merge_fan_in, 1024)?,
        )?,
        limits.memory_bytes,
    )?;
    let source = source.canonicalize()?;
    let mut input = File::open(&source)?;
    if !input.metadata()?.is_file() {
        return Err(error("CPU span index source must be a regular file"));
    }
    let mut lease = Lease::create(workspace, &source, limits.disk_bytes, before_write)?;
    let run_capacity = limits.run_bytes / size_of::<Row>();
    let mut rows = Vec::new();
    rows.try_reserve_exact(run_capacity)
        .map_err(|_| error("CPU span run allocation failed"))?;
    memory(
        sum(mul(rows.capacity(), size_of::<Row>())?, page_memory)?,
        limits.memory_bytes,
    )?;
    let mut builder = Builder {
        lease: &mut lease,
        rows,
        run_capacity,
        page_records: limits.page_records,
        runs: 0,
        stats: GlobalPairingStats::default(),
        range: None,
        clock: None,
        cancelled,
    };
    let report = scan_profile(&mut input, &mut builder, &mut |p| {
        !cancelled.load(Ordering::Relaxed)
            && callback(Progress {
                phase: Phase::Decode,
                completed: p.bytes_read,
                total: p.total_bytes,
            })
    })?;
    if SourceIdentity::from(&report) != *expected {
        return Err(error("CPU span index source identity changed"));
    }
    builder.flush()?;
    if builder.runs == 0 {
        builder.write_empty()?;
    }
    let runs = builder.runs;
    let mut info = IndexInfo {
        identity: expected.clone(),
        clock: builder.clock.unwrap(),
        completed_tick_range: builder.range,
        global_pairing: builder.stats,
        legacy_cuda_buffers: report.summary.legacy_cuda_buffers,
        disk_bytes: 0,
        peak_disk_bytes: 0,
        initial_runs: runs,
        merge_passes: 0,
    };
    drop(builder);
    let metadata = write_metadata(&mut lease, &report.summary, cancelled, &mut callback)?;
    drop(report);
    let mut first = 0;
    let mut count = runs;
    while count > 1 {
        let next_first = lease.next_id();
        let mut next_count = 0;
        for start in (0..count).step_by(limits.merge_fan_in) {
            check(cancelled)?;
            let end = (start + limits.merge_fan_in as u64).min(count);
            merge(
                &mut lease,
                first + start..first + end,
                limits.page_records,
                cancelled,
                &mut callback,
            )?;
            next_count += 1;
        }
        first = next_first;
        count = next_count;
        info.merge_passes += 1;
    }
    let data = Run::open(&lease, first)?;
    if data.records != info.global_pairing.complete_spans {
        return Err(error("CPU span index lost records during merge"));
    }
    info.disk_bytes = lease.used;
    info.peak_disk_bytes = lease.peak;
    progress(
        cancelled,
        &mut callback,
        Phase::Merge,
        data.records,
        data.records,
    )?;
    Ok(CpuSpanIndex {
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
    run_capacity: usize,
    page_records: usize,
    runs: u64,
    stats: GlobalPairingStats,
    range: Option<TickRange>,
    clock: Option<ClockRatio>,
    cancelled: &'a AtomicBool,
}
impl Builder<'_> {
    fn flush(&mut self) -> Result<(), Error> {
        if self.rows.is_empty() {
            return Ok(());
        }
        self.rows.sort_unstable_by_key(Row::key);
        let mut writer = RunWriter::new(self.lease, self.page_records)?;
        for row in &self.rows {
            check(self.cancelled)?;
            writer.push(self.lease, row)?;
        }
        writer.finish(self.lease)?;
        self.rows.clear();
        add(&mut self.runs, 1)
    }
    fn write_empty(&mut self) -> Result<(), Error> {
        RunWriter::new(self.lease, self.page_records)?.finish(self.lease)?;
        self.runs = 1;
        Ok(())
    }
}
impl ProfileObserver for Builder<'_> {
    fn batch(&mut self, analysis: &Analysis) -> Result<(), Error> {
        for span in &analysis.spans {
            check(self.cancelled)?;
            if span.end < span.start {
                return Err(error("CPU span index contains reversed span"));
            }
            let ordinal = self.stats.complete_spans;
            add(&mut self.stats.complete_spans, 1)?;
            self.range = Some(match self.range {
                None => TickRange {
                    start: span.start,
                    end: span.end,
                },
                Some(old) => TickRange {
                    start: old.start.min(span.start),
                    end: old.end.max(span.end),
                },
            });
            self.rows.push(Row {
                span: span.clone(),
                ordinal,
            });
            if self.rows.len() == self.run_capacity {
                self.flush()?;
            }
        }
        Ok(())
    }
    fn finish(&mut self, analysis: &Analysis, header: &Header) -> Result<(), Error> {
        if header.timestamp_numerator == 0 || header.timestamp_denominator == 0 {
            return Err(error("CPU span index clock ratio is invalid"));
        }
        self.clock = Some(ClockRatio {
            numerator: header.timestamp_numerator,
            denominator: header.timestamp_denominator,
        });
        for (_, count) in analysis.open_events() {
            check(self.cancelled)?;
            add(&mut self.stats.open_starts, count as u64)?;
        }
        for &count in analysis.unmatched_stop_events.values() {
            check(self.cancelled)?;
            add(&mut self.stats.unmatched_stops, count)?;
        }
        Ok(())
    }
}

fn merge(
    lease: &mut Lease,
    ids: std::ops::Range<u64>,
    page_records: usize,
    cancelled: &AtomicBool,
    callback: &mut impl FnMut(Progress) -> bool,
) -> Result<(), Error> {
    let mut readers = Vec::new();
    let mut total = 0;
    for id in ids.clone() {
        let run = Run::open(lease, id)?;
        add(&mut total, run.records)?;
        readers.push(RunReader::new(lease, run)?);
    }
    let mut heap = BinaryHeap::new();
    for (i, reader) in readers.iter_mut().enumerate() {
        if let Some(row) = reader.next()? {
            heap.push(HeapRow { row, reader: i });
        }
    }
    let mut writer = RunWriter::new(lease, page_records)?;
    let mut completed = 0;
    while let Some(item) = heap.pop() {
        if completed % page_records as u64 == 0 {
            progress(cancelled, callback, Phase::Merge, completed, total)?;
        }
        writer.push(lease, &item.row)?;
        completed += 1;
        if let Some(row) = readers[item.reader].next()? {
            heap.push(HeapRow {
                row,
                reader: item.reader,
            });
        }
    }
    if completed != total {
        return Err(error("CPU span merge is incomplete"));
    }
    writer.finish(lease)?;
    drop(readers);
    for id in ids {
        lease.remove(id)?;
    }
    Ok(())
}

struct HeapRow {
    row: Row,
    reader: usize,
}
impl PartialEq for HeapRow {
    fn eq(&self, other: &Self) -> bool {
        self.row.key() == other.row.key()
    }
}
impl Eq for HeapRow {}
impl PartialOrd for HeapRow {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for HeapRow {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other.row.key().cmp(&self.row.key())
    }
}

impl CpuSpanIndex {
    pub fn info(&self) -> &IndexInfo {
        &self.info
    }

    pub fn window(
        &self,
        request: WindowRequest,
        cancelled: &AtomicBool,
        mut callback: impl FnMut(Progress) -> bool,
    ) -> Result<WindowReport, Error> {
        if request.ticks.start > request.ticks.end
            || !(1..=MAX_WINDOW_SPANS).contains(&request.limits.spans)
            || !(1..=MAX_WINDOW_BYTES).contains(&request.limits.bytes)
        {
            return Err(error("invalid CPU span index window or limits"));
        }
        verify_source(&self.source, &self.info.identity, cancelled, &mut callback)?;
        let working_memory = self
            .limits
            .memory_bytes
            .checked_sub(sum(
                mul(self.limits.page_records, RECORD_BYTES)?,
                IO_MEMORY,
            )?)
            .ok_or_else(|| error("CPU span index working memory budget exceeded"))?;
        let mut selected = Selected::new(request, working_memory);
        let run = Run::open(&self.lease, self.data.id)?;
        if run != self.data {
            return Err(corrupt());
        }
        let mut reader = RunReader::new(&self.lease, run)?;
        for page in 0..run.pages {
            progress(cancelled, &mut callback, Phase::Query, page, run.pages)?;
            let header = reader.page_header(page)?;
            if request
                .thread
                .is_some_and(|thread| thread < header.min_thread || thread > header.max_thread)
                || header.min_start > request.ticks.end
                || header.max_end < request.ticks.start
            {
                continue;
            }
            reader.read_page(page, header)?;
            for index in 0..reader.page_len() {
                let row = reader.row(index)?;
                if row.ordinal >= self.info.global_pairing.complete_spans
                    || row.span.received_frame > self.info.identity.frames
                {
                    return Err(corrupt());
                }
                if request
                    .thread
                    .is_some_and(|thread| row.span.thread != thread)
                    || request
                        .context
                        .is_some_and(|context| row.span.context != context)
                    || row.span.start > request.ticks.end
                    || row.span.end < request.ticks.start
                {
                    continue;
                }
                selected.push(row)?;
            }
        }
        read_metadata(
            &self.lease,
            self.metadata,
            &mut selected,
            cancelled,
            &mut callback,
        )?;
        let contents = selected.finish()?;
        progress(cancelled, &mut callback, Phase::Query, run.pages, run.pages)?;
        Ok(WindowReport {
            identity: self.info.identity.clone(),
            clock: self.info.clock,
            request,
            spans: contents.spans,
            names: contents.names,
            global_pairing: self.info.global_pairing,
            legacy_cuda_buffers: self.info.legacy_cuda_buffers,
            estimated_index_bytes: contents.bytes,
        })
    }
}

fn verify_source(
    path: &Path,
    expected: &SourceIdentity,
    cancelled: &AtomicBool,
    callback: &mut impl FnMut(Progress) -> bool,
) -> Result<(), Error> {
    let mut file = File::open(path)?;
    if file.metadata()?.len() != expected.bytes {
        return Err(error("CPU span index source identity changed"));
    }
    let mut bytes = [0; 65536];
    let mut hasher = blake3::Hasher::new();
    let mut completed = 0;
    loop {
        progress(
            cancelled,
            callback,
            Phase::VerifySource,
            completed,
            expected.bytes,
        )?;
        let count = match file.read(&mut bytes) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            break;
        }
        add(&mut completed, count as u64)?;
        if completed > expected.bytes {
            return Err(error("CPU span index source identity changed"));
        }
        hasher.update(&bytes[..count]);
    }
    if completed != expected.bytes || hasher.finalize().as_bytes() != &expected.blake3 {
        return Err(error("CPU span index source identity changed"));
    }
    Ok(())
}

struct Selected {
    request: WindowRequest,
    rows: Vec<Row>,
    names: BTreeMap<(u64, u16), EventNames>,
    names_bytes: usize,
    memory_limit: usize,
}
struct WindowContents {
    spans: Vec<ProfileSpan>,
    names: BTreeMap<(u64, u16), EventNames>,
    bytes: usize,
}
impl Selected {
    fn new(request: WindowRequest, memory_limit: usize) -> Self {
        Self {
            request,
            rows: Vec::new(),
            names: BTreeMap::new(),
            names_bytes: 0,
            memory_limit,
        }
    }
    fn charge_name(&mut self, extra: usize) -> Result<(), Error> {
        self.names_bytes = checked_names(
            self.names_bytes,
            extra,
            self.rows.capacity(),
            self.request,
            self.memory_limit,
            0,
        )?;
        Ok(())
    }
    fn zone_name(&mut self, zone: u64, name: &String) -> Result<(), Error> {
        for (_, names) in self.names.range_mut((zone, 0)..=(zone, u16::MAX)) {
            checked_names(
                self.names_bytes,
                name.len(),
                self.rows.capacity(),
                self.request,
                self.memory_limit,
                name.capacity(),
            )?;
            let mut copy = String::new();
            copy.try_reserve_exact(name.len())
                .map_err(|_| window_limit())?;
            let bytes = checked_names(
                self.names_bytes,
                copy.capacity(),
                self.rows.capacity(),
                self.request,
                self.memory_limit,
                name.capacity(),
            )?;
            copy.push_str(name);
            names.zone = Some(copy);
            self.names_bytes = bytes;
        }
        Ok(())
    }
    fn push(&mut self, row: Row) -> Result<(), Error> {
        if self.rows.len() == self.request.limits.spans {
            return Err(window_limit());
        }
        let key = (row.span.zone, row.span.event);
        if !self.names.contains_key(&key) {
            self.charge_name(NAME_BYTES)?;
            self.names.insert(key, EventNames::default());
        }
        if self.rows.len() == self.rows.capacity() {
            let old = self.rows.capacity();
            let next = old.saturating_mul(2).max(1).min(self.request.limits.spans);
            if sum(self.names_bytes, mul(next, size_of::<ProfileSpan>())?)?
                > self.request.limits.bytes
            {
                return Err(window_limit());
            }
            memory(
                sum(self.names_bytes, mul(sum(old, next)?, size_of::<Row>())?)?,
                self.memory_limit,
            )?;
            self.rows
                .try_reserve_exact(next - old)
                .map_err(|_| window_limit())?;
            memory(
                sum(
                    self.names_bytes,
                    mul(sum(old, self.rows.capacity())?, size_of::<Row>())?,
                )?,
                self.memory_limit,
            )?;
            if sum(
                self.names_bytes,
                mul(self.rows.capacity(), size_of::<ProfileSpan>())?,
            )? > self.request.limits.bytes
            {
                return Err(window_limit());
            }
        }
        self.rows.push(row);
        Ok(())
    }
    fn finish(mut self) -> Result<WindowContents, Error> {
        self.rows.sort_unstable_by_key(|row| row.ordinal);
        if self
            .rows
            .windows(2)
            .any(|pair| pair[0].ordinal >= pair[1].ordinal)
        {
            return Err(corrupt());
        }
        let mut spans = Vec::new();
        memory(
            sum(
                self.names_bytes,
                sum(
                    mul(self.rows.capacity(), size_of::<Row>())?,
                    mul(self.rows.len(), size_of::<ProfileSpan>())?,
                )?,
            )?,
            self.memory_limit,
        )?;
        spans
            .try_reserve_exact(self.rows.len())
            .map_err(|_| window_limit())?;
        memory(
            sum(
                self.names_bytes,
                sum(
                    mul(self.rows.capacity(), size_of::<Row>())?,
                    mul(spans.capacity(), size_of::<ProfileSpan>())?,
                )?,
            )?,
            self.memory_limit,
        )?;
        for row in self.rows {
            spans.push(row.span);
        }
        let bytes = sum(
            self.names_bytes,
            mul(spans.capacity(), size_of::<ProfileSpan>())?,
        )?;
        if bytes > self.request.limits.bytes {
            return Err(window_limit());
        }
        Ok(WindowContents {
            spans,
            names: self.names,
            bytes,
        })
    }
}
fn checked_names(
    bytes: usize,
    extra: usize,
    row_capacity: usize,
    request: WindowRequest,
    memory_limit: usize,
    temporary: usize,
) -> Result<usize, Error> {
    let names = sum(bytes, extra)?;
    if sum(names, mul(row_capacity, size_of::<ProfileSpan>())?)? > request.limits.bytes {
        return Err(window_limit());
    }
    memory(
        sum(names, sum(mul(row_capacity, size_of::<Row>())?, temporary)?)?,
        memory_limit,
    )?;
    Ok(names)
}
fn window_limit() -> Error {
    error("CPU span window exceeds its budget; narrow the time range or filter thread/context")
}
fn corrupt() -> Error {
    error("CPU span index is corrupt or incompatible; rebuild it")
}
