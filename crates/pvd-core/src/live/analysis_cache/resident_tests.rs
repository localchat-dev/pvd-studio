use super::*;
use crate::analysis::resident::{ResidentAnalysis, full_fixture_bytes};
use crate::analysis::view::MemorySource;

fn resident(domain: &Domain) -> SharedAnalysis {
    let data: SharedAnalysis =
        ResidentAnalysis::from_legacy_bytes(&full_fixture_bytes(), domain.clone(), &mut || false)
            .unwrap()
            .into();
    assert!(data.memory_snapshot().event_count() > 0);
    data
}

fn legacy() -> SharedAnalysis {
    postcard::from_bytes::<Analysis>(&full_fixture_bytes())
        .unwrap()
        .into()
}

fn wire(data: &SharedAnalysis) -> Vec<u8> {
    postcard::to_stdvec(data).unwrap()
}

fn assert_frame(cache: &AnalysisCache, frame: usize, expected: &[u8]) {
    let (actual_frame, data) = cache.best(frame, frame).unwrap();
    assert_eq!(actual_frame, frame);
    assert_eq!(wire(&data), expected, "frame {frame}");
}

#[test]
fn resident_replay_keeps_120_distinct_completed_roots_and_evicts_the_least_recently_used() {
    let domain = Domain::new(8 << 20);
    let cache = AnalysisCache::default();
    let lease = ResourceLease::new(Vec::new());
    let mut data = resident(&domain);
    let mut reference = legacy();
    let mut expected = Vec::new();
    for frame in 0..120 {
        for root in [&mut data, &mut reference] {
            root.add_zone(10_000 + frame as u64, format!("frame {frame}"))
                .unwrap();
        }
        expected.push(wire(&reference));
        cache.insert_checkpoint(frame, data.clone(), &lease);
    }
    assert_eq!(cache.stats().0, 120);
    for (frame, bytes) in expected.iter().enumerate() {
        assert_frame(&cache, frame, bytes);
    }
    assert_frame(&cache, 0, &expected[0]);

    for frame in 120..122 {
        for root in [&mut data, &mut reference] {
            root.add_zone(10_000 + frame as u64, format!("frame {frame}"))
                .unwrap();
        }
        expected.push(wire(&reference));
        cache.insert_checkpoint(frame, data.clone(), &lease);
        assert_eq!(cache.stats().0, 120);
        assert!(cache.best(frame - 119, frame - 119).is_none());
        assert_frame(&cache, 0, &expected[0]);
        assert_frame(&cache, frame, &expected[frame]);
    }
    for (frame, bytes) in expected.iter().enumerate().skip(3) {
        assert_frame(&cache, frame, bytes);
    }
    drop((cache, data));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn resident_payload_is_charged_once_in_its_domain_under_a_metadata_only_cache_limit() {
    const CACHE_BYTES: usize = 64 << 10;
    let domain = Domain::new(8 << 20);
    let mut data = resident(&domain);
    data.add_zone(90_000, "x".repeat(256 << 10)).unwrap();
    let expected = wire(&data);
    let before = domain.statistics();
    assert!(before.logical_used > CACHE_BYTES);
    assert!(data.accounted_bytes() > CACHE_BYTES);
    let cache = AnalysisCache::with_limits(120, CACHE_BYTES);
    let lease = ResourceLease::new(Vec::new());
    for frame in 0..120 {
        cache.insert_checkpoint(frame, data.clone(), &lease);
    }
    assert_eq!(cache.stats().0, 120);
    assert!(cache.bytes() <= CACHE_BYTES);
    for frame in [0, 59, 119] {
        assert_frame(&cache, frame, &expected);
    }
    let after = domain.statistics();
    assert_eq!(after.logical_used, before.logical_used);
    assert_eq!(after.logical_peak, before.logical_peak);
    assert_eq!(
        after.object_allocations_total,
        before.object_allocations_total
    );
    assert_eq!(cache.clear_resident(), 120);
    assert_eq!(domain.statistics().logical_used, before.logical_used);
    assert_eq!(wire(&data), expected);
    drop(data);
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn reclaiming_one_domain_preserves_external_old_roots_and_other_domains() {
    let first_domain = Domain::new(8 << 20);
    let other_domain = Domain::new(8 << 20);
    let cache = AnalysisCache::default();
    let lease = ResourceLease::new(Vec::new());
    let mut data = resident(&first_domain);
    let displayed_old = data.clone();
    let old_bytes = wire(&displayed_old);
    let old_used = first_domain.statistics().logical_used;
    cache.insert(0, data.clone(), &lease);
    data.add_zone(91_000, "later root".into()).unwrap();
    cache.insert_checkpoint(1, data, &lease);
    let other = resident(&other_domain);
    let other_bytes = wire(&other);
    let other_used = other_domain.statistics().logical_used;
    cache.insert_checkpoint(2, other, &lease);
    assert!(first_domain.statistics().logical_used > old_used);

    assert_eq!(cache.reclaim_resident(&first_domain), 2);
    assert_eq!(cache.reclaim_resident(&first_domain), 0);
    assert!(cache.best(0, 1).is_none());
    assert_eq!(first_domain.statistics().logical_used, old_used);
    assert_eq!(wire(&displayed_old), old_bytes);
    assert_frame(&cache, 2, &other_bytes);
    assert_eq!(other_domain.statistics().logical_used, other_used);
    drop(displayed_old);
    assert_eq!(first_domain.statistics().logical_used, 0);
    assert_eq!(cache.clear_resident(), 1);
    assert_eq!(other_domain.statistics().logical_used, 0);
}

#[test]
fn clearing_all_resident_roots_retains_complete_legacy_analyses() {
    let first_domain = Domain::new(8 << 20);
    let second_domain = Domain::new(8 << 20);
    let cache = AnalysisCache::default();
    let lease = ResourceLease::new(Vec::new());
    let first_legacy = legacy();
    let first_bytes = wire(&first_legacy);
    cache.insert(1, first_legacy, &lease);
    cache.insert_checkpoint(2, resident(&first_domain), &lease);
    let mut second_legacy = legacy();
    second_legacy
        .add_zone(92_000, "legacy later".into())
        .unwrap();
    let second_bytes = wire(&second_legacy);
    cache.insert(3, second_legacy, &lease);
    cache.insert_checkpoint(4, resident(&second_domain), &lease);

    assert_eq!(cache.clear_resident(), 2);
    assert_eq!(cache.clear_resident(), 0);
    assert_eq!(cache.stats().0, 2);
    assert_eq!(first_domain.statistics().logical_used, 0);
    assert_eq!(second_domain.statistics().logical_used, 0);
    assert_frame(&cache, 1, &first_bytes);
    assert_frame(&cache, 3, &second_bytes);
    assert!(cache.best(2, 2).is_none());
    assert!(cache.best(4, 4).is_none());
}

#[test]
fn unavailable_segment_leases_remove_resident_entries_without_revoking_displayed_roots() {
    let held_domain = Domain::new(8 << 20);
    let dropped_domain = Domain::new(8 << 20);
    let cache = AnalysisCache::default();
    let invalidated = ResourceLease::new(Vec::new());
    let dropped = ResourceLease::new(Vec::new());
    let displayed = resident(&held_domain);
    let expected = wire(&displayed);
    cache.insert_checkpoint(5, displayed.clone(), &invalidated);
    cache.insert_checkpoint(6, resident(&dropped_domain), &dropped);
    invalidated.invalidate().unwrap();
    assert!(cache.best(5, 5).is_none());
    assert!(cache.best(6, 6).is_some());
    cache.insert_checkpoint(7, displayed.clone(), &invalidated);
    assert!(cache.best(7, 7).is_none());
    assert_eq!(wire(&displayed), expected);
    assert!(held_domain.statistics().logical_used > 0);

    drop(dropped);
    cache.discard_expired();
    assert_eq!(cache.stats().0, 0);
    assert!(cache.best(6, 6).is_none());
    assert_eq!(dropped_domain.statistics().logical_used, 0);
    drop(displayed);
    assert_eq!(held_domain.statistics().logical_used, 0);
}

#[test]
fn mixed_cache_keeps_independent_120_resident_and_32_legacy_limits() {
    let domain = Domain::new(8 << 20);
    let cache = AnalysisCache::default();
    let lease = ResourceLease::new(Vec::new());
    let data = resident(&domain);
    let legacy = legacy();
    let resident_bytes = wire(&data);
    let legacy_bytes = wire(&legacy);
    for frame in 0..120 {
        cache.insert_checkpoint(frame, data.clone(), &lease);
    }
    for frame in 1_000..1_032 {
        cache.insert(frame, legacy.clone(), &lease);
    }
    assert_eq!(cache.stats().0, 152);
    assert_frame(&cache, 0, &resident_bytes);
    assert_frame(&cache, 1_000, &legacy_bytes);
    cache.insert_checkpoint(120, data.clone(), &lease);
    assert_eq!(cache.stats().0, 152);
    assert!(cache.best(1, 1).is_none());
    cache.insert(1_032, legacy, &lease);
    assert_eq!(cache.stats().0, 152);
    assert!(cache.best(1_001, 1_001).is_none());
    assert_frame(&cache, 0, &resident_bytes);
    assert_frame(&cache, 120, &resident_bytes);
    assert_frame(&cache, 1_000, &legacy_bytes);
    assert_frame(&cache, 1_032, &legacy_bytes);
    assert!(cache.bytes() <= MAX_BYTES);

    assert_eq!(cache.clear_resident(), 120);
    assert_eq!(cache.stats().0, 32);
    for frame in std::iter::once(1_000).chain(1_002..1_033) {
        assert_frame(&cache, frame, &legacy_bytes);
    }
    drop(data);
    assert_eq!(domain.statistics().logical_used, 0);
}
