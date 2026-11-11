use super::*;

pub(super) fn metadata() -> Metadata {
    let strings = [
        "",
        "physx3",
        "PxScene",
        "PvdU8",
        "PvdI32",
        "_debugger_",
        "PxProfileMemoryEventBuffer",
        "_debugger",
        "MemoryEventTotals",
        "events",
        "totals",
        "SectionEventData",
        "frame",
        "Allocation",
        "source.cpp",
    ]
    .into_iter()
    .enumerate()
    .map(|(i, s)| (i as u32, s.into()))
    .collect();
    let name = |namespace, name| Name { namespace, name };
    let class = |name, size, properties, dynamic| {
        Some(Class {
            name,
            base: u32::MAX,
            packing: crate::native::ClassPacking::default(),
            size,
            properties,
            dynamic,
            has_pointers: false,
            string_offsets: Vec::new(),
        })
    };
    Metadata {
        strings,
        classes: vec![
            class(name(1, 2), 0, vec![], false),
            class(name(1, 3), 1, vec![], false),
            class(name(1, 4), 4, vec![], false),
            class(name(5, 6), 24, vec![0], true),
            class(name(7, 8), 24, vec![1], true),
            class(name(5, 11), 24, vec![], false),
        ],
        properties: vec![
            Some(Property {
                owner: name(5, 6),
                name: 9,
                datatype: name(1, 3),
                datatype_id: 1,
                kind: 2,
                semantic: 0,
                offset32: 0,
                values: Vec::new(),
            }),
            Some(Property {
                owner: name(7, 8),
                name: 10,
                datatype: name(1, 4),
                datatype_id: 2,
                kind: 2,
                semantic: 0,
                offset32: 0,
                values: Vec::new(),
            }),
        ],
        messages: Vec::new(),
    }
}

pub(super) fn words(out: &mut Vec<u8>, values: &[u32]) {
    for value in values {
        out.extend(value.to_le_bytes());
    }
}

fn blob(out: &mut Vec<u8>, bytes: &[u8]) {
    words(out, &[bytes.len() as u32]);
    out.extend(bytes);
}

pub(super) fn catalog_bytes(ids: [u32; 2], entries: &[(&str, &str, u32, u32)]) -> Vec<u8> {
    let mut out = Vec::new();
    words(&mut out, &[ids[0], ids[1], entries.len() as u32]);
    for &(kind, file, line, index) in entries {
        blob(&mut out, &[kind.as_bytes(), &[0]].concat());
        blob(&mut out, &[file.as_bytes(), &[0]].concat());
        words(&mut out, &[line, index]);
    }
    out
}

pub(super) fn catalog() -> Catalog {
    let bytes = catalog_bytes(
        [2, 3],
        &[
            ("Future", "future.cpp", 9, 2),
            ("Allocation", "source.cpp", 7, 0),
            ("Other", "other.cpp", 8, 1),
        ],
    );
    Catalog::read(
        &mut Reader::new(&bytes),
        &BTreeMap::from([(2, 0x11102)]),
        &mut 0,
        &mut || true,
    )
    .unwrap()
}

pub(super) fn builder() -> (Metadata, RecordingBuilder) {
    let metadata = metadata();
    let mut builder = super::super::super::tests::catalog_builder();
    metadata.register(&mut builder).unwrap();
    (metadata, builder)
}

pub(super) fn create(metadata: &Metadata, builder: &mut RecordingBuilder, id: u64, class: u32) {
    push(
        builder,
        Event::CreateInstance {
            class: metadata.class(class).unwrap().name,
            id,
        },
    )
    .unwrap();
}

pub(super) fn set_totals(metadata: &Metadata, builder: &mut RecordingBuilder, values: &[i32]) {
    metadata
        .set_with_progress(builder, 3, 1, &packed(values), &mut || true)
        .unwrap();
}

pub(super) fn packed(values: &[i32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

pub(super) fn create_event(id: u32, class: u32) -> Vec<u8> {
    let mut out = Vec::new();
    words(&mut out, &[1, id, class]);
    out
}

pub(super) fn set_event(id: u32, property: u32, value: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    words(&mut out, &[3, id, property]);
    blob(&mut out, value);
    words(&mut out, &[0]);
    out
}

pub(super) fn end_frame(timestamp: u64) -> Vec<u8> {
    let mut section = 0x11101u64.to_le_bytes().to_vec();
    section.extend(timestamp.to_le_bytes());
    words(&mut section, &[12, 0]);
    let mut out = Vec::new();
    words(&mut out, &[8, 5]);
    blob(&mut out, &section);
    out
}

pub(super) fn initial_memory(values: &[i32]) -> Vec<(u32, u32, Vec<u8>)> {
    vec![
        (1, 0, Vec::new()),
        (2, 3, vec![2, 0, 64, 13, 14, 7, 42]),
        (3, 4, packed(values)),
    ]
}

fn metadata_bytes(metadata: &Metadata) -> Vec<u8> {
    let mut out = Vec::new();
    words(
        &mut out,
        &[
            1,
            metadata.classes.len() as u32,
            metadata.strings.len() as u32 - 1,
            metadata.strings.len() as u32,
        ],
    );
    for (&handle, value) in &metadata.strings {
        if handle == 0 {
            continue;
        }
        words(&mut out, &[handle]);
        blob(&mut out, &[value.as_bytes(), &[0]].concat());
    }
    words(&mut out, &[metadata.properties.len() as u32]);
    for (id, property) in metadata.properties.iter().enumerate() {
        words(&mut out, &[u32::from(property.is_some())]);
        let Some(p) = property else {
            continue;
        };
        words(
            &mut out,
            &[
                p.owner.namespace,
                p.owner.name,
                0,
                p.semantic,
                p.datatype_id,
                p.datatype.namespace,
                p.datatype.name,
                u32::from(p.kind),
                id as u32,
                p.offset32 as u32,
                p.offset32 as u32,
                0,
                p.name,
            ],
        );
    }
    words(&mut out, &[metadata.classes.len() as u32]);
    for (id, class) in metadata.classes.iter().enumerate() {
        words(&mut out, &[u32::from(class.is_some())]);
        let Some(c) = class else {
            continue;
        };
        words(
            &mut out,
            &[
                c.name.namespace,
                c.name.name,
                id as u32,
                c.base,
                c.packing.uniform_width as u32,
                c.packing.class_type as u32,
            ],
        );
        out.extend([0, u8::from(c.dynamic)]);
        words(
            &mut out,
            &[
                c.size as u32,
                c.size as u32,
                4,
                c.size as u32,
                c.size as u32,
                4,
                c.properties.len() as u32,
            ],
        );
        words(&mut out, &c.properties);
        words(&mut out, &[0, 0]);
    }
    words(&mut out, &[0]);
    out
}

pub(super) fn container(initial: &[(u32, u32, Vec<u8>)], events: &[Vec<u8>]) -> Vec<u8> {
    let metadata = metadata();
    let object = metadata_bytes(&metadata);
    let mut communication = Vec::new();
    words(&mut communication, &[1]);
    communication.extend(1u64.to_le_bytes());
    communication.extend(1u64.to_le_bytes());
    words(&mut communication, &[3]);
    for id in [1, 2, 4u32] {
        words(&mut communication, &[id]);
        communication.extend((0x11100u64 + u64::from(id)).to_le_bytes());
        words(&mut communication, &[id]);
        communication.extend([0; 10]);
    }
    words(&mut communication, &[0, 0, 0, 0]);
    communication.extend(catalog_bytes(
        [2, 3],
        &[
            ("Future", "future.cpp", 9, 2),
            ("Allocation", "source.cpp", 7, 0),
            ("Other", "other.cpp", 8, 1),
        ],
    ));
    let mut out = Vec::new();
    words(
        &mut out,
        &[MAGIC, 1, object.len() as u32, communication.len() as u32],
    );
    out.extend(object);
    out.extend(communication);
    out.resize((out.len() / 4096 + 1) * 4096, 0);

    let snapshot_length = 4 + initial.len() * 12;
    let mut records = Vec::new();
    let mut snapshot = Vec::new();
    words(&mut snapshot, &[initial.len() as u32]);
    for (id, class, data) in initial {
        words(&mut snapshot, &[*id]);
        snapshot.extend(((snapshot_length + records.len()) as u64).to_le_bytes());
        let mut body = Vec::new();
        blob(&mut body, &vec![0xa5; metadata.class(*class).unwrap().size]);
        if *class == 3 || *class == 4 {
            blob(&mut body, data);
        }
        words(&mut records, &[*class, *id]);
        blob(&mut records, &body);
    }
    let mut backing = snapshot;
    backing.extend(records);
    let mut page = Vec::new();
    words(&mut page, &[events.len() as u32]);
    let mut offset = 0;
    for event in events {
        words(&mut page, &[offset]);
        offset += event.len() as u32;
    }
    page.extend(events.concat());
    let page_position = backing.len() as u64;
    out.extend((events.len() as u64).to_le_bytes());
    words(&mut out, &[u32::from(!events.is_empty())]);
    if !events.is_empty() {
        out.extend(0u64.to_le_bytes());
        out.extend(page_position.to_le_bytes());
        words(&mut out, &[events.len() as u32, page.len() as u32]);
        backing.extend(page);
    }
    words(&mut out, &[1]);
    out.extend(0u64.to_le_bytes());
    out.extend(0u64.to_le_bytes());
    words(&mut out, &[snapshot_length as u32, 492, 0]);
    out.resize((out.len() / 4096 + 1) * 4096, 0);
    out.extend(backing);
    out
}
