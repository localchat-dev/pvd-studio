use super::*;
use crate::native::{Class, Property, container_tests};
use pvd_protocol::Name;

pub(super) fn word(out: &mut Vec<u8>, value: u32) {
    out.extend(value.to_le_bytes());
}
fn wide(out: &mut Vec<u8>, value: u64) {
    out.extend(value.to_le_bytes());
}
fn blob(out: &mut Vec<u8>, value: &[u8]) {
    word(out, value.len() as u32);
    out.extend(value);
}

fn metadata() -> Metadata {
    let mut metadata = container_tests::fixture();
    for (id, text) in [
        (14, "PvdU8"),
        (15, "PvdI32"),
        (16, "PxProfileMemoryEventBuffer"),
        (17, "events"),
        (18, "_debugger"),
        (19, "MemoryEventTotals"),
        (20, "totals"),
        (21, "Allocation"),
        (22, "source.cpp"),
        (23, "Other"),
        (24, "other.cpp"),
        (25, "Future"),
        (26, "future.cpp"),
    ] {
        metadata.strings.insert(id, text.into());
    }
    metadata.classes[1].as_mut().unwrap().has_pointers = true;
    metadata.classes[1].as_mut().unwrap().string_offsets = vec![0];
    metadata.classes[2].as_mut().unwrap().has_pointers = true;
    let name = |namespace, name| Name { namespace, name };
    for (label, size, properties, dynamic) in [
        (name(1, 14), 1, vec![], false),
        (name(1, 15), 4, vec![], false),
        (name(11, 16), 24, vec![3], true),
        (name(18, 19), 24, vec![4], true),
    ] {
        metadata.classes.push(Some(Class {
            name: label,
            base: u32::MAX,
            packing: crate::native::ClassPacking::default(),
            size,
            properties,
            dynamic,
            has_pointers: false,
            string_offsets: vec![],
        }));
    }
    for (owner, label, datatype, datatype_id) in [
        (name(11, 16), 17, name(1, 14), 6),
        (name(18, 19), 20, name(1, 15), 7),
    ] {
        metadata.properties.push(Some(Property {
            owner,
            name: label,
            datatype,
            datatype_id,
            kind: 2,
            semantic: 0,
            offset32: 0,
            values: vec![],
        }));
    }
    metadata
}

fn metadata_bytes(metadata: &Metadata) -> Vec<u8> {
    let mut out = Vec::new();
    for value in [
        1,
        metadata.classes.len() as u32,
        metadata.strings.len() as u32 - 1,
        metadata.strings.len() as u32,
    ] {
        word(&mut out, value);
    }
    for (&handle, text) in &metadata.strings {
        if handle != 0 {
            word(&mut out, handle);
            blob(&mut out, &[text.as_bytes(), &[0]].concat());
        }
    }
    word(&mut out, metadata.properties.len() as u32);
    for (index, property) in metadata.properties.iter().enumerate() {
        word(&mut out, u32::from(property.is_some()));
        let Some(p) = property else { continue };
        for value in [
            p.owner.namespace,
            p.owner.name,
            0,
            p.semantic,
            p.datatype_id,
            p.datatype.namespace,
            p.datatype.name,
            u32::from(p.kind),
            index as u32,
            p.offset32 as u32,
            p.offset32 as u32,
            0,
            p.name,
        ] {
            word(&mut out, value);
        }
    }
    word(&mut out, metadata.classes.len() as u32);
    for (index, class) in metadata.classes.iter().enumerate() {
        word(&mut out, u32::from(class.is_some()));
        let Some(c) = class else { continue };
        for value in [
            c.name.namespace,
            c.name.name,
            index as u32,
            c.base,
            u32::MAX,
            u32::MAX,
        ] {
            word(&mut out, value);
        }
        out.extend([0, u8::from(c.dynamic)]);
        for value in [
            c.size as u32,
            c.size as u32,
            4,
            c.size as u32,
            c.size as u32,
            4,
            c.properties.len() as u32,
        ] {
            word(&mut out, value);
        }
        for &p in &c.properties {
            word(&mut out, p);
        }
        for _ in 0..2 {
            word(
                &mut out,
                u32::from(index == 2) + c.string_offsets.len() as u32,
            );
            if index == 2 {
                word(&mut out, 1);
                word(&mut out, 0);
            }
            for &offset in &c.string_offsets {
                word(&mut out, 2);
                word(&mut out, offset as u32);
            }
        }
    }
    word(&mut out, 0);
    out
}

pub(super) fn totals(values: &[i32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}
pub(super) fn old_buffer() -> Vec<u8> {
    vec![2, 0, 64, 21, 22, 7, 42]
}
pub(super) fn first_flush() -> Vec<u8> {
    vec![3, 0, 42, 2, 0, 8, 23, 24, 8, 9]
}
pub(super) fn second_flush() -> Vec<u8> {
    vec![3, 0, 9]
}
pub(super) fn body(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    blob(&mut out, &[0x5a; 24]);
    blob(&mut out, data);
    out
}

fn set(id: u32, property: u32, data: &[u8], previous: usize, ordinal: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for value in [3, id, property] {
        word(&mut out, value);
    }
    blob(&mut out, data);
    word(&mut out, 1);
    word(&mut out, (ordinal - previous) as u32);
    out
}
fn section(ticks: u64, begin: bool) -> Vec<u8> {
    let mut payload = Vec::new();
    wide(&mut payload, 0x111);
    wide(&mut payload, ticks);
    word(&mut payload, 13);
    payload.extend([u8::from(begin), 0, 0, 0]);
    let mut out = Vec::new();
    word(&mut out, 8);
    word(&mut out, 5);
    blob(&mut out, &payload);
    out
}
fn saved(id: u32, position: u64) -> Vec<u8> {
    let mut out = Vec::new();
    word(&mut out, 10);
    word(&mut out, id);
    wide(&mut out, position);
    out
}
fn destroy(id: u32) -> Vec<u8> {
    let mut out = Vec::new();
    word(&mut out, 2);
    word(&mut out, id);
    out
}

pub(super) fn source(deleted: bool) -> Vec<u8> {
    let metadata = metadata();
    let objects = metadata_bytes(&metadata);
    let scene = container_tests::instance(1, 7, &[2, 3]);
    let mut records = vec![
        (1, 4, scene[12..].to_vec(), false),
        (2, 8, body(&old_buffer()), false),
        (3, 9, body(&totals(&[64, 16])), false),
    ];
    if deleted {
        records.extend([
            (2, 8, body(&second_flush()), true),
            (3, 9, body(&totals(&[0, 16])), true),
        ]);
    }
    let mut positions = Vec::new();
    let mut backing = Vec::new();
    word(&mut backing, 3);
    let mut position = 128u64;
    for (i, (id, _, data, _)) in records.iter().enumerate() {
        positions.push(position);
        if i < 3 {
            word(&mut backing, *id);
            wide(&mut backing, position);
        }
        position += data.len() as u64 + 12;
    }
    backing.resize(128, 0);
    for (id, class, data, _) in &records {
        word(&mut backing, *class);
        word(&mut backing, *id);
        blob(&mut backing, data);
    }
    let mut events = vec![9u32.to_le_bytes().to_vec()];
    let mut rows = Vec::new();
    let mut previous = [0, 0];
    for frame in 0..3 {
        let ticks = 100 + frame * 100;
        let begin = events.len() as u64;
        events.push(section(ticks, true));
        if frame < 2 || !deleted {
            let (buffer, values) = match frame {
                0 => (first_flush(), vec![0, 24]),
                1 => (second_flush(), vec![0, 16]),
                _ => (vec![2, 0, 4, 25, 26, 9, 10], vec![0, 16, 4]),
            };
            for (slot, data) in [(0, buffer), (1, totals(&values))] {
                let ordinal = events.len();
                events.push(set(
                    (slot + 2) as u32,
                    (slot + 3) as u32,
                    &data,
                    previous[slot],
                    ordinal,
                ));
                previous[slot] = ordinal;
            }
        }
        if deleted && frame == 1 {
            events.push(set(1, 2, &[], 0, events.len()));
            for slot in 0..2 {
                events.push(saved((slot + 2) as u32, positions[slot + 3]));
                events.push(destroy((slot + 2) as u32));
            }
        }
        let end = events.len() as u64;
        events.push(section(ticks + 1, false));
        rows.push([ticks, begin, ticks + 1, end]);
    }
    if !deleted {
        for (slot, data) in [(0, vec![3, 0, 10]), (1, totals(&[0, 16]))] {
            let ordinal = events.len();
            events.push(set(
                (slot + 2) as u32,
                (slot + 3) as u32,
                &data,
                previous[slot],
                ordinal,
            ));
        }
    }
    let mut comm = Vec::new();
    word(&mut comm, 1);
    wide(&mut comm, 1);
    wide(&mut comm, 1);
    word(&mut comm, 2);
    for (id, source) in [(1, 0x111), (2, 0x222)] {
        word(&mut comm, id);
        wide(&mut comm, source);
        word(&mut comm, id);
        comm.extend([0; 10]);
    }
    word(&mut comm, if deleted { 1 } else { 2 });
    wide(&mut comm, 0x111);
    word(&mut comm, 1);
    if !deleted {
        wide(&mut comm, 0x222);
        word(&mut comm, 2);
    }
    word(&mut comm, 0);
    word(&mut comm, 1);
    wide(&mut comm, 0x111);
    word(&mut comm, 13);
    word(&mut comm, rows.len() as u32);
    for row in rows {
        for value in row {
            wide(&mut comm, value);
        }
    }
    word(&mut comm, 0);
    word(&mut comm, 2);
    word(&mut comm, 3);
    word(&mut comm, 3);
    for (kind, file, line, index) in [
        ("Future", "future.cpp", 9, 2),
        ("Allocation", "source.cpp", 7, 0),
        ("Other", "other.cpp", 8, 1),
    ] {
        blob(&mut comm, &[kind.as_bytes(), &[0]].concat());
        blob(&mut comm, &[file.as_bytes(), &[0]].concat());
        word(&mut comm, line);
        word(&mut comm, index);
    }
    let mut page = Vec::new();
    word(&mut page, events.len() as u32);
    let mut offset = 0;
    for event in &events {
        word(&mut page, offset);
        offset += event.len() as u32;
    }
    page.extend(events.concat());
    backing.resize(4096, 0);
    backing.extend(&page);
    let mut out = Vec::new();
    for value in [MAGIC, 1, objects.len() as u32, comm.len() as u32] {
        word(&mut out, value);
    }
    out.extend(objects);
    out.extend(comm);
    out.resize((out.len() / 4096 + 1) * 4096, 0);
    wide(&mut out, events.len() as u64);
    word(&mut out, 1);
    wide(&mut out, 0);
    wide(&mut out, 4096);
    word(&mut out, events.len() as u32);
    word(&mut out, page.len() as u32);
    word(&mut out, 1);
    wide(&mut out, 0);
    wide(&mut out, 0);
    word(&mut out, 40);
    word(&mut out, 492);
    word(&mut out, if deleted { 2 } else { 0 });
    if deleted {
        for index in 3..5 {
            word(&mut out, records[index].0);
            wide(&mut out, positions[index]);
            word(&mut out, records[index].2.len() as u32 + 12);
        }
    }
    out.resize((out.len() / 4096 + 1) * 4096, 0);
    out.extend(backing);
    out
}

pub(super) struct Raw {
    pub(super) initial: BTreeMap<u32, (u32, Vec<u8>)>,
    pub(super) initial_positions: BTreeMap<u32, u64>,
    pub(super) events: Vec<Vec<u8>>,
    pub(super) memory: Vec<u8>,
    pub(super) deleted: Vec<(u32, u32, Vec<u8>)>,
    pub(super) reserved: u32,
}

pub(super) fn raw(bytes: &[u8]) -> Raw {
    let u32_at = |offset: usize| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    let object_len = u32_at(8) as usize;
    let comm_len = u32_at(12) as usize;
    let end = 16 + object_len + comm_len;
    let mut comm = Reader::new(&bytes[16 + object_len..end]);
    assert_eq!(comm.u32().unwrap(), 1);
    comm.take(16).unwrap();
    let n = comm.u32().unwrap() as usize;
    comm.take(n * 26).unwrap();
    let n = comm.u32().unwrap() as usize;
    comm.take(n * 12).unwrap();
    let n = comm.u32().unwrap() as usize;
    comm.take(n * 8).unwrap();
    for _ in 0..comm.u32().unwrap() {
        comm.take(12).unwrap();
        let n = comm.u32().unwrap() as usize;
        comm.take(n * 32).unwrap();
    }
    assert_eq!(comm.u32().unwrap(), 0);
    let memory = comm.bytes[comm.pos..].to_vec();
    let mut directory = Reader::new(&bytes[(end / 4096 + 1) * 4096..]);
    directory.u64().unwrap();
    assert_eq!(directory.u32().unwrap(), 1);
    assert_eq!(directory.u64().unwrap(), 0);
    let page_position = directory.u64().unwrap() as usize;
    let count = directory.u32().unwrap() as usize;
    let length = directory.u32().unwrap() as usize;
    assert_eq!(directory.u32().unwrap(), 1);
    assert_eq!(directory.u64().unwrap(), 0);
    let snapshot = directory.u64().unwrap() as usize;
    let _snapshot_length = directory.u32().unwrap();
    let reserved = directory.u32().unwrap();
    let mut deleted_records = Vec::new();
    for _ in 0..directory.u32().unwrap() {
        let id = directory.u32().unwrap();
        let position = directory.u64().unwrap() as usize;
        directory.u32().unwrap();
        deleted_records.push((id, position));
    }
    let base = (((end / 4096 + 1) * 4096 + directory.pos) / 4096 + 1) * 4096;
    let record = |id, position| {
        let mut r = Reader::new(&bytes[base + position..]);
        let class = r.u32().unwrap();
        assert_eq!(r.u32().unwrap(), id);
        (class, r.blob().unwrap().to_vec())
    };
    let mut initial = BTreeMap::new();
    let mut initial_positions = BTreeMap::new();
    let mut snap = Reader::new(&bytes[base + snapshot..]);
    for _ in 0..snap.u32().unwrap() {
        let id = snap.u32().unwrap();
        let position = snap.u64().unwrap() as usize;
        initial.insert(id, record(id, position));
        initial_positions.insert(id, position as u64);
    }
    let deleted = deleted_records
        .into_iter()
        .map(|(id, position)| {
            let (class, body) = record(id, position);
            (id, class, body)
        })
        .collect();
    let mut page = Reader::new(&bytes[base + page_position..base + page_position + length]);
    assert_eq!(page.u32().unwrap() as usize, count);
    let mut offsets = Vec::new();
    for _ in 0..count {
        offsets.push(page.u32().unwrap() as usize);
    }
    let payload = &page.bytes[page.pos..];
    offsets.push(payload.len());
    let events = offsets
        .windows(2)
        .map(|range| payload[range[0]..range[1]].to_vec())
        .collect();
    Raw {
        initial,
        initial_positions,
        events,
        memory,
        deleted,
        reserved,
    }
}
