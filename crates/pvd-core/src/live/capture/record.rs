use super::*;
use serde::de::{self, DeserializeSeed, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor};
use std::cell::{Cell, RefCell};

const ALLOCATION_ALLOWANCE: usize = 512 << 20;
const MAX_DEPTH: usize = 64;

pub(in crate::live) fn decode(
    bytes: &[u8],
    cancelled: &mut impl FnMut() -> bool,
) -> Result<Record, Error> {
    decode_value(bytes, cancelled, ALLOCATION_ALLOWANCE)
}

#[cfg(test)]
pub(in crate::live) fn decode_for_accounting(
    bytes: &[u8],
    cancelled: &mut impl FnMut() -> bool,
    allocation_allowance: usize,
) -> Result<Record, Error> {
    decode_value(bytes, cancelled, allocation_allowance)
}

pub(in crate::live) fn decode_value<'de, T: Deserialize<'de>>(
    bytes: &'de [u8],
    cancelled: &mut impl FnMut() -> bool,
    limit: usize,
) -> Result<T, Error> {
    let state = State {
        used: Cell::new(0),
        depth: Cell::new(0),
        limit,
        cancelled: RefCell::new(cancelled),
        failure: RefCell::new(None),
    };
    let mut decoder = postcard::Deserializer::from_bytes(bytes);
    let result = T::deserialize(Limited {
        inner: &mut decoder,
        state: &state,
    });
    let failure = state.failure.borrow_mut().take();
    drop(state);
    if let Some(failure) = failure {
        return Err(failure);
    }
    let value = result?;
    if !decoder.finalize()?.is_empty() {
        return Err(error("capture record has trailing bytes"));
    }
    check_replay_cancelled(cancelled)?;
    Ok(value)
}

struct State<'a> {
    used: Cell<usize>,
    depth: Cell<usize>,
    limit: usize,
    cancelled: RefCell<&'a mut dyn FnMut() -> bool>,
    failure: RefCell<Option<Error>>,
}
impl<'c> State<'c> {
    fn fail<E: de::Error>(&self, failure: Error) -> E {
        *self.failure.borrow_mut() = Some(failure);
        E::custom("capture record decoding stopped")
    }
    fn charge<E: de::Error>(&self, bytes: usize) -> Result<(), E> {
        if (self.cancelled.borrow_mut())() {
            return Err(self.fail(
                std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "capture record decoding cancelled",
                )
                .into(),
            ));
        }
        let next = self
            .used
            .get()
            .checked_add(bytes)
            .filter(|n| *n <= self.limit)
            .ok_or_else(|| {
                self.fail(error(
                    "capture record exceeds its decoded allocation allowance",
                ))
            })?;
        self.used.set(next);
        Ok(())
    }
    fn text<E: de::Error>(&self, bytes: usize) -> Result<(), E> {
        let allowance = bytes
            .checked_mul(3)
            .and_then(|n| n.checked_add(64))
            .ok_or_else(|| self.fail(error("capture decoded string size overflow")))?;
        self.charge(allowance)
    }
    fn enter<E: de::Error>(&self) -> Result<Depth<'_, 'c>, E> {
        self.charge(0)?;
        if self.depth.get() >= MAX_DEPTH {
            return Err(self.fail(error("capture record nesting exceeds its limit")));
        }
        self.depth.set(self.depth.get() + 1);
        Ok(Depth(self))
    }
    fn count<E: de::Error>(&self, hint: Option<usize>) -> Result<(), E> {
        if hint.is_some_and(|n| n > MAX_RECORD) {
            return Err(self.fail(error("capture record collection count exceeds its limit")));
        }
        self.charge(0)
    }
}
struct Depth<'s, 'c>(&'s State<'c>);
impl Drop for Depth<'_, '_> {
    fn drop(&mut self) {
        self.0.depth.set(self.0.depth.get() - 1);
    }
}
struct Limited<'s, 'c, D> {
    inner: D,
    state: &'s State<'c>,
}
struct Visit<'s, 'c, V> {
    inner: V,
    state: &'s State<'c>,
}
struct Access<'s, 'c, A> {
    inner: A,
    state: &'s State<'c>,
}
struct Seed<'s, 'c, S> {
    inner: S,
    state: &'s State<'c>,
}

macro_rules! plain_decoder {
    ($($name:ident),* $(,)?) => {$(
        fn $name<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
            let _depth = self.state.enter::<Self::Error>()?;
            self.inner.$name(Visit { inner: visitor, state: self.state })
        }
    )*};
}
impl<'de, D: de::Deserializer<'de>> de::Deserializer<'de> for Limited<'_, '_, D> {
    type Error = D::Error;
    plain_decoder!(
        deserialize_any,
        deserialize_bool,
        deserialize_i8,
        deserialize_i16,
        deserialize_i32,
        deserialize_i64,
        deserialize_i128,
        deserialize_u8,
        deserialize_u16,
        deserialize_u32,
        deserialize_u64,
        deserialize_u128,
        deserialize_f32,
        deserialize_f64,
        deserialize_char,
        deserialize_str,
        deserialize_string,
        deserialize_bytes,
        deserialize_byte_buf,
        deserialize_option,
        deserialize_unit,
        deserialize_seq,
        deserialize_map,
        deserialize_identifier,
        deserialize_ignored_any
    );
    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let _depth = self.state.enter::<Self::Error>()?;
        self.inner.deserialize_unit_struct(
            name,
            Visit {
                inner: visitor,
                state: self.state,
            },
        )
    }
    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let _depth = self.state.enter::<Self::Error>()?;
        self.inner.deserialize_newtype_struct(
            name,
            Visit {
                inner: visitor,
                state: self.state,
            },
        )
    }
    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let _depth = self.state.enter::<Self::Error>()?;
        self.inner.deserialize_tuple(
            len,
            Visit {
                inner: visitor,
                state: self.state,
            },
        )
    }
    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let _depth = self.state.enter::<Self::Error>()?;
        self.inner.deserialize_tuple_struct(
            name,
            len,
            Visit {
                inner: visitor,
                state: self.state,
            },
        )
    }
    fn deserialize_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let _depth = self.state.enter::<Self::Error>()?;
        self.inner.deserialize_struct(
            name,
            fields,
            Visit {
                inner: visitor,
                state: self.state,
            },
        )
    }
    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let _depth = self.state.enter::<Self::Error>()?;
        self.inner.deserialize_enum(
            name,
            variants,
            Visit {
                inner: visitor,
                state: self.state,
            },
        )
    }
    fn is_human_readable(&self) -> bool {
        self.inner.is_human_readable()
    }
}
impl<'de, V: Visitor<'de>> Visitor<'de> for Visit<'_, '_, V> {
    type Value = V::Value;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        self.inner.expecting(f)
    }
    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        self.inner.visit_bool(value)
    }
    fn visit_i8<E: de::Error>(self, value: i8) -> Result<Self::Value, E> {
        self.inner.visit_i8(value)
    }
    fn visit_i16<E: de::Error>(self, value: i16) -> Result<Self::Value, E> {
        self.inner.visit_i16(value)
    }
    fn visit_i32<E: de::Error>(self, value: i32) -> Result<Self::Value, E> {
        self.inner.visit_i32(value)
    }
    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        self.inner.visit_i64(value)
    }
    fn visit_i128<E: de::Error>(self, value: i128) -> Result<Self::Value, E> {
        self.inner.visit_i128(value)
    }
    fn visit_u8<E: de::Error>(self, value: u8) -> Result<Self::Value, E> {
        self.inner.visit_u8(value)
    }
    fn visit_u16<E: de::Error>(self, value: u16) -> Result<Self::Value, E> {
        self.inner.visit_u16(value)
    }
    fn visit_u32<E: de::Error>(self, value: u32) -> Result<Self::Value, E> {
        self.inner.visit_u32(value)
    }
    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        self.inner.visit_u64(value)
    }
    fn visit_u128<E: de::Error>(self, value: u128) -> Result<Self::Value, E> {
        self.inner.visit_u128(value)
    }
    fn visit_f32<E: de::Error>(self, value: f32) -> Result<Self::Value, E> {
        self.inner.visit_f32(value)
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        self.inner.visit_f64(value)
    }
    fn visit_char<E: de::Error>(self, value: char) -> Result<Self::Value, E> {
        self.inner.visit_char(value)
    }
    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        self.state.text::<E>(value.len())?;
        self.inner.visit_str(value)
    }
    fn visit_borrowed_str<E: de::Error>(self, value: &'de str) -> Result<Self::Value, E> {
        self.state.text::<E>(value.len())?;
        self.inner.visit_borrowed_str(value)
    }
    fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
        self.state.text::<E>(value.capacity())?;
        self.inner.visit_string(value)
    }
    fn visit_bytes<E: de::Error>(self, value: &[u8]) -> Result<Self::Value, E> {
        self.state.text::<E>(value.len())?;
        self.inner.visit_bytes(value)
    }
    fn visit_borrowed_bytes<E: de::Error>(self, value: &'de [u8]) -> Result<Self::Value, E> {
        self.state.text::<E>(value.len())?;
        self.inner.visit_borrowed_bytes(value)
    }
    fn visit_byte_buf<E: de::Error>(self, value: Vec<u8>) -> Result<Self::Value, E> {
        self.state.text::<E>(value.capacity())?;
        self.inner.visit_byte_buf(value)
    }
    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        self.inner.visit_unit()
    }
    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        self.inner.visit_none()
    }
    fn visit_some<D: de::Deserializer<'de>>(self, decoder: D) -> Result<Self::Value, D::Error> {
        self.inner.visit_some(Limited {
            inner: decoder,
            state: self.state,
        })
    }
    fn visit_newtype_struct<D: de::Deserializer<'de>>(
        self,
        decoder: D,
    ) -> Result<Self::Value, D::Error> {
        self.inner.visit_newtype_struct(Limited {
            inner: decoder,
            state: self.state,
        })
    }
    fn visit_seq<A: SeqAccess<'de>>(self, access: A) -> Result<Self::Value, A::Error> {
        self.state.count::<A::Error>(access.size_hint())?;
        self.inner.visit_seq(Access {
            inner: access,
            state: self.state,
        })
    }
    fn visit_map<A: MapAccess<'de>>(self, access: A) -> Result<Self::Value, A::Error> {
        self.state.count::<A::Error>(access.size_hint())?;
        self.inner.visit_map(Access {
            inner: access,
            state: self.state,
        })
    }
    fn visit_enum<A: EnumAccess<'de>>(self, access: A) -> Result<Self::Value, A::Error> {
        self.inner.visit_enum(Access {
            inner: access,
            state: self.state,
        })
    }
}
impl<'de, S: DeserializeSeed<'de>> DeserializeSeed<'de> for Seed<'_, '_, S> {
    type Value = S::Value;
    fn deserialize<D: de::Deserializer<'de>>(self, decoder: D) -> Result<Self::Value, D::Error> {
        self.inner.deserialize(Limited {
            inner: decoder,
            state: self.state,
        })
    }
}
impl<'de, A: SeqAccess<'de>> SeqAccess<'de> for Access<'_, '_, A> {
    type Error = A::Error;
    fn next_element_seed<S: DeserializeSeed<'de>>(
        &mut self,
        seed: S,
    ) -> Result<Option<S::Value>, Self::Error> {
        self.state
            .charge::<Self::Error>(size_of::<S::Value>().max(1).saturating_mul(4))?;
        self.inner.next_element_seed(Seed {
            inner: seed,
            state: self.state,
        })
    }
    // Collection capacities grow from decoded elements, never from an untrusted wire count.
    fn size_hint(&self) -> Option<usize> {
        Some(0)
    }
}
impl<'de, A: MapAccess<'de>> MapAccess<'de> for Access<'_, '_, A> {
    type Error = A::Error;
    fn next_key_seed<S: DeserializeSeed<'de>>(
        &mut self,
        seed: S,
    ) -> Result<Option<S::Value>, Self::Error> {
        self.state
            .charge::<Self::Error>(size_of::<S::Value>().saturating_mul(12).saturating_add(128))?;
        self.inner.next_key_seed(Seed {
            inner: seed,
            state: self.state,
        })
    }
    fn next_value_seed<S: DeserializeSeed<'de>>(
        &mut self,
        seed: S,
    ) -> Result<S::Value, Self::Error> {
        self.state
            .charge::<Self::Error>(size_of::<S::Value>().saturating_mul(12).saturating_add(128))?;
        self.inner.next_value_seed(Seed {
            inner: seed,
            state: self.state,
        })
    }
    fn size_hint(&self) -> Option<usize> {
        Some(0)
    }
}
impl<'de, 's, 'c, A: EnumAccess<'de>> EnumAccess<'de> for Access<'s, 'c, A> {
    type Error = A::Error;
    type Variant = Access<'s, 'c, A::Variant>;
    fn variant_seed<S: DeserializeSeed<'de>>(
        self,
        seed: S,
    ) -> Result<(S::Value, Self::Variant), Self::Error> {
        let (value, access) = self.inner.variant_seed(Seed {
            inner: seed,
            state: self.state,
        })?;
        Ok((
            value,
            Access {
                inner: access,
                state: self.state,
            },
        ))
    }
}
impl<'de, A: VariantAccess<'de>> VariantAccess<'de> for Access<'_, '_, A> {
    type Error = A::Error;
    fn unit_variant(self) -> Result<(), Self::Error> {
        self.inner.unit_variant()
    }
    fn newtype_variant_seed<S: DeserializeSeed<'de>>(
        self,
        seed: S,
    ) -> Result<S::Value, Self::Error> {
        self.state
            .charge::<Self::Error>(size_of::<S::Value>().saturating_add(32))?;
        self.inner.newtype_variant_seed(Seed {
            inner: seed,
            state: self.state,
        })
    }
    fn tuple_variant<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.inner.tuple_variant(
            len,
            Visit {
                inner: visitor,
                state: self.state,
            },
        )
    }
    fn struct_variant<V: Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.inner.struct_variant(
            fields,
            Visit {
                inner: visitor,
                state: self.state,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn count_allowance_cancellation_and_depth_are_checked_before_publication() {
        let huge = postcard::to_stdvec(&u64::MAX).unwrap();
        assert!(decode_value::<Vec<u8>>(&huge, &mut || false, ALLOCATION_ALLOWANCE).is_err());
        let bytes = postcard::to_stdvec(&vec!["allocation".to_string(); 100]).unwrap();
        assert!(decode_value::<Vec<String>>(&bytes, &mut || false, 100).is_err());
        let value =
            decode_value::<Vec<String>>(&bytes, &mut || false, ALLOCATION_ALLOWANCE).unwrap();
        assert_eq!(value.len(), 100);
        let mut calls = 0;
        let failure = decode_value::<Vec<String>>(
            &bytes,
            &mut || {
                calls += 1;
                calls > 20
            },
            ALLOCATION_ALLOWANCE,
        )
        .unwrap_err();
        assert_eq!(
            failure.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
        let mut nested = Value::U64(7);
        for _ in 0..80 {
            nested = Value::Array(Arc::new(vec![nested]));
        }
        let bytes = postcard::to_stdvec(&Record::Value(nested)).unwrap();
        assert!(decode(&bytes, &mut || false).is_err());
    }
    #[test]
    fn bounded_decode_keeps_record_wire_and_map_entries() {
        let mut object = Object {
            id: 17,
            generation: 2,
            type_name: "T".into(),
            properties: BTreeMap::new(),
        };
        object.properties.insert(
            "values".into(),
            Value::Array(Arc::new(vec![Value::String("text".into()), Value::Ref(31)])),
        );
        object
            .properties
            .insert("bytes".into(), Value::Bytes(vec![42; 8192].into()));
        let value = Record::Value(Value::Array(Arc::new(
            object.properties.into_values().collect(),
        )));
        let wire = postcard::to_stdvec(&value).unwrap();
        let restored = decode(&wire, &mut || false).unwrap();
        assert_eq!(postcard::to_stdvec(&restored).unwrap(), wire);
    }
}
