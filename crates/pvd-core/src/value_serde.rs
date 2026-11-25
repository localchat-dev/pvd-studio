use crate::{MeshArray, Value};
use serde::{Serialize, Serializer, ser::SerializeStructVariant};

impl Serialize for MeshArray {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.iter())
    }
}

// Project captures and property JSON expose the same Value::Array for both storage layouts.
impl Serialize for Value {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Bool(v) => serializer.serialize_newtype_variant("Value", 0, "Bool", v),
            Self::I64(v) => serializer.serialize_newtype_variant("Value", 1, "I64", v),
            Self::U64(v) => serializer.serialize_newtype_variant("Value", 2, "U64", v),
            Self::F32(v) => serializer.serialize_newtype_variant("Value", 3, "F32", v),
            Self::F64(v) => serializer.serialize_newtype_variant("Value", 4, "F64", v),
            Self::String(v) => serializer.serialize_newtype_variant("Value", 5, "String", v),
            Self::Vec3(v) => serializer.serialize_newtype_variant("Value", 6, "Vec3", v),
            Self::Quat(v) => serializer.serialize_newtype_variant("Value", 7, "Quat", v),
            Self::Transform { position, rotation } => {
                let mut value = serializer.serialize_struct_variant("Value", 8, "Transform", 2)?;
                value.serialize_field("position", position)?;
                value.serialize_field("rotation", rotation)?;
                value.end()
            }
            Self::Ref(v) => serializer.serialize_newtype_variant("Value", 9, "Ref", v),
            Self::Array(v) => serializer.serialize_newtype_variant("Value", 10, "Array", v),
            Self::Bytes(v) => serializer.serialize_newtype_variant("Value", 11, "Bytes", v),
            Self::MeshArray(v) => serializer.serialize_newtype_variant("Value", 10, "Array", v),
        }
    }
}
