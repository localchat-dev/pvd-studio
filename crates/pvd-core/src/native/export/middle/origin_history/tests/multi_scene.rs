use super::*;

struct MultiSceneFixture {
    bytes: Vec<u8>,
    events: Vec<Vec<u8>>,
    starts: Vec<usize>,
    ends: Vec<usize>,
}

fn multi_fixture(canonical: bool, message: bool, delayed_shift: bool) -> MultiSceneFixture {
    let mut encoded = vec![words(&[9])];
    let mut initial_counters = Vec::new();
    for (scene, rigid_static, rigid_dynamic, scale) in [(1, 10, 20, 1.0), (2, 11, 21, 10.0)] {
        for (id, class) in [(scene, 4), (rigid_static, 6), (rigid_dynamic, 7)] {
            encoded.push(words(&[1, id, class]));
        }
        encoded.extend([
            set(scene, 1, &words(&[rigid_static]), &[]),
            array(5, scene, 2, 0, Some(rigid_dynamic)),
            set(rigid_static, 7, &words(&[scene]), &[]),
            set(rigid_dynamic, 7, &words(&[scene]), &[]),
            set(
                rigid_static,
                8,
                &pose([100.0, 200.0, 300.0].map(|x| x * scale)),
                &[],
            ),
            set(
                rigid_static,
                9,
                &floats(&[90.0, 180.0, 270.0, 110.0, 220.0, 330.0].map(|x| x * scale)),
                &[],
            ),
            set(
                rigid_dynamic,
                10,
                &pose([10.0, 20.0, 30.0].map(|x| x * scale)),
                &[],
            ),
            set(
                rigid_dynamic,
                11,
                &floats(&[8.0, 16.0, 24.0, 12.0, 24.0, 36.0].map(|x| x * scale)),
                &[],
            ),
            set(
                rigid_dynamic,
                12,
                &pose([12.0, 24.0, 36.0].map(|x| x * scale)),
                &[],
            ),
        ]);
        initial_counters.push(encoded.len());
        encoded.push(set(
            rigid_dynamic,
            13,
            &words(&[if scene == 1 { 7 } else { 70 }]),
            &[],
        ));
    }
    let displacements = [
        [
            [2.0, 4.0, 0.0],
            [1.0, -2.0, 4.0],
            [4.0, 0.0, -8.0],
            [-2.0, 8.0, 2.0],
        ],
        [
            [16.0, 8.0, 4.0],
            [-4.0, 16.0, -2.0],
            [8.0, -4.0, 16.0],
            [-16.0, 2.0, 8.0],
        ],
    ];
    let pairs = [
        [[20, 12], [10, 9], [20, 10], [10, 8], [20, 11]],
        [[11, 8], [21, 11], [21, 12], [11, 9], [21, 10]],
    ];
    let mut rows = [Vec::new(), Vec::new()];
    let mut starts = Vec::new();
    let mut ends = Vec::new();
    for group in 0..4 {
        starts.push(encoded.len());
        let ticks = [100 + group as u64 * 10, 9000 + group as u64 * 100];
        let mut begins = [0u64; 2];
        for (index, begin) in begins.iter_mut().enumerate() {
            *begin = encoded.len() as u64;
            encoded.push(section(index as u32 + 1, ticks[index], true));
        }
        if group == 2 {
            for (index, id, value) in [(0, 20, 9), (1, 21, 99)] {
                let distance = (encoded.len() - initial_counters[index]) as u32;
                let mut event = set(id, 13, &words(&[value]), &[distance]);
                if message {
                    event[..4].copy_from_slice(&4u32.to_le_bytes());
                    event[8..12].copy_from_slice(&0u32.to_le_bytes());
                }
                encoded.push(event);
            }
        }
        let delayed = delayed_shift && group == 1;
        for (index, displacement) in displacements.iter().enumerate() {
            if !(delayed && index == 0) {
                encoded.push(shift(index as u32 + 1, displacement[group], &pairs[index]));
            }
        }
        let order = if group % 2 == 0 { [0, 1] } else { [1, 0] };
        for (position, index) in order.into_iter().enumerate() {
            let end = encoded.len() as u64;
            encoded.push(section(index as u32 + 1, ticks[index] + 1, false));
            rows[index].push([ticks[index], begins[index], ticks[index] + 1, end]);
            if delayed && position == 0 {
                encoded.push(shift(1, displacements[0][group], &pairs[0]));
            }
        }
        ends.push(encoded.len() - 1);
    }
    let seed = fixture_with_layout(message, canonical);
    let mut layout = Layout::parse(&seed.bytes, &mut || true).unwrap();
    let mut parse_budget = 0;
    layout.events = encoded
        .iter()
        .enumerate()
        .map(|(ordinal, raw)| {
            HistoryEvent::parse(raw, ordinal as u64, &layout.metadata, &mut parse_budget).unwrap()
        })
        .collect();
    layout.pages = layout
        .events
        .chunks(5)
        .enumerate()
        .map(|(index, events)| Page {
            first: (index * 5) as u64,
            position: ((index + 1) * 4096) as u64,
            count: events.len() as u32,
            len: encode_page(events, &mut || true).unwrap().len() as u32,
        })
        .collect();
    layout.communication.instances = [1, 2, 10, 11, 20, 21]
        .into_iter()
        .map(|id| crate::native::export::Instance {
            id,
            source: u64::from(id) * 0x111,
            flags: [0; 10],
        })
        .collect();
    layout.communication.reverse = [1, 2, 10, 11, 20, 21]
        .into_iter()
        .map(|id| (u64::from(id) * 0x111, id))
        .collect();
    layout.communication.sections = rows
        .into_iter()
        .enumerate()
        .map(|(index, rows)| Section {
            source: (index as u64 + 1) * 0x111,
            name: 11,
            rows,
        })
        .collect();
    layout.backing = &[];
    layout.gaps.clear();
    let bytes = layout.encode(false, &mut || true).unwrap();
    MultiSceneFixture {
        bytes,
        events: encoded,
        starts,
        ends,
    }
}

fn check_state(state: &crate::SceneState, global_frame: usize) {
    let group = global_frame / 2;
    assert_frame(state, group);
    let positions = [
        [984.0, 1992.0, 2996.0],
        [988.0, 1976.0, 2998.0],
        [980.0, 1980.0, 2982.0],
        [996.0, 1978.0, 2974.0],
    ];
    let dynamics = [
        [84.0, 192.0, 296.0],
        [88.0, 176.0, 298.0],
        [80.0, 180.0, 282.0],
        [96.0, 178.0, 274.0],
    ];
    let targets = [
        [104.0, 232.0, 356.0],
        [108.0, 216.0, 358.0],
        [100.0, 220.0, 342.0],
        [116.0, 218.0, 334.0],
    ];
    for (id, property, expected) in [
        (11, "GlobalPose", positions[group]),
        (21, "GlobalPose", dynamics[group]),
        (21, "KinematicTarget", targets[group]),
    ] {
        let crate::Value::Transform { position, rotation } =
            state.objects[&id].properties[property]
        else {
            panic!("transform")
        };
        assert_eq!(position, expected);
        assert_eq!(
            rotation.map(f32::to_bits),
            [0, 0x8000_0000, 0, 1.0f32.to_bits()]
        );
    }
    let static_bounds = [
        [884.0, 1792.0, 2696.0, 1084.0, 2192.0, 3296.0],
        [888.0, 1776.0, 2698.0, 1088.0, 2176.0, 3298.0],
        [880.0, 1780.0, 2682.0, 1080.0, 2180.0, 3282.0],
        [896.0, 1778.0, 2674.0, 1096.0, 2178.0, 3274.0],
    ];
    let dynamic_bounds = [
        [64.0, 152.0, 236.0, 104.0, 232.0, 356.0],
        [68.0, 136.0, 238.0, 108.0, 216.0, 358.0],
        [60.0, 140.0, 222.0, 100.0, 220.0, 342.0],
        [76.0, 138.0, 214.0, 116.0, 218.0, 334.0],
    ];
    assert_eq!(
        state.objects[&11].properties["WorldBounds"],
        value_array(&static_bounds[group])
    );
    assert_eq!(
        state.objects[&21].properties["WorldBounds"],
        value_array(&dynamic_bounds[group])
    );
    assert_eq!(
        state.objects[&21].properties["Counter"],
        crate::Value::U64(if group < 2 { 70 } else { 99 })
    );
    let scene_shifts = [
        [
            [2.0, 4.0, 0.0],
            [1.0, -2.0, 4.0],
            [4.0, 0.0, -8.0],
            [-2.0, 8.0, 2.0],
        ],
        [
            [16.0, 8.0, 4.0],
            [-4.0, 16.0, -2.0],
            [8.0, -4.0, 16.0],
            [-16.0, 2.0, 8.0],
        ],
    ];
    for (index, values) in scene_shifts.iter().enumerate() {
        assert_eq!(
            state.objects[&(index as u64 + 1)].properties["$origin_shift"],
            crate::Value::Vec3(values[group])
        );
    }
}

fn check_initial_bodies(layout: &Layout<'_>, first: usize) {
    let index = first / 2 - 1;
    let positions = [
        [[8.0, 16.0, 30.0], [7.0, 18.0, 26.0], [3.0, 18.0, 34.0]],
        [
            [84.0, 192.0, 296.0],
            [88.0, 176.0, 298.0],
            [80.0, 180.0, 282.0],
        ],
    ];
    let bounds = [
        [
            [6.0, 12.0, 24.0, 10.0, 20.0, 36.0],
            [5.0, 14.0, 20.0, 9.0, 22.0, 32.0],
            [1.0, 14.0, 28.0, 5.0, 22.0, 40.0],
        ],
        [
            [64.0, 152.0, 236.0, 104.0, 232.0, 356.0],
            [68.0, 136.0, 238.0, 108.0, 216.0, 358.0],
            [60.0, 140.0, 222.0, 100.0, 220.0, 342.0],
        ],
    ];
    let targets = [
        [[10.0, 20.0, 36.0], [9.0, 22.0, 32.0], [5.0, 22.0, 40.0]],
        [
            [104.0, 232.0, 356.0],
            [108.0, 216.0, 358.0],
            [100.0, 220.0, 342.0],
        ],
    ];
    let counters = [[7, 7, 9], [70, 70, 99]];
    let snapshot = &layout.snapshot_bytes[0];
    assert_eq!(snapshot.len(), 4 + 6 * 12);
    assert_eq!(&snapshot[..4], &6u32.to_le_bytes());
    for (scene, position_values) in positions.iter().enumerate() {
        let id = 20u32 + scene as u32;
        let entry = snapshot[4..]
            .chunks_exact(12)
            .find(|entry| entry[..4] == id.to_le_bytes())
            .unwrap();
        let position = u64::from_le_bytes(entry[4..].try_into().unwrap());
        let record = layout
            .saved
            .iter()
            .find(|record| record.position == position)
            .unwrap();
        assert_eq!((record.id, record.class, record.deleted), (id, 7, false));
        let mut expected = words(&[88, scene as u32 + 1]);
        expected.extend(pose(position_values[index]));
        expected.extend(floats(&bounds[scene][index]));
        expected.extend(pose(targets[scene][index]));
        expected.extend(words(&[counters[scene][index]]));
        assert_eq!(record.body.as_ref(), expected);
    }
}

#[test]
fn multi_scene_middle_preserves_each_rigid_baseline_clock_and_shift_bytes() {
    for canonical in [false, true] {
        for message in [false, true] {
            let fixture = multi_fixture(canonical, message, false);
            let (source, _) =
                crate::native::read_with_progress(&fixture.bytes, |_, _| true).unwrap();
            assert_eq!(source.frame_count(), 8);
            for frame in 0..8 {
                check_state(&source.state_at(frame).unwrap(), frame);
            }
            for (first, last) in [(0, 3), (0, 5), (2, 3), (2, 5), (2, 7), (4, 7), (6, 7)] {
                let mut bytes = Vec::new();
                let report =
                    trim_scene_frames(&fixture.bytes, first, last, &mut bytes, || true).unwrap();
                assert_eq!((report.source_frames, report.frames), (8, last - first + 1));
                let layout = Layout::parse(&bytes, &mut || true).unwrap();
                if first != 0 {
                    check_initial_bodies(&layout, first);
                }
                let raw = raw_events(&layout);
                let start = if first == 0 {
                    0
                } else {
                    fixture.starts[first / 2]
                };
                let end = fixture.ends[last / 2];
                let leading = usize::from(first != 0);
                for (source_ordinal, event) in
                    fixture.events.iter().enumerate().take(end + 1).skip(start)
                {
                    if event[..4] == 11u32.to_le_bytes() {
                        assert_eq!(raw[source_ordinal - start + leading], event);
                    }
                }
                for (index, section) in layout.communication.sections.iter().enumerate() {
                    assert_eq!(section.source, (index as u64 + 1) * 0x111);
                    assert_eq!(section.rows.len(), (last - first).div_ceil(2));
                    for (offset, row) in section.rows.iter().enumerate() {
                        let group = first / 2 + offset;
                        let tick = if index == 0 {
                            100 + group as u64 * 10
                        } else {
                            9000 + group as u64 * 100
                        };
                        assert_eq!((row[0], row[2]), (tick, tick + 1));
                    }
                }
                let (recording, _) =
                    crate::native::read_with_progress(&bytes, |_, _| true).unwrap();
                for frame in (0..recording.frame_count())
                    .chain((0..recording.frame_count()).rev())
                    .chain([0, last - first, 0])
                {
                    let state = recording.state_at(frame).unwrap();
                    check_state(&state, first + frame);
                    assert_eq!(state, source.state_at(first + frame).unwrap());
                }
                if first == 0 && last == 5 {
                    let mut retrimmed = Vec::new();
                    trim_scene_frames(&bytes, 2, 3, &mut retrimmed, || true).unwrap();
                    let (recording, _) =
                        crate::native::read_with_progress(&retrimmed, |_, _| true).unwrap();
                    for frame in 0..2 {
                        check_state(&recording.state_at(frame).unwrap(), frame + 2);
                    }
                }
            }
        }
    }
}

#[test]
fn multi_scene_middle_requires_each_preceding_shift_before_first_observation() {
    let fixture = multi_fixture(true, false, true);
    let mut output = Vec::new();
    let failure = trim_scene_frames(&fixture.bytes, 2, 5, &mut output, || true).unwrap_err();
    assert!(
        failure
            .to_string()
            .contains("first frame must contain a captured Scene shift"),
        "{failure}"
    );
    assert!(output.is_empty());
    let fixture = multi_fixture(true, false, false);
    for (first, last) in [(1, 3), (2, 4)] {
        assert!(trim_scene_frames(&fixture.bytes, first, last, &mut output, || true).is_err());
        assert!(output.is_empty());
    }
}

fn prove(
    layout: &Layout<'_>,
    fixture: &MultiSceneFixture,
    scope: &mut Scope,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<Proof, Error> {
    scope.prove_range(
        layout,
        Range {
            start: fixture.starts[1],
            end: fixture.ends[2] + 1,
            first_end: fixture.ends[1] - 1,
            leading: 1,
        },
        proceed,
    )
}

#[test]
fn multi_scene_origin_proof_rejects_unknown_ambiguous_sources_budget_and_cancel() {
    let fixture = multi_fixture(true, false, false);
    for mode in 0..4 {
        let mut layout = Layout::parse(&fixture.bytes, &mut || true).unwrap();
        match mode {
            0 => layout
                .communication
                .instances
                .push(crate::native::export::Instance {
                    id: 99,
                    source: 0x222,
                    flags: [0; 10],
                }),
            1 => layout
                .communication
                .instances
                .retain(|instance| instance.id != 2),
            2 => {
                layout.communication.sections.pop();
            }
            _ => layout.communication.sections[1].source = 0x111,
        }
        let failure = prove(
            &layout,
            &fixture,
            &mut Scope { work: WORK_LIMIT },
            &mut || true,
        )
        .unwrap_err();
        assert!(failure.to_string().contains("Scene"), "{failure}");
    }
    let layout = Layout::parse(&fixture.bytes, &mut || true).unwrap();
    let failure = prove(&layout, &fixture, &mut Scope { work: 0 }, &mut || true).unwrap_err();
    assert!(failure.to_string().contains("work budget"));
    let mut calls = 0;
    prove(
        &layout,
        &fixture,
        &mut Scope { work: WORK_LIMIT },
        &mut || {
            calls += 1;
            true
        },
    )
    .unwrap();
    for stop in [1, calls / 2, calls] {
        let mut at = 0;
        let failure = prove(
            &layout,
            &fixture,
            &mut Scope { work: WORK_LIMIT },
            &mut || {
                at += 1;
                at != stop
            },
        )
        .unwrap_err();
        assert_eq!(
            failure.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
    }
}

#[test]
fn multi_scene_origin_middle_keeps_unverified_nonempty_scopes_closed() {
    let fixture = multi_fixture(true, false, false);
    let mut middle = Vec::new();
    trim_scene_frames(&fixture.bytes, 2, 7, &mut middle, || true).unwrap();
    for (first, last) in [(0, 3), (2, 3)] {
        let mut output = Vec::new();
        assert!(trim_scene_frames(&middle, first, last, &mut output, || true).is_err());
        assert!(output.is_empty());
    }
}

#[test]
fn multi_scene_origin_rejects_cross_scene_rows_membership_and_unknown_layout() {
    let fixture = multi_fixture(true, false, false);
    for mode in 0..3 {
        let bad = if mode == 0 {
            shift(
                2,
                [16.0, 8.0, 4.0],
                &[[10, 8], [21, 11], [21, 12], [11, 9], [21, 10]],
            )
        } else {
            set(21, 7, &words(&[1]), &[])
        };
        let mut layout = Layout::parse(&fixture.bytes, &mut || true).unwrap();
        if mode < 2 {
            let ordinal = layout
                .events
                .iter()
                .position(|event| match event {
                    HistoryEvent::OriginShift(event) if mode == 0 => event.scene() == 2,
                    HistoryEvent::Set {
                        kind: 3,
                        id: 21,
                        property: 7,
                        ..
                    } if mode == 1 => true,
                    _ => false,
                })
                .unwrap();
            layout.events[ordinal] =
                HistoryEvent::parse(&bad, ordinal as u64, &layout.metadata, &mut 0).unwrap();
        } else {
            layout.metadata.properties[12].as_mut().unwrap().offset32 = 80;
        }
        let failure = Scope::prepare(&layout, layout.events.len(), &mut || true)
            .err()
            .expect("unverified OriginShift scope");
        assert!(failure.to_string().contains("OriginShift"), "{failure}");
    }
}

#[test]
fn multi_scene_middle_disk_cursor_restores_both_scenes_forward_reverse_and_random() {
    let fixture = multi_fixture(true, true, false);
    let mut bytes = Vec::new();
    trim_scene_frames(&fixture.bytes, 2, 7, &mut bytes, || true).unwrap();
    let directory = std::env::temp_dir().join(format!(
        "pvd-native-origin-multi-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let history = crate::live::HistoryConfig {
        directory: directory.clone(),
        disk_budget_bytes: 32 << 20,
        segment_target_bytes: 1 << 20,
        segment_frames: 1,
        min_free_disk_bytes: 0,
    };
    let (recording, _) = crate::native::import::read_source_with_progress(
        std::io::Cursor::new(&bytes),
        Some(history),
        |_, _| true,
    )
    .unwrap();
    assert!(recording.is_disk_backed());
    assert!(recording.disk_usage().unwrap().segments >= 6);
    let mut cursor = crate::live::ReplayCursor::default();
    let mut state = crate::SceneState::default();
    for frame in (0..6).chain((0..6).rev()).chain([3, 0, 5, 1, 4, 2]) {
        cursor
            .seek(&recording, frame, || false)
            .unwrap()
            .update
            .apply(&mut state);
        check_state(&state, frame + 2);
    }
    drop(cursor);
    drop(recording);
    assert!(!directory.exists() || std::fs::read_dir(&directory).unwrap().next().is_none());
    if directory.exists() {
        std::fs::remove_dir(directory).unwrap();
    }
}
