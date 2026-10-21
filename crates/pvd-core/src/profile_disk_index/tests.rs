use super::*;

#[test]
fn record_roundtrip_preserves_extreme_ids_and_rejects_reserved_bytes() {
    let row = Row {
        span: ProfileSpan {
            zone: u64::MAX,
            event: u16::MAX,
            thread: u32::MAX,
            context: u64::MAX,
            start: 0,
            end: u64::MAX,
            received_frame: usize::MAX,
        },
        ordinal: u64::MAX,
    };
    assert_eq!(row.key(), (u32::MAX, 0, u64::MAX));
    let encoded = row.encode();
    assert_eq!(encoded.len(), RECORD_BYTES);
    let decoded = Row::decode(&encoded).unwrap();
    assert_eq!(decoded.key(), row.key());
    assert_eq!(decoded.span.zone, u64::MAX);
    assert_eq!(decoded.span.event, u16::MAX);
    assert_eq!(decoded.span.context, u64::MAX);
    assert_eq!(decoded.span.end, u64::MAX);
    assert_eq!(decoded.span.received_frame, usize::MAX);
    for at in [6, 7, 56, 63] {
        let mut invalid = encoded;
        invalid[at] = 1;
        assert!(Row::decode(&invalid).is_err());
    }
    for length in [0, 1, 63] {
        assert!(Row::decode(&encoded[..length]).is_err());
    }
}

#[test]
fn registered_file_write_failure_propagates_and_lease_cleans_only_its_files() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../output")
        .join(format!("index-write-error-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let source = root.join("source");
    std::fs::write(&source, b"original").unwrap();
    let work = root.join("new-work");
    let mut lease = Lease::create(&work, &source.canonicalize().unwrap(), 1024, None).unwrap();
    let (id, file) = lease.create_file().unwrap();
    drop(file);
    let mut read_only = File::open(lease.path(id)).unwrap();
    assert!(lease.write(&mut read_only, b"cannot write").is_err());
    drop(read_only);
    drop(lease);
    assert!(!work.exists());
    assert_eq!(std::fs::read(&source).unwrap(), b"original");
    std::fs::remove_file(source).unwrap();
    std::fs::remove_dir(root).unwrap();
}

#[test]
fn query_budget_charges_reserved_names_and_row_to_span_conversion() {
    let request = WindowRequest {
        ticks: TickRange { start: 0, end: 1 },
        thread: None,
        context: None,
        limits: Default::default(),
    };
    let row = Row {
        span: ProfileSpan {
            zone: 1,
            event: 1,
            thread: 1,
            context: 1,
            start: 0,
            end: 1,
            received_frame: 0,
        },
        ordinal: 0,
    };
    let mut selected = Selected::new(request, NAME_BYTES + size_of::<Row>() + 1);
    selected.push(row.clone()).unwrap();
    let mut name = String::with_capacity(1024);
    name.push('a');
    assert!(selected.zone_name(1, &name).is_err());
    assert!(selected.names[&(1, 1)].zone.is_none());
    selected.memory_limit += name.capacity();
    selected.zone_name(1, &name).unwrap();
    assert_eq!(
        selected.names_bytes,
        NAME_BYTES + selected.names[&(1, 1)].zone.as_ref().unwrap().capacity()
    );
    let mut tight = Selected::new(
        request,
        NAME_BYTES + size_of::<Row>() + size_of::<ProfileSpan>() - 1,
    );
    tight.push(row).unwrap();
    assert!(tight.finish().is_err());
}
