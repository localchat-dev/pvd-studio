use super::*;

fn assert_no_temporary(root: &Path) {
    assert!(std::fs::read_dir(root).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".pvd-capture-")
    }));
}

#[test]
fn atomic_export_rejects_a_wire_valid_capture_that_exceeds_the_open_budget() {
    let fixture = Fixture::new(true);
    let source = fixture.recording.as_ref().unwrap();
    let range = CaptureRange { first: 3, last: 6 };
    let (_, report) = fixture.export(range);
    let limits = CaptureLimits {
        index_bytes: 256 << 10,
        ..Default::default()
    };
    assert!(report.index_bytes < limits.index_bytes);

    // Leave room for the wire and source-directory decoder's allocations. The
    // resource reader alone reserves 256 KiB, so its directory and frame indexes
    // cannot also fit under this same limit when the full reader is admitted.
    let mut written = Cursor::new(Vec::new());
    write_retained(source, &mut written, range, &limits, |_| true).unwrap();
    let reference = fixture.root.join("wire-valid.pvdcapture");
    std::fs::write(&reference, written.into_inner()).unwrap();
    let failure = open_with_progress(&reference, &limits, |_| true).unwrap_err();
    assert!(failure.to_string().contains("metadata budget"), "{failure}");

    let output = fixture.root.join("unopenable.pvdcapture");
    let rejected = write_retained_new(source, &output, range, &limits, 0, |_| true).unwrap_err();
    assert!(
        rejected.to_string().contains("metadata budget"),
        "the reader admission failure must prevent publication: {rejected}"
    );
    assert!(!output.exists());
    assert_no_temporary(&fixture.root);
    assert_eq!(source.state_at(6).unwrap(), fixture.states[6]);
}

#[test]
fn cancellation_during_full_reader_admission_removes_the_temporary_capture() {
    let fixture = Fixture::new(true);
    let output = fixture.root.join("cancelled-admission.pvdcapture");
    let mut directory_scans = 0;
    let failure = write_retained_new(
        fixture.recording.as_ref().unwrap(),
        &output,
        CaptureRange { first: 3, last: 6 },
        &CaptureLimits::default(),
        0,
        |progress| {
            if progress.phase == CapturePhase::Validate && progress.completed == MAGIC.len() as u64
            {
                directory_scans += 1;
            }
            directory_scans < 2
        },
    )
    .unwrap_err();
    assert_eq!(directory_scans, 2, "full reader admission was not reached");
    assert_eq!(
        failure.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
    assert!(!output.exists());
    assert_no_temporary(&fixture.root);
}
