use super::*;
use crate::debug_draw::scan_context;
use pvd_protocol::Endian;

mod proof;
pub(in crate::native::export) use proof::Proof;

#[derive(Clone, Copy, Debug)]
struct Buffer {
    class: u32,
    property: u32,
    position: u64,
    profile: bool,
}

pub(in crate::native::export) struct Scope {
    buffers: BTreeMap<u32, Buffer>,
    profile_class: u32,
    middle: Option<Middle>,
    bytes: usize,
}

struct Middle {
    initial: blake3::Hash,
    catalog: blake3::Hash,
    history: blake3::Hash,
    seeds: blake3::Hash,
    retained: usize,
    work_remaining: usize,
}

#[derive(Clone, Copy)]
enum Window {
    Prefix {
        end: usize,
    },
    Middle {
        start: usize,
        end: usize,
        previous: usize,
    },
}

struct Work {
    remaining: usize,
}

impl Work {
    fn new() -> Self {
        Self { remaining: 1 << 30 }
    }

    fn charge(&mut self, bytes: usize) -> Result<(), Error> {
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .ok_or_else(|| error("initial analysis proof exceeds its work budget"))?;
        Ok(())
    }
}

struct Track<'a> {
    fixed: &'a [u8],
    last: &'a [u8],
    previous: usize,
    source_context: bool,
    retained_context: bool,
}

struct Profile {
    source: u64,
    sources: usize,
    events: BTreeSet<u16>,
    bootstrap: bool,
}

impl Scope {
    #[cfg(test)]
    pub(in crate::native::export) fn read(
        initial: &State,
        layout: &Layout<'_>,
        first: usize,
        last: usize,
        allocated: &mut usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Option<Self>, Error> {
        Self::read_with_cameras(initial, layout, first, last, None, allocated, proceed)
    }

    pub(in crate::native::export) fn read_with_cameras(
        initial: &State,
        layout: &Layout<'_>,
        first: usize,
        last: usize,
        cameras: Option<&camera::Permit<'_>>,
        allocated: &mut usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Option<Self>, Error> {
        if layout.communication.zones.is_empty() {
            return Ok(None);
        }
        if layout.communication.sections.len() != 1 {
            return Err(error(
                "initial analysis requires one Scene with stable zones",
            ));
        }
        let rows = &layout.communication.sections[0].rows;
        let start = usize::try_from(
            rows.get(first)
                .ok_or_else(|| error("missing first analysis frame"))?[1],
        )?;
        let end = usize::try_from(
            rows.get(last)
                .ok_or_else(|| error("missing last analysis frame"))?[3],
        )?
        .checked_add(1)
        .ok_or_else(|| error("initial analysis end overflow"))?;
        let previous = usize::try_from(
            first
                .checked_sub(1)
                .and_then(|i| rows.get(i))
                .ok_or_else(|| error("initial analysis requires a preceding frame"))?[3],
        )?;
        Self::read_window(
            initial,
            layout,
            Window::Middle {
                start,
                end,
                previous,
            },
            cameras,
            allocated,
            proceed,
        )
    }

    #[cfg(test)]
    pub(in crate::native::export) fn read_prefix(
        initial: &State,
        layout: &Layout<'_>,
        cut: usize,
        allocated: &mut usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Option<Self>, Error> {
        Self::read_prefix_with_cameras(initial, layout, cut, None, allocated, proceed)
    }

    pub(in crate::native::export) fn read_prefix_with_cameras(
        initial: &State,
        layout: &Layout<'_>,
        cut: usize,
        cameras: Option<&camera::Permit<'_>>,
        allocated: &mut usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Option<Self>, Error> {
        if initial.entries.is_empty() {
            return Ok(None);
        }
        for zone in &layout.communication.zones {
            check(proceed)?;
            if initial.classes.contains_key(&zone.id) {
                return Self::read_window(
                    initial,
                    layout,
                    Window::Prefix { end: cut },
                    cameras,
                    allocated,
                    proceed,
                );
            }
        }
        Ok(None)
    }

    fn read_window(
        initial: &State,
        layout: &Layout<'_>,
        window: Window,
        cameras: Option<&camera::Permit<'_>>,
        allocated: &mut usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Option<Self>, Error> {
        if layout.communication.zones.is_empty()
            || layout.communication.sections.len() != 1
            || !matches!(layout.events.first(), Some(HistoryEvent::Marker))
        {
            return Err(error(
                "initial analysis requires one Scene with stable zones",
            ));
        }
        let (start, end, previous) = match window {
            Window::Prefix { end } => (0, end, None),
            Window::Middle {
                start,
                end,
                previous,
            } => (start, end, Some(previous)),
        };
        if !layout.communication.cameras.is_empty()
            && cameras.is_none_or(|permit| {
                *allocated < permit.bytes()
                    || match window {
                        Window::Prefix { .. } => !permit.prefix(layout, end),
                        Window::Middle { .. } => !permit.middle(layout, start, end),
                    }
            })
        {
            return Err(error(
                "initial analysis requires a verified camera scope and shared budget",
            ));
        }
        let rows = &layout.communication.sections[0].rows;
        let first_begin = usize::try_from(
            rows.first()
                .ok_or_else(|| error("missing initial analysis frame"))?[1],
        )?;
        if previous.is_some_and(|previous| previous >= start)
            || start >= end
            || end > layout.events.len()
            || first_begin <= 1
            || !rows.iter().any(|row| row[3] == (end - 1) as u64)
        {
            return Err(error("invalid initial analysis frame boundaries"));
        }
        let before = *allocated;
        metadata_budget(allocated, 1, std::mem::size_of::<Self>())?;
        metadata_budget(allocated, layout.communication.zones.len(), 768)?;
        let mut work = Work::new();
        let mut profiles = BTreeMap::new();
        for zone in &layout.communication.zones {
            check(proceed)?;
            work.charge(16)?;
            if profiles.contains_key(&zone.id) {
                return Err(error("initial profile zone identities are ambiguous"));
            }
            if !initial.classes.contains_key(&zone.id) {
                return Err(error("initial profile has no unique captured source"));
            }
            metadata_budget(allocated, zone.events.len(), 32)?;
            let mut events = BTreeSet::new();
            for &(_, id, _) in &zone.events {
                check(proceed)?;
                work.charge(16)?;
                if !events.insert(id) {
                    return Err(error("initial profile event identities are ambiguous"));
                }
            }
            profiles.insert(
                zone.id,
                Profile {
                    source: zone.source,
                    sources: 0,
                    events,
                    bootstrap: false,
                },
            );
        }
        for instance in &layout.communication.instances {
            check(proceed)?;
            work.charge(24)?;
            if let Some(profile) = profiles.get_mut(&instance.id) {
                profile.sources += 1;
                if instance.source != profile.source {
                    return Err(error("initial profile source differs from its zone"));
                }
            }
        }
        for profile in profiles.values() {
            check(proceed)?;
            work.charge(16)?;
            if profile.sources != 1 {
                return Err(error("initial profile has no unique captured source"));
            }
        }
        let mut buffers = BTreeMap::new();
        let mut tracks = BTreeMap::new();
        for entry in &initial.entries {
            check(proceed)?;
            work.charge(16)?;
            let saved = &layout.saved[entry.record];
            let class = layout.metadata.class(saved.class)?;
            let name = layout
                .metadata
                .strings
                .get(&class.name.name)
                .map(String::as_str);
            let profile = name == Some("PxProfileZone");
            if !profile && name != Some("PvdUserRenderer") {
                continue;
            }
            if profile && !profiles.contains_key(&saved.id) {
                return Err(error("initial profile instance has no matching zone"));
            }
            let property = property(layout, saved.class, profile, &mut work, proceed)?;
            let (fixed, payload) = payload(saved, class.size)?;
            if saved.deleted || (profile && !payload.is_empty()) {
                return Err(error(
                    "initial profile requires an empty live captured property",
                ));
            }
            metadata_budget(allocated, 1, 256)?;
            buffers.insert(
                saved.id,
                Buffer {
                    class: saved.class,
                    property,
                    position: saved.position,
                    profile,
                },
            );
            tracks.insert(
                saved.id,
                Track {
                    fixed,
                    last: payload,
                    previous: 0,
                    source_context: false,
                    retained_context: false,
                },
            );
        }
        let mut profile_class = None;
        for id in profiles.keys() {
            check(proceed)?;
            work.charge(16)?;
            let buffer = buffers
                .get(id)
                .filter(|buffer| buffer.profile)
                .ok_or_else(|| error("initial zone does not identify a profile instance"))?;
            if *profile_class.get_or_insert(buffer.class) != buffer.class {
                return Err(error("initial profile zones require one captured class"));
            }
        }
        let profile_class = profile_class.unwrap();
        let mut bootstraps = 0;
        let mut pending = history::profile::Pending::default();
        for (ordinal, event) in layout.events.iter().enumerate() {
            check(proceed)?;
            work.charge(16)?;
            match event {
                HistoryEvent::Create(id, class) => {
                    let name = layout
                        .metadata
                        .strings
                        .get(&layout.metadata.class(*class)?.name.name)
                        .map(String::as_str);
                    if buffers.contains_key(id)
                        || matches!(name, Some("PxProfileZone" | "PvdUserRenderer"))
                    {
                        return Err(error("initial analysis requires stable captured lifetimes"));
                    }
                }
                HistoryEvent::Destroy(id)
                | HistoryEvent::Saved { id, .. }
                | HistoryEvent::Array { id, .. }
                    if buffers.contains_key(id) =>
                {
                    return Err(error(
                        "initial analysis requires stable complete properties",
                    ));
                }
                HistoryEvent::Set {
                    kind,
                    id,
                    property,
                    data,
                    undo,
                } if buffers.contains_key(id) => {
                    let buffer = buffers[id];
                    if *kind != 3 || *property != buffer.property {
                        return Err(error("initial analysis requires complete captured buffers"));
                    }
                    let track = tracks.get_mut(id).unwrap();
                    source_undo(
                        layout,
                        ordinal,
                        (*id, *property),
                        track.previous,
                        undo,
                        &mut work,
                        proceed,
                    )?;
                    track.previous = ordinal;
                    if buffer.profile {
                        if ordinal < first_begin {
                            let profile = profiles.get_mut(id).unwrap();
                            if ordinal != bootstraps + 1 || profile.bootstrap {
                                return Err(error(
                                    "initial profile requires contiguous captured starts per zone",
                                ));
                            }
                            absolute_starts(data, &profile.events, &mut work, proceed)?;
                            profile.bootstrap = true;
                            bootstraps += 1;
                        }
                        if ordinal < start {
                            work.charge(data.len())?;
                            pending.ingest_bounded(u64::from(*id), data, *allocated, proceed)?;
                        }
                    } else if ordinal < end && !data.is_empty() {
                        work.charge(data.len())?;
                        let summary = scan_context(data, Endian::Little, MAX_METADATA, proceed)?;
                        if (!track.source_context || (ordinal >= start && !track.retained_context))
                            && (summary.uses_previous_instance
                                || summary.selected_instance.is_none())
                        {
                            return Err(error(
                                "initial drawing requires an explicit captured instance",
                            ));
                        }
                        if previous.is_some_and(|previous| previous < ordinal && ordinal < start) {
                            return Err(error("initial middle cannot drop interframe drawing"));
                        }
                        track.source_context = true;
                        track.retained_context |= ordinal >= start;
                    }
                    if ordinal < start {
                        track.last = data;
                    }
                }
                HistoryEvent::OriginShift(_) => {
                    return Err(error("initial analysis with OriginShift is not verified"));
                }
                _ => {}
            }
        }
        for (&id, profile) in &profiles {
            check(proceed)?;
            work.charge(16)?;
            if !profile.bootstrap || !tracks[&id].last.is_empty() {
                return Err(error(
                    "initial profile requires captured starts and an empty cut property",
                ));
            }
        }
        for (&id, buffer) in &buffers {
            check(proceed)?;
            if !buffer.profile && (!tracks[&id].source_context || !tracks[&id].retained_context) {
                return Err(error("initial drawing has no retained captured instance"));
            }
        }
        let middle = if matches!(window, Window::Middle { .. }) {
            metadata_budget(allocated, profiles.len(), 128)?;
            let mut properties = BTreeMap::new();
            let mut alive = BTreeSet::new();
            for &id in profiles.keys() {
                check(proceed)?;
                work.charge(16)?;
                properties.insert(id, buffers[&id].property);
                alive.insert(id);
            }
            let seeds = pending.seeds_bounded(&properties, &alive, allocated, proceed)?;
            for seed in &seeds {
                check(proceed)?;
                absolute_starts(&seed.bytes, &profiles[&seed.id].events, &mut work, proceed)?;
            }
            Some(Middle {
                seeds: proof::seeds_hash(&seeds, &mut work, proceed)?,
                initial: proof::initial_hash(
                    &buffers,
                    |id| Ok((tracks[&id].fixed, tracks[&id].last)),
                    &mut work,
                    proceed,
                )?,
                catalog: proof::catalog_hash(layout, &buffers, &mut work, proceed)?,
                history: proof::history_hash(
                    layout,
                    &buffers,
                    start..end,
                    start,
                    true,
                    &mut work,
                    proceed,
                )?,
                retained: end - start,
                work_remaining: work.remaining,
            })
        } else {
            None
        };
        let bytes = *allocated - before;
        Ok(Some(Self {
            buffers,
            profile_class,
            middle,
            bytes,
        }))
    }

    pub(in crate::native::export) fn contains(&self, id: u32, class: u32) -> bool {
        self.buffers
            .get(&id)
            .is_some_and(|buffer| buffer.class == class)
    }

    pub(in crate::native::export) fn contains_record(&self, saved: &Saved<'_>) -> bool {
        self.buffers.get(&saved.id).is_some_and(|buffer| {
            buffer.class == saved.class && buffer.position == saved.position && !saved.deleted
        })
    }

    pub(in crate::native::export) fn profile_class(&self, class: u32) -> bool {
        self.profile_class == class
    }

    pub(in crate::native::export) fn renderer_property(&self, id: u32) -> Option<u32> {
        self.buffers
            .get(&id)
            .filter(|buffer| !buffer.profile)
            .map(|buffer| buffer.property)
    }

    pub(in crate::native::export) fn bytes(&self) -> usize {
        self.bytes
    }
}

fn property(
    layout: &Layout<'_>,
    id: u32,
    profile: bool,
    work: &mut Work,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<u32, Error> {
    let class = layout.metadata.class(id)?;
    let label = |id| layout.metadata.strings.get(&id).map(String::as_str);
    let name = if profile {
        "PxProfileZone"
    } else {
        "PvdUserRenderer"
    };
    let mut definitions = 0;
    for candidate in layout.metadata.classes.iter().flatten() {
        check(proceed)?;
        work.charge(16)?;
        definitions += usize::from(candidate.name == class.name);
    }
    if label(class.name.namespace) != Some("_debugger_")
        || label(class.name.name) != Some(name)
        || definitions != 1
        || class.base != u32::MAX
        || class.size != 24
        || !class.dynamic
        || class.has_pointers
        || !class.string_offsets.is_empty()
        || class.properties.len() != 1
        || layout
            .pointer_layouts
            .get(id as usize)
            .is_none_or(|p| !p.is_empty())
    {
        return Err(error("unsupported initial analysis class layout"));
    }
    let property = class.properties[0];
    let p = layout.metadata.property(property)?;
    let datatype = layout.metadata.class(p.datatype_id)?;
    if p.owner != class.name
        || label(p.name) != Some("events")
        || p.kind != 2
        || p.offset32 != 0
        || datatype.name != p.datatype
        || label(p.datatype.namespace) != Some("physx3")
        || label(p.datatype.name) != Some("PvdU8")
        || datatype.size != 1
        || datatype.dynamic
        || datatype.base != u32::MAX
        || !datatype.properties.is_empty()
        || datatype.has_pointers
        || !datatype.string_offsets.is_empty()
        || layout
            .pointer_layouts
            .get(p.datatype_id as usize)
            .is_none_or(|p| !p.is_empty())
    {
        return Err(error("unsupported initial analysis event layout"));
    }
    Ok(property)
}

fn payload<'a>(saved: &'a Saved<'_>, size: usize) -> Result<(&'a [u8], &'a [u8]), Error> {
    let mut reader = Reader::new(&saved.body);
    let fixed = reader.blob()?;
    if fixed.len() != size {
        return Err(error("initial analysis fixed size differs from its class"));
    }
    let data = reader.blob()?;
    reader.end()?;
    Ok((fixed, data))
}

fn absolute_starts(
    data: &[u8],
    events: &BTreeSet<u16>,
    work: &mut Work,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    if data.is_empty() || !data.len().is_multiple_of(26) {
        return Err(error(
            "initial profile bootstrap requires explicit absolute starts",
        ));
    }
    work.charge(data.len())?;
    for event in data.chunks_exact(26) {
        check(proceed)?;
        if event[..2] != [1, 15] || !events.contains(&u16::from_le_bytes(event[2..4].try_into()?)) {
            return Err(error(
                "initial profile bootstrap requires known absolute starts",
            ));
        }
    }
    Ok(())
}

fn source_undo(
    layout: &Layout<'_>,
    ordinal: usize,
    identity: (u32, u32),
    previous: usize,
    refs: &[u32],
    work: &mut Work,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    let mut last = None;
    for &distance in refs {
        check(proceed)?;
        work.charge(4)?;
        let target = ordinal
            .checked_sub(distance as usize)
            .filter(|at| *at < ordinal)
            .ok_or_else(|| error("initial analysis undo must target preceding history"))?;
        if last.is_some_and(|last| target <= last) {
            return Err(error("initial analysis undo must be ordered and distinct"));
        }
        match &layout.events[target] {
            HistoryEvent::Marker if target == 0 => (),
            HistoryEvent::Set {
                kind: 3,
                id,
                property,
                ..
            } if (*id, *property) == identity => (),
            _ => return Err(error("initial analysis undo targets another operation")),
        }
        last = Some(target);
    }
    if last != Some(previous) {
        return Err(error(
            "initial analysis undo does not restore its preceding property",
        ));
    }
    Ok(())
}
