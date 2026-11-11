use super::*;

pub(super) const OLD: &[u8] = &[2, 0, 64, 37, 38, 7, 42];
pub(super) const FIRST: &[u8] = &[3, 0, 42, 2, 0, 8, 39, 40, 8, 9];

pub(super) struct Combined {
    pub(super) input: Input,
}

fn memory_body(id: u32, class: u32, data: &[u8], position: usize) -> SavedInput {
    let mut body = Vec::new();
    blob(&mut body, &[0x5a; 24]).unwrap();
    blob(&mut body, data).unwrap();
    let mut bytes = raw_words(&[class, id]);
    blob(&mut bytes, &body).unwrap();
    SavedInput {
        id,
        position,
        bytes,
        deleted: false,
    }
}

fn values(values: &[i32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

fn memory_set(events: &mut Vec<Vec<u8>>, previous: &mut [usize; 2], slot: usize, data: &[u8]) {
    let ordinal = events.len();
    events.push(set(
        3,
        20 + slot as u32,
        8 + slot as u32,
        data,
        &[(ordinal - previous[slot]) as u32],
    ));
    previous[slot] = ordinal;
}

fn append_memory(events: &mut Vec<Vec<u8>>, previous: &mut [usize; 2], frame: usize) {
    let buffers: &[&[u8]] = match frame {
        0 => &[],
        1 => &[FIRST],
        2 => &[&[3, 0, 9], &[2, 0, 4, 41, 42, 9, 10]],
        3 => &[&[2, 0, 12, 37, 38, 7, 11]],
        4 => &[&[3, 0, 10]],
        5 => &[&[3, 0, 11, 2, 0, 6, 39, 40, 8, 12]],
        6 => &[&[2, 0, 2, 41, 42, 9, 13]],
        7 => &[&[3, 0, 12]],
        _ => panic!("unexpected frame"),
    };
    for buffer in buffers {
        memory_set(events, previous, 0, buffer);
    }
    if frame != 0 {
        memory_set(events, previous, 1, &values(TOTALS[frame]));
    }
    if frame == 4 {
        memory_set(events, previous, 0, &[]);
    }
}

impl Combined {
    pub(super) fn new(initial: bool, gap_buffer: Option<&[u8]>) -> Self {
        let mut input = Input::new(initial);
        for (handle, text) in (30..).zip([
            "PvdU8",
            "PvdI32",
            "PxProfileMemoryEventBuffer",
            "events",
            "_debugger",
            "MemoryEventTotals",
            "totals",
            "Allocation",
            "source.cpp",
            "Other",
            "other.cpp",
            "Future",
            "future.cpp",
        ]) {
            input.metadata.strings.insert(handle, text.into());
        }
        for (id, class_name, size, properties) in [
            (6, name(1, 30), 1, vec![]),
            (7, name(1, 31), 4, vec![]),
            (8, name(11, 32), 24, vec![8]),
            (9, name(34, 35), 24, vec![9]),
        ] {
            assert!(input.metadata.classes[id].is_none());
            input.metadata.classes[id] = Some(Class {
                name: class_name,
                base: u32::MAX,
                packing: ClassPacking::default(),
                size,
                properties,
                dynamic: id >= 8,
                has_pointers: false,
                string_offsets: vec![],
            });
        }
        for (owner, label, datatype, datatype_id) in [
            (name(11, 32), 33, name(1, 30), 6),
            (name(34, 35), 36, name(1, 31), 7),
        ] {
            input.metadata.properties.push(Some(Property {
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
        if initial {
            input.saved.extend([
                memory_body(20, 8, OLD, 512),
                memory_body(21, 9, &values(&[64, 16]), 640),
            ]);
            input.initial[..4].copy_from_slice(&5u32.to_le_bytes());
            for (id, position) in [(20, 512), (21, 640)] {
                w32(&mut input.initial, id);
                w64(&mut input.initial, position);
            }
        }
        let original = std::mem::take(&mut input.events);
        let ends: BTreeMap<_, _> = input
            .rows
            .values()
            .flatten()
            .map(|row| row[3] as usize)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .enumerate()
            .map(|(frame, end)| (end, frame))
            .collect();
        let mut remap = vec![0; original.len()];
        let mut previous = [0; 2];
        for (old, bytes) in original.into_iter().enumerate() {
            if let Some(&frame) = ends.get(&old) {
                append_memory(&mut input.events, &mut previous, frame);
            }
            remap[old] = input.events.len();
            let mut reader = Reader::new(&bytes);
            let kind = reader.u32().unwrap();
            let bytes = if matches!(kind, 3 | 4) {
                let id = reader.u32().unwrap();
                let property = reader.u32().unwrap();
                let data = reader.blob().unwrap();
                let undo: Vec<_> = (0..reader.count().unwrap())
                    .map(|_| {
                        let distance = reader.u32().unwrap() as usize;
                        assert!(distance > 0 && distance <= old);
                        (remap[old] - remap[old - distance]) as u32
                    })
                    .collect();
                reader.end().unwrap();
                set(kind, id, property, data, &undo)
            } else {
                bytes
            };
            input.events.push(bytes);
            if old == 0 && !initial {
                for (slot, class) in [8, 9].into_iter().enumerate() {
                    previous[slot] = input.events.len();
                    input.events.push(raw_words(&[1, 20 + slot as u32, class]));
                }
                memory_set(&mut input.events, &mut previous, 0, OLD);
                memory_set(&mut input.events, &mut previous, 1, &values(&[64, 16]));
            }
            match ends.get(&old) {
                Some(1) => {
                    memory_set(&mut input.events, &mut previous, 1, &values(&[-7, 24]));
                    if let Some(buffer) = gap_buffer {
                        memory_set(&mut input.events, &mut previous, 0, buffer);
                    }
                }
                Some(3) => {
                    memory_set(&mut input.events, &mut previous, 1, &values(&[5, 17, 4]));
                }
                Some(7) => {
                    memory_set(&mut input.events, &mut previous, 0, &[3, 0, 13]);
                    memory_set(&mut input.events, &mut previous, 1, &values(&[-7, 17, 0]));
                }
                _ => {}
            }
        }
        for row in input.rows.values_mut().flatten() {
            row[1] = remap[row[1] as usize] as u64;
            row[3] = remap[row[3] as usize] as u64;
        }
        Self { input }
    }

    pub(super) fn bytes(&self) -> Vec<u8> {
        let source = self.input.bytes();
        let metadata_end = 16 + u32::from_le_bytes(source[8..12].try_into().unwrap()) as usize;
        let old_comm_len = u32::from_le_bytes(source[12..16].try_into().unwrap()) as usize;
        let mut comm = raw_words(&[1]);
        w64(&mut comm, 3);
        w64(&mut comm, 7);
        w32(&mut comm, 3);
        for (id, identity) in [(1, 0x111), (2, 0x222), (20, 0xeee)] {
            w32(&mut comm, id);
            w64(&mut comm, identity);
            w32(&mut comm, id);
            comm.extend([0; 10]);
        }
        w32(&mut comm, 3);
        for (id, identity) in [(20, 0xeee), (2, 0x222), (1, 0x111)] {
            w64(&mut comm, identity);
            w32(&mut comm, id);
        }
        w32(&mut comm, 4);
        for (label, id) in [(24, 10), (14, 7), (23, 9), (15, 8)] {
            comm.extend(raw_words(&[label, id]));
        }
        w32(&mut comm, 2);
        for (&id, rows) in self.input.rows.iter().rev() {
            w64(&mut comm, super::super::source(id));
            comm.extend(raw_words(&[13, rows.len() as u32]));
            for row in rows {
                for &value in row {
                    w64(&mut comm, value);
                }
            }
        }
        comm.extend(raw_words(&[0, 20, 21, 3]));
        for (kind, file, line, index) in [
            ("Future", "future.cpp", 9, 2),
            ("Allocation", "source.cpp", 7, 0),
            ("Other", "other.cpp", 8, 1),
        ] {
            blob(&mut comm, &[kind.as_bytes(), &[0]].concat()).unwrap();
            blob(&mut comm, &[file.as_bytes(), &[0]].concat()).unwrap();
            comm.extend(raw_words(&[line, index]));
        }
        let mut bytes = source[..metadata_end].to_vec();
        bytes[12..16].copy_from_slice(&(comm.len() as u32).to_le_bytes());
        bytes.extend(comm);
        bytes.resize((bytes.len() / 4096 + 1) * 4096, 0);
        let table = ((metadata_end + old_comm_len) / 4096 + 1) * 4096;
        bytes.extend_from_slice(&source[table..]);
        bytes
    }
}
