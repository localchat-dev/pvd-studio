use super::*;
use crate::analysis::view::MemorySource;
use crate::analysis_export::{AnalysisCsvKind, write_analysis_csv};

mod fixture;
use fixture::*;

fn collected(values: &[i32]) -> (Metadata, RecordingBuilder, Collector) {
    let (metadata, mut builder) = builder();
    let mut collector = Collector::new(catalog(), &metadata, &mut builder, &mut || true).unwrap();
    create(&metadata, &mut builder, 1, 0);
    create(&metadata, &mut builder, 2, 3);
    create(&metadata, &mut builder, 3, 4);
    set_totals(&metadata, &mut builder, values);
    collector
        .initialize(&mut builder, 3, &mut 0, &mut || true)
        .unwrap();
    (metadata, builder, collector)
}

fn values(totals: &NativeMemoryTotals) -> Vec<i32> {
    (0..totals.len())
        .map(|index| totals.row(index).unwrap().bytes)
        .collect()
}

#[test]
fn shared_views_keep_signed_values_index_order_and_future_sources_absent() {
    let (_, mut builder, collector) = collected(&[64, -2]);
    collector
        .finish(&mut builder, &mut 0, &mut || true)
        .unwrap();
    let recording = builder.finish().unwrap();
    let initial = recording.native_initial_memory_totals().unwrap().unwrap();
    let end = recording.native_end_memory_totals().unwrap().unwrap();
    assert_eq!(initial.len(), 2);
    assert!(!initial.is_empty());
    assert_eq!(values(&initial), vec![64, -2]);
    assert_eq!(initial.nonnegative_bytes(), None);
    assert!(initial.row(2).is_none());
    assert_eq!(
        initial.row(0).unwrap(),
        NativeMemoryTotal {
            index: 0,
            type_name: "Allocation",
            file: "source.cpp",
            line: 7,
            bytes: 64,
        }
    );
    assert!(Arc::ptr_eq(&initial.catalog, &end.catalog));
    assert!(Arc::ptr_eq(&initial.values, &end.values));
    assert!(initial.matches_state(&recording, recording.initial_state()));
    drop(recording);
    assert_eq!(initial.row(1).unwrap().type_name, "Other");
}

#[test]
fn cache_matches_recording_identity_generation_and_shared_array_only() {
    let (_, mut builder, collector) = collected(&[7, 8]);
    let state = builder.state.clone();
    collector
        .finish(&mut builder, &mut 0, &mut || true)
        .unwrap();
    let recording = builder.finish().unwrap();
    let totals = recording.native_memory_totals_at(&state).unwrap().unwrap();
    assert_eq!(totals.nonnegative_bytes(), Some(15));
    assert!(totals.matches_state(&recording.clone(), &state.clone()));
    let mut changed = state.clone();
    changed.objects.get_mut(&3).unwrap().properties.insert(
        "totals".into(),
        Value::Array(vec![Value::I64(7), Value::I64(8)].into()),
    );
    assert!(!totals.matches_state(&recording, &changed));
    changed = state.clone();
    changed.objects.get_mut(&2).unwrap().generation += 1;
    assert!(!totals.matches_state(&recording, &changed));
    changed = state.clone();
    changed.objects.remove(&2);
    assert!(!totals.matches_state(&recording, &changed));
    let (_, mut other, collector) = collected(&[7, 8]);
    collector.finish(&mut other, &mut 0, &mut || true).unwrap();
    let other = other.finish().unwrap();
    assert!(!totals.matches_state(&other, &state));
    assert!(!totals.matches_state(&recording, other.initial_state()));
}

#[test]
fn current_totals_reject_malformed_values_without_partial_rows() {
    let (_, mut builder, collector) = collected(&[1]);
    let state = builder.state.clone();
    collector
        .finish(&mut builder, &mut 0, &mut || true)
        .unwrap();
    let recording = builder.finish().unwrap();
    for invalid in [
        Value::U64(1),
        Value::Array(vec![Value::U64(1)].into()),
        Value::Array(vec![Value::I64(i64::from(i32::MAX) + 1)].into()),
        Value::Array(vec![Value::I64(0); 4].into()),
    ] {
        let mut changed = state.clone();
        changed
            .objects
            .get_mut(&3)
            .unwrap()
            .properties
            .insert("totals".into(), invalid);
        assert!(
            recording
                .native_memory_totals_at(&changed)
                .unwrap_err()
                .to_string()
                .contains("signed source array")
        );
    }
    let mut changed = state.clone();
    changed
        .objects
        .get_mut(&3)
        .unwrap()
        .properties
        .remove("totals");
    assert!(
        recording
            .native_memory_totals_at(&changed)
            .unwrap()
            .is_none()
    );
    changed.objects.remove(&2);
    assert!(
        recording
            .native_memory_totals_at(&changed)
            .unwrap_err()
            .to_string()
            .contains("live buffer")
    );
}

#[test]
fn zero_frame_native_import_retains_initial_totals_without_replaying_buffer() {
    let bytes = container(&initial_memory(&[64, 16]), &[]);
    for disk in [false, true] {
        let config = super::super::container_tests::file_history_config("memory-zero");
        let directory = config.directory.clone();
        let (recording, _) = import::read_source_with_progress(
            std::io::Cursor::new(&bytes),
            disk.then_some(config),
            |_, _| true,
        )
        .unwrap();
        assert_eq!(recording.frame_count(), 0);
        assert!(!recording.is_disk_backed());
        assert_eq!(
            recording.memory_history_scope(),
            MemoryHistoryScope::AfterInitialSnapshot
        );
        assert_eq!(
            values(&recording.native_initial_memory_totals().unwrap().unwrap()),
            vec![64, 16]
        );
        assert_eq!(
            recording
                .native_end_memory_totals()
                .unwrap()
                .unwrap()
                .nonnegative_bytes(),
            Some(80)
        );
        let memory = recording.analysis.memory_snapshot();
        assert_eq!(memory.event_count(), 0);
        assert_eq!(memory.sample_count(), 0);
        assert_eq!(memory.summary().allocations, 0);
        assert_eq!(memory.summary().live_bytes, 0);
        assert!(
            matches!(&recording.initial_state().objects[&2].properties["events"], Value::Bytes(data) if data.as_ref() == [2, 0, 64, 13, 14, 7, 42])
        );
        let mut output = Vec::new();
        assert!(
            write_analysis_csv(&recording, &mut output, AnalysisCsvKind::Memory, |_| true)
                .unwrap_err()
                .to_string()
                .contains("before the initial snapshot")
        );
        assert!(output.is_empty());
        drop(recording);
        if disk {
            let _ = std::fs::remove_dir(directory);
        }
    }
}

#[test]
fn imported_frame_and_end_totals_have_distinct_values_and_observed_free_only() {
    let bytes = container(
        &initial_memory(&[64, 16]),
        &[
            set_event(3, 1, &packed(&[32, 16])),
            set_event(2, 0, &[3, 0, 42]),
            end_frame(100),
            set_event(3, 1, &packed(&[20, 8, 4])),
            end_frame(200),
            set_event(3, 1, &packed(&[5, 6, 7])),
        ],
    );
    for disk in [false, true] {
        let config = super::super::container_tests::file_history_config("memory-frames");
        let directory = config.directory.clone();
        let (recording, _) = import::read_source_with_progress(
            std::io::Cursor::new(&bytes),
            disk.then_some(config),
            |_, _| true,
        )
        .unwrap();
        assert_eq!(recording.is_disk_backed(), disk);
        assert_eq!(
            values(&recording.native_initial_memory_totals().unwrap().unwrap()),
            vec![64, 16]
        );
        assert_eq!(
            values(&recording.native_end_memory_totals().unwrap().unwrap()),
            vec![5, 6, 7]
        );
        for frame in [1, 0, 1, 0] {
            let state = recording.state_at(frame).unwrap();
            let totals = recording.native_memory_totals_at(&state).unwrap().unwrap();
            assert_eq!(
                values(&totals),
                if frame == 0 {
                    vec![32, 16]
                } else {
                    vec![20, 8, 4]
                }
            );
            assert!(totals.matches_state(&recording, &state));
        }
        let summary = recording.analysis.memory_snapshot().summary();
        assert_eq!(summary.allocations, 0);
        assert_eq!(summary.frees, 1);
        assert_eq!(summary.unmatched_frees, 1);
        assert_eq!(summary.live_bytes, 0);
        assert_eq!(summary.total_allocated_bytes, 0);
        recording.verify_replay().unwrap();
        let mut output = std::io::Cursor::new(Vec::new());
        assert!(
            crate::capture::write_retained(
                &recording,
                &mut output,
                crate::capture::CaptureRange { first: 0, last: 1 },
                &Default::default(),
                |_| true
            )
            .unwrap_err()
            .to_string()
            .contains("SDK history or a project capture")
        );
        assert!(output.into_inner().is_empty());
        drop(recording);
        if disk {
            let _ = std::fs::remove_dir(directory);
        }
    }
}

#[test]
fn ordinary_creates_keep_captured_scope_and_have_no_initial_totals() {
    let bytes = container(
        &[],
        &[
            create_event(1, 0),
            create_event(2, 3),
            create_event(3, 4),
            set_event(3, 1, &packed(&[7, 8])),
            end_frame(100),
        ],
    );
    let recording = from_bytes_with_progress(&bytes, |_, _| true).unwrap();
    assert_eq!(
        recording.memory_history_scope(),
        MemoryHistoryScope::CapturedEvents
    );
    assert!(recording.native_initial_memory_totals().unwrap().is_none());
    assert_eq!(
        recording
            .native_end_memory_totals()
            .unwrap()
            .unwrap()
            .nonnegative_bytes(),
        Some(15)
    );
    let mut output = Vec::new();
    write_analysis_csv(&recording, &mut output, AnalysisCsvKind::Memory, |_| true).unwrap();
    assert!(!output.is_empty());
}

#[test]
fn unrelated_initial_objects_still_preclude_a_complete_memory_history() {
    let bytes = container(&[(1, 0, Vec::new())], &[end_frame(100)]);
    let recording = from_bytes_with_progress(&bytes, |_, _| true).unwrap();
    assert_eq!(
        recording.memory_history_scope(),
        MemoryHistoryScope::AfterInitialSnapshot
    );
    assert!(recording.native_initial_memory_totals().unwrap().is_none());
    let mut output = Vec::new();
    assert!(
        write_analysis_csv(&recording, &mut output, AnalysisCsvKind::Memory, |_| true).is_err()
    );
    assert!(output.is_empty());
    let (_, builder) = builder();
    let sdk = builder.finish().unwrap();
    assert_eq!(
        sdk.memory_history_scope(),
        MemoryHistoryScope::CapturedEvents
    );
    assert!(sdk.native_initial_memory_totals().unwrap().is_none());
}

#[test]
fn catalog_rejects_conflicting_indices_missing_pairs_and_shared_metadata_overflow() {
    let sources = BTreeMap::from([(2, 20), (3, 30)]);
    for (ids, indices) in [
        ([2, 3], [0, 0]),
        ([2, 3], [0, 2]),
        ([2, 2], [0, 1]),
        ([4, 3], [0, 1]),
        ([u32::MAX, 3], [0, 1]),
    ] {
        let bytes = catalog_bytes(ids, &[("A", "a", 1, indices[0]), ("B", "b", 2, indices[1])]);
        let catalog =
            Catalog::read(&mut Reader::new(&bytes), &sources, &mut 0, &mut || true).unwrap();
        let (metadata, mut builder) = builder();
        let collector = Collector::new(catalog, &metadata, &mut builder, &mut || true).unwrap();
        collector
            .finish(&mut builder, &mut 0, &mut || true)
            .unwrap();
        assert!(
            builder
                .finish()
                .unwrap()
                .native_end_memory_totals()
                .is_err()
        );
    }
    let bytes = catalog_bytes([2, 3], &[("A", &"x".repeat(4096), 1, 0)]);
    let catalog = Catalog::read(&mut Reader::new(&bytes), &sources, &mut 0, &mut || true).unwrap();
    let mut allocation = MAX_METADATA - catalog.bytes;
    Catalog::read(
        &mut Reader::new(&bytes),
        &sources,
        &mut allocation,
        &mut || true,
    )
    .unwrap();
    assert_eq!(allocation, MAX_METADATA);
    let mut allocation = MAX_METADATA - catalog.bytes + 1;
    assert!(
        Catalog::read(
            &mut Reader::new(&bytes),
            &sources,
            &mut allocation,
            &mut || true
        )
        .err()
        .unwrap()
        .to_string()
        .contains("metadata exceeds 64 MiB")
    );
}

#[test]
fn unsupported_schema_extra_buffers_and_recreated_ids_are_explicitly_unavailable() {
    let (mut metadata, mut builder) = builder();
    metadata.classes[4].as_mut().unwrap().size = 12;
    let collector = Collector::new(catalog(), &metadata, &mut builder, &mut || true).unwrap();
    collector
        .finish(&mut builder, &mut 0, &mut || true)
        .unwrap();
    assert!(
        builder
            .finish()
            .unwrap()
            .native_end_memory_totals()
            .unwrap_err()
            .to_string()
            .contains("class layout")
    );
    for recreation in [false, true] {
        let (metadata, mut builder, mut collector) = collected(&[9]);
        if recreation {
            push(&mut builder, Event::DestroyInstance { id: 2 }).unwrap();
        }
        let id = if recreation { 2 } else { 4 };
        create(&metadata, &mut builder, id, 3);
        collector.observe_created(&builder, id as u32);
        collector
            .finish(&mut builder, &mut 0, &mut || true)
            .unwrap();
        let recording = builder.finish().unwrap();
        assert!(
            recording
                .native_initial_memory_totals()
                .unwrap_err()
                .to_string()
                .contains("lifetimes")
        );
        assert!(recording.native_end_memory_totals().is_err());
    }
}

#[test]
fn empty_catalog_does_not_assign_a_sentinel_id_to_an_ordinary_object() {
    let bytes = catalog_bytes([u32::MAX; 2], &[]);
    let catalog = Catalog::read(
        &mut Reader::new(&bytes),
        &BTreeMap::new(),
        &mut 0,
        &mut || true,
    )
    .unwrap();
    let (metadata, mut builder) = builder();
    let mut collector = Collector::new(catalog, &metadata, &mut builder, &mut || true).unwrap();
    create(&metadata, &mut builder, u64::from(u32::MAX), 0);
    collector
        .initialize(&mut builder, 1, &mut 0, &mut || true)
        .unwrap();
    collector
        .finish(&mut builder, &mut 0, &mut || true)
        .unwrap();
    let recording = builder.finish().unwrap();
    assert!(recording.native_initial_memory_totals().unwrap().is_none());
    assert_eq!(
        recording.memory_history_scope(),
        MemoryHistoryScope::AfterInitialSnapshot
    );
}

#[test]
fn catalog_and_saved_arrays_include_the_disk_scene_in_the_shared_budget() {
    let config = super::super::container_tests::file_history_config("memory-budget");
    let directory = config.directory.clone();
    let history = crate::live::HistoryWriter::new_paged(config).unwrap();
    let (metadata, mut builder) = builder();
    builder.streaming = Some(Box::new(crate::live::Streaming::new(history)));
    create(&metadata, &mut builder, 2, 3);
    create(&metadata, &mut builder, 3, 4);
    set_totals(&metadata, &mut builder, &[7, 8]);
    let state_bytes = builder.streaming.as_ref().unwrap().state_bytes;
    let index_bytes = builder.recording.frame_sources.index_bytes();
    assert!(state_bytes > 0);
    let retained = builder.retained_bytes;
    let bytes = catalog().bytes;
    builder.memory_limit = retained + index_bytes + state_bytes + bytes - 1;
    assert!(bytes <= builder.memory_limit - retained);
    assert!(
        Collector::new(catalog(), &metadata, &mut builder, &mut || true)
            .err()
            .unwrap()
            .to_string()
            .contains("history memory budget")
    );
    assert_eq!(builder.retained_bytes, retained);
    builder.memory_limit = usize::MAX;
    let mut collector = Collector::new(catalog(), &metadata, &mut builder, &mut || true).unwrap();
    let retained = builder.retained_bytes;
    let Value::Array(values) = &builder.state.objects[&3].properties["totals"] else {
        panic!()
    };
    let bytes = values.capacity() * size_of::<Value>() + size_of::<Vec<Value>>() + 16;
    builder.memory_limit = retained + index_bytes + state_bytes + bytes - 1;
    assert!(bytes <= builder.memory_limit - retained);
    assert!(
        collector
            .initialize(&mut builder, 2, &mut 0, &mut || true)
            .unwrap_err()
            .to_string()
            .contains("history memory budget")
    );
    assert_eq!(builder.retained_bytes, retained);
    assert!(matches!(collector.0.initial, SavedTotals::Missing));
    assert!(builder.recording.native_memory.is_none());
    builder.memory_limit += 1;
    collector
        .initialize(&mut builder, 2, &mut 0, &mut || true)
        .unwrap();
    assert_eq!(builder.retained_bytes, retained + bytes);
    let retained = builder.retained_bytes;
    collector
        .finish(&mut builder, &mut 0, &mut || true)
        .unwrap();
    assert_eq!(builder.retained_bytes, retained);
    drop(builder);
    let _ = std::fs::remove_dir(directory);
}

#[test]
fn initial_and_end_arrays_share_the_metadata_ceiling() {
    let (metadata, mut builder, collector) = collected(&[7, 8]);
    set_totals(&metadata, &mut builder, &[9, 10, 11]);
    let retained = builder.retained_bytes;
    let bytes = 3 * size_of::<Value>() + size_of::<Vec<Value>>() + 16;
    let mut allocation = MAX_METADATA - bytes + 1;
    assert!(
        collector
            .finish(&mut builder, &mut allocation, &mut || true)
            .unwrap_err()
            .to_string()
            .contains("metadata exceeds 64 MiB")
    );
    assert!(builder.recording.native_memory.is_none());
    assert_eq!(builder.retained_bytes, retained);
}

#[test]
fn cancellation_interrupts_catalog_current_scan_and_publication() {
    let bytes = catalog_bytes([2, 3], &[("A", "a", 1, 0)]);
    assert!(
        Catalog::read(
            &mut Reader::new(&bytes),
            &BTreeMap::new(),
            &mut 0,
            &mut || false
        )
        .is_err()
    );
    let (_, mut builder, collector) = collected(&[7, 8]);
    let error = collector
        .finish(&mut builder, &mut 0, &mut || false)
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
    assert!(builder.recording.native_memory.is_none());
    let (_, mut builder, mut collector) = collected(&[7, 8]);
    let mut catalog = Catalog {
        ids: [2, 3],
        sites: Vec::new(),
        indices: Vec::new(),
        problem: None,
        bytes: 0,
    };
    for index in 0..600 {
        catalog.sites.push(Site {
            type_name: "A".into(),
            file: "a".into(),
            line: 1,
        });
        catalog.indices.push(index);
    }
    collector.0.catalog = Arc::new(catalog);
    collector
        .finish(&mut builder, &mut 0, &mut || true)
        .unwrap();
    let recording = builder.finish().unwrap();
    let mut state = recording.initial_state().clone();
    state.objects.get_mut(&3).unwrap().properties.insert(
        "totals".into(),
        Value::Array(vec![Value::I64(1); 600].into()),
    );
    let mut checks = 0;
    let error = recording
        .native_memory_totals_at_with_cancel(&state, || {
            checks += 1;
            checks == 4
        })
        .unwrap_err();
    assert_eq!(checks, 4);
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
    assert_eq!(
        recording
            .native_memory_totals_at(&state)
            .unwrap()
            .unwrap()
            .nonnegative_bytes(),
        Some(600)
    );
}

#[test]
#[ignore = "writes constructed fixtures to PVD_NATIVE_MEMORY_FIXTURE_DIRECTORY"]
fn write_constructed_memory_totals_fixtures() {
    let directory = std::path::PathBuf::from(
        std::env::var_os("PVD_NATIVE_MEMORY_FIXTURE_DIRECTORY").expect("fixture output directory"),
    );
    std::fs::create_dir_all(&directory).unwrap();
    let captures = [
        (
            "memory-initial-zero.pxd2",
            container(&initial_memory(&[64, 16]), &[]),
        ),
        (
            "memory-initial-frames.pxd2",
            container(
                &initial_memory(&[64, 16]),
                &[
                    set_event(3, 1, &packed(&[32, 16])),
                    set_event(2, 0, &[3, 0, 42]),
                    end_frame(100),
                    set_event(3, 1, &packed(&[20, 8, 4])),
                    end_frame(200),
                    set_event(3, 1, &packed(&[5, 6, 7])),
                ],
            ),
        ),
        (
            "memory-current.pxd2",
            container(
                &[],
                &[
                    create_event(1, 0),
                    create_event(2, 3),
                    create_event(3, 4),
                    set_event(3, 1, &packed(&[7, 8])),
                    end_frame(100),
                ],
            ),
        ),
    ];
    for (name, bytes) in captures {
        let recording = from_bytes_with_progress(&bytes, |_, _| true).unwrap();
        assert!(recording.native_end_memory_totals().unwrap().is_some());
        std::fs::write(directory.join(name), bytes).unwrap();
    }
}
