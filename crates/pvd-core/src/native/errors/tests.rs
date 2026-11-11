use super::super::tests::{catalog_builder, error_fixture, error_payload};
use super::*;

fn snapshot_errors(records: &[(u32, &[u32])]) -> (Metadata, Vec<u8>, Vec<u8>) {
    let (mut metadata, _) = error_fixture();
    metadata.classes[3].as_mut().unwrap().size = 12;
    let mut backing = Vec::new();
    let mut directory = (records.len() as u32).to_le_bytes().to_vec();
    for &(id, indices) in records {
        directory.extend(id.to_le_bytes());
        directory.extend((backing.len() as u64).to_le_bytes());
        let mut body = 12u32.to_le_bytes().to_vec();
        body.extend([0xa5; 12]);
        body.extend((indices.len() as u32 * 20).to_le_bytes());
        for &index in indices {
            body.extend(error_payload(index));
        }
        backing.extend(3u32.to_le_bytes());
        backing.extend(id.to_le_bytes());
        backing.extend((body.len() as u32).to_le_bytes());
        backing.extend(body);
    }
    (metadata, backing, directory)
}

#[test]
fn initial_errors_keep_container_array_order_duplicates_and_no_frame_position() {
    let (metadata, backing, directory) = snapshot_errors(&[(29, &[9, 2, 2]), (7, &[4, 0])]);
    let mut builder = catalog_builder();
    metadata.register(&mut builder).unwrap();
    snapshot::restore(
        &metadata,
        &mut builder,
        &backing,
        &directory,
        &mut 0,
        &mut || true,
    )
    .unwrap();
    let recording = builder.finish().unwrap();
    assert_eq!(recording.frame_count(), 0);
    assert!(recording.untimed_error_events().is_empty());
    assert_eq!(
        recording.initial_untimed_errors(),
        &[
            expected(29, 9),
            expected(29, 2),
            expected(29, 2),
            expected(7, 4),
            expected(7, 0),
        ]
    );
    assert_eq!(recording.initial_state().objects.len(), 2);
    assert!(recording.error_events().next().is_none());
}

fn expected(id: u64, index: u32) -> InitialUntimedError {
    InitialUntimedError {
        code: 4,
        message: "untimed error".into(),
        file: "source.cpp".into(),
        line: 248,
        index,
        container_instance: id,
        container_generation: 0,
    }
}

#[test]
fn initial_errors_survive_container_destruction_and_do_not_repeat_on_replay() {
    let (metadata, backing, directory) = snapshot_errors(&[(29, &[42])]);
    let mut builder = catalog_builder();
    metadata.register(&mut builder).unwrap();
    snapshot::restore(
        &metadata,
        &mut builder,
        &backing,
        &directory,
        &mut 0,
        &mut || true,
    )
    .unwrap();
    for timestamp in [100, 200] {
        if timestamp == 200 {
            push(&mut builder, Event::DestroyInstance { id: 29 }).unwrap();
        }
        push(
            &mut builder,
            Event::EndSection {
                id: 1,
                name: 15,
                timestamp,
            },
        )
        .unwrap();
    }
    let recording = builder.finish().unwrap();
    for frame in [1, 0, 1, 0] {
        let state = recording.state_at(frame).unwrap();
        assert_eq!(state.objects.contains_key(&29), frame == 0);
        assert_eq!(recording.initial_untimed_errors(), &[expected(29, 42)]);
        assert!(recording.untimed_error_events().is_empty());
    }
    recording.verify_replay().unwrap();
}

#[test]
fn initial_and_received_error_lists_share_the_record_limit() {
    let (metadata, mut builder) = error_fixture();
    metadata
        .restore_initial_errors(&mut builder, 29, &error_payload(4), &mut || true)
        .unwrap();
    metadata
        .append_error(&mut builder, 29, &error_payload(3), 10)
        .unwrap();
    assert!(check_error_count(&builder.recording, MAX_ITEMS - 2).is_ok());
    for additional in [MAX_ITEMS - 1, MAX_ITEMS, usize::MAX] {
        assert!(
            check_error_count(&builder.recording, additional)
                .unwrap_err()
                .to_string()
                .contains("one million")
        );
    }
    assert_eq!(
        builder.recording.initial_untimed_errors(),
        &[expected(29, 4)]
    );
    assert_eq!(builder.recording.untimed_error_events()[0].index, 3);
}

#[test]
fn initial_errors_reject_invalid_payload_handles_and_budget_before_copying() {
    let (mut metadata, mut builder) = error_fixture();
    for length in 1..20 {
        assert!(
            metadata
                .restore_initial_errors(&mut builder, 29, &error_payload(0)[..length], &mut || true)
                .is_err()
        );
    }
    for offset in [4, 8] {
        let mut data = error_payload(0);
        data[offset..offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(
            metadata
                .restore_initial_errors(&mut builder, 29, &data, &mut || true)
                .unwrap_err()
                .to_string()
                .contains("undefined PVD3 error string")
        );
    }
    metadata.strings.insert(13, "x".repeat(4096));
    builder.memory_limit = builder.retained_bytes + 1024;
    assert!(
        metadata
            .restore_initial_errors(&mut builder, 29, &error_payload(0), &mut || true)
            .unwrap_err()
            .to_string()
            .contains("history memory budget")
    );
    assert!(builder.recording.initial_untimed_errors().is_empty());
    assert!(builder.recording.untimed_error_events().is_empty());
}

#[test]
fn initial_error_scans_check_cancellation_per_record() {
    let (metadata, mut builder) = error_fixture();
    let data = [error_payload(7), error_payload(9)].concat();
    let failure = metadata
        .restore_initial_errors(&mut builder, 29, &data, &mut || false)
        .unwrap_err();
    assert_eq!(
        failure.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
    assert!(builder.recording.initial_untimed_errors().is_empty());
    let mut checks = 0;
    let failure = metadata
        .restore_initial_errors(&mut builder, 29, &data, &mut || {
            checks += 1;
            checks == 1
        })
        .unwrap_err();
    assert_eq!(
        failure.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
    assert_eq!(checks, 2);
    assert_eq!(
        builder.recording.initial_untimed_errors(),
        &[expected(29, 7)]
    );
}

#[test]
fn error_strings_share_the_disk_scene_budget_before_copying() {
    let (mut metadata, _) = error_fixture();
    metadata.strings.insert(13, "x".repeat(4096));
    let config = super::super::container_tests::file_history_config("error-budget");
    let directory = config.directory.clone();
    let history = crate::live::HistoryWriter::new_paged(config).unwrap();
    let mut builder = catalog_builder();
    builder.streaming = Some(Box::new(crate::live::Streaming::new(history)));
    metadata.register(&mut builder).unwrap();
    push(
        &mut builder,
        Event::CreateInstance {
            class: metadata.classes[3].as_ref().unwrap().name,
            id: 29,
        },
    )
    .unwrap();
    let state_bytes = builder.streaming.as_ref().unwrap().state_bytes;
    assert!(state_bytes > 0);
    let retained = builder.retained_bytes;
    for initial in [true, false] {
        let width = if initial {
            size_of::<InitialUntimedError>()
        } else {
            size_of::<UntimedErrorEvent>()
        };
        let bytes = 4096 + "source.cpp".len() + 4 * width;
        builder.memory_limit = retained + state_bytes + bytes - 1;
        assert!(bytes <= builder.memory_limit - retained);
        let failure = if initial {
            metadata.restore_initial_errors(&mut builder, 29, &error_payload(0), &mut || true)
        } else {
            metadata.append_error(&mut builder, 29, &error_payload(0), 1)
        }
        .unwrap_err();
        assert!(
            failure.to_string().contains("history memory budget"),
            "{failure}"
        );
    }
    assert_eq!(builder.retained_bytes, retained);
    assert!(builder.recording.initial_untimed_errors().is_empty());
    assert!(builder.recording.untimed_error_events().is_empty());
    drop(builder);
    let _ = std::fs::remove_dir(directory);
}

#[test]
fn zero_frame_disk_import_keeps_state_and_error_memory_accounted() {
    let (metadata, backing, directory) = snapshot_errors(&[(29, &[7, 9])]);
    let config = super::super::container_tests::file_history_config("zero-frame-errors");
    let cache = config.directory.clone();
    let history = crate::live::HistoryWriter::new_paged(config).unwrap();
    let mut builder = catalog_builder();
    builder.streaming = Some(Box::new(crate::live::Streaming::new(history)));
    metadata.register(&mut builder).unwrap();
    snapshot::restore(
        &metadata,
        &mut builder,
        &backing,
        &directory,
        &mut 0,
        &mut || true,
    )
    .unwrap();
    let before = builder.memory_usage().estimated_history_bytes;
    assert!(before > builder.retained_bytes);
    let recording = import::finish_recording(builder).unwrap();
    assert_eq!(
        recording.history_memory_usage().estimated_history_bytes,
        before
    );
    assert_eq!(recording.frame_count(), 0);
    assert!(!recording.is_disk_backed());
    assert_eq!(
        recording.initial_untimed_errors(),
        &[expected(29, 7), expected(29, 9)]
    );
    assert!(recording.initial_state().objects.contains_key(&29));
    drop(recording);
    let _ = std::fs::remove_dir(cache);
}
