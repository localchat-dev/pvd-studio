use super::*;
use crate::mesh_array::{Element, MIN_PACKED_ITEMS, MeshArray};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::live) enum PropertyLayout {
    General,
    Points,
    Indices,
}

impl PropertyLayout {
    pub(super) fn for_property(class: &str, key: &str) -> Self {
        if class.rsplit("::").next() == Some("PxTriangleMesh") {
            match key {
                "Points" => return Self::Points,
                "Triangles" => return Self::Indices,
                _ => {}
            }
        }
        Self::General
    }

    pub(super) fn decode(
        self,
        reader: &HistoryReader,
        bytes: &[u8],
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Value, Error> {
        if self != Self::General
            && let Some(value) = self.packed(bytes, cancelled)?
        {
            return Ok(Value::MeshArray(value));
        }
        let Record::Value(value) = reader.decode(bytes, cancelled)? else {
            return Err(error("history property record type mismatch"));
        };
        Ok(value)
    }

    fn packed(
        self,
        bytes: &[u8],
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Option<MeshArray>, Error> {
        // PVDCAP stores Record::Value(Value::Array) with one Value tag per element.
        let Some(payload) = bytes.strip_prefix(&[6, 10]) else {
            return Ok(None);
        };
        let (count, mut items) = postcard::take_from_bytes::<usize>(payload)?;
        if count < MIN_PACKED_ITEMS {
            return Ok(None);
        }
        let mut packed = Vec::new();
        let kind = match self {
            Self::Points if count <= crate::mesh::MAX_VERTICES && items.len() == 13 * count => {
                packed.try_reserve_exact(count * 12)?;
                for (index, point) in items.chunks_exact(13).enumerate() {
                    if index.is_multiple_of(4096) {
                        check_replay_cancelled(cancelled)?;
                    }
                    if point[0] != 6 {
                        return Ok(None);
                    }
                    packed.extend_from_slice(&point[1..]);
                }
                Element::Point
            }
            Self::Indices if count <= crate::mesh::MAX_INDICES && count * 2 <= items.len() => {
                packed.try_reserve_exact(count * 4)?;
                let mut largest_index = 0;
                for index in 0..count {
                    if index.is_multiple_of(4096) {
                        check_replay_cancelled(cancelled)?;
                    }
                    let Some(payload) = items.strip_prefix(&[2]) else {
                        return Ok(None);
                    };
                    let (number, remaining) = postcard::take_from_bytes::<u64>(payload)?;
                    let Ok(number) = u32::try_from(number) else {
                        return Ok(None);
                    };
                    largest_index = largest_index.max(number);
                    packed.extend_from_slice(&number.to_le_bytes());
                    items = remaining;
                }
                if !items.is_empty() {
                    return Err(error("history record has trailing bytes"));
                }
                if largest_index <= u32::from(u16::MAX) {
                    for index in 0..count {
                        packed.copy_within(4 * index..4 * index + 2, 2 * index);
                    }
                    packed.truncate(count * 2);
                    Element::Index16
                } else {
                    Element::Index32
                }
            }
            _ => return Ok(None),
        };
        check_replay_cancelled(cancelled)?;
        Ok(Some(MeshArray::from_le_bytes(kind, packed)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_replay_reads_legacy_values_and_falls_back_for_other_layouts() {
        let point = Value::Vec3([f32::from_bits(0x7fc0_2345), -0.0, 3.0]);
        for (layout, element) in [
            (PropertyLayout::Points, point),
            (PropertyLayout::Indices, Value::U64(65535)),
            (PropertyLayout::Indices, Value::U64(4_000_003)),
        ] {
            let value = Value::Array(Arc::new(vec![element; 4097]));
            let encoded = postcard::to_stdvec(&Record::Value(value.clone())).unwrap();
            let packed = layout.packed(&encoded, &mut || false).unwrap().unwrap();
            if matches!(value.array_item(0).as_deref(), Some(Value::U64(65535))) {
                assert_eq!(packed.bytes().len(), 4097 * 2);
            }
            assert_eq!(Value::MeshArray(packed), value);
            assert!(layout.packed(&encoded, &mut || true).is_err());
        }
        for element in [Value::U64(u64::MAX), Value::Ref(3)] {
            let value = Value::Array(Arc::new(vec![element; 256]));
            let encoded = postcard::to_stdvec(&Record::Value(value)).unwrap();
            assert!(
                PropertyLayout::Indices
                    .packed(&encoded, &mut || false)
                    .unwrap()
                    .is_none()
            );
        }
    }
}
