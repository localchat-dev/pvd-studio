use super::*;

mod sealed {
    pub trait Sealed {}
    impl Sealed for u64 {}
    impl Sealed for super::ProfileSpan {}
    impl Sealed for super::ProfileValue {}
    impl Sealed for super::MemorySample {}
    impl Sealed for super::EventRef {}
}

pub(in super::super::super) trait BufferCodec:
    sealed::Sealed + Item + Clone
{
    const KIND: u8;
    const WIDTH: usize;
    fn encode<C: PageWrite>(
        values: &[Self],
        out: &mut Encoder,
        cx: &mut C,
    ) -> Result<Dependencies, Error>;
    fn decode<C: PageRead>(
        input: &mut Decoder<'_>,
        count: usize,
        cx: &mut C,
    ) -> Result<(Owned<Vec<Self>>, Dependencies), Error>;
}

macro_rules! plain_codec {
    ($ty:ty, $kind:literal, $width:literal, |$value:ident, $out:ident| $encode:block, |$input:ident| $decode:expr) => {
        impl BufferCodec for $ty {
            const KIND: u8 = $kind;
            const WIDTH: usize = $width;
            fn encode<C: PageWrite>(
                values: &[Self],
                $out: &mut Encoder,
                cx: &mut C,
            ) -> Result<Dependencies, Error> {
                for $value in values {
                    cx.check()?;
                    $encode
                }
                Ok(Dependencies::new())
            }
            fn decode<C: PageRead>(
                $input: &mut Decoder<'_>,
                count: usize,
                cx: &mut C,
            ) -> Result<(Owned<Vec<Self>>, Dependencies), Error> {
                let mut result = Tail::new(cx.domain(), count)?;
                for _ in 0..count {
                    cx.check()?;
                    result.values.push($decode);
                }
                Ok((result.finish()?, Dependencies::new()))
            }
        }
    };
}

plain_codec!(
    u64,
    1,
    8,
    |value, out| {
        out.u64(*value);
    },
    |input| input.u64()?
);
plain_codec!(
    ProfileSpan,
    2,
    46,
    |value, out| {
        out.u64(value.zone);
        out.u16(value.event);
        out.u32(value.thread);
        out.u64(value.context);
        out.u64(value.start);
        out.u64(value.end);
        out.u64(value.received_frame.try_into()?);
    },
    |input| ProfileSpan {
        zone: input.u64()?,
        event: input.u16()?,
        thread: input.u32()?,
        context: input.u64()?,
        start: input.u64()?,
        end: input.u64()?,
        received_frame: input.u64()?.try_into()?,
    }
);
plain_codec!(
    ProfileValue,
    3,
    38,
    |value, out| {
        out.u64(value.zone);
        out.u16(value.event);
        out.u32(value.thread);
        out.u64(value.context);
        out.u64(value.raw_value);
        out.u64(value.received_frame.try_into()?);
    },
    |input| ProfileValue {
        zone: input.u64()?,
        event: input.u16()?,
        thread: input.u32()?,
        context: input.u64()?,
        raw_value: input.u64()?,
        received_frame: input.u64()?.try_into()?,
    }
);
plain_codec!(
    MemorySample,
    4,
    40,
    |value, out| {
        out.u64(value.received_frame.try_into()?);
        out.u64(value.live_bytes);
        out.u64(value.live_allocations.try_into()?);
        out.u64(value.allocations);
        out.u64(value.frees);
    },
    |input| MemorySample {
        received_frame: input.u64()?.try_into()?,
        live_bytes: input.u64()?,
        live_allocations: input.u64()?.try_into()?,
        allocations: input.u64()?,
        frees: input.u64()?,
    }
);

const EMPTY_ROW: RowRef = RowRef { page: 0, row: 0 };

#[derive(Clone, Copy)]
struct EventRow {
    buffer: u64,
    address: u64,
    frame: usize,
    kind: u8,
    allocation: RowRef,
    replaced: RowRef,
}
const EMPTY_EVENT: EventRow = EventRow {
    buffer: 0,
    address: 0,
    frame: 0,
    kind: 0,
    allocation: EMPTY_ROW,
    replaced: EMPTY_ROW,
};

fn optional_row(input: &mut Decoder<'_>) -> Result<RowRef, Error> {
    let row = RowRef {
        page: input.u64()?,
        row: input.u32()?,
    };
    if row.page == 0 && row.row != 0 {
        return Err(error("empty sequence value reference has a row"));
    }
    Ok(row)
}

impl BufferCodec for EventRef {
    const KIND: u8 = 5;
    const WIDTH: usize = 49;

    fn encode<C: PageWrite>(
        values: &[Self],
        out: &mut Encoder,
        cx: &mut C,
    ) -> Result<Dependencies, Error> {
        let mut sources = Vec::new();
        sources.try_reserve_exact(MAX_DEPENDENCIES)?;
        if sources.capacity() != MAX_DEPENDENCIES {
            return Err(error("sequence value batch exceeds its scratch allowance"));
        }
        for event in values {
            cx.check()?;
            if !event.same_domain(cx.domain()) {
                return Err(error("sequence event belongs to another domain"));
            }
            match &event.kind {
                EventKind::Allocate {
                    allocation,
                    replaced,
                } => {
                    sources.push(allocation.clone());
                    if let Some(value) = replaced {
                        sources.push(value.clone());
                    }
                }
                EventKind::Free {
                    allocation: Some(value),
                } => sources.push(value.clone()),
                EventKind::Free { allocation: None } => {}
            }
        }
        let references = cx.values(&sources)?;
        if references.len() != sources.len()
            || references.capacity() > MAX_DEPENDENCIES
            || references.iter().any(|reference| reference.page == 0)
        {
            return Err(error("sequence value batch returned invalid references"));
        }
        let mut dependencies = Dependencies::new();
        for reference in &references {
            dependencies.add(reference.page)?;
        }
        let mut index = 0;
        for event in values {
            cx.check()?;
            out.u64(event.buffer);
            out.u64(event.address);
            out.u64(event.received_frame.try_into()?);
            let (kind, allocation, replaced) = match &event.kind {
                EventKind::Allocate { replaced, .. } => {
                    let allocation = references[index];
                    index += 1;
                    let replaced = if replaced.is_some() {
                        let reference = references[index];
                        index += 1;
                        reference
                    } else {
                        EMPTY_ROW
                    };
                    (0, allocation, replaced)
                }
                EventKind::Free { allocation } => {
                    let allocation = if allocation.is_some() {
                        let reference = references[index];
                        index += 1;
                        reference
                    } else {
                        EMPTY_ROW
                    };
                    (1, allocation, EMPTY_ROW)
                }
            };
            out.u8(kind);
            out.row(allocation);
            out.row(replaced);
        }
        if index != references.len() {
            return Err(error("sequence value batch was not consumed"));
        }
        Ok(dependencies)
    }

    fn decode<C: PageRead>(
        input: &mut Decoder<'_>,
        count: usize,
        cx: &mut C,
    ) -> Result<(Owned<Vec<Self>>, Dependencies), Error> {
        let mut rows = [EMPTY_EVENT; CHUNK_LIMIT];
        let mut references = [EMPTY_ROW; MAX_DEPENDENCIES];
        let mut reference_count = 0;
        let mut dependencies = Dependencies::new();
        for row in &mut rows[..count] {
            cx.check()?;
            *row = EventRow {
                buffer: input.u64()?,
                address: input.u64()?,
                frame: input.u64()?.try_into()?,
                kind: input.u8()?,
                allocation: optional_row(input)?,
                replaced: optional_row(input)?,
            };
            match row.kind {
                0 if row.allocation.page != 0 => {}
                1 if row.replaced.page == 0 => {}
                _ => return Err(error("invalid sequence memory event kind or reference")),
            }
            for reference in [row.allocation, row.replaced] {
                if reference.page != 0 {
                    references[reference_count] = reference;
                    reference_count += 1;
                    dependencies.add(reference.page)?;
                }
            }
        }
        let values = cx.values(&references[..reference_count])?;
        if values.len() != reference_count
            || !values.domain().same(cx.domain())
            || values
                .iter()
                .any(|value| !value.0.domain().same(cx.domain()))
        {
            return Err(error(
                "sequence values belong to another domain or have the wrong count",
            ));
        }
        let mut result = Tail::new(cx.domain(), count)?;
        let mut index = 0;
        for row in &rows[..count] {
            cx.check()?;
            let allocation = if row.allocation.page != 0 {
                let value = values[index].clone();
                index += 1;
                Some(value)
            } else {
                None
            };
            let replaced = if row.replaced.page != 0 {
                let value = values[index].clone();
                index += 1;
                Some(value)
            } else {
                None
            };
            let kind = match row.kind {
                0 => EventKind::Allocate {
                    allocation: allocation.unwrap(),
                    replaced,
                },
                1 => EventKind::Free { allocation },
                _ => return Err(error("invalid sequence memory event kind")),
            };
            result.values.push(EventRef(Owned::new(
                EventData {
                    buffer: row.buffer,
                    address: row.address,
                    received_frame: row.frame,
                    kind,
                },
                cx.domain(),
                0,
            )?));
        }
        Ok((result.finish()?, dependencies))
    }
}
