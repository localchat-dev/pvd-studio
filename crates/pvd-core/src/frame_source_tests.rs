use super::*;
use crate::{
    analysis::view::ProfileSource,
    debug_draw::Primitive,
    frame_sources::{FrameObject, FrameSection},
    live::{HistoryConfig, ReplayCursor},
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

#[test]
fn live_abort_token_can_be_attached_before_or_after_stream_initialization() {
    for before_header in [true, false] {
        let directory = TestDirectory::new();
        let mut live = LiveRecording::with_persistent_history(directory.config()).unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        if before_header {
            live.set_cancellation(cancelled.clone());
        }
        live.feed(&wire(&mixed_events())).unwrap();
        if !before_header {
            live.set_cancellation(cancelled.clone());
        }
        let old = live.snapshot().unwrap();
        let count = live.frame_count();
        cancelled.store(true, Ordering::Release);
        let failure = live.feed(&[0xff]).unwrap_err();
        assert_eq!(
            failure
                .downcast_ref::<std::io::Error>()
                .map(std::io::Error::kind),
            Some(std::io::ErrorKind::Interrupted),
            "{failure}"
        );
        assert_eq!(live.frame_count(), count);
        let retained = live.snapshot().unwrap();
        for frame in 0..count {
            assert_eq!(
                retained.state_at(frame).unwrap().objects,
                old.state_at(frame).unwrap().objects
            );
        }
    }
}

#[test]
fn live_abort_before_the_header_returns_typed_interruption() {
    let mut live = LiveRecording::default();
    live.set_cancellation(Arc::new(AtomicBool::new(true)));
    let failure = live.feed(&wire_header()).unwrap_err();
    assert_eq!(
        failure
            .downcast_ref::<std::io::Error>()
            .map(std::io::Error::kind),
        Some(std::io::ErrorKind::Interrupted),
        "{failure}"
    );
    assert_eq!(live.frame_count(), 0);
    assert!(live.builder.is_none());
}

pub(crate) struct TestDirectory(PathBuf);
impl TestDirectory {
    pub(crate) fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../output/frame-source-tests")
            .join(format!(
                "{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                DIRECTORY_ID.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    pub(crate) fn config(&self) -> HistoryConfig {
        HistoryConfig {
            directory: self.0.clone(),
            segment_frames: 2,
            min_free_disk_bytes: 0,
            disk_budget_bytes: 16 << 20,
            segment_target_bytes: 1 << 20,
        }
    }
    fn assert_empty(&self) {
        assert_eq!(fs::read_dir(&self.0).unwrap().count(), 0);
    }
}
impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}

fn header() -> Header {
    Header {
        version: 1,
        timestamp_numerator: 3,
        timestamp_denominator: 7,
        flags: 1,
        endian: Endian::Little,
    }
}
fn group(events: Vec<Event>) -> EventGroup {
    EventGroup {
        stream_id: 7,
        timestamp: 777,
        offset: 0,
        events,
    }
}
fn class(namespace: u32, name: u32) -> Name {
    Name { namespace, name }
}
fn key(section: FrameSection, source: u64) -> FrameSourceKey {
    FrameSourceKey::Captured { section, source }
}
fn end(name: u32, id: u64, timestamp: u64) -> Event {
    Event::EndSection {
        name,
        id,
        timestamp,
    }
}
fn set(value: u32) -> Event {
    Event::SetPropertyValue {
        id: 7,
        property: 31,
        datatype: class(0, 32),
        items: 1,
        data: Arc::from(value.to_le_bytes()),
    }
}
fn initial() -> Vec<Event> {
    let mut events: Vec<_> = [
        (20, "frame"),
        (21, "ApexFrame"),
        (30, "Actor"),
        (31, "value"),
        (32, "PvdU32"),
        (33, "physx3"),
        (34, "PxScene"),
        (35, "Apex"),
        (36, "scene"),
    ]
    .into_iter()
    .map(|(handle, value)| Event::StringHandle {
        handle,
        value: value.into(),
    })
    .collect();
    events.extend([
        Event::CreateClass { name: class(0, 30) },
        Event::CreateClass {
            name: class(33, 34),
        },
        Event::CreateClass {
            name: class(35, 36),
        },
        Event::CreateInstance {
            class: class(33, 34),
            id: 100,
        },
        Event::CreateInstance {
            class: class(33, 34),
            id: 200,
        },
        Event::CreateInstance {
            class: class(35, 36),
            id: 300,
        },
        Event::CreateInstance {
            class: class(0, 30),
            id: 7,
        },
        Event::CreateInstance {
            class: class(0, 30),
            id: 8,
        },
    ]);
    events
}
fn mixed_events() -> Vec<Event> {
    let mut events = initial();
    events.extend([
        set(10),
        end(21, 300, 9000),
        set(20),
        end(20, 100, 11),
        set(30),
        end(20, 200, 800),
        set(40),
        end(21, 300, 8000),
        set(50),
        end(20, 100, 9),
        Event::DestroyInstance { id: 100 },
        Event::CreateInstance {
            class: class(33, 34),
            id: 100,
        },
        set(60),
        end(20, 100, 15),
        set(70),
        end(21, 300, 7000),
        set(999),
    ]);
    events
}
fn make_builder() -> RecordingBuilder {
    let mut builder = RecordingBuilder::new(header());
    builder.push(group(initial())).unwrap();
    builder
}
fn live(directory: &TestDirectory, source: Option<FrameSourceKey>, paged: bool) -> LiveRecording {
    let mut live = if paged {
        LiveRecording::with_history(directory.config())
    } else {
        LiveRecording::with_legacy_history(directory.config())
    }
    .unwrap();
    live.set_frame_source(source);
    live.feed(&wire_header()).unwrap();
    live.builder
        .as_mut()
        .unwrap()
        .push(group(initial()))
        .unwrap();
    live
}
fn push(live: &mut LiveRecording, events: Vec<Event>) {
    live.builder.as_mut().unwrap().push(group(events)).unwrap();
}
fn action(live: &mut LiveRecording, action: Action) {
    live.builder.as_mut().unwrap().action(action).unwrap();
}
fn camera(x: f32) -> SourceCamera {
    SourceCamera {
        position: [x, 2.0, 3.0],
        up: [0.0, 1.0, 0.0],
        target: [0.0; 3],
    }
}
fn draw(x: f32) -> Action {
    Action::Debug(
        7,
        vec![Primitive::Point {
            instance: 7,
            position: [x, 2.0, 3.0],
            color: 42,
        }]
        .into(),
    )
}
fn report(code: u32) -> Action {
    Action::Error(ErrorEvent {
        code,
        message: format!("error-{code}"),
        file: "fixture.cpp".into(),
        line: code,
        timestamp: 777,
    })
}
fn assert_value(state: &SceneState, value: u64) {
    assert_eq!(state.objects[&7].properties["value"], Value::U64(value));
}

#[test]
fn sdk_section_sources_preserve_global_order_raw_clocks_and_object_lifetimes() {
    let mut builder = RecordingBuilder::new(header());
    builder.push(group(mixed_events())).unwrap();
    let recording = builder.finish().unwrap();
    assert_eq!(
        recording
            .frames
            .iter()
            .map(|f| (f.index, f.scene_id, f.timestamp))
            .collect::<Vec<_>>(),
        [
            (0, 300, 9000),
            (1, 100, 11),
            (2, 200, 800),
            (3, 300, 8000),
            (4, 100, 9),
            (5, 100, 15),
            (6, 300, 7000)
        ]
    );
    recording
        .frame_sources
        .validate_frames(&recording.frames)
        .unwrap();
    let physx = recording
        .frame_sources
        .source(key(FrameSection::PhysX, 100))
        .unwrap();
    assert_eq!(
        physx
            .frames()
            .map(|f| (f.index, f.ordinal, f.scene.unwrap().generation))
            .collect::<Vec<_>>(),
        [(1, 0, 0), (4, 1, 0), (5, 2, 1)]
    );
    assert_eq!(
        physx.last().unwrap().source_object,
        Some(FrameObject {
            id: 100,
            generation: 1
        })
    );
    let apex = recording
        .frame_sources
        .source(key(FrameSection::Apex, 300))
        .unwrap();
    assert_eq!(
        apex.frames()
            .map(|f| (f.index, f.ordinal))
            .collect::<Vec<_>>(),
        [(0, 0), (3, 1), (6, 2)]
    );
    assert!(apex.frames().all(|frame| frame.scene.is_none()));
    assert!(apex.frames().all(|frame| frame.source_object
        == Some(FrameObject {
            id: 300,
            generation: 0
        })));
    assert_eq!(
        recording.frame_sources.default_source(),
        Some(key(FrameSection::PhysX, 100))
    );
    for index in [0, 1, 2, 3, 4, 5, 6, 5, 1, 6, 0] {
        assert_value(
            &recording.state_at(index).unwrap(),
            [10, 20, 30, 40, 50, 60, 70][index],
        );
    }
}

#[test]
fn converted_native_source_uses_legacy_axis_without_raw_source_claims() {
    let mut builder = RecordingBuilder::new(header());
    builder.recording.source = RecordingSource::Pvd3Database;
    builder.push(group(mixed_events())).unwrap();
    let recording = builder.finish().unwrap();
    assert_eq!(recording.frame_sources.sources().count(), 1);
    let legacy = recording
        .frame_sources
        .source(FrameSourceKey::Legacy)
        .unwrap();
    assert_eq!(
        legacy
            .frames()
            .map(|frame| (frame.index, frame.ordinal))
            .collect::<Vec<_>>(),
        (0..4).map(|index| (index, index)).collect::<Vec<_>>()
    );
    assert!(
        legacy
            .frames()
            .all(|frame| frame.source_object.is_none() && frame.scene.is_none())
    );
}

#[test]
fn selected_live_endpoint_excludes_later_sources_and_accumulates_lifetimes_and_analysis() {
    for paged in [false, true] {
        let directory = TestDirectory::new();
        let source = key(FrameSection::PhysX, 100);
        let mut live = live(&directory, Some(source), paged);
        assert_eq!(live.selected_frame_source(), Some(source));
        action(&mut live, Action::Set(7, "target".into(), Value::Ref(8)));
        action(&mut live, Action::Camera("main".into(), camera(1.0)));
        action(&mut live, draw(1.0));
        action(&mut live, report(7));
        push(
            &mut live,
            vec![
                Event::AddProfileZone {
                    id: 41,
                    name: "chosen".into(),
                },
                set(10),
                end(20, 100, 10),
            ],
        );
        action(&mut live, Action::RemoveCamera("main".into()));
        action(&mut live, Action::Camera("other".into(), camera(2.0)));
        action(&mut live, draw(2.0));
        action(&mut live, report(9));
        push(
            &mut live,
            vec![
                Event::DestroyInstance { id: 8 },
                Event::CreateInstance {
                    class: class(0, 30),
                    id: 8,
                },
                Event::AddProfileZone {
                    id: 42,
                    name: "later".into(),
                },
                set(20),
                end(20, 200, 800),
                set(999),
            ],
        );
        let published = live.snapshot().unwrap();
        assert_eq!(published.analysis.profile_zone_name(41), Some("chosen"));
        assert_eq!(published.analysis.profile_zone_name(42), None);
        assert_eq!(published.frames.len(), 2);
        let (frame, update) = live.take_update().unwrap();
        assert_eq!(frame, 0);
        assert!(update.replace_all);
        let mut displayed = SceneState::default();
        update.apply(&mut displayed);
        assert_value(&displayed, 10);
        assert_eq!(
            displayed
                .resolve_reference(7, "target", 8)
                .unwrap()
                .generation,
            0
        );
        assert_eq!(displayed.cameras["main"], camera(1.0));
        assert!(!displayed.cameras.contains_key("other"));
        assert_eq!(
            displayed
                .errors
                .iter()
                .map(|error| error.code)
                .collect::<Vec<_>>(),
            [7]
        );
        assert!(
            matches!(&displayed.debug_draw[&7][0], Primitive::Point { position, .. } if *position == [1.0, 2.0, 3.0])
        );
        assert!(live.take_update().is_none());
        action(&mut live, draw(3.0));
        push(&mut live, vec![end(20, 100, 9)]);
        let (frame, update) = live.take_update().unwrap();
        assert_eq!(frame, 2);
        assert!(!update.replace_all);
        update.apply(&mut displayed);
        assert_value(&displayed, 999);
        assert_eq!(displayed.objects[&8].generation, 1);
        assert!(displayed.resolve_reference(7, "target", 8).is_none());
        assert!(!displayed.cameras.contains_key("main"));
        assert_eq!(displayed.cameras["other"], camera(2.0));
        assert_eq!(
            displayed
                .errors
                .iter()
                .map(|error| error.code)
                .collect::<Vec<_>>(),
            [7, 9]
        );
        assert!(
            matches!(&displayed.debug_draw[&7][0], Primitive::Point { position, .. } if *position == [2.0, 2.0, 3.0])
        );
        assert_eq!(displayed.debug_draw[&7].len(), 2);
        assert!(
            matches!(&displayed.debug_draw[&7][1], Primitive::Point { position, .. } if *position == [3.0, 2.0, 3.0])
        );
        assert_eq!(
            live.snapshot().unwrap().analysis.profile_zone_name(42),
            Some("later")
        );
        assert_eq!(
            live.display_frame_source()
                .map(|(key, frame)| (key, frame.index, frame.ordinal)),
            Some((source, 2, 1))
        );
        action(&mut live, Action::Set(7, "target".into(), Value::Ref(8)));
        push(&mut live, vec![set(1000), end(20, 100, 8)]);
        let (_, update) = live.take_update().unwrap();
        assert!(!update.replace_all);
        update.apply(&mut displayed);
        assert_value(&displayed, 1000);
        assert_eq!(
            displayed
                .resolve_reference(7, "target", 8)
                .unwrap()
                .generation,
            1
        );
        assert!(displayed.debug_draw.is_empty());
        drop(published);
        drop(live);
        directory.assert_empty();
    }
}

#[test]
fn source_switch_and_refresh_wait_for_matching_end_and_replace_consumer_state() {
    let directory = TestDirectory::new();
    let first = key(FrameSection::PhysX, 100);
    let second = key(FrameSection::Apex, 300);
    let mut live = live(&directory, Some(first), true);
    push(
        &mut live,
        vec![
            Event::AddProfileZone {
                id: 41,
                name: "first".into(),
            },
            set(10),
            end(20, 100, 1),
        ],
    );
    let mut displayed = SceneState::default();
    live.take_update().unwrap().1.apply(&mut displayed);
    live.set_frame_source(Some(second));
    assert!(live.display_frame_source().is_none());
    push(
        &mut live,
        vec![
            Event::AddProfileZone {
                id: 42,
                name: "unselected".into(),
            },
            set(20),
            end(20, 100, 2),
        ],
    );
    assert!(live.take_update().is_none());
    let metadata = live.snapshot().unwrap();
    assert_eq!(metadata.analysis.profile_zone_name(41), Some("first"));
    assert_eq!(metadata.analysis.profile_zone_name(42), None);
    drop(metadata);
    push(&mut live, vec![set(30), end(21, 300, 9000), set(999)]);
    let (frame, update) = live.take_update().unwrap();
    assert_eq!(frame, 2);
    assert!(update.replace_all);
    displayed.objects.insert(
        999,
        Object {
            id: 999,
            generation: 0,
            type_name: "stale".into(),
            properties: BTreeMap::new(),
        },
    );
    update.apply(&mut displayed);
    assert_value(&displayed, 30);
    assert!(!displayed.objects.contains_key(&999));
    live.set_frame_source(Some(second));
    live.refresh_frame_source();
    assert!(live.take_update().is_none());
    push(&mut live, vec![end(20, 100, 3)]);
    assert!(live.take_update().is_none());
    push(&mut live, vec![set(40), end(21, 300, 8000)]);
    let (frame, update) = live.take_update().unwrap();
    assert_eq!(frame, 4);
    assert!(update.replace_all);
    update.apply(&mut displayed);
    assert_value(&displayed, 40);
}

#[test]
fn automatic_live_source_promotes_first_physx_once_and_remains_stable() {
    let directory = TestDirectory::new();
    let mut live = live(&directory, None, false);
    push(&mut live, vec![set(10), end(21, 300, 9000)]);
    assert_eq!(
        live.selected_frame_source(),
        Some(key(FrameSection::Apex, 300))
    );
    assert_eq!(live.take_update().unwrap().0, 0);
    push(&mut live, vec![set(20), end(20, 200, 20)]);
    assert_eq!(
        live.selected_frame_source(),
        Some(key(FrameSection::PhysX, 200))
    );
    assert!(live.take_update().unwrap().1.replace_all);
    push(
        &mut live,
        vec![set(30), end(20, 100, 10), set(40), end(21, 300, 8000)],
    );
    assert_eq!(
        live.selected_frame_source(),
        Some(key(FrameSection::PhysX, 200))
    );
    assert!(live.take_update().is_none());
    live.set_frame_source(Some(key(FrameSection::PhysX, 100)));
    push(&mut live, vec![set(50), end(20, 100, 9)]);
    assert_eq!(live.take_update().unwrap().0, 4);
    live.set_frame_source(None);
    assert_eq!(
        live.selected_frame_source(),
        Some(key(FrameSection::PhysX, 200))
    );
    push(&mut live, vec![set(60), end(20, 200, 19)]);
    assert!(live.take_update().unwrap().1.replace_all);
}

#[test]
fn mixed_sources_disk_cursor_restores_each_true_endpoint_in_both_directions() {
    let directory = TestDirectory::new();
    let mut live = LiveRecording::with_history(directory.config()).unwrap();
    let bytes = wire(&mixed_events());
    for part in bytes.chunks(17) {
        live.feed(part).unwrap();
    }
    let recording = live.finish().unwrap();
    recording
        .frame_sources
        .validate_frames(&recording.frames)
        .unwrap();
    let mut cursor = ReplayCursor::default();
    let mut displayed = SceneState::default();
    for frame in [0, 1, 2, 3, 4, 5, 6, 5, 4, 3, 2, 1, 0, 6, 2, 5] {
        cursor
            .seek(&recording, frame, || false)
            .unwrap()
            .update
            .apply(&mut displayed);
        assert_value(&displayed, [10, 20, 30, 40, 50, 60, 70][frame]);
        assert_eq!(displayed.objects[&100].generation, u32::from(frame >= 5));
    }
    drop(cursor);
    drop(recording);
    directory.assert_empty();
}

#[test]
fn source_directory_growth_shares_the_configured_core_memory_budget() {
    let mut builder = make_builder();
    let before = builder.memory_usage().estimated_history_bytes;
    builder.memory_limit = before + 128;
    let failure = builder
        .push(group(vec![end(20, 100, 1)]))
        .unwrap_err()
        .to_string();
    assert!(failure.contains("history memory budget"), "{failure}");
    assert!(builder.recording.frames.is_empty());
    assert_eq!(builder.recording.frame_sources.retained_frames(), 0);
    let mut builder = make_builder();
    builder.push(group(vec![end(20, 100, 1)])).unwrap();
    assert_eq!(
        builder.memory_usage().estimated_history_bytes,
        builder.retained_bytes
            + builder.recording.frame_sources.index_bytes()
            + builder.draw_history_bytes()
    );
}

#[test]
fn apex_sdk_clipping_preserves_source_first_frames_and_excludes_unended_tail() {
    for events in [mixed_events(), {
        let mut events = initial();
        events.extend([
            set(10),
            end(20, 100, 10),
            set(20),
            end(21, 300, 9000),
            set(999),
        ]);
        events
    }] {
        let bytes = wire(&events);
        let source = Recording::from_bytes(&bytes).unwrap();
        for timeline in source.frame_sources.sources() {
            let first = timeline.first().unwrap();
            let output = clip::trim_scene_frames(&bytes, first.index, first.index).unwrap();
            let mut streamed = Vec::new();
            let report = clip::trim_scene_frames_streaming(
                &mut std::io::Cursor::new(&bytes),
                &mut streamed,
                first.index,
                first.index,
                |_| true,
            )
            .unwrap();
            assert_eq!(report.output_frames, 1);
            assert_eq!(streamed, output);
            let trimmed = Recording::from_bytes(&output).unwrap();
            assert_eq!(trimmed.frame_count(), 1);
            assert_eq!(trimmed.frame_sources.sources().count(), 1);
            let (key, frame) = trimmed.frame_sources.frame(0).unwrap();
            assert_eq!(key, timeline.source());
            assert_eq!(frame.ordinal, 0);
            assert_eq!(trimmed.seconds_at(0), Some(0.0));
            assert_eq!(
                trimmed.frame_info(0).unwrap().timestamp,
                source.frame_info(first.index).unwrap().timestamp
            );
            assert_eq!(
                trimmed.state_at(0).unwrap(),
                source.state_at(first.index).unwrap()
            );
        }
        let last = source.frame_count() - 1;
        let output = clip::trim_scene_frames(&bytes, 0, last).unwrap();
        let trimmed = Recording::from_bytes(&output).unwrap();
        assert_eq!(trimmed.frame_count(), source.frame_count());
        assert_eq!(
            trimmed.state_at(last).unwrap(),
            source.state_at(last).unwrap()
        );
        assert_ne!(
            trimmed.state_at(last).unwrap().objects[&7].properties["value"],
            Value::U64(999)
        );
        let mut decoder = Decoder::default();
        let groups = decoder.feed(&output).unwrap();
        decoder.finish().unwrap();
        assert!(matches!(
            groups.last().unwrap().events.last(),
            Some(Event::EndSection { .. })
        ));
    }
}

#[test]
fn elapsed_seconds_keep_large_source_clock_offsets_independent() {
    let mut builder = make_builder();
    builder
        .push(group(vec![
            end(21, 100, 9_000_000_000_000_000_000),
            end(20, 100, 11),
            end(20, 200, 4_000_000_000_000_000_000),
            end(21, 100, 9_000_000_007_000_000_000),
            end(20, 100, 14_000_000_011),
            end(20, 200, 4_000_000_021_000_000_000),
            end(20, 100, 10),
            end(21, 100, 8_999_999_999_999_999_999),
        ]))
        .unwrap();
    let mut recording = builder.finish().unwrap();
    for (index, expected) in [
        Some(0.0),
        Some(0.0),
        Some(0.0),
        Some(3.0),
        Some(6.0),
        Some(9.0),
        None,
        None,
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(recording.seconds_at(index), expected, "frame {index}");
    }
    assert_eq!(recording.seconds_at(8), None);
    recording.frame_sources = FrameDirectory::default();
    assert_eq!(recording.seconds_at(0), None);
}

#[test]
fn legacy_elapsed_seconds_use_the_first_visible_recording_frame() {
    let mut builder = make_builder();
    builder
        .push(group(vec![
            end(20, 100, 4_000_000_000_000_000_000),
            end(21, 300, 4_000_000_007_000_000_000),
            end(20, 100, 4_000_000_014_000_000_000),
        ]))
        .unwrap();
    let mut recording = builder.finish().unwrap();
    recording.frame_sources =
        FrameDirectory::from_legacy(&recording.frames, Default::default()).unwrap();
    assert_eq!(recording.seconds_at(0), Some(0.0));
    assert_eq!(recording.seconds_at(1), Some(3.0));
    assert_eq!(recording.seconds_at(2), Some(6.0));
}

#[test]
fn physx_metrics_and_reference_windows_do_not_mix_apex_with_the_same_raw_source() {
    use crate::{
        frame_metrics::{self, Metric, Status},
        profile_curves::{self, ReferenceScene},
    };
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let mut events = initial();
    events.extend([
        set(10),
        end(20, 100, 100),
        set(20),
        end(21, 100, 9000),
        set(30),
        end(20, 100, 200),
    ]);
    let bytes = wire(&events);
    let recording = Recording::from_bytes(&bytes).unwrap();
    assert_eq!(recording.frame_sources.sources().count(), 2);
    assert!(frame_metrics::supports_stages(&recording, 100, 0));
    assert!(frame_metrics::supports_stages(&recording, 100, 1));
    let absent =
        frame_metrics::sample(&recording, 100, 1, Metric::Simulate, &[0, 1, 2], &cancelled)
            .unwrap();
    assert_eq!(
        absent.iter().map(|point| point.status).collect::<Vec<_>>(),
        [
            Status::OtherGeneration,
            Status::OtherScene,
            Status::OtherGeneration
        ]
    );
    assert!(!frame_metrics::supports_stages(&recording, 0, 0));
    let points = frame_metrics::sample(
        &recording,
        100,
        0,
        Metric::FrameInterval,
        &[0, 1, 2],
        &cancelled,
    )
    .unwrap();
    assert_eq!(
        points.iter().map(|point| point.status).collect::<Vec<_>>(),
        [Status::FirstFrame, Status::OtherScene, Status::Recorded]
    );
    assert!((points[2].milliseconds.unwrap() - 100.0 * 3.0 / 7.0 / 1e6).abs() < 1e-12);
    let scene = ReferenceScene {
        id: 100,
        generation: 0,
    };
    assert_eq!(
        profile_curves::sampling_frames(&recording, scene, &cancelled)
            .unwrap()
            .frames,
        [0, 2]
    );
    let report = frame_metrics::sample_streaming(
        &mut std::io::Cursor::new(&bytes),
        &recording,
        scene,
        Metric::Simulate,
        &[0, 1, 2],
        &cancelled,
        |_| true,
    )
    .unwrap();
    assert_eq!(report.identity.frames, 3);
    assert_eq!(
        report
            .points
            .iter()
            .map(|point| point.status)
            .collect::<Vec<_>>(),
        [Status::Missing, Status::OtherScene, Status::Missing]
    );
    let mut only_apex = initial();
    only_apex.extend([end(21, 100, 9000), end(21, 0, 8000)]);
    let recording = Recording::from_bytes(&wire(&only_apex)).unwrap();
    assert!(!frame_metrics::supports_stages(&recording, 100, 0));
    assert!(!frame_metrics::supports_stages(&recording, 0, 0));
    assert!(profile_curves::sampling_frames(&recording, scene, &cancelled).is_err());
}

fn wire_header() -> Vec<u8> {
    let mut bytes = pvd_protocol::STREAM_MAGIC.to_le_bytes().to_vec();
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(3u64.to_le_bytes());
    bytes.extend(7u64.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    bytes
}
fn wire(events: &[Event]) -> Vec<u8> {
    let mut payload = Vec::new();
    for event in events {
        match event {
            Event::StringHandle { handle, value } => {
                payload.push(1);
                payload.extend((value.len() as u32 + 1).to_le_bytes());
                payload.extend(value.bytes());
                payload.push(0);
                payload.extend(handle.to_le_bytes());
            }
            Event::CreateClass { name } => {
                payload.push(2);
                payload.extend(name.namespace.to_le_bytes());
                payload.extend(name.name.to_le_bytes());
            }
            Event::CreateInstance { class, id } => {
                payload.push(6);
                payload.extend(class.namespace.to_le_bytes());
                payload.extend(class.name.to_le_bytes());
                payload.extend(id.to_le_bytes());
            }
            Event::SetPropertyValue {
                id,
                property,
                datatype,
                items,
                data,
            } => {
                payload.push(7);
                payload.extend(id.to_le_bytes());
                payload.extend(property.to_le_bytes());
                payload.extend(datatype.namespace.to_le_bytes());
                payload.extend(datatype.name.to_le_bytes());
                payload.extend(items.to_le_bytes());
                payload.extend((data.len() as u32).to_le_bytes());
                payload.extend(data.iter());
            }
            Event::DestroyInstance { id } => {
                payload.push(15);
                payload.extend(id.to_le_bytes());
            }
            Event::EndSection {
                id,
                name,
                timestamp,
            } => {
                payload.push(19);
                payload.extend(id.to_le_bytes());
                payload.extend(name.to_le_bytes());
                payload.extend(timestamp.to_le_bytes());
            }
            _ => panic!("unsupported fixture event"),
        }
    }
    let mut bytes = wire_header();
    bytes.extend((payload.len() as u32).to_le_bytes());
    bytes.extend((events.len() as u32).to_le_bytes());
    bytes.extend(7u64.to_le_bytes());
    bytes.extend(777u64.to_le_bytes());
    bytes.extend(payload);
    bytes
}
