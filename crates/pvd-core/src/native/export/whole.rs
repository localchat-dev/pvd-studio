use super::*;
use crate::native::{
    import::backing_range,
    read_source::{EventPage, Source},
    source_identity::CheckedSource,
};

const EVENT_LIMIT: u64 = 20_000_000;
const EVENT_BYTES: usize = MAX_METADATA + super::super::MAX_ITEMS * 4 + 32;

pub(super) fn write<R: Read + Seek>(
    reader: &mut R,
    output: &mut (impl Write + ?Sized),
    progress: &mut dyn FnMut(Progress) -> bool,
) -> Result<Report, Error> {
    let mut checked = CheckedSource::new(reader, |completed, total| {
        progress(Progress {
            phase: Phase::Read,
            completed,
            total,
        })
    })?;
    let length = checked.len();
    let mut report = validate(&mut checked, &mut || {
        progress(Progress {
            phase: Phase::Export,
            completed: 0,
            total: length,
        })
    })?;
    checked.rewind()?;
    let mut bytes = [0; 64 << 10];
    let mut completed = 0;
    let mut digest = blake3::Hasher::new();
    while completed < length {
        check(&mut || {
            progress(Progress {
                phase: Phase::Export,
                completed,
                total: length,
            })
        })?;
        let size = (length - completed).min(bytes.len() as u64) as usize;
        checked.read_exact(&mut bytes[..size])?;
        output.write_all(&bytes[..size])?;
        digest.update(&bytes[..size]);
        completed += size as u64;
    }
    if digest.finalize() != checked.digest() {
        return Err(error("PVD3 whole recording output differs from its source"));
    }
    checked.recheck(|completed, total| {
        progress(Progress {
            phase: Phase::Recheck,
            completed,
            total,
        })
    })?;
    report.source_blake3 = checked.digest();
    Ok(report)
}

#[derive(Default)]
struct Index {
    allocation: usize,
    regions: BTreeMap<u64, u64>,
    saved: BTreeMap<u64, u32>,
}

impl Index {
    fn charge(&mut self, count: usize, size: usize) -> Result<(), Error> {
        metadata_budget(&mut self.allocation, count, size)
    }

    fn region(&mut self, begin: u64, len: u64) -> Result<(), Error> {
        let end = begin
            .checked_add(len)
            .ok_or_else(|| error("backing region overflow"))?;
        if self
            .regions
            .range(..=begin)
            .next_back()
            .is_some_and(|(_, previous)| *previous > begin)
            || self
                .regions
                .range(begin..)
                .next()
                .is_some_and(|(next, _)| *next < end)
        {
            return Err(error("overlapping backing regions"));
        }
        self.charge(1, 64)?;
        self.regions.insert(begin, end);
        Ok(())
    }

    fn saved<R: Read + Seek>(
        &mut self,
        source: &mut Source<R>,
        base: u64,
        metadata: &Metadata,
        entry: (u32, u64, Option<u32>),
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let (id, position, expected_length) = entry;
        if let Some(previous) = self.saved.get(&position) {
            if *previous != id || expected_length.is_some() {
                return Err(error("saved instance directory mismatch or duplicate"));
            }
            return Ok(());
        }
        let mut header = source.cursor(backing_range(base, position, 12)?)?;
        metadata.class(header.u32(proceed)?)?;
        if header.u32(proceed)? != id {
            return Err(error("saved instance ID disagrees with its reference"));
        }
        let body_len = header.u32(proceed)? as usize;
        if body_len > MAX_METADATA
            || expected_length.is_some_and(|len| len as usize != body_len + 12)
        {
            return Err(error("saved instance directory length mismatch"));
        }
        source.check_range(backing_range(base, position, body_len as u64 + 12)?)?;
        self.region(position, body_len as u64 + 12)?;
        self.charge(1, 64)?;
        self.saved.insert(position, id);
        Ok(())
    }
}

type SectionPoint = (u64, u64, u32, u8);

fn section_index(
    communication: &Communication<'_>,
    total: u64,
    index: &mut Index,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<BTreeMap<u64, SectionPoint>, Error> {
    let mut points = BTreeMap::new();
    for section in &communication.sections {
        let mut previous = None;
        for &[begin_ticks, begin_event, end_ticks, end_event] in &section.rows {
            check(proceed)?;
            if begin_event >= end_event
                || begin_ticks > end_ticks
                || end_event >= total
                || previous.is_some_and(|last| begin_event <= last)
            {
                return Err(error("invalid section boundaries"));
            }
            for (ordinal, ticks, begin) in
                [(begin_event, begin_ticks, 1), (end_event, end_ticks, 0)]
            {
                index.charge(1, 96)?;
                if points
                    .insert(ordinal, (section.source, ticks, section.name, begin))
                    .is_some()
                {
                    return Err(error("section event referenced twice"));
                }
            }
            previous = Some(end_event);
        }
    }
    Ok(points)
}

fn aligned(position: u64) -> Result<u64, Error> {
    position
        .checked_div(4096)
        .and_then(|n| n.checked_add(1))
        .and_then(|n| n.checked_mul(4096))
        .ok_or_else(|| error("PVD3 page alignment overflow"))
}

fn directory_count<R: Read + Seek>(
    cursor: &mut crate::native::read_source::Cursor<'_, R>,
    index: &mut Index,
    width: usize,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<usize, Error> {
    let count = cursor.u32(proceed)? as usize;
    if count > super::super::MAX_ITEMS {
        return Err(error("PVD3 directory exceeds item budget"));
    }
    index.charge(count, width)?;
    Ok(count)
}

fn validate<R: Read + Seek>(
    reader: &mut R,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<Report, Error> {
    let mut source = Source::new(reader)?;
    let length = source.len();
    let mut header = source.cursor(0..length)?;
    if header.u32(proceed)? != MAGIC || header.u32(proceed)? != 1 {
        return Err(error("unsupported container version"));
    }
    let object_len = header.u32(proceed)? as usize;
    let comm_len = header.u32(proceed)? as usize;
    if object_len > MAX_METADATA || comm_len > MAX_METADATA {
        return Err(error("container metadata exceeds 64 MiB"));
    }
    let object_end = 16 + object_len as u64;
    let comm_end = object_end + comm_len as u64;
    let object_bytes = source.read_block(16..object_end, MAX_METADATA, proceed)?;
    let metadata = Metadata::parse_with_progress(&object_bytes, proceed)?;
    let mut index = Index::default();
    pointer_layouts(&object_bytes, &mut index.allocation)?;
    drop(object_bytes);
    let comm_bytes = source.read_block(object_end..comm_end, MAX_METADATA, proceed)?;
    let communication =
        Communication::parse(&comm_bytes, &metadata, &mut index.allocation, proceed)?;
    if communication.encode(proceed)? != comm_bytes {
        return Err(error("communication reconstruction is not byte-identical"));
    }
    let frames = communication
        .sections
        .iter()
        .filter(|section| {
            metadata
                .strings
                .get(&section.name)
                .is_some_and(|name| name == "frame")
        })
        .try_fold(0usize, |count, section| {
            count
                .checked_add(section.rows.len())
                .ok_or_else(|| error("frame count overflow"))
        })?;
    let mut required_instances = BTreeSet::new();
    for id in communication.cameras.iter().map(|(_, id)| *id).chain(
        communication
            .memory_instances
            .into_iter()
            .filter(|id| *id != u32::MAX),
    ) {
        if !required_instances.contains(&id) {
            index.charge(1, 64)?;
            required_instances.insert(id);
        }
    }
    let mut directory = source.cursor(aligned(comm_end)?..length)?;
    let total = directory.u64(proceed)?;
    if total > EVENT_LIMIT {
        return Err(error("PVD3 history exceeds 20 million events"));
    }
    let mut points = section_index(&communication, total, &mut index, proceed)?;
    let page_count = directory_count(&mut directory, &mut index, 32, proceed)?;
    let mut pages = Vec::with_capacity(page_count);
    for _ in 0..page_count {
        pages.push(Page {
            first: directory.u64(proceed)?,
            position: directory.u64(proceed)?,
            count: directory.u32(proceed)?,
            len: directory.u32(proceed)?,
        });
    }
    let snapshot_count = directory_count(&mut directory, &mut index, 32, proceed)?;
    let mut snapshots = Vec::with_capacity(snapshot_count);
    for _ in 0..snapshot_count {
        snapshots.push(Snapshot {
            event: directory.u64(proceed)?,
            position: directory.u64(proceed)?,
            len: directory.u32(proceed)?,
            reserved: directory.u32(proceed)?,
        });
    }
    let saved_count = directory_count(&mut directory, &mut index, 32, proceed)?;
    let mut saved = Vec::with_capacity(saved_count);
    let mut saved_ids = BTreeSet::new();
    for _ in 0..saved_count {
        let id = directory.u32(proceed)?;
        index.charge(1, 64)?;
        if !saved_ids.insert(id) {
            return Err(error("saved instance directory mismatch or duplicate"));
        }
        saved.push((id, directory.u64(proceed)?, Some(directory.u32(proceed)?)));
    }
    let base = aligned(directory.position())?;
    source.check_range(base..length)?;
    for entry in saved {
        index.saved(&mut source, base, &metadata, entry, proceed)?;
        required_instances.remove(&entry.0);
    }
    for snapshot in snapshots {
        if snapshot.event > total {
            return Err(error("snapshot event exceeds history"));
        }
        index.region(snapshot.position, u64::from(snapshot.len))?;
        let bytes = source.read_block(
            backing_range(base, snapshot.position, u64::from(snapshot.len))?,
            MAX_METADATA,
            proceed,
        )?;
        let mut data = Reader::new(&bytes);
        let count = count(&mut data, 64, &mut index.allocation)?;
        let mut ids = BTreeSet::new();
        for _ in 0..count {
            check(proceed)?;
            let id = data.u32()?;
            let position = data.u64()?;
            if !ids.insert(id) {
                return Err(error("snapshot contains a duplicate instance"));
            }
            index.saved(&mut source, base, &metadata, (id, position, None), proceed)?;
            required_instances.remove(&id);
        }
        data.end()?;
    }
    let mut ordinal = 0;
    let mut previous_end = 0;
    for page in pages {
        check(proceed)?;
        if page.first != ordinal || page.count == 0 || page.position < previous_end {
            return Err(error("invalid page order"));
        }
        previous_end = page
            .position
            .checked_add(u64::from(page.len))
            .ok_or_else(|| error("page range overflow"))?;
        index.region(page.position, u64::from(page.len))?;
        let offsets = EventPage::read(
            &mut source,
            backing_range(base, page.position, u64::from(page.len))?,
            page.count,
            MAX_METADATA.saturating_sub(index.allocation),
            proceed,
        )?;
        let page_index_bytes = offsets
            .event_count()
            .checked_add(1)
            .and_then(|count| count.checked_mul(4))
            .ok_or_else(|| error("page offset allocation overflow"))?;
        index.charge(page_index_bytes, 1)?;
        for row in 0..offsets.event_count() {
            check(proceed)?;
            if ordinal >= total {
                return Err(error("total event count mismatch"));
            }
            let bytes = source.read_block(offsets.event_range(row)?, EVENT_BYTES, proceed)?;
            let mut event_allocation = index.allocation;
            let event = HistoryEvent::parse(&bytes, ordinal, &metadata, &mut event_allocation)?;
            let mut encoded = Vec::new();
            event.encode(&mut encoded)?;
            if encoded != bytes {
                return Err(error("event reconstruction is not byte-identical"));
            }
            match event {
                HistoryEvent::Create(id, _) => {
                    required_instances.remove(&id);
                }
                HistoryEvent::Saved { id, position } => {
                    index.saved(&mut source, base, &metadata, (id, position, None), proceed)?;
                    required_instances.remove(&id);
                }
                HistoryEvent::Section {
                    source,
                    ticks,
                    name,
                    begin,
                    ..
                } if points.remove(&ordinal) != Some((source, ticks, name, begin)) => {
                    return Err(error("section table disagrees with history event"));
                }
                _ => {}
            }
            ordinal += 1;
        }
        drop(offsets);
        index.allocation -= page_index_bytes;
    }
    if ordinal != total {
        return Err(error("total event count mismatch"));
    }
    if !points.is_empty() {
        return Err(error("section table disagrees with history event"));
    }
    if !required_instances.is_empty() {
        return Err(error("communication catalog refers to a missing instance"));
    }
    source.check_length()?;
    let length = usize::try_from(length)?;
    Ok(Report {
        source_bytes: length,
        source_blake3: blake3::hash(&[]),
        source_frames: frames,
        output_bytes: length,
        events: usize::try_from(total)?,
        frames,
        saved_instances: index.saved.len(),
    })
}

#[cfg(test)]
mod tests;
