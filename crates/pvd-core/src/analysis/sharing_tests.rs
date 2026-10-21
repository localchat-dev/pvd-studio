use super::*;

fn restore_reference(analysis: &mut Analysis) {
    use std::collections::HashMap;
    fn intern(value: &mut Arc<str>, strings: &mut HashMap<Arc<str>, Arc<str>>) {
        *value = strings
            .entry(value.clone())
            .or_insert_with(|| value.clone())
            .clone();
    }
    fn allocation(
        value: &mut Arc<Allocation>,
        strings: &mut HashMap<Arc<str>, Arc<str>>,
        allocations: &mut HashMap<Allocation, Arc<Allocation>>,
    ) {
        let entry = Arc::make_mut(value);
        intern(&mut entry.type_name, strings);
        intern(&mut entry.file, strings);
        *value = allocations
            .entry(entry.clone())
            .or_insert_with(|| value.clone())
            .clone();
    }
    let mut strings = HashMap::new();
    let mut allocations = HashMap::new();
    let memory = Arc::make_mut(&mut analysis.memory);
    for value in memory.strings.values_mut() {
        intern(value, &mut strings);
    }
    for value in memory
        .live
        .values_mut()
        .chain(Arc::make_mut(&mut memory.base).live.values_mut())
    {
        allocation(value, &mut strings, &mut allocations);
    }
    for event in &mut memory.events {
        match &mut Arc::make_mut(event).kind {
            MemoryEventKind::Allocate {
                allocation: value,
                replaced,
            } => {
                allocation(value, &mut strings, &mut allocations);
                if let Some(value) = replaced {
                    allocation(value, &mut strings, &mut allocations);
                }
            }
            MemoryEventKind::Free {
                allocation: Some(value),
            } => allocation(value, &mut strings, &mut allocations),
            MemoryEventKind::Free { allocation: None } => (),
        }
    }
}

fn assert_equivalent(actual: &Analysis, expected: &Analysis) {
    assert_eq!(
        postcard::to_stdvec(actual).unwrap(),
        postcard::to_stdvec(expected).unwrap()
    );
    assert_eq!(actual.retained_bytes(), expected.retained_bytes());
    assert_eq!(actual.accounted_bytes(), expected.accounted_bytes());
    let actual_shared: SharedAnalysis = actual.clone().into();
    let expected_shared: SharedAnalysis = expected.clone().into();
    assert!(std::ptr::eq(
        actual_shared.legacy().unwrap(),
        actual_shared.clone().legacy().unwrap()
    ));
    assert!(std::ptr::eq(
        expected_shared.legacy().unwrap(),
        expected_shared.clone().legacy().unwrap()
    ));
    for frame in [0, 1, 3, 12, 4, 2, 99, 0] {
        assert_eq!(
            actual.memory.state_at_frame(frame),
            expected.memory.state_at_frame(frame)
        );
    }
}

fn fixture() -> Analysis {
    fn name(handle: u32, text: &str) -> Vec<u8> {
        let mut bytes = 1u16.to_le_bytes().to_vec();
        bytes.extend(((text.len() + 1) as u32).to_le_bytes());
        bytes.extend(text.as_bytes());
        bytes.push(0);
        bytes.extend(handle.to_le_bytes());
        bytes
    }
    fn allocate(size: u8, address: u8) -> Vec<u8> {
        let mut bytes = 2u16.to_le_bytes().to_vec();
        bytes.extend([size, 1, 2, 20, address]);
        bytes
    }
    let mut names = name(1, "same-content");
    names.extend(name(2, "source.cpp"));
    names.extend(name(3, "same-content"));
    names.extend(allocate(64, 88));
    names.extend(allocate(32, 89));
    let mut analysis = Analysis::default();
    for (frame, bytes) in [
        names,
        allocate(96, 88),
        vec![3, 0, 89],
        vec![3, 0, 99],
        allocate(8, 90),
        vec![3, 0, 88],
        allocate(7, 88),
    ]
    .iter()
    .enumerate()
    {
        analysis
            .memory_buffer(1, bytes, Endian::Little, frame)
            .unwrap();
    }
    analysis
        .memory_buffer(2, &name(1, "same-content"), Endian::Little, 7)
        .unwrap();
    analysis.retain_recent(1);
    analysis
}

#[test]
fn restore_preserves_wire_accounting_and_shared_metadata_without_mutating_snapshots() {
    let bytes = postcard::to_stdvec(&fixture()).unwrap();
    let mut expected: Analysis = postcard::from_bytes(&bytes).unwrap();
    let mut actual: Analysis = postcard::from_bytes(&bytes).unwrap();
    let held = actual.clone();
    let held_bytes = postcard::to_stdvec(&held).unwrap();
    let held_retained = held.retained_bytes();
    restore_reference(&mut expected);
    actual.restore_sharing();
    assert_equivalent(&actual, &expected);
    assert_eq!(postcard::to_stdvec(&actual).unwrap(), bytes);
    assert_eq!(postcard::to_stdvec(&held).unwrap(), held_bytes);
    assert_eq!(held.retained_bytes(), held_retained);
    assert_eq!(actual.memory.strings.len(), 4);
    assert_eq!(
        actual.memory.strings.keys().collect::<Vec<_>>(),
        held.memory.strings.keys().collect::<Vec<_>>()
    );
    assert!(Arc::ptr_eq(
        &actual.memory.strings[&(1, 1)],
        &actual.memory.strings[&(1, 3)]
    ));
    assert!(Arc::ptr_eq(
        &actual.memory.strings[&(1, 1)],
        &actual.memory.strings[&(2, 1)]
    ));
    assert!(actual.memory.events.iter().any(|event| matches!(
        event.kind,
        MemoryEventKind::Allocate {
            replaced: Some(_),
            ..
        }
    )));
    assert_eq!(
        actual
            .memory
            .events
            .iter()
            .filter(|event| matches!(event.kind, MemoryEventKind::Free { allocation: None }))
            .count(),
        1
    );
    assert!(actual.memory.events.iter().any(|event| matches!(
        event.kind,
        MemoryEventKind::Free {
            allocation: Some(_)
        }
    )));
    actual.restore_sharing();
    assert_equivalent(&actual, &expected);
}

#[test]
fn full_allocation_identity_distinguishes_each_field_and_reuses_only_equal_content() {
    let seed = Allocation {
        address: 8,
        size: 32,
        type_name: "type".into(),
        file: "source.cpp".into(),
        line: 42,
        received_frame: 7,
    };
    let mut values = vec![seed.clone(); 7];
    values[1].address += 1;
    values[2].size += 1;
    values[3].type_name = "other-type".into();
    values[4].file = "other.cpp".into();
    values[5].line += 1;
    values[6].received_frame += 1;
    let mut original = Analysis::default();
    let memory = Arc::make_mut(&mut original.memory);
    for (index, value) in values.into_iter().enumerate() {
        memory
            .live
            .insert((index as u64, value.address), Arc::new(value.clone()));
        Arc::make_mut(&mut memory.base)
            .live
            .insert((index as u64, value.address), Arc::new(value.clone()));
        memory.events.push(Arc::new(MemoryEvent {
            buffer: index as u64,
            address: value.address,
            received_frame: value.received_frame,
            kind: MemoryEventKind::Allocate {
                allocation: Arc::new(value),
                replaced: None,
            },
        }));
    }
    let bytes = postcard::to_stdvec(&original).unwrap();
    let mut expected: Analysis = postcard::from_bytes(&bytes).unwrap();
    let mut actual: Analysis = postcard::from_bytes(&bytes).unwrap();
    restore_reference(&mut expected);
    actual.restore_sharing();
    assert_equivalent(&actual, &expected);
    let unique: BTreeSet<_> = actual.memory.live.values().map(Arc::as_ptr).collect();
    assert_eq!(unique.len(), 7);
    for (key, value) in &actual.memory.live {
        assert!(Arc::ptr_eq(value, &actual.memory.base.live[key]));
        let MemoryEventKind::Allocate { allocation, .. } =
            &actual.memory.events[key.0 as usize].kind
        else {
            unreachable!()
        };
        assert!(Arc::ptr_eq(value, allocation));
    }
}

#[test]
fn empty_restore_matches_production() {
    let mut actual = Analysis::default();
    let mut expected = actual.clone();
    restore_reference(&mut expected);
    actual.restore_sharing();
    assert_equivalent(&actual, &expected);
    assert!(actual.memory.live.is_empty());
    assert!(actual.memory.strings.is_empty());
}

#[test]
fn shared_snapshots_and_later_mutations_keep_the_same_cache_accounting() {
    let bytes = postcard::to_stdvec(&fixture()).unwrap();
    let mut actual: Analysis = postcard::from_bytes(&bytes).unwrap();
    let mut expected: Analysis = postcard::from_bytes(&bytes).unwrap();
    let held_actual = actual.clone();
    let held_expected = expected.clone();
    let actual_events = actual.memory.events.clone();
    let expected_events = expected.memory.events.clone();
    let actual_state = actual.memory.state_at_frame(4);
    let expected_state = expected.memory.state_at_frame(4);
    let held_actual_bytes = postcard::to_stdvec(&held_actual).unwrap();
    let held_state_bytes = postcard::to_stdvec(&actual_state).unwrap();
    actual.restore_sharing();
    restore_reference(&mut expected);
    assert_equivalent(&actual, &expected);
    assert_eq!(
        actual.additional_cache_bytes(&held_actual),
        expected.additional_cache_bytes(&held_expected)
    );
    assert_eq!(
        postcard::to_stdvec(&held_actual).unwrap(),
        held_actual_bytes
    );
    assert_eq!(
        postcard::to_stdvec(&actual_state).unwrap(),
        held_state_bytes
    );
    assert_eq!(actual_state, expected_state);
    assert_eq!(actual_events, expected_events);

    let mut actual: SharedAnalysis = actual.into();
    let mut expected: SharedAnalysis = expected.into();
    let mut actual_cursor = MemoryCursor::default();
    let mut expected_cursor = MemoryCursor::default();
    for (index, data) in [
        vec![2, 0, 12, 1, 2, 21, 88],
        vec![3, 0, 90],
        vec![3, 0, 99],
        vec![2, 0, 13, 1, 2, 22, 90],
        vec![3, 0, 88],
    ]
    .iter()
    .enumerate()
    {
        let before_actual = actual.clone();
        let before_expected = expected.clone();
        actual
            .ingest(
                1,
                "sdk::PxProfileMemoryEventBuffer",
                data,
                Endian::Little,
                8 + index,
            )
            .unwrap();
        expected
            .ingest(
                1,
                "sdk::PxProfileMemoryEventBuffer",
                data,
                Endian::Little,
                8 + index,
            )
            .unwrap();
        assert_eq!(
            actual
                .legacy()
                .unwrap()
                .additional_cache_bytes(before_actual.legacy().unwrap()),
            expected
                .legacy()
                .unwrap()
                .additional_cache_bytes(before_expected.legacy().unwrap())
        );
        assert_equivalent(actual.legacy().unwrap(), expected.legacy().unwrap());
        for frame in [12, 1, 7, 3, 11, 2, 99] {
            assert_eq!(
                actual
                    .legacy()
                    .unwrap()
                    .memory
                    .seek(&mut actual_cursor, frame),
                expected
                    .legacy()
                    .unwrap()
                    .memory
                    .seek(&mut expected_cursor, frame)
            );
        }
        if index % 2 == 1 {
            actual.retain_recent(7 + index).unwrap();
            expected.retain_recent(7 + index).unwrap();
            assert_eq!(
                actual
                    .legacy()
                    .unwrap()
                    .additional_cache_bytes(before_actual.legacy().unwrap()),
                expected
                    .legacy()
                    .unwrap()
                    .additional_cache_bytes(before_expected.legacy().unwrap())
            );
            assert_equivalent(actual.legacy().unwrap(), expected.legacy().unwrap());
        }
        assert_equivalent(
            before_actual.legacy().unwrap(),
            before_expected.legacy().unwrap(),
        );
    }
    assert_eq!(
        postcard::to_stdvec(&held_actual).unwrap(),
        held_actual_bytes
    );
    assert_eq!(
        postcard::to_stdvec(&actual_state).unwrap(),
        held_state_bytes
    );
    actual.legacy_mut().unwrap().restore_sharing();
    restore_reference(expected.legacy_mut().unwrap());
    assert_equivalent(actual.legacy().unwrap(), expected.legacy().unwrap());
}

#[test]
fn event_only_unique_and_duplicate_long_records_preserve_complete_identity() {
    for duplicate in [false, true] {
        let mut original = Analysis::default();
        let memory = Arc::make_mut(&mut original.memory);
        for index in 0..512 {
            let allocation = Arc::new(Allocation {
                address: if duplicate { 8 } else { index },
                size: 32,
                type_name: "type".repeat(512).into(),
                file: "source.cpp".repeat(1024).into(),
                line: 42,
                received_frame: 0,
            });
            memory.events.push(Arc::new(MemoryEvent {
                buffer: 7,
                address: allocation.address,
                received_frame: 0,
                kind: MemoryEventKind::Allocate {
                    allocation,
                    replaced: None,
                },
            }));
        }
        let bytes = postcard::to_stdvec(&original).unwrap();
        let mut actual: Analysis = postcard::from_bytes(&bytes).unwrap();
        let mut expected: Analysis = postcard::from_bytes(&bytes).unwrap();
        actual.restore_sharing();
        restore_reference(&mut expected);
        assert_equivalent(&actual, &expected);
        let unique: BTreeSet<_> = actual
            .memory
            .events
            .iter()
            .map(|event| {
                let MemoryEventKind::Allocate { allocation, .. } = &event.kind else {
                    unreachable!()
                };
                Arc::as_ptr(allocation)
            })
            .collect();
        assert_eq!(unique.len(), if duplicate { 1 } else { 512 });
        assert!(actual.memory.live.is_empty());
        assert!(actual.memory.base.live.is_empty());
    }
}
