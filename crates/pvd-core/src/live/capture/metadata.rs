use super::*;

const LIMIT: usize = 8 << 20;

pub(super) fn encode(
    source: &crate::Recording,
    range: CaptureRange,
    budget: &mut Budget,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<Vec<u8>, Error> {
    let mut measure = Encoder {
        out: std::io::sink(),
        bytes: 0,
    };
    measure.recording(source, range, cancelled)?;
    budget.charge(measure.bytes)?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(measure.bytes)?;
    budget.charge(bytes.capacity() - measure.bytes)?;
    let mut encoder = Encoder {
        out: bytes,
        bytes: 0,
    };
    encoder.recording(source, range, cancelled)?;
    Ok(encoder.out)
}

struct Encoder<W> {
    out: W,
    bytes: usize,
}
impl<W: Write> Encoder<W> {
    fn put(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .filter(|n| *n <= LIMIT)
            .ok_or_else(|| error("capture metadata exceeds 8 MiB"))?;
        self.out.write_all(bytes)?;
        Ok(())
    }
    fn number(&mut self, n: u64) -> Result<(), Error> {
        self.put(&n.to_le_bytes())
    }
    fn text(&mut self, text: &str) -> Result<(), Error> {
        self.number(text.len() as u64)?;
        self.put(text.as_bytes())
    }
    fn recording(
        &mut self,
        source: &crate::Recording,
        range: CaptureRange,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), Error> {
        for number in [
            if source.rooms.is_empty() { 1 } else { 2 },
            range.first as u64,
            range.last as u64,
            u64::from(source.header.version),
            source.header.timestamp_numerator,
            source.header.timestamp_denominator,
            u64::from(source.header.flags),
            u64::from(source.header.endian == pvd_protocol::Endian::Little),
            source.classes.len() as u64,
        ] {
            self.number(number)?;
        }
        for (name, class) in &source.classes {
            check_replay_cancelled(cancelled)?;
            self.text(name)?;
            self.number(u64::from(class.parent.is_some()))?;
            if let Some(parent) = &class.parent {
                self.text(parent)?;
            }
            self.number(class.properties.len() as u64)?;
            for (name, property) in &class.properties {
                check_replay_cancelled(cancelled)?;
                self.text(name)?;
                self.text(&property.datatype)?;
                self.text(&property.semantic)?;
                self.number(u64::from(property.array))?;
                self.number(property.named_values.len() as u64)?;
                for (name, value) in &property.named_values {
                    self.text(name)?;
                    self.number(u64::from(*value))?;
                }
            }
        }
        self.number(source.diagnostics.len() as u64)?;
        for text in &source.diagnostics {
            check_replay_cancelled(cancelled)?;
            self.text(text)?;
        }
        if !source.rooms.is_empty() {
            let include_tail = range.last.checked_add(1) == Some(source.frame_count());
            let entries = || {
                source
                    .rooms
                    .iter()
                    .filter(|room| include_tail || room.first_observed_frame <= range.last)
            };
            self.number(if include_tail {
                source.rooms.observed_count()
            } else {
                entries().map(|room| room.number).max().unwrap_or(0)
            })?;
            self.number(entries().count() as u64)?;
            for room in entries() {
                check_replay_cancelled(cancelled)?;
                for number in [
                    room.scene.id,
                    u64::from(room.scene.generation),
                    room.number,
                    room.first_observed_frame as u64,
                    room.destroyed_at_frame
                        .filter(|frame| include_tail || *frame <= range.last)
                        .map_or(u64::MAX, |frame| frame as u64),
                ] {
                    self.number(number)?;
                }
            }
        }
        Ok(())
    }
}

fn text(input: &mut wire::Input<'_>, budget: &mut Budget) -> Result<String, Error> {
    let len = input.count(LIMIT, 1)?;
    let bytes = input.take(len)?;
    let source = std::str::from_utf8(bytes)?;
    budget.charge(
        len.checked_add(64)
            .ok_or_else(|| error("capture text size overflow"))?,
    )?;
    let mut value = String::new();
    value.try_reserve_exact(len)?;
    budget.charge(value.capacity() - len)?;
    value.push_str(source);
    Ok(value)
}
fn flag(input: &mut wire::Input<'_>) -> Result<bool, Error> {
    match input.u64()? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(error("invalid capture metadata flag")),
    }
}
pub(super) fn decode(
    bytes: &[u8],
    budget: &mut Budget,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<Metadata, Error> {
    let mut input = wire::Input::new(bytes);
    let version = u32::try_from(input.u64()?)?;
    let first = input.u64()?;
    let last = input.u64()?;
    let sdk_version = u32::try_from(input.u64()?)?;
    let timestamp_numerator = input.u64()?;
    let timestamp_denominator = input.u64()?;
    let flags = u32::try_from(input.u64()?)?;
    let little_endian = flag(&mut input)?;
    if !matches!(version, 1 | 2) || timestamp_denominator == 0 || first > last || last == u64::MAX {
        return Err(error("invalid capture metadata version or frame bounds"));
    }
    let count = input.count(LIMIT / 24, 24)?;
    budget.entries(count, 256)?;
    let mut classes = BTreeMap::new();
    for _ in 0..count {
        check_replay_cancelled(cancelled)?;
        let name = text(&mut input, budget)?;
        let parent = if flag(&mut input)? {
            Some(text(&mut input, budget)?)
        } else {
            None
        };
        let count = input.count(LIMIT / 40, 40)?;
        budget.entries(count, 320)?;
        let mut properties = BTreeMap::new();
        for _ in 0..count {
            check_replay_cancelled(cancelled)?;
            let name = text(&mut input, budget)?;
            let datatype = text(&mut input, budget)?;
            let semantic = text(&mut input, budget)?;
            let array = flag(&mut input)?;
            let count = input.count(LIMIT / 16, 16)?;
            budget.entries(count, size_of::<(String, u32)>())?;
            let mut named_values = Vec::new();
            named_values.try_reserve_exact(count)?;
            budget.entries(named_values.capacity() - count, size_of::<(String, u32)>())?;
            for _ in 0..count {
                named_values.push((text(&mut input, budget)?, u32::try_from(input.u64()?)?));
            }
            if properties
                .insert(
                    name,
                    crate::PropertyDefinition {
                        datatype,
                        semantic,
                        array,
                        named_values,
                    },
                )
                .is_some()
            {
                return Err(error("duplicate capture property metadata"));
            }
        }
        if classes
            .insert(name, crate::ClassDefinition { parent, properties })
            .is_some()
        {
            return Err(error("duplicate capture class metadata"));
        }
    }
    let count = input.count(LIMIT / 8, 8)?;
    budget.entries(count, size_of::<String>())?;
    let mut diagnostics = Vec::new();
    diagnostics.try_reserve_exact(count)?;
    budget.entries(diagnostics.capacity() - count, size_of::<String>())?;
    for _ in 0..count {
        check_replay_cancelled(cancelled)?;
        diagnostics.push(text(&mut input, budget)?);
    }
    let rooms = if version == 2 {
        let observed = input.u64()?;
        let count = input.count(crate::rooms::ROOM_LIMIT, 40)?;
        budget.entries(count, 256 + size_of::<crate::rooms::RoomInfo>())?;
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            check_replay_cancelled(cancelled)?;
            let scene = crate::frame_sources::FrameObject {
                id: input.u64()?,
                generation: u32::try_from(input.u64()?)?,
            };
            let number = input.u64()?;
            let first_observed_frame = input.usize()?;
            let destroyed = input.u64()?;
            if first_observed_frame as u64 > last + 1
                || (destroyed != u64::MAX && destroyed > last + 1)
            {
                return Err(error("capture room lifecycle exceeds its history endpoint"));
            }
            entries.push(crate::rooms::RoomInfo {
                scene,
                number,
                first_observed_frame,
                destroyed_at_frame: if destroyed == u64::MAX {
                    None
                } else {
                    Some(usize::try_from(destroyed)?)
                },
            });
        }
        Some(crate::rooms::RoomDirectory::from_entries(
            entries, observed,
        )?)
    } else {
        None
    };
    input.finish()?;
    Ok(Metadata {
        first,
        last,
        sdk_version,
        timestamp_numerator,
        timestamp_denominator,
        flags,
        little_endian,
        classes,
        diagnostics,
        rooms,
    })
}
