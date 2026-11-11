use super::*;
use crate::native::{Class, Message, MessageEntry, Property};
use pvd_protocol::Name;

pub(super) const CAMERA_CLASS: u32 = 16;
pub(super) const CAMERA_MESSAGE: u32 = 1;
pub(super) const NAME: u32 = 14;
pub(super) const ORIGIN: u32 = 15;
pub(super) const MAIN: u32 = 46;
pub(super) const LATE: u32 = 50;
pub(super) const FUTURE: u32 = 51;
pub(super) const A: [f32; 9] = [1., 2., 3., 0., 1., 0., 4., 5., 6.];
pub(super) const B: [f32; 9] = [10., 20., 30., 0., 1., 0., 40., 50., 60.];
pub(super) const GAP: [f32; 9] = [11., 21., 31., 0., 1., 0., 40., 50., 60.];
pub(super) const C: [f32; 9] = [-1., -2., -3., 0., 0., 1., -4., -5., -6.];
pub(super) const D: [f32; 9] = [7., 8., 9., 1., 0., 0., 10., 11., 12.];

pub(super) fn vectors(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

pub(super) fn camera_body(name: u32, values: &[f32; 9]) -> Vec<u8> {
    let mut out = 40u32.to_le_bytes().to_vec();
    word(&mut out, name);
    out.extend(vectors(values));
    out
}

pub(super) fn section(tick: u64, begin: bool) -> Vec<u8> {
    let mut out = Vec::new();
    for value in [8, 5, 24] {
        word(&mut out, value);
    }
    out.extend(0x111u64.to_le_bytes());
    out.extend(tick.to_le_bytes());
    word(&mut out, 18);
    out.extend([u8::from(begin), 0, 0, 0]);
    out
}

fn add_metadata(f: &mut Fixture) {
    for (id, value) in [
        (39, "Camera"),
        (40, "PxVec3"),
        (41, "PvdF32"),
        (42, "Name"),
        (43, "Origin"),
        (44, "Up"),
        (45, "Target"),
        (MAIN, "Main"),
        (47, "x"),
        (48, "y"),
        (49, "z"),
        (LATE, "Late"),
        (FUTURE, "FutureCamera"),
    ] {
        f.metadata.strings.insert(id, value.into());
    }
    for (namespace, name, size, properties, strings) in [
        (1, 40, 12, vec![18, 19, 20], vec![]),
        (0, 39, 40, vec![14, 15, 16, 17], vec![0]),
        (1, 41, 4, vec![], vec![]),
    ] {
        f.metadata.classes.push(Some(Class {
            name: Name { namespace, name },
            base: u32::MAX,
            packing: crate::native::ClassPacking::default(),
            size,
            properties,
            dynamic: false,
            has_pointers: !strings.is_empty(),
            string_offsets: strings,
        }));
    }
    for (owner, name, datatype_id, offset32) in [
        (16, 42, 6, 0),
        (16, 43, 15, 4),
        (16, 44, 15, 16),
        (16, 45, 15, 28),
        (15, 47, 17, 0),
        (15, 48, 17, 4),
        (15, 49, 17, 8),
    ] {
        f.metadata.properties.push(Some(Property {
            owner: f.metadata.class(owner).unwrap().name,
            name,
            datatype: f.metadata.class(datatype_id).unwrap().name,
            datatype_id,
            kind: 1,
            semantic: 0,
            offset32,
            values: vec![],
        }));
    }
    f.metadata.messages.push(Some(Message {
        class_name: f.metadata.class(CAMERA_CLASS).unwrap().name,
        class: CAMERA_CLASS,
        size: 36,
        entries: (0..3)
            .map(|i| MessageEntry {
                datatype: f.metadata.class(15).unwrap().name,
                datatype_id: 15,
                property: ORIGIN + i,
                offset: i as usize * 12,
                size: 12,
                destination_size: 12,
            })
            .collect(),
    }));
}

pub(super) fn fixture() -> Fixture {
    let mut f = capture::fixture();
    add_metadata(&mut f);
    f.initial.push(f.records.len());
    f.records.push(Record {
        id: 20,
        class: CAMERA_CLASS,
        position: 2816,
        body: camera_body(MAIN, &A),
        deleted: false,
    });
    f.records.push(Record {
        id: 21,
        class: CAMERA_CLASS,
        position: 3072,
        body: camera_body(LATE, &D),
        deleted: true,
    });
    f.events = vec![
        9u32.to_le_bytes().to_vec(),
        set(3, 10, 11, &capture::RIGHT_17_12, &[1]),
        set(3, 7, 11, &capture::LEFT_17_10, &[2]),
        section(100, true),
        set(3, 6, 10, &sample::drawing(2, 10., 0xff222222), &[4]),
        set(3, 8, 12, &[2, 0, 8, 37, 38, 7, 9], &[5]),
        set(3, 9, 13, &72i32.to_le_bytes(), &[6]),
        set(
            3,
            7,
            11,
            &[sample::profile(1, 18, 30), sample::profile(2, 17, 40)].concat(),
            &[5],
        ),
        set(
            3,
            10,
            11,
            &[sample::profile(1, 19, 35), sample::profile(2, 17, 45)].concat(),
            &[7],
        ),
        set(3, 7, 11, &[], &[2]),
        set(3, 10, 11, &[], &[2]),
        set(4, 20, CAMERA_MESSAGE, &vectors(&B), &[11]),
        section(101, false),
        set(3, 20, ORIGIN, &vectors(&GAP[..3]), &[2]),
        section(200, true),
        set(3, 6, 10, &sample::drawing(1, 20., 0xff333333), &[11]),
        set(3, 8, 12, &[3, 0, 9], &[11]),
        set(3, 9, 13, &64i32.to_le_bytes(), &[11]),
        set(
            3,
            10,
            11,
            &[sample::profile(1, 17, 55), sample::profile(2, 19, 85)].concat(),
            &[8],
        ),
        set(
            3,
            7,
            11,
            &[sample::profile(1, 17, 50), sample::profile(2, 18, 80)].concat(),
            &[10],
        ),
        set(3, 10, 11, &[], &[2]),
        set(3, 7, 11, &[], &[2]),
        set(4, 20, CAMERA_MESSAGE, &vectors(&C), &[11, 9]),
        array(5, 3, 9, 2, &error_record(9), &[]),
        array(5, 4, 9, 1, &error_record(51), &[]),
        saved(2, 1280),
        destroy(2),
        create(21, CAMERA_CLASS),
        set(3, 21, NAME, &LATE.to_le_bytes(), &[1]),
        set(4, 21, CAMERA_MESSAGE, &vectors(&D), &[2]),
        section(201, false),
        section(300, true),
        set(3, 6, 10, &sample::drawing(1, 30., 0xff444444), &[17]),
        set(3, 7, 11, &sample::profile(2, 17, 140), &[12]),
        set(3, 10, 11, &sample::profile(2, 17, 150), &[14]),
        set(3, 7, 11, &[], &[2]),
        set(3, 10, 11, &[], &[2]),
        set(4, 20, CAMERA_MESSAGE, &vectors(&D), &[15]),
        saved(21, 3072),
        destroy(21),
        section(301, false),
        create(22, CAMERA_CLASS),
        set(3, 22, NAME, &FUTURE.to_le_bytes(), &[1]),
        set(4, 22, CAMERA_MESSAGE, &vectors(&A), &[2]),
    ];
    f
}

pub(super) fn source() -> Vec<u8> {
    fixture().bytes_with_cameras(
        &capture::catalogs(),
        &[(FUTURE, 22), (LATE, 21), (MAIN, 20)],
    )
}

pub(super) fn zone_catalogs() -> Vec<u8> {
    let all = capture::catalogs();
    let mut reader = Reader::new(&all);
    let count = reader.count().unwrap();
    for _ in 0..count {
        reader.take(16).unwrap();
        reader.blob().unwrap();
        reader.u32().unwrap();
        for _ in 0..reader.count().unwrap() {
            reader.blob().unwrap();
            reader.take(3).unwrap();
        }
    }
    let mut out = all[..reader.pos].to_vec();
    for value in [u32::MAX, u32::MAX, 0] {
        word(&mut out, value);
    }
    out
}

pub(super) fn empty_source() -> Vec<u8> {
    let mut f = fixture();
    f.initial.clear();
    f.records.clear();
    f.ids = vec![1, 7, 10];
    f.alive = f.ids.clone();
    f.events = vec![
        9u32.to_le_bytes().to_vec(),
        create(1, 0),
        create(20, CAMERA_CLASS),
        set(3, 20, NAME, &MAIN.to_le_bytes(), &[1]),
        set(4, 20, CAMERA_MESSAGE, &vectors(&A), &[2]),
        create(7, 11),
        create(10, 11),
        set(3, 10, 11, &capture::RIGHT_17_12, &[1]),
        set(3, 7, 11, &capture::LEFT_17_10, &[3]),
        section(100, true),
        set(
            3,
            7,
            11,
            &[sample::profile(2, 17, 40), capture::LEFT_17_50.to_vec()].concat(),
            &[2],
        ),
        set(
            3,
            10,
            11,
            &[sample::profile(2, 17, 45), capture::RIGHT_17_55.to_vec()].concat(),
            &[4],
        ),
        set(3, 7, 11, &[], &[2]),
        set(3, 10, 11, &[], &[2]),
        section(101, false),
        section(200, true),
        set(4, 20, CAMERA_MESSAGE, &vectors(&B), &[12]),
        set(
            3,
            7,
            11,
            &[sample::profile(2, 17, 140), sample::profile(1, 17, 160)].concat(),
            &[5],
        ),
        set(
            3,
            10,
            11,
            &[sample::profile(2, 17, 150), sample::profile(1, 17, 170)].concat(),
            &[5],
        ),
        set(3, 7, 11, &[], &[2]),
        set(3, 10, 11, &[], &[2]),
        section(201, false),
        section(300, true),
        set(4, 20, CAMERA_MESSAGE, &vectors(&C), &[7]),
        set(3, 7, 11, &sample::profile(2, 17, 200), &[5]),
        set(3, 10, 11, &sample::profile(2, 17, 210), &[5]),
        set(3, 7, 11, &[], &[2]),
        set(3, 10, 11, &[], &[2]),
        section(301, false),
    ];
    f.bytes_with_cameras(&zone_catalogs(), &[(MAIN, 20)])
}

pub(super) fn single_source() -> Vec<u8> {
    let mut f = fixture();
    f.initial.clear();
    f.records.clear();
    f.ids = vec![1, 7];
    f.alive = f.ids.clone();
    f.events = vec![
        9u32.to_le_bytes().to_vec(),
        create(1, 0),
        create(20, CAMERA_CLASS),
        set(3, 20, NAME, &MAIN.to_le_bytes(), &[1]),
        set(4, 20, CAMERA_MESSAGE, &vectors(&A), &[2]),
        create(7, 11),
        set(3, 7, 11, &capture::LEFT_17_10, &[1]),
        section(100, true),
        set(
            3,
            7,
            11,
            &[sample::profile(2, 17, 40), capture::LEFT_17_50.to_vec()].concat(),
            &[2],
        ),
        set(3, 7, 11, &[], &[1]),
        section(101, false),
        section(200, true),
        set(4, 20, CAMERA_MESSAGE, &vectors(&B), &[8]),
        set(
            3,
            7,
            11,
            &[sample::profile(2, 17, 140), sample::profile(1, 17, 160)].concat(),
            &[4],
        ),
        set(3, 7, 11, &[], &[1]),
        section(201, false),
        section(300, true),
        set(4, 20, CAMERA_MESSAGE, &vectors(&C), &[5]),
        set(3, 7, 11, &sample::profile(2, 17, 200), &[4]),
        set(3, 7, 11, &[], &[1]),
        section(301, false),
    ];
    let mut catalog = Vec::new();
    word(&mut catalog, 1);
    catalog.extend((7 * 0x111u64).to_le_bytes());
    catalog.extend(0x123456789abcdef0u64.to_le_bytes());
    word(&mut catalog, 5);
    catalog.extend(b"Zone\0");
    word(&mut catalog, 7);
    word(&mut catalog, 1);
    word(&mut catalog, 8);
    catalog.extend(b"Event17\0");
    catalog.extend(17u16.to_le_bytes());
    catalog.push(1);
    for v in [u32::MAX, u32::MAX, 0] {
        word(&mut catalog, v);
    }
    f.bytes_with_cameras(&catalog, &[(MAIN, 20)])
}

pub(super) fn later_profile_source() -> Vec<u8> {
    let mut f = fixture();
    f.records.retain(|r| r.id == 1 || r.id == 20);
    f.initial = (0..f.records.len()).collect();
    f.ids = vec![1, 7, 10];
    f.alive = f.ids.clone();
    f.events = vec![
        9u32.to_le_bytes().to_vec(),
        section(100, true),
        section(101, false),
        create(7, 11),
        create(10, 11),
        set(3, 7, 11, &capture::LEFT_17_10, &[2]),
        set(3, 10, 11, &capture::RIGHT_17_12, &[2]),
        section(200, true),
        set(4, 20, CAMERA_MESSAGE, &vectors(&B), &[8]),
        set(3, 7, 11, &sample::profile(2, 17, 40), &[4]),
        set(3, 10, 11, &sample::profile(2, 17, 45), &[4]),
        set(3, 7, 11, &[], &[2]),
        set(3, 10, 11, &[], &[2]),
        section(201, false),
    ];
    f.bytes_with_cameras(&zone_catalogs(), &[(MAIN, 20)])
}
