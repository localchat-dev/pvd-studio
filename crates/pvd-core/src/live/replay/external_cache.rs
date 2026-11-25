use super::*;

const SLOTS: usize = 64;
const BYTE_LIMIT: usize = 8 << 20;
const ENTRY_LIMIT: usize = 2 << 20;
const DEPTH_LIMIT: usize = 32;
const VISIT_LIMIT: usize = 65_536;
const ARC_ALLOWANCE: usize = 32;
const RESOURCE_ID: u64 = 1 << 63;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Value(PropertyLayout),
    References,
    ObjectKeys,
}

#[derive(Clone)]
enum Payload {
    Value(Value),
    References(Arc<BTreeMap<u64, u32>>),
    ObjectKeys(Arc<Vec<String>>),
}

struct Entry {
    id: u64,
    kind: Kind,
    digest: blake3::Hash,
    payload: Payload,
    bytes: usize,
    age: usize,
}

pub(in crate::live) struct ExternalCache {
    slots: [Option<Entry>; SLOTS],
    bytes: usize,
    byte_limit: usize,
    entry_limit: usize,
}

impl Default for ExternalCache {
    fn default() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
            bytes: size_of::<Self>(),
            byte_limit: BYTE_LIMIT,
            entry_limit: ENTRY_LIMIT,
        }
    }
}

impl ExternalCache {
    pub(in crate::live) fn object_keys(
        &mut self,
        id: u64,
        bytes: &[u8],
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Arc<Vec<String>>, Error> {
        let digest = record_digest(bytes, cancelled)?;
        if let Some(Payload::ObjectKeys(keys)) = self.get(id, Kind::ObjectKeys, digest) {
            check_replay_cancelled(cancelled)?;
            return Ok(keys);
        }
        let keys = object_keys::decode(bytes, cancelled)?;
        let allowance = keys.iter().try_fold(
            size_of::<Vec<String>>() + ARC_ALLOWANCE + keys.capacity() * size_of::<String>(),
            |bytes, key| bytes.checked_add(key.capacity()),
        );
        let keys = Arc::new(keys);
        check_replay_cancelled(cancelled)?;
        if let Some(allowance) = allowance {
            self.insert(id, Kind::ObjectKeys, digest, allowance, || {
                Payload::ObjectKeys(keys.clone())
            });
        }
        Ok(keys)
    }

    pub(super) fn property(
        &mut self,
        reader: &mut HistoryReader,
        id: u64,
        size: u64,
        layout: PropertyLayout,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Value, Error> {
        let bytes = read_record_bytes_with_cancel(reader, id, size, cancelled)?;
        self.value_bytes(reader, id, &bytes, layout, cancelled)
    }

    fn value_bytes(
        &mut self,
        reader: &HistoryReader,
        id: u64,
        bytes: &[u8],
        layout: PropertyLayout,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Value, Error> {
        let digest = record_digest(bytes, cancelled)?;
        let kind = Kind::Value(layout);
        if let Some(Payload::Value(value)) = self.get(id, kind, digest) {
            check_replay_cancelled(cancelled)?;
            self.observe(true, true);
            return Ok(value);
        }
        let value = layout.decode(reader, bytes, cancelled)?;
        #[cfg(all(test, feature = "scene-read-census"))]
        census::decoded_value();
        let allowance = value_allowance(&value, self.entry_limit, cancelled)?;
        check_replay_cancelled(cancelled)?;
        if let Some(allowance) = allowance {
            self.insert(id, kind, digest, allowance, || {
                Payload::Value(value.clone())
            });
        }
        self.observe(false, true);
        Ok(value)
    }

    pub(super) fn references(
        &mut self,
        reader: &mut HistoryReader,
        id: u64,
        size: u64,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Arc<BTreeMap<u64, u32>>, Error> {
        let bytes = read_record_bytes_with_cancel(reader, id, size, cancelled)?;
        self.reference_bytes(reader, id, &bytes, cancelled)
    }

    fn reference_bytes(
        &mut self,
        reader: &HistoryReader,
        id: u64,
        bytes: &[u8],
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Arc<BTreeMap<u64, u32>>, Error> {
        let digest = record_digest(bytes, cancelled)?;
        if let Some(Payload::References(value)) = self.get(id, Kind::References, digest) {
            check_replay_cancelled(cancelled)?;
            self.observe(true, false);
            return Ok(value);
        }
        let Record::References(value) = reader.decode(bytes, cancelled)? else {
            return Err(error("history reference record type mismatch"));
        };
        #[cfg(all(test, feature = "scene-read-census"))]
        census::decoded_references();
        let allowance = reference_allowance(value.len());
        let value = Arc::new(value);
        check_replay_cancelled(cancelled)?;
        if let Some(allowance) = allowance {
            self.insert(id, Kind::References, digest, allowance, || {
                Payload::References(value.clone())
            });
        }
        self.observe(false, false);
        Ok(value)
    }

    fn get(&mut self, id: u64, kind: Kind, digest: blake3::Hash) -> Option<Payload> {
        let index = self.slots.iter().position(|entry| {
            entry
                .as_ref()
                .is_some_and(|entry| entry.id == id && entry.kind == kind && entry.digest == digest)
        })?;
        self.touch(index);
        Some(self.slots[index].as_ref().unwrap().payload.clone())
    }

    fn touch(&mut self, index: usize) {
        let age = self.slots[index].as_ref().unwrap().age;
        for entry in self.slots.iter_mut().flatten() {
            if entry.age < age {
                entry.age += 1;
            }
        }
        self.slots[index].as_mut().unwrap().age = 0;
    }

    fn remove(&mut self, index: usize) {
        if let Some(entry) = self.slots[index].take() {
            self.bytes -= entry.bytes;
            for other in self.slots.iter_mut().flatten() {
                if other.age > entry.age {
                    other.age -= 1;
                }
            }
        }
    }

    fn insert(
        &mut self,
        id: u64,
        kind: Kind,
        digest: blake3::Hash,
        bytes: usize,
        payload: impl FnOnce() -> Payload,
    ) {
        if id & RESOURCE_ID == 0
            || bytes > self.entry_limit
            || size_of::<Self>()
                .checked_add(bytes)
                .is_none_or(|total| total > self.byte_limit)
        {
            return;
        }
        if let Some(index) = self.slots.iter().position(|entry| {
            entry
                .as_ref()
                .is_some_and(|entry| entry.id == id && entry.kind == kind)
        }) {
            self.remove(index);
        }
        while self
            .bytes
            .checked_add(bytes)
            .is_none_or(|total| total > self.byte_limit)
            || self.slots.iter().all(Option::is_some)
        {
            let index = self
                .slots
                .iter()
                .enumerate()
                .filter_map(|(index, entry)| entry.as_ref().map(|entry| (index, entry.age)))
                .max_by_key(|&(_, age)| age)
                .unwrap()
                .0;
            self.remove(index);
        }
        let index = self.slots.iter().position(Option::is_none).unwrap();
        for entry in self.slots.iter_mut().flatten() {
            entry.age += 1;
        }
        self.bytes += bytes;
        self.slots[index] = Some(Entry {
            id,
            kind,
            digest,
            payload: payload(),
            bytes,
            age: 0,
        });
    }

    fn observe(&self, hit: bool, value: bool) {
        #[cfg(all(test, feature = "scene-read-census"))]
        census::external_read(hit, value, self.slots.iter().flatten().count(), self.bytes);
        #[cfg(not(all(test, feature = "scene-read-census")))]
        let _ = (hit, value);
    }
}

fn record_digest(
    bytes: &[u8],
    cancelled: &mut impl FnMut() -> bool,
) -> Result<blake3::Hash, Error> {
    let mut digest = blake3::Hasher::new();
    for part in bytes.chunks(64 << 10) {
        check_replay_cancelled(cancelled)?;
        digest.update(part);
    }
    check_replay_cancelled(cancelled)?;
    Ok(digest.finalize())
}

fn reference_allowance(entries: usize) -> Option<usize> {
    entries.checked_mul(80)?.checked_add(256)
}

fn value_allowance(
    value: &Value,
    limit: usize,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<Option<usize>, Error> {
    check_replay_cancelled(cancelled)?;
    if !matches!(
        value,
        Value::Array(_) | Value::Bytes(_) | Value::MeshArray(_)
    ) {
        return Ok(None);
    }
    let mut stack: [std::slice::Iter<'_, Value>; DEPTH_LIMIT] = std::array::from_fn(|_| [].iter());
    stack[0] = std::slice::from_ref(value).iter();
    let mut depth = 1;
    let mut visited = 0usize;
    let mut bytes = 0usize;
    while depth != 0 {
        let Some(value) = stack[depth - 1].next() else {
            depth -= 1;
            continue;
        };
        if visited.is_multiple_of(256) {
            check_replay_cancelled(cancelled)?;
        }
        visited += 1;
        if visited > VISIT_LIMIT {
            return Ok(None);
        }
        let extra = match value {
            Value::String(text) => Some(text.capacity()),
            Value::Bytes(value) => ARC_ALLOWANCE.checked_add(value.len()),
            Value::MeshArray(value) => ARC_ALLOWANCE.checked_add(value.bytes().len()),
            Value::Array(value) => {
                size_of::<Vec<Value>>()
                    .checked_add(ARC_ALLOWANCE)
                    .and_then(|n| {
                        value
                            .capacity()
                            .checked_mul(size_of::<Value>())
                            .and_then(|m| n.checked_add(m))
                    })
            }
            _ => Some(0),
        };
        let Some(total) = extra
            .and_then(|extra| bytes.checked_add(extra))
            .filter(|n| *n <= limit)
        else {
            return Ok(None);
        };
        bytes = total;
        if let Value::Array(value) = value
            && !value.is_empty()
        {
            if depth == DEPTH_LIMIT {
                return Ok(None);
            }
            stack[depth] = value.iter();
            depth += 1;
        }
    }
    check_replay_cancelled(cancelled)?;
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests;
