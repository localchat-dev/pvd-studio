use super::*;
use std::collections::BTreeSet;

fn fill(domain: &Domain, count: usize) -> ScratchSet {
    let mut set = ScratchSet::new(domain).unwrap();
    for key in 1..=count {
        assert_eq!(set.insert(key, (), &mut || false).unwrap(), None);
    }
    set
}

fn contents(set: &ScratchSet) -> BTreeSet<usize> {
    let mut result: BTreeSet<_> = set
        .table
        .slots
        .iter()
        .copied()
        .filter(|&key| key != 0)
        .collect();
    if set.zero {
        result.insert(0);
    }
    assert_eq!(result.len(), set.len);
    result
}

#[test]
fn identities_duplicates_and_extreme_keys_match_a_set() {
    let domain = Domain::new(4 << 20);
    let mut set = ScratchSet::new(&domain).unwrap();
    let mut expected = BTreeSet::new();
    let mut random = 0x713ba379_u64;
    for key in [0, usize::MAX, 1, usize::MAX - 1, 0, 1] {
        let fresh = expected.insert(key);
        assert_eq!(set.insert(key, (), &mut || false).unwrap().is_none(), fresh);
    }
    for _ in 0..20_000 {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        let key = random as usize & 8191;
        let fresh = expected.insert(key);
        assert_eq!(set.insert(key, (), &mut || false).unwrap().is_none(), fresh);
    }
    assert_eq!(contents(&set), expected);
    assert!(set.table.slots.len().is_power_of_two());
    assert!(set.len <= maximum_load(set.table.slots.len()));
    assert_eq!(
        domain.statistics().logical_used,
        size_of::<ScratchSet>() + set.table.slots.capacity() * size_of::<usize>()
    );
    drop(set);
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn table_growth_charges_old_and_new_capacity_until_publication() {
    let domain = Domain::new(1 << 20);
    let mut set = fill(&domain, 12);
    let before = domain.statistics().logical_used;
    assert_eq!(set.table.slots.capacity(), 16);
    assert_eq!(set.insert(13, (), &mut || false).unwrap(), None);
    assert_eq!(set.table.slots.capacity(), 32);
    let after = size_of::<ScratchSet>() + 32 * size_of::<usize>();
    assert_eq!(domain.statistics().logical_used, after);
    assert_eq!(domain.statistics().logical_peak, before + after);
    drop(set);
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn insufficient_growth_budget_preserves_contents_capacity_and_ticket() {
    let before = size_of::<ScratchSet>() + 16 * size_of::<usize>();
    let next = size_of::<ScratchSet>() + 32 * size_of::<usize>();
    let domain = Domain::new(before + next - 1);
    let mut set = fill(&domain, 12);
    let expected = contents(&set);
    let failure = set.insert(13, (), &mut || false).unwrap_err();
    assert!(failure.downcast_ref::<ResidentBudgetExceeded>().is_some());
    assert_eq!(contents(&set), expected);
    assert_eq!(set.table.slots.capacity(), 16);
    assert_eq!(domain.statistics().logical_used, before);
    assert_eq!(set.insert(8, (), &mut || false).unwrap(), Some(()));
    drop(set);
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn every_growth_cancellation_boundary_preserves_the_existing_set() {
    let domain = Domain::new(1 << 20);
    let count = maximum_load(256);
    let original = fill(&domain, count);
    let hash = original.hash.clone();
    let before = domain.statistics().logical_used;
    let expected = contents(&original);
    drop(original);
    let mut successful = ScratchSet::new(&domain).unwrap();
    successful.hash = hash.clone();
    for key in 1..=count {
        successful.insert(key, (), &mut || false).unwrap();
    }
    let mut calls = 0;
    successful
        .insert(count + 1, (), &mut || {
            calls += 1;
            false
        })
        .unwrap();
    drop(successful);
    assert!(calls > count);
    for fail_at in 1..=calls {
        let mut set = ScratchSet::new(&domain).unwrap();
        set.hash = hash.clone();
        for key in 1..=count {
            set.insert(key, (), &mut || false).unwrap();
        }
        let mut seen = 0;
        let result = set.insert(count + 1, (), &mut || {
            seen += 1;
            seen == fail_at
        });
        assert!(result.is_err(), "cancellation {fail_at}/{calls}");
        assert_eq!(contents(&set), expected);
        assert_eq!(set.table.slots.capacity(), 256);
        assert_eq!(domain.statistics().logical_used, before);
        drop(set);
        assert_eq!(domain.statistics().logical_used, 0);
    }
}

#[test]
fn cancellation_does_not_insert_zero_or_a_non_growing_key() {
    let domain = Domain::new(1 << 20);
    let mut set = fill(&domain, 3);
    let expected = contents(&set);
    let before = domain.statistics().logical_used;
    for key in [0, 1, 4] {
        assert!(set.insert(key, (), &mut || true).is_err());
        assert_eq!(contents(&set), expected);
        assert_eq!(domain.statistics().logical_used, before);
    }
    assert_eq!(set.insert(0, (), &mut || false).unwrap(), None);
    assert_eq!(set.insert(0, (), &mut || false).unwrap(), Some(()));
    drop(set);
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn probing_handles_collisions_and_has_bounded_cancellation_points() {
    let hash = RandomState::new();
    let mut slots = vec![usize::MAX; 1024];
    let key = 1;
    let start = hash.hash_one(key) as usize & 1023;
    slots[(start + 600) & 1023] = 0;
    let mut calls = 0;
    let result = locate(&slots, &hash, key, &mut || {
        calls += 1;
        false
    })
    .unwrap();
    assert_eq!(result, ((start + 600) & 1023, false));
    assert_eq!(calls, 3);
    let mut calls = 0;
    assert!(
        locate(&slots, &hash, key, &mut || {
            calls += 1;
            calls == 2
        })
        .is_err()
    );
    assert_eq!(calls, 2);
    slots.fill(usize::MAX);
    assert!(locate(&slots, &hash, key, &mut || false).is_err());
    assert!(locate(&[], &hash, key, &mut || false).is_err());
    assert!(locate(&[0; 3], &hash, key, &mut || false).is_err());
}

#[test]
fn unique_entry_and_checked_size_limits_do_not_allocate() {
    assert_eq!(next_len(MAX_ENTRIES - 1).unwrap(), MAX_ENTRIES);
    assert!(next_len(MAX_ENTRIES).is_err());
    assert!(next_len(usize::MAX).is_err());
    let domain = Domain::new(1 << 20);
    assert!(Table::new(&domain.0, usize::MAX, &mut || false).is_err());
    assert_eq!(domain.statistics().logical_used, 0);
    assert!(ScratchSet::new(&Domain::new(size_of::<ScratchSet>() - 1)).is_err());
}

#[test]
fn load_and_hint_boundaries_preserve_an_empty_fraction_of_the_table() {
    assert_eq!(maximum_load(0), 0);
    for slots in [16, 32, 64, 128, 256, 1024, MAX_SLOTS] {
        let threshold = maximum_load(slots);
        assert!(threshold * 5 <= slots * 4);
        assert!((threshold + 1) * 5 > slots * 4);
        if threshold <= MAX_ENTRIES {
            assert_eq!(hinted_slots(threshold).unwrap(), slots);
        }
        if threshold < MAX_ENTRIES {
            assert_eq!(hinted_slots(threshold + 1).unwrap(), slots * 2);
        }
    }
    assert_eq!(hinted_slots(0).unwrap(), 0);
    assert_eq!(hinted_slots(1).unwrap(), MIN_SLOTS);
    assert_eq!(hinted_slots(usize::MAX).unwrap(), MAX_SLOTS);
    assert!(maximum_load(MAX_SLOTS) >= MAX_ENTRIES);
}

#[test]
fn successful_hint_pays_actual_capacity_without_rehash_overlap() {
    let domain = Domain::new(1 << 20);
    let mut set = ScratchSet::with_hint(&domain, 102, &mut || false).unwrap();
    assert_eq!(set.table.slots.capacity(), 128);
    let reserved = size_of::<ScratchSet>() + 128 * size_of::<usize>();
    for key in 1..=102 {
        assert_eq!(set.insert(key, (), &mut || false).unwrap(), None);
    }
    assert_eq!(domain.statistics().logical_used, reserved);
    assert_eq!(domain.statistics().logical_peak, reserved);
    assert_eq!(set.insert(103, (), &mut || false).unwrap(), None);
    assert_eq!(set.table.slots.capacity(), 256);
    assert_eq!(contents(&set), (1..=103).collect());
    drop(set);
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn oversized_hint_falls_back_to_dynamic_growth_for_repeated_identities() {
    let domain = Domain::new(4096);
    let mut set = ScratchSet::with_hint(&domain, MAX_ENTRIES, &mut || false).unwrap();
    assert!(set.table.slots.is_empty());
    for index in 0..10_000 {
        let key = index % 20;
        assert_eq!(
            set.insert(key, (), &mut || false).unwrap().is_none(),
            index < 20
        );
    }
    assert_eq!(contents(&set), (0..20).collect());
    assert_eq!(set.table.slots.capacity(), 32);
    assert_eq!(
        domain.statistics().logical_used,
        size_of::<ScratchSet>() + 32 * size_of::<usize>()
    );
    drop(set);
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn hint_cancellation_and_error_classification_do_not_turn_into_retries() {
    let domain = Domain::new(1 << 20);
    let mut calls = 0;
    let error = match ScratchSet::with_hint(&domain, 1000, &mut || {
        calls += 1;
        calls == 3
    }) {
        Ok(_) => panic!("cancelled hint must fail"),
        Err(error) => error,
    };
    assert!(!allocation_failure(&error));
    assert_eq!(calls, 3);
    assert_eq!(domain.statistics().logical_used, 0);
    assert!(!allocation_failure(&super::error("invalid layout")));
    let failure: Error = Box::new(Vec::<u8>::new().try_reserve_exact(usize::MAX).unwrap_err());
    assert!(allocation_failure(&failure));
    let budget = match ScratchSet::new(&Domain::new(0)) {
        Ok(_) => panic!("zero budget must fail"),
        Err(error) => error,
    };
    assert!(allocation_failure(&budget));
}
