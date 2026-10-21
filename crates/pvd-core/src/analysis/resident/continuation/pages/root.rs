use super::*;

#[cfg(test)]
mod diagnostics;

const MAGIC: &[u8; 8] = b"PVDAFC02";

pub(super) struct WriteWorkspace {
    domain: Domain,
    _reservation: owner::Reservation,
}
impl WriteWorkspace {
    pub(super) fn new(domain: &Domain) -> Result<Self, Error> {
        Ok(Self {
            domain: domain.clone(),
            _reservation: domain.reserve(64 << 10)?,
        })
    }
}

struct MemoryWire {
    live: MapRef,
    samples: BufferRef,
    counters: [u64; 7],
    strings: TreeRef<(u64, u32)>,
    events: BufferRef,
    base: MapRef,
    base_counters: [u64; 7],
    first_retained: u64,
}
struct RootWire {
    first_retained: u64,
    zones: TreeRef<u64>,
    spans: BufferRef,
    values: BufferRef,
    unmatched_stops: u64,
    unmatched_events: TreeRef<(u64, u16, u32, u64)>,
    legacy_cuda: u64,
    memory: MemoryWire,
    defined: TreeRef<u64>,
    ambiguous: TreeRef<u64>,
    ambiguous_events: TreeRef<(u64, u16)>,
    starts: TreeRef<(u64, u16, u32, u64)>,
    pending: u64,
    budget_used: u64,
    budget_limit: u64,
}

fn temporary_encoder() -> Encoder {
    Encoder { bytes: Vec::new() }
}
fn text_wire<I: PageIo, C: FnMut() -> bool>(
    text: &Text,
    cx: &mut WriteContext<'_, I, C>,
) -> Result<ValueWire, Error> {
    let reference = cx.text(text)?;
    let mut out = temporary_encoder();
    out.row(reference);
    ValueWire::new(&out.bytes, &[reference.page])
}
fn read_text<I: PageIo, C: FnMut() -> bool>(
    value: &ValueWire,
    cx: &mut ReadContext<'_, I, C>,
) -> Result<Text, Error> {
    let mut input = Decoder {
        bytes: value.data(),
        offset: 0,
    };
    let reference = input.row()?;
    input.finish()?;
    value.check_dependencies(&[reference.page])?;
    cx.text(reference)
}
fn unit_wire<C: PageWrite>(_: &(), _: &mut C) -> Result<ValueWire, Error> {
    ValueWire::new(&[], &[])
}
fn read_unit<C: PageRead>(wire: &ValueWire, _: &mut C) -> Result<(), Error> {
    if !wire.data().is_empty() {
        return Err(error("unit metadata has payload"));
    }
    wire.check_dependencies(&[])
}
fn integer_wire<C: PageWrite>(value: &u64, _: &mut C) -> Result<ValueWire, Error> {
    ValueWire::new(&value.to_le_bytes(), &[])
}
fn read_integer<C: PageRead>(wire: &ValueWire, _: &mut C) -> Result<u64, Error> {
    wire.check_dependencies(&[])?;
    Ok(u64::from_le_bytes(wire.data().try_into()?))
}

pub(super) fn write<I: PageIo, C: FnMut() -> bool>(
    analysis: &ResidentAnalysis,
    old: &FullMemos<I::Store>,
    staged: &mut FullMemos<I::Store>,
    cx: &mut WriteContext<'_, I, C>,
    workspace: &WriteWorkspace,
) -> Result<u64, Error> {
    cx.check()?;
    if !workspace.domain.same(cx.domain) {
        return Err(error("analysis write workspace belongs to another domain"));
    }
    let data = &analysis.data;
    let memory = &data.memory;
    let live = cx.maps.map(&memory.live)?;
    let spans = sequences::write(&data.spans, &old.spans, &mut staged.spans, cx)?;
    let values = sequences::write(&data.values, &old.values, &mut staged.values, cx)?;
    let samples = sequences::write(&memory.samples, &old.samples, &mut staged.samples, cx)?;
    let events = sequences::write(
        &memory.events,
        &old.memory_events,
        &mut staged.memory_events,
        cx,
    )?;
    let strings = metadata::write(
        &memory.strings,
        &old.strings,
        &mut staged.strings,
        cx,
        &mut text_wire,
    )?;
    let unmatched_events = metadata::write(
        &data.unmatched_stop_events,
        &old.counts,
        &mut staged.counts,
        cx,
        &mut integer_wire,
    )?;
    let defined = metadata::write(
        &data.defined_zones.0,
        &old.sets,
        &mut staged.sets,
        cx,
        &mut unit_wire,
    )?;
    let ambiguous = metadata::write(
        &data.ambiguous_zones.0,
        &old.sets,
        &mut staged.sets,
        cx,
        &mut unit_wire,
    )?;
    let ambiguous_events = metadata::write(
        &data.ambiguous_events.0,
        &old.ambiguities,
        &mut staged.ambiguities,
        cx,
        &mut unit_wire,
    )?;
    let starts = metadata::write(
        &data.starts,
        &old.starts,
        &mut staged.starts,
        cx,
        &mut |ticks, cx| {
            let reference = sequences::write(ticks, &old.ticks, &mut staged.ticks, cx)?;
            let mut out = temporary_encoder();
            reference.encode(&mut out);
            ValueWire::new(&out.bytes, &[reference.root.page])
        },
    )?;
    let zones = metadata::write(
        &data.zones,
        &old.zones,
        &mut staged.zones,
        cx,
        &mut |zone, cx| {
            let name = cx.text(&zone.name)?;
            let events = metadata::write(
                &zone.events,
                &old.events,
                &mut staged.events,
                cx,
                &mut text_wire,
            )?;
            let mut out = temporary_encoder();
            out.row(name);
            events.encode(&mut out);
            ValueWire::new(&out.bytes, &[name.page, events.page])
        },
    )?;
    let base = &memory.base;
    let root = RootWire {
        first_retained: data.first_retained_frame.try_into()?,
        zones,
        spans,
        values,
        unmatched_stops: data.unmatched_stops,
        unmatched_events,
        legacy_cuda: data.legacy_cuda_buffers,
        memory: MemoryWire {
            live,
            samples,
            counters: [
                memory.live_bytes,
                memory.peak_bytes,
                memory.total_allocated_bytes,
                memory.allocations,
                memory.frees,
                memory.unmatched_frees,
                memory.replaced_addresses,
            ],
            strings,
            events,
            base: cx.maps.map(&base.live)?,
            base_counters: [
                base.live_bytes,
                base.total_allocated_bytes,
                base.total_freed_bytes,
                base.allocations,
                base.frees,
                base.unmatched_frees,
                base.replaced_addresses,
            ],
            first_retained: memory.first_retained_frame.try_into()?,
        },
        defined,
        ambiguous,
        ambiguous_events,
        starts,
        pending: data.pending_starts.try_into()?,
        budget_used: data.budget.used.try_into()?,
        budget_limit: data.budget.limit.try_into()?,
    };
    // Each root refers only to field roots; their registered dependencies retain the page graph.
    let bytes = root.encode();
    let page = cx.page(&bytes, &root.dependencies())?;
    staged.roots.put(data.identity(), data, page);
    Ok(page)
}

impl RootWire {
    fn dependencies(&self) -> Vec<u64> {
        dependency_ids(
            [
                self.zones.page,
                self.spans.root.page,
                self.values.root.page,
                self.unmatched_events.page,
                self.memory.live.page,
                self.memory.samples.root.page,
                self.memory.strings.page,
                self.memory.events.root.page,
                self.memory.base.page,
                self.defined.page,
                self.ambiguous.page,
                self.ambiguous_events.page,
                self.starts.page,
            ]
            .into_iter()
            .filter(|id| *id != 0),
        )
    }
    fn encode(&self) -> Vec<u8> {
        let mut out = Encoder::new(MAGIC);
        out.u64(self.first_retained);
        self.zones.encode(&mut out);
        self.spans.encode(&mut out);
        self.values.encode(&mut out);
        out.u64(self.unmatched_stops);
        self.unmatched_events.encode(&mut out);
        out.u64(self.legacy_cuda);
        out.map(self.memory.live);
        self.memory.samples.encode(&mut out);
        for count in self.memory.counters {
            out.u64(count);
        }
        self.memory.strings.encode(&mut out);
        self.memory.events.encode(&mut out);
        out.map(self.memory.base);
        for count in self.memory.base_counters {
            out.u64(count);
        }
        out.u64(self.memory.first_retained);
        self.defined.encode(&mut out);
        self.ambiguous.encode(&mut out);
        self.ambiguous_events.encode(&mut out);
        self.starts.encode(&mut out);
        out.u64(self.pending);
        out.u64(self.budget_used);
        out.u64(self.budget_limit);
        out.bytes
    }
    fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let mut input = Decoder::new(bytes, MAGIC)?;
        let first_retained = input.u64()?;
        let zones = TreeRef::decode(&mut input)?;
        let spans = BufferRef::decode(&mut input)?;
        let values = BufferRef::decode(&mut input)?;
        let unmatched_stops = input.u64()?;
        let unmatched_events = TreeRef::decode(&mut input)?;
        let legacy_cuda = input.u64()?;
        let live = input.map()?;
        let samples = BufferRef::decode(&mut input)?;
        let mut counters = [0; 7];
        for count in &mut counters {
            *count = input.u64()?;
        }
        let strings = TreeRef::decode(&mut input)?;
        let events = BufferRef::decode(&mut input)?;
        let base = input.map()?;
        let mut base_counters = [0; 7];
        for count in &mut base_counters {
            *count = input.u64()?;
        }
        let memory = MemoryWire {
            live,
            samples,
            counters,
            strings,
            events,
            base,
            base_counters,
            first_retained: input.u64()?,
        };
        let defined = TreeRef::decode(&mut input)?;
        let ambiguous = TreeRef::decode(&mut input)?;
        let ambiguous_events = TreeRef::decode(&mut input)?;
        let starts = TreeRef::decode(&mut input)?;
        let result = Self {
            first_retained,
            zones,
            spans,
            values,
            unmatched_stops,
            unmatched_events,
            legacy_cuda,
            memory,
            defined,
            ambiguous,
            ambiguous_events,
            starts,
            pending: input.u64()?,
            budget_used: input.u64()?,
            budget_limit: input.u64()?,
        };
        input.finish()?;
        if result.budget_used > result.budget_limit
            || result.budget_limit > crate::analysis::MAX_ANALYSIS_BYTES as u64
            || result.pending > crate::analysis::MAX_RECORDS as u64
        {
            return Err(error("analysis root budget or pending count invalid"));
        }
        Ok(result)
    }
}

pub(super) fn read<I: PageIo, C: FnMut() -> bool>(
    root: u64,
    registry: &mut FullRegistry<I::Store>,
    cx: &mut ReadContext<'_, I, C>,
) -> Result<ResidentAnalysis, Error> {
    #[cfg(test)]
    let mut diagnostic = diagnostics::ReadTrace::from_env(root);
    cx.check()?;
    let page = cx.load(root, MAX_PAGE_BYTES)?;
    let _scratch = cx.domain.reserve(64 << 10)?;
    let rootwire = RootWire::decode(&page.loaded.bytes)?;
    dependencies_match(&rootwire.dependencies(), &page.loaded.dependencies)?;
    let r = rootwire;
    #[cfg(test)]
    diagnostic.advance(diagnostics::Phase::LiveMap);
    let live = cx.map(r.memory.live)?;
    #[cfg(test)]
    diagnostic.advance(diagnostics::Phase::BaseMap);
    let base = cx.map(r.memory.base)?;
    #[cfg(test)]
    diagnostic.advance(diagnostics::Phase::Sequences);
    let spans = sequences::read(r.spans, &mut registry.spans, cx)?;
    let values = sequences::read(r.values, &mut registry.values, cx)?;
    let samples = sequences::read(r.memory.samples, &mut registry.samples, cx)?;
    let events = sequences::read(r.memory.events, &mut registry.memory_events, cx)?;
    #[cfg(test)]
    diagnostic.advance(diagnostics::Phase::Metadata);
    let strings = metadata::read(r.memory.strings, &mut registry.strings, cx, &mut read_text)?;
    let counts = metadata::read(
        r.unmatched_events,
        &mut registry.counts,
        cx,
        &mut read_integer,
    )?;
    let defined = metadata::read(r.defined, &mut registry.sets, cx, &mut read_unit)?;
    let ambiguous = metadata::read(r.ambiguous, &mut registry.sets, cx, &mut read_unit)?;
    let ambiguous_events = metadata::read(
        r.ambiguous_events,
        &mut registry.ambiguities,
        cx,
        &mut read_unit,
    )?;
    let starts = metadata::read(r.starts, &mut registry.starts, cx, &mut |wire, cx| {
        let mut input = Decoder {
            bytes: wire.data(),
            offset: 0,
        };
        let reference = BufferRef::decode(&mut input)?;
        input.finish()?;
        wire.check_dependencies(&[reference.root.page])?;
        sequences::read(reference, &mut registry.ticks, cx)
    })?;
    let zones = metadata::read(r.zones, &mut registry.zones, cx, &mut |wire, cx| {
        let mut input = Decoder {
            bytes: wire.data(),
            offset: 0,
        };
        let name = input.row()?;
        let events = TreeRef::decode(&mut input)?;
        input.finish()?;
        wire.check_dependencies(&[name.page, events.page])?;
        Ok(ZoneData {
            name: cx.text(name)?,
            events: metadata::read(events, &mut registry.events, cx, &mut read_text)?,
        })
    })?;
    #[cfg(test)]
    diagnostic.advance(diagnostics::Phase::Construction);
    let [
        live_bytes,
        total_allocated_bytes,
        total_freed_bytes,
        allocations,
        frees,
        unmatched_frees,
        replaced_addresses,
    ] = r.memory.base_counters;
    let base = StateRef(Owned::new(
        StateData {
            live: base,
            live_bytes,
            total_allocated_bytes,
            total_freed_bytes,
            allocations,
            frees,
            unmatched_frees,
            replaced_addresses,
        },
        cx.domain,
        0,
    )?);
    let [
        live_bytes,
        peak_bytes,
        total_allocated_bytes,
        allocations,
        frees,
        unmatched_frees,
        replaced_addresses,
    ] = r.memory.counters;
    let memory = Owned::new(
        MemoryData {
            live,
            samples,
            live_bytes,
            peak_bytes,
            total_allocated_bytes,
            allocations,
            frees,
            unmatched_frees,
            replaced_addresses,
            strings,
            events,
            base,
            first_retained_frame: r.memory.first_retained.try_into()?,
        },
        cx.domain,
        0,
    )?;
    let data = Continuation {
        first_retained_frame: r.first_retained.try_into()?,
        zones,
        spans,
        values,
        unmatched_stops: r.unmatched_stops,
        unmatched_stop_events: counts,
        legacy_cuda_buffers: r.legacy_cuda,
        memory,
        defined_zones: tree::Set(defined),
        ambiguous_zones: tree::Set(ambiguous),
        ambiguous_events: tree::Set(ambiguous_events),
        starts,
        pending_starts: r.pending.try_into()?,
        budget: Budget {
            used: r.budget_used.try_into()?,
            limit: r.budget_limit.try_into()?,
        },
    };
    #[cfg(test)]
    diagnostic.advance(diagnostics::Phase::Validate);
    data.validate()?;
    cx.check()?;
    #[cfg(test)]
    diagnostic.advance(diagnostics::Phase::Publish);
    let data = Owned::new(data, cx.domain, 0)?;
    let revision = crate::analysis::ANALYSIS_REVISION.fetch_add(1, Ordering::Relaxed);
    registry.roots.put(root, &data, revision);
    #[cfg(test)]
    diagnostic.finish();
    Ok(ResidentAnalysis {
        domain: cx.domain.clone(),
        data,
        revision,
    })
}
