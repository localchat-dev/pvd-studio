use pvd_core::Value;
use std::fmt;

pub const MAX_BYTES: usize = 1 << 20;
pub const MAX_DEPTH: usize = 64;
pub const MAX_NODES: usize = 100_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyError {
    Bytes,
    Depth,
    Nodes,
}

impl fmt::Display for CopyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bytes => write!(
                formatter,
                "property copy exceeds the {MAX_BYTES} byte limit"
            ),
            Self::Depth => write!(
                formatter,
                "property copy exceeds the {MAX_DEPTH} level nesting limit"
            ),
            Self::Nodes => write!(
                formatter,
                "property copy exceeds the {MAX_NODES} value-node limit"
            ),
        }
    }
}

impl std::error::Error for CopyError {}

pub fn text(value: &Value) -> Result<String, CopyError> {
    let mut writer = Writer {
        output: String::new(),
        remaining_nodes: MAX_NODES,
    };
    writer.value(value, 0)?;
    Ok(writer.output)
}

struct Writer {
    output: String,
    remaining_nodes: usize,
}

impl Writer {
    fn ensure(&self, bytes: usize) -> Result<(), CopyError> {
        if bytes > MAX_BYTES.saturating_sub(self.output.len()) {
            Err(CopyError::Bytes)
        } else {
            Ok(())
        }
    }

    fn append(&mut self, value: &str) -> Result<(), CopyError> {
        self.ensure(value.len())?;
        self.output.push_str(value);
        Ok(())
    }

    fn number(&mut self, decimal: String, scientific: String) -> Result<(), CopyError> {
        self.append(if decimal.len() <= scientific.len() {
            &decimal
        } else {
            &scientific
        })
    }

    fn f32(&mut self, value: f32) -> Result<(), CopyError> {
        if value.is_finite() {
            self.number(value.to_string(), format!("{value:e}"))
        } else {
            let label = nonfinite_name(value.is_nan(), value.is_sign_negative());
            self.append(&format!("{label}(f32, bits=0x{:08X})", value.to_bits()))
        }
    }

    fn f64(&mut self, value: f64) -> Result<(), CopyError> {
        if value.is_finite() {
            self.number(value.to_string(), format!("{value:e}"))
        } else {
            let label = nonfinite_name(value.is_nan(), value.is_sign_negative());
            self.append(&format!("{label}(f64, bits=0x{:016X})", value.to_bits()))
        }
    }

    fn tuple(&mut self, name: &str, values: &[f32]) -> Result<(), CopyError> {
        self.append(name)?;
        self.append("(")?;
        for (index, value) in values.iter().enumerate() {
            if index > 0 {
                self.append(", ")?;
            }
            self.f32(*value)?;
        }
        self.append(")")
    }

    fn quoted(&mut self, value: &str) -> Result<(), CopyError> {
        self.ensure(value.len())?;
        self.append("\"")?;
        for character in value.chars() {
            match character {
                '"' => self.append("\\\"")?,
                '\\' => self.append("\\\\")?,
                '\n' => self.append("\\n")?,
                '\r' => self.append("\\r")?,
                '\t' => self.append("\\t")?,
                '\u{8}' => self.append("\\b")?,
                '\u{c}' => self.append("\\f")?,
                character if character <= '\u{1f}' => {
                    self.append(&format!("\\u{:04X}", u32::from(character)))?
                }
                character => self.append(character.encode_utf8(&mut [0; 4]))?,
            }
        }
        self.append("\"")
    }

    fn bytes(&mut self, values: &[u8]) -> Result<(), CopyError> {
        let length = if values.is_empty() {
            7
        } else {
            values
                .len()
                .checked_mul(3)
                .and_then(|length| length.checked_add(6))
                .ok_or(CopyError::Bytes)?
        };
        self.ensure(length)?;
        self.append("bytes[")?;
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        for (index, byte) in values.iter().enumerate() {
            if index > 0 {
                self.output.push(' ');
            }
            self.output.push(char::from(HEX[usize::from(byte >> 4)]));
            self.output.push(char::from(HEX[usize::from(byte & 15)]));
        }
        self.append("]")
    }

    fn value(&mut self, value: &Value, depth: usize) -> Result<(), CopyError> {
        if depth > MAX_DEPTH {
            return Err(CopyError::Depth);
        }
        if self.remaining_nodes == 0 {
            return Err(CopyError::Nodes);
        }
        self.remaining_nodes -= 1;
        match value {
            Value::Bool(value) => self.append(if *value { "true" } else { "false" }),
            Value::I64(value) => self.append(&value.to_string()),
            Value::U64(value) => self.append(&value.to_string()),
            Value::F32(value) => self.f32(*value),
            Value::F64(value) => self.f64(*value),
            Value::String(value) if depth == 0 => self.append(value),
            Value::String(value) => self.quoted(value),
            Value::Vec3(value) => self.tuple("vec3", value),
            Value::Quat(value) => self.tuple("quat", value),
            Value::Transform { position, rotation } => {
                self.append("transform { position: ")?;
                self.tuple("vec3", position)?;
                self.append(", rotation: ")?;
                self.tuple("quat", rotation)?;
                self.append(" }")
            }
            Value::Ref(value) => self.append(&format!("0x{value:016X}")),
            Value::Array(_) | Value::MeshArray(_) => {
                let values = value.array_items().unwrap();
                if values.len() > self.remaining_nodes {
                    return Err(CopyError::Nodes);
                }
                self.append("[")?;
                for (index, value) in values.enumerate() {
                    if index > 0 {
                        self.append(", ")?;
                    }
                    self.value(&value, depth + 1)?;
                }
                self.append("]")
            }
            Value::Bytes(values) => self.bytes(values),
        }
    }
}

fn nonfinite_name(nan: bool, negative: bool) -> &'static str {
    if nan {
        "NaN"
    } else if negative {
        "-Infinity"
    } else {
        "+Infinity"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn finite_floats_round_trip_without_losing_negative_zero_or_small_values() {
        for value in [
            0.0f32,
            -0.0,
            f32::from_bits(1),
            -f32::from_bits(1),
            f32::MIN_POSITIVE,
            f32::MAX,
            f32::MIN,
            1.2345679,
            std::f32::consts::PI,
        ] {
            let copied = text(&Value::F32(value)).unwrap();
            assert_eq!(
                copied.parse::<f32>().unwrap().to_bits(),
                value.to_bits(),
                "{copied}"
            );
        }
        for value in [
            0.0f64,
            -0.0,
            f64::from_bits(1),
            -f64::from_bits(1),
            f64::MIN_POSITIVE,
            f64::MAX,
            f64::MIN,
            1.2345678901234567,
            std::f64::consts::PI,
        ] {
            let copied = text(&Value::F64(value)).unwrap();
            assert_eq!(
                copied.parse::<f64>().unwrap().to_bits(),
                value.to_bits(),
                "{copied}"
            );
        }
        assert_eq!(text(&Value::F32(f32::from_bits(1))).unwrap(), "1e-45");
        assert_eq!(text(&Value::F64(f64::from_bits(1))).unwrap(), "5e-324");
        assert_eq!(text(&Value::F32(-0.0)).unwrap(), "-0");
        let mut bits = 0xD1B5_4A32_D192_ED03u64;
        for _ in 0..20_000 {
            bits ^= bits << 13;
            bits ^= bits >> 7;
            bits ^= bits << 17;
            let value = f64::from_bits(bits);
            if value.is_finite() {
                assert_eq!(
                    text(&Value::F64(value))
                        .unwrap()
                        .parse::<f64>()
                        .unwrap()
                        .to_bits(),
                    bits
                );
            }
            let value = f32::from_bits(bits as u32);
            if value.is_finite() {
                assert_eq!(
                    text(&Value::F32(value))
                        .unwrap()
                        .parse::<f32>()
                        .unwrap()
                        .to_bits(),
                    bits as u32
                );
            }
        }
    }

    #[test]
    fn nonfinite_values_preserve_type_and_exact_ieee_bits() {
        for bits in [0x7FC0_1234, 0xFFC0_0001, 0x7F80_0001] {
            assert_eq!(
                text(&Value::F32(f32::from_bits(bits))).unwrap(),
                format!("NaN(f32, bits=0x{bits:08X})")
            );
        }
        for bits in [
            0x7FF8_0000_0000_1234,
            0xFFF8_0000_0000_0001,
            0x7FF0_0000_0000_0001,
        ] {
            assert_eq!(
                text(&Value::F64(f64::from_bits(bits))).unwrap(),
                format!("NaN(f64, bits=0x{bits:016X})")
            );
        }
        assert_eq!(
            text(&Value::F32(f32::INFINITY)).unwrap(),
            "+Infinity(f32, bits=0x7F800000)"
        );
        assert_eq!(
            text(&Value::F64(f64::NEG_INFINITY)).unwrap(),
            "-Infinity(f64, bits=0xFFF0000000000000)"
        );
    }

    #[test]
    fn arrays_strings_bytes_and_transforms_copy_complete_values() {
        let value = Value::Array(Arc::new((0..12).map(Value::U64).collect()));
        assert_eq!(
            text(&value).unwrap(),
            "[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]"
        );
        let string = "中英\n\"\\\t\0\u{8}\u{c}";
        assert_eq!(text(&Value::String(string.into())).unwrap(), string);
        assert_eq!(
            text(&Value::Array(Arc::new(vec![Value::String(string.into())]))).unwrap(),
            "[\"中英\\n\\\"\\\\\\t\\u0000\\b\\f\"]"
        );
        assert_eq!(
            text(&Value::Bytes(Arc::from([0, 1, 15, 16, 128, 255]))).unwrap(),
            "bytes[00 01 0F 10 80 FF]"
        );
        assert_eq!(text(&Value::Bytes(Arc::from([]))).unwrap(), "bytes[]");
        let position = [1.2345679, -0.0, f32::from_bits(1)];
        let rotation = [0.000012345678, -0.25, 0.5, 0.12345679];
        let expected_position = position
            .map(|value| text(&Value::F32(value)).unwrap())
            .join(", ");
        let expected_rotation = rotation
            .map(|value| text(&Value::F32(value)).unwrap())
            .join(", ");
        assert_eq!(
            text(&Value::Vec3(position)).unwrap(),
            format!("vec3({expected_position})")
        );
        assert_eq!(
            text(&Value::Quat(rotation)).unwrap(),
            format!("quat({expected_rotation})")
        );
        assert_eq!(
            text(&Value::Transform { position, rotation }).unwrap(),
            format!(
                "transform {{ position: vec3({expected_position}), rotation: quat({expected_rotation}) }}"
            )
        );
        assert_eq!(text(&Value::I64(i64::MIN)).unwrap(), i64::MIN.to_string());
        assert_eq!(text(&Value::U64(u64::MAX)).unwrap(), u64::MAX.to_string());
        assert_eq!(text(&Value::Ref(u64::MAX)).unwrap(), "0xFFFFFFFFFFFFFFFF");
    }

    #[test]
    fn size_depth_and_node_limits_fail_without_returning_truncated_text() {
        assert_eq!(
            text(&Value::String("x".repeat(MAX_BYTES))).unwrap().len(),
            MAX_BYTES
        );
        assert_eq!(
            text(&Value::String("x".repeat(MAX_BYTES + 1))).err(),
            Some(CopyError::Bytes)
        );
        let escaped = Value::Array(Arc::new(vec![Value::String("\0".repeat(MAX_BYTES / 6))]));
        assert_eq!(text(&escaped).unwrap().len(), MAX_BYTES);
        let escaped = Value::Array(Arc::new(vec![Value::String(
            "\0".repeat(MAX_BYTES / 6 + 1),
        )]));
        assert_eq!(text(&escaped).err(), Some(CopyError::Bytes));
        let byte_count = (MAX_BYTES - 6) / 3;
        assert!(
            text(&Value::Bytes(vec![255; byte_count].into()))
                .unwrap()
                .len()
                <= MAX_BYTES
        );
        assert_eq!(
            text(&Value::Bytes(vec![255; byte_count + 1].into())).err(),
            Some(CopyError::Bytes)
        );
        let mut nested = Value::U64(1);
        for _ in 0..MAX_DEPTH {
            nested = Value::Array(Arc::new(vec![nested]));
        }
        assert!(text(&nested).is_ok());
        nested = Value::Array(Arc::new(vec![nested]));
        assert_eq!(text(&nested).err(), Some(CopyError::Depth));
        let nodes = Value::Array(Arc::new(vec![Value::Bool(false); MAX_NODES - 1]));
        assert!(text(&nodes).is_ok());
        let nodes = Value::Array(Arc::new(vec![Value::Bool(false); MAX_NODES]));
        assert_eq!(text(&nodes).err(), Some(CopyError::Nodes));
    }
}
