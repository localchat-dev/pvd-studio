use super::*;

fn trim(source: &[u8], first: usize, last: usize) -> Vec<u8> {
    let mut output = Vec::new();
    trim_scene_frames(source, first, last, &mut output, || true).unwrap();
    let mut rewritten = Vec::new();
    rewrite(&output, &mut rewritten, || true).unwrap();
    assert_eq!(rewritten, output);
    output
}

fn keep(name: &str, bytes: &[u8]) {
    if let Some(directory) = std::env::var_os("PVD_NATIVE_CONSTRUCTED_EXPORT_DIRECTORY") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join(format!("camera-middle-{name}.pxd2")), bytes).unwrap();
    }
}

fn initial_fixed(layout: &Layout<'_>, id: u32) -> Option<Vec<u8>> {
    let mut directory = Reader::new(&layout.snapshot_bytes[0]);
    for _ in 0..directory.count().unwrap() {
        let found = directory.u32().unwrap();
        let position = directory.u64().unwrap();
        if found == id {
            let record = layout
                .saved
                .iter()
                .find(|record| record.position == position)
                .unwrap();
            assert_eq!((record.id, record.class, record.deleted), (id, 4, false));
            let mut body = Reader::new(&record.body);
            let fixed = body.blob().unwrap().to_vec();
            body.end().unwrap();
            assert_eq!(fixed.len(), 40);
            return Some(fixed);
        }
    }
    None
}

fn raw_camera(handle: u32, value: SourceCamera) -> Vec<u8> {
    let mut bytes = handle.to_le_bytes().to_vec();
    bytes.extend(vectors(value));
    bytes
}

fn assert_slice(bytes: &[u8], source: &Recording, first: usize, last: usize) -> Recording {
    let recording = read_with_progress(bytes, |_, _| true).unwrap().0;
    let count = last - first + 1;
    assert_eq!(recording.frame_count(), count);
    for frame in (0..count)
        .chain((0..count).rev())
        .chain([count - 1, 0, count / 2])
    {
        let state = recording.state_at(frame).unwrap();
        assert_eq!(state.cameras, expected(first + frame));
        let original = source.state_at(first + frame).unwrap();
        assert_eq!(state.objects, original.objects);
        assert_eq!(state.cameras, original.cameras);
        assert_eq!(state.errors, original.errors);
        assert_eq!(state.debug_draw, original.debug_draw);
        assert_eq!(state.contacts, original.contacts);
        assert_eq!(recording.frames[frame].timestamp, ROWS[first + frame][2]);
    }
    recording.verify_replay().unwrap();
    disk_cursor(bytes, &recording);
    recording
}

fn disk_cursor(bytes: &[u8], memory: &Recording) {
    let mut config = crate::native::container_tests::file_history_config("camera-middle");
    config.segment_frames = 2;
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
    let count = memory.frame_count();
    assert!(disk.is_disk_backed());
    assert_eq!(disk.disk_usage().unwrap().retained_frames, count);
    assert!(disk.disk_usage().unwrap().segments >= count.div_ceil(2));
    let mut cursor = crate::live::ReplayCursor::default();
    let mut state = crate::SceneState::default();
    for frame in (0..count)
        .chain((0..count).rev())
        .chain([count - 1, 0, count / 2])
    {
        cursor
            .seek(&disk, frame, || false)
            .unwrap()
            .update
            .apply(&mut state);
        assert_eq!(
            state,
            memory.state_at(frame).unwrap(),
            "disk cursor frame {frame}"
        );
        assert_eq!(state, disk.state_at(frame).unwrap());
    }
    disk.verify_replay().unwrap();
    drop(state);
    drop(cursor);
    drop(disk);
    let resolved = directory.canonicalize().unwrap();
    assert_eq!(resolved, expected_directory);
    assert_eq!(resolved.parent(), Some(temporary_root.as_path()));
    assert_ne!(resolved, temporary_root);
    std::fs::remove_dir_all(&resolved).unwrap();
    assert!(!directory.exists());
}

#[test]
fn camera_middle_preserves_raw_baselines_directory_undo_and_retrim_chains() {
    let fixture = Fixture::new();
    let source = fixture.bytes();
    keep("source", &source);
    let original = read_with_progress(&source, |_, _| true).unwrap().0;
    assert_frames(&original, 4);
    disk_cursor(&source, &original);
    for (first, last) in [(1, 4), (2, 3), (3, 4), (4, 4)] {
        let output = trim(&source, first, last);
        keep(&format!("f{first}-l{last}"), &output);
        assert_slice(&output, &original, first, last);
        let layout = Layout::parse(&output, &mut || true).unwrap();
        let main = match first {
            1 => Some(MAIN_A),
            2 => Some(MAIN_B),
            3 => Some(MAIN_C),
            _ => None,
        };
        assert_eq!(
            initial_fixed(&layout, 7),
            main.map(|value| raw_camera(14, value))
        );
        assert_eq!(
            initial_fixed(&layout, 8),
            (first >= 3).then(|| raw_camera(15, LATE_A))
        );
        let directory: &[(u32, u32)] = if first == 4 {
            &[(15, 8)]
        } else {
            &[(14, 7), (15, 8)]
        };
        assert_eq!(layout.communication.cameras, directory);
        assert!(!layout.communication.cameras.iter().any(|row| row.1 == 9));
        if first == 2 {
            let HistoryEvent::Set { undo, .. } = &layout.events[2] else {
                panic!()
            };
            assert_eq!(undo, &[2]);
            let HistoryEvent::Saved { id, position } = layout.events[8] else {
                panic!()
            };
            assert_eq!(id, 7);
            let saved = layout
                .saved
                .iter()
                .find(|record| record.position == position)
                .unwrap();
            assert!(saved.deleted);
            assert_eq!(&saved.body[4..], raw_camera(14, MAIN_C));
        }
        let prefix = trim(&output, 0, 0);
        keep(&format!("f{first}-l{last}-prefix"), &prefix);
        assert_slice(&prefix, &original, first, first);
        assert_eq!(
            initial_fixed(&Layout::parse(&prefix, &mut || true).unwrap(), 7),
            main.map(|value| raw_camera(14, value))
        );
        if first < last {
            let middle = trim(&output, 1, last - first);
            keep(&format!("f{first}-l{last}-middle"), &middle);
            assert_slice(&middle, &original, first + 1, last);
            let final_prefix = trim(&middle, 0, 0);
            keep(&format!("f{first}-l{last}-middle-prefix"), &final_prefix);
            assert_slice(&final_prefix, &original, first + 1, first + 1);
        }
    }
    let prefix = trim(&source, 0, 3);
    let middle = trim(&prefix, 1, 2);
    keep("prefix-middle", &middle);
    assert_slice(&middle, &original, 1, 2);
}

#[test]
fn camera_middle_uses_updates_and_destruction_between_completed_frames() {
    let gap = SourceCamera {
        position: [101.0, 102.0, 103.0],
        up: [1.0, 2.0, 3.0],
        target: [4.0, 5.0, 7.0],
    };
    let mut fixture = Fixture::new();
    fixture.metadata.strings.insert(22, "main".into());
    fixture
        .events
        .insert(10, set(3, 7, 0, &22u32.to_le_bytes(), &[7]));
    fixture
        .events
        .insert(11, set(4, 7, 0, &vectors(gap), &[7, 3]));
    fixture.events[13] = set(4, 7, 0, &vectors(MAIN_C), &[2]);
    fixture.saved[0].bytes[16..20].copy_from_slice(&22u32.to_le_bytes());
    fixture.rows[2] = [300, 12, 301, 17];
    fixture.rows[3] = [400, 18, 401, 21];
    fixture.rows[4] = [500, 22, 501, 24];
    let source = fixture.bytes();
    keep("gap-updates-source", &source);
    let original = read_with_progress(&source, |_, _| true).unwrap().0;
    assert_eq!(original.state_at(1).unwrap().cameras, expected(1));
    let output = trim(&source, 2, 3);
    keep("gap-updates-f2-l3", &output);
    assert_eq!(
        initial_fixed(&Layout::parse(&output, &mut || true).unwrap(), 7),
        Some(raw_camera(22, gap))
    );
    assert_slice(&output, &original, 2, 3);

    let mut fixture = Fixture::new();
    fixture.events[16] = saved_event(7, 128);
    fixture.events[17] = raw_words(&[2, 7]);
    fixture.events[18] = section(400, true);
    fixture.rows[3][1] = 18;
    let source = fixture.bytes();
    keep("gap-destroy-source", &source);
    let original = read_with_progress(&source, |_, _| true).unwrap().0;
    let output = trim(&source, 3, 4);
    keep("gap-destroy-f3-l4", &output);
    let layout = Layout::parse(&output, &mut || true).unwrap();
    assert_eq!(layout.communication.cameras, [(15, 8)]);
    assert_eq!(initial_fixed(&layout, 7), None);
    assert_slice(&output, &original, 3, 4);
}

#[test]
fn camera_middle_keeps_zero_initial_values_empty_names_and_float_bits() {
    let zero = SourceCamera {
        position: [0.0; 3],
        up: [0.0; 3],
        target: [0.0; 3],
    };
    for directory_name in [0, 14] {
        let mut fixture = Fixture::new();
        fixture.cameras[1].0 = directory_name;
        fixture.events[3] = raw_words(&[9]);
        fixture.events[4] = raw_words(&[9]);
        fixture.events[8] = raw_words(&[9]);
        fixture.events[11] = set(4, 7, 0, &vectors(MAIN_C), &[9]);
        fixture.saved[0].bytes[16..20].fill(0);
        let source = fixture.bytes();
        let output = trim(&source, 1, 2);
        keep(&format!("zero-name-{directory_name}-source"), &source);
        keep(&format!("zero-name-{directory_name}-f1-l2"), &output);
        let layout = Layout::parse(&output, &mut || true).unwrap();
        assert_eq!(initial_fixed(&layout, 7), Some(vec![0; 40]));
        let recording = read_with_progress(&output, |_, _| true).unwrap().0;
        let first = if directory_name == 0 {
            BTreeMap::from([(String::new(), zero)])
        } else {
            BTreeMap::new()
        };
        assert_eq!(recording.state_at(0).unwrap().cameras, first);
        let mut second = BTreeMap::from([("late".into(), LATE_A)]);
        if directory_name == 0 {
            second.insert(String::new(), MAIN_C);
        }
        assert_eq!(recording.state_at(1).unwrap().cameras, second);
        recording.verify_replay().unwrap();
        disk_cursor(&output, &recording);
        let repeated = trim(&output, 1, 1);
        assert_eq!(
            read_with_progress(&repeated, |_, _| true)
                .unwrap()
                .0
                .state_at(0)
                .unwrap()
                .cameras,
            second
        );
        assert_eq!(
            initial_fixed(&Layout::parse(&repeated, &mut || true).unwrap(), 7),
            Some(vec![0; 40])
        );
    }
    let captured = SourceCamera {
        position: [f32::from_bits(0x7fc0_5678), -0.0, f32::INFINITY],
        up: [-0.0, f32::from_bits(0x7fc0_1357), 0.0],
        target: [-0.0, 0.0, 1.0],
    };
    let mut fixture = Fixture::new();
    fixture.events[4] = set(4, 7, 0, &vectors(captured), &[2]);
    let source = fixture.bytes();
    let output = trim(&source, 1, 1);
    keep("float-bits-source", &source);
    keep("float-bits-f1-l1", &output);
    assert_eq!(
        initial_fixed(&Layout::parse(&output, &mut || true).unwrap(), 7),
        Some(raw_camera(14, captured))
    );
    let recording = read_with_progress(&output, |_, _| true).unwrap().0;
    assert_eq!(
        recording.state_at(0).unwrap().cameras,
        BTreeMap::from([(
            "main".into(),
            SourceCamera {
                position: MAIN_B.position,
                ..captured
            }
        )])
    );
    recording.verify_replay().unwrap();
    disk_cursor(&output, &recording);
}

#[test]
fn camera_trim_rejects_invalid_source_undo_before_collapsing_to_the_baseline() {
    for (event, bytes, reason) in [
        (
            11,
            set(4, 7, 0, &vectors(MAIN_C), &[3, 7]),
            "ordered and distinct",
        ),
        (
            14,
            set(4, 8, 0, &vectors(LATE_A), &[3]),
            "unrelated operation",
        ),
        (
            8,
            set(3, 7, 1, &vectors(MAIN_B)[..12], &[5]),
            "another property",
        ),
        (
            11,
            set(4, 7, 0, &vectors(MAIN_C), &[9]),
            "preceding raw state",
        ),
    ] {
        let mut fixture = Fixture::new();
        fixture.events[event] = bytes;
        let source = fixture.bytes();
        assert!(read_with_progress(&source, |_, _| true).is_ok());
        rejected(&source, 0, 0, reason);
        rejected(&source, 2, 3, reason);
    }
    let mut fixture = Fixture::new();
    fixture.saved[0].bytes[20..24].copy_from_slice(&777.0f32.to_le_bytes());
    let source = fixture.bytes();
    assert!(read_with_progress(&source, |_, _| true).is_ok());
    rejected(
        &source,
        0,
        0,
        "saved body differs from its captured raw state",
    );
    rejected(
        &source,
        2,
        3,
        "saved body differs from its captured raw state",
    );
}

#[test]
fn camera_proof_rejects_a_hidden_baseline_change_and_history_tampering() {
    let source = Fixture::new().bytes();
    let source_layout = Layout::parse(&source, &mut || true).unwrap();
    let proof = camera::Scope::prepare(&source_layout, 2, 2, &mut || true)
        .unwrap()
        .unwrap();
    let output = trim(&source, 2, 2);
    let mut layout = Layout::parse(&output, &mut || true).unwrap();
    proof.verify_layout(&layout, 1, &mut || true).unwrap();
    let record = layout
        .saved
        .iter_mut()
        .find(|record| record.id == 7 && !record.deleted)
        .unwrap();
    record.body.to_mut()[8..12].copy_from_slice(&999.0f32.to_le_bytes());
    let corrupted = layout.encode(false, &mut || true).unwrap();
    assert_eq!(
        read_with_progress(&corrupted, |_, _| true)
            .unwrap()
            .0
            .state_at(0)
            .unwrap(),
        read_with_progress(&output, |_, _| true)
            .unwrap()
            .0
            .state_at(0)
            .unwrap()
    );
    assert!(
        proof
            .verify_layout(&layout, 1, &mut || true)
            .unwrap_err()
            .to_string()
            .contains("baseline")
    );

    for variant in 0..4 {
        let mut layout = Layout::parse(&output, &mut || true).unwrap();
        match variant {
            0 => layout.communication.cameras.reverse(),
            1 => {
                let HistoryEvent::Set { data, .. } = &mut layout.events[2] else {
                    panic!()
                };
                data.to_mut()[0] ^= 1;
            }
            2 => {
                let HistoryEvent::Set { undo, .. } = &mut layout.events[2] else {
                    panic!()
                };
                *undo = vec![1];
            }
            _ => {
                let HistoryEvent::Set { kind, .. } = &mut layout.events[2] else {
                    panic!()
                };
                *kind = 3;
            }
        }
        assert!(
            proof.verify_layout(&layout, 1, &mut || true).is_err(),
            "tamper {variant}"
        );
    }
    let prefix = trim(&output, 0, 0);
    let output_layout = Layout::parse(&output, &mut || true).unwrap();
    let prefix_proof = camera::Scope::prepare(&output_layout, 0, 0, &mut || true)
        .unwrap()
        .unwrap();
    let mut layout = Layout::parse(&prefix, &mut || true).unwrap();
    prefix_proof
        .verify_layout(&layout, 0, &mut || true)
        .unwrap();
    layout.communication.cameras[0].0 = 16;
    assert!(
        prefix_proof
            .verify_layout(&layout, 0, &mut || true)
            .is_err()
    );
}

#[test]
fn camera_proof_bounds_work_memory_and_cancellation_without_publishing() {
    let source = Fixture::new().bytes();
    let layout = Layout::parse(&source, &mut || true).unwrap();
    assert!(
        camera::Scope::prepare_bounded(&layout, 1, 4, 0, usize::MAX, &mut || true)
            .err()
            .unwrap()
            .to_string()
            .contains("memory budget")
    );
    assert!(
        camera::Scope::prepare_bounded(&layout, 1, 4, MAX_METADATA, 0, &mut || true)
            .err()
            .unwrap()
            .to_string()
            .contains("work budget")
    );
    let mut total = 0;
    trim_scene_frames(&source, 1, 4, &mut Vec::new(), || {
        total += 1;
        true
    })
    .unwrap();
    for limit in [0, 8, 32, 128, total / 3, total / 2, total - 2] {
        let mut calls = 0;
        let mut output = Vec::new();
        let failure = trim_scene_frames(&source, 1, 4, &mut output, || {
            calls += 1;
            calls <= limit
        })
        .unwrap_err();
        assert_eq!(
            failure.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
        assert!(output.is_empty());
    }
}

#[test]
fn camera_trim_requires_exact_scalar_and_message_layouts() {
    let source = Fixture::new().bytes();
    for variant in 0..7 {
        let mut layout = Layout::parse(&source, &mut || true).unwrap();
        match variant {
            0 => layout.metadata.classes[4].as_mut().unwrap().size = 44,
            1 => layout.metadata.properties[1].as_mut().unwrap().kind = 2,
            2 => layout.pointer_layouts[4].push((1, 36)),
            3 => layout.metadata.messages[0]
                .as_mut()
                .unwrap()
                .entries
                .swap(0, 1),
            4 => layout.metadata.messages[0].as_mut().unwrap().size = 40,
            5 => layout.metadata.properties[5].as_mut().unwrap().offset32 = 8,
            _ => layout.metadata.properties[6].as_mut().unwrap().datatype_id = 1,
        }
        for (first, last) in [(0, 0), (2, 3)] {
            assert!(
                camera::Scope::prepare(&layout, first, last, &mut || true).is_err(),
                "layout {variant}"
            );
        }
    }
}
