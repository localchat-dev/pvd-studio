use super::*;

fn id(origin: u8, sequence: u64) -> identity::WireIdentity {
    identity::WireIdentity {
        origin: [origin; 16],
        sequence,
    }
}

fn row(index: u64) -> AllocationRow {
    AllocationRow {
        address: index + 100,
        size: index * 7,
        type_name: RowRef { page: 2, row: 1 },
        file: RowRef { page: 3, row: 2 },
        line: index as u32,
        received_frame: index * 19,
    }
}

fn assert_rows(actual: &[AllocationRow], expected: &[AllocationRow]) {
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(actual.address, expected.address);
        assert_eq!(actual.size, expected.size);
        assert_eq!(actual.type_name, expected.type_name);
        assert_eq!(actual.file, expected.file);
        assert_eq!(actual.line, expected.line);
        assert_eq!(actual.received_frame, expected.received_frame);
    }
}

fn flat_page(rows: &[AllocationRow], identities: &[identity::WireIdentity]) -> Vec<u8> {
    let mut out = Encoder::new(IDENTIFIED_VALUES_MAGIC);
    out.u16(rows.len() as u16);
    for (row, identity) in rows.iter().zip(identities) {
        out.identity(*identity);
        out.u64(row.address);
        out.u64(row.size);
        out.row(row.type_name);
        out.row(row.file);
        out.u32(row.line);
        out.u64(row.received_frame);
    }
    out.bytes
}

#[test]
fn repeated_identity_and_equal_sequences_from_distinct_origins_stay_distinct() {
    let identities = [id(1, 5), id(1, 6), id(1, 5), id(2, 5), id(2, 6)];
    let mut dictionary = IdentityDictionary::new();
    for identity in identities {
        dictionary.insert(identity).unwrap();
    }
    assert_eq!(dictionary.len, 2);
    let mut out = Encoder::new(COMPACT_VALUES_MAGIC);
    dictionary.write(&mut out);
    for identity in identities {
        dictionary.write_row(&mut out, identity).unwrap();
    }
    let mut input = Decoder::new(&out.bytes, COMPACT_VALUES_MAGIC).unwrap();
    let read = IdentityDictionary::read(&mut input, identities.len()).unwrap();
    for expected in identities {
        assert_eq!(read.read_row(&mut input).unwrap(), expected);
    }
    input.finish().unwrap();
    assert_eq!(
        out.bytes.len(),
        8 + dictionary.encoded_bytes(identities.len()).unwrap()
    );
}

#[test]
fn all_sixty_four_origins_are_bounded_without_a_heap_directory() {
    let mut dictionary = IdentityDictionary::new();
    for origin in 1..=64 {
        dictionary.insert(id(origin, u64::from(origin))).unwrap();
    }
    assert_eq!(dictionary.len, 64);
    assert!(size_of::<IdentityDictionary>() <= 1040);
    assert!(dictionary.insert(id(65, 1)).is_err());
    assert_eq!(dictionary.len, 64);
    let mut out = Encoder::new(COMPACT_VALUES_MAGIC);
    dictionary.write(&mut out);
    for origin in 1..=64 {
        dictionary
            .write_row(&mut out, id(origin, u64::from(origin)))
            .unwrap();
    }
    let mut input = Decoder::new(&out.bytes, COMPACT_VALUES_MAGIC).unwrap();
    let read = IdentityDictionary::read(&mut input, 64).unwrap();
    for origin in 1..=64 {
        assert_eq!(
            read.read_row(&mut input).unwrap(),
            id(origin, u64::from(origin))
        );
    }
    input.finish().unwrap();
}

#[test]
fn invalid_sources_counts_sequences_and_row_indices_are_rejected() {
    let mut dictionary = IdentityDictionary::new();
    for invalid in [id(0, 1), id(1, 0), id(1, u64::MAX)] {
        assert!(dictionary.insert(invalid).is_err());
    }
    dictionary.insert(id(1, 1)).unwrap();
    let mut output = Encoder::new(COMPACT_VALUES_MAGIC);
    assert!(dictionary.write_row(&mut output, id(2, 1)).is_err());
    assert_eq!(output.bytes.len(), 8);
    for count in [0, 2, 65, 255] {
        let mut out = Encoder::new(COMPACT_VALUES_MAGIC);
        out.u8(count);
        let mut input = Decoder::new(&out.bytes, COMPACT_VALUES_MAGIC).unwrap();
        assert!(IdentityDictionary::read(&mut input, 1).is_err());
    }
    for origin in [[0; 16], [1; 16]] {
        let mut out = Encoder::new(COMPACT_VALUES_MAGIC);
        out.u8(2);
        out.bytes.extend([1; 16]);
        out.bytes.extend(origin);
        let mut input = Decoder::new(&out.bytes, COMPACT_VALUES_MAGIC).unwrap();
        assert!(IdentityDictionary::read(&mut input, 2).is_err());
    }
    for (index, sequence) in [(1, 1), (0, 0), (0, u64::MAX)] {
        let mut out = Encoder::new(COMPACT_VALUES_MAGIC);
        out.u8(index);
        out.u64(sequence);
        let mut input = Decoder::new(&out.bytes, COMPACT_VALUES_MAGIC).unwrap();
        assert!(dictionary.read_row(&mut input).is_err());
    }
    assert!(dictionary.encoded_bytes(0).is_err());
    assert!(dictionary.encoded_bytes(usize::MAX).is_err());
}

#[test]
fn compact_values_keep_every_field_and_reduce_repeated_origin_bytes() {
    let rows = (0..127).map(row).collect::<Vec<_>>();
    let identities = (0..127)
        .map(|index| id(1, index % 11 + 1))
        .collect::<Vec<_>>();
    let encoded = encode_identified_values(&rows, &identities).unwrap();
    assert_eq!(&encoded[..8], TABLE_VALUES_MAGIC);
    assert_eq!(encoded.len(), 10 + 17 + 127 * 61);
    assert!(encoded.len() < flat_page(&rows, &identities).len());
    let (decoded, decoded_ids) = decode_identified_values(&encoded).unwrap();
    assert_rows(&decoded, &rows);
    assert_eq!(decoded_ids.unwrap(), identities);
    assert_eq!(&encoded[10 + 17 + 127 * 9..], &encode_values(&rows)[10..]);
}

#[test]
fn versions_one_and_two_remain_readable_and_sparse_pages_keep_version_two() {
    let rows = [row(1), row(2)];
    let identities = [id(1, 1), id(2, 1)];
    let (decoded, decoded_ids) = decode_identified_values(&encode_values(&rows)).unwrap();
    assert_rows(&decoded, &rows);
    assert!(decoded_ids.is_none());
    let old = flat_page(&rows, &identities);
    let (decoded, decoded_ids) = decode_identified_values(&old).unwrap();
    assert_rows(&decoded, &rows);
    assert_eq!(decoded_ids.unwrap(), identities);
    assert_eq!(encode_identified_values(&rows, &identities).unwrap(), old);
    assert_eq!(
        encode_identified_values(&rows[..1], &identities[..1]).unwrap(),
        flat_page(&rows[..1], &identities[..1])
    );
}

#[test]
fn compact_page_lengths_and_identity_damage_fail_before_returning_rows() {
    let rows = [row(1), row(2)];
    let encoded = encode_identified_values(&rows, &[id(1, 1), id(1, 2)]).unwrap();
    assert_eq!(&encoded[..8], TABLE_VALUES_MAGIC);
    for length in 0..encoded.len() {
        assert!(decode_identified_values(&encoded[..length]).is_err());
    }
    let mut trailing = encoded.clone();
    trailing.push(0);
    assert!(decode_identified_values(&trailing).is_err());
    for (offset, value) in [(10, 0), (10, 65), (27, 1)] {
        let mut damaged = encoded.clone();
        damaged[offset] = value;
        assert!(decode_identified_values(&damaged).is_err());
    }
    let mut damaged = encoded;
    damaged[28..36].fill(0);
    assert!(decode_identified_values(&damaged).is_err());
    assert!(encode_identified_values(&[], &[]).is_err());
    assert!(encode_identified_values(&rows, &[id(1, 1)]).is_err());
}

#[test]
fn interleaved_version_three_stays_readable_after_identity_table_encoding() {
    let rows = [row(1), row(2), row(3)];
    let identities = [id(1, 2), id(2, 2), id(1, 2)];
    let mut dictionary = IdentityDictionary::new();
    for identity in identities {
        dictionary.insert(identity).unwrap();
    }
    let mut out = Encoder::new(COMPACT_VALUES_MAGIC);
    out.u16(rows.len() as u16);
    dictionary.write(&mut out);
    for (row, identity) in rows.iter().zip(identities) {
        dictionary.write_row(&mut out, identity).unwrap();
        out.u64(row.address);
        out.u64(row.size);
        out.row(row.type_name);
        out.row(row.file);
        out.u32(row.line);
        out.u64(row.received_frame);
    }
    let (decoded, decoded_ids) = decode_identified_values(&out.bytes).unwrap();
    assert_rows(&decoded, &rows);
    assert_eq!(decoded_ids.unwrap(), identities);
    for length in 0..out.bytes.len() {
        assert!(decode_identified_values(&out.bytes[..length]).is_err());
    }
}
