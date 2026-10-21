use super::*;

pub(super) const MAX_BYTES: usize = 8 << 20;
#[cfg(test)]
pub(super) const RECORD_TAG: u8 = 9;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct StoredUndo {
    pub from: FrameInfo,
    pub to: usize,
    pub changes: IndexUpdate,
    pub removed_generations: BTreeSet<u64>,
}

fn add(total: &mut usize, bytes: usize) -> Option<()> {
    *total = total.checked_add(bytes).filter(|n| *n <= MAX_BYTES)?;
    Some(())
}

fn allowance(changes: &IndexUpdate, errors: Option<&[ErrorEvent]>) -> Option<usize> {
    // These charges cover both constructed containers and the bounded serde visitor's allowances.
    let mut total = 4096;
    for count in [
        changes.objects.len(),
        changes.debug.len(),
        changes.contacts.len(),
        changes.generations.len(),
    ] {
        add(&mut total, count.checked_mul(512)?)?;
    }
    for name in changes.cameras.keys() {
        add(
            &mut total,
            2048usize.checked_add(name.len().checked_mul(3)?)?,
        )?;
    }
    if let Some(errors) = errors {
        for event in errors {
            let text = event.message.len().checked_add(event.file.len())?;
            add(&mut total, 1024usize.checked_add(text.checked_mul(3)?)?)?;
        }
    }
    Some(total)
}

pub(super) fn prepare(
    from: &FrameInfo,
    previous: Option<usize>,
    before: &StateIndex,
    changes: &IndexUpdate,
) -> Result<Option<Vec<u8>>, Error> {
    let Some(to) = previous.filter(|frame| frame.checked_add(1) == Some(from.index)) else {
        return Ok(None);
    };
    let errors = changes.errors.as_ref().map(|_| before.errors.as_slice());
    if allowance(changes, errors).is_none() {
        return Ok(None);
    }
    let prior = |entries: &[(u64, Option<u64>)], values: &BTreeMap<u64, u64>| {
        entries
            .iter()
            .map(|&(id, _)| (id, values.get(&id).copied()))
            .collect()
    };
    let undo = StoredUndo {
        from: from.clone(),
        to,
        changes: IndexUpdate {
            objects: prior(&changes.objects, &before.objects),
            debug: prior(&changes.debug, &before.debug),
            contacts: prior(&changes.contacts, &before.contacts),
            generations: changes
                .generations
                .keys()
                .filter_map(|id| {
                    before
                        .generations
                        .get(id)
                        .map(|generation| (*id, *generation))
                })
                .collect(),
            cameras: changes
                .cameras
                .keys()
                .map(|name| (name.clone(), before.cameras.get(name).copied()))
                .collect(),
            errors: errors.map(<[ErrorEvent]>::to_vec),
        },
        removed_generations: changes
            .generations
            .keys()
            .filter(|id| !before.generations.contains_key(id))
            .copied()
            .collect(),
    };
    undo.validate()?;
    let mut count = Count(0);
    postcard::to_io(&RecordRef::Undo(&undo), &mut count)?;
    if count.0 > MAX_BYTES {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(count.0)?;
    if bytes.capacity() > MAX_BYTES {
        return Ok(None);
    }
    postcard::to_io(&RecordRef::Undo(&undo), &mut bytes)?;
    Ok(Some(bytes))
}

struct Count(usize);
impl Write for Count {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("scene undo encoded size overflow"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl StoredUndo {
    pub(super) fn validate(&self) -> Result<(), Error> {
        if self.to.checked_add(1) != Some(self.from.index) {
            return Err(error("scene undo frame continuity mismatch"));
        }
        for entries in [
            &self.changes.objects,
            &self.changes.debug,
            &self.changes.contacts,
        ] {
            if entries.windows(2).any(|pair| pair[0].0 >= pair[1].0)
                || entries
                    .iter()
                    .any(|(_, id)| id.is_some_and(|id| id & (1 << 63) == 0))
            {
                return Err(error("scene undo resource keys or identifiers are invalid"));
            }
        }
        if self
            .removed_generations
            .iter()
            .any(|id| self.changes.generations.contains_key(id))
        {
            return Err(error("scene undo generation operations overlap"));
        }
        let Some(mut bytes) = allowance(&self.changes, self.changes.errors.as_deref()) else {
            return Err(error("scene undo exceeds its construction allowance"));
        };
        if self
            .removed_generations
            .len()
            .checked_mul(512)
            .and_then(|n| add(&mut bytes, n))
            .is_none()
        {
            return Err(error("scene undo generation removal allowance exceeded"));
        }
        Ok(())
    }

    pub(super) fn verify_before(
        &self,
        before: &StateIndex,
        forward: &IndexUpdate,
    ) -> Result<(), Error> {
        for (old, changed, values) in [
            (&self.changes.objects, &forward.objects, &before.objects),
            (&self.changes.debug, &forward.debug, &before.debug),
            (&self.changes.contacts, &forward.contacts, &before.contacts),
        ] {
            if old.len() != changed.len()
                || old
                    .iter()
                    .zip(changed)
                    .any(|((id, value), (key, _))| id != key || *value != values.get(id).copied())
            {
                return Err(error(
                    "scene undo does not restore the prior resource index",
                ));
            }
        }
        if self.changes.cameras.len() != forward.cameras.len()
            || self.changes.cameras.iter().any(|(name, value)| {
                !forward.cameras.contains_key(name) || *value != before.cameras.get(name).copied()
            })
            || self.changes.errors.as_ref() != forward.errors.as_ref().map(|_| &before.errors)
            || self
                .changes
                .generations
                .len()
                .checked_add(self.removed_generations.len())
                != Some(forward.generations.len())
            || forward
                .generations
                .keys()
                .any(|id| match before.generations.get(id) {
                    Some(value) => self.changes.generations.get(id) != Some(value),
                    None => !self.removed_generations.contains(id),
                })
        {
            return Err(error(
                "scene undo does not restore the prior metadata index",
            ));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn apply(&self, index: &mut StateIndex) {
        for id in &self.removed_generations {
            index.generations.remove(id);
        }
        self.changes.apply(index);
    }

    pub(super) fn matches(&self, info: &FrameInfo, to: usize) -> bool {
        self.from.index == info.index
            && self.from.timestamp == info.timestamp
            && self.from.scene_id == info.scene_id
            && self.to == to
    }
}

pub(super) fn decode(
    bytes: &[u8],
    cancelled: &mut impl FnMut() -> bool,
) -> Result<StoredUndo, Error> {
    if bytes.len() > MAX_BYTES {
        return Err(error("scene undo exceeds its encoded limit"));
    }
    let Record::Undo(undo) = capture::record::decode_value(bytes, cancelled, MAX_BYTES)? else {
        return Err(error("scene undo record type mismatch"));
    };
    undo.validate()?;
    // BTreeMap/BTreeSet serde merges duplicate keys; canonical bytes reject that ambiguity.
    let mut comparison = Compare {
        expected: bytes,
        offset: 0,
    };
    postcard::to_io(&RecordRef::Undo(&undo), &mut comparison)?;
    if comparison.offset != bytes.len() {
        return Err(error("scene undo encoding is not canonical"));
    }
    check_replay_cancelled(cancelled)?;
    Ok(undo)
}

struct Compare<'a> {
    expected: &'a [u8],
    offset: usize,
}
impl Write for Compare<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let end = self
            .offset
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("scene undo comparison overflow"))?;
        if self.expected.get(self.offset..end) != Some(bytes) {
            return Err(std::io::Error::other(
                "scene undo encoding is not canonical",
            ));
        }
        self.offset = end;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(super) fn read(
    reader: &mut HistoryReader,
    source: &SegmentView,
    location: &FrameLocation,
    to: usize,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<Option<StoredUndo>, Error> {
    let Some(offset) = location.undo else {
        return Ok(None);
    };
    let position = source
        .frames
        .binary_search_by_key(&location.info.index, |entry| entry.info.index)
        .map_err(|_| error("scene undo source frame is absent"))?;
    let preceding = if position == 0 {
        source.previous_frame
    } else {
        Some(source.frames[position - 1].info.index)
    };
    if preceding != Some(to) || to.checked_add(1) != Some(location.info.index) {
        return Err(error("scene undo frame directory is discontinuous"));
    }
    if offset.get() & (1 << 63) != 0 || offset.get() <= location.offset {
        return Err(error(
            "scene undo location is not an inline record after its frame",
        ));
    }
    if !source.lease.available() {
        return Err(error("scene undo source segment expired"));
    }
    let bytes = read_record_bytes_with_limit(
        reader,
        offset.get(),
        source.committed_end,
        MAX_BYTES,
        cancelled,
    )?;
    let undo = decode(&bytes, cancelled)?;
    if !undo.matches(&location.info, to) {
        return Err(error("scene undo metadata differs from its frame index"));
    }
    Ok(Some(undo))
}

#[cfg(test)]
mod tests;
