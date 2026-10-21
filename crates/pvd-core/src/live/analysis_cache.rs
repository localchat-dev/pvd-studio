use super::{Mutex, ResourceLease, WeakResourceLease};
use crate::analysis::resident::Domain;
use crate::analysis::{Analysis, SharedAnalysis};
use std::sync::Arc;

#[cfg(test)]
mod resident_tests;

const MAX_ENTRIES: usize = 32;
const RESIDENT_FRAMES: usize = 120;
const MAX_BYTES: usize = 192 << 20;
const RECENT_CHECKPOINTS: usize = 6;
// A half interval coalesces segment boundaries without merging regular replay checkpoints.
const CHECKPOINT_COVERAGE_FRAMES: usize = super::replay::ANALYSIS_CHECKPOINT_FRAMES / 2;

enum AnchorKind {
    Baseline,
    SegmentReplay,
    Replay,
}

#[derive(Clone, Debug, Default)]
pub(super) struct AnalysisCache(Arc<Mutex<CachedAnalyses>>);

#[derive(Debug)]
struct CachedAnalysis {
    frame: usize,
    data: SharedAnalysis,
    lease: WeakResourceLease,
    baseline: bool,
    used: u64,
    additional_bytes: usize,
}

#[derive(Debug)]
struct CachedAnalyses {
    entries: Vec<CachedAnalysis>,
    data_bytes: usize,
    clock: u64,
    entry_limit: usize,
    resident_limit: usize,
    byte_limit: usize,
}

impl Default for CachedAnalyses {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            data_bytes: 0,
            clock: 0,
            entry_limit: MAX_ENTRIES,
            resident_limit: RESIDENT_FRAMES,
            byte_limit: MAX_BYTES,
        }
    }
}

impl CachedAnalyses {
    fn bytes(&self) -> usize {
        self.data_bytes
            .saturating_add(size_of::<Self>() + 2 * size_of::<usize>())
            .saturating_add(self.entries.capacity() * size_of::<CachedAnalysis>())
    }

    fn refresh(&mut self, position: usize) {
        let Some(entry) = self.entries.get(position) else {
            return;
        };
        let previous = position
            .checked_sub(1)
            .map(|before| &self.entries[before].data);
        let additional = legacy_bytes(&entry.data, previous);
        self.data_bytes = self
            .data_bytes
            .saturating_sub(entry.additional_bytes)
            .saturating_add(additional);
        self.entries[position].additional_bytes = additional;
    }

    fn remove(&mut self, position: usize) {
        let entry = self.entries.remove(position);
        self.data_bytes = self.data_bytes.saturating_sub(entry.additional_bytes);
        self.refresh(position);
    }

    fn store(&mut self, entry: CachedAnalysis) {
        let frame = entry.frame;
        let position = match self
            .entries
            .binary_search_by_key(&frame, |entry| entry.frame)
        {
            Ok(position) => {
                self.data_bytes = self
                    .data_bytes
                    .saturating_sub(self.entries[position].additional_bytes);
                self.entries[position] = entry;
                position
            }
            Err(position) => {
                self.entries.insert(position, entry);
                position
            }
        };
        self.refresh(position);
        self.refresh(position + 1);
        self.trim(frame);
    }

    fn eviction_position(&self, protected: usize) -> usize {
        let checkpoint = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.frame != protected && !entry.baseline)
            .min_by_key(|(_, entry)| entry.used)
            .map(|(position, _)| position);
        if self.entries.iter().filter(|entry| !entry.baseline).count() > RECENT_CHECKPOINTS
            && let Some(position) = checkpoint
        {
            return position;
        }
        (1..self.entries.len().saturating_sub(1))
            .filter(|&position| {
                self.entries[position].frame != protected && self.entries[position].baseline
            })
            .min_by_key(|&position| {
                (
                    self.entries[position + 1].frame - self.entries[position - 1].frame,
                    self.entries[position].used,
                )
            })
            .or(checkpoint)
            .or_else(|| {
                self.entries
                    .iter()
                    .position(|entry| entry.frame != protected)
            })
            .unwrap_or(0)
    }

    fn discard_expired(&mut self) {
        for position in (0..self.entries.len()).rev() {
            if !self.entries[position].lease.available() {
                self.remove(position);
            }
        }
        self.trim(self.entries.last().map_or(0, |entry| entry.frame));
    }

    fn trim(&mut self, protected: usize) {
        loop {
            let residents = self
                .entries
                .iter()
                .filter(|entry| entry.data.resident().is_some())
                .count();
            let legacy = self.entries.len() - residents;
            let resident_overflow = residents > self.resident_limit;
            let legacy_overflow = legacy > self.entry_limit;
            if self.entries.is_empty()
                || (!resident_overflow && !legacy_overflow && self.bytes() <= self.byte_limit)
            {
                break;
            }
            let position = if resident_overflow || (legacy == 0 && residents != 0) {
                self.resident_eviction(protected).unwrap()
            } else if residents == 0 {
                self.eviction_position(protected)
            } else {
                self.entries
                    .iter()
                    .enumerate()
                    .filter(|(_, entry)| entry.data.legacy().is_some() && entry.frame != protected)
                    .min_by_key(|(_, entry)| entry.used)
                    .or_else(|| {
                        self.entries
                            .iter()
                            .enumerate()
                            .find(|(_, entry)| entry.data.legacy().is_some())
                    })
                    .map(|(position, _)| position)
                    .unwrap_or_else(|| self.resident_eviction(protected).unwrap())
            };
            self.remove(position);
        }
    }

    fn resident_eviction(&self, protected: usize) -> Option<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.data.resident().is_some() && entry.frame != protected)
            .min_by_key(|(_, entry)| entry.used)
            .or_else(|| {
                self.entries
                    .iter()
                    .enumerate()
                    .find(|(_, entry)| entry.data.resident().is_some())
            })
            .map(|(position, _)| position)
    }
}

fn legacy_bytes(data: &SharedAnalysis, previous: Option<&SharedAnalysis>) -> usize {
    let Some(analysis) = data.legacy() else {
        // Resident payloads are charged once by their shared Domain, including roots held by views.
        return 0;
    };
    match previous.and_then(SharedAnalysis::legacy) {
        Some(previous) if std::ptr::eq(analysis, previous) => 0,
        Some(previous) => analysis.additional_cache_bytes(previous),
        None => analysis
            .accounted_bytes()
            .saturating_add(size_of::<Analysis>() + 16),
    }
}

fn compact_snapshot(data: SharedAnalysis) -> SharedAnalysis {
    let Some(analysis) = data.legacy() else {
        return data;
    };
    if analysis.spans.len() == analysis.spans.capacity()
        && analysis.values.len() == analysis.values.capacity()
        && analysis.memory.samples.len() == analysis.memory.samples.capacity()
    {
        return data;
    }
    let mut analysis = analysis.clone();
    analysis.memory = Arc::new((*analysis.memory).clone());
    analysis.into()
}

impl AnalysisCache {
    pub(super) fn insert(&self, frame: usize, data: SharedAnalysis, lease: &ResourceLease) {
        self.insert_kind(frame, data, lease, AnchorKind::Baseline);
    }

    pub(super) fn insert_checkpoint(
        &self,
        frame: usize,
        data: SharedAnalysis,
        lease: &ResourceLease,
    ) {
        self.insert_kind(frame, data, lease, AnchorKind::Replay);
    }

    pub(super) fn insert_segment_checkpoint(
        &self,
        frame: usize,
        data: SharedAnalysis,
        lease: &ResourceLease,
    ) {
        self.insert_kind(frame, data, lease, AnchorKind::SegmentReplay);
    }

    fn insert_kind(
        &self,
        frame: usize,
        data: SharedAnalysis,
        lease: &ResourceLease,
        kind: AnchorKind,
    ) {
        let baseline = !matches!(kind, AnchorKind::Replay);
        if let Ok(mut cache) = self.0.lock() {
            cache.discard_expired();
            if !lease.available() || legacy_bytes(&data, None) > cache.byte_limit {
                return;
            }
            cache.clock = cache.clock.saturating_add(1);
            let used = cache.clock;
            // Committed analyses are immutable; baselines and replay anchors name the completed frame.
            let position = match cache
                .entries
                .binary_search_by_key(&frame, |entry| entry.frame)
            {
                Ok(position) => {
                    let entry = &mut cache.entries[position];
                    entry.used = used;
                    entry.baseline |= baseline;
                    return;
                }
                Err(position) => position,
            };
            // Nearby analysis already bounds replay; another snapshot duplicates retained vectors.
            if data.legacy().is_some()
                && !matches!(kind, AnchorKind::Baseline)
                && position.checked_sub(1).is_some_and(|before| {
                    frame - cache.entries[before].frame <= CHECKPOINT_COVERAGE_FRAMES
                })
            {
                return;
            }
            let entry = CachedAnalysis {
                frame,
                data: compact_snapshot(data),
                lease: lease.downgrade(),
                baseline,
                used,
                additional_bytes: 0,
            };
            cache.store(entry);
        }
    }

    pub(super) fn best(&self, first: usize, last: usize) -> Option<(usize, SharedAnalysis)> {
        let mut cache = self.0.lock().ok()?;
        cache.discard_expired();
        let position = cache
            .entries
            .iter()
            .rposition(|entry| entry.frame >= first && entry.frame <= last)?;
        cache.clock = cache.clock.saturating_add(1);
        let used = cache.clock;
        let entry = &mut cache.entries[position];
        entry.used = used;
        Some((entry.frame, entry.data.clone()))
    }

    pub(super) fn discard_expired(&self) {
        if let Ok(mut cache) = self.0.lock() {
            cache.discard_expired();
        }
    }

    pub(super) fn reclaim_resident(&self, domain: &Domain) -> usize {
        let Ok(mut cache) = self.0.lock() else {
            return 0;
        };
        let mut removed = 0;
        for position in (0..cache.entries.len()).rev() {
            if cache.entries[position]
                .data
                .resident()
                .is_some_and(|root| root.domain().same(domain))
            {
                cache.remove(position);
                removed += 1;
            }
        }
        removed
    }

    pub(super) fn clear_resident(&self) -> usize {
        let Ok(mut cache) = self.0.lock() else {
            return 0;
        };
        let mut removed = 0;
        for position in (0..cache.entries.len()).rev() {
            if cache.entries[position].data.resident().is_some() {
                cache.remove(position);
                removed += 1;
            }
        }
        removed
    }

    #[cfg(test)]
    pub(super) fn with_limits(entry_limit: usize, byte_limit: usize) -> Self {
        Self(Arc::new(Mutex::new(CachedAnalyses {
            entry_limit,
            resident_limit: entry_limit,
            byte_limit,
            ..Default::default()
        })))
    }

    pub(super) fn stats(&self) -> (usize, usize) {
        self.0
            .lock()
            .map_or((0, 0), |cache| (cache.entries.len(), cache.bytes()))
    }

    pub(super) fn bytes(&self) -> usize {
        self.stats().1
    }

    #[cfg(test)]
    pub(super) fn describe(&self) -> Vec<serde_json::Value> {
        self.0.lock().map_or_else(
            |_| Vec::new(),
            |cache| {
                cache
                    .entries
                    .iter()
                    .enumerate()
                    .map(|(position, entry)| {
                        use crate::analysis::view::MemorySource;
                        let legacy = entry.data.legacy();
                        let memory = legacy.map(|data| &data.memory);
                        let before = position.checked_sub(1)
                            .and_then(|position| cache.entries[position].data.legacy())
                            .map(|data| &data.memory);
                        let common = memory.zip(before).is_some_and(|(memory, before)| {
                            memory.events().first().zip(before.events().first()).is_some_and(|(a, b)| Arc::ptr_eq(a, b))
                                && before.events().last().zip(memory.events().get(before.events().len().saturating_sub(1))).is_some_and(|(a, b)| Arc::ptr_eq(a, b))
                        });
                        let snapshot = entry.data.memory_snapshot();
                        serde_json::json!({
                            "frame": entry.frame,
                            "charge": entry.additional_bytes,
                            "accounted": entry.data.accounted_bytes(),
                            "baseline": entry.baseline,
                            "first": entry.data.first_retained_frame(),
                            "live": snapshot.summary().live_allocations,
                            "live_extra": memory.zip(before).map(|(memory, before)| memory.live.additional_bytes(&before.live)),
                            "events": snapshot.event_count(),
                            "events_common": common,
                            "samples_capacity": memory.map(|memory| memory.samples.capacity()),
                            "spans_capacity": legacy.map(|data| data.spans.capacity()),
                            "values_capacity": legacy.map(|data| data.values.capacity()),
                            "resident_domain_bytes": entry.data.resident().map(|root| root.domain().statistics().logical_used),
                        })
                    })
                    .collect()
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::Allocation;

    fn legacy_fixture(count: usize) -> Analysis {
        let mut analysis = Analysis::default();
        let memory = Arc::make_mut(&mut analysis.memory);
        for address in 0..count as u64 {
            memory.live.insert(
                (1, address),
                Arc::new(Allocation {
                    address,
                    size: 1,
                    type_name: "resident".into(),
                    file: "fixture.cpp".into(),
                    line: 1,
                    received_frame: 0,
                }),
            );
        }
        analysis.retain_recent(0);
        analysis
    }

    fn legacy_allocations(count: usize) -> SharedAnalysis {
        legacy_fixture(count).into()
    }

    fn weak_fixture(count: usize) -> (SharedAnalysis, std::sync::Weak<Analysis>) {
        let data = Arc::new(legacy_fixture(count));
        let weak = Arc::downgrade(&data);
        (data.into(), weak)
    }

    fn assert_accounting(cache: &AnalysisCache) {
        let values = cache.0.lock().unwrap();
        let mut previous: Option<&SharedAnalysis> = None;
        let expected = values.entries.iter().fold(
            size_of::<CachedAnalyses>()
                + 2 * size_of::<usize>()
                + values.entries.capacity() * size_of::<CachedAnalysis>(),
            |bytes, entry| {
                let additional = previous.map_or_else(
                    || entry.data.accounted_bytes() + size_of::<Analysis>() + 16,
                    |before| {
                        let current = entry.data.legacy().unwrap();
                        let before = before.legacy().unwrap();
                        if std::ptr::eq(current, before) {
                            0
                        } else {
                            current.additional_cache_bytes(before)
                        }
                    },
                );
                previous = Some(&entry.data);
                bytes + additional
            },
        );
        assert_eq!(values.bytes(), expected);
        assert!(values.bytes() <= values.byte_limit);
        assert!(values.entries.len() <= values.entry_limit);
        assert!(
            values
                .entries
                .windows(2)
                .all(|pair| pair[0].frame < pair[1].frame)
        );
    }

    fn replace(cache: &AnalysisCache, frame: usize, data: SharedAnalysis, lease: &ResourceLease) {
        cache.0.lock().unwrap().store(CachedAnalysis {
            frame,
            data,
            lease: lease.downgrade(),
            baseline: true,
            used: 0,
            additional_bytes: 0,
        });
    }

    #[test]
    fn entry_limit_keeps_sparse_coverage_and_shares_unchanged_allocations() {
        let cache = AnalysisCache::default();
        let lease = ResourceLease::new(Vec::new());
        let data = legacy_allocations(4096);
        let before = data.accounted_bytes();
        for frame in 0..MAX_ENTRIES + 5 {
            cache.insert(frame, data.clone(), &lease);
        }
        assert_eq!(cache.stats().0, MAX_ENTRIES);
        assert!(cache.stats().1 < before + (64 << 10));
        let earliest = cache.best(0, 0).unwrap().1;
        assert!(std::ptr::eq(
            data.legacy().unwrap(),
            earliest.legacy().unwrap()
        ));
        cache.insert(MAX_ENTRIES + 5, data.clone(), &lease);
        assert!(cache.best(0, 0).is_some());
        assert!(cache.best(MAX_ENTRIES + 5, MAX_ENTRIES + 5).is_some());
        assert_eq!(cache.stats().0, MAX_ENTRIES);
        assert!(cache.stats().1 <= MAX_BYTES);
    }

    #[test]
    fn distinct_legacy_states_obey_byte_limit_and_release_evicted_data() {
        let cache = AnalysisCache::default();
        let lease = ResourceLease::new(Vec::new());
        let (first, first_weak) = weak_fixture(2048);
        let second = legacy_allocations(2048);
        let limit = first.accounted_bytes() + (16 << 10);
        cache.0.lock().unwrap().byte_limit = limit;
        cache.insert(0, first, &lease);
        assert!(first_weak.upgrade().is_some());
        cache.insert(1, second, &lease);
        assert_eq!(cache.stats().0, 1);
        assert!(cache.stats().1 <= limit);
        assert!(first_weak.upgrade().is_none());
        assert!(cache.best(0, 0).is_none());
        assert!(cache.best(1, 1).is_some());
    }

    #[test]
    fn invalidated_or_dropped_leases_release_cached_analyses() {
        let cache = AnalysisCache::default();
        let lease = ResourceLease::new(Vec::new());
        let (data, data_weak) = weak_fixture(128);
        cache.insert(5, data, &lease);
        assert_eq!(Arc::strong_count(&lease.0), 1);
        lease.invalidate().unwrap();
        cache.discard_expired();
        assert_eq!(cache.stats().0, 0);
        assert!(data_weak.upgrade().is_none());
        cache.insert(6, legacy_allocations(1), &lease);
        assert_eq!(cache.stats().0, 0);
        let lease = ResourceLease::new(Vec::new());
        let weak_lease = lease.downgrade();
        cache.insert(7, legacy_allocations(1), &lease);
        drop(lease);
        assert!(!weak_lease.available());
        assert!(cache.best(7, 7).is_none());
        assert_eq!(cache.stats().0, 0);
    }

    #[test]
    fn sorted_accounting_is_stable_across_recent_access_and_out_of_order_insertions() {
        let cache = AnalysisCache::default();
        let lease = ResourceLease::new(Vec::new());
        let mut data = legacy_allocations(128);
        cache.insert(0, data.clone(), &lease);
        data.add_zone(1, "zone".into()).unwrap();
        cache.insert(2, data.clone(), &lease);
        data.add_event(1, 7, "event".into()).unwrap();
        cache.insert(1, data.clone(), &lease);
        let bytes = cache.stats().1;
        for frame in [2, 0, 1, 0] {
            cache.best(frame, frame).unwrap();
        }
        let mut values = cache.0.lock().unwrap();
        for position in 0..values.entries.len() {
            values.refresh(position);
        }
        drop(values);
        assert_eq!(cache.stats().1, bytes);
        let cached = cache.best(1, 1).unwrap().1;
        data.add_zone(2, "later".into()).unwrap();
        assert!(!cached.legacy().unwrap().zones.contains_key(&2));
    }

    #[test]
    fn incremental_costs_match_full_accounting_when_replacing_and_removing_any_position() {
        let cache = AnalysisCache::default();
        let lease = ResourceLease::new(Vec::new());
        let first = legacy_allocations(128);
        let held = first.clone();
        let mut second = first.clone();
        Arc::make_mut(&mut second.legacy_mut().unwrap().memory)
            .live
            .remove(&(1, 7));
        second.retain_recent(0).unwrap();
        let mut third = second.clone();
        third.add_zone(3, "third".into()).unwrap();
        for (frame, data) in [
            (0, first.clone()),
            (4, third.clone()),
            (2, second),
            (6, first),
        ] {
            cache.insert(frame, data, &lease);
            assert_accounting(&cache);
        }
        let before = cache.stats().1;
        replace(&cache, 2, held.clone(), &lease);
        assert_accounting(&cache);
        assert!(cache.stats().1 <= before);
        replace(&cache, 2, third, &lease);
        assert_accounting(&cache);
        for position in [0, 1, 1, 0] {
            cache.0.lock().unwrap().remove(position);
            assert_accounting(&cache);
        }
        assert!(held.legacy().unwrap().memory.live.get(&(1, 7)).is_some());
    }

    #[test]
    fn recent_checkpoints_share_the_budget_with_sparse_native_baselines() {
        let cache = AnalysisCache::default();
        let lease = ResourceLease::new(Vec::new());
        let data = legacy_allocations(64);
        for frame in (0..100).step_by(2) {
            cache.insert(frame, data.clone(), &lease);
        }
        for frame in (400..3400).step_by(300) {
            cache.insert_checkpoint(frame, data.clone(), &lease);
            assert_accounting(&cache);
        }
        let values = cache.0.lock().unwrap();
        assert_eq!(
            values
                .entries
                .iter()
                .filter(|entry| !entry.baseline)
                .count(),
            RECENT_CHECKPOINTS
        );
        assert_eq!(values.entries.first().unwrap().frame, 0);
        assert_eq!(values.entries.last().unwrap().frame, 3100);
        drop(values);
        lease.invalidate().unwrap();
        cache.discard_expired();
        assert_accounting(&cache);
    }

    #[test]
    fn nearby_checkpoints_skip_allocation_and_preserve_native_coverage() {
        let cache = AnalysisCache::default();
        let lease = ResourceLease::new(Vec::new());
        let mut data = legacy_allocations(64);
        data.legacy_mut().unwrap().spans.reserve_exact(4096);
        cache.insert(0, data.clone(), &lease);
        let stats = cache.stats();
        let published = data.clone();
        cache.insert_checkpoint(CHECKPOINT_COVERAGE_FRAMES, data.clone(), &lease);
        assert_eq!(cache.stats(), stats);
        assert!(cache.best(0, 0).is_some());
        assert!(
            cache
                .best(CHECKPOINT_COVERAGE_FRAMES, CHECKPOINT_COVERAGE_FRAMES)
                .is_none()
        );
        assert_eq!(published.legacy().unwrap().spans.capacity(), 4096);
        cache.insert_checkpoint(CHECKPOINT_COVERAGE_FRAMES + 1, data.clone(), &lease);
        assert_eq!(cache.stats().0, 2);
        cache.insert(CHECKPOINT_COVERAGE_FRAMES + 2, data, &lease);
        assert_eq!(cache.stats().0, 3);
        assert_accounting(&cache);
    }

    #[test]
    fn expired_coverage_does_not_reject_a_checkpoint_for_a_valid_lease() {
        let cache = AnalysisCache::default();
        let expired = ResourceLease::new(Vec::new());
        cache.insert(0, legacy_allocations(64), &expired);
        expired.invalidate().unwrap();
        let valid = ResourceLease::new(Vec::new());
        cache.insert_checkpoint(1, legacy_allocations(64), &valid);
        assert_eq!(cache.stats().0, 1);
        assert!(cache.best(1, 1).is_some());
        assert_accounting(&cache);
    }

    #[test]
    fn compact_anchors_release_spare_capacity_without_mutating_published_analysis() {
        let cache = AnalysisCache::default();
        let lease = ResourceLease::new(Vec::new());
        let mut data = legacy_allocations(128);
        data.legacy_mut().unwrap().spans.reserve_exact(4096);
        data.legacy_mut()
            .unwrap()
            .spans
            .push(crate::analysis::ProfileSpan {
                zone: 1,
                event: 8,
                thread: 9,
                context: 10,
                start: 11,
                end: 12,
                received_frame: 0,
            });
        Arc::make_mut(&mut data.legacy_mut().unwrap().memory)
            .samples
            .reserve_exact(4096);
        Arc::make_mut(&mut data.legacy_mut().unwrap().memory)
            .samples
            .push(crate::analysis::MemorySample::default());
        let published = data.clone();
        let before = postcard::to_stdvec(&published).unwrap();
        cache.insert(0, data.clone(), &lease);
        let cached = cache.best(0, 0).unwrap().1;
        assert_eq!(
            cached.legacy().unwrap().spans.capacity(),
            cached.legacy().unwrap().spans.len()
        );
        assert_eq!(
            cached.legacy().unwrap().memory.samples.capacity(),
            cached.legacy().unwrap().memory.samples.len()
        );
        assert_eq!(
            cached
                .legacy()
                .unwrap()
                .memory
                .live
                .additional_bytes(&published.legacy().unwrap().memory.live),
            0
        );
        assert_eq!(postcard::to_stdvec(&cached).unwrap(), before);
        assert_eq!(published.legacy().unwrap().spans.capacity(), 4096);
        assert_eq!(published.legacy().unwrap().memory.samples.capacity(), 4096);
        data.legacy_mut().unwrap().spans.clear();
        cache.insert(0, data, &lease);
        assert_eq!(cache.best(0, 0).unwrap().1.legacy().unwrap().spans.len(), 1);
        assert_eq!(postcard::to_stdvec(&published).unwrap(), before);
        assert_accounting(&cache);
    }
}
