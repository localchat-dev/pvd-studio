use pvd_core::analysis::view::ProfileSource;
use pvd_core::{
    Recording,
    profile_catalog_index::{
        self as catalog_index, CatalogIndex, FirstStartCursor, FirstStartPosition, IndexLimits,
        PageLimits, Phase,
    },
    profile_curves::{self, SourceIdentity},
};
use std::{
    io::Cursor,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};
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
fn frame(tick: u64) -> Vec<u8> {
    let mut bytes = vec![19];
    bytes.extend(9u64.to_le_bytes());
    bytes.extend(3u32.to_le_bytes());
    bytes.extend(tick.to_le_bytes());
    bytes
}
fn profile(kind: u8, event: u16, thread: u32, context: u8, tick: u64) -> Vec<u8> {
    let mut bytes = vec![kind, 3];
    bytes.extend(event.to_le_bytes());
    bytes.extend(thread.to_le_bytes());
    bytes.extend([context, 0, 0]);
    bytes.extend(tick.to_le_bytes());
    bytes
}
fn payload(data: &[u8]) -> Vec<u8> {
    let mut bytes = vec![7];
    bytes.extend(10u64.to_le_bytes());
    for value in [5, 1, 6, data.len() as u32, data.len() as u32] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.extend(data);
    bytes
}
fn span(event: u16, thread: u32, context: u8, start: u64, end: u64) -> Vec<u8> {
    let mut bytes = profile(1, event, thread, context, start);
    bytes.extend(profile(2, event, thread, context, end));
    payload(&bytes)
}
fn definition(zone: u64, name: &str, event: u16, event_name: &str) -> Vec<Vec<u8>> {
    let mut first = vec![24];
    first.extend(zone.to_le_bytes());
    first.extend(text(name));
    let mut second = vec![25];
    second.extend(zone.to_le_bytes());
    second.extend(text(event_name));
    second.extend(event.to_le_bytes());
    second.push(1);
    vec![first, second]
}
fn group(events: &[Vec<u8>]) -> Vec<u8> {
    let payload: Vec<_> = events.iter().flatten().copied().collect();
    let mut bytes = (payload.len() as u32).to_le_bytes().to_vec();
    bytes.extend((events.len() as u32).to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    bytes.extend(payload);
    bytes
}
fn fixture(complete: bool) -> Vec<u8> {
    let mut bytes = pvd_protocol::STREAM_MAGIC.to_le_bytes().to_vec();
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(1_000_000u64.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    let mut events = vec![
        string(1, "physx3"),
        string(2, "PxScene"),
        string(3, "frame"),
        string(4, "PxProfileZone"),
        string(5, "events"),
        string(6, "PvdU8"),
        instance(2, 9),
        instance(4, 10),
    ];
    if complete {
        events.extend([
            payload(&profile(1, 1, 7, 9, 10)),
            payload(&profile(1, 1, 7, 9, 20)),
            payload(&profile(2, 1, 7, 9, 30)),
            frame(100),
            span(1, 8, 9, 40, 60),
            span(1, 7, 8, 40, 60),
            span(2, 7, 9, 50, 50),
            frame(200),
            span(1, 2, 9, 1, 2),
            frame(300),
            payload(&profile(2, 1, 7, 9, 90)),
        ]);
    } else {
        events.push(frame(100));
    }
    events.extend([
        payload(&profile(1, 1, 100, 9, 1000)),
        payload(&profile(2, 1, 99, 9, 9000)),
    ]);
    events.extend(definition(10, "late zone", 1, "same name"));
    events.extend(definition(10, "late zone", 2, "same name"));
    bytes.extend(group(&events));
    bytes
}
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
struct Case {
    root: std::path::PathBuf,
    source: std::path::PathBuf,
    work: std::path::PathBuf,
}
impl Case {
    fn new(bytes: &[u8]) -> Self {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../output")
            .join(format!(
                "catalog-index-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.pvdstream");
        std::fs::write(&source, bytes).unwrap();
        let work = root.join("index");
        Self { root, source, work }
    }
    fn identity(&self) -> pvd_core::profile_curves::SourceIdentity {
        profile_curves::catalog_page_streaming(
            &mut std::fs::File::open(&self.source).unwrap(),
            None,
            4096,
            &AtomicBool::new(false),
            |_| true,
        )
        .unwrap()
        .identity
    }
}
impl Drop for Case {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.source);
        let _ = std::fs::remove_dir(&self.root);
    }
}
fn limits() -> IndexLimits {
    IndexLimits {
        memory_bytes: 1 << 20,
        disk_bytes: 4 << 20,
        run_bytes: 128,
        page_records: 1,
        merge_fan_in: 2,
    }
}

fn build(case: &Case) -> CatalogIndex {
    catalog_index::build_index(
        &case.source,
        &case.work,
        &case.identity(),
        limits(),
        &AtomicBool::new(false),
        |_| true,
    )
    .unwrap()
}
fn pages(index: &CatalogIndex, limit: usize) -> Vec<profile_curves::CatalogEntry> {
    let cancel = AtomicBool::new(false);
    let mut cursor = None;
    let mut entries = Vec::new();
    loop {
        let page = index
            .page(
                cursor.as_ref(),
                PageLimits {
                    entries: limit,
                    ..Default::default()
                },
                &cancel,
                |_| true,
            )
            .unwrap();
        for entry in &page.entries {
            if let Some(last) = entries.last() {
                let last: &profile_curves::CatalogEntry = last;
                assert!(
                    FirstStartPosition {
                        first_start: last.first_start,
                        key: last.key
                    } < FirstStartPosition {
                        first_start: entry.first_start,
                        key: entry.key
                    }
                );
            }
            entries.push(entry.clone());
        }
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
        assert!(entries.len() <= 10000);
    }
    entries
}

#[test]
fn full_aggregation_precedes_time_pagination_with_late_starts_pending_and_duplicates() {
    let mut bytes = fixture(true);
    for round in 0..17 {
        bytes.extend(group(&[
            span(1, 7, 9, 400 + round, 500 + round),
            span(3, 7, 9, 300 + round, 400 + round),
            span(4, 7, 9, 200 + round, 300 + round),
        ]));
    }
    bytes.extend(group(&[
        span(3, 7, 9, 0, 1),
        span(4, 8, 9, u64::MAX, u64::MAX),
        payload(&profile(1, 9, 7, 0, u64::MAX)),
        payload(&profile(2, 7, 7, 0, 0)),
    ]));
    let mut another_zone = span(u16::MAX, u32::MAX, 255, 40, 60);
    another_zone[1..9].copy_from_slice(&11u64.to_le_bytes());
    let mut definitions = vec![instance(4, 11), another_zone];
    definitions.extend(definition(11, "other zone", u16::MAX, "other event"));
    bytes.extend(group(&definitions));
    bytes.extend(group(&definition(10, "changed zone", 1, "new name")));
    let recording = Recording::from_bytes(&bytes).unwrap();
    let expected = profile_curves::catalog(&recording, &AtomicBool::new(false)).unwrap();
    let case = Case::new(&bytes);
    let index = build(&case);
    assert!(index.info().key_merge_passes > 1);
    assert!(index.info().start_merge_passes > 1);
    assert!(index.info().peak_disk_bytes > index.info().disk_bytes);
    for count in [1, 2, 3, 4096] {
        assert_eq!(pages(&index, count), expected);
    }
    assert_eq!(index.info().series, expected.len() as u64);
    assert_eq!(
        index.info().global_pairing.complete_spans,
        recording.analysis.profile_spans().len() as u64
    );
    let last = expected.last().unwrap();
    let cursor = FirstStartCursor {
        identity: case.identity(),
        sort_version: 1,
        after: FirstStartPosition {
            first_start: last.first_start,
            key: last.key,
        },
    };
    let page = index
        .page(
            Some(&cursor),
            PageLimits::default(),
            &AtomicBool::new(false),
            |_| true,
        )
        .unwrap();
    assert!(page.entries.is_empty() && page.next.is_none());
    assert!(expected.iter().any(|e| e.first_start == Some(u64::MAX)));
    assert!(expected.last().unwrap().first_start.is_none());
    drop(index);
    assert!(!case.work.exists());
    assert_eq!(std::fs::read(&case.source).unwrap(), bytes);
}

#[test]
fn empty_pending_and_unknown_start_pages_preserve_counts_and_names() {
    for complete in [false, true] {
        let bytes = fixture(complete);
        let case = Case::new(&bytes);
        let index = build(&case);
        let recording = Recording::from_bytes(&bytes).unwrap();
        assert_eq!(
            pages(&index, 1),
            profile_curves::catalog(&recording, &AtomicBool::new(false)).unwrap()
        );
        let page = index
            .page(None, PageLimits::default(), &AtomicBool::new(false), |_| {
                true
            })
            .unwrap();
        assert_eq!(page.zones[&10].name, "late zone");
        assert_eq!(page.zones[&10].events[&1], "same name");
        drop(index);
        assert!(!case.work.exists());
    }
    let bytes = fixture(false);
    let end = 28;
    let empty = bytes[..end].to_vec();
    let case = Case::new(&empty);
    let index = build(&case);
    assert_eq!(index.info().series, 0);
    assert!(pages(&index, 1).is_empty());
    drop(index);
    assert!(!case.work.exists());
}

#[test]
fn limits_cancellation_source_binding_and_owned_files_are_enforced() {
    let case = Case::new(&fixture(true));
    let identity = case.identity();
    let cancel = AtomicBool::new(false);
    std::fs::create_dir(&case.work).unwrap();
    assert!(
        catalog_index::build_index(
            &case.source,
            &case.work,
            &identity,
            limits(),
            &cancel,
            |_| true
        )
        .is_err()
    );
    assert!(case.work.exists());
    std::fs::remove_dir(&case.work).unwrap();
    for config in [
        IndexLimits {
            memory_bytes: 1,
            ..limits()
        },
        IndexLimits {
            disk_bytes: 80,
            ..limits()
        },
    ] {
        assert!(
            catalog_index::build_index(
                &case.source,
                &case.work,
                &identity,
                config,
                &cancel,
                |_| true
            )
            .is_err()
        );
        assert!(!case.work.exists());
    }
    for phase in [Phase::Decode, Phase::Merge, Phase::Metadata] {
        assert!(
            catalog_index::build_index(
                &case.source,
                &case.work,
                &identity,
                limits(),
                &cancel,
                |p| p.phase != phase
            )
            .is_err()
        );
        assert!(!case.work.exists());
    }
    let index = build(&case);
    for phase in [Phase::VerifySource, Phase::Query, Phase::Metadata] {
        assert!(
            index
                .page(None, PageLimits::default(), &cancel, |p| p.phase != phase)
                .is_err()
        );
    }
    for config in [
        PageLimits {
            entries: 0,
            bytes: 4096,
        },
        PageLimits {
            entries: 4097,
            bytes: 4096,
        },
        PageLimits {
            entries: 1,
            bytes: 1,
        },
    ] {
        assert!(index.page(None, config, &cancel, |_| true).is_err());
    }
    let mut cursor = index
        .page(
            None,
            PageLimits {
                entries: 1,
                ..Default::default()
            },
            &cancel,
            |_| true,
        )
        .unwrap()
        .next
        .unwrap();
    cursor.sort_version = 2;
    assert!(
        index
            .page(Some(&cursor), PageLimits::default(), &cancel, |_| true)
            .is_err()
    );
    cursor.sort_version = 1;
    cursor.identity.blake3[0] ^= 1;
    assert!(
        index
            .page(Some(&cursor), PageLimits::default(), &cancel, |_| true)
            .is_err()
    );
    cancel.store(true, Ordering::Relaxed);
    assert!(
        index
            .page(None, PageLimits::default(), &cancel, |_| true)
            .is_err()
    );
    cancel.store(false, Ordering::Relaxed);
    let mut changed = std::fs::read(&case.source).unwrap();
    changed[0] ^= 1;
    std::fs::write(&case.source, &changed).unwrap();
    assert!(
        index
            .page(None, PageLimits::default(), &cancel, |_| true)
            .unwrap_err()
            .to_string()
            .contains("identity changed")
    );
    let foreign = case.work.join("unrelated");
    std::fs::write(&foreign, b"preserved").unwrap();
    drop(index);
    assert_eq!(std::fs::read(&foreign).unwrap(), b"preserved");
    assert_eq!(std::fs::read_dir(&case.work).unwrap().count(), 1);
    std::fs::remove_file(foreign).unwrap();
    std::fs::remove_dir(&case.work).unwrap();
}

#[test]
fn decoder_failures_and_corrupt_pages_never_publish_partial_catalogs() {
    let mut overflow = profile(1, 1, 7, 9, u64::MAX);
    overflow.extend([3, 0, 1, 0, 1]);
    for tail in [
        group(&[payload(&[255, 0, 1, 0])]),
        group(&[payload(&profile(1, 1, 7, 9, 50)[..8])]),
        group(&[payload(&overflow)]),
        vec![1, 2, 3],
    ] {
        let mut bytes = fixture(true);
        bytes.extend(tail);
        let case = Case::new(&bytes);
        let identity = SourceIdentity {
            blake3: *blake3::hash(&bytes).as_bytes(),
            bytes: bytes.len() as u64,
            frames: 0,
        };
        assert!(
            catalog_index::build_index(
                &case.source,
                &case.work,
                &identity,
                limits(),
                &AtomicBool::new(false),
                |_| true
            )
            .is_err()
        );
        assert!(!case.work.exists());
    }
    let case = Case::new(&fixture(true));
    let index = build(&case);
    for entry in std::fs::read_dir(&case.work).unwrap() {
        let path = entry.unwrap().path();
        let bytes = std::fs::read(&path).unwrap();
        for at in [0, bytes.len() / 2, bytes.len() - 1] {
            let mut corrupt = bytes.clone();
            corrupt[at] ^= 1;
            std::fs::write(&path, &corrupt).unwrap();
            assert!(
                index
                    .page(None, PageLimits::default(), &AtomicBool::new(false), |_| {
                        true
                    })
                    .is_err()
            );
            std::fs::write(&path, &bytes).unwrap();
        }
        std::fs::write(&path, &bytes[..bytes.len() - 1]).unwrap();
        assert!(
            index
                .page(None, PageLimits::default(), &AtomicBool::new(false), |_| {
                    true
                })
                .is_err()
        );
        std::fs::write(&path, &bytes).unwrap();
    }
    drop(index);
    assert!(!case.work.exists());
}

#[test]
fn same_layout_pages_from_another_source_or_build_cannot_replace_the_catalog() {
    let mut a = fixture(false);
    a.extend(group(&[span(1, 7, 1, 1, 2)]));
    let mut b = fixture(false);
    b.extend(group(&[span(1, 7, 2, 1, 2)]));
    let first = Case::new(&a);
    let other = Case::new(&b);
    let index = build(&first);
    let second = build(&other);
    let expected = pages(&index, 4096);
    for entry in std::fs::read_dir(&first.work).unwrap() {
        let path = entry.unwrap().path();
        let original = std::fs::read(&path).unwrap();
        if &original[..8] != b"PVDCAT01" {
            continue;
        }
        let foreign = std::fs::read(other.work.join(path.file_name().unwrap())).unwrap();
        assert_eq!(original.len(), foreign.len());
        assert_eq!(&original[..48], &foreign[..48]);
        assert_ne!(&original[48..80], &foreign[48..80]);
        let mut mixed = original.clone();
        mixed[128..].copy_from_slice(&foreign[128..]);
        std::fs::write(&path, mixed).unwrap();
        assert!(
            index
                .page(None, PageLimits::default(), &AtomicBool::new(false), |_| {
                    true
                })
                .is_err()
        );
        std::fs::write(&path, foreign).unwrap();
        assert!(
            index
                .page(None, PageLimits::default(), &AtomicBool::new(false), |_| {
                    true
                })
                .is_err()
        );
        std::fs::write(&path, original).unwrap();
    }
    assert_eq!(pages(&index, 4096), expected);
    drop((index, second));
    assert!(!first.work.exists() && !other.work.exists());
}

#[test]
fn mid_merge_cancel_write_policy_failure_and_peak_disk_budget_remove_every_registered_file() {
    let mut bytes = fixture(true);
    for index in 0..25 {
        bytes.extend(group(&[span(20, 7, index, 100, 200)]));
    }
    let case = Case::new(&bytes);
    let identity = case.identity();
    let cancel = AtomicBool::new(false);
    let mut mid_merge = false;
    assert!(
        catalog_index::build_index(
            &case.source,
            &case.work,
            &identity,
            limits(),
            &cancel,
            |p| {
                if p.phase == Phase::Merge && p.completed > 0 {
                    mid_merge = true;
                    false
                } else {
                    true
                }
            }
        )
        .is_err()
    );
    assert!(mid_merge && !case.work.exists());
    let writes = std::sync::Arc::new(AtomicU64::new(0));
    let count = writes.clone();
    let options = catalog_index::IndexOptions {
        limits: limits(),
        before_write: Some(std::sync::Arc::new(move |_| {
            if count.fetch_add(1, Ordering::Relaxed) >= 5 {
                Err(std::io::Error::other(
                    "injected catalog write policy failure",
                ))
            } else {
                Ok(())
            }
        })),
    };
    let failure = catalog_index::build_index(
        &case.source,
        &case.work,
        &identity,
        options,
        &cancel,
        |_| true,
    )
    .err()
    .unwrap();
    assert!(
        failure
            .to_string()
            .contains("injected catalog write policy failure")
    );
    assert!(!case.work.exists());
    let index = build(&case);
    let final_bytes = index.info().disk_bytes;
    assert!(index.info().peak_disk_bytes > final_bytes);
    assert!(
        index
            .page(None, PageLimits::default(), &cancel, |p| {
                if p.phase == Phase::Query && p.completed == p.total {
                    cancel.store(true, Ordering::Relaxed);
                }
                true
            })
            .is_err()
    );
    cancel.store(false, Ordering::Relaxed);
    drop(index);
    assert!(!case.work.exists());
    assert!(
        catalog_index::build_index(
            &case.source,
            &case.work,
            &identity,
            IndexLimits {
                disk_bytes: final_bytes,
                ..limits()
            },
            &cancel,
            |_| true
        )
        .is_err()
    );
    assert!(!case.work.exists());
}

#[test]
#[ignore = "bounded synthetic two-million pending-start decoder limit; writes about 38 MiB"]
fn pending_pairing_limit_fails_instead_of_publishing_a_partial_catalog() {
    use std::io::Write;
    let case = Case::new(&fixture(true));
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&case.source)
        .unwrap();
    let event = profile(1, 15, 700, 0, 500);
    let data = event.repeat(1000);
    let group = group(&[payload(&data)]);
    for _ in 0..2001 {
        file.write_all(&group).unwrap();
    }
    file.flush().unwrap();
    drop(file);
    let identity = SourceIdentity {
        blake3: [0; 32],
        bytes: std::fs::metadata(&case.source).unwrap().len(),
        frames: 0,
    };
    let failure = catalog_index::build_index(
        &case.source,
        &case.work,
        &identity,
        limits(),
        &AtomicBool::new(false),
        |_| true,
    )
    .err()
    .unwrap();
    assert!(
        failure
            .to_string()
            .contains("profile analysis exceeds two million events"),
        "{failure}"
    );
    assert!(!case.work.exists());
}

#[test]
#[ignore = "requires preserved official SDK QPC files"]
fn official_sdk_first_start_pages_match_complete_and_key_catalogs() {
    let root =
        std::path::PathBuf::from(std::env::var_os("PVD_TIMELINE_ROOT").expect("PVD_TIMELINE_ROOT"));
    for name in ["physx-3.4-profile-metrics", "physx-3.4-profile-events"] {
        let bytes = std::fs::read(root.join("captures").join(format!("{name}.pvdstream"))).unwrap();
        let case = Case::new(&bytes);
        let recording = Recording::from_bytes(&bytes).unwrap();
        let expected = profile_curves::catalog(&recording, &AtomicBool::new(false)).unwrap();
        let index = catalog_index::build_index(
            &case.source,
            &case.work,
            &case.identity(),
            IndexLimits {
                run_bytes: 4096,
                page_records: 7,
                ..limits()
            },
            &AtomicBool::new(false),
            |_| true,
        )
        .unwrap();
        let actual = pages(&index, 3);
        assert_eq!(actual, expected);
        let mut key_cursor = None;
        let mut checked = 0;
        loop {
            let page = profile_curves::catalog_page_streaming(
                &mut Cursor::new(&bytes),
                key_cursor.as_ref(),
                3,
                &AtomicBool::new(false),
                |_| true,
            )
            .unwrap();
            for entry in page.entries {
                assert_eq!(actual.iter().find(|row| row.key == entry.key), Some(&entry));
                checked += 1;
            }
            key_cursor = page.next;
            if key_cursor.is_none() {
                break;
            }
        }
        assert_eq!(checked, actual.len());
        println!(
            "first_start_sdk {name} bytes={} keys={} runs={} peak_disk={}",
            bytes.len(),
            actual.len(),
            index.info().contribution_runs,
            index.info().peak_disk_bytes
        );
        drop(index);
        assert!(!case.work.exists());
    }
}
