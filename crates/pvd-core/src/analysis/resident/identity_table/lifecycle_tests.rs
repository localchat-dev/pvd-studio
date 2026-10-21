use super::*;

fn batch(domain: &Domain, label: &str, count: usize) -> Vec<Text> {
    (0..count)
        .map(|index| Text::new(&format!("{label}-{index}"), domain, &mut || false).unwrap())
        .collect()
}

fn key(sequence: u64) -> identity::WireIdentity {
    identity::WireIdentity {
        origin: [42; 16],
        sequence,
    }
}

#[test]
fn released_unqueried_owners_do_not_permanently_block_new_canonical_ids() {
    let domain = Domain::new(1 << 20);
    let mut cache = Canonical::new(&domain, 16).unwrap();
    let old = batch(&domain, "old", 12);
    for text in &old {
        cache.put(&text.0, &mut || false).unwrap();
        assert!(
            cache
                .get(text.0.wire_identity().unwrap())
                .unwrap()
                .same(&text.0)
        );
    }
    let capacity = cache.capacity_bytes();
    drop(old);
    assert_eq!(domain.statistics().live_strong_cells, 0);
    let dead_weak_bytes = domain.statistics().logical_weak_fixed_bytes;
    assert!(dead_weak_bytes > 0);
    let new = batch(&domain, "new", 12);
    for text in &new {
        cache.put(&text.0, &mut || false).unwrap();
    }
    let admitted = new
        .iter()
        .filter(|text| {
            cache
                .get(text.0.wire_identity().unwrap())
                .is_some_and(|value| value.same(&text.0))
        })
        .count();
    let weak_bytes = domain.statistics().logical_weak_fixed_bytes;
    eprintln!(
        "canonical old=12 released=12 new=12 admitted={admitted} capacity={capacity} dead_weak_bytes={dead_weak_bytes} final_weak_bytes={weak_bytes}"
    );
    assert_eq!(cache.capacity_bytes(), capacity);
    drop(new);
    cache.clear();
    assert_eq!(domain.statistics().logical_used, 0);
    assert_eq!(
        admitted, 12,
        "unqueried dead IDs must not permanently exclude a new live batch"
    );
}

#[test]
fn repeated_owner_generations_keep_the_directory_and_weak_tickets_bounded() {
    let domain = Domain::new(1 << 20);
    let mut cache = Canonical::new(&domain, 16).unwrap();
    let mut maximum_weak = 0;
    for round in 0..64 {
        let values = batch(&domain, &format!("round-{round}"), 12);
        for text in &values {
            cache.put(&text.0, &mut || false).unwrap();
        }
        for text in &values {
            assert!(
                cache
                    .get(text.0.wire_identity().unwrap())
                    .unwrap()
                    .same(&text.0)
            );
        }
        assert_eq!(cache.table.count, 12);
        assert_eq!(
            cache.capacity_bytes(),
            16 * size_of::<Slot<OwnedWeak<Box<str>>>>()
        );
        maximum_weak = maximum_weak.max(domain.statistics().logical_weak_fixed_bytes);
        drop(values);
        assert_eq!(domain.statistics().live_strong_cells, 0);
    }
    assert_eq!(maximum_weak, 12 * Owned::<Box<str>>::fixed_bytes());
    cache.clear();
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn sweep_preserves_colliding_live_probe_chains_and_canonical_aliases() {
    let domain = Domain::new(1 << 20);
    let mut cache = Canonical::new(&domain, 16).unwrap();
    let keys = (1..)
        .map(key)
        .filter(|&key| cache.table.start(key, 16) == 0)
        .take(18)
        .collect::<Vec<_>>();
    let mut owners = keys[..12]
        .iter()
        .map(|&key| {
            Some(Text::with_wire_identity("same", &domain, &mut || false, Some(key)).unwrap())
        })
        .collect::<Vec<_>>();
    for text in owners.iter().flatten() {
        cache.put(&text.0, &mut || false).unwrap();
    }
    for index in (0..12).step_by(2) {
        owners[index] = None;
    }
    assert!(cache.get(key(1_000_000)).is_none());
    assert_eq!(cache.table.count, 6);
    for text in owners.iter().flatten() {
        let sid = text.0.wire_identity().unwrap();
        assert!(cache.get(sid).unwrap().same(&text.0));
        let alias = Text::with_wire_identity("same", &domain, &mut || false, Some(sid)).unwrap();
        cache.put(&alias.0, &mut || false).unwrap();
        assert!(cache.get(sid).unwrap().same(&text.0));
    }
    let next = keys[12..]
        .iter()
        .map(|&key| Text::with_wire_identity("same", &domain, &mut || false, Some(key)).unwrap())
        .collect::<Vec<_>>();
    for text in &next {
        cache.put(&text.0, &mut || false).unwrap();
    }
    for text in owners.iter().flatten().chain(next.iter()) {
        assert!(
            cache
                .get(text.0.wire_identity().unwrap())
                .unwrap()
                .same(&text.0)
        );
    }
    drop((owners, next));
    cache.clear();
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn sparse_dead_entries_are_reclaimed_without_queries_for_their_keys() {
    let domain = Domain::new(1 << 20);
    let mut cache = Canonical::new(&domain, 128).unwrap();
    let owners = batch(&domain, "sparse", 60);
    for text in &owners {
        cache.put(&text.0, &mut || false).unwrap();
    }
    assert_eq!(cache.table.slots.len(), 128);
    drop(owners);
    for _ in 0..128 {
        assert!(cache.get(key(1_000_000)).is_none());
    }
    assert_eq!(cache.table.count, 0);
    assert_eq!(domain.statistics().logical_weak_fixed_bytes, 0);
    assert_eq!(domain.statistics().logical_used, cache.capacity_bytes());
    cache.clear();
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn live_owners_survive_full_admission_and_cancelled_put() {
    let domain = Domain::new(1 << 20);
    let mut cache = Canonical::new(&domain, 16).unwrap();
    let owners = batch(&domain, "live", 12);
    for text in &owners {
        cache.put(&text.0, &mut || false).unwrap();
    }
    let before = domain.statistics().logical_used;
    let error = cache.put(&owners[0].0, &mut || true).unwrap_err();
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
    assert_eq!(domain.statistics().logical_used, before);
    let newcomer = Text::new("new", &domain, &mut || false).unwrap();
    cache.put(&newcomer.0, &mut || false).unwrap();
    assert!(cache.get(newcomer.0.wire_identity().unwrap()).is_none());
    for text in &owners {
        assert!(
            cache
                .get(text.0.wire_identity().unwrap())
                .unwrap()
                .same(&text.0)
        );
    }
    drop((owners, newcomer));
    cache.clear();
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn tombstones_remain_usable_when_same_capacity_rehash_cannot_reserve_a_second_table() {
    let capacity_bytes = 16 * size_of::<Slot<u64>>();
    let domain = Domain::new(capacity_bytes);
    let mut table = IdentityTable::resident(&domain, 16).unwrap();
    let keys = (0..16)
        .map(|bucket| {
            (1..)
                .map(key)
                .find(|&key| table.start(key, 16) == bucket)
                .unwrap()
        })
        .collect::<Vec<_>>();
    for (index, &sid) in keys[..15].iter().enumerate() {
        assert!(table.insert(sid, 1u64, &mut || false).unwrap());
        if index >= 2 {
            table.remove(sid);
        }
    }
    assert_eq!(table.count, 2);
    assert_eq!(table.occupied, 15);
    let mut checks = 0;
    let error = table
        .insert(keys[15], 2, &mut || {
            checks += 1;
            checks == 2
        })
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
    assert_eq!(table.count, 2);
    assert_eq!(table.occupied, 15);
    assert_eq!(domain.statistics().logical_used, capacity_bytes);
    assert!(table.insert(keys[15], 2, &mut || false).unwrap());
    assert_eq!(table.get(keys[15]), Some(&2));
    table.remove(keys[15]);
    assert_eq!(table.occupied, 16);
    assert!(table.insert(key(1_000_000), 3, &mut || false).unwrap());
    assert_eq!(table.get(key(1_000_000)), Some(&3));
    for &sid in &keys[..2] {
        assert_eq!(table.get(sid), Some(&1));
    }
    assert_eq!(domain.statistics().logical_used, capacity_bytes);
    assert_eq!(domain.statistics().logical_peak, capacity_bytes);
    drop(table);
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn explicit_reclaim_keeps_live_identity_owners_and_releases_dead_weak_tickets() {
    let domain = Domain::new(1 << 20);
    let mut cache = Canonical::new(&domain, 16).unwrap();
    let old = batch(&domain, "old", 12);
    for text in &old {
        cache.put(&text.0, &mut || false).unwrap();
    }
    let held = old[3].clone();
    let wire = held.0.wire_identity().unwrap();
    drop(old);
    let before = domain.statistics();
    assert!(cache.reclaim(&mut || true).is_err());
    assert_eq!(domain.statistics().logical_used, before.logical_used);
    cache.reclaim(&mut || false).unwrap();
    assert_eq!(cache.table.count, 1);
    assert_eq!(
        domain.statistics().logical_weak_fixed_bytes,
        Owned::<Box<str>>::fixed_bytes()
    );
    assert!(cache.get(wire).unwrap().same(&held.0));
    drop(held);
    cache.reclaim(&mut || false).unwrap();
    assert_eq!(cache.capacity_bytes(), 0);
    assert_eq!(domain.statistics().logical_used, 0);
}
