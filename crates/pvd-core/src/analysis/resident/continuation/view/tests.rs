use super::*;

#[test]
fn resident_borrowed_profile_and_memory_facades_match_legacy_without_dense_adaptation() {
    let expected = super::super::tests::source();
    let domain = Domain::new(8 << 20);
    let actual = ResidentAnalysis::from_legacy_bytes(
        &postcard::to_stdvec(&expected).unwrap(),
        domain.clone(),
        &mut || false,
    )
    .unwrap();
    assert_eq!(actual.profile_summary(), expected.profile_summary());
    assert_eq!(
        postcard::to_stdvec(&actual.profile_spans().collect::<Vec<_>>()).unwrap(),
        postcard::to_stdvec(&expected.profile_spans().collect::<Vec<_>>()).unwrap()
    );
    assert_eq!(
        postcard::to_stdvec(&actual.profile_values().collect::<Vec<_>>()).unwrap(),
        postcard::to_stdvec(&expected.profile_values().collect::<Vec<_>>()).unwrap()
    );
    assert_eq!(
        actual.profile_names().collect::<Vec<_>>(),
        expected.profile_names().collect::<Vec<_>>()
    );
    assert_eq!(
        actual.open_profile_events().collect::<Vec<_>>(),
        expected.open_profile_events().collect::<Vec<_>>()
    );
    for key in [(3, 2, 4, 5), (9, 4, 7, 8), (999, 1, 2, 3)] {
        assert_eq!(
            actual.profile_open_event(key),
            expected.profile_open_event(key)
        );
        assert_eq!(
            actual.unmatched_profile_event(key),
            expected.unmatched_profile_event(key)
        );
    }
    let expected: crate::analysis::SharedAnalysis = expected.into();
    let memory = expected.memory_snapshot();
    let mut cursor = ResidentMemoryCursor::default();
    let mut legacy = crate::analysis::view::MemoryViewCursor::default();
    for frame in [0, 1, 2, 0, 3, 99] {
        MemorySource::seek(&actual, &mut cursor, frame).unwrap();
        memory.seek(&mut legacy, frame).unwrap();
        assert_eq!(cursor.live_count(), legacy.live_count());
        assert_eq!(cursor.live_bytes(), legacy.live_bytes());
        for index in 0..=cursor.live_count() {
            assert_eq!(cursor.allocation_at(index), legacy.allocation_at(index));
        }
        assert_eq!(
            MemorySource::frame_events(&actual, frame),
            memory.frame_events(frame)
        );
        assert_eq!(MemorySource::churn(&actual, frame), memory.churn(frame));
    }
    assert_eq!(actual.event_count(), memory.event_count());
    for index in 0..actual.event_count() {
        assert_eq!(actual.event_at(index), memory.event_at(index));
    }
    assert_eq!(actual.sample_count(), memory.sample_count());
    for index in 0..actual.sample_count() {
        assert_eq!(
            postcard::to_stdvec(&actual.sample_at(index)).unwrap(),
            postcard::to_stdvec(&memory.sample_at(index)).unwrap()
        );
    }
    MemorySource::seek(&actual, &mut cursor, 2).unwrap();
    assert_eq!(cursor.allocation_at(0).unwrap().size, 40);
    drop(actual);
    assert!(domain.statistics().logical_used > 0);
    assert!(cursor.live_count() > 0);
    assert_eq!(cursor.allocation_at(0).unwrap().file, "second.cpp");
    drop(cursor);
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn cursor_switches_source_only_after_success_and_keeps_owned_rows() {
    let bytes = postcard::to_stdvec(&super::super::tests::source()).unwrap();
    let first_domain = Domain::new(8 << 20);
    let second_domain = Domain::new(8 << 20);
    let first =
        ResidentAnalysis::from_legacy_bytes(&bytes, first_domain.clone(), &mut || false).unwrap();
    let second =
        ResidentAnalysis::from_legacy_bytes(&bytes, second_domain.clone(), &mut || false).unwrap();
    let mut cursor = ResidentMemoryCursor::default();
    MemorySource::seek(&first, &mut cursor, 2).unwrap();
    let original = cursor.0.as_ref().unwrap().state().clone();
    let statistics = second_domain.statistics();
    let block = second_domain
        .reserve(statistics.limit - statistics.logical_used)
        .unwrap();
    assert!(MemorySource::seek(&second, &mut cursor, 2).is_err());
    assert!(cursor.0.as_ref().unwrap().state().0.same(&original.0));
    drop(block);
    MemorySource::seek(&second, &mut cursor, 2).unwrap();
    assert!(
        cursor
            .0
            .as_ref()
            .unwrap()
            .state()
            .0
            .domain()
            .same(&second_domain)
    );
    drop((first, original));
    assert_eq!(first_domain.statistics().logical_used, 0);
    drop(second);
    assert_eq!(cursor.allocation_at(0).unwrap().size, 40);
    drop(cursor);
    assert_eq!(second_domain.statistics().logical_used, 0);
}

#[test]
fn resident_churn_keeps_frame_cohort_survivors_after_retention_and_reverse_seek() {
    use crate::analysis::Analysis;
    use pvd_protocol::Endian;

    let mut source = Analysis::default();
    let mut strings = Vec::new();
    for (handle, text) in [(1u32, "Block"), (2, "source.cpp")] {
        strings.extend(1u16.to_le_bytes());
        strings.extend((text.len() as u32 + 1).to_le_bytes());
        strings.extend(text.bytes());
        strings.push(0);
        strings.extend(handle.to_le_bytes());
    }
    let alloc = |size: u8, address: u8| vec![2, 0, size, 1, 2, 17, address];
    let free = |address: u8| vec![3, 0, address];
    for (buffer, frame, data) in [
        (7, 0, [strings.clone(), alloc(64, 5)].concat()),
        (
            7,
            1,
            [
                free(5),
                alloc(24, 6),
                alloc(32, 6),
                free(6),
                alloc(0, 7),
                alloc(40, 8),
            ]
            .concat(),
        ),
        (9, 1, [strings, alloc(12, 8)].concat()),
        (7, 2, free(8)),
        (9, 2, alloc(16, 8)),
    ] {
        source
            .ingest(
                buffer,
                "PxProfileMemoryEventBuffer",
                &data,
                Endian::Little,
                frame,
            )
            .unwrap();
    }
    let domain = Domain::new(8 << 20);
    let mut actual = ResidentAnalysis::from_legacy_bytes(
        &postcard::to_stdvec(&source).unwrap(),
        domain,
        &mut || false,
    )
    .unwrap();
    let mut cursor = ResidentMemoryCursor::default();
    for (frame, count, bytes) in [
        (0, 1, 64),
        (1, 3, 52),
        (2, 1, 16),
        (1, 3, 52),
        (0, 1, 64),
        (9, 0, 0),
    ] {
        MemorySource::seek(&actual, &mut cursor, frame).unwrap();
        let churn = MemorySource::churn(&actual, frame);
        assert_eq!(
            (churn.surviving_allocations, churn.surviving_bytes),
            (count, bytes)
        );
        assert_eq!(churn, source.memory.churn_at_frame(frame));
    }
    actual.try_retain_recent(1, &mut || false).unwrap();
    source.retain_recent(1);
    for frame in [1, 2, 1] {
        assert_eq!(
            MemorySource::churn(&actual, frame),
            source.memory.churn_at_frame(frame)
        );
    }
}
