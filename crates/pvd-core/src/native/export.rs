use super::{
    Error, MAGIC, MAX_METADATA, Metadata, Reader, cancelled, error, metadata_budget, next_page,
};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    io::{Read, Seek, SeekFrom, Write},
};

mod camera;
mod frames;
mod history;
mod initial;
mod memory_middle;
mod middle;
mod origin_prefix;
mod origin_shift;
mod prefix;
mod render_context;
mod whole;

#[cfg(test)]
#[path = "export/initial/memory/tests/fixture.rs"]
mod memory_fixture;

const OUTPUT_LIMIT: usize = 512 << 20;

fn output_size(base: usize, length: usize) -> Result<usize, Error> {
    base.checked_add(length)
        .filter(|n| *n <= OUTPUT_LIMIT)
        .ok_or_else(|| error("container output exceeds 512 MiB budget"))
}

#[derive(Debug)]
pub struct Report {
    pub source_bytes: usize,
    pub source_blake3: blake3::Hash,
    pub source_frames: usize,
    pub output_bytes: usize,
    pub events: usize,
    pub frames: usize,
    pub saved_instances: usize,
}

#[derive(Clone, Copy, Debug)]
pub enum Selection {
    Whole,
    Frames { first: usize, last: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Read,
    Export,
    Recheck,
}

#[derive(Clone, Copy, Debug)]
pub struct Progress {
    pub phase: Phase,
    pub completed: u64,
    pub total: u64,
}

/// The caller must publish a staged sink only after successful source revalidation.
pub fn write_reader(
    source: &mut (impl Read + Seek),
    selection: Selection,
    output: &mut (impl Write + ?Sized),
    mut progress: impl FnMut(Progress) -> bool,
) -> Result<Report, Error> {
    if matches!(selection, Selection::Whole) {
        return whole::write(source, output, &mut progress);
    }
    let length = source.seek(SeekFrom::End(0))?;
    if length > OUTPUT_LIMIT as u64 {
        return Err(error("PVD3 export input exceeds 512 MiB"));
    }
    source.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    let mut chunk = [0; 64 << 10];
    loop {
        check(&mut || {
            progress(Progress {
                phase: Phase::Read,
                completed: bytes.len() as u64,
                total: length,
            })
        })?;
        let read = source.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        output_size(bytes.len(), read)?;
        bytes.extend_from_slice(&chunk[..read]);
    }
    if bytes.len() as u64 != length {
        return Err(error("PVD3 source length changed during export"));
    }
    let mut proceed = || {
        progress(Progress {
            phase: Phase::Export,
            completed: 0,
            total: 0,
        })
    };
    let report = match selection {
        Selection::Whole => rewrite(&bytes, output, &mut proceed),
        Selection::Frames { first, last } => {
            trim_scene_frames(&bytes, first, last, output, &mut proceed)
        }
    }?;
    drop(bytes);
    source.seek(SeekFrom::Start(0))?;
    let mut digest = blake3::Hasher::new();
    let mut completed = 0u64;
    loop {
        check(&mut || {
            progress(Progress {
                phase: Phase::Recheck,
                completed,
                total: length,
            })
        })?;
        let read = source.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        completed = completed
            .checked_add(read as u64)
            .filter(|value| *value <= length)
            .ok_or_else(|| error("PVD3 source length changed during export"))?;
        digest.update(&chunk[..read]);
    }
    if completed != length || digest.finalize() != report.source_blake3 {
        return Err(error("PVD3 source changed during export"));
    }
    Ok(report)
}

#[derive(Clone, Debug)]
struct Instance {
    id: u32,
    source: u64,
    flags: [u8; 10],
}

#[derive(Clone, Debug)]
struct Section {
    source: u64,
    name: u32,
    rows: Vec<[u64; 4]>,
}

#[derive(Clone, Debug)]
struct Zone<'a> {
    source: u64,
    saved_word: u64,
    name: &'a [u8],
    id: u32,
    events: Vec<(&'a [u8], u16, u8)>,
}

#[derive(Clone, Debug)]
struct Communication<'a> {
    clock: [u64; 2],
    instances: Vec<Instance>,
    reverse: Vec<(u64, u32)>,
    cameras: Vec<(u32, u32)>,
    sections: Vec<Section>,
    zones: Vec<Zone<'a>>,
    memory_instances: [u32; 2],
    memory_names: Vec<(&'a [u8], &'a [u8], u32, u32)>,
}

#[derive(Clone, Debug)]
enum HistoryEvent<'a> {
    Create(u32, u32),
    Destroy(u32),
    Set {
        kind: u32,
        id: u32,
        property: u32,
        data: Cow<'a, [u8]>,
        undo: Vec<u32>,
    },
    Array {
        kind: u32,
        id: u32,
        property: u32,
        index: u32,
        data: Option<&'a [u8]>,
        undo: Vec<u32>,
    },
    Section {
        class: u32,
        source: u64,
        ticks: u64,
        name: u32,
        begin: u8,
        padding: [u8; 3],
    },
    Marker,
    Saved {
        id: u32,
        position: u64,
    },
    OriginShift(origin_shift::Event<'a>),
}

#[derive(Clone, Debug)]
struct Page {
    first: u64,
    position: u64,
    count: u32,
    len: u32,
}

#[derive(Clone, Debug)]
struct Snapshot {
    event: u64,
    position: u64,
    len: u32,
    reserved: u32,
}

#[derive(Clone, Debug)]
struct Saved<'a> {
    id: u32,
    position: u64,
    class: u32,
    body: Cow<'a, [u8]>,
    deleted: bool,
}

struct Layout<'a> {
    source: &'a [u8],
    object_bytes: &'a [u8],
    metadata: Metadata,
    pointer_layouts: Vec<Vec<(u32, usize)>>,
    communication: Communication<'a>,
    metadata_padding: &'a [u8],
    history_padding: &'a [u8],
    pages: Vec<Page>,
    snapshots: Vec<Snapshot>,
    snapshot_bytes: Vec<Cow<'a, [u8]>>,
    saved: Vec<Saved<'a>>,
    events: Vec<HistoryEvent<'a>>,
    backing: &'a [u8],
    gaps: Vec<(usize, &'a [u8])>,
}

fn check(proceed: &mut dyn FnMut() -> bool) -> Result<(), Error> {
    if proceed() { Ok(()) } else { Err(cancelled()) }
}

fn string_bytes<'a>(r: &mut Reader<'a>) -> Result<&'a [u8], Error> {
    let bytes = r.blob()?;
    if !bytes.is_empty()
        && (bytes.last() != Some(&0)
            || bytes.len() > 32 << 10
            || std::str::from_utf8(&bytes[..bytes.len() - 1]).is_err())
    {
        return Err(error("invalid container string"));
    }
    Ok(bytes)
}

fn count(r: &mut Reader<'_>, width: usize, budget: &mut usize) -> Result<usize, Error> {
    let n = r.count()?;
    metadata_budget(budget, n, width)?;
    Ok(n)
}

fn range(data: &[u8], position: u64, len: usize) -> Result<&[u8], Error> {
    let start = usize::try_from(position).map_err(|_| error("backing position overflow"))?;
    data.get(
        start
            ..start
                .checked_add(len)
                .ok_or_else(|| error("backing range overflow"))?,
    )
    .ok_or_else(|| error("backing range exceeds file"))
}

fn u32_len(n: usize) -> Result<u32, Error> {
    u32::try_from(n).map_err(|_| error("serialized container length exceeds u32"))
}

fn w32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn w64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn blob(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), Error> {
    w32(out, u32_len(bytes.len())?);
    out.extend_from_slice(bytes);
    Ok(())
}
fn words(out: &mut Vec<u8>, values: &[u32]) -> Result<(), Error> {
    w32(out, u32_len(values.len())?);
    for &value in values {
        w32(out, value);
    }
    Ok(())
}

fn pointer_layouts(bytes: &[u8], allocation: &mut usize) -> Result<Vec<Vec<(u32, usize)>>, Error> {
    let mut r = Reader::new(bytes);
    r.take(8)?;
    let strings = r.count()?;
    r.take(4)?;
    for _ in 0..strings {
        r.u32()?;
        r.blob()?;
    }
    for _ in 0..r.count()? {
        if r.marker()? {
            r.take(44)?;
            r.words(8)?;
            r.u32()?;
        }
    }
    let mut layouts = Vec::new();
    for _ in 0..count(&mut r, 24, allocation)? {
        let mut offsets = Vec::new();
        if r.marker()? {
            r.take(50)?;
            r.words(4)?;
            for _ in 0..count(&mut r, 16, allocation)? {
                offsets.push((r.u32()?, r.u32()? as usize));
            }
            r.words(8)?;
        }
        layouts.push(offsets);
    }
    Ok(layouts)
}

impl<'a> Communication<'a> {
    fn parse(
        bytes: &'a [u8],
        metadata: &Metadata,
        budget: &mut usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Self, Error> {
        let mut r = Reader::new(bytes);
        if r.u32()? != 1 {
            return Err(error("unsupported communication version"));
        }
        let clock = [r.u64()?, r.u64()?];
        if clock.contains(&0) {
            return Err(error("invalid clock conversion"));
        }
        let mut instances = Vec::new();
        let mut ids = BTreeMap::new();
        for _ in 0..count(&mut r, 512, budget)? {
            check(proceed)?;
            let id = r.u32()?;
            let source = r.u64()?;
            if r.u32()? != id || ids.insert(id, source).is_some() {
                return Err(error("duplicate or inconsistent instance metadata"));
            }
            let flags: [u8; 10] = r.take(10)?.try_into()?;
            if flags[8] > 1 || flags[9] > 1 {
                return Err(error("invalid instance display boolean"));
            }
            instances.push(Instance { id, source, flags });
        }
        let mut reverse = Vec::new();
        let mut reverse_keys = BTreeSet::new();
        for _ in 0..count(&mut r, 32, budget)? {
            let source = r.u64()?;
            let id = r.u32()?;
            if ids.get(&id) != Some(&source) || !reverse_keys.insert(source) {
                return Err(error("invalid source reverse map"));
            }
            reverse.push((source, id));
        }
        let mut cameras = Vec::new();
        let mut camera_names = BTreeSet::new();
        for _ in 0..count(&mut r, 16, budget)? {
            let name = r.u32()?;
            let id = r.u32()?;
            if !metadata.strings.contains_key(&name) || !camera_names.insert(name) {
                return Err(error("invalid camera dictionary reference"));
            }
            cameras.push((name, id));
        }
        let mut sections = Vec::new();
        let mut section_keys = BTreeSet::new();
        for _ in 0..count(&mut r, 64, budget)? {
            let source = r.u64()?;
            let name = r.u32()?;
            if !metadata.strings.contains_key(&name)
                || !ids.values().any(|v| *v == source)
                || !section_keys.insert((source, name))
            {
                return Err(error("invalid section identity"));
            }
            let mut rows = Vec::new();
            for _ in 0..count(&mut r, 32, budget)? {
                rows.push([r.u64()?, r.u64()?, r.u64()?, r.u64()?]);
            }
            sections.push(Section { source, name, rows });
        }
        let mut zones = Vec::new();
        let mut zone_ids = BTreeSet::new();
        for _ in 0..count(&mut r, 512, budget)? {
            check(proceed)?;
            let source = r.u64()?;
            let saved_word = r.u64()?;
            let name = string_bytes(&mut r)?;
            let id = r.u32()?;
            if ids.get(&id) != Some(&source) || !zone_ids.insert(id) {
                return Err(error("invalid profile zone reference"));
            }
            let mut events = Vec::new();
            for _ in 0..count(&mut r, 32, budget)? {
                let name = string_bytes(&mut r)?;
                let event = r.u16()?;
                let enabled = r.take(1)?[0];
                if enabled > 1 {
                    return Err(error("invalid profile enabled flag"));
                }
                events.push((name, event, enabled));
            }
            zones.push(Zone {
                source,
                saved_word,
                name,
                id,
                events,
            });
        }
        let memory_instances = [r.u32()?, r.u32()?];
        let mut memory_names = Vec::new();
        for _ in 0..count(&mut r, 48, budget)? {
            check(proceed)?;
            memory_names.push((
                string_bytes(&mut r)?,
                string_bytes(&mut r)?,
                r.u32()?,
                r.u32()?,
            ));
        }
        r.end()?;
        Ok(Self {
            clock,
            instances,
            reverse,
            cameras,
            sections,
            zones,
            memory_instances,
            memory_names,
        })
    }

    fn encode(&self, proceed: &mut dyn FnMut() -> bool) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        w32(&mut out, 1);
        for value in self.clock {
            w64(&mut out, value);
        }
        w32(&mut out, u32_len(self.instances.len())?);
        for v in &self.instances {
            check(proceed)?;
            w32(&mut out, v.id);
            w64(&mut out, v.source);
            w32(&mut out, v.id);
            out.extend_from_slice(&v.flags);
        }
        w32(&mut out, u32_len(self.reverse.len())?);
        for &(source, id) in &self.reverse {
            check(proceed)?;
            w64(&mut out, source);
            w32(&mut out, id);
        }
        w32(&mut out, u32_len(self.cameras.len())?);
        for &(name, id) in &self.cameras {
            check(proceed)?;
            w32(&mut out, name);
            w32(&mut out, id);
        }
        w32(&mut out, u32_len(self.sections.len())?);
        for section in &self.sections {
            check(proceed)?;
            w64(&mut out, section.source);
            w32(&mut out, section.name);
            w32(&mut out, u32_len(section.rows.len())?);
            for row in &section.rows {
                check(proceed)?;
                for &v in row {
                    w64(&mut out, v);
                }
            }
        }
        w32(&mut out, u32_len(self.zones.len())?);
        for zone in &self.zones {
            check(proceed)?;
            w64(&mut out, zone.source);
            w64(&mut out, zone.saved_word);
            blob(&mut out, zone.name)?;
            w32(&mut out, zone.id);
            w32(&mut out, u32_len(zone.events.len())?);
            for &(name, id, enabled) in &zone.events {
                check(proceed)?;
                blob(&mut out, name)?;
                out.extend_from_slice(&id.to_le_bytes());
                out.push(enabled);
            }
        }
        for id in self.memory_instances {
            w32(&mut out, id);
        }
        w32(&mut out, u32_len(self.memory_names.len())?);
        for &(kind, file, line, index) in &self.memory_names {
            check(proceed)?;
            blob(&mut out, kind)?;
            blob(&mut out, file)?;
            w32(&mut out, line);
            w32(&mut out, index);
        }
        Ok(out)
    }
}

impl<'a> HistoryEvent<'a> {
    fn parse(
        bytes: &'a [u8],
        ordinal: u64,
        metadata: &Metadata,
        budget: &mut usize,
    ) -> Result<Self, Error> {
        let mut r = Reader::new(bytes);
        fn undo(r: &mut Reader<'_>, ordinal: u64, budget: &mut usize) -> Result<Vec<u32>, Error> {
            let mut values = Vec::new();
            for _ in 0..count(r, 4, budget)? {
                let value = r.u32()?;
                if u64::from(value) > ordinal {
                    return Err(error("undo reference exceeds current event"));
                }
                values.push(value);
            }
            Ok(values)
        }
        let kind = r.u32()?;
        let value = match kind {
            1 => {
                let id = r.u32()?;
                let class = r.u32()?;
                metadata.class(class)?;
                Self::Create(id, class)
            }
            2 => Self::Destroy(r.u32()?),
            3 | 4 => {
                let id = r.u32()?;
                let property = r.u32()?;
                let data = r.blob()?;
                if kind == 3 {
                    metadata.property(property)?;
                } else if metadata
                    .messages
                    .get(property as usize)
                    .and_then(Option::as_ref)
                    .is_none()
                {
                    return Err(error("undefined property message"));
                }
                Self::Set {
                    kind,
                    id,
                    property,
                    data: data.into(),
                    undo: undo(&mut r, ordinal, budget)?,
                }
            }
            5..=7 => {
                let id = r.u32()?;
                let property = r.u32()?;
                let index = r.u32()?;
                if metadata.property(property)?.kind != 2 {
                    return Err(error("array event targets scalar"));
                }
                let data = if kind == 6 { None } else { Some(r.blob()?) };
                let undo = if kind == 5 {
                    Vec::new()
                } else {
                    undo(&mut r, ordinal, budget)?
                };
                Self::Array {
                    kind,
                    id,
                    property,
                    index,
                    data,
                    undo,
                }
            }
            8 => {
                let class = r.u32()?;
                let cl = metadata.class(class)?;
                if metadata.strings.get(&cl.name.namespace).map(String::as_str)
                    != Some("_debugger_")
                    || metadata.strings.get(&cl.name.name).map(String::as_str)
                        != Some("SectionEventData")
                {
                    return Err(error("unsupported user event"));
                }
                let mut data = Reader::new(r.blob()?);
                let source = data.u64()?;
                let ticks = data.u64()?;
                let name = data.u32()?;
                let begin = data.take(1)?[0];
                let padding = data.take(3)?.try_into()?;
                data.end()?;
                if begin > 1 || !metadata.strings.contains_key(&name) {
                    return Err(error("invalid section event"));
                }
                Self::Section {
                    class,
                    source,
                    ticks,
                    name,
                    begin,
                    padding,
                }
            }
            9 => Self::Marker,
            10 => Self::Saved {
                id: r.u32()?,
                position: r.u64()?,
            },
            11 => Self::OriginShift(origin_shift::Event::parse(&mut r)?),
            _ => return Err(error(format!("unsupported container event {kind}"))),
        };
        r.end()?;
        Ok(value)
    }

    fn encode(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        match self {
            Self::Create(id, class) => {
                w32(out, 1);
                w32(out, *id);
                w32(out, *class);
            }
            Self::Destroy(id) => {
                w32(out, 2);
                w32(out, *id);
            }
            Self::Set {
                kind,
                id,
                property,
                data,
                undo,
            } => {
                w32(out, *kind);
                w32(out, *id);
                w32(out, *property);
                blob(out, data)?;
                words(out, undo)?;
            }
            Self::Array {
                kind,
                id,
                property,
                index,
                data,
                undo,
            } => {
                w32(out, *kind);
                w32(out, *id);
                w32(out, *property);
                w32(out, *index);
                if let Some(data) = data {
                    blob(out, data)?;
                }
                if *kind != 5 {
                    words(out, undo)?;
                }
            }
            Self::Section {
                class,
                source,
                ticks,
                name,
                begin,
                padding,
            } => {
                w32(out, 8);
                w32(out, *class);
                w32(out, 24);
                w64(out, *source);
                w64(out, *ticks);
                w32(out, *name);
                out.push(*begin);
                out.extend_from_slice(padding);
            }
            Self::Marker => w32(out, 9),
            Self::Saved { id, position } => {
                w32(out, 10);
                w32(out, *id);
                w64(out, *position);
            }
            Self::OriginShift(event) => event.encode(out)?,
        }
        Ok(())
    }
}

fn place(
    out: &mut [u8],
    base: usize,
    position: u64,
    bytes: &[u8],
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    let start = base
        .checked_add(usize::try_from(position)?)
        .ok_or_else(|| error("backing position overflow"))?;
    let end = start
        .checked_add(bytes.len())
        .ok_or_else(|| error("backing length overflow"))?;
    let target = out
        .get_mut(start..end)
        .ok_or_else(|| error("serialized backing exceeds layout"))?;
    for (dst, src) in target.chunks_mut(64 << 10).zip(bytes.chunks(64 << 10)) {
        check(proceed)?;
        dst.copy_from_slice(src);
    }
    Ok(())
}

fn encode_page(
    events: &[HistoryEvent<'_>],
    proceed: &mut dyn FnMut() -> bool,
) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    let mut payload = Vec::new();
    w32(&mut out, u32_len(events.len())?);
    for event in events {
        check(proceed)?;
        w32(&mut out, u32_len(payload.len())?);
        event.encode(&mut payload)?;
    }
    out.extend_from_slice(&payload);
    Ok(out)
}

fn saved_record<'a>(
    backing: &'a [u8],
    metadata: &Metadata,
    id: u32,
    position: u64,
    length: Option<u32>,
    deleted: bool,
) -> Result<Saved<'a>, Error> {
    let mut header = Reader::new(range(backing, position, 12)?);
    let class = header.u32()?;
    metadata
        .class(class)
        .map_err(|cause| error(format!("invalid saved instance class: {cause}")))?;
    if header.u32()? != id {
        return Err(error("saved instance ID disagrees with its reference"));
    }
    let body_len = header.u32()? as usize;
    if body_len > MAX_METADATA || length.is_some_and(|length| length as usize != body_len + 12) {
        return Err(error("saved instance directory length mismatch"));
    }
    let bytes = range(backing, position, body_len + 12)?;
    Ok(Saved {
        id,
        position,
        class,
        body: Cow::Borrowed(&bytes[12..]),
        deleted,
    })
}

impl<'a> Layout<'a> {
    fn parse(source: &'a [u8], proceed: &mut dyn FnMut() -> bool) -> Result<Self, Error> {
        check(proceed)?;
        if source.len() > 512 << 20 {
            return Err(error(
                "experimental container rewrite is limited to 512 MiB",
            ));
        }
        let mut r = Reader::new(source);
        if r.u32()? != MAGIC || r.u32()? != 1 {
            return Err(error("unsupported container version"));
        }
        let object_len = r.u32()? as usize;
        let comm_len = r.u32()? as usize;
        if object_len > MAX_METADATA || comm_len > MAX_METADATA {
            return Err(error("container metadata exceeds 64 MiB"));
        }
        let object_bytes = r.take(object_len)?;
        let metadata = Metadata::parse(object_bytes)?;
        let mut allocation = 0;
        let pointer_layouts = pointer_layouts(object_bytes, &mut allocation)?;
        let communication =
            Communication::parse(r.take(comm_len)?, &metadata, &mut allocation, proceed)?;
        let metadata_padding = r.take(next_page(r.pos)? - r.pos)?;
        let total = r.u64()?;
        if total > (MAX_METADATA / 128) as u64 {
            return Err(error("container event index exceeds experimental budget"));
        }
        let mut pages = Vec::new();
        for _ in 0..count(&mut r, 32, &mut allocation)? {
            pages.push(Page {
                first: r.u64()?,
                position: r.u64()?,
                count: r.u32()?,
                len: r.u32()?,
            });
        }
        let mut snapshots = Vec::new();
        for _ in 0..count(&mut r, 32, &mut allocation)? {
            snapshots.push(Snapshot {
                event: r.u64()?,
                position: r.u64()?,
                len: r.u32()?,
                reserved: r.u32()?,
            });
        }
        let mut saved_directory = Vec::new();
        for _ in 0..count(&mut r, 32, &mut allocation)? {
            saved_directory.push((r.u32()?, r.u64()?, r.u32()?));
        }
        let history_padding = r.take(next_page(r.pos)? - r.pos)?;
        let backing = r.take(source.len() - r.pos)?;
        let mut regions = Vec::new();
        let mut snapshot_bytes = Vec::new();
        for snapshot in &snapshots {
            if snapshot.event > total {
                return Err(error("snapshot event exceeds history"));
            }
            let bytes = range(backing, snapshot.position, snapshot.len as usize)?;
            let mut s = Reader::new(bytes);
            let n = count(&mut s, 16, &mut allocation)?;
            if s.take(n * 12)?.len() != n * 12 {
                return Err(error("snapshot directory truncated"));
            }
            s.end()?;
            snapshot_bytes.push(Cow::Borrowed(bytes));
            regions.push((snapshot.position as usize, snapshot.len as usize));
        }
        let mut saved = Vec::new();
        let mut saved_positions = BTreeMap::new();
        let mut saved_ids = BTreeSet::new();
        for (id, position, len) in saved_directory {
            check(proceed)?;
            if !saved_ids.insert(id) || saved_positions.insert(position, id).is_some() {
                return Err(error("saved instance directory mismatch or duplicate"));
            }
            saved.push(saved_record(
                backing,
                &metadata,
                id,
                position,
                Some(len),
                true,
            )?);
            regions.push((position as usize, len as usize));
        }
        for bytes in &snapshot_bytes {
            let mut s = Reader::new(bytes);
            let n = count(&mut s, 16, &mut allocation)?;
            let mut ids = BTreeSet::new();
            for _ in 0..n {
                let id = s.u32()?;
                let position = s.u64()?;
                if !ids.insert(id) {
                    return Err(error("snapshot contains a duplicate instance"));
                }
                if let Some(previous) = saved_positions.get(&position) {
                    if *previous != id {
                        return Err(error("snapshot saved instance ID mismatch"));
                    }
                } else {
                    metadata_budget(&mut allocation, 1, 128)?;
                    let record = saved_record(backing, &metadata, id, position, None, false)?;
                    regions.push((position as usize, record.body.len() + 12));
                    saved_positions.insert(position, id);
                    saved.push(record);
                }
            }
        }
        metadata_budget(&mut allocation, total as usize, 128)?;
        let mut events = Vec::with_capacity(total as usize);
        let mut previous_end = 0;
        for page in &pages {
            check(proceed)?;
            if page.first != events.len() as u64 || page.count == 0 || page.position < previous_end
            {
                return Err(error("invalid page order"));
            }
            let bytes = range(backing, page.position, page.len as usize)?;
            previous_end = page.position + u64::from(page.len);
            regions.push((page.position as usize, page.len as usize));
            let mut p = Reader::new(bytes);
            if count(&mut p, 4, &mut allocation)? != page.count as usize {
                return Err(error("page count mismatch"));
            }
            let mut offsets = Vec::new();
            for _ in 0..page.count {
                offsets.push(p.u32()? as usize);
            }
            let payload = p.take(p.bytes.len() - p.pos)?;
            if offsets.first() != Some(&0) {
                return Err(error("first page offset is not zero"));
            }
            offsets.push(payload.len());
            for pair in offsets.windows(2) {
                check(proceed)?;
                if pair[0] >= pair[1] {
                    return Err(error("event offsets are not increasing"));
                }
                let bytes = payload
                    .get(pair[0]..pair[1])
                    .ok_or_else(|| error("event exceeds page"))?;
                let event =
                    HistoryEvent::parse(bytes, events.len() as u64, &metadata, &mut allocation)?;
                if let HistoryEvent::Saved { id, position } = &event {
                    if let Some(previous) = saved_positions.get(position) {
                        if previous != id {
                            return Err(error("auxiliary saved instance ID mismatch"));
                        }
                    } else {
                        metadata_budget(&mut allocation, 1, 128)?;
                        let record = saved_record(backing, &metadata, *id, *position, None, false)?;
                        regions.push((*position as usize, record.body.len() + 12));
                        saved_positions.insert(*position, *id);
                        saved.push(record);
                    }
                }
                events.push(event);
            }
        }
        if events.len() as u64 != total {
            return Err(error("total event count mismatch"));
        }
        regions.sort_unstable();
        let mut previous_end = 0;
        let mut gaps = Vec::new();
        for (start, len) in regions {
            if start < previous_end {
                return Err(error("overlapping backing regions"));
            }
            if start > previous_end {
                gaps.push((previous_end, &backing[previous_end..start]));
            }
            previous_end = start + len;
        }
        if previous_end < backing.len() {
            gaps.push((previous_end, &backing[previous_end..]));
        }
        let layout = Self {
            source,
            object_bytes,
            metadata,
            pointer_layouts,
            communication,
            metadata_padding,
            history_padding,
            pages,
            snapshots,
            snapshot_bytes,
            saved,
            events,
            backing,
            gaps,
        };
        layout.validate_sections()?;
        Ok(layout)
    }

    fn validate_sections(&self) -> Result<(), Error> {
        let object_ids: BTreeSet<_> = self
            .events
            .iter()
            .filter_map(|event| match event {
                HistoryEvent::Create(id, _) => Some(*id),
                _ => None,
            })
            .chain(self.saved.iter().map(|s| s.id))
            .collect();
        if self
            .communication
            .cameras
            .iter()
            .any(|(_, id)| !object_ids.contains(id))
            || self
                .communication
                .memory_instances
                .iter()
                .any(|id| *id != u32::MAX && !object_ids.contains(id))
        {
            return Err(error("communication catalog refers to a missing instance"));
        }
        let mut referenced = BTreeSet::new();
        for section in &self.communication.sections {
            let mut previous = None;
            for &[begin_ticks, begin_event, end_ticks, end_event] in &section.rows {
                if begin_event >= end_event
                    || begin_ticks > end_ticks
                    || previous.is_some_and(|last| begin_event <= last)
                {
                    return Err(error("invalid section boundaries"));
                }
                for (ordinal, ticks, begin) in
                    [(begin_event, begin_ticks, 1), (end_event, end_ticks, 0)]
                {
                    match self.events.get(ordinal as usize) {
                        Some(HistoryEvent::Section {
                            source,
                            ticks: event_ticks,
                            name,
                            begin: event_begin,
                            ..
                        }) if *source == section.source
                            && *name == section.name
                            && *event_ticks == ticks
                            && *event_begin == begin => {}
                        _ => return Err(error("section table disagrees with history event")),
                    }
                    if !referenced.insert(ordinal) {
                        return Err(error("section event referenced twice"));
                    }
                }
                previous = Some(end_event);
            }
        }
        for (i, event) in self.events.iter().enumerate() {
            if matches!(event, HistoryEvent::Section { .. }) && !referenced.contains(&(i as u64)) {
                return Err(error("unpaired or unindexed section event"));
            }
        }
        Ok(())
    }

    fn encode(
        &self,
        preserve_padding: bool,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Vec<u8>, Error> {
        let comm = self.communication.encode(proceed)?;
        let mut out = Vec::new();
        w32(&mut out, MAGIC);
        w32(&mut out, 1);
        w32(&mut out, u32_len(self.object_bytes.len())?);
        w32(&mut out, u32_len(comm.len())?);
        out.extend_from_slice(self.object_bytes);
        out.extend_from_slice(&comm);
        if preserve_padding {
            out.extend_from_slice(self.metadata_padding);
        } else {
            out.resize(next_page(out.len())?, 0);
        }
        w64(&mut out, self.events.len() as u64);
        w32(&mut out, u32_len(self.pages.len())?);
        for p in &self.pages {
            w64(&mut out, p.first);
            w64(&mut out, p.position);
            w32(&mut out, p.count);
            w32(&mut out, p.len);
        }
        w32(&mut out, u32_len(self.snapshots.len())?);
        for s in &self.snapshots {
            w64(&mut out, s.event);
            w64(&mut out, s.position);
            w32(&mut out, s.len);
            w32(&mut out, s.reserved);
        }
        w32(
            &mut out,
            u32_len(self.saved.iter().filter(|s| s.deleted).count())?,
        );
        for s in self.saved.iter().filter(|s| s.deleted) {
            w32(&mut out, s.id);
            w64(&mut out, s.position);
            w32(&mut out, u32_len(s.body.len() + 12)?);
        }
        if preserve_padding {
            out.extend_from_slice(self.history_padding);
        } else {
            out.resize(next_page(out.len())?, 0);
        }
        let base = out.len();
        let len = self
            .pages
            .last()
            .map(|p| p.position as usize + p.len as usize)
            .unwrap_or(0)
            .max(
                self.snapshots
                    .iter()
                    .map(|s| s.position as usize + s.len as usize)
                    .max()
                    .unwrap_or(0),
            )
            .max(
                self.saved
                    .iter()
                    .map(|s| s.position as usize + s.body.len() + 12)
                    .max()
                    .unwrap_or(0),
            );
        out.resize(
            output_size(
                base,
                if preserve_padding {
                    self.backing.len()
                } else {
                    len
                },
            )?,
            0,
        );
        for (snapshot, bytes) in self.snapshots.iter().zip(&self.snapshot_bytes) {
            place(&mut out, base, snapshot.position, bytes, proceed)?;
        }
        for s in &self.saved {
            let mut bytes = Vec::new();
            w32(&mut bytes, s.class);
            w32(&mut bytes, s.id);
            blob(&mut bytes, &s.body)?;
            place(&mut out, base, s.position, &bytes, proceed)?;
        }
        for p in &self.pages {
            let bytes = encode_page(
                &self.events[p.first as usize..p.first as usize + p.count as usize],
                proceed,
            )?;
            if bytes.len() != p.len as usize {
                return Err(error("page serialization length mismatch"));
            }
            place(&mut out, base, p.position, &bytes, proceed)?;
        }
        if preserve_padding {
            for &(position, bytes) in &self.gaps {
                place(&mut out, base, position as u64, bytes, proceed)?;
            }
        }
        Ok(out)
    }

    fn verify_prefix_trim_events(
        &self,
        cut: usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let origin_verified = origin_prefix::verify_prefix_events(self, cut, proceed)?;
        self.verify_trim_events_mode(cut, origin_verified, proceed)
    }

    fn verify_trim_events_mode(
        &self,
        cut: usize,
        origin_verified: bool,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let mut classes = initial::State::read(self, proceed)?.classes;
        let mut allocation = 0;
        metadata_budget(&mut allocation, classes.len(), 64)?;
        for event in self
            .events
            .get(..cut)
            .ok_or_else(|| error("trim event exceeds history"))?
        {
            check(proceed)?;
            match event {
                HistoryEvent::Create(id, class) => {
                    if !classes.contains_key(id) {
                        metadata_budget(&mut allocation, 64, 1)?;
                    }
                    classes.insert(*id, *class);
                }
                HistoryEvent::Destroy(id) if classes.remove(id).is_some() => {
                    allocation -= 64;
                }
                HistoryEvent::OriginShift(_) if !origin_verified => {
                    return Err(error("OriginShift middle trimming is not verified"));
                }
                HistoryEvent::Set {
                    kind: 4,
                    id,
                    property,
                    data,
                    ..
                } => {
                    let class = classes
                        .get(id)
                        .ok_or_else(|| error("property message targets missing instance"))?;
                    middle::validate_message_instance(&self.metadata, *property, *class, proceed)?;
                    middle::validate_message(&self.metadata, *property, data, proceed)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn verify_event_pointers(
        &self,
        cut: usize,
        created: &BTreeSet<u32>,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        for event in &self.events[..cut] {
            check(proceed)?;
            match event {
                HistoryEvent::Set {
                    kind: 3,
                    property,
                    data,
                    ..
                } => self.verify_property_pointers(*property, data, created, proceed)?,
                HistoryEvent::Array {
                    property,
                    data: Some(data),
                    ..
                } => self.verify_property_pointers(*property, data, created, proceed)?,
                HistoryEvent::Set {
                    kind: 4,
                    property,
                    data,
                    ..
                } => {
                    let message = self.metadata.messages[*property as usize]
                        .as_ref()
                        .ok_or_else(|| error("undefined message"))?;
                    for entry in &message.entries {
                        let end = entry
                            .offset
                            .checked_add(entry.size)
                            .ok_or_else(|| error("message offset overflow"))?;
                        let bytes = data
                            .get(entry.offset..end)
                            .ok_or_else(|| error("message field exceeds payload"))?;
                        self.verify_property_pointers(entry.property, bytes, created, proceed)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn verify_property_pointers(
        &self,
        property: u32,
        data: &[u8],
        created: &BTreeSet<u32>,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let property = self.metadata.property(property)?;
        let datatype = self.metadata.class(property.datatype_id)?;
        let offsets = &self.pointer_layouts[property.datatype_id as usize];
        if datatype.dynamic {
            return Err(error("unsupported packed pointer datatype layout"));
        }
        if offsets.is_empty() {
            return Ok(());
        }
        if datatype.size == 0 || !data.len().is_multiple_of(datatype.size) {
            return Err(error("unsupported packed pointer datatype layout"));
        }
        for value in data.chunks_exact(datatype.size) {
            check(proceed)?;
            for &(kind, offset) in offsets {
                let end = offset
                    .checked_add(4)
                    .ok_or_else(|| error("pointer offset overflow"))?;
                let bytes = value
                    .get(offset..end)
                    .ok_or_else(|| error("pointer exceeds datatype"))?;
                let id = u32::from_le_bytes(bytes.try_into()?);
                match kind {
                    1 if id == 0 || created.contains(&id) => {}
                    2 if self.metadata.strings.contains_key(&id) => {}
                    1 => {
                        return Err(error(
                            "event packed reference is outside retained object lifecycles",
                        ));
                    }
                    2 => return Err(error("event packed string handle is undefined")),
                    _ => return Err(error("unsupported event pointer-offset kind")),
                }
            }
        }
        Ok(())
    }

    fn verify_saved_prefix(
        &self,
        cut: usize,
        created: &BTreeSet<u32>,
        saved_refs: &BTreeMap<u32, u64>,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        if saved_refs.is_empty() {
            return Ok(());
        }
        let header = pvd_protocol::Header {
            version: 1,
            timestamp_numerator: self.communication.clock[0],
            timestamp_denominator: self.communication.clock[1],
            flags: 0,
            endian: pvd_protocol::Endian::Little,
        };
        let mut restored = crate::RecordingBuilder::new(header.clone());
        self.metadata.register(&mut restored)?;
        let mut directory = Vec::new();
        w32(&mut directory, u32_len(saved_refs.len())?);
        for (&id, &position) in saved_refs {
            w32(&mut directory, id);
            w64(&mut directory, position);
        }
        super::snapshot::restore(
            &self.metadata,
            &mut restored,
            self.backing,
            &directory,
            &mut 0,
            proceed,
        )?;
        for saved in &self.saved {
            if saved_refs.get(&saved.id) == Some(&saved.position) {
                self.verify_saved_pointers(saved, created, proceed)?;
            }
        }
        for object in restored.state.objects.values() {
            for value in object.properties.values() {
                check(proceed)?;
                let mut targets = Vec::new();
                value.collect_references(&mut targets);
                if targets.iter().any(|id| {
                    *id != 0
                        && u32::try_from(*id)
                            .ok()
                            .is_none_or(|id| !created.contains(&id))
                }) {
                    return Err(error(
                        "saved instance refers outside retained object lifecycles",
                    ));
                }
            }
        }
        let mut replay = crate::RecordingBuilder::new(header);
        self.metadata.register(&mut replay)?;
        if self.snapshot_bytes[0].as_ref() != [0, 0, 0, 0] {
            super::snapshot::restore(
                &self.metadata,
                &mut replay,
                self.backing,
                &self.snapshot_bytes[0],
                &mut 0,
                proceed,
            )?;
        }
        let initial_properties = super::snapshot::initial_properties(&self.metadata, proceed)?;
        let catalog = self
            .communication
            .zones
            .iter()
            .map(|z| super::ProfileCatalogEntry {
                local_id: u64::from(z.id),
                name: String::from_utf8_lossy(z.name.strip_suffix(&[0]).unwrap_or(z.name))
                    .into_owned(),
                events: z
                    .events
                    .iter()
                    .map(|(name, id, enabled)| {
                        (
                            *id,
                            String::from_utf8_lossy(name.strip_suffix(&[0]).unwrap_or(name))
                                .into_owned(),
                            *enabled != 0,
                        )
                    })
                    .collect(),
            })
            .collect();
        super::register_profile_catalog(&mut replay, catalog)?;
        let mut sources = BTreeMap::<u64, Vec<u64>>::new();
        for instance in &self.communication.instances {
            sources
                .entry(instance.source)
                .or_default()
                .push(u64::from(instance.id));
        }
        let mut bytes = Vec::new();
        for (ordinal, event) in self.events[..cut].iter().enumerate() {
            check(proceed)?;
            if let HistoryEvent::Saved { id, .. } = event {
                let expected = replay
                    .state
                    .objects
                    .get(&u64::from(*id))
                    .ok_or_else(|| error("saved record has no live event state"))?;
                let actual = &restored.state.objects[&u64::from(*id)];
                if actual.type_name != expected.type_name
                    || expected
                        .properties
                        .iter()
                        .any(|(name, value)| actual.properties.get(name) != Some(value))
                {
                    return Err(error(
                        "saved record disagrees with state at its auxiliary event",
                    ));
                }
            }
            bytes.clear();
            event.encode(&mut bytes)?;
            super::import_event_with_progress(
                &self.metadata,
                &sources,
                &mut replay,
                &bytes,
                ordinal as u64,
                self.backing.len(),
                proceed,
            )?;
            if let HistoryEvent::Create(id, _) = event {
                initial_properties.restore_instance(&self.metadata, &mut replay, *id, proceed)?;
            }
        }
        Ok(())
    }

    fn verify_saved_pointers(
        &self,
        saved: &Saved<'_>,
        created: &BTreeSet<u32>,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let mut r = Reader::new(&saved.body);
        let fixed = r.blob()?;
        let mut ancestors = Vec::new();
        let mut current = saved.class;
        while current != u32::MAX {
            if ancestors.len() >= 128 || ancestors.contains(&current) {
                return Err(error("invalid saved class inheritance"));
            }
            ancestors.push(current);
            current = self.metadata.class(current)?.base;
        }
        for class in ancestors.into_iter().rev() {
            for &property in &self.metadata.class(class)?.properties {
                check(proceed)?;
                let property = self.metadata.property(property)?;
                let datatype = self.metadata.class(property.datatype_id)?;
                if datatype.dynamic || datatype.size == 0 {
                    return Err(error("unsupported saved nested datatype"));
                }
                let data = if property.kind == 2 {
                    r.blob()?
                } else {
                    fixed
                        .get(
                            property.offset32
                                ..property
                                    .offset32
                                    .checked_add(datatype.size)
                                    .ok_or_else(|| error("saved datatype offset overflow"))?,
                        )
                        .ok_or_else(|| error("saved datatype exceeds fixed body"))?
                };
                if !data.len().is_multiple_of(datatype.size) {
                    return Err(error("saved array does not match datatype width"));
                }
                for value in data.chunks_exact(datatype.size) {
                    check(proceed)?;
                    for &(kind, offset) in &self.pointer_layouts[property.datatype_id as usize] {
                        let word = value
                            .get(
                                offset
                                    ..offset
                                        .checked_add(4)
                                        .ok_or_else(|| error("saved pointer offset overflow"))?,
                            )
                            .ok_or_else(|| error("saved pointer exceeds datatype"))?;
                        let id = u32::from_le_bytes(word.try_into()?);
                        match kind {
                            1 if id == 0 || created.contains(&id) => {}
                            2 if self.metadata.strings.contains_key(&id) => {}
                            1 => {
                                return Err(error(
                                    "saved packed reference is outside retained object lifecycles",
                                ));
                            }
                            2 => return Err(error("saved packed string handle is undefined")),
                            _ => return Err(error("unsupported saved pointer-offset kind")),
                        }
                    }
                }
            }
        }
        r.end()
    }
}

fn write_result(
    layout: &Layout<'_>,
    bytes: &[u8],
    out: &mut (impl Write + ?Sized),
    proceed: &mut dyn FnMut() -> bool,
) -> Result<Report, Error> {
    for chunk in bytes.chunks(64 << 10) {
        check(proceed)?;
        out.write_all(chunk)?;
    }
    check(proceed)?;
    let frames = layout
        .communication
        .sections
        .iter()
        .filter(|section| {
            layout
                .metadata
                .strings
                .get(&section.name)
                .is_some_and(|name| name == "frame")
        })
        .map(|section| section.rows.len())
        .sum();
    Ok(Report {
        source_bytes: layout.source.len(),
        source_blake3: blake3::hash(layout.source),
        source_frames: frames,
        output_bytes: bytes.len(),
        events: layout.events.len(),
        frames,
        saved_instances: layout.saved.len(),
    })
}

/// The sink must be staged: an I/O error or cancellation can leave a partial byte prefix.
pub fn rewrite(
    source: &[u8],
    output: &mut (impl Write + ?Sized),
    mut proceed: impl FnMut() -> bool,
) -> Result<Report, Error> {
    let layout = Layout::parse(source, &mut proceed)?;
    let bytes = layout.encode(true, &mut proceed)?;
    if bytes != source {
        return Err(error("container reconstruction is not byte-identical"));
    }
    write_result(&layout, &bytes, output, &mut proceed)
}

/// The sink must be staged; success establishes internal layout checks, not original-app acceptance.
pub fn trim_scene_frames(
    source: &[u8],
    first_frame: usize,
    last_frame: usize,
    output: &mut (impl Write + ?Sized),
    mut proceed: impl FnMut() -> bool,
) -> Result<Report, Error> {
    if first_frame > last_frame {
        return Err(error("first frame exceeds last frame"));
    }
    let (recording, _) = super::read_with_progress(source, |_, _| proceed())?;
    if recording.analysis.first_retained_frame() != 0
        || recording
            .diagnostics
            .iter()
            .any(|d| d.starts_with("Analysis decoding stopped at frame"))
    {
        return Err(error("native trim requires complete analysis decoding"));
    }
    let source_frames = recording.frame_count();
    let mut layout = Layout::parse(source, &mut proceed)?;
    let nonempty_initial = layout
        .snapshot_bytes
        .first()
        .is_some_and(|bytes| bytes.as_ref() != [0, 0, 0, 0]);
    let prefix_cut = if first_frame == 0 {
        Some(layout.prefix_cut(last_frame, &mut proceed)?)
    } else {
        None
    };
    let retained_origin = match prefix_cut {
        Some(cut) => origin_prefix::contains_shift(&layout, cut, &mut proceed)?,
        None => false,
    };
    let cameras = camera::Scope::prepare(&layout, first_frame, last_frame, &mut proceed)?;
    let camera_permit = cameras
        .as_ref()
        .map(|scope| scope.permit(&layout))
        .transpose()?;
    let prefix_proof = if let Some(cut) = prefix_cut
        && (nonempty_initial || retained_origin)
    {
        Some(initial::Proof::prepare_with_cameras(
            &layout,
            cut,
            camera_permit.as_ref(),
            &mut proceed,
        )?)
    } else {
        None
    };
    let recording = (first_frame != 0
        || layout.communication.sections.len() > 1
        || nonempty_initial
        || retained_origin
        || !layout.communication.cameras.is_empty())
    .then_some(recording);
    if layout.encode(true, &mut proceed)? != source {
        return Err(error("container reconstruction is not byte-identical"));
    }
    let history = if first_frame == 0 {
        if cameras.is_some() {
            layout.trim_prefix_with_analysis(last_frame, None, cameras.as_ref(), &mut proceed)?;
        } else {
            layout.trim_prefix(last_frame, &mut proceed)?;
        }
        None
    } else if cameras.is_some() {
        Some(layout.trim_middle_with_cameras(
            first_frame,
            last_frame,
            cameras.as_ref(),
            &mut proceed,
        )?)
    } else {
        Some(layout.trim_middle(first_frame, last_frame, &mut proceed)?)
    };
    let bytes = layout.encode(false, &mut proceed)?;
    let encoded_layout = Layout::parse(&bytes, &mut proceed)?;
    if let Some(history) = &history {
        history.verify_layout(&encoded_layout, &mut proceed)?;
    }
    let output_camera_permit = cameras
        .as_ref()
        .map(|scope| {
            scope.verify_with_permit(
                &encoded_layout,
                history.as_ref().map_or(0, history::Proof::leading_count),
                &mut proceed,
            )
        })
        .transpose()?;
    if let Some(proof) = &prefix_proof {
        proof.verify_with_cameras(&encoded_layout, output_camera_permit.as_ref(), &mut proceed)?;
    }
    drop(encoded_layout);
    if let Some(recording) = recording {
        let (restored, _) = super::read_with_progress(&bytes, |_, _| proceed())?;
        if restored.frame_count() != last_frame - first_frame + 1 {
            return Err(error("trim frame count mismatch"));
        }
        if let Some(history) = history {
            history.verify(&restored, &recording, first_frame, &mut proceed)?;
        }
        if let Some(proof) = &prefix_proof {
            proof.verify_errors(&restored, &recording, &mut proceed)?;
        }
        let mut actual = restored.state_at_with_cancel(0, || !proceed())?;
        let mut expected = recording.state_at_with_cancel(first_frame, || !proceed())?;
        for frame in 0..restored.frame_count() {
            check(&mut proceed)?;
            if frame != 0 {
                for (source, state, index) in [
                    (&restored, &mut actual, frame),
                    (&recording, &mut expected, first_frame + frame),
                ] {
                    let actions = source
                        .actions
                        .get(source.frame_ends[index - 1]..source.frame_ends[index])
                        .ok_or_else(|| error("trim frame actions are unavailable"))?;
                    for action in actions {
                        check(&mut proceed)?;
                        action.apply(state);
                    }
                }
            }
            if actual.objects != expected.objects
                || actual.cameras != expected.cameras
                || actual.errors != expected.errors
                || actual.debug_draw != expected.debug_draw
                || actual.contacts != expected.contacts
                || restored.frames[frame].timestamp
                    != recording.frames[first_frame + frame].timestamp
                || restored.frames[frame].scene_id != recording.frames[first_frame + frame].scene_id
            {
                return Err(error("trim does not preserve recorded frame state"));
            }
        }
    }
    let mut report = write_result(&layout, &bytes, output, &mut proceed)?;
    report.source_frames = source_frames;
    Ok(report)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod camera_tests;
