use super::*;
use pvd_core::{
    Recording, RecordingBuilder,
    frame_sources::{FrameDirectory, FrameDirectoryLimits},
};
use pvd_protocol::{Endian, Event, EventGroup, Header, Name};

fn room(id: u64, generation: u32) -> FrameObject {
    FrameObject { id, generation }
}

fn create(id: u64, class: u32) -> Event {
    Event::CreateInstance {
        id,
        class: Name {
            namespace: 1,
            name: class,
        },
    }
}

fn reference(id: u64, target: u64) -> Event {
    Event::SetPropertyValue {
        id,
        property: 4,
        datatype: Name {
            namespace: 1,
            name: 5,
        },
        items: 1,
        data: target.to_le_bytes().to_vec().into(),
    }
}

fn recording() -> Recording {
    let mut builder = RecordingBuilder::new(Header {
        version: 1,
        timestamp_numerator: 1,
        timestamp_denominator: 1,
        flags: 1,
        endian: Endian::Little,
    });
    let mut initial: Vec<_> = [
        (1, "physx3"),
        (2, "PxScene"),
        (3, "PxRigidDynamic"),
        (4, "Scene"),
        (5, "ObjectRef"),
        (6, "frame"),
    ]
    .into_iter()
    .map(|(handle, value)| Event::StringHandle {
        handle,
        value: value.into(),
    })
    .collect();
    initial.extend([create(100, 2), create(10, 3), reference(10, 100)]);
    for (index, mut events) in [
        initial,
        vec![create(200, 2), create(20, 3), reference(20, 200)],
        vec![Event::DestroyInstance { id: 100 }],
        vec![create(100, 2), create(11, 3), reference(11, 100)],
        vec![],
    ]
    .into_iter()
    .enumerate()
    {
        events.push(Event::EndSection {
            id: if matches!(index, 0 | 3) { 100 } else { 200 },
            name: 6,
            timestamp: index as u64,
        });
        if index == 4 {
            events.push(Event::DestroyInstance { id: 200 });
        }
        builder
            .push(EventGroup {
                stream_id: 1,
                timestamp: 0,
                offset: 0,
                events,
            })
            .unwrap();
    }
    let mut recording = builder.snapshot().unwrap();
    recording.frame_sources =
        FrameDirectory::from_legacy(&recording.frames, FrameDirectoryLimits::default()).unwrap();
    recording
}

#[test]
fn shared_legacy_axis_remains_replayable_and_is_bounded_by_scene_lifetime() {
    let mut studio = Studio::default();
    studio.set_recording(recording());
    for (selected, bounds) in [
        (room(100, 0), (0, 1)),
        (room(200, 0), (1, 4)),
        (room(100, 1), (3, 4)),
    ] {
        assert_eq!(studio.room_source(selected), Some(FrameSourceKey::Legacy));
        assert!(studio.room_has_history(selected));
        studio.select_room(Some(selected));
        assert_eq!(studio.displayed_room(), Some(selected));
        assert_eq!(studio.frame_source(), Some(FrameSourceKey::Legacy));
        assert_eq!(studio.playback_bounds(), Some(bounds));
        assert_eq!(studio.playback_ordinal_bounds(), Some(bounds));
        assert_eq!(studio.retained_playback_bounds(), Some(bounds));
        assert_eq!(studio.frame_at_ordinal(0), Some(bounds.0));
        assert_eq!(studio.frame_at_ordinal(usize::MAX), Some(bounds.1));
        studio.seek(bounds.0);
        assert_eq!(studio.room_view.requested, Some(selected));
        assert_eq!(studio.displayed_room(), Some(selected));
        assert_eq!(studio.next_playback_frame(-1), None);
        for frame in bounds.0 + 1..=bounds.1 {
            assert_eq!(studio.next_playback_frame(1), Some(frame));
            studio.step_frame(1);
            assert_eq!(studio.frame, frame);
            assert_eq!(
                studio.state.objects[&selected.id].generation,
                selected.generation
            );
        }
        assert_eq!(studio.next_playback_frame(1), None);
        for frame in (bounds.0..bounds.1).rev() {
            studio.step_frame(-1);
            assert_eq!(studio.frame, frame);
        }
    }
    assert!(studio.room_scope.contains(11));
    assert!(!studio.room_scope.contains(10));
}

#[test]
fn legacy_room_switch_waits_for_seek_completion_and_uses_same_global_position() {
    let mut studio = Studio::default();
    studio.set_recording(recording());
    studio.select_room(Some(room(200, 0)));
    studio.seek(3);
    assert_eq!(studio.frame, 3);
    studio.select_room(Some(room(100, 1)));
    assert_eq!(studio.frame, 3);
    assert_eq!(studio.displayed_room(), Some(room(100, 1)));
    studio.room_view.requested = Some(room(200, 0));
    studio.seeking = Some(4);
    studio.commit_room_display();
    assert_eq!(studio.displayed_room(), Some(room(100, 1)));
    assert!(studio.room_selection_pending());
    studio.seeking = None;
    studio.seek(4);
    assert_eq!(studio.displayed_room(), Some(room(200, 0)));
    assert!(!studio.room_selection_pending());
}

#[test]
fn legacy_missing_history_does_not_replace_the_displayed_room() {
    let mut recording = recording();
    recording.frame_sources.discard_before(2);
    let mut studio = Studio::default();
    studio.set_recording(recording);
    assert!(!studio.room_has_history(room(100, 0)));
    studio.select_room(Some(room(200, 0)));
    let displayed = studio.displayed_room();
    let frame = studio.frame;
    studio.select_room(Some(room(100, 0)));
    assert_eq!(studio.displayed_room(), displayed);
    assert_eq!(studio.room_view.requested, displayed);
    assert_eq!(studio.frame, frame);
    assert_eq!(studio.playback_bounds(), Some((2, 4)));
}
