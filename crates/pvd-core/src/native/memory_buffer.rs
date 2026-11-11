use super::*;

pub(super) fn normalize(
    data: &[u8],
    strings: &BTreeMap<u32, String>,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<Vec<u8>, Error> {
    let mut reader = Reader::new(data);
    let mut handles = BTreeSet::new();
    let mut length = data.len();
    let mut events = 0usize;
    while reader.pos < data.len() {
        if !proceed() {
            return Err(cancelled());
        }
        events += 1;
        if events > 2_000_000 {
            return Err(error("PVD3 memory buffer exceeds two million events"));
        }
        let header = reader.u16()?;
        let address_width = 1 << ((header >> 4) & 3);
        match header & 15 {
            2 => {
                let type_width = (1 << ((header >> 6) & 3)).min(4);
                let file_width = (1 << ((header >> 8) & 3)).min(4);
                let size_width = (1 << ((header >> 10) & 3)).min(4);
                let line_width = (1 << ((header >> 12) & 3)).min(4);
                reader.take(size_width)?;
                for width in [type_width, file_width] {
                    let mut word = [0; 4];
                    word[..width].copy_from_slice(reader.take(width)?);
                    let handle = u32::from_le_bytes(word);
                    if handles.contains(&handle) {
                        continue;
                    }
                    let text = strings.get(&handle).ok_or_else(|| {
                        error(format!("undefined PVD3 memory string handle {handle}"))
                    })?;
                    length = length
                        .checked_add(11)
                        .and_then(|length| length.checked_add(text.len()))
                        .ok_or_else(|| error("PVD3 normalized memory buffer size overflow"))?;
                    if length > MAX_METADATA || handles.len() >= MAX_ITEMS {
                        return Err(error("PVD3 normalized memory buffer exceeds 64 MiB"));
                    }
                    handles.insert(handle);
                }
                reader.take(line_width)?;
                reader.take(address_width)?;
            }
            3 => {
                reader.take(address_width)?;
            }
            1 => {
                return Err(error(
                    "PVD3 memory buffers must use container string handles",
                ));
            }
            kind => {
                return Err(error(format!("unsupported PVD3 memory event {kind}")));
            }
        }
    }
    if length > MAX_METADATA {
        return Err(error("PVD3 normalized memory buffer exceeds 64 MiB"));
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(length)?;
    for handle in handles {
        if !proceed() {
            return Err(cancelled());
        }
        let text = &strings[&handle];
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(u32::try_from(text.len() + 1)?.to_le_bytes());
        bytes.extend_from_slice(text.as_bytes());
        bytes.push(0);
        bytes.extend(handle.to_le_bytes());
    }
    for chunk in data.chunks(64 * 1024) {
        if !proceed() {
            return Err(cancelled());
        }
        bytes.extend_from_slice(chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::Analysis;

    fn allocation() -> Vec<u8> {
        let header = 2u16 | (3 << 4) | (2 << 6) | (2 << 8) | (2 << 10) | (2 << 12);
        let mut bytes = header.to_le_bytes().to_vec();
        for word in [64u32, 7, 9, 42] {
            bytes.extend(word.to_le_bytes());
        }
        bytes.extend(0x8000u64.to_le_bytes());
        bytes
    }

    fn strings() -> BTreeMap<u32, String> {
        BTreeMap::from([(7, "PxScene".into()), (9, "Source.cpp".into())])
    }

    #[test]
    fn normalized_memory_preserves_native_names_allocations_and_delivery_frame() {
        let mut bytes = allocation();
        bytes.extend(0x33u16.to_le_bytes());
        bytes.extend(0x8000u64.to_le_bytes());
        let strings = strings();
        let normalized = normalize(&bytes, &strings, &mut || true).unwrap();
        let mut native = Analysis::default();
        native
            .ingest_native_memory(11, &bytes, 7, |handle| {
                strings
                    .get(&handle)
                    .map(String::as_str)
                    .ok_or_else(|| error("missing"))
            })
            .unwrap();
        let mut sdk = Analysis::default();
        sdk.ingest(
            11,
            "_debugger_::PxProfileMemoryEventBuffer",
            &normalized,
            Endian::Little,
            7,
        )
        .unwrap();
        assert_eq!(native.memory.events(), sdk.memory.events());
        assert_eq!((native.memory.peak_bytes, sdk.memory.peak_bytes), (64, 64));
        assert_eq!(
            native.memory.state_at_frame(6),
            sdk.memory.state_at_frame(6)
        );
        assert_eq!(
            native.memory.state_at_frame(7),
            sdk.memory.state_at_frame(7)
        );
        assert_eq!(
            native.memory.churn_at_frame(7),
            sdk.memory.churn_at_frame(7)
        );
        assert_eq!(
            postcard::to_stdvec(&native.memory.samples).unwrap(),
            postcard::to_stdvec(&sdk.memory.samples).unwrap()
        );
    }

    #[test]
    fn native_buffer_rejects_stream_definitions_unknown_handles_and_truncated_fields() {
        let bytes = allocation();
        for length in 1..bytes.len() {
            assert!(normalize(&bytes[..length], &strings(), &mut || true).is_err());
        }
        assert!(normalize(&bytes, &BTreeMap::new(), &mut || true).is_err());
        assert!(normalize(&[1, 0], &strings(), &mut || true).is_err());
        assert!(normalize(&[15, 0], &strings(), &mut || true).is_err());
        let failure = normalize(&bytes, &strings(), &mut || false).unwrap_err();
        assert_eq!(
            failure.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
    }
}
