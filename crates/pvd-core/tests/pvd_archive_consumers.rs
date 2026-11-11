#[path = "support/pvd_archive.rs"]
mod fixture;

use pvd_core::{Recording, RecordingSource, Value};

fn check(recording: &Recording, source: RecordingSource) {
    assert_eq!(recording.source, source);
    assert_eq!(recording.frame_count(), 3);
    for index in [0, 1, 2, 1, 0, 2] {
        let state = recording.state_at(index).unwrap();
        assert_eq!(
            state.objects[&1].properties["Value"],
            Value::U64([11, 22, 33][index])
        );
        assert_eq!(recording.frames[index].timestamp, [11, 21, 31][index]);
    }
    recording.verify_replay().unwrap();
}

#[test]
fn pvd_archive_bytes_and_path_use_the_inner_format_and_values() {
    let scratch = fixture::Scratch::new("core");
    for (name, inner, source) in [
        ("sdk.bin", fixture::sdk(), RecordingSource::SdkStream),
        (
            "native.pvd",
            fixture::native(),
            RecordingSource::Pvd3Database,
        ),
    ] {
        let wrapped = fixture::stored("nested/capture.pxd2", &inner);
        let path = scratch.write(name, &wrapped);
        check(&Recording::from_bytes(&inner).unwrap(), source);
        check(&Recording::from_bytes(&wrapped).unwrap(), source);
        let recording = Recording::from_path(&path).unwrap();
        check(&recording, source);
        if source == RecordingSource::Pvd3Database {
            assert!(recording.is_disk_backed());
        }
        let failure = Recording::from_path_with_progress(&path, |_, _| false).unwrap_err();
        assert_eq!(
            failure.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
        let failure = Recording::from_bytes_with_progress(&wrapped, |_, _| false).unwrap_err();
        assert_eq!(
            failure.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
    }
}

#[test]
fn pvd_archive_rejects_corruption_and_invalid_inner_protocol_before_returning_a_recording() {
    let scratch = fixture::Scratch::new("invalid");
    let inner = fixture::sdk();
    let mut corrupt = fixture::stored("capture.pxd2", &inner);
    corrupt[30 + "capture.pxd2".len() + 20] ^= 1;
    let mut malformed = inner;
    malformed.pop();
    for (index, bytes) in [corrupt, fixture::stored("capture.pxd2", &malformed)]
        .into_iter()
        .enumerate()
    {
        let path = scratch.write(&format!("bad-{index}.pvd"), &bytes);
        assert!(Recording::from_bytes(&bytes).is_err());
        assert!(Recording::from_path(path).is_err());
    }
}

#[test]
#[ignore = "requires PVD_ARCHIVE_NATIVE_CAPTURE and PVD_ARCHIVE_SDK_CAPTURE"]
fn pvd_archive_wrapped_real_native_and_sdk_match_the_unwrapped_capture() {
    let scratch = fixture::Scratch::new("real");
    for (variable, name) in [
        ("PVD_ARCHIVE_NATIVE_CAPTURE", "native.pvd"),
        ("PVD_ARCHIVE_SDK_CAPTURE", "sdk.pvd"),
    ] {
        let source = std::path::PathBuf::from(std::env::var_os(variable).unwrap());
        let bytes = std::fs::read(&source).unwrap();
        let wrapped = fixture::stored("recording.pxd2", &bytes);
        let path = scratch.write(name, &wrapped);
        let expected = Recording::from_path(&source).unwrap();
        assert!(expected.frame_count() > 0);
        for actual in [
            Recording::from_bytes(&wrapped).unwrap(),
            Recording::from_path(&path).unwrap(),
        ] {
            assert_eq!(actual.source, expected.source);
            assert_eq!(actual.frame_count(), expected.frame_count());
            for index in [0, expected.frame_count() / 2, expected.frame_count() - 1] {
                assert_eq!(
                    actual.state_at(index).unwrap(),
                    expected.state_at(index).unwrap()
                );
                assert_eq!(
                    actual.frames[index].scene_id,
                    expected.frames[index].scene_id
                );
                assert_eq!(
                    actual.frames[index].timestamp,
                    expected.frames[index].timestamp
                );
            }
            actual.verify_replay().unwrap();
        }
        if expected.source == RecordingSource::Pvd3Database {
            let archive =
                pvd_core::pvd_archive::open_with_progress(&path, &Default::default(), |_, _| true)
                    .unwrap();
            let mut output = Vec::new();
            let report = pvd_core::native::export::write_reader(
                &mut std::io::Cursor::new(&archive.bytes),
                pvd_core::native::export::Selection::Whole,
                &mut output,
                |_| true,
            )
            .unwrap();
            assert_eq!(output, bytes);
            assert_eq!(report.source_bytes, bytes.len());
            assert_eq!(report.source_blake3, blake3::hash(&bytes));
        }
    }
}
