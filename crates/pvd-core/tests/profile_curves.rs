#[path = "support/analysis.rs"]
mod analysis_support;
use analysis_support::{edit_legacy_fixture, legacy_fixture};
use pvd_core::analysis::view::ProfileSource;
use pvd_core::{
    Recording, RecordingBuilder,
    analysis::{Analysis, ProfileSpan},
    profile_curves::{self, ExactKey, ReferenceScene, Status},
};
use pvd_protocol::{Endian, Event, EventGroup, Header, Name};
use std::{path::PathBuf, sync::atomic::AtomicBool};

const KEY: ExactKey = ExactKey {
    zone: 1,
    event: 2,
    thread: 42,
    context: 9,
};
const SCENE: ReferenceScene = ReferenceScene {
    id: 7,
    generation: 0,
};

fn fixture(reuse: bool) -> Recording {
    let mut builder = RecordingBuilder::new(Header {
        version: 1,
        timestamp_numerator: 1_000_000,
        timestamp_denominator: 1,
        flags: 1,
        endian: Endian::Little,
    });
    let class = Name {
        namespace: 1,
        name: 2,
    };
    let mut events = vec![
        Event::StringHandle {
            handle: 1,
            value: "physx3".into(),
        },
        Event::StringHandle {
            handle: 2,
            value: "PxScene".into(),
        },
        Event::StringHandle {
            handle: 3,
            value: "frame".into(),
        },
        Event::CreateClass { name: class },
        Event::CreateInstance { class, id: 7 },
        Event::CreateInstance { class, id: 8 },
    ];
    for (frame, (id, timestamp)) in [
        (7, 100),
        (8, 150),
        (7, 200),
        (7, 300),
        (7, 400),
        (7, 500),
        (7, 600),
    ]
    .into_iter()
    .enumerate()
    {
        if reuse && frame == 4 {
            events.push(Event::DestroyInstance { id: 7 });
            events.push(Event::CreateInstance { class, id: 7 });
        }
        events.push(Event::EndSection {
            id,
            name: 3,
            timestamp,
        });
    }
    builder
        .push(EventGroup {
            stream_id: 1,
            timestamp: 0,
            offset: 0,
            events,
        })
        .unwrap();
    let mut recording = builder.finish().unwrap();
    recording
        .analysis
        .add_zone(KEY.zone, "Example zone".into())
        .unwrap();
    recording
        .analysis
        .add_event(KEY.zone, KEY.event, "Example event".into())
        .unwrap();
    recording
}

fn alternating_scene_fixture(
    frames: usize,
    reuse: Option<(usize, usize)>,
    class_name: &str,
) -> Recording {
    let mut builder = RecordingBuilder::new(Header {
        version: 1,
        timestamp_numerator: 1_000_000,
        timestamp_denominator: 1,
        flags: 1,
        endian: Endian::Little,
    });
    let class = Name {
        namespace: 1,
        name: 2,
    };
    let mut events = vec![
        Event::StringHandle {
            handle: 1,
            value: "physx3".into(),
        },
        Event::StringHandle {
            handle: 2,
            value: class_name.into(),
        },
        Event::StringHandle {
            handle: 3,
            value: "frame".into(),
        },
        Event::CreateClass { name: class },
        Event::CreateInstance { class, id: 7 },
        Event::CreateInstance { class, id: 8 },
    ];
    for frame in 0..frames {
        if reuse.is_some_and(|(first, last)| frame == first || frame == last) {
            events.push(Event::DestroyInstance { id: 8 });
            events.push(Event::CreateInstance { class, id: 8 });
        }
        events.push(Event::EndSection {
            id: if frame % 2 == 0 { 7 } else { 8 },
            name: 3,
            timestamp: (frame as u64 + 1) * 100,
        });
    }
    builder
        .push(EventGroup {
            stream_id: 1,
            timestamp: 0,
            offset: 0,
            events,
        })
        .unwrap();
    builder.finish().unwrap()
}

#[test]
fn reference_sampling_preserves_interleaved_scenes_and_short_reused_generations() {
    let cancel = AtomicBool::new(false);
    let recording = alternating_scene_fixture(5001, None, "PxScene");
    let scene = ReferenceScene {
        id: 8,
        generation: 0,
    };
    let sampled = profile_curves::sampling_frames(&recording, scene, &cancel).unwrap();
    assert_eq!(sampled.reference_frame_count, 2500);
    assert_eq!(sampled.stride, 1);
    assert_eq!(sampled.frames, (1..5001).step_by(2).collect::<Vec<_>>());
    let recording = alternating_scene_fixture(5001, Some((201, 205)), "PxScene");
    let short = profile_curves::sampling_frames(
        &recording,
        ReferenceScene {
            generation: 1,
            ..scene
        },
        &cancel,
    )
    .unwrap();
    assert_eq!(short.frames, [201, 203]);
    assert_eq!(short.reference_frame_count, 2);
    assert_eq!(short.stride, 1);
    for generation in 0..=2 {
        let sampled = profile_curves::sampling_frames(
            &recording,
            ReferenceScene {
                generation,
                ..scene
            },
            &cancel,
        )
        .unwrap();
        for &frame in &sampled.frames {
            assert_eq!(recording.frames[frame].scene_id, scene.id);
            assert_eq!(
                recording.state_at(frame).unwrap().objects[&scene.id].generation,
                generation
            );
        }
    }
}

#[test]
fn reference_sampling_preserves_single_and_budget_boundary_endpoints() {
    let scene = ReferenceScene {
        id: 8,
        generation: 0,
    };
    let cancel = AtomicBool::new(false);
    for reference_count in [1, 4999, 5000, 5001, 5002] {
        let recording = alternating_scene_fixture(reference_count * 2, None, "PxScene");
        let sampled = profile_curves::sampling_frames(&recording, scene, &cancel).unwrap();
        assert_eq!(sampled.reference_frame_count, reference_count);
        assert_eq!(sampled.frames.first(), Some(&1));
        assert_eq!(sampled.frames.last(), Some(&(reference_count * 2 - 1)));
        assert!(sampled.frames.len() <= profile_curves::MAX_SAMPLES);
        assert!(sampled.frames.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(sampled.frames.iter().all(|frame| frame % 2 == 1));
        if reference_count <= profile_curves::MAX_SAMPLES {
            assert_eq!(sampled.stride, 1);
            assert_eq!(sampled.frames.len(), reference_count);
        } else {
            assert_eq!(sampled.stride, 2);
            assert!(
                sampled.frames[..sampled.frames.len() - 1]
                    .windows(2)
                    .all(|pair| pair[1] - pair[0] == sampled.stride * 2)
            );
        }
    }
}

#[test]
fn reference_sampling_rejects_absent_generations_non_scene_objects_and_cancellation() {
    let cancel = AtomicBool::new(false);
    let recording = alternating_scene_fixture(2, None, "PxScene");
    for scene in [
        ReferenceScene {
            id: 999,
            generation: 0,
        },
        ReferenceScene {
            id: 8,
            generation: 999,
        },
    ] {
        assert!(
            profile_curves::sampling_frames(&recording, scene, &cancel)
                .unwrap_err()
                .to_string()
                .contains("no completed frames")
        );
    }
    assert!(
        profile_curves::sampling_frames(&recording, SCENE, &AtomicBool::new(true))
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
    for recording in [
        alternating_scene_fixture(0, None, "PxScene"),
        alternating_scene_fixture(2, None, "PxRigidStatic"),
    ] {
        assert!(
            profile_curves::sampling_frames(&recording, SCENE, &cancel)
                .unwrap_err()
                .to_string()
                .contains("no completed frames")
        );
    }
    let mut recording = alternating_scene_fixture(2, None, "PxScene");
    recording.frames[0].index = 42;
    assert!(
        profile_curves::sampling_frames(&recording, SCENE, &cancel)
            .unwrap_err()
            .to_string()
            .contains("inconsistent")
    );
    recording
        .frames
        .resize(2_000_001, recording.frames[1].clone());
    assert!(
        profile_curves::sampling_frames(&recording, SCENE, &cancel)
            .unwrap_err()
            .to_string()
            .contains("index budget")
    );
}

#[test]
fn reference_clock_validation_and_span_search_are_isolated_to_the_selected_generation() {
    let mut recording = fixture(true);
    recording.frames[4].timestamp = 50;
    recording.frames[5].timestamp = 150;
    recording.frames[6].timestamp = 250;
    let new_key = ExactKey { event: 3, ..KEY };
    edit_legacy_fixture(&mut recording, |analysis| {
        analysis.spans = vec![span(KEY, 210, 220), span(new_key, 70, 80)];
    });
    assert_eq!(sample(&recording, &[3])[0].milliseconds, Some(10.0));
    let scene = ReferenceScene {
        generation: 1,
        ..SCENE
    };
    let cancel = AtomicBool::new(false);
    let selected = profile_curves::sampling_frames(&recording, scene, &cancel).unwrap();
    assert_eq!(selected.frames, [4, 5, 6]);
    let points =
        profile_curves::sample(&recording, new_key, scene, &selected.frames, &cancel).unwrap();
    assert_eq!(points[0].status, Status::OtherGeneration);
    assert_eq!(points[1].milliseconds, Some(10.0));
    assert_eq!(points[2].status, Status::Missing);
    recording.frames[3].timestamp = 180;
    assert_eq!(
        profile_curves::sample(&recording, new_key, scene, &selected.frames, &cancel).unwrap(),
        points
    );
    assert!(
        sample(&recording, &[2, 3])
            .iter()
            .all(|point| point.status == Status::NonMonotonicClock)
    );
}

fn span(key: ExactKey, start: u64, end: u64) -> ProfileSpan {
    ProfileSpan {
        zone: key.zone,
        event: key.event,
        thread: key.thread,
        context: key.context,
        start,
        end,
        received_frame: usize::MAX,
    }
}

fn ingest(
    analysis: &mut pvd_core::analysis::SharedAnalysis,
    key: ExactKey,
    start: bool,
    timestamp: u64,
) {
    let mut bytes = vec![if start { 1 } else { 2 }, 15];
    bytes.extend(key.event.to_le_bytes());
    bytes.extend(key.thread.to_le_bytes());
    bytes.extend(key.context.to_le_bytes());
    bytes.extend([0, 0]);
    bytes.extend(timestamp.to_le_bytes());
    analysis
        .ingest(
            key.zone,
            "physx::PxProfileZone",
            &bytes,
            Endian::Little,
            999,
        )
        .unwrap();
}

fn sample(recording: &Recording, frames: &[usize]) -> Vec<profile_curves::Point> {
    profile_curves::sample(recording, KEY, SCENE, frames, &AtomicBool::new(false)).unwrap()
}

#[test]
fn inclusive_sum_keeps_exact_identity_missing_and_measured_zero_distinct() {
    let mut recording = fixture(false);
    edit_legacy_fixture(&mut recording, |analysis| {
        analysis.spans = vec![
            span(KEY, 20, 50),
            span(KEY, 120, 180),
            span(KEY, 130, 160),
            span(KEY, 185, 190),
            span(KEY, 350, 350),
            span(KEY, 400, 500),
            span(ExactKey { zone: 2, ..KEY }, 101, 199),
            span(ExactKey { event: 3, ..KEY }, 101, 199),
            span(ExactKey { thread: 43, ..KEY }, 101, 199),
            span(ExactKey { context: 10, ..KEY }, 101, 199),
        ];
    });
    let points = sample(&recording, &[0, 1, 2, 3, 4, 5, 6]);
    assert_eq!(points[0].status, Status::FirstFrame);
    assert_eq!(points[1].status, Status::OtherScene);
    assert_eq!(
        (points[2].milliseconds, points[2].occurrence_count),
        (Some(95.0), 3)
    );
    assert_eq!(points[3].status, Status::Missing);
    assert_eq!(
        (points[4].milliseconds, points[4].occurrence_count),
        (Some(0.0), 1)
    );
    assert_eq!(points[5].milliseconds, Some(100.0));
    assert_eq!(points[6].status, Status::Missing);
    assert!(
        points
            .iter()
            .filter(|point| point.status != Status::Recorded)
            .all(|point| point.milliseconds.is_none())
    );
    for frame in [6, 5, 4, 3, 2, 1, 0, 4, 0, 6, 2] {
        assert_eq!(sample(&recording, &[frame])[0], points[frame]);
    }
}

#[test]
fn crossing_spans_invalidate_every_intersected_window_including_sparse_samples() {
    let mut recording = fixture(false);
    edit_legacy_fixture(&mut recording, |analysis| {
        analysis.spans = vec![
            span(KEY, 110, 120),
            span(KEY, 180, 320),
            span(KEY, 550, 700),
        ];
    });
    let points = sample(&recording, &[0, 1, 2, 3, 4, 5, 6]);
    for frame in [2, 3, 4, 6] {
        assert_eq!(points[frame].status, Status::CrossBoundary);
        assert!(points[frame].milliseconds.is_none());
    }
    assert_eq!(points[2].occurrence_count, 1);
    assert_eq!(points[5].status, Status::Missing);
    assert_eq!(
        sample(&recording, &[2, 4, 6]),
        [points[2].clone(), points[4].clone(), points[6].clone()]
    );
}

#[test]
fn pending_start_only_invalidates_later_windows_but_unknown_start_invalidates_series() {
    let mut recording = fixture(false);
    edit_legacy_fixture(&mut recording, |analysis| {
        analysis.spans = vec![
            span(KEY, 110, 120),
            span(KEY, 310, 320),
            span(KEY, 510, 520),
        ];
    });
    ingest(&mut recording.analysis, KEY, true, 450);
    let points = sample(&recording, &[2, 4, 5, 6]);
    assert_eq!(points[0].milliseconds, Some(10.0));
    assert_eq!(points[1].milliseconds, Some(10.0));
    assert_eq!(points[2].status, Status::OpenSpan);
    assert_eq!(points[3].status, Status::OpenSpan);
    ingest(&mut recording.analysis, KEY, false, 550);
    ingest(&mut recording.analysis, KEY, false, 560);
    assert!(
        sample(&recording, &[2, 4, 5, 6])
            .iter()
            .all(|point| point.status == Status::UnmatchedStop && point.milliseconds.is_none())
    );
}

#[test]
fn first_appearance_uses_start_ticks_including_pending_and_ties_use_exact_key() {
    let mut recording = fixture(false);
    let nested = ExactKey { event: 3, ..KEY };
    let pending = ExactKey { event: 4, ..KEY };
    let same_tick = ExactKey { thread: 43, ..KEY };
    let unmatched = ExactKey { event: 5, ..KEY };
    edit_legacy_fixture(&mut recording, |analysis| {
        analysis.spans = vec![
            span(nested, 120, 140),
            span(KEY, 110, 180),
            span(same_tick, 110, 190),
        ];
    });
    ingest(&mut recording.analysis, pending, true, 90);
    ingest(&mut recording.analysis, unmatched, false, 200);
    let catalog = profile_curves::catalog(&recording, &AtomicBool::new(false)).unwrap();
    assert_eq!(
        catalog.iter().map(|entry| entry.key).collect::<Vec<_>>(),
        [pending, KEY, same_tick, nested, unmatched]
    );
    assert_eq!(catalog[0].first_start, Some(90));
    assert_eq!(catalog[0].open_spans, 1);
    assert_eq!(catalog[4].first_start, None);
    assert_eq!(catalog[4].unmatched_stops, 1);
    assert!(catalog.iter().all(|entry| !entry.metadata_ambiguous));
}

#[test]
fn redefined_metadata_cannot_relabel_historical_spans_and_identical_definitions_are_safe() {
    let mut recording = fixture(false);
    edit_legacy_fixture(&mut recording, |analysis| {
        analysis.spans.push(span(KEY, 110, 120));
    });
    recording
        .analysis
        .add_zone(KEY.zone, "Example zone".into())
        .unwrap();
    recording
        .analysis
        .add_event(KEY.zone, KEY.event, "Example event".into())
        .unwrap();
    assert_eq!(sample(&recording, &[2])[0].status, Status::Recorded);
    recording
        .analysis
        .add_event(KEY.zone, KEY.event, "Other event".into())
        .unwrap();
    recording
        .analysis
        .add_event(KEY.zone, KEY.event, "Example event".into())
        .unwrap();
    assert_eq!(
        sample(&recording, &[2])[0].status,
        Status::MetadataAmbiguous
    );
    assert!(
        profile_curves::catalog(&recording, &AtomicBool::new(false)).unwrap()[0].metadata_ambiguous
    );
    let mut analysis = Analysis::default();
    analysis.add_event(9, 1, "Before zone".into()).unwrap();
    analysis.add_zone(9, String::new()).unwrap();
    assert!(!analysis.profile_metadata_ambiguous(9, 1));
    analysis
        .add_zone(9, "Defined empty name changed".into())
        .unwrap();
    assert!(analysis.profile_metadata_ambiguous(9, 1));
    assert!(analysis.profile_metadata_ambiguous(9, 99));
}

#[test]
fn reference_scene_generation_requires_both_boundaries_and_clock_is_monotonic() {
    let mut recording = fixture(true);
    edit_legacy_fixture(&mut recording, |analysis| {
        analysis.spans = vec![
            span(KEY, 110, 120),
            span(KEY, 310, 320),
            span(KEY, 410, 420),
        ];
    });
    let old = sample(&recording, &[2, 4, 5]);
    assert_eq!(old[0].status, Status::Recorded);
    assert!(
        old[1..]
            .iter()
            .all(|point| point.status == Status::OtherGeneration)
    );
    let new = profile_curves::sample(
        &recording,
        KEY,
        ReferenceScene {
            generation: 1,
            ..SCENE
        },
        &[2, 4, 5],
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(new[0].status, Status::OtherGeneration);
    assert_eq!(new[1].status, Status::OtherGeneration);
    assert_eq!(new[2].milliseconds, Some(10.0));
    recording.frames[3].timestamp = 200;
    let points = sample(&recording, &[0, 1, 2, 3, 4]);
    assert_eq!(points[1].status, Status::OtherScene);
    assert!(
        points
            .iter()
            .filter(|point| point.frame != 1)
            .all(|point| point.status == Status::NonMonotonicClock)
    );
}

#[test]
fn sample_validation_cancellation_and_catalog_budget_are_explicit() {
    let mut recording = fixture(false);
    for indices in [vec![2, 2], vec![3, 2], vec![7], vec![0; 5001]] {
        assert!(
            profile_curves::sample(&recording, KEY, SCENE, &indices, &AtomicBool::new(false))
                .is_err()
        );
    }
    assert!(profile_curves::sample(&recording, KEY, SCENE, &[2], &AtomicBool::new(true)).is_err());
    assert!(profile_curves::catalog(&recording, &AtomicBool::new(true)).is_err());
    recording.header.timestamp_denominator = 0;
    assert!(profile_curves::sample(&recording, KEY, SCENE, &[2], &AtomicBool::new(false)).is_err());
    recording.header.timestamp_denominator = 1;
    edit_legacy_fixture(&mut recording, |analysis| {
        analysis.spans.push(span(KEY, 120, 110));
    });
    assert!(profile_curves::sample(&recording, KEY, SCENE, &[2], &AtomicBool::new(false)).is_err());
    edit_legacy_fixture(&mut recording, |analysis| {
        analysis.spans = (0..=profile_curves::MAX_SERIES)
            .map(|context| {
                span(
                    ExactKey {
                        context: context as u64,
                        ..KEY
                    },
                    110,
                    120,
                )
            })
            .collect();
    });
    assert!(
        profile_curves::catalog(&recording, &AtomicBool::new(false))
            .unwrap_err()
            .to_string()
            .contains("100000")
    );
}

fn capture(variable: &str, filename: &str) -> PathBuf {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../captures")
                .join(filename)
        })
}

#[test]
#[ignore = "requires real SDK 3.4 profile-metrics capture and independent QPC bounds"]
fn sdk_exact_event_sampling_preserves_independently_measured_phase_ticks() {
    let path = capture(
        "PVD_PROFILE_METRICS_CAPTURE",
        "physx-3.4-profile-metrics.pvdstream",
    );
    let recording = Recording::from_path(&path).unwrap();
    let truth: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path.with_extension("json")).unwrap()).unwrap();
    let scene = ReferenceScene {
        id: truth["scene"].as_u64().unwrap(),
        generation: 0,
    };
    let scale = recording.header.timestamp_numerator as f64
        / recording.header.timestamp_denominator as f64
        / 1e6;
    assert!((scale - 1000.0 / truth["counter_frequency"].as_u64().unwrap() as f64).abs() < 1e-12);
    let cancel = AtomicBool::new(false);
    let frames: Vec<_> = (0..recording.frame_count()).collect();
    let catalog = profile_curves::catalog(&recording, &cancel).unwrap();
    let mut checked = 0;
    for name in ["Basic.simulate", "Basic.collision", "Basic.rigidBodySolver"] {
        let keys: Vec<_> = catalog
            .iter()
            .filter(|entry| {
                entry.key.context == scene.id
                    && entry.key.thread == pvd_core::frame_metrics::CROSS_THREAD_ID
                    && legacy_fixture(&recording).zones[&entry.key.zone].name == "PhysXSDK"
                    && legacy_fixture(&recording).zones[&entry.key.zone].events[&entry.key.event]
                        == name
            })
            .collect();
        assert_eq!(keys.len(), 1);
        let key = keys[0].key;
        let points = profile_curves::sample(&recording, key, scene, &frames, &cancel).unwrap();
        assert_eq!(points[0].status, Status::FirstFrame);
        if keys[0].open_spans > 0 {
            assert_eq!(name, "Basic.rigidBodySolver");
            let starts: Vec<_> = legacy_fixture(&recording)
                .open_event_starts()
                .filter(|(candidate, _)| {
                    **candidate == (key.zone, key.event, key.thread, key.context)
                })
                .flat_map(|(_, starts)| starts.iter().copied())
                .collect();
            assert!(starts[0] < recording.frames[0].timestamp);
            println!(
                "pending key={key:?}, first_tick={}, last_tick={}, first_frame_end={}, start frame positions={:?}",
                starts[0],
                starts[starts.len() - 1],
                recording.frames[0].timestamp,
                starts
                    .iter()
                    .map(|start| recording
                        .frames
                        .partition_point(|frame| frame.timestamp < *start))
                    .collect::<Vec<_>>()
            );
            println!(
                "{name}: {} complete spans, {} pending starts; incomplete windows remain blank",
                keys[0].complete_spans, keys[0].open_spans
            );
        }
        for expected in truth["spans"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|span| span["event"].as_str() == Some(name))
        {
            let start = expected["start_lower"].as_u64().unwrap()
                ..=expected["start_upper"].as_u64().unwrap();
            let end =
                expected["end_lower"].as_u64().unwrap()..=expected["end_upper"].as_u64().unwrap();
            let found: Vec<_> = recording
                .analysis
                .profile_spans()
                .filter(|span| {
                    (span.zone, span.event, span.thread, span.context)
                        == (key.zone, key.event, key.thread, key.context)
                        && start.contains(&span.start)
                        && end.contains(&span.end)
                })
                .collect();
            assert_eq!(found.len(), 1);
            let frame = expected["frame"].as_u64().unwrap() as usize;
            if frame > 0 {
                assert_eq!(points[frame].occurrence_count, 1);
                if keys[0].open_spans == 0 {
                    assert_eq!(
                        points[frame].milliseconds,
                        Some((found[0].end - found[0].start) as f64 * scale)
                    );
                } else {
                    assert_eq!(points[frame].status, Status::OpenSpan);
                    assert!(points[frame].milliseconds.is_none());
                }
            }
            checked += 1;
        }
        for frame in [23, 0, 5, 1, 12, 22, 1] {
            assert_eq!(
                profile_curves::sample(&recording, key, scene, &[frame], &cancel).unwrap()[0],
                points[frame]
            );
        }
    }
    assert_eq!(checked, 72);
}

#[test]
#[ignore = "requires official SDK 3.4 custom profile-event capture and independent QPC callback truth"]
fn sdk_arbitrary_events_preserve_inclusive_timing_thread_context_order_and_missing_windows() {
    let path = capture(
        "PVD_PROFILE_EVENTS_CAPTURE",
        "physx-3.4-profile-events.pvdstream",
    );
    let mut recording = Recording::from_path(&path).unwrap();
    let truth: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path.with_extension("json")).unwrap()).unwrap();
    let scene = ReferenceScene {
        id: truth["scene"].as_u64().unwrap(),
        generation: 0,
    };
    let main_thread = truth["main_thread"].as_u64().unwrap() as u32;
    let worker_thread = truth["worker_thread"].as_u64().unwrap() as u32;
    let context_a = truth["context_a"].as_u64().unwrap();
    let context_b = truth["context_b"].as_u64().unwrap();
    assert_ne!(context_a, scene.id);
    assert_ne!(context_b, scene.id);
    assert_ne!(main_thread, worker_thread);
    let scale = recording.header.timestamp_numerator as f64
        / recording.header.timestamp_denominator as f64
        / 1e6;
    assert!((scale - 1000.0 / truth["counter_frequency"].as_u64().unwrap() as f64).abs() < 1e-12);
    let cancel = AtomicBool::new(false);
    let catalog = profile_curves::catalog(&recording, &cancel).unwrap();
    let event_name =
        |key: ExactKey| legacy_fixture(&recording).zones[&key.zone].events[&key.event].as_str();
    let custom: Vec<_> = catalog
        .iter()
        .filter(|entry| event_name(entry.key).starts_with("PvdStudio.Profile."))
        .collect();
    assert_eq!(custom.len(), 6);
    assert_eq!(event_name(custom[0].key), "PvdStudio.Profile.Sample");
    assert_eq!(custom[0].key.thread, main_thread);
    assert_eq!(custom[0].key.context, context_a);
    assert_eq!(event_name(custom[1].key), "PvdStudio.Profile.Second");
    let first_complete = recording
        .analysis
        .profile_spans()
        .find(|span| {
            event_name(ExactKey {
                zone: span.zone,
                event: span.event,
                thread: span.thread,
                context: span.context,
            })
            .starts_with("PvdStudio.Profile.")
        })
        .unwrap();
    assert_eq!(
        legacy_fixture(&recording).zones[&first_complete.zone].events[&first_complete.event],
        "PvdStudio.Profile.Second"
    );
    let frames: Vec<_> = (0..recording.frame_count()).collect();
    assert_eq!(frames.len(), 8);
    for step in truth["steps"].as_array().unwrap() {
        let frame = step["frame"].as_u64().unwrap() as usize;
        assert_eq!(recording.frames[frame].scene_id, scene.id);
        assert!(
            (step["lower"].as_u64().unwrap()..=step["upper"].as_u64().unwrap())
                .contains(&recording.frames[frame].timestamp)
        );
    }
    let mut checked = 0;
    for entry in &custom {
        let key = entry.key;
        let name = event_name(key);
        assert_eq!(
            legacy_fixture(&recording).zones[&key.zone].name,
            truth["zone_name"].as_str().unwrap()
        );
        assert!(!entry.metadata_ambiguous);
        assert_eq!(entry.unmatched_stops, 0);
        let expected: Vec<_> = truth["spans"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|span| {
                span["event"].as_str() == Some(name)
                    && span["thread"].as_u64() == Some(key.thread.into())
                    && span["context"].as_u64() == Some(key.context)
            })
            .collect();
        assert_eq!(entry.complete_spans, expected.len());
        for expected in &expected {
            let start = expected["start_lower"].as_u64().unwrap()
                ..=expected["start_upper"].as_u64().unwrap();
            let end =
                expected["end_lower"].as_u64().unwrap()..=expected["end_upper"].as_u64().unwrap();
            let matches = recording
                .analysis
                .profile_spans()
                .filter(|span| {
                    (span.zone, span.event, span.thread, span.context)
                        == (key.zone, key.event, key.thread, key.context)
                        && start.contains(&span.start)
                        && end.contains(&span.end)
                })
                .count();
            assert_eq!(matches, 1, "independent callback span {expected}");
            checked += 1;
        }
        let points = profile_curves::sample(&recording, key, scene, &frames, &cancel).unwrap();
        assert_eq!(points[0].status, Status::FirstFrame);
        for (frame, point) in points.iter().enumerate().skip(1) {
            if name == "PvdStudio.Profile.CrossFrame" && [3, 4].contains(&frame) {
                assert_eq!(point.status, Status::CrossBoundary);
                assert!(point.milliseconds.is_none());
            } else if name == "PvdStudio.Profile.Pending" && frame >= 6 {
                assert_eq!(point.status, Status::OpenSpan);
                assert!(point.milliseconds.is_none());
            } else {
                let same_frame: Vec<_> = expected
                    .iter()
                    .filter(|span| {
                        span["start_frame"].as_u64() == Some(frame as u64)
                            && span["end_frame"].as_u64() == Some(frame as u64)
                    })
                    .collect();
                assert_eq!(point.occurrence_count, same_frame.len());
                if same_frame.is_empty() {
                    assert_eq!(point.status, Status::Missing);
                    assert!(point.milliseconds.is_none());
                } else {
                    let lower: u64 = same_frame
                        .iter()
                        .map(|span| {
                            span["end_lower"]
                                .as_u64()
                                .unwrap()
                                .saturating_sub(span["start_upper"].as_u64().unwrap())
                        })
                        .sum();
                    let upper: u64 = same_frame
                        .iter()
                        .map(|span| {
                            span["end_upper"].as_u64().unwrap()
                                - span["start_lower"].as_u64().unwrap()
                        })
                        .sum();
                    assert_eq!(point.status, Status::Recorded);
                    assert!(
                        (lower as f64 * scale..=upper as f64 * scale)
                            .contains(&point.milliseconds.unwrap()),
                        "{name} {frame}: {point:?}"
                    );
                }
            }
        }
        for frame in [7, 6, 5, 4, 3, 2, 1, 0, 2, 5, 1, 7] {
            assert_eq!(
                profile_curves::sample(&recording, key, scene, &[frame], &cancel).unwrap()[0],
                points[frame]
            );
        }
    }
    assert_eq!(checked, 12);
    let pending = custom
        .iter()
        .find(|entry| event_name(entry.key) == "PvdStudio.Profile.Pending")
        .unwrap();
    assert_eq!(pending.open_spans, 1);
    let pending_truth = &truth["pending"][0];
    let starts: Vec<_> = legacy_fixture(&recording)
        .open_event_starts()
        .filter(|(key, _)| {
            **key
                == (
                    pending.key.zone,
                    pending.key.event,
                    pending.key.thread,
                    pending.key.context,
                )
        })
        .flat_map(|(_, starts)| starts.iter().copied())
        .collect();
    assert_eq!(starts.len(), 1);
    assert!(
        (pending_truth["lower"].as_u64().unwrap()..=pending_truth["upper"].as_u64().unwrap())
            .contains(&starts[0])
    );
    let primary = custom[0].key;
    let before = profile_curves::sample(&recording, primary, scene, &frames, &cancel).unwrap();
    edit_legacy_fixture(&mut recording, |analysis| {
        for span in &mut analysis.spans {
            span.received_frame = usize::MAX;
        }
    });
    assert_eq!(
        profile_curves::sample(&recording, primary, scene, &frames, &cancel).unwrap(),
        before
    );
    println!(
        "8 SDK scene frames, 12 complete spans, 1 pending start: independent QPC/thread/context/first-appearance and random/reverse sampling passed"
    );
}
