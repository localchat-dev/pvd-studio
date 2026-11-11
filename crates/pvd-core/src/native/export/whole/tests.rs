use super::*;
use std::io::{self, Cursor};

fn fixture() -> Vec<u8> {
    super::super::memory_fixture::source(true)
}

#[test]
fn whole_save_keeps_snapshots_saved_versions_padding_and_tail_bytes() {
    let mut bytes = fixture();
    bytes.extend((0..100_003).map(|i| (i % 251) as u8));
    let mut expected = Vec::new();
    let old = rewrite(&bytes, &mut expected, || true).unwrap();
    let mut output = Vec::new();
    let report = write_reader(
        &mut Cursor::new(&bytes),
        Selection::Whole,
        &mut output,
        |_| true,
    )
    .unwrap();
    assert_eq!(output, bytes);
    assert_eq!(output, expected);
    assert_eq!(report.source_blake3, blake3::hash(&bytes));
    assert_eq!(report.frames, 3);
    assert_eq!(report.saved_instances, 5);
    assert_eq!(report.events, old.events);
    assert_eq!(report.output_bytes, bytes.len());
}

#[test]
fn streamed_whole_save_preserves_all_structural_rejections() {
    let bytes = fixture();
    let layout = Layout::parse(&bytes, &mut || true).unwrap();
    let base = layout.backing.as_ptr() as usize - bytes.as_ptr() as usize;
    let directory = next_page(
        16 + layout.object_bytes.len() + layout.communication.encode(&mut || true).unwrap().len(),
    )
    .unwrap();
    let page = base + layout.pages[0].position as usize;
    let payload = page + 4 + layout.pages[0].count as usize * 4;
    let first_saved = base + layout.saved[0].position as usize;
    let begin = layout.communication.sections[0].rows[0][1] as usize;
    let begin_offset = u32::from_le_bytes(
        bytes[page + 4 + begin * 4..page + 8 + begin * 4]
            .try_into()
            .unwrap(),
    ) as usize;
    let snapshot = base + layout.snapshots[0].position as usize;
    let cases = [
        (page + 8, 0u32.to_le_bytes().to_vec()),
        (payload, 77u32.to_le_bytes().to_vec()),
        (first_saved + 4, 999u32.to_le_bytes().to_vec()),
        (first_saved + 8, u32::MAX.to_le_bytes().to_vec()),
        (payload + begin_offset + 12, 0x999u64.to_le_bytes().to_vec()),
        (snapshot + 16, 1u32.to_le_bytes().to_vec()),
        (directory + 20, 0u64.to_le_bytes().to_vec()),
        (directory + 40, u64::MAX.to_le_bytes().to_vec()),
    ];
    for (offset, replacement) in cases {
        let mut bad = bytes.clone();
        bad[offset..offset + replacement.len()].copy_from_slice(&replacement);
        assert!(
            rewrite(&bad, &mut Vec::new(), || true).is_err(),
            "legacy accepted offset {offset}"
        );
        let mut output = Vec::new();
        assert!(
            write_reader(
                &mut Cursor::new(&bad),
                Selection::Whole,
                &mut output,
                |_| true
            )
            .is_err(),
            "stream accepted offset {offset}"
        );
        assert!(output.is_empty());
    }
}

#[test]
fn streamed_save_cancels_in_every_phase_and_propagates_sink_failure() {
    let bytes = fixture();
    for phase in [Phase::Read, Phase::Export, Phase::Recheck] {
        let mut output = Vec::new();
        let failure = write_reader(
            &mut Cursor::new(&bytes),
            Selection::Whole,
            &mut output,
            |p| p.phase != phase,
        )
        .unwrap_err();
        assert_eq!(
            failure.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(output.is_empty(), phase != Phase::Recheck);
    }
    struct BrokenSink;
    impl Write for BrokenSink {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::StorageFull.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let failure = write_reader(
        &mut Cursor::new(&bytes),
        Selection::Whole,
        &mut BrokenSink,
        |_| true,
    )
    .unwrap_err();
    assert_eq!(
        failure.downcast_ref::<io::Error>().unwrap().kind(),
        io::ErrorKind::StorageFull
    );
}

#[test]
fn whole_save_rejects_missing_and_duplicate_section_endpoints() {
    let bytes = fixture();
    for duplicate in [false, true] {
        let mut layout = Layout::parse(&bytes, &mut || true).unwrap();
        if duplicate {
            let mut section = layout.communication.sections[0].clone();
            section.name = 10;
            layout.communication.sections.push(section);
        } else {
            layout.communication.sections.clear();
        }
        let invalid = layout.encode(false, &mut || true).unwrap();
        let mut output = Vec::new();
        let failure = write_reader(
            &mut Cursor::new(invalid),
            Selection::Whole,
            &mut output,
            |_| true,
        )
        .unwrap_err();
        assert!(failure.to_string().contains(if duplicate {
            "referenced twice"
        } else {
            "section table disagrees"
        }));
        assert!(output.is_empty());
    }
}

#[test]
fn source_changes_during_copy_are_detected_for_copied_and_pending_blocks() {
    use std::{cell::RefCell, rc::Rc};
    struct Mutable {
        data: Rc<RefCell<Vec<u8>>>,
        position: u64,
    }
    impl Read for Mutable {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let data = self.data.borrow();
            let start = (self.position as usize).min(data.len());
            let size = output.len().min(data.len() - start);
            output[..size].copy_from_slice(&data[start..start + size]);
            self.position += size as u64;
            Ok(size)
        }
    }
    impl Seek for Mutable {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            let position = match position {
                SeekFrom::Start(p) => i128::from(p),
                SeekFrom::Current(p) => i128::from(self.position) + i128::from(p),
                SeekFrom::End(p) => self.data.borrow().len() as i128 + i128::from(p),
            };
            self.position = u64::try_from(position)
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
            Ok(self.position)
        }
    }
    for changed_at in [0, 64 << 10] {
        let mut bytes = fixture();
        bytes.resize(3 * (64 << 10) + 7, 0x5a);
        let data = Rc::new(RefCell::new(bytes));
        let mut source = Mutable {
            data: data.clone(),
            position: 0,
        };
        let mut changed = false;
        let mut output = Vec::new();
        let failure = write_reader(&mut source, Selection::Whole, &mut output, |p| {
            if p.phase == Phase::Export && p.completed > 0 && !changed {
                data.borrow_mut()[changed_at] ^= 1;
                changed = true;
            }
            true
        })
        .unwrap_err();
        assert!(changed);
        assert!(failure.to_string().contains("source changed"));
        assert_eq!(
            output.len(),
            if changed_at == 0 {
                data.borrow().len()
            } else {
                64 << 10
            }
        );
    }
}

struct PaddedSource {
    prefix: Vec<u8>,
    length: u64,
    position: u64,
    largest_read: usize,
}

impl Read for PaddedSource {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let size = (self.length.saturating_sub(self.position)).min(output.len() as u64) as usize;
        self.largest_read = self.largest_read.max(size);
        output[..size].fill(0);
        let begin = (self.position as usize).min(self.prefix.len());
        let prefix_size = (self.prefix.len() - begin).min(size);
        output[..prefix_size].copy_from_slice(&self.prefix[begin..begin + prefix_size]);
        self.position += size as u64;
        Ok(size)
    }
}

impl Seek for PaddedSource {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let position = match position {
            SeekFrom::Start(p) => i128::from(p),
            SeekFrom::End(p) => i128::from(self.length) + i128::from(p),
            SeekFrom::Current(p) => i128::from(self.position) + i128::from(p),
        };
        self.position =
            u64::try_from(position).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        Ok(self.position)
    }
}

#[test]
#[ignore = "scans a constructed 513 MiB recording three times without retaining its padding"]
fn whole_save_above_512_mib_uses_bounded_reads_and_preserves_every_byte() {
    let prefix = fixture();
    struct CheckedSink<'a> {
        prefix: &'a [u8],
        written: u64,
    }
    impl Write for CheckedSink<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            assert!(bytes.len() <= 64 << 10);
            let begin = (self.written as usize).min(self.prefix.len());
            let size = (self.prefix.len() - begin).min(bytes.len());
            assert_eq!(&bytes[..size], &self.prefix[begin..begin + size]);
            assert!(bytes[size..].iter().all(|byte| *byte == 0));
            self.written += bytes.len() as u64;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut sink = CheckedSink {
        prefix: &prefix,
        written: 0,
    };
    let mut source = PaddedSource {
        prefix: prefix.clone(),
        length: (513 << 20) + 19,
        position: 0,
        largest_read: 0,
    };
    let report = write_reader(&mut source, Selection::Whole, &mut sink, |_| true).unwrap();
    assert_eq!(sink.written, source.length);
    assert_eq!(report.source_bytes as u64, source.length);
    assert_eq!(report.frames, 3);
    assert_eq!(report.saved_instances, 5);
    assert!(source.largest_read <= 64 << 10);
}

#[test]
#[ignore = "requires preserved real PVD3 containers"]
fn whole_streamed_save_matches_original_containers_and_byte_reconstruction() {
    let directory = std::env::var_os("PVD_NATIVE_FIXTURES")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../captures/native")
        });
    for name in [
        "baseline-paired-native.pxd2",
        "rich-paired-native.pxd2",
        "limits/joint-limits-live-native.pxd2",
    ] {
        let bytes = std::fs::read(directory.join(name)).unwrap();
        let mut expected = Vec::new();
        let old = rewrite(&bytes, &mut expected, || true).unwrap();
        let mut output = Vec::new();
        let actual = write_reader(
            &mut Cursor::new(&bytes),
            Selection::Whole,
            &mut output,
            |_| true,
        )
        .unwrap();
        assert_eq!(output, bytes, "{name}");
        assert_eq!(output, expected, "{name}");
        assert_eq!(actual.frames, old.frames);
        assert_eq!(actual.events, old.events);
        assert_eq!(actual.saved_instances, old.saved_instances);
    }
}
