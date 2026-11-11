use super::*;

mod fixture;
use fixture::{
    A, B, C, CAMERA_CLASS, CAMERA_MESSAGE, D, FUTURE, GAP, LATE, MAIN, camera_body, empty_source,
    fixture as camera_fixture, later_profile_source, single_source, source, vectors,
};

fn value(v: [f32; 9]) -> crate::SourceCamera {
    crate::SourceCamera {
        position: v[..3].try_into().unwrap(),
        up: v[3..6].try_into().unwrap(),
        target: v[6..].try_into().unwrap(),
    }
}

fn analysis_catalog(bytes: &[u8]) -> &[u8] {
    let word = |at| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    let start = 16 + word(8);
    let end = start + word(12);
    let mut reader = Reader::new(&bytes[start..end]);
    assert_eq!(reader.u32().unwrap(), 1);
    reader.take(16).unwrap();
    for width in [26, 12, 8] {
        let count = reader.count().unwrap();
        reader.take(count * width).unwrap();
    }
    assert_eq!(reader.u32().unwrap(), 1);
    reader.take(12).unwrap();
    let rows = reader.count().unwrap();
    reader.take(rows * 32).unwrap();
    &bytes[start + reader.pos..end]
}

fn checked(source: &[u8], first: usize, last: usize) -> Vec<u8> {
    let bytes = trim(source, first, last);
    let original = Recording::from_bytes(source).unwrap();
    let output = Recording::from_bytes(&bytes).unwrap();
    for frame in (0..output.frame_count()).chain((0..output.frame_count()).rev()) {
        assert_eq!(
            output.state_at(frame).unwrap().cameras,
            original.state_at(first + frame).unwrap().cameras
        );
    }
    bytes
}

#[test]
fn camera_multizone_initial_middle_preserves_raw_baseline_seed_undo_and_occurrences() {
    let source = source();
    let full = checked(&source, 0, 2);
    let full_raw = raw_with_cameras(&full, &[(LATE, 21), (MAIN, 20)]);
    assert_eq!(
        full_raw.initial,
        raw_with_cameras(&source, &[(FUTURE, 22), (LATE, 21), (MAIN, 20)]).initial,
    );
    let mut expected = camera_fixture().events[..41].to_vec();
    expected[25] = saved(2, full_raw.deleted[&2].0);
    expected[38] = saved(21, full_raw.deleted[&21].0);
    assert_eq!(full_raw.events, expected);
    assert_eq!(analysis_catalog(&full), capture::catalogs());
    let middle = checked(&source, 1, 2);
    assert_eq!(checked(&full, 1, 2), middle);
    assert_eq!(checked(&middle, 0, 0), checked(&source, 1, 1));
    let last = checked(&source, 2, 2);
    assert_eq!(checked(&middle, 1, 1), last);
    assert_eq!(checked(&last, 0, 0), last);
    let output = raw_with_cameras(&middle, &[(LATE, 21), (MAIN, 20)]);
    assert_eq!(analysis_catalog(&middle), capture::catalogs());
    assert_eq!(output.rows, [[200, 3, 201, 19], [300, 20, 301, 29]]);
    assert_eq!(output.initial[&20], (CAMERA_CLASS, camera_body(MAIN, &GAP)));
    assert!(!output.initial.contains_key(&21));
    assert_eq!(output.events[1], set(3, 7, 11, &capture::LEFT_18_30, &[1]));
    assert_eq!(
        output.events[2],
        set(3, 10, 11, &capture::RIGHT_19_35, &[2])
    );
    assert_eq!(
        output.events[11],
        set(4, 20, CAMERA_MESSAGE, &vectors(&C), &[11])
    );
    assert_eq!(output.events[14], saved(2, output.deleted[&2].0));
    assert_eq!(output.events[27], saved(21, output.deleted[&21].0));
    assert_eq!(output.deleted[&21].2, camera_body(LATE, &D));
    assert_eq!(
        output.events[26],
        set(4, 20, CAMERA_MESSAGE, &vectors(&D), &[15])
    );
    let layout = Layout::parse(&middle, &mut || true).unwrap();
    assert_eq!(layout.communication.cameras, [(LATE, 21), (MAIN, 20)]);
    let original = Recording::from_bytes(&source).unwrap();
    for (frame, main, late) in [(0, B, false), (1, C, true), (2, D, false)] {
        let mut expected = BTreeMap::from([("Main".into(), value(main))]);
        if late {
            expected.insert("Late".into(), value(D));
        }
        assert_eq!(original.state_at(frame).unwrap().cameras, expected);
    }
    let recording = Recording::from_bytes(&middle).unwrap();
    assert_eq!(
        spans(&recording),
        [
            (10, 19, 35, 85, 0),
            (7, 18, 30, 80, 0),
            (7, 17, 50, 140, 1),
            (10, 17, 55, 150, 1),
        ]
    );
    assert_eq!(
        recording
            .untimed_error_events()
            .iter()
            .map(|e| (
                e.container_instance,
                e.index,
                e.history_event,
                e.received_frame
            ))
            .collect::<Vec<_>>(),
        [(3, 9, 12, 0), (4, 51, 13, 0)]
    );
    assert_eq!(
        recording.memory_history_scope(),
        MemoryHistoryScope::AfterInitialSnapshot
    );
    assert_eq!(
        recording
            .native_initial_memory_totals()
            .unwrap()
            .unwrap()
            .row(0)
            .unwrap()
            .bytes,
        72
    );
    let final_raw = raw_with_cameras(&last, &[(LATE, 21), (MAIN, 20)]);
    assert_eq!(final_raw.rows, [[300, 3, 301, 12]]);
    assert_eq!(
        final_raw.initial[&20],
        (CAMERA_CLASS, camera_body(MAIN, &C))
    );
    assert_eq!(
        final_raw.initial[&21],
        (CAMERA_CLASS, camera_body(LATE, &D))
    );
    assert_eq!(
        final_raw.events[1],
        set(3, 7, 11, &capture::LEFT_17_50, &[1])
    );
    assert_eq!(
        final_raw.events[2],
        set(3, 10, 11, &capture::RIGHT_17_55, &[2])
    );
    assert_eq!(
        final_raw.events[9],
        set(4, 20, CAMERA_MESSAGE, &vectors(&D), &[9])
    );
}

#[test]
fn camera_multizone_empty_source_and_derived_retrims_keep_independent_starts() {
    let source = empty_source();
    let middle = checked(&source, 1, 2);
    let output = raw_with_cameras(&middle, &[(MAIN, 20)]);
    assert_eq!(output.rows, [[200, 3, 201, 9], [300, 10, 301, 16]]);
    assert_eq!(output.events[1], set(3, 7, 11, &capture::LEFT_17_50, &[1]));
    assert_eq!(
        output.events[2],
        set(3, 10, 11, &capture::RIGHT_17_55, &[2])
    );
    assert_eq!(
        output.events[4],
        set(4, 20, CAMERA_MESSAGE, &vectors(&B), &[4])
    );
    assert_eq!(output.initial[&20], (CAMERA_CLASS, camera_body(MAIN, &A)));
    assert_eq!(checked(&middle, 0, 0), checked(&source, 1, 1));
    let last = checked(&source, 2, 2);
    assert_eq!(checked(&middle, 1, 1), last);
    assert_eq!(checked(&last, 0, 0), last);
    let recording = Recording::from_bytes(&last).unwrap();
    assert_eq!(
        spans(&recording),
        [(7, 17, 160, 200, 0), (10, 17, 170, 210, 0)]
    );
    assert_eq!(
        recording.state_at(0).unwrap().cameras,
        BTreeMap::from([("Main".into(), value(C))])
    );
}

#[test]
fn camera_initial_analysis_single_zone_and_later_created_prefix_remain_separate() {
    let source = single_source();
    let middle = checked(&source, 1, 2);
    let output = raw_with_cameras(&middle, &[(MAIN, 20)]);
    assert_eq!(output.rows, [[200, 2, 201, 6], [300, 7, 301, 11]]);
    assert_eq!(output.events[1], set(3, 7, 11, &capture::LEFT_17_50, &[1]));
    assert_eq!(
        output.events[3],
        set(4, 20, CAMERA_MESSAGE, &vectors(&B), &[3])
    );
    assert_eq!(checked(&middle, 0, 0), checked(&source, 1, 1));
    let last = checked(&source, 2, 2);
    assert_eq!(checked(&middle, 1, 1), last);
    assert_eq!(checked(&last, 0, 0), last);
    assert_eq!(
        spans(&Recording::from_bytes(&last).unwrap()),
        [(7, 17, 160, 200, 0)]
    );
    let source = later_profile_source();
    let original = raw_with_cameras(&source, &[(MAIN, 20)]);
    for last in [0, 1] {
        let bytes = checked(&source, 0, last);
        let output = raw_with_cameras(&bytes, &[(MAIN, 20)]);
        assert_eq!(output.initial, original.initial);
        assert_eq!(
            output.events,
            original.events[..if last == 0 { 3 } else { 14 }]
        );
        let recording = Recording::from_bytes(&bytes).unwrap();
        assert_eq!(
            recording.state_at(last).unwrap().cameras,
            BTreeMap::from([("Main".into(), value(if last == 0 { A } else { B }))])
        );
        let expected = if last == 0 {
            vec![]
        } else {
            vec![(7, 17, 10, 40, 1), (10, 17, 12, 45, 1)]
        };
        assert_eq!(spans(&recording), expected);
    }
}

#[test]
fn camera_multizone_disk_cursor_crosses_segments_in_both_directions() {
    let source = source();
    for bytes in [&source, &checked(&source, 1, 2)] {
        let memory = Recording::from_bytes(bytes).unwrap();
        let mut config = crate::native::container_tests::file_history_config("camera-analysis");
        config.segment_frames = 1;
        let directory = config.directory.clone();
        let temporary_root = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let expected_directory = temporary_root.join(directory.file_name().unwrap());
        assert_eq!(
            directory.parent().unwrap().canonicalize().unwrap(),
            temporary_root
        );
        assert!(!directory.exists());
        let (disk, _) = crate::native::import::read_source_with_progress(
            std::io::Cursor::new(bytes),
            Some(config),
            |_, _| true,
        )
        .unwrap();
        assert!(disk.is_disk_backed());
        assert!(disk.disk_usage().unwrap().segments >= memory.frame_count());
        let mut cursor = crate::live::ReplayCursor::default();
        let mut state = crate::SceneState::default();
        for frame in (0..memory.frame_count()).chain((0..memory.frame_count()).rev()) {
            cursor
                .seek(&disk, frame, || false)
                .unwrap()
                .update
                .apply(&mut state);
            assert_eq!(state, memory.state_at(frame).unwrap());
            assert_eq!(state, disk.state_at(frame).unwrap());
        }
        drop(cursor);
        drop(state);
        drop(disk);
        let resolved = directory.canonicalize().unwrap();
        assert_eq!(resolved, expected_directory);
        assert_eq!(resolved.parent(), Some(temporary_root.as_path()));
        assert_ne!(resolved, temporary_root);
        std::fs::remove_dir_all(&resolved).unwrap();
    }
}

#[test]
fn camera_multizone_independent_proofs_reject_hidden_baseline_and_cross_seed_undo() {
    let source = source();
    let source_layout = Layout::parse(&source, &mut || true).unwrap();
    let camera = camera::Scope::prepare(&source_layout, 2, 2, &mut || true)
        .unwrap()
        .unwrap();
    let permit = camera.permit(&source_layout).unwrap();
    let initial = initial::State::read(&source_layout, &mut || true).unwrap();
    let mut allocated = permit.bytes();
    let scope = initial::analysis::Scope::read_with_cameras(
        &initial,
        &source_layout,
        2,
        2,
        Some(&permit),
        &mut allocated,
        &mut || true,
    )
    .unwrap()
    .unwrap();
    let seeds = [
        history::Seed {
            id: 7,
            property: 11,
            bytes: capture::LEFT_17_50.to_vec(),
        },
        history::Seed {
            id: 10,
            property: 11,
            bytes: capture::RIGHT_17_55.to_vec(),
        },
    ];
    let proof = scope.prove(&seeds, &mut allocated, &mut || true).unwrap();
    let bytes = checked(&source, 2, 2);
    let good = Layout::parse(&bytes, &mut || true).unwrap();
    proof.verify_layout(&good, &mut || true).unwrap();
    camera.verify_layout(&good, 3, &mut || true).unwrap();
    for seed in [1, 2] {
        let mut changed = Layout::parse(&bytes, &mut || true).unwrap();
        let HistoryEvent::Set { undo, .. } = &mut changed.events[9] else {
            panic!()
        };
        *undo = vec![9 - seed];
        assert!(
            camera
                .verify_with_permit(&changed, 3, &mut || true)
                .is_err()
        );
    }
    for (ordinal, wrong_target) in [(5, 2), (6, 1)] {
        let mut changed = Layout::parse(&bytes, &mut || true).unwrap();
        let HistoryEvent::Set { undo, .. } = &mut changed.events[ordinal] else {
            panic!()
        };
        *undo = vec![ordinal as u32 - wrong_target];
        assert!(proof.verify_layout(&changed, &mut || true).is_err());
    }
    for variant in 0..3 {
        let mut changed = Layout::parse(&bytes, &mut || true).unwrap();
        match variant {
            0 => {
                changed.events.swap(1, 2);
                for ordinal in 1..=2 {
                    let HistoryEvent::Set { undo, .. } = &mut changed.events[ordinal] else {
                        panic!()
                    };
                    *undo = vec![ordinal as u32];
                }
            }
            1 => {
                let HistoryEvent::Set { data, .. } = &mut changed.events[2] else {
                    panic!()
                };
                *data = capture::LEFT_17_50.to_vec().into();
            }
            _ => changed.events.insert(2, changed.events[1].clone()),
        }
        assert!(proof.verify_layout(&changed, &mut || true).is_err());
    }
    let mut changed = Layout::parse(&bytes, &mut || true).unwrap();
    let record = changed
        .saved
        .iter_mut()
        .find(|r| r.id == 20 && !r.deleted)
        .unwrap();
    record.body.to_mut()[8..12].copy_from_slice(&999f32.to_le_bytes());
    let corrupt = changed.encode(false, &mut || true).unwrap();
    assert_eq!(
        Recording::from_bytes(&corrupt)
            .unwrap()
            .state_at(0)
            .unwrap(),
        Recording::from_bytes(&bytes).unwrap().state_at(0).unwrap()
    );
    assert!(
        camera
            .verify_with_permit(&changed, 3, &mut || true)
            .is_err()
    );
}

#[test]
fn camera_multizone_permits_bind_source_output_range_and_shared_budget() {
    let source = source();
    let input = Layout::parse(&source, &mut || true).unwrap();
    let camera = camera::Scope::prepare(&input, 0, 1, &mut || true)
        .unwrap()
        .unwrap();
    let source_permit = camera.permit(&input).unwrap();
    let output = checked(&source, 0, 1);
    let target = Layout::parse(&output, &mut || true).unwrap();
    let target_permit = camera.verify_with_permit(&target, 0, &mut || true).unwrap();
    let initial = initial::State::read(&target, &mut || true).unwrap();
    assert!(!source_permit.prefix(&target, 31));
    assert!(!target_permit.prefix(&target, 13));
    assert!(camera.permit(&target).is_err());
    let same_bytes = Layout::parse(&output, &mut || true).unwrap();
    assert!(!target_permit.prefix(&same_bytes, 31));
    assert!(
        initial
            .prefix_analysis_with_cameras(&target, 31, Some(&source_permit), &mut || true)
            .is_err()
    );
    initial
        .prefix_analysis_with_cameras(&target, 31, Some(&target_permit), &mut || true)
        .unwrap();
    let source_proof =
        initial::Proof::prepare_with_cameras(&input, 31, Some(&source_permit), &mut || true)
            .unwrap();
    source_proof
        .verify_with_cameras(&target, Some(&target_permit), &mut || true)
        .unwrap();
    assert!(
        source_proof
            .verify_with_cameras(&target, Some(&source_permit), &mut || true)
            .is_err()
    );
    for (layout, permit) in [(&input, &source_permit), (&target, &target_permit)] {
        let initial = initial::State::read(layout, &mut || true).unwrap();
        let mut allocated = 4096;
        initial
            .prefix_analysis_bounded(layout, 31, Some(permit), &mut allocated, &mut || true)
            .unwrap();
        let cost = allocated - 4096;
        assert!(cost > permit.bytes());
        let mut exact = MAX_METADATA - cost;
        assert!(exact > 0);
        initial
            .prefix_analysis_bounded(layout, 31, Some(permit), &mut exact, &mut || true)
            .unwrap();
        assert_eq!(exact, MAX_METADATA);
        assert!(
            initial
                .prefix_analysis_bounded(
                    layout,
                    31,
                    Some(permit),
                    &mut (MAX_METADATA - cost + 1),
                    &mut || true
                )
                .is_err()
        );
    }
    let camera = camera::Scope::prepare(&input, 2, 2, &mut || true)
        .unwrap()
        .unwrap();
    let permit = camera.permit(&input).unwrap();
    let initial = initial::State::read(&input, &mut || true).unwrap();
    let mut allocated = permit.bytes() + 4096;
    let start = allocated;
    initial::analysis::Scope::read_with_cameras(
        &initial,
        &input,
        2,
        2,
        Some(&permit),
        &mut allocated,
        &mut || true,
    )
    .unwrap();
    let cost = allocated - start;
    let mut exact = MAX_METADATA - cost;
    initial::analysis::Scope::read_with_cameras(
        &initial,
        &input,
        2,
        2,
        Some(&permit),
        &mut exact,
        &mut || true,
    )
    .unwrap();
    assert_eq!(exact, MAX_METADATA);
    assert!(
        initial::analysis::Scope::read_with_cameras(
            &initial,
            &input,
            2,
            2,
            Some(&permit),
            &mut (MAX_METADATA - cost + 1),
            &mut || true
        )
        .is_err()
    );
    assert!(
        initial::analysis::Scope::read_with_cameras(
            &initial,
            &input,
            2,
            2,
            Some(&permit),
            &mut 0,
            &mut || true
        )
        .is_err()
    );
}

#[test]
fn camera_multizone_target_indices_include_owned_proof_maps_and_camera_budget() {
    let source = source();
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    let camera = camera::Scope::prepare(&layout, 2, 2, &mut || true)
        .unwrap()
        .unwrap();
    let history = layout
        .trim_middle_with_cameras(2, 2, Some(&camera), &mut || true)
        .unwrap();
    let mut proof = history.initial.unwrap();
    let bytes = layout.encode(false, &mut || true).unwrap();
    let target = Layout::parse(&bytes, &mut || true).unwrap();
    proof.verify_layout(&target, &mut || true).unwrap();
    let state = initial::State::read(&target, &mut || true).unwrap();
    let indices = state.entries.len() * (std::mem::size_of::<initial::Entry>() + 64)
        + target.saved.len() * 64;
    drop(state);
    assert!(proof.owned_bytes() >= std::mem::size_of::<Proof>() + 3 * 96);
    proof.camera_bytes = MAX_METADATA - proof.owned_bytes() - indices;
    assert!(proof.camera_bytes > camera.bytes());
    proof.verify_layout(&target, &mut || true).unwrap();
    proof.camera_bytes += 1;
    assert!(proof.verify_layout(&target, &mut || true).is_err());
}

#[test]
fn camera_multizone_guards_remain_strict_and_cancel_before_publication() {
    let source = source();
    for variant in 0..7 {
        let mut layout = Layout::parse(&source, &mut || true).unwrap();
        match variant {
            0 => layout.events[1] = HistoryEvent::Marker,
            1 => {
                let record = layout.saved.iter_mut().find(|r| r.id == 10).unwrap();
                record.body = sample::buffer(&capture::RIGHT_17_12).into();
            }
            2 => layout.communication.zones[0].source ^= 1,
            3 => layout.events[36] = HistoryEvent::Destroy(10),
            4 => layout.events[36] = HistoryEvent::Create(30, 11),
            5 => {
                let HistoryEvent::Set { data, .. } = &mut layout.events[20] else {
                    panic!()
                };
                *data = capture::RIGHT_17_55.to_vec().into();
            }
            _ => {
                let HistoryEvent::Set { undo, .. } = &mut layout.events[22] else {
                    panic!()
                };
                *undo = vec![20];
            }
        }
        if variant < 6 {
            let camera = camera::Scope::prepare(&layout, 2, 2, &mut || true)
                .unwrap()
                .unwrap();
            let permit = camera.permit(&layout).unwrap();
            let initial = initial::State::read(&layout, &mut || true).unwrap();
            assert!(
                initial::analysis::Scope::read_with_cameras(
                    &initial,
                    &layout,
                    2,
                    2,
                    Some(&permit),
                    &mut permit.bytes(),
                    &mut || true,
                )
                .is_err(),
                "analysis variant {variant}"
            );
        }
        assert_eq!(layout.pages.len(), 1);
        let page = &mut layout.pages[0];
        assert_eq!(page.first, 0);
        assert_eq!(page.count as usize, layout.events.len());
        page.len = u32::try_from(encode_page(&layout.events, &mut || true).unwrap().len()).unwrap();
        let bytes = layout.encode(false, &mut || true).unwrap();
        let mut output = Vec::new();
        assert!(
            trim_scene_frames(&bytes, 2, 2, &mut output, || true).is_err(),
            "variant {variant}"
        );
        assert!(output.is_empty());
    }
    for (first, last) in [(0, 1), (1, 2)] {
        let mut calls = 0;
        let mut complete = Vec::new();
        trim_scene_frames(&source, first, last, &mut complete, || {
            calls += 1;
            true
        })
        .unwrap();
        assert!(complete.len() < 64 << 10);
        for stop in [1, calls / 2, calls - 1] {
            let mut remaining = stop;
            let mut output = Vec::new();
            let error = trim_scene_frames(&source, first, last, &mut output, || {
                remaining -= 1;
                remaining != 0
            })
            .unwrap_err();
            assert_eq!(
                error.downcast_ref::<std::io::Error>().unwrap().kind(),
                std::io::ErrorKind::Interrupted
            );
            assert!(output.is_empty());
        }
    }
}

#[test]
#[ignore = "requires PVD_NATIVE_CONSTRUCTED_EXPORT_DIRECTORY"]
fn write_constructed_camera_analysis_fixtures() {
    let directory = std::path::PathBuf::from(
        std::env::var_os("PVD_NATIVE_CONSTRUCTED_EXPORT_DIRECTORY")
            .expect("fixture output directory"),
    );
    std::fs::create_dir_all(&directory).unwrap();
    for (name, source) in [
        ("initial", source()),
        ("empty", empty_source()),
        ("single", single_source()),
    ] {
        std::fs::write(
            directory.join(format!("camera-analysis-{name}-source.pxd2")),
            &source,
        )
        .unwrap();
        for (kind, first, last) in [("prefix", 0, 1), ("middle", 1, 2), ("last", 2, 2)] {
            let bytes = checked(&source, first, last);
            std::fs::write(
                directory.join(format!("camera-analysis-{name}-{kind}.pxd2")),
                bytes,
            )
            .unwrap();
        }
    }
}
