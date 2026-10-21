use super::*;

impl<S: Clone> FullMemos<S> {
    pub fn register<I: PageIo<Store = S>>(
        &self,
        registry: &mut FullRegistry<S>,
        io: &I,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), Error> {
        self.ticks.register(&mut registry.ticks, io, cancelled)?;
        self.spans.register(&mut registry.spans, io, cancelled)?;
        self.values.register(&mut registry.values, io, cancelled)?;
        self.samples
            .register(&mut registry.samples, io, cancelled)?;
        self.memory_events
            .register(&mut registry.memory_events, io, cancelled)?;
        self.zones.register(&mut registry.zones, io, cancelled)?;
        self.events.register(&mut registry.events, io, cancelled)?;
        self.counts.register(&mut registry.counts, io, cancelled)?;
        self.sets.register(&mut registry.sets, io, cancelled)?;
        self.strings
            .register(&mut registry.strings, io, cancelled)?;
        self.starts.register(&mut registry.starts, io, cancelled)?;
        self.ambiguities
            .register(&mut registry.ambiguities, io, cancelled)?;
        Ok(())
    }
}

pub(super) struct FullMemos<S> {
    pub roots: Cache<usize, Continuation, u64>,
    pub ticks: BufferMemos<u64, S>,
    pub spans: BufferMemos<ProfileSpan, S>,
    pub values: BufferMemos<ProfileValue, S>,
    pub samples: BufferMemos<MemorySample, S>,
    pub memory_events: BufferMemos<EventRef, S>,
    pub zones: TreeMemos<u64, ZoneData, S>,
    pub events: TreeMemos<u16, Text, S>,
    pub counts: TreeMemos<(u64, u16, u32, u64), u64, S>,
    pub sets: TreeMemos<u64, (), S>,
    pub strings: TreeMemos<(u64, u32), Text, S>,
    pub starts: TreeMemos<(u64, u16, u32, u64), Buffer<u64>, S>,
    pub ambiguities: TreeMemos<(u64, u16), (), S>,
}
impl<S: Clone> FullMemos<S> {
    pub fn new(domain: &Domain, store: S, limits: &FullLimits) -> Result<Self, Error> {
        Ok(Self {
            roots: Cache::dynamic(limits.root_slots, domain)?,
            ticks: BufferMemos::new(
                domain,
                store.clone(),
                limits.sequence_node_slots,
                limits.sequence_chunk_slots,
            )?,
            spans: BufferMemos::new(
                domain,
                store.clone(),
                limits.sequence_node_slots,
                limits.sequence_chunk_slots,
            )?,
            values: BufferMemos::new(
                domain,
                store.clone(),
                limits.sequence_node_slots,
                limits.sequence_chunk_slots,
            )?,
            samples: BufferMemos::new(
                domain,
                store.clone(),
                limits.sequence_node_slots,
                limits.sequence_chunk_slots,
            )?,
            memory_events: BufferMemos::new(
                domain,
                store.clone(),
                limits.sequence_node_slots,
                limits.sequence_chunk_slots,
            )?,
            zones: TreeMemos::new(domain, store.clone(), 1, limits.metadata_slots)?,
            events: TreeMemos::new(domain, store.clone(), 2, limits.metadata_slots)?,
            counts: TreeMemos::new(domain, store.clone(), 3, limits.metadata_slots)?,
            sets: TreeMemos::new(domain, store.clone(), 4, limits.metadata_slots)?,
            strings: TreeMemos::new(domain, store.clone(), 5, limits.metadata_slots)?,
            starts: TreeMemos::new(domain, store.clone(), 6, limits.metadata_slots)?,
            ambiguities: TreeMemos::new(domain, store.clone(), 7, limits.metadata_slots)?,
        })
    }
    pub fn clear(&mut self) {
        self.roots.clear();
        self.ticks.clear();
        self.spans.clear();
        self.values.clear();
        self.samples.clear();
        self.memory_events.clear();
        self.zones.clear();
        self.events.clear();
        self.counts.clear();
        self.sets.clear();
        self.strings.clear();
        self.starts.clear();
        self.ambiguities.clear();
    }
    pub fn capacity_bytes(&self) -> usize {
        self.roots.capacity_bytes()
            + self.ticks.capacity_bytes()
            + self.spans.capacity_bytes()
            + self.values.capacity_bytes()
            + self.samples.capacity_bytes()
            + self.memory_events.capacity_bytes()
            + self.zones.capacity_bytes()
            + self.events.capacity_bytes()
            + self.counts.capacity_bytes()
            + self.sets.capacity_bytes()
            + self.strings.capacity_bytes()
            + self.starts.capacity_bytes()
            + self.ambiguities.capacity_bytes()
    }
    pub fn absorb<I: PageIo<Store = S>>(&mut self, other: Self, io: &I) -> Result<(), Error> {
        self.roots.absorb(other.roots)?;
        self.ticks.absorb(other.ticks, io)?;
        self.spans.absorb(other.spans, io)?;
        self.values.absorb(other.values, io)?;
        self.samples.absorb(other.samples, io)?;
        self.memory_events.absorb(other.memory_events, io)?;
        self.zones.absorb(other.zones, io)?;
        self.events.absorb(other.events, io)?;
        self.counts.absorb(other.counts, io)?;
        self.sets.absorb(other.sets, io)?;
        self.strings.absorb(other.strings, io)?;
        self.starts.absorb(other.starts, io)?;
        self.ambiguities.absorb(other.ambiguities, io)?;
        Ok(())
    }
    pub fn statistics(&self) -> FullStatistics {
        let mut stats = FullStatistics {
            cache_capacity_bytes: self.capacity_bytes(),
            ..Default::default()
        };
        stats.sequence_nodes += self.ticks.statistics.encoded_nodes;
        stats.sequence_chunks += self.ticks.statistics.encoded_chunks;
        stats.sequence_elements += self.ticks.statistics.encoded_elements;
        stats.sequence_nodes += self.spans.statistics.encoded_nodes;
        stats.sequence_chunks += self.spans.statistics.encoded_chunks;
        stats.sequence_elements += self.spans.statistics.encoded_elements;
        stats.sequence_nodes += self.values.statistics.encoded_nodes;
        stats.sequence_chunks += self.values.statistics.encoded_chunks;
        stats.sequence_elements += self.values.statistics.encoded_elements;
        stats.sequence_nodes += self.samples.statistics.encoded_nodes;
        stats.sequence_chunks += self.samples.statistics.encoded_chunks;
        stats.sequence_elements += self.samples.statistics.encoded_elements;
        stats.sequence_nodes += self.memory_events.statistics.encoded_nodes;
        stats.sequence_chunks += self.memory_events.statistics.encoded_chunks;
        stats.sequence_elements += self.memory_events.statistics.encoded_elements;
        stats.metadata_nodes += self.zones.visited_nodes;
        stats.metadata_nodes += self.events.visited_nodes;
        stats.metadata_nodes += self.counts.visited_nodes;
        stats.metadata_nodes += self.sets.visited_nodes;
        stats.metadata_nodes += self.strings.visited_nodes;
        stats.metadata_nodes += self.starts.visited_nodes;
        stats.metadata_nodes += self.ambiguities.visited_nodes;
        stats
    }
}

pub(super) struct FullRegistry<S> {
    pub roots: Cache<u64, Continuation, u64>,
    pub ticks: BufferRegistry<u64, S>,
    pub spans: BufferRegistry<ProfileSpan, S>,
    pub values: BufferRegistry<ProfileValue, S>,
    pub samples: BufferRegistry<MemorySample, S>,
    pub memory_events: BufferRegistry<EventRef, S>,
    pub zones: TreeRegistry<u64, ZoneData, S>,
    pub events: TreeRegistry<u16, Text, S>,
    pub counts: TreeRegistry<(u64, u16, u32, u64), u64, S>,
    pub sets: TreeRegistry<u64, (), S>,
    pub strings: TreeRegistry<(u64, u32), Text, S>,
    pub starts: TreeRegistry<(u64, u16, u32, u64), Buffer<u64>, S>,
    pub ambiguities: TreeRegistry<(u64, u16), (), S>,
}
impl<S: Clone> FullRegistry<S> {
    pub fn new(domain: &Domain, store: S, limits: &FullLimits) -> Result<Self, Error> {
        Ok(Self {
            roots: Cache::live(limits.root_slots, domain)?,
            ticks: BufferRegistry::new(
                domain,
                store.clone(),
                limits.sequence_node_slots,
                limits.sequence_chunk_slots,
            )?,
            spans: BufferRegistry::new(
                domain,
                store.clone(),
                limits.sequence_node_slots,
                limits.sequence_chunk_slots,
            )?,
            values: BufferRegistry::new(
                domain,
                store.clone(),
                limits.sequence_node_slots,
                limits.sequence_chunk_slots,
            )?,
            samples: BufferRegistry::new(
                domain,
                store.clone(),
                limits.sequence_node_slots,
                limits.sequence_chunk_slots,
            )?,
            memory_events: BufferRegistry::new(
                domain,
                store.clone(),
                limits.sequence_node_slots,
                limits.sequence_chunk_slots,
            )?,
            zones: TreeRegistry::new(domain, store.clone(), 1, limits.metadata_slots)?,
            events: TreeRegistry::new(domain, store.clone(), 2, limits.metadata_slots)?,
            counts: TreeRegistry::new(domain, store.clone(), 3, limits.metadata_slots)?,
            sets: TreeRegistry::new(domain, store.clone(), 4, limits.metadata_slots)?,
            strings: TreeRegistry::new(domain, store.clone(), 5, limits.metadata_slots)?,
            starts: TreeRegistry::new(domain, store.clone(), 6, limits.metadata_slots)?,
            ambiguities: TreeRegistry::new(domain, store.clone(), 7, limits.metadata_slots)?,
        })
    }
    pub fn clear(&mut self) {
        self.roots.clear();
        self.ticks.clear();
        self.spans.clear();
        self.values.clear();
        self.samples.clear();
        self.memory_events.clear();
        self.zones.clear();
        self.events.clear();
        self.counts.clear();
        self.sets.clear();
        self.strings.clear();
        self.starts.clear();
        self.ambiguities.clear();
    }
    pub fn reclaim(&mut self, cancelled: &mut impl FnMut() -> bool) -> Result<(), Error> {
        self.roots.reclaim_dead(cancelled)?;
        self.ticks.reclaim(cancelled)?;
        self.spans.reclaim(cancelled)?;
        self.values.reclaim(cancelled)?;
        self.samples.reclaim(cancelled)?;
        self.memory_events.reclaim(cancelled)?;
        self.zones.reclaim(cancelled)?;
        self.events.reclaim(cancelled)?;
        self.counts.reclaim(cancelled)?;
        self.sets.reclaim(cancelled)?;
        self.strings.reclaim(cancelled)?;
        self.starts.reclaim(cancelled)?;
        self.ambiguities.reclaim(cancelled)
    }
    pub fn capacity_bytes(&self) -> usize {
        self.roots.capacity_bytes()
            + self.ticks.capacity_bytes()
            + self.spans.capacity_bytes()
            + self.values.capacity_bytes()
            + self.samples.capacity_bytes()
            + self.memory_events.capacity_bytes()
            + self.zones.capacity_bytes()
            + self.events.capacity_bytes()
            + self.counts.capacity_bytes()
            + self.sets.capacity_bytes()
            + self.strings.capacity_bytes()
            + self.starts.capacity_bytes()
            + self.ambiguities.capacity_bytes()
    }
    pub fn reset_statistics(&mut self) {
        self.ticks.statistics = sequences::BufferStatistics::default();
        self.spans.statistics = sequences::BufferStatistics::default();
        self.values.statistics = sequences::BufferStatistics::default();
        self.samples.statistics = sequences::BufferStatistics::default();
        self.memory_events.statistics = sequences::BufferStatistics::default();
        self.zones.loaded_pages = 0;
        self.zones.cached_pages = 0;
        self.events.loaded_pages = 0;
        self.events.cached_pages = 0;
        self.counts.loaded_pages = 0;
        self.counts.cached_pages = 0;
        self.sets.loaded_pages = 0;
        self.sets.cached_pages = 0;
        self.strings.loaded_pages = 0;
        self.strings.cached_pages = 0;
        self.starts.loaded_pages = 0;
        self.starts.cached_pages = 0;
        self.ambiguities.loaded_pages = 0;
        self.ambiguities.cached_pages = 0;
    }
}
