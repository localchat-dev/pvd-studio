use super::*;
use containers::{Item, TreeKey};
use serde::de::{self, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor};
use std::{fmt, marker::PhantomData};

pub(super) struct Context<'a> {
    pub domain: &'a Domain,
    pub cancelled: &'a mut dyn FnMut() -> bool,
    pub failure: Option<Error>,
}
impl Context<'_> {
    fn check<E: de::Error>(&mut self) -> Result<(), E> {
        let checked = check_replay_cancelled(self.cancelled);
        self.result(checked)
    }
    fn result<T, E: de::Error>(&mut self, result: Result<T, Error>) -> Result<T, E> {
        result.map_err(|error| {
            if self.failure.is_none() {
                self.failure = Some(error);
            }
            E::custom("resident state construction failed")
        })
    }
}

pub(super) struct Seed<'a, 'b, T> {
    context: &'a mut Context<'b>,
    marker: PhantomData<T>,
}
impl<'a, 'b, T> Seed<'a, 'b, T> {
    pub(super) fn new(context: &'a mut Context<'b>) -> Self {
        Self {
            context,
            marker: PhantomData,
        }
    }
}
pub(super) trait Decode: Sized {
    fn decode<'de, D: serde::Deserializer<'de>>(
        context: &mut Context<'_>,
        deserializer: D,
    ) -> Result<Self, D::Error>;
}

pub(super) struct RecordSeed<'a, 'b>(pub(super) &'a mut Context<'b>);
impl<'de> DeserializeSeed<'de> for RecordSeed<'_, '_> {
    type Value = Continuation;
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Continuation, D::Error> {
        struct AnalysisVariant<'a, 'b>(&'a mut Context<'b>);
        impl<'de> Visitor<'de> for AnalysisVariant<'_, '_> {
            type Value = Continuation;
            fn expecting(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt.write_str("the Analysis variant in a history record")
            }
            fn visit_enum<A: EnumAccess<'de>>(self, input: A) -> Result<Continuation, A::Error> {
                let (tag, variant) = input.variant::<u32>()?;
                if tag != 5 {
                    return Err(de::Error::custom("history record is not Analysis"));
                }
                variant.newtype_variant_seed(Seed::<Continuation>::new(self.0))
            }
        }
        self.0.check()?;
        deserializer.deserialize_enum(
            "Record",
            &[
                "Object",
                "Debug",
                "Contacts",
                "Checkpoint",
                "Frame",
                "Analysis",
                "Value",
                "References",
            ],
            AnalysisVariant(self.0),
        )
    }
}
impl<'de, T: Decode> DeserializeSeed<'de> for Seed<'_, '_, T> {
    type Value = T;
    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<T, D::Error> {
        self.context.check()?;
        T::decode(self.context, deserializer)
    }
}

macro_rules! scalar {
    ($($ty:ty),* $(,)?) => {$(
        impl Decode for $ty {
            fn decode<'de, D: serde::Deserializer<'de>>(
                _context: &mut Context<'_>,
                deserializer: D,
            ) -> Result<Self, D::Error> {
                serde::Deserialize::deserialize(deserializer)
            }
        }
    )*};
}
scalar!(
    u16,
    u32,
    u64,
    usize,
    (),
    (u64, u32),
    (u64, u64),
    (u64, u16),
    (u64, u16, u32, u64),
    Budget,
    ProfileSpan,
    ProfileValue,
    MemorySample
);

macro_rules! structure {
    ($ty:ident { $($field:ident: $field_type:ty),* $(,)? }) => {
        impl Decode for $ty {
            fn decode<'de, D: serde::Deserializer<'de>>(
                context: &mut Context<'_>,
                deserializer: D,
            ) -> Result<Self, D::Error> {
                struct Fields<'a, 'b>(&'a mut Context<'b>);
                impl<'de> Visitor<'de> for Fields<'_, '_> {
                    type Value = $ty;
                    fn expecting(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
                        fmt.write_str(stringify!($ty))
                    }
                    fn visit_seq<A: SeqAccess<'de>>(self, mut input: A) -> Result<$ty, A::Error> {
                        Ok($ty {
                            $($field: input.next_element_seed(Seed::<$field_type>::new(self.0))?
                                .ok_or_else(|| de::Error::custom(concat!("missing ", stringify!($field))))?,)*
                        })
                    }
                }
                deserializer.deserialize_struct(
                    stringify!($ty),
                    &[$(stringify!($field)),*],
                    Fields(context),
                )
            }
        }
    };
}

impl Decode for Text {
    fn decode<'de, D: serde::Deserializer<'de>>(
        context: &mut Context<'_>,
        deserializer: D,
    ) -> Result<Self, D::Error> {
        struct Str<'a, 'b>(&'a mut Context<'b>);
        impl Visitor<'_> for Str<'_, '_> {
            type Value = Text;
            fn expecting(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt.write_str("a budgeted UTF-8 string")
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Text, E> {
                if value.len() > MAX_STRING_BYTES {
                    return Err(E::custom("resident string exceeds its byte limit"));
                }
                let value = Text::new(value, self.0.domain, &mut || (self.0.cancelled)());
                self.0.result(value)
            }
            fn visit_borrowed_str<E: de::Error>(self, value: &str) -> Result<Text, E> {
                self.visit_str(value)
            }
        }
        deserializer.deserialize_str(Str(context))
    }
}
impl<T: Decode> Decode for Owned<T> {
    fn decode<'de, D: serde::Deserializer<'de>>(
        context: &mut Context<'_>,
        deserializer: D,
    ) -> Result<Self, D::Error> {
        let value = T::decode(context, deserializer)?;
        context.result(Owned::new(value, context.domain, 0))
    }
}
impl<T: Decode> Decode for Option<T> {
    fn decode<'de, D: serde::Deserializer<'de>>(
        context: &mut Context<'_>,
        deserializer: D,
    ) -> Result<Self, D::Error> {
        struct Optional<'a, 'b, T>(&'a mut Context<'b>, PhantomData<T>);
        impl<'de, T: Decode> Visitor<'de> for Optional<'_, '_, T> {
            type Value = Option<T>;
            fn expecting(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt.write_str("an optional resident value")
            }
            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(None)
            }
            fn visit_some<D: serde::Deserializer<'de>>(
                self,
                input: D,
            ) -> Result<Self::Value, D::Error> {
                Seed::<T>::new(self.0).deserialize(input).map(Some)
            }
        }
        deserializer.deserialize_option(Optional(context, PhantomData))
    }
}
impl<T: Decode + Clone + Item> Decode for Buffer<T> {
    fn decode<'de, D: serde::Deserializer<'de>>(
        context: &mut Context<'_>,
        deserializer: D,
    ) -> Result<Self, D::Error> {
        struct Sequence<'a, 'b, T>(&'a mut Context<'b>, PhantomData<T>);
        impl<'de, T: Decode + Clone + Item> Visitor<'de> for Sequence<'_, '_, T> {
            type Value = Buffer<T>;
            fn expecting(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt.write_str("a budgeted resident sequence")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut input: A) -> Result<Self::Value, A::Error> {
                let mut values = self.0.result(BufferDraft::new(
                    self.0.domain,
                    input.size_hint().unwrap_or(0),
                ))?;
                while let Some(value) = input.next_element_seed(Seed::<T>::new(self.0))? {
                    self.0.result(values.push(value))?;
                }
                self.0.result(values.finish())
            }
        }
        deserializer.deserialize_seq(Sequence(context, PhantomData))
    }
}
impl<K: Decode + TreeKey, V: Decode + Clone + Item> Decode for Tree<K, V> {
    fn decode<'de, D: serde::Deserializer<'de>>(
        context: &mut Context<'_>,
        deserializer: D,
    ) -> Result<Self, D::Error> {
        struct Map<'a, 'b, K, V>(&'a mut Context<'b>, PhantomData<(K, V)>);
        impl<'de, K: Decode + TreeKey, V: Decode + Clone + Item> Visitor<'de> for Map<'_, '_, K, V> {
            type Value = Tree<K, V>;
            fn expecting(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt.write_str("a budgeted resident map")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut input: A) -> Result<Self::Value, A::Error> {
                let mut values = self.0.result(TreeDraft::new(
                    self.0.domain,
                    input.size_hint().unwrap_or(0),
                ))?;
                while let Some(key) = input.next_key_seed(Seed::<K>::new(self.0))? {
                    let value = input.next_value_seed(Seed::<V>::new(self.0))?;
                    self.0.result(values.insert(key, value))?;
                }
                self.0.result(values.finish())
            }
        }
        deserializer.deserialize_map(Map(context, PhantomData))
    }
}
impl<K: Decode + TreeKey> Decode for Set<K> {
    fn decode<'de, D: serde::Deserializer<'de>>(
        context: &mut Context<'_>,
        deserializer: D,
    ) -> Result<Self, D::Error> {
        struct Keys<'a, 'b, K>(&'a mut Context<'b>, PhantomData<K>);
        impl<'de, K: Decode + TreeKey> Visitor<'de> for Keys<'_, '_, K> {
            type Value = Set<K>;
            fn expecting(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt.write_str("a budgeted resident set")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut input: A) -> Result<Self::Value, A::Error> {
                let mut values = self.0.result(TreeDraft::new(
                    self.0.domain,
                    input.size_hint().unwrap_or(0),
                ))?;
                while let Some(key) = input.next_element_seed(Seed::<K>::new(self.0))? {
                    self.0.result(values.insert(key, ()))?;
                }
                self.0.result(values.finish().map(Set))
            }
        }
        deserializer.deserialize_seq(Keys(context, PhantomData))
    }
}
impl Decode for ResidentMap {
    fn decode<'de, D: serde::Deserializer<'de>>(
        context: &mut Context<'_>,
        deserializer: D,
    ) -> Result<Self, D::Error> {
        struct Entries<'a, 'b>(&'a mut Context<'b>);
        impl<'de> Visitor<'de> for Entries<'_, '_> {
            type Value = ResidentMap;
            fn expecting(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt.write_str("an allocation map in a shared budget domain")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut input: A) -> Result<ResidentMap, A::Error> {
                if input.size_hint().is_some_and(|n| n > 1_000_000) {
                    return Err(de::Error::custom(
                        "resident allocation map exceeds its entry limit",
                    ));
                }
                let mut map = ResidentMap::empty(self.0.domain.clone());
                while let Some(key) = input.next_key_seed(Seed::<Key>::new(self.0))? {
                    let value = input.next_value_seed(Seed::<ResidentValue>::new(self.0))?;
                    let inserted = map.try_insert(key, value, &mut || (self.0.cancelled)());
                    self.0.result(inserted)?;
                    if map.len() > 1_000_000 {
                        return Err(de::Error::custom(
                            "resident allocation map exceeds its entry limit",
                        ));
                    }
                }
                Ok(map)
            }
        }
        deserializer.deserialize_map(Entries(context))
    }
}

structure!(AllocationData {
    address: u64,
    size: u64,
    type_name: Text,
    file: Text,
    line: u32,
    received_frame: usize,
});
impl Decode for ResidentValue {
    fn decode<'de, D: serde::Deserializer<'de>>(
        context: &mut Context<'_>,
        deserializer: D,
    ) -> Result<Self, D::Error> {
        let value = AllocationData::decode(context, deserializer)?;
        context.result(ResidentValue::new(value, context.domain))
    }
}
structure!(ZoneData { name: Text, events: Tree<u16, Text> });
structure!(StateData {
    live: ResidentMap,
    live_bytes: u64,
    total_allocated_bytes: u64,
    total_freed_bytes: u64,
    allocations: u64,
    frees: u64,
    unmatched_frees: u64,
    replaced_addresses: u64,
});
impl Decode for StateRef {
    fn decode<'de, D: serde::Deserializer<'de>>(
        context: &mut Context<'_>,
        deserializer: D,
    ) -> Result<Self, D::Error> {
        Owned::<StateData>::decode(context, deserializer).map(Self)
    }
}

#[derive(Clone)]
struct AllocateBody {
    allocation: ResidentValue,
    replaced: Option<ResidentValue>,
}
structure!(AllocateBody { allocation: ResidentValue, replaced: Option<ResidentValue> });
struct FreeBody {
    allocation: Option<ResidentValue>,
}
structure!(FreeBody { allocation: Option<ResidentValue> });
impl Decode for EventKind {
    fn decode<'de, D: serde::Deserializer<'de>>(
        context: &mut Context<'_>,
        deserializer: D,
    ) -> Result<Self, D::Error> {
        struct Kind<'a, 'b>(&'a mut Context<'b>);
        impl<'de> Visitor<'de> for Kind<'_, '_> {
            type Value = EventKind;
            fn expecting(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt.write_str("an allocation or free event")
            }
            fn visit_enum<A: EnumAccess<'de>>(self, input: A) -> Result<EventKind, A::Error> {
                let (kind, variant) = input.variant::<u32>()?;
                match kind {
                    0 => {
                        struct Fields<'a, 'b>(&'a mut Context<'b>);
                        impl<'de> Visitor<'de> for Fields<'_, '_> {
                            type Value = AllocateBody;
                            fn expecting(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
                                fmt.write_str("allocation and optional replaced value")
                            }
                            fn visit_seq<A: SeqAccess<'de>>(
                                self,
                                mut input: A,
                            ) -> Result<AllocateBody, A::Error> {
                                Ok(AllocateBody {
                                    allocation: input
                                        .next_element_seed(Seed::<ResidentValue>::new(self.0))?
                                        .ok_or_else(|| de::Error::custom("missing allocation"))?,
                                    replaced: input
                                        .next_element_seed(Seed::<Option<ResidentValue>>::new(
                                            self.0,
                                        ))?
                                        .ok_or_else(|| {
                                            de::Error::custom("missing replaced value")
                                        })?,
                                })
                            }
                        }
                        let body =
                            variant.struct_variant(&["allocation", "replaced"], Fields(self.0))?;
                        Ok(EventKind::Allocate {
                            allocation: body.allocation,
                            replaced: body.replaced,
                        })
                    }
                    1 => {
                        struct Fields<'a, 'b>(&'a mut Context<'b>);
                        impl<'de> Visitor<'de> for Fields<'_, '_> {
                            type Value = FreeBody;
                            fn expecting(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
                                fmt.write_str("an optional freed value")
                            }
                            fn visit_seq<A: SeqAccess<'de>>(
                                self,
                                mut input: A,
                            ) -> Result<FreeBody, A::Error> {
                                Ok(FreeBody {
                                    allocation: input
                                        .next_element_seed(Seed::<Option<ResidentValue>>::new(
                                            self.0,
                                        ))?
                                        .ok_or_else(|| de::Error::custom("missing freed value"))?,
                                })
                            }
                        }
                        let body = variant.struct_variant(&["allocation"], Fields(self.0))?;
                        Ok(EventKind::Free {
                            allocation: body.allocation,
                        })
                    }
                    _ => Err(de::Error::custom("unknown resident memory event variant")),
                }
            }
        }
        deserializer.deserialize_enum("MemoryEventKind", &["Allocate", "Free"], Kind(context))
    }
}
structure!(EventData {
    buffer: u64,
    address: u64,
    received_frame: usize,
    kind: EventKind
});
impl Decode for EventRef {
    fn decode<'de, D: serde::Deserializer<'de>>(
        context: &mut Context<'_>,
        deserializer: D,
    ) -> Result<Self, D::Error> {
        Owned::<EventData>::decode(context, deserializer).map(Self)
    }
}
structure!(MemoryData {
    live: ResidentMap, samples: Buffer<MemorySample>, live_bytes: u64, peak_bytes: u64,
    total_allocated_bytes: u64, allocations: u64, frees: u64, unmatched_frees: u64,
    replaced_addresses: u64, strings: Tree<(u64, u32), Text>, events: Buffer<EventRef>,
    base: StateRef, first_retained_frame: usize,
});
structure!(Continuation {
    first_retained_frame: usize, zones: Tree<u64, ZoneData>, spans: Buffer<ProfileSpan>,
    values: Buffer<ProfileValue>, unmatched_stops: u64,
    unmatched_stop_events: Tree<(u64, u16, u32, u64), u64>, legacy_cuda_buffers: u64,
    memory: Owned<MemoryData>, defined_zones: Set<u64>, ambiguous_zones: Set<u64>,
    ambiguous_events: Set<(u64, u16)>, starts: Tree<(u64, u16, u32, u64), Buffer<u64>>,
    pending_starts: usize, budget: Budget,
});
