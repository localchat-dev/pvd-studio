use super::*;

#[test]
fn live_cache_keeps_existing_owners_and_refuses_a_full_live_directory() {
    let domain = Domain::new(1 << 20);
    let values = [value(1, &domain), value(2, &domain), value(3, &domain)];
    let mut cache = Cache::<u64, AllocationData, u64>::live(2, &domain).unwrap();
    cache.put(1, &values[0].0, 11);
    cache.put(2, &values[1].0, 22);
    let before = domain.statistics().logical_used;
    cache.put(3, &values[2].0, 33);
    cache.put(1, &values[2].0, 99);
    assert!(cache.get(1).unwrap().0.same(&values[0].0));
    assert_eq!(cache.get(1).unwrap().1, 11);
    assert!(cache.get(2).unwrap().0.same(&values[1].0));
    assert!(cache.get(3).is_none());
    assert_eq!(cache.directory.len(), 2);
    assert_eq!(cache.slots.len(), 2);
    let mut staged = Cache::live(2, &domain).unwrap();
    staged.put(1, &values[2].0, 99);
    cache.absorb(staged).unwrap();
    assert!(cache.get(1).unwrap().0.same(&values[0].0));
    assert_eq!(cache.get(1).unwrap().1, 11);
    assert_eq!(domain.statistics().logical_used, before);
    drop((cache, values));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn live_cache_reuses_dead_entries_before_empty_slots() {
    let domain = Domain::new(1 << 20);
    let first = value(1, &domain);
    let kept = value(2, &domain);
    let added = value(3, &domain);
    let mut cache = Cache::<u64, AllocationData, u64>::live(3, &domain).unwrap();
    cache.put(1, &first.0, 1);
    cache.put(2, &kept.0, 2);
    let dead_slot = cache.directory[&1];
    drop(first);
    cache.put(3, &added.0, 3);
    assert_eq!(cache.directory[&3], dead_slot);
    assert!(cache.get(1).is_none());
    assert!(cache.get(2).unwrap().0.same(&kept.0));
    assert!(cache.get(3).unwrap().0.same(&added.0));
    assert_eq!(
        cache.slots.iter().filter(|entry| entry.is_none()).count(),
        1
    );
    drop((cache, kept, added));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn structural_reclaim_keeps_live_aliases_in_a_full_domain_and_releases_dead_weak() {
    let domain = Domain::new(1 << 20);
    let kept = value(1, &domain);
    let dead = value(2, &domain);
    let mut cache = Cache::<u64, AllocationData, u64>::live(3, &domain).unwrap();
    cache.put(1, &kept.0, 1);
    cache.put(2, &dead.0, 2);
    drop(dead);
    let before = domain.statistics();
    assert!(cache.reclaim_dead(&mut || true).is_err());
    assert_eq!(domain.statistics().logical_used, before.logical_used);
    let filled = domain.reserve(before.limit - before.logical_used).unwrap();
    cache.reclaim_dead(&mut || false).unwrap();
    assert!(cache.get(1).unwrap().0.same(&kept.0));
    assert!(!cache.directory.contains_key(&2));
    assert!(domain.statistics().logical_weak_fixed_bytes < before.logical_weak_fixed_bytes);
    assert!(cache.capacity_bytes() > 0);
    drop(kept);
    cache.reclaim_dead(&mut || false).unwrap();
    assert_eq!(cache.capacity_bytes(), 0);
    assert_eq!(domain.statistics().logical_weak_fixed_bytes, 0);
    drop((filled, cache));
    assert_eq!(domain.statistics().logical_used, 0);
}

fn value(key: u64, domain: &Domain) -> ResidentValue {
    ResidentValue::from_source(
        &Allocation {
            address: key,
            size: key + 1,
            type_name: "Cache".into(),
            file: "cache.cpp".into(),
            line: 1,
            received_frame: 0,
        },
        domain,
        &mut || false,
    )
    .unwrap()
}

#[test]
fn every_key_fits_until_slot_capacity_then_fifo_eviction_keeps_the_directory_valid() {
    let domain = Domain::new(1 << 20);
    let values = [value(1, &domain), value(2, &domain), value(3, &domain)];
    let mut cache = Cache::<u64, AllocationData, u64>::new(2, &domain).unwrap();
    assert!(cache.capacity_bytes() > cache.slot_capacity_bytes());
    cache.put(1001, &values[0].0, 1);
    cache.put(9001, &values[1].0, 2);
    assert!(cache.get(1001).unwrap().0.same(&values[0].0));
    assert!(cache.get(9001).unwrap().0.same(&values[1].0));
    cache.put(1001, &values[0].0, 4);
    assert_eq!(cache.get(1001).unwrap().1, 4);
    cache.put(17001, &values[2].0, 3);
    assert!(cache.get(1001).is_none());
    assert!(cache.get(9001).is_some());
    assert!(cache.get(17001).unwrap().0.same(&values[2].0));
    assert_eq!(cache.directory.len(), 2);
    drop(values);
    assert_eq!(domain.statistics().live_strong_cells, 0);
    assert!(cache.get(9001).is_none());
    assert!(cache.get(17001).is_none());
    assert!(domain.statistics().logical_weak_fixed_bytes > 0);
    cache.clear();
    assert!(cache.directory.is_empty());
    assert_eq!(domain.statistics().logical_weak_fixed_bytes, 0);
    assert_eq!(domain.statistics().logical_used, cache.capacity_bytes());
    drop(cache);
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn weak_reservation_failure_skips_only_the_new_cache_entry() {
    let domain = Domain::new(1 << 20);
    let values = [value(1, &domain), value(2, &domain)];
    let mut cache = Cache::<u64, AllocationData, u64>::new(3, &domain).unwrap();
    cache.put(1, &values[0].0, 1);
    let before = domain.statistics();
    let hold = domain.reserve(before.limit - before.logical_used).unwrap();
    cache.put(2, &values[1].0, 2);
    assert!(cache.get(1).is_some());
    assert!(cache.get(2).is_none());
    assert_eq!(cache.directory.len(), 1);
    drop(hold);
    cache.put(2, &values[1].0, 2);
    assert!(cache.get(2).is_some());
    drop((cache, values));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn dynamic_cache_charges_only_created_slots_and_releases_them_on_clear() {
    let domain = Domain::new(1 << 20);
    let values = (0..40).map(|i| value(i, &domain)).collect::<Vec<_>>();
    let baseline = domain.statistics().logical_used;
    let mut cache = Cache::<u64, AllocationData, u64>::dynamic(40, &domain).unwrap();
    assert_eq!(cache.capacity_bytes(), 0);
    for (i, value) in values.iter().enumerate() {
        cache.put(i as u64, &value.0, i as u64);
        assert!(cache.get(i as u64).unwrap().0.same(&value.0));
        assert!(cache.slots.capacity() <= 40);
        assert_eq!(cache.slots.len(), cache.slots.capacity());
    }
    assert_eq!(cache.directory.len(), 40);
    assert!(cache.capacity_bytes() > 0);
    assert!(domain.statistics().logical_peak > domain.statistics().logical_used);
    cache.clear();
    assert_eq!(cache.slots.capacity(), 0);
    assert_eq!(cache.capacity_bytes(), 0);
    assert_eq!(domain.statistics().logical_used, baseline);
    drop((cache, values));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn dynamic_cache_growth_failure_preserves_bound_and_later_retries() {
    let domain = Domain::new(1 << 20);
    let values = (0..40).map(|i| value(i, &domain)).collect::<Vec<_>>();
    let mut cache = Cache::<u64, AllocationData, u64>::dynamic(40, &domain).unwrap();
    for i in 0..16 {
        cache.put(i, &values[i as usize].0, i);
    }
    let before = domain.statistics();
    let hold = domain.reserve(before.limit - before.logical_used).unwrap();
    cache.put(16, &values[16].0, 16);
    assert_eq!(cache.slots.len(), 16);
    assert_eq!(domain.statistics().logical_used, before.limit);
    assert!(cache.get(1).is_some());
    drop(hold);
    for i in 16..40 {
        cache.put(i, &values[i as usize].0, i);
    }
    assert_eq!(cache.slots.len(), 40);
    assert!(cache.get(39).is_some());
    drop((cache, values));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn stable_identity_preserves_aliases_without_merging_equal_independent_objects() {
    let source = Domain::new(1 << 20);
    let target = Domain::new(1 << 20);
    let a = Text::new("shared", &source, &mut || false).unwrap();
    let b = Text::new("shared", &source, &mut || false).unwrap();
    assert_ne!(a.0.semantic_identity(), b.0.semantic_identity());
    let wire = a.0.wire_identity().unwrap();
    let first = Text::with_wire_identity("shared", &target, &mut || false, Some(wire)).unwrap();
    let before = target.statistics().logical_used;
    let second = Text::with_wire_identity("shared", &target, &mut || false, Some(wire)).unwrap();
    assert!(!first.0.same(&second.0));
    assert_eq!(first.0.semantic_identity(), second.0.semantic_identity());
    assert_eq!(first.0.wire_identity().unwrap(), wire);
    assert_eq!(
        target.statistics().logical_used - before,
        Owned::<Box<str>>::fixed_bytes() + 6
    );
    let independent = Text::with_wire_identity(
        "shared",
        &target,
        &mut || false,
        Some(b.0.wire_identity().unwrap()),
    )
    .unwrap();
    assert_ne!(
        first.0.semantic_identity(),
        independent.0.semantic_identity()
    );
    let local = Text::new("shared", &target, &mut || false).unwrap();
    assert_ne!(local.0.wire_identity().unwrap().origin, wire.origin);
    assert_ne!(local.0.semantic_identity(), first.0.semantic_identity());
    drop((a, b, first, second, independent, local));
    assert_eq!(source.statistics().logical_used, 0);
    assert_eq!(target.statistics().logical_used, 0);
}

#[test]
fn imported_origins_are_bounded_and_last_strong_release_ignores_weak_storage() {
    let target = Domain::new(1 << 20);
    let sources = (0..64)
        .map(|_| {
            let domain = Domain::new(1024);
            Text::new("x", &domain, &mut || false).unwrap()
        })
        .collect::<Vec<_>>();
    let mut imported = sources[..63]
        .iter()
        .map(|text| {
            Text::with_wire_identity(
                "x",
                &target,
                &mut || false,
                Some(text.0.wire_identity().unwrap()),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let used = target.statistics().logical_used;
    assert!(
        Text::with_wire_identity(
            "x",
            &target,
            &mut || false,
            Some(sources[63].0.wire_identity().unwrap())
        )
        .is_err()
    );
    assert_eq!(target.statistics().logical_used, used);
    let old = imported.remove(0);
    let weak = old.0.weak().unwrap();
    drop(old);
    assert!(weak.upgrade().is_none());
    let final_import = Text::with_wire_identity(
        "x",
        &target,
        &mut || false,
        Some(sources[63].0.wire_identity().unwrap()),
    )
    .unwrap();
    assert_eq!(
        final_import.0.wire_identity().unwrap(),
        sources[63].0.wire_identity().unwrap()
    );
    drop((imported, final_import, sources));
    assert_eq!(target.statistics().live_strong_cells, 0);
    assert_eq!(
        target.statistics().logical_used,
        target.statistics().logical_weak_fixed_bytes
    );
    drop(weak);
    assert_eq!(target.statistics().logical_used, 0);
}

#[test]
fn unused_identity_permits_and_invalid_imports_leave_no_owned_payload() {
    let source = Domain::new(1024);
    let target = Domain::new(1 << 20);
    let text = Text::new("owned", &source, &mut || false).unwrap();
    let wire = text.0.wire_identity().unwrap();
    for _ in 0..100 {
        drop(target.0.identities.import(wire).unwrap());
    }
    for invalid in [
        identity::WireIdentity {
            origin: [0; 16],
            sequence: 1,
        },
        identity::WireIdentity {
            origin: wire.origin,
            sequence: 0,
        },
        identity::WireIdentity {
            origin: wire.origin,
            sequence: u64::MAX,
        },
    ] {
        assert!(Text::with_wire_identity("owned", &target, &mut || false, Some(invalid)).is_err());
        assert_eq!(target.statistics().logical_used, 0);
        assert_eq!(target.statistics().live_strong_cells, 0);
    }
    let local_future = identity::WireIdentity {
        origin: target.0.identities.wire(1).unwrap().origin,
        sequence: 1000,
    };
    assert!(target.0.identities.import(local_future).is_err());
    drop(text);
    assert_eq!(source.statistics().logical_used, 0);
}

#[test]
fn semantic_identity_uses_the_existing_cell_word_and_inline_cow_still_owns_new_nodes() {
    #[expect(dead_code, reason = "The layout witness is never instantiated.")]
    struct PriorCell<T> {
        value: Option<T>,
        memory: MemoryLease,
        payload_bytes: usize,
    }
    assert_eq!(
        size_of::<Cell<AllocationData>>(),
        size_of::<PriorCell<AllocationData>>()
    );
    assert_eq!(
        size_of::<Cell<Box<str>>>(),
        size_of::<PriorCell<Box<str>>>()
    );
    let domain = Domain::new(1 << 20);
    let mut node = Owned::new(
        ResidentNode {
            key: (1, 2),
            value: value(2, &domain),
            left: None,
            right: None,
            height: 1,
            subtree_len: 1,
        },
        &domain,
        0,
    )
    .unwrap();
    let old = node.clone();
    let original = node.wire_identity().unwrap();
    node.make_mut().unwrap().key = (2, 3);
    assert_ne!(node.wire_identity().unwrap(), original);
    assert_eq!(old.wire_identity().unwrap(), original);
    drop((node, old));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn dynamic_absorption_grows_with_existing_weak_tickets_and_respects_foreign_domain() {
    let domain = Domain::new(1 << 20);
    let values = (0..40).map(|n| value(n, &domain)).collect::<Vec<_>>();
    let mut left = Cache::<u64, AllocationData, u64>::dynamic(40, &domain).unwrap();
    let mut right = Cache::<u64, AllocationData, u64>::dynamic(40, &domain).unwrap();
    for (i, value) in values.iter().enumerate() {
        right.put(i as u64, &value.0, i as u64);
    }
    let weak = domain.statistics().logical_weak_fixed_bytes;
    left.absorb(right).unwrap();
    assert_eq!(domain.statistics().logical_weak_fixed_bytes, weak);
    assert_eq!(left.directory.len(), 40);
    assert!(left.get(39).is_some());
    let foreign = Domain::new(10000);
    assert!(left.absorb(Cache::dynamic(1, &foreign).unwrap()).is_err());
    assert_eq!(left.directory.len(), 40);
    drop((values, left));
    assert_eq!(domain.statistics().logical_used, 0);
}
