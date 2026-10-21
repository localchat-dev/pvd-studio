use pvd_core::analysis::view::ProfileSource;
#[path = "support/analysis.rs"]
mod analysis_support;
use analysis_support::edit_legacy_fixture;
use pvd_core::{
    Recording,
    profile_curves::{
        self, CatalogCursor, ExactKey, ReferenceScene, Status, catalog_key_streaming,
        catalog_page_streaming, catalog_streaming, sample_streaming,
    },
};
use std::{fs::File, io::Cursor, path::PathBuf, sync::atomic::AtomicBool};

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

fn frame(id: u64, timestamp: u64) -> Vec<u8> {
    let mut bytes = vec![19];
    bytes.extend(id.to_le_bytes());
    bytes.extend(3u32.to_le_bytes());
    bytes.extend(timestamp.to_le_bytes());
    bytes
}

fn payload(zone: u64, data: &[u8]) -> Vec<u8> {
    let mut bytes = vec![7];
    bytes.extend(zone.to_le_bytes());
    for value in [5, 1, 6, data.len() as u32, data.len() as u32] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.extend(data);
    bytes
}

fn profile(kind: u8, event: u16, thread: u32, context: u8, timestamp: u64) -> Vec<u8> {
    let mut bytes = vec![kind, 3];
    bytes.extend(event.to_le_bytes());
    bytes.extend(thread.to_le_bytes());
    bytes.extend([context, 0, 0]);
    bytes.extend(timestamp.to_le_bytes());
    bytes
}

fn metadata(zone: u64, name: &str, event: u16, event_name: &str) -> Vec<Vec<u8>> {
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

fn fixture(reuse: bool, ambiguous: bool, unmatched: bool, nonmonotonic: bool) -> Vec<u8> {
    let mut bytes = pvd_protocol::STREAM_MAGIC.to_le_bytes().to_vec();
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(1_000_000u64.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(group(&[
        string(1, "physx3"),
        string(2, "PxScene"),
        string(3, "frame"),
        string(4, "PxProfileZone"),
        string(5, "events"),
        string(6, "PvdU8"),
        instance(2, 9),
        instance(2, 8),
        instance(4, 10),
        instance(4, 11),
        frame(9, 100),
        frame(8, 150),
    ]));
    for (zone, event) in [(10, 1), (11, 1), (10, 2), (10, 3)] {
        bytes.extend(group(&metadata(
            zone,
            "duplicate zone",
            event,
            "duplicate event",
        )));
    }
    let mut events = Vec::new();
    for (zone, event, thread, context, start, end) in [
        (10, 1, 7, 9, 110, 120),
        (10, 1, 7, 8, 130, 140),
        (10, 1, 8, 9, 150, 170),
        (11, 1, 7, 9, 110, 119),
        (10, 2, 7, 9, 190, 210),
        (10, 3, 7, 9, 250, 260),
        (10, 1, 7, 9, 200, 200),
    ] {
        let mut data = profile(1, event, thread, context, start);
        data.extend(profile(2, event, thread, context, end));
        events.push(payload(zone, &data));
    }
    events.push(frame(9, 200));
    events.push(frame(9, if nonmonotonic { 190 } else { 300 }));
    if reuse {
        let mut destroy = vec![15];
        destroy.extend(9u64.to_le_bytes());
        events.extend([destroy, instance(2, 9)]);
    }
    events.push(payload(10, &profile(1, 3, 7, 9, 390)));
    if unmatched {
        events.push(payload(10, &profile(2, 1, 7, 8, 350)));
    }
    events.extend([frame(9, 400), frame(9, 500), frame(9, 600)]);
    bytes.extend(group(&events));
    if ambiguous {
        bytes.extend(group(&metadata(10, "renamed zone", 1, "renamed event")));
    }
    bytes
}

fn compare(bytes: &[u8]) {
    let cancelled = AtomicBool::new(false);
    let recording = Recording::from_bytes(bytes).unwrap();
    let expected = profile_curves::catalog(&recording, &cancelled).unwrap();
    let catalog = catalog_streaming(&mut Cursor::new(bytes), &cancelled, |_| true).unwrap();
    assert_eq!(catalog.entries, expected);
    assert_eq!(catalog.identity.blake3, *blake3::hash(bytes).as_bytes());
    assert_eq!(catalog.identity.frames, recording.frame_count());
    let metadata = |zones: &std::collections::BTreeMap<u64, pvd_core::analysis::ProfileZone>| {
        zones
            .iter()
            .map(|(id, zone)| (*id, zone.name.clone(), zone.events.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        metadata(&catalog.zones),
        metadata(&analysis_support::legacy_fixture(&recording).zones)
    );
    assert!(catalog.estimated_index_bytes <= 32 << 20);
    for limit in [1, 2, 3, profile_curves::MAX_CATALOG_PAGE] {
        let mut cursor = None;
        let mut actual = Vec::new();
        loop {
            let page = catalog_page_streaming(
                &mut Cursor::new(bytes),
                cursor.as_ref(),
                limit,
                &cancelled,
                |_| true,
            )
            .unwrap();
            assert_eq!(page.identity, catalog.identity);
            assert!(page.entries.len() <= limit);
            assert!(page.estimated_index_bytes <= 32 << 20);
            for entry in &page.entries {
                let expected = expected
                    .iter()
                    .find(|expected| expected.key == entry.key)
                    .unwrap();
                assert_eq!(entry, expected);
                assert_eq!(
                    page.zones[&entry.key.zone].name,
                    catalog.zones[&entry.key.zone].name
                );
                assert_eq!(
                    page.zones[&entry.key.zone].events[&entry.key.event],
                    catalog.zones[&entry.key.zone].events[&entry.key.event]
                );
            }
            actual.extend(page.entries);
            let Some(next) = page.next else { break };
            assert!(
                cursor
                    .as_ref()
                    .is_none_or(|cursor: &CatalogCursor| next.after > cursor.after)
            );
            cursor = Some(next);
        }
        let mut sorted = expected.clone();
        sorted.sort_unstable_by_key(|entry| entry.key);
        assert_eq!(actual, sorted);
    }
    for entry in &expected {
        let selected =
            catalog_key_streaming(&mut Cursor::new(bytes), entry.key, &cancelled, |_| true)
                .unwrap();
        assert_eq!(selected.entries, std::slice::from_ref(entry));
        assert_eq!(selected.identity, catalog.identity);
        assert_eq!(selected.zones.len(), 1);
        assert_eq!(selected.zones[&entry.key.zone].events.len(), 1);
        assert_eq!(
            selected.zones[&entry.key.zone].events[&entry.key.event],
            recording
                .analysis
                .profile_event_name(entry.key.zone, entry.key.event)
                .unwrap()
        );
    }
    let absent = ExactKey {
        zone: 999,
        event: 1,
        thread: 7,
        context: 9,
    };
    let selected =
        catalog_key_streaming(&mut Cursor::new(bytes), absent, &cancelled, |_| true).unwrap();
    assert!(selected.entries.is_empty());
    assert!(selected.zones.is_empty());
    for scene in [
        ReferenceScene {
            id: 9,
            generation: 0,
        },
        ReferenceScene {
            id: 9,
            generation: 1,
        },
    ] {
        for frames in [
            (0..recording.frame_count()).collect::<Vec<_>>(),
            vec![0, 2, 4, 6],
            vec![1, 3, 5],
        ] {
            for entry in &expected {
                let expected =
                    profile_curves::sample(&recording, entry.key, scene, &frames, &cancelled)
                        .unwrap();
                let report = sample_streaming(
                    &mut Cursor::new(bytes),
                    &recording,
                    entry.key,
                    scene,
                    &frames,
                    &cancelled,
                    |_| true,
                )
                .unwrap();
                assert_eq!(
                    report.points, expected,
                    "key={:?} scene={scene:?} frames={frames:?}",
                    entry.key
                );
                assert_eq!(report.identity, catalog.identity);
                assert!(report.estimated_index_bytes <= 64 << 20);
            }
        }
    }
}

#[test]
fn whole_file_identity_catalog_sparse_windows_and_ambiguous_boundaries_match_memory() {
    for (reuse, ambiguous, unmatched, nonmonotonic) in [
        (false, false, false, false),
        (true, false, false, false),
        (false, true, false, false),
        (false, false, true, false),
        (false, false, false, true),
    ] {
        compare(&fixture(reuse, ambiguous, unmatched, nonmonotonic));
    }
    let bytes = fixture(false, false, false, false);
    let catalog =
        catalog_streaming(&mut Cursor::new(&bytes), &AtomicBool::new(false), |_| true).unwrap();
    assert_eq!(catalog.entries.len(), 6);
    assert_eq!(
        catalog
            .entries
            .iter()
            .filter(|entry| entry.key.event == 1)
            .count(),
        4
    );
}

#[test]
fn complete_source_sampling_does_not_use_the_trimmed_analysis_window() {
    let bytes = fixture(false, false, false, false);
    let mut recording = Recording::from_bytes(&bytes).unwrap();
    let cancelled = AtomicBool::new(false);
    let scene = ReferenceScene {
        id: 9,
        generation: 0,
    };
    let key = ExactKey {
        zone: 10,
        event: 1,
        thread: 7,
        context: 9,
    };
    let frames = vec![0, 2, 3, 4, 5, 6];
    let expected = profile_curves::sample(&recording, key, scene, &frames, &cancelled).unwrap();
    edit_legacy_fixture(&mut recording, |analysis| {
        analysis.first_retained_frame = 4;
        analysis.spans.clear();
    });
    assert_eq!(
        profile_curves::sample(&recording, key, scene, &frames, &cancelled).unwrap()[1].status,
        Status::AnalysisExpired
    );
    let actual = sample_streaming(
        &mut Cursor::new(&bytes),
        &recording,
        key,
        scene,
        &frames,
        &cancelled,
        |_| true,
    )
    .unwrap();
    assert_eq!(actual.points, expected);
}

#[test]
fn cancellation_truncation_unknown_payload_and_sample_bounds_return_errors() {
    let bytes = fixture(false, false, false, false);
    assert!(catalog_streaming(&mut Cursor::new(&bytes), &AtomicBool::new(true), |_| true).is_err());
    let mut callbacks = 0;
    assert!(
        catalog_streaming(&mut Cursor::new(&bytes), &AtomicBool::new(false), |_| {
            callbacks += 1;
            callbacks < 4
        })
        .is_err()
    );
    assert!(callbacks >= 4);
    assert!(
        catalog_streaming(
            &mut Cursor::new(&bytes[..bytes.len() - 1]),
            &AtomicBool::new(false),
            |_| true
        )
        .is_err()
    );
    let mut unknown = bytes.clone();
    unknown.extend(group(&[payload(10, &[255, 0, 1, 0])]));
    assert!(
        catalog_streaming(&mut Cursor::new(&unknown), &AtomicBool::new(false), |_| {
            true
        })
        .unwrap_err()
        .to_string()
        .contains("unknown profile event")
    );
    let recording = Recording::from_bytes(&bytes).unwrap();
    let key = ExactKey {
        zone: 10,
        event: 1,
        thread: 7,
        context: 9,
    };
    let scene = ReferenceScene {
        id: 9,
        generation: 0,
    };
    for frames in [vec![0; 5001], vec![2, 1], vec![recording.frame_count()]] {
        assert!(
            sample_streaming(
                &mut Cursor::new(&bytes),
                &recording,
                key,
                scene,
                &frames,
                &AtomicBool::new(false),
                |_| true
            )
            .is_err()
        );
    }
}

#[test]
fn page_admission_keeps_complete_counts_for_adversarial_payload_order() {
    let mut bytes = fixture(false, false, false, false);
    for thread in (1..20).rev().chain([1, 2, 3, 19, 18]) {
        let mut data = profile(1, 1, thread, 9, 70 + u64::from(thread));
        data.extend(profile(2, 1, thread, 9, 100 + u64::from(thread)));
        bytes.extend(group(&[payload(10, &data)]));
    }
    let cancelled = AtomicBool::new(false);
    let recording = Recording::from_bytes(&bytes).unwrap();
    let mut expected = profile_curves::catalog(&recording, &cancelled).unwrap();
    expected.sort_unstable_by_key(|entry| entry.key);
    let mut result = Vec::new();
    let mut cursor = None;
    loop {
        let page = catalog_page_streaming(
            &mut Cursor::new(&bytes),
            cursor.as_ref(),
            3,
            &cancelled,
            |_| true,
        )
        .unwrap();
        result.extend(page.entries);
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(result, expected);
}

#[test]
fn pages_validate_limits_source_identity_cancellation_and_unread_tails() {
    let bytes = fixture(false, true, true, false);
    let cancel = AtomicBool::new(false);
    for limit in [0, profile_curves::MAX_CATALOG_PAGE + 1] {
        assert!(
            catalog_page_streaming(&mut Cursor::new([]), None, limit, &cancel, |_| true)
                .unwrap_err()
                .to_string()
                .contains("page limit")
        );
    }
    let first =
        catalog_page_streaming(&mut Cursor::new(&bytes), None, 1, &cancel, |_| true).unwrap();
    let cursor = first.next.unwrap();
    assert!(
        catalog_page_streaming(
            &mut Cursor::new(&bytes),
            Some(&cursor),
            1,
            &AtomicBool::new(true),
            |_| true
        )
        .is_err()
    );
    assert!(
        catalog_page_streaming(&mut Cursor::new(&bytes), Some(&cursor), 1, &cancel, |_| {
            false
        })
        .is_err()
    );
    let mut changed = bytes.clone();
    let index = changed
        .windows(b"renamed zone".len())
        .position(|window| window == b"renamed zone")
        .unwrap();
    changed[index] = b'R';
    assert_eq!(
        Recording::from_bytes(&changed).unwrap().frame_count(),
        cursor.identity.frames
    );
    assert_eq!(changed.len() as u64, cursor.identity.bytes);
    assert!(
        catalog_page_streaming(
            &mut Cursor::new(&changed),
            Some(&cursor),
            1,
            &cancel,
            |_| true
        )
        .unwrap_err()
        .to_string()
        .contains("different source")
    );
    let after_last = CatalogCursor {
        identity: cursor.identity,
        after: ExactKey {
            zone: u64::MAX,
            event: u16::MAX,
            thread: u32::MAX,
            context: u64::MAX,
        },
    };
    let empty = catalog_page_streaming(
        &mut Cursor::new(&bytes),
        Some(&after_last),
        1,
        &cancel,
        |_| true,
    )
    .unwrap();
    assert!(empty.entries.is_empty());
    assert!(empty.zones.is_empty());
    assert!(empty.next.is_none());
    for malformed in [bytes[..bytes.len() - 1].to_vec(), {
        let mut invalid = bytes.clone();
        invalid.extend(group(&[payload(10, &[255, 0, 1, 0])]));
        invalid
    }] {
        assert!(
            catalog_page_streaming(&mut Cursor::new(&malformed), None, 1, &cancel, |_| true)
                .is_err()
        );
    }
}

#[test]
fn selected_key_catalog_remains_complete_above_the_global_series_limit() {
    let mut bytes = fixture(false, false, false, false);
    for first in (100..100_101).step_by(1000) {
        let mut data = Vec::new();
        for thread in first..(first + 1000).min(100_101) {
            data.extend(profile(1, 1, thread, 9, 110));
            data.extend(profile(2, 1, thread, 9, 120));
        }
        bytes.extend(group(&[payload(10, &data)]));
    }
    let cancelled = AtomicBool::new(false);
    assert!(
        catalog_streaming(&mut Cursor::new(&bytes), &cancelled, |_| true)
            .unwrap_err()
            .to_string()
            .contains("series budget")
    );
    let key = ExactKey {
        zone: 10,
        event: 1,
        thread: 100_100,
        context: 9,
    };
    let first =
        catalog_page_streaming(&mut Cursor::new(&bytes), None, 4096, &cancelled, |_| true).unwrap();
    assert_eq!(first.entries.len(), 4096);
    assert!(first.estimated_index_bytes < 2 << 20);
    let cursor = first.next.as_ref().unwrap();
    assert_eq!(cursor.after.thread, 4192);
    let second = catalog_page_streaming(
        &mut Cursor::new(&bytes),
        Some(cursor),
        4096,
        &cancelled,
        |_| true,
    )
    .unwrap();
    assert_eq!(second.entries.len(), 4096);
    assert_eq!(second.entries[0].key.thread, 4193);
    assert!(second.entries.iter().all(|entry| entry.complete_spans == 1));
    assert_eq!(first.identity, second.identity);
    let mut all = first.entries.clone();
    all.extend(second.entries.clone());
    let mut cursor = second.next;
    while let Some(after) = cursor {
        let page = catalog_page_streaming(
            &mut Cursor::new(&bytes),
            Some(&after),
            4096,
            &cancelled,
            |_| true,
        )
        .unwrap();
        all.extend(page.entries);
        cursor = page.next;
    }
    let mut expected = catalog_streaming(
        &mut Cursor::new(fixture(false, false, false, false)),
        &cancelled,
        |_| true,
    )
    .unwrap()
    .entries;
    expected.extend((100..100_101).map(|thread| profile_curves::CatalogEntry {
        key: ExactKey {
            zone: 10,
            event: 1,
            thread,
            context: 9,
        },
        first_start: Some(110),
        complete_spans: 1,
        open_spans: 0,
        unmatched_stops: 0,
        metadata_ambiguous: false,
    }));
    expected.sort_unstable_by_key(|entry| entry.key);
    assert_eq!(all.len(), 100_007);
    assert_eq!(all, expected);
    let selected =
        catalog_key_streaming(&mut Cursor::new(&bytes), key, &cancelled, |_| true).unwrap();
    assert_eq!(selected.entries.len(), 1);
    assert_eq!(selected.entries[0].key, key);
    assert_eq!(selected.entries[0].complete_spans, 1);
    assert_eq!(selected.identity.bytes, bytes.len() as u64);
    assert_eq!(selected.identity.blake3, *blake3::hash(&bytes).as_bytes());
    assert!(selected.estimated_index_bytes < 4096);
    assert!(
        catalog_key_streaming(
            &mut Cursor::new(&bytes),
            key,
            &AtomicBool::new(true),
            |_| true
        )
        .is_err()
    );
    bytes.extend(group(&[payload(10, &[255, 0, 1, 0])]));
    assert!(
        catalog_key_streaming(&mut Cursor::new(&bytes), key, &cancelled, |_| true)
            .unwrap_err()
            .to_string()
            .contains("unknown profile event")
    );
}

#[test]
fn incomplete_rolling_frame_directories_cannot_be_sampled_as_complete_files() {
    let mut bytes = fixture(false, false, false, false);
    bytes.extend(group(
        &(7..1207)
            .map(|index| frame(9, index * 100))
            .collect::<Vec<_>>(),
    ));
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../output/profile-curve-limit-tests")
        .join(format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    let mut live = pvd_core::LiveRecording::with_history(pvd_core::live::HistoryConfig {
        directory: directory.clone(),
        disk_budget_bytes: 32 << 10,
        segment_frames: 1,
        segment_target_bytes: u64::MAX,
        min_free_disk_bytes: 0,
    })
    .unwrap();
    live.feed(&bytes).unwrap();
    let recording = live.finish().unwrap();
    let usage = recording.disk_usage().unwrap();
    assert!(usage.warning.is_none(), "{usage:?}");
    assert!(usage.retained_frames > 0, "{usage:?}");
    assert!(usage.first_frame > 0, "{usage:?}");
    assert!(usage.retained_frames < recording.frame_count());
    let key = ExactKey {
        zone: 10,
        event: 1,
        thread: 7,
        context: 9,
    };
    let result = sample_streaming(
        &mut Cursor::new(&bytes),
        &recording,
        key,
        ReferenceScene {
            id: 9,
            generation: 0,
        },
        &[usage.first_frame],
        &AtomicBool::new(false),
        |_| true,
    );
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("complete SDK frame directory")
    );
    drop(recording);
    assert!(std::fs::read_dir(&directory).unwrap().next().is_none());
    std::fs::remove_dir(&directory).unwrap();
}

#[test]
#[ignore = "requires preserved official SDK QPC profile captures"]
fn sdk_catalog_and_every_exact_series_match_complete_memory_sampling() {
    let root = PathBuf::from(
        std::env::var_os("PVD_PROFILE_STREAM_ROOT").expect("PVD_PROFILE_STREAM_ROOT"),
    );
    for filename in [
        "physx-3.4-profile-events.pvdstream",
        "physx-3.4-profile-metrics.pvdstream",
    ] {
        let path = root.join("captures").join(filename);
        let bytes = std::fs::read(&path).unwrap();
        let recording = Recording::from_bytes(&bytes).unwrap();
        let truth: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path.with_extension("json")).unwrap()).unwrap();
        let cancelled = AtomicBool::new(false);
        let catalog =
            catalog_streaming(&mut File::open(&path).unwrap(), &cancelled, |_| true).unwrap();
        assert_eq!(
            catalog.entries,
            profile_curves::catalog(&recording, &cancelled).unwrap()
        );
        let mut cursor = None;
        let mut paged = Vec::new();
        loop {
            let page = catalog_page_streaming(
                &mut File::open(&path).unwrap(),
                cursor.as_ref(),
                17,
                &cancelled,
                |_| true,
            )
            .unwrap();
            assert_eq!(page.identity, catalog.identity);
            paged.extend(page.entries);
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
        }
        let mut expected = catalog.entries.clone();
        expected.sort_unstable_by_key(|entry| entry.key);
        assert_eq!(paged, expected);
        let scene = ReferenceScene {
            id: truth["scene"].as_u64().unwrap(),
            generation: 0,
        };
        let frames: Vec<_> = (0..recording.frame_count()).collect();
        let mut qpc_rows = 0;
        for entry in &catalog.entries {
            let expected =
                profile_curves::sample(&recording, entry.key, scene, &frames, &cancelled).unwrap();
            let report = sample_streaming(
                &mut File::open(&path).unwrap(),
                &recording,
                entry.key,
                scene,
                &frames,
                &cancelled,
                |_| true,
            )
            .unwrap();
            assert_eq!(report.points, expected);
            assert_eq!(report.identity, catalog.identity);
            let name = &catalog.zones[&entry.key.zone].events[&entry.key.event];
            let matching: Vec<_> = truth["spans"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|span| {
                    span["event"].as_str() == Some(name.as_str())
                        && span["context"].as_u64() == Some(entry.key.context)
                        && span["thread"].as_u64().map_or(
                            entry.key.thread == pvd_core::frame_metrics::CROSS_THREAD_ID,
                            |thread| thread == u64::from(entry.key.thread),
                        )
                })
                .collect();
            if matching.is_empty() {
                continue;
            }
            for point in report
                .points
                .iter()
                .filter(|point| point.status == Status::Recorded)
            {
                let Some(previous) = point.frame.checked_sub(1) else {
                    continue;
                };
                let lower = recording.frame_info(previous).unwrap().timestamp;
                let upper = recording.frame_info(point.frame).unwrap().timestamp;
                let spans: Vec<_> = matching
                    .iter()
                    .filter(|span| {
                        span["start_lower"].as_u64().unwrap() >= lower
                            && span["end_upper"].as_u64().unwrap() <= upper
                    })
                    .collect();
                let low: u64 = spans
                    .iter()
                    .map(|span| {
                        span["end_lower"]
                            .as_u64()
                            .unwrap()
                            .saturating_sub(span["start_upper"].as_u64().unwrap())
                    })
                    .sum();
                let high: u64 = spans
                    .iter()
                    .map(|span| {
                        span["end_upper"].as_u64().unwrap() - span["start_lower"].as_u64().unwrap()
                    })
                    .sum();
                let scale = 1000.0 / truth["counter_frequency"].as_u64().unwrap() as f64;
                assert_eq!(
                    point.occurrence_count,
                    spans.len(),
                    "{name} frame {}",
                    point.frame
                );
                let ms = point.milliseconds.unwrap();
                assert!(
                    ms >= low as f64 * scale - 1e-9 && ms <= high as f64 * scale + 1e-9,
                    "{name}: {ms} outside QPC bounds"
                );
                qpc_rows += 1;
            }
        }
        assert!(qpc_rows > 0);
        eprintln!(
            "{filename}: {} exact series, {} complete source frames match, {qpc_rows} independently bounded QPC rows",
            catalog.entries.len(),
            frames.len()
        );
    }
}

#[test]
#[ignore = "requires preserved 600-second SDK stream and sufficient temporary history space"]
fn complete_long_file_retains_early_profile_curves_outside_the_analysis_window() {
    use std::io::Read;
    let path = PathBuf::from(
        std::env::var_os("PVD_PROFILE_STREAM_LONG_INPUT").expect("PVD_PROFILE_STREAM_LONG_INPUT"),
    );
    let directory = PathBuf::from(
        std::env::var_os("PVD_PROFILE_STREAM_LONG_HISTORY")
            .expect("PVD_PROFILE_STREAM_LONG_HISTORY"),
    );
    std::fs::create_dir_all(&directory).unwrap();
    let mut input = File::open(&path).unwrap();
    let mut live = pvd_core::LiveRecording::with_history(pvd_core::live::HistoryConfig {
        directory: directory.clone(),
        min_free_disk_bytes: 0,
        ..Default::default()
    })
    .unwrap();
    let mut buffer = [0; 65536];
    loop {
        let n = input.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        live.feed(&buffer[..n]).unwrap();
        let _ = live.take_update();
    }
    let recording = live.finish().unwrap();
    eprintln!(
        "loaded_long frames={} first={} analysis_first={} usage={:?}",
        recording.frame_count(),
        recording.frames.first().unwrap().index,
        recording.analysis.first_retained_frame(),
        recording.disk_usage()
    );
    assert_eq!(
        recording.frames.first().unwrap().index,
        0,
        "complete file directory exceeded current disk/history metadata budget"
    );
    let usage = recording.disk_usage().unwrap();
    assert_eq!(usage.retained_frames, recording.frame_count());
    assert!(usage.warning.is_none(), "{usage:?}");
    let cancelled = AtomicBool::new(false);
    let catalog_error =
        catalog_streaming(&mut File::open(&path).unwrap(), &cancelled, |_| true).unwrap_err();
    assert!(catalog_error.to_string().contains("series budget"));
    let scene = ReferenceScene {
        id: recording.frames.first().unwrap().scene_id,
        generation: 0,
    };
    let keys: Vec<_> = recording
        .analysis
        .profile_names()
        .filter(|names| names.zone_name == "PhysXSDK" && names.event_name == "Basic.collision")
        .map(|names| ExactKey {
            zone: names.zone,
            event: names.event,
            thread: pvd_core::frame_metrics::CROSS_THREAD_ID,
            context: scene.id,
        })
        .collect();
    assert_eq!(keys.len(), 1);
    let key = keys[0];
    let catalog =
        catalog_key_streaming(&mut File::open(&path).unwrap(), key, &cancelled, |_| true).unwrap();
    assert_eq!(catalog.identity.frames, recording.frame_count());
    assert_eq!(catalog.identity.bytes, input.metadata().unwrap().len());
    assert_eq!(catalog.entries.len(), 1);
    let entry = &catalog.entries[0];
    assert_eq!(entry.key, key);
    let sampling = profile_curves::sampling_frames(&recording, scene, &cancelled).unwrap();
    assert!(sampling.frames.len() <= 5000);
    let report = sample_streaming(
        &mut File::open(&path).unwrap(),
        &recording,
        entry.key,
        scene,
        &sampling.frames,
        &cancelled,
        |_| true,
    )
    .unwrap();
    assert_eq!(report.identity, catalog.identity);
    assert!(report.points.iter().any(|point| point.frame
        < recording.analysis.first_retained_frame()
        && point.status == Status::Recorded));
    let windowed =
        profile_curves::sample(&recording, entry.key, scene, &sampling.frames, &cancelled).unwrap();
    let mut tail_matches = 0;
    for (actual, expected) in report.points.iter().zip(&windowed) {
        if expected.frame > recording.analysis.first_retained_frame() {
            assert_eq!(actual, expected);
            tail_matches += 1;
        }
    }
    assert!(tail_matches > 0);
    eprintln!(
        "long_complete_profile {}",
        serde_json::json!({"identity":{"blake3":blake3::Hash::from(report.identity.blake3).to_hex().to_string(),"bytes":report.identity.bytes,"frames":report.identity.frames},"key":{"zone":key.zone,"event":key.event,"thread":key.thread,"context":key.context},"full_catalog":"rejected_series_budget","filtered_catalog_entries":catalog.entries.len(),"catalog_index_bytes":catalog.estimated_index_bytes,"sample_index_bytes":report.estimated_index_bytes,"points":report.points.len(),"stride":sampling.stride,"analysis_first_retained_frame":recording.analysis.first_retained_frame(),"early_recorded":report.points.iter().filter(|point| point.frame < recording.analysis.first_retained_frame() && point.status==Status::Recorded).count(),"tail_matches":tail_matches,"series_complete_spans":entry.complete_spans})
    );
    drop(recording);
    let remaining = std::fs::read_dir(&directory).unwrap().count();
    assert_eq!(
        remaining, 0,
        "temporary history remained after recording drop"
    );
}

#[test]
#[ignore = "requires the preserved 600-second SDK capture; scans without building history"]
fn complete_long_catalog_pages_remain_bounded_above_the_global_series_limit() {
    let path = PathBuf::from(
        std::env::var_os("PVD_PROFILE_PAGES_INPUT").expect("PVD_PROFILE_PAGES_INPUT"),
    );
    let cancel = AtomicBool::new(false);
    let first =
        catalog_page_streaming(&mut File::open(&path).unwrap(), None, 4096, &cancel, |_| {
            true
        })
        .unwrap();
    let second = catalog_page_streaming(
        &mut File::open(&path).unwrap(),
        first.next.as_ref(),
        4096,
        &cancel,
        |_| true,
    )
    .unwrap();
    assert_eq!(first.identity, second.identity);
    assert_eq!(first.identity.frames, 19293);
    assert_eq!(first.identity.bytes, 251_301_039);
    assert_eq!(
        blake3::Hash::from(first.identity.blake3).to_hex().as_str(),
        "9a99d0525325e7ad2085bb2db7e175eb53e4d0be331d68ff1373f41bb9a181f1"
    );
    for page in [&first, &second] {
        assert_eq!(page.entries.len(), 4096);
        assert!(
            page.entries
                .windows(2)
                .all(|entries| entries[0].key < entries[1].key)
        );
        assert!(page.entries.iter().all(|entry| entry.complete_spans > 0
            || entry.open_spans > 0
            || entry.unmatched_stops > 0));
        assert!(page.estimated_index_bytes < 2 << 20);
        assert!(page.next.is_some());
    }
    assert!(first.entries.last().unwrap().key < second.entries[0].key);
    let known = ExactKey {
        zone: 2_271_340_683_392,
        event: 24,
        thread: 99_999_789,
        context: 2_271_345_934_976,
    };
    let catalog =
        catalog_key_streaming(&mut File::open(&path).unwrap(), known, &cancel, |_| true).unwrap();
    assert_eq!(catalog.identity, first.identity);
    assert_eq!(catalog.entries[0].complete_spans, 19293);
    if let Some(entry) = first
        .entries
        .iter()
        .chain(&second.entries)
        .find(|entry| entry.key == known)
    {
        assert_eq!(std::slice::from_ref(entry), catalog.entries);
    } else {
        let before = CatalogCursor {
            identity: first.identity.clone(),
            after: ExactKey {
                context: known.context - 1,
                ..known
            },
        };
        let page = catalog_page_streaming(
            &mut File::open(&path).unwrap(),
            Some(&before),
            1,
            &cancel,
            |_| true,
        )
        .unwrap();
        assert_eq!(page.entries, catalog.entries);
    }
    let mut bytes_at_cancel = 0;
    assert!(
        catalog_page_streaming(
            &mut File::open(&path).unwrap(),
            first.next.as_ref(),
            4096,
            &cancel,
            |status| {
                bytes_at_cancel = status.bytes_read;
                status.bytes_read < 4 << 20
            }
        )
        .is_err()
    );
    assert!(bytes_at_cancel >= 4 << 20 && bytes_at_cancel < first.identity.bytes);
    if let Some(report) = std::env::var_os("PVD_PROFILE_PAGES_REPORT") {
        let key = |entry: &profile_curves::CatalogEntry| serde_json::json!({ "zone": entry.key.zone, "event": entry.key.event, "thread": entry.key.thread, "context": entry.key.context, "complete_spans": entry.complete_spans, "open_spans": entry.open_spans, "unmatched_stops": entry.unmatched_stops, "first_start": entry.first_start, "ambiguous": entry.metadata_ambiguous });
        std::fs::write(report, serde_json::to_vec_pretty(&serde_json::json!({
            "input": path, "source_blake3": blake3::Hash::from(first.identity.blake3).to_hex().to_string(), "bytes": first.identity.bytes, "frames": first.identity.frames,
            "page1_entries": first.entries.len(), "page2_entries": second.entries.len(), "page1_estimated_bytes": first.estimated_index_bytes, "page2_estimated_bytes": second.estimated_index_bytes,
            "page1_first": key(&first.entries[0]), "page1_last": key(first.entries.last().unwrap()), "page2_first": key(&second.entries[0]), "page2_last": key(second.entries.last().unwrap()),
            "known_key": key(&catalog.entries[0]), "cancelled_at_bytes": bytes_at_cancel,
            "history_created": false, "scope": "two bounded pages plus explicit known key; no total distinct count claimed"
        })).unwrap()).unwrap();
    }
}
