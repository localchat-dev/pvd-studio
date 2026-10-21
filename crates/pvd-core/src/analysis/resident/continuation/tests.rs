use super::*;
use crate::analysis::{Analysis, MemoryEvent, MemoryEventKind, MemoryState, ProfileZone};
use memory::ResidentCursor;
use pvd_protocol::Endian;

pub(super) fn source() -> Analysis {
    let first = Arc::new(Allocation {
        address: 7,
        size: 12,
        type_name: "Actor".into(),
        file: "first.cpp".into(),
        line: 9,
        received_frame: 0,
    });
    let second = Arc::new(Allocation {
        address: 7,
        size: 40,
        type_name: "Actor v2".into(),
        file: "second.cpp".into(),
        line: 10,
        received_frame: 2,
    });
    let mut analysis = Analysis::default();
    analysis.add_zone(3, "Physics".into()).unwrap();
    analysis.add_event(3, 2, "Simulation".into()).unwrap();
    analysis.add_zone(3, "Physics v2".into()).unwrap();
    analysis.add_event(3, 2, "Simulation v2".into()).unwrap();
    analysis.zones.insert(
        9,
        ProfileZone {
            name: "Other".into(),
            events: [(4, "Same".into())].into(),
        },
    );
    analysis.spans.push(ProfileSpan {
        zone: 3,
        event: 2,
        thread: 15,
        context: 17,
        start: 800,
        end: 1000,
        received_frame: 2,
    });
    analysis.values.push(ProfileValue {
        zone: 3,
        event: 2,
        thread: 16,
        context: 18,
        raw_value: 1234,
        received_frame: 2,
    });
    analysis.unmatched_stops = 4;
    analysis.unmatched_stop_events.insert((3, 2, 15, 19), 4);
    analysis.legacy_cuda_buffers = 2;
    analysis.starts.insert((3, 4, 15, 19), vec![400, 600]);
    analysis.pending_starts = 2;
    let memory = Arc::make_mut(&mut analysis.memory);
    memory.base = Arc::new(MemoryState {
        live: [((1, 7), first.clone())].into_iter().collect(),
        live_bytes: 12,
        allocations: 1,
        total_allocated_bytes: 12,
        ..Default::default()
    });
    memory.live.insert((1, 7), second.clone());
    memory.live_bytes = 40;
    memory.peak_bytes = 52;
    memory.total_allocated_bytes = 52;
    memory.allocations = 2;
    memory.frees = 2;
    memory.unmatched_frees = 1;
    memory.replaced_addresses = 1;
    memory.strings.insert((1, 1), first.type_name.clone());
    memory.strings.insert((1, 2), second.file.clone());
    for (frame, kind) in [
        (
            2,
            MemoryEventKind::Allocate {
                allocation: second.clone(),
                replaced: Some(first.clone()),
            },
        ),
        (
            3,
            MemoryEventKind::Free {
                allocation: Some(second),
            },
        ),
        (4, MemoryEventKind::Free { allocation: None }),
    ] {
        memory.events.push(Arc::new(MemoryEvent {
            buffer: 1,
            address: 7,
            received_frame: frame,
            kind,
        }));
    }
    memory.samples.push(MemorySample {
        received_frame: 4,
        live_bytes: 0,
        live_allocations: 0,
        allocations: 2,
        frees: 2,
    });
    analysis
}

#[test]
fn full_continuation_seed_preserves_every_legacy_field_and_opaque_export_lifetime() {
    let source = source();
    let bytes = postcard::to_stdvec(&source).unwrap();
    let domain = Domain::new(2 << 20);
    let analysis =
        ResidentAnalysis::from_legacy_bytes(&bytes, domain.clone(), &mut || false).unwrap();
    assert_eq!(postcard::to_stdvec(&analysis).unwrap(), bytes);
    assert_eq!(analysis.open_spans(), 2);
    assert_eq!(analysis.accounted_bytes(), source.accounted_bytes());
    assert_eq!(analysis.spans().get(0).unwrap().start, 800);
    assert_eq!(analysis.values().get(0).unwrap().raw_value, 1234);
    assert_eq!(&*analysis.zone(3).unwrap().name, "Physics v2");
    assert_eq!(
        analysis.memory().samples().get(0).unwrap().received_frame,
        4
    );
    let before = domain.statistics().logical_used;
    let published = analysis.clone();
    assert!(published.same(&analysis));
    assert_eq!(domain.statistics().logical_used, before);
    let event = analysis.memory().events().get(0).unwrap().clone();
    let allocation = match &event.kind {
        EventKind::Allocate {
            replaced: Some(value),
            ..
        } => value.clone(),
        _ => panic!("replacement is missing"),
    };
    let text = allocation.file.clone();
    let base = analysis.memory().base.clone();
    drop((analysis, published));
    assert!(domain.statistics().logical_used < before);
    assert_eq!(base.live.get(&(1, 7)).unwrap().size, 12);
    drop((base, event));
    assert_eq!(domain.statistics().live_strong_cells, 3);
    drop(allocation);
    assert_eq!(domain.statistics().live_strong_cells, 1);
    assert_eq!(&*text, "first.cpp");
    drop(text);
    assert_eq!(domain.statistics().live_strong_cells, 0);
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn seed_budget_cancel_truncation_and_trailing_bytes_release_every_draft() {
    let bytes = postcard::to_stdvec(&source()).unwrap();
    let mut errors = 0;
    let mut success = 0;
    for allowance in (0..32_000).step_by(191) {
        let domain = Domain::new(allowance);
        match ResidentAnalysis::from_legacy_bytes(&bytes, domain.clone(), &mut || false) {
            Ok(value) => {
                success += 1;
                drop(value);
            }
            Err(_) => errors += 1,
        }
        assert_eq!(domain.statistics().logical_used, 0);
        assert_eq!(domain.statistics().live_strong_cells, 0);
    }
    assert!(errors > 5 && success > 5);
    for stop in 0..170 {
        let domain = Domain::new(2 << 20);
        let mut visits = 0;
        let result = ResidentAnalysis::from_legacy_bytes(&bytes, domain.clone(), &mut || {
            visits += 1;
            visits > stop
        });
        drop(result);
        assert_eq!(domain.statistics().logical_used, 0);
    }
    for end in 0..bytes.len() {
        let domain = Domain::new(2 << 20);
        assert!(
            ResidentAnalysis::from_legacy_bytes(&bytes[..end], domain.clone(), &mut || false)
                .is_err()
        );
        assert_eq!(domain.statistics().logical_used, 0);
    }
    let mut trailing = bytes;
    trailing.push(0);
    let domain = Domain::new(2 << 20);
    assert!(ResidentAnalysis::from_legacy_bytes(&trailing, domain.clone(), &mut || false).is_err());
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn record_seed_preserves_variant_and_original_budget_or_cancel_error_classification() {
    let bytes = postcard::to_stdvec(&source()).unwrap();
    let domain = Domain::new(2 << 20);
    let analysis =
        ResidentAnalysis::from_legacy_bytes(&bytes, domain.clone(), &mut || false).unwrap();
    let record = postcard::to_stdvec(&analysis.legacy_record()).unwrap();
    assert_eq!(record[0], 5);
    assert_eq!(&record[1..], bytes);
    let decoded =
        ResidentAnalysis::from_legacy_record_bytes(&record, domain.clone(), &mut || false).unwrap();
    assert_eq!(
        postcard::to_stdvec(&decoded.legacy_record()).unwrap(),
        record
    );
    let small = Domain::new(0);
    let failure = ResidentAnalysis::from_legacy_record_bytes(&record, small.clone(), &mut || false)
        .err()
        .unwrap();
    assert!(failure.downcast_ref::<ResidentBudgetExceeded>().is_some());
    assert_eq!(small.statistics().logical_used, 0);
    let cancelled =
        ResidentAnalysis::from_legacy_record_bytes(&record, small.clone(), &mut || true)
            .err()
            .unwrap();
    assert_eq!(
        cancelled.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
    let mut wrong = record;
    wrong[0] = 4;
    let rejected = ResidentAnalysis::from_legacy_record_bytes(&wrong, small.clone(), &mut || false)
        .err()
        .unwrap();
    assert!(rejected.downcast_ref::<ResidentBudgetExceeded>().is_none());
    assert_eq!(small.statistics().logical_used, 0);
    drop((analysis, decoded));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn owned_container_drafts_reserve_overlap_and_fail_without_mutating_shared_revisions() {
    let domain = Domain::new(2 << 20);
    let mut values = BufferDraft::new(&domain, 3).unwrap();
    for n in 0u64..3 {
        values.push(n).unwrap();
    }
    let values = values.finish().unwrap();
    let copy = values.clone();
    assert!(copy.same(&values));
    let mut draft = values.edit(&domain, &mut || false).unwrap();
    draft.push(4).unwrap();
    let next = draft.finish().unwrap();
    assert_eq!(
        values.view().iter().copied().collect::<Vec<_>>(),
        &[0, 1, 2]
    );
    assert_eq!(
        next.view().iter().copied().collect::<Vec<_>>(),
        &[0, 1, 2, 4]
    );
    assert_eq!(next.capacity(), 6);
    let mut tree = TreeDraft::new(&domain, 0).unwrap();
    tree.insert(2u64, 20u64).unwrap();
    tree.insert(1, 10).unwrap();
    let tree = tree.finish().unwrap();
    let old = tree.clone();
    assert!(tree.same(&old));
    let mut draft = tree.edit(&domain, &mut || false).unwrap();
    draft.insert(2, 30).unwrap();
    draft.remove(&1).unwrap();
    let next_tree = draft.finish().unwrap();
    assert_eq!(old.get(&2), Some(&20));
    assert_eq!(next_tree.get(&2), Some(&30));
    let held = domain
        .reserve(domain.statistics().limit - domain.statistics().logical_used)
        .unwrap();
    let mut blocked_values = values.edit(&domain, &mut || false).unwrap();
    assert!(blocked_values.push(9).is_err());
    assert!(blocked_values.finish().is_err());
    let mut blocked_tree = tree.edit(&domain, &mut || false).unwrap();
    assert!(blocked_tree.insert(3, 30).is_err());
    assert!(blocked_tree.finish().is_err());
    assert_eq!(
        values.view().iter().copied().collect::<Vec<_>>(),
        &[0, 1, 2]
    );
    assert_eq!(tree.len(), 2);
    drop((held, values, copy, next, tree, old, next_tree));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn explicit_map_seed_keeps_unsorted_and_last_duplicate_key_semantics() {
    use serde::ser::SerializeMap;
    struct Unsorted(Vec<(Key, Arc<Allocation>)>);
    impl Serialize for Unsorted {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut map = serializer.serialize_map(Some(self.0.len()))?;
            for (key, value) in &self.0 {
                map.serialize_entry(key, value)?;
            }
            map.end()
        }
    }
    let allocation = |address, size| {
        Arc::new(Allocation {
            address,
            size,
            type_name: "same".into(),
            file: "f.cpp".into(),
            line: 1,
            received_frame: 0,
        })
    };
    let bytes = postcard::to_stdvec(&Unsorted(vec![
        ((1, 8), allocation(8, 1)),
        ((1, 2), allocation(2, 2)),
        ((1, 8), allocation(8, 3)),
    ]))
    .unwrap();
    let domain = Domain::new(2 << 20);
    let mut cancel = || false;
    let mut context = seed::Context {
        domain: &domain,
        cancelled: &mut cancel,
        failure: None,
    };
    let mut de = postcard::Deserializer::from_bytes(&bytes);
    let map = seed::Seed::<ResidentMap>::new(&mut context)
        .deserialize(&mut de)
        .unwrap();
    assert!(de.finalize().unwrap().is_empty());
    assert_eq!(map.len(), 2);
    assert_eq!(map.get(&(1, 8)).unwrap().size, 3);
    assert_eq!(
        map.iter().map(|(key, _)| key).collect::<Vec<_>>(),
        vec![(1, 2), (1, 8)]
    );
    drop(map);
    assert_eq!(domain.statistics().logical_used, 0);
}

fn string(handle: u32, value: &[u8], endian: Endian) -> Vec<u8> {
    let mut bytes = uint(1, 2, endian);
    bytes.extend(uint((value.len() + 1) as u64, 4, endian));
    bytes.extend(value);
    bytes.push(0);
    bytes.extend(uint(u64::from(handle), 4, endian));
    bytes
}
fn uint(value: u64, width: usize, endian: Endian) -> Vec<u8> {
    match endian {
        Endian::Little => value.to_le_bytes()[..width].to_vec(),
        Endian::Big => value.to_be_bytes()[8 - width..].to_vec(),
    }
}
fn allocation(address: u64, size: u64, endian: Endian) -> Vec<u8> {
    let mut bytes = uint(
        2 | (3 << 4) | (2 << 6) | (2 << 8) | (2 << 10) | (2 << 12),
        2,
        endian,
    );
    for (value, width) in [(size, 4), (1, 4), (2, 4), (20, 4), (address, 8)] {
        bytes.extend(uint(value, width, endian));
    }
    bytes
}
fn free(address: u64, endian: Endian) -> Vec<u8> {
    let mut bytes = uint(3 | (3 << 4), 2, endian);
    bytes.extend(uint(address, 8, endian));
    bytes
}
fn profile(
    kind: u8,
    event: u16,
    thread: u32,
    context: u64,
    timestamp: u64,
    endian: Endian,
) -> Vec<u8> {
    let mut bytes = vec![kind, 15];
    bytes.extend(uint(u64::from(event), 2, endian));
    bytes.extend(uint(u64::from(thread), 4, endian));
    bytes.extend(uint(context, 8, endian));
    bytes.extend([0, 0]);
    bytes.extend(uint(timestamp, 8, endian));
    bytes
}
fn value_event(value: u64, endian: Endian) -> Vec<u8> {
    let mut bytes = vec![5, 15];
    bytes.extend(uint(9, 2, endian));
    bytes.extend(uint(value, 8, endian));
    bytes.extend(uint(3, 8, endian));
    bytes.extend(uint(5, 4, endian));
    bytes
}
pub(super) fn buffers(endian: Endian) -> Vec<(u64, &'static str, Vec<u8>)> {
    let mut names = string(1, b"Actor", endian);
    names.extend(string(2, b"source.cpp", endian));
    names.extend(allocation(7, 12, endian));
    names.extend(allocation(8, 20, endian));
    let mut replace = string(1, b"Actor\xffv2", endian);
    replace.extend(allocation(7, 40, endian));
    replace.extend(free(8, endian));
    replace.extend(free(99, endian));
    let mut start = profile(1, 4, 2, 3, 100, endian);
    start.extend(profile(1, 4, 2, 3, 120, endian));
    start.extend(value_event(1234, endian));
    let mut stop = profile(2, 4, 2, 3, 180, endian);
    stop.extend(profile(2, 4, 2, 3, 200, endian));
    stop.extend(profile(2, 4, 2, 3, 220, endian));
    let mut cuda = vec![6, 0, 0, 0];
    cuda.extend([0; 12]);
    cuda.extend(uint(3, 4, endian));
    cuda.extend([1, 2, 3]);
    cuda.extend([0; 8]);
    let memory = "PxProfileMemoryEventBuffer";
    let profile = "PxProfileZone";
    vec![
        (1, memory, names),
        (3, profile, start),
        (1, memory, replace),
        (3, profile, stop),
        (3, profile, cuda),
        (1, memory, free(7, endian)),
        (3, profile, profile_start(endian)),
        (1, memory, Vec::new()),
    ]
}
fn profile_start(endian: Endian) -> Vec<u8> {
    profile(1, 6, 7, 8, 400, endian)
}

#[test]
fn complete_fallible_ingest_matches_legacy_for_endian_metadata_pending_and_every_memory_event() {
    for endian in [Endian::Little, Endian::Big] {
        let domain = Domain::new(8 << 20);
        let mut actual = ResidentAnalysis::new(domain.clone()).unwrap();
        assert!(actual.domain().same(&domain));
        let mut expected = Analysis::default();
        for (zone, name) in [(3, "Physics"), (3, "Physics v2")] {
            actual.add_zone(zone, name, &mut || false).unwrap();
            expected.add_zone(zone, name.into()).unwrap();
        }
        for name in ["simulate", "simulate v2"] {
            actual.add_event(3, 4, name, &mut || false).unwrap();
            expected.add_event(3, 4, name.into()).unwrap();
        }
        let mut snapshots = Vec::new();
        for (frame, (id, kind, bytes)) in buffers(endian).iter().enumerate() {
            let published = actual.clone();
            let old_wire = postcard::to_stdvec(&published).unwrap();
            let revision = actual.revision();
            actual
                .ingest(*id, kind, bytes, endian, frame, &mut || false)
                .unwrap();
            expected.ingest(*id, kind, bytes, endian, frame).unwrap();
            assert_ne!(actual.revision(), revision);
            assert_eq!(postcard::to_stdvec(&published).unwrap(), old_wire);
            assert_eq!(
                postcard::to_stdvec(&actual).unwrap(),
                postcard::to_stdvec(&expected).unwrap(),
                "frame {frame}"
            );
            snapshots.push(actual.clone());
        }
        let memory = actual.memory();
        let mut cursor = ResidentCursor::new(memory);
        for frame in [7, 2, 0, 6, 3, 1, 7] {
            memory.try_seek(&mut cursor, frame, &mut || false).unwrap();
            assert_eq!(
                postcard::to_stdvec(cursor.state()).unwrap(),
                postcard::to_stdvec(&expected.memory.state_at_frame(frame)).unwrap()
            );
            let state = memory.try_state_at_frame(frame, &mut || false).unwrap();
            assert_eq!(
                postcard::to_stdvec(&state).unwrap(),
                postcard::to_stdvec(cursor.state()).unwrap()
            );
        }
        let retained_event = actual.memory().events().get(0).unwrap().clone();
        let retained_text = actual.memory().string(1, 1).unwrap().clone();
        drop((actual, snapshots, cursor));
        assert!(domain.statistics().live_strong_cells < 10);
        assert!(!retained_text.is_empty());
        drop((retained_event, retained_text));
        assert_eq!(domain.statistics().logical_used, 0);
    }
}

#[test]
fn whole_buffer_and_cursor_failures_keep_published_revision_and_all_counters_unchanged() {
    let domain = Domain::new(2 << 20);
    let mut base = ResidentAnalysis::new(domain.clone()).unwrap();
    let first = &buffers(Endian::Little)[0];
    base.ingest(first.0, first.1, &first.2, Endian::Little, 0, &mut || false)
        .unwrap();
    let published = base.clone();
    let initial = postcard::to_stdvec(&published).unwrap();
    let mut bytes = allocation(7, 70, Endian::Little);
    bytes.extend(allocation(10, 100, Endian::Little));
    bytes.extend(free(8, Endian::Little));
    bytes.extend(allocation(11, 120, Endian::Little));
    let used = domain.statistics().logical_used;
    let mut failures = 0;
    let mut success = 0;
    for headroom in (0..24_000).step_by(97) {
        let mut draft = base.clone();
        let held = domain
            .reserve(domain.statistics().limit - domain.statistics().logical_used - headroom)
            .unwrap();
        let before = domain.statistics().logical_used;
        let revision = draft.revision();
        match draft.ingest(
            1,
            "PxProfileMemoryEventBuffer",
            &bytes,
            Endian::Little,
            1,
            &mut || false,
        ) {
            Ok(()) => {
                success += 1;
                assert_eq!(draft.memory().live.len(), 3);
            }
            Err(_) => {
                failures += 1;
                assert!(draft.same(&published));
                assert_eq!(draft.revision(), revision);
                assert_eq!(postcard::to_stdvec(&draft).unwrap(), initial);
                assert_eq!(domain.statistics().logical_used, before);
            }
        }
        drop((held, draft));
        assert_eq!(domain.statistics().logical_used, used);
    }
    assert!(failures > 10 && success > 10);
    for stop in 0..120 {
        let mut draft = base.clone();
        let mut visits = 0;
        let result = draft.ingest(
            1,
            "PxProfileMemoryEventBuffer",
            &bytes,
            Endian::Little,
            1,
            &mut || {
                visits += 1;
                visits > stop
            },
        );
        if result.is_err() {
            assert!(draft.same(&base));
        }
        drop(draft);
        assert_eq!(domain.statistics().logical_used, used);
    }
    let mut completed = base.clone();
    completed
        .ingest(
            1,
            "PxProfileMemoryEventBuffer",
            &bytes,
            Endian::Little,
            1,
            &mut || false,
        )
        .unwrap();
    let memory = completed.memory();
    let mut cursor = ResidentCursor::new(memory);
    memory.try_seek(&mut cursor, 0, &mut || false).unwrap();
    let cursor_wire = postcard::to_stdvec(cursor.state()).unwrap();
    let position = cursor.position();
    for stop in 0..65 {
        let mut next = cursor.clone();
        let mut visits = 0;
        if memory
            .try_seek(&mut next, 1, &mut || {
                visits += 1;
                visits > stop
            })
            .is_err()
        {
            assert_eq!(next.position(), position);
            assert_eq!(postcard::to_stdvec(next.state()).unwrap(), cursor_wire);
        }
    }
    drop((cursor, completed, base, published));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn native_resolver_uses_the_same_domain_and_malformed_late_record_does_not_publish_prefix() {
    let domain = Domain::new(2 << 20);
    let mut actual = ResidentAnalysis::new(domain.clone()).unwrap();
    let mut expected = Analysis::default();
    let bytes = allocation(7, 12, Endian::Little);
    actual
        .ingest_native_memory(
            1,
            &bytes,
            0,
            |handle| Ok(if handle == 1 { "Actor" } else { "source.cpp" }),
            &mut || false,
        )
        .unwrap();
    expected
        .ingest_native_memory(1, &bytes, 0, |handle| {
            Ok(if handle == 1 { "Actor" } else { "source.cpp" })
        })
        .unwrap();
    assert_eq!(
        postcard::to_stdvec(&actual).unwrap(),
        postcard::to_stdvec(&expected).unwrap()
    );
    let before = actual.clone();
    let mut truncated = bytes;
    truncated.extend([2, 0]);
    assert!(
        actual
            .ingest(
                1,
                "PxProfileMemoryEventBuffer",
                &truncated,
                Endian::Little,
                1,
                &mut || false
            )
            .is_err()
    );
    assert!(actual.same(&before));
    let profile = profile(1, 4, 2, 3, 100, Endian::Little);
    let mut invalid = profile.clone();
    invalid.extend([255, 0, 0, 0]);
    assert!(
        actual
            .ingest(3, "PxProfileZone", &invalid, Endian::Little, 1, &mut || {
                false
            })
            .is_err()
    );
    assert!(actual.same(&before));
    actual
        .ingest(3, "PxProfileZone", &profile, Endian::Little, 1, &mut || {
            false
        })
        .unwrap();
    assert_eq!(actual.open_spans(), 1);
    drop((actual, before));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn retention_recounts_legacy_layout_and_cursor_clamps_without_changing_exported_states() {
    let domain = Domain::new(8 << 20);
    let mut actual = ResidentAnalysis::new(domain.clone()).unwrap();
    let mut expected = Analysis::default();
    actual.add_zone(3, "Physics", &mut || false).unwrap();
    expected.add_zone(3, "Physics".into()).unwrap();
    for (frame, (id, kind, bytes)) in buffers(Endian::Little).iter().enumerate() {
        actual
            .ingest(*id, kind, bytes, Endian::Little, frame, &mut || false)
            .unwrap();
        expected
            .ingest(*id, kind, bytes, Endian::Little, frame)
            .unwrap();
    }
    let old = actual.clone();
    let original = postcard::to_stdvec(&old).unwrap();
    let state = old.memory().try_state_at_frame(1, &mut || false).unwrap();
    let state_wire = postcard::to_stdvec(&state).unwrap();
    for first in [0, 1, 2, 4, 7, 8] {
        let mut current = actual.clone();
        let mut oracle = expected.clone();
        current.try_retain_recent(first, &mut || false).unwrap();
        oracle.retain_recent(first);
        assert_eq!(
            postcard::to_stdvec(&current).unwrap(),
            postcard::to_stdvec(&oracle).unwrap(),
            "retained {first}"
        );
        for frame in [0, 2, 5, 7, 10] {
            let state = current
                .memory()
                .try_state_at_frame(frame, &mut || false)
                .unwrap();
            assert_eq!(
                postcard::to_stdvec(&state).unwrap(),
                postcard::to_stdvec(&oracle.memory.state_at_frame(frame)).unwrap()
            );
        }
        let mut captured = actual.clone();
        captured
            .try_retain_captured(first, oracle.accounted_bytes(), &mut || false)
            .unwrap();
        assert_eq!(
            postcard::to_stdvec(&captured).unwrap(),
            postcard::to_stdvec(&oracle).unwrap()
        );
        assert_eq!(postcard::to_stdvec(&old).unwrap(), original);
        assert_eq!(postcard::to_stdvec(&state).unwrap(), state_wire);
    }
    let mut failed = old.clone();
    let mut calls = 0;
    assert!(
        failed
            .try_retain_recent(7, &mut || {
                calls += 1;
                calls > 10
            })
            .is_err()
    );
    assert!(failed.same(&old));
    drop((actual, old, failed, state));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn typed_containers_reject_foreign_nested_owners_and_release_empty_tree_before_reducing_allowance()
{
    let domain = Domain::new(2 << 20);
    let foreign = Domain::new(2 << 20);
    let text = Text::new("foreign", &foreign, &mut || false).unwrap();
    let mut strings = TreeDraft::new(&domain, 0).unwrap();
    assert!(strings.insert(1u64, text.clone()).is_err());
    assert_eq!(strings.len(), 0);
    let nested = Buffer::<u64>::empty(&foreign).unwrap();
    let mut starts = TreeDraft::new(&domain, 0).unwrap();
    assert!(starts.insert(1u64, nested).is_err());
    let analysis = ResidentAnalysis::new(foreign.clone()).unwrap();
    let mut other = analysis.clone();
    let first = &buffers(Endian::Little)[0];
    other
        .ingest(first.0, first.1, &first.2, Endian::Little, 0, &mut || false)
        .unwrap();
    let mut events = BufferDraft::new(&domain, 0).unwrap();
    assert!(
        events
            .push(other.memory().events().get(0).unwrap().clone())
            .is_err()
    );
    assert_eq!(events.len(), 0);
    let mut local = TreeDraft::new(&domain, 0).unwrap();
    let empty_used = domain.statistics().logical_used;
    local.insert(1u64, 2u64).unwrap();
    assert!(domain.statistics().logical_used > empty_used);
    assert_eq!(local.remove(&1).unwrap(), Some(2));
    assert_eq!(domain.statistics().logical_used, empty_used);
    let local = local.finish().unwrap();
    assert_eq!(local.len(), 0);
    drop((text, strings, starts, analysis, other, events, local));
    assert_eq!(domain.statistics().logical_used, 0);
    assert_eq!(foreign.statistics().logical_used, 0);
}

#[test]
#[ignore = "requires PVD_BASELINE_FIXTURE pointing to the frozen Analysis history record"]
fn real_complete_legacy_analysis_seed_roundtrips_without_an_unbudgeted_legacy_object_graph() {
    let path =
        std::path::PathBuf::from(std::env::var_os("PVD_BASELINE_FIXTURE").expect("fixture path"));
    let metadata: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path.with_extension("json")).unwrap()).unwrap();
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(
        blake3::hash(&bytes).to_hex().to_string(),
        metadata["payload_blake3"]
    );
    let domain = Domain::new(192 << 20);
    let analysis =
        ResidentAnalysis::from_legacy_record_bytes(&bytes, domain.clone(), &mut || false).unwrap();
    assert_eq!(
        postcard::to_stdvec(&analysis.legacy_record()).unwrap(),
        bytes
    );
    println!(
        "resident full Analysis seed: live={} events={} spans={} values={} used={} peak={}",
        analysis.memory().live.len(),
        analysis.memory().events().len(),
        analysis.spans().len(),
        analysis.values().len(),
        domain.statistics().logical_used,
        domain.statistics().logical_peak
    );
    drop(analysis);
    assert_eq!(domain.statistics().logical_used, 0);
    assert_eq!(domain.statistics().live_strong_cells, 0);
}

#[test]
fn shared_clones_do_no_copy_and_memory_append_copies_only_the_current_tails() {
    let domain = Domain::new(2 << 20);
    let mut analysis = ResidentAnalysis::new(domain.clone()).unwrap();
    let first = &buffers(Endian::Little)[0];
    analysis
        .ingest(first.0, first.1, &first.2, Endian::Little, 0, &mut || false)
        .unwrap();
    let before = domain.statistics();
    let old = analysis.clone();
    let cloned = domain.statistics();
    assert_eq!(before.buffer_copied_elements, cloned.buffer_copied_elements);
    assert_eq!(before.tree_copied_entries, cloned.tree_copied_entries);
    let events = analysis.memory().events().len();
    let samples = analysis.memory().samples().len();
    let old_wire = postcard::to_stdvec(&old).unwrap();
    analysis
        .ingest(
            1,
            "PxProfileMemoryEventBuffer",
            &allocation(20, 16, Endian::Little),
            Endian::Little,
            1,
            &mut || false,
        )
        .unwrap();
    let changed = domain.statistics();
    assert_eq!(
        changed.buffer_copied_elements - before.buffer_copied_elements,
        events + samples
    );
    assert_eq!(
        changed.buffer_copied_layout_bytes - before.buffer_copied_layout_bytes,
        old.memory().events().len() * size_of::<EventRef>()
            + old.memory().samples().len() * size_of::<MemorySample>()
    );
    assert_eq!(changed.tree_copied_entries, before.tree_copied_entries);
    assert_eq!(postcard::to_stdvec(&old).unwrap(), old_wire);
    drop((analysis, old));
    assert_eq!(domain.statistics().logical_used, 0);
}
