use super::*;

#[test]
fn borrowed_fragments_match_contiguous_edits_at_every_boundary() {
    let source = b"abcdefghijkl";
    for len in 0..=source.len() {
        for offset in 0..=len {
            for removed in 0..=len - offset {
                for data in [b"".as_slice(), b"Z", b"XYZ"] {
                    let mut expected = source[..len].to_vec();
                    expected.splice(offset..offset + removed, data.iter().copied());
                    let mut value = View::new(&source[..len]).unwrap();
                    let mut budget = Budget::default();
                    value
                        .splice(offset, removed, data, &mut budget, &mut || true)
                        .unwrap();
                    value.verify(&expected, &mut budget, &mut || true).unwrap();
                    assert!(value.parts.iter().all(|part| !part.is_empty()));
                }
            }
        }
    }
}

#[test]
fn repeated_fragmented_edits_and_resets_match_an_independent_byte_vector() {
    let source = b"abcdefghijkl";
    let payloads = [b"".as_slice(), b"X", b"YZ", b"PQR"];
    let mut expected = source.to_vec();
    let mut value = View::new(source).unwrap();
    let mut budget = Budget::default();
    let mut random = 0x1234_5678u64;
    for step in 0..1000 {
        random = random
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        if step % 31 == 0 {
            value.reset(source, &mut budget).unwrap();
            expected = source.to_vec();
        }
        let offset = random as usize % (expected.len() + 1);
        let removed = (random >> 16) as usize % (expected.len() - offset + 1);
        let data = payloads[(random >> 32) as usize % payloads.len()];
        expected.splice(offset..offset + removed, data.iter().copied());
        value
            .splice(offset, removed, data, &mut budget, &mut || true)
            .unwrap();
        value.verify(&expected, &mut budget, &mut || true).unwrap();
    }
}

#[test]
fn native_insert_appends_and_native_set_ignores_out_of_range_indices() {
    for index in [3, 99, u32::MAX] {
        let mut value = View::new(b"abc").unwrap();
        let mut budget = Budget::default();
        value
            .insert(index, 1, b"X", &mut budget, &mut || true)
            .unwrap();
        value.verify(b"abcX", &mut budget, &mut || true).unwrap();
        value.set(4, 1, b"Y", &mut budget, &mut || true).unwrap();
        value
            .set(u32::MAX, 1, b"Y", &mut budget, &mut || true)
            .unwrap();
        value.verify(b"abcX", &mut budget, &mut || true).unwrap();
    }
}

#[test]
fn failed_fragment_planning_keeps_the_borrowed_value_and_bounds_allocation() {
    let mut value = View::new(b"abcd").unwrap();
    let mut empty = Budget { remaining: 0 };
    assert!(
        value
            .splice(2, 1, b"X", &mut empty, &mut || true)
            .unwrap_err()
            .to_string()
            .contains("budget")
    );
    value
        .verify(b"abcd", &mut Budget::default(), &mut || true)
        .unwrap();
    let count = MAX_METADATA / std::mem::size_of::<&[u8]>() + 1;
    assert!(
        value
            .reserve(count)
            .unwrap_err()
            .to_string()
            .contains("fragment index")
    );
    let bytes = vec![0u8; MAX_METADATA];
    let mut maximum = View::new(&bytes).unwrap();
    assert!(
        maximum
            .splice(bytes.len(), 0, b"X", &mut Budget::default(), &mut || true)
            .unwrap_err()
            .to_string()
            .contains("64 MiB")
    );
    assert_eq!(maximum.len, MAX_METADATA);
    assert_eq!(maximum.parts.len(), 1);
}

#[test]
fn comparison_checks_cancellation_inside_large_fragments() {
    let bytes = vec![0u8; 4 * (64 << 10)];
    let expected = bytes.clone();
    let value = View::new(&bytes).unwrap();
    let mut checks = 0;
    let failure = value
        .verify(&expected, &mut Budget::default(), &mut || {
            checks += 1;
            checks < 4
        })
        .unwrap_err();
    assert!(failure.to_string().contains("cancelled"));
    assert_eq!(checks, 4);
}

#[test]
fn unmatched_message_scanning_preserves_the_array_and_checks_budget_and_cancellation() {
    let mut metadata = crate::native::container_tests::fixture();
    metadata.messages.push(Some(crate::native::Message {
        class_name: metadata.classes[3].as_ref().unwrap().name,
        class: 3,
        size: 4,
        entries: (0..2048)
            .map(|_| crate::native::MessageEntry {
                datatype: metadata.classes[0].as_ref().unwrap().name,
                datatype_id: 0,
                property: 0,
                offset: 0,
                size: 4,
                destination_size: 4,
            })
            .collect(),
    }));
    let bytes = 10u32.to_le_bytes();
    let mut instance = RawInstance::create(&metadata, 4).unwrap();
    instance.set(&metadata, 2, &bytes).unwrap();
    let baseline = BTreeMap::from([(2, instance)]);
    let events = [
        HistoryEvent::Marker,
        HistoryEvent::Set {
            kind: 4,
            id: 2,
            property: 0,
            data: bytes.as_slice().into(),
            undo: vec![1],
        },
        HistoryEvent::Set {
            kind: 3,
            id: 2,
            property: 2,
            data: bytes.as_slice().into(),
            undo: vec![1],
        },
    ];
    verify(
        &metadata,
        &baseline,
        &events,
        2,
        &baseline[&2],
        &mut Budget::default(),
        &mut || true,
    )
    .unwrap();
    let failure = verify(
        &metadata,
        &baseline,
        &events,
        2,
        &baseline[&2],
        &mut Budget { remaining: 256 },
        &mut || true,
    )
    .unwrap_err();
    assert!(failure.to_string().contains("work budget"), "{failure}");
    let mut checks = 0;
    let failure = verify(
        &metadata,
        &baseline,
        &events,
        2,
        &baseline[&2],
        &mut Budget::default(),
        &mut || {
            checks += 1;
            checks < 80
        },
    )
    .unwrap_err();
    assert!(failure.to_string().contains("cancelled"), "{failure}");
    assert_eq!(checks, 80);
}
