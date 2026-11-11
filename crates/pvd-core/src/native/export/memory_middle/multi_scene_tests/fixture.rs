use super::*;

#[derive(Clone, Copy)]
pub(super) enum Step<'a> {
    Begin(u32, u64),
    End(u32, u64),
    Buffer(&'a [u8]),
    Totals(&'a [i32]),
    Raw(&'a [u8]),
}

pub(super) const OLD: &[u8] = &[2, 0, 64, 21, 22, 7, 42];
pub(super) const FIRST: &[u8] = &[3, 0, 42, 2, 0, 8, 23, 24, 8, 9];
pub(super) const STEPS: &[Step<'static>] = &[
    Step::Begin(1, 100),
    Step::End(1, 110),
    Step::Begin(4, 120),
    Step::Buffer(FIRST),
    Step::Totals(&[0, 24]),
    Step::End(4, 130),
    Step::Totals(&[-7, 24]),
    Step::Begin(4, 200),
    Step::Buffer(&[3, 0, 9]),
    Step::Begin(1, 210),
    Step::Buffer(&[2, 0, 4, 25, 26, 9, 10]),
    Step::Totals(&[-7, 16, 4]),
    Step::End(4, 220),
    Step::Buffer(&[2, 0, 12, 21, 22, 7, 11]),
    Step::Totals(&[5, 16, 4]),
    Step::End(1, 230),
    Step::Totals(&[5, 17, 4]),
    Step::Begin(4, 300),
    Step::Buffer(&[3, 0, 10]),
    Step::Totals(&[5, 17, 0]),
    Step::Buffer(&[]),
    Step::End(4, 310),
    Step::Begin(1, 320),
    Step::Buffer(&[3, 0, 11, 2, 0, 6, 23, 24, 8, 12]),
    Step::Totals(&[-7, 23, 0]),
    Step::End(1, 330),
    Step::Begin(1, 400),
    Step::Buffer(&[2, 0, 2, 25, 26, 9, 13]),
    Step::Begin(4, 410),
    Step::Totals(&[-7, 23, 2]),
    Step::End(1, 420),
    Step::Buffer(&[3, 0, 12]),
    Step::Totals(&[-7, 17, 2]),
    Step::End(4, 430),
    Step::Buffer(&[3, 0, 13]),
    Step::Totals(&[-7, 17, 0]),
];

fn word(out: &mut Vec<u8>, value: u32) {
    out.extend(value.to_le_bytes());
}
fn wide(out: &mut Vec<u8>, value: u64) {
    out.extend(value.to_le_bytes());
}
fn blob(out: &mut Vec<u8>, bytes: &[u8]) {
    word(out, bytes.len() as u32);
    out.extend(bytes);
}
fn source(id: u32) -> u64 {
    match id {
        1 => 0x111,
        2 => 0x222,
        4 => 0x444,
        _ => panic!("unexpected source ID"),
    }
}
fn create(id: u32, class: u32) -> Vec<u8> {
    [1u32, id, class]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect()
}
fn set(events: &mut Vec<Vec<u8>>, id: u32, property: u32, data: &[u8], previous: usize) {
    let mut bytes = Vec::new();
    for value in [3, id, property] {
        word(&mut bytes, value);
    }
    blob(&mut bytes, data);
    word(&mut bytes, 1);
    word(&mut bytes, (events.len() - previous) as u32);
    events.push(bytes);
}
fn section(id: u32, ticks: u64, begin: bool) -> Vec<u8> {
    let mut payload = Vec::new();
    wide(&mut payload, source(id));
    wide(&mut payload, ticks);
    word(&mut payload, 13);
    payload.extend([u8::from(begin), 0, 0, 0]);
    let mut bytes = Vec::new();
    word(&mut bytes, 8);
    word(&mut bytes, 5);
    blob(&mut bytes, &payload);
    bytes
}

pub(super) fn build(initial: bool, steps: &[Step<'_>]) -> Vec<u8> {
    let template = preserved::source(false);
    let raw = preserved::raw(&template);
    let object_len = u32::from_le_bytes(template[8..12].try_into().unwrap()) as usize;
    let objects = &template[16..16 + object_len];
    let mut events = vec![9u32.to_le_bytes().to_vec()];
    let mut previous = [0usize; 2];
    if !initial {
        for (id, class) in [(1, 4), (4, 4), (2, 8), (3, 9)] {
            events.push(create(id, class));
        }
        for (id, value, peer, created) in [(1, 7u32, 4u32, 1), (4, 11, 1, 2)] {
            set(&mut events, id, 0, &value.to_le_bytes(), created);
            set(&mut events, id, 1, &10u32.to_le_bytes(), created);
            let peers: Vec<_> = [2, 3, peer]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect();
            set(&mut events, id, 2, &peers, created);
        }
        previous[0] = events.len();
        set(&mut events, 2, 3, OLD, 3);
        previous[1] = events.len();
        set(&mut events, 3, 4, &preserved::totals(&[64, 16]), 4);
    }
    let mut starts = BTreeMap::new();
    let mut rows = BTreeMap::<u32, Vec<[u64; 4]>>::new();
    for step in steps {
        match *step {
            Step::Begin(id, ticks) => {
                assert!(starts.insert(id, (ticks, events.len() as u64)).is_none());
                events.push(section(id, ticks, true));
            }
            Step::End(id, ticks) => {
                let (start_ticks, start_event) = starts.remove(&id).unwrap();
                rows.entry(id).or_default().push([
                    start_ticks,
                    start_event,
                    ticks,
                    events.len() as u64,
                ]);
                events.push(section(id, ticks, false));
            }
            Step::Buffer(_) | Step::Totals(_) => {
                let (slot, data) = match step {
                    Step::Buffer(data) => (0, data.to_vec()),
                    Step::Totals(data) => (1, preserved::totals(data)),
                    _ => unreachable!(),
                };
                let ordinal = events.len();
                set(
                    &mut events,
                    2 + slot as u32,
                    3 + slot as u32,
                    &data,
                    previous[slot],
                );
                previous[slot] = ordinal;
            }
            Step::Raw(bytes) => events.push(bytes.to_vec()),
        }
    }
    assert!(starts.is_empty());
    let mut comm = Vec::new();
    word(&mut comm, 1);
    wide(&mut comm, 1);
    wide(&mut comm, 1);
    word(&mut comm, 3);
    for id in [1, 4, 2] {
        word(&mut comm, id);
        wide(&mut comm, source(id));
        word(&mut comm, id);
        comm.extend([0; 10]);
    }
    word(&mut comm, 3);
    for id in [4, 2, 1] {
        wide(&mut comm, source(id));
        word(&mut comm, id);
    }
    word(&mut comm, 0);
    word(&mut comm, rows.len() as u32);
    for (id, frames) in rows.into_iter().rev() {
        wide(&mut comm, source(id));
        word(&mut comm, 13);
        word(&mut comm, frames.len() as u32);
        for row in frames {
            for value in row {
                wide(&mut comm, value);
            }
        }
    }
    word(&mut comm, 0);
    comm.extend(raw.memory);
    let records = if initial {
        vec![
            (
                1u32,
                4u32,
                crate::native::container_tests::instance(1, 7, &[2, 3, 4])[12..].to_vec(),
            ),
            (
                4,
                4,
                crate::native::container_tests::instance(4, 11, &[2, 3, 1])[12..].to_vec(),
            ),
            (2, 8, preserved::body(OLD)),
            (3, 9, preserved::body(&preserved::totals(&[64, 16]))),
        ]
    } else {
        vec![]
    };
    let mut backing = Vec::new();
    word(&mut backing, records.len() as u32);
    let mut position = 128;
    for (id, _, body) in &records {
        word(&mut backing, *id);
        wide(&mut backing, position);
        position += body.len() as u64 + 12;
    }
    let snapshot_len = backing.len() as u32;
    backing.resize(128, 0);
    for (id, class, body) in &records {
        word(&mut backing, *class);
        word(&mut backing, *id);
        blob(&mut backing, body);
    }
    let mut page = Vec::new();
    word(&mut page, events.len() as u32);
    let mut offset = 0;
    for event in &events {
        word(&mut page, offset);
        offset += event.len() as u32;
    }
    page.extend(events.concat());
    assert!(backing.len() < 4096);
    backing.resize(4096, 0);
    backing.extend(&page);
    let mut output = Vec::new();
    for value in [MAGIC, 1, objects.len() as u32, comm.len() as u32] {
        word(&mut output, value);
    }
    output.extend(objects);
    output.extend(comm);
    output.resize((output.len() / 4096 + 1) * 4096, 0);
    wide(&mut output, events.len() as u64);
    word(&mut output, 1);
    wide(&mut output, 0);
    wide(&mut output, 4096);
    word(&mut output, events.len() as u32);
    word(&mut output, page.len() as u32);
    word(&mut output, 1);
    wide(&mut output, 0);
    wide(&mut output, 0);
    word(&mut output, snapshot_len);
    word(&mut output, 492);
    word(&mut output, 0);
    output.resize((output.len() / 4096 + 1) * 4096, 0);
    output.extend(backing);
    output
}
