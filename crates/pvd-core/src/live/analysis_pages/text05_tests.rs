use super::*;

#[test]
fn actual_resource_text_dictionary_preserves_distinct_stable_owners_and_retain_wire() {
    let history = HistoryWriter::new(config(16 << 20, 2)).unwrap();
    let resources = history.resources.as_ref().unwrap().clone();
    let mut io = HistoryPageIo::new(&resources, ResourcePlan::default());
    let domain = Domain::new(16 << 20);
    let mut source = ResidentAnalysis::new(domain.clone()).unwrap();
    let (_, kind, mut payload) = full_fixture_inputs(pvd_protocol::Endian::Little).remove(0);
    for address in 9..710u64 {
        payload.extend_from_slice(
            &(2u16 | (3 << 4) | (2 << 6) | (2 << 8) | (2 << 10) | (2 << 12)).to_le_bytes(),
        );
        for value in [12u32, 1, 2, 20] {
            payload.extend_from_slice(&value.to_le_bytes());
        }
        payload.extend_from_slice(&address.to_le_bytes());
    }
    source
        .ingest(
            1,
            kind,
            &payload,
            pvd_protocol::Endian::Little,
            0,
            &mut || false,
        )
        .unwrap();
    let seed = postcard::to_stdvec(&source.legacy_record()).unwrap();
    drop(source);
    assert_eq!(domain.statistics().logical_used, 0);
    let Record::Analysis(mut oracle) = postcard::from_bytes(&seed).unwrap() else {
        panic!("analysis fixture")
    };
    let mut source =
        ResidentAnalysis::from_legacy_record_bytes(&seed, domain.clone(), &mut || false).unwrap();
    source.try_retain_recent(0, &mut || false).unwrap();
    oracle.retain_recent(0);
    assert_eq!(
        postcard::to_stdvec(&source).unwrap(),
        postcard::to_stdvec(&oracle).unwrap()
    );
    let limits = FullLimits {
        writer_memo_bytes: 0,
        maps: crate::analysis::resident::ResidentLimits {
            value_slots: 1,
            string_slots: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut writer = FullAnalysisWriter::new(&io, domain.clone(), limits.clone()).unwrap();
    let prepared = writer.prepare(&source, &mut io, &mut || false).unwrap();
    io.commit().unwrap();
    let snapshot = writer.publish(prepared, &io).unwrap();
    let descriptor = snapshot.descriptor(&io);
    let mut pending = vec![descriptor.root];
    let mut visited = BTreeSet::new();
    let mut dictionaries = 0;
    while let Some(id) = pending.pop() {
        if !visited.insert(id) {
            continue;
        }
        assert!(visited.len() < 10_000);
        let loaded = io.read(id, 2 << 20, &mut || false).unwrap();
        pending.extend(loaded.dependencies);
        let bytes = loaded.bytes;
        if bytes.get(..8) == Some(b"PVDASP05") {
            let count = usize::from(u16::from_le_bytes(bytes[8..10].try_into().unwrap()));
            let offset = 11 + usize::from(bytes[10]) * 16;
            assert_eq!(
                bytes[offset..offset + count * 9]
                    .chunks_exact(9)
                    .collect::<BTreeSet<_>>()
                    .len(),
                count
            );
            let body = offset + count * 9;
            let unique = usize::from(u16::from_le_bytes(
                bytes[body..body + 2].try_into().unwrap(),
            ));
            assert!(unique < count);
            dictionaries += 1;
        }
    }
    assert!(dictionaries > 0);
    let mut reader = FullAnalysisReader::new(
        HistoryPageIo::new(&resources, ResourcePlan::default()),
        domain.clone(),
        limits,
    )
    .unwrap();
    let mut decoded = reader.read(&descriptor, &mut || false).unwrap();
    assert_eq!(
        postcard::to_stdvec(&decoded).unwrap(),
        postcard::to_stdvec(&source).unwrap()
    );
    decoded.try_retain_recent(0, &mut || false).unwrap();
    assert_eq!(
        postcard::to_stdvec(&decoded).unwrap(),
        postcard::to_stdvec(&oracle).unwrap()
    );
    drop((source, decoded, reader, writer, snapshot, history));
    io.rollback();
    resources.collect().unwrap();
    assert_eq!(resources.physical_bytes(), 0);
    assert_eq!(domain.statistics().logical_used, 0);
}
