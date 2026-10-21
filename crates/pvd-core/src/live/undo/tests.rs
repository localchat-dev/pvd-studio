use super::*;
use std::{fs, sync::atomic::Ordering};

fn info(index: usize) -> FrameInfo {
    FrameInfo {
        index,
        timestamp: index as u64 * 7,
        scene_id: 1 + index as u64 % 2,
    }
}

fn directory() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "pvd-persistent-undo-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&path).unwrap();
    path
}

fn history(path: &Path, frames: usize) -> HistoryWriter {
    HistoryWriter::new(HistoryConfig {
        directory: path.to_owned(),
        disk_budget_bytes: 16 << 20,
        segment_target_bytes: 1 << 20,
        segment_frames: frames,
        min_free_disk_bytes: 0,
    })
    .unwrap()
}

fn recording(history: &HistoryWriter) -> crate::Recording {
    let mut recording = crate::RecordingBuilder::new(pvd_protocol::Header {
        version: 1,
        timestamp_numerator: 1,
        timestamp_denominator: 1,
        flags: 1,
        endian: pvd_protocol::Endian::Little,
    })
    .recording;
    recording.disk = Some(history.snapshot());
    recording.frames = recording.disk.as_ref().unwrap().frames();
    recording
}

fn append(history: &mut HistoryWriter, frame: usize, scene: &SceneState, patch: &SceneUpdate) {
    history.append(&info(frame), scene, patch, &Analysis::default(), Vec::new());
    assert!(history.warning.is_none(), "{:?}", history.warning);
}

#[test]
fn inverse_covers_old_resources_generations_cameras_errors_and_distinct_scene_ids() {
    let before = StateIndex {
        objects: [(4, (1 << 63) + 8)].into(),
        debug: [(4, (1 << 63) + 9)].into(),
        contacts: [(4, (1 << 63) + 10)].into(),
        generations: [(4, 7), (99, 12)].into(),
        errors: vec![ErrorEvent {
            code: 3,
            message: "old".into(),
            file: "source".into(),
            line: 2,
            timestamp: 5,
        }],
        cameras: [(
            "camera".into(),
            SourceCamera {
                position: [1.; 3],
                up: [0.; 3],
                target: [2.; 3],
            },
        )]
        .into(),
    };
    let changes = IndexUpdate {
        objects: vec![(4, None), (6, Some((1 << 63) + 11))],
        debug: vec![(4, None)],
        contacts: vec![(4, None)],
        generations: [(4, 8), (6, 1)].into(),
        cameras: [("camera".into(), None), ("new".into(), None)].into(),
        errors: Some(Vec::new()),
    };
    let bytes = prepare(&info(5), Some(4), &before, &changes)
        .unwrap()
        .unwrap();
    assert_eq!(bytes[0], 9);
    let undo = decode(&bytes, &mut || false).unwrap();
    undo.verify_before(&before, &changes).unwrap();
    let mut after = before.clone();
    changes.apply(&mut after);
    undo.apply(&mut after);
    assert_eq!(after, before);
    assert!(
        prepare(&info(8), Some(4), &before, &changes)
            .unwrap()
            .is_none()
    );
    assert!(
        prepare(&info(5), None, &before, &changes)
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_near_allowance_string_and_map_record_published_by_writer_decodes() {
    let mut before = StateIndex::default();
    let mut changes = IndexUpdate {
        errors: Some(Vec::new()),
        ..Default::default()
    };
    let camera = SourceCamera {
        position: [0.; 3],
        up: [1.; 3],
        target: [2.; 3],
    };
    for key in 0..1800 {
        let name = format!("camera-{key:04}-{}", "x".repeat(256));
        before.cameras.insert(name.clone(), camera);
        changes.cameras.insert(name, None);
        before.generations.insert(key, 2);
        changes.generations.insert(key, 3);
    }
    before.errors.push(ErrorEvent {
        code: 2,
        message: "m".repeat(500_000),
        file: "fixture".into(),
        line: 1,
        timestamp: 0,
    });
    let charge = allowance(&changes, Some(&before.errors)).unwrap();
    assert!(charge > MAX_BYTES * 4 / 5 && charge <= MAX_BYTES);
    let encoded = prepare(&info(2), Some(1), &before, &changes)
        .unwrap()
        .unwrap();
    let undo = decode(&encoded, &mut || false).unwrap();
    undo.verify_before(&before, &changes).unwrap();
    before.errors[0].message.push_str(&"m".repeat(1_000_000));
    assert!(
        prepare(&info(2), Some(1), &before, &changes)
            .unwrap()
            .is_none()
    );
}

#[test]
fn cold_position_then_long_reverse_uses_persistent_edges_and_preserves_scene() {
    let path = directory();
    {
        let mut history = history(&path, 40);
        let mut scene = SceneState::default();
        Action::Create(Object {
            id: 1,
            generation: 3,
            type_name: "Actor".into(),
            properties: BTreeMap::new(),
        })
        .apply(&mut scene);
        Action::Create(Object {
            id: 2,
            generation: 4,
            type_name: "Actor".into(),
            properties: BTreeMap::new(),
        })
        .apply(&mut scene);
        Action::Set(1, "target".into(), Value::Ref(2)).apply(&mut scene);
        let mut expected = Vec::new();
        for frame in 0..381 {
            Action::Set(1, "value".into(), Value::U64(frame as u64)).apply(&mut scene);
            if frame == 150 {
                Action::Destroy(2).apply(&mut scene);
            } else if frame == 170 {
                Action::Create(Object {
                    id: 2,
                    generation: 5,
                    type_name: "Actor".into(),
                    properties: BTreeMap::new(),
                })
                .apply(&mut scene);
            }
            let patch = if frame == 0 {
                SceneUpdate::full(&scene)
            } else {
                SceneUpdate::capture(&scene, &[1, 2].into(), false)
            };
            append(&mut history, frame, &scene, &patch);
            expected.push(scene.clone());
        }
        let recording = recording(&history);
        recording.disk.as_ref().unwrap().verify_replay().unwrap();
        let mut cursor = ReplayCursor::default();
        let mut borrowed = SceneState::default();
        let mut owned = SceneState::default();
        for frame in (0..381).rev() {
            let result = cursor.seek_scene(&recording, frame, || false).unwrap();
            result.update.apply(&mut borrowed);
            result.update.apply_owned(&mut owned);
            assert_eq!(borrowed, expected[frame]);
            assert_eq!(owned, expected[frame]);
        }
        let routes = cursor.scene_route_counts();
        assert_eq!(routes.index, 1);
        assert_eq!(routes.adjacent_forward, 0);
        assert_eq!(routes.memory_undo + routes.persistent_undo, 380);
        assert!(routes.persistent_undo > 240);
        let report = history
            .physical_accounting(disk_accounting::Limits::default(), &mut || false)
            .unwrap();
        assert_eq!(report.segment.undos, 380);
        assert_eq!(report.segment.unpublished_tail_bytes, 0);
        assert_eq!(report.partition_sum, history.bytes());
    }
    assert_eq!(fs::read_dir(&path).unwrap().count(), 0);
    fs::remove_dir(path).unwrap();
}

#[test]
fn undo_decode_rejects_type_tail_continuity_and_cancellation() {
    let mut undo = decode(
        &prepare(
            &info(1),
            Some(0),
            &StateIndex::default(),
            &IndexUpdate::default(),
        )
        .unwrap()
        .unwrap(),
        &mut || false,
    )
    .unwrap();
    undo.to = 10;
    let wrong = postcard::to_stdvec(&RecordRef::Undo(&undo)).unwrap();
    assert!(decode(&wrong, &mut || false).is_err());
    undo.to = 0;
    let bytes = postcard::to_stdvec(&RecordRef::Undo(&undo)).unwrap();
    assert!(decode(&bytes[..bytes.len() - 1], &mut || false).is_err());
    let mut tail = bytes.clone();
    tail.push(0);
    assert!(decode(&tail, &mut || false).is_err());
    assert!(
        decode(
            &postcard::to_stdvec(&Record::AnalysisPages(1)).unwrap(),
            &mut || false
        )
        .is_err()
    );
    let failure = decode(&bytes, &mut || true).unwrap_err();
    assert_eq!(
        failure.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
}

#[test]
fn canonical_decoder_rejects_duplicate_map_and_set_keys() {
    #[derive(Serialize)]
    struct LooseUpdate {
        objects: Vec<(u64, Option<u64>)>,
        generations: Vec<(u64, u32)>,
        debug: Vec<(u64, Option<u64>)>,
        contacts: Vec<(u64, Option<u64>)>,
        errors: Option<Vec<ErrorEvent>>,
        cameras: Vec<(String, Option<SourceCamera>)>,
    }
    #[derive(Serialize)]
    struct LooseUndo {
        from: FrameInfo,
        to: usize,
        changes: LooseUpdate,
        removed_generations: Vec<u64>,
    }
    for field in 0..3 {
        let mut loose = LooseUndo {
            from: info(1),
            to: 0,
            changes: LooseUpdate {
                objects: Vec::new(),
                generations: Vec::new(),
                debug: Vec::new(),
                contacts: Vec::new(),
                errors: None,
                cameras: Vec::new(),
            },
            removed_generations: Vec::new(),
        };
        match field {
            0 => loose.changes.generations = vec![(7, 1), (7, 2)],
            1 => loose.changes.cameras = vec![("camera".into(), None), ("camera".into(), None)],
            2 => loose.removed_generations = vec![7, 7],
            _ => unreachable!(),
        }
        let mut bytes = vec![9];
        bytes.extend(postcard::to_stdvec(&loose).unwrap());
        assert!(decode(&bytes, &mut || false).is_err(), "field {field}");
    }
}

#[test]
fn persistent_read_rejects_bad_bytes_expired_source_and_cancel_without_scene_publication() {
    for failure in ["checksum", "truncate", "expired", "cancel"] {
        let path = directory();
        {
            let mut history = history(&path, 1);
            let mut scene = SceneState::default();
            Action::Create(Object {
                id: 1,
                generation: 0,
                type_name: "Actor".into(),
                properties: BTreeMap::new(),
            })
            .apply(&mut scene);
            for frame in 0..3 {
                Action::Set(1, "value".into(), Value::U64(frame as u64)).apply(&mut scene);
                let patch = SceneUpdate::capture(&scene, &[1].into(), false);
                append(&mut history, frame, &scene, &patch);
            }
            let recording = recording(&history);
            let disk = recording.disk.as_ref().unwrap();
            let source = disk.segment(2).unwrap();
            let location = source.frames.last().unwrap();
            let offset = location.undo.unwrap().get();
            let mut cursor = ReplayCursor::default();
            let mut displayed = SceneState::default();
            cursor
                .seek_scene(&recording, 2, || false)
                .unwrap()
                .update
                .apply(&mut displayed);
            let held = displayed.clone();
            match failure {
                "checksum" => {
                    let mut file = OpenOptions::new().write(true).open(&source.path).unwrap();
                    file.seek(SeekFrom::Start(offset + 36)).unwrap();
                    file.write_all(&[255]).unwrap();
                    file.flush().unwrap();
                }
                "truncate" => {
                    OpenOptions::new()
                        .write(true)
                        .open(&source.path)
                        .unwrap()
                        .set_len(offset + 37)
                        .unwrap();
                }
                "expired" => source.lease.invalidate().unwrap(),
                "cancel" => (),
                _ => unreachable!(),
            }
            let mut checks = 0;
            let result = cursor.seek_scene(&recording, 1, || {
                checks += 1;
                failure == "cancel" && checks >= 4
            });
            assert!(result.is_err(), "{failure}");
            assert_eq!(displayed, held);
            assert_eq!(cursor.scene_route_counts().persistent_undo, 0);
        }
        assert_eq!(fs::read_dir(&path).unwrap().count(), 0);
        fs::remove_dir(path).unwrap();
    }
}

#[test]
fn sparse_directory_preserves_contiguous_undo_and_rebuilds_across_the_gap() {
    let path = directory();
    {
        let mut history = history(&path, 40);
        let mut scene = SceneState::default();
        Action::Create(Object {
            id: 1,
            generation: 3,
            type_name: "Actor".into(),
            properties: BTreeMap::new(),
        })
        .apply(&mut scene);
        let mut expected = BTreeMap::new();
        for frame in [0, 2, 3] {
            Action::Set(1, "value".into(), Value::U64(frame as u64)).apply(&mut scene);
            let patch = SceneUpdate::capture(&scene, &[1].into(), false);
            append(&mut history, frame, &scene, &patch);
            expected.insert(frame, scene.clone());
        }
        let recording = recording(&history);
        let disk = recording.disk.as_ref().unwrap();
        let source = disk.segment(3).unwrap();
        assert_eq!(
            source
                .frames
                .iter()
                .map(|entry| entry.info.index)
                .collect::<Vec<_>>(),
            [0, 2, 3]
        );
        assert!(source.frames[1].undo.is_none());
        assert!(source.frames[2].undo.is_some());
        disk.verify_replay().unwrap();

        let mut cursor = ReplayCursor::default();
        let mut borrowed = SceneState::default();
        let mut owned = SceneState::default();
        for frame in [3, 2] {
            let result = cursor.seek_scene(&recording, frame, || false).unwrap();
            result.update.apply(&mut borrowed);
            result.update.apply_owned(&mut owned);
            assert_eq!(borrowed, expected[&frame]);
            assert_eq!(owned, expected[&frame]);
        }
        let held = borrowed.clone();
        let before = cursor.scene_route_counts();
        assert_eq!(before.index, 1);
        assert_eq!(before.memory_undo, 1);
        assert_eq!(before.persistent_undo, 0);
        assert_eq!(before.adjacent_forward, 0);
        let failure = match cursor.seek_scene(&recording, 1, || false) {
            Ok(_) => panic!("a frame absent from the sparse directory was accepted"),
            Err(failure) => failure.to_string(),
        };
        assert!(
            failure.contains("outside the visible retained history"),
            "{failure}"
        );
        assert_eq!(borrowed, held);
        assert_eq!(owned, held);

        let result = cursor.seek_scene(&recording, 0, || false).unwrap();
        result.update.apply(&mut borrowed);
        result.update.apply_owned(&mut owned);
        assert_eq!(borrowed, expected[&0]);
        assert_eq!(owned, expected[&0]);
        let routes = cursor.scene_route_counts();
        assert_eq!(routes.index, 2);
        assert_eq!(routes.memory_undo, 1);
        assert_eq!(routes.persistent_undo, 0);
        assert_eq!(routes.adjacent_forward, 0);
    }
    assert_eq!(fs::read_dir(&path).unwrap().count(), 0);
    fs::remove_dir(path).unwrap();
}

#[test]
fn verification_and_replay_reject_undo_without_the_declared_directory_predecessor() {
    for gap in [false, true] {
        let path = directory();
        {
            let mut history = history(&path, if gap { 40 } else { 1 });
            let scene = SceneState::default();
            for frame in 0..3 {
                append(&mut history, frame, &scene, &SceneUpdate::default());
            }
            let mut recording = recording(&history);
            let disk = recording.disk.as_mut().unwrap();
            disk.verify_replay().unwrap();
            let source = disk.active.as_mut().unwrap();
            assert!(source.frames.last().unwrap().undo.is_some());
            if gap {
                source.frames = source
                    .frames
                    .iter()
                    .filter(|entry| entry.info.index != 1)
                    .cloned()
                    .collect();
            } else {
                source.previous_frame = None;
            }
            let failure = disk.verify_replay().unwrap_err().to_string();
            assert!(
                failure.contains("frame directory is discontinuous"),
                "{failure}"
            );
            let mut cursor = ReplayCursor::default();
            let mut displayed = SceneState::default();
            cursor
                .seek_scene(&recording, 2, || false)
                .unwrap()
                .update
                .apply(&mut displayed);
            let held = displayed.clone();
            let failure = match cursor.seek_scene(&recording, 1, || false) {
                Ok(_) => panic!("undo with a missing predecessor was accepted"),
                Err(failure) => failure.to_string(),
            };
            assert!(
                failure.contains("frame directory is discontinuous"),
                "{failure}"
            );
            assert_eq!(displayed, held);
            assert_eq!(cursor.scene_route_counts().persistent_undo, 0);
        }
        assert_eq!(fs::read_dir(&path).unwrap().count(), 0);
        fs::remove_dir(path).unwrap();
    }
}

#[cfg(feature = "resource-test-write-faults")]
#[test]
fn partial_pair_flush_keeps_old_frames_readable_and_charges_unpublished_tail() {
    let frame_bytes = 36
        + postcard::to_stdvec(&RecordRef::Frame(StoredFrameRef {
            info: &info(1),
            update: &IndexUpdate::default(),
            analysis: &[],
        }))
        .unwrap()
        .len();
    for prefix in [
        Some(7),
        Some(frame_bytes + 7),
        Some(frame_bytes + 36 + 7),
        None,
    ] {
        let path = directory();
        {
            let mut history = history(&path, 40);
            let scene = SceneState::default();
            append(&mut history, 0, &scene, &SceneUpdate::default());
            let active = history.active.as_mut().unwrap();
            let committed = active.view.committed_end;
            let segment_path = active.view.path.clone();
            if let Some(bytes) = prefix {
                active.fail_segment_write_after(bytes);
            } else {
                active.fail_segment_flush();
            }
            history.append(
                &info(1),
                &scene,
                &SceneUpdate::default(),
                &Analysis::default(),
                Vec::new(),
            );
            assert!(history.warning.is_some());
            assert!(history.active.is_none());
            let actual = fs::metadata(segment_path).unwrap().len();
            assert!(actual > committed);
            if let Some(bytes) = prefix {
                assert_eq!(actual, committed + bytes as u64);
            }
            let disk = history.snapshot();
            assert_eq!(disk.frames().len(), 1);
            assert_eq!(disk.state_at(0).unwrap(), scene);
            assert!(disk.state_at(1).is_err());
            let report = history
                .physical_accounting(disk_accounting::Limits::default(), &mut || false)
                .unwrap();
            assert_eq!(report.segment.frames, 1);
            assert_eq!(report.segment.undos, 0);
            assert_eq!(report.segment.unpublished_tail_bytes, actual - committed);
            assert_eq!(report.partition_sum, history.bytes());
            history.retry_after = Instant::now();
            append(&mut history, 2, &scene, &SceneUpdate::default());
            let restored = history.snapshot();
            assert_eq!(restored.state_at(0).unwrap(), scene);
            assert_eq!(restored.state_at(2).unwrap(), scene);
            assert!(
                restored
                    .segment(2)
                    .unwrap()
                    .frames
                    .last()
                    .unwrap()
                    .undo
                    .is_none()
            );
        }
        assert_eq!(fs::read_dir(&path).unwrap().count(), 0);
        fs::remove_dir(path).unwrap();
    }
}
