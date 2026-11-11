use pvd_core::{MemoryHistoryScope, NativeMemoryTotals, Recording, analysis::MemoryState, native};
use std::{collections::BTreeMap, path::PathBuf};

fn fixture(name: &str) -> PathBuf {
    std::env::var_os("PVD_NATIVE_MEMORY_DIRECTORY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../captures/native"))
        .join(name)
}

fn verify_sources(totals: &NativeMemoryTotals, expected: &MemoryState) {
    let mut grouped = BTreeMap::<(String, String, u32), u64>::new();
    for allocation in expected.live.values() {
        *grouped
            .entry((
                allocation.type_name.to_string(),
                allocation.file.to_string(),
                allocation.line,
            ))
            .or_default() += allocation.size;
    }
    assert_eq!(totals.nonnegative_bytes(), Some(expected.live_bytes));
    for index in 0..totals.len() {
        let row = totals.row(index).unwrap();
        assert_eq!(row.index, index as u32);
        assert_eq!(
            u64::try_from(row.bytes).unwrap(),
            grouped
                .remove(&(row.type_name.into(), row.file.into(), row.line))
                .unwrap_or(0),
            "source {row:?}"
        );
    }
    assert!(
        grouped.values().all(|bytes| *bytes == 0),
        "live sources absent from captured totals: {grouped:?}"
    );
    assert!(totals.row(totals.len()).is_none());
}

#[test]
#[ignore = "requires the exact original rich paired native container and SDK stream"]
fn original_rich_source_totals_match_independent_sdk_live_allocations() {
    let path = fixture("rich-paired-native.pxd2");
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.len(), 1_034_096);
    let (memory, info) = native::read_with_progress(&bytes, |_, _| true).unwrap();
    assert_eq!(info.history_events, 3457);
    assert_eq!(memory.frame_count(), 120);
    let raw = Recording::from_path(fixture("rich-paired.pvdstream")).unwrap();
    assert_eq!(raw.frame_count(), 120);
    assert_eq!(
        raw.memory_history_scope(),
        MemoryHistoryScope::CapturedEvents
    );
    assert!(raw.native_initial_memory_totals().unwrap().is_none());
    let expected = &raw.analysis.legacy().unwrap().memory;
    assert_eq!(expected.events().len(), 7549);
    assert_eq!(expected.samples.len(), 135);
    assert_eq!(expected.state_at_frame(120).live_bytes, 552_366);

    let directory = std::env::temp_dir().join(format!(
        "pvd-native-memory-rich-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let config = pvd_core::live::HistoryConfig {
        directory: directory.clone(),
        disk_budget_bytes: 128 << 20,
        segment_frames: 17,
        min_free_disk_bytes: 0,
        ..Default::default()
    };
    let (mut disk, disk_info) =
        native::read_path_with_progress(&path, config, |_, _| true).unwrap();
    assert_eq!(disk_info.source_blake3, info.source_blake3);
    assert!(disk.is_disk_backed());
    for recording in [&memory, &disk] {
        assert_eq!(
            recording.memory_history_scope(),
            MemoryHistoryScope::CapturedEvents
        );
        assert!(recording.native_initial_memory_totals().unwrap().is_none());
        for frame in (0..120).map(|index| index * 73 % 120) {
            let state = recording.state_at(frame).unwrap();
            let totals = recording.native_memory_totals_at(&state).unwrap().unwrap();
            verify_sources(&totals, &expected.state_at_frame(frame));
            assert!(totals.matches_state(recording, &state));
        }
        let end = recording.native_end_memory_totals().unwrap().unwrap();
        assert_eq!(end.len(), 273);
        verify_sources(&end, &expected.state_at_frame(120));
    }
    let retained = disk.native_end_memory_totals().unwrap().unwrap();
    let state = disk.state_at(119).unwrap();
    let current = disk.native_memory_totals_at(&state).unwrap().unwrap();
    assert!(!current.matches_state(&memory, &state));
    assert!(!retained.matches_state(&disk, &state));
    disk.analysis.retain_recent(119).unwrap();
    disk.analysis = disk.analysis_at(0).unwrap();
    verify_sources(
        &disk.native_end_memory_totals().unwrap().unwrap(),
        &expected.state_at_frame(120),
    );
    drop(disk);
    verify_sources(&retained, &expected.state_at_frame(120));
    let _ = std::fs::remove_dir(directory);
}
