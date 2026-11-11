use pvd_core::{
    LiveRecording,
    analysis::ProfileSpan,
    frame_sources::{FrameSection, FrameSourceKey},
    live::HistoryConfig,
    profile_disk_index::{self as disk, IndexLimits},
    profile_timeline::{self as timeline, TickRange, WindowLimits, WindowRequest},
};
use serde_json::Value;
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

fn json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn fields(span: &ProfileSpan) -> (u64, u16, u32, u64, u64, u64, usize) {
    (
        span.zone,
        span.event,
        span.thread,
        span.context,
        span.start,
        span.end,
        span.received_frame,
    )
}

#[test]
#[ignore = "requires an official APEX ALL capture and independent profile-oracle.py output"]
fn official_apex_profile_matches_raw_events_and_source_clocks() {
    let capture = PathBuf::from(std::env::var_os("PVD_APEX_PROFILE_CAPTURE").unwrap());
    let oracle = PathBuf::from(std::env::var_os("PVD_APEX_PROFILE_ORACLE").unwrap());
    let summary = json(&oracle.join("summary.json"));
    let sections = json(&oracle.join("sections.json"));
    let metadata = json(&oracle.join("metadata.json"));
    let expected: Vec<ProfileSpan> =
        serde_json::from_slice(&fs::read(oracle.join("spans.json")).unwrap()).unwrap();
    let bytes = fs::read(&capture).unwrap();
    assert_eq!(
        bytes.len() as u64,
        summary["capture_bytes"].as_u64().unwrap()
    );
    assert_eq!(crc32fast::hash(&bytes), summary["capture_crc32"]);
    assert!(!expected.is_empty());
    let cancelled = AtomicBool::new(false);
    let started = Instant::now();
    let catalog = timeline::catalog_page_streaming(
        &mut File::open(&capture).unwrap(),
        None,
        4096,
        &cancelled,
        |_| true,
    )
    .unwrap();
    assert!(catalog.next.is_none());
    assert_eq!(catalog.identity.blake3, *blake3::hash(&bytes).as_bytes());
    assert_eq!(catalog.identity.frames as u64, summary["source_frames"]);
    assert_eq!(catalog.clock.numerator, summary["clock"]["numerator"]);
    assert_eq!(catalog.clock.denominator, summary["clock"]["denominator"]);
    assert_eq!(
        catalog.global_pairing.complete_spans,
        summary["completed_spans"]
    );
    assert_eq!(catalog.global_pairing.open_starts, summary["open_starts"]);
    assert_eq!(
        catalog.global_pairing.unmatched_stops,
        summary["unmatched_stops"]
    );
    let ticks = TickRange {
        start: expected.iter().map(|span| span.start).min().unwrap(),
        end: expected.iter().map(|span| span.end).max().unwrap(),
    };
    assert_eq!(catalog.completed_tick_range, Some(ticks));
    let query = WindowRequest {
        ticks,
        thread: None,
        context: None,
        limits: WindowLimits::default(),
    };
    let direct = timeline::window_streaming(
        &mut File::open(&capture).unwrap(),
        &catalog.identity,
        query,
        &cancelled,
        |_| true,
    )
    .unwrap();
    let direct_ms = started.elapsed().as_secs_f64() * 1000.0;
    let expected_fields: Vec<_> = expected.iter().map(fields).collect();
    assert_eq!(
        direct.spans.iter().map(fields).collect::<Vec<_>>(),
        expected_fields
    );
    for span in &expected {
        let name = metadata
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["zone"] == span.zone && row["event"] == span.event)
            .unwrap();
        let names = &direct.names[&(span.zone, span.event)];
        assert_eq!(names.event.as_deref(), name["name"].as_str());
        assert_eq!(
            names.zone.as_deref(),
            summary["zones"][span.zone.to_string()].as_str()
        );
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let scratch = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../output")
        .join(format!("apex-profile-{}-{nonce}", std::process::id()));
    fs::create_dir(&scratch).unwrap();
    let index_path = scratch.join("index");
    let started = Instant::now();
    let index = disk::build_index(
        &capture,
        &index_path,
        &catalog.identity,
        IndexLimits {
            memory_bytes: 4 << 20,
            disk_bytes: 64 << 20,
            run_bytes: 16 << 10,
            page_records: 31,
            merge_fan_in: 3,
        },
        &cancelled,
        |_| true,
    )
    .unwrap();
    assert!(index.info().initial_runs > 1);
    assert!(index.info().merge_passes > 0);
    let indexed = index.window(query, &cancelled, |_| true).unwrap();
    let indexed_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(
        indexed.spans.iter().map(fields).collect::<Vec<_>>(),
        expected_fields
    );
    assert_eq!(indexed.names, direct.names);
    assert_eq!(indexed.clock, direct.clock);
    assert_eq!(indexed.global_pairing, direct.global_pairing);
    drop(index);
    assert!(!index_path.exists());

    let history_path = scratch.join("history");
    let mut live = LiveRecording::with_history(HistoryConfig {
        directory: history_path.clone(),
        disk_budget_bytes: 256 << 20,
        segment_target_bytes: 8 << 20,
        segment_frames: 8,
        min_free_disk_bytes: 0,
    })
    .unwrap();
    for chunk in bytes.chunks(997) {
        live.feed(chunk).unwrap();
    }
    let recording = live.finish().unwrap();
    let ends: Vec<_> = sections
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["kind"] == "end")
        .collect();
    assert_eq!(recording.frame_count(), ends.len());
    assert_eq!(recording.frame_sources.sources().count(), 2);
    for end in ends {
        let frame = end["frame_index"].as_u64().unwrap() as usize;
        let expected_key = FrameSourceKey::Captured {
            section: match end["name"].as_str().unwrap() {
                "frame" => FrameSection::PhysX,
                "ApexFrame" => FrameSection::Apex,
                other => panic!("unexpected section {other}"),
            },
            source: end["id"].as_u64().unwrap(),
        };
        let (key, source_frame) = recording.frame_sources.frame(frame).unwrap();
        assert_eq!(key, expected_key);
        assert_eq!(source_frame.index, frame);
        assert_eq!(source_frame.ordinal as u64, end["source_ordinal"]);
        let object = source_frame.source_object.unwrap();
        assert_eq!(object.id, end["id"]);
        assert_eq!(object.generation, end["source_generation"]);
        assert_eq!(recording.frames[frame].timestamp, end["tick"]);
    }
    drop(recording);
    assert!(fs::read_dir(&history_path).unwrap().next().is_none());
    fs::remove_dir(&history_path).unwrap();
    fs::remove_dir(&scratch).unwrap();
    println!(
        "{}",
        serde_json::json!({"matched_spans": expected.len(), "matched_fields": 7,
            "source_frames": catalog.identity.frames, "source_count": 2,
            "catalog_and_direct_ms": direct_ms, "build_and_index_query_ms": indexed_ms})
    );
}
