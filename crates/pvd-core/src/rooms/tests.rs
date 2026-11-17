use super::*;
use crate::{
    LiveRecording, RecordingBuilder,
    frame_source_tests::TestDirectory,
    frame_sources::{FrameSection, FrameSourceKey},
};
use pvd_protocol::{Endian, Event, EventGroup, Header};

fn identity(id: u64, generation: u32) -> FrameObject {
    FrameObject { id, generation }
}

fn header() -> Header {
    Header {
        version: 1,
        timestamp_numerator: 1,
        timestamp_denominator: 1,
        flags: 1,
        endian: Endian::Little,
    }
}

fn create(builder: &mut RecordingBuilder, id: u64, generation: u32) {
    builder
        .action(Action::Create(Object {
            id,
            generation,
            type_name: "physx3::PxScene".into(),
            properties: BTreeMap::new(),
        }))
        .unwrap();
}

fn complete(builder: &mut RecordingBuilder, id: u64, timestamp: u64) {
    builder
        .push(EventGroup {
            stream_id: 7,
            timestamp,
            offset: 0,
            events: vec![
                Event::StringHandle {
                    handle: 1,
                    value: "frame".into(),
                },
                Event::EndSection {
                    id,
                    name: 1,
                    timestamp,
                },
            ],
        })
        .unwrap();
}

#[test]
fn number_order_follows_creation_and_reused_address_has_new_identity() {
    let mut builder = RecordingBuilder::new(header());
    create(&mut builder, 20, 0);
    create(&mut builder, 10, 0);
    builder.action(Action::Destroy(20)).unwrap();
    create(&mut builder, 20, 1);
    let recording = builder.finish().unwrap();
    assert_eq!(recording.frame_count(), 0);
    assert_eq!(
        recording
            .rooms
            .iter()
            .map(|room| (room.number, room.scene, room.destroyed_at_frame))
            .collect::<Vec<_>>(),
        [
            (1, identity(20, 0), Some(0)),
            (2, identity(10, 0), None),
            (3, identity(20, 1), None),
        ]
    );
}

#[test]
fn tail_destruction_is_independent_of_selected_source_and_replay() {
    let directory = TestDirectory::new();
    let mut builder = RecordingBuilder::new(header());
    builder.enable_streaming(crate::live::HistoryWriter::new_paged(directory.config()).unwrap());
    create(&mut builder, 20, 0);
    create(&mut builder, 10, 0);
    builder
        .streaming
        .as_mut()
        .unwrap()
        .set_frame_source(Some(FrameSourceKey::Captured {
            section: FrameSection::PhysX,
            source: 20,
        }));
    complete(&mut builder, 20, 100);
    complete(&mut builder, 10, 200);
    let mut live = LiveRecording {
        builder: Some(builder),
        continuous: true,
        ..Default::default()
    };
    assert_eq!(live.take_update().unwrap().0, 0);
    let before = live.rooms().unwrap().clone();
    live.builder
        .as_mut()
        .unwrap()
        .action(Action::Destroy(20))
        .unwrap();
    assert!(live.take_update().is_none());
    assert_eq!(live.frame_count(), 2);
    let after = live.rooms().unwrap();
    assert!(after.revision() > before.revision());
    assert_eq!(
        before.get(identity(20, 0)).unwrap().destroyed_at_frame,
        None
    );
    assert_eq!(
        after.get(identity(20, 0)).unwrap().destroyed_at_frame,
        Some(2)
    );
    let recording = live.snapshot().unwrap();
    assert!(recording.state_at(0).unwrap().objects.contains_key(&20));
    assert_eq!(
        recording
            .rooms
            .get(identity(20, 0))
            .unwrap()
            .destroyed_at_frame,
        Some(2)
    );
    assert_eq!(recording.frame_count(), 2);
}

#[test]
fn failed_create_does_not_publish_a_room() {
    let mut builder = RecordingBuilder::with_memory_limit(header(), 0);
    assert!(
        builder
            .action(Action::Create(Object {
                id: 1,
                generation: 0,
                type_name: "physx3::PxScene".into(),
                properties: BTreeMap::new(),
            }))
            .is_err()
    );
    assert!(builder.recording.rooms.is_empty());
}

#[test]
fn bounded_directory_reclaims_terminal_entries_without_reusing_numbers() {
    let mut rooms = RoomDirectory::default();
    for id in 0..ROOM_LIMIT as u64 {
        rooms.observe(identity(id, 0), 0);
    }
    rooms.observe(identity(50_000, 0), 0);
    assert_eq!(rooms.len(), ROOM_LIMIT);
    assert_eq!(rooms.omitted_count(), 1);
    rooms.destroy(identity(5, 0), 3);
    rooms.observe(identity(60_000, 0), 4);
    assert!(rooms.get(identity(5, 0)).is_none());
    assert_eq!(
        rooms.get(identity(60_000, 0)).unwrap().number,
        ROOM_LIMIT as u64 + 2
    );
    assert_eq!(rooms.len(), ROOM_LIMIT);
    assert_eq!(rooms.omitted_count(), 2);
    assert!(rooms.memory_bytes() < 2 << 20);
}

#[test]
fn unrelated_objects_and_unproven_destroy_do_not_create_room_entries() {
    let mut builder = RecordingBuilder::new(header());
    builder.action(Action::Destroy(777)).unwrap();
    builder
        .action(Action::Create(Object {
            id: 777,
            generation: 0,
            type_name: "physx3::PxRigidDynamic".into(),
            properties: BTreeMap::new(),
        }))
        .unwrap();
    builder.action(Action::Destroy(777)).unwrap();
    assert!(builder.recording.rooms.is_empty());
}

#[test]
fn saved_capture_keeps_tail_lifecycles_but_prefix_excludes_future_observations() {
    let directory = TestDirectory::new();
    let mut builder = RecordingBuilder::new(header());
    builder.enable_streaming(crate::live::HistoryWriter::new_paged(directory.config()).unwrap());
    create(&mut builder, 20, 0);
    complete(&mut builder, 20, 100);
    create(&mut builder, 10, 0);
    complete(&mut builder, 10, 200);
    builder.action(Action::Destroy(20)).unwrap();
    create(&mut builder, 30, 0);
    builder.action(Action::Destroy(30)).unwrap();
    let recording = builder.finish().unwrap();
    for last in [0, 1] {
        let path = directory
            .config()
            .directory
            .join(format!("rooms-{last}.pvdcap"));
        let mut output = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        crate::capture::write_retained(
            &recording,
            &mut output,
            crate::capture::CaptureRange { first: 0, last },
            &Default::default(),
            |_| true,
        )
        .unwrap();
        drop(output);
        let reopened = crate::Recording::from_path(&path).unwrap();
        assert_eq!(reopened.frame_count(), last + 1);
        assert!(reopened.state_at(0).unwrap().objects.contains_key(&20));
        if last == 0 {
            assert_eq!(reopened.rooms.len(), 1);
            assert_eq!(reopened.rooms.omitted_count(), 0);
            assert_eq!(
                reopened
                    .rooms
                    .get(identity(20, 0))
                    .unwrap()
                    .destroyed_at_frame,
                None
            );
        } else {
            assert_eq!(reopened.rooms.len(), 3);
            assert_eq!(
                reopened
                    .rooms
                    .get(identity(20, 0))
                    .unwrap()
                    .destroyed_at_frame,
                Some(2)
            );
            let zero_frame = reopened.rooms.get(identity(30, 0)).unwrap();
            assert_eq!(zero_frame.first_observed_frame, 2);
            assert_eq!(zero_frame.destroyed_at_frame, Some(2));
            assert_eq!(zero_frame.number, 3);
        }
        drop(reopened);
        std::fs::remove_file(path).unwrap();
    }
}

#[test]
fn directory_metadata_rejects_duplicate_identity_and_invalid_lifetime() {
    let valid = RoomInfo {
        scene: identity(4, 0),
        number: 1,
        first_observed_frame: 3,
        destroyed_at_frame: Some(4),
    };
    assert!(RoomDirectory::from_entries([valid.clone(), valid.clone()], 2).is_err());
    let mut invalid = valid.clone();
    invalid.number = 2;
    assert!(RoomDirectory::from_entries([valid.clone(), invalid], 2).is_err());
    let mut invalid = valid.clone();
    invalid.destroyed_at_frame = Some(2);
    assert!(RoomDirectory::from_entries([invalid], 1).is_err());
    assert!(RoomDirectory::from_entries([valid], 0).is_err());
}
