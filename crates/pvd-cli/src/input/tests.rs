use super::*;
use crate::archive_fixture as fixture;

#[test]
fn pvd_archive_cli_input_keeps_inner_bytes_identity_and_native_dispatch() {
    let scratch = fixture::Scratch::new("cli-input");
    for (name, inner, native) in [
        ("sdk.pvd", fixture::sdk(), false),
        ("native.pvd", fixture::native(), true),
    ] {
        let wrapped = fixture::stored("nested/recording.pxd2", &inner);
        let path = scratch.write(name, &wrapped);
        let mut input = Input::open(&path, |_, _| true).unwrap();
        assert_eq!(input.length(), inner.len() as u64);
        assert_eq!(
            input.archive(),
            Some(("nested/recording.pxd2", wrapped.len() as u64))
        );
        assert_eq!(input.is_native().unwrap(), native);
        let mut bytes = Vec::new();
        input.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, inner);
        input.rewind().unwrap();
        if native {
            let recording = input.native_recording(|_, _| true).unwrap();
            assert_eq!(recording.frame_count(), 3);
            let mut output = Vec::new();
            let report = pvd_core::native::export::write_reader(
                &mut input,
                pvd_core::native::export::Selection::Whole,
                &mut output,
                |_| true,
            )
            .unwrap();
            assert_eq!(output, inner);
            assert_eq!(report.source_bytes, inner.len());
            assert_eq!(report.source_blake3, blake3::hash(&inner));
        } else {
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let catalog =
                pvd_core::profile_curves::catalog_streaming(&mut input, &cancel, |_| true).unwrap();
            assert_eq!(catalog.identity.bytes, inner.len() as u64);
            assert_eq!(catalog.identity.blake3, *blake3::hash(&inner).as_bytes());
            assert_eq!(catalog.identity.frames, 3);
        }
    }
}

#[cfg(windows)]
#[test]
fn pvd_archive_cli_commands_analyze_and_export_only_the_inner_format() {
    let scratch = fixture::Scratch::new("cli-commands");
    for (name, inner, native) in [
        ("sdk.pvd", fixture::sdk(), false),
        ("native.pvd", fixture::native(), true),
    ] {
        let path = scratch.write(name, &fixture::stored("recording.pxd2", &inner));
        let path = path.to_str().unwrap();
        crate::inspect(path).unwrap();
        crate::verify(path).unwrap();
        crate::profile_events(path).unwrap();
        crate::profile_csv(
            path,
            scratch
                .path(&format!("{name}-profile.csv"))
                .to_str()
                .unwrap(),
        )
        .unwrap();
        crate::memory_csv(
            path,
            scratch
                .path(&format!("{name}-memory.csv"))
                .to_str()
                .unwrap(),
        )
        .unwrap();
        let output = scratch.path(&format!(
            "{name}-clip.{}",
            if native { "pxd2" } else { "pvdstream" }
        ));
        if native {
            let whole = scratch.path("whole.pxd2");
            crate::native_export::run(
                path,
                whole.to_str().unwrap(),
                pvd_core::native::export::Selection::Whole,
            )
            .unwrap();
            assert_eq!(std::fs::read(whole).unwrap(), inner);
            crate::native_export::run(
                path,
                output.to_str().unwrap(),
                pvd_core::native::export::Selection::Frames { first: 1, last: 2 },
            )
            .unwrap();
            let rejected = scratch.path("wrong-sdk.pvdstream");
            assert!(crate::trim(path, 0, 1, rejected.to_str().unwrap()).is_err());
            assert!(!rejected.exists());
        } else {
            crate::trim(path, 1, 2, output.to_str().unwrap()).unwrap();
            crate::profile_pages::run(path, 16, None).unwrap();
            let rejected = scratch.path("wrong-native.pxd2");
            assert!(
                crate::native_export::run(
                    path,
                    rejected.to_str().unwrap(),
                    pvd_core::native::export::Selection::Whole
                )
                .is_err()
            );
            assert!(!rejected.exists());
        }
        let reopened = pvd_core::Recording::from_path(output).unwrap();
        assert_eq!(reopened.frame_count(), 2);
        for (index, value) in [22, 33].into_iter().enumerate() {
            assert_eq!(
                reopened.state_at(index).unwrap().objects[&1].properties["Value"],
                pvd_core::Value::U64(value)
            );
        }
    }
    let mut corrupt = fixture::stored("recording.pxd2", &fixture::sdk());
    corrupt[30 + "recording.pxd2".len() + 20] ^= 1;
    let path = scratch.write("corrupt.pvd", &corrupt);
    let output = scratch.path("corrupt.csv");
    assert!(crate::memory_csv(path.to_str().unwrap(), output.to_str().unwrap()).is_err());
    assert!(!output.exists());
}
