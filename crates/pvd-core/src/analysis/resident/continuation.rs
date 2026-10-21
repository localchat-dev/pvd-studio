use super::*;
use crate::analysis::{Budget, MemorySample, ProfileSpan, ProfileValue};
#[cfg(test)]
use serde::de::DeserializeSeed;

mod buffer;
mod containers;
mod emergency;
pub(crate) use emergency::EmergencyReset;
mod ingest;
mod memory;
pub(crate) use memory::ResidentCursor;
pub(super) mod pages;
#[cfg(test)]
mod real_tests;
mod retain;
mod scratch;
#[cfg(test)]
mod seed;
#[cfg(test)]
mod tests;
mod tree;
mod view;
use buffer::{Buffer, BufferEdit};
#[cfg(test)]
use buffer::{BufferDraft, BufferView};
use scratch::ScratchSet;
#[cfg(test)]
use tree::TreeDraft;
use tree::{Set, Tree, TreeEdit};
pub(crate) use view::ResidentMemoryCursor;

fn check_replay_cancelled(cancelled: &mut dyn FnMut() -> bool) -> Result<(), Error> {
    crate::check_replay_cancelled(&mut || cancelled())
}

#[derive(Clone, Serialize)]
pub(crate) struct ZoneData {
    pub name: Text,
    events: Tree<u16, Text>,
}

#[derive(Clone, Serialize)]
pub(crate) struct StateData {
    pub live: ResidentMap,
    pub live_bytes: u64,
    pub total_allocated_bytes: u64,
    pub total_freed_bytes: u64,
    pub allocations: u64,
    pub frees: u64,
    pub unmatched_frees: u64,
    pub replaced_addresses: u64,
}

#[derive(Clone, Serialize)]
pub(crate) struct StateRef(Owned<StateData>);
impl Deref for StateRef {
    type Target = StateData;
    fn deref(&self) -> &StateData {
        &self.0
    }
}

#[derive(Clone, Serialize)]
pub(crate) enum EventKind {
    Allocate {
        allocation: ResidentValue,
        replaced: Option<ResidentValue>,
    },
    Free {
        allocation: Option<ResidentValue>,
    },
}
#[derive(Clone, Serialize)]
pub(crate) struct EventData {
    pub buffer: u64,
    pub address: u64,
    pub received_frame: usize,
    pub kind: EventKind,
}
#[derive(Clone, Serialize)]
pub(crate) struct EventRef(Owned<EventData>);
impl Deref for EventRef {
    type Target = EventData;
    fn deref(&self) -> &EventData {
        &self.0
    }
}
impl EventRef {
    pub(crate) fn same(&self, other: &Self) -> bool {
        self.0.same(&other.0)
    }
}

#[derive(Clone, Serialize)]
pub(crate) struct MemoryData {
    pub live: ResidentMap,
    samples: Buffer<MemorySample>,
    pub live_bytes: u64,
    pub peak_bytes: u64,
    pub total_allocated_bytes: u64,
    pub allocations: u64,
    pub frees: u64,
    pub unmatched_frees: u64,
    pub replaced_addresses: u64,
    strings: Tree<(u64, u32), Text>,
    events: Buffer<EventRef>,
    pub base: StateRef,
    pub first_retained_frame: usize,
}
#[cfg(test)]
impl MemoryData {
    pub(crate) fn samples(&self) -> BufferView<'_, MemorySample> {
        self.samples.view()
    }
    pub(crate) fn events(&self) -> BufferView<'_, EventRef> {
        self.events.view()
    }
    pub(crate) fn string(&self, buffer: u64, handle: u32) -> Option<&Text> {
        self.strings.get(&(buffer, handle))
    }
}

#[derive(Clone, Serialize)]
struct Continuation {
    first_retained_frame: usize,
    zones: Tree<u64, ZoneData>,
    spans: Buffer<ProfileSpan>,
    values: Buffer<ProfileValue>,
    unmatched_stops: u64,
    unmatched_stop_events: Tree<(u64, u16, u32, u64), u64>,
    legacy_cuda_buffers: u64,
    memory: Owned<MemoryData>,
    defined_zones: Set<u64>,
    ambiguous_zones: Set<u64>,
    ambiguous_events: Set<(u64, u16)>,
    starts: Tree<(u64, u16, u32, u64), Buffer<u64>>,
    pending_starts: usize,
    budget: Budget,
}

#[derive(Clone)]
pub(crate) struct ResidentAnalysis {
    domain: Domain,
    data: Owned<Continuation>,
    revision: u64,
}
impl Serialize for ResidentAnalysis {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.data.serialize(serializer)
    }
}
impl ResidentAnalysis {
    #[cfg(test)]
    pub(crate) fn from_legacy_bytes(
        bytes: &[u8],
        domain: Domain,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<Self, Error> {
        Self::decode_legacy(bytes, domain, cancelled, false)
    }
    #[cfg(test)]
    pub(crate) fn from_legacy_record_bytes(
        bytes: &[u8],
        domain: Domain,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<Self, Error> {
        Self::decode_legacy(bytes, domain, cancelled, true)
    }
    #[cfg(test)]
    fn decode_legacy(
        bytes: &[u8],
        domain: Domain,
        cancelled: &mut dyn FnMut() -> bool,
        record: bool,
    ) -> Result<Self, Error> {
        if bytes.len() > 64 << 20 {
            return Err(error(
                "resident analysis exceeds the history record byte limit",
            ));
        }
        let mut context = seed::Context {
            domain: &domain,
            cancelled,
            failure: None,
        };
        let mut deserializer = postcard::Deserializer::from_bytes(bytes);
        let decoded = if record {
            seed::RecordSeed(&mut context).deserialize(&mut deserializer)
        } else {
            seed::Seed::<Continuation>::new(&mut context).deserialize(&mut deserializer)
        };
        let data = decoded
            .map_err(|fallback| context.failure.take().unwrap_or_else(|| fallback.into()))?;
        if !deserializer.finalize()?.is_empty() {
            return Err(error("resident analysis has trailing wire bytes"));
        }
        data.validate()?;
        check_replay_cancelled(context.cancelled)?;
        let data = Owned::new(data, &domain, 0)?;
        Ok(Self {
            domain,
            data,
            revision: crate::analysis::ANALYSIS_REVISION.fetch_add(1, Ordering::Relaxed),
        })
    }
    pub(crate) fn memory(&self) -> &MemoryData {
        &self.data.memory
    }
    #[cfg(test)]
    pub(crate) fn spans(&self) -> BufferView<'_, ProfileSpan> {
        self.data.spans.view()
    }
    #[cfg(test)]
    pub(crate) fn values(&self) -> BufferView<'_, ProfileValue> {
        self.data.values.view()
    }
    #[cfg(test)]
    pub(crate) fn zone(&self, id: u64) -> Option<&ZoneData> {
        self.data.zones.get(&id)
    }
    pub(crate) fn domain(&self) -> &Domain {
        &self.domain
    }
    pub(crate) fn accounted_bytes(&self) -> usize {
        self.data.budget.used
    }
    #[cfg(test)]
    pub(crate) fn open_spans(&self) -> usize {
        self.data.pending_starts
    }
    #[cfg(test)]
    pub(crate) fn same(&self, other: &Self) -> bool {
        self.data.same(&other.data)
    }
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }
    #[cfg(test)]
    pub(crate) fn legacy_record(&self) -> LegacyRecordRef<'_> {
        LegacyRecordRef(self)
    }
}

#[cfg(test)]
pub(crate) struct LegacyRecordRef<'a>(&'a ResidentAnalysis);
#[cfg(test)]
impl Serialize for LegacyRecordRef<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_newtype_variant("Record", 5, "Analysis", self.0)
    }
}

impl Continuation {
    fn validate(&self) -> Result<(), Error> {
        let pending = self
            .starts
            .values()
            .try_fold(0usize, |n, values| add(n, values.len()))?;
        if pending != self.pending_starts
            || self.budget.used > self.budget.limit
            || self.budget.limit > crate::analysis::MAX_ANALYSIS_BYTES
            || self.memory.live.len() > 1_000_000
            || self.memory.base.live.len() > 1_000_000
            || self.memory.strings.len() > 1_000_000
        {
            return Err(error("resident analysis continuation invariants disagree"));
        }
        let memory = &self.memory;
        if memory
            .events
            .view()
            .iter()
            .zip(memory.events.view().iter().skip(1))
            .any(|(a, b)| a.received_frame > b.received_frame)
            || memory
                .samples
                .view()
                .iter()
                .zip(memory.samples.view().iter().skip(1))
                .any(|(a, b)| a.received_frame > b.received_frame)
            || memory
                .events
                .view()
                .first()
                .is_some_and(|e| e.received_frame < memory.first_retained_frame)
        {
            return Err(error("resident memory continuation frames disagree"));
        }
        for map in [&memory.live, &memory.base.live] {
            map.validate()?;
            if map.iter().any(|(key, value)| key.1 != value.address) {
                return Err(error("resident memory map address disagrees"));
            }
        }
        Ok(())
    }
}
