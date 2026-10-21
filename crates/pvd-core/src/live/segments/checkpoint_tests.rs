use super::*;
use std::{fs, sync::atomic::Ordering};

fn directory() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "pvd-checkpoint-resource-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&path).unwrap();
    path
}

fn info(index: usize) -> FrameInfo {
    FrameInfo {
        index,
        timestamp: index as u64 * 10,
        scene_id: 1,
    }
}

fn scene(objects: u64) -> SceneState {
    let mut scene = SceneState::default();
    for id in 1..=objects {
        Action::Create(Object {
            id,
            generation: 0,
            type_name: "Actor".into(),
            properties: [("value".into(), Value::U64(id))].into(),
        })
        .apply(&mut scene);
    }
    scene
}

fn append(writer: &mut SegmentWriter, index: usize, scene: &SceneState) {
    let plan = writer
        .prepare(
            &info(index),
            scene,
            &SceneUpdate::default(),
            &Analysis::default(),
            &[],
        )
        .unwrap();
    let before = writer.view.resources.physical_bytes();
    plan.persist_resources(&writer.view.resources).unwrap();
    let added = writer.view.resources.physical_bytes() - before;
    writer.commit(plan, &info(index), added).unwrap();
}

#[test]
fn repeated_full_checkpoints_share_blocks_without_object_dependency_vectors() {
    let path = directory();
    {
        let owner = CacheDirectory::new(&path).unwrap();
        let mut writer = SegmentWriter::new(&owner, 0).unwrap();
        let scene = scene(8192);
        append(&mut writer, 0, &scene);
        let first_id = writer.view.checkpoints[&0];
        assert_ne!(first_id & (1 << 63), 0);
        let resources = writer.view.resources.clone();
        let first = resources.pin(first_id).unwrap();
        assert!(first.info().0 > 64 << 10);
        assert_eq!(first.info().1, 0);
        assert!(first.info().2);
        let bytes = resources.reader().read(first_id, &mut || false).unwrap();
        for index in 1..120 {
            append(&mut writer, index, &scene);
        }
        let plan = writer
            .prepare(
                &info(120),
                &scene,
                &SceneUpdate::default(),
                &Analysis::default(),
                &[],
            )
            .unwrap();
        let next_id = plan.checkpoint.unwrap();
        assert_eq!(plan.handles.len(), 1);
        assert_ne!(first_id, next_id);
        let next = resources.pin(next_id).unwrap();
        assert_eq!(next.info(), (bytes.len(), 0, false));
        assert!(resources.reader().read(next_id, &mut || false).is_err());
        assert_eq!(resources.planned_bytes(&plan.resources), 0);
        assert_eq!(
            plan.reservation(&resources),
            plan.frame.len() as u64
                + 36
                + plan
                    .undo
                    .as_ref()
                    .map_or(0, |bytes| bytes.len() as u64 + 36)
        );
        let before = resources.physical_bytes();
        plan.persist_resources(&resources).unwrap();
        writer.commit(plan, &info(120), 0).unwrap();
        assert_eq!(resources.physical_bytes(), before);
        assert_eq!(
            resources.reader().read(next_id, &mut || false).unwrap(),
            bytes
        );
        let mut reader = DiskRecording::open(&writer.view).unwrap();
        let Record::Checkpoint(restored) =
            read_record_with_cancel(&mut reader, next_id, writer.view.bytes, &mut || false)
                .unwrap()
        else {
            panic!("checkpoint type")
        };
        assert_eq!(restored, writer.state.index);
        let inline = fs::read(&writer.view.path).unwrap();
        let mut offset = 8;
        let mut frames = 0;
        let mut undos = 0;
        for location in &writer.view.frames {
            assert_eq!(location.offset, offset as u64);
            let len = u32::from_le_bytes(inline[offset..offset + 4].try_into().unwrap()) as usize;
            let Record::Frame(frame) =
                decode_record(&inline[offset + 36..offset + 36 + len], &mut || false).unwrap()
            else {
                panic!("frame type");
            };
            let expected = info(frames);
            for actual in [&frame.info, &location.info] {
                assert_eq!(actual.index, expected.index);
                assert_eq!(actual.timestamp, expected.timestamp);
                assert_eq!(actual.scene_id, expected.scene_id);
            }
            offset += 36 + len;
            if frames == 0 {
                assert!(location.undo.is_none());
            } else {
                assert_eq!(location.undo.unwrap().get(), offset as u64);
                let len =
                    u32::from_le_bytes(inline[offset..offset + 4].try_into().unwrap()) as usize;
                let Record::Undo(undo) =
                    decode_record(&inline[offset + 36..offset + 36 + len], &mut || false).unwrap()
                else {
                    panic!("undo type");
                };
                assert!(undo.matches(&expected, frames - 1));
                undos += 1;
                offset += 36 + len;
            }
            frames += 1;
        }
        assert_eq!(frames, 121);
        assert_eq!(undos, 120);
        assert_eq!(offset, inline.len());
    }
    assert_eq!(fs::read_dir(&path).unwrap().count(), 0);
    fs::remove_dir(path).unwrap();
}

#[test]
fn inherited_versions_survive_rollover_but_not_the_last_segment_lease() {
    let path = directory();
    {
        let mut history = HistoryWriter::new(HistoryConfig {
            directory: path.clone(),
            segment_frames: 2,
            min_free_disk_bytes: 0,
            ..Default::default()
        })
        .unwrap();
        let mut scene = scene(2);
        Action::Set(1, "target".into(), Value::Ref(2)).apply(&mut scene);
        Action::Set(1, "geometry".into(), Value::Bytes(vec![17; 8192].into())).apply(&mut scene);
        let mut expected = Vec::new();
        for index in 0..8 {
            let changed = match index {
                3 => {
                    Action::Destroy(2).apply(&mut scene);
                    [2].into()
                }
                4 => {
                    Action::Create(Object {
                        id: 2,
                        generation: 1,
                        type_name: "Replacement".into(),
                        properties: BTreeMap::new(),
                    })
                    .apply(&mut scene);
                    [2].into()
                }
                5 => {
                    Action::Set(1, "target".into(), Value::Ref(2)).apply(&mut scene);
                    [1].into()
                }
                _ => BTreeSet::new(),
            };
            history.append(
                &info(index),
                &scene,
                &SceneUpdate::capture(&scene, &changed, false),
                &Analysis::default(),
                Vec::new(),
            );
            assert!(history.warning.is_none(), "{:?}", history.warning);
            expected.push(scene.clone());
        }
        let disk = history.snapshot();
        let old = disk.segments[0].clone();
        let resources = old.resources.clone();
        let checkpoint = resources.pin(old.checkpoints[&0]).unwrap();
        assert!(checkpoint.dependency_ids().is_empty());
        let object_id = {
            let mut reader = DiskRecording::open(&old).unwrap();
            let Record::Checkpoint(index) =
                read_record_with_cancel(&mut reader, checkpoint.id(), old.bytes, &mut || false)
                    .unwrap()
            else {
                panic!("checkpoint type")
            };
            index.objects[&1]
        };
        let reader = DiskRecording::open(&old).unwrap();
        assert!(history.evict_one().unwrap());
        assert!(!old.lease.available());
        assert!(old.path.exists());
        assert!(disk.state_at(0).is_err());
        for index in (2..8).rev().chain(2..8) {
            assert_eq!(disk.state_at(index).unwrap(), expected[index]);
        }
        drop(reader);
        let mut opportunistic_compaction = true;
        history
            .reclaim(0, &resources, &mut opportunistic_compaction)
            .unwrap();
        assert!(!old.path.exists());
        drop((old, disk, history));
        resources.collect().unwrap();
        assert!(resources.pin(object_id).is_err());
        assert!(
            resources
                .reader()
                .read(checkpoint.id(), &mut || false)
                .is_ok()
        );
        drop(checkpoint);
        resources.collect().unwrap();
        assert_eq!(resources.physical_bytes(), 0);
    }
    assert_eq!(fs::read_dir(&path).unwrap().count(), 0);
    fs::remove_dir(path).unwrap();
}

#[test]
fn checkpoint_resources_do_not_publish_a_frame_when_segment_flush_fails() {
    let path = directory();
    {
        let owner = CacheDirectory::new(&path).unwrap();
        let mut writer = SegmentWriter::new(&owner, 0).unwrap();
        let state = scene(2);
        let plan = writer
            .prepare(
                &info(0),
                &state,
                &SceneUpdate::default(),
                &Analysis::default(),
                &[],
            )
            .unwrap();
        let checkpoint_id = plan.checkpoint.unwrap();
        let resources = writer.view.resources.clone();
        plan.persist_resources(&resources).unwrap();
        writer.file.flush().unwrap();
        writer.file = BufWriter::new(segment_file(File::open(&writer.view.path).unwrap()));
        assert!(
            writer
                .commit(plan, &info(0), resources.physical_bytes())
                .is_err()
        );
        assert!(writer.view.frames.is_empty());
        assert!(writer.view.checkpoints.is_empty());
        assert_eq!(writer.state.index, StateIndex::default());
        assert!(writer.state.roots.is_empty());
        resources.collect().unwrap();
        assert!(resources.pin(checkpoint_id).is_err());
        assert_eq!(resources.physical_bytes(), 0);
    }
    assert_eq!(fs::read_dir(&path).unwrap().count(), 0);
    fs::remove_dir(path).unwrap();
}

#[test]
fn inline_checkpoints_remain_readable_with_resource_backed_object_versions() {
    let path = directory();
    {
        let owner = CacheDirectory::new(&path).unwrap();
        let mut writer = SegmentWriter::new(&owner, 0).unwrap();
        let expected = scene(3);
        append(&mut writer, 0, &expected);
        let index = writer.state.index.clone();
        let offset = writer.write(&RecordRef::Checkpoint(&index)).unwrap();
        assert_eq!(offset & (1 << 63), 0);
        writer.file.flush().unwrap();
        Arc::make_mut(&mut writer.view.checkpoints).insert(0, offset);
        let mut reader = DiskRecording::open(&writer.view).unwrap();
        let Record::Checkpoint(actual) =
            read_record_with_cancel(&mut reader, offset, writer.view.bytes, &mut || false).unwrap()
        else {
            panic!("checkpoint type")
        };
        assert_eq!(actual, index);
    }
    assert_eq!(fs::read_dir(&path).unwrap().count(), 0);
    fs::remove_dir(path).unwrap();
}

#[test]
fn checkpoint_read_rejects_cancel_wrong_type_and_expired_lease() {
    let path = directory();
    {
        let mut history = HistoryWriter::new(HistoryConfig {
            directory: path.clone(),
            min_free_disk_bytes: 0,
            ..Default::default()
        })
        .unwrap();
        let expected = scene(2);
        history.append(
            &info(0),
            &expected,
            &SceneUpdate::default(),
            &Analysis::default(),
            Vec::new(),
        );
        assert!(history.warning.is_none(), "{:?}", history.warning);
        let mut disk = history.snapshot();
        let cancelled = disk.state_at_with_cancel(0, &mut || true).unwrap_err();
        assert_eq!(
            cancelled.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
        let checkpoint = disk.active.as_ref().unwrap().checkpoints[&0];
        let object = history.active.as_ref().unwrap().state.index.objects[&1];
        Arc::make_mut(&mut disk.active.as_mut().unwrap().checkpoints).insert(0, object);
        assert!(
            disk.state_at(0)
                .unwrap_err()
                .to_string()
                .contains("checkpoint record type mismatch")
        );
        Arc::make_mut(&mut disk.active.as_mut().unwrap().checkpoints).insert(0, checkpoint);
        assert_eq!(disk.state_at(0).unwrap(), expected);
        disk.active.as_ref().unwrap().lease.invalidate().unwrap();
        assert!(
            disk.state_at(0)
                .unwrap_err()
                .to_string()
                .contains("expired")
        );
    }
    assert_eq!(fs::read_dir(&path).unwrap().count(), 0);
    fs::remove_dir(path).unwrap();
}

#[test]
fn checkpoint_resource_corruption_length_and_truncation_are_rejected_and_recoverable() {
    let path = directory();
    {
        let mut history = HistoryWriter::new(HistoryConfig {
            directory: path.clone(),
            min_free_disk_bytes: 0,
            ..Default::default()
        })
        .unwrap();
        let expected = scene(2);
        history.append(
            &info(0),
            &expected,
            &SceneUpdate::default(),
            &Analysis::default(),
            Vec::new(),
        );
        assert!(history.warning.is_none(), "{:?}", history.warning);
        let disk = history.snapshot();
        let checkpoint = disk.active.as_ref().unwrap().checkpoints[&0];
        assert_ne!(checkpoint & (1 << 63), 0);
        let resources = history.resources.as_ref().unwrap();
        let bytes = resources.reader().read(checkpoint, &mut || false).unwrap();
        assert!(matches!(
            decode_record(&bytes, &mut || false).unwrap(),
            Record::Checkpoint(_)
        ));
        let mut packet = (bytes.len() as u32).to_le_bytes().to_vec();
        packet.extend_from_slice(blake3::hash(&bytes).as_bytes());
        packet.extend_from_slice(&bytes);
        let mut matches = Vec::new();
        let files = history
            .owner
            .as_ref()
            .unwrap()
            .files
            .lock()
            .unwrap()
            .clone();
        for file in files {
            let original = fs::read(&file).unwrap();
            for (offset, block) in original.windows(packet.len()).enumerate() {
                if block == packet {
                    matches.push((file.clone(), original.clone(), offset));
                }
            }
        }
        assert_eq!(matches.len(), 1);
        let (file, original, offset) = matches.pop().unwrap();
        assert_eq!(disk.state_at(0).unwrap(), expected);

        let mut changed = original.clone();
        changed[offset + 36] ^= 0x80;
        fs::write(&file, changed).unwrap();
        drop(resources.reader());
        assert!(
            disk.state_at(0)
                .unwrap_err()
                .to_string()
                .contains("checksum")
        );
        fs::write(&file, &original).unwrap();
        drop(resources.reader());
        assert_eq!(disk.state_at(0).unwrap(), expected);

        let mut changed = original.clone();
        changed[offset..offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        fs::write(&file, changed).unwrap();
        drop(resources.reader());
        assert!(
            disk.state_at(0)
                .unwrap_err()
                .to_string()
                .contains("header mismatch")
        );
        fs::write(&file, &original).unwrap();
        drop(resources.reader());
        assert_eq!(disk.state_at(0).unwrap(), expected);

        fs::write(&file, &original[..offset + packet.len() - 1]).unwrap();
        drop(resources.reader());
        let error = disk.state_at(0).unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        fs::write(&file, &original).unwrap();
        drop(resources.reader());
        assert_eq!(disk.state_at(0).unwrap(), expected);
    }
    assert_eq!(fs::read_dir(&path).unwrap().count(), 0);
    fs::remove_dir(path).unwrap();
}
