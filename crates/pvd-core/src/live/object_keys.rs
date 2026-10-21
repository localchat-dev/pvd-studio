use super::*;
use std::sync::atomic::Ordering;

pub(super) const MAGIC: &[u8; 8] = b"PVDKEY01";
pub(super) const RECORD_TAG: u8 = 10;
pub(super) const MAX_BYTES: usize = 64 << 10;
const MAX_KEYS: usize = 16_384;
const MIN_KEY_BYTES: usize = 256;
const SLOTS: usize = 256;
const MEMO_BYTES: usize = 4 << 20;

#[derive(Default)]
struct Budget(AtomicUsize);

struct Charge {
    _metadata: resources::MetadataReservation,
    budget: Arc<Budget>,
    bytes: usize,
}

impl Drop for Charge {
    fn drop(&mut self) {
        self.budget.0.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

impl Budget {
    fn reserve(
        self: &Arc<Self>,
        resources: &Resources,
        bytes: usize,
    ) -> Result<Option<Charge>, Error> {
        if self
            .0
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                used.checked_add(bytes).filter(|used| *used <= MEMO_BYTES)
            })
            .is_err()
        {
            return Ok(None);
        }
        let metadata = resources.try_reserve_metadata(bytes);
        match metadata {
            Ok(Some(metadata)) => Ok(Some(Charge {
                _metadata: metadata,
                budget: self.clone(),
                bytes,
            })),
            other => {
                self.0.fetch_sub(bytes, Ordering::Relaxed);
                other.map(|_| None)
            }
        }
    }
}

struct Entry {
    encoded: Vec<u8>,
    _charge: Charge,
    digest: [u8; 32],
    id: u64,
}

#[derive(Default)]
pub(super) struct KeyLists {
    slots: Vec<Option<Entry>>,
    directory: Option<Charge>,
    budget: Arc<Budget>,
    store: Option<Resources>,
    next: usize,
}

impl KeyLists {
    pub(super) fn staged(&self) -> Self {
        Self {
            slots: Vec::new(),
            directory: None,
            budget: self.budget.clone(),
            store: self.store.clone(),
            next: 0,
        }
    }

    pub(super) fn manager_bytes(&self) -> usize {
        size_of::<Self>() + size_of::<Budget>() + 2 * size_of::<usize>()
    }

    fn bind(&mut self, resources: &Resources) -> Result<(), Error> {
        if self
            .store
            .as_ref()
            .is_some_and(|store| !store.same(resources))
        {
            return Err(error("object key memo belongs to another resource store"));
        }
        self.store.get_or_insert_with(|| resources.clone());
        Ok(())
    }

    fn directory(&mut self, resources: &Resources) -> Result<bool, Error> {
        if !self.slots.is_empty() {
            return Ok(true);
        }
        let bytes = SLOTS * size_of::<Option<Entry>>() + 64;
        let Some(charge) = self.budget.reserve(resources, bytes)? else {
            return Ok(false);
        };
        let mut slots = Vec::new();
        if slots.try_reserve_exact(SLOTS).is_err() || slots.capacity() != SLOTS {
            return Ok(false);
        }
        slots.resize_with(SLOTS, || None);
        self.slots = slots;
        self.directory = Some(charge);
        Ok(true)
    }

    fn find(
        &mut self,
        resources: &Resources,
        digest: &[u8; 32],
        keys: &impl KeySource,
        pending: bool,
    ) -> Result<Option<ResourceHandle>, Error> {
        for slot in &mut self.slots {
            let Some(entry) = slot else { continue };
            if &entry.digest != digest || !keys.matches(&entry.encoded) {
                continue;
            }
            match resources.try_pin(entry.id)? {
                Some(handle) if pending || handle.info().2 => return Ok(Some(handle)),
                Some(_) | None => *slot = None,
            }
        }
        Ok(None)
    }

    fn sweep(&mut self, resources: &Resources) -> Result<(), Error> {
        for _ in 0..8.min(self.slots.len()) {
            let slot = &mut self.slots[self.next];
            if let Some(entry) = slot
                && resources.try_pin(entry.id)?.is_none()
            {
                *slot = None;
            }
            self.next = (self.next + 1) % self.slots.len();
        }
        Ok(())
    }

    pub(super) fn prepare(
        &mut self,
        staged: &mut Self,
        resources: &Resources,
        plan: &mut ResourcePlan,
        values: &BTreeMap<String, Value>,
        external: &BTreeMap<&str, u64>,
    ) -> Result<Option<ResourceHandle>, Error> {
        self.bind(resources)?;
        staged.bind(resources)?;
        if !Arc::ptr_eq(&self.budget, &staged.budget) {
            return Err(error("object key preparation belongs to another writer"));
        }
        let keys = InlineKeys { values, external };
        let Some((size, digest)) = keys.profile() else {
            return Ok(None);
        };
        if let Some(handle) = self.find(resources, &digest, &keys, false)? {
            return Ok(Some(handle));
        }
        if let Some(handle) = staged.find(resources, &digest, &keys, true)? {
            return Ok(Some(handle));
        }
        self.sweep(resources)?;
        if self.slots.iter().flatten().count() + staged.slots.iter().flatten().count() >= SLOTS
            || !staged.directory(resources)?
        {
            return Ok(None);
        }
        let Some(charge) = self.budget.reserve(resources, size + 64)? else {
            return Ok(None);
        };
        let mut encoded = Vec::new();
        if encoded.try_reserve_exact(size).is_err() || encoded.capacity() != size {
            return Ok(None);
        }
        keys.write(&mut encoded)?;
        let handle = match resources.plan(&encoded, &[], plan) {
            Ok(handle) => handle,
            Err(failure) if resources::admission_failure(&failure) => return Ok(None),
            Err(failure) => return Err(failure),
        };
        let slot = staged.slots.iter_mut().find(|slot| slot.is_none()).unwrap();
        *slot = Some(Entry {
            encoded,
            _charge: charge,
            digest,
            id: handle.id(),
        });
        Ok(Some(handle))
    }

    pub(super) fn publish(&mut self, mut staged: Self, resources: &Resources) -> Result<(), Error> {
        self.bind(resources)?;
        staged.bind(resources)?;
        if !Arc::ptr_eq(&self.budget, &staged.budget) {
            return Err(error("object key publication belongs to another writer"));
        }
        for entry in staged.slots.iter().flatten() {
            if !resources.pin(entry.id)?.info().2 {
                return Err(error(
                    "object key schema was not committed before publication",
                ));
            }
        }
        if self.slots.is_empty() {
            std::mem::swap(self, &mut staged);
            return Ok(());
        }
        for entry in staged.slots.iter_mut().filter_map(Option::take) {
            if let Some(slot) = self.slots.iter_mut().find(|slot| slot.is_none()) {
                *slot = Some(entry);
            }
        }
        Ok(())
    }
}

trait KeySource {
    fn matches(&self, encoded: &[u8]) -> bool;
}

struct InlineKeys<'a> {
    values: &'a BTreeMap<String, Value>,
    external: &'a BTreeMap<&'a str, u64>,
}

impl InlineKeys<'_> {
    fn iter(&self) -> impl Iterator<Item = &str> {
        self.values
            .keys()
            .map(String::as_str)
            .filter(|key| !self.external.contains_key(key))
    }

    fn profile(&self) -> Option<(usize, [u8; 32])> {
        let count = self.iter().count();
        if count == 0 || count > MAX_KEYS {
            return None;
        }
        let mut size = 12usize;
        let mut old = 0usize;
        for key in self.iter() {
            size = size.checked_add(4)?.checked_add(key.len())?;
            old = old
                .checked_add(key.len())?
                .checked_add(varint_len(key.len()))?;
        }
        if size > MAX_BYTES || old < MIN_KEY_BYTES {
            return None;
        }
        let mut digest = HashWriter(blake3::Hasher::new());
        self.write(&mut digest).ok()?;
        Some((size, *digest.0.finalize().as_bytes()))
    }

    fn write(&self, output: &mut impl Write) -> Result<(), Error> {
        output.write_all(MAGIC)?;
        output.write_all(&u32::try_from(self.iter().count())?.to_le_bytes())?;
        for key in self.iter() {
            output.write_all(&u32::try_from(key.len())?.to_le_bytes())?;
            output.write_all(key.as_bytes())?;
        }
        Ok(())
    }
}

impl KeySource for InlineKeys<'_> {
    fn matches(&self, encoded: &[u8]) -> bool {
        let mut input = Input {
            bytes: encoded,
            offset: 12,
        };
        let mut keys = self.iter();
        while input.offset < encoded.len() {
            let Ok(key) = input.text() else { return false };
            if keys.next() != Some(key) {
                return false;
            }
        }
        keys.next().is_none()
    }
}

fn varint_len(mut value: usize) -> usize {
    let mut bytes = 1;
    while value >= 128 {
        value >>= 7;
        bytes += 1;
    }
    bytes
}

struct HashWriter(blake3::Hasher);
impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Input<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> Input<'a> {
    fn take(&mut self, size: usize) -> Result<&'a [u8], Error> {
        let end = self
            .offset
            .checked_add(size)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| error("object key schema is truncated"))?;
        let bytes = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(bytes)
    }
    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }
    fn text(&mut self) -> Result<&'a str, Error> {
        let size = self.u32()? as usize;
        Ok(std::str::from_utf8(self.take(size)?)?)
    }
}

pub(super) fn decode(
    bytes: &[u8],
    cancelled: &mut impl FnMut() -> bool,
) -> Result<Vec<String>, Error> {
    check_replay_cancelled(cancelled)?;
    if bytes.len() > MAX_BYTES || !bytes.starts_with(MAGIC) {
        return Err(error("invalid object key schema header or size"));
    }
    let mut input = Input { bytes, offset: 8 };
    let count = input.u32()? as usize;
    if count == 0
        || count > MAX_KEYS
        || count
            .checked_mul(4)
            .is_none_or(|n| n > bytes.len() - input.offset)
    {
        return Err(error("object key schema count exceeds its bounds"));
    }
    let mut keys = Vec::<String>::new();
    keys.try_reserve_exact(count)?;
    if keys.capacity() > MAX_KEYS {
        return Err(error("object key schema capacity exceeds its bounds"));
    }
    for _ in 0..count {
        check_replay_cancelled(cancelled)?;
        let key = input.text()?;
        if keys.last().is_some_and(|previous| previous.as_str() >= key) {
            return Err(error("object key schema names are not unique and sorted"));
        }
        keys.push(key.to_owned());
    }
    if input.offset != bytes.len() {
        return Err(error("object key schema has trailing bytes"));
    }
    Ok(keys)
}

pub(super) fn deserialize_values<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> Result<Vec<Value>, D::Error> {
    use serde::de::{DeserializeSeed, Error, SeqAccess, Visitor};
    struct Values;
    struct Element {
        full: bool,
    }
    impl<'de> DeserializeSeed<'de> for Element {
        type Value = Value;
        fn deserialize<D: serde::Deserializer<'de>>(self, decoder: D) -> Result<Value, D::Error> {
            if self.full {
                return Err(D::Error::custom("object values exceed the key count limit"));
            }
            Value::deserialize(decoder)
        }
    }
    impl<'de> Visitor<'de> for Values {
        type Value = Vec<Value>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("an object values sequence within the key count limit")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
            let hint = sequence.size_hint().unwrap_or_default();
            if hint > MAX_KEYS {
                return Err(A::Error::custom("object values exceed the key count limit"));
            }
            let mut values = Vec::new();
            values.try_reserve_exact(hint).map_err(A::Error::custom)?;
            if values.capacity() > MAX_KEYS {
                return Err(A::Error::custom("object values capacity exceeds its limit"));
            }
            while let Some(value) = sequence.next_element_seed(Element {
                full: values.len() == MAX_KEYS,
            })? {
                if values.len() == values.capacity() {
                    let capacity = values.capacity().saturating_mul(2).clamp(4, MAX_KEYS);
                    values
                        .try_reserve_exact(capacity - values.len())
                        .map_err(A::Error::custom)?;
                    if values.capacity() > MAX_KEYS {
                        return Err(A::Error::custom("object values capacity exceeds its limit"));
                    }
                }
                values.push(value);
            }
            Ok(values)
        }
    }
    decoder.deserialize_seq(Values)
}

pub(super) fn expand(
    object: KeyedObject,
    reader: &mut HistoryReader,
    owner: u64,
    cache: Option<&mut replay::ExternalCache>,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<StoredObject, Error> {
    if reader
        .capture
        .as_ref()
        .is_some_and(|capture| capture.format == capture::Format::V1)
    {
        return Err(error("keyed objects require project capture version 02"));
    }
    let bytes = reader
        .resources
        .as_ref()
        .ok_or_else(|| error("object key resource directory is missing"))?
        .read_dependency(owner, object.keys, MAX_BYTES, cancelled)?;
    let keys = if let Some(cache) = cache {
        cache.object_keys(object.keys, &bytes, cancelled)?
    } else {
        Arc::new(decode(&bytes, cancelled)?)
    };
    if keys.len() != object.values.len() {
        return Err(error("object key schema and values count differ"));
    }
    let mut properties = BTreeMap::new();
    for (key, value) in keys.iter().zip(object.values) {
        check_replay_cancelled(cancelled)?;
        properties.insert(key.clone(), value);
    }
    Ok(StoredObject {
        object: Object {
            id: object.id,
            generation: object.generation,
            type_name: object.type_name,
            properties,
        },
        references: object.references,
        large_properties: object.large_properties,
        large_references: object.large_references,
    })
}

#[cfg(test)]
mod tests;
