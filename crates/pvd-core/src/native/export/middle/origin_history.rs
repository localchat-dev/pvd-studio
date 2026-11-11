use super::*;

const WORK_LIMIT: usize = 1 << 30;

pub(super) struct Scope {
    work: usize,
}

#[derive(Debug)]
pub(in crate::native::export) struct Proof {
    events: Vec<(usize, blake3::Hash)>,
}

pub(super) struct Range {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) first_end: usize,
    pub(super) leading: usize,
}

impl Scope {
    pub(super) fn prepare(
        layout: &Layout<'_>,
        end: usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Option<Self>, Error> {
        if !origin_prefix::contains_shift(layout, end, proceed)? {
            return Ok(None);
        }
        origin_prefix::verify_prefix_events(layout, end, proceed)?;
        Ok(Some(Self { work: WORK_LIMIT }))
    }

    fn scan(&mut self, bytes: usize, proceed: &mut dyn FnMut() -> bool) -> Result<(), Error> {
        check(proceed)?;
        self.work = self
            .work
            .checked_sub(bytes)
            .ok_or_else(|| error("OriginShift middle verification exceeds its work budget"))?;
        Ok(())
    }

    pub(super) fn prove_range(
        &mut self,
        layout: &Layout<'_>,
        range: Range,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Proof, Error> {
        let Range {
            start,
            end,
            first_end,
            leading,
        } = range;
        let events = layout
            .events
            .get(..end)
            .ok_or_else(|| error("OriginShift middle end exceeds history"))?;
        if start > first_end || first_end >= end {
            return Err(error("OriginShift middle first frame exceeds interval"));
        }
        let mut allocated = 0;
        let mut sources = BTreeMap::new();
        for section in &layout.communication.sections {
            self.scan(24, proceed)?;
            metadata_budget(&mut allocated, 1, 64)?;
            if sources.insert(section.source, None).is_some() {
                return Err(error("OriginShift middle Scene source is ambiguous"));
            }
        }
        if sources.is_empty() {
            return Err(error("OriginShift middle has no Scene section"));
        }
        for instance in &layout.communication.instances {
            self.scan(24, proceed)?;
            if let Some(scene) = sources.get_mut(&instance.source)
                && scene.replace(instance.id).is_some()
            {
                return Err(error("OriginShift middle Scene source is ambiguous"));
            }
        }
        let mut pending = BTreeMap::new();
        for scene in sources.into_values() {
            self.scan(24, proceed)?;
            metadata_budget(&mut allocated, 1, 64)?;
            let id = scene.ok_or_else(|| error("OriginShift middle Scene source is missing"))?;
            if pending.insert(id, false).is_some() {
                return Err(error("OriginShift middle Scene source is ambiguous"));
            }
        }
        let mut hashes = Vec::new();
        let mut shift_count = 0;
        for event in &events[start..] {
            self.scan(4, proceed)?;
            shift_count += usize::from(matches!(event, HistoryEvent::OriginShift(_)));
        }
        metadata_budget(
            &mut allocated,
            shift_count,
            std::mem::size_of::<(usize, blake3::Hash)>(),
        )?;
        hashes.try_reserve_exact(shift_count)?;
        for (ordinal, event) in events.iter().enumerate() {
            self.scan(16, proceed)?;
            if let HistoryEvent::OriginShift(event) = event {
                let unmatched = pending.get_mut(&event.scene()).ok_or_else(|| {
                    error("OriginShift middle Scene disagrees with frame section")
                })?;
                if ordinal < start {
                    *unmatched = true;
                } else {
                    if ordinal <= first_end {
                        *unmatched = false;
                    }
                    let mut hash = blake3::Hasher::new();
                    event.digest(&mut hash, proceed)?;
                    let at = ordinal
                        .checked_sub(start)
                        .and_then(|ordinal| ordinal.checked_add(leading))
                        .ok_or_else(|| error("OriginShift middle ordinal overflow"))?;
                    hashes.push((at, hash.finalize()));
                }
            }
            if ordinal < start {
                continue;
            }
            let undo = match event {
                HistoryEvent::Set { undo, .. } | HistoryEvent::Array { undo, .. } => undo,
                _ => continue,
            };
            for &distance in undo {
                self.scan(4, proceed)?;
                let target = ordinal
                    .checked_sub(distance as usize)
                    .filter(|&target| target < ordinal)
                    .ok_or_else(|| error("OriginShift middle requires preceding undo targets"))?;
                if matches!(events.get(target), Some(HistoryEvent::OriginShift(_))) {
                    return Err(error(
                        "OriginShift previous-event undo is not verified for middle trim",
                    ));
                }
            }
        }
        // Native bodies have no derived Scene marker. Every preceding Scene
        // shift needs a captured successor before the first retained observation.
        for unmatched in pending.into_values() {
            self.scan(8, proceed)?;
            if unmatched {
                return Err(error(
                    "OriginShift middle first frame must contain a captured Scene shift",
                ));
            }
        }
        Ok(Proof { events: hashes })
    }

    pub(super) fn apply(
        &mut self,
        metadata: &Metadata,
        state: &mut RawState,
        event: &super::super::origin_shift::Event<'_>,
        allocated: usize,
        inverse: bool,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        self.scan(32, proceed)?;
        let scene = state
            .get(&event.scene())
            .ok_or_else(|| error("OriginShift middle targets missing Scene"))?;
        let scene_class = metadata.class(scene.class)?;
        if label(metadata, scene_class.name.namespace)? != "physx3"
            || label(metadata, scene_class.name.name)? != "PxScene"
        {
            return Err(error("OriginShift middle targets unsupported Scene"));
        }
        let count = event.properties().len() / 8;
        let mut peak = allocated;
        metadata_budget(&mut peak, count, std::mem::size_of::<Update>())?;
        let mut updates = Vec::new();
        updates.try_reserve_exact(count)?;
        let shift = event.displacement_bits().map(f32::from_bits);
        for row in event.properties().chunks_exact(8) {
            self.scan(128, proceed)?;
            let id = u32::from_le_bytes(row[..4].try_into()?);
            let property = u32::from_le_bytes(row[4..].try_into()?);
            let instance = state
                .get(&id)
                .ok_or_else(|| error("OriginShift middle targets missing Actor"))?;
            let (offset, scalars, width) =
                field(metadata, instance.class, property, &mut |bytes| {
                    self.scan(bytes, proceed)
                })?;
            let before = instance
                .values
                .get(&property)
                .filter(|bytes| bytes.len() == width && instance.defined.contains(&property))
                .ok_or_else(|| error("OriginShift middle has no defined fixed property"))?;
            metadata_budget(&mut peak, width, 1)?;
            let mut value = before.clone();
            for scalar in 0..scalars {
                self.scan(16, proceed)?;
                let at = offset + scalar * 4;
                let previous = f32::from_le_bytes(before[at..at + 4].try_into()?);
                let displacement = shift[scalar % 3];
                let next = previous - displacement;
                if !previous.is_finite() || !next.is_finite() {
                    return Err(error("OriginShift middle requires finite fixed scalars"));
                }
                if inverse {
                    // Native undo applies the sign-flipped displacement; a
                    // synthesized baseline must preserve the exact preceding bits.
                    let reversed = next - f32::from_bits(displacement.to_bits() ^ 0x8000_0000);
                    if reversed.to_bits() != previous.to_bits() {
                        return Err(error(
                            "OriginShift middle inverse does not preserve scalar bits",
                        ));
                    }
                }
                value[at..at + 4].copy_from_slice(&next.to_le_bytes());
            }
            updates.push(Update {
                id,
                property,
                value,
            });
        }
        check(proceed)?;
        for update in updates {
            let value = state
                .get_mut(&update.id)
                .and_then(|instance| instance.values.get_mut(&update.property))
                .ok_or_else(|| error("OriginShift middle update lost its verified target"))?;
            *value = update.value;
        }
        Ok(())
    }
}

struct Update {
    id: u32,
    property: u32,
    value: Vec<u8>,
}

fn label(metadata: &Metadata, id: u32) -> Result<&str, Error> {
    metadata
        .strings
        .get(&id)
        .map(String::as_str)
        .ok_or_else(|| error("undefined OriginShift middle metadata string"))
}

fn field(
    metadata: &Metadata,
    class: u32,
    id: u32,
    scan: &mut dyn FnMut(usize) -> Result<(), Error>,
) -> Result<(usize, usize, usize), Error> {
    let class = metadata.class(class)?;
    let actor = label(metadata, class.name.name)?;
    let property = metadata.property(id)?;
    let datatype = metadata.class(property.datatype_id)?;
    if label(metadata, class.name.namespace)? != "physx3"
        || !matches!(actor, "PxRigidStatic" | "PxRigidDynamic")
        || datatype.name != property.datatype
        || property.kind != 1
        || datatype.dynamic
        || datatype.has_pointers
        || !datatype.string_offsets.is_empty()
        || datatype.base != u32::MAX
        || label(metadata, datatype.name.namespace)? != "physx3"
    {
        return Err(error("unsupported OriginShift middle fixed layout"));
    }
    let layout = match (
        label(metadata, property.name)?,
        label(metadata, datatype.name.name)?,
    ) {
        ("GlobalPose", "PxTransform") => (16, 3, 28),
        ("KinematicTarget", "PxTransform") if actor == "PxRigidDynamic" => (16, 3, 28),
        ("WorldBounds", "PxBounds3") => (0, 6, 24),
        _ => return Err(error("unsupported OriginShift middle fixed property")),
    };
    if datatype.size != layout.2 {
        return Err(error("OriginShift middle datatype has invalid width"));
    }
    if !origin_shift::fixed_layout(metadata, property.datatype_id, scan)? {
        return Err(error("unsupported OriginShift middle fixed layout"));
    }
    Ok(layout)
}

impl Proof {
    pub(in crate::native::export) fn verify_layout(
        &self,
        layout: &Layout<'_>,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let mut expected = self.events.iter();
        for (ordinal, event) in layout.events.iter().enumerate() {
            check(proceed)?;
            let HistoryEvent::OriginShift(event) = event else {
                continue;
            };
            let (at, expected_hash) = expected
                .next()
                .ok_or_else(|| error("OriginShift middle introduced a shift event"))?;
            let mut hash = blake3::Hasher::new();
            event.digest(&mut hash, proceed)?;
            if ordinal != *at || hash.finalize() != *expected_hash {
                return Err(error(
                    "OriginShift middle changed retained header or auxiliary rows",
                ));
            }
        }
        if expected.next().is_some() {
            return Err(error("OriginShift middle lost a retained shift event"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
