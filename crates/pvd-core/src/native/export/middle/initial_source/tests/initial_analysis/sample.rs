use super::*;
use crate::native::{Class, Property};
use pvd_protocol::Name;

fn wide(out: &mut Vec<u8>, value: u64) {
    out.extend(value.to_le_bytes());
}
fn blob(out: &mut Vec<u8>, bytes: &[u8]) {
    word(out, bytes.len() as u32);
    out.extend(bytes);
}

pub(super) fn profile(kind: u8, event: u16, tick: u64) -> Vec<u8> {
    let mut bytes = vec![kind, 15];
    bytes.extend(event.to_le_bytes());
    word(&mut bytes, 99);
    wide(&mut bytes, 0x111);
    bytes.extend([6, 7]);
    wide(&mut bytes, tick);
    bytes
}

pub(super) fn drawing(instance: u64, x: f32, color: u32) -> Vec<u8> {
    let mut out = vec![1];
    wide(&mut out, instance);
    out.push(2);
    word(&mut out, 1);
    for value in [x, 2., 3.] {
        out.extend(value.to_le_bytes());
    }
    word(&mut out, color);
    out
}

pub(super) fn buffer(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    blob(&mut out, &[0x5a; 24]);
    blob(&mut out, bytes);
    out
}

fn section(tick: u64, begin: bool) -> Vec<u8> {
    let mut payload = Vec::new();
    wide(&mut payload, 0x111);
    wide(&mut payload, tick);
    word(&mut payload, 18);
    payload.extend([u8::from(begin), 0, 0, 0]);
    let mut out = Vec::new();
    word(&mut out, 8);
    word(&mut out, 5);
    blob(&mut out, &payload);
    out
}

pub(super) fn fixture_with_analysis() -> Fixture {
    let mut f = fixture();
    for (id, label) in [
        (28, "PxProfileZone"),
        (29, "PxProfileMemoryEventBuffer"),
        (30, "MemoryEventTotals"),
        (31, "_debugger"),
        (32, "PvdI32"),
        (33, "totals"),
        (34, "Zone"),
        (35, "Start17"),
        (36, "Start18"),
        (37, "Allocation"),
        (38, "alloc.cpp"),
    ] {
        f.metadata.strings.insert(id, label.into());
    }
    f.metadata.classes[10].as_mut().unwrap().size = 24;
    let name = |namespace, name| Name { namespace, name };
    for (label, size, properties, dynamic) in [
        (name(7, 28), 24, vec![11], true),
        (name(7, 29), 24, vec![12], true),
        (name(1, 32), 4, vec![], false),
        (name(31, 30), 24, vec![13], true),
    ] {
        f.metadata.classes.push(Some(Class {
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
    for (owner, label, datatype) in [(11, 25, 9), (12, 25, 9), (14, 33, 13)] {
        f.metadata.properties.push(Some(Property {
            owner: f.metadata.class(owner).unwrap().name,
            name: label,
            datatype: f.metadata.class(datatype).unwrap().name,
            datatype_id: datatype,
            kind: 2,
            semantic: 0,
            offset32: 0,
            values: vec![],
        }));
    }
    f.records.remove(4);
    f.records[4].body = body(7, &[10, 20], 1, 0xa1b2c3d4);
    for (id, class, position, payload) in [
        (6, 10, 1536, drawing(4, 100., 0xff111111)),
        (7, 11, 1792, vec![]),
        (8, 12, 2048, vec![2, 0, 64, 37, 38, 7, 42]),
        (9, 14, 2304, 64i32.to_le_bytes().to_vec()),
    ] {
        f.initial.push(f.records.len());
        f.records.push(Record {
            id,
            class,
            position,
            body: buffer(&payload),
            deleted: false,
        });
    }
    f.ids = vec![1, 2, 3, 4, 6, 7, 8];
    f.alive = vec![1, 3, 4, 6, 7, 8];
    f.events = vec![
        9u32.to_le_bytes().to_vec(),
        set(3, 7, 11, &profile(1, 17, 10), &[1]),
        section(100, true),
        set(3, 6, 10, &drawing(2, 10., 0xff222222), &[3]),
        set(3, 8, 12, &[2, 0, 8, 37, 38, 7, 9], &[4]),
        set(3, 9, 13, &72i32.to_le_bytes(), &[5]),
        set(
            3,
            7,
            11,
            &[profile(1, 18, 30), profile(2, 17, 40)].concat(),
            &[5],
        ),
        set(3, 7, 11, &[], &[1]),
        section(101, false),
        section(200, true),
        set(3, 6, 10, &drawing(1, 20., 0xff333333), &[7]),
        set(3, 8, 12, &[3, 0, 9], &[7]),
        set(3, 9, 13, &64i32.to_le_bytes(), &[7]),
        set(3, 7, 11, &profile(2, 18, 80), &[6]),
        set(3, 7, 11, &[], &[1]),
        array(5, 3, 9, 2, &error_record(9), &[]),
        array(5, 4, 9, 1, &error_record(51), &[]),
        saved(2, 1280),
        destroy(2),
        section(201, false),
    ];
    f
}

pub(super) fn catalogs() -> Vec<u8> {
    let mut out = Vec::new();
    word(&mut out, 1);
    wide(&mut out, 7 * 0x111);
    wide(&mut out, 0xfeedcafedeadbeef);
    blob(&mut out, b"Zone\0");
    word(&mut out, 7);
    word(&mut out, 2);
    for (name, id, enabled) in [
        (b"Start18\0".as_slice(), 18u16, 0u8),
        (b"Start17\0".as_slice(), 17, 1),
    ] {
        blob(&mut out, name);
        out.extend(id.to_le_bytes());
        out.push(enabled);
    }
    word(&mut out, 8);
    word(&mut out, 9);
    word(&mut out, 2);
    for (kind, file, line, index) in [
        (b"Future\0".as_slice(), b"future.cpp\0".as_slice(), 9, 1),
        (b"Allocation\0".as_slice(), b"alloc.cpp\0".as_slice(), 7, 0),
    ] {
        blob(&mut out, kind);
        blob(&mut out, file);
        word(&mut out, line);
        word(&mut out, index);
    }
    out
}

pub(super) fn source() -> Vec<u8> {
    fixture_with_analysis().bytes_with_catalogs(&catalogs())
}
