use super::*;
#[cfg(test)]
mod tests;
use crate::analysis::view::{
    AllocationView, MemoryAction, MemoryEventView, MemoryRows, MemorySource, MemorySummary,
    OpenProfileEvent, ProfileKey, ProfileMetadataSource, ProfileNames, ProfileSource,
    ProfileSummary,
};

impl ProfileSource for ResidentAnalysis {
    fn profile_summary(&self) -> ProfileSummary {
        ProfileSummary {
            first_retained_frame: self.data.first_retained_frame,
            complete_spans: self.data.spans.len(),
            open_spans: self.data.pending_starts,
            unmatched_stops: self.data.unmatched_stops,
            legacy_cuda_buffers: self.data.legacy_cuda_buffers,
        }
    }
    fn profile_span(&self, index: usize) -> Option<&ProfileSpan> {
        self.data.spans.view().get(index)
    }
    fn profile_spans(&self) -> impl DoubleEndedIterator<Item = &ProfileSpan> + ExactSizeIterator {
        self.data.spans.view().iter()
    }
    fn profile_values(&self) -> impl DoubleEndedIterator<Item = &ProfileValue> + ExactSizeIterator {
        self.data.values.view().iter()
    }
    fn profile_zone_name(&self, zone: u64) -> Option<&str> {
        self.data.zones.get(&zone).map(|zone| &*zone.name)
    }
    fn profile_event_name(&self, zone: u64, event: u16) -> Option<&str> {
        self.data
            .zones
            .get(&zone)?
            .events
            .get(&event)
            .map(|name| &**name)
    }
    fn profile_names(&self) -> impl Iterator<Item = ProfileNames<'_>> {
        self.data.zones.iter().flat_map(|(&zone, data)| {
            data.events.iter().map(move |(&event, name)| ProfileNames {
                zone,
                event,
                zone_name: &data.name,
                event_name: name,
            })
        })
    }
    fn profile_metadata_ambiguous(&self, zone: u64, event: u16) -> bool {
        self.data.ambiguous_zones.0.contains_key(&zone)
            || self.data.ambiguous_events.0.contains_key(&(zone, event))
    }
    fn profile_open_counts(&self) -> impl Iterator<Item = (ProfileKey, usize)> {
        self.data
            .starts
            .iter()
            .filter(|(_, starts)| starts.len() != 0)
            .map(|(&key, starts)| (key, starts.len()))
    }

    fn open_profile_events(&self) -> impl Iterator<Item = OpenProfileEvent> {
        self.data.starts.iter().filter_map(|(&key, starts)| {
            starts
                .view()
                .iter()
                .copied()
                .min()
                .map(|earliest| OpenProfileEvent {
                    key,
                    count: starts.len(),
                    earliest,
                })
        })
    }
    fn profile_open_event(&self, key: ProfileKey) -> Option<OpenProfileEvent> {
        let starts = self.data.starts.get(&key)?;
        starts
            .view()
            .iter()
            .copied()
            .min()
            .map(|earliest| OpenProfileEvent {
                key,
                count: starts.len(),
                earliest,
            })
    }
    fn unmatched_profile_events(&self) -> impl Iterator<Item = (ProfileKey, u64)> {
        self.data
            .unmatched_stop_events
            .iter()
            .map(|(&key, &count)| (key, count))
    }
    fn unmatched_profile_event(&self, key: ProfileKey) -> u64 {
        self.data
            .unmatched_stop_events
            .get(&key)
            .copied()
            .unwrap_or(0)
    }
}

#[derive(Clone, Default)]
pub(crate) struct ResidentMemoryCursor(Option<ResidentCursor>);

impl serde::Serialize for ResidentMemoryCursor {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.0 {
            Some(cursor) => serde::Serialize::serialize(cursor.state(), serializer),
            None => {
                serde::Serialize::serialize(&crate::analysis::MemoryState::default(), serializer)
            }
        }
    }
}

fn allocation(value: &ResidentValue) -> AllocationView<'_> {
    AllocationView {
        address: value.address,
        size: value.size,
        type_name: &value.type_name,
        file: &value.file,
        line: value.line,
        received_frame: value.received_frame,
    }
}

impl MemoryRows for ResidentMemoryCursor {
    fn live_count(&self) -> usize {
        self.0
            .as_ref()
            .map_or(0, |cursor| cursor.state().live.len())
    }
    fn live_bytes(&self) -> u64 {
        self.0
            .as_ref()
            .map_or(0, |cursor| cursor.state().live_bytes)
    }
    fn allocation_at(&self, index: usize) -> Option<AllocationView<'_>> {
        self.0
            .as_ref()?
            .state()
            .live
            .get_index(index)
            .map(|(_, value)| allocation(value))
    }
}

impl MemorySource for ResidentAnalysis {
    type Cursor = ResidentMemoryCursor;
    fn revision(&self) -> u64 {
        self.revision
    }
    fn first_retained_frame(&self) -> usize {
        self.data.first_retained_frame
    }
    fn summary(&self) -> MemorySummary {
        MemorySummary {
            allocations: self.memory().allocations,
            frees: self.memory().frees,
            peak_bytes: self.memory().peak_bytes,
            unmatched_frees: self.memory().unmatched_frees,
            replaced_addresses: self.memory().replaced_addresses,
            live_bytes: self.memory().live_bytes,
            live_allocations: self.memory().live.len(),
            total_allocated_bytes: self.memory().total_allocated_bytes,
        }
    }
    fn seek(&self, cursor: &mut Self::Cursor, frame: usize) -> Result<(), Error> {
        let mut draft = cursor
            .0
            .as_ref()
            .filter(|cursor| cursor.state().0.domain().same(&self.domain))
            .cloned()
            .unwrap_or_else(|| ResidentCursor::new(self.memory()));
        self.memory().try_seek(&mut draft, frame, &mut || false)?;
        cursor.0 = Some(draft);
        Ok(())
    }
    fn event_count(&self) -> usize {
        self.memory().events.len()
    }
    fn event_at(&self, index: usize) -> Option<MemoryEventView<'_>> {
        let event = self.memory().events.view().get(index)?;
        let (action, value) = match &event.kind {
            EventKind::Allocate {
                allocation: value,
                replaced,
            } => (
                if replaced.is_some() {
                    MemoryAction::Replace
                } else {
                    MemoryAction::Allocate
                },
                Some(allocation(value)),
            ),
            EventKind::Free { allocation: value } => {
                (MemoryAction::Free, value.as_ref().map(allocation))
            }
        };
        Some(MemoryEventView {
            buffer: event.buffer,
            address: event.address,
            received_frame: event.received_frame,
            action,
            allocation: value,
        })
    }
    fn frame_events(&self, frame: usize) -> std::ops::Range<usize> {
        let events = self.memory().events.view();
        events.partition_point(|event| event.received_frame < frame)
            ..events.partition_point(|event| event.received_frame <= frame)
    }
    fn churn(&self, frame: usize) -> crate::analysis::MemoryChurn {
        let mut churn = crate::analysis::MemoryChurn::default();
        for event in self
            .memory()
            .events
            .view()
            .range(self.frame_events(frame))
            .expect("validated event range")
        {
            match &event.kind {
                EventKind::Allocate {
                    allocation,
                    replaced,
                } => {
                    churn.allocations += 1;
                    churn.allocated_bytes += allocation.size;
                    churn.surviving_allocations += 1;
                    churn.surviving_bytes += allocation.size;
                    if let Some(replaced) = replaced {
                        churn.replaced_addresses += 1;
                        churn.replaced_bytes += replaced.size;
                        if replaced.received_frame == frame {
                            churn.surviving_allocations -= 1;
                            churn.surviving_bytes -= replaced.size;
                        }
                    }
                }
                EventKind::Free { allocation } => {
                    churn.frees += 1;
                    if let Some(allocation) = allocation {
                        churn.freed_bytes += allocation.size;
                        if allocation.received_frame == frame {
                            churn.surviving_allocations -= 1;
                            churn.surviving_bytes -= allocation.size;
                        }
                    } else {
                        churn.unmatched_frees += 1;
                    }
                }
            }
        }
        churn
    }
    fn sample_count(&self) -> usize {
        self.memory().samples.len()
    }
    fn sample_at(&self, index: usize) -> Option<MemorySample> {
        self.memory().samples.view().get(index).cloned()
    }
}

impl ResidentAnalysis {
    pub(crate) fn semantic_budget_remaining(&self) -> usize {
        self.data.budget.limit.saturating_sub(self.data.budget.used)
    }
}

impl ProfileMetadataSource for ResidentAnalysis {
    fn profile_zones(&self) -> impl Iterator<Item = (u64, &str)> {
        self.data.zones.iter().map(|(&id, zone)| (id, &*zone.name))
    }

    fn profile_ambiguous_zones(&self) -> impl Iterator<Item = u64> {
        self.data.ambiguous_zones.0.iter().map(|(&id, _)| id)
    }

    fn profile_ambiguous_events(&self) -> impl Iterator<Item = (u64, u16)> {
        self.data.ambiguous_events.0.iter().map(|(&key, _)| key)
    }

    fn profile_metadata_budget(&self) -> usize {
        let domain = self.domain.statistics();
        self.semantic_budget_remaining()
            .min(domain.limit.saturating_sub(domain.logical_used))
    }
}
