use super::*;

pub(super) enum Step<'a> {
    Begin(u64),
    End(u64),
    Buffer(&'a [u8]),
    Totals(&'a [i32]),
    Raw(&'a [u8]),
}

fn word(out: &mut Vec<u8>, value: u32) {
    out.extend(value.to_le_bytes());
}
fn wide(out: &mut Vec<u8>, value: u64) {
    out.extend(value.to_le_bytes());
}
fn blob(out: &mut Vec<u8>, value: &[u8]) {
    word(out, value.len() as u32);
    out.extend(value);
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

pub(super) fn build(steps: &[Step<'_>], old_buffer: &[u8], old_totals: &[i32]) -> Vec<u8> {
    let original = preserved::source(false);
    let raw = preserved::raw(&original);
    let length = u32::from_le_bytes(original[8..12].try_into().unwrap()) as usize;
    let objects = &original[16..16 + length];
    let mut events = vec![9u32.to_le_bytes().to_vec()];
    let mut rows = Vec::new();
    let mut previous = [0usize; 2];
    let mut begin = None;
    for step in steps {
        let ordinal = events.len();
        match step {
            Step::Begin(ticks) => {
                assert!(begin.replace((*ticks, ordinal as u64)).is_none());
                events.push(section(*ticks, true));
            }
            Step::End(ticks) => {
                let (tick, event) = begin.take().unwrap();
                rows.push([tick, event, *ticks, ordinal as u64]);
                events.push(section(*ticks, false));
            }
            Step::Buffer(_) | Step::Totals(_) => {
                let (slot, bytes) = match step {
                    Step::Buffer(bytes) => (0, bytes.to_vec()),
                    Step::Totals(values) => (1, preserved::totals(values)),
                    _ => unreachable!(),
                };
                let mut event = Vec::new();
                for value in [3, 2 + slot as u32, 3 + slot as u32] {
                    word(&mut event, value);
                }
                blob(&mut event, &bytes);
                word(&mut event, 1);
                word(&mut event, (ordinal - previous[slot]) as u32);
                previous[slot] = ordinal;
                events.push(event);
            }
            Step::Raw(bytes) => events.push(bytes.to_vec()),
        }
    }
    assert!(begin.is_none());
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
    word(&mut comm, 2);
    for (source, id) in [(0x111, 1), (0x222, 2)] {
        wide(&mut comm, source);
        word(&mut comm, id);
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
    comm.extend(raw.memory);
    let records = [
        (1u32, 4u32, raw.initial[&1].1.clone()),
        (2, 8, preserved::body(old_buffer)),
        (3, 9, preserved::body(&preserved::totals(old_totals))),
    ];
    let mut backing = Vec::new();
    word(&mut backing, 3);
    let mut position = 128;
    for (id, _, body) in &records {
        word(&mut backing, *id);
        wide(&mut backing, position);
        position += (body.len() + 12) as u64;
    }
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
    for event in &events {
        page.extend(event);
    }
    assert!(backing.len() < 4096);
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
    word(&mut out, 0);
    out.resize((out.len() / 4096 + 1) * 4096, 0);
    out.extend(backing);
    out
}
