use super::*;
use pvd_core::{
    analysis_export::{AnalysisCsvKind, export_analysis_csv_streaming},
    profile_timeline::{self, TickRange, WindowLimits, WindowRequest},
};

fn memory_events(mode: u8) -> Vec<Vec<u8>> {
    let bytes = [2, 0, 56, 13, 40, 22, 99];
    let mut events = vec![
        string(20, "PxProfileMemoryEventBuffer"),
        string(21, "MemoryMessage"),
        instance(20, 99),
    ];
    match mode {
        0 => events.push(payload(99, &bytes)),
        1 => {
            let mut begin = vec![8];
            begin.extend(99u64.to_le_bytes());
            for value in [5u32, 1, 6] {
                begin.extend(value.to_le_bytes());
            }
            events.push(begin);
            for chunk in bytes.chunks(3) {
                let mut append = vec![9];
                append.extend((chunk.len() as u32).to_le_bytes());
                append.extend(chunk);
                append.extend((chunk.len() as u32).to_le_bytes());
                events.push(append);
            }
            events.push(vec![10]);
        }
        2 | 3 => {
            let mut definition = vec![5];
            for value in [1u32, 20, 1, 21, 1, 5, 1, 6, 0, 7, 7] {
                definition.extend(value.to_le_bytes());
            }
            events.push(definition);
            let mut message = vec![if mode == 2 { 11 } else { 13 }];
            message.extend(99u64.to_le_bytes());
            if mode == 2 {
                message.extend(1u32.to_le_bytes());
                message.extend(21u32.to_le_bytes());
            } else {
                let mut begin = vec![12];
                begin.extend(1u32.to_le_bytes());
                begin.extend(21u32.to_le_bytes());
                events.push(begin);
            }
            message.extend(7u32.to_le_bytes());
            message.extend(bytes);
            events.push(message);
            if mode == 3 {
                events.push(vec![14]);
            }
        }
        _ => unreachable!(),
    }
    events
}

fn source(mode: u8) -> Vec<u8> {
    let source = fixture(false, false, false, false);
    let prefix_end = 28 + 24 + u32::from_le_bytes(source[28..32].try_into().unwrap()) as usize;
    let mut bytes = source[..prefix_end].to_vec();
    bytes.extend(group(&memory_events(mode)));
    bytes.extend(&source[prefix_end..]);
    bytes
}

fn request() -> WindowRequest {
    WindowRequest {
        ticks: TickRange { start: 0, end: 600 },
        thread: None,
        context: None,
        limits: WindowLimits::default(),
    }
}

#[test]
fn memory_dictionary_failure_does_not_block_profile_catalog_curves_or_timeline() {
    let cancel = AtomicBool::new(false);
    let reference = Recording::from_bytes(&fixture(false, false, false, false)).unwrap();
    for mode in 0..4 {
        let bytes = source(mode);
        let failure = Recording::from_bytes(&bytes).err().unwrap();
        assert_eq!(failure.to_string(), "undefined memory type handle 13");
        let catalog = catalog_streaming(&mut Cursor::new(&bytes), &cancel, |_| true).unwrap();
        assert_eq!(catalog.identity.blake3, *blake3::hash(&bytes).as_bytes());
        assert_eq!(catalog.identity.frames, 7);
        assert_eq!(catalog.entries.len(), 6);
        assert_eq!(
            catalog
                .entries
                .iter()
                .map(|entry| entry.complete_spans)
                .sum::<usize>(),
            7
        );
        assert_eq!(
            catalog
                .entries
                .iter()
                .map(|entry| entry.open_spans)
                .sum::<usize>(),
            1
        );
        let key = ExactKey {
            zone: 11,
            event: 1,
            thread: 7,
            context: 9,
        };
        let exact =
            catalog_key_streaming(&mut Cursor::new(&bytes), key, &cancel, |_| true).unwrap();
        assert_eq!(exact.entries.len(), 1);
        assert_eq!(exact.entries[0].complete_spans, 1);
        assert_eq!(exact.entries[0].first_start, Some(110));
        let page =
            catalog_page_streaming(&mut Cursor::new(&bytes), None, 2, &cancel, |_| true).unwrap();
        assert_eq!(page.entries.len(), 2);
        assert!(page.next.is_some());
        let curve = sample_streaming(
            &mut Cursor::new(&bytes),
            &reference,
            key,
            ReferenceScene {
                id: 9,
                generation: 0,
            },
            &[0, 2, 3],
            &cancel,
            |_| true,
        )
        .unwrap();
        assert_eq!(curve.points[0].status, Status::FirstFrame);
        assert_eq!(curve.points[1].milliseconds, Some(9.0));
        assert_eq!(curve.points[1].occurrence_count, 1);
        assert_eq!(curve.points[2].status, Status::Missing);
        let window = profile_timeline::window_streaming(
            &mut Cursor::new(&bytes),
            &catalog.identity,
            request(),
            &cancel,
            |_| true,
        )
        .unwrap();
        assert_eq!(window.global_pairing.complete_spans, 7);
        assert_eq!(window.global_pairing.open_starts, 1);
        let mut intervals: Vec<_> = window
            .spans
            .iter()
            .map(|span| {
                (
                    span.zone,
                    span.event,
                    span.thread,
                    span.context,
                    span.start,
                    span.end,
                )
            })
            .collect();
        intervals.sort_unstable();
        assert_eq!(
            intervals,
            [
                (10, 1, 7, 8, 130, 140),
                (10, 1, 7, 9, 110, 120),
                (10, 1, 7, 9, 200, 200),
                (10, 1, 8, 9, 150, 170),
                (10, 2, 7, 9, 190, 210),
                (10, 3, 7, 9, 250, 260),
                (11, 1, 7, 9, 110, 119),
            ]
        );
    }
}

#[test]
fn profile_queries_still_validate_scene_protocol_profile_data_and_cancellation() {
    let cancel = AtomicBool::new(false);
    for tail in [
        group(&[payload(1234, &[1])]),
        group(&[payload(10, &[255])]),
        vec![1],
    ] {
        let mut bytes = source(0);
        bytes.extend(tail);
        assert!(catalog_streaming(&mut Cursor::new(bytes), &cancel, |_| true).is_err());
    }
    let bytes = source(0);
    assert!(catalog_streaming(&mut Cursor::new(&bytes), &AtomicBool::new(true), |_| true).is_err());
    assert!(catalog_streaming(&mut Cursor::new(&bytes), &cancel, |_| false).is_err());
    for kind in [AnalysisCsvKind::Profile, AnalysisCsvKind::Memory] {
        let mut output = Vec::new();
        let failure =
            export_analysis_csv_streaming(&mut Cursor::new(&bytes), &mut output, kind, |_| true)
                .unwrap_err();
        assert_eq!(failure.to_string(), "undefined memory type handle 13");
        assert!(output.is_empty());
    }
}
