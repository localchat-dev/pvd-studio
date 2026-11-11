use super::*;
use crate::{
    debug_draw::Primitive,
    frame_source_tests::TestDirectory,
    frame_sources::FrameSection,
    live::{HistoryWriter, ReplayCursor},
};

fn group(events: Vec<Event>) -> EventGroup {
    EventGroup {
        stream_id: 1,
        timestamp: 0,
        offset: 0,
        events,
    }
}
fn endpoint(name: u32, source: u64, tick: u64) -> Event {
    Event::EndSection {
        id: source,
        name,
        timestamp: tick,
    }
}
fn builder(mode: u8, directory: &TestDirectory) -> RecordingBuilder {
    let mut builder = RecordingBuilder::new(Header {
        version: 1,
        timestamp_numerator: 1,
        timestamp_denominator: 1,
        flags: 1,
        endian: Endian::Little,
    });
    if mode == 3 {
        builder.validation = Some(clip::ValidationState::default());
    } else if mode != 0 {
        let history = if mode == 1 {
            HistoryWriter::new(directory.config())
        } else {
            HistoryWriter::new_paged(directory.config())
        }
        .unwrap();
        if let Some(analysis) = history.initial_analysis() {
            builder.recording.analysis = analysis;
        }
        builder.enable_streaming(history);
    }
    builder.strings.insert(1, "frame".into());
    builder.strings.insert(2, "ApexFrame".into());
    for (id, type_name) in [
        (100, "physx3::PxScene"),
        (200, "Apex::scene"),
        (7, "_debugger_::PvdUserRenderer"),
        (8, "Actor"),
    ] {
        builder
            .action(Action::Create(Object {
                id,
                generation: 0,
                type_name: type_name.into(),
                properties: BTreeMap::new(),
            }))
            .unwrap();
    }
    builder
}
fn draw(builder: &mut RecordingBuilder, x: f32) {
    builder
        .action(Action::Debug(
            7,
            vec![Primitive::Point {
                instance: 8,
                position: [x, 2.0, 3.0],
                color: 27,
            }]
            .into(),
        ))
        .unwrap();
}
fn xs(state: &SceneState) -> Vec<f32> {
    state
        .debug_draw
        .values()
        .flat_map(|draw| draw.iter())
        .map(|primitive| match primitive {
            Primitive::Point { position, .. } => position[0],
            _ => panic!("unexpected primitive"),
        })
        .collect()
}
fn finish_frame(builder: &mut RecordingBuilder, name: u32, source: u64, value: u64) {
    builder
        .action(Action::Set(8, "value".into(), Value::U64(value)))
        .unwrap();
    builder
        .push(group(vec![endpoint(name, source, 1000 - value)]))
        .unwrap();
}

#[test]
fn in_memory_and_both_disk_backends_restore_source_windows_with_global_object_state() {
    let expected: &[&[f32]] = &[
        &[1.0],
        &[1.0, 2.0],
        &[2.0, 11.0],
        &[11.0, 12.0],
        &[12.0],
        &[],
    ];
    for mode in 0..=2 {
        let directory = TestDirectory::new();
        let mut builder = builder(mode, &directory);
        draw(&mut builder, 1.0);
        finish_frame(&mut builder, 1, 100, 1);
        draw(&mut builder, 2.0);
        finish_frame(&mut builder, 2, 200, 2);
        draw(&mut builder, 11.0);
        finish_frame(&mut builder, 1, 100, 3);
        draw(&mut builder, 12.0);
        finish_frame(&mut builder, 2, 200, 4);
        finish_frame(&mut builder, 1, 100, 5);
        finish_frame(&mut builder, 2, 200, 6);
        draw(&mut builder, 999.0);
        let recording = builder.finish().unwrap();
        assert_eq!(recording.frame_count(), 6);
        for (frame, expected_points) in expected.iter().enumerate() {
            let state = recording.state_at(frame).unwrap();
            assert_eq!(&xs(&state), expected_points, "mode {mode}, frame {frame}");
            assert_eq!(
                state.objects[&8].properties["value"],
                Value::U64(frame as u64 + 1)
            );
        }
        if mode != 0 {
            let mut cursor = ReplayCursor::default();
            let mut displayed = SceneState::default();
            for frame in [0, 1, 2, 3, 4, 5, 4, 3, 2, 1, 0, 5, 2, 4] {
                cursor
                    .seek(&recording, frame, || false)
                    .unwrap()
                    .update
                    .apply(&mut displayed);
                assert_eq!(
                    xs(&displayed),
                    expected[frame],
                    "mode {mode}, cursor {frame}"
                );
                assert_eq!(
                    displayed.objects[&8].properties["value"],
                    Value::U64(frame as u64 + 1)
                );
            }
        }
    }
}

#[test]
fn repeated_end_preparation_is_idempotent_and_late_first_source_reuses_the_view() {
    let directory = TestDirectory::new();
    let mut builder = builder(0, &directory);
    draw(&mut builder, 1.0);
    let end = endpoint(1, 100, 1);
    builder.prepare_end_view(&end).unwrap();
    let actions = builder.recording.actions.len();
    let bytes = builder.memory_usage().estimated_history_bytes;
    let first = builder.state.debug_draw[&7].clone();
    builder.prepare_end_view(&end).unwrap();
    assert_eq!(builder.recording.actions.len(), actions);
    assert_eq!(builder.memory_usage().estimated_history_bytes, bytes);
    assert!(Arc::ptr_eq(&first, &builder.state.debug_draw[&7]));
    builder.push(group(vec![end])).unwrap();
    builder.push(group(vec![endpoint(2, 200, 2)])).unwrap();
    let recording = builder.finish().unwrap();
    let first = recording.state_at(0).unwrap();
    let second = recording.state_at(1).unwrap();
    assert_eq!(xs(&second), [1.0]);
    assert!(Arc::ptr_eq(&first.debug_draw[&7], &second.debug_draw[&7]));
}

#[test]
fn live_selected_source_replays_draws_before_other_source_ends() {
    let directory = TestDirectory::new();
    let mut live = LiveRecording {
        builder: Some(builder(2, &directory)),
        continuous: true,
        ..Default::default()
    };
    let source = FrameSourceKey::Captured {
        section: FrameSection::PhysX,
        source: 100,
    };
    live.set_frame_source(Some(source));
    let mut displayed = SceneState::default();
    draw(live.builder.as_mut().unwrap(), 1.0);
    finish_frame(live.builder.as_mut().unwrap(), 1, 100, 1);
    live.take_update().unwrap().1.apply(&mut displayed);
    assert_eq!(xs(&displayed), [1.0]);
    draw(live.builder.as_mut().unwrap(), 2.0);
    finish_frame(live.builder.as_mut().unwrap(), 2, 200, 2);
    assert!(live.take_update().is_none());
    draw(live.builder.as_mut().unwrap(), 11.0);
    finish_frame(live.builder.as_mut().unwrap(), 1, 100, 3);
    let (frame, update) = live.take_update().unwrap();
    assert_eq!(frame, 2);
    update.apply(&mut displayed);
    assert_eq!(xs(&displayed), [2.0, 11.0]);
    live.refresh_frame_source();
    finish_frame(live.builder.as_mut().unwrap(), 2, 200, 4);
    assert!(live.take_update().is_none());
    finish_frame(live.builder.as_mut().unwrap(), 1, 100, 5);
    let (_, update) = live.take_update().unwrap();
    assert!(update.replace_all);
    update.apply(&mut displayed);
    assert!(xs(&displayed).is_empty());
}

#[test]
fn renderer_lifetime_reuse_and_subject_destruction_do_not_confuse_windows() {
    let directory = TestDirectory::new();
    let mut builder = builder(0, &directory);
    draw(&mut builder, 1.0);
    finish_frame(&mut builder, 1, 100, 1);
    builder.action(Action::Destroy(8)).unwrap();
    builder.push(group(vec![endpoint(2, 200, 2)])).unwrap();
    builder.action(Action::Destroy(7)).unwrap();
    builder
        .action(Action::Create(Object {
            id: 7,
            generation: 1,
            type_name: "_debugger_::PvdUserRenderer".into(),
            properties: BTreeMap::new(),
        }))
        .unwrap();
    draw(&mut builder, 9.0);
    builder.push(group(vec![endpoint(1, 100, 3)])).unwrap();
    let recording = builder.finish().unwrap();
    assert_eq!(xs(&recording.state_at(0).unwrap()), [1.0]);
    let subject_gone = recording.state_at(1).unwrap();
    assert_eq!(xs(&subject_gone), [1.0]);
    assert!(!subject_gone.objects.contains_key(&8));
    let renderer_reused = recording.state_at(2).unwrap();
    assert_eq!(xs(&renderer_reused), [9.0]);
    assert_eq!(renderer_reused.objects[&7].generation, 1);
}

#[test]
fn origin_shift_changes_captured_object_properties_without_translating_raw_draw_windows() {
    let directory = TestDirectory::new();
    let mut builder = builder(0, &directory);
    builder
        .action(Action::Create(Object {
            id: 10,
            generation: 0,
            type_name: "physx3::PxRigidStatic".into(),
            properties: BTreeMap::new(),
        }))
        .unwrap();
    builder
        .action(Action::Set(
            10,
            "GlobalPose".into(),
            Value::Transform {
                position: [100.0, 0.0, 0.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
            },
        ))
        .unwrap();
    builder
        .action(Action::Push(100, "RigidStatics".into(), 10))
        .unwrap();
    draw(&mut builder, 10.0);
    finish_frame(&mut builder, 1, 100, 1);
    draw(&mut builder, 20.0);
    finish_frame(&mut builder, 2, 200, 2);
    builder
        .push(group(vec![Event::OriginShift {
            id: 100,
            shift: [100.0, 0.0, 0.0],
        }]))
        .unwrap();
    draw(&mut builder, 30.0);
    finish_frame(&mut builder, 1, 100, 3);
    let recording = builder.finish().unwrap();
    let state = recording.state_at(2).unwrap();
    assert_eq!(xs(&state), [20.0, 30.0]);
    assert_eq!(
        state.objects[&10].properties["GlobalPose"],
        Value::Transform {
            position: [0.0; 3],
            rotation: [0.0, 0.0, 0.0, 1.0]
        }
    );
}

#[test]
fn validation_preparation_preserves_source_windows_and_stable_current_state_accounting() {
    let directory = TestDirectory::new();
    let mut builder = builder(3, &directory);
    let base = builder.validation.as_ref().unwrap().state_bytes;
    for index in 0..100 {
        let name = if index % 2 == 0 { 1 } else { 2 };
        let source = if name == 1 { 100 } else { 200 };
        draw(&mut builder, index as f32);
        let end = endpoint(name, source, index);
        builder.prepare_end_view(&end).unwrap();
        let expected = if index == 0 {
            vec![0.0]
        } else {
            vec![(index - 1) as f32, index as f32]
        };
        assert_eq!(xs(&builder.state), expected);
        let prepared_bytes = builder.validation.as_ref().unwrap().state_bytes;
        builder.prepare_end_view(&end).unwrap();
        assert_eq!(
            builder.validation.as_ref().unwrap().state_bytes,
            prepared_bytes
        );
        builder.push(group(vec![end])).unwrap();
        assert_eq!(builder.validation.as_ref().unwrap().state_bytes, base);
        assert!(builder.state.debug_draw.is_empty());
    }
    assert_eq!(builder.validation.as_ref().unwrap().frames, 100);
    assert!(builder.recording.actions.is_empty());
}
