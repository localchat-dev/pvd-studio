use pvd_core::{
    MemoryHistoryScope, Recording,
    analysis::{Allocation, MemoryEvent, MemoryEventKind, MemoryState},
};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Default)]
struct Expected {
    live: BTreeMap<(u64, u64), Arc<Allocation>>,
    live_bytes: u64,
    allocated_bytes: u64,
    freed_bytes: u64,
    peak_bytes: u64,
    allocations: u64,
    frees: u64,
    unknown: u64,
    replacements: u64,
}

impl Expected {
    fn apply(&mut self, event: &MemoryEvent) -> Option<Arc<Allocation>> {
        let key = (event.buffer, event.address);
        let old = self.live.remove(&key);
        if let Some(value) = &old {
            self.live_bytes -= value.size;
        }
        match &event.kind {
            MemoryEventKind::Allocate { allocation, .. } => {
                self.replacements += u64::from(old.is_some());
                self.allocations += 1;
                self.allocated_bytes += allocation.size;
                self.live_bytes += allocation.size;
                self.peak_bytes = self.peak_bytes.max(self.live_bytes);
                self.live.insert(key, allocation.clone());
            }
            MemoryEventKind::Free { .. } => {
                self.frees += 1;
                if let Some(value) = &old {
                    self.freed_bytes += value.size;
                } else {
                    self.unknown += 1;
                }
            }
        }
        old
    }

    fn verify_state(&self, actual: &MemoryState, ids: &BTreeMap<u64, u64>, first: usize) {
        assert_eq!(
            (
                actual.live_bytes,
                actual.total_allocated_bytes,
                actual.total_freed_bytes,
                actual.allocations,
                actual.frees,
                actual.unmatched_frees,
                actual.replaced_addresses
            ),
            (
                self.live_bytes,
                self.allocated_bytes,
                self.freed_bytes,
                self.allocations,
                self.frees,
                self.unknown,
                self.replacements
            )
        );
        assert_eq!(actual.live.len(), self.live.len());
        for (&(buffer, address), allocation) in actual.live.iter() {
            let expected = &self.live[&(ids[&buffer], address)];
            allocation_matches(allocation, expected, first);
        }
    }
}

fn allocation_matches(actual: &Allocation, expected: &Allocation, first: usize) {
    assert_eq!(
        (
            actual.address,
            actual.size,
            actual.type_name.as_ref(),
            actual.file.as_ref(),
            actual.line,
            actual.received_frame.checked_add(first)
        ),
        (
            expected.address,
            expected.size,
            expected.type_name.as_ref(),
            expected.file.as_ref(),
            expected.line,
            Some(expected.received_frame)
        )
    );
}

fn optional_allocation_matches(
    actual: &Option<Arc<Allocation>>,
    expected: &Option<Arc<Allocation>>,
    first: usize,
) {
    match (actual, expected) {
        (Some(actual), Some(expected)) => allocation_matches(actual, expected, first),
        (None, None) => (),
        _ => panic!("partial allocation context differs: {actual:?} / {expected:?}"),
    }
}

pub fn verify_partial_interval(
    clipped: &Recording,
    sdk_source: &Recording,
    first: usize,
    last: usize,
    source_ids: &BTreeMap<u64, u64>,
) {
    assert!(first > 0 && first <= last && sdk_source.frame_count() > last);
    assert_eq!(clipped.frame_count(), last - first + 1);
    assert_eq!(
        clipped.memory_history_scope(),
        MemoryHistoryScope::AfterInitialSnapshot
    );
    assert_eq!(
        sdk_source.memory_history_scope(),
        MemoryHistoryScope::CapturedEvents
    );
    let raw_memory = &sdk_source
        .analysis
        .legacy()
        .expect("complete SDK source analysis")
        .memory;
    let memory = &clipped.analysis.legacy().unwrap().memory;
    let selected = raw_memory
        .events()
        .iter()
        .filter(|event| (first..=last).contains(&event.received_frame))
        .collect::<Vec<_>>();
    assert_eq!(memory.events().len(), selected.len());
    let mut expected = Expected::default();
    for (actual, event) in memory.events().iter().zip(&selected) {
        assert_eq!(
            (
                source_ids[&actual.buffer],
                actual.address,
                actual.received_frame + first
            ),
            (event.buffer, event.address, event.received_frame)
        );
        let old = expected.apply(event);
        match (&actual.kind, &event.kind) {
            (
                MemoryEventKind::Allocate {
                    allocation,
                    replaced,
                },
                MemoryEventKind::Allocate {
                    allocation: original,
                    ..
                },
            ) => {
                allocation_matches(allocation, original, first);
                optional_allocation_matches(replaced, &old, first);
            }
            (MemoryEventKind::Free { allocation }, MemoryEventKind::Free { .. }) => {
                optional_allocation_matches(allocation, &old, first)
            }
            _ => panic!("memory event kind changed"),
        }
    }
    expected.verify_state(&memory.state_at_frame(last - first), source_ids, first);
    assert_eq!(memory.peak_bytes, expected.peak_bytes);
    for frame in first..=last {
        let mut expected = Expected::default();
        for event in selected
            .iter()
            .filter(|event| event.received_frame <= frame)
        {
            let _ = expected.apply(event);
        }
        expected.verify_state(&memory.state_at_frame(frame - first), source_ids, first);
    }

    let samples = raw_memory
        .samples
        .iter()
        .filter(|sample| (first..=last).contains(&sample.received_frame))
        .collect::<Vec<_>>();
    assert_eq!(memory.samples.len(), samples.len());
    let mut next = raw_memory
        .events()
        .partition_point(|event| event.received_frame < first);
    let mut expected = Expected::default();
    for (actual, sample) in memory.samples.iter().zip(samples) {
        let end = usize::try_from(sample.allocations + sample.frees).unwrap();
        assert!(end >= next);
        for event in &raw_memory.events()[next..end] {
            let _ = expected.apply(event);
        }
        next = end;
        assert_eq!(
            (
                actual.received_frame + first,
                actual.live_bytes,
                actual.live_allocations,
                actual.allocations,
                actual.frees
            ),
            (
                sample.received_frame,
                expected.live_bytes,
                expected.live.len(),
                expected.allocations,
                expected.frees
            )
        );
    }
}
