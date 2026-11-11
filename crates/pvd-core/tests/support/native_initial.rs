use pvd_core::Value;
use std::collections::BTreeMap;

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> &'a [u8] {
        let result = &self.bytes[self.position..self.position + length];
        self.position += length;
        result
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take(4).try_into().unwrap())
    }
    fn blob(&mut self) -> &'a [u8] {
        let length = self.u32() as usize;
        self.take(length)
    }
    fn skip_words(&mut self, width: usize) {
        let count = self.u32() as usize;
        self.take(count * width);
    }
}

struct Property {
    name: u32,
    datatype: usize,
    kind: u32,
    offset: usize,
}

struct Class {
    namespace: u32,
    name: u32,
    base: u32,
    size: usize,
    dynamic: bool,
    properties: Vec<usize>,
}

pub struct NativeInitialValues {
    fields: BTreeMap<(String, String), Value>,
    display: BTreeMap<(u64, String), Value>,
}

impl NativeInitialValues {
    pub fn parse(bytes: &[u8]) -> Self {
        let mut input = Reader { bytes, position: 0 };
        assert_eq!(input.u32(), 0x36101bb0);
        assert_eq!(input.u32(), 1);
        let object_length = input.u32() as usize;
        let communication_length = input.u32() as usize;
        let mut metadata = Reader {
            bytes: input.take(object_length),
            position: 0,
        };
        assert_eq!(metadata.u32(), 1);
        metadata.u32();
        let string_count = metadata.u32();
        metadata.u32();
        let mut strings = BTreeMap::from([(0, String::new())]);
        for _ in 0..string_count {
            let id = metadata.u32();
            let text = metadata.blob();
            assert_eq!(text.last(), Some(&0));
            strings.insert(
                id,
                std::str::from_utf8(&text[..text.len() - 1]).unwrap().into(),
            );
        }
        let mut properties = Vec::new();
        for _ in 0..metadata.u32() {
            if metadata.u32() == 0 {
                properties.push(None);
                continue;
            }
            let fixed = (0..11).map(|_| metadata.u32()).collect::<Vec<_>>();
            metadata.skip_words(8);
            properties.push(Some(Property {
                name: metadata.u32(),
                datatype: fixed[4] as usize,
                kind: fixed[7],
                offset: fixed[9] as usize,
            }));
        }
        let mut classes = Vec::new();
        for _ in 0..metadata.u32() {
            if metadata.u32() == 0 {
                classes.push(None);
                continue;
            }
            let fixed = (0..6).map(|_| metadata.u32()).collect::<Vec<_>>();
            let flags = metadata.take(2);
            let sizes = (0..6).map(|_| metadata.u32()).collect::<Vec<_>>();
            let class_properties = (0..metadata.u32())
                .map(|_| metadata.u32() as usize)
                .collect();
            metadata.skip_words(8);
            metadata.skip_words(8);
            classes.push(Some(Class {
                namespace: fixed[0],
                name: fixed[1],
                base: fixed[3],
                size: sizes[0] as usize,
                dynamic: flags[1] != 0,
                properties: class_properties,
            }));
        }
        let mut fields = BTreeMap::new();
        for class in classes.iter().flatten() {
            let namespace = &strings[&class.namespace];
            let name = &strings[&class.name];
            let type_name = if namespace.is_empty() {
                name.clone()
            } else {
                format!("{namespace}::{name}")
            };
            let mut ancestor = Some(class);
            let mut depth = 0;
            while let Some(owner) = ancestor {
                depth += 1;
                assert!(depth <= 128);
                for &id in &owner.properties {
                    let property = properties[id].as_ref().unwrap();
                    let datatype = classes[property.datatype].as_ref().unwrap();
                    let storage = if property.kind == 2 {
                        12
                    } else {
                        datatype.size
                    };
                    assert!(property.offset + storage <= class.size);
                    if strings[&datatype.namespace] != "physx3" || datatype.dynamic {
                        continue;
                    }
                    let Some(value) = zero_value(&strings[&datatype.name], datatype.size) else {
                        continue;
                    };
                    let value = if property.kind == 2 {
                        Value::Array(Vec::new().into())
                    } else {
                        value
                    };
                    fields.insert((type_name.clone(), strings[&property.name].clone()), value);
                }
                ancestor = (owner.base != u32::MAX)
                    .then(|| classes[owner.base as usize].as_ref().unwrap());
            }
        }
        let mut comm = Reader {
            bytes: input.take(communication_length),
            position: 20,
        };
        let mut display = BTreeMap::new();
        for _ in 0..comm.u32() {
            let id = u64::from(comm.u32());
            comm.take(8);
            assert_eq!(u64::from(comm.u32()), id);
            let flags = comm.u32();
            let color = Value::Bytes(comm.take(4).to_vec().into());
            let pickable = comm.take(1)[0] != 0;
            let top_level = comm.take(1)[0] != 0;
            for (bit, key, value) in [
                (1, "$pickable", Value::Bool(pickable)),
                (2, "$color", color),
                (4, "$top_level", Value::Bool(top_level)),
            ] {
                if flags & bit != 0 {
                    display.insert((id, key.into()), value);
                }
            }
        }
        Self { fields, display }
    }

    pub fn assert_value(&self, id: u64, class: &str, key: &str, actual: &Value) {
        let expected = self
            .display
            .get(&(id, key.into()))
            .or_else(|| self.fields.get(&(class.into(), key.into())))
            .unwrap_or_else(|| panic!("native-only field has no verified layout: {class}.{key}"));
        assert_eq!(
            actual, expected,
            "native Create/saved preference {class}.{key}"
        );
    }
}

fn zero_value(name: &str, size: usize) -> Option<Value> {
    Some(match (name, size) {
        ("PvdBool", 1) => Value::Bool(false),
        ("PvdU8", 1) | ("PvdU16", 2) | ("PvdU32", 4) | ("PvdU64", 8) => Value::U64(0),
        ("PvdI8", 1) | ("PvdI16", 2) | ("PvdI32", 4) | ("PvdI64", 8) => Value::I64(0),
        ("PvdF32", 4) => Value::F32(0.0),
        ("PvdF64", 8) => Value::F64(0.0),
        ("StringHandle", 4) => Value::String(String::new()),
        ("ObjectRef" | "VoidPtr", 4) => Value::Ref(0),
        ("PxVec3", 12) => Value::Vec3([0.0; 3]),
        ("PxQuat", 16) => Value::Quat([0.0; 4]),
        ("PxTransform", 28) => Value::Transform {
            position: [0.0; 3],
            rotation: [0.0; 4],
        },
        ("PxVec2", 8) | ("PxVec4", 16) | ("PxBounds3", 24) | ("PxMat33", 36) | ("PxMat44", 64) => {
            Value::Array(vec![Value::F32(0.0); size / 4].into())
        }
        ("U32Array4", 16) => Value::Array(vec![Value::U64(0); 4].into()),
        ("PvdHullPolygonData", 4) => Value::Array(vec![Value::U64(0); 2].into()),
        ("PxHeightFieldSample", 4) => {
            Value::Array(vec![Value::I64(0), Value::U64(0), Value::U64(0)].into())
        }
        _ => return None,
    })
}
