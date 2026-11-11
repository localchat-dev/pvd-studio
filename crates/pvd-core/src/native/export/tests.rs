use super::*;
use std::{
    cell::Cell,
    io::{self, Cursor, Read, Seek, SeekFrom, Write},
    path::PathBuf,
};

fn fixture() -> Vec<u8> {
    let directory = std::env::var_os("PVD_NATIVE_FIXTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../captures/native"));
    std::fs::read(directory.join("baseline-paired-native.pxd2")).unwrap()
}

fn rejected<T>(result: Result<T, Error>, message: &str) {
    let error = match result {
        Ok(_) => panic!("expected rejection: {message}"),
        Err(e) => e.to_string(),
    };
    assert!(
        error.contains(message),
        "expected {message:?}, got {error:?}"
    );
}

#[test]
fn output_budget_checks_addition_and_limit_before_allocating() {
    assert_eq!(output_size(OUTPUT_LIMIT - 1, 1).unwrap(), OUTPUT_LIMIT);
    rejected(output_size(OUTPUT_LIMIT, 1), "output exceeds");
    rejected(output_size(usize::MAX, 1), "output exceeds");
}

#[test]
fn clip_reader_rejects_oversized_sources_and_whole_reader_honors_cancellation() {
    struct AdvertisedLength;
    impl Read for AdvertisedLength {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            panic!("oversized input must not be read")
        }
    }
    impl Seek for AdvertisedLength {
        fn seek(&mut self, _: SeekFrom) -> io::Result<u64> {
            Ok(OUTPUT_LIMIT as u64 + 1)
        }
    }
    let mut output = Vec::new();
    rejected(
        write_reader(
            &mut AdvertisedLength,
            Selection::Frames { first: 0, last: 0 },
            &mut output,
            |_| true,
        ),
        "input exceeds 512 MiB",
    );
    let mut source = Cursor::new(vec![0; 128]);
    let error = write_reader(&mut source, Selection::Whole, &mut output, |_| false).unwrap_err();
    assert_eq!(
        error.downcast_ref::<io::Error>().unwrap().kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(source.position(), 0);
    assert!(output.is_empty());
}

#[test]
fn malformed_source_never_writes_to_the_sink() {
    for length in [0, 4, 8, 16, 64] {
        let mut output = Vec::new();
        assert!(
            write_reader(
                &mut Cursor::new(vec![0; length]),
                Selection::Whole,
                &mut output,
                |_| true,
            )
            .is_err()
        );
        assert!(output.is_empty());
    }
}

#[test]
#[ignore = "requires preserved real PVD3 baseline container"]
fn reader_rechecks_source_content_and_observes_post_write_cancellation() {
    struct ChangingSource {
        bytes: Cursor<Vec<u8>>,
        passes: u32,
    }
    impl Read for ChangingSource {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            self.bytes.read(output)
        }
    }
    impl Seek for ChangingSource {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            if position == SeekFrom::Start(0) {
                self.passes += 1;
                if self.passes == 2 {
                    self.bytes.get_mut()[0] ^= 1;
                }
            }
            self.bytes.seek(position)
        }
    }
    let mut output = Vec::new();
    rejected(
        write_reader(
            &mut ChangingSource {
                bytes: Cursor::new(fixture()),
                passes: 0,
            },
            Selection::Whole,
            &mut output,
            |_| true,
        ),
        "source changed during export",
    );
    assert!(output.is_empty());
    output.clear();
    let error = write_reader(
        &mut Cursor::new(fixture()),
        Selection::Whole,
        &mut output,
        |progress| progress.phase != Phase::Recheck,
    )
    .unwrap_err();
    assert_eq!(
        error.downcast_ref::<io::Error>().unwrap().kind(),
        io::ErrorKind::Interrupted
    );
    assert!(!output.is_empty());
}

#[test]
#[ignore = "requires preserved real PVD3 baseline container"]
fn strict_scope_and_reference_guards_reject_without_output() {
    let bytes = fixture();
    let mut output = Vec::new();
    rejected(
        trim_scene_frames(&bytes, 2, 1, &mut output, || true),
        "first frame exceeds last frame",
    );
    rejected(
        trim_scene_frames(&bytes, 0, 120, &mut output, || true),
        "last frame out of range",
    );
    assert!(output.is_empty());
    let mut layout = Layout::parse(&bytes, &mut || true).unwrap();
    layout.communication.cameras.push((
        layout.communication.sections[0].name,
        layout.communication.instances[0].id,
    ));
    rejected(
        layout.trim_prefix(90, &mut || true),
        "camera saved body has a different class",
    );
    let mut layout = Layout::parse(&bytes, &mut || true).unwrap();
    layout.snapshot_bytes[0] = Cow::Borrowed(&[1, 0, 0, 0]);
    rejected(
        layout.trim_prefix(90, &mut || true),
        "array count exceeds containing block",
    );
    let mut layout = Layout::parse(&bytes, &mut || true).unwrap();
    layout.snapshots[0].event = 1;
    rejected(
        layout.trim_prefix(90, &mut || true),
        "one event-zero snapshot",
    );
    let mut layout = Layout::parse(&bytes, &mut || true).unwrap();
    layout
        .communication
        .sections
        .push(layout.communication.sections[0].clone());
    rejected(layout.trim_prefix(90, &mut || true), "referenced twice");
    let mut layout = Layout::parse(&bytes, &mut || true).unwrap();
    let scene_class = layout
        .events
        .iter()
        .find_map(|event| match event {
            HistoryEvent::Create(_, class)
                if layout
                    .metadata
                    .strings
                    .get(&layout.metadata.class(*class).unwrap().name.name)
                    .is_some_and(|n| n == "PxScene") =>
            {
                Some(*class)
            }
            _ => None,
        })
        .unwrap();
    layout
        .events
        .push(HistoryEvent::Create(900_000, scene_class));
    rejected(
        layout.trim_prefix(90, &mut || true),
        "Scene is missing its source identity",
    );
    let mut layout = Layout::parse(&bytes, &mut || true).unwrap();
    let repeated = layout
        .events
        .iter()
        .find(|e| matches!(e, HistoryEvent::Create(..)))
        .unwrap()
        .clone();
    layout.events.push(repeated);
    rejected(
        layout.trim_prefix(0, &mut || true),
        "container local ID reuse",
    );
    let mut layout = Layout::parse(&bytes, &mut || true).unwrap();
    let scene_source = layout.communication.sections[0].source;
    layout
        .communication
        .instances
        .retain(|instance| instance.source != scene_source);
    rejected(
        layout.trim_prefix(90, &mut || true),
        "Scene is missing its source identity",
    );
    let layout = Layout::parse(&bytes, &mut || true).unwrap();
    rejected(
        HistoryEvent::parse(&12u32.to_le_bytes(), 0, &layout.metadata, &mut 0),
        "unsupported container event 12",
    );
    let mut event = layout
        .events
        .iter()
        .find(|e| matches!(e, HistoryEvent::Set { .. }))
        .unwrap()
        .clone();
    if let HistoryEvent::Set { undo, .. } = &mut event {
        undo.push(3);
    }
    let mut encoded = Vec::new();
    event.encode(&mut encoded).unwrap();
    rejected(
        HistoryEvent::parse(&encoded, 2, &layout.metadata, &mut 0),
        "undo reference exceeds",
    );
    let mut layout = Layout::parse(&bytes, &mut || true).unwrap();
    layout.communication.sections[0].rows[0][3] += 1;
    rejected(layout.validate_sections(), "section");
    let mut layout = Layout::parse(&bytes, &mut || true).unwrap();
    if let Some(HistoryEvent::Saved { position, .. }) = layout
        .events
        .iter_mut()
        .find(|e| matches!(e, HistoryEvent::Saved { .. }))
    {
        *position += 1;
    }
    let encoded = layout.encode(false, &mut || true).unwrap();
    rejected(Layout::parse(&encoded, &mut || true), "saved instance");
}

#[test]
#[ignore = "requires preserved real PVD3 baseline container"]
fn compact_tiny_pages_are_rejected_during_planning_before_output_allocation() {
    const MARKERS: usize = 140_000;
    let bytes = fixture();
    let mut layout = Layout::parse(&bytes, &mut || true).unwrap();
    let mut events = vec![HistoryEvent::Marker; MARKERS];
    for mut event in layout.events.drain(..) {
        match &mut event {
            HistoryEvent::Set { undo, .. } | HistoryEvent::Array { undo, .. } => {
                for n in undo {
                    *n += MARKERS as u32;
                }
            }
            _ => {}
        }
        events.push(event);
    }
    layout.events = events;
    for section in &mut layout.communication.sections {
        for row in &mut section.rows {
            row[1] += MARKERS as u64;
            row[3] += MARKERS as u64;
        }
    }
    let mut position = layout
        .saved
        .iter()
        .map(|s| s.position + s.body.len() as u64 + 12)
        .max()
        .unwrap()
        .max(4);
    layout.pages.clear();
    for (i, event) in layout.events.iter().enumerate() {
        let len = encode_page(std::slice::from_ref(event), &mut || true)
            .unwrap()
            .len() as u32;
        layout.pages.push(Page {
            first: i as u64,
            position,
            count: 1,
            len,
        });
        position += u64::from(len);
    }
    let compact = layout.encode(false, &mut || true).unwrap();
    assert!(compact.len() < 16 << 20);
    let mut parsed = Layout::parse(&compact, &mut || true).unwrap();
    rejected(
        parsed.trim_prefix(0, &mut || true),
        "output exceeds 512 MiB",
    );
    println!(
        "compact {} byte input with {} pages rejected by output planning",
        compact.len(),
        MARKERS + 2602
    );
}

fn ref_word_offset(layout: &Layout<'_>, saved: &Saved<'_>) -> Option<usize> {
    let mut r = Reader::new(&saved.body);
    let fixed = r.blob().unwrap();
    let mut chain = Vec::new();
    let mut current = saved.class;
    while current != u32::MAX {
        chain.push(current);
        current = layout.metadata.class(current).unwrap().base;
    }
    for class in chain.into_iter().rev() {
        for id in &layout.metadata.class(class).unwrap().properties {
            let property = layout.metadata.property(*id).unwrap();
            let datatype = layout.metadata.class(property.datatype_id).unwrap();
            let data = if property.kind == 2 {
                r.blob().unwrap()
            } else {
                &fixed[property.offset32..property.offset32 + datatype.size]
            };
            for element in data.chunks_exact(datatype.size) {
                for &(kind, offset) in &layout.pointer_layouts[property.datatype_id as usize] {
                    if kind == 1 {
                        return Some(
                            element.as_ptr() as usize + offset - saved.body.as_ptr() as usize,
                        );
                    }
                }
            }
        }
    }
    None
}

#[test]
#[ignore = "requires preserved real PVD3 baseline container"]
fn saved_records_require_valid_layout_prefix_references_and_exact_event_state() {
    let original = fixture();
    let layout = Layout::parse(&original, &mut || true).unwrap();
    let cut = layout.communication.sections[0].rows[90][3] as usize + 1;
    let retained: BTreeSet<_> = layout.events[..cut]
        .iter()
        .filter_map(|e| match e {
            HistoryEvent::Saved { id, .. } => Some(*id),
            _ => None,
        })
        .collect();
    let saved = layout
        .saved
        .iter()
        .find(|s| retained.contains(&s.id) && ref_word_offset(&layout, s).is_some())
        .unwrap();
    let offset = saved.body.as_ptr() as usize - original.as_ptr() as usize;
    let reference_offset = ref_word_offset(&layout, saved).unwrap();
    let saved_id = saved.id;
    let class = saved.class;
    let mut malformed = original.clone();
    malformed[offset..offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(
        crate::Recording::from_bytes(&malformed).is_ok(),
        "forward reader does not consume auxiliary saved bodies"
    );
    let mut out = Vec::new();
    assert!(trim_scene_frames(&malformed, 0, 90, &mut out, || true).is_err());
    assert!(out.is_empty());
    let mut future_ref = original.clone();
    future_ref[offset + reference_offset..offset + reference_offset + 4]
        .copy_from_slice(&900_000u32.to_le_bytes());
    let mut layout = Layout::parse(&future_ref, &mut || true).unwrap();
    layout.events.push(HistoryEvent::Create(900_000, class));
    let page = layout.pages.last_mut().unwrap();
    page.count += 1;
    page.len = encode_page(&layout.events[page.first as usize..], &mut || true)
        .unwrap()
        .len() as u32;
    let encoded = layout.encode(false, &mut || true).unwrap();
    assert!(crate::Recording::from_bytes(&encoded).is_ok());
    rejected(
        trim_scene_frames(&encoded, 0, 90, &mut out, || true),
        "outside retained object lifecycles",
    );
    assert!(out.is_empty());
    let mut layout = Layout::parse(&original, &mut || true).unwrap();
    let saved = layout.saved.iter().find(|s| s.id == saved_id).unwrap();
    let datatype_with_ref = layout
        .pointer_layouts
        .iter()
        .position(|offsets| offsets.iter().any(|(kind, _)| *kind == 1))
        .unwrap();
    // The packed-pointer table is independent of decoded Value variants.
    let before = layout.pointer_layouts[datatype_with_ref].clone();
    for (kind, _) in &mut layout.pointer_layouts[datatype_with_ref] {
        *kind = 99;
    }
    let created = layout.events[..cut]
        .iter()
        .filter_map(|e| match e {
            HistoryEvent::Create(id, _) => Some(*id),
            _ => None,
        })
        .collect();
    let result = layout.verify_saved_pointers(saved, &created, &mut || true);
    assert!(result.is_err(), "unsupported pointer kind must fail");
    layout.pointer_layouts[datatype_with_ref] = before;
    let mut changed = original.clone();
    let word = offset + reference_offset;
    let previous = u32::from_le_bytes(original[word..word + 4].try_into().unwrap());
    let different = if previous == 0 { 1u32 } else { 0u32 };
    changed[word..word + 4].copy_from_slice(&different.to_le_bytes());
    rejected(
        trim_scene_frames(&changed, 0, 90, &mut out, || true),
        "saved record disagrees with state",
    );
    assert!(out.is_empty());
}

struct FailingWriter {
    remaining: usize,
    written: usize,
}
impl Write for FailingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "injected sink failure",
            ));
        }
        let n = self.remaining.min(bytes.len());
        self.remaining -= n;
        self.written += n;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
#[ignore = "requires preserved real PVD3 baseline container"]
fn cancellation_is_observed_in_parse_planning_encoding_and_sink_failures_propagate() {
    let bytes = fixture();
    let mut output = Vec::new();
    assert!(rewrite(&bytes, &mut output, || false).is_err());
    assert!(output.is_empty());
    let mut calls = 0;
    assert!(
        Layout::parse(&bytes, &mut || {
            calls += 1;
            calls < 20
        })
        .is_err()
    );
    assert_eq!(calls, 20);
    let mut layout = Layout::parse(&bytes, &mut || true).unwrap();
    let mut calls = 0;
    assert!(
        layout
            .trim_prefix(90, &mut || {
                calls += 1;
                calls < 20
            })
            .is_err()
    );
    assert_eq!(calls, 20);
    let events = vec![HistoryEvent::Marker; 1000];
    let mut calls = 0;
    assert!(
        encode_page(&events, &mut || {
            calls += 1;
            calls < 20
        })
        .is_err()
    );
    assert_eq!(calls, 20);
    let mut writer = FailingWriter {
        remaining: 31,
        written: 0,
    };
    rejected(
        rewrite(&bytes, &mut writer, || true),
        "injected sink failure",
    );
    assert_eq!(writer.written, 31);
    let cancelled = Cell::new(false);
    struct CancelAfterWrite<'a> {
        cancelled: &'a Cell<bool>,
        written: usize,
    }
    impl Write for CancelAfterWrite<'_> {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.written += b.len();
            self.cancelled.set(true);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut writer = CancelAfterWrite {
        cancelled: &cancelled,
        written: 0,
    };
    assert!(rewrite(&bytes, &mut writer, || !cancelled.get()).is_err());
    assert_eq!(writer.written, 64 << 10);
}

#[test]
#[ignore = "requires preserved real PVD3 baseline container"]
fn ordinary_packed_properties_cannot_reference_an_object_created_after_the_cut() {
    let original = fixture();
    let layout = Layout::parse(&original, &mut || true).unwrap();
    let cut = layout.communication.sections[0].rows[0][3] as usize + 1;
    let data = layout.events[..cut]
        .iter()
        .find_map(|event| match event {
            HistoryEvent::Set {
                kind: 3,
                property,
                data,
                ..
            } => {
                let property = layout.metadata.property(*property).unwrap();
                (data.len() == 4
                    && layout.pointer_layouts[property.datatype_id as usize] == [(1, 0)])
                .then_some(data.as_ref())
            }
            _ => None,
        })
        .unwrap();
    let offset = data.as_ptr() as usize - original.as_ptr() as usize;
    let mut changed = original.clone();
    changed[offset..offset + 4].copy_from_slice(&900_000u32.to_le_bytes());
    let mut layout = Layout::parse(&changed, &mut || true).unwrap();
    let class = layout
        .events
        .iter()
        .find_map(|event| match event {
            HistoryEvent::Create(_, class) => Some(*class),
            _ => None,
        })
        .unwrap();
    layout.events.push(HistoryEvent::Create(900_000, class));
    let page = layout.pages.last_mut().unwrap();
    page.count += 1;
    page.len = encode_page(&layout.events[page.first as usize..], &mut || true)
        .unwrap()
        .len() as u32;
    let encoded = layout.encode(false, &mut || true).unwrap();
    assert!(crate::Recording::from_bytes(&encoded).is_ok());
    let mut output = Vec::new();
    rejected(
        trim_scene_frames(&encoded, 0, 0, &mut output, || true),
        "event packed reference is outside retained object lifecycles",
    );
    assert!(output.is_empty());
}

#[test]
#[ignore = "requires preserved real PVD3 baseline container"]
fn dynamic_datatype_without_declared_pointers_is_still_rejected() {
    let bytes = fixture();
    let mut layout = Layout::parse(&bytes, &mut || true).unwrap();
    let (property, datatype) = layout
        .metadata
        .properties
        .iter()
        .enumerate()
        .find_map(|(id, p)| {
            let p = p.as_ref()?;
            layout.pointer_layouts[p.datatype_id as usize]
                .is_empty()
                .then_some((id as u32, p.datatype_id as usize))
        })
        .unwrap();
    layout.metadata.classes[datatype].as_mut().unwrap().dynamic = true;
    rejected(
        layout.verify_property_pointers(property, &[], &BTreeSet::new(), &mut || true),
        "unsupported packed pointer datatype layout",
    );
}
