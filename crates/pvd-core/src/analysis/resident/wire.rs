use super::*;

mod identity_codec;
mod text_codec;
pub(super) use identity_codec::IdentityDictionary;
pub(super) use text_codec::{TEXT_READ_SCRATCH, TextDictionary, read_text_ranges};

pub(super) const MAP_MAGIC: &[u8; 8] = b"PVDAMP01";
pub(super) const IDENTIFIED_VALUES_MAGIC: &[u8; 8] = b"PVDAVP02";
pub(super) const IDENTIFIED_STRINGS_MAGIC: &[u8; 8] = b"PVDASP02";
pub(super) const IDENTIFIED_LARGE_STRING_MAGIC: &[u8; 8] = b"PVDALP02";
pub(super) const COMPACT_VALUES_MAGIC: &[u8; 8] = b"PVDAVP03";
pub(super) const COMPACT_STRINGS_MAGIC: &[u8; 8] = b"PVDASP03";
pub(super) const COMPACT_LARGE_STRING_MAGIC: &[u8; 8] = b"PVDALP03";
pub(super) const TABLE_VALUES_MAGIC: &[u8; 8] = b"PVDAVP04";
pub(super) const DEDUP_STRINGS_MAGIC: &[u8; 8] = b"PVDASP05";
pub(super) const TABLE_STRINGS_MAGIC: &[u8; 8] = b"PVDASP04";
pub(super) const TABLE_LARGE_STRING_MAGIC: &[u8; 8] = b"PVDALP04";
pub(super) const VALUES_MAGIC: &[u8; 8] = b"PVDAVP01";
pub(super) const STRINGS_MAGIC: &[u8; 8] = b"PVDASP01";
pub(super) const LARGE_STRING_MAGIC: &[u8; 8] = b"PVDALP01";

#[derive(Clone, Copy)]
pub(super) enum Child {
    Empty,
    Local(u16),
    External(MapRef),
}

#[derive(Clone, Copy)]
pub(super) struct NodeRow {
    pub key: Key,
    pub value: RowRef,
    pub left: Child,
    pub right: Child,
    pub height: u8,
    pub len: u64,
    pub charge: u64,
}

#[derive(Clone, Copy)]
pub(super) struct AllocationRow {
    pub address: u64,
    pub size: u64,
    pub type_name: RowRef,
    pub file: RowRef,
    pub line: u32,
    pub received_frame: u64,
}

pub(super) struct Encoder {
    pub bytes: Vec<u8>,
}
impl Encoder {
    pub fn new(magic: &[u8; 8]) -> Self {
        Self {
            bytes: magic.to_vec(),
        }
    }
    pub fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }
    pub fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }
    pub fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }
    pub fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }
    pub fn identity(&mut self, value: identity::WireIdentity) {
        self.bytes.extend_from_slice(&value.origin);
        self.u64(value.sequence);
    }
    pub fn key(&mut self, value: Key) {
        self.u64(value.0);
        self.u64(value.1);
    }
    pub fn row(&mut self, value: RowRef) {
        self.u64(value.page);
        self.u32(value.row);
    }
    pub fn map(&mut self, value: MapRef) {
        self.u64(value.page);
        self.u64(value.len);
        self.u8(value.height);
        self.key(value.minimum);
        self.key(value.maximum);
        self.u64(value.charge);
    }
    pub fn child(&mut self, value: Child) {
        match value {
            Child::Empty => self.u8(0),
            Child::Local(row) => {
                self.u8(1);
                self.u16(row);
            }
            Child::External(map) => {
                self.u8(2);
                self.map(map);
            }
        }
    }
}

pub(super) struct Decoder<'a> {
    pub bytes: &'a [u8],
    pub offset: usize,
}
impl<'a> Decoder<'a> {
    pub fn new(bytes: &'a [u8], magic: &[u8; 8]) -> Result<Self, Error> {
        if bytes.get(..8) != Some(magic) {
            return Err(error("allocation page has the wrong kind or version"));
        }
        Ok(Self { bytes, offset: 8 })
    }
    pub fn take(&mut self, count: usize) -> Result<&'a [u8], Error> {
        let end = add(self.offset, count)?;
        let result = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| error("truncated allocation page"))?;
        self.offset = end;
        Ok(result)
    }
    pub fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into()?))
    }
    pub fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }
    pub fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into()?))
    }
    pub fn identity(&mut self) -> Result<identity::WireIdentity, Error> {
        let origin = self.take(16)?.try_into()?;
        let result = identity::WireIdentity {
            origin,
            sequence: self.u64()?,
        };
        result.validate()?;
        Ok(result)
    }
    pub fn key(&mut self) -> Result<Key, Error> {
        Ok((self.u64()?, self.u64()?))
    }
    pub fn row(&mut self) -> Result<RowRef, Error> {
        let result = RowRef {
            page: self.u64()?,
            row: self.u32()?,
        };
        if result.page == 0 {
            return Err(error("allocation value references an empty page"));
        }
        Ok(result)
    }
    pub fn map(&mut self) -> Result<MapRef, Error> {
        let page = self.u64()?;
        self.map_body(page)
    }
    pub fn map_header(&mut self, page: u64) -> Result<MapRef, Error> {
        if self.u64()? != 0 {
            return Err(error("allocation page stores a nonzero self reference"));
        }
        self.map_body(page)
    }
    fn map_body(&mut self, page: u64) -> Result<MapRef, Error> {
        let result = MapRef {
            page,
            len: self.u64()?,
            height: self.u8()?,
            minimum: self.key()?,
            maximum: self.key()?,
            charge: self.u64()?,
        };
        if result.len == 0 {
            if result != MapRef::empty() {
                return Err(error("empty allocation map has nonempty metadata"));
            }
        } else if result.page == 0
            || result.height == 0
            || result.height > MAX_AVL_HEIGHT
            || result.minimum > result.maximum
            || result.charge < result.len.saturating_mul(TREE_ARC_BYTES as u64)
        {
            return Err(error("invalid allocation map metadata"));
        }
        Ok(result)
    }
    pub fn child(&mut self, row: usize) -> Result<Child, Error> {
        Ok(match self.u8()? {
            0 => Child::Empty,
            1 => {
                let index = self.u16()?;
                if usize::from(index) >= row {
                    return Err(error("allocation page local link is not postorder"));
                }
                Child::Local(index)
            }
            2 => {
                let map = self.map()?;
                if map.len == 0 {
                    return Err(error("external allocation subtree is empty"));
                }
                Child::External(map)
            }
            _ => return Err(error("unknown allocation child link")),
        })
    }
    pub fn count(&mut self, maximum: usize) -> Result<usize, Error> {
        let count = usize::from(self.u16()?);
        if count == 0 || count > maximum {
            return Err(error("allocation page row count exceeds its limit"));
        }
        Ok(count)
    }
    pub fn finish(self) -> Result<(), Error> {
        if self.offset != self.bytes.len() {
            return Err(error("allocation page has trailing bytes"));
        }
        Ok(())
    }
}

pub(super) fn encode_map(meta: MapRef, rows: &[NodeRow]) -> Result<Vec<u8>, Error> {
    let mut out = Encoder::new(MAP_MAGIC);
    out.map(meta);
    out.u16(rows.len().try_into()?);
    for row in rows {
        out.key(row.key);
        out.row(row.value);
        out.child(row.left);
        out.child(row.right);
        out.u8(row.height);
        out.u64(row.len);
        out.u64(row.charge);
    }
    if out.bytes.len() > MAX_PAGE_BYTES {
        return Err(error("allocation tree page exceeds its byte limit"));
    }
    Ok(out.bytes)
}

pub(super) fn encode_values(rows: &[AllocationRow]) -> Vec<u8> {
    let mut out = Encoder::new(VALUES_MAGIC);
    out.u16(rows.len() as u16);
    for row in rows {
        out.u64(row.address);
        out.u64(row.size);
        out.row(row.type_name);
        out.row(row.file);
        out.u32(row.line);
        out.u64(row.received_frame);
    }
    out.bytes
}

pub(super) fn decode_values(bytes: &[u8]) -> Result<Vec<AllocationRow>, Error> {
    let mut input = Decoder::new(bytes, VALUES_MAGIC)?;
    let count = input.count(MAX_VALUE_ROWS)?;
    if bytes.len() != 10 + count * 52 {
        return Err(error(
            "allocation value page length disagrees with its count",
        ));
    }
    let mut rows = Vec::new();
    rows.try_reserve_exact(count)?;
    for _ in 0..count {
        rows.push(AllocationRow {
            address: input.u64()?,
            size: input.u64()?,
            type_name: input.row()?,
            file: input.row()?,
            line: input.u32()?,
            received_frame: input.u64()?,
        });
    }
    input.finish()?;
    Ok(rows)
}

pub(super) fn encode_identified_values(
    rows: &[AllocationRow],
    identities: &[identity::WireIdentity],
) -> Result<Vec<u8>, Error> {
    if rows.len() != identities.len() || rows.is_empty() || rows.len() > MAX_VALUE_ROWS {
        return Err(error("identified allocation page count disagrees"));
    }
    let mut dictionary = IdentityDictionary::new();
    for identity in identities {
        dictionary.insert(*identity)?;
    }
    let compact = dictionary.is_smaller(rows.len())?;
    let mut out = Encoder::new(if compact {
        TABLE_VALUES_MAGIC
    } else {
        IDENTIFIED_VALUES_MAGIC
    });
    out.u16(rows.len().try_into()?);
    if compact {
        dictionary.write(&mut out);
        for identity in identities {
            dictionary.write_row(&mut out, *identity)?;
        }
    }
    for (row, identity) in rows.iter().zip(identities) {
        if !compact {
            out.identity(*identity);
        }
        out.u64(row.address);
        out.u64(row.size);
        out.row(row.type_name);
        out.row(row.file);
        out.u32(row.line);
        out.u64(row.received_frame);
    }
    Ok(out.bytes)
}
pub(super) fn decode_identified_values(
    bytes: &[u8],
) -> Result<(Vec<AllocationRow>, Option<Vec<identity::WireIdentity>>), Error> {
    let table = bytes.get(..8) == Some(TABLE_VALUES_MAGIC);
    let compact = table || bytes.get(..8) == Some(COMPACT_VALUES_MAGIC);
    if !compact && bytes.get(..8) != Some(IDENTIFIED_VALUES_MAGIC) {
        return Ok((decode_values(bytes)?, None));
    }
    let mut input = Decoder::new(
        bytes,
        if table {
            TABLE_VALUES_MAGIC
        } else if compact {
            COMPACT_VALUES_MAGIC
        } else {
            IDENTIFIED_VALUES_MAGIC
        },
    )?;
    let count = input.count(MAX_VALUE_ROWS)?;
    let dictionary = if compact {
        Some(IdentityDictionary::read(&mut input, count)?)
    } else {
        None
    };
    let expected_bytes = if let Some(dictionary) = &dictionary {
        add(10 + count * 52, dictionary.encoded_bytes(count)?)?
    } else {
        10 + count * 76
    };
    if bytes.len() != expected_bytes {
        return Err(error(
            "identified allocation page length disagrees with its count",
        ));
    }
    let mut rows = Vec::new();
    let mut identities = Vec::new();
    rows.try_reserve_exact(count)?;
    identities.try_reserve_exact(count)?;
    if table {
        let dictionary = dictionary.as_ref().unwrap();
        for _ in 0..count {
            identities.push(dictionary.read_row(&mut input)?);
        }
    }
    for _ in 0..count {
        if !table {
            identities.push(if let Some(dictionary) = &dictionary {
                dictionary.read_row(&mut input)?
            } else {
                input.identity()?
            });
        }
        rows.push(AllocationRow {
            address: input.u64()?,
            size: input.u64()?,
            type_name: input.row()?,
            file: input.row()?,
            line: input.u32()?,
            received_frame: input.u64()?,
        });
    }
    input.finish()?;
    Ok((rows, Some(identities)))
}
