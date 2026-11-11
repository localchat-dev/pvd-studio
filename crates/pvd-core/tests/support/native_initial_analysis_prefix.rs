use super::*;

struct Truth {
    path: PathBuf,
    original_bytes: Vec<u8>,
    original: Recording,
    sdk_bytes: Vec<u8>,
    sdk: Recording,
    initial: native_initial::NativeInitialValues,
}

impl Truth {
    fn read() -> Self {
        let path = pair("rich", "-native.pxd2");
        let original_bytes = std::fs::read(&path).unwrap();
        assert_eq!(original_bytes.len(), 1_034_096);
        let original = Recording::from_bytes(&original_bytes).unwrap();
        let sdk_bytes = std::fs::read(pair("rich", ".pvdstream")).unwrap();
        let sdk = Recording::from_bytes(&sdk_bytes).unwrap();
        assert_eq!((original.frame_count(), sdk.frame_count()), (120, 120));
        let initial = native_initial::NativeInitialValues::parse(&original_bytes);
        Self {
            path,
            original_bytes,
            original,
            sdk_bytes,
            sdk,
            initial,
        }
    }

    fn verify(
        &self,
        source: &[u8],
        output: &[u8],
        window: Window,
        frames: (usize, usize, usize),
        captured: (usize, usize, u64),
    ) {
        let (local_first, local_last, absolute_first) = frames;
        let (seed_starts, initial_total_rows, initial_total_bytes) = captured;
        verify_window(
            Case {
                source,
                output,
                original: &self.original,
                original_bytes: &self.original_bytes,
                sdk: &self.sdk,
                sdk_bytes: &self.sdk_bytes,
                local_first,
                local_last,
                absolute_first,
                seed_starts,
                initial_total_rows,
                initial_total_bytes,
            },
            &self.initial,
            window,
        );
    }

    fn unchanged(&self) {
        assert_eq!(std::fs::read(&self.path).unwrap(), self.original_bytes);
    }
}

fn trim(source: &[u8], first: usize, last: usize) -> Vec<u8> {
    let mut output = Vec::new();
    trim_scene_frames(source, first, last, &mut output, || true).unwrap();
    output
}

fn persist(name: &str, bytes: &[u8]) {
    if let Some(directory) = std::env::var_os("PVD_NATIVE_INITIAL_ANALYSIS_PREFIX_DIRECTORY") {
        let directory = PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join(name), bytes).unwrap();
    }
}

#[test]
#[ignore = "requires the preserved original rich native container and its same-run SDK stream"]
fn actual_rich_initial_analysis_prefix_keeps_initial_bytes_events_and_sdk_interval() {
    let truth = Truth::read();
    for (first, last, seed, rows, bytes) in [
        (1, 2, 2, 249, 1_188_750),
        (30, 31, 31, 253, 1_196_138),
        (60, 61, 61, 269, 1_461_161),
        (30, 34, 31, 253, 1_196_138),
    ] {
        let source = trim(&truth.original_bytes, first, last);
        let zero = trim(&source, 0, 0);
        let one = trim(&source, 0, 1);
        for (prefix_last, output) in [(0, &zero), (1, &one)] {
            truth.verify(
                &source,
                output,
                Window::Prefix,
                (0, prefix_last, first),
                (seed, rows, bytes),
            );
            let source_raw = Raw::parse(&source);
            let actual = Raw::parse(output);
            assert_eq!(
                actual.events.as_slice(),
                &source_raw.events[..=source_raw.frames[prefix_last].1]
            );
            assert_eq!(
                set(actual.events[1]).unwrap().data,
                set(source_raw.events[1]).unwrap().data
            );
            persist(
                &format!("rich-{first}-{last}-prefix-{prefix_last}.pxd2"),
                output,
            );
        }
        let twice = trim(&one, 0, 0);
        truth.verify(
            &one,
            &twice,
            Window::Prefix,
            (0, 0, first),
            (seed, rows, bytes),
        );
        assert_eq!(Raw::parse(&twice).initial, Raw::parse(&zero).initial);
        assert_eq!(Raw::parse(&twice).events, Raw::parse(&zero).events);
        persist(&format!("rich-{first}-{last}-source.pxd2"), &source);
        persist(&format!("rich-{first}-{last}-prefix-twice.pxd2"), &twice);
    }
    truth.unchanged();
}

#[test]
#[ignore = "requires the preserved original rich native container and its same-run SDK stream"]
fn actual_rich_initial_analysis_prefix_middle_combinations_match_direct_and_sdk_truth() {
    let truth = Truth::read();
    for (first, last, seed, rows, bytes) in [
        (1, 2, 3, 252, 1_196_122),
        (30, 31, 32, 253, 1_196_138),
        (60, 61, 62, 269, 1_461_161),
        (30, 34, 32, 253, 1_196_138),
    ] {
        let source = trim(&truth.original_bytes, first, last);
        let prefix = trim(&source, 0, 1);
        let middle = trim(&prefix, 1, 1);
        truth.verify(
            &prefix,
            &middle,
            Window::Middle,
            (1, 1, first + 1),
            (seed, rows, bytes),
        );
        let repeated_prefix = trim(&middle, 0, 0);
        truth.verify(
            &middle,
            &repeated_prefix,
            Window::Prefix,
            (0, 0, first + 1),
            (seed, rows, bytes),
        );
        assert_eq!(
            Raw::parse(&repeated_prefix).initial,
            Raw::parse(&middle).initial
        );
        assert_eq!(
            Raw::parse(&repeated_prefix).events,
            Raw::parse(&middle).events
        );
        persist(&format!("rich-{first}-{last}-prefix-middle.pxd2"), &middle);
        persist(
            &format!("rich-{first}-{last}-prefix-middle-prefix.pxd2"),
            &repeated_prefix,
        );
    }
    let source = trim(&truth.original_bytes, 30, 34);
    let middle = trim(&source, 1, 3);
    truth.verify(
        &source,
        &middle,
        Window::Middle,
        (1, 3, 31),
        (32, 253, 1_196_138),
    );
    for prefix_last in [0, 1] {
        let prefix = trim(&middle, 0, prefix_last);
        truth.verify(
            &middle,
            &prefix,
            Window::Prefix,
            (0, prefix_last, 31),
            (32, 253, 1_196_138),
        );
        if prefix_last == 1 {
            let again = trim(&prefix, 1, 1);
            truth.verify(
                &prefix,
                &again,
                Window::Middle,
                (1, 1, 32),
                (33, 253, 1_195_778),
            );
            persist("rich-30-34-middle-prefix-middle.pxd2", &again);
        }
        persist(
            &format!("rich-30-34-middle-prefix-{prefix_last}.pxd2"),
            &prefix,
        );
    }
    truth.unchanged();
}
