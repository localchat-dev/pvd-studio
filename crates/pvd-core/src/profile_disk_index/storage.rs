use super::*;
use std::{
    fs::{self, OpenOptions},
    io::Write,
};

pub(super) const RECORD_BYTES: usize = 64;
pub(super) const HEADER_BYTES: usize = 80;
const PAGE_HEADER: usize = 96;
const FILE_LIMIT: u64 = 16_384;
const MAGIC: &[u8; 8] = b"PVDSIDX1";
pub(super) const META_MAGIC: &[u8; 8] = b"PVDNAME1";

fn page_buffer(records: usize) -> Result<Vec<u8>, Error> {
    let length = mul(records, RECORD_BYTES)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| error("CPU span page allocation failed"))?;
    if bytes.capacity() != length {
        return Err(error(
            "CPU span page allocation exceeds its planned capacity",
        ));
    }
    Ok(bytes)
}

pub(super) struct Lease {
    directory: PathBuf,
    next: u64,
    pub used: u64,
    pub peak: u64,
    limit: u64,
    before_write: Option<WritePolicy>,
}
impl Lease {
    pub fn create(
        directory: &Path,
        source: &Path,
        limit: u64,
        before_write: Option<WritePolicy>,
    ) -> Result<Self, Error> {
        if fs::symlink_metadata(directory).is_ok() {
            return Err(error(
                "CPU span index requires a new workspace; existing directories are not adopted",
            ));
        }
        let parent = directory
            .parent()
            .ok_or_else(|| error("CPU span index workspace has no parent"))?
            .canonicalize()?;
        let name = directory
            .file_name()
            .ok_or_else(|| error("CPU span index workspace has no name"))?;
        let directory = parent.join(name);
        if source.starts_with(&directory) {
            return Err(error("CPU span index workspace aliases its source"));
        }
        fs::create_dir(&directory)?;
        Ok(Self {
            directory,
            next: 0,
            used: 0,
            peak: 0,
            limit,
            before_write,
        })
    }
    pub fn next_id(&self) -> u64 {
        self.next
    }
    pub fn path(&self, id: u64) -> PathBuf {
        self.directory.join(format!("{id:08x}.idx"))
    }
    pub fn create_file(&mut self) -> Result<(u64, File), Error> {
        if self.next >= FILE_LIMIT {
            return Err(error("CPU span index file budget exceeded"));
        }
        let id = self.next;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(self.path(id))?;
        self.next += 1;
        Ok((id, file))
    }
    pub fn write(&mut self, file: &mut File, bytes: &[u8]) -> Result<(), Error> {
        let used = self
            .used
            .checked_add(bytes.len() as u64)
            .filter(|&value| value <= self.limit)
            .ok_or_else(|| error("CPU span index disk budget exceeded"))?;
        self.check_write(bytes.len() as u64)?;
        self.used = used;
        self.peak = self.peak.max(self.used);
        file.write_all(bytes)?;
        Ok(())
    }
    pub(super) fn check_write(&self, additional_bytes: u64) -> Result<(), Error> {
        if let Some(before_write) = &self.before_write {
            before_write(additional_bytes)?;
        }
        Ok(())
    }
    pub fn remove(&mut self, id: u64) -> Result<(), Error> {
        if id >= self.next {
            return Err(corrupt());
        }
        let path = self.path(id);
        let bytes = fs::metadata(&path)?.len();
        fs::remove_file(path)?;
        self.used = self.used.checked_sub(bytes).ok_or_else(corrupt)?;
        Ok(())
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        // Only monotonically registered filenames belong to the lease; unrelated entries survive.
        for id in 0..self.next {
            let _ = fs::remove_file(self.path(id));
        }
        let _ = fs::remove_dir(&self.directory);
    }
}

#[derive(Clone, Debug)]
pub(super) struct Row {
    pub span: ProfileSpan,
    pub ordinal: u64,
}
impl Row {
    pub fn key(&self) -> (u32, u64, u64) {
        (self.span.thread, self.span.start, self.ordinal)
    }
    pub(super) fn encode(&self) -> [u8; RECORD_BYTES] {
        let mut data = [0; RECORD_BYTES];
        data[..4].copy_from_slice(&self.span.thread.to_le_bytes());
        data[4..6].copy_from_slice(&self.span.event.to_le_bytes());
        for (offset, value) in [
            (8, self.span.zone),
            (16, self.span.context),
            (24, self.span.start),
            (32, self.span.end),
            (40, self.span.received_frame as u64),
            (48, self.ordinal),
        ] {
            data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        data
    }
    pub(super) fn decode(data: &[u8]) -> Result<Self, Error> {
        if data.len() != RECORD_BYTES || data[6..8] != [0; 2] || data[56..] != [0; 8] {
            return Err(corrupt());
        }
        let span = ProfileSpan {
            thread: u32at(data, 0),
            event: u16::from_le_bytes(data[4..6].try_into().unwrap()),
            zone: u64at(data, 8),
            context: u64at(data, 16),
            start: u64at(data, 24),
            end: u64at(data, 32),
            received_frame: usize::try_from(u64at(data, 40)).map_err(|_| corrupt())?,
        };
        if span.end < span.start {
            return Err(corrupt());
        }
        Ok(Self {
            span,
            ordinal: u64at(data, 48),
        })
    }
}
fn u32at(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}
fn u64at(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(data[at..at + 8].try_into().unwrap())
}
fn file_length(records: u64, pages: u64) -> Result<u64, Error> {
    records
        .checked_mul(RECORD_BYTES as u64)
        .and_then(|n| {
            pages
                .checked_mul(PAGE_HEADER as u64)
                .and_then(|p| n.checked_add(p))
        })
        .and_then(|n| n.checked_add(HEADER_BYTES as u64))
        .ok_or_else(corrupt)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Run {
    pub id: u64,
    pub records: u64,
    pub pages: u64,
    pub page_records: usize,
    pub bytes: u64,
}
impl Run {
    pub fn open(lease: &Lease, id: u64) -> Result<Self, Error> {
        if id >= lease.next {
            return Err(corrupt());
        }
        let mut file = File::open(lease.path(id))?;
        let mut h = [0; HEADER_BYTES];
        file.read_exact(&mut h).map_err(|_| corrupt())?;
        if &h[..8] != MAGIC
            || u32at(&h, 8) != 1
            || u64at(&h, 32) != HEADER_BYTES as u64
            || blake3::hash(&h[..48]).as_bytes() != &h[48..]
        {
            return Err(corrupt());
        }
        let page_records = u32at(&h, 12) as usize;
        if !(1..=1024).contains(&page_records) {
            return Err(corrupt());
        }
        let records = u64at(&h, 16);
        let pages = u64at(&h, 24);
        let bytes = u64at(&h, 40);
        if pages != records.div_ceil(page_records as u64)
            || bytes != file_length(records, pages)?
            || file.metadata()?.len() != bytes
        {
            return Err(corrupt());
        }
        Ok(Self {
            id,
            records,
            pages,
            page_records,
            bytes,
        })
    }
    fn header(&self) -> [u8; HEADER_BYTES] {
        let mut h = [0; HEADER_BYTES];
        h[..8].copy_from_slice(MAGIC);
        h[8..12].copy_from_slice(&1u32.to_le_bytes());
        h[12..16].copy_from_slice(&(self.page_records as u32).to_le_bytes());
        for (at, value) in [
            (16, self.records),
            (24, self.pages),
            (32, HEADER_BYTES as u64),
            (40, self.bytes),
        ] {
            h[at..at + 8].copy_from_slice(&value.to_le_bytes());
        }
        let hash = blake3::hash(&h[..48]);
        h[48..].copy_from_slice(hash.as_bytes());
        h
    }
    fn page_offset(&self, page: u64) -> Result<u64, Error> {
        if page >= self.pages {
            return Err(corrupt());
        }
        page.checked_mul((PAGE_HEADER + self.page_records * RECORD_BYTES) as u64)
            .and_then(|n| n.checked_add(HEADER_BYTES as u64))
            .ok_or_else(corrupt)
    }
    fn page_count(&self, page: u64) -> Result<usize, Error> {
        let first = page
            .checked_mul(self.page_records as u64)
            .ok_or_else(corrupt)?;
        Ok(self
            .records
            .checked_sub(first)
            .ok_or_else(corrupt)?
            .min(self.page_records as u64) as usize)
    }
}

#[derive(Clone, Copy)]
pub(super) struct PageHeader {
    count: usize,
    pub min_thread: u32,
    pub max_thread: u32,
    pub min_start: u64,
    pub max_end: u64,
    payload: [u8; 32],
}
impl PageHeader {
    fn encode(&self) -> [u8; PAGE_HEADER] {
        let mut h = [0; PAGE_HEADER];
        for (at, value) in [
            (0, self.count as u32),
            (4, self.min_thread),
            (8, self.max_thread),
        ] {
            h[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        h[16..24].copy_from_slice(&self.min_start.to_le_bytes());
        h[24..32].copy_from_slice(&self.max_end.to_le_bytes());
        h[32..64].copy_from_slice(&self.payload);
        let hash = blake3::hash(&h[..64]);
        h[64..].copy_from_slice(hash.as_bytes());
        h
    }
    fn decode(h: &[u8; PAGE_HEADER]) -> Result<Self, Error> {
        if h[12..16] != [0; 4] || blake3::hash(&h[..64]).as_bytes() != &h[64..] {
            return Err(corrupt());
        }
        let result = Self {
            count: u32at(h, 0) as usize,
            min_thread: u32at(h, 4),
            max_thread: u32at(h, 8),
            min_start: u64at(h, 16),
            max_end: u64at(h, 24),
            payload: h[32..64].try_into().unwrap(),
        };
        if result.min_thread > result.max_thread || result.min_start > result.max_end {
            return Err(corrupt());
        }
        Ok(result)
    }
}

pub(super) struct RunWriter {
    file: File,
    run: Run,
    page: Vec<u8>,
    summary: Option<PageHeader>,
    previous: Option<(u32, u64, u64)>,
}
impl RunWriter {
    pub fn new(lease: &mut Lease, page_records: usize) -> Result<Self, Error> {
        let (id, mut file) = lease.create_file()?;
        lease.write(&mut file, &[0; HEADER_BYTES])?;
        Ok(Self {
            file,
            run: Run {
                id,
                records: 0,
                pages: 0,
                page_records,
                bytes: HEADER_BYTES as u64,
            },
            page: page_buffer(page_records)?,
            summary: None,
            previous: None,
        })
    }
    pub fn push(&mut self, lease: &mut Lease, row: &Row) -> Result<(), Error> {
        if self.previous.is_some_and(|key| key >= row.key()) {
            return Err(error("CPU span run is not strictly ordered"));
        }
        self.previous = Some(row.key());
        self.page.extend(row.encode());
        self.summary = Some(match self.summary {
            None => PageHeader {
                count: 1,
                min_thread: row.span.thread,
                max_thread: row.span.thread,
                min_start: row.span.start,
                max_end: row.span.end,
                payload: [0; 32],
            },
            Some(mut h) => {
                h.count += 1;
                h.min_thread = h.min_thread.min(row.span.thread);
                h.max_thread = h.max_thread.max(row.span.thread);
                h.min_start = h.min_start.min(row.span.start);
                h.max_end = h.max_end.max(row.span.end);
                h
            }
        });
        add(&mut self.run.records, 1)?;
        if self.page.len() == self.run.page_records * RECORD_BYTES {
            self.flush(lease)?;
        }
        Ok(())
    }
    fn flush(&mut self, lease: &mut Lease) -> Result<(), Error> {
        let Some(mut header) = self.summary.take() else {
            return Ok(());
        };
        header.payload = *blake3::hash(&self.page).as_bytes();
        lease.write(&mut self.file, &header.encode())?;
        lease.write(&mut self.file, &self.page)?;
        self.page.clear();
        add(&mut self.run.pages, 1)
    }
    pub fn finish(mut self, lease: &mut Lease) -> Result<Run, Error> {
        self.flush(lease)?;
        self.run.bytes = file_length(self.run.records, self.run.pages)?;
        if self.file.stream_position()? != self.run.bytes {
            return Err(corrupt());
        }
        self.file.seek(SeekFrom::Start(0))?;
        lease.check_write(0)?;
        self.file.write_all(&self.run.header())?;
        self.file.sync_all()?;
        Ok(self.run)
    }
}

pub(super) struct RunReader {
    file: File,
    run: Run,
    page: Vec<u8>,
    next_page: u64,
    next_row: usize,
    previous: Option<(u32, u64, u64)>,
}
impl RunReader {
    pub fn new(lease: &Lease, run: Run) -> Result<Self, Error> {
        Ok(Self {
            file: File::open(lease.path(run.id))?,
            run,
            page: page_buffer(run.page_records)?,
            next_page: 0,
            next_row: 0,
            previous: None,
        })
    }
    pub fn page_header(&mut self, page: u64) -> Result<PageHeader, Error> {
        self.file
            .seek(SeekFrom::Start(self.run.page_offset(page)?))?;
        let mut data = [0; PAGE_HEADER];
        self.file.read_exact(&mut data).map_err(|_| corrupt())?;
        let header = PageHeader::decode(&data)?;
        if header.count != self.run.page_count(page)? {
            return Err(corrupt());
        }
        Ok(header)
    }
    pub fn read_page(&mut self, page: u64, header: PageHeader) -> Result<(), Error> {
        self.file.seek(SeekFrom::Start(
            self.run
                .page_offset(page)?
                .checked_add(PAGE_HEADER as u64)
                .ok_or_else(corrupt)?,
        ))?;
        self.page.resize(header.count * RECORD_BYTES, 0);
        self.file
            .read_exact(&mut self.page)
            .map_err(|_| corrupt())?;
        if blake3::hash(&self.page).as_bytes() != &header.payload {
            return Err(corrupt());
        }
        let mut minimum = u64::MAX;
        let mut maximum = 0;
        for i in 0..header.count {
            let row = self.row(i)?;
            minimum = minimum.min(row.span.start);
            maximum = maximum.max(row.span.end);
            if row.span.thread < header.min_thread || row.span.thread > header.max_thread {
                return Err(corrupt());
            }
        }
        if minimum != header.min_start || maximum != header.max_end {
            return Err(corrupt());
        }
        Ok(())
    }
    pub fn page_len(&self) -> usize {
        self.page.len() / RECORD_BYTES
    }
    pub fn row(&self, index: usize) -> Result<Row, Error> {
        let at = index.checked_mul(RECORD_BYTES).ok_or_else(corrupt)?;
        Row::decode(self.page.get(at..at + RECORD_BYTES).ok_or_else(corrupt)?)
    }
    pub fn next(&mut self) -> Result<Option<Row>, Error> {
        if self.next_row == self.page_len() {
            if self.next_page == self.run.pages {
                return Ok(None);
            }
            let header = self.page_header(self.next_page)?;
            self.read_page(self.next_page, header)?;
            self.next_page += 1;
            self.next_row = 0;
        }
        let row = self.row(self.next_row)?;
        self.next_row += 1;
        if self.previous.is_some_and(|key| key >= row.key()) {
            return Err(corrupt());
        }
        self.previous = Some(row.key());
        Ok(Some(row))
    }
}

#[derive(Clone, Copy)]
pub(super) struct Blob {
    id: u64,
    pub(super) bytes: u64,
    pub(super) hash: [u8; 32],
}
struct BlobWriter {
    id: u64,
    file: File,
    bytes: u64,
    hash: blake3::Hasher,
}
impl BlobWriter {
    fn new(lease: &mut Lease) -> Result<Self, Error> {
        let (id, file) = lease.create_file()?;
        Ok(Self {
            id,
            file,
            bytes: 0,
            hash: blake3::Hasher::new(),
        })
    }
    fn write(
        &mut self,
        lease: &mut Lease,
        data: &[u8],
        cancelled: &AtomicBool,
    ) -> Result<(), Error> {
        for part in data.chunks(65536) {
            check(cancelled)?;
            lease.write(&mut self.file, part)?;
            add(&mut self.bytes, part.len() as u64)?;
            self.hash.update(part);
        }
        Ok(())
    }
    fn string(
        &mut self,
        lease: &mut Lease,
        value: &str,
        cancelled: &AtomicBool,
    ) -> Result<(), Error> {
        self.write(lease, &(value.len() as u64).to_le_bytes(), cancelled)?;
        self.write(lease, value.as_bytes(), cancelled)
    }
    fn finish(self) -> Result<Blob, Error> {
        self.file.sync_all()?;
        Ok(Blob {
            id: self.id,
            bytes: self.bytes,
            hash: *self.hash.finalize().as_bytes(),
        })
    }
}

pub(super) fn write_metadata(
    lease: &mut Lease,
    summary: &AnalysisExportSummary,
    cancelled: &AtomicBool,
    callback: &mut impl FnMut(Progress) -> bool,
) -> Result<Blob, Error> {
    let mut out = BlobWriter::new(lease)?;
    out.write(lease, META_MAGIC, cancelled)?;
    out.write(
        lease,
        &(summary.zones.len() as u64).to_le_bytes(),
        cancelled,
    )?;
    for (i, (&id, zone)) in summary.zones.iter().enumerate() {
        progress(
            cancelled,
            callback,
            Phase::Metadata,
            i as u64,
            summary.zones.len() as u64,
        )?;
        out.write(lease, &id.to_le_bytes(), cancelled)?;
        out.write(
            lease,
            &[u8::from(summary.ambiguous_zones.binary_search(&id).is_ok())],
            cancelled,
        )?;
        out.string(lease, &zone.name, cancelled)?;
        out.write(lease, &(zone.events.len() as u64).to_le_bytes(), cancelled)?;
        for (&event, name) in &zone.events {
            out.write(lease, &event.to_le_bytes(), cancelled)?;
            out.write(
                lease,
                &[u8::from(
                    summary.ambiguous_events.binary_search(&(id, event)).is_ok(),
                )],
                cancelled,
            )?;
            out.string(lease, name, cancelled)?;
        }
    }
    out.finish()
}

pub(super) struct BlobReader<'a, F> {
    file: std::io::BufReader<File>,
    blob: Blob,
    pub(super) read: u64,
    pub(super) hash: blake3::Hasher,
    cancelled: &'a AtomicBool,
    callback: &'a mut F,
}
impl<F: FnMut(Progress) -> bool> BlobReader<'_, F> {
    pub(super) fn read(&mut self, bytes: &mut [u8]) -> Result<(), Error> {
        let end = self
            .read
            .checked_add(bytes.len() as u64)
            .filter(|&n| n <= self.blob.bytes)
            .ok_or_else(corrupt)?;
        for part in bytes.chunks_mut(65536) {
            progress(
                self.cancelled,
                self.callback,
                Phase::Metadata,
                self.read,
                self.blob.bytes,
            )?;
            self.file.read_exact(part).map_err(|_| corrupt())?;
            self.hash.update(part);
            self.read += part.len() as u64;
        }
        if self.read != end {
            return Err(corrupt());
        }
        Ok(())
    }
    pub(super) fn u64(&mut self) -> Result<u64, Error> {
        let mut bytes = [0; 8];
        self.read(&mut bytes)?;
        Ok(u64::from_le_bytes(bytes))
    }
    pub(super) fn u16(&mut self) -> Result<u16, Error> {
        let mut bytes = [0; 2];
        self.read(&mut bytes)?;
        Ok(u16::from_le_bytes(bytes))
    }
    pub(super) fn flag(&mut self) -> Result<bool, Error> {
        let mut bytes = [0];
        self.read(&mut bytes)?;
        match bytes[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(corrupt()),
        }
    }
    pub(super) fn string(
        &mut self,
        selected: bool,
        budget: usize,
    ) -> Result<Option<String>, Error> {
        let length = usize::try_from(self.u64()?).map_err(|_| corrupt())?;
        if length as u64 > self.blob.bytes - self.read {
            return Err(corrupt());
        }
        if selected {
            if length > budget {
                return Err(window_limit());
            }
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(length)
                .map_err(|_| window_limit())?;
            if bytes.capacity() > budget {
                return Err(window_limit());
            }
            bytes.resize(length, 0);
            self.read(&mut bytes)?;
            Ok(Some(String::from_utf8(bytes).map_err(|_| corrupt())?))
        } else {
            let mut bytes = [0; 65536];
            let mut left = length;
            while left != 0 {
                let count = left.min(bytes.len());
                self.read(&mut bytes[..count])?;
                left -= count;
            }
            Ok(None)
        }
    }
}

pub(super) fn open_metadata<'a, F: FnMut(Progress) -> bool>(
    lease: &Lease,
    blob: Blob,
    cancelled: &'a AtomicBool,
    callback: &'a mut F,
) -> Result<BlobReader<'a, F>, Error> {
    let mut file = File::open(lease.path(blob.id))?;
    if file.metadata()?.len() != blob.bytes {
        return Err(corrupt());
    }
    let mut hasher = blake3::Hasher::new();
    let mut scratch = [0; 65536];
    let mut verified = 0;
    while verified < blob.bytes {
        progress(cancelled, callback, Phase::Metadata, verified, blob.bytes)?;
        let count = (blob.bytes - verified).min(scratch.len() as u64) as usize;
        file.read_exact(&mut scratch[..count])
            .map_err(|_| corrupt())?;
        hasher.update(&scratch[..count]);
        verified += count as u64;
    }
    if hasher.finalize().as_bytes() != &blob.hash {
        return Err(corrupt());
    }
    file.seek(SeekFrom::Start(0))?;
    let file = std::io::BufReader::with_capacity(65536, file);
    if file.capacity() != 65536 {
        return Err(error(
            "CPU span metadata buffer exceeds its planned capacity",
        ));
    }
    Ok(BlobReader {
        file,
        blob,
        read: 0,
        hash: blake3::Hasher::new(),
        cancelled,
        callback,
    })
}

pub(super) fn read_metadata(
    lease: &Lease,
    blob: Blob,
    selected: &mut Selected,
    cancelled: &AtomicBool,
    callback: &mut impl FnMut(Progress) -> bool,
) -> Result<(), Error> {
    let mut reader = open_metadata(lease, blob, cancelled, callback)?;
    let mut magic = [0; 8];
    reader.read(&mut magic)?;
    if &magic != META_MAGIC {
        return Err(corrupt());
    }
    let count = reader.u64()?;
    let mut previous_zone = None;
    for _ in 0..count {
        let zone = reader.u64()?;
        if previous_zone.is_some_and(|old| old >= zone) {
            return Err(corrupt());
        }
        previous_zone = Some(zone);
        let range = (zone, 0)..=(zone, u16::MAX);
        let keys = selected.names.range(range.clone()).count();
        let ambiguous = reader.flag()?;
        if keys != 0 && ambiguous {
            return Err(error(format!(
                "timeline window has redefined profile metadata at zone {zone}"
            )));
        }
        let retained = sum(
            selected.names_bytes,
            mul(selected.rows.capacity(), size_of::<Row>())?,
        )?;
        let available = selected
            .request
            .limits
            .bytes
            .saturating_sub(selected.names_bytes)
            .min(selected.memory_limit.saturating_sub(retained));
        let name = reader.string(keys != 0, available)?;
        if let Some(name) = name {
            selected.zone_name(zone, &name)?;
        }
        let events = reader.u64()?;
        let mut previous_event = None;
        for _ in 0..events {
            let event = reader.u16()?;
            if previous_event.is_some_and(|old| old >= event) {
                return Err(corrupt());
            }
            previous_event = Some(event);
            let keep = selected.names.contains_key(&(zone, event));
            if reader.flag()? && keep {
                return Err(error(format!(
                    "timeline window has redefined profile metadata at zone {zone}, event {event}"
                )));
            }
            let retained = sum(
                selected.names_bytes,
                mul(selected.rows.capacity(), size_of::<Row>())?,
            )?;
            let available = selected
                .request
                .limits
                .bytes
                .saturating_sub(selected.names_bytes)
                .min(selected.memory_limit.saturating_sub(retained));
            if let Some(name) = reader.string(keep, available)? {
                selected.charge_name(name.capacity())?;
                selected.names.get_mut(&(zone, event)).unwrap().event = Some(name);
            }
        }
    }
    if reader.read != blob.bytes || reader.hash.finalize().as_bytes() != &blob.hash {
        return Err(corrupt());
    }
    Ok(())
}
