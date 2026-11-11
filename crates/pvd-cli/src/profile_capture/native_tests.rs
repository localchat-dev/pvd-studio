use super::*;
use pvd_core::profile_curves::Status;

use crate::archive_fixture as fixture;

fn key(event: u16) -> ExactKey {
    ExactKey {
        zone: 3,
        event,
        thread: 7,
        context: 9,
    }
}

fn expected_entries() -> Vec<CatalogEntry> {
    [(1, 14, 2), (2, 34, 1)]
        .into_iter()
        .map(|(event, first_start, complete_spans)| CatalogEntry {
            key: key(event),
            first_start: Some(first_start),
            complete_spans,
            open_spans: 0,
            unmatched_stops: 0,
            metadata_ambiguous: false,
        })
        .collect()
}

fn assert_catalog(catalog: &Catalog) {
    assert_eq!(catalog.entries, expected_entries());
    assert_eq!(catalog.zones.len(), 1);
    assert_eq!(catalog.zones[&3].name, "Simulation");
    assert_eq!(
        catalog.zones[&3].events,
        BTreeMap::from([(1, "Step".into()), (2, "Flush".into())])
    );
}

fn assert_curve(capture: &mut Capture, cancel: &AtomicBool) {
    let scene = ReferenceScene {
        id: 1,
        generation: 0,
    };
    let frames = profile_curves::sampling_frames(&capture.recording, scene, cancel)
        .unwrap()
        .frames;
    assert_eq!(frames, [0, 1, 2]);
    assert_eq!(
        capture.sample(key(1), scene, &frames, cancel).unwrap(),
        vec![
            Point {
                frame: 0,
                milliseconds: None,
                occurrence_count: 0,
                status: Status::FirstFrame,
            },
            Point {
                frame: 1,
                milliseconds: Some(0.000002),
                occurrence_count: 1,
                status: Status::Recorded,
            },
            Point {
                frame: 2,
                milliseconds: Some(0.000002),
                occurrence_count: 1,
                status: Status::Recorded,
            },
        ]
    );
    let tail = capture.sample(key(2), scene, &frames, cancel).unwrap();
    assert_eq!(tail[0].status, Status::FirstFrame);
    assert!(tail[1..].iter().all(|point| {
        point.status == Status::Missing
            && point.milliseconds.is_none()
            && point.occurrence_count == 0
    }));
}

#[test]
fn native_profile_catalog_and_curves_use_the_complete_file_or_archive() {
    let scratch = fixture::Scratch::new("native-profile");
    let native = fixture::native_profile();
    let cancel = AtomicBool::new(false);
    for (name, bytes) in [
        ("source.pxd2", native.clone()),
        ("source.pvd", fixture::stored("recording.pxd2", &native)),
    ] {
        let path = scratch.write(name, &bytes);
        let path = path.to_str().unwrap();
        assert_catalog(&catalog(path, &cancel).unwrap());
        let mut capture = Capture::open(path, &cancel).unwrap();
        assert!(capture.recording.is_disk_backed());
        assert!(capture.recording.analysis.legacy().is_none());
        capture.recording.analysis = capture.recording.analysis_at(0).unwrap();
        assert_eq!(
            capture.recording.analysis.profile_summary().complete_spans,
            0
        );
        assert_catalog(&capture.catalog(&cancel).unwrap());
        assert_eq!(
            capture.catalog_key(key(2), &cancel).unwrap().entries,
            [expected_entries()[1].clone()]
        );
        assert_curve(&mut capture, &cancel);
        let cancelled = AtomicBool::new(true);
        assert!(capture.catalog(&cancelled).is_err());
        assert!(capture.catalog_key(key(1), &cancelled).is_err());
    }
}

#[test]
fn native_profile_catalog_keeps_tail_spans_across_disk_segments() {
    let scratch = fixture::Scratch::new("native-profile-segments");
    let (recording, _) = pvd_core::native::read_bytes_with_history(
        &fixture::native_profile(),
        scratch.history(),
        |_, _| true,
    )
    .unwrap();
    assert!(recording.disk_usage().unwrap().segments >= 3);
    let mut capture = Capture {
        recording,
        source: None,
    };
    let cancel = AtomicBool::new(false);
    assert_catalog(&capture.catalog(&cancel).unwrap());
    assert_curve(&mut capture, &cancel);
}

#[cfg(windows)]
#[test]
fn native_profile_commands_export_named_series_and_exact_keys() {
    let scratch = fixture::Scratch::new("native-profile-commands");
    let native = fixture::native_profile();
    for (name, bytes) in [
        ("source.pxd2", native.clone()),
        ("source.pvd", fixture::stored("recording.pxd2", &native)),
    ] {
        let path = scratch.write(name, &bytes);
        let path = path.to_str().unwrap();
        crate::profile_events(path).unwrap();
        let error = crate::profile_pages::run(path, 16, None).unwrap_err();
        assert!(error.to_string().contains("requires an SDK stream"));
        for (label, selection) in [
            ("series", crate::ProfileSelection::Series(0)),
            ("exact", crate::ProfileSelection::Key(key(1))),
        ] {
            let output = scratch.path(&format!("{name}-{label}.csv"));
            crate::profile_event_csv(path, selection, 1, 0, output.to_str().unwrap()).unwrap();
            let csv = std::fs::read_to_string(output).unwrap();
            let rows: Vec<Vec<_>> = csv
                .lines()
                .skip(1)
                .map(|row| row.split(',').collect())
                .collect();
            assert_eq!(rows.len(), 3);
            for (frame, row) in rows.iter().enumerate() {
                assert_eq!(row.len(), 19);
                assert_eq!(
                    &row[1..9],
                    [
                        "0x3",
                        "1",
                        "7",
                        "0x9",
                        "\"Simulation\"",
                        "\"Step\"",
                        "0x1",
                        "0"
                    ]
                );
                assert_eq!(row[9].parse::<usize>().unwrap(), frame);
                if frame == 0 {
                    assert_eq!(&row[10..13], ["", "0", "no_previous_frame"]);
                } else {
                    assert_eq!(row[10].parse::<f64>().unwrap(), 0.000002);
                    assert_eq!(&row[11..13], ["1", "recorded"]);
                }
            }
        }
    }
}

#[test]
fn native_profile_catalog_names_respect_the_metadata_budget() {
    let mut remaining = 131;
    assert_eq!(catalog_name("abc", &mut remaining).unwrap(), "abc");
    assert_eq!(remaining, 0);
    assert!(catalog_name("", &mut remaining).is_err());
    assert_eq!(remaining, 0);
}
