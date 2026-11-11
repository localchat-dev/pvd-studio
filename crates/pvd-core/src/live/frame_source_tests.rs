use super::*;
use crate::{RecordingBuilder, frame_source_tests::TestDirectory};
use pvd_protocol::{Endian, Header};

fn key(source: u64) -> FrameSourceKey {
    FrameSourceKey::Captured {
        section: FrameSection::PhysX,
        source,
    }
}
fn binding(source: u64) -> FrameBinding {
    FrameBinding {
        source: key(source),
        source_object: None,
        scene: None,
    }
}
fn streaming(directory: &TestDirectory) -> (Streaming, SceneState, FrameDirectory) {
    let history = HistoryWriter::new(directory.config()).unwrap();
    let mut live = Streaming::new(history);
    live.set_frame_source(Some(key(100)));
    let mut state = SceneState::default();
    live.action(
        Action::Create(Object {
            id: 7,
            generation: 0,
            type_name: "Actor".into(),
            properties: BTreeMap::new(),
        }),
        &mut state,
        1 << 20,
    )
    .unwrap();
    (live, state, Streaming::frame_directory())
}
fn complete(
    live: &mut Streaming,
    state: &mut SceneState,
    directory: &mut FrameDirectory,
    source: u64,
    value: u64,
) {
    live.action(
        Action::Set(7, "value".into(), Value::U64(value)),
        state,
        1 << 20,
    )
    .unwrap();
    let info = FrameInfo {
        index: live.frames_seen,
        timestamp: 9000 - value,
        scene_id: source,
    };
    directory
        .index_bytes_after_observe(info.index, binding(source), true)
        .unwrap();
    live.complete(
        info,
        binding(source),
        state,
        &SharedAnalysis::default(),
        directory,
    )
    .unwrap();
}
fn recording(live: &Streaming, directory: &FrameDirectory) -> crate::Recording {
    let mut recording = RecordingBuilder::new(Header {
        version: 1,
        timestamp_numerator: 1,
        timestamp_denominator: 1,
        flags: 1,
        endian: Endian::Little,
    })
    .recording;
    let disk = live.snapshot();
    recording.frames = disk.frames();
    recording.disk = Some(disk);
    recording.frame_sources = directory.clone();
    recording
}

#[test]
fn failed_disk_boundaries_keep_live_ordinals_without_fabricating_seekable_frames() {
    let directory = TestDirectory::new();
    let (mut live, mut state, mut sources) = streaming(&directory);
    for index in 0..5 {
        live.history.retry_after = if index % 2 == 1 {
            Instant::now() + Duration::from_secs(60)
        } else {
            Instant::now()
        };
        complete(&mut live, &mut state, &mut sources, 100, 10 + index as u64);
        assert_eq!(
            live.display_source_frame.map(|(_, frame)| frame.ordinal),
            Some(index)
        );
        assert_eq!(live.frame.as_ref().unwrap().index, index);
        if index % 2 == 1 {
            assert!(sources.frame(index).is_none());
        }
    }
    let timeline = sources.source(key(100)).unwrap();
    assert_eq!(timeline.observed_frames(), 5);
    assert_eq!(
        timeline
            .frames()
            .map(|frame| (frame.index, frame.ordinal))
            .collect::<Vec<_>>(),
        [(0, 0), (2, 2), (4, 4)]
    );
    assert!(timeline.at_ordinal(1).is_none());
    let recording = recording(&live, &sources);
    sources.validate_frames(&recording.frames).unwrap();
    for frame in [0, 2, 4] {
        assert_eq!(
            recording.state_at(frame).unwrap().objects[&7].properties["value"],
            Value::U64(10 + frame as u64)
        );
    }
    assert!(recording.state_at(1).is_err());
    let mut displayed = SceneState::default();
    live.pending.apply(&mut displayed);
    assert_eq!(displayed.objects[&7].properties["value"], Value::U64(14));
}

#[test]
fn rolling_source_directory_tracks_physical_eviction_and_keeps_source_ordinals() {
    let directory = TestDirectory::new();
    let (mut live, mut state, mut sources) = streaming(&directory);
    for frame in 0..6 {
        complete(
            &mut live,
            &mut state,
            &mut sources,
            if frame % 2 == 0 { 100 } else { 200 },
            frame,
        );
        live.history.close_segment();
    }
    for _ in 0..5 {
        assert!(live.history.evict_one().unwrap());
    }
    live.prune_frame_sources(&mut sources);
    assert_eq!(sources.retained_frames(), 1);
    assert!(sources.source(key(100)).unwrap().is_empty());
    assert_eq!(sources.source(key(100)).unwrap().observed_frames(), 3);
    assert_eq!(sources.default_source(), Some(key(100)));
    assert_eq!(live.selected_source, Some(key(100)));
    assert_eq!(
        live.display_source_frame
            .map(|(_, frame)| (frame.index, frame.ordinal)),
        Some((4, 2))
    );
    assert_eq!(
        sources
            .source(key(200))
            .unwrap()
            .frames()
            .map(|frame| (frame.index, frame.ordinal))
            .collect::<Vec<_>>(),
        [(5, 2)]
    );
    complete(&mut live, &mut state, &mut sources, 100, 6);
    assert_eq!(
        sources
            .source(key(100))
            .unwrap()
            .frames()
            .map(|frame| (frame.index, frame.ordinal))
            .collect::<Vec<_>>(),
        [(6, 3)]
    );
    let recording = recording(&live, &sources);
    sources.validate_frames(&recording.frames).unwrap();
    assert_eq!(
        recording
            .frames
            .iter()
            .map(|frame| frame.index)
            .collect::<Vec<_>>(),
        [5, 6]
    );
    assert!(recording.state_at(4).is_err());
    assert_eq!(
        recording.state_at(6).unwrap().objects[&7].properties["value"],
        Value::U64(6)
    );
}

#[test]
fn display_dirty_bookkeeping_yields_to_state_budget_without_losing_changes() {
    let directory = TestDirectory::new();
    let (mut live, mut state, mut sources) = streaming(&directory);
    complete(&mut live, &mut state, &mut sources, 100, 10);
    let mut displayed = SceneState::default();
    std::mem::take(&mut live.pending).apply(&mut displayed);
    let limit = live.state_bytes + 32;
    live.action(
        Action::Set(7, "value".into(), Value::U64(11)),
        &mut state,
        limit,
    )
    .unwrap();
    assert!(live.display_requires_full);
    assert_eq!(live.display_dirty_bytes, 0);
    assert!(live.memory_bytes() <= limit);
    live.complete(
        FrameInfo {
            index: 1,
            timestamp: 8000,
            scene_id: 200,
        },
        binding(200),
        &state,
        &SharedAnalysis::default(),
        &mut sources,
    )
    .unwrap();
    assert!(live.pending.is_empty());
    live.complete(
        FrameInfo {
            index: 2,
            timestamp: 20,
            scene_id: 100,
        },
        binding(100),
        &state,
        &SharedAnalysis::default(),
        &mut sources,
    )
    .unwrap();
    assert!(live.pending.replace_all);
    live.pending.apply(&mut displayed);
    assert_eq!(displayed.objects[&7].properties["value"], Value::U64(11));
}
