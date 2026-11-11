use super::*;

mod memory_combo;

pub(super) struct Input {
    pub(super) metadata: Metadata,
    pub(super) events: Vec<Vec<u8>>,
    initial: Vec<u8>,
    saved: Vec<SavedInput>,
    rows: BTreeMap<u32, Vec<[u64; 4]>>,
    starts: BTreeMap<u32, (u64, usize)>,
    live: BTreeMap<u32, [u8; 40]>,
    previous: BTreeMap<u32, [usize; 4]>,
    views: BTreeMap<u32, usize>,
}

fn source(id: u32) -> u64 {
    match id {
        1 => 0x111,
        2 => 0x222,
        _ => panic!("unexpected Scene"),
    }
}

fn record(id: u32, class: u32, fixed: &[u8], position: usize, deleted: bool) -> SavedInput {
    let mut body = Vec::new();
    blob(&mut body, fixed).unwrap();
    let mut bytes = raw_words(&[class, id]);
    blob(&mut bytes, &body).unwrap();
    SavedInput {
        id,
        position,
        bytes,
        deleted,
    }
}

impl Input {
    pub(super) fn new(initial: bool) -> Self {
        let mut metadata = metadata();
        for (id, text) in [(22, "main"), (23, "orbit"), (24, "future"), (25, "View")] {
            metadata.strings.insert(id, text.into());
        }
        let scene = metadata.classes[0].as_mut().unwrap();
        scene.size = 4;
        scene.properties = vec![7];
        scene.has_pointers = true;
        metadata.properties.push(Some(Property {
            owner: name(1, 2),
            name: 25,
            datatype: name(1, 4),
            datatype_id: 2,
            kind: 1,
            semantic: 0,
            offset32: 0,
            values: vec![],
        }));
        let mut input = Self {
            metadata,
            events: vec![raw_words(&[9])],
            initial: vec![0; 4],
            saved: vec![],
            rows: BTreeMap::new(),
            starts: BTreeMap::new(),
            live: BTreeMap::new(),
            previous: BTreeMap::new(),
            views: BTreeMap::new(),
        };
        if initial {
            input.saved = vec![
                record(1, 0, &7u32.to_le_bytes(), 128, false),
                record(2, 0, &7u32.to_le_bytes(), 192, false),
                record(7, 4, &raw_camera(14, MAIN_A), 256, false),
            ];
            input.initial = raw_words(&[3]);
            for saved in &input.saved {
                w32(&mut input.initial, saved.id);
                w64(&mut input.initial, saved.position as u64);
            }
            input.live.insert(7, raw_camera(14, MAIN_A));
            input.previous.insert(7, [0; 4]);
            input.views.extend([(1, 0), (2, 0)]);
        } else {
            for id in [1, 2] {
                input.views.insert(id, input.events.len());
                input.events.push(raw_words(&[1, id, 0]));
            }
            input.create_camera(7, 14, MAIN_A);
            input.view(1, 7);
            input.view(2, 7);
        }

        input.begin(1, 1000);
        input.end(1, 1010);
        input.scalar(7, 0, &22u32.to_le_bytes());
        input.vectors(7, GAP);
        input.begin(2, 10);
        input.scalar(7, 1, &vectors(MAIN_B)[..12]);
        input.end(2, 20);
        input.vectors(7, MAIN_C);
        input.create_camera(8, 15, LATE_A);
        input.view(2, 8);
        input.begin(2, 30);
        input.scalar(7, 1, &vectors(MAIN_D)[..12]);
        input.begin(1, 1100);
        input.scalar(8, 3, &vectors(LATE_C)[24..]);
        input.end(2, 40);
        input.vectors(7, MAIN_E);
        input.view(1, 8);
        input.end(1, 1110);
        input.destroy_camera(7);
        input.scalar(8, 1, &vectors(LATE_D)[..12]);
        input.begin(2, 50);
        input.vectors(8, LATE_E);
        input.end(2, 60);
        input.begin(1, 1200);
        input.create_camera(9, 23, ORBIT_A);
        input.view(1, 9);
        input.end(1, 1210);
        input.begin(1, 1300);
        input.scalar(8, 1, &vectors(LATE_F)[..12]);
        input.vectors(9, ORBIT_B);
        input.begin(2, 70);
        input.end(1, 1310);
        input.view(2, 9);
        input.destroy_camera(8);
        input.end(2, 80);
        input.vectors(9, TAIL);
        input.create_camera(10, 24, TAIL);
        assert!(input.starts.is_empty());
        input
    }

    fn create_camera(&mut self, id: u32, name: u32, value: SourceCamera) {
        let ordinal = self.events.len();
        self.events.push(raw_words(&[1, id, 4]));
        assert!(self.live.insert(id, [0; 40]).is_none());
        self.previous.insert(id, [ordinal; 4]);
        self.scalar(id, 0, &name.to_le_bytes());
        self.vectors(id, value);
    }

    fn scalar(&mut self, id: u32, slot: usize, bytes: &[u8]) {
        let ordinal = self.events.len();
        let previous = &mut self.previous.get_mut(&id).unwrap()[slot];
        self.events.push(set(
            3,
            id,
            slot as u32,
            bytes,
            &[(ordinal - *previous) as u32],
        ));
        *previous = ordinal;
        let offset = if slot == 0 { 0 } else { 4 + (slot - 1) * 12 };
        self.live.get_mut(&id).unwrap()[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    fn vectors(&mut self, id: u32, value: SourceCamera) {
        let ordinal = self.events.len();
        let previous = self.previous.get_mut(&id).unwrap();
        let targets: BTreeSet<_> = previous[1..].iter().copied().collect();
        let undo: Vec<_> = targets
            .into_iter()
            .map(|target| (ordinal - target) as u32)
            .collect();
        let bytes = vectors(value);
        self.events.push(set(4, id, 0, &bytes, &undo));
        previous[1..].fill(ordinal);
        self.live.get_mut(&id).unwrap()[4..].copy_from_slice(&bytes);
    }

    fn view(&mut self, id: u32, target: u32) {
        let ordinal = self.events.len();
        let previous = self.views.insert(id, ordinal).unwrap();
        self.events.push(set(
            3,
            id,
            7,
            &target.to_le_bytes(),
            &[(ordinal - previous) as u32],
        ));
    }

    fn destroy_camera(&mut self, id: u32) {
        let fixed = self.live.remove(&id).unwrap();
        let position = 1024 + self.saved.iter().filter(|saved| saved.deleted).count() * 128;
        self.saved.push(record(id, 4, &fixed, position, true));
        self.events.push(saved_event(id, position as u64));
        self.events.push(raw_words(&[2, id]));
        self.previous.remove(&id);
    }

    fn begin(&mut self, id: u32, ticks: u64) {
        assert!(self.starts.insert(id, (ticks, self.events.len())).is_none());
        self.section(id, ticks, true);
    }

    fn end(&mut self, id: u32, ticks: u64) {
        let (begin_ticks, begin) = self.starts.remove(&id).unwrap();
        self.rows.entry(id).or_default().push([
            begin_ticks,
            begin as u64,
            ticks,
            self.events.len() as u64,
        ]);
        self.section(id, ticks, false);
    }

    fn section(&mut self, id: u32, ticks: u64, begin: bool) {
        let mut data = source(id).to_le_bytes().to_vec();
        w64(&mut data, ticks);
        w32(&mut data, 13);
        data.extend([u8::from(begin), 0, 0, 0]);
        let mut event = raw_words(&[8, 5]);
        blob(&mut event, &data).unwrap();
        self.events.push(event);
    }

    pub(super) fn bytes(&self) -> Vec<u8> {
        let metadata = middle::tests::metadata_bytes(&self.metadata);
        let mut comm = raw_words(&[1]);
        w64(&mut comm, 3);
        w64(&mut comm, 7);
        w32(&mut comm, 2);
        for id in [1, 2] {
            w32(&mut comm, id);
            w64(&mut comm, source(id));
            w32(&mut comm, id);
            comm.extend([0; 10]);
        }
        w32(&mut comm, 2);
        for id in [1, 2] {
            w64(&mut comm, source(id));
            w32(&mut comm, id);
        }
        w32(&mut comm, 4);
        for (name, id) in [(24, 10), (14, 7), (23, 9), (15, 8)] {
            comm.extend(raw_words(&[name, id]));
        }
        w32(&mut comm, 2);
        for (&id, rows) in self.rows.iter().rev() {
            w64(&mut comm, source(id));
            comm.extend(raw_words(&[13, rows.len() as u32]));
            for row in rows {
                for &value in row {
                    w64(&mut comm, value);
                }
            }
        }
        comm.extend(raw_words(&[0, u32::MAX, u32::MAX, 0]));
        let mut page = raw_words(&[self.events.len() as u32]);
        let mut offset = 0;
        for event in &self.events {
            w32(&mut page, offset);
            offset += event.len() as u32;
        }
        page.extend(self.events.concat());
        let mut bytes = raw_words(&[MAGIC, 1, metadata.len() as u32, comm.len() as u32]);
        bytes.extend(metadata);
        bytes.extend(comm);
        bytes.resize((bytes.len() / 4096 + 1) * 4096, 0);
        w64(&mut bytes, self.events.len() as u64);
        w32(&mut bytes, 1);
        w64(&mut bytes, 0);
        w64(&mut bytes, 4096);
        w32(&mut bytes, self.events.len() as u32);
        w32(&mut bytes, page.len() as u32);
        w32(&mut bytes, 1);
        bytes.extend([0; 16]);
        w32(&mut bytes, self.initial.len() as u32);
        w32(&mut bytes, 492);
        w32(
            &mut bytes,
            self.saved.iter().filter(|saved| saved.deleted).count() as u32,
        );
        for saved in self.saved.iter().filter(|saved| saved.deleted) {
            w32(&mut bytes, saved.id);
            w64(&mut bytes, saved.position as u64);
            w32(&mut bytes, saved.bytes.len() as u32);
        }
        bytes.resize((bytes.len() / 4096 + 1) * 4096, 0);
        let mut backing = vec![0; 4096];
        backing[..self.initial.len()].copy_from_slice(&self.initial);
        for saved in &self.saved {
            backing[saved.position..saved.position + saved.bytes.len()]
                .copy_from_slice(&saved.bytes);
        }
        bytes.extend(backing);
        bytes.extend(page);
        bytes
    }
}
