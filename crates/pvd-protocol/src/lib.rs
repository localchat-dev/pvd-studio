//! PhysX SDK communication stream v1 decoder, independent of the desktop runtime.
pub mod rewrite;
use std::{fmt, sync::Arc};

pub const STREAM_MAGIC: u32 = 837_150_850;
const PVD3_CLIENT_DATABASE_MAGIC: u32 = 0x3610_1BB0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endian {
    Little,
    Big,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub version: u32,
    pub timestamp_numerator: u64,
    pub timestamp_denominator: u64,
    pub flags: u32,
    pub endian: Endian,
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_group_bytes: usize,
    pub max_blob_bytes: usize,
    pub max_array_items: usize,
    pub max_events_per_group: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_group_bytes: 64 << 20,
            max_blob_bytes: 32 << 20,
            max_array_items: 1_000_000,
            max_events_per_group: 1_000_000,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub offset: u64,
    pub message: String,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "stream byte {}: {}", self.offset, self.message)
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Name {
    pub namespace: u32,
    pub name: u32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageArg {
    pub property: u32,
    pub datatype: Name,
    pub offset: u32,
    pub size: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    StringHandle {
        handle: u32,
        value: String,
    },
    CreateClass {
        name: Name,
    },
    DeriveClass {
        parent: Name,
        child: Name,
    },
    CreateProperty {
        class: Name,
        name: u32,
        semantic: u32,
        datatype: Name,
        kind: u8,
        named_values: Vec<(u32, u32)>,
    },
    CreatePropertyMessage {
        class: Name,
        name: Name,
        args: Vec<MessageArg>,
        size: u32,
    },
    CreateInstance {
        class: Name,
        id: u64,
    },
    SetPropertyValue {
        id: u64,
        property: u32,
        datatype: Name,
        items: u32,
        data: Arc<[u8]>,
    },
    BeginSetPropertyValue {
        id: u64,
        property: u32,
        datatype: Name,
    },
    AppendPropertyValueData {
        data: Arc<[u8]>,
        items: u32,
    },
    EndSetPropertyValue,
    SetPropertyMessage {
        id: u64,
        message: Name,
        data: Arc<[u8]>,
    },
    BeginPropertyMessageGroup {
        message: Name,
    },
    SendPropertyMessageFromGroup {
        id: u64,
        data: Arc<[u8]>,
    },
    EndPropertyMessageGroup,
    DestroyInstance {
        id: u64,
    },
    PushBackObjectRef {
        id: u64,
        property: u32,
        target: u64,
    },
    RemoveObjectRef {
        id: u64,
        property: u32,
        target: u64,
    },
    BeginSection {
        id: u64,
        name: u32,
        timestamp: u64,
    },
    EndSection {
        id: u64,
        name: u32,
        timestamp: u64,
    },
    SetPickable {
        id: u64,
        value: bool,
    },
    SetColor {
        id: u64,
        rgba: [u8; 4],
    },
    SetIsTopLevel {
        id: u64,
        value: bool,
    },
    SetCamera {
        name: String,
        position: [f32; 3],
        up: [f32; 3],
        target: [f32; 3],
    },
    AddProfileZone {
        id: u64,
        name: String,
    },
    AddProfileZoneEvent {
        id: u64,
        name: String,
        event_id: u16,
        enabled: bool,
    },
    StreamEnd {
        name: String,
    },
    ErrorMessage {
        code: u32,
        message: String,
        file: String,
        line: u32,
    },
    OriginShift {
        id: u64,
        shift: [f32; 3],
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct EventGroup {
    pub stream_id: u64,
    pub timestamp: u64,
    pub offset: u64,
    pub events: Vec<Event>,
}

pub struct Decoder {
    limits: Limits,
    header: Option<Header>,
    buffer: Vec<u8>,
    offset: u64,
    failed: bool,
}
impl Default for Decoder {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}
impl Decoder {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            header: None,
            buffer: Vec::new(),
            offset: 0,
            failed: false,
        }
    }
    pub fn header(&self) -> Option<&Header> {
        self.header.as_ref()
    }
    pub fn buffered_bytes(&self) -> usize {
        self.buffer.len()
    }
    pub fn feed(&mut self, mut bytes: &[u8]) -> Result<Vec<EventGroup>, Error> {
        if self.failed {
            return Err(self.error("decoder is failed; create a new decoder for another stream"));
        }
        let result = self.feed_inner(&mut bytes);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn feed_inner(&mut self, bytes: &mut &[u8]) -> Result<Vec<EventGroup>, Error> {
        let mut groups = Vec::new();
        loop {
            let needed = match self.header.as_ref() {
                None => 28,
                Some(_) if self.buffer.len() < 24 => 24,
                Some(header) => {
                    let mut r = Reader::new(&self.buffer, self.offset, header.endian, self.limits);
                    let size = r.u32()? as usize;
                    if size > self.limits.max_group_bytes {
                        return Err(self.error("event group exceeds configured byte limit"));
                    }
                    24 + size
                }
            };
            let take = needed.saturating_sub(self.buffer.len()).min(bytes.len());
            self.buffer.extend_from_slice(&bytes[..take]);
            *bytes = &bytes[take..];
            if self.header.is_none()
                && self.buffer.len() >= 4
                && u32::from_le_bytes(self.buffer[..4].try_into().unwrap())
                    == PVD3_CLIENT_DATABASE_MAGIC
            {
                return Err(self.error("PhysX Visual Debugger 3 Client database signature detected (0x36101BB0); native metadata and history pages are not supported. This reader accepts SDK communication streams, not PVD3 Save containers."));
            }
            if self.buffer.len() < needed {
                break;
            }
            match self.header.as_ref() {
                None => {
                    let magic: [u8; 4] = self.buffer[..4].try_into().unwrap();
                    let endian = if u32::from_le_bytes(magic) == STREAM_MAGIC {
                        Endian::Little
                    } else if u32::from_be_bytes(magic) == STREAM_MAGIC {
                        Endian::Big
                    } else {
                        return Err(self.error("unrecognized file/stream signature; expected PhysX communication stream v1 (PVD3 database containers and OmniPVD are separate formats)"));
                    };
                    let mut r = Reader::new(&self.buffer, self.offset, endian, self.limits);
                    r.u32()?;
                    let version = r.u32()?;
                    if version != 1 {
                        return Err(self.error(&format!("unsupported stream version {version}")));
                    }
                    let timestamp_numerator = r.u64()?;
                    let timestamp_denominator = r.u64()?;
                    if timestamp_denominator == 0 {
                        return Err(self.error("zero timestamp denominator"));
                    }
                    let flags = r.u32()?;
                    if flags & !1 != 0 {
                        return Err(self.error("unsupported stream flags"));
                    }
                    self.header = Some(Header {
                        version,
                        timestamp_numerator,
                        timestamp_denominator,
                        flags,
                        endian,
                    });
                    self.offset += 28;
                    self.buffer.clear();
                }
                Some(header) => {
                    if self.buffer.len() == 24 {
                        let size = {
                            let mut r =
                                Reader::new(&self.buffer, self.offset, header.endian, self.limits);
                            r.u32()? as usize
                        };
                        if size > self.limits.max_group_bytes {
                            return Err(self.error("event group exceeds configured byte limit"));
                        }
                        if size > 0 {
                            if bytes.is_empty() {
                                break;
                            }
                            continue;
                        }
                    }
                    let mut r = Reader::new(&self.buffer, self.offset, header.endian, self.limits);
                    let size = r.u32()? as usize;
                    if self.buffer.len() < 24 + size {
                        continue;
                    }
                    let count = r.u32()? as usize;
                    if count > self.limits.max_events_per_group || count > size {
                        return Err(self.error("invalid event count"));
                    }
                    let stream_id = r.u64()?;
                    let timestamp = r.u64()?;
                    let mut events = Vec::with_capacity(count.min(4096));
                    for _ in 0..count {
                        events.push(r.event()?);
                    }
                    if r.pos != self.buffer.len() {
                        return Err(
                            r.error("event group byte size does not match its decoded events")
                        );
                    }
                    groups.push(EventGroup {
                        stream_id,
                        timestamp,
                        offset: self.offset,
                        events,
                    });
                    self.offset += self.buffer.len() as u64;
                    self.buffer.clear();
                }
            }
            if bytes.is_empty() {
                break;
            }
        }
        Ok(groups)
    }
    pub fn finish(&self) -> Result<(), Error> {
        if self.failed {
            Err(self.error("decoder failed before end of stream"))
        } else if self.header.is_none() {
            Err(self.error("truncated stream initialization"))
        } else if !self.buffer.is_empty() {
            Err(self.error("truncated event group"))
        } else {
            Ok(())
        }
    }
    fn error(&self, message: &str) -> Error {
        Error {
            offset: self.offset,
            message: message.to_owned(),
        }
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    offset: u64,
    endian: Endian,
    limits: Limits,
}
impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8], offset: u64, endian: Endian, limits: Limits) -> Self {
        Self {
            bytes,
            pos: 0,
            offset,
            endian,
            limits,
        }
    }
    fn error(&self, message: &str) -> Error {
        Error {
            offset: self.offset + self.pos as u64,
            message: message.to_owned(),
        }
    }
    fn take(&mut self, count: usize) -> Result<&'a [u8], Error> {
        let end = self
            .pos
            .checked_add(count)
            .ok_or_else(|| self.error("length overflow"))?;
        let data = self
            .bytes
            .get(self.pos..end)
            .ok_or_else(|| self.error("event payload exceeds its group boundary"))?;
        self.pos = end;
        Ok(data)
    }
    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, Error> {
        let b = self.take(2)?.try_into().unwrap();
        Ok(match self.endian {
            Endian::Little => u16::from_le_bytes(b),
            Endian::Big => u16::from_be_bytes(b),
        })
    }
    fn u32(&mut self) -> Result<u32, Error> {
        let b = self.take(4)?.try_into().unwrap();
        Ok(match self.endian {
            Endian::Little => u32::from_le_bytes(b),
            Endian::Big => u32::from_be_bytes(b),
        })
    }
    fn u64(&mut self) -> Result<u64, Error> {
        let b = self.take(8)?.try_into().unwrap();
        Ok(match self.endian {
            Endian::Little => u64::from_le_bytes(b),
            Endian::Big => u64::from_be_bytes(b),
        })
    }
    fn f32(&mut self) -> Result<f32, Error> {
        Ok(f32::from_bits(self.u32()?))
    }
    fn vec3(&mut self) -> Result<[f32; 3], Error> {
        Ok([self.f32()?, self.f32()?, self.f32()?])
    }
    fn name(&mut self) -> Result<Name, Error> {
        Ok(Name {
            namespace: self.u32()?,
            name: self.u32()?,
        })
    }
    fn boolean(&mut self) -> Result<bool, Error> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(self.error("invalid boolean")),
        }
    }
    fn count(&mut self, stride: usize) -> Result<usize, Error> {
        let n = self.u32()? as usize;
        if n > self.limits.max_array_items || n > (self.bytes.len() - self.pos) / stride {
            return Err(self.error("array length exceeds boundary or configured limit"));
        }
        Ok(n)
    }
    fn blob(&mut self) -> Result<Arc<[u8]>, Error> {
        let n = self.u32()? as usize;
        if n > self.limits.max_blob_bytes {
            return Err(self.error("payload exceeds configured byte limit"));
        }
        Ok(Arc::from(self.take(n)?))
    }
    fn string(&mut self) -> Result<String, Error> {
        let data = self.blob()?;
        if data.is_empty() {
            return Ok(String::new());
        }
        if data.last() != Some(&0) {
            return Err(self.error("string is not NUL terminated"));
        }
        Ok(String::from_utf8_lossy(&data[..data.len() - 1]).into_owned())
    }
    fn event(&mut self) -> Result<Event, Error> {
        Ok(match self.u8()? {
            1 => {
                let value = self.string()?;
                Event::StringHandle {
                    handle: self.u32()?,
                    value,
                }
            }
            2 => Event::CreateClass { name: self.name()? },
            3 => Event::DeriveClass {
                parent: self.name()?,
                child: self.name()?,
            },
            4 => {
                let class = self.name()?;
                let name = self.u32()?;
                let semantic = self.u32()?;
                let datatype = self.name()?;
                let kind = self.u8()?;
                let n = self.count(8)?;
                let mut named_values = Vec::with_capacity(n);
                for _ in 0..n {
                    named_values.push((self.u32()?, self.u32()?));
                }
                Event::CreateProperty {
                    class,
                    name,
                    semantic,
                    datatype,
                    kind,
                    named_values,
                }
            }
            5 => {
                let class = self.name()?;
                let name = self.name()?;
                let n = self.count(20)?;
                let mut args = Vec::with_capacity(n);
                for _ in 0..n {
                    args.push(MessageArg {
                        property: self.u32()?,
                        datatype: self.name()?,
                        offset: self.u32()?,
                        size: self.u32()?,
                    });
                }
                Event::CreatePropertyMessage {
                    class,
                    name,
                    args,
                    size: self.u32()?,
                }
            }
            6 => Event::CreateInstance {
                class: self.name()?,
                id: self.u64()?,
            },
            7 => Event::SetPropertyValue {
                id: self.u64()?,
                property: self.u32()?,
                datatype: self.name()?,
                items: self.u32()?,
                data: self.blob()?,
            },
            8 => Event::BeginSetPropertyValue {
                id: self.u64()?,
                property: self.u32()?,
                datatype: self.name()?,
            },
            9 => Event::AppendPropertyValueData {
                data: self.blob()?,
                items: self.u32()?,
            },
            10 => Event::EndSetPropertyValue,
            11 => Event::SetPropertyMessage {
                id: self.u64()?,
                message: self.name()?,
                data: self.blob()?,
            },
            12 => Event::BeginPropertyMessageGroup {
                message: self.name()?,
            },
            13 => Event::SendPropertyMessageFromGroup {
                id: self.u64()?,
                data: self.blob()?,
            },
            14 => Event::EndPropertyMessageGroup,
            15 => Event::DestroyInstance { id: self.u64()? },
            16 => Event::PushBackObjectRef {
                id: self.u64()?,
                property: self.u32()?,
                target: self.u64()?,
            },
            17 => Event::RemoveObjectRef {
                id: self.u64()?,
                property: self.u32()?,
                target: self.u64()?,
            },
            18 => Event::BeginSection {
                id: self.u64()?,
                name: self.u32()?,
                timestamp: self.u64()?,
            },
            19 => Event::EndSection {
                id: self.u64()?,
                name: self.u32()?,
                timestamp: self.u64()?,
            },
            20 => Event::SetPickable {
                id: self.u64()?,
                value: self.boolean()?,
            },
            21 => Event::SetColor {
                id: self.u64()?,
                rgba: self.take(4)?.try_into().unwrap(),
            },
            22 => Event::SetIsTopLevel {
                id: self.u64()?,
                value: self.boolean()?,
            },
            23 => Event::SetCamera {
                name: self.string()?,
                position: self.vec3()?,
                up: self.vec3()?,
                target: self.vec3()?,
            },
            24 => Event::AddProfileZone {
                id: self.u64()?,
                name: self.string()?,
            },
            25 => Event::AddProfileZoneEvent {
                id: self.u64()?,
                name: self.string()?,
                event_id: self.u16()?,
                enabled: self.boolean()?,
            },
            26 => Event::StreamEnd {
                name: self.string()?,
            },
            27 => Event::ErrorMessage {
                code: self.u32()?,
                message: self.string()?,
                file: self.string()?,
                line: self.u32()?,
            },
            28 => Event::OriginShift {
                id: self.u64()?,
                shift: self.vec3()?,
            },
            kind => {
                return Err(self.error(&format!(
                    "unknown event type {kind}; no reliable per-event boundary exists"
                )));
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn header() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(STREAM_MAGIC.to_le_bytes());
        b.extend(1u32.to_le_bytes());
        b.extend(1u64.to_le_bytes());
        b.extend(10u64.to_le_bytes());
        b.extend(1u32.to_le_bytes());
        b
    }
    fn group(payload: &[u8]) -> Vec<u8> {
        let mut b = header();
        b.extend((payload.len() as u32).to_le_bytes());
        b.extend(1u32.to_le_bytes());
        b.extend(3u64.to_le_bytes());
        b.extend(4u64.to_le_bytes());
        b.extend(payload);
        b
    }
    #[test]
    fn arbitrary_tcp_fragmentation() {
        let b = group(&[15, 4, 0, 0, 0, 0, 0, 0, 0]);
        for chunk in 1..b.len() {
            let mut d = Decoder::default();
            let mut events = Vec::new();
            for part in b.chunks(chunk) {
                events.extend(d.feed(part).unwrap());
            }
            d.finish().unwrap();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].events, vec![Event::DestroyInstance { id: 4 }]);
        }
    }
    #[test]
    fn every_truncated_payload_fails_at_finish() {
        let b = group(&[15, 4, 0, 0, 0, 0, 0, 0, 0]);
        for n in 0..b.len() {
            if n == 28 {
                continue;
            }
            let mut d = Decoder::default();
            let fed = d.feed(&b[..n]);
            assert!(fed.is_err() || d.finish().is_err(), "{n}");
        }
    }
    #[test]
    fn unknown_event_is_not_skipped() {
        assert!(
            Decoder::default()
                .feed(&group(&[255]))
                .unwrap_err()
                .message
                .contains("unknown event")
        );
    }
    #[test]
    fn native_database_is_reported_without_accepting_its_metadata_as_sdk_events() {
        let native_prefix = [
            PVD3_CLIENT_DATABASE_MAGIC.to_le_bytes(),
            1u32.to_le_bytes(),
            u32::MAX.to_le_bytes(),
            u32::MAX.to_le_bytes(),
        ]
        .concat();
        for chunk_size in 1..=native_prefix.len() {
            let mut decoder = Decoder::default();
            let rejected = native_prefix
                .chunks(chunk_size)
                .find_map(|chunk| decoder.feed(chunk).err())
                .expect("native database must fail before its length fields are used");
            assert!(rejected.message.contains("PVD3 Save containers"));
            assert!(decoder.header().is_none());
            assert!(
                decoder
                    .feed(&header())
                    .unwrap_err()
                    .message
                    .contains("failed")
            );
        }
        let mut swapped = vec![0; 28];
        swapped[..4].copy_from_slice(&PVD3_CLIENT_DATABASE_MAGIC.to_be_bytes());
        assert!(
            Decoder::default()
                .feed(&swapped)
                .unwrap_err()
                .message
                .contains("unrecognized file/stream signature")
        );
    }
    #[test]
    fn hostile_lengths_rejected_before_allocation() {
        let mut b = header();
        b.extend(u32::MAX.to_le_bytes());
        b.resize(52, 0);
        assert!(
            Decoder::default()
                .feed(&b)
                .unwrap_err()
                .message
                .contains("limit")
        );
        let b = group(&[1, 255, 255, 255, 255]);
        assert!(Decoder::default().feed(&b).is_err());
    }
    #[test]
    fn event_cannot_consume_next_group() {
        let mut b = group(&[15]);
        b.extend(&group(&[15, 4, 0, 0, 0, 0, 0, 0, 0])[28..]);
        assert!(Decoder::default().feed(&b).is_err());
    }
}
