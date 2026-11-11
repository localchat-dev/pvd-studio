use super::*;
use crate::Value;

mod fixture;
use fixture::Input;

const GAP: SourceCamera = SourceCamera {
    position: [101.0, -0.0, f32::from_bits(1)],
    up: [1.0, 2.0, 3.0],
    target: [4.0, 5.0, 7.0],
};
const MAIN_GAP_ORIGIN: SourceCamera = SourceCamera {
    position: MAIN_B.position,
    ..GAP
};
const MAIN_D: SourceCamera = SourceCamera {
    position: [201.0, 202.0, 203.0],
    ..MAIN_C
};
const MAIN_E: SourceCamera = SourceCamera {
    position: [301.0, 302.0, 303.0],
    up: [0.0, -1.0, 0.0],
    target: [9.0, 8.0, 7.0],
};
const LATE_C: SourceCamera = SourceCamera {
    target: [40.0, 50.0, 60.0],
    ..LATE_A
};
const LATE_D: SourceCamera = SourceCamera {
    position: LATE_B.position,
    ..LATE_C
};
const LATE_E: SourceCamera = SourceCamera {
    position: [701.0, 702.0, 703.0],
    up: [0.0, 0.0, -1.0],
    target: [7.0, 6.0, 5.0],
};
const LATE_F: SourceCamera = SourceCamera {
    position: [801.0, 802.0, 803.0],
    ..LATE_E
};
const ORBIT_A: SourceCamera = SourceCamera {
    position: [401.0, 402.0, 403.0],
    up: [1.0, 0.0, 0.0],
    target: [-4.0, -5.0, -6.0],
};
const ORBIT_B: SourceCamera = SourceCamera {
    position: [501.0, 502.0, 503.0],
    ..ORBIT_A
};
const TAIL: SourceCamera = SourceCamera {
    position: [901.0, 902.0, 903.0],
    ..ORBIT_B
};
const SCENES: [u64; 8] = [1, 2, 2, 1, 2, 1, 1, 2];
const TICKS: [u64; 8] = [1010, 20, 40, 1110, 60, 1210, 1310, 80];
const VIEWS: [(u64, u64); 8] = [
    (7, 7),
    (7, 7),
    (7, 8),
    (8, 8),
    (8, 8),
    (9, 8),
    (9, 8),
    (9, 9),
];

fn raw_camera(handle: u32, value: SourceCamera) -> [u8; 40] {
    let mut bytes = [0; 40];
    bytes[..4].copy_from_slice(&handle.to_le_bytes());
    bytes[4..].copy_from_slice(&vectors(value));
    bytes
}

fn cameras(frame: usize) -> BTreeMap<String, SourceCamera> {
    let rows: &[(&str, SourceCamera)] = match frame {
        0 => &[("main", MAIN_A)],
        1 => &[("main", MAIN_GAP_ORIGIN)],
        2 => &[("main", MAIN_D), ("late", LATE_C)],
        3 => &[("main", MAIN_E), ("late", LATE_C)],
        4 => &[("late", LATE_E)],
        5 => &[("late", LATE_E), ("orbit", ORBIT_A)],
        6 => &[("late", LATE_F), ("orbit", ORBIT_B)],
        7 => &[("orbit", ORBIT_B)],
        _ => panic!("unexpected global frame"),
    };
    rows.iter()
        .map(|&(name, value)| (name.into(), value))
        .collect()
}

fn check_state(state: &crate::SceneState, frame: usize) {
    assert_eq!(state.cameras, cameras(frame), "global frame {frame}");
    assert_eq!(
        state.objects[&1].properties["View"],
        Value::Ref(VIEWS[frame].0)
    );
    assert_eq!(
        state.objects[&2].properties["View"],
        Value::Ref(VIEWS[frame].1)
    );
    assert!(!state.objects.contains_key(&10));
    assert_eq!(state.objects.contains_key(&7), frame < 4);
    assert_eq!(state.objects.contains_key(&8), (2..7).contains(&frame));
    assert_eq!(state.objects.contains_key(&9), frame >= 5);
}

fn check(recording: &Recording, first: usize, last: usize) {
    assert_eq!(recording.frame_count(), last - first + 1);
    for frame in (0..recording.frame_count())
        .chain((0..recording.frame_count()).rev())
        .chain([last - first, 0, (last - first) / 2])
    {
        check_state(&recording.state_at(frame).unwrap(), first + frame);
        assert_eq!(recording.frames[frame].scene_id, SCENES[first + frame]);
        assert_eq!(recording.frames[frame].timestamp, TICKS[first + frame]);
    }
    recording.verify_replay().unwrap();
}

fn trim(bytes: &[u8], first: usize, last: usize) -> Vec<u8> {
    let mut output = Vec::new();
    trim_scene_frames(bytes, first, last, &mut output, || true).unwrap();
    let original = Recording::from_bytes(bytes).unwrap();
    let result = Recording::from_bytes(&output).unwrap();
    for frame in (0..result.frame_count()).rev() {
        let actual = result.state_at(frame).unwrap();
        let expected = original.state_at(first + frame).unwrap();
        assert_eq!(actual.objects, expected.objects);
        assert_eq!(actual.reference_lifetimes, expected.reference_lifetimes);
        assert_eq!(actual.cameras, expected.cameras);
        assert_eq!(actual.debug_draw, expected.debug_draw);
        assert_eq!(actual.contacts, expected.contacts);
        assert_eq!(actual.errors, expected.errors);
    }
    let layout = Layout::parse(&output, &mut || true).unwrap();
    assert_eq!(layout.communication.clock, [3, 7]);
    assert_eq!(layout.snapshots[0].reserved, 492);
    let mut rewritten = Vec::new();
    rewrite(&output, &mut rewritten, || true).unwrap();
    assert_eq!(rewritten, output);
    output
}

fn initial_cameras(layout: &Layout<'_>) -> BTreeMap<u32, [u8; 40]> {
    let mut result = BTreeMap::new();
    let mut reader = Reader::new(&layout.snapshot_bytes[0]);
    for _ in 0..reader.count().unwrap() {
        let id = reader.u32().unwrap();
        let position = reader.u64().unwrap();
        if !layout.communication.cameras.iter().any(|row| row.1 == id) {
            continue;
        }
        let record = layout
            .saved
            .iter()
            .find(|record| record.position == position)
            .unwrap();
        assert_eq!((record.id, record.class, record.deleted), (id, 4, false));
        let mut body = Reader::new(&record.body);
        result.insert(id, body.blob().unwrap().try_into().unwrap());
        body.end().unwrap();
    }
    reader.end().unwrap();
    result
}

fn raw_history(layout: &Layout<'_>) -> Vec<Vec<u8>> {
    layout
        .events
        .iter()
        .map(|event| {
            let mut event = event.clone();
            if let HistoryEvent::Saved { position, .. } = &mut event {
                *position = 0;
            }
            let mut bytes = Vec::new();
            event.encode(&mut bytes).unwrap();
            bytes
        })
        .collect()
}

#[test]
fn camera_multi_scene_uses_global_end_order_without_a_scene_or_clock_assignment() {
    for initial in [false, true] {
        let source = Input::new(initial).bytes();
        let (recording, info) = read_with_progress(&source, |_, _| true).unwrap();
        check(&recording, 0, 7);
        for id in [7, 8, 9, 10] {
            assert!(!info.source_ids.contains_key(&id));
        }
        let layout = Layout::parse(&source, &mut || true).unwrap();
        assert_eq!(
            layout
                .communication
                .sections
                .iter()
                .map(|section| section.source)
                .collect::<Vec<_>>(),
            [0x222, 0x111]
        );
        for last in [0, 1, 3, 4, 5, 7] {
            let bytes = trim(&source, 0, last);
            check(&Recording::from_bytes(&bytes).unwrap(), 0, last);
        }
        for (first, last) in [(1, 1), (1, 3), (2, 3), (2, 7), (4, 7), (5, 5), (6, 7)] {
            let bytes = trim(&source, first, last);
            check(&Recording::from_bytes(&bytes).unwrap(), first, last);
        }
    }
}

#[test]
fn camera_multi_scene_preserves_gap_updates_raw_baselines_and_retired_bodies() {
    for initial in [false, true] {
        let source = Input::new(initial).bytes();
        for (first, expected) in [
            (1, vec![(7, raw_camera(22, GAP))]),
            (
                2,
                vec![(7, raw_camera(22, MAIN_C)), (8, raw_camera(15, LATE_A))],
            ),
            (4, vec![(8, raw_camera(15, LATE_D))]),
            (5, vec![(8, raw_camera(15, LATE_E))]),
            (
                6,
                vec![(8, raw_camera(15, LATE_E)), (9, raw_camera(23, ORBIT_A))],
            ),
        ] {
            let bytes = trim(&source, first, 7);
            let layout = Layout::parse(&bytes, &mut || true).unwrap();
            assert_eq!(initial_cameras(&layout), expected.into_iter().collect());
            let directory: &[(u32, u32)] = if first < 4 {
                &[(14, 7), (23, 9), (15, 8)]
            } else {
                &[(23, 9), (15, 8)]
            };
            assert_eq!(layout.communication.cameras, directory);
            for record in layout.saved.iter().filter(|record| record.deleted) {
                let expected = match record.id {
                    7 => raw_camera(22, MAIN_E),
                    8 => raw_camera(15, LATE_F),
                    _ => panic!("unexpected deleted camera"),
                };
                let mut body = Reader::new(&record.body);
                assert_eq!(body.blob().unwrap(), expected);
                body.end().unwrap();
            }
            if first == 2 {
                let HistoryEvent::Set {
                    id, property, undo, ..
                } = &layout.events[2]
                else {
                    panic!("expected the retained Origin update")
                };
                assert_eq!((*id, *property, undo.as_slice()), (7, 1, &[2][..]));
            }
        }
        let bytes = trim(&source, 5, 5);
        assert_eq!(
            Layout::parse(&bytes, &mut || true)
                .unwrap()
                .communication
                .cameras,
            [(23, 9), (15, 8)]
        );
        let layout = Layout::parse(&source, &mut || true).unwrap();
        assert!(layout.communication.cameras.contains(&(24, 10)));
        for id in [9, 10] {
            let data = layout
                .events
                .iter()
                .rev()
                .find_map(|event| match event {
                    HistoryEvent::Set {
                        kind: 4,
                        id: owner,
                        data,
                        ..
                    } if *owner == id => Some(data),
                    _ => None,
                })
                .unwrap();
            assert_eq!(data.as_ref(), vectors(TAIL));
        }
    }
}

#[test]
fn camera_multi_scene_retrim_matches_the_direct_source_interval() {
    for initial in [false, true] {
        let source = Input::new(initial).bytes();
        let middle = trim(&source, 2, 7);
        let repeated = trim(&middle, 2, 5);
        let direct = trim(&source, 4, 7);
        check(&Recording::from_bytes(&repeated).unwrap(), 4, 7);
        let left = Layout::parse(&repeated, &mut || true).unwrap();
        let right = Layout::parse(&direct, &mut || true).unwrap();
        assert_eq!(initial_cameras(&left), initial_cameras(&right));
        assert_eq!(raw_history(&left), raw_history(&right));
        assert_eq!(left.communication.cameras, right.communication.cameras);
        let prefix = trim(&middle, 0, 1);
        check(&Recording::from_bytes(&prefix).unwrap(), 2, 3);
        let prefix = trim(&source, 0, 5);
        let middle = trim(&prefix, 2, 3);
        check(&Recording::from_bytes(&middle).unwrap(), 2, 3);
    }
}

#[test]
fn camera_multi_scene_proof_binds_frame_events_directory_and_clock() {
    let source = Input::new(true).bytes();
    let layout = Layout::parse(&source, &mut || true).unwrap();
    let proof = camera::Scope::prepare(&layout, 2, 7, &mut || true)
        .unwrap()
        .unwrap();
    let bytes = trim(&source, 2, 7);
    proof
        .verify_layout(
            &Layout::parse(&bytes, &mut || true).unwrap(),
            1,
            &mut || true,
        )
        .unwrap();
    for variant in 0..8 {
        let mut changed = Layout::parse(&bytes, &mut || true).unwrap();
        match variant {
            0 => changed.communication.clock[1] += 1,
            1 => changed.communication.sections.reverse(),
            2 => {
                changed.communication.sections[0].rows.remove(0);
            }
            3 => changed.communication.sections[0].rows[0][2] += 1,
            4 => changed.communication.sections[0].source ^= 1,
            _ => {
                let event = changed
                    .events
                    .iter_mut()
                    .find(|event| matches!(event, HistoryEvent::Section { begin: 0, .. }))
                    .unwrap();
                let HistoryEvent::Section {
                    source,
                    ticks,
                    padding,
                    ..
                } = event
                else {
                    unreachable!()
                };
                match variant {
                    5 => {
                        *ticks += 1;
                        changed.communication.sections[0].rows[0][2] += 1;
                    }
                    6 => *source ^= 1,
                    7 => padding[0] ^= 1,
                    _ => unreachable!(),
                }
            }
        }
        assert!(
            proof.verify_layout(&changed, 1, &mut || true).is_err(),
            "frame proof variant {variant}"
        );
    }
    let prefix_proof = camera::Scope::prepare(&layout, 0, 3, &mut || true)
        .unwrap()
        .unwrap();
    let prefix = trim(&source, 0, 3);
    let mut changed = Layout::parse(&prefix, &mut || true).unwrap();
    prefix_proof
        .verify_layout(&changed, 0, &mut || true)
        .unwrap();
    changed.communication.sections[0].rows[0][0] += 1;
    assert!(
        prefix_proof
            .verify_layout(&changed, 0, &mut || true)
            .is_err()
    );
}

#[test]
fn camera_multi_scene_keeps_boundary_undo_and_reference_guards() {
    for initial in [false, true] {
        let source = Input::new(initial).bytes();
        for (first, last, reason) in [
            (0, 2, "ends inside"),
            (3, 7, "starts inside"),
            (7, 7, "starts inside"),
            (2, 2, "ends inside"),
            (4, 6, "ends inside"),
            (1, 8, "out of range"),
        ] {
            rejected(&source, first, last, reason);
        }
        let mut input = Input::new(initial);
        let event = input
            .events
            .iter_mut()
            .find(|bytes| bytes.len() >= 12 && bytes[..12] == raw_words(&[3, 7, 1]))
            .unwrap();
        let end = event.len();
        event[end - 4..].fill(0);
        rejected(&input.bytes(), 2, 7, "undo target must strictly precede");
        let mut input = Input::new(initial);
        let event = input
            .events
            .iter_mut()
            .find(|bytes| bytes.len() >= 12 && bytes[..12] == raw_words(&[3, 10, 0]))
            .unwrap();
        event[16..20].copy_from_slice(&16u32.to_le_bytes());
        rejected(
            &input.bytes(),
            0,
            0,
            "camera name differs from its directory",
        );
    }
    let mut input = Input::new(false);
    let event = input
        .events
        .iter_mut()
        .find(|bytes| bytes.len() >= 12 && bytes[..12] == raw_words(&[3, 1, 7]))
        .unwrap();
    event[16..20].copy_from_slice(&99u32.to_le_bytes());
    let mut output = Vec::new();
    assert!(trim_scene_frames(&input.bytes(), 0, 1, &mut output, || true).is_err());
    assert!(output.is_empty());
}

#[test]
fn camera_multi_scene_disk_cursor_restores_forward_reverse_and_random_frames() {
    for initial in [false, true] {
        let source = Input::new(initial).bytes();
        let bytes = trim(&source, 2, 7);
        let memory = Recording::from_bytes(&bytes).unwrap();
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../output/multi-scene-camera-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        let config = crate::live::HistoryConfig {
            directory: directory.clone(),
            disk_budget_bytes: 32 << 20,
            segment_target_bytes: 1 << 20,
            segment_frames: 1,
            min_free_disk_bytes: 0,
        };
        let (disk, _) = crate::native::import::read_source_with_progress(
            std::io::Cursor::new(&bytes),
            Some(config),
            |_, _| true,
        )
        .unwrap();
        assert!(disk.is_disk_backed());
        assert!(disk.disk_usage().unwrap().segments >= 6);
        let mut cursor = crate::live::ReplayCursor::default();
        let mut state = crate::SceneState::default();
        for frame in (0..6).chain((0..6).rev()).chain([3, 0, 5, 1, 4, 2]) {
            cursor
                .seek(&disk, frame, || false)
                .unwrap()
                .update
                .apply(&mut state);
            check_state(&state, frame + 2);
            assert_eq!(state, memory.state_at(frame).unwrap());
        }
        drop(cursor);
        drop(state);
        drop(disk);
        assert!(std::fs::read_dir(&directory).unwrap().next().is_none());
        std::fs::remove_dir(&directory).unwrap();
    }
}
