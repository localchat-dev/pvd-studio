use pvd_core::{
    Recording,
    clip::{TrimPhase, trim_scene_frames, trim_scene_frames_streaming},
};
use std::{
    fs::File,
    io::{self, Cursor, Read, Seek, SeekFrom, Write},
    path::PathBuf,
};

fn group(events: &[Vec<u8>], timestamp: u64) -> Vec<u8> {
    let data: Vec<_> = events.iter().flatten().copied().collect();
    let mut bytes = Vec::new();
    bytes.extend((data.len() as u32).to_le_bytes());
    bytes.extend((events.len() as u32).to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(timestamp.to_le_bytes());
    bytes.extend(data);
    bytes
}

fn string(handle: u32, value: &str) -> Vec<u8> {
    let mut bytes = vec![1];
    bytes.extend((value.len() as u32 + 1).to_le_bytes());
    bytes.extend(value.as_bytes());
    bytes.push(0);
    bytes.extend(handle.to_le_bytes());
    bytes
}

fn section(kind: u8, frame: u64) -> Vec<u8> {
    let mut bytes = vec![kind];
    bytes.extend(17u64.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(frame.to_le_bytes());
    bytes
}

fn fixture(frames: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(pvd_protocol::STREAM_MAGIC.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(group(&[string(1, "frame")], 0));
    for frame in 0..frames {
        bytes.extend(group(
            &[section(18, frame as u64), section(19, frame as u64)],
            frame as u64,
        ));
    }
    bytes
}

#[test]
fn sequential_export_preserves_raw_bytes_and_reports_complete_source_identity() {
    let bytes = fixture(4097);
    let expected = trim_scene_frames(&bytes, 2047, 2051).unwrap();
    let mut source = Cursor::new(&bytes);
    let mut output = Vec::new();
    let mut progress = Vec::new();
    let report = trim_scene_frames_streaming(&mut source, &mut output, 2047, 2051, |state| {
        progress.push(state);
        true
    })
    .unwrap();
    assert_eq!(output, expected);
    assert_eq!(report.source_blake3, *blake3::hash(&bytes).as_bytes());
    assert_eq!(report.source_bytes, bytes.len() as u64);
    assert_eq!(report.source_frames, 4097);
    assert_eq!(report.output_frames, 5);
    assert_eq!(report.output_bytes, output.len() as u64);
    for phase in [TrimPhase::Inspect, TrimPhase::Export] {
        let states: Vec<_> = progress
            .iter()
            .filter(|state| state.phase == phase)
            .collect();
        assert_eq!(states.first().unwrap().bytes_read, 0);
        assert_eq!(states.last().unwrap().bytes_read, bytes.len() as u64);
        assert!(
            states
                .windows(2)
                .all(|pair| pair[0].bytes_read <= pair[1].bytes_read)
        );
    }
}

#[test]
fn big_endian_group_headers_and_original_timestamps_are_preserved() {
    let mut bytes = Vec::new();
    bytes.extend(pvd_protocol::STREAM_MAGIC.to_be_bytes());
    bytes.extend(1u32.to_be_bytes());
    bytes.extend(3u64.to_be_bytes());
    bytes.extend(7u64.to_be_bytes());
    bytes.extend(1u32.to_be_bytes());
    let mut string = vec![1];
    string.extend(6u32.to_be_bytes());
    string.extend(b"frame\0");
    string.extend(1u32.to_be_bytes());
    let mut groups = vec![(1u32, string)];
    for frame in 0..4u64 {
        let mut events = Vec::new();
        for kind in [18, 19] {
            events.push(kind);
            events.extend(17u64.to_be_bytes());
            events.extend(1u32.to_be_bytes());
            events.extend((1_000 + frame * 17).to_be_bytes());
        }
        groups.push((2, events));
    }
    for (count, data) in groups {
        bytes.extend((data.len() as u32).to_be_bytes());
        bytes.extend(count.to_be_bytes());
        bytes.extend(99u64.to_be_bytes());
        bytes.extend(123_456u64.to_be_bytes());
        bytes.extend(data);
    }
    let mut output = Vec::new();
    trim_scene_frames_streaming(&mut Cursor::new(&bytes), &mut output, 1, 3, |_| true).unwrap();
    assert_eq!(output, trim_scene_frames(&bytes, 1, 3).unwrap());
    let recording = Recording::from_bytes(&output).unwrap();
    assert_eq!(recording.header.endian, pvd_protocol::Endian::Big);
    assert_eq!(recording.frames[0].timestamp, 1017);
    assert_eq!(recording.frames[2].timestamp, 1051);
}

#[test]
fn cancellation_and_invalid_source_never_report_a_successful_clip() {
    let bytes = fixture(7);
    for phase in [TrimPhase::Inspect, TrimPhase::Export] {
        let mut output = Vec::new();
        let error =
            trim_scene_frames_streaming(&mut Cursor::new(&bytes), &mut output, 2, 5, |state| {
                !(state.phase == phase && state.bytes_read > 28)
            })
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::Interrupted
        );
        if phase == TrimPhase::Inspect {
            assert!(output.is_empty());
        }
    }
    for (bytes, first, last) in [
        (bytes[..bytes.len() - 1].to_vec(), 0, 0),
        (bytes.clone(), 4, 3),
        (bytes.clone(), 0, 7),
        (
            pvd_core::native::MAGIC
                .to_le_bytes()
                .into_iter()
                .chain([0; 24])
                .collect(),
            0,
            0,
        ),
    ] {
        let mut output = Vec::new();
        assert!(
            trim_scene_frames_streaming(&mut Cursor::new(bytes), &mut output, first, last, |_| {
                true
            })
            .is_err()
        );
        assert!(output.is_empty());
    }
}

struct ChangingSource {
    bytes: Cursor<Vec<u8>>,
    starts: usize,
}
impl Read for ChangingSource {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.bytes.read(output)
    }
}
impl Seek for ChangingSource {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        if position == SeekFrom::Start(0) {
            self.starts += 1;
            if self.starts == 2 {
                let length = self.bytes.get_ref().len();
                self.bytes.get_mut()[length - 8] ^= 1;
            }
        }
        self.bytes.seek(position)
    }
}

#[test]
fn mutations_after_the_selected_last_frame_are_detected_by_the_full_digest() {
    let mut source = ChangingSource {
        bytes: Cursor::new(fixture(8)),
        starts: 0,
    };
    let error =
        trim_scene_frames_streaming(&mut source, &mut Vec::new(), 0, 1, |_| true).unwrap_err();
    assert!(
        error.to_string().contains("changed between clip scans"),
        "{error}"
    );
}

struct FragmentedSource {
    bytes: Cursor<Vec<u8>>,
    largest_read: usize,
}
impl Read for FragmentedSource {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.largest_read = self.largest_read.max(output.len());
        let length = output.len().min(7);
        self.bytes.read(&mut output[..length])
    }
}
impl Seek for FragmentedSource {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.bytes.seek(position)
    }
}

#[test]
fn fragmented_reads_and_writer_failures_preserve_io_semantics() {
    let mut source = FragmentedSource {
        bytes: Cursor::new(fixture(3)),
        largest_read: 0,
    };
    let mut output = Vec::new();
    trim_scene_frames_streaming(&mut source, &mut output, 1, 2, |_| true).unwrap();
    assert!(source.largest_read <= 64 << 10);
    assert_eq!(Recording::from_bytes(&output).unwrap().frame_count(), 2);
    struct FailingWriter;
    impl Write for FailingWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::StorageFull.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let error =
        trim_scene_frames_streaming(&mut source, &mut FailingWriter, 0, 0, |_| true).unwrap_err();
    assert_eq!(
        error.downcast_ref::<io::Error>().unwrap().kind(),
        io::ErrorKind::StorageFull
    );
}

fn capture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../captures")
        .join(name)
}

fn instance(class: u32) -> Vec<u8> {
    let mut bytes = vec![6];
    bytes.extend(5u32.to_le_bytes());
    bytes.extend(class.to_le_bytes());
    bytes.extend(17u64.to_le_bytes());
    bytes
}

fn property(data: &[u8], datatype: u32) -> Vec<u8> {
    let mut bytes = vec![7];
    bytes.extend(17u64.to_le_bytes());
    for value in [3, 5, datatype, 1, data.len() as u32] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.extend(data);
    bytes
}

fn renderer_fixture(transaction: bool, cross_boundary: bool, silent: bool) -> Vec<u8> {
    let mut bytes = fixture(0);
    bytes.extend(group(
        &[
            string(2, "PvdUserRenderer"),
            string(3, "events"),
            string(4, "PvdU8"),
            string(5, "physx3"),
            instance(2),
        ],
        0,
    ));
    let mut data = vec![1];
    data.extend(17u64.to_le_bytes());
    data.push(2);
    data.extend(1u32.to_le_bytes());
    data.extend([0; 16]);
    let mut renderer = property(&data, 4);
    renderer[21..25].copy_from_slice(&(data.len() as u32).to_le_bytes());
    let begin = {
        let mut event = vec![8];
        event.extend(17u64.to_le_bytes());
        for value in [3u32, 5, 4] {
            event.extend(value.to_le_bytes());
        }
        event
    };
    for frame in 0..3 {
        bytes.extend(group(&[section(18, frame)], frame));
        if frame == 1 && cross_boundary {
            bytes.extend(group(&[vec![10]], frame));
        }
        if !(frame == 1 && (silent || cross_boundary)) {
            if transaction {
                bytes.extend(group(std::slice::from_ref(&begin), frame));
                for chunk in data.chunks(7) {
                    let mut event = vec![9];
                    event.extend((chunk.len() as u32).to_le_bytes());
                    event.extend(chunk);
                    event.extend((chunk.len() as u32).to_le_bytes());
                    bytes.extend(group(&[event], frame));
                }
                if !(frame == 0 && cross_boundary) {
                    bytes.extend(group(&[vec![10]], frame));
                }
            } else {
                bytes.extend(group(std::slice::from_ref(&renderer), frame));
            }
        }
        bytes.extend(group(&[section(19, frame)], frame));
    }
    bytes
}

#[test]
fn renderer_transactions_crossing_boundaries_and_silent_payloads_keep_strict_outcomes() {
    for transaction in [false, true] {
        for cross in [false, true] {
            if cross && !transaction {
                continue;
            }
            let bytes = renderer_fixture(transaction, cross, false);
            let expected = trim_scene_frames(&bytes, 1, 2).unwrap();
            let mut output = Vec::new();
            trim_scene_frames_streaming(&mut Cursor::new(&bytes), &mut output, 1, 2, |_| true)
                .unwrap();
            assert_eq!(output, expected);
            if cross {
                let error = trim_scene_frames_streaming(
                    &mut Cursor::new(&bytes),
                    &mut Vec::new(),
                    0,
                    0,
                    |_| true,
                )
                .unwrap_err();
                assert!(
                    error.to_string().contains("property transaction"),
                    "{error}"
                );
            }
        }
    }
    let bytes = renderer_fixture(false, false, true);
    let error =
        trim_scene_frames_streaming(&mut Cursor::new(bytes), &mut Vec::new(), 1, 2, |_| true)
            .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cannot preserve complete scene state"),
        "{error}"
    );
}

struct RepeatedSource {
    prefix: Vec<u8>,
    repeated: Vec<u8>,
    repetitions: u64,
    position: u64,
    largest_read: usize,
}
impl RepeatedSource {
    fn length(&self) -> u64 {
        self.prefix.len() as u64 + self.repetitions * self.repeated.len() as u64
    }
}
impl Read for RepeatedSource {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.largest_read = self.largest_read.max(output.len());
        assert!(
            output.len() <= 64 << 10,
            "stream reader requested an unbounded input buffer"
        );
        if self.position == self.length() {
            return Ok(0);
        }
        let data = if self.position < self.prefix.len() as u64 {
            &self.prefix[self.position as usize..]
        } else {
            let offset = (self.position - self.prefix.len() as u64) % self.repeated.len() as u64;
            &self.repeated[offset as usize..]
        };
        let length = output.len().min(data.len());
        output[..length].copy_from_slice(&data[..length]);
        self.position += length as u64;
        Ok(length)
    }
}
impl Seek for RepeatedSource {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let position = match position {
            SeekFrom::Start(value) => i128::from(value),
            SeekFrom::End(value) => i128::from(self.length()) + i128::from(value),
            SeekFrom::Current(value) => i128::from(self.position) + i128::from(value),
        };
        if !(0..=i128::from(self.length())).contains(&position) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        self.position = position as u64;
        Ok(self.position)
    }
}

#[test]
#[ignore = "explicit resource regression streams more than 512 MiB of synthetic bytes without allocating the whole capture"]
fn capture_larger_than_the_in_memory_limit_streams_with_fixed_input_buffers() {
    let mut prefix = fixture(0);
    prefix.extend(group(
        &[
            string(2, "OpaqueOwner"),
            string(3, "payload"),
            string(4, "OpaqueData"),
            string(5, "synthetic"),
            instance(2),
        ],
        0,
    ));
    let repeated = group(
        &[
            section(18, 0),
            property(&vec![73; 1 << 20], 4),
            section(19, 0),
        ],
        0,
    );
    let mut source = RepeatedSource {
        prefix,
        repeated,
        repetitions: 513,
        position: 0,
        largest_read: 0,
    };
    let length = source.length();
    assert!(length > 512 << 20);
    let report =
        trim_scene_frames_streaming(&mut source, &mut io::sink(), 510, 512, |_| true).unwrap();
    assert_eq!(report.source_bytes, length);
    assert_eq!(report.source_frames, 513);
    assert_eq!(report.output_frames, 3);
    assert!(report.output_bytes > 512 << 20);
    assert!(source.largest_read <= 64 << 10);
    assert!(source.prefix.len() + source.repeated.len() < 2 << 20);
    eprintln!(
        "synthetic source={} output={} retained input template={} max read={}",
        report.source_bytes,
        report.output_bytes,
        source.prefix.len() + source.repeated.len(),
        source.largest_read
    );
}

#[test]
#[ignore = "requires official SDK 3.4 captures; byte-for-byte comparison with the validated in-memory clip exporter"]
fn real_sdk_streaming_clips_match_existing_scene_renderer_analysis_and_lifecycle_exports() {
    for (filename, ranges) in [
        (
            "physx-3.4.pvdstream",
            &[(0, 0), (1, 119), (59, 61), (119, 119)][..],
        ),
        (
            "physx-3.4-rich.pvdstream",
            &[(0, 119), (1, 1), (70, 100), (119, 119)][..],
        ),
        (
            "physx-3.4-joint-limits.pvdstream",
            &[(0, 5), (2, 5), (5, 5)][..],
        ),
        (
            "physx-3.4-heightfield.pvdstream",
            &[(0, 74), (35, 55), (74, 74)][..],
        ),
        (
            "physx-3.4-query-shapes.pvdstream",
            &[(0, 11), (3, 3), (5, 8)][..],
        ),
        ("physx-3.4-error.pvdstream", &[(0, 11), (5, 8)][..]),
        (
            "physx-3.4-all.pvdstream",
            &[(0, 0), (70, 100), (119, 119)][..],
        ),
    ] {
        let path = capture(filename);
        let bytes = std::fs::read(&path).unwrap();
        for &(first, last) in ranges {
            let expected = trim_scene_frames(&bytes, first, last).unwrap();
            let mut output = Vec::new();
            let report = trim_scene_frames_streaming(
                &mut File::open(&path).unwrap(),
                &mut output,
                first,
                last,
                |_| true,
            )
            .unwrap_or_else(|error| panic!("{filename} {first}..={last}: {error}"));
            assert_eq!(output, expected, "{filename} {first}..={last}");
            assert_eq!(report.source_blake3, *blake3::hash(&bytes).as_bytes());
            let actual = Recording::from_bytes(&output).unwrap();
            actual.verify_replay().unwrap();
        }
        eprintln!(
            "{filename}: {} streaming ranges match every SDK output byte",
            ranges.len()
        );
    }
}
