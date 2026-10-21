use super::*;

#[test]
fn cache_absorb_moves_weak_ownership_and_rejects_foreign_domains() {
    let domain = Domain::new(1 << 20);
    let item = Text::new("held", &domain, &mut || false).unwrap();
    let mut current = Cache::<u64, Box<str>, u64>::new(2, &domain).unwrap();
    let mut staged = Cache::new(2, &domain).unwrap();
    staged.put(1, &item.0, 4);
    let weak_before = domain.statistics().logical_weak_fixed_bytes;
    current.absorb(staged).unwrap();
    assert_eq!(domain.statistics().logical_weak_fixed_bytes, weak_before);
    assert!(current.get(1).unwrap().0.same(&item.0));
    let foreign = Domain::new(1 << 20);
    let other = Cache::new(1, &foreign).unwrap();
    assert!(current.absorb(other).is_err());
    drop(item);
    assert!(current.get(1).is_none());
    drop(current);
    assert_eq!(domain.statistics().logical_used, 0);
    assert_eq!(foreign.statistics().logical_used, 0);
}
