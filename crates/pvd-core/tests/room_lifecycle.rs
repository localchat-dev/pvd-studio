use pvd_core::{
    LiveRecording, Recording,
    capture::{CaptureRange, write_retained},
    frame_sources::FrameObject,
    live::HistoryConfig,
    rooms::RoomInfo,
};
use serde::Deserialize;
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
struct Truth {
    scenario: String,
    disconnected: bool,
    rooms: Vec<Room>,
    checkpoints: Vec<Checkpoint>,
}

#[derive(Deserialize)]
struct Room {
    number: u64,
    id: u64,
    steps: usize,
    destroyed: bool,
}

#[derive(Deserialize)]
struct Checkpoint {
    name: String,
    bytes: usize,
    frames: usize,
    alive: Vec<u64>,
}

fn expected_rooms(truth: &Truth) -> Vec<RoomInfo> {
    let at = |name: &str| {
        truth
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.name == name)
            .unwrap()
            .frames
    };
    let positions = [
        (0, Some(at("tail-destroyed"))),
        (0, Some(at("b-destroyed"))),
        (0, Some(at("zero-destroyed"))),
        (at("b-destroyed"), None),
    ];
    truth
        .rooms
        .iter()
        .enumerate()
        .map(|(index, room)| RoomInfo {
            scene: FrameObject {
                id: room.id,
                generation: truth.rooms[..index]
                    .iter()
                    .filter(|earlier| earlier.id == room.id)
                    .count() as u32,
            },
            number: room.number,
            first_observed_frame: positions[index].0,
            destroyed_at_frame: positions[index].1,
        })
        .collect()
}

fn save_and_check(
    source: &Recording,
    baseline: &Recording,
    expected: &[RoomInfo],
    range: CaptureRange,
    path: &Path,
) -> Recording {
    let mut output = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap();
    write_retained(source, &mut output, range, &Default::default(), |_| true).unwrap();
    drop(output);
    let reopened = Recording::from_path(path).unwrap();
    assert_eq!(reopened.first_frame(), range.first);
    assert_eq!(reopened.frame_count(), range.last + 1);
    assert_eq!(reopened.frames.len(), range.last - range.first + 1);
    let include_tail = range.last + 1 == baseline.frame_count();
    let expected: Vec<_> = expected
        .iter()
        .filter(|room| include_tail || room.first_observed_frame <= range.last)
        .cloned()
        .map(|mut room| {
            room.destroyed_at_frame = room
                .destroyed_at_frame
                .filter(|frame| include_tail || *frame <= range.last);
            room
        })
        .collect();
    assert_eq!(reopened.rooms.iter().cloned().collect::<Vec<_>>(), expected);
    assert_eq!(reopened.rooms.omitted_count(), 0);
    for frame in (range.first..=range.last).chain((range.first..=range.last).rev()) {
        assert_eq!(
            reopened.state_at(frame).unwrap(),
            baseline.state_at(frame).unwrap(),
            "saved range {}..={} at frame {frame}",
            range.first,
            range.last
        );
        assert_eq!(
            reopened.frame_sources.frame(frame),
            baseline.frame_sources.frame(frame)
        );
    }
    reopened
}

#[test]
#[ignore = "requires PhysX 3.4 multiscene --lifecycle capture and same-run JSON"]
fn sdk_room_directory_follows_zero_frame_and_tail_destruction_without_next_end() {
    let path = PathBuf::from(
        std::env::var_os("PVD_ROOM_LIFECYCLE_CAPTURE")
            .expect("PVD_ROOM_LIFECYCLE_CAPTURE must identify a same-run capture/JSON pair"),
    );
    let bytes = fs::read(&path).unwrap();
    let truth: Truth =
        serde_json::from_slice(&fs::read(path.with_extension("json")).unwrap()).unwrap();
    assert_eq!(truth.scenario, "room-lifecycle");
    assert!(truth.disconnected);
    assert_eq!(truth.rooms.len(), 4);
    let cache = path.with_file_name(format!("room-core-history-{}", std::process::id()));
    let mut live = LiveRecording::with_history(HistoryConfig {
        directory: cache.clone(),
        segment_frames: 2,
        segment_target_bytes: 1 << 20,
        disk_budget_bytes: 16 << 20,
        min_free_disk_bytes: 0,
    })
    .unwrap();
    let mut offset = 0;
    for checkpoint in &truth.checkpoints {
        for chunk in bytes[offset..checkpoint.bytes].chunks(997) {
            live.feed(chunk).unwrap();
        }
        offset = checkpoint.bytes;
        assert_eq!(live.frame_count(), checkpoint.frames, "{}", checkpoint.name);
        let rooms = live.rooms().unwrap();
        assert_eq!(
            rooms
                .iter()
                .filter(|room| room.destroyed_at_frame.is_none())
                .map(|room| room.number)
                .collect::<Vec<_>>(),
            checkpoint.alive,
            "{}",
            checkpoint.name
        );
        for room in rooms.iter() {
            assert_eq!(room.scene.id, truth.rooms[room.number as usize - 1].id);
        }
        if checkpoint.name == "zero-destroyed" {
            let zero = rooms.iter().find(|room| room.number == 3).unwrap();
            assert_eq!(zero.destroyed_at_frame, Some(0));
            assert_eq!(live.frame_count(), 0);
        }
        if checkpoint.name == "tail-destroyed" {
            let first = rooms.iter().find(|room| room.number == 1).unwrap();
            assert_eq!(first.destroyed_at_frame, Some(checkpoint.frames));
        }
    }
    live.feed(&bytes[offset..]).unwrap();
    let recorded = live.finish().unwrap();
    assert_eq!(
        recorded.frame_count(),
        truth.rooms.iter().map(|room| room.steps).sum::<usize>()
    );
    for room in &truth.rooms {
        let observed = recorded
            .rooms
            .iter()
            .find(|entry| entry.number == room.number)
            .unwrap();
        assert_eq!(observed.destroyed_at_frame.is_some(), room.destroyed);
        assert_eq!(observed.scene.id, room.id);
    }
    let first = recorded.rooms.iter().next().unwrap().scene;
    assert!(
        recorded
            .state_at(0)
            .unwrap()
            .objects
            .contains_key(&first.id)
    );
    assert_eq!(
        recorded.rooms.get(first).unwrap().destroyed_at_frame,
        Some(8)
    );
    let reopened = Recording::from_path(&path).unwrap();
    for room in recorded.rooms.iter() {
        assert_eq!(reopened.rooms.get(room.scene), Some(room));
    }
    let last = recorded.rooms.iter().last().unwrap();
    assert_eq!(last.destroyed_at_frame, None);
    assert_eq!(
        last.scene,
        FrameObject {
            id: truth.rooms[3].id,
            generation: truth.rooms[..3]
                .iter()
                .filter(|room| room.id == truth.rooms[3].id)
                .count() as u32
        }
    );
    let expected = expected_rooms(&truth);
    assert_eq!(recorded.rooms.iter().cloned().collect::<Vec<_>>(), expected);
    let full_path = cache.join("rooms-full.pvdcapture");
    let full = save_and_check(
        &recorded,
        &reopened,
        &expected,
        CaptureRange { first: 0, last: 7 },
        &full_path,
    );
    for (label, range) in [
        ("prefix", CaptureRange { first: 0, last: 3 }),
        ("middle", CaptureRange { first: 2, last: 5 }),
        ("tail", CaptureRange { first: 4, last: 7 }),
    ] {
        let saved_path = cache.join(format!("rooms-{label}.pvdcapture"));
        let saved = save_and_check(&recorded, &reopened, &expected, range, &saved_path);
        let cropped_path = cache.join(format!("rooms-recut-{label}.pvdcapture"));
        let cropped = save_and_check(&full, &reopened, &expected, range, &cropped_path);
        assert_eq!(
            saved.rooms.iter().collect::<Vec<_>>(),
            cropped.rooms.iter().collect::<Vec<_>>()
        );
        drop(cropped);
        drop(saved);
        fs::remove_file(cropped_path).unwrap();
        fs::remove_file(saved_path).unwrap();
    }
    drop(full);
    fs::remove_file(full_path).unwrap();
    drop(reopened);
    drop(recorded);
    assert_eq!(fs::read_dir(&cache).unwrap().count(), 0);
    fs::remove_dir(cache).unwrap();
}
