use super::*;
use crate::debug_draw::scan_context;
use pvd_protocol::Endian;

#[derive(Debug)]
struct Renderer {
    class: u32,
    property: u32,
    initial: bool,
    end_alive: bool,
    seed: Option<u64>,
}

struct Track<'a> {
    class: u32,
    property: u32,
    created: usize,
    previous: usize,
    alive: bool,
    initial: bool,
    selected: Option<u64>,
    captured: Option<u64>,
    awaiting_context: bool,
    last: &'a [u8],
    first_write: Option<usize>,
    seed: Option<u64>,
}

#[derive(Debug)]
pub(super) struct Proof {
    renderers: BTreeMap<u32, Renderer>,
    retained_events: usize,
    initial: blake3::Hash,
    history: blake3::Hash,
    allocation: usize,
    work_remaining: usize,
}

struct Work {
    remaining: usize,
}

impl Default for Work {
    fn default() -> Self {
        Self { remaining: 1 << 30 }
    }
}

impl Work {
    fn charge(&mut self, bytes: usize) -> Result<(), Error> {
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .ok_or_else(|| error("renderer context proof exceeds its work budget"))?;
        Ok(())
    }
}

fn field(
    hash: &mut blake3::Hasher,
    bytes: &[u8],
    work: &mut Work,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    check(proceed)?;
    work.charge(
        bytes
            .len()
            .checked_add(8)
            .ok_or_else(|| error("renderer context proof size overflow"))?,
    )?;
    hash.update(&(bytes.len() as u64).to_le_bytes());
    for chunk in bytes.chunks(64 * 1024) {
        check(proceed)?;
        hash.update(chunk);
    }
    Ok(())
}

fn property(
    layout: &Layout<'_>,
    id: u32,
    work: &mut Work,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<Option<u32>, Error> {
    let mut ancestor = id;
    let mut found = false;
    for _ in 0..128 {
        check(proceed)?;
        work.charge(4)?;
        let class = layout.metadata.class(ancestor)?;
        if layout
            .metadata
            .strings
            .get(&class.name.name)
            .map(String::as_str)
            == Some("PvdUserRenderer")
        {
            if ancestor != id {
                return Err(error(
                    "renderer context does not support derived renderer classes",
                ));
            }
            found = true;
            break;
        }
        if class.base == u32::MAX {
            return Ok(None);
        }
        ancestor = class.base;
    }
    if !found {
        return Err(error("renderer context has invalid class inheritance"));
    }
    let metadata = &layout.metadata;
    let class = metadata.class(id)?;
    let label = |id| metadata.strings.get(&id).map(String::as_str);
    if label(class.name.namespace) != Some("_debugger_")
        || class.base != u32::MAX
        || !matches!(class.size, 12 | 24)
        || class.has_pointers
        || !class.string_offsets.is_empty()
        || class.properties.len() != 1
        || layout
            .pointer_layouts
            .get(id as usize)
            .is_none_or(|p| !p.is_empty())
    {
        return Err(error("unsupported renderer context class layout"));
    }
    let id = class.properties[0];
    let property = metadata.property(id)?;
    let datatype = metadata.class(property.datatype_id)?;
    if property.owner != class.name
        || label(property.name) != Some("events")
        || property.kind != 2
        || property.offset32 != 0
        || label(property.datatype.namespace) != Some("physx3")
        || label(property.datatype.name) != Some("PvdU8")
        || datatype.name != property.datatype
        || datatype.size != 1
        || datatype.base != u32::MAX
        || !datatype.properties.is_empty()
        || datatype.dynamic
        || datatype.has_pointers
        || !datatype.string_offsets.is_empty()
        || layout
            .pointer_layouts
            .get(property.datatype_id as usize)
            .is_none_or(|p| !p.is_empty())
    {
        return Err(error("unsupported renderer context event property"));
    }
    Ok(Some(id))
}

fn source_undo(
    layout: &Layout<'_>,
    current: usize,
    id: u32,
    track: &Track<'_>,
    references: &[u32],
    work: &mut Work,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    let mut last = None;
    for &distance in references {
        check(proceed)?;
        work.charge(4)?;
        let target = current
            .checked_sub(distance as usize)
            .filter(|target| *target < current)
            .ok_or_else(|| error("renderer source undo must target preceding history"))?;
        if last.is_some_and(|last| target <= last) {
            return Err(error(
                "renderer source undo targets must be ordered and distinct",
            ));
        }
        match &layout.events[target] {
            HistoryEvent::Create(candidate, _) if *candidate == id => (),
            HistoryEvent::Set {
                kind: 3,
                id: candidate,
                property: field,
                ..
            } if *candidate == id && *field == track.property => (),
            _ => {
                return Err(error(
                    "renderer source undo targets another object or operation",
                ));
            }
        }
        last = Some(target);
    }
    if last != Some(track.previous) {
        return Err(error(
            "renderer source undo does not restore its preceding complete buffer",
        ));
    }
    Ok(())
}

fn initial_hash(
    layout: &Layout<'_>,
    renderers: &BTreeMap<u32, Renderer>,
    values: &BTreeMap<u32, &[u8]>,
    reverse_rows: Option<&[(u64, u32)]>,
    work: &mut Work,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<blake3::Hash, Error> {
    let mut hash = blake3::Hasher::new();
    field(&mut hash, b"middle-renderer-initial-v1", work, proceed)?;
    field(&mut hash, layout.object_bytes, work, proceed)?;
    for (&id, renderer) in renderers {
        check(proceed)?;
        for value in [id, renderer.class, renderer.property] {
            field(&mut hash, &value.to_le_bytes(), work, proceed)?;
        }
        field(&mut hash, &[u8::from(renderer.initial)], work, proceed)?;
        if renderer.initial {
            field(
                &mut hash,
                values
                    .get(&id)
                    .ok_or_else(|| error("renderer context lost its captured initial property"))?,
                work,
                proceed,
            )?;
        }
        let mut mapped = 0u32;
        for instance in &layout.communication.instances {
            check(proceed)?;
            work.charge(24)?;
            if instance.id == id {
                mapped += 1;
                if mapped > 1 {
                    return Err(error(
                        "renderer context has duplicate captured source identities",
                    ));
                }
                field(&mut hash, &instance.source.to_le_bytes(), work, proceed)?;
                field(&mut hash, &instance.flags, work, proceed)?;
            }
        }
        field(&mut hash, &mapped.to_le_bytes(), work, proceed)?;
    }
    let mut reverse = 0usize;
    for &(source, id) in reverse_rows.unwrap_or(&layout.communication.reverse) {
        check(proceed)?;
        work.charge(16)?;
        if renderers.contains_key(&id) {
            reverse += 1;
            field(&mut hash, &source.to_le_bytes(), work, proceed)?;
            field(&mut hash, &id.to_le_bytes(), work, proceed)?;
        }
    }
    field(&mut hash, &(reverse as u64).to_le_bytes(), work, proceed)?;
    Ok(hash.finalize())
}

#[derive(Clone, Copy)]
enum Coordinates<'a> {
    Source {
        start: usize,
    },
    Output {
        leading: usize,
        seeds: &'a BTreeMap<u32, usize>,
    },
}

fn undo(
    hash: &mut blake3::Hasher,
    id: u32,
    current: usize,
    references: &[u32],
    coordinates: Coordinates<'_>,
    work: &mut Work,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    let mut previous = None;
    let mut anchor = false;
    let mut retained = 0usize;
    if references.is_empty() {
        return Err(error(
            "renderer context requires an explicit complete-buffer undo",
        ));
    }
    for &distance in references {
        check(proceed)?;
        work.charge(4)?;
        let target = current
            .checked_sub(distance as usize)
            .filter(|target| *target < current)
            .ok_or_else(|| error("renderer undo target must precede its event"))?;
        if previous.is_some_and(|last| target <= last) {
            return Err(error("renderer undo targets must be ordered and distinct"));
        }
        previous = Some(target);
        match coordinates {
            Coordinates::Source { start } if target < start => anchor = true,
            Coordinates::Output { leading, seeds } if target < leading => {
                if target != seeds.get(&id).copied().unwrap_or(0) || anchor {
                    return Err(error(
                        "renderer output undo targets the wrong initial buffer",
                    ));
                }
                anchor = true;
            }
            _ => retained += 1,
        }
    }
    field(hash, &[u8::from(anchor)], work, proceed)?;
    field(hash, &(retained as u64).to_le_bytes(), work, proceed)?;
    for &distance in references {
        check(proceed)?;
        let target = current - distance as usize;
        let keep = match coordinates {
            Coordinates::Source { start } => target >= start,
            Coordinates::Output { leading, .. } => target >= leading,
        };
        if keep {
            field(hash, &distance.to_le_bytes(), work, proceed)?;
        }
    }
    Ok(())
}

fn history_hash(
    layout: &Layout<'_>,
    renderers: &BTreeMap<u32, Renderer>,
    range: std::ops::Range<usize>,
    coordinates: Coordinates<'_>,
    work: &mut Work,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<blake3::Hash, Error> {
    let start = range.start;
    let mut hash = blake3::Hasher::new();
    field(&mut hash, b"middle-renderer-history-v1", work, proceed)?;
    for (offset, event) in layout.events[range].iter().enumerate() {
        check(proceed)?;
        work.charge(16)?;
        let id = match event {
            HistoryEvent::Create(id, _)
            | HistoryEvent::Destroy(id)
            | HistoryEvent::Set { id, .. }
            | HistoryEvent::Array { id, .. }
            | HistoryEvent::Saved { id, .. } => *id,
            _ => continue,
        };
        let Some(renderer) = renderers.get(&id) else {
            continue;
        };
        field(&mut hash, &(offset as u64).to_le_bytes(), work, proceed)?;
        field(&mut hash, &id.to_le_bytes(), work, proceed)?;
        match event {
            HistoryEvent::Create(_, class) => {
                field(&mut hash, &1u32.to_le_bytes(), work, proceed)?;
                field(&mut hash, &class.to_le_bytes(), work, proceed)?;
            }
            HistoryEvent::Destroy(_) => field(&mut hash, &2u32.to_le_bytes(), work, proceed)?,
            HistoryEvent::Set {
                kind: 3,
                property,
                data,
                undo: references,
                ..
            } if *property == renderer.property => {
                field(&mut hash, &3u32.to_le_bytes(), work, proceed)?;
                field(&mut hash, &property.to_le_bytes(), work, proceed)?;
                field(&mut hash, data, work, proceed)?;
                undo(
                    &mut hash,
                    id,
                    start + offset,
                    references,
                    coordinates,
                    work,
                    proceed,
                )?;
            }
            HistoryEvent::Saved { position, .. } => {
                field(&mut hash, &10u32.to_le_bytes(), work, proceed)?;
                let mut saved = None;
                for record in &layout.saved {
                    check(proceed)?;
                    work.charge(16)?;
                    if record.position == *position && record.id == id {
                        saved = Some(record);
                        break;
                    }
                }
                let saved = saved.ok_or_else(|| error("renderer has no exact saved witness"))?;
                field(&mut hash, &saved.class.to_le_bytes(), work, proceed)?;
                field(&mut hash, &saved.body, work, proceed)?;
            }
            _ => {
                return Err(error(
                    "renderer context requires complete captured property buffers",
                ));
            }
        }
    }
    Ok(hash.finalize())
}

impl Proof {
    pub(super) fn prepare(
        layout: &Layout<'_>,
        start: usize,
        end: usize,
        previous_end: usize,
        first_end: usize,
        allocation: &mut usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Option<Self>, Error> {
        let events = layout
            .events
            .get(..=end)
            .ok_or_else(|| error("renderer interval exceeds history"))?;
        if !(previous_end < start && start <= first_end && first_end <= end) {
            return Err(error("renderer interval has invalid frame boundaries"));
        }
        let mut work = Work::default();
        let mut classes = BTreeMap::new();
        let mut tracks: BTreeMap<u32, Track<'_>> = BTreeMap::new();
        let mut values = BTreeMap::new();
        let mut has_buffers = false;
        let mut has_origin = false;
        for (ordinal, event) in events.iter().enumerate() {
            check(proceed)?;
            work.charge(16)?;
            if ordinal == start {
                for (&id, track) in &mut tracks {
                    check(proceed)?;
                    if track.alive {
                        track.initial = true;
                        track.captured = track.selected;
                        track.awaiting_context = true;
                        metadata_budget(allocation, 1, 64)?;
                        values.insert(id, track.last);
                    }
                }
            }
            match event {
                HistoryEvent::Create(id, class) => {
                    let property = if let Some(property) = classes.get(class) {
                        *property
                    } else {
                        let value = property(layout, *class, &mut work, proceed)?;
                        metadata_budget(allocation, 1, 64)?;
                        classes.insert(*class, value);
                        value
                    };
                    if let Some(property) = property {
                        if tracks.contains_key(id) {
                            return Err(error(
                                "renderer context requires unique renderer lifetimes",
                            ));
                        }
                        metadata_budget(allocation, 1, std::mem::size_of::<Track<'_>>() + 64)?;
                        tracks.insert(
                            *id,
                            Track {
                                class: *class,
                                property,
                                created: ordinal,
                                previous: ordinal,
                                alive: true,
                                initial: false,
                                selected: None,
                                captured: None,
                                awaiting_context: false,
                                last: &[],
                                first_write: None,
                                seed: None,
                            },
                        );
                    }
                }
                HistoryEvent::Destroy(id) if tracks.contains_key(id) => {
                    let track = tracks.get_mut(id).unwrap();
                    if !std::mem::replace(&mut track.alive, false) {
                        return Err(error("renderer context has a repeated destruction"));
                    }
                    track.selected = None;
                    track.awaiting_context = false;
                }
                HistoryEvent::Set {
                    kind,
                    id,
                    property,
                    data,
                    undo,
                } if tracks.contains_key(id) => {
                    let track = tracks.get_mut(id).unwrap();
                    if !track.alive || *kind != 3 || *property != track.property {
                        return Err(error(
                            "renderer context requires complete live property buffers",
                        ));
                    }
                    source_undo(layout, ordinal, *id, track, undo, &mut work, proceed)?;
                    work.charge(data.len())?;
                    let summary = scan_context(data, Endian::Little, 1 << 30, proceed)?;
                    has_buffers |= !data.is_empty();
                    if previous_end < ordinal && ordinal < start && summary.primitive_count != 0 {
                        return Err(error("renderer first-frame gap contains captured drawing"));
                    }
                    if ordinal >= start {
                        track.first_write.get_or_insert(ordinal);
                        if track.awaiting_context && summary.uses_previous_instance {
                            track.seed = track.captured;
                        }
                        if summary.selected_instance.is_some() {
                            track.awaiting_context = false;
                        }
                    }
                    if let Some(instance) = summary.selected_instance {
                        track.selected = Some(instance);
                    }
                    track.last = data;
                    track.previous = ordinal;
                }
                HistoryEvent::Array { id, .. } if tracks.contains_key(id) => {
                    return Err(error("renderer context does not support array editing"));
                }
                HistoryEvent::Saved { id, .. }
                    if tracks.get(id).is_some_and(|track| !track.alive) =>
                {
                    return Err(error("renderer saved witness has no live instance"));
                }
                HistoryEvent::OriginShift(_) => has_origin = true,
                _ => {}
            }
        }
        if tracks.is_empty() || !has_buffers {
            return Ok(None);
        }
        if layout.snapshots.len() != 1
            || layout.snapshots[0].event != 0
            || layout.snapshot_bytes[0].as_ref() != [0, 0, 0, 0]
        {
            return Err(error(
                "renderer context bootstrap requires an empty source snapshot",
            ));
        }
        let mut renderers = BTreeMap::new();
        for (id, track) in tracks {
            check(proceed)?;
            if !track.initial && track.created < start {
                continue;
            }
            if track.seed.is_some() && track.first_write.is_none_or(|event| event > first_end) {
                return Err(error(
                    "renderer bootstrap requires a captured buffer in the first retained frame",
                ));
            }
            metadata_budget(allocation, 1, 128)?;
            renderers.insert(
                id,
                Renderer {
                    class: track.class,
                    property: track.property,
                    initial: track.initial,
                    end_alive: track.alive,
                    seed: track.seed,
                },
            );
        }
        if renderers.is_empty() {
            return Ok(None);
        }
        if has_origin {
            let mut seeded = false;
            for renderer in renderers.values() {
                check(proceed)?;
                seeded |= renderer.seed.is_some();
            }
            if seeded {
                return Err(error("renderer context with OriginShift is not verified"));
            }
            return Ok(None);
        }
        let mut reverse = BTreeMap::new();
        for instance in &layout.communication.instances {
            check(proceed)?;
            work.charge(16)?;
            if renderers
                .get(&instance.id)
                .is_some_and(|renderer| renderer.end_alive)
            {
                metadata_budget(allocation, 1, 80)?;
                if reverse.insert(instance.source, instance.id).is_some() {
                    return Err(error(
                        "renderer interval has duplicate live source identities",
                    ));
                }
            }
        }
        let mut expected_reverse = Vec::new();
        metadata_budget(allocation, reverse.len(), std::mem::size_of::<(u64, u32)>())?;
        expected_reverse.try_reserve_exact(reverse.len())?;
        metadata_budget(
            allocation,
            expected_reverse.capacity() - reverse.len(),
            std::mem::size_of::<(u64, u32)>(),
        )?;
        for row in reverse {
            check(proceed)?;
            expected_reverse.push(row);
        }
        let initial = initial_hash(
            layout,
            &renderers,
            &values,
            Some(&expected_reverse),
            &mut work,
            proceed,
        )?;
        let history = history_hash(
            layout,
            &renderers,
            start..end + 1,
            Coordinates::Source { start },
            &mut work,
            proceed,
        )?;
        Ok(Some(Self {
            renderers,
            retained_events: end - start + 1,
            initial,
            history,
            allocation: *allocation,
            work_remaining: work.remaining,
        }))
    }

    pub(super) fn seeds(
        &self,
        allocation: &mut usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Vec<history::Seed>, Error> {
        let mut count = 0usize;
        for renderer in self.renderers.values() {
            check(proceed)?;
            count += usize::from(renderer.seed.is_some());
        }
        metadata_budget(allocation, count, std::mem::size_of::<history::Seed>() + 9)?;
        let mut output = Vec::new();
        output.try_reserve_exact(count)?;
        metadata_budget(
            allocation,
            output.capacity() - count,
            std::mem::size_of::<history::Seed>(),
        )?;
        for (&id, renderer) in &self.renderers {
            check(proceed)?;
            if let Some(instance) = renderer.seed {
                let mut bytes = Vec::new();
                bytes.try_reserve_exact(9)?;
                metadata_budget(allocation, bytes.capacity() - 9, 1)?;
                bytes.push(1);
                bytes.extend(instance.to_le_bytes());
                output.push(history::Seed {
                    id,
                    property: renderer.property,
                    bytes,
                });
            }
        }
        Ok(output)
    }

    pub(super) fn allows(&self, id: u32, property: u32) -> bool {
        self.renderers
            .get(&id)
            .is_some_and(|renderer| renderer.property == property)
    }

    pub(super) fn verify_layout(
        &self,
        layout: &Layout<'_>,
        leading: usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        if leading == 0
            || layout.events.len().checked_sub(leading) != Some(self.retained_events)
            || !matches!(layout.events.first(), Some(HistoryEvent::Marker))
        {
            return Err(error("renderer output changed its retained interval"));
        }
        let mut work = Work {
            remaining: self.work_remaining,
        };
        let mut allocation = self.allocation;
        let mut directory = Reader::new(
            layout
                .snapshot_bytes
                .first()
                .ok_or_else(|| error("renderer output has no initial snapshot"))?,
        );
        metadata_budget(
            &mut allocation,
            directory.count()?,
            std::mem::size_of::<initial::Entry>() + 64,
        )?;
        metadata_budget(&mut allocation, layout.saved.len(), 64)?;
        let initial = initial::State::read(layout, proceed)?;
        let mut values = BTreeMap::new();
        for entry in &initial.entries {
            check(proceed)?;
            work.charge(16)?;
            let saved = &layout.saved[entry.record];
            let field = property(layout, saved.class, &mut work, proceed)?;
            if let Some(renderer) = self.renderers.get(&entry.id) {
                if !renderer.initial
                    || saved.class != renderer.class
                    || field != Some(renderer.property)
                {
                    return Err(error(
                        "renderer output changed an initial renderer identity",
                    ));
                }
                let mut reader = Reader::new(&saved.body);
                let fixed = reader.blob()?;
                if fixed.len() != layout.metadata.class(saved.class)?.size
                    || fixed.iter().any(|byte| *byte != 0)
                {
                    return Err(error("renderer output changed its initial fixed layout"));
                }
                let data = reader.blob()?;
                reader.end()?;
                metadata_budget(&mut allocation, 1, 64)?;
                values.insert(entry.id, data);
            } else if field.is_some() {
                return Err(error("renderer output introduced an initial renderer"));
            }
        }
        if initial_hash(layout, &self.renderers, &values, None, &mut work, proceed)? != self.initial
        {
            return Err(error(
                "renderer output changed captured initial buffers or identities",
            ));
        }
        let mut seeds = BTreeMap::new();
        for (ordinal, event) in layout.events.iter().enumerate() {
            check(proceed)?;
            work.charge(16)?;
            match event {
                HistoryEvent::Create(id, class) => {
                    let field = property(layout, *class, &mut work, proceed)?;
                    if field.is_some() && !self.renderers.contains_key(id) {
                        return Err(error("renderer output introduced a renderer lifetime"));
                    }
                    if self.renderers.contains_key(id) && ordinal < leading {
                        return Err(error("renderer output introduced a bootstrap lifetime"));
                    }
                }
                HistoryEvent::Set {
                    kind,
                    id,
                    property,
                    data,
                    undo,
                } if ordinal < leading && self.renderers.contains_key(id) => {
                    let renderer = &self.renderers[id];
                    let instance = renderer
                        .seed
                        .ok_or_else(|| error("renderer output introduced an uncaptured context"))?;
                    let mut expected = [0u8; 9];
                    expected[0] = 1;
                    expected[1..].copy_from_slice(&instance.to_le_bytes());
                    if *kind != 3
                        || *property != renderer.property
                        || data.as_ref() != expected
                        || undo.as_slice() != [u32_len(ordinal)?]
                    {
                        return Err(error(
                            "renderer output changed its captured context bootstrap",
                        ));
                    }
                    metadata_budget(&mut allocation, 1, 64)?;
                    if seeds.insert(*id, ordinal).is_some() {
                        return Err(error("renderer output repeats a context bootstrap"));
                    }
                }
                HistoryEvent::Destroy(id)
                | HistoryEvent::Array { id, .. }
                | HistoryEvent::Saved { id, .. }
                    if ordinal < leading && self.renderers.contains_key(id) =>
                {
                    return Err(error("renderer output introduced a bootstrap operation"));
                }
                _ => {}
            }
        }
        for (&id, renderer) in &self.renderers {
            check(proceed)?;
            if renderer.seed.is_some() != seeds.contains_key(&id) {
                return Err(error("renderer output lost its captured context bootstrap"));
            }
        }
        if history_hash(
            layout,
            &self.renderers,
            leading..layout.events.len(),
            Coordinates::Output {
                leading,
                seeds: &seeds,
            },
            &mut work,
            proceed,
        )? != self.history
        {
            return Err(error(
                "renderer output changed captured buffers, lifetimes or undo",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderer_context_hash_checks_cancellation_within_payload() {
        let bytes = vec![7u8; 256 * 1024];
        let mut hash = blake3::Hasher::new();
        let mut calls = 0;
        let failure = field(&mut hash, &bytes, &mut Work::default(), &mut || {
            calls += 1;
            calls < 4
        })
        .unwrap_err();
        assert_eq!(calls, 4);
        assert_eq!(
            failure.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
    }

    #[test]
    fn renderer_context_hash_checks_work_budget_before_processing_payload() {
        let mut hash = blake3::Hasher::new();
        let failure = field(
            &mut hash,
            &[0; 16],
            &mut Work { remaining: 23 },
            &mut || true,
        )
        .unwrap_err();
        assert!(failure.to_string().contains("work budget"));
        assert_eq!(hash.finalize(), blake3::hash(&[]));
    }

    #[test]
    fn renderer_context_seeds_share_the_exact_remaining_metadata_budget() {
        let renderer = |seed| Renderer {
            class: 6,
            property: 4,
            initial: true,
            end_alive: true,
            seed,
        };
        let proof = Proof {
            renderers: BTreeMap::from([(2, renderer(Some(17))), (3, renderer(None))]),
            retained_events: 0,
            initial: blake3::hash(&[]),
            history: blake3::hash(&[]),
            allocation: 0,
            work_remaining: 0,
        };
        let required = std::mem::size_of::<history::Seed>() + 9;
        let mut allocation = MAX_METADATA - required;
        let seeds = proof.seeds(&mut allocation, &mut || true).unwrap();
        assert_eq!(allocation, MAX_METADATA);
        assert_eq!(seeds.len(), 1);
        assert_eq!(seeds.capacity(), 1);
        assert_eq!(seeds[0].bytes, [1, 17, 0, 0, 0, 0, 0, 0, 0]);
        let mut allocation = MAX_METADATA - required + 1;
        assert!(proof.seeds(&mut allocation, &mut || true).is_err());
    }
}
