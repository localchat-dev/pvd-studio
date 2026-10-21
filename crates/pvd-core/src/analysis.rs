pub(crate) mod resident;
use crate::{Error, error};
use pvd_protocol::Endian;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

mod allocation_map;
#[cfg(test)]
mod sharing_tests;
pub mod view;
pub use allocation_map::AllocationMap;

static ANALYSIS_REVISION: AtomicU64 = AtomicU64::new(1);

mod shared;
pub use shared::SharedAnalysis;

const MAX_RECORDS: usize = 2_000_000;
const MAX_ANALYSIS_BYTES: usize = 128 * 1024 * 1024;
const MAP_ENTRY_BYTES: usize = 96;
const LIVE_ALLOCATION_ENTRY_BYTES: usize =
    MAP_ENTRY_BYTES + std::mem::size_of::<(u64, u64)>() + std::mem::size_of::<Arc<Allocation>>();
const MEMORY_EVENT_BYTES: usize = std::mem::size_of::<MemoryEvent>()
    + 2 * std::mem::size_of::<usize>()
    + 2 * std::mem::size_of::<Arc<MemoryEvent>>();

type ExportDrain<'a> = Option<&'a mut dyn FnMut(&mut Analysis) -> Result<(), Error>>;

#[derive(Clone, Debug, Default)]
pub(crate) struct RetentionSchedule {
    retained_bytes: usize,
}

impl RetentionSchedule {
    #[cfg(test)]
    pub(crate) fn after_retention(analysis: &Analysis) -> Self {
        Self {
            retained_bytes: analysis.accounted_bytes(),
        }
    }

    #[cfg(test)]
    fn cutoff(&self, analysis: &Analysis, frame: usize) -> Option<usize> {
        self.cutoff_usage(
            analysis.accounted_bytes(),
            analysis.budget.limit,
            analysis.first_retained_frame,
            frame,
        )
    }

    fn cutoff_usage(
        &self,
        accounted_bytes: usize,
        limit: usize,
        first_retained_frame: usize,
        frame: usize,
    ) -> Option<usize> {
        let available = limit.saturating_sub(self.retained_bytes);
        let growth_budget = (available / 2).clamp(1, 32 << 20);
        // Live allocations and metadata can exceed the pressure window by themselves.
        // Only growth since a completed sweep can justify another pressure sweep.
        if accounted_bytes.saturating_sub(self.retained_bytes) >= growth_budget {
            return Some(frame.saturating_sub(60));
        }
        let cutoff = frame.saturating_sub(600);
        (frame.is_multiple_of(60) && cutoff > first_retained_frame).then_some(cutoff)
    }

    #[cfg(test)]
    pub(crate) fn maintain(&mut self, analysis: &mut Analysis, frame: usize) -> bool {
        let Some(cutoff) = self.cutoff(analysis, frame) else {
            return false;
        };
        analysis.retain_recent(cutoff);
        self.retained_bytes = analysis.accounted_bytes();
        true
    }
}

fn retained_string_bytes(value: &Arc<str>, strings: &mut BTreeSet<usize>) -> usize {
    if strings.insert(Arc::as_ptr(value) as *const u8 as usize) {
        value.len() + 2 * size_of::<usize>()
    } else {
        0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Budget {
    used: usize,
    limit: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            used: 0,
            limit: MAX_ANALYSIS_BYTES,
        }
    }
}

impl Budget {
    fn charge(&mut self, bytes: usize) -> Result<(), Error> {
        let used = self
            .used
            .checked_add(bytes)
            .ok_or_else(|| error("analysis byte count overflow"))?;
        if used > self.limit {
            return Err(error(format!(
                "analysis exceeds its {} byte allocation budget",
                self.limit
            )));
        }
        self.used = used;
        Ok(())
    }

    fn release(&mut self, bytes: usize) {
        self.used = self.used.saturating_sub(bytes);
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProfileZone {
    pub name: String,
    pub events: BTreeMap<u16, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProfileSpan {
    pub zone: u64,
    pub event: u16,
    pub thread: u32,
    pub context: u64,
    pub start: u64,
    pub end: u64,
    pub received_frame: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProfileValue {
    pub zone: u64,
    pub event: u16,
    pub thread: u32,
    pub context: u64,
    pub raw_value: u64,
    pub received_frame: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Allocation {
    pub address: u64,
    pub size: u64,
    pub type_name: Arc<str>,
    pub file: Arc<str>,
    pub line: u32,
    pub received_frame: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryEventKind {
    Allocate {
        allocation: Arc<Allocation>,
        replaced: Option<Arc<Allocation>>,
    },
    Free {
        allocation: Option<Arc<Allocation>>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEvent {
    pub buffer: u64,
    pub address: u64,
    pub received_frame: usize,
    pub kind: MemoryEventKind,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryState {
    pub live: AllocationMap,
    pub live_bytes: u64,
    pub total_allocated_bytes: u64,
    pub total_freed_bytes: u64,
    pub allocations: u64,
    pub frees: u64,
    pub unmatched_frees: u64,
    pub replaced_addresses: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryChurn {
    pub allocations: u64,
    pub frees: u64,
    pub allocated_bytes: u64,
    pub freed_bytes: u64,
    pub replaced_bytes: u64,
    pub unmatched_frees: u64,
    pub replaced_addresses: u64,
}

impl MemoryChurn {
    pub fn live_byte_change(&self) -> i128 {
        i128::from(self.allocated_bytes)
            - i128::from(self.freed_bytes)
            - i128::from(self.replaced_bytes)
    }
}

#[derive(Clone, Debug, Default)]
pub struct MemoryCursor {
    state: MemoryState,
    position: usize,
    anchor: Option<Arc<MemoryEvent>>,
    base: Option<Arc<MemoryState>>,
}

impl MemoryCursor {
    pub fn state(&self) -> &MemoryState {
        &self.state
    }
}

impl MemoryState {
    fn apply(&mut self, event: &MemoryEvent) {
        let key = (event.buffer, event.address);
        match &event.kind {
            MemoryEventKind::Allocate {
                allocation,
                replaced,
            } => {
                if let Some(replaced) = replaced {
                    self.live_bytes -= replaced.size;
                    self.replaced_addresses += 1;
                }
                self.live.insert(key, allocation.clone());
                self.live_bytes += allocation.size;
                self.total_allocated_bytes += allocation.size;
                self.allocations += 1;
            }
            MemoryEventKind::Free { allocation } => {
                if let Some(allocation) = allocation {
                    self.live.remove(&key);
                    self.live_bytes -= allocation.size;
                    self.total_freed_bytes += allocation.size;
                } else {
                    self.unmatched_frees += 1;
                }
                self.frees += 1;
            }
        }
    }

    fn revert(&mut self, event: &MemoryEvent) {
        let key = (event.buffer, event.address);
        match &event.kind {
            MemoryEventKind::Allocate {
                allocation,
                replaced,
            } => {
                self.live_bytes -= allocation.size;
                self.total_allocated_bytes -= allocation.size;
                self.allocations -= 1;
                if let Some(replaced) = replaced {
                    self.live.insert(key, replaced.clone());
                    self.live_bytes += replaced.size;
                    self.replaced_addresses -= 1;
                } else {
                    self.live.remove(&key);
                }
            }
            MemoryEventKind::Free { allocation } => {
                if let Some(allocation) = allocation {
                    self.live.insert(key, allocation.clone());
                    self.live_bytes += allocation.size;
                    self.total_freed_bytes -= allocation.size;
                } else {
                    self.unmatched_frees -= 1;
                }
                self.frees -= 1;
            }
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MemorySample {
    pub received_frame: usize,
    pub live_bytes: u64,
    pub live_allocations: usize,
    pub allocations: u64,
    pub frees: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MemoryAnalysis {
    pub live: AllocationMap,
    pub samples: Vec<MemorySample>,
    pub live_bytes: u64,
    pub peak_bytes: u64,
    pub total_allocated_bytes: u64,
    pub allocations: u64,
    pub frees: u64,
    pub unmatched_frees: u64,
    pub replaced_addresses: u64,
    strings: BTreeMap<(u64, u32), Arc<str>>,
    events: Vec<Arc<MemoryEvent>>,
    base: Arc<MemoryState>,
    first_retained_frame: usize,
}

impl MemoryAnalysis {
    pub fn events(&self) -> &[Arc<MemoryEvent>] {
        &self.events
    }

    pub fn events_at_frame(&self, received_frame: usize) -> &[Arc<MemoryEvent>] {
        let start = self
            .events
            .partition_point(|event| event.received_frame < received_frame);
        let end = self
            .events
            .partition_point(|event| event.received_frame <= received_frame);
        &self.events[start..end]
    }

    pub fn state_at_frame(&self, received_frame: usize) -> MemoryState {
        let received_frame = received_frame.max(self.first_retained_frame);
        let mut state = self.base.as_ref().clone();
        for event in &self.events[..self
            .events
            .partition_point(|event| event.received_frame <= received_frame)]
        {
            state.apply(event);
        }
        state
    }

    pub fn seek<'a>(&self, cursor: &'a mut MemoryCursor, received_frame: usize) -> &'a MemoryState {
        let received_frame = received_frame.max(self.first_retained_frame);
        let end = self
            .events
            .partition_point(|event| event.received_frame <= received_frame);
        let same_anchor = match (&cursor.anchor, cursor.position.checked_sub(1)) {
            (None, None) => true,
            (Some(anchor), Some(index)) => self
                .events
                .get(index)
                .is_some_and(|event| Arc::ptr_eq(anchor, event)),
            _ => false,
        };
        let same_base = cursor
            .base
            .as_ref()
            .is_some_and(|base| Arc::ptr_eq(base, &self.base));
        if !same_base || !same_anchor || end < cursor.position.saturating_sub(end) {
            *cursor = MemoryCursor {
                state: self.base.as_ref().clone(),
                base: Some(self.base.clone()),
                ..Default::default()
            };
        }
        while cursor.position > end {
            cursor.position -= 1;
            cursor.state.revert(&self.events[cursor.position]);
        }
        for event in &self.events[cursor.position..end] {
            cursor.state.apply(event);
        }
        cursor.position = end;
        cursor.anchor = end.checked_sub(1).map(|index| self.events[index].clone());
        &cursor.state
    }

    fn retain_recent(&mut self, first_frame: usize) {
        let end = self
            .events
            .partition_point(|event| event.received_frame < first_frame);
        if end != 0 {
            let base = Arc::make_mut(&mut self.base);
            for event in &self.events[..end] {
                base.apply(event);
            }
            self.events.drain(..end);
        }
        self.events.shrink_to_fit();
        self.samples
            .retain(|sample| sample.received_frame >= first_frame);
        self.samples.shrink_to_fit();
        self.first_retained_frame = first_frame;
    }

    pub fn churn_at_frame(&self, received_frame: usize) -> MemoryChurn {
        let mut churn = MemoryChurn::default();
        for event in self.events_at_frame(received_frame) {
            match &event.kind {
                MemoryEventKind::Allocate {
                    allocation,
                    replaced,
                } => {
                    churn.allocations += 1;
                    churn.allocated_bytes += allocation.size;
                    if let Some(replaced) = replaced {
                        churn.replaced_addresses += 1;
                        churn.replaced_bytes += replaced.size;
                    }
                }
                MemoryEventKind::Free { allocation } => {
                    churn.frees += 1;
                    if let Some(allocation) = allocation {
                        churn.freed_bytes += allocation.size;
                    } else {
                        churn.unmatched_frees += 1;
                    }
                }
            }
        }
        churn
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Analysis {
    pub first_retained_frame: usize,
    pub zones: BTreeMap<u64, ProfileZone>,
    pub spans: Vec<ProfileSpan>,
    pub values: Vec<ProfileValue>,
    pub unmatched_stops: u64,
    pub unmatched_stop_events: BTreeMap<(u64, u16, u32, u64), u64>,
    pub legacy_cuda_buffers: u64,
    pub memory: Arc<MemoryAnalysis>,
    defined_zones: BTreeSet<u64>,
    ambiguous_zones: BTreeSet<u64>,
    ambiguous_events: BTreeSet<(u64, u16)>,
    starts: BTreeMap<(u64, u16, u32, u64), Vec<u64>>,
    pending_starts: usize,
    budget: Budget,
}

impl Analysis {
    pub(crate) fn discard_completed_exports(&mut self, recount: bool) {
        self.spans.clear();
        self.values.clear();
        let memory = Arc::make_mut(&mut self.memory);
        memory.events.clear();
        memory.samples.clear();
        if recount {
            self.budget.used = self.retained_bytes();
        }
    }

    pub(crate) fn export_metadata_budget(&self) -> usize {
        self.budget.limit.saturating_sub(self.budget.used)
    }

    pub(crate) fn ingest_for_export(
        &mut self,
        id: u64,
        type_name: &str,
        data: &[u8],
        endian: Endian,
        frame: usize,
        drain: &mut dyn FnMut(&mut Analysis) -> Result<(), Error>,
    ) -> Result<(), Error> {
        if type_name.ends_with("PxProfileZone") {
            self.profile_buffer_with_drain(id, data, endian, frame, Some(drain))
        } else if type_name.ends_with("PxProfileMemoryEventBuffer") {
            self.memory_buffer_with_resolver(id, data, endian, frame, None, Some(drain))
        } else {
            Ok(())
        }
    }

    pub(crate) fn retain_captured(
        &mut self,
        first_frame: usize,
        accounted_bytes: usize,
    ) -> Result<(), Error> {
        if accounted_bytes > self.budget.limit {
            return Err(error("captured analysis accounting exceeds its budget"));
        }
        self.retain_recent(first_frame);
        self.budget.used = accounted_bytes;
        Ok(())
    }

    pub(crate) fn restore_sharing(&mut self) {
        use std::collections::HashMap;
        fn intern(value: &mut Arc<str>, strings: &mut HashMap<Arc<str>, Arc<str>>) {
            *value = strings
                .entry(value.clone())
                .or_insert_with(|| value.clone())
                .clone();
        }
        fn allocation(
            value: &mut Arc<Allocation>,
            strings: &mut HashMap<Arc<str>, Arc<str>>,
            allocations: &mut HashMap<Allocation, Arc<Allocation>>,
        ) {
            let entry = Arc::make_mut(value);
            intern(&mut entry.type_name, strings);
            intern(&mut entry.file, strings);
            *value = allocations
                .entry(entry.clone())
                .or_insert_with(|| value.clone())
                .clone();
        }
        fn reuse_allocation(
            value: &mut Arc<Allocation>,
            strings: &mut HashMap<Arc<str>, Arc<str>>,
            allocations: &mut HashMap<Allocation, Arc<Allocation>>,
        ) {
            if let Some(shared) = allocations.get(value.as_ref()) {
                *value = shared.clone();
            } else {
                allocation(value, strings, allocations);
            }
        }
        let mut strings = HashMap::new();
        let mut allocations = HashMap::new();
        let memory = Arc::make_mut(&mut self.memory);
        for value in memory.strings.values_mut() {
            intern(value, &mut strings);
        }
        let live = memory.live.values_mut();
        let base = Arc::make_mut(&mut memory.base).live.values_mut();
        for value in live {
            allocation(value, &mut strings, &mut allocations);
        }
        for value in base {
            reuse_allocation(value, &mut strings, &mut allocations);
        }
        for event in &mut memory.events {
            match &mut Arc::make_mut(event).kind {
                MemoryEventKind::Allocate {
                    allocation: value,
                    replaced,
                } => {
                    reuse_allocation(value, &mut strings, &mut allocations);
                    if let Some(value) = replaced {
                        reuse_allocation(value, &mut strings, &mut allocations);
                    }
                }
                MemoryEventKind::Free {
                    allocation: Some(value),
                } => reuse_allocation(value, &mut strings, &mut allocations),
                MemoryEventKind::Free { allocation: None } => (),
            }
        }
    }

    /// Memory state queries clamp to this frame; disk history is needed for earlier frames.
    /// Open spans, metadata and live allocations remain available to decode later events.
    pub fn retain_recent(&mut self, first_frame: usize) {
        let first_frame = first_frame.max(self.first_retained_frame);
        self.spans.retain(|span| span.received_frame >= first_frame);
        self.spans.shrink_to_fit();
        self.values
            .retain(|value| value.received_frame >= first_frame);
        self.values.shrink_to_fit();
        Arc::make_mut(&mut self.memory).retain_recent(first_frame);
        for starts in self.starts.values_mut() {
            starts.shrink_to_fit();
        }
        self.first_retained_frame = first_frame;
        self.budget.used = self.retained_bytes();
    }

    fn profile_retained_bytes(&self) -> usize {
        let mut bytes = 2 * self.spans.capacity() * size_of::<ProfileSpan>()
            + 2 * self.values.capacity() * size_of::<ProfileValue>()
            + self.defined_zones.len() * (MAP_ENTRY_BYTES + size_of::<u64>())
            + self.ambiguous_zones.len() * (MAP_ENTRY_BYTES + size_of::<u64>())
            + self.ambiguous_events.len() * (MAP_ENTRY_BYTES + size_of::<(u64, u16)>())
            + self.unmatched_stop_events.len()
                * (MAP_ENTRY_BYTES + size_of::<(u64, u16, u32, u64)>() + size_of::<u64>());
        for zone in self.zones.values() {
            bytes += MAP_ENTRY_BYTES
                + size_of::<u64>()
                + size_of::<ProfileZone>()
                + zone.name.capacity();
            for name in zone.events.values() {
                bytes += MAP_ENTRY_BYTES + size_of::<u16>() + size_of::<String>() + name.capacity();
            }
        }
        for starts in self.starts.values() {
            bytes += MAP_ENTRY_BYTES
                + size_of::<(u64, u16, u32, u64)>()
                + size_of::<Vec<u64>>()
                + 2 * starts.capacity() * size_of::<u64>();
        }
        bytes
    }

    pub(crate) fn additional_cache_bytes(&self, previous: &Self) -> usize {
        let mut bytes = size_of::<Self>() + 2 * size_of::<usize>() + self.profile_retained_bytes();
        if Arc::ptr_eq(&self.memory, &previous.memory) {
            return bytes;
        }
        let memory = &self.memory;
        let before = &previous.memory;
        bytes += size_of::<MemoryAnalysis>()
            + 2 * size_of::<usize>()
            + memory.live.additional_bytes(&before.live)
            + 2 * memory.samples.capacity() * size_of::<MemorySample>()
            + 2 * memory.events.capacity() * size_of::<Arc<MemoryEvent>>()
            + memory.strings.len()
                * (MAP_ENTRY_BYTES + size_of::<(u64, u32)>() + size_of::<Arc<str>>());
        if !Arc::ptr_eq(&memory.base, &before.base) {
            bytes += size_of::<MemoryState>()
                + 2 * size_of::<usize>()
                + memory.base.live.additional_bytes(&before.base.live);
        }
        for (id, value) in &memory.strings {
            if before
                .strings
                .get(id)
                .is_none_or(|other| !Arc::ptr_eq(value, other))
            {
                bytes += value.len() + 2 * size_of::<usize>();
            }
        }
        let common = if memory.events.len() >= before.events.len()
            && memory
                .events
                .first()
                .zip(before.events.first())
                .is_some_and(|(event, old)| Arc::ptr_eq(event, old))
            && before
                .events
                .last()
                .zip(memory.events.get(before.events.len().saturating_sub(1)))
                .is_some_and(|(old, event)| Arc::ptr_eq(event, old))
        {
            before.events.len()
        } else {
            0
        };
        for event in &memory.events[common..] {
            bytes += size_of::<MemoryEvent>() + 2 * size_of::<usize>();
            let mut allocation = |value: &Allocation| {
                bytes += size_of::<Allocation>()
                    + 6 * size_of::<usize>()
                    + value.type_name.len()
                    + value.file.len();
            };
            match &event.kind {
                MemoryEventKind::Allocate {
                    allocation: value,
                    replaced,
                } => {
                    allocation(value);
                    if let Some(value) = replaced {
                        allocation(value);
                    }
                }
                MemoryEventKind::Free {
                    allocation: Some(value),
                } => allocation(value),
                MemoryEventKind::Free { allocation: None } => (),
            }
        }
        bytes
    }

    fn retained_bytes(&self) -> usize {
        let mut bytes = self.profile_retained_bytes();
        let memory = &self.memory;
        bytes += (memory.live.len() + memory.base.live.len()) * LIVE_ALLOCATION_ENTRY_BYTES
            + 2 * memory.samples.capacity() * size_of::<MemorySample>()
            + 2 * memory.events.capacity() * size_of::<Arc<MemoryEvent>>()
            + memory.events.len() * (size_of::<MemoryEvent>() + 2 * size_of::<usize>())
            + size_of::<MemoryState>()
            + 2 * size_of::<usize>();
        let mut strings = BTreeSet::new();
        let mut allocations = BTreeSet::new();
        for value in memory.strings.values() {
            bytes += MAP_ENTRY_BYTES + size_of::<(u64, u32)>() + size_of::<Arc<str>>();
            bytes += retained_string_bytes(value, &mut strings);
        }
        let mut allocation_bytes = |allocation: &Arc<Allocation>| {
            if allocations.insert(Arc::as_ptr(allocation) as usize) {
                size_of::<Allocation>()
                    + 2 * size_of::<usize>()
                    + retained_string_bytes(&allocation.type_name, &mut strings)
                    + retained_string_bytes(&allocation.file, &mut strings)
            } else {
                0
            }
        };
        for allocation in memory.live.values().chain(memory.base.live.values()) {
            bytes += allocation_bytes(allocation);
        }
        for event in &memory.events {
            match &event.kind {
                MemoryEventKind::Allocate {
                    allocation,
                    replaced,
                } => {
                    bytes += allocation_bytes(allocation);
                    if let Some(replaced) = replaced {
                        bytes += allocation_bytes(replaced);
                    }
                }
                MemoryEventKind::Free { allocation } => {
                    if let Some(allocation) = allocation {
                        bytes += allocation_bytes(allocation);
                    }
                }
            }
        }
        bytes
    }

    pub fn add_zone(&mut self, id: u64, name: String) -> Result<(), Error> {
        let first_definition = !self.defined_zones.contains(&id);
        let ambiguous = !first_definition
            && self.zones.get(&id).is_some_and(|zone| zone.name != name)
            && !self.ambiguous_zones.contains(&id);
        self.budget.charge(
            name.len()
                + usize::from(first_definition) * (MAP_ENTRY_BYTES + size_of::<u64>())
                + usize::from(ambiguous) * (MAP_ENTRY_BYTES + size_of::<u64>())
                + if self.zones.contains_key(&id) {
                    0
                } else {
                    MAP_ENTRY_BYTES + std::mem::size_of::<ProfileZone>()
                },
        )?;
        self.defined_zones.insert(id);
        if ambiguous {
            self.ambiguous_zones.insert(id);
        }
        self.zones.entry(id).or_default().name = name;
        Ok(())
    }

    pub fn add_event(&mut self, zone: u64, event: u16, name: String) -> Result<(), Error> {
        let ambiguous = self
            .zones
            .get(&zone)
            .and_then(|zone| zone.events.get(&event))
            .is_some_and(|previous| previous != &name)
            && !self.ambiguous_events.contains(&(zone, event));
        self.budget.charge(
            name.len()
                + MAP_ENTRY_BYTES
                + size_of::<ProfileZone>()
                + usize::from(ambiguous) * (MAP_ENTRY_BYTES + size_of::<(u64, u16)>()),
        )?;
        if ambiguous {
            self.ambiguous_events.insert((zone, event));
        }
        self.zones
            .entry(zone)
            .or_default()
            .events
            .insert(event, name);
        Ok(())
    }

    pub fn accounted_bytes(&self) -> usize {
        self.budget.used
    }

    pub fn open_spans(&self) -> usize {
        self.pending_starts
    }

    pub fn open_events(&self) -> impl Iterator<Item = (&(u64, u16, u32, u64), usize)> {
        self.starts
            .iter()
            .filter(|(_, starts)| !starts.is_empty())
            .map(|(key, starts)| (key, starts.len()))
    }

    pub fn open_event_starts(&self) -> impl Iterator<Item = (&(u64, u16, u32, u64), &[u64])> {
        self.starts
            .iter()
            .filter(|(_, starts)| !starts.is_empty())
            .map(|(key, starts)| (key, starts.as_slice()))
    }

    pub fn profile_metadata_ambiguous(&self, zone: u64, event: u16) -> bool {
        self.ambiguous_zones.contains(&zone) || self.ambiguous_events.contains(&(zone, event))
    }

    pub fn ingest(
        &mut self,
        id: u64,
        type_name: &str,
        data: &[u8],
        endian: Endian,
        frame: usize,
    ) -> Result<(), Error> {
        if type_name.ends_with("PxProfileZone") {
            self.profile_buffer(id, data, endian, frame)
        } else if type_name.ends_with("PxProfileMemoryEventBuffer") {
            self.memory_buffer(id, data, endian, frame)
        } else {
            Ok(())
        }
    }

    fn profile_buffer(
        &mut self,
        zone: u64,
        bytes: &[u8],
        endian: Endian,
        frame: usize,
    ) -> Result<(), Error> {
        self.profile_buffer_with_drain(zone, bytes, endian, frame, None)
    }

    fn profile_buffer_with_drain(
        &mut self,
        zone: u64,
        bytes: &[u8],
        endian: Endian,
        frame: usize,
        mut drain: ExportDrain<'_>,
    ) -> Result<(), Error> {
        if frame < self.first_retained_frame {
            return Err(error("profile received_frame precedes retained history"));
        }
        let mut reader = Reader {
            bytes,
            position: 0,
            endian,
        };
        let mut timestamp = 0u64;
        let mut thread = 0;
        let mut context = 0;
        let mut batch_events = 0usize;
        while !reader.done() {
            if let Some(drain) = &mut drain
                && (batch_events.is_multiple_of(256)
                    || self.budget.used > self.budget.limit.saturating_sub(1 << 20))
            {
                drain(self)?;
            }
            batch_events += 1;
            if self.spans.len()
                + self.values.len()
                + self.open_spans()
                + self.unmatched_stop_events.len()
                + self.starts.len()
                >= MAX_RECORDS
            {
                return Err(error("profile analysis exceeds two million events"));
            }
            let kind = reader.uint(1)?;
            let options = reader.uint(1)?;
            let event = reader.uint(2)? as u16;
            let time_width = 1 << (options & 3);
            let context_width = 1 << ((options >> 2) & 3);
            match kind {
                1..=4 => {
                    if kind <= 2 {
                        thread = reader.uint(4)? as u32;
                        context = reader.uint(context_width)?;
                        reader.take(2)?;
                    }
                    let encoded = reader.uint(time_width)?;
                    timestamp = if time_width == 8 {
                        encoded
                    } else {
                        timestamp
                            .checked_add(encoded)
                            .ok_or_else(|| error("profile timestamp overflow"))?
                    };
                    let key = (zone, event, thread, context);
                    if kind == 1 || kind == 3 {
                        self.budget.charge(
                            2 * std::mem::size_of::<u64>()
                                + if self.starts.contains_key(&key) {
                                    0
                                } else {
                                    MAP_ENTRY_BYTES + std::mem::size_of::<Vec<u64>>()
                                },
                        )?;
                        self.starts.entry(key).or_default().push(timestamp);
                        self.pending_starts += 1;
                    } else if let Some(start) = self
                        .starts
                        .get(&key)
                        .and_then(|values| values.last())
                        .copied()
                    {
                        if timestamp < start {
                            return Err(error("profile stop precedes its start"));
                        }
                        self.budget.charge(2 * std::mem::size_of::<ProfileSpan>())?;
                        let starts = self.starts.get_mut(&key).unwrap();
                        starts.pop();
                        if starts.is_empty() {
                            self.starts.remove(&key);
                            self.budget.release(
                                MAP_ENTRY_BYTES
                                    + std::mem::size_of::<Vec<u64>>()
                                    + 2 * std::mem::size_of::<u64>(),
                            );
                        }
                        self.spans.push(ProfileSpan {
                            zone,
                            event,
                            thread,
                            context,
                            start,
                            end: timestamp,
                            received_frame: frame,
                        });
                        self.pending_starts -= 1;
                    } else {
                        if !self.unmatched_stop_events.contains_key(&key) {
                            self.budget.charge(
                                MAP_ENTRY_BYTES
                                    + std::mem::size_of_val(&key)
                                    + std::mem::size_of::<u64>(),
                            )?;
                        }
                        self.unmatched_stops += 1;
                        *self.unmatched_stop_events.entry(key).or_default() += 1;
                    }
                }
                5 => {
                    let raw_value = reader.uint(time_width)?;
                    let value_context = reader.uint(context_width)?;
                    let value_thread = reader.uint(4)? as u32;
                    self.budget
                        .charge(2 * std::mem::size_of::<ProfileValue>())?;
                    self.values.push(ProfileValue {
                        zone,
                        event,
                        thread: value_thread,
                        context: value_context,
                        raw_value,
                        received_frame: frame,
                    });
                }
                6 => {
                    reader.take(12)?;
                    let length = reader.uint(4)? as usize;
                    reader.take(length)?;
                    reader.take(8)?;
                    self.legacy_cuda_buffers += 1;
                }
                _ => {
                    return Err(error(format!(
                        "unknown profile event {kind} at embedded byte {}; event boundary is unavailable",
                        reader.position - 4
                    )));
                }
            }
        }
        if let Some(drain) = &mut drain {
            drain(self)?;
        }
        Ok(())
    }

    fn memory_buffer(
        &mut self,
        id: u64,
        bytes: &[u8],
        endian: Endian,
        frame: usize,
    ) -> Result<(), Error> {
        self.memory_buffer_with_resolver(id, bytes, endian, frame, None, None)
    }

    pub(crate) fn ingest_native_memory<'a>(
        &mut self,
        id: u64,
        bytes: &[u8],
        frame: usize,
        mut resolve: impl FnMut(u32) -> Result<&'a str, Error>,
    ) -> Result<(), Error> {
        self.memory_buffer_with_resolver(id, bytes, Endian::Little, frame, Some(&mut resolve), None)
    }

    fn memory_buffer_with_resolver<'a>(
        &mut self,
        id: u64,
        bytes: &[u8],
        endian: Endian,
        frame: usize,
        mut resolve: Option<&mut dyn FnMut(u32) -> Result<&'a str, Error>>,
        mut drain: ExportDrain<'_>,
    ) -> Result<(), Error> {
        let mut reader = Reader {
            bytes,
            position: 0,
            endian,
        };
        let memory = &self.memory;
        if frame < memory.first_retained_frame
            || memory
                .samples
                .last()
                .is_some_and(|sample| sample.received_frame > frame)
            || memory
                .events
                .last()
                .is_some_and(|event| event.received_frame > frame)
        {
            return Err(error("memory flush received_frame moved backwards"));
        }
        let mut batch_events = 0usize;
        while !reader.done() {
            if let Some(drain) = &mut drain
                && (batch_events.is_multiple_of(256)
                    || self.budget.used > self.budget.limit.saturating_sub(1 << 20))
            {
                drain(self)?;
            }
            batch_events += 1;
            let memory = Arc::make_mut(&mut self.memory);
            let budget = &mut self.budget;
            if memory.live.len() >= 1_000_000 || memory.strings.len() >= 1_000_000 {
                return Err(error(
                    "memory analysis exceeds one million live allocations or strings",
                ));
            }
            let header = reader.uint(2)?;
            let address_width = 1 << ((header >> 4) & 3);
            let type_width = (1 << ((header >> 6) & 3)).min(4);
            let file_width = (1 << ((header >> 8) & 3)).min(4);
            let size_width = (1 << ((header >> 10) & 3)).min(4);
            let line_width = (1 << ((header >> 12) & 3)).min(4);
            match header & 15 {
                1 => {
                    if resolve.is_some() {
                        return Err(error(
                            "PVD3 memory buffers must use container string handles",
                        ));
                    }
                    let length = reader.uint(4)? as usize;
                    let bytes = reader.take(length)?;
                    let text = bytes
                        .strip_suffix(&[0])
                        .ok_or_else(|| error("memory string lacks null terminator"))?;
                    let value = String::from_utf8_lossy(text);
                    let handle = reader.uint(4)? as u32;
                    // Earlier allocations can still reference a redefined handle's string.
                    // Retention recomputes the storage charge from surviving references.
                    budget.charge(
                        value.len()
                            + MAP_ENTRY_BYTES
                            + size_of::<(u64, u32)>()
                            + size_of::<Arc<str>>()
                            + 2 * size_of::<usize>(),
                    )?;
                    memory
                        .strings
                        .insert((id, handle), Arc::from(value.as_ref()));
                }
                2 => {
                    let size = reader.uint(size_width)?;
                    let type_id = reader.uint(type_width)? as u32;
                    let file_id = reader.uint(file_width)? as u32;
                    let line = reader.uint(line_width)? as u32;
                    let address = reader.uint(address_width)?;
                    let mut string = |handle, field| -> Result<Arc<str>, Error> {
                        if let Some(value) = memory.strings.get(&(id, handle)) {
                            return Ok(value.clone());
                        }
                        let resolver = resolve.as_mut().ok_or_else(|| {
                            error(format!("undefined memory {field} handle {handle}"))
                        })?;
                        if memory.strings.len() >= 1_000_000 {
                            return Err(error("memory analysis exceeds one million strings"));
                        }
                        let value = resolver(handle)?;
                        budget.charge(
                            value.len()
                                + MAP_ENTRY_BYTES
                                + size_of::<(u64, u32)>()
                                + size_of::<Arc<str>>()
                                + 2 * size_of::<usize>(),
                        )?;
                        let value: Arc<str> = Arc::from(value);
                        memory.strings.insert((id, handle), value.clone());
                        Ok(value)
                    };
                    let type_name = string(type_id, "type")?;
                    let file = string(file_id, "file")?;
                    if memory.events.len() >= MAX_RECORDS {
                        return Err(error(
                            "memory history exceeds two million allocation/free events",
                        ));
                    }
                    let replaced = memory.live.get(&(id, address)).cloned();
                    let live_bytes = (memory.live_bytes
                        - replaced.as_ref().map_or(0, |old| old.size))
                    .checked_add(size)
                    .ok_or_else(|| error("live memory byte count overflow"))?;
                    let allocated_bytes = memory
                        .total_allocated_bytes
                        .checked_add(size)
                        .ok_or_else(|| error("allocated memory byte count overflow"))?;
                    budget.charge(
                        MEMORY_EVENT_BYTES
                            + std::mem::size_of::<Allocation>()
                            + 2 * std::mem::size_of::<usize>()
                            + if replaced.is_none() {
                                LIVE_ALLOCATION_ENTRY_BYTES
                            } else {
                                0
                            },
                    )?;
                    let allocation = Arc::new(Allocation {
                        address,
                        size,
                        type_name,
                        file,
                        line,
                        received_frame: frame,
                    });
                    memory.live.insert((id, address), allocation.clone());
                    memory.replaced_addresses += u64::from(replaced.is_some());
                    memory.events.push(Arc::new(MemoryEvent {
                        buffer: id,
                        address,
                        received_frame: frame,
                        kind: MemoryEventKind::Allocate {
                            allocation,
                            replaced,
                        },
                    }));
                    memory.live_bytes = live_bytes;
                    memory.peak_bytes = memory.peak_bytes.max(live_bytes);
                    memory.total_allocated_bytes = allocated_bytes;
                    memory.allocations += 1;
                }
                3 => {
                    let address = reader.uint(address_width)?;
                    if memory.events.len() >= MAX_RECORDS {
                        return Err(error(
                            "memory history exceeds two million allocation/free events",
                        ));
                    }
                    budget.charge(MEMORY_EVENT_BYTES)?;
                    let allocation = memory.live.remove(&(id, address));
                    if let Some(allocation) = &allocation {
                        budget.release(LIVE_ALLOCATION_ENTRY_BYTES);
                        memory.live_bytes -= allocation.size;
                    } else {
                        memory.unmatched_frees += 1;
                    }
                    memory.events.push(Arc::new(MemoryEvent {
                        buffer: id,
                        address,
                        received_frame: frame,
                        kind: MemoryEventKind::Free { allocation },
                    }));
                    memory.frees += 1;
                }
                kind => {
                    return Err(error(format!(
                        "unsupported memory event {kind} at embedded byte {}; event boundary is unavailable",
                        reader.position - 2
                    )));
                }
            }
        }
        let memory = Arc::make_mut(&mut self.memory);
        let budget = &mut self.budget;
        if memory.samples.len() >= MAX_RECORDS {
            return Err(error("memory history exceeds two million flushes"));
        }
        budget.charge(2 * std::mem::size_of::<MemorySample>())?;
        memory.samples.push(MemorySample {
            received_frame: frame,
            live_bytes: memory.live_bytes,
            live_allocations: memory.live.len(),
            allocations: memory.allocations,
            frees: memory.frees,
        });
        if let Some(drain) = &mut drain {
            drain(self)?;
        }
        Ok(())
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
    endian: Endian,
}
impl<'a> Reader<'a> {
    fn done(&self) -> bool {
        self.position == self.bytes.len()
    }
    fn take(&mut self, count: usize) -> Result<&'a [u8], Error> {
        let end = self
            .position
            .checked_add(count)
            .ok_or_else(|| error("embedded length overflow"))?;
        let data = self.bytes.get(self.position..end).ok_or_else(|| {
            error(format!(
                "truncated embedded event at byte {} requesting {count} bytes",
                self.position
            ))
        })?;
        self.position = end;
        Ok(data)
    }
    fn uint(&mut self, count: usize) -> Result<u64, Error> {
        let endian = self.endian;
        let data = self.take(count)?;
        let mut bytes = [0; 8];
        Ok(match endian {
            Endian::Little => {
                bytes[..count].copy_from_slice(data);
                u64::from_le_bytes(bytes)
            }
            Endian::Big => {
                bytes[8 - count..].copy_from_slice(data);
                u64::from_be_bytes(bytes)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allocation(size: u8, address: u8) -> Vec<u8> {
        let mut bytes = 2u16.to_le_bytes().to_vec();
        bytes.extend([size, 1, 2, 20, address]);
        bytes
    }

    fn free(address: u8) -> Vec<u8> {
        vec![3, 0, address]
    }

    fn memory_fixture() -> Analysis {
        let mut names = memory_string(1, "shape");
        names.extend(memory_string(2, "source.cpp"));
        names.extend(allocation(64, 88));
        names.extend(allocation(32, 89));
        let mut analysis = Analysis::default();
        for (frame, bytes) in [
            names,
            allocation(96, 88),
            free(89),
            free(99),
            allocation(8, 90),
            free(88),
            allocation(7, 88),
            Vec::new(),
            free(90),
            allocation(9, 90),
            free(88),
            Vec::new(),
        ]
        .iter()
        .enumerate()
        {
            analysis
                .memory_buffer(1, bytes, Endian::Little, frame)
                .unwrap();
        }
        analysis
    }

    fn resident_memory_fixture() -> Analysis {
        let retained_per_allocation =
            2 * LIVE_ALLOCATION_ENTRY_BYTES + size_of::<Allocation>() + 2 * size_of::<usize>();
        let count = (33 << 20) / retained_per_allocation + 1;
        let mut bytes = memory_string(1, "resident");
        bytes.extend(memory_string(2, "source.cpp"));
        for address in 1..=count as u32 {
            bytes.extend(0x22u16.to_le_bytes());
            bytes.extend([1, 1, 2, 20]);
            bytes.extend(address.to_le_bytes());
        }
        let mut analysis = Analysis::default();
        analysis
            .memory_buffer(7, &bytes, Endian::Little, 0)
            .unwrap();
        analysis.retain_recent(1);
        assert!(analysis.accounted_bytes() > 32 << 20);
        assert!(analysis.accounted_bytes() < MAX_ANALYSIS_BYTES);
        assert_eq!(analysis.memory.live.len(), count);
        assert!(analysis.memory.events.is_empty());
        analysis
    }

    fn profile_value(value: u8) -> Vec<u8> {
        let mut bytes = vec![5, 0, 8, 0, value, 9];
        bytes.extend(42u32.to_le_bytes());
        bytes
    }

    #[test]
    fn live_maintenance_does_not_rescan_resident_allocations_for_each_profile_buffer() {
        let mut analysis = SharedAnalysis::from(resident_memory_fixture());
        analysis.retain_recent(1).unwrap();
        let resident_count = analysis.legacy().unwrap().memory.live.len();
        let initial_memory = analysis.legacy().unwrap().memory.clone();
        let original = analysis.clone();
        let mut sweeps = 0;
        for frame in 2..720 {
            let published = analysis.clone();
            let revision = analysis.revision();
            let swept = analysis.maintain_live_history(frame).unwrap();
            sweeps += usize::from(swept);
            if !swept {
                assert_eq!(analysis.revision(), revision);
                assert!(std::ptr::eq(
                    analysis.legacy().unwrap(),
                    published.legacy().unwrap()
                ));
            }
            let memory = analysis.legacy().unwrap().memory.clone();
            analysis
                .ingest(
                    1,
                    "sdk::PxProfileZone",
                    &profile_value(7),
                    Endian::Little,
                    frame,
                )
                .unwrap();
            assert!(Arc::ptr_eq(&analysis.legacy().unwrap().memory, &memory));
            if frame < 660 {
                assert!(Arc::ptr_eq(
                    &analysis.legacy().unwrap().memory,
                    &initial_memory
                ));
            }
            assert_eq!(
                published
                    .legacy()
                    .unwrap()
                    .values
                    .last()
                    .map(|value| value.received_frame),
                (frame > 2).then_some(frame - 1)
            );
        }
        assert_eq!(sweeps, 1);
        assert_eq!(analysis.legacy().unwrap().first_retained_frame, 60);
        assert_eq!(
            analysis
                .legacy()
                .unwrap()
                .values
                .first()
                .unwrap()
                .received_frame,
            60
        );
        assert_eq!(
            analysis
                .legacy()
                .unwrap()
                .values
                .last()
                .unwrap()
                .received_frame,
            719
        );
        assert_eq!(analysis.legacy().unwrap().memory.live.len(), resident_count);
        assert_eq!(
            analysis.legacy().unwrap().memory.live_bytes,
            resident_count as u64
        );
        assert!(original.legacy().unwrap().values.is_empty());
        assert_eq!(original.legacy().unwrap().first_retained_frame, 1);
        assert_eq!(original.legacy().unwrap().memory.live.len(), resident_count);
    }

    #[test]
    fn memory_sharing_keeps_published_snapshots_immutable_and_serialization_transparent() {
        let original = memory_fixture();
        let bytes = postcard::to_stdvec(original.memory.as_ref()).unwrap();
        assert_eq!(postcard::to_stdvec(&original.memory).unwrap(), bytes);
        let decoded: MemoryAnalysis = postcard::from_bytes(&bytes).unwrap();
        let shared_decoded: Arc<MemoryAnalysis> = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(
            postcard::to_stdvec(&decoded).unwrap(),
            postcard::to_stdvec(&shared_decoded).unwrap()
        );

        let mut analysis = SharedAnalysis::from(original);
        let published = analysis.clone();
        analysis
            .ingest(
                1,
                "sdk::PxProfileZone",
                &profile_value(7),
                Endian::Little,
                12,
            )
            .unwrap();
        assert!(Arc::ptr_eq(
            &analysis.legacy().unwrap().memory,
            &published.legacy().unwrap().memory
        ));
        analysis
            .ingest(
                1,
                "sdk::PxProfileMemoryEventBuffer",
                &free(90),
                Endian::Little,
                12,
            )
            .unwrap();
        assert!(!Arc::ptr_eq(
            &analysis.legacy().unwrap().memory,
            &published.legacy().unwrap().memory
        ));
        assert_eq!(analysis.legacy().unwrap().memory.live_bytes, 0);
        assert_eq!(published.legacy().unwrap().memory.live_bytes, 9);
        assert_eq!(
            published
                .legacy()
                .unwrap()
                .memory
                .state_at_frame(11)
                .live_bytes,
            9
        );
        assert_eq!(
            analysis
                .legacy()
                .unwrap()
                .memory
                .state_at_frame(12)
                .live_bytes,
            0
        );
        assert!(published.legacy().unwrap().values.is_empty());
    }

    #[test]
    fn maintenance_growth_budget_preserves_the_hard_limit_and_explicit_recalculation() {
        let mut analysis = SharedAnalysis::default();
        analysis.legacy_mut().unwrap().budget.limit = 8192;
        analysis.retain_recent(0).unwrap();
        let mut sweeps = 0;
        for frame in 1..500 {
            sweeps += usize::from(analysis.maintain_live_history(frame).unwrap());
            analysis
                .ingest(
                    1,
                    "sdk::PxProfileZone",
                    &profile_value(7),
                    Endian::Little,
                    frame,
                )
                .unwrap();
            assert!(analysis.accounted_bytes() <= 8192);
        }
        assert!(sweeps > 0);
        assert!(analysis.legacy().unwrap().values.len() < 100);
        assert_eq!(
            analysis
                .legacy()
                .unwrap()
                .values
                .last()
                .unwrap()
                .received_frame,
            499
        );

        let first_frame = analysis.legacy().unwrap().first_retained_frame;
        analysis.legacy_mut().unwrap().values.insert(
            0,
            ProfileValue {
                zone: 1,
                event: 8,
                thread: 42,
                context: 9,
                raw_value: 1,
                received_frame: first_frame.saturating_sub(1),
            },
        );
        analysis.retain_recent(first_frame).unwrap();
        assert!(
            analysis
                .legacy()
                .unwrap()
                .values
                .iter()
                .all(|value| value.received_frame >= first_frame)
        );
        let current = analysis.accounted_bytes();
        analysis.legacy_mut().unwrap().budget.limit = current;
        assert!(
            analysis
                .ingest(
                    1,
                    "sdk::PxProfileZone",
                    &profile_value(8),
                    Endian::Little,
                    500
                )
                .unwrap_err()
                .to_string()
                .contains("allocation budget")
        );
    }

    #[test]
    fn retention_schedule_initializes_after_recalculation_and_restarts_after_reset() {
        let mut analysis = Analysis::default();
        analysis.budget.limit = 8192;
        for _ in 0..50 {
            analysis
                .profile_buffer(1, &profile_value(7), Endian::Little, 0)
                .unwrap();
        }
        analysis.retain_recent(0);
        let mut retention = RetentionSchedule::after_retention(&analysis);
        assert!(!retention.maintain(&mut analysis, 0));

        analysis = Analysis::default();
        analysis.budget.limit = 8192;
        analysis.first_retained_frame = 100;
        retention = RetentionSchedule::default();
        for _ in 0..60 {
            analysis
                .profile_buffer(1, &profile_value(7), Endian::Little, 100)
                .unwrap();
        }
        assert!(retention.maintain(&mut analysis, 100));
        assert_eq!(analysis.values.len(), 60);
        assert_eq!(analysis.first_retained_frame, 100);
        assert!(!retention.maintain(&mut analysis, 100));
    }

    #[test]
    fn disk_analysis_maintains_between_same_frame_buffers_and_replays_segment_resets() {
        use crate::analysis::view::{MemorySource, MemoryViewCursor, ProfileSource};
        use crate::live::{AnalysisInput, HistoryConfig, HistoryWriter, Streaming};

        let directory = std::env::temp_dir().join(format!(
            "pvd-analysis-pressure-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let history = HistoryWriter::new(HistoryConfig {
            directory: directory.clone(),
            disk_budget_bytes: 32 << 20,
            segment_target_bytes: 8 << 20,
            segment_frames: 150,
            min_free_disk_bytes: 0,
        })
        .unwrap();
        let mut builder = crate::RecordingBuilder::new(pvd_protocol::Header {
            version: 1,
            timestamp_numerator: 1,
            timestamp_denominator: 1,
            flags: 1,
            endian: Endian::Little,
        });
        builder.streaming = Some(Box::new(Streaming::new(history)));
        builder
            .recording
            .analysis
            .legacy_mut()
            .unwrap()
            .budget
            .limit = 16 * 1024;
        let feed = |builder: &mut crate::RecordingBuilder, kind: &str, bytes: Vec<u8>, frame| {
            if builder
                .recording
                .analysis
                .maintain_live_history(frame)
                .unwrap()
            {
                builder
                    .streaming
                    .as_mut()
                    .unwrap()
                    .analysis_input(AnalysisInput::Retain {
                        first_frame: builder.recording.analysis.first_retained_frame(),
                        accounted_bytes: builder.recording.analysis.accounted_bytes(),
                    });
            }
            builder
                .recording
                .analysis
                .ingest(1, kind, &bytes, Endian::Little, frame)
                .unwrap();
            builder
                .streaming
                .as_mut()
                .unwrap()
                .analysis_input(AnalysisInput::Buffer {
                    id: 1,
                    kind: kind.into(),
                    bytes: bytes.into(),
                    little_endian: true,
                    frame,
                });
        };
        let profile_batch = |count| profile_value(7).repeat(count);
        let targets = [100usize, 149, 200, 290, 309, 310, 320];
        let mut expected = BTreeMap::new();
        for frame in 0..=320 {
            if frame == 0 {
                let mut bytes = memory_string(1, "resident");
                bytes.extend(memory_string(2, "source.cpp"));
                bytes.extend(allocation(9, 88));
                feed(
                    &mut builder,
                    "sdk::PxProfileMemoryEventBuffer",
                    bytes,
                    frame,
                );
                feed(
                    &mut builder,
                    "sdk::PxProfileZone",
                    profile_batch(120),
                    frame,
                );
            }
            if frame == 100 {
                let mut without_buffer_maintenance =
                    builder.recording.analysis.legacy().unwrap().clone();
                let error = (0..10)
                    .try_for_each(|_| {
                        without_buffer_maintenance.ingest(
                            1,
                            "sdk::PxProfileZone",
                            &profile_batch(15),
                            Endian::Little,
                            frame,
                        )
                    })
                    .unwrap_err();
                assert!(error.to_string().contains("allocation budget"));
            }
            if frame == 100 || frame == 290 {
                for _ in 0..10 {
                    feed(&mut builder, "sdk::PxProfileZone", profile_batch(15), frame);
                }
                feed(
                    &mut builder,
                    "sdk::PxProfileMemoryEventBuffer",
                    free(88),
                    frame,
                );
            }
            if frame == 130 || frame == 200 {
                feed(
                    &mut builder,
                    "sdk::PxProfileZone",
                    profile_batch(if frame == 130 { 5 } else { 20 }),
                    frame,
                );
            }
            if frame == 200 {
                feed(
                    &mut builder,
                    "sdk::PxProfileMemoryEventBuffer",
                    allocation(17, 88),
                    frame,
                );
            }
            if frame == 310 {
                builder.recording.analysis = SharedAnalysis::default();
                builder
                    .recording
                    .analysis
                    .legacy_mut()
                    .unwrap()
                    .first_retained_frame = frame;
                builder
                    .streaming
                    .as_mut()
                    .unwrap()
                    .analysis_input(AnalysisInput::Reset(frame));
                let mut bytes = memory_string(1, "after-reset");
                bytes.extend(memory_string(2, "reset.cpp"));
                bytes.extend(allocation(7, 88));
                feed(
                    &mut builder,
                    "sdk::PxProfileMemoryEventBuffer",
                    bytes,
                    frame,
                );
                feed(&mut builder, "sdk::PxProfileZone", profile_batch(3), frame);
            }
            if frame.is_multiple_of(60)
                && builder
                    .recording
                    .analysis
                    .maintain_live_history(frame)
                    .unwrap()
            {
                builder
                    .streaming
                    .as_mut()
                    .unwrap()
                    .analysis_input(AnalysisInput::Retain {
                        first_frame: builder.recording.analysis.first_retained_frame(),
                        accounted_bytes: builder.recording.analysis.accounted_bytes(),
                    });
            }
            builder.streaming.as_mut().unwrap().complete(
                frame as u64,
                1,
                &builder.state,
                &builder.recording.analysis,
            );
            if targets.contains(&frame) {
                let analysis = builder.recording.analysis.legacy().unwrap();
                let values = analysis
                    .values
                    .iter()
                    .filter(|value| value.received_frame == frame)
                    .collect::<Vec<_>>();
                expected.insert(
                    frame,
                    (
                        postcard::to_stdvec(&values).unwrap(),
                        analysis.memory.state_at_frame(frame),
                        analysis.budget.limit,
                    ),
                );
            }
        }
        let recording = builder.snapshot().unwrap();
        assert_eq!(recording.disk_usage().unwrap().segments, 3);
        for frame in [290usize, 100, 320, 149, 310, 200, 309, 100] {
            let replay = recording.analysis_at(frame).unwrap();
            let (values, memory, limit) = &expected[&frame];
            let actual_values = replay
                .profile_values()
                .filter(|value| value.received_frame == frame)
                .collect::<Vec<_>>();
            assert_eq!(
                &postcard::to_stdvec(&actual_values).unwrap(),
                values,
                "profile frame {frame}"
            );
            let mut cursor = MemoryViewCursor::default();
            replay.memory_snapshot().seek(&mut cursor, frame).unwrap();
            assert_eq!(
                postcard::to_stdvec(&cursor).unwrap(),
                postcard::to_stdvec(memory).unwrap(),
                "memory frame {frame}"
            );
            assert!(replay.first_retained_frame() <= frame);
            assert!(replay.accounted_bytes() <= *limit);
        }
        let reset = recording.analysis_at(310).unwrap();
        assert_eq!(reset.memory_snapshot().summary().allocations, 1);
        assert_eq!(reset.memory_snapshot().summary().frees, 0);
        assert_eq!(reset.memory_snapshot().summary().live_bytes, 7);
        drop(recording);
        drop(builder);
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    #[ignore = "local structural performance experiment; encoded event fixture is not SDK compatibility evidence"]
    fn resident_memory_profile_maintenance_benchmark() {
        let seed = resident_memory_fixture();
        let frame_count = 120usize;
        let bytes = profile_value(7);
        let run = |scheduled: bool| {
            let mut analysis = SharedAnalysis::from(seed.clone());
            analysis.retain_recent(1).unwrap();
            let started = std::time::Instant::now();
            let mut sweeps = 0usize;
            for frame in 2..frame_count + 2 {
                let published = analysis.clone();
                if scheduled {
                    sweeps += usize::from(analysis.maintain_live_history(frame).unwrap());
                } else if analysis.accounted_bytes() > 32 << 20 {
                    analysis.retain_recent(frame.saturating_sub(60)).unwrap();
                    sweeps += 1;
                }
                analysis
                    .ingest(1, "sdk::PxProfileZone", &bytes, Endian::Little, frame)
                    .unwrap();
                std::hint::black_box(&published);
            }
            (started.elapsed().as_secs_f64(), sweeps, analysis)
        };
        let (per_buffer_seconds, per_buffer_sweeps, baseline) = run(false);
        let (scheduled_seconds, scheduled_sweeps, mut scheduled) = run(true);
        scheduled
            .retain_recent(baseline.first_retained_frame())
            .unwrap();
        assert_eq!(
            postcard::to_stdvec(&scheduled).unwrap(),
            postcard::to_stdvec(&baseline).unwrap()
        );
        eprintln!(
            "resident_analysis_benchmark allocations={} resident_bytes={} buffers={} per_buffer_seconds={per_buffer_seconds:.6} per_buffer_sweeps={per_buffer_sweeps} scheduled_seconds={scheduled_seconds:.6} scheduled_sweeps={scheduled_sweeps}",
            seed.memory.live.len(),
            seed.accounted_bytes(),
            frame_count
        );
    }

    #[test]
    fn retained_memory_replays_allocations_and_resets_cursors_across_bases() {
        let original = memory_fixture();
        let mut retained = original.clone();
        let mut cursor = MemoryCursor::default();
        original.memory.seek(&mut cursor, 7);
        for first_frame in [1, 3, 4, 6, 9, 11] {
            retained.retain_recent(first_frame);
            assert_eq!(retained.first_retained_frame, first_frame);
            assert!(
                retained
                    .memory
                    .events()
                    .iter()
                    .all(|event| event.received_frame >= first_frame)
            );
            for frame in [11, first_frame, 10, first_frame, 0, 8, 11] {
                let expected = original.memory.state_at_frame(frame.max(first_frame));
                assert_eq!(retained.memory.state_at_frame(frame), expected);
                assert_eq!(*retained.memory.seek(&mut cursor, frame), expected);
            }
            assert_eq!(
                *original.memory.seek(&mut cursor, 0),
                original.memory.state_at_frame(0)
            );
        }
        retained.retain_recent(2);
        assert_eq!(retained.first_retained_frame, 11);
    }

    #[test]
    fn retained_allocation_can_be_freed_after_its_creation_event_is_evicted() {
        let mut analysis = memory_fixture();
        analysis.retain_recent(11);
        assert_eq!(analysis.memory.state_at_frame(11).live_bytes, 9);
        let mut cursor = MemoryCursor::default();
        analysis.memory.seek(&mut cursor, 11);
        analysis
            .memory_buffer(1, &free(90), Endian::Little, 12)
            .unwrap();
        assert_eq!(analysis.memory.seek(&mut cursor, 12).live_bytes, 0);
        assert_eq!(analysis.memory.seek(&mut cursor, 11).live_bytes, 9);
        analysis.retain_recent(13);
        assert_eq!(analysis.memory.seek(&mut cursor, 13).live_bytes, 0);
        assert_eq!(analysis.memory.seek(&mut cursor, 13).allocations, 6);
        assert_eq!(analysis.memory.seek(&mut cursor, 13).frees, 6);
    }

    #[test]
    fn bounded_memory_history_releases_budget_and_preserves_lifetime_totals() {
        let mut analysis = Analysis::default();
        analysis.budget.limit = 8192;
        let mut names = memory_string(1, "shape");
        names.extend(memory_string(2, "source.cpp"));
        analysis
            .memory_buffer(1, &names, Endian::Little, 0)
            .unwrap();
        let mut batch = allocation(64, 88);
        batch.extend(free(88));
        for frame in 1..=2000 {
            analysis
                .memory_buffer(1, &batch, Endian::Little, frame)
                .unwrap();
            analysis.retain_recent(frame.saturating_sub(4));
            assert!(analysis.accounted_bytes() < 4096);
            assert!(analysis.memory.events().len() <= 10);
            assert!(analysis.memory.samples.len() <= 5);
        }
        let state = analysis.memory.state_at_frame(2000);
        assert_eq!(state.allocations, 2000);
        assert_eq!(state.frees, 2000);
        assert_eq!(state.total_allocated_bytes, 2000 * 64);
        assert_eq!(state.total_freed_bytes, 2000 * 64);
        assert!(state.live.is_empty());
    }

    #[test]
    fn retention_keeps_open_profile_spans_and_bounds_completed_values() {
        let mut analysis = Analysis::default();
        analysis.budget.limit = 8192;
        analysis
            .profile_buffer(1, &profile_start(), Endian::Little, 0)
            .unwrap();
        for frame in 1..=1000 {
            let mut value = vec![5, 0, 8, 0, 7, 9];
            value.extend(42u32.to_le_bytes());
            analysis
                .profile_buffer(1, &value, Endian::Little, frame)
                .unwrap();
            analysis.retain_recent(frame.saturating_sub(4));
            assert!(analysis.values.len() <= 5);
            assert_eq!(analysis.open_spans(), 1);
            assert!(analysis.accounted_bytes() < 2048);
        }
        let mut stop = profile_start();
        stop[0] = 2;
        stop[11..19].copy_from_slice(&2000u64.to_le_bytes());
        analysis
            .profile_buffer(1, &stop, Endian::Little, 1001)
            .unwrap();
        assert_eq!(analysis.open_spans(), 0);
        assert_eq!(analysis.spans[0].start, 1000);
        assert_eq!(analysis.spans[0].end, 2000);
        analysis.retain_recent(1002);
        assert!(analysis.values.is_empty());
        assert!(analysis.spans.is_empty());
        assert!(
            analysis
                .profile_buffer(1, &stop, Endian::Little, 1000)
                .is_err()
        );
    }

    #[test]
    fn captured_analysis_restores_sharing_and_accounting_without_changing_wire_state() {
        let mut original = memory_fixture();
        original.retain_recent(6);
        let bytes = postcard::to_stdvec(&original).unwrap();
        let mut restored: Analysis = postcard::from_bytes(&bytes).unwrap();
        restored.restore_sharing();
        assert_eq!(postcard::to_stdvec(&restored).unwrap(), bytes);
        restored
            .retain_captured(6, original.accounted_bytes())
            .unwrap();
        assert_eq!(postcard::to_stdvec(&restored).unwrap(), bytes);
        assert!(restored.retain_captured(6, MAX_ANALYSIS_BYTES + 1).is_err());
        let before: SharedAnalysis = restored.into();
        assert!(std::ptr::eq(
            before.legacy().unwrap(),
            before.clone().legacy().unwrap()
        ));
        let mut after = before.clone();
        after
            .ingest(
                1,
                "sdk::PxProfileZone",
                &profile_value(7),
                Endian::Little,
                12,
            )
            .unwrap();
        assert!(Arc::ptr_eq(
            &after.legacy().unwrap().memory,
            &before.legacy().unwrap().memory
        ));
        assert!(
            after
                .legacy()
                .unwrap()
                .additional_cache_bytes(before.legacy().unwrap())
                < after.accounted_bytes() + 4096
        );
        assert!(before.legacy().unwrap().values.is_empty());
    }

    #[test]
    fn serialized_retained_analysis_preserves_base_and_cursor_results() {
        let original = memory_fixture();
        let mut retained = original.clone();
        retained.retain_recent(6);
        let bytes = postcard::to_stdvec(&retained).unwrap();
        let mut restored: Analysis = postcard::from_bytes(&bytes).unwrap();
        restored.retain_recent(restored.first_retained_frame);
        let mut cursor = MemoryCursor::default();
        retained.memory.seek(&mut cursor, 10);
        for frame in [11, 6, 9, 7, 11, 6] {
            assert_eq!(
                *restored.memory.seek(&mut cursor, frame),
                original.memory.state_at_frame(frame)
            );
        }
        let charge = restored.accounted_bytes();
        restored.retain_recent(6);
        assert_eq!(restored.accounted_bytes(), charge);
    }

    #[test]
    fn retention_reclaims_redefined_strings_only_after_the_last_allocation_reference() {
        let mut names = memory_string(1, &"n".repeat(16 * 1024));
        names.extend(memory_string(2, "source.cpp"));
        names.extend(allocation(64, 88));
        let mut analysis = Analysis::default();
        analysis
            .memory_buffer(1, &names, Endian::Little, 0)
            .unwrap();
        analysis
            .memory_buffer(1, &memory_string(1, "replacement"), Endian::Little, 1)
            .unwrap();
        analysis.retain_recent(2);
        assert!(analysis.accounted_bytes() >= 16 * 1024);
        assert_eq!(
            analysis.memory.state_at_frame(2).live[&(1, 88)]
                .type_name
                .len(),
            16 * 1024
        );
        analysis
            .memory_buffer(1, &free(88), Endian::Little, 2)
            .unwrap();
        analysis.retain_recent(3);
        assert!(analysis.accounted_bytes() < 1024);
        assert_eq!(analysis.memory.strings[&(1, 1)].as_ref(), "replacement");
        assert_eq!(analysis.memory.state_at_frame(3).total_freed_bytes, 64);
    }

    #[test]
    fn native_memory_names_are_resolved_once_and_budgeted_before_copying() {
        let mut bytes = Vec::new();
        for address in 1..=8 {
            bytes.extend(2u16.to_le_bytes());
            bytes.extend([64, 1, 2, 20, address]);
        }
        let name = "n".repeat(2048);
        let mut calls = 0;
        let mut analysis = Analysis::default();
        analysis
            .ingest_native_memory(9, &bytes, 3, |_| {
                calls += 1;
                Ok(name.as_str())
            })
            .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(analysis.memory.events.len(), 8);
        assert!(
            analysis
                .memory
                .events
                .iter()
                .all(|event| event.received_frame == 3)
        );
        let first = &analysis.memory.live[&(9, 1)].type_name;
        assert!(
            analysis
                .memory
                .live
                .values()
                .all(|a| Arc::ptr_eq(first, &a.type_name))
        );
        analysis.budget.limit = analysis.accounted_bytes() + 1024;
        assert!(
            analysis
                .ingest_native_memory(10, &bytes, 3, |_| Ok(name.as_str()))
                .is_err()
        );
        assert!(!analysis.memory.strings.contains_key(&(10, 1)));
        assert_eq!(analysis.memory.events.len(), 8);

        let mut limited = Analysis::default();
        limited.budget.limit = 1024;
        let error = limited
            .ingest_native_memory(9, &bytes, 3, |_| Ok(name.as_str()))
            .unwrap_err();
        assert!(error.to_string().contains("allocation budget"));
        assert!(limited.memory.strings.is_empty());
        assert!(limited.memory.events.is_empty());
        assert_eq!(limited.accounted_bytes(), 0);
    }

    #[test]
    fn native_memory_rejects_stream_strings_unknown_handles_and_truncation() {
        let definition = memory_string(1, "stream string");
        assert!(
            Analysis::default()
                .ingest_native_memory(1, &definition, 0, |_| Ok("saved"))
                .unwrap_err()
                .to_string()
                .contains("container string handles")
        );
        let mut allocation = 2u16.to_le_bytes().to_vec();
        allocation.extend([64, 1, 2, 20, 88]);
        assert!(
            Analysis::default()
                .ingest_native_memory(1, &allocation, 0, |_| Err(error(
                    "undefined container handle"
                )))
                .unwrap_err()
                .to_string()
                .contains("undefined container handle")
        );
        for len in 1..allocation.len() {
            assert!(
                Analysis::default()
                    .ingest_native_memory(1, &allocation[..len], 0, |_| Ok("saved"))
                    .is_err()
            );
        }
        assert!(
            Analysis::default()
                .ingest_native_memory(1, &[0xff, 0xff], 0, |_| Ok("saved"))
                .unwrap_err()
                .to_string()
                .contains("unsupported memory event")
        );
    }

    fn memory_string(handle: u32, text: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend(1u16.to_le_bytes());
        bytes.extend((text.len() as u32 + 1).to_le_bytes());
        bytes.extend(text.as_bytes());
        bytes.push(0);
        bytes.extend(handle.to_le_bytes());
        bytes
    }

    #[test]
    fn allocation_names_share_storage_and_keep_their_original_handle_value() {
        let mut bytes = memory_string(1, &"a".repeat(32 * 1024));
        bytes.extend(memory_string(2, "source.cpp"));
        for address in 1..=100 {
            bytes.extend(2u16.to_le_bytes());
            bytes.extend([64, 1, 2, 20, address]);
        }
        let mut analysis = Analysis::default();
        analysis
            .memory_buffer(7, &bytes, Endian::Little, 0)
            .unwrap();
        let first = analysis.memory.live[&(7, 1)].type_name.clone();
        for allocation in analysis.memory.live.values() {
            assert!(Arc::ptr_eq(&first, &allocation.type_name));
        }
        assert!(analysis.accounted_bytes() < 64 * 1024);
        analysis
            .memory_buffer(7, &memory_string(1, "replacement"), Endian::Little, 1)
            .unwrap();
        assert_eq!(analysis.memory.live[&(7, 1)].type_name.len(), 32 * 1024);
        assert_eq!(analysis.memory.strings[&(7, 1)].as_ref(), "replacement");
    }

    #[test]
    fn analysis_budget_limits_strings_and_records_across_buffers() {
        let mut analysis = Analysis::default();
        analysis.budget.limit = 1024;
        analysis
            .memory_buffer(1, &memory_string(1, &"x".repeat(300)), Endian::Little, 0)
            .unwrap();
        let error = analysis
            .memory_buffer(1, &memory_string(2, &"y".repeat(600)), Endian::Little, 1)
            .unwrap_err();
        assert!(error.to_string().contains("allocation budget"));
        assert!(!analysis.memory.strings.contains_key(&(1, 2)));
        assert!(analysis.accounted_bytes() <= 1024);

        let mut profile = Analysis::default();
        profile.budget.limit = 1024;
        let mut bytes = profile_start();
        for event in 0..100u16 {
            bytes[2..4].copy_from_slice(&event.to_le_bytes());
            if profile
                .profile_buffer(1, &bytes, Endian::Little, 0)
                .is_err()
            {
                assert!(profile.open_spans() < 100);
                assert!(profile.accounted_bytes() <= 1024);
                return;
            }
        }
        panic!("profile analysis must share the aggregate budget");
    }

    fn profile_start() -> Vec<u8> {
        let mut bytes = vec![1, 3, 7, 0];
        bytes.extend(42u32.to_le_bytes());
        bytes.extend([9, 0, 0]);
        bytes.extend(1000u64.to_le_bytes());
        bytes
    }

    #[test]
    fn relative_profile_context_resets_per_buffer_but_open_spans_survive() {
        let mut analysis = Analysis::default();
        analysis
            .profile_buffer(1, &profile_start(), Endian::Little, 0)
            .unwrap();
        let mut stop = profile_start();
        stop[0] = 2;
        stop[11..19].copy_from_slice(&1100u64.to_le_bytes());
        analysis
            .profile_buffer(1, &stop, Endian::Little, 1)
            .unwrap();
        assert_eq!(analysis.spans[0].end - analysis.spans[0].start, 100);
        assert_eq!(analysis.spans[0].thread, 42);
        assert_eq!(analysis.open_spans(), 0);
    }

    #[test]
    fn profile_payload_truncation_and_unknown_type_fail() {
        let bytes = profile_start();
        for length in 1..bytes.len() {
            assert!(
                Analysis::default()
                    .profile_buffer(1, &bytes[..length], Endian::Little, 0)
                    .is_err()
            );
        }
        assert!(
            Analysis::default()
                .profile_buffer(1, &[99, 0, 0, 0], Endian::Little, 0)
                .is_err()
        );
    }

    #[test]
    fn profile_definition_ambiguity_is_bounded_and_failed_updates_do_not_relabel() {
        let mut analysis = Analysis::default();
        analysis.add_zone(1, "Zone".into()).unwrap();
        analysis.add_event(1, 7, "Original".into()).unwrap();
        let baseline = analysis.accounted_bytes();
        analysis.budget.limit = baseline + 1;
        assert!(analysis.add_zone(1, "Other".into()).is_err());
        assert!(analysis.add_event(1, 7, "Replacement".into()).is_err());
        assert_eq!(analysis.zones[&1].name, "Zone");
        assert_eq!(analysis.zones[&1].events[&7], "Original");
        assert!(!analysis.profile_metadata_ambiguous(1, 7));
        assert_eq!(analysis.accounted_bytes(), baseline);
        analysis.budget.limit = baseline + 512;
        analysis.add_event(1, 7, "Replacement".into()).unwrap();
        assert!(analysis.profile_metadata_ambiguous(1, 7));
        assert!(!analysis.profile_metadata_ambiguous(1, 8));
        assert!(analysis.accounted_bytes() > baseline + MAP_ENTRY_BYTES);
    }

    #[test]
    fn memory_allocation_free_and_unknown_free_are_distinguished() {
        let mut bytes = Vec::new();
        for (handle, value) in [(1u32, "shape"), (2, "source.cpp")] {
            bytes.extend(1u16.to_le_bytes());
            bytes.extend((value.len() as u32 + 1).to_le_bytes());
            bytes.extend(value.as_bytes());
            bytes.push(0);
            bytes.extend(handle.to_le_bytes());
        }
        bytes.extend(2u16.to_le_bytes());
        bytes.extend([64, 1, 2, 20, 88]);
        bytes.extend(3u16.to_le_bytes());
        bytes.push(88);
        bytes.extend(3u16.to_le_bytes());
        bytes.push(99);
        let mut analysis = Analysis::default();
        analysis
            .memory_buffer(1, &bytes, Endian::Little, 3)
            .unwrap();
        assert_eq!(analysis.memory.peak_bytes, 64);
        assert_eq!(analysis.memory.live_bytes, 0);
        assert_eq!(analysis.memory.unmatched_frees, 1);
        assert_eq!(analysis.memory.allocations, 1);
        assert_eq!(analysis.memory.frees, 2);
    }

    #[test]
    fn freed_allocations_remain_bounded_history_and_share_original_records() {
        let mut analysis = Analysis::default();
        analysis.budget.limit = 8192;
        let mut names = memory_string(1, "shape");
        names.extend(memory_string(2, "source.cpp"));
        analysis
            .memory_buffer(1, &names, Endian::Little, 0)
            .unwrap();
        let mut batch = 2u16.to_le_bytes().to_vec();
        batch.extend([64, 1, 2, 20, 88]);
        batch.extend(3u16.to_le_bytes());
        batch.push(88);
        let mut exhausted = false;
        for frame in 1..200 {
            if let Err(error) = analysis.memory_buffer(1, &batch, Endian::Little, frame) {
                assert!(error.to_string().contains("allocation budget"));
                exhausted = true;
                break;
            }
        }
        assert!(exhausted);
        assert!(analysis.accounted_bytes() <= 8192);
        assert!(analysis.memory.events().len() < 100);
        let MemoryEventKind::Allocate { allocation, .. } = &analysis.memory.events[0].kind else {
            panic!()
        };
        let MemoryEventKind::Free {
            allocation: Some(freed),
        } = &analysis.memory.events[1].kind
        else {
            panic!()
        };
        assert!(Arc::ptr_eq(allocation, freed));
        assert_eq!(analysis.memory.state_at_frame(1).live_bytes, 0);
        assert_eq!(analysis.memory.events_at_frame(1).len(), 2);
    }

    #[test]
    fn partial_invalid_memory_buffer_cannot_allow_out_of_order_history() {
        let mut analysis = Analysis::default();
        let mut names = memory_string(1, "shape");
        names.extend(memory_string(2, "source.cpp"));
        analysis
            .memory_buffer(1, &names, Endian::Little, 0)
            .unwrap();
        let mut partial = 2u16.to_le_bytes().to_vec();
        partial.extend([64, 1, 2, 20, 88]);
        partial.extend(3u16.to_le_bytes());
        assert!(
            analysis
                .memory_buffer(1, &partial, Endian::Little, 5)
                .is_err()
        );
        assert_eq!(analysis.memory.events().len(), 1);
        assert!(
            analysis
                .memory_buffer(1, &[3, 0, 88], Endian::Little, 2)
                .unwrap_err()
                .to_string()
                .contains("moved backwards")
        );
        assert_eq!(analysis.memory.events().len(), 1);
    }
}
