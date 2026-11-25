use super::*;
use crate::{RecordingBuilder, frame_source_tests::TestDirectory};
use pvd_protocol::{Endian, Header};

mod cancellation;

fn metadata() -> crate::Recording {
    RecordingBuilder::new(Header {
        version: 1,
        timestamp_numerator: 1,
        timestamp_denominator: 1,
        flags: 1,
        endian: Endian::Little,
    })
    .recording
}

fn info(index: usize) -> FrameInfo {
    FrameInfo {
        index,
        timestamp: index as u64,
        scene_id: 100,
    }
}

fn state(value: u64) -> SceneState {
    let mut state = SceneState::default();
    state.objects.insert(
        7,
        Object {
            id: 7,
            generation: 0,
            type_name: "Actor".into(),
            properties: [("value".into(), Value::U64(value))].into(),
        },
    );
    state
}

fn append(history: &mut HistoryWriter, index: usize) {
    let state = state(index as u64);
    let patch = SceneUpdate::capture(&state, &[7].into(), false);
    history.append(&info(index), &state, &patch, &Analysis::default(), vec![]);
}

fn recording(history: &HistoryWriter) -> crate::Recording {
    let mut recording = metadata();
    let disk = history.snapshot();
    recording.frames = disk.frames();
    recording.disk = Some(disk);
    recording
}

fn source(history: &HistoryWriter) -> crate::Recording {
    let mut recording = recording(history);
    recording.frame_sources =
        FrameDirectory::from_legacy(&recording.frames, Default::default()).unwrap();
    recording
}

fn assert_value(recording: &crate::Recording, index: usize) {
    assert_eq!(
        recording.state_at(index).unwrap().objects[&7].properties["value"],
        Value::U64(index as u64)
    );
}

#[test]
fn fixed_analysis_reservation_does_not_seal_every_frame() {
    let directory = TestDirectory::new();
    let mut config = directory.config();
    config.segment_frames = 600;
    let mut history = HistoryWriter::new_persistent(config).unwrap();
    assert!(history.resources.as_ref().unwrap().metadata_bytes() > 32 << 20);
    for index in 0..40 {
        history.prepare_persistent(&source(&history), 5).unwrap();
        append(&mut history, index);
        assert!(history.accepting_frames(), "{:?}", history.warning);
    }
    assert_eq!(history.archive.as_ref().unwrap().len(), 0);
    assert_eq!(history.snapshot().stats.retained_frames, 40);
    assert!(history.bytes() < 64 << 10);
    for index in [0, 20, 39] {
        assert_value(&recording(&history), index);
    }
}

#[test]
fn dense_frames_seal_before_replay_index_admission_fails() {
    let directory = TestDirectory::new();
    let mut config = directory.config();
    config.segment_frames = 600;
    let mut history = HistoryWriter::new_persistent(config).unwrap();
    history.archive.as_mut().unwrap().index_limit = 1 << 20;
    let mut live = Streaming::new(history);
    let mut recording = metadata();
    let mut scene = SceneState::default();
    for id in 1..=24 {
        live.action(
            Action::Create(Object {
                id,
                generation: 0,
                type_name: "Actor".into(),
                properties: BTreeMap::new(),
            }),
            &mut scene,
            8 << 20,
        )
        .unwrap();
    }
    for index in 0..100 {
        for id in 1..=24 {
            live.action(
                Action::Set(id, "value".into(), Value::U64(index as u64)),
                &mut scene,
                8 << 20,
            )
            .unwrap();
        }
        live.prepare_persistent(&recording).unwrap();
        live.complete(info(index), binding(), &scene, &mut recording)
            .unwrap();
        assert!(
            live.history.accepting_frames(),
            "{:?}",
            live.history.warning
        );
    }
    live.history.seal_bank(&recording).unwrap();
    let archive = live.history.archive.as_ref().unwrap();
    assert!(archive.len() > 1 && archive.len() < 50);
    let replay = source(&live.history);
    for index in (0..100).rev() {
        assert_value(&replay, index);
    }
}

#[test]
fn schema_declared_after_first_frame_counts_toward_bank_admission() {
    use pvd_protocol::{Event, EventGroup, Name};
    let directory = TestDirectory::new();
    let mut history = HistoryWriter::new_persistent(directory.config()).unwrap();
    history.archive.as_mut().unwrap().index_limit = 1 << 20;
    let mut builder = RecordingBuilder::new(metadata().header);
    builder.enable_streaming(history);
    let group = |events| EventGroup {
        stream_id: 1,
        timestamp: 0,
        offset: 0,
        events,
    };
    builder
        .push(group(vec![Event::StringHandle {
            handle: 1,
            value: "frame".into(),
        }]))
        .unwrap();
    for id in 1..=24 {
        builder
            .action(Action::Create(Object {
                id,
                generation: 0,
                type_name: "Actor".into(),
                properties: BTreeMap::new(),
            }))
            .unwrap();
    }
    for index in 0..80 {
        if index == 1 {
            let class = Name {
                namespace: 0,
                name: 2,
            };
            let mut events = vec![Event::StringHandle {
                handle: 2,
                value: "Actor".into(),
            }];
            for handle in 3..803 {
                events.push(Event::StringHandle {
                    handle,
                    value: format!("field{handle}"),
                });
                events.push(Event::CreateProperty {
                    class,
                    name: handle,
                    semantic: 0,
                    datatype: class,
                    kind: 1,
                    named_values: vec![],
                });
            }
            builder.push(group(events)).unwrap();
        }
        for id in 1..=24 {
            builder
                .action(Action::Set(id, "value".into(), Value::U64(index)))
                .unwrap();
        }
        builder
            .push(group(vec![Event::EndSection {
                id: 0,
                name: 1,
                timestamp: index,
            }]))
            .unwrap();
        let history = &builder.streaming.as_ref().unwrap().history;
        assert!(
            history.warning.is_none(),
            "frame {index}: {:?}",
            history.warning
        );
    }
    let history = &mut builder.streaming.as_mut().unwrap().history;
    history.seal_bank(&builder.recording).unwrap();
    assert!(history.archive.as_ref().unwrap().len() > 1);
    let replay = builder.snapshot().unwrap();
    for index in [0, 20, 79] {
        assert_value(&replay, index);
    }
}

#[test]
fn unreclaimable_metadata_pressure_does_not_repeat_sealing() {
    let directory = TestDirectory::new();
    let mut config = directory.config();
    config.segment_frames = 600;
    config.disk_budget_bytes = 8 << 30;
    let mut history = HistoryWriter::new_persistent(config).unwrap();
    let resources = history.resources.as_ref().unwrap().clone();
    // Model a large live analysis reservation that remains owned after sealing.
    let _reservation = resources.reserve_metadata(360 << 20).unwrap();
    assert!(resources.needs_reclaim(0));
    for index in 0..12 {
        history.prepare_persistent(&source(&history), 5).unwrap();
        append(&mut history, index);
        assert!(history.accepting_frames(), "{:?}", history.warning);
    }
    assert_eq!(history.archive.as_ref().unwrap().len(), 0);
    assert_eq!(history.snapshot().stats.retained_frames, 12);
    for index in [0, 6, 11] {
        assert_value(&recording(&history), index);
    }
}

#[test]
fn static_scene_above_half_disk_budget_does_not_force_repeated_banks() {
    let directory = TestDirectory::new();
    let mut config = directory.config();
    config.segment_frames = 600;
    let budget = config.disk_budget_bytes;
    let mut history = HistoryWriter::new_persistent(config).unwrap();
    let mut seed = 41u64;
    let bytes = (0..9 << 20)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as u8
        })
        .collect::<Vec<_>>();
    let mut scene = state(0);
    scene.objects.insert(
        8,
        Object {
            id: 8,
            generation: 0,
            type_name: "StaticMesh".into(),
            properties: [("vertices".into(), Value::Bytes(bytes.into()))].into(),
        },
    );
    for index in 0..8 {
        history.prepare_persistent(&source(&history), 5).unwrap();
        scene
            .objects
            .get_mut(&7)
            .unwrap()
            .properties
            .insert("value".into(), Value::U64(index as u64));
        let patch = SceneUpdate::capture(&scene, &[7].into(), false);
        history.append(&info(index), &scene, &patch, &Analysis::default(), vec![]);
        assert!(history.accepting_frames(), "{:?}", history.warning);
        assert!(history.bytes() > budget / 2);
        assert!(history.bytes() < budget);
    }
    assert_eq!(history.archive.as_ref().unwrap().len(), 0);
    assert_eq!(history.snapshot().stats.retained_frames, 8);
    for index in [0, 4, 7] {
        let restored = recording(&history).state_at(index).unwrap();
        assert_eq!(restored.objects[&8], scene.objects[&8]);
        assert_value(&recording(&history), index);
    }
}

#[test]
fn sealing_below_disk_reserve_stops_without_losing_or_reopening_the_prefix() {
    let directory = TestDirectory::new();
    let mut history = HistoryWriter::new_persistent(directory.config()).unwrap();
    for index in 0..3 {
        append(&mut history, index);
    }
    let source = source(&history);
    let budget = history.config.disk_budget_bytes;
    history.config.min_free_disk_bytes = u64::MAX;
    // Trigger the regular bank-sealing path without writing a large fixture.
    history.config.disk_budget_bytes = history.bytes().max(1);
    history.prepare_persistent(&source, 2).unwrap();
    assert!(!history.accepting_frames());
    let warning = history.warning.clone().unwrap();
    assert!(warning.contains("stopped"), "{warning}");
    assert!(warning.contains("configured reserve"), "{warning}");
    assert_eq!(history.archive.as_ref().unwrap().len(), 0);
    assert_eq!(history.snapshot().stats.first_frame, 0);
    assert_eq!(history.snapshot().stats.retained_frames, 3);
    for index in 0..3 {
        assert_value(&recording(&history), index);
    }

    history.config.min_free_disk_bytes = 0;
    history.config.disk_budget_bytes = budget;
    history.retry_after = Instant::now();
    for index in 3..6 {
        history.prepare_persistent(&source, 2).unwrap();
        append(&mut history, index);
        assert_eq!(history.warning.as_deref(), Some(warning.as_str()));
        assert_eq!(history.frame_infos.len(), 3);
    }
    assert_eq!(history.frame_infos.last().unwrap().index, 2);
    assert!(recording(&history).state_at(3).is_err());
    assert_value(&recording(&history), 0);
}

#[cfg(windows)]
#[test]
fn append_below_disk_reserve_permanently_keeps_the_committed_prefix() {
    let directory = TestDirectory::new();
    let mut config = directory.config();
    config.segment_frames = 8;
    let mut history = HistoryWriter::new_persistent(config).unwrap();
    for index in 0..3 {
        append(&mut history, index);
    }
    history.config.min_free_disk_bytes = u64::MAX;
    append(&mut history, 3);
    let warning = history.warning.clone().unwrap();
    assert!(warning.contains("configured reserve"), "{warning}");
    assert!(!history.accepting_frames());
    history.config.min_free_disk_bytes = 0;
    history.retry_after = Instant::now();
    append(&mut history, 4);
    assert_eq!(history.warning.as_deref(), Some(warning.as_str()));
    assert_eq!(history.frame_infos.len(), 3);
    assert_eq!(history.snapshot().stats.first_frame, 0);
    for index in 0..3 {
        assert_value(&recording(&history), index);
    }
    assert!(recording(&history).state_at(3).is_err());
    assert!(recording(&history).state_at(4).is_err());
}

#[test]
fn navigation_limit_stops_before_append_without_discarding_the_prefix() {
    let directory = TestDirectory::new();
    let mut history = HistoryWriter::new_persistent(directory.config()).unwrap();
    for index in 0..3 {
        append(&mut history, index);
    }
    let source = source(&history);
    // Only the navigation directory is synthetic; the prefix remains real disk history.
    for index in 3..MAX_INDEX_FRAMES {
        history.frame_infos.push(info(index));
    }
    history.prepare_persistent(&source, 2).unwrap();
    assert!(!history.accepting_frames());
    let warning = history.warning.clone().unwrap();
    assert!(warning.contains("1,000,000"), "{warning}");
    for index in MAX_INDEX_FRAMES..MAX_INDEX_FRAMES + 2 {
        append(&mut history, index);
        assert_eq!(history.warning.as_deref(), Some(warning.as_str()));
    }
    assert_eq!(history.frame_infos.len(), MAX_INDEX_FRAMES);
    assert!(
        history
            .frame_infos
            .iter()
            .enumerate()
            .all(|(index, frame)| frame.index == index)
    );
    assert_eq!(history.snapshot().stats.first_frame, 0);
    assert_eq!(history.snapshot().stats.retained_frames, MAX_INDEX_FRAMES);
    assert_value(&recording(&history), 0);
    assert_value(&recording(&history), 2);
    assert!(recording(&history).state_at(MAX_INDEX_FRAMES).is_err());
}

fn binding() -> FrameBinding {
    FrameBinding {
        source: FrameSourceKey::Captured {
            section: FrameSection::PhysX,
            source: 100,
        },
        source_object: None,
        scene: None,
    }
}

fn check_directory_limit(limits: FrameDirectoryLimits, expected_warning: &str) {
    let directory = TestDirectory::new();
    let history = HistoryWriter::new_persistent(directory.config()).unwrap();
    let mut live = Streaming::new(history);
    live.set_frame_source(Some(binding().source));
    let mut state = SceneState::default();
    live.action(
        Action::Create(Object {
            id: 7,
            generation: 0,
            type_name: "Actor".into(),
            properties: BTreeMap::new(),
        }),
        &mut state,
        1 << 20,
    )
    .unwrap();
    let mut capture = metadata();
    capture.frame_sources = FrameDirectory::new(limits);
    let mut stopped_warning = None;
    for index in 0..5 {
        live.action(
            Action::Set(7, "value".into(), Value::U64(index as u64)),
            &mut state,
            1 << 20,
        )
        .unwrap();
        let bytes = live
            .directory_bytes(index, binding(), &capture.frame_sources)
            .unwrap();
        assert!(bytes <= limits.index_bytes);
        live.complete(info(index), binding(), &state, &mut capture)
            .unwrap();
        if index < 2 {
            assert!(live.history.accepting_frames());
            assert!(capture.frame_sources.frame(index).is_some());
        } else {
            assert!(!live.history.accepting_frames());
            assert!(capture.frame_sources.frame(index).is_none());
            let warning = live.history.warning.clone().unwrap();
            assert!(warning.contains(expected_warning), "{warning}");
            assert_eq!(
                stopped_warning.get_or_insert(warning.clone()).as_str(),
                warning.as_str()
            );
        }
    }
    assert_eq!(live.frames_seen, 5);
    assert_eq!(
        capture
            .frame_sources
            .source(binding().source)
            .unwrap()
            .observed_frames(),
        5
    );
    assert_eq!(capture.frame_sources.retained_frames(), 2);
    assert_eq!(live.history.snapshot().stats.first_frame, 0);
    let mut recording = recording(&live.history);
    recording.frame_sources = capture.frame_sources;
    recording
        .frame_sources
        .validate_frames(&recording.frames)
        .unwrap();
    assert_value(&recording, 0);
    assert_value(&recording, 1);
    assert!(recording.state_at(2).is_err());
    assert_eq!(live.frame.as_ref().unwrap().index, 4);
}

#[test]
fn frame_directory_limit_preserves_history_and_keeps_live_observation_running() {
    check_directory_limit(
        FrameDirectoryLimits {
            frames: 2,
            ..Default::default()
        },
        "frame limit exceeded",
    );
}

#[test]
fn directory_byte_limit_preserves_history_and_keeps_live_observation_running() {
    let mut probe = FrameDirectory::default();
    probe.observe(0, binding(), true).unwrap();
    probe.observe(1, binding(), true).unwrap();
    check_directory_limit(
        FrameDirectoryLimits {
            index_bytes: probe.index_bytes(),
            ..Default::default()
        },
        "index byte limit exceeded",
    );
}

#[test]
fn sealed_banks_keep_old_snapshots_readable_while_the_single_bank_cache_changes() {
    let directory = TestDirectory::new();
    let mut history = HistoryWriter::new_persistent(directory.config()).unwrap();
    for index in 0..2 {
        append(&mut history, index);
    }
    let old_snapshot = source(&history);
    history.seal_bank(&old_snapshot).unwrap();
    for index in 2..4 {
        append(&mut history, index);
    }
    history.seal_bank(&source(&history)).unwrap();
    let archive = history.archive.as_ref().unwrap();
    assert_eq!(archive.len(), 2);
    assert!(history.completed.is_empty());
    assert!(history.active.is_none());
    let current = source(&history);
    for index in [0, 2, 1, 3] {
        assert_value(&current, index);
        let cache_index = archive.directory.lock().unwrap().cached.as_ref().unwrap().0;
        assert_eq!(cache_index, index / 2);
    }
    assert_value(&old_snapshot, 0);
    assert_value(&old_snapshot, 1);
    assert!(old_snapshot.state_at(2).is_err());
    assert_eq!(current.frames.len(), 4);
    assert_eq!(current.disk.as_ref().unwrap().stats.first_frame, 0);
    assert!(archive.open(0, &mut || true).is_err());
    assert_value(&current, 0);
}

#[test]
fn replay_prepares_only_the_next_bank_within_budget_and_cancels_its_owner() {
    let directory = TestDirectory::new();
    let mut history = HistoryWriter::new_persistent(directory.config()).unwrap();
    for index in 0..4 {
        append(&mut history, index);
        if index % 2 == 1 {
            history.seal_bank(&source(&history)).unwrap();
        }
    }
    let recording = source(&history);
    let archive = history.archive.as_ref().unwrap();
    assert!(recording.replay_lookahead(0, 1).is_none());
    assert_value(&recording, 1);
    let forward = recording.replay_lookahead(1, 3).unwrap();
    assert_eq!(forward.frame(), 2);
    let live = Arc::new(AtomicBool::new(false));
    assert!(forward.prepare(&recording, live.clone()).unwrap());
    let prepared = {
        let directory = archive.directory.lock().unwrap();
        assert_eq!(directory.cached.as_ref().unwrap().0, 0);
        directory.prepared.as_ref().unwrap().recording.clone()
    };
    assert!(Arc::ptr_eq(
        &prepared,
        &archive.open(2, &mut || false).unwrap().unwrap()
    ));
    assert_value(&recording, 3);
    assert!(forward.cancel(&live).is_none());

    let reverse = recording.replay_lookahead(2, 0).unwrap();
    assert_eq!(reverse.frame(), 1);
    let cancelled = Arc::new(AtomicBool::new(false));
    assert!(reverse.prepare(&recording, cancelled.clone()).unwrap());
    assert!(reverse.cancel(&cancelled).is_some());
    assert!(reverse.prepare(&recording, cancelled).is_err());
    archive.directory.lock().unwrap().banks[0]
        .admission
        .as_mut()
        .unwrap()
        .index_bytes = BANK_INDEX_BYTES;
    assert!(
        !reverse
            .prepare(&recording, Arc::new(AtomicBool::new(false)))
            .unwrap()
    );
    assert_value(&recording, 1);
}

#[test]
fn replay_keeps_unchanged_objects_across_sealed_banks_and_the_hot_tail() {
    let directory = TestDirectory::new();
    let mut history = HistoryWriter::new_persistent(directory.config()).unwrap();
    let mut scene = state(0);
    let mut fixed = scene.objects[&7].clone();
    fixed.id = 8;
    scene.objects.insert(fixed.id, fixed);
    for index in 0..6 {
        scene
            .objects
            .get_mut(&7)
            .unwrap()
            .properties
            .insert("value".into(), Value::U64(index as u64));
        let patch = SceneUpdate::capture(&scene, &[7].into(), false);
        history.append(&info(index), &scene, &patch, &Analysis::default(), vec![]);
        if index == 1 || index == 3 {
            history.seal_bank(&source(&history)).unwrap();
        }
    }
    let recording = source(&history);
    let archive = history.archive.as_ref().unwrap();
    let (_, first_bank) = archive.bank(0).unwrap();
    let (_, second_bank) = archive.bank(2).unwrap();
    let BankLocation::File(path) = &second_bank.location else {
        panic!("temporary bank should be a file");
    };
    assert!(
        capture::open_bank(
            path,
            &capture::CaptureLimits::default(),
            first_bank.admission.unwrap(),
            |_| true,
        )
        .is_err()
    );
    let mut cursor = ReplayCursor::default();
    let mut visible = SceneState::default();
    for (position, frame) in [1, 2, 4, 3, 0, 5].into_iter().enumerate() {
        let replay = cursor.seek(&recording, frame, || false).unwrap();
        assert_eq!(replay.update.replace_all, position == 0);
        if position != 0 {
            let changed: Vec<_> = replay.update.changed_objects().map(|(id, _)| id).collect();
            assert_eq!(changed, [7]);
        }
        replay.update.apply(&mut visible);
        assert_eq!(visible, recording.state_at(frame).unwrap());
        assert_eq!(
            postcard::to_stdvec(&replay.analysis).unwrap(),
            postcard::to_stdvec(&recording.analysis_at(frame).unwrap()).unwrap(),
        );
    }
}

#[test]
fn analysis_failure_keeps_scene_history_recording_but_complete_export_stays_strict() {
    let directory = TestDirectory::new();
    let mut history = HistoryWriter::new_persistent(directory.config()).unwrap();
    for index in 0..3 {
        append(&mut history, index);
    }
    let diagnostic = "Analysis decoding stopped at frame 1: captured analysis buffer is incomplete";
    let mut old_snapshot = source(&history);
    old_snapshot.diagnostics.push(diagnostic.into());
    history.seal_bank(&old_snapshot).unwrap();
    assert_eq!(history.archive.as_ref().unwrap().len(), 1);
    assert!(history.completed.is_empty());
    assert!(history.warning.is_none());
    assert!(history.accepting_frames());

    for index in 3..6 {
        append(&mut history, index);
        assert!(history.warning.is_none(), "{:?}", history.warning);
        assert!(history.accepting_frames());
    }
    let mut current = source(&history);
    current.diagnostics = old_snapshot.diagnostics.clone();
    assert_eq!(current.first_frame(), 0);
    assert_eq!(current.frames.len(), 6);
    for index in [0, 2, 5, 3, 1, 4] {
        assert_value(&current, index);
    }
    assert_value(&old_snapshot, 0);
    assert_value(&old_snapshot, 2);
    assert!(old_snapshot.state_at(3).is_err());

    let cold = history
        .archive
        .as_ref()
        .unwrap()
        .open(0, &mut || false)
        .unwrap()
        .unwrap();
    assert!(cold.diagnostics.iter().any(|text| text == diagnostic));
    assert_value(&cold, 0);
    assert_value(&cold, 2);

    for retained in [&old_snapshot, &current, cold.as_ref()] {
        let mut output = std::io::Cursor::new(Vec::new());
        let failure = capture::write_retained(
            retained,
            &mut output,
            capture::CaptureRange {
                first: 0,
                last: retained.frames.last().unwrap().index,
            },
            &capture::CaptureLimits::default(),
            |_| true,
        )
        .unwrap_err();
        assert!(
            failure
                .to_string()
                .contains("cannot export complete analysis"),
            "{failure}"
        );
        assert!(output.into_inner().is_empty());
    }

    history.seal_bank(&current).unwrap();
    assert_eq!(history.archive.as_ref().unwrap().len(), 2);
    append(&mut history, 6);
    assert!(history.accepting_frames());
    assert!(history.warning.is_none(), "{:?}", history.warning);
    assert_eq!(history.frame_infos.len(), 7);
    assert_value(&recording(&history), 0);
    assert_value(&recording(&history), 6);
    let later_cold = history
        .archive
        .as_ref()
        .unwrap()
        .open(4, &mut || false)
        .unwrap()
        .unwrap();
    assert!(later_cold.diagnostics.iter().any(|text| text == diagnostic));
    assert_value(&later_cold, 4);
    assert_value(&old_snapshot, 1);
}

fn payload(index: usize) -> Vec<u8> {
    let mut seed = 0x9e37_79b9_7f4a_7c15_u64.wrapping_add(index as u64);
    (0..32 << 10)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as u8
        })
        .collect()
}

#[test]
fn cold_session_bytes_can_exceed_the_hot_budget_without_stopping_or_rolling() {
    let directory = TestDirectory::new();
    let mut config = directory.config();
    config.disk_budget_bytes = 256 << 10;
    let budget = config.disk_budget_bytes;
    let mut history = HistoryWriter::new_persistent(config).unwrap();
    let mut archived_bytes = 0;
    for index in 0..24 {
        history.prepare_persistent(&source(&history), 5).unwrap();
        let mut state = state(index as u64);
        state
            .objects
            .get_mut(&7)
            .unwrap()
            .properties
            .insert("payload".into(), Value::Bytes(payload(index).into()));
        let patch = SceneUpdate::capture(&state, &[7].into(), false);
        history.append(&info(index), &state, &patch, &Analysis::default(), vec![]);
        assert!(
            history.accepting_frames(),
            "frame {index}: {:?}",
            history.warning
        );
        assert!(
            history.warning.is_none(),
            "frame {index}: {:?}",
            history.warning
        );
        assert_eq!(history.frame_infos.len(), index + 1);
        assert_eq!(history.frame_infos.first().unwrap().index, 0);
        let bytes = history.archive.as_ref().unwrap().bytes();
        assert!(bytes >= archived_bytes);
        archived_bytes = bytes;
    }
    assert!(
        archived_bytes > budget,
        "cold bytes: {archived_bytes}, budget: {budget}"
    );
    let recording = source(&history);
    let stats = &recording.disk.as_ref().unwrap().stats;
    assert!(stats.disk_bytes > budget);
    assert!(stats.cache_disk_bytes <= budget);
    assert_eq!(stats.first_frame, 0);
    assert_eq!(stats.retained_frames, 24);
    assert!(stats.warning.is_none());
    for index in 0..24 {
        let state = recording.state_at(index).unwrap();
        assert_eq!(
            state.objects[&7].properties["value"],
            Value::U64(index as u64)
        );
        assert_eq!(
            state.objects[&7].properties["payload"],
            Value::Bytes(payload(index).into())
        );
    }
}

#[test]
fn sealing_waits_for_an_inflight_hot_read_then_old_snapshot_reads_the_cold_bank() {
    use std::sync::mpsc;
    let directory = TestDirectory::new();
    let mut history = HistoryWriter::new_persistent(directory.config()).unwrap();
    append(&mut history, 0);
    append(&mut history, 1);
    let old = source(&history);
    let reader_snapshot = old.clone();
    let (reading, read_started) = mpsc::channel();
    let (resume, resumed) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut checks = 0;
        reader_snapshot
            .state_at_with_cancel(0, || {
                checks += 1;
                // The first check precedes routing; later checks run while reading
                // the selected segment under its transition guard.
                if checks == 3 {
                    reading.send(()).unwrap();
                    resumed.recv().unwrap();
                }
                false
            })
            .unwrap()
    });
    read_started.recv_timeout(Duration::from_secs(5)).unwrap();
    let (finished, publication) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        history.seal_bank(&source(&history)).unwrap();
        finished.send(()).unwrap();
        history
    });
    assert!(matches!(
        publication.recv_timeout(Duration::from_millis(50)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    resume.send(()).unwrap();
    assert_eq!(
        reader.join().unwrap().objects[&7].properties["value"],
        Value::U64(0)
    );
    publication.recv_timeout(Duration::from_secs(5)).unwrap();
    let history = writer.join().unwrap();
    assert_eq!(history.archive.as_ref().unwrap().len(), 1);
    assert_value(&old, 0);
    assert_value(&old, 1);
}
