use super::*;
use std::io::Write;

pub(super) const WIDTH: usize = 64;
pub(super) const HEADER: usize = 128;
const PAGE_HEADER: usize = 48;
const MAGIC: &[u8; 8] = b"PVDCAT01";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Order {
    Key = 1,
    Start = 2,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Row {
    pub key: ExactKey,
    pub first: Option<u64>,
    pub complete: u64,
    pub open: u64,
    pub unmatched: u64,
}

impl Row {
    pub fn position(&self) -> FirstStartPosition {
        FirstStartPosition {
            first_start: self.first,
            key: self.key,
        }
    }
    pub fn compare(&self, other: &Self, order: Order) -> std::cmp::Ordering {
        match order {
            Order::Key => self.key.cmp(&other.key),
            Order::Start => self.position().cmp(&other.position()),
        }
    }
    pub fn add(&mut self, other: &Self) -> Result<(), Error> {
        if self.key != other.key {
            return Err(corrupt_catalog());
        }
        self.first = match (self.first, other.first) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        add(&mut self.complete, other.complete)?;
        add(&mut self.open, other.open)?;
        add(&mut self.unmatched, other.unmatched)
    }
    fn encode(&self) -> [u8; WIDTH] {
        let mut b = [0; WIDTH];
        for (at, value) in [
            (0, self.key.zone),
            (8, self.key.context),
            (16, self.first.unwrap_or(0)),
            (24, self.complete),
            (32, self.open),
            (40, self.unmatched),
        ] {
            b[at..at + 8].copy_from_slice(&value.to_le_bytes());
        }
        b[48..52].copy_from_slice(&self.key.thread.to_le_bytes());
        b[52..54].copy_from_slice(&self.key.event.to_le_bytes());
        b[54] = u8::from(self.first.is_some());
        b
    }
    fn decode(b: &[u8]) -> Result<Self, Error> {
        if b.len() != WIDTH || b[54] > 1 || b[55..] != [0; 9] {
            return Err(corrupt_catalog());
        }
        let n = |at| u64::from_le_bytes(b[at..at + 8].try_into().unwrap());
        let row = Self {
            key: ExactKey {
                zone: n(0),
                context: n(8),
                thread: u32::from_le_bytes(b[48..52].try_into().unwrap()),
                event: u16::from_le_bytes(b[52..54].try_into().unwrap()),
            },
            first: (b[54] == 1).then(|| n(16)),
            complete: n(24),
            open: n(32),
            unmatched: n(40),
        };
        if (b[54] == 0 && n(16) != 0)
            || (row.complete == 0 && row.open == 0 && row.unmatched == 0)
            || (row.first.is_some() != (row.complete != 0 || row.open != 0))
        {
            return Err(corrupt_catalog());
        }
        Ok(row)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Run {
    pub id: u64,
    identity: BuildIdentity,
    pub records: u64,
    pages: u64,
    pub page_records: usize,
    pub order: Order,
}
impl Run {
    fn header(&self) -> [u8; HEADER] {
        let mut b = [0; HEADER];
        b[..8].copy_from_slice(MAGIC);
        b[8..12].copy_from_slice(&(self.order as u32).to_le_bytes());
        b[12..16].copy_from_slice(&(WIDTH as u32).to_le_bytes());
        for (at, value) in [
            (16, self.page_records as u64),
            (24, self.records),
            (32, self.pages),
            (40, self.id),
        ] {
            b[at..at + 8].copy_from_slice(&value.to_le_bytes());
        }
        b[48..80].copy_from_slice(&self.identity.source_hash);
        b[80..96].copy_from_slice(&self.identity.session);
        let hash = blake3::hash(&b[..96]);
        b[96..].copy_from_slice(hash.as_bytes());
        b
    }
    fn len(&self) -> Result<u64, Error> {
        (HEADER as u64)
            .checked_add(
                self.pages
                    .checked_mul(PAGE_HEADER as u64)
                    .ok_or_else(corrupt_catalog)?,
            )
            .and_then(|n| n.checked_add(self.records.checked_mul(WIDTH as u64)?))
            .ok_or_else(corrupt_catalog)
    }
    pub fn open(
        lease: &Lease,
        id: u64,
        order: Order,
        identity: BuildIdentity,
    ) -> Result<Self, Error> {
        let mut file = File::open(lease.path(id))?;
        let mut b = [0; HEADER];
        file.read_exact(&mut b).map_err(|_| corrupt_catalog())?;
        if &b[..8] != MAGIC
            || b[8..12] != (order as u32).to_le_bytes()
            || b[12..16] != (WIDTH as u32).to_le_bytes()
            || b[40..48] != id.to_le_bytes()
            || b[48..80] != identity.source_hash
            || b[80..96] != identity.session
            || blake3::hash(&b[..96]).as_bytes() != &b[96..]
        {
            return Err(corrupt_catalog());
        }
        let n = |at| u64::from_le_bytes(b[at..at + 8].try_into().unwrap());
        let page_records = usize::try_from(n(16)).map_err(|_| corrupt_catalog())?;
        if !(1..=1024).contains(&page_records) {
            return Err(corrupt_catalog());
        }
        let run = Self {
            id,
            identity,
            records: n(24),
            pages: n(32),
            page_records,
            order,
        };
        if run.pages != run.records.div_ceil(page_records as u64)
            || file.metadata()?.len() != run.len()?
        {
            return Err(corrupt_catalog());
        }
        Ok(run)
    }
}

fn buffer(records: usize) -> Result<Vec<u8>, Error> {
    let bytes = mul(records, WIDTH)?;
    let mut result = Vec::new();
    result.try_reserve_exact(bytes)?;
    if result.capacity() != bytes {
        return Err(error("catalog page capacity exceeds its budget"));
    }
    Ok(result)
}

fn page_hash(run: &Run, page: u64, bytes: &[u8]) -> blake3::Hash {
    let mut hash = blake3::Hasher::new();
    hash.update(MAGIC);
    hash.update(&run.identity.source_hash);
    hash.update(&run.identity.session);
    hash.update(&run.id.to_le_bytes());
    hash.update(&(run.order as u32).to_le_bytes());
    hash.update(&page.to_le_bytes());
    hash.update(&((bytes.len() / WIDTH) as u64).to_le_bytes());
    hash.update(bytes);
    hash.finalize()
}

pub(super) struct Writer {
    file: File,
    run: Run,
    bytes: Vec<u8>,
    previous: Option<Row>,
}
impl Writer {
    pub fn new(
        lease: &mut Lease,
        page_records: usize,
        order: Order,
        identity: BuildIdentity,
    ) -> Result<Self, Error> {
        let bytes = buffer(page_records)?;
        let (id, mut file) = lease.create_file()?;
        lease.write(&mut file, &[0; HEADER])?;
        Ok(Self {
            file,
            run: Run {
                id,
                identity,
                records: 0,
                pages: 0,
                page_records,
                order,
            },
            bytes,
            previous: None,
        })
    }
    pub fn push(&mut self, lease: &mut Lease, row: &Row) -> Result<(), Error> {
        if self
            .previous
            .as_ref()
            .is_some_and(|old| old.compare(row, self.run.order) != std::cmp::Ordering::Less)
        {
            return Err(corrupt_catalog());
        }
        self.bytes.extend_from_slice(&row.encode());
        add(&mut self.run.records, 1)?;
        self.previous = Some(row.clone());
        if self.bytes.len() == self.run.page_records * WIDTH {
            self.flush(lease)?;
        }
        Ok(())
    }
    fn flush(&mut self, lease: &mut Lease) -> Result<(), Error> {
        if self.bytes.is_empty() {
            return Ok(());
        }
        let mut header = [0; PAGE_HEADER];
        header[..8].copy_from_slice(&((self.bytes.len() / WIDTH) as u64).to_le_bytes());
        header[8..16].copy_from_slice(&self.run.pages.to_le_bytes());
        header[16..].copy_from_slice(page_hash(&self.run, self.run.pages, &self.bytes).as_bytes());
        lease.write(&mut self.file, &header)?;
        lease.write(&mut self.file, &self.bytes)?;
        self.bytes.clear();
        add(&mut self.run.pages, 1)
    }
    pub fn finish(mut self, lease: &mut Lease) -> Result<Run, Error> {
        self.flush(lease)?;
        self.file.flush()?;
        self.file.seek(SeekFrom::Start(0))?;
        lease.check_write(0)?;
        self.file.write_all(&self.run.header())?;
        self.file.flush()?;
        if self.file.metadata()?.len() != self.run.len()? {
            return Err(corrupt_catalog());
        }
        Ok(self.run)
    }
}

pub(super) struct Reader {
    file: File,
    pub run: Run,
    bytes: Vec<u8>,
    page: Option<u64>,
    cursor: u64,
    previous: Option<Row>,
}
impl Reader {
    pub fn new(lease: &Lease, run: Run) -> Result<Self, Error> {
        if Run::open(lease, run.id, run.order, run.identity)? != run {
            return Err(corrupt_catalog());
        }
        let bytes = buffer(run.page_records)?;
        Ok(Self {
            file: File::open(lease.path(run.id))?,
            run,
            bytes,
            page: None,
            cursor: 0,
            previous: None,
        })
    }
    pub fn at(&mut self, index: u64) -> Result<Row, Error> {
        if index >= self.run.records {
            return Err(corrupt_catalog());
        }
        let page = index / self.run.page_records as u64;
        if self.page != Some(page) {
            let stride = (PAGE_HEADER as u64)
                .checked_add(
                    (self.run.page_records as u64)
                        .checked_mul(WIDTH as u64)
                        .ok_or_else(corrupt_catalog)?,
                )
                .ok_or_else(corrupt_catalog)?;
            let offset = (HEADER as u64)
                .checked_add(page.checked_mul(stride).ok_or_else(corrupt_catalog)?)
                .ok_or_else(corrupt_catalog)?;
            self.file.seek(SeekFrom::Start(offset))?;
            let mut header = [0; PAGE_HEADER];
            self.file
                .read_exact(&mut header)
                .map_err(|_| corrupt_catalog())?;
            let count = (self.run.records - page * self.run.page_records as u64)
                .min(self.run.page_records as u64) as usize;
            if header[..8] != (count as u64).to_le_bytes() || header[8..16] != page.to_le_bytes() {
                return Err(corrupt_catalog());
            }
            self.bytes.resize(count * WIDTH, 0);
            self.file
                .read_exact(&mut self.bytes)
                .map_err(|_| corrupt_catalog())?;
            if page_hash(&self.run, page, &self.bytes).as_bytes() != &header[16..] {
                return Err(corrupt_catalog());
            }
            let mut previous = None;
            for bytes in self.bytes.chunks_exact(WIDTH) {
                let row = Row::decode(bytes)?;
                if previous.as_ref().is_some_and(|old: &Row| {
                    old.compare(&row, self.run.order) != std::cmp::Ordering::Less
                }) {
                    return Err(corrupt_catalog());
                }
                previous = Some(row);
            }
            self.page = Some(page);
        }
        let at = (index % self.run.page_records as u64) as usize * WIDTH;
        Row::decode(&self.bytes[at..at + WIDTH])
    }
    pub fn next(&mut self) -> Result<Option<Row>, Error> {
        if self.cursor == self.run.records {
            return Ok(None);
        }
        let row = self.at(self.cursor)?;
        if self
            .previous
            .as_ref()
            .is_some_and(|old| old.compare(&row, self.run.order) != std::cmp::Ordering::Less)
        {
            return Err(corrupt_catalog());
        }
        self.previous = Some(row.clone());
        self.cursor += 1;
        Ok(Some(row))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row() -> Row {
        Row {
            key: ExactKey {
                zone: u64::MAX,
                event: u16::MAX,
                thread: u32::MAX,
                context: u64::MAX,
            },
            first: Some(u64::MAX),
            complete: 1,
            open: 0,
            unmatched: 0,
        }
    }
    #[test]
    fn fixed_records_preserve_extremes_and_reject_invalid_flags_and_empty_counts() {
        let original = row();
        let bytes = original.encode();
        assert_eq!(Row::decode(&bytes).unwrap(), original);
        for at in [54, 55, 56, 63] {
            let mut b = bytes;
            b[at] = 2;
            assert!(Row::decode(&b).is_err());
        }
        let mut b = bytes;
        b[24..32].fill(0);
        assert!(Row::decode(&b).is_err());
        let mut b = bytes;
        b[54] = 0;
        assert!(Row::decode(&b).is_err());
        assert!(Row::decode(&bytes[..63]).is_err());
        let unknown = Row {
            first: None,
            complete: 0,
            unmatched: 1,
            ..original.clone()
        };
        assert_eq!(Row::decode(&unknown.encode()).unwrap(), unknown);
        assert!(original.position() < unknown.position());
    }
    #[test]
    fn aggregation_counts_and_file_offsets_are_checked() {
        for field in [0, 1, 2] {
            let mut a = row();
            let mut b = row();
            match field {
                0 => {
                    a.complete = u64::MAX;
                }
                1 => {
                    a.open = u64::MAX;
                    b.open = 1;
                }
                _ => {
                    a.unmatched = u64::MAX;
                    b.unmatched = 1;
                }
            }
            assert!(a.add(&b).is_err());
        }
        let run = Run {
            id: 0,
            identity: BuildIdentity::new([1; 32]).unwrap(),
            records: u64::MAX,
            pages: u64::MAX,
            page_records: 1024,
            order: Order::Key,
        };
        assert!(run.len().is_err());
        let mut a = row();
        let b = Row {
            first: Some(0),
            ..row()
        };
        a.add(&b).unwrap();
        assert_eq!(a.first, Some(0));
    }

    #[test]
    fn complete_valid_pages_cannot_move_within_a_run_or_between_runs() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../output")
            .join(format!("catalog-page-binding-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source");
        std::fs::write(&source, b"preserved source").unwrap();
        let work = root.join("index");
        let mut lease =
            Lease::create(&work, &source.canonicalize().unwrap(), 1 << 20, None).unwrap();
        let identity = BuildIdentity::new([1; 32]).unwrap();
        let mut make_run = || {
            let mut writer = Writer::new(&mut lease, 1, Order::Start, identity).unwrap();
            for first in [10, 20] {
                writer
                    .push(
                        &mut lease,
                        &Row {
                            first: Some(first),
                            ..row()
                        },
                    )
                    .unwrap();
            }
            writer.finish(&mut lease).unwrap()
        };
        let first = make_run();
        let second = make_run();
        let path = lease.path(first.id);
        let original = std::fs::read(&path).unwrap();
        let other = std::fs::read(lease.path(second.id)).unwrap();
        let stride = PAGE_HEADER + WIDTH;
        let mut swapped = original.clone();
        swapped[HEADER..HEADER + stride]
            .copy_from_slice(&original[HEADER + stride..HEADER + 2 * stride]);
        swapped[HEADER + stride..].copy_from_slice(&original[HEADER..HEADER + stride]);
        std::fs::write(&path, swapped).unwrap();
        assert!(Reader::new(&lease, first.clone()).unwrap().at(0).is_err());
        let mut cross = original.clone();
        cross[HEADER..HEADER + stride].copy_from_slice(&other[HEADER..HEADER + stride]);
        std::fs::write(&path, cross).unwrap();
        assert!(Reader::new(&lease, first.clone()).unwrap().at(0).is_err());
        std::fs::write(&path, &original).unwrap();
        assert_eq!(
            Reader::new(&lease, first).unwrap().at(1).unwrap().first,
            Some(20)
        );
        drop(lease);
        assert!(!work.exists());
        assert_eq!(std::fs::read(&source).unwrap(), b"preserved source");
        std::fs::remove_file(source).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn same_source_builds_cannot_exchange_same_id_intermediate_pages() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../output")
            .join(format!("catalog-build-binding-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source");
        std::fs::write(&source, b"preserved source").unwrap();
        let source = source.canonicalize().unwrap();
        let source_hash = *blake3::hash(b"preserved source").as_bytes();
        let mut first = Lease::create(&root.join("first"), &source, 1 << 20, None).unwrap();
        let mut second = Lease::create(&root.join("second"), &source, 1 << 20, None).unwrap();
        let make_run = |lease: &mut Lease, complete| {
            let identity = BuildIdentity::new(source_hash).unwrap();
            let mut writer = Writer::new(lease, 2, Order::Key, identity).unwrap();
            for event in [1, 2] {
                writer
                    .push(
                        lease,
                        &Row {
                            key: ExactKey { event, ..row().key },
                            complete,
                            ..row()
                        },
                    )
                    .unwrap();
            }
            writer.finish(lease).unwrap()
        };
        let a = make_run(&mut first, 2);
        let b = make_run(&mut second, 3);
        assert_eq!(a.id, b.id);
        assert_eq!(a.identity.source_hash, b.identity.source_hash);
        assert_ne!(a.identity.session, b.identity.session);
        let path = first.path(a.id);
        let original = std::fs::read(&path).unwrap();
        let other = std::fs::read(second.path(b.id)).unwrap();
        let mut mixed = original.clone();
        mixed[HEADER..].copy_from_slice(&other[HEADER..]);
        std::fs::write(&path, mixed).unwrap();
        assert!(Reader::new(&first, a.clone()).unwrap().at(0).is_err());
        std::fs::write(&path, other).unwrap();
        assert!(Reader::new(&first, a.clone()).is_err());
        std::fs::write(&path, original).unwrap();
        assert_eq!(Reader::new(&first, a).unwrap().at(0).unwrap().complete, 2);
        drop((first, second));
        std::fs::remove_file(source).unwrap();
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        std::fs::remove_dir(root).unwrap();
    }
}
