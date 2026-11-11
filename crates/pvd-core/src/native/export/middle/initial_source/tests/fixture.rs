use super::*;
use crate::native::{Class, Message, MessageEntry, Property};
use pvd_protocol::Name;

pub(super) fn word(out: &mut Vec<u8>, value: u32) {
    out.extend(value.to_le_bytes());
}
fn wide(out: &mut Vec<u8>, value: u64) {
    out.extend(value.to_le_bytes());
}
fn blob(out: &mut Vec<u8>, bytes: &[u8]) {
    word(out, bytes.len() as u32);
    out.extend(bytes);
}
fn words(values: &[u32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

pub(super) fn metadata() -> Metadata {
    let strings = [
        "",
        "physx3",
        "PxScene",
        "PvdU32",
        "ObjectRef",
        "Base",
        "Body",
        "_debugger_",
        "SectionEventData",
        "StringHandle",
        "PvdErrorMessage",
        "UserCustomedEvent",
        "PvdU8",
        "PvdUserRenderer",
        "base",
        "counter",
        "values",
        "parent",
        "frame",
        "Code",
        "Message",
        "File",
        "Line",
        "Index",
        "Events",
        "events",
        "Failure",
        "source.cpp",
    ]
    .into_iter()
    .enumerate()
    .map(|(index, text)| (index as u32, text.to_owned()))
    .collect();
    let name = |namespace, name| Name { namespace, name };
    let class = |namespace, label, size, base, properties, pointers: Vec<usize>, dynamic| {
        Some(Class {
            name: name(namespace, label),
            base,
            packing: crate::native::ClassPacking::default(),
            size,
            properties,
            dynamic,
            has_pointers: !pointers.is_empty(),
            string_offsets: pointers,
        })
    };
    let mut classes = vec![
        class(1, 2, 0, u32::MAX, vec![], vec![], false),
        class(1, 3, 4, u32::MAX, vec![], vec![], false),
        class(1, 4, 4, u32::MAX, vec![], vec![], false),
        class(1, 5, 4, u32::MAX, vec![0], vec![], false),
        class(1, 6, 28, 3, vec![1, 2, 3], vec![], true),
        class(7, 8, 24, u32::MAX, vec![], vec![], false),
        class(1, 9, 4, u32::MAX, vec![], vec![0], false),
        class(7, 10, 20, u32::MAX, vec![4, 5, 6, 7, 8], vec![4, 8], false),
        class(7, 11, 12, u32::MAX, vec![9], vec![], true),
        class(1, 12, 1, u32::MAX, vec![], vec![], false),
        class(7, 13, 12, u32::MAX, vec![10], vec![], true),
    ];
    classes[2].as_mut().unwrap().has_pointers = true;
    let mut properties = Vec::new();
    for (owner, label, datatype, kind, offset32) in [
        (3, 14, 1, 1, 0),
        (4, 15, 1, 1, 4),
        (4, 16, 1, 2, 8),
        (4, 17, 2, 1, 20),
        (7, 19, 1, 1, 0),
        (7, 20, 6, 1, 4),
        (7, 21, 6, 1, 8),
        (7, 22, 1, 1, 12),
        (7, 23, 1, 1, 16),
        (8, 24, 7, 2, 0),
        (10, 25, 9, 2, 0),
    ] {
        properties.push(Some(Property {
            owner: classes[owner].as_ref().unwrap().name,
            name: label,
            datatype: classes[datatype].as_ref().unwrap().name,
            datatype_id: datatype as u32,
            kind,
            semantic: 0,
            offset32,
            values: vec![],
        }));
    }
    Metadata {
        strings,
        classes,
        properties,
        messages: vec![Some(Message {
            class_name: name(1, 6),
            class: 4,
            size: 8,
            entries: vec![
                MessageEntry {
                    datatype: name(1, 3),
                    datatype_id: 1,
                    property: 1,
                    offset: 0,
                    size: 4,
                    destination_size: 4,
                },
                MessageEntry {
                    datatype: name(1, 3),
                    datatype_id: 1,
                    property: 0,
                    offset: 4,
                    size: 4,
                    destination_size: 4,
                },
            ],
        })],
    }
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
    for (&id, value) in &metadata.strings {
        if id != 0 {
            word(&mut out, id);
            blob(&mut out, &[value.as_bytes(), &[0]].concat());
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
        for &property in &c.properties {
            word(&mut out, property);
        }
        for _ in 0..2 {
            let object_offset = if index == 2 {
                Some(0)
            } else if index == 4 && c.has_pointers && c.string_offsets.is_empty() {
                Some(24)
            } else {
                None
            };
            word(
                &mut out,
                u32::from(object_offset.is_some()) + c.string_offsets.len() as u32,
            );
            if let Some(offset) = object_offset {
                word(&mut out, 1);
                word(&mut out, offset);
            }
            for &offset in &c.string_offsets {
                word(&mut out, 2);
                word(&mut out, offset as u32);
            }
        }
    }
    word(&mut out, metadata.messages.len() as u32);
    for (index, message) in metadata.messages.iter().enumerate() {
        word(&mut out, u32::from(message.is_some()));
        let Some(message) = message else { continue };
        for value in [
            message.class_name.namespace,
            message.class_name.name,
            message.class,
            1,
            5,
            index as u32,
            message.size as u32,
            message.entries.len() as u32,
        ] {
            word(&mut out, value);
        }
        for entry in &message.entries {
            for value in [
                entry.datatype.namespace,
                entry.datatype.name,
                entry.datatype_id,
                entry.offset as u32,
                entry.size as u32,
                entry.destination_size as u32,
                entry.property,
            ] {
                word(&mut out, value);
            }
        }
        word(&mut out, 0);
    }
    out
}

pub(super) fn body(counter: u32, values: &[u32], parent: u32, pad: u32) -> Vec<u8> {
    let mut out = Vec::new();
    blob(
        &mut out,
        &words(&[11, counter, 0x11223344, 0x55667788, 0x99aabbcc, parent, pad]),
    );
    blob(&mut out, &words(values));
    out
}
pub(super) fn error_record(index: u32) -> Vec<u8> {
    words(&[4, 26, 27, 42, index])
}
pub(super) fn error_body(indices: &[u32]) -> Vec<u8> {
    let mut out = Vec::new();
    blob(&mut out, &[0x38; 12]);
    blob(
        &mut out,
        &indices
            .iter()
            .flat_map(|index| error_record(*index))
            .collect::<Vec<_>>(),
    );
    out
}
pub(super) fn renderer_body(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    blob(&mut out, &[0x6a; 12]);
    blob(&mut out, bytes);
    out
}
pub(super) fn set(kind: u32, id: u32, property: u32, data: &[u8], references: &[u32]) -> Vec<u8> {
    let mut out = words(&[kind, id, property]);
    blob(&mut out, data);
    word(&mut out, references.len() as u32);
    out.extend(words(references));
    out
}
pub(super) fn array(
    kind: u32,
    id: u32,
    property: u32,
    index: u32,
    data: &[u8],
    references: &[u32],
) -> Vec<u8> {
    let mut out = words(&[kind, id, property, index]);
    if kind != 6 {
        blob(&mut out, data);
    }
    if kind != 5 {
        word(&mut out, references.len() as u32);
        out.extend(words(references));
    }
    out
}
fn section(tick: u64, begin: bool) -> Vec<u8> {
    let mut data = Vec::new();
    wide(&mut data, 0x111);
    wide(&mut data, tick);
    word(&mut data, 18);
    data.extend([u8::from(begin), 0, 0, 0]);
    let mut out = words(&[8, 5]);
    blob(&mut out, &data);
    out
}
pub(super) fn saved(id: u32, position: u64) -> Vec<u8> {
    let mut out = words(&[10, id]);
    wide(&mut out, position);
    out
}
pub(super) fn destroy(id: u32) -> Vec<u8> {
    words(&[2, id])
}
pub(super) fn create(id: u32, class: u32) -> Vec<u8> {
    words(&[1, id, class])
}
pub(super) fn point(explicit: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    if explicit {
        bytes.push(1);
        wide(&mut bytes, 1);
    }
    bytes.push(2);
    word(&mut bytes, 1);
    for value in [1f32, 2., 3.] {
        bytes.extend(value.to_le_bytes());
    }
    word(&mut bytes, 0xff123456);
    bytes
}

pub(super) struct Record {
    pub(super) id: u32,
    pub(super) class: u32,
    pub(super) position: u64,
    pub(super) body: Vec<u8>,
    pub(super) deleted: bool,
}
pub(super) struct Fixture {
    pub(super) metadata: Metadata,
    pub(super) records: Vec<Record>,
    pub(super) initial: Vec<usize>,
    pub(super) events: Vec<Vec<u8>>,
    pub(super) ids: Vec<u32>,
    pub(super) alive: Vec<u32>,
}

pub(super) fn fixture() -> Fixture {
    Fixture {
        metadata: metadata(),
        records: vec![
            Record {
                id: 1,
                class: 0,
                position: 128,
                body: words(&[0]),
                deleted: false,
            },
            Record {
                id: 2,
                class: 4,
                position: 256,
                body: body(7, &[10, 20], 1, 0xa1b2c3d4),
                deleted: false,
            },
            Record {
                id: 4,
                class: 8,
                position: 512,
                body: error_body(&[50]),
                deleted: false,
            },
            Record {
                id: 3,
                class: 8,
                position: 768,
                body: error_body(&[7, 7]),
                deleted: false,
            },
            Record {
                id: 4,
                class: 8,
                position: 1024,
                body: error_body(&[50]),
                deleted: true,
            },
            Record {
                id: 2,
                class: 4,
                position: 1280,
                body: body(12, &[25, 30, 40], 0, 0xdeadbeef),
                deleted: true,
            },
        ],
        initial: vec![2, 1, 0, 3],
        events: vec![
            words(&[9]),
            set(3, 2, 1, &9u32.to_le_bytes(), &[1]),
            array(5, 2, 2, 2, &30u32.to_le_bytes(), &[]),
            section(100, true),
            array(7, 2, 2, 1, &25u32.to_le_bytes(), &[4, 2]),
            section(101, false),
            array(5, 3, 9, 2, &error_record(8), &[]),
            saved(4, 1024),
            destroy(4),
            section(200, true),
            set(3, 2, 1, &12u32.to_le_bytes(), &[9]),
            array(6, 2, 2, 0, &[], &[11, 9, 7]),
            array(5, 3, 9, 3, &error_record(9), &[]),
            create(5, 4),
            set(3, 5, 1, &42u32.to_le_bytes(), &[1]),
            section(201, false),
            section(300, true),
            array(5, 2, 2, 2, &40u32.to_le_bytes(), &[]),
            set(3, 2, 3, &0u32.to_le_bytes(), &[18]),
            saved(2, 1280),
            destroy(2),
            array(5, 3, 9, 4, &error_record(10), &[]),
            section(301, false),
            array(5, 3, 9, 5, &error_record(11), &[]),
            section(400, true),
            section(401, false),
        ],
        ids: vec![1, 2, 3, 4, 5],
        alive: vec![1, 3, 5],
    }
}

impl Fixture {
    pub(super) fn add_renderer(&mut self) {
        self.initial.push(self.records.len());
        self.records.push(Record {
            id: 6,
            class: 10,
            position: 1536,
            body: renderer_body(&[]),
            deleted: false,
        });
        self.ids.push(6);
        self.alive.push(6);
    }

    pub(super) fn bytes(&self) -> Vec<u8> {
        self.bytes_with_catalogs(&words(&[0, u32::MAX, u32::MAX, 0]))
    }

    pub(super) fn bytes_with_catalogs(&self, catalogs: &[u8]) -> Vec<u8> {
        self.bytes_with_cameras(catalogs, &[])
    }

    pub(super) fn bytes_with_cameras(&self, catalogs: &[u8], cameras: &[(u32, u32)]) -> Vec<u8> {
        let objects = metadata_bytes(&self.metadata);
        let mut rows = Vec::new();
        let mut begin = None;
        for (ordinal, event) in self.events.iter().enumerate() {
            if event[..4] == 8u32.to_le_bytes() {
                let tick = u64::from_le_bytes(event[20..28].try_into().unwrap());
                if event[32] == 1 {
                    begin = Some((tick, ordinal as u64));
                } else {
                    let (tick0, ordinal0) = begin.take().unwrap();
                    rows.push([tick0, ordinal0, tick, ordinal as u64]);
                }
            }
        }
        assert!(begin.is_none());
        let mut comm = Vec::new();
        word(&mut comm, 1);
        wide(&mut comm, 1);
        wide(&mut comm, 1);
        word(&mut comm, self.ids.len() as u32);
        for &id in &self.ids {
            word(&mut comm, id);
            wide(&mut comm, u64::from(id) * 0x111);
            word(&mut comm, id);
            comm.extend([0; 10]);
        }
        word(&mut comm, self.alive.len() as u32);
        for &id in &self.alive {
            wide(&mut comm, u64::from(id) * 0x111);
            word(&mut comm, id);
        }
        word(&mut comm, cameras.len() as u32);
        for &(name, id) in cameras {
            word(&mut comm, name);
            word(&mut comm, id);
        }
        word(&mut comm, 1);
        wide(&mut comm, 0x111);
        word(&mut comm, 18);
        word(&mut comm, rows.len() as u32);
        for row in rows {
            for value in row {
                wide(&mut comm, value);
            }
        }
        comm.extend(catalogs);
        let mut backing = Vec::new();
        word(&mut backing, self.initial.len() as u32);
        for &index in &self.initial {
            let record = &self.records[index];
            word(&mut backing, record.id);
            wide(&mut backing, record.position);
        }
        let mut records = self.records.iter().collect::<Vec<_>>();
        records.sort_by_key(|record| record.position);
        for record in records {
            assert!(backing.len() <= record.position as usize);
            backing.resize(record.position as usize, 0);
            word(&mut backing, record.class);
            word(&mut backing, record.id);
            blob(&mut backing, &record.body);
        }
        let mut page = Vec::new();
        word(&mut page, self.events.len() as u32);
        let mut offset = 0;
        for event in &self.events {
            word(&mut page, offset);
            offset += event.len() as u32;
        }
        for event in &self.events {
            page.extend(event);
        }
        assert!(backing.len() <= 4096);
        backing.resize(4096, 0);
        backing.extend(&page);
        let mut out = Vec::new();
        for value in [MAGIC, 1, objects.len() as u32, comm.len() as u32] {
            word(&mut out, value);
        }
        out.extend(objects);
        out.extend(comm);
        out.resize((out.len() / 4096 + 1) * 4096, 0);
        wide(&mut out, self.events.len() as u64);
        word(&mut out, 1);
        wide(&mut out, 0);
        wide(&mut out, 4096);
        word(&mut out, self.events.len() as u32);
        word(&mut out, page.len() as u32);
        word(&mut out, 1);
        wide(&mut out, 0);
        wide(&mut out, 0);
        word(&mut out, (4 + self.initial.len() * 12) as u32);
        word(&mut out, 492);
        word(
            &mut out,
            self.records.iter().filter(|record| record.deleted).count() as u32,
        );
        for record in &self.records {
            if record.deleted {
                word(&mut out, record.id);
                wide(&mut out, record.position);
                word(&mut out, record.body.len() as u32 + 12);
            }
        }
        out.resize((out.len() / 4096 + 1) * 4096, 0);
        out.extend(backing);
        out
    }
}

pub(super) struct Raw {
    pub(super) initial: BTreeMap<u32, (u32, Vec<u8>)>,
    pub(super) positions: BTreeMap<u32, u64>,
    pub(super) deleted: BTreeMap<u32, (u64, u32, Vec<u8>)>,
    pub(super) events: Vec<Vec<u8>>,
    pub(super) rows: Vec<[u64; 4]>,
    pub(super) reverse: Vec<u32>,
    pub(super) reserved: u32,
}

pub(super) fn raw(bytes: &[u8]) -> Raw {
    raw_with_cameras(bytes, &[])
}

pub(super) fn raw_with_cameras(bytes: &[u8], expected_cameras: &[(u32, u32)]) -> Raw {
    let at = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
    let object_len = at(8);
    let comm_len = at(12);
    let end = 16 + object_len + comm_len;
    let mut comm = Reader::new(&bytes[16 + object_len..end]);
    assert_eq!(comm.u32().unwrap(), 1);
    comm.take(16).unwrap();
    let count = comm.u32().unwrap() as usize;
    comm.take(count * 26).unwrap();
    let mut reverse = Vec::new();
    for _ in 0..comm.u32().unwrap() {
        comm.u64().unwrap();
        reverse.push(comm.u32().unwrap());
    }
    let cameras = (0..comm.u32().unwrap())
        .map(|_| (comm.u32().unwrap(), comm.u32().unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(cameras, expected_cameras);
    assert_eq!(comm.u32().unwrap(), 1);
    comm.take(12).unwrap();
    let mut rows = Vec::new();
    for _ in 0..comm.u32().unwrap() {
        rows.push([
            comm.u64().unwrap(),
            comm.u64().unwrap(),
            comm.u64().unwrap(),
            comm.u64().unwrap(),
        ]);
    }
    let history = (end / 4096 + 1) * 4096;
    let mut directory = Reader::new(&bytes[history..]);
    directory.u64().unwrap();
    let mut pages = Vec::new();
    for _ in 0..directory.u32().unwrap() {
        directory.u64().unwrap();
        pages.push((
            directory.u64().unwrap() as usize,
            directory.u32().unwrap() as usize,
            directory.u32().unwrap() as usize,
        ));
    }
    assert_eq!(directory.u32().unwrap(), 1);
    assert_eq!(directory.u64().unwrap(), 0);
    let snapshot = directory.u64().unwrap() as usize;
    directory.u32().unwrap();
    let reserved = directory.u32().unwrap();
    let mut deleted_entries = Vec::new();
    for _ in 0..directory.u32().unwrap() {
        let id = directory.u32().unwrap();
        let position = directory.u64().unwrap();
        directory.u32().unwrap();
        deleted_entries.push((id, position));
    }
    let backing = (history + directory.pos) / 4096 * 4096 + 4096;
    let record = |id, position| {
        let mut r = Reader::new(&bytes[backing + position as usize..]);
        let class = r.u32().unwrap();
        assert_eq!(r.u32().unwrap(), id);
        (class, r.blob().unwrap().to_vec())
    };
    let mut initial = BTreeMap::new();
    let mut positions = BTreeMap::new();
    let mut snapshot = Reader::new(&bytes[backing + snapshot..]);
    for _ in 0..snapshot.u32().unwrap() {
        let id = snapshot.u32().unwrap();
        let position = snapshot.u64().unwrap();
        initial.insert(id, record(id, position));
        positions.insert(id, position);
    }
    let deleted = deleted_entries
        .into_iter()
        .map(|(id, position)| {
            let (class, body) = record(id, position);
            (id, (position, class, body))
        })
        .collect();
    let mut events = Vec::new();
    for (position, count, length) in pages {
        let mut page = Reader::new(&bytes[backing + position..backing + position + length]);
        assert_eq!(page.u32().unwrap() as usize, count);
        let mut offsets = Vec::new();
        for _ in 0..count {
            offsets.push(page.u32().unwrap() as usize);
        }
        let payload = &page.bytes[page.pos..];
        offsets.push(payload.len());
        events.extend(
            offsets
                .windows(2)
                .map(|range| payload[range[0]..range[1]].to_vec()),
        );
    }
    Raw {
        initial,
        positions,
        deleted,
        events,
        rows,
        reverse,
        reserved,
    }
}
