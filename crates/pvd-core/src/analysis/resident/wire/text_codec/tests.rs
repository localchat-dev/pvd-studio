use super::*;

fn encode(values: &[&str]) -> Vec<u8> {
    let table = TextDictionary::new(values.len(), |index| values[index], &mut || false).unwrap();
    let mut output = Encoder::new(DEDUP_STRINGS_MAGIC);
    table
        .write(&mut output, |index| values[index], &mut || false)
        .unwrap();
    assert_eq!(output.bytes.len(), 8 + table.encoded_bytes());
    output.bytes
}

fn decode(bytes: &[u8], count: usize) -> Result<Vec<&str>, Error> {
    let mut input = Decoder::new(bytes, DEDUP_STRINGS_MAGIC)?;
    let ranges = read_text_ranges(&mut input, count, &mut || false)?;
    input.finish()?;
    ranges
        .into_iter()
        .map(|range| Ok(std::str::from_utf8(&bytes[range])?))
        .collect()
}

#[test]
fn repeated_contents_share_ranges_but_keep_every_row() {
    let values = ["相同内容", "", "相同内容", "other", "", "other"];
    let bytes = encode(&values);
    assert_eq!(decode(&bytes, values.len()).unwrap(), values);
    let mut input = Decoder::new(&bytes, DEDUP_STRINGS_MAGIC).unwrap();
    let ranges = read_text_ranges(&mut input, values.len(), &mut || false).unwrap();
    assert_eq!(ranges[0], ranges[2]);
    assert_eq!(ranges[1], ranges[4]);
    assert_eq!(ranges[3], ranges[5]);
}

#[test]
fn full_page_and_sparse_content_tables_are_bounded() {
    let values = (0..MAX_STRING_ROWS)
        .map(|n| format!("{n:04}-unique"))
        .collect::<Vec<_>>();
    let refs = values.iter().map(String::as_str).collect::<Vec<_>>();
    let table = TextDictionary::new(refs.len(), |i| refs[i], &mut || false).unwrap();
    assert!(!table.is_smaller());
    assert_eq!(decode(&encode(&refs), refs.len()).unwrap(), refs);
    assert!(TextDictionary::new(0, |_| "", &mut || false).is_err());
    assert!(TextDictionary::new(MAX_STRING_ROWS + 1, |_| "", &mut || false).is_err());
    assert!(
        read_text_ranges(
            &mut Decoder::new(&encode(&["x"]), DEDUP_STRINGS_MAGIC).unwrap(),
            MAX_STRING_ROWS + 1,
            &mut || false
        )
        .is_err()
    );
}

#[test]
fn wire_damage_and_invalid_indices_are_rejected() {
    let bytes = encode(&["alpha", "alpha", "beta"]);
    for length in 0..bytes.len() {
        assert!(decode(&bytes[..length], 3).is_err());
    }
    let mut bad = bytes.clone();
    bad.extend_from_slice(&[0]);
    assert!(decode(&bad, 3).is_err());
    let mut bad = bytes.clone();
    bad[8..10].copy_from_slice(&0u16.to_le_bytes());
    assert!(decode(&bad, 3).is_err());
    bad[8..10].copy_from_slice(&4u16.to_le_bytes());
    assert!(decode(&bad, 3).is_err());
    let mut bad = bytes.clone();
    let last = bad.len() - 2;
    bad[last..].copy_from_slice(&2u16.to_le_bytes());
    assert!(decode(&bad, 3).is_err());
    let mut bad = bytes;
    bad[14] = 0xff;
    assert!(decode(&bad, 3).is_err());
}

#[test]
fn cancellation_cannot_return_a_partial_content_table() {
    let values = ["repeat"; 128];
    let mut steps = 0;
    assert!(
        TextDictionary::new(values.len(), |i| values[i], &mut || {
            steps += 1;
            steps > 40
        })
        .is_err()
    );
    let table = TextDictionary::new(values.len(), |i| values[i], &mut || false).unwrap();
    let mut output = Encoder::new(DEDUP_STRINGS_MAGIC);
    assert!(
        table
            .write(&mut output, |i| values[i], &mut || true)
            .is_err()
    );
    let bytes = encode(&values);
    let mut input = Decoder::new(&bytes, DEDUP_STRINGS_MAGIC).unwrap();
    let mut steps = 0;
    assert!(
        read_text_ranges(&mut input, values.len(), &mut || {
            steps += 1;
            steps > 40
        })
        .is_err()
    );
}
