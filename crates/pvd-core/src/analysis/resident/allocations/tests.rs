use super::*;

#[test]
fn indexed_resident_rows_preserve_layout_wire_and_atomic_rank_updates() {
    if size_of::<usize>() == 8 {
        assert_eq!(size_of::<ResidentNode>(), 48);
    }
    let domain = Domain::new(8 << 20);
    let mut map = ResidentMap::empty(domain.clone());
    let mut oracle = BTreeMap::new();
    let mut generator = 0x851d_372d_27fa_e29bu64;
    for step in 0..1400 {
        generator ^= generator << 13;
        generator ^= generator >> 7;
        generator ^= generator << 17;
        let key = (generator % 3, (generator >> 8) % 211);
        let previous = map.clone();
        let previous_wire = postcard::to_stdvec(&previous).unwrap();
        if generator & 3 == 0 {
            map.try_remove(&key, &mut || false).unwrap();
            oracle.remove(&key);
        } else {
            let value = Allocation {
                address: key.1,
                size: step,
                type_name: "row".into(),
                file: "rank.cpp".into(),
                line: 4,
                received_frame: step as usize,
            };
            map.try_insert(
                key,
                ResidentValue::from_source(&value, &domain, &mut || false).unwrap(),
                &mut || false,
            )
            .unwrap();
            oracle.insert(key, Arc::new(value));
        }
        map.validate().unwrap();
        assert_eq!(postcard::to_stdvec(&previous).unwrap(), previous_wire);
        assert_eq!(
            postcard::to_stdvec(&map).unwrap(),
            postcard::to_stdvec(&oracle).unwrap()
        );
        for (index, (&key, value)) in oracle.iter().enumerate() {
            let (actual_key, actual) = map.get_index(index).unwrap();
            assert_eq!(actual_key, key);
            assert_eq!(actual.size, value.size);
        }
        assert!(map.get_index(oracle.len()).is_none());
    }
    let value = ResidentValue::from_source(
        &Allocation {
            address: 999,
            size: 1,
            type_name: "new".into(),
            file: "new.cpp".into(),
            line: 0,
            received_frame: 0,
        },
        &domain,
        &mut || false,
    )
    .unwrap();
    let wire = postcard::to_stdvec(&map).unwrap();
    let before = domain.statistics();
    let barrier = domain.reserve(before.limit - before.logical_used).unwrap();
    assert!(
        map.try_insert((4, 999), value, &mut || false)
            .err()
            .unwrap()
            .downcast_ref::<ResidentBudgetExceeded>()
            .is_some()
    );
    assert_eq!(postcard::to_stdvec(&map).unwrap(), wire);
    assert!(
        map.try_remove(oracle.first_key_value().unwrap().0, &mut || true)
            .is_err()
    );
    assert_eq!(postcard::to_stdvec(&map).unwrap(), wire);
    drop((barrier, map));
    assert_eq!(domain.statistics().logical_used, 0);
}
