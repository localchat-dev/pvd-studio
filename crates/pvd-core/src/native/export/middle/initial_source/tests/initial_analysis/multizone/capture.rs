use super::super::{Fixture, Record, array, destroy, error_record, sample, saved, set, word};

pub(super) const LEFT_17_10: [u8; 26] = [
    1, 15, 17, 0, 99, 0, 0, 0, 17, 1, 0, 0, 0, 0, 0, 0, 6, 7, 10, 0, 0, 0, 0, 0, 0, 0,
];
pub(super) const RIGHT_17_12: [u8; 26] = [
    1, 15, 17, 0, 99, 0, 0, 0, 17, 1, 0, 0, 0, 0, 0, 0, 6, 7, 12, 0, 0, 0, 0, 0, 0, 0,
];
pub(super) const LEFT_18_30: [u8; 26] = [
    1, 15, 18, 0, 99, 0, 0, 0, 17, 1, 0, 0, 0, 0, 0, 0, 6, 7, 30, 0, 0, 0, 0, 0, 0, 0,
];
pub(super) const RIGHT_19_35: [u8; 26] = [
    1, 15, 19, 0, 99, 0, 0, 0, 17, 1, 0, 0, 0, 0, 0, 0, 6, 7, 35, 0, 0, 0, 0, 0, 0, 0,
];
pub(super) const LEFT_17_50: [u8; 26] = [
    1, 15, 17, 0, 99, 0, 0, 0, 17, 1, 0, 0, 0, 0, 0, 0, 6, 7, 50, 0, 0, 0, 0, 0, 0, 0,
];
pub(super) const RIGHT_17_55: [u8; 26] = [
    1, 15, 17, 0, 99, 0, 0, 0, 17, 1, 0, 0, 0, 0, 0, 0, 6, 7, 55, 0, 0, 0, 0, 0, 0, 0,
];

fn blob(out: &mut Vec<u8>, bytes: &[u8]) {
    word(out, bytes.len() as u32);
    out.extend(bytes);
}

fn section(tick: u64, begin: bool) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend(0x111u64.to_le_bytes());
    payload.extend(tick.to_le_bytes());
    word(&mut payload, 18);
    payload.extend([u8::from(begin), 0, 0, 0]);
    let mut out = Vec::new();
    word(&mut out, 8);
    word(&mut out, 5);
    blob(&mut out, &payload);
    out
}

pub(super) fn right_buffer() -> Vec<u8> {
    let mut body = sample::buffer(&[]);
    body[4..28].fill(0xa7);
    body
}

pub(super) fn fixture() -> Fixture {
    let mut f = sample::fixture_with_analysis();
    f.initial.insert(0, f.records.len());
    f.records.push(Record {
        id: 10,
        class: 11,
        position: 2560,
        body: right_buffer(),
        deleted: false,
    });
    f.ids.insert(0, 10);
    f.alive.insert(0, 10);
    f.events = vec![
        9u32.to_le_bytes().to_vec(),
        set(3, 10, 11, &RIGHT_17_12, &[1]),
        set(3, 7, 11, &LEFT_17_10, &[2]),
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
        section(101, false),
        section(200, true),
        set(3, 6, 10, &sample::drawing(1, 20., 0xff333333), &[9]),
        set(3, 8, 12, &[3, 0, 9], &[9]),
        set(3, 9, 13, &64i32.to_le_bytes(), &[9]),
        set(
            3,
            10,
            11,
            &[sample::profile(1, 17, 55), sample::profile(2, 19, 85)].concat(),
            &[6],
        ),
        set(
            3,
            7,
            11,
            &[sample::profile(1, 17, 50), sample::profile(2, 18, 80)].concat(),
            &[8],
        ),
        set(3, 10, 11, &[], &[2]),
        set(3, 7, 11, &[], &[2]),
        array(5, 3, 9, 2, &error_record(9), &[]),
        array(5, 4, 9, 1, &error_record(51), &[]),
        saved(2, 1280),
        destroy(2),
        section(201, false),
        section(300, true),
        set(3, 6, 10, &sample::drawing(1, 30., 0xff444444), &[13]),
        set(3, 7, 11, &sample::profile(2, 17, 140), &[8]),
        set(3, 10, 11, &sample::profile(2, 17, 150), &[10]),
        set(3, 7, 11, &[], &[2]),
        set(3, 10, 11, &[], &[2]),
        section(301, false),
    ];
    f
}

pub(super) fn catalogs() -> Vec<u8> {
    let mut out = Vec::new();
    word(&mut out, 2);
    for (id, opaque, names) in [
        (
            10u32,
            0x0123456789abcdefu64,
            [
                (b"Right19\0".as_slice(), 19u16, 1u8),
                (b"Right17\0".as_slice(), 17, 0),
            ],
        ),
        (
            7,
            0xfedcba9876543210,
            [
                (b"Left18\0".as_slice(), 18, 0),
                (b"Left17\0".as_slice(), 17, 1),
            ],
        ),
    ] {
        out.extend((u64::from(id) * 0x111).to_le_bytes());
        out.extend(opaque.to_le_bytes());
        blob(&mut out, b"Shared\0");
        word(&mut out, id);
        word(&mut out, 2);
        for (name, event, enabled) in names {
            blob(&mut out, name);
            out.extend(event.to_le_bytes());
            out.push(enabled);
        }
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
    fixture().bytes_with_catalogs(&catalogs())
}

pub(super) fn expected_middle(saved_position: u64) -> Vec<Vec<u8>> {
    vec![
        9u32.to_le_bytes().to_vec(),
        set(3, 7, 11, &LEFT_18_30, &[1]),
        set(3, 10, 11, &RIGHT_19_35, &[2]),
        section(200, true),
        set(3, 6, 10, &sample::drawing(1, 20., 0xff333333), &[4]),
        set(3, 8, 12, &[3, 0, 9], &[5]),
        set(3, 9, 13, &64i32.to_le_bytes(), &[6]),
        set(
            3,
            10,
            11,
            &[sample::profile(1, 17, 55), sample::profile(2, 19, 85)].concat(),
            &[5],
        ),
        set(
            3,
            7,
            11,
            &[sample::profile(1, 17, 50), sample::profile(2, 18, 80)].concat(),
            &[7],
        ),
        set(3, 10, 11, &[], &[2]),
        set(3, 7, 11, &[], &[2]),
        array(5, 3, 9, 2, &error_record(9), &[]),
        array(5, 4, 9, 1, &error_record(51), &[]),
        saved(2, saved_position),
        destroy(2),
        section(201, false),
        section(300, true),
        set(3, 6, 10, &sample::drawing(1, 30., 0xff444444), &[13]),
        set(3, 7, 11, &sample::profile(2, 17, 140), &[8]),
        set(3, 10, 11, &sample::profile(2, 17, 150), &[10]),
        set(3, 7, 11, &[], &[2]),
        set(3, 10, 11, &[], &[2]),
        section(301, false),
    ]
}

pub(super) fn expected_last() -> Vec<Vec<u8>> {
    vec![
        9u32.to_le_bytes().to_vec(),
        set(3, 7, 11, &LEFT_17_50, &[1]),
        set(3, 10, 11, &RIGHT_17_55, &[2]),
        section(300, true),
        set(3, 6, 10, &sample::drawing(1, 30., 0xff444444), &[4]),
        set(3, 7, 11, &sample::profile(2, 17, 140), &[4]),
        set(3, 10, 11, &sample::profile(2, 17, 150), &[4]),
        set(3, 7, 11, &[], &[2]),
        set(3, 10, 11, &[], &[2]),
        section(301, false),
    ]
}
