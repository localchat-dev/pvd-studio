use super::*;

mod tests;

pub(in crate::live) const OBJECT: u16 = 1;
pub(in crate::live) const VALUE: u16 = 2;
pub(in crate::live) const REFERENCES: u16 = 4;
pub(in crate::live) const DEBUG: u16 = 8;
pub(in crate::live) const CONTACTS: u16 = 16;
pub(in crate::live) const CHECKPOINT: u16 = 32;
pub(in crate::live) const ANALYSIS_KIND: u16 = 64;
pub(in crate::live) const UNKNOWN_KIND: u16 = 128;
pub(in crate::live) const OBJECT_KEYS: u16 = 256;
pub(in crate::live) const SCENE_KINDS: u16 = 63 | OBJECT_KEYS;
pub(in crate::live) const MAX_ITEMS: usize = 1_048_576;
const MAX_CLASSES: usize = 512;
const MAX_CLASS_BYTES: usize = 128;
pub(in crate::live) const DECODE_ALLOWANCE: usize = 128 << 20;

#[derive(Debug, Default, serde::Serialize)]
pub(in crate::live) struct LogicalBucket {
    pub records: u64,
    pub encoded_bytes: u64,
}

#[derive(Debug, Default, serde::Serialize)]
pub(in crate::live) struct ObjectComponents {
    pub identity_and_type: u64,
    pub property_keys: u64,
    pub key_schema_id: u64,
    pub property_values: u64,
    pub property_map_prefix: u64,
    pub inline_references: u64,
    pub external_property_ids: u64,
    pub external_reference_ids: u64,
    pub record_variant: u64,
}

impl ObjectComponents {
    fn total(&self) -> Result<u64, Error> {
        [
            self.identity_and_type,
            self.property_keys,
            self.key_schema_id,
            self.property_values,
            self.property_map_prefix,
            self.inline_references,
            self.external_property_ids,
            self.external_reference_ids,
            self.record_variant,
        ]
        .into_iter()
        .try_fold(0, add)
    }

    fn merge(&mut self, other: &Self) -> Result<(), Error> {
        self.identity_and_type = add(self.identity_and_type, other.identity_and_type)?;
        self.property_keys = add(self.property_keys, other.property_keys)?;
        self.key_schema_id = add(self.key_schema_id, other.key_schema_id)?;
        self.property_values = add(self.property_values, other.property_values)?;
        self.property_map_prefix = add(self.property_map_prefix, other.property_map_prefix)?;
        self.inline_references = add(self.inline_references, other.inline_references)?;
        self.external_property_ids = add(self.external_property_ids, other.external_property_ids)?;
        self.external_reference_ids =
            add(self.external_reference_ids, other.external_reference_ids)?;
        self.record_variant = add(self.record_variant, other.record_variant)?;
        Ok(())
    }
}

#[derive(Debug, Default, serde::Serialize)]
pub(in crate::live) struct ObjectClass {
    pub records: u64,
    pub encoded_bytes: u64,
    pub components: ObjectComponents,
}

impl ObjectClass {
    fn observe(&mut self, size: u64, components: &ObjectComponents) -> Result<(), Error> {
        self.records = add(self.records, 1)?;
        self.encoded_bytes = add(self.encoded_bytes, size)?;
        self.components.merge(components)
    }
}

#[derive(Debug, serde::Serialize)]
pub(in crate::live) struct SceneRecords {
    pub scope: &'static str,
    pub max_directory_items: usize,
    pub per_record_wire_limit: usize,
    pub per_record_decoded_allowance: usize,
    pub class_limit: usize,
    pub class_name_byte_limit: usize,
    pub physical_kind_bits: BTreeMap<&'static str, u16>,
    pub max_wire_bytes_observed: usize,
    pub logical_by_kind: BTreeMap<&'static str, LogicalBucket>,
    pub object_total: ObjectClass,
    pub object_classes: BTreeMap<String, ObjectClass>,
    pub overflow_classes: ObjectClass,
    #[serde(skip)]
    pub kinds: BTreeMap<u64, u16>,
}

impl SceneRecords {
    pub fn new(limits: Limits) -> Self {
        Self {
            scope: "Unique live SCENE-reachable resource IDs, each decoded once after complete resource hash verification. Logical encoded-record/component bytes can overlap through block deduplication and are not physical bytes. Class overflow is explicit. Temporary diagnostic directories and decoded records are bounded independently of ResidentDomain; limits are not measured allocator or process memory.",
            max_directory_items: limits.items.min(MAX_ITEMS),
            per_record_wire_limit: limits.record_bytes.min(MAX_RECORD),
            per_record_decoded_allowance: DECODE_ALLOWANCE,
            class_limit: MAX_CLASSES,
            class_name_byte_limit: MAX_CLASS_BYTES,
            physical_kind_bits: BTreeMap::from([
                ("object", OBJECT),
                ("value", VALUE),
                ("references", REFERENCES),
                ("debug", DEBUG),
                ("contacts", CONTACTS),
                ("checkpoint", CHECKPOINT),
                ("object_keys", OBJECT_KEYS),
                ("analysis", ANALYSIS_KIND),
                ("unknown", UNKNOWN_KIND),
            ]),
            max_wire_bytes_observed: 0,
            logical_by_kind: BTreeMap::new(),
            object_total: ObjectClass::default(),
            object_classes: BTreeMap::new(),
            overflow_classes: ObjectClass::default(),
            kinds: BTreeMap::new(),
        }
    }

    pub fn observe(
        &mut self,
        id: u64,
        bytes: &[u8],
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), Error> {
        check_replay_cancelled(cancelled)?;
        if self.kinds.contains_key(&id) {
            return Err(error(
                "scene accounting resource was decoded more than once",
            ));
        }
        limited(self.kinds.len().saturating_add(1), self.max_directory_items)?;
        limited(bytes.len(), self.per_record_wire_limit)?;
        if bytes.starts_with(object_keys::MAGIC) {
            object_keys::decode(bytes, cancelled)?;
            self.add_bucket(id, bytes.len(), OBJECT_KEYS, "object_keys")?;
            return check_replay_cancelled(cancelled);
        }
        let record = capture::record::decode_for_accounting(
            bytes,
            cancelled,
            self.per_record_decoded_allowance,
        )?;
        if encoded_bytes(&record)? != bytes.len() as u64 {
            return Err(error(
                "scene accounting resource is not canonically encoded",
            ));
        }
        let (kind, label) = match &record {
            Record::Object(object) => {
                let components = object_components(object, bytes.len() as u64, cancelled)?;
                self.object_total.observe(bytes.len() as u64, &components)?;
                let name = &object.object.type_name;
                if name.len() <= self.class_name_byte_limit
                    && (self.object_classes.contains_key(name)
                        || self.object_classes.len() < self.class_limit)
                {
                    self.object_classes
                        .entry(name.clone())
                        .or_default()
                        .observe(bytes.len() as u64, &components)?;
                } else {
                    self.overflow_classes
                        .observe(bytes.len() as u64, &components)?;
                }
                (OBJECT, "object")
            }
            Record::KeyedObject(object) => {
                let components = keyed_object_components(object, bytes.len() as u64, cancelled)?;
                self.object_total.observe(bytes.len() as u64, &components)?;
                let name = &object.type_name;
                if name.len() <= self.class_name_byte_limit
                    && (self.object_classes.contains_key(name)
                        || self.object_classes.len() < self.class_limit)
                {
                    self.object_classes
                        .entry(name.clone())
                        .or_default()
                        .observe(bytes.len() as u64, &components)?;
                } else {
                    self.overflow_classes
                        .observe(bytes.len() as u64, &components)?;
                }
                (OBJECT, "object")
            }
            Record::Value(_) => (VALUE, "value"),
            Record::References(_) => (REFERENCES, "references"),
            Record::Debug(..) => (DEBUG, "debug"),
            Record::Contacts(..) => (CONTACTS, "contacts"),
            Record::Checkpoint(_) => (CHECKPOINT, "checkpoint"),
            _ => return Err(error("scene accounting found a non-scene resource record")),
        };
        self.add_bucket(id, bytes.len(), kind, label)?;
        check_replay_cancelled(cancelled)
    }

    fn add_bucket(
        &mut self,
        id: u64,
        size: usize,
        kind: u16,
        label: &'static str,
    ) -> Result<(), Error> {
        let bucket = self.logical_by_kind.entry(label).or_default();
        bucket.records = add(bucket.records, 1)?;
        bucket.encoded_bytes = add(bucket.encoded_bytes, size as u64)?;
        self.max_wire_bytes_observed = self.max_wire_bytes_observed.max(size);
        self.kinds.insert(id, kind);
        Ok(())
    }

    pub fn verify(&self) -> Result<(), Error> {
        let records = self
            .logical_by_kind
            .values()
            .try_fold(0, |sum, bucket| add(sum, bucket.records))?;
        let classes = self.object_classes.values().try_fold(
            (
                self.overflow_classes.records,
                self.overflow_classes.encoded_bytes,
            ),
            |(count, bytes), class| {
                Ok::<_, Error>((add(count, class.records)?, add(bytes, class.encoded_bytes)?))
            },
        )?;
        if records != self.kinds.len() as u64
            || self.object_total.components.total()? != self.object_total.encoded_bytes
            || classes != (self.object_total.records, self.object_total.encoded_bytes)
        {
            return Err(error("scene logical accounting does not reconcile"));
        }
        Ok(())
    }
}

fn keyed_object_components(
    object: &KeyedObject,
    size: u64,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<ObjectComponents, Error> {
    let mut result = ObjectComponents {
        identity_and_type: encoded_bytes(&(object.id, object.generation, &object.type_name))?,
        key_schema_id: encoded_bytes(&object.keys)?,
        inline_references: encoded_bytes(&object.references)?,
        external_property_ids: encoded_bytes(&object.large_properties)?,
        external_reference_ids: encoded_bytes(&object.large_references)?,
        ..Default::default()
    };
    for value in &object.values {
        check_replay_cancelled(cancelled)?;
        result.property_values = add(result.property_values, encoded_bytes(value)?)?;
    }
    result.property_map_prefix = encoded_bytes(&object.values)?
        .checked_sub(result.property_values)
        .ok_or_else(|| error("scene keyed values encoding size is inconsistent"))?;
    result.record_variant = size
        .checked_sub(result.total()?)
        .ok_or_else(|| error("scene keyed object encoding size is inconsistent"))?;
    if result.record_variant != 1 || result.total()? != size {
        return Err(error(
            "scene keyed object component partition does not reconcile",
        ));
    }
    Ok(result)
}

fn object_components(
    object: &StoredObject,
    size: u64,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<ObjectComponents, Error> {
    let mut result = ObjectComponents {
        identity_and_type: encoded_bytes(&(
            object.object.id,
            object.object.generation,
            &object.object.type_name,
        ))?,
        inline_references: encoded_bytes(&object.references)?,
        external_property_ids: encoded_bytes(&object.large_properties)?,
        external_reference_ids: encoded_bytes(&object.large_references)?,
        ..Default::default()
    };
    for (key, value) in &object.object.properties {
        check_replay_cancelled(cancelled)?;
        result.property_keys = add(result.property_keys, encoded_bytes(key)?)?;
        result.property_values = add(result.property_values, encoded_bytes(value)?)?;
    }
    result.property_map_prefix = encoded_bytes(&object.object.properties)?
        .checked_sub(add(result.property_keys, result.property_values)?)
        .ok_or_else(|| error("scene property encoding size is inconsistent"))?;
    result.record_variant = size
        .checked_sub(result.total()?)
        .ok_or_else(|| error("scene object encoding size is inconsistent"))?;
    if result.record_variant != 1 || result.total()? != size {
        return Err(error("scene object component partition does not reconcile"));
    }
    Ok(result)
}
