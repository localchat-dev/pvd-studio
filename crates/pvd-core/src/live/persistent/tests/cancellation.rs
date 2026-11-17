use super::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc,
};

fn interrupted(failure: &Error) {
    assert_eq!(
        failure
            .downcast_ref::<std::io::Error>()
            .map(std::io::Error::kind),
        Some(std::io::ErrorKind::Interrupted),
        "{failure}"
    );
}

fn history_with_cold_prefix(directory: &TestDirectory) -> HistoryWriter {
    let mut config = directory.config();
    config.segment_frames = 1;
    let mut history = HistoryWriter::new_persistent(config).unwrap();
    for index in 0..2 {
        append(&mut history, index);
    }
    history.seal_bank(&source(&history)).unwrap();
    for index in 2..6 {
        append(&mut history, index);
    }
    history
}

fn unpublished_bank(history: &HistoryWriter) -> PathBuf {
    history.owner.as_ref().unwrap().path.join(format!(
        "{:020}.pvdhistory",
        (1u64 << 61) + history.archive.as_ref().unwrap().len() as u64
    ))
}

fn assert_prefix_survives(history: &HistoryWriter, old: &crate::Recording, incomplete: &Path) {
    assert_eq!(history.archive.as_ref().unwrap().len(), 1);
    assert_eq!(history.frame_infos.len(), 6);
    assert!(history.accepting_frames());
    assert!(history.warning.is_none());
    assert!(!incomplete.exists());
    assert!(
        !history
            .owner
            .as_ref()
            .unwrap()
            .files
            .lock()
            .unwrap()
            .contains(incomplete)
    );
    for index in 0..6 {
        assert_value(old, index);
        assert_value(&recording(history), index);
    }
}

#[test]
fn cancellation_during_bank_copy_removes_partial_output_and_keeps_hot_and_cold_prefix() {
    let directory = TestDirectory::new();
    let mut history = history_with_cold_prefix(&directory);
    let old = source(&history);
    let incomplete = unpublished_bank(&history);
    let cancelled = Arc::new(AtomicBool::new(false));
    history.set_cancellation(IngestCancellation(Some(cancelled.clone())));
    let mut observed_copy = false;
    let failure = history
        .seal_bank_with_progress(&old, |phase, progress| {
            if phase == SealPhase::Write
                && let Some(progress) = progress
                && progress.phase == capture::CapturePhase::Copy
                && progress.completed > 0
                && progress.completed < progress.total
            {
                assert!(std::fs::metadata(&incomplete).unwrap().len() > 0);
                observed_copy = true;
                cancelled.store(true, Ordering::Release);
            }
        })
        .unwrap_err();
    assert!(observed_copy);
    interrupted(&failure);
    assert_prefix_survives(&history, &old, &incomplete);
}

#[test]
fn cancellation_during_bank_admission_removes_unpublished_output_and_keeps_prefix() {
    let directory = TestDirectory::new();
    let mut history = history_with_cold_prefix(&directory);
    let old = source(&history);
    let incomplete = unpublished_bank(&history);
    let cancelled = Arc::new(AtomicBool::new(false));
    history.set_cancellation(IngestCancellation(Some(cancelled.clone())));
    let mut observed_admission = false;
    let failure = history
        .seal_bank_with_progress(&old, |phase, progress| {
            if phase == SealPhase::Admission
                && let Some(progress) = progress
                && progress.phase == capture::CapturePhase::Validate
                && progress.completed > 0
                && progress.completed == progress.total
            {
                assert!(std::fs::metadata(&incomplete).unwrap().len() > 0);
                observed_admission = true;
                cancelled.store(true, Ordering::Release);
            }
        })
        .unwrap_err();
    assert!(observed_admission);
    interrupted(&failure);
    assert_prefix_survives(&history, &old, &incomplete);
}

#[test]
fn cancellation_while_publication_waits_for_reader_does_not_require_reader_to_finish() {
    let directory = TestDirectory::new();
    let mut history = history_with_cold_prefix(&directory);
    let old = source(&history);
    let incomplete = unpublished_bank(&history);
    let cancelled = Arc::new(AtomicBool::new(false));
    history.set_cancellation(IngestCancellation(Some(cancelled.clone())));
    let archive = history.archive.as_ref().unwrap().clone();
    let reader = archive.transition.read().unwrap();
    let (waiting, wait_started) = mpsc::channel();
    let (finished, completion) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        let mut waiting = Some(waiting);
        let failure = history
            .seal_bank_with_progress(&source(&history), |phase, _| {
                if phase == SealPhase::WaitingForReaders
                    && let Some(waiting) = waiting.take()
                {
                    waiting.send(()).unwrap();
                }
            })
            .unwrap_err();
        finished.send(()).unwrap();
        (history, failure)
    });
    wait_started.recv_timeout(Duration::from_secs(5)).unwrap();
    cancelled.store(true, Ordering::Release);
    let completed_while_reading = completion.recv_timeout(Duration::from_secs(2));
    drop(reader);
    let (history, failure) = writer.join().unwrap();
    completed_while_reading.expect("cancelled bank waited for an unrelated reader");
    interrupted(&failure);
    assert_prefix_survives(&history, &old, &incomplete);
}

#[test]
fn cancelled_frame_preparation_is_an_interruption_not_a_retention_limit() {
    let directory = TestDirectory::new();
    let mut history = history_with_cold_prefix(&directory);
    let old = source(&history);
    let incomplete = unpublished_bank(&history);
    history.set_cancellation(IngestCancellation(Some(Arc::new(AtomicBool::new(true)))));
    interrupted(&history.prepare_persistent(&old, 5).unwrap_err());
    assert_prefix_survives(&history, &old, &incomplete);
}

#[test]
fn cancellation_after_publication_keeps_the_sealed_bank_and_readable_prefix() {
    let directory = TestDirectory::new();
    let mut history = history_with_cold_prefix(&directory);
    let old = source(&history);
    let published = unpublished_bank(&history);
    let cancelled = Arc::new(AtomicBool::new(false));
    history.set_cancellation(IngestCancellation(Some(cancelled.clone())));
    let mut reached_retirement = false;
    let failure = history
        .seal_bank_with_progress(&old, |phase, _| {
            if phase == SealPhase::Retire {
                reached_retirement = true;
                cancelled.store(true, Ordering::Release);
            }
        })
        .unwrap_err();
    assert!(reached_retirement);
    interrupted(&failure);
    assert_eq!(history.archive.as_ref().unwrap().len(), 2);
    assert!(!history.completed.is_empty());
    assert!(published.exists());
    assert!(
        history
            .owner
            .as_ref()
            .unwrap()
            .files
            .lock()
            .unwrap()
            .contains(&published)
    );
    assert!(history.accepting_frames());
    assert!(history.warning.is_none());
    for index in 0..6 {
        assert_value(&old, index);
        assert_value(&recording(&history), index);
    }
}
