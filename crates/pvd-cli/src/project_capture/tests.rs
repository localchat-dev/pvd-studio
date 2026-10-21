use super::*;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

#[path = "../../../../test-support/retained_capture_fixture.rs"]
mod fixture;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Case(PathBuf);
impl Drop for Case {
    fn drop(&mut self) {
        fn payloads(path: &std::path::Path) -> usize {
            if !path.exists() {
                return 0;
            }
            std::fs::read_dir(path)
                .unwrap()
                .map(|entry| {
                    let path = entry.unwrap().path();
                    if path.is_dir() { payloads(&path) } else { 1 }
                })
                .sum()
        }
        assert_eq!(payloads(&self.0.join("history")), 0);
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn inspect_verify_frame_and_bench_use_only_visible_absolute_frames() {
    let root = std::env::temp_dir().join(format!(
        "pvd-cli-retained-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&root).unwrap();
    let case = Case(root);
    let mut live = pvd_core::LiveRecording::with_history(pvd_core::live::HistoryConfig {
        directory: case.0.join("history"),
        disk_budget_bytes: 32 << 20,
        segment_frames: 2,
        segment_target_bytes: 64 << 20,
        min_free_disk_bytes: 0,
    })
    .unwrap();
    live.feed(&fixture::capture(9)).unwrap();
    let recording = live.snapshot().unwrap();
    let path = case.0.join("retained.any-extension");
    pvd_core::capture::write_retained_new(
        &recording,
        &path,
        pvd_core::capture::CaptureRange { first: 3, last: 6 },
        &Default::default(),
        0,
        |_| true,
    )
    .unwrap();
    drop(recording);
    drop(live);
    let path = path.to_str().unwrap();
    assert!(is_capture(path).unwrap());
    let reopened = Recording::from_path(path).unwrap();
    assert_eq!(sample_frames(&reopened).unwrap(), [3, 5, 6]);
    drop(reopened);
    crate::inspect(path).unwrap();
    crate::verify(path).unwrap();
    crate::bench(path, 5).unwrap();
    crate::frame(path, 3).unwrap();
    assert!(crate::frame(path, 2).is_err());
    let output = case.0.join("full.csv");
    assert!(
        crate::profile_csv(path, output.to_str().unwrap())
            .unwrap_err()
            .to_string()
            .contains("retained interval")
    );
    assert!(crate::memory_csv(path, output.to_str().unwrap()).is_err());
    assert!(crate::csv(path, 9, "Counter", output.to_str().unwrap()).is_err());
    assert!(crate::profile_pages::run(path, 10, None).is_err());
    assert!(crate::profile_capture::catalog(path, &Default::default()).is_err());
    assert!(crate::profile_capture::Capture::open(path, &Default::default()).is_err());
    assert!(!output.exists());
    let raw = case.0.join("clip.pvdstream");
    assert!(crate::trim(path, 3, 6, raw.to_str().unwrap()).is_err());
    assert!(!raw.exists());
}
