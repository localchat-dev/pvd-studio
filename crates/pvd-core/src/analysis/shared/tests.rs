use super::*;
use crate::analysis::resident::{Domain, full_fixture_bytes, full_fixture_inputs};
use crate::analysis::view::{MemoryRows, MemorySource, MemoryViewCursor, ProfileMetadataSource};

fn resident(domain: &Domain) -> SharedAnalysis {
    ResidentAnalysis::from_legacy_bytes(&full_fixture_bytes(), domain.clone(), &mut || false)
        .unwrap()
        .into()
}

fn legacy() -> SharedAnalysis {
    postcard::from_bytes::<Analysis>(&full_fixture_bytes())
        .unwrap()
        .into()
}

#[test]
fn published_legacy_revisions_keep_original_arc_until_all_readers_drop() {
    let original = Arc::new(Analysis::default());
    let weak = Arc::downgrade(&original);
    let mut current: SharedAnalysis = original.into();
    let published = current.clone();
    let revision = current.revision();
    current.add_zone(4, "physics".into()).unwrap();
    assert_ne!(current.revision(), revision);
    assert_eq!(published.profile_zone_name(4), None);
    assert_eq!(current.profile_zone_name(4), Some("physics"));
    assert!(weak.upgrade().is_some());
    drop(published);
    assert!(weak.upgrade().is_none());
}

#[test]
fn both_backends_serialize_complete_continuation_and_resume_identically() {
    let domain = Domain::new(8 << 20);
    let mut actual = resident(&domain);
    let mut expected = legacy();
    assert!(actual.legacy().is_none());
    assert_eq!(postcard::to_stdvec(&actual).unwrap(), full_fixture_bytes());
    let published = actual.clone();
    for (id, kind, bytes) in full_fixture_inputs(Endian::Little) {
        actual.ingest(id, kind, &bytes, Endian::Little, 5).unwrap();
        expected
            .ingest(id, kind, &bytes, Endian::Little, 5)
            .unwrap();
        assert_eq!(
            postcard::to_stdvec(&actual).unwrap(),
            postcard::to_stdvec(&expected).unwrap()
        );
    }
    assert_eq!(
        postcard::to_stdvec(&published).unwrap(),
        full_fixture_bytes()
    );
    assert_ne!(actual.revision(), published.revision());
    drop((actual, published));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn borrowed_metadata_and_double_ended_iterators_preserve_order_without_owned_rows() {
    let domain = Domain::new(8 << 20);
    let mut actual = resident(&domain);
    let mut expected = legacy();
    for root in [&mut actual, &mut expected] {
        root.add_zone(77, "empty".into()).unwrap();
        root.add_zone(77, "renamed empty".into()).unwrap();
    }
    let before = domain.statistics();
    assert_eq!(actual.profile_summary(), expected.profile_summary());
    assert_eq!(
        actual.profile_zones().collect::<Vec<_>>(),
        expected.profile_zones().collect::<Vec<_>>()
    );
    assert_eq!(
        actual.profile_names().collect::<Vec<_>>(),
        expected.profile_names().collect::<Vec<_>>()
    );
    assert_eq!(
        actual.profile_ambiguous_zones().collect::<Vec<_>>(),
        expected.profile_ambiguous_zones().collect::<Vec<_>>()
    );
    assert_eq!(
        actual.profile_ambiguous_events().collect::<Vec<_>>(),
        expected.profile_ambiguous_events().collect::<Vec<_>>()
    );
    assert_eq!(
        actual.profile_open_counts().collect::<Vec<_>>(),
        expected.profile_open_counts().collect::<Vec<_>>()
    );
    assert_eq!(
        actual.open_profile_events().collect::<Vec<_>>(),
        expected.open_profile_events().collect::<Vec<_>>()
    );
    assert_eq!(
        actual.unmatched_profile_events().collect::<Vec<_>>(),
        expected.unmatched_profile_events().collect::<Vec<_>>()
    );
    assert_eq!(
        actual.profile_metadata_budget(),
        expected
            .profile_metadata_budget()
            .min(before.limit - before.logical_used)
    );
    for reverse in [false, true] {
        let mut left = actual.profile_spans();
        let mut right = expected.profile_spans();
        assert_eq!(left.len(), right.len());
        while left.len() != 0 {
            let a = if reverse {
                left.next_back()
            } else {
                left.next()
            };
            let b = if reverse {
                right.next_back()
            } else {
                right.next()
            };
            assert_eq!(
                postcard::to_stdvec(&a).unwrap(),
                postcard::to_stdvec(&b).unwrap()
            );
            assert_eq!(left.len(), right.len());
        }
    }
    assert_eq!(
        postcard::to_stdvec(&actual.profile_values().rev().collect::<Vec<_>>()).unwrap(),
        postcard::to_stdvec(&expected.profile_values().rev().collect::<Vec<_>>()).unwrap()
    );
    let after = domain.statistics();
    assert_eq!(after.logical_used, before.logical_used);
    assert_eq!(
        after.object_allocations_total,
        before.object_allocations_total
    );
}

#[test]
fn cursor_switches_backend_or_domain_only_after_success_and_owns_rows_after_root_drop() {
    let first_domain = Domain::new(8 << 20);
    let second_domain = Domain::new(8 << 20);
    let original = legacy();
    let first = resident(&first_domain);
    let second = resident(&second_domain);
    let mut cursor = MemoryViewCursor::default();
    assert_eq!(cursor.live_count(), 0);
    original.memory_snapshot().seek(&mut cursor, 2).unwrap();
    assert_eq!(cursor.allocation_at(0).unwrap().size, 40);
    for (source, domain) in [(&first, &first_domain), (&second, &second_domain)] {
        let previous_size = cursor.allocation_at(0).unwrap().size;
        let stats = domain.statistics();
        let block = domain.reserve(stats.limit - stats.logical_used).unwrap();
        assert!(source.memory_snapshot().seek(&mut cursor, 2).is_err());
        assert_eq!(cursor.allocation_at(0).unwrap().size, previous_size);
        drop(block);
        source.memory_snapshot().seek(&mut cursor, 0).unwrap();
        assert_eq!(cursor.allocation_at(0).unwrap().size, 12);
        source.memory_snapshot().seek(&mut cursor, 2).unwrap();
    }
    drop((original, first, second));
    assert_eq!(first_domain.statistics().logical_used, 0);
    assert!(second_domain.statistics().logical_used > 0);
    assert_eq!(cursor.allocation_at(0).unwrap().file, "second.cpp");
    let empty = SharedAnalysis::default();
    empty.memory_snapshot().seek(&mut cursor, 0).unwrap();
    assert_eq!(cursor.live_count(), 0);
    assert_eq!(second_domain.statistics().logical_used, 0);
}

#[test]
fn resident_mutation_refusal_keeps_revision_continuation_and_retention_schedule() {
    let domain = Domain::new(8 << 20);
    let mut actual = resident(&domain);
    let old_revision = actual.revision();
    let old_bytes = postcard::to_stdvec(&actual).unwrap();
    let old_retained = actual.retention.retained_bytes;
    let stats = domain.statistics();
    let block = domain.reserve(stats.limit - stats.logical_used).unwrap();
    assert!(actual.add_zone(90, "refused".into()).is_err());
    assert!(actual.reset_at(2).is_err());
    assert!(actual.retain_recent(2).is_err());
    assert!(actual.maintain_live_history(660).is_err());
    assert_eq!(actual.revision(), old_revision);
    assert_eq!(postcard::to_stdvec(&actual).unwrap(), old_bytes);
    assert_eq!(actual.retention.retained_bytes, old_retained);
    drop(block);
    assert!(actual.maintain_live_history(660).unwrap());
    assert_eq!(actual.first_retained_frame(), 60);
    assert_ne!(actual.revision(), old_revision);
}

#[test]
fn shared_memory_views_match_all_summary_events_samples_and_reversible_rows() {
    let domain = Domain::new(8 << 20);
    let actual = resident(&domain);
    let expected = legacy();
    let a = actual.memory_snapshot();
    let b = expected.memory_snapshot();
    assert_eq!(a.summary(), b.summary());
    assert_eq!(a.event_count(), b.event_count());
    for index in 0..=a.event_count() {
        assert_eq!(a.event_at(index), b.event_at(index));
    }
    for index in 0..=a.sample_count() {
        assert_eq!(
            postcard::to_stdvec(&a.sample_at(index)).unwrap(),
            postcard::to_stdvec(&b.sample_at(index)).unwrap()
        );
    }
    let mut ac = MemoryViewCursor::default();
    let mut bc = MemoryViewCursor::default();
    for frame in [0, 2, 4, 3, 2, 0, 99] {
        a.seek(&mut ac, frame).unwrap();
        b.seek(&mut bc, frame).unwrap();
        assert_eq!(a.churn(frame), b.churn(frame));
        assert_eq!(a.frame_events(frame), b.frame_events(frame));
        assert_eq!(ac.live_count(), bc.live_count());
        assert_eq!(ac.live_bytes(), bc.live_bytes());
        assert_eq!(
            postcard::to_stdvec(&ac).unwrap(),
            postcard::to_stdvec(&bc).unwrap()
        );
        for index in 0..=ac.live_count() {
            assert_eq!(ac.allocation_at(index), bc.allocation_at(index));
        }
    }
}
