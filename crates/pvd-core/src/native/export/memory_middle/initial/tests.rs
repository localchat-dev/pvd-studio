use super::*;

use crate::native::export::memory_fixture as fixture;

fn captured<'a>(layout: &'a Layout<'_>) -> Values<'a> {
    read(
        layout,
        [2, 3],
        [8, 9],
        &mut 0,
        &mut Work::default(),
        &mut || true,
    )
    .unwrap()
    .unwrap()
}

#[test]
fn captured_pair_uses_initial_positions_and_keeps_nonzero_fixed_bytes() {
    for deleted in [false, true] {
        let bytes = fixture::source(deleted);
        let mut layout = Layout::parse(&bytes, &mut || true).unwrap();
        layout.saved.sort_by_key(|saved| !saved.deleted);
        if deleted {
            for (id, payload) in [(2, fixture::second_flush()), (3, fixture::totals(&[0, 16]))] {
                let first = layout.saved.iter().find(|saved| saved.id == id).unwrap();
                assert!(first.deleted);
                assert_eq!(first.body.as_ref(), fixture::body(&payload));
            }
        }
        for _ in 0..2 {
            let values = captured(&layout);
            assert_eq!(values.fixed, [&[0x5a; 24][..]; 2]);
            assert_eq!(values.payloads[0], fixture::old_buffer());
            assert_eq!(values.payloads[1], fixture::totals(&[64, 16]));
            layout.saved.reverse();
        }
    }
}

#[test]
fn captured_pair_rejects_deleted_aliases_and_mismatched_saved_identities() {
    let source = fixture::source(true);
    for variant in 0..3 {
        let mut layout = Layout::parse(&source, &mut || true).unwrap();
        let entry = layout
            .saved
            .iter()
            .position(|saved| saved.id == 2 && !saved.deleted)
            .unwrap();
        match variant {
            0 => layout.saved[entry].deleted = true,
            1 => layout.saved[entry].id = 99,
            2 => layout.saved[entry].class = 9,
            _ => unreachable!(),
        }
        let failure = read(
            &layout,
            [2, 3],
            [8, 9],
            &mut 0,
            &mut Work::default(),
            &mut || true,
        )
        .err()
        .unwrap();
        assert!(
            failure.to_string().contains("exact live saved body"),
            "{failure}"
        );
    }
}

#[test]
fn captured_pair_directory_scan_respects_shared_budget_work_and_cancellation() {
    let source = fixture::source(false);
    let layout = Layout::parse(&source, &mut || true).unwrap();
    let bytes = std::mem::size_of::<Values<'_>>();
    let mut allocation = MAX_METADATA - bytes;
    let mut work = Work { remaining: 171 };
    read(
        &layout,
        [2, 3],
        [8, 9],
        &mut allocation,
        &mut work,
        &mut || true,
    )
    .unwrap()
    .unwrap();
    assert_eq!((allocation, work.remaining), (MAX_METADATA, 0));
    assert!(
        read(
            &layout,
            [2, 3],
            [8, 9],
            &mut (MAX_METADATA - bytes + 1),
            &mut Work::default(),
            &mut || true
        )
        .is_err()
    );
    assert!(
        read(
            &layout,
            [2, 3],
            [8, 9],
            &mut 0,
            &mut Work { remaining: 170 },
            &mut || true
        )
        .is_err()
    );
    let mut calls = 0;
    let failure = read(
        &layout,
        [2, 3],
        [8, 9],
        &mut 0,
        &mut Work::default(),
        &mut || {
            calls += 1;
            calls < 3
        },
    )
    .err()
    .unwrap();
    assert_eq!(calls, 3);
    assert_eq!(
        failure.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
}

#[test]
fn genuine_marker_undo_is_only_available_to_a_captured_initial_pair() {
    let source = fixture::source(false);
    let layout = Layout::parse(&source, &mut || true).unwrap();
    captured(&layout);
    source_undo(
        &layout,
        2,
        (2, 3),
        SourcePrevious {
            event: 0,
            initial: true,
        },
        &[2],
        &mut Work::default(),
        &mut || true,
    )
    .unwrap();
    let failure = source_undo(
        &layout,
        2,
        (2, 3),
        SourcePrevious {
            event: 0,
            initial: false,
        },
        &[2],
        &mut Work::default(),
        &mut || true,
    )
    .unwrap_err();
    assert!(failure.to_string().contains("source undo targets another"));
    assert!(
        source_undo(
            &layout,
            2,
            (2, 3),
            SourcePrevious {
                event: 0,
                initial: true
            },
            &[1],
            &mut Work::default(),
            &mut || true
        )
        .is_err()
    );
}
