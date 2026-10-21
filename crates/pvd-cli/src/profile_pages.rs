use crate::{Result, cancellation, csv_cell};
use pvd_core::profile_curves::{
    CatalogCursor, CatalogPageReport, ExactKey, MAX_CATALOG_PAGE, SourceIdentity,
    catalog_page_streaming,
};
use std::{fs::File, io::Write};

pub fn run(path: &str, limit: usize, token: Option<&str>) -> Result<()> {
    crate::project_capture::require_full_source(path)?;
    if !(1..=MAX_CATALOG_PAGE).contains(&limit) {
        return Err("profile catalog page limit must be between 1 and 4096".into());
    }
    let cursor = token.map(decode_cursor).transpose()?;
    let cancel = cancellation::Cancellation::install()?;
    if crate::is_native_capture(path)? {
        return Err(
            "profile-events-page requires an SDK stream; use profile-events for a native capture"
                .into(),
        );
    }
    let mut file = File::open(path)?;
    let mut last = None;
    let page =
        catalog_page_streaming(&mut file, cursor.as_ref(), limit, cancel.flag(), |status| {
            if last.is_none_or(|time: std::time::Instant| time.elapsed().as_secs() >= 1) {
                eprintln!(
                    "indexing profile catalog page: {} / {} bytes",
                    status.bytes_read, status.total_bytes
                );
                last = Some(std::time::Instant::now());
            }
            cancel.check().is_ok()
        })?;
    cancel.check()?;
    write_page(&mut std::io::stdout().lock(), &page, || {
        cancel.check().map_err(Into::into)
    })
}

fn encode_cursor(cursor: &CatalogCursor) -> String {
    let identity = &cursor.identity;
    let key = cursor.after;
    format!(
        "{}:{}:{}:{}:{}:{}:{}",
        blake3::Hash::from(identity.blake3).to_hex(),
        identity.bytes,
        identity.frames,
        key.zone,
        key.event,
        key.thread,
        key.context
    )
}

fn decode_cursor(token: &str) -> Result<CatalogCursor> {
    if token.len() > 256 {
        return Err("profile catalog cursor is too long".into());
    }
    let fields: Vec<_> = token.split(':').collect();
    if fields.len() != 7 || fields[0].len() != 64 {
        return Err("profile catalog cursor must contain a BLAKE3 hash, source size, frame count and exact key".into());
    }
    let mut digest = [0; 32];
    if !fields[0].is_ascii() {
        return Err("profile catalog cursor hash must be ASCII hexadecimal".into());
    }
    for (index, value) in digest.iter_mut().enumerate() {
        *value = u8::from_str_radix(&fields[0][index * 2..index * 2 + 2], 16)?;
    }
    Ok(CatalogCursor {
        identity: SourceIdentity {
            blake3: digest,
            bytes: fields[1].parse()?,
            frames: fields[2].parse()?,
        },
        after: ExactKey {
            zone: fields[3].parse()?,
            event: fields[4].parse()?,
            thread: fields[5].parse()?,
            context: fields[6].parse()?,
        },
    })
}

fn write_page(
    output: &mut impl Write,
    page: &CatalogPageReport,
    mut check: impl FnMut() -> Result<()>,
) -> Result<()> {
    check()?;
    writeln!(
        output,
        "# source_blake3={}",
        blake3::Hash::from(page.identity.blake3).to_hex()
    )?;
    writeln!(output, "# source_bytes={}", page.identity.bytes)?;
    writeln!(output, "# source_frames={}", page.identity.frames)?;
    writeln!(output, "# order=zone,event,thread,context")?;
    writeln!(
        output,
        "# estimated_index_bytes={}",
        page.estimated_index_bytes
    )?;
    writeln!(
        output,
        "page_row,zone_id,event_id,thread_id,context_id,zone_name,event_name,first_start_tick,complete_spans,open_spans,unmatched_stops,metadata_ambiguous"
    )?;
    for (row, entry) in page.entries.iter().enumerate() {
        check()?;
        let key = entry.key;
        let zone = (!entry.metadata_ambiguous)
            .then(|| page.zones.get(&key.zone))
            .flatten();
        writeln!(
            output,
            "{row},0x{:x},{},{},0x{:x},{},{},{},{},{},{},{}",
            key.zone,
            key.event,
            key.thread,
            key.context,
            csv_cell(zone.map_or("", |zone| zone.name.as_str())),
            csv_cell(
                zone.and_then(|zone| zone.events.get(&key.event))
                    .map_or("", String::as_str)
            ),
            entry
                .first_start
                .map(|tick| tick.to_string())
                .unwrap_or_default(),
            entry.complete_spans,
            entry.open_spans,
            entry.unmatched_stops,
            entry.metadata_ambiguous
        )?;
    }
    check()?;
    writeln!(
        output,
        "# next_cursor={}",
        page.next.as_ref().map(encode_cursor).unwrap_or_default()
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_roundtrip_preserves_every_identity_field_and_rejects_invalid_tokens() {
        let cursor = CatalogCursor {
            identity: SourceIdentity {
                blake3: [255; 32],
                bytes: u64::MAX,
                frames: usize::MAX,
            },
            after: ExactKey {
                zone: u64::MAX,
                event: u16::MAX,
                thread: u32::MAX,
                context: u64::MAX,
            },
        };
        let encoded = encode_cursor(&cursor);
        assert_eq!(decode_cursor(&encoded).unwrap(), cursor);
        for token in [
            "",
            ":",
            "abc:1:2:3:4:5:6",
            &format!("{}:1:2:3:4:5:6", "z".repeat(64)),
            &format!("{}:1:2:3:65536:5:6", "a".repeat(64)),
            &"x".repeat(257),
        ] {
            assert!(decode_cursor(token).is_err());
        }
    }

    #[test]
    fn page_output_labels_local_rows_and_publishes_a_source_bound_cursor() {
        let cursor = CatalogCursor {
            identity: SourceIdentity {
                blake3: [1; 32],
                bytes: 123,
                frames: 7,
            },
            after: ExactKey {
                zone: 1,
                event: 2,
                thread: 3,
                context: 4,
            },
        };
        let page = CatalogPageReport {
            identity: cursor.identity.clone(),
            entries: vec![],
            zones: Default::default(),
            next: Some(cursor.clone()),
            estimated_index_bytes: 0,
        };
        let mut bytes = Vec::new();
        write_page(&mut bytes, &page, || Ok(())).unwrap();
        let output = String::from_utf8(bytes).unwrap();
        assert!(output.contains("page_row,zone_id"));
        assert!(!output.contains("series_index"));
        let token = output
            .lines()
            .find_map(|line| line.strip_prefix("# next_cursor="))
            .unwrap();
        assert_eq!(decode_cursor(token).unwrap(), cursor);
    }
    #[test]
    fn cancellation_before_page_output_publishes_neither_names_nor_cursor() {
        let page = CatalogPageReport {
            identity: SourceIdentity {
                blake3: [1; 32],
                bytes: 12,
                frames: 1,
            },
            entries: vec![],
            zones: Default::default(),
            next: None,
            estimated_index_bytes: 0,
        };
        let mut bytes = Vec::new();
        assert!(write_page(&mut bytes, &page, || Err("cancelled".into())).is_err());
        assert!(bytes.is_empty());
    }
}
