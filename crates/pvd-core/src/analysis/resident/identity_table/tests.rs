use super::*;

fn key(sequence: u64) -> identity::WireIdentity {
    identity::WireIdentity {
        origin: [1; 16],
        sequence,
    }
}

#[test]
fn table_growth_cancel_and_budget_preserve_existing_entries() {
    let domain = Domain::new(1 << 20);
    let mut table = IdentityTable::<u64>::resident(&domain, 128).unwrap();
    for n in 1..=12 {
        assert!(table.insert(key(n), n, &mut || false).unwrap());
    }
    let before = domain.statistics().logical_used;
    let mut checks = 0;
    assert!(
        table
            .insert(key(13), 13, &mut || {
                checks += 1;
                checks == 3
            })
            .is_err()
    );
    assert_eq!(table.count, 12);
    assert_eq!(domain.statistics().logical_used, before);
    let block = domain
        .reserve(domain.statistics().limit - domain.statistics().logical_used - 1_000)
        .unwrap();
    assert!(!table.insert(key(13), 13, &mut || false).unwrap());
    for n in 1..=12 {
        assert_eq!(table.get(key(n)), Some(&n));
    }
    assert_eq!(table.get(key(13)), None);
    drop(block);
    table.clear();
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn full_origin_and_sequence_survive_collisions_tombstones_and_rehash() {
    let domain = Domain::new(1 << 20);
    let mut table = IdentityTable::<u64>::resident(&domain, 1 << 10).unwrap();
    for n in 1..=400 {
        assert!(table.insert(key(n), n, &mut || false).unwrap());
    }
    for n in (1..=400).step_by(2) {
        table.remove(key(n));
    }
    for n in 401..=700 {
        assert!(table.insert(key(n), n, &mut || false).unwrap());
    }
    let other = identity::WireIdentity {
        origin: [2; 16],
        sequence: 2,
    };
    assert!(table.insert(other, 900, &mut || false).unwrap());
    assert_eq!(table.get(key(2)), Some(&2));
    assert_eq!(table.get(other), Some(&900));
    for n in (2..=400).step_by(2) {
        assert_eq!(table.get(key(n)), Some(&n));
    }
    for n in 401..=700 {
        assert_eq!(table.get(key(n)), Some(&n));
    }
    drop(table);
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn canonical_owner_is_shared_without_holding_payload_and_conflicts_are_rejected() {
    let domain = Domain::new(1 << 20);
    let text = Text::new("same", &domain, &mut || false).unwrap();
    let wire = text.0.wire_identity().unwrap();
    let mut cache = Canonical::new(&domain, 128).unwrap();
    cache.put(&text.0, &mut || false).unwrap();
    let bytes = domain.statistics().logical_weak_fixed_bytes;
    cache.put(&text.0, &mut || false).unwrap();
    assert_eq!(domain.statistics().logical_weak_fixed_bytes, bytes);
    assert!(cache.get(wire).unwrap().same(&text.0));
    let duplicate = Text::with_wire_identity("same", &domain, &mut || false, Some(wire)).unwrap();
    cache.put(&duplicate.0, &mut || false).unwrap();
    assert!(cache.get(wire).unwrap().same(&text.0));
    let wrong = Text::with_wire_identity("different", &domain, &mut || false, Some(wire)).unwrap();
    assert!(cache.put(&wrong.0, &mut || false).is_err());
    let foreign = Domain::new(1 << 20);
    let foreign_text = Text::new("foreign", &foreign, &mut || false).unwrap();
    assert!(cache.put(&foreign_text.0, &mut || false).is_err());
    drop((text, duplicate, wrong, foreign_text));
    assert_eq!(domain.statistics().live_strong_cells, 0);
    assert!(domain.statistics().logical_weak_fixed_bytes > 0);
    assert!(cache.get(wire).is_none());
    cache.clear();
    assert_eq!(domain.statistics().logical_used, 0);
    assert_eq!(foreign.statistics().logical_used, 0);
}

#[test]
fn bounded_directory_skips_new_admission_and_keeps_live_owners() {
    let domain = Domain::new(1 << 20);
    let mut table = IdentityTable::<u64>::resident(&domain, 16).unwrap();
    for n in 1..=12 {
        assert!(table.insert(key(n), n, &mut || false).unwrap());
    }
    let bytes = table.capacity_bytes();
    assert!(!table.insert(key(13), 13, &mut || false).unwrap());
    assert_eq!(table.capacity_bytes(), bytes);
    assert_eq!(table.get(key(1)), Some(&1));
    let mut staged = IdentityTable::resident(&domain, 16).unwrap();
    staged.insert(key(2), 55, &mut || false).unwrap();
    table.absorb(staged, &mut || false).unwrap();
    assert_eq!(table.get(key(2)), Some(&55));
    drop(table);
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn writer_directories_charge_old_new_and_staged_capacity_to_one_reservation() {
    let budget = MetadataBudget::new(1_800);
    let mut old = IdentityTable::<u64>::with_metadata(4_096, &budget);
    for n in 1..=12 {
        assert!(old.insert(key(n), n, &mut || false).unwrap());
    }
    let mut staged = IdentityTable::<u64>::with_metadata(4_096, &budget);
    assert!(staged.insert(key(100), 100, &mut || false).unwrap());
    assert!(!old.insert(key(13), 13, &mut || false).unwrap());
    assert_eq!(
        budget.used.load(Ordering::Acquire),
        old.capacity_bytes() + staged.capacity_bytes()
    );
    drop(staged);
    assert!(!old.insert(key(13), 13, &mut || false).unwrap());
    old.clear();
    assert_eq!(budget.used.load(Ordering::Acquire), 0);
}

#[test]
fn empty_destination_adopts_staged_table_without_duplicate_growth() {
    let bytes = 32 * size_of::<Slot<u64>>();
    let budget = MetadataBudget::new(bytes + bytes / 2);
    let mut current = IdentityTable::<u64>::with_metadata(bytes, &budget);
    let mut staged = IdentityTable::<u64>::with_metadata(bytes, &budget);
    for n in 1..=24 {
        assert!(staged.insert(key(n), n, &mut || false).unwrap());
    }
    let allocated = budget.used.load(Ordering::Acquire);
    current.absorb(staged, &mut || false).unwrap();
    assert_eq!(current.count, 24);
    assert_eq!(budget.used.load(Ordering::Acquire), allocated);
    for n in 1..=24 {
        assert_eq!(current.get(key(n)), Some(&n));
    }
    let other_budget = MetadataBudget::new(bytes);
    let wrong = IdentityTable::<u64>::with_metadata(bytes, &other_budget);
    assert!(current.absorb(wrong, &mut || false).is_err());
    drop(current);
    assert_eq!(budget.used.load(Ordering::Acquire), 0);
}
