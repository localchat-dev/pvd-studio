//! NVIDIA PVD3 client database v1 import, distinct from SDK communication streams.
use crate::{
    Action, Error, InitialUntimedError, Recording, RecordingBuilder, RecordingSource,
    UntimedErrorEvent, Value, analysis::view::ProfileSource, decode_value, error,
};
use pvd_protocol::{Endian, Event, EventGroup, Header, Name};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

pub(crate) mod analysis_history;
#[cfg(test)]
mod container_tests;
mod contexts;
mod errors;
pub mod export;
mod import;
mod memory_buffer;
pub(crate) mod memory_totals;
mod read_source;
mod settings;
mod snapshot;
mod source_identity;
#[cfg(test)]
mod timeline_tests;
pub(crate) use contexts::SceneContexts;

pub const MAGIC: u32 = 0x3610_1bb0;
const MAX_ITEMS: usize = 1_000_000;
const MAX_METADATA: usize = 64 << 20;

#[derive(Debug)]
pub struct ContainerInfo {
    pub source_bytes: u64,
    pub source_blake3: Option<[u8; 32]>,
    pub object_metadata_bytes: usize,
    pub communication_metadata_bytes: usize,
    pub classes: usize,
    pub properties: usize,
    pub property_messages: usize,
    pub history_events: u64,
    pub history_pages: usize,
    pub source_ids: BTreeMap<u64, u64>,
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }
    fn take(&mut self, len: usize) -> Result<&'a [u8], Error> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or_else(|| error("PVD3 offset overflow"))?;
        let value = self.bytes.get(self.pos..end).ok_or_else(|| {
            error(format!(
                "PVD3 truncated block at {}: need {len} bytes",
                self.pos
            ))
        })?;
        self.pos = end;
        Ok(value)
    }
    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }
    fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into()?))
    }
    fn string(&mut self) -> Result<&'a str, Error> {
        let bytes = self.blob()?;
        if bytes.is_empty() {
            return Ok("");
        }
        if bytes.len() > 32 << 10 || bytes.last() != Some(&0) {
            return Err(error("invalid PVD3 communication string"));
        }
        Ok(std::str::from_utf8(&bytes[..bytes.len() - 1])?)
    }
    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into()?))
    }
    fn count(&mut self) -> Result<usize, Error> {
        let n = self.u32()? as usize;
        if n > MAX_ITEMS {
            return Err(error("PVD3 array exceeds one million entries"));
        }
        if n > self.bytes.len() - self.pos {
            return Err(error("PVD3 array count exceeds containing block"));
        }
        Ok(n)
    }
    fn blob(&mut self) -> Result<&'a [u8], Error> {
        let n = self.u32()? as usize;
        if n > MAX_METADATA {
            return Err(error("PVD3 blob exceeds 64 MiB"));
        }
        self.take(n)
    }
    fn marker(&mut self) -> Result<bool, Error> {
        match self.u32()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(error("PVD3 invalid metadata presence marker")),
        }
    }
    fn name(&mut self) -> Result<Name, Error> {
        Ok(Name {
            namespace: self.u32()?,
            name: self.u32()?,
        })
    }
    fn words(&mut self, width: usize) -> Result<(), Error> {
        let count = self.count()?;
        self.take(count * width)?;
        Ok(())
    }
    fn end(&self) -> Result<(), Error> {
        if self.pos != self.bytes.len() {
            return Err(error(format!(
                "PVD3 block has {} unexplained trailing bytes",
                self.bytes.len() - self.pos
            )));
        }
        Ok(())
    }
}

struct Property {
    owner: Name,
    name: u32,
    datatype: Name,
    datatype_id: u32,
    kind: u8,
    semantic: u32,
    offset32: usize,
    values: Vec<(u32, u32)>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ClassPacking {
    uniform_width: i32,
    class_type: i32,
}

impl Default for ClassPacking {
    fn default() -> Self {
        Self {
            uniform_width: -1,
            class_type: -1,
        }
    }
}

struct Class {
    name: Name,
    base: u32,
    packing: ClassPacking,
    size: usize,
    properties: Vec<u32>,
    dynamic: bool,
    has_pointers: bool,
    string_offsets: Vec<usize>,
}
struct MessageEntry {
    datatype: Name,
    datatype_id: u32,
    property: u32,
    offset: usize,
    size: usize,
    destination_size: usize,
}
struct Message {
    class_name: Name,
    class: u32,
    size: usize,
    entries: Vec<MessageEntry>,
}
struct Metadata {
    strings: BTreeMap<u32, String>,
    properties: Vec<Option<Property>>,
    classes: Vec<Option<Class>>,
    messages: Vec<Option<Message>>,
}

#[derive(Debug)]
struct ProfileCatalogEntry {
    local_id: u64,
    name: String,
    events: Vec<(u16, String, bool)>,
}

struct CommunicationCatalog {
    profiles: Vec<ProfileCatalogEntry>,
    memory: memory_totals::Catalog,
}

#[cfg(test)]
fn profile_catalog(
    comm: &mut Reader<'_>,
    sources: &BTreeMap<u64, u64>,
    allocation: &mut usize,
    proceed: impl FnMut() -> bool,
) -> Result<Vec<ProfileCatalogEntry>, Error> {
    comm.words(12)?;
    comm.words(8)?;
    Ok(profile_catalog_tail(comm, sources, allocation, proceed)?.profiles)
}

fn profile_catalog_tail(
    comm: &mut Reader<'_>,
    sources: &BTreeMap<u64, u64>,
    allocation: &mut usize,
    mut proceed: impl FnMut() -> bool,
) -> Result<CommunicationCatalog, Error> {
    for _ in 0..comm.count()? {
        if !proceed() {
            return Err(cancelled());
        }
        comm.take(12)?;
        comm.words(32)?;
    }
    let count = comm.count()?;
    metadata_budget(allocation, count, size_of::<ProfileCatalogEntry>() + 112)?;
    let mut catalog = Vec::with_capacity(count);
    let mut seen = BTreeSet::new();
    for _ in 0..count {
        if !proceed() {
            return Err(cancelled());
        }
        let source = comm.u64()?;
        comm.take(8)?;
        let name = comm.string()?;
        metadata_budget(allocation, name.len(), 1)?;
        let local_id = u64::from(comm.u32()?);
        if sources.get(&local_id) != Some(&source) || !seen.insert(local_id) {
            return Err(error(
                "PVD3 profile zone has an ambiguous or missing instance mapping",
            ));
        }
        let count = comm.count()?;
        metadata_budget(allocation, count, size_of::<(u16, String, bool)>() + 224)?;
        let mut events = Vec::with_capacity(count);
        for _ in 0..count {
            if !proceed() {
                return Err(cancelled());
            }
            let event_name = comm.string()?;
            metadata_budget(allocation, event_name.len(), 1)?;
            let id = comm.u16()?;
            let enabled = match comm.take(1)?[0] {
                0 => false,
                1 => true,
                _ => return Err(error("invalid PVD3 profile event enabled flag")),
            };
            events.push((id, event_name.to_owned(), enabled));
        }
        catalog.push(ProfileCatalogEntry {
            local_id,
            name: name.to_owned(),
            events,
        });
    }
    let memory = memory_totals::Catalog::read(comm, sources, allocation, &mut proceed)?;
    comm.end()?;
    Ok(CommunicationCatalog {
        profiles: catalog,
        memory,
    })
}

fn register_profile_catalog(
    builder: &mut RecordingBuilder,
    catalog: Vec<ProfileCatalogEntry>,
) -> Result<(), Error> {
    register_profile_catalog_with_progress(builder, catalog, &mut || true)
}

fn register_profile_catalog_with_progress(
    builder: &mut RecordingBuilder,
    catalog: Vec<ProfileCatalogEntry>,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    let mut ambiguous = BTreeSet::new();
    for zone in catalog {
        if !proceed() {
            return Err(cancelled());
        }
        apply_analysis_input(
            builder,
            crate::live::AnalysisInput::Zone(zone.local_id, zone.name),
            proceed,
        )?;
        let mut enabled_flags = BTreeMap::new();
        for (id, name, enabled) in zone.events {
            if enabled_flags
                .insert(id, enabled)
                .is_some_and(|old| old != enabled)
            {
                return Err(error(
                    "PVD3 profile catalog has conflicting event enabled definitions",
                ));
            }
            apply_analysis_input(
                builder,
                crate::live::AnalysisInput::Event(zone.local_id, id, name),
                proceed,
            )?;
            if builder
                .recording
                .analysis
                .profile_metadata_ambiguous(zone.local_id, id)
            {
                ambiguous.insert((zone.local_id, id));
            }
        }
    }
    if !ambiguous.is_empty() {
        builder.recording.diagnostics.push(format!(
            "PVD3 saved profile catalog contains {} conflicting event definitions; affected series remain metadata_ambiguous and have no curve values.",
            ambiguous.len()
        ));
    }
    Ok(())
}
impl Metadata {
    fn parse(bytes: &[u8]) -> Result<Self, Error> {
        Self::parse_with_progress(bytes, &mut || true)
    }
    fn parse_with_progress(
        bytes: &[u8],
        progress: &mut dyn FnMut() -> bool,
    ) -> Result<Self, Error> {
        let mut allocation = 0usize;
        let mut r = Reader::new(bytes);
        if r.u32()? != 1 {
            return Err(error("unsupported PVD3 object metadata version"));
        }
        r.u32()?;
        let count = r.count()?;
        metadata_budget(&mut allocation, count, 96)?;
        r.u32()?;
        let mut strings = BTreeMap::from([(0, String::new())]);
        for _ in 0..count {
            if !progress() {
                return Err(cancelled());
            }
            let id = r.u32()?;
            let bytes = r.blob()?;
            if bytes.is_empty() || bytes.last() != Some(&0) || bytes.len() > 32 << 10 {
                return Err(error("invalid PVD3 metadata string"));
            }
            let value = std::str::from_utf8(&bytes[..bytes.len() - 1])?;
            metadata_budget(&mut allocation, value.len(), 1)?;
            let value = value.to_owned();
            if strings.insert(id, value).is_some() {
                return Err(error("duplicate PVD3 string handle"));
            }
        }
        let count = r.count()?;
        metadata_budget(
            &mut allocation,
            count,
            std::mem::size_of::<Option<Property>>(),
        )?;
        let mut properties = Vec::with_capacity(count);
        for index in 0..count {
            if !progress() {
                return Err(cancelled());
            }
            if !r.marker()? {
                properties.push(None);
                continue;
            }
            let owner = r.name()?;
            r.u32()?;
            let semantic = r.u32()?;
            let datatype_id = r.u32()?;
            let datatype = r.name()?;
            let kind = r.u32()?;
            if !(1..=2).contains(&kind) {
                return Err(error("unsupported PVD3 property kind"));
            }
            if r.u32()? as usize != index {
                return Err(error("PVD3 property ID differs from table index"));
            }
            let offset32 = r.u32()? as usize;
            r.u32()?;
            let count = r.count()?;
            metadata_budget(&mut allocation, count, 8)?;
            let mut values = Vec::with_capacity(count);
            for _ in 0..count {
                if !progress() {
                    return Err(cancelled());
                }
                let value = r.u32()?;
                values.push((r.u32()?, value));
            }
            let name = r.u32()?;
            properties.push(Some(Property {
                owner,
                name,
                datatype,
                datatype_id,
                kind: kind as u8,
                semantic,
                offset32,
                values,
            }));
        }
        let count = r.count()?;
        metadata_budget(&mut allocation, count, std::mem::size_of::<Option<Class>>())?;
        let mut classes = Vec::with_capacity(count);
        for index in 0..count {
            if !progress() {
                return Err(cancelled());
            }
            if !r.marker()? {
                classes.push(None);
                continue;
            }
            let name = r.name()?;
            if r.u32()? as usize != index {
                return Err(error("PVD3 class ID differs from table index"));
            }
            let base = r.u32()?;
            let packing = ClassPacking {
                uniform_width: r.u32()? as i32,
                class_type: r.u32()? as i32,
            };
            let flags = r.take(2)?;
            if flags.iter().any(|flag| *flag > 1) {
                return Err(error("invalid PVD3 class flags"));
            }
            let dynamic = flags[1] != 0;
            let size = r.u32()? as usize;
            r.take(20)?;
            let count = r.count()?;
            metadata_budget(&mut allocation, count, size_of::<u32>())?;
            let mut class_properties = Vec::with_capacity(count);
            for _ in 0..count {
                if !progress() {
                    return Err(cancelled());
                }
                class_properties.push(r.u32()?);
            }
            let pointer_count = r.count()?;
            metadata_budget(&mut allocation, pointer_count, 8)?;
            let mut has_pointers = pointer_count != 0;
            let mut string_offsets = Vec::new();
            for _ in 0..pointer_count {
                if !progress() {
                    return Err(cancelled());
                }
                let kind = r.u32()?;
                let offset = r.u32()? as usize;
                if kind == 2 {
                    string_offsets.push(offset);
                }
            }
            let pointer_count = r.count()?;
            has_pointers |= pointer_count != 0;
            r.take(pointer_count * 8)?;
            classes.push(Some(Class {
                name,
                base,
                packing,
                size,
                properties: class_properties,
                dynamic,
                has_pointers,
                string_offsets,
            }));
        }
        let count = r.count()?;
        metadata_budget(
            &mut allocation,
            count,
            std::mem::size_of::<Option<Message>>(),
        )?;
        let mut messages = Vec::with_capacity(count);
        for index in 0..count {
            if !progress() {
                return Err(cancelled());
            }
            if !r.marker()? {
                messages.push(None);
                continue;
            }
            let class_name = r.name()?;
            let class = r.u32()?;
            r.name()?;
            if r.u32()? as usize != index {
                return Err(error("PVD3 message ID differs from table index"));
            }
            let size = r.u32()? as usize;
            let count = r.count()?;
            metadata_budget(&mut allocation, count, std::mem::size_of::<MessageEntry>())?;
            let mut entries = Vec::with_capacity(count);
            for _ in 0..count {
                if !progress() {
                    return Err(cancelled());
                }
                let datatype = r.name()?;
                let datatype_id = r.u32()?;
                let offset = r.u32()? as usize;
                let len = r.u32()? as usize;
                let destination_size = r.u32()? as usize;
                let property = r.u32()?;
                if offset.checked_add(len).is_none_or(|end| end > size) {
                    return Err(error("PVD3 message property exceeds message size"));
                }
                entries.push(MessageEntry {
                    datatype,
                    datatype_id,
                    property,
                    offset,
                    size: len,
                    destination_size,
                });
            }
            r.words(4)?;
            messages.push(Some(Message {
                class_name,
                class,
                size,
                entries,
            }));
        }
        r.end()?;
        Ok(Self {
            strings,
            properties,
            classes,
            messages,
        })
    }
    fn property(&self, id: u32) -> Result<&Property, Error> {
        self.properties
            .get(id as usize)
            .and_then(Option::as_ref)
            .ok_or_else(|| error(format!("undefined PVD3 property {id}")))
    }
    fn class(&self, id: u32) -> Result<&Class, Error> {
        self.classes
            .get(id as usize)
            .and_then(Option::as_ref)
            .ok_or_else(|| error(format!("undefined PVD3 class {id}")))
    }
    fn register(&self, builder: &mut RecordingBuilder) -> Result<(), Error> {
        self.register_with_progress(builder, &mut || true)
    }
    fn register_with_progress(
        &self,
        builder: &mut RecordingBuilder,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        for (&handle, value) in &self.strings {
            if !proceed() {
                return Err(cancelled());
            }
            push(
                builder,
                Event::StringHandle {
                    handle,
                    value: value.clone(),
                },
            )?;
        }
        for class in self.classes.iter().flatten() {
            if !proceed() {
                return Err(cancelled());
            }
            push(builder, Event::CreateClass { name: class.name })?;
        }
        for class in self.classes.iter().flatten() {
            if !proceed() {
                return Err(cancelled());
            }
            if class.base != u32::MAX {
                push(
                    builder,
                    Event::DeriveClass {
                        parent: self.class(class.base)?.name,
                        child: class.name,
                    },
                )?;
            }
        }
        let mut missing_semantics = 0;
        for p in self.properties.iter().flatten() {
            if !proceed() {
                return Err(cancelled());
            }
            let semantic = if p.semantic != 0 && !self.strings.contains_key(&p.semantic) {
                missing_semantics += 1;
                0
            } else {
                p.semantic
            };
            push(
                builder,
                Event::CreateProperty {
                    class: p.owner,
                    name: p.name,
                    semantic,
                    datatype: p.datatype,
                    kind: p.kind,
                    named_values: p.values.clone(),
                },
            )?;
        }
        if missing_semantics != 0 {
            builder.recording.diagnostics.push(format!("PVD3 metadata contains {missing_semantics} unresolved semantic string handles; their labels are unavailable."));
        }
        Ok(())
    }
    fn value(
        &self,
        builder: &RecordingBuilder,
        property: &Property,
        data: &[u8],
        array: bool,
    ) -> Result<Value, Error> {
        let class = self.class(property.datatype_id)?;
        if class.size == 0 || !data.len().is_multiple_of(class.size) {
            return Err(error("PVD3 property has an invalid packed byte length"));
        }
        let items = data.len() / class.size;
        if items > MAX_ITEMS || (!array && items != 1) {
            return Err(error("PVD3 property item count differs from metadata"));
        }
        let typename = builder.name(property.datatype)?;
        let type_name = typename.rsplit("::").next().unwrap_or(&typename);
        let nested_values = match type_name {
            "PxVec2" | "PvdHullPolygonData" | "PxClothParticle" | "PxClothFabricPhase" => 2,
            "PxHeightFieldSample" => 3,
            "PxVec4" | "U32Array4" => 4,
            "PxBounds3" => 6,
            "PxMat33" => 9,
            "PxMat44" => 16,
            "PvdRaycast" | "PvdSweep" | "PvdOverlap" | "PvdSqHit" => 40,
            _ => 0,
        };
        let value_bytes = items
            .checked_mul((nested_values + 1) * std::mem::size_of::<Value>())
            .and_then(|v| v.checked_add(data.len()))
            .ok_or_else(|| error("PVD3 decoded value size overflow"))?;
        value_budget(builder, value_bytes)?;
        if type_name == "StringHandle" {
            if class.size != 4 {
                return Err(error("unsupported PVD3 string-handle width"));
            }
            let mut expanded_bytes = value_bytes;
            for handle in data.chunks_exact(4) {
                let handle = u32::from_le_bytes(handle.try_into()?);
                let text = self
                    .strings
                    .get(&handle)
                    .ok_or_else(|| error(format!("undefined PVD3 string handle {handle}")))?;
                expanded_bytes = expanded_bytes
                    .checked_add(text.len())
                    .ok_or_else(|| error("PVD3 string array size overflow"))?;
                value_budget(builder, expanded_bytes)?;
            }
            let mut values = Vec::with_capacity(items);
            for bytes in data.chunks_exact(4) {
                values.push(Value::String(
                    builder.string(u32::from_le_bytes(bytes.try_into()?))?,
                ));
            }
            return Ok(if array {
                Value::Array(values.into())
            } else {
                values.into_iter().next().unwrap()
            });
        }
        if matches!(type_name, "ObjectRef" | "VoidPtr") {
            if class.size != 4 {
                return Err(error("unsupported PVD3 internal reference width"));
            }
            let values = data
                .chunks_exact(4)
                .map(|b| Value::Ref(u64::from(u32::from_le_bytes(b.try_into().unwrap()))))
                .collect::<Vec<_>>();
            return Ok(if array {
                Value::Array(values.into())
            } else {
                values.into_iter().next().unwrap()
            });
        }
        let mut expanded = Vec::new();
        let data = if matches!(
            typename.as_str(),
            "physx3::PvdRaycast" | "physx3::PvdSweep" | "physx3::PvdOverlap"
        ) {
            let expected = match typename.as_str() {
                "physx3::PvdRaycast" => (64, &[52][..]),
                "physx3::PvdSweep" => (72, &[24, 36, 48, 60][..]),
                _ => (76, &[52, 64][..]),
            };
            if class.size != expected.0 || class.string_offsets != expected.1 || items > 100_000 {
                return Err(error("unsupported PVD3 scene-query layout or record limit"));
            }
            let mut expanded_bytes = data.len();
            for record in data.chunks_exact(class.size) {
                for offset in &class.string_offsets {
                    let handle = record
                        .get(*offset..*offset + 4)
                        .ok_or_else(|| error("PVD3 string offset exceeds record"))?;
                    let handle = u32::from_le_bytes(handle.try_into()?);
                    let value = self
                        .strings
                        .get(&handle)
                        .ok_or_else(|| error(format!("undefined PVD3 string handle {handle}")))?;
                    if value.len() > 256 {
                        return Err(error("PVD3 scene-query array name exceeds 256 bytes"));
                    }
                    expanded_bytes = expanded_bytes
                        .checked_add(value.len() + 1)
                        .ok_or_else(|| error("PVD3 expanded scene-query size overflow"))?;
                }
            }
            value_budget(
                builder,
                value_bytes
                    .checked_add(expanded_bytes)
                    .ok_or_else(|| error("PVD3 decoded scene-query size overflow"))?,
            )?;
            expanded.try_reserve_exact(expanded_bytes)?;
            for record in data.chunks_exact(class.size) {
                expanded.extend_from_slice(record);
                for offset in &class.string_offsets {
                    let handle = u32::from_le_bytes(record[*offset..*offset + 4].try_into()?);
                    let value = &self.strings[&handle];
                    expanded.extend_from_slice(value.as_bytes());
                    expanded.push(0);
                }
            }
            expanded.as_slice()
        } else {
            data
        };
        decode_value(
            &typename,
            data,
            items as u32,
            array,
            &builder.recording.header,
            &self.strings,
        )
    }
    #[cfg(test)]
    fn set(
        &self,
        builder: &mut RecordingBuilder,
        id: u64,
        property: u32,
        data: &[u8],
    ) -> Result<(), Error> {
        self.set_with_progress(builder, id, property, data, &mut || true)
    }

    fn set_with_progress(
        &self,
        builder: &mut RecordingBuilder,
        id: u64,
        property: u32,
        data: &[u8],
        progress: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let p = self.property(property)?;
        let name = builder.string(p.name)?;
        property_owner(builder, id, p)?;
        if self.is_error_property(builder, p)? {
            return Err(error(
                "PVD3 error array replacement is not supported; expected an appended error record",
            ));
        }
        if name == "events" {
            value_budget(builder, data.len())?;
            let object_type = builder.state.objects[&id].type_name.clone();
            if matches!(
                object_type.as_str(),
                "_debugger_::PxProfileZone"
                    | "_debugger_::PxProfileMemoryEventBuffer"
                    | "_debugger_::PvdUserRenderer"
            ) {
                if p.kind != 2
                    || builder.name(p.datatype)? != "physx3::PvdU8"
                    || metadata_size(self, p)? != 1
                {
                    return Err(error("unsupported PVD3 event buffer property layout"));
                }
                let frame = received_frame(builder);
                if object_type == "_debugger_::PvdUserRenderer" {
                    let limit = MAX_METADATA
                        .min(builder.memory_limit.saturating_sub(builder.retained_bytes));
                    let primitives = builder
                        .render_decoders
                        .entry(id)
                        .or_default()
                        .decode_bounded(data, Endian::Little, limit, progress)?;
                    if !primitives.is_empty() {
                        builder.action(Action::Debug(id, primitives.into()))?;
                    }
                } else if object_type == "_debugger_::PxProfileZone" {
                    if builder.recording.analysis.profile_zone_name(id).is_none() {
                        return Err(error("PVD3 profile buffer has no saved zone catalog"));
                    }
                    apply_analysis_input(
                        builder,
                        crate::live::AnalysisInput::Buffer {
                            id,
                            kind: object_type,
                            bytes: Arc::from(data),
                            little_endian: true,
                            frame,
                        },
                        progress,
                    )?;
                } else if builder.streaming.is_some() {
                    let normalized = memory_buffer::normalize(data, &self.strings, progress)?;
                    apply_analysis_input(
                        builder,
                        crate::live::AnalysisInput::Buffer {
                            id,
                            kind: object_type,
                            bytes: normalized.into(),
                            little_endian: true,
                            frame,
                        },
                        progress,
                    )?;
                } else {
                    builder
                        .recording
                        .analysis
                        .legacy_mut()?
                        .ingest_native_memory(id, data, frame, |handle| {
                            self.strings
                                .get(&handle)
                                .map(String::as_str)
                                .ok_or_else(|| {
                                    error(format!("undefined PVD3 memory string handle {handle}"))
                                })
                        })?;
                }
            }
            return builder.action(Action::Set(id, name, Value::Bytes(Arc::from(data))));
        }
        if name == "Contacts" && builder.name(p.datatype)? == "physx3::PvdContact" {
            let size = metadata_size(self, p)?;
            if !data.len().is_multiple_of(size) {
                return Err(error("invalid PVD3 contact array length"));
            }
            value_budget(
                builder,
                data.len() / size * std::mem::size_of::<crate::contacts::Contact>(),
            )?;
            let contacts = crate::contacts::decode(
                data,
                (data.len() / size) as u32,
                &builder.recording.header,
            )?;
            builder.action(Action::Contacts(id, contacts.into()))?;
        }
        let value = self.value(builder, p, data, p.kind == 2)?;
        builder.action(Action::Set(id, name, value))
    }

    fn is_error_property(
        &self,
        builder: &RecordingBuilder,
        property: &Property,
    ) -> Result<bool, Error> {
        Ok(
            builder.name(property.owner)? == "_debugger_::UserCustomedEvent"
                && builder.string(property.name)? == "Events",
        )
    }

    fn validate_error_layout(
        &self,
        builder: &RecordingBuilder,
        property: &Property,
    ) -> Result<(), Error> {
        let class = self.class(property.datatype_id)?;
        if property.kind != 2
            || builder.name(property.datatype)? != "_debugger_::PvdErrorMessage"
            || class.name != property.datatype
            || class.size != 20
            || class.base != u32::MAX
            || class.string_offsets != [4, 8]
        {
            return Err(error("unsupported PVD3 error record layout"));
        }
        let expected = [
            ("Code", "physx3::PvdU32", 0),
            ("Message", "physx3::StringHandle", 4),
            ("File", "physx3::StringHandle", 8),
            ("Line", "physx3::PvdU32", 12),
            ("Index", "physx3::PvdU32", 16),
        ];
        let mut found = [false; 5];
        for field in self
            .properties
            .iter()
            .flatten()
            .filter(|p| p.owner == class.name)
        {
            let name = builder.string(field.name)?;
            let Some(index) = expected.iter().position(|(label, _, _)| *label == name) else {
                return Err(error("unsupported PVD3 error record field"));
            };
            if found[index]
                || field.kind != 1
                || field.offset32 != expected[index].2
                || builder.name(field.datatype)? != expected[index].1
                || self.class(field.datatype_id)?.name != field.datatype
                || self.class(field.datatype_id)?.size != 4
            {
                return Err(error("unsupported PVD3 error record field layout"));
            }
            found[index] = true;
        }
        if !found.into_iter().all(|value| value) {
            return Err(error(
                "PVD3 error record metadata is missing a required field",
            ));
        }
        Ok(())
    }
}

fn metadata_budget(bytes: &mut usize, count: usize, size: usize) -> Result<(), Error> {
    *bytes = bytes
        .checked_add(
            count
                .checked_mul(size)
                .ok_or_else(|| error("PVD3 metadata allocation overflow"))?,
        )
        .ok_or_else(|| error("PVD3 metadata allocation overflow"))?;
    if *bytes > MAX_METADATA {
        return Err(error("PVD3 decoded metadata exceeds 64 MiB memory budget"));
    }
    Ok(())
}

fn metadata_size(metadata: &Metadata, property: &Property) -> Result<usize, Error> {
    let size = metadata.class(property.datatype_id)?.size;
    if size == 0 {
        return Err(error("PVD3 datatype has zero packed size"));
    }
    Ok(size)
}

fn value_budget(builder: &RecordingBuilder, bytes: usize) -> Result<(), Error> {
    let state_bytes = builder
        .streaming
        .as_ref()
        .map_or(0, |streaming| streaming.state_bytes);
    if bytes > MAX_METADATA
        || bytes
            > builder
                .memory_limit
                .saturating_sub(builder.retained_bytes.saturating_add(state_bytes))
    {
        return Err(error(
            "PVD3 decoded property exceeds 64 MiB or remaining history memory budget",
        ));
    }
    Ok(())
}

fn received_frame(builder: &RecordingBuilder) -> usize {
    builder
        .streaming
        .as_ref()
        .map_or(builder.recording.frames.len(), |streaming| {
            streaming.frames_seen
        })
}

fn apply_analysis_input(
    builder: &mut RecordingBuilder,
    input: crate::live::AnalysisInput,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    builder.mutate_analysis(|analysis| input.apply_shared(analysis, &mut || !proceed()))?;
    if let Some(streaming) = &mut builder.streaming {
        streaming.analysis_input(input);
    }
    Ok(())
}

fn property_owner(builder: &RecordingBuilder, id: u64, property: &Property) -> Result<(), Error> {
    let owner = builder.name(property.owner)?;
    let object = builder
        .state
        .objects
        .get(&id)
        .ok_or_else(|| error("PVD3 property targets a missing instance"))?;
    let mut class = object.type_name.as_str();
    for _ in 0..128 {
        if class == owner {
            return Ok(());
        }
        let Some(parent) = builder
            .recording
            .classes
            .get(class)
            .and_then(|c| c.parent.as_deref())
        else {
            break;
        };
        class = parent;
    }
    Err(error(format!(
        "PVD3 property owner {owner} is not an ancestor of instance {id} ({})",
        object.type_name
    )))
}

fn push(builder: &mut RecordingBuilder, event: Event) -> Result<(), Error> {
    builder.push(EventGroup {
        stream_id: 0,
        timestamp: 0,
        offset: 0,
        events: vec![event],
    })
}
fn next_page(value: usize) -> Result<usize, Error> {
    (value / 4096)
        .checked_add(1)
        .and_then(|v| v.checked_mul(4096))
        .ok_or_else(|| error("PVD3 page alignment overflow"))
}
#[cfg(test)]
fn checked_range(bytes: &[u8], offset: u64, len: u64) -> Result<&[u8], Error> {
    let end = offset
        .checked_add(len)
        .ok_or_else(|| error("PVD3 file range overflow"))?;
    bytes
        .get(usize::try_from(offset)?..usize::try_from(end)?)
        .ok_or_else(|| error("PVD3 file range exceeds capture size"))
}
fn cancelled() -> Error {
    std::io::Error::new(std::io::ErrorKind::Interrupted, "PVD3 import cancelled").into()
}

/// Import the verified little-endian client database v1 subset. Progress is history events.
pub fn from_bytes_with_progress(
    bytes: &[u8],
    progress: impl FnMut(u64, u64) -> bool,
) -> Result<Recording, Error> {
    read_with_progress(bytes, progress).map(|(recording, _)| recording)
}

pub fn read_with_progress(
    bytes: &[u8],
    progress: impl FnMut(u64, u64) -> bool,
) -> Result<(Recording, ContainerInfo), Error> {
    if bytes.len() > 512 << 20 {
        return Err(error("PVD3 recording exceeds 512 MiB"));
    }
    import::read_source_with_progress(std::io::Cursor::new(bytes), None, progress)
}

/// Imports immutable client database bytes into disk history. Progress is history events.
pub fn read_bytes_with_history(
    bytes: &[u8],
    history: crate::live::HistoryConfig,
    progress: impl FnMut(u64, u64) -> bool,
) -> Result<(Recording, ContainerInfo), Error> {
    if bytes.len() > 512 << 20 {
        return Err(error("PVD3 recording exceeds 512 MiB"));
    }
    import::read_source_with_progress(std::io::Cursor::new(bytes), Some(history), progress)
}

/// Loads the verified client database v1 subset into disk history. Progress is history events.
pub fn read_path_with_progress(
    path: impl AsRef<std::path::Path>,
    history: crate::live::HistoryConfig,
    mut progress: impl FnMut(u64, u64) -> bool,
) -> Result<(Recording, ContainerInfo), Error> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        options.share_mode(FILE_SHARE_READ);
    }
    let mut file = options.open(path)?;
    let before = file.metadata()?;
    if !before.is_file() {
        return Err(error("PVD3 source must be a regular file"));
    }
    let modified = before.modified()?;
    let result = if before.len() > 512 << 20 {
        let mut checked = source_identity::CheckedSource::new(&mut file, |_, _| progress(0, 0))?;
        let (recording, mut info) =
            import::read_source_with_progress(&mut checked, Some(history), &mut progress)?;
        checked.recheck(|_, _| progress(info.history_events, info.history_events))?;
        info.source_blake3 = Some(*checked.digest().as_bytes());
        (recording, info)
    } else {
        import::read_source_with_progress(&mut file, Some(history), progress)?
    };
    let after = file.metadata()?;
    if before.len() != after.len() || modified != after.modified()? {
        return Err(error("PVD3 source changed during import"));
    }
    Ok(result)
}

fn validate_saved_event(bytes: &[u8], instances: &BTreeMap<u64, (u32, u32)>) -> Result<(), Error> {
    let mut event = Reader::new(bytes);
    if event.u32()? == 10 {
        let id = event.u32()?;
        let position = event.u64()?;
        if instances.get(&position).map(|entry| entry.0) != Some(id) {
            return Err(error(
                "PVD3 saved instance event differs from its directory",
            ));
        }
        event.end()?;
    }
    Ok(())
}

#[cfg(test)]
fn import_event(
    metadata: &Metadata,
    source_ids: &BTreeMap<u64, Vec<u64>>,
    builder: &mut RecordingBuilder,
    bytes: &[u8],
    index: u64,
    data_len: usize,
) -> Result<(), Error> {
    import_event_with_progress(
        metadata,
        source_ids,
        builder,
        bytes,
        index,
        data_len,
        &mut || true,
    )
}

fn import_event_with_progress(
    metadata: &Metadata,
    source_ids: &BTreeMap<u64, Vec<u64>>,
    builder: &mut RecordingBuilder,
    bytes: &[u8],
    index: u64,
    data_len: usize,
    progress: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    let mut r = Reader::new(bytes);
    let kind = r.u32()?;
    match kind {
        1 => {
            let id = u64::from(r.u32()?);
            let class = metadata.class(r.u32()?)?;
            push(
                builder,
                Event::CreateInstance {
                    class: class.name,
                    id,
                },
            )?;
        }
        2 => {
            let id = u64::from(r.u32()?);
            if !builder.state.objects.contains_key(&id) {
                return Err(error("destroy targets a missing PVD3 instance"));
            }
            push(builder, Event::DestroyInstance { id })?;
        }
        3 | 4 => {
            let id = u64::from(r.u32()?);
            let property = r.u32()?;
            let data = r.blob()?;
            if kind == 3 {
                metadata.set_with_progress(builder, id, property, data, progress)?;
            } else {
                let msg = metadata
                    .messages
                    .get(property as usize)
                    .and_then(Option::as_ref)
                    .ok_or_else(|| error("undefined PVD3 property message"))?;
                if data.len() < msg.size {
                    return Err(error("PVD3 property message is truncated"));
                }
                for entry in &msg.entries {
                    metadata.set_with_progress(
                        builder,
                        id,
                        entry.property,
                        &data[entry.offset..entry.offset + entry.size],
                        progress,
                    )?;
                }
            }
            let count = r.count()?;
            for _ in 0..count {
                if u64::from(r.u32()?) > index {
                    return Err(error("PVD3 undo reference points past current event"));
                }
            }
        }
        5..=7 => {
            let id = u64::from(r.u32()?);
            let p = metadata.property(r.u32()?)?;
            property_owner(builder, id, p)?;
            if builder.name(p.owner)? == "_debugger_::PvdUserRenderer"
                && builder.string(p.name)? == "events"
            {
                return Err(error(
                    "PVD3 renderer array editing is not supported; expected a complete event buffer",
                ));
            }
            let is_error = metadata.is_error_property(builder, p)?;
            if is_error {
                metadata.validate_error_layout(builder, p)?;
                if kind != 5 {
                    return Err(error(
                        "PVD3 error array editing is not supported; expected an appended error record",
                    ));
                }
            }
            if p.kind != 2 {
                return Err(error("PVD3 array operation targets scalar property"));
            }
            let element = r.u32()? as usize;
            let key = builder.string(p.name)?;
            let object = builder
                .state
                .objects
                .get(&id)
                .ok_or_else(|| error("PVD3 array operation targets missing instance"))?;
            if let Some(value) = object.properties.get(&key) {
                value_budget(builder, value.retained_bytes())?;
            }
            let mut values = match object.properties.get(&key) {
                None => Vec::new(),
                Some(Value::Array(values)) => values.as_ref().clone(),
                _ => return Err(error("PVD3 array operation targets opaque array")),
            };
            if is_error && element != values.len() {
                return Err(error(
                    "PVD3 error array insertion must append in history order",
                ));
            }
            if kind == 6 {
                if element >= values.len() {
                    return Err(error("PVD3 array removal index out of bounds"));
                }
                values.remove(element);
                r.words(4)?;
            } else {
                let payload = r.blob()?;
                if is_error {
                    metadata.append_error(builder, id, payload, index)?;
                }
                let value = metadata.value(builder, p, payload, false)?;
                if kind == 5 {
                    if element > values.len() || values.len() >= MAX_ITEMS {
                        return Err(error("PVD3 array insertion index or size exceeds limit"));
                    }
                    values.insert(element, value);
                } else {
                    *values
                        .get_mut(element)
                        .ok_or_else(|| error("PVD3 array update index out of bounds"))? = value;
                    r.words(4)?;
                }
            }
            builder.action(Action::Set(id, key, Value::Array(Arc::new(values))))?;
        }
        8 => {
            let class = metadata.class(r.u32()?)?;
            let data = r.blob()?;
            if builder.name(class.name)? != "_debugger_::SectionEventData" {
                return Err(error("unsupported PVD3 user event class"));
            }
            if data.len() != 24 {
                return Err(error("unsupported PVD3 SectionEventData packed layout"));
            }
            let mut section = Reader::new(data);
            let source = section.u64()?;
            let timestamp = section.u64()?;
            let name = section.u32()?;
            let begin = section.take(1)?[0];
            if begin > 1 {
                return Err(error("invalid PVD3 section flag"));
            }
            if begin == 0 && builder.string(name)? == "frame" {
                let candidates = source_ids
                    .get(&source)
                    .ok_or_else(|| error("PVD3 frame source has no instance mapping"))?;
                let mut alive = candidates.iter().filter(|id| {
                    builder
                        .state
                        .objects
                        .get(id)
                        .is_some_and(|o| o.type_name.ends_with("::PxScene"))
                });
                let id = *alive
                    .next()
                    .ok_or_else(|| error("PVD3 frame source has no live scene"))?;
                if alive.next().is_some() {
                    return Err(error("PVD3 frame source maps to multiple live scenes"));
                }
                push(
                    builder,
                    Event::EndSection {
                        id,
                        name,
                        timestamp,
                    },
                )?;
            }
        }
        9 => {}
        10 => {
            r.u32()?;
            let position = r.u64()?;
            if position >= data_len as u64 {
                return Err(error("PVD3 saved instance position exceeds capture size"));
            }
        }
        11 => {
            let id = u64::from(r.u32()?);
            let shift = [
                f32::from_bits(r.u32()?),
                f32::from_bits(r.u32()?),
                f32::from_bits(r.u32()?),
            ];
            r.words(8)?;
            r.end()?;
            builder.shift_origin(id, shift, progress)?;
        }
        _ => {
            return Err(error(format!(
                "unsupported PVD3 history event type {kind}; import stopped at its verified page boundary"
            )));
        }
    }
    r.end()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn renderer_fixture() -> (Metadata, RecordingBuilder) {
        let renderer = Name {
            namespace: 1,
            name: 2,
        };
        let byte = Name {
            namespace: 3,
            name: 4,
        };
        let metadata = Metadata {
            strings: [
                "",
                "_debugger_",
                "PvdUserRenderer",
                "physx3",
                "PvdU8",
                "events",
                "frame",
            ]
            .into_iter()
            .enumerate()
            .map(|(i, s)| (i as u32, s.to_owned()))
            .collect(),
            classes: vec![
                Some(Class {
                    name: renderer,
                    base: u32::MAX,
                    packing: crate::native::ClassPacking::default(),
                    size: 0,
                    properties: vec![0],
                    dynamic: true,
                    has_pointers: false,
                    string_offsets: vec![],
                }),
                Some(Class {
                    name: byte,
                    base: u32::MAX,
                    packing: crate::native::ClassPacking::default(),
                    size: 1,
                    properties: vec![],
                    dynamic: false,
                    has_pointers: false,
                    string_offsets: vec![],
                }),
            ],
            properties: vec![Some(Property {
                owner: renderer,
                name: 5,
                datatype: byte,
                datatype_id: 1,
                kind: 2,
                semantic: 0,
                offset32: 0,
                values: vec![],
            })],
            messages: vec![],
        };
        let mut builder = catalog_builder();
        metadata.register(&mut builder).unwrap();
        push(
            &mut builder,
            Event::CreateInstance {
                class: renderer,
                id: 5,
            },
        )
        .unwrap();
        (metadata, builder)
    }

    fn renderer_point(instance: Option<u64>) -> Vec<u8> {
        let mut data = Vec::new();
        if let Some(instance) = instance {
            data.push(1);
            data.extend(instance.to_le_bytes());
        }
        data.push(2);
        data.extend(1u32.to_le_bytes());
        for value in [1.0f32, 2.0, 3.0] {
            data.extend(value.to_le_bytes());
        }
        data.extend(0xff123456u32.to_le_bytes());
        data
    }

    #[test]
    fn renderer_buffers_append_clear_and_reset_on_renderer_generation() {
        let (metadata, mut builder) = renderer_fixture();
        let frame = |builder: &mut RecordingBuilder, timestamp| {
            push(
                builder,
                Event::EndSection {
                    id: 1,
                    name: 6,
                    timestamp,
                },
            )
            .unwrap();
        };
        metadata
            .set(&mut builder, 5, 0, &renderer_point(Some(42)))
            .unwrap();
        metadata
            .set(&mut builder, 5, 0, &renderer_point(None))
            .unwrap();
        frame(&mut builder, 100);
        metadata.set(&mut builder, 5, 0, &[]).unwrap();
        frame(&mut builder, 200);
        metadata
            .set(&mut builder, 5, 0, &renderer_point(None))
            .unwrap();
        frame(&mut builder, 300);
        push(&mut builder, Event::DestroyInstance { id: 5 }).unwrap();
        push(
            &mut builder,
            Event::CreateInstance {
                class: Name {
                    namespace: 1,
                    name: 2,
                },
                id: 5,
            },
        )
        .unwrap();
        metadata
            .set(&mut builder, 5, 0, &renderer_point(None))
            .unwrap();
        frame(&mut builder, 400);
        metadata
            .set(&mut builder, 5, 0, &renderer_point(Some(99)))
            .unwrap();
        push(&mut builder, Event::DestroyInstance { id: 5 }).unwrap();
        frame(&mut builder, 500);
        let recording = builder.finish().unwrap();
        recording.verify_replay().unwrap();
        for frame in [0, 1, 2, 3, 4, 3, 2, 1, 0, 4, 2, 0, 3] {
            let state = recording.state_at(frame).unwrap();
            let expected = match frame {
                0 => Some((2, 42)),
                2 => Some((1, 42)),
                3 => Some((1, 0)),
                _ => None,
            };
            if let Some((count, instance)) = expected {
                let primitives = &state.debug_draw[&5];
                assert_eq!(primitives.len(), count);
                assert!(
                    primitives
                        .iter()
                        .all(|p| matches!(p, crate::debug_draw::Primitive::Point {
                    instance:id, color:0xff123456, .. } if *id == instance))
                );
            } else {
                assert!(state.debug_draw.is_empty());
            }
        }
        assert_eq!(recording.state_at(3).unwrap().objects[&5].generation, 1);
    }

    #[test]
    fn renderer_metadata_budget_truncation_cancellation_and_edits_fail() {
        let (mut metadata, mut builder) = renderer_fixture();
        let data = renderer_point(Some(42));
        for end in 1..data.len() {
            if end == 9 {
                continue;
            }
            assert!(metadata.set(&mut builder, 5, 0, &data[..end]).is_err());
        }
        assert!(builder.state.debug_draw.is_empty());
        builder.memory_limit = builder.retained_bytes + 128;
        assert!(
            metadata
                .set(&mut builder, 5, 0, &data)
                .unwrap_err()
                .to_string()
                .contains("memory budget")
        );
        builder.memory_limit = 512 << 20;
        assert!(
            metadata
                .set_with_progress(&mut builder, 5, 0, &data, &mut || false)
                .is_err()
        );
        metadata.properties[0].as_mut().unwrap().kind = 1;
        assert!(
            metadata
                .set(&mut builder, 5, 0, &data)
                .unwrap_err()
                .to_string()
                .contains("layout")
        );
        metadata.properties[0].as_mut().unwrap().kind = 2;
        for kind in 5..=7u32 {
            let event: Vec<_> = [kind, 5, 0]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect();
            assert!(
                import_event(&metadata, &BTreeMap::new(), &mut builder, &event, 0, 1000)
                    .unwrap_err()
                    .to_string()
                    .contains("renderer array editing")
            );
        }
        assert!(
            metadata
                .set(&mut builder, 5, 0, &[255])
                .unwrap_err()
                .to_string()
                .contains("no event boundary")
        );
    }

    pub(super) fn error_fixture() -> (Metadata, RecordingBuilder) {
        let error_type = Name {
            namespace: 1,
            name: 2,
        };
        let container = Name {
            namespace: 1,
            name: 3,
        };
        let uint = Name {
            namespace: 4,
            name: 5,
        };
        let string = Name {
            namespace: 4,
            name: 6,
        };
        let strings = [
            "",
            "_debugger_",
            "PvdErrorMessage",
            "UserCustomedEvent",
            "physx3",
            "PvdU32",
            "StringHandle",
            "Events",
            "Code",
            "Message",
            "File",
            "Line",
            "Index",
            "untimed error",
            "source.cpp",
            "frame",
        ]
        .into_iter()
        .enumerate()
        .map(|(i, s)| (i as u32, s.to_owned()))
        .collect();
        let mut properties: Vec<_> = (0..5)
            .map(|i| {
                Some(Property {
                    owner: error_type,
                    name: i + 8,
                    datatype: if i == 1 || i == 2 { string } else { uint },
                    datatype_id: u32::from(i == 1 || i == 2),
                    kind: 1,
                    semantic: 0,
                    offset32: i as usize * 4,
                    values: Vec::new(),
                })
            })
            .collect();
        properties.push(Some(Property {
            owner: container,
            name: 7,
            datatype: error_type,
            datatype_id: 2,
            kind: 2,
            semantic: 0,
            offset32: 0,
            values: Vec::new(),
        }));
        let metadata = Metadata {
            strings,
            properties,
            messages: Vec::new(),
            classes: vec![
                Some(Class {
                    name: uint,
                    base: u32::MAX,
                    packing: crate::native::ClassPacking::default(),
                    size: 4,
                    properties: vec![],
                    dynamic: false,
                    has_pointers: false,
                    string_offsets: Vec::new(),
                }),
                Some(Class {
                    name: string,
                    base: u32::MAX,
                    packing: crate::native::ClassPacking::default(),
                    size: 4,
                    properties: vec![],
                    dynamic: false,
                    has_pointers: true,
                    string_offsets: Vec::new(),
                }),
                Some(Class {
                    name: error_type,
                    base: u32::MAX,
                    packing: crate::native::ClassPacking::default(),
                    size: 20,
                    properties: (0..5).collect(),
                    dynamic: false,
                    has_pointers: true,
                    string_offsets: vec![4, 8],
                }),
                Some(Class {
                    name: container,
                    base: u32::MAX,
                    packing: crate::native::ClassPacking::default(),
                    size: 0,
                    properties: vec![5],
                    dynamic: true,
                    has_pointers: true,
                    string_offsets: Vec::new(),
                }),
            ],
        };
        let mut builder = catalog_builder();
        metadata.register(&mut builder).unwrap();
        push(
            &mut builder,
            Event::CreateInstance {
                class: container,
                id: 29,
            },
        )
        .unwrap();
        (metadata, builder)
    }

    pub(super) fn error_payload(index: u32) -> Vec<u8> {
        [4u32, 13, 14, 248, index]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect()
    }

    fn error_insert(element: u32, index: u32) -> Vec<u8> {
        let mut bytes: Vec<_> = [5u32, 29, 5, element, 20]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        bytes.extend(error_payload(index));
        bytes
    }

    #[test]
    fn snapshot_errors_keep_array_order_without_forged_history() {
        let (mut metadata, _) = error_fixture();
        metadata.classes[3].as_mut().unwrap().size = 12;
        let mut builder = catalog_builder();
        metadata.register(&mut builder).unwrap();
        let mut body = 12u32.to_le_bytes().to_vec();
        body.extend([0xa5; 12]);
        let payload = error_payload(0);
        body.extend((payload.len() as u32).to_le_bytes());
        body.extend(payload);
        let mut record = 3u32.to_le_bytes().to_vec();
        record.extend(29u32.to_le_bytes());
        record.extend((body.len() as u32).to_le_bytes());
        record.extend(body);
        let directory = [1u32.to_le_bytes(), 29u32.to_le_bytes(), [0; 4], [0; 4]].concat();
        snapshot::restore(
            &metadata,
            &mut builder,
            &record,
            &directory,
            &mut 0,
            &mut || true,
        )
        .unwrap();
        assert!(builder.recording.untimed_error_events().is_empty());
        let Value::Array(values) = &builder.state.objects[&29].properties["Events"] else {
            panic!("snapshot error array missing")
        };
        assert_eq!(values.len(), 1);
        import_event(
            &metadata,
            &BTreeMap::new(),
            &mut builder,
            &error_insert(1, 1),
            12,
            record.len(),
        )
        .unwrap();
        let Value::Array(values) = &builder.state.objects[&29].properties["Events"] else {
            panic!("appended error array missing")
        };
        assert_eq!(values.len(), 2);
        let errors = builder.recording.untimed_error_events();
        assert_eq!(errors.len(), 1);
        assert_eq!((errors[0].index, errors[0].history_event), (1, 12));
    }

    #[test]
    fn untimed_errors_keep_source_index_receive_order_and_unframed_tail() {
        let (metadata, mut builder) = error_fixture();
        import_event(
            &metadata,
            &BTreeMap::new(),
            &mut builder,
            &error_insert(0, 42),
            100,
            1000,
        )
        .unwrap();
        push(
            &mut builder,
            Event::EndSection {
                id: 1,
                name: 15,
                timestamp: 9000,
            },
        )
        .unwrap();
        import_event(
            &metadata,
            &BTreeMap::new(),
            &mut builder,
            &error_insert(1, 7),
            120,
            1000,
        )
        .unwrap();
        let recording = builder.finish().unwrap();
        let errors = recording.untimed_error_events();
        assert_eq!(errors.len(), 2);
        assert_eq!(
            (
                errors[0].index,
                errors[0].history_event,
                errors[0].received_frame
            ),
            (42, 100, 0)
        );
        assert_eq!(
            (
                errors[1].index,
                errors[1].history_event,
                errors[1].received_frame
            ),
            (7, 120, 1)
        );
        assert_eq!(errors[1].received_frame, recording.frame_count());
        assert!(
            errors
                .iter()
                .all(|e| e.container_instance == 29 && e.container_generation == 0)
        );
        assert!(recording.error_events().next().is_none());
        assert!(recording.state_at(0).unwrap().errors.is_empty());
        let state = recording.state_at(0).unwrap();
        let Value::Array(before_tail) = &state.objects[&29].properties["Events"] else {
            panic!("error array missing")
        };
        assert_eq!(before_tail.len(), 1);
        recording.verify_replay().unwrap();
    }

    #[test]
    fn untimed_error_payload_rejects_truncation_handles_and_budget_before_copy() {
        let (mut metadata, mut builder) = error_fixture();
        for length in 0..20 {
            assert!(
                metadata
                    .append_error(&mut builder, 29, &error_payload(0)[..length], 1)
                    .is_err()
            );
        }
        let mut unknown = error_payload(0);
        unknown[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(
            metadata
                .append_error(&mut builder, 29, &unknown, 1)
                .unwrap_err()
                .to_string()
                .contains("undefined PVD3 error string")
        );
        metadata.strings.insert(13, "x".repeat(4096));
        builder.memory_limit = builder.retained_bytes + 1024;
        assert!(
            metadata
                .append_error(&mut builder, 29, &error_payload(0), 1)
                .unwrap_err()
                .to_string()
                .contains("history memory budget")
        );
        assert!(builder.recording.untimed_errors.is_empty());
    }

    #[test]
    fn untimed_error_layout_and_edit_operations_are_explicitly_bounded() {
        let (mut metadata, mut builder) = error_fixture();
        metadata.properties[1].as_mut().unwrap().offset32 = 8;
        assert!(
            import_event(
                &metadata,
                &BTreeMap::new(),
                &mut builder,
                &error_insert(0, 0),
                1,
                1000
            )
            .unwrap_err()
            .to_string()
            .contains("field layout")
        );
        metadata.properties[1].as_mut().unwrap().offset32 = 4;
        metadata.classes[2].as_mut().unwrap().size = 24;
        assert!(
            import_event(
                &metadata,
                &BTreeMap::new(),
                &mut builder,
                &error_insert(0, 0),
                1,
                1000
            )
            .unwrap_err()
            .to_string()
            .contains("record layout")
        );
        metadata.classes[2].as_mut().unwrap().size = 20;
        import_event(
            &metadata,
            &BTreeMap::new(),
            &mut builder,
            &error_insert(0, 0),
            1,
            1000,
        )
        .unwrap();
        assert!(
            import_event(
                &metadata,
                &BTreeMap::new(),
                &mut builder,
                &error_insert(0, 1),
                2,
                1000
            )
            .unwrap_err()
            .to_string()
            .contains("append in history order")
        );
        for kind in [6u32, 7] {
            let mut edit = error_insert(0, 0);
            edit[..4].copy_from_slice(&kind.to_le_bytes());
            assert!(
                import_event(&metadata, &BTreeMap::new(), &mut builder, &edit, 2, 1000)
                    .unwrap_err()
                    .to_string()
                    .contains("error array editing")
            );
        }
        assert!(
            metadata
                .set(&mut builder, 29, 5, &[])
                .unwrap_err()
                .to_string()
                .contains("error array replacement")
        );
        assert_eq!(builder.recording.untimed_errors.len(), 1);
    }

    fn catalog_bytes(events: &[(u16, &str, u8)]) -> Vec<u8> {
        let mut bytes = vec![0; 12];
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(100u64.to_le_bytes());
        bytes.extend([0; 8]);
        bytes.extend(5u32.to_le_bytes());
        bytes.extend(b"Zone\0");
        bytes.extend(7u32.to_le_bytes());
        bytes.extend((events.len() as u32).to_le_bytes());
        for &(id, name, flag) in events {
            bytes.extend((name.len() as u32 + 1).to_le_bytes());
            bytes.extend(name.as_bytes());
            bytes.push(0);
            bytes.extend(id.to_le_bytes());
            bytes.push(flag);
        }
        bytes.extend([0; 12]);
        bytes
    }

    pub(super) fn catalog_builder() -> RecordingBuilder {
        RecordingBuilder::new(Header {
            version: 1,
            timestamp_numerator: 1,
            timestamp_denominator: 1,
            flags: 0,
            endian: Endian::Little,
        })
    }

    #[test]
    fn saved_profile_conflicts_are_ambiguous_and_cannot_silently_relabel() {
        let bytes = catalog_bytes(&[(4, "First", 1), (4, "Changed", 1)]);
        let sources = BTreeMap::from([(7, 100)]);
        let catalog = profile_catalog(&mut Reader::new(&bytes), &sources, &mut 0, || true).unwrap();
        let mut builder = catalog_builder();
        register_profile_catalog(&mut builder, catalog).unwrap();
        assert!(builder.recording.analysis.profile_metadata_ambiguous(7, 4));
        assert!(
            builder
                .recording
                .diagnostics
                .iter()
                .any(|d| d.contains("conflicting event definitions"))
        );

        let bytes = catalog_bytes(&[(4, "Same", 0), (4, "Same", 1)]);
        let catalog = profile_catalog(&mut Reader::new(&bytes), &sources, &mut 0, || true).unwrap();
        assert!(
            register_profile_catalog(&mut catalog_builder(), catalog)
                .unwrap_err()
                .to_string()
                .contains("conflicting event enabled")
        );
    }

    #[test]
    fn saved_profile_catalog_enforces_bounds_mapping_flags_and_cancellation() {
        let bytes = catalog_bytes(&[(4, "Event", 1)]);
        let sources = BTreeMap::from([(7, 100)]);
        for len in 0..bytes.len() {
            assert!(
                profile_catalog(&mut Reader::new(&bytes[..len]), &sources, &mut 0, || true)
                    .is_err()
            );
        }
        let invalid = catalog_bytes(&[(4, "Event", 2)]);
        assert!(
            profile_catalog(&mut Reader::new(&invalid), &sources, &mut 0, || true)
                .unwrap_err()
                .to_string()
                .contains("enabled flag")
        );
        assert!(
            profile_catalog(&mut Reader::new(&bytes), &BTreeMap::new(), &mut 0, || true)
                .unwrap_err()
                .to_string()
                .contains("instance mapping")
        );
        let mut exhausted_budget = MAX_METADATA;
        assert!(
            profile_catalog(
                &mut Reader::new(&bytes),
                &sources,
                &mut exhausted_budget,
                || true
            )
            .unwrap_err()
            .to_string()
            .contains("memory budget")
        );
        let mut calls = 0;
        assert!(
            profile_catalog(&mut Reader::new(&bytes), &sources, &mut 0, || {
                calls += 1;
                calls < 2
            })
            .unwrap_err()
            .to_string()
            .contains("cancelled")
        );
        assert_eq!(calls, 2);
    }

    #[test]
    fn string_handle_expansion_checks_budget_before_cloning() {
        string_handle_budget(1);
    }

    #[test]
    fn bare_string_handle_uses_the_same_expansion_budget() {
        string_handle_budget(0);
    }

    fn string_handle_budget(namespace: u32) {
        let name = Name { namespace, name: 2 };
        let strings = BTreeMap::from([
            (0, String::new()),
            (1, "physx3".into()),
            (2, "StringHandle".into()),
            (3, "a".repeat(512)),
        ]);
        let metadata = Metadata {
            strings: strings.clone(),
            classes: vec![Some(Class {
                name,
                base: u32::MAX,
                packing: crate::native::ClassPacking::default(),
                size: 4,
                properties: vec![],
                dynamic: false,
                has_pointers: true,
                string_offsets: vec![],
            })],
            properties: vec![],
            messages: vec![],
        };
        let mut builder = RecordingBuilder::with_memory_limit(
            Header {
                version: 1,
                timestamp_numerator: 1,
                timestamp_denominator: 1,
                flags: 0,
                endian: Endian::Little,
            },
            2048,
        );
        builder.strings = strings;
        let property = Property {
            owner: name,
            name: 2,
            datatype: name,
            datatype_id: 0,
            kind: 2,
            semantic: 0,
            offset32: 0,
            values: vec![],
        };
        let repeated = [3u32.to_le_bytes(); 8].concat();
        let error = metadata
            .value(&builder, &property, &repeated, true)
            .unwrap_err();
        assert!(error.to_string().contains("memory budget"));
        let one = metadata
            .value(&builder, &property, &repeated[..4], true)
            .unwrap();
        assert!(
            matches!(one, Value::Array(values) if values == Arc::new(vec![Value::String("a".repeat(512))]))
        );
        builder.retained_bytes = 2000;
        assert!(
            metadata
                .value(&builder, &property, &repeated[..4], true)
                .is_err()
        );
    }

    #[test]
    fn metadata_budget_rejects_count_amplification_and_overflow() {
        let mut bytes = 0;
        assert!(metadata_budget(&mut bytes, MAX_ITEMS, 96).is_err());
        let mut bytes = usize::MAX;
        assert!(metadata_budget(&mut bytes, 1, 1).is_err());
    }
}
