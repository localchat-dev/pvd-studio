use super::*;

#[test]
fn prefix_digest_hashes_original_header_bits_and_all_auxiliary_rows_without_reencoding() {
    let properties = [[20u32, 12], [10, 9], [20, 10], [10, 8], [20, 11]]
        .into_iter()
        .flatten()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    let event = Event {
        id: 1,
        shift_bits: [2.0f32.to_bits(), 4.0f32.to_bits(), 0x8000_0000],
        shifted_properties: &properties,
    };
    let mut expected = Vec::new();
    for word in [11, 1, 2.0f32.to_bits(), 4.0f32.to_bits(), 0x8000_0000, 5] {
        expected.extend_from_slice(&word.to_le_bytes());
    }
    expected.extend_from_slice(&properties);
    let mut expected_hash = blake3::Hasher::new();
    expected_hash.update(&(expected.len() as u64).to_le_bytes());
    expected_hash.update(&expected);
    let mut hash = blake3::Hasher::new();
    event.digest(&mut hash, &mut || true).unwrap();
    assert_eq!(hash.finalize(), expected_hash.finalize());
    let mut reordered = properties.clone();
    reordered[..16].rotate_left(8);
    for changed in [
        Event {
            shift_bits: [2.0f32.to_bits(), 4.0f32.to_bits(), 0],
            ..event.clone()
        },
        Event {
            shifted_properties: &reordered,
            ..event.clone()
        },
        Event {
            shifted_properties: &properties[..properties.len() - 8],
            ..event.clone()
        },
    ] {
        let mut changed_hash = blake3::Hasher::new();
        changed.digest(&mut changed_hash, &mut || true).unwrap();
        assert_ne!(changed_hash.finalize(), hash.finalize());
    }
}

#[test]
fn prefix_digest_checks_each_bounded_auxiliary_chunk_for_cancellation() {
    let rows = vec![0; (64 << 10) + 8];
    let event = Event {
        id: 1,
        shift_bits: [0; 3],
        shifted_properties: &rows,
    };
    let mut calls = 0;
    event
        .digest(&mut blake3::Hasher::new(), &mut || {
            calls += 1;
            true
        })
        .unwrap();
    assert_eq!(calls, 3);
    for stop in 1..=calls {
        let mut current = 0;
        let failure = event
            .digest(&mut blake3::Hasher::new(), &mut || {
                current += 1;
                current != stop
            })
            .unwrap_err();
        assert_eq!(
            failure.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
        assert_eq!(current, stop);
    }
}
