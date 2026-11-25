use crate::{Error, Value, error, mesh};
use pvd_protocol::Endian;
use std::sync::Arc;

pub(crate) const MIN_PACKED_ITEMS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Element {
    Point,
    Index16,
    Index32,
}

impl Element {
    fn width(self) -> usize {
        match self {
            Self::Point => 12,
            Self::Index16 => 2,
            Self::Index32 => 4,
        }
    }

    fn max_items(self) -> usize {
        match self {
            Self::Point => mesh::MAX_VERTICES,
            Self::Index16 | Self::Index32 => mesh::MAX_INDICES,
        }
    }

    fn value(self, bytes: &[u8]) -> Value {
        match self {
            Self::Point => Value::Vec3(std::array::from_fn(|i| {
                f32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap())
            })),
            Self::Index16 => Value::U64(u16::from_le_bytes(bytes.try_into().unwrap()).into()),
            Self::Index32 => Value::U64(u32::from_le_bytes(bytes.try_into().unwrap()).into()),
        }
    }
}

/// Captured triangle mesh elements in canonical little-endian order.
#[derive(Clone, Debug)]
pub struct MeshArray {
    kind: Element,
    // Spatial caches can keep a weak identity after the byte allocation is released.
    bytes: Arc<Box<[u8]>>,
}

impl MeshArray {
    pub(crate) fn from_le_bytes(kind: Element, bytes: Vec<u8>) -> Result<Self, Error> {
        if !bytes.len().is_multiple_of(kind.width())
            || bytes.len() / kind.width() > kind.max_items()
        {
            return Err(error("invalid packed mesh element count"));
        }
        Ok(Self {
            kind,
            bytes: Arc::new(bytes.into_boxed_slice()),
        })
    }

    pub(crate) fn decode(
        class: &str,
        key: &str,
        datatype: &str,
        count: u32,
        bytes: &[u8],
        endian: Endian,
    ) -> Result<Option<Self>, Error> {
        if (count as usize) < MIN_PACKED_ITEMS
            || class.rsplit("::").next() != Some("PxTriangleMesh")
        {
            return Ok(None);
        }
        let kind = match (key, datatype.rsplit("::").next()) {
            ("Points", Some("PxVec3")) => Element::Point,
            ("Triangles", Some("PvdU16")) => Element::Index16,
            ("Triangles", Some("PvdU32")) => Element::Index32,
            _ => return Ok(None),
        };
        let items = count as usize;
        if items > kind.max_items() || bytes.len() != items * kind.width() {
            return Err(error(format!(
                "invalid {key} buffer for {count} {datatype} elements"
            )));
        }
        let mut bytes: Box<[u8]> = Box::from(bytes);
        if endian == Endian::Big {
            let width = if kind == Element::Index16 { 2 } else { 4 };
            for scalar in bytes.chunks_exact_mut(width) {
                scalar.reverse();
            }
        }
        Ok(Some(Self {
            kind,
            bytes: Arc::new(bytes),
        }))
    }

    pub fn len(&self) -> usize {
        self.bytes.len() / self.kind.width()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<Value> {
        self.bytes
            .chunks_exact(self.kind.width())
            .nth(index)
            .map(|bytes| self.kind.value(bytes))
    }

    pub(crate) fn bytes(&self) -> &Arc<Box<[u8]>> {
        &self.bytes
    }

    pub(crate) fn iter(&self) -> impl ExactSizeIterator<Item = Value> + '_ {
        self.bytes
            .chunks_exact(self.kind.width())
            .map(|bytes| self.kind.value(bytes))
    }
}

impl PartialEq for MeshArray {
    fn eq(&self, other: &Self) -> bool {
        if self.kind == other.kind {
            return Arc::ptr_eq(&self.bytes, &other.bytes) || self.bytes == other.bytes;
        }
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MESH: &str = "physx3::PxTriangleMesh";

    #[test]
    fn packed_mesh_values_keep_bits_and_legacy_array_semantics() {
        let point = [0x8000_0000_u32, 0x7fc0_01b3, 0x3f80_0000];
        for endian in [Endian::Big, Endian::Little] {
            let words = point.map(|word| match endian {
                Endian::Little => word.to_le_bytes(),
                Endian::Big => word.to_be_bytes(),
            });
            let bytes = words.concat().repeat(256);
            let packed = Value::MeshArray(
                MeshArray::decode(MESH, "Points", "PxVec3", 256, &bytes, endian)
                    .unwrap()
                    .unwrap(),
            );
            let expected = Value::Vec3(point.map(f32::from_bits));
            let legacy = Value::Array(vec![expected.clone(); 256].into());
            assert_eq!(packed, legacy);
            assert_eq!(*packed.array_item(255).unwrap(), expected);
            assert!(packed.array_item(usize::MAX).is_none());
            let saved = postcard::to_stdvec(&packed).unwrap();
            assert_eq!(saved, postcard::to_stdvec(&legacy).unwrap());
            assert_eq!(
                serde_json::to_value(&packed).unwrap(),
                serde_json::to_value(&legacy).unwrap()
            );
            assert_eq!(postcard::from_bytes::<Value>(&saved).unwrap(), legacy);
            assert_eq!(packed.retained_bytes(), bytes.len());
        }
    }

    #[test]
    fn wide_mesh_indices_have_bounded_counts_and_valid_wire_lengths() {
        for (kind, datatype) in [(Element::Index16, "PvdU16"), (Element::Index32, "PvdU32")] {
            let n = 1_000_002;
            let width = kind.width();
            let word = 0x1234_u32.to_be_bytes();
            let bytes = word[4 - width..].repeat(n);
            let packed =
                MeshArray::decode(MESH, "Triangles", datatype, n as u32, &bytes, Endian::Big)
                    .unwrap()
                    .unwrap();
            assert_eq!(packed.len(), n);
            assert_eq!(packed.get(n - 1), Some(Value::U64(0x1234)));
            assert!(
                MeshArray::decode(
                    MESH,
                    "Triangles",
                    datatype,
                    n as u32,
                    &bytes[1..],
                    Endian::Big
                )
                .is_err()
            );
            assert!(
                MeshArray::decode(MESH, "Triangles", datatype, 3_000_001, &[], Endian::Little)
                    .is_err()
            );
            assert!(
                MeshArray::decode(
                    "other::PxShape",
                    "Triangles",
                    datatype,
                    n as u32,
                    &bytes,
                    Endian::Big
                )
                .unwrap()
                .is_none()
            );
        }
        assert!(MeshArray::from_le_bytes(Element::Index16, vec![42]).is_err());
    }
}
