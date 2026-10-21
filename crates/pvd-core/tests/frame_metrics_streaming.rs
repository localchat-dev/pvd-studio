use pvd_core::analysis::view::ProfileSource;
#[path = "support/analysis.rs"]
mod analysis_support;
use analysis_support::edit_legacy_fixture;
use pvd_core::{
    Recording,
    analysis_export::AnalysisExportPhase,
    frame_metrics::{self, Metric, Point, Status},
    profile_curves::ReferenceScene,
};
use std::{
    io::{self, Cursor, Read, Seek, SeekFrom},
    sync::atomic::{AtomicBool, Ordering},
};

fn text(value: &str) -> Vec<u8> {
    let mut out = (value.len() as u32 + 1).to_le_bytes().to_vec();
    out.extend(value.as_bytes());
    out.push(0);
    out
}

fn string(id: u32, value: &str) -> Vec<u8> {
    let mut out = vec![1];
    out.extend(text(value));
    out.extend(id.to_le_bytes());
    out
}

fn instance(class: u32, id: u64) -> Vec<u8> {
    let mut out = vec![6];
    out.extend(1u32.to_le_bytes());
    out.extend(class.to_le_bytes());
    out.extend(id.to_le_bytes());
    out
}

fn frame(scene: u64, tick: u64) -> Vec<u8> {
    let mut out = vec![19];
    out.extend(scene.to_le_bytes());
    out.extend(3u32.to_le_bytes());
    out.extend(tick.to_le_bytes());
    out
}

fn definition(zone: u64, zone_name: &str, event: u16, event_name: &str) -> Vec<Vec<u8>> {
    let mut zone_def = vec![24];
    zone_def.extend(zone.to_le_bytes());
    zone_def.extend(text(zone_name));
    let mut event_def = vec![25];
    event_def.extend(zone.to_le_bytes());
    event_def.extend(text(event_name));
    event_def.extend(event.to_le_bytes());
    event_def.push(1);
    vec![zone_def, event_def]
}

fn profile(kind: u8, event: u16, thread: u32, context: u8, tick: u64) -> Vec<u8> {
    let mut out = vec![kind, 3];
    out.extend(event.to_le_bytes());
    out.extend(thread.to_le_bytes());
    out.extend([context, 0, 0]);
    out.extend(tick.to_le_bytes());
    out
}

fn payload(data: &[u8]) -> Vec<u8> {
    let mut out = vec![7];
    out.extend(10u64.to_le_bytes());
    for value in [5, 1, 6, data.len() as u32, data.len() as u32] {
        out.extend(value.to_le_bytes());
    }
    out.extend(data);
    out
}

fn span(event: u16, start: u64, end: u64) -> Vec<u8> {
    let mut data = profile(1, event, frame_metrics::CROSS_THREAD_ID, 9, start);
    data.extend(profile(2, event, frame_metrics::CROSS_THREAD_ID, 9, end));
    payload(&data)
}

fn group(events: &[Vec<u8>]) -> Vec<u8> {
    let payload: Vec<_> = events.iter().flatten().copied().collect();
    let mut out = (payload.len() as u32).to_le_bytes().to_vec();
    out.extend((events.len() as u32).to_le_bytes());
    out.extend(1u64.to_le_bytes());
    out.extend(0u64.to_le_bytes());
    out.extend(payload);
    out
}

fn fixture(reuse: bool, nonmonotonic: bool, late_names: bool) -> Vec<u8> {
    let mut out = pvd_protocol::STREAM_MAGIC.to_le_bytes().to_vec();
    out.extend(1u32.to_le_bytes());
    out.extend(1_000_000u64.to_le_bytes());
    out.extend(1u64.to_le_bytes());
    out.extend(1u32.to_le_bytes());
    let mut events = vec![
        string(1, "physx3"),
        string(2, "PxScene"),
        string(3, "frame"),
        string(4, "PxProfileZone"),
        string(5, "events"),
        string(6, "PvdU8"),
        instance(2, 9),
        instance(2, 8),
        instance(4, 10),
    ];
    let definitions: Vec<_> = [Metric::Simulate, Metric::Collision, Metric::Solver]
        .iter()
        .enumerate()
        .flat_map(|(index, metric)| definition(10, "PhysXSDK", index as u16 + 1, metric.name()))
        .collect();
    if !late_names {
        events.extend(definitions.clone());
    }
    events.extend([
        frame(9, 100),
        frame(8, 150),
        frame(9, 200),
        frame(9, if nonmonotonic { 190 } else { 300 }),
    ]);
    if reuse {
        let mut destroy = vec![15];
        destroy.extend(9u64.to_le_bytes());
        events.extend([destroy, instance(2, 9)]);
    }
    events.extend([frame(9, 400), frame(9, 500), frame(9, 600)]);
    events.extend([
        span(1, 20, 50),
        span(1, 110, 180),
        span(1, 120, 150),
        span(1, 180, 220),
        span(1, 450, 460),
    ]);
    events.extend([
        span(2, 20, 30),
        span(2, 110, 120),
        span(2, 350, 370),
        span(2, 560, 570),
    ]);
    events.extend([span(3, 30, 45), span(3, 110, 125), span(3, 350, 375)]);
    events.push(payload(&profile(
        1,
        3,
        frame_metrics::CROSS_THREAD_ID,
        9,
        10,
    )));
    events.push(payload(&profile(
        2,
        2,
        frame_metrics::CROSS_THREAD_ID,
        9,
        590,
    )));
    let mut irrelevant = profile(1, 1, 7, 9, 350);
    irrelevant.extend(profile(2, 1, 7, 9, 380));
    irrelevant.extend(profile(1, 1, frame_metrics::CROSS_THREAD_ID, 8, 350));
    irrelevant.extend(profile(2, 1, frame_metrics::CROSS_THREAD_ID, 8, 380));
    events.push(payload(&irrelevant));
    if late_names {
        events.extend(definitions);
    }
    out.extend(group(&events));
    out
}

fn assert_points(actual: &[Point], expected: &[Point]) {
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(actual.frame, expected.frame);
        assert_eq!(actual.status, expected.status, "frame {}", actual.frame);
        assert_eq!(
            actual.milliseconds, expected.milliseconds,
            "frame {}",
            actual.frame
        );
    }
}

fn reference(recording: &Recording, generation: u32) -> ReferenceScene {
    ReferenceScene {
        id: recording.frames[0].scene_id,
        generation,
    }
}

#[test]
fn complete_stages_match_retained_oracle_even_after_the_analysis_window_expires() {
    let cancelled = AtomicBool::new(false);
    for reuse in [false, true] {
        for nonmonotonic in [false, true] {
            for late_names in [false, true] {
                let bytes = fixture(reuse, nonmonotonic, late_names);
                let mut recording = Recording::from_bytes(&bytes).unwrap();
                let frames: Vec<_> = (0..recording.frame_count()).collect();
                for generation in [0, 1, 99] {
                    for metric in [Metric::Simulate, Metric::Collision, Metric::Solver] {
                        let expected = frame_metrics::sample(
                            &recording, 9, generation, metric, &frames, &cancelled,
                        )
                        .unwrap();
                        let result = frame_metrics::sample_streaming(
                            &mut Cursor::new(&bytes),
                            &recording,
                            reference(&recording, generation),
                            metric,
                            &frames,
                            &cancelled,
                            |_| true,
                        )
                        .unwrap();
                        assert_points(&result.points, &expected);
                        assert_eq!(result.identity.bytes, bytes.len() as u64);
                        assert_eq!(result.identity.blake3, *blake3::hash(&bytes).as_bytes());
                        assert_eq!(result.open_spans, usize::from(metric == Metric::Solver));
                        assert_eq!(
                            result.unmatched_stops,
                            u64::from(metric == Metric::Collision)
                        );
                        assert!(result.estimated_index_bytes <= 64 << 20);
                    }
                }
                let expected =
                    frame_metrics::sample(&recording, 9, 0, Metric::Simulate, &frames, &cancelled)
                        .unwrap();
                let count = recording.frame_count();
                edit_legacy_fixture(&mut recording, |analysis| {
                    analysis.spans.clear();
                    analysis.first_retained_frame = count;
                });
                let result = frame_metrics::sample_streaming(
                    &mut Cursor::new(&bytes),
                    &recording,
                    reference(&recording, 0),
                    Metric::Simulate,
                    &frames,
                    &cancelled,
                    |_| true,
                )
                .unwrap();
                assert_points(&result.points, &expected);
            }
        }
    }
}

#[test]
fn duplicates_first_frame_cross_boundary_and_pending_keep_the_fixed_stage_definition() {
    let bytes = fixture(false, false, false);
    let recording = Recording::from_bytes(&bytes).unwrap();
    let generation = recording.state_at(0).unwrap().objects[&9].generation;
    let frames: Vec<_> = (0..recording.frame_count()).collect();
    let cancelled = AtomicBool::new(false);
    let simulate = frame_metrics::sample_streaming(
        &mut Cursor::new(&bytes),
        &recording,
        reference(&recording, generation),
        Metric::Simulate,
        &frames,
        &cancelled,
        |_| true,
    )
    .unwrap();
    assert_eq!(simulate.points[0].milliseconds, Some(30.0));
    assert_eq!(simulate.points[1].status, Status::OtherScene);
    assert_eq!(simulate.points[2].status, Status::Ambiguous);
    assert_eq!(simulate.points[3].status, Status::CrossBoundary);
    assert_eq!(simulate.points[4].status, Status::Missing);
    assert_eq!(simulate.points[5].milliseconds, Some(10.0));
    let solver = frame_metrics::sample_streaming(
        &mut Cursor::new(&bytes),
        &recording,
        reference(&recording, generation),
        Metric::Solver,
        &frames,
        &cancelled,
        |_| true,
    )
    .unwrap();
    assert_eq!(solver.open_spans, 1);
    assert_eq!(solver.points[0].milliseconds, Some(15.0));
    assert_eq!(solver.points[2].milliseconds, Some(15.0));
}

#[test]
fn only_actual_redefinitions_affecting_the_selected_stage_are_rejected() {
    for (zone, event, zone_name, event_name, rejected) in [
        (10, 1, "PhysXSDK", "Basic.simulate", false),
        (10, 1, "PhysXSDK", "not_a_stage", true),
        (10, 1, "not_the_sdk", "Basic.simulate", true),
        (10, 2, "PhysXSDK", "custom_collision", false),
        (11, 1, "custom_zone", "custom_event", false),
    ] {
        let mut bytes = fixture(false, false, false);
        bytes.extend(group(&definition(zone, zone_name, event, event_name)));
        let recording = Recording::from_bytes(&bytes).unwrap();
        let result = frame_metrics::sample_streaming(
            &mut Cursor::new(&bytes),
            &recording,
            reference(&recording, 0),
            Metric::Simulate,
            &[0],
            &AtomicBool::new(false),
            |_| true,
        );
        assert_eq!(result.is_err(), rejected, "{zone_name}/{event_name}");
        if let Err(error) = result {
            assert!(error.to_string().contains("redefined profile metadata"));
        }
    }
    let mut bytes = fixture(false, false, false);
    bytes.extend(group(&definition(11, "custom_a", 1, "custom_a")));
    bytes.extend(group(&definition(11, "custom_b", 1, "custom_b")));
    let recording = Recording::from_bytes(&bytes).unwrap();
    assert!(
        frame_metrics::sample_streaming(
            &mut Cursor::new(&bytes),
            &recording,
            reference(&recording, 0),
            Metric::Simulate,
            &[0],
            &AtomicBool::new(false),
            |_| true
        )
        .is_ok()
    );
    bytes.extend(group(&definition(11, "PhysXSDK", 1, "Basic.simulate")));
    let recording = Recording::from_bytes(&bytes).unwrap();
    let error = frame_metrics::sample_streaming(
        &mut Cursor::new(&bytes),
        &recording,
        reference(&recording, 0),
        Metric::Simulate,
        &[0],
        &AtomicBool::new(false),
        |_| true,
    )
    .unwrap_err();
    assert!(error.to_string().contains("redefined profile metadata"));
}

struct ChangingReader {
    bytes: Cursor<Vec<u8>>,
    scans: usize,
    replacement: Vec<u8>,
}
impl Read for ChangingReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.bytes.read(out)
    }
}
impl Seek for ChangingReader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        if position == SeekFrom::End(0) {
            self.scans += 1;
            if self.scans == 2 {
                self.bytes = Cursor::new(self.replacement.clone());
            }
        }
        self.bytes.seek(position)
    }
}

#[test]
fn cancellation_invalid_ranges_and_between_scan_source_changes_do_not_return_partial_points() {
    let bytes = fixture(false, false, false);
    let recording = Recording::from_bytes(&bytes).unwrap();
    let scene = reference(&recording, 0);
    for phase in [AnalysisExportPhase::Inspect, AnalysisExportPhase::Export] {
        let cancelled = AtomicBool::new(false);
        let result = frame_metrics::sample_streaming(
            &mut Cursor::new(&bytes),
            &recording,
            scene,
            Metric::Simulate,
            &[0, 2, 3],
            &cancelled,
            |status| {
                if status.phase == phase {
                    cancelled.store(true, Ordering::Relaxed);
                }
                true
            },
        );
        assert!(result.unwrap_err().to_string().contains("cancelled"));
    }
    let changed = fixture(false, true, false);
    assert_eq!(bytes.len(), changed.len());
    let mut source = ChangingReader {
        bytes: Cursor::new(bytes.clone()),
        scans: 0,
        replacement: changed,
    };
    let error = frame_metrics::sample_streaming(
        &mut source,
        &recording,
        scene,
        Metric::Simulate,
        &[0],
        &AtomicBool::new(false),
        |_| true,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("changed between stage sampling scans")
    );
    for frames in [vec![2, 0], vec![0, 0], vec![99], vec![0; 5001]] {
        assert!(
            frame_metrics::sample_streaming(
                &mut Cursor::new(&bytes),
                &recording,
                scene,
                Metric::Simulate,
                &frames,
                &AtomicBool::new(false),
                |_| true
            )
            .is_err()
        );
    }
    assert!(
        frame_metrics::sample_streaming(
            &mut Cursor::new(&bytes),
            &recording,
            scene,
            Metric::FrameInterval,
            &[0],
            &AtomicBool::new(false),
            |_| true
        )
        .is_err()
    );
    let mut broken = bytes.clone();
    broken.pop();
    assert!(
        frame_metrics::sample_streaming(
            &mut Cursor::new(&broken),
            &recording,
            scene,
            Metric::Simulate,
            &[0],
            &AtomicBool::new(false),
            |_| true
        )
        .is_err()
    );
}

#[test]
#[ignore = "requires official SDK 3.4 capture and independent QPC callback bounds"]
fn complete_fixed_stages_match_independent_sdk_qpc_bounds_after_window_expiry() {
    let path = std::env::var_os("PVD_PROFILE_METRICS_CAPTURE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../captures/physx-3.4-profile-metrics.pvdstream")
        });
    let bytes = std::fs::read(&path).unwrap();
    let truth: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path.with_extension("json")).unwrap()).unwrap();
    let mut recording = Recording::from_bytes(&bytes).unwrap();
    let scene = truth["scene"].as_u64().unwrap();
    let generation = recording.state_at(0).unwrap().objects[&scene].generation;
    let frames: Vec<_> = (0..recording.frame_count()).collect();
    let cancelled = AtomicBool::new(false);
    let original = recording.analysis.clone();
    let count = recording.frame_count();
    edit_legacy_fixture(&mut recording, |analysis| {
        analysis.spans.clear();
        analysis.first_retained_frame = count;
    });
    let scale = 1000.0 / truth["counter_frequency"].as_u64().unwrap() as f64;
    let mut checked = 0;
    for metric in [Metric::Simulate, Metric::Collision, Metric::Solver] {
        let report = frame_metrics::sample_streaming(
            &mut Cursor::new(&bytes),
            &recording,
            ReferenceScene {
                id: scene,
                generation,
            },
            metric,
            &frames,
            &cancelled,
            |_| true,
        )
        .unwrap();
        for expected in truth["spans"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|span| span["event"].as_str() == Some(metric.name()))
        {
            let frame = expected["frame"].as_u64().unwrap() as usize;
            let start = expected["start_lower"].as_u64().unwrap()
                ..=expected["start_upper"].as_u64().unwrap();
            let end =
                expected["end_lower"].as_u64().unwrap()..=expected["end_upper"].as_u64().unwrap();
            let matching: Vec<_> = original
                .profile_spans()
                .filter(|span| {
                    span.context == scene
                        && span.thread == frame_metrics::CROSS_THREAD_ID
                        && start.contains(&span.start)
                        && end.contains(&span.end)
                        && original.profile_event_name(span.zone, span.event).unwrap()
                            == metric.name()
                })
                .collect();
            assert_eq!(matching.len(), 1);
            let value = report.points[frame].milliseconds.unwrap();
            assert_eq!(report.points[frame].status, Status::Recorded);
            assert!((value - (matching[0].end - matching[0].start) as f64 * scale).abs() < 1e-9);
            let lower = expected["end_lower"]
                .as_u64()
                .unwrap()
                .saturating_sub(expected["start_upper"].as_u64().unwrap())
                as f64
                * scale;
            let upper = expected["end_upper"]
                .as_u64()
                .unwrap()
                .saturating_sub(expected["start_lower"].as_u64().unwrap())
                as f64
                * scale;
            assert!((lower..=upper).contains(&value));
            checked += 1;
        }
        if metric == Metric::Solver {
            assert_eq!(report.open_spans, 24);
        }
    }
    assert_eq!(checked, 72);
    println!(
        "72 complete stage durations within independent SDK QPC bounds; first frame and solver pending semantics preserved after analysis expiry"
    );
}

#[test]
#[ignore = "requires preserved long SDK capture and temporary disk history capacity"]
fn complete_long_sdk_stages_restore_early_values_and_match_individual_replay_windows() {
    let path = std::path::PathBuf::from(
        std::env::var_os("PVD_STAGE_LONG_INPUT").expect("PVD_STAGE_LONG_INPUT"),
    );
    let directory = std::path::PathBuf::from(
        std::env::var_os("PVD_STAGE_LONG_HISTORY").expect("PVD_STAGE_LONG_HISTORY"),
    );
    std::fs::create_dir_all(&directory).unwrap();
    let mut input = std::fs::File::open(&path).unwrap();
    let mut live = pvd_core::LiveRecording::with_history(pvd_core::live::HistoryConfig {
        directory: directory.clone(),
        min_free_disk_bytes: 0,
        ..Default::default()
    })
    .unwrap();
    let mut buffer = [0; 65536];
    let mut hash = blake3::Hasher::new();
    loop {
        let count = input.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
        live.feed(&buffer[..count]).unwrap();
        let _ = live.take_update();
    }
    let recording = live.finish().unwrap();
    let usage = recording.disk_usage().unwrap();
    assert_eq!(usage.first_frame, 0);
    assert_eq!(usage.retained_frames, recording.frame_count());
    assert!(usage.warning.is_none(), "{usage:?}");
    assert!(recording.analysis.first_retained_frame() > 256);
    let last = recording.frame_count() - 1;
    let mut frames = vec![
        0,
        1,
        31,
        127,
        255,
        last / 2,
        last.saturating_sub(256),
        last.saturating_sub(128),
        last,
    ];
    frames.sort_unstable();
    frames.dedup();
    let scene = ReferenceScene {
        id: recording.frames[0].scene_id,
        generation: 0,
    };
    let cancelled = AtomicBool::new(false);
    let mut snapshots = Vec::new();
    for &frame in &frames {
        let mut snapshot = recording.clone();
        // Pressure retention keeps at least 60 frames; the oracle must still own the target.
        snapshot.analysis = recording.analysis_at((frame + 32).min(last)).unwrap();
        assert!(snapshot.analysis.first_retained_frame() <= frame);
        snapshots.push(snapshot);
    }
    for metric in [Metric::Simulate, Metric::Collision, Metric::Solver] {
        let report = frame_metrics::sample_streaming(
            &mut std::fs::File::open(&path).unwrap(),
            &recording,
            scene,
            metric,
            &frames,
            &cancelled,
            |_| true,
        )
        .unwrap();
        assert_eq!(report.identity.blake3, *hash.finalize().as_bytes());
        assert_eq!(report.identity.bytes, input.metadata().unwrap().len());
        assert!(report.points.iter().any(|point| point.frame
            < recording.analysis.first_retained_frame()
            && point.status == Status::Recorded));
        let mut recorded = 0;
        for (point, snapshot) in report.points.iter().zip(&snapshots) {
            let expected = frame_metrics::sample(
                snapshot,
                scene.id,
                scene.generation,
                metric,
                &[point.frame],
                &cancelled,
            )
            .unwrap();
            assert_points(std::slice::from_ref(point), &expected);
            recorded += usize::from(point.status == Status::Recorded);
        }
        println!(
            "long_fixed_stage {}",
            serde_json::json!({"metric":metric.name(),"frames":report.identity.frames,"bytes":report.identity.bytes,"blake3":blake3::Hash::from(report.identity.blake3).to_hex().to_string(),"analysis_first":recording.analysis.first_retained_frame(),"sampled":frames,"recorded":recorded,"open":report.open_spans,"unmatched":report.unmatched_stops,"index_bytes":report.estimated_index_bytes})
        );
    }
    drop(snapshots);
    drop(recording);
    assert!(
        std::fs::read_dir(&directory).unwrap().next().is_none(),
        "temporary history survived owner release"
    );
}
