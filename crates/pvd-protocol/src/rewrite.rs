use crate::{Endian, Error, Event, Limits, Reader};
use std::borrow::Cow;

pub fn retain_group_events(
    bytes: &[u8],
    endian: Endian,
    mut keep: impl FnMut(&Event) -> bool,
) -> Result<Option<Vec<u8>>, Error> {
    map_group_events(bytes, endian, |event, raw| {
        Ok(keep(event).then_some(Cow::Borrowed(raw)))
    })
}

pub fn map_group_events<'a>(
    bytes: &'a [u8],
    endian: Endian,
    mut map: impl FnMut(&Event, &'a [u8]) -> Result<Option<Cow<'a, [u8]>>, Error>,
) -> Result<Option<Vec<u8>>, Error> {
    let limits = Limits::default();
    let mut reader = Reader::new(bytes, 0, endian, limits);
    let size = reader.u32()? as usize;
    let count = reader.u32()? as usize;
    reader.take(16)?;
    if size > limits.max_group_bytes
        || size.checked_add(24) != Some(bytes.len())
        || count > size
        || count > limits.max_events_per_group
    {
        return Err(reader.error("invalid event group size/count for rewriting"));
    }
    let mut output = bytes[..24].to_vec();
    let mut kept = 0u32;
    for _ in 0..count {
        let start = reader.pos;
        let event = reader.event()?;
        if let Some(mapped) = map(&event, &bytes[start..reader.pos])? {
            if mapped.as_ptr() != bytes[start..reader.pos].as_ptr()
                || mapped.len() != reader.pos - start
            {
                let mut replacement = Reader::new(&mapped, start as u64, endian, limits);
                replacement.event()?;
                if replacement.pos != mapped.len() {
                    return Err(reader.error("replacement must contain exactly one complete event"));
                }
            }
            if mapped.len() > limits.max_group_bytes.saturating_sub(output.len() - 24) {
                return Err(reader.error("rewritten event group exceeds configured byte limit"));
            }
            output.extend_from_slice(&mapped);
            kept += 1;
        }
    }
    if reader.pos != bytes.len() {
        return Err(reader.error("unconsumed bytes in rewritten event group"));
    }
    if kept == 0 {
        return Ok(None);
    }
    let length = (output.len() - 24) as u32;
    let (length, kept) = match endian {
        Endian::Little => (length.to_le_bytes(), kept.to_le_bytes()),
        Endian::Big => (length.to_be_bytes(), kept.to_be_bytes()),
    };
    output[..4].copy_from_slice(&length);
    output[4..8].copy_from_slice(&kept);
    Ok(Some(output))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_only_counts_and_retains_exact_payload_bytes() {
        for endian in [Endian::Little, Endian::Big] {
            let n = |v: u32| match endian {
                Endian::Little => v.to_le_bytes(),
                Endian::Big => v.to_be_bytes(),
            };
            let mut bytes = Vec::from(n(10));
            bytes.extend(n(2));
            bytes.extend([0; 16]);
            bytes.push(15);
            bytes.extend([37; 8]);
            bytes.push(10);
            assert_eq!(
                retain_group_events(&bytes, endian, |_| true)
                    .unwrap()
                    .unwrap(),
                bytes
            );
            assert!(
                retain_group_events(&bytes, endian, |_| false)
                    .unwrap()
                    .is_none()
            );
            let rewritten =
                retain_group_events(&bytes, endian, |e| !matches!(e, Event::EndSetPropertyValue))
                    .unwrap()
                    .unwrap();
            assert_eq!(&rewritten[..4], &n(9));
            assert_eq!(&rewritten[4..8], &n(1));
            assert_eq!(&rewritten[8..], &bytes[8..bytes.len() - 1]);
        }
    }

    #[test]
    fn mapping_preserves_headers_and_rejects_unframed_replacements() {
        for endian in [Endian::Little, Endian::Big] {
            let n = |v: u32| match endian {
                Endian::Little => v.to_le_bytes(),
                Endian::Big => v.to_be_bytes(),
            };
            let mut bytes = Vec::from(n(9));
            bytes.extend(n(1));
            bytes.extend([11; 16]);
            bytes.push(15);
            bytes.extend([37; 8]);
            let mut replacement = vec![15];
            replacement.extend([22; 8]);
            let mapped = map_group_events(&bytes, endian, |_, _| {
                Ok(Some(Cow::Owned(replacement.clone())))
            })
            .unwrap()
            .unwrap();
            assert_eq!(&mapped[..24], &bytes[..24]);
            assert_eq!(&mapped[24..], replacement);
            for invalid in [
                vec![],
                vec![15],
                vec![255],
                vec![15, 0, 0, 0, 0, 0, 0, 0, 0, 10],
            ] {
                assert!(
                    map_group_events(&bytes, endian, |_, _| Ok(Some(Cow::Borrowed(&invalid))))
                        .is_err()
                );
            }
            for size in 0..bytes.len() {
                assert!(
                    map_group_events(&bytes[..size], endian, |_, raw| Ok(Some(Cow::Borrowed(
                        raw
                    ))))
                    .is_err()
                );
            }
            let mut hostile = bytes.clone();
            hostile[..4].copy_from_slice(&n(u32::MAX));
            assert!(retain_group_events(&hostile, endian, |_| false).is_err());
            let mut unknown = Vec::from(n(1));
            unknown.extend(n(1));
            unknown.extend([0; 16]);
            unknown.push(255);
            assert!(retain_group_events(&unknown, endian, |_| false).is_err());
        }
    }
}
