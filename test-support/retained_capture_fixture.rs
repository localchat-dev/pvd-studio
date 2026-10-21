fn text(value: &str) -> Vec<u8> {
    let mut bytes = (value.len() as u32 + 1).to_le_bytes().to_vec();
    bytes.extend(value.as_bytes());
    bytes.push(0);
    bytes
}

fn string(id: u32, value: &str) -> Vec<u8> {
    let mut bytes = vec![1];
    bytes.extend(text(value));
    bytes.extend(id.to_le_bytes());
    bytes
}

fn instance(class: u32, id: u64) -> Vec<u8> {
    let mut bytes = vec![6];
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(class.to_le_bytes());
    bytes.extend(id.to_le_bytes());
    bytes
}

fn property(id: u64, name: u32, datatype: u32, data: &[u8], count: u32) -> Vec<u8> {
    let mut bytes = vec![7];
    bytes.extend(id.to_le_bytes());
    for value in [name, 1, datatype, count, data.len() as u32] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.extend(data);
    bytes
}

fn group(events: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = (events.iter().map(Vec::len).sum::<usize>() as u32)
        .to_le_bytes()
        .to_vec();
    bytes.extend((events.len() as u32).to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    for event in events {
        bytes.extend(event);
    }
    bytes
}

fn frame_groups(first: usize, count: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    for frame in first..first + count {
        let mut profile = Vec::new();
        for (kind, timestamp) in [(1, frame as u64 * 100 + 10), (2, frame as u64 * 100 + 20)] {
            profile.extend([kind, 3, 1, 0]);
            profile.extend(7u32.to_le_bytes());
            profile.extend([9, 0, 0]);
            profile.extend(timestamp.to_le_bytes());
        }
        let memory = [2, 0, 20, 1, 2, 13, (frame % 200 + 40) as u8];
        let mut end = vec![19];
        end.extend(9u64.to_le_bytes());
        end.extend(6u32.to_le_bytes());
        end.extend(((frame as u64 + 1) * 100).to_le_bytes());
        bytes.extend(group(&[
            property(10, 4, 5, &profile, profile.len() as u32),
            property(11, 4, 5, &memory, memory.len() as u32),
            property(9, 8, 9, &(frame as u32).to_le_bytes(), 1),
            end,
        ]));
    }
    bytes
}

pub fn capture(frames: usize) -> Vec<u8> {
    let mut bytes = pvd_protocol::STREAM_MAGIC.to_le_bytes().to_vec();
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(1_000_000u64.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    let mut events: Vec<_> = [
        (1, "physx3"),
        (2, "PxProfileZone"),
        (3, "PxProfileMemoryEventBuffer"),
        (4, "events"),
        (5, "PvdU8"),
        (6, "frame"),
        (7, "PxScene"),
        (8, "Counter"),
        (9, "PvdU32"),
    ]
    .into_iter()
    .map(|(id, value)| string(id, value))
    .collect();
    events.extend([instance(2, 10), instance(3, 11), instance(7, 9)]);
    let mut zone = vec![24];
    zone.extend(10u64.to_le_bytes());
    zone.extend(text("Demand fixture"));
    events.push(zone);
    let mut event = vec![25];
    event.extend(10u64.to_le_bytes());
    event.extend(text("work"));
    event.extend(1u16.to_le_bytes());
    event.push(1);
    events.push(event);
    let mut memory = Vec::new();
    for (id, value) in [(1u32, "Allocation"), (2, "fixture.cpp")] {
        memory.extend(1u16.to_le_bytes());
        memory.extend(text(value));
        memory.extend(id.to_le_bytes());
    }
    events.push(property(11, 4, 5, &memory, memory.len() as u32));
    bytes.extend(group(&events));
    bytes.extend(frame_groups(0, frames));
    bytes
}
