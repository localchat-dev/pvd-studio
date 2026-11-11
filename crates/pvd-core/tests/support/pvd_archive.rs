#![allow(dead_code)]

use std::path::{Path, PathBuf};

fn word(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend(value.to_le_bytes());
}
fn wide(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend(value.to_le_bytes());
}
fn words(values: &[u32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}
fn blob(bytes: &mut Vec<u8>, value: &[u8]) {
    word(bytes, value.len() as u32);
    bytes.extend(value);
}

pub fn stored(member: &str, payload: &[u8]) -> Vec<u8> {
    let mut crc = u32::MAX;
    for &byte in payload {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    let crc = !crc;
    let length = payload.len() as u32;
    let mut bytes = words(&[0x04034b50]);
    for value in [20u16, 0, 0, 0, 0] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.extend(words(&[crc, length, length]));
    bytes.extend((member.len() as u16).to_le_bytes());
    bytes.extend(0u16.to_le_bytes());
    bytes.extend(member.as_bytes());
    bytes.extend(payload);
    let central = bytes.len() as u32;
    word(&mut bytes, 0x02014b50);
    for value in [20u16, 20, 0, 0, 0, 0] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.extend(words(&[crc, length, length]));
    for value in [member.len() as u16, 0, 0, 0, 0] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.extend(words(&[0, 0]));
    bytes.extend(member.as_bytes());
    let directory_bytes = bytes.len() as u32 - central;
    word(&mut bytes, 0x06054b50);
    for value in [0u16, 0, 1, 1] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.extend(words(&[directory_bytes, central]));
    bytes.extend(0u16.to_le_bytes());
    bytes
}

pub fn sdk() -> Vec<u8> {
    let mut events = Vec::new();
    for (handle, text) in ["", "physx3", "PxScene", "Value", "PvdU32", "frame"]
        .into_iter()
        .enumerate()
    {
        let mut event = vec![1];
        blob(&mut event, &[text.as_bytes(), &[0]].concat());
        word(&mut event, handle as u32);
        events.push(event);
    }
    let mut class = vec![2];
    class.extend(words(&[1, 2]));
    events.push(class);
    let mut property = vec![4];
    property.extend(words(&[1, 2, 3, 0, 1, 4]));
    property.push(1);
    word(&mut property, 0);
    events.push(property);
    let mut instance = vec![6];
    instance.extend(words(&[1, 2]));
    wide(&mut instance, 1);
    events.push(instance);
    for (frame, value) in [11u32, 22, 33].into_iter().enumerate() {
        for begin in [true, false] {
            if !begin {
                let mut event = vec![7];
                wide(&mut event, 1);
                event.extend(words(&[3, 1, 4, 1, 4, value]));
                events.push(event);
            }
            let mut event = vec![if begin { 18 } else { 19 }];
            wide(&mut event, 1);
            word(&mut event, 5);
            wide(&mut event, 10 * (frame as u64 + 1) + u64::from(!begin));
            events.push(event);
        }
    }
    let payload: Vec<_> = events.iter().flatten().copied().collect();
    let mut bytes = words(&[pvd_protocol::STREAM_MAGIC, 1]);
    wide(&mut bytes, 1);
    wide(&mut bytes, 1);
    word(&mut bytes, 1);
    bytes.extend(words(&[payload.len() as u32, events.len() as u32]));
    wide(&mut bytes, 1);
    wide(&mut bytes, 0);
    bytes.extend(payload);
    bytes
}

pub fn native() -> Vec<u8> {
    native_capture(false)
}

pub fn native_profile() -> Vec<u8> {
    native_capture(true)
}

fn native_capture(profile: bool) -> Vec<u8> {
    let mut strings = vec![
        "",
        "physx3",
        "PxScene",
        "Value",
        "PvdU32",
        "_debugger_",
        "SectionEventData",
        "frame",
    ];
    if profile {
        strings.extend(["PvdU8", "PxProfileZone", "events"]);
    }
    let class_count = if profile { 5 } else { 3 };
    let mut metadata = words(&[
        1,
        class_count,
        strings.len() as u32 - 1,
        strings.len() as u32,
    ]);
    for (handle, text) in strings.into_iter().enumerate().skip(1) {
        word(&mut metadata, handle as u32);
        blob(&mut metadata, &[text.as_bytes(), &[0]].concat());
    }
    word(&mut metadata, if profile { 2 } else { 1 });
    metadata.extend(words(&[1, 1, 2, 0, 0, 1, 1, 4, 1, 0, 0, 0, 0, 3]));
    if profile {
        metadata.extend(words(&[1, 5, 9, 0, 0, 3, 1, 8, 2, 1, 0, 0, 0, 10]));
    }
    word(&mut metadata, class_count);
    let mut classes = vec![
        (0, 1, 2, 4, vec![0]),
        (1, 1, 4, 4, vec![]),
        (2, 5, 6, 24, vec![]),
    ];
    if profile {
        classes.extend([(3, 1, 8, 1, vec![]), (4, 5, 9, 12, vec![1])]);
    }
    for (id, namespace, name, size, properties) in classes {
        metadata.extend(words(&[
            1,
            namespace,
            name,
            id,
            u32::MAX,
            u32::MAX,
            u32::MAX,
        ]));
        metadata.extend([0, 0]);
        metadata.extend(words(&[
            size,
            size,
            4,
            size,
            size,
            4,
            properties.len() as u32,
        ]));
        metadata.extend(words(&properties));
        metadata.extend(words(&[0, 0]));
    }
    word(&mut metadata, 0);
    let mut events = vec![words(&[9]), words(&[1, 1, 0])];
    let mut previous = 1;
    let mut profile_previous = 0;
    if profile {
        profile_previous = events.len();
        events.push(words(&[1, 3, 4]));
    }
    let mut rows = Vec::new();
    for (frame, value) in [11u32, 22, 33].into_iter().enumerate() {
        let ticks = 10 * (frame as u64 + 1);
        let start = events.len() as u64;
        for begin in [true, false] {
            if !begin {
                let ordinal = events.len();
                events.push(words(&[3, 1, 0, 4, value, 1, (ordinal - previous) as u32]));
                previous = ordinal;
                if profile && frame != 0 {
                    append_profile(&mut events, &mut profile_previous, 1, ticks - 6, ticks - 4);
                }
            }
            let mut payload = Vec::new();
            wide(&mut payload, 0x111);
            wide(&mut payload, ticks + u64::from(!begin));
            word(&mut payload, 7);
            payload.extend([u8::from(begin), 0, 0, 0]);
            let mut event = words(&[8, 2]);
            blob(&mut event, &payload);
            if !begin {
                rows.push([ticks, start, ticks + 1, events.len() as u64]);
            }
            events.push(event);
        }
    }
    if profile {
        append_profile(&mut events, &mut profile_previous, 2, 34, 36);
    }
    let mut comm = words(&[1]);
    wide(&mut comm, 1);
    wide(&mut comm, 1);
    let instances = if profile {
        vec![(1, 0x111), (3, 0x333)]
    } else {
        vec![(1, 0x111)]
    };
    word(&mut comm, instances.len() as u32);
    for &(id, source) in &instances {
        word(&mut comm, id);
        wide(&mut comm, source);
        word(&mut comm, id);
        comm.extend([0; 10]);
    }
    word(&mut comm, instances.len() as u32);
    for (id, source) in instances {
        wide(&mut comm, source);
        word(&mut comm, id);
    }
    comm.extend(words(&[0, 1]));
    wide(&mut comm, 0x111);
    comm.extend(words(&[7, rows.len() as u32]));
    for row in rows {
        for value in row {
            wide(&mut comm, value);
        }
    }
    word(&mut comm, u32::from(profile));
    if profile {
        wide(&mut comm, 0x333);
        wide(&mut comm, 0);
        blob(&mut comm, b"Simulation\0");
        comm.extend(words(&[3, 2]));
        for (event, name) in [(1u16, b"Step\0".as_slice()), (2, b"Flush\0".as_slice())] {
            blob(&mut comm, name);
            comm.extend(event.to_le_bytes());
            comm.push(1);
        }
    }
    comm.extend(words(&[u32::MAX, u32::MAX, 0]));
    let mut page = words(&[events.len() as u32]);
    let mut offset = 0;
    for event in &events {
        word(&mut page, offset);
        offset += event.len() as u32;
    }
    page.extend(events.concat());
    let mut bytes = words(&[
        pvd_core::native::MAGIC,
        1,
        metadata.len() as u32,
        comm.len() as u32,
    ]);
    bytes.extend(metadata);
    bytes.extend(comm);
    bytes.resize((bytes.len() / 4096 + 1) * 4096, 0);
    wide(&mut bytes, events.len() as u64);
    word(&mut bytes, 1);
    wide(&mut bytes, 0);
    wide(&mut bytes, 4096);
    bytes.extend(words(&[events.len() as u32, page.len() as u32, 1]));
    wide(&mut bytes, 0);
    wide(&mut bytes, 0);
    bytes.extend(words(&[4, 0, 0]));
    bytes.resize((bytes.len() / 4096 + 1) * 4096 + 4096, 0);
    bytes.extend(page);
    bytes
}

fn append_profile(
    events: &mut Vec<Vec<u8>>,
    previous: &mut usize,
    event: u16,
    start: u64,
    end: u64,
) {
    let mut buffer = Vec::new();
    for (kind, tick) in [(1, start), (2, end)] {
        buffer.extend([kind, 3]);
        buffer.extend(event.to_le_bytes());
        word(&mut buffer, 7);
        buffer.extend([9, 0, 0]);
        wide(&mut buffer, tick);
    }
    let ordinal = events.len();
    let mut set = words(&[3, 3, 1]);
    blob(&mut set, &buffer);
    set.extend(words(&[1, (ordinal - *previous) as u32]));
    events.push(set);
    *previous = ordinal;
}

pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(label: &str) -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../output/pvd-archive-consumer-tests")
            .join(format!(
                "{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    pub fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
    pub fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.path(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }
    pub fn history(&self) -> pvd_core::live::HistoryConfig {
        pvd_core::live::HistoryConfig {
            directory: self.0.join("history"),
            disk_budget_bytes: 64 << 20,
            segment_target_bytes: 1 << 20,
            segment_frames: 1,
            min_free_disk_bytes: 0,
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        fn remove_empty_or_file(path: &Path) {
            for entry in std::fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    assert!(std::fs::read_dir(entry.path()).unwrap().next().is_none());
                    std::fs::remove_dir(entry.path()).unwrap();
                } else {
                    std::fs::remove_file(entry.path()).unwrap();
                }
            }
            std::fs::remove_dir(path).unwrap();
        }
        remove_empty_or_file(&self.0);
    }
}
