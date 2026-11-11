use pvd_core::{MemoryHistoryScope, NativeMemoryTotals, Recording, native};
use std::path::PathBuf;

#[path = "support/native_memory_interval.rs"]
mod native_memory_interval;

fn fixture(name: &str) -> PathBuf {
    std::env::var_os("PVD_NATIVE_MEMORY_DIRECTORY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../captures/native"))
        .join(name)
}

fn totals_match(actual: &NativeMemoryTotals, expected: &NativeMemoryTotals) {
    assert_eq!(actual.len(), expected.len());
    assert_eq!(actual.nonnegative_bytes(), expected.nonnegative_bytes());
    for index in 0..expected.len() {
        assert_eq!(actual.row(index), expected.row(index));
    }
}

#[test]
#[ignore = "requires the exact original rich paired native container and SDK stream"]
fn original_rich_middle_preserves_source_totals_and_partial_sdk_event_history() {
    let path = fixture("rich-paired-native.pxd2");
    let source = std::fs::read(&path).unwrap();
    assert_eq!(source.len(), 1_034_096);
    let (original, info) = native::read_with_progress(&source, |_, _| true).unwrap();
    assert_eq!((original.frame_count(), info.history_events), (120, 3457));
    let raw = Recording::from_path(fixture("rich-paired.pvdstream")).unwrap();
    assert_eq!(raw.frame_count(), 120);
    let raw_memory = &raw.analysis.legacy().unwrap().memory;
    assert_eq!(
        (raw_memory.events().len(), raw_memory.samples.len()),
        (7549, 135)
    );

    for (first, last, initial_rows, initial_bytes) in [
        (1, 2, 249, 1_188_750),
        (30, 31, 253, 1_196_138),
        (60, 61, 269, 1_461_161),
    ] {
        let mut bytes = Vec::new();
        let report =
            native::export::trim_scene_frames(&source, first, last, &mut bytes, || true).unwrap();
        let clipped = Recording::from_bytes(&bytes).unwrap();
        assert_eq!(
            clipped.memory_history_scope(),
            MemoryHistoryScope::AfterInitialSnapshot
        );
        let initial = clipped.native_initial_memory_totals().unwrap().unwrap();
        assert_eq!(
            (initial.len(), initial.nonnegative_bytes()),
            (initial_rows, Some(initial_bytes))
        );
        totals_match(
            &initial,
            &original
                .native_memory_totals_at(&original.state_at(first - 1).unwrap())
                .unwrap()
                .unwrap(),
        );
        assert_eq!(
            initial_bytes,
            raw_memory.state_at_frame(first - 1).live_bytes
        );
        for frame in (0..clipped.frame_count())
            .chain((0..clipped.frame_count()).rev())
            .chain([1, 0, 1])
        {
            let state = clipped.state_at(frame).unwrap();
            let expected = original.state_at(first + frame).unwrap();
            assert_eq!(state.objects, expected.objects);
            assert_eq!(state.cameras, expected.cameras);
            assert_eq!(state.errors, expected.errors);
            assert_eq!(state.debug_draw, expected.debug_draw);
            assert_eq!(state.contacts, expected.contacts);
            assert_eq!(
                clipped.frames[frame].timestamp,
                original.frames[first + frame].timestamp
            );
            totals_match(
                &clipped.native_memory_totals_at(&state).unwrap().unwrap(),
                &original
                    .native_memory_totals_at(&expected)
                    .unwrap()
                    .unwrap(),
            );
        }
        totals_match(
            &clipped.native_end_memory_totals().unwrap().unwrap(),
            &original
                .native_memory_totals_at(&original.state_at(last).unwrap())
                .unwrap()
                .unwrap(),
        );

        native_memory_interval::verify_partial_interval(
            &clipped,
            &raw,
            first,
            last,
            &info.source_ids,
        );
        let memory = &clipped.analysis.legacy().unwrap().memory;
        let mut csv = Vec::new();
        assert!(
            pvd_core::analysis_export::write_analysis_csv(
                &clipped,
                &mut csv,
                pvd_core::analysis_export::AnalysisCsvKind::Memory,
                |_| true
            )
            .is_err()
        );
        assert!(csv.is_empty());
        clipped.verify_replay().unwrap();
        let mut rewritten = Vec::new();
        native::export::rewrite(&bytes, &mut rewritten, || true).unwrap();
        assert_eq!(bytes, rewritten);
        if let Some(directory) = std::env::var_os("PVD_NATIVE_MEMORY_MIDDLE_DIRECTORY") {
            let directory = PathBuf::from(directory);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                directory.join(format!("rich-{first}-{last}-partial.pxd2")),
                &bytes,
            )
            .unwrap();
        }
        eprintln!(
            "rich partial {first}..{last}: {report:?}; memory_events={}, flushes={}, unknown_frees={}",
            memory.events().len(),
            memory.samples.len(),
            memory.unmatched_frees
        );
    }
    assert_eq!(std::fs::read(path).unwrap(), source);
}
