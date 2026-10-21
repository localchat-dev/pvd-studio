use pvd_core::{
    analysis::ProfileZone,
    profile_catalog_index::{self as disk, FirstStartPosition, IndexLimits, PageLimits},
    profile_curves::{CatalogEntry, ExactKey, SourceIdentity, catalog_page_streaming},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    sync::atomic::AtomicBool,
};

const MAX_LINE: usize = 8192;
const MAX_NAMES: usize = 1024;
const MAX_FILE: u64 = 64 << 20;
const EXPECTED_PAGE_BYTES: usize = 8 << 20;

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Wire {
    key: (u64, u16, u32, u64),
    first: Option<u64>,
    complete: u64,
    open: u64,
    unmatched: u64,
    ambiguous: bool,
    zone: Option<String>,
    event: Option<String>,
}
impl Wire {
    fn from(entry: &CatalogEntry, zones: &BTreeMap<u64, ProfileZone>) -> Self {
        let key = entry.key;
        let zone = zones.get(&key.zone);
        let event = zone.and_then(|zone| zone.events.get(&key.event));
        assert!(zone.map_or(0, |zone| zone.name.len()) + event.map_or(0, String::len) <= MAX_NAMES);
        Self {
            key: (key.zone, key.event, key.thread, key.context),
            first: entry.first_start,
            complete: entry.complete_spans.try_into().unwrap(),
            open: entry.open_spans.try_into().unwrap(),
            unmatched: entry.unmatched_stops,
            ambiguous: entry.metadata_ambiguous,
            zone: zone.map(|zone| zone.name.clone()),
            event: event.cloned(),
        }
    }
    fn position(&self) -> FirstStartPosition {
        FirstStartPosition {
            first_start: self.first,
            key: self.key.into(),
        }
    }
    fn names_bytes(&self) -> usize {
        self.zone.as_ref().map_or(0, String::capacity)
            + self.event.as_ref().map_or(0, String::capacity)
    }
}
fn read_wire(reader: &mut impl BufRead) -> Option<Wire> {
    let mut bytes = Vec::new();
    let count = (&mut *reader)
        .take((MAX_LINE + 1) as u64)
        .read_until(b'\n', &mut bytes)
        .unwrap();
    if count == 0 {
        return None;
    }
    assert!(count <= MAX_LINE && bytes.last() == Some(&b'\n'));
    let row: Wire = serde_json::from_slice(&bytes).unwrap();
    assert!(row.names_bytes() <= MAX_NAMES);
    Some(row)
}
struct Workspace(PathBuf);
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0.join("rows.jsonl"));
        let _ = std::fs::remove_dir(&self.0);
    }
}

#[test]
#[ignore = "600-second preserved SDK input; bounded key-page oracle repeatedly decodes the complete source"]
fn long_sdk_first_start_catalog_matches_every_key_page_with_bounded_scratch() {
    let root =
        PathBuf::from(std::env::var_os("PVD_TIMELINE_LONG_ROOT").expect("PVD_TIMELINE_LONG_ROOT"));
    let source = root.join("raw/capture-1792077385976907200-8920-1-0.pvdstream");
    let digest =
        blake3::Hash::from_hex("9a99d0525325e7ad2085bb2db7e175eb53e4d0be331d68ff1373f41bb9a181f1")
            .unwrap();
    let identity = SourceIdentity {
        blake3: *digest.as_bytes(),
        bytes: 251301039,
        frames: 19293,
    };
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../output")
        .join(format!("catalog-long-{}", std::process::id()));
    assert!(!path.exists());
    std::fs::create_dir_all(&path).unwrap();
    let workspace = Workspace(path);
    let work = workspace.0.join("index");
    let cancel = AtomicBool::new(false);
    let started = std::time::Instant::now();
    let index = disk::build_index(
        &source,
        &work,
        &identity,
        IndexLimits::default(),
        &cancel,
        |_| true,
    )
    .unwrap();
    let build_seconds = started.elapsed().as_secs_f64();
    assert_eq!(index.info().global_pairing.complete_spans, 3_275_660);
    let info = index.info().clone();
    let output = workspace.0.join("rows.jsonl");
    let mut file = File::create_new(&output).unwrap();
    let mut cursor = None;
    let mut previous = None;
    let mut rows = 0u64;
    let mut bytes = 0u64;
    let mut totals = [0u64; 3];
    loop {
        let page = index
            .page(
                cursor.as_ref(),
                PageLimits {
                    entries: 4096,
                    ..Default::default()
                },
                &cancel,
                |_| true,
            )
            .unwrap();
        assert_eq!(page.identity, identity);
        for entry in &page.entries {
            let row = Wire::from(entry, &page.zones);
            assert!(previous.is_none_or(|old| old < row.position()));
            previous = Some(row.position());
            let mut encoded = serde_json::to_vec(&row).unwrap();
            encoded.push(b'\n');
            assert!(encoded.len() <= MAX_LINE);
            bytes = bytes
                .checked_add(encoded.len() as u64)
                .filter(|&n| n <= MAX_FILE)
                .unwrap();
            file.write_all(&encoded).unwrap();
            rows = rows.checked_add(1).unwrap();
            for (sum, n) in totals
                .iter_mut()
                .zip([row.complete, row.open, row.unmatched])
            {
                *sum = sum.checked_add(n).unwrap();
            }
        }
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
        assert_eq!(cursor.as_ref().unwrap().after, previous.unwrap());
    }
    file.flush().unwrap();
    drop(file);
    assert_eq!(std::fs::metadata(&output).unwrap().len(), bytes);
    assert!(rows > 100_000);
    assert_eq!(rows, info.series);
    assert_eq!(
        totals,
        [
            info.global_pairing.complete_spans,
            info.global_pairing.open_starts,
            info.global_pairing.unmatched_stops
        ]
    );
    drop(index);
    assert!(!work.exists());
    let oracle_started = std::time::Instant::now();
    let mut key_cursor = None;
    let mut checked = 0u64;
    let mut oracle_pages = 0;
    loop {
        let page = catalog_page_streaming(
            &mut File::open(&source).unwrap(),
            key_cursor.as_ref(),
            4096,
            &cancel,
            |_| true,
        )
        .unwrap();
        assert_eq!(page.identity, identity);
        let mut expected = BTreeMap::<ExactKey, Wire>::new();
        let mut page_bytes = 0usize;
        for entry in &page.entries {
            let row = Wire::from(entry, &page.zones);
            page_bytes = page_bytes
                .checked_add(size_of::<(ExactKey, Wire)>() + 128 + row.names_bytes())
                .filter(|&n| n <= EXPECTED_PAGE_BYTES)
                .unwrap();
            assert!(expected.insert(entry.key, row).is_none());
        }
        key_cursor = page.next;
        drop(page.entries);
        drop(page.zones);
        let mut found = BTreeSet::new();
        let mut reader = BufReader::with_capacity(65536, File::open(&output).unwrap());
        while let Some(row) = read_wire(&mut reader) {
            let key = ExactKey::from(row.key);
            if let Some(expected) = expected.get(&key) {
                assert!(found.insert(key), "duplicate key across first-start pages");
                assert_eq!(&row, expected);
            }
        }
        assert_eq!(found.len(), expected.len());
        checked = checked.checked_add(found.len() as u64).unwrap();
        oracle_pages += 1;
        println!("catalog_long_oracle page={oracle_pages} checked={checked} scratch_bytes={bytes}");
        if key_cursor.is_none() {
            break;
        }
    }
    assert_eq!(checked, rows);
    println!(
        "catalog_long {}",
        serde_json::json!({"keys":rows,"spans":totals[0],"oracle_pages":oracle_pages,"scratch_bytes":bytes,"index_peak_disk":info.peak_disk_bytes,"build_seconds":build_seconds,"oracle_seconds":oracle_started.elapsed().as_secs_f64(),"scope":"functional bounded oracle, not isolated performance"})
    );
    std::fs::remove_file(output).unwrap();
    assert_eq!(std::fs::read_dir(&workspace.0).unwrap().count(), 0);
}
