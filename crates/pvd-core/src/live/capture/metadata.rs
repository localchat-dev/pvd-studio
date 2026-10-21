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
            1,
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
    if version != 1 || timestamp_denominator == 0 || first > last || last == u64::MAX {
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
    })
}
