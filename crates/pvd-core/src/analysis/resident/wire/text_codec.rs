use super::*;

const EMPTY: u16 = u16::MAX;
const SLOTS: usize = MAX_STRING_ROWS * 2;
pub(in super::super) const TEXT_READ_SCRATCH: usize =
    size_of::<[(usize, usize); MAX_STRING_ROWS]>();

pub(in super::super) struct TextDictionary {
    slots: [u16; SLOTS],
    unique: [u16; MAX_STRING_ROWS],
    rows: [u16; MAX_STRING_ROWS],
    count: usize,
    unique_count: usize,
    original_bytes: usize,
    encoded_bytes: usize,
}

impl TextDictionary {
    pub fn new<'a>(
        count: usize,
        text: impl Fn(usize) -> &'a str,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Self, Error> {
        if count == 0 || count > MAX_STRING_ROWS {
            return Err(error("text dictionary row count exceeds its limit"));
        }
        let mut result = Self {
            slots: [EMPTY; SLOTS],
            unique: [0; MAX_STRING_ROWS],
            rows: [0; MAX_STRING_ROWS],
            count,
            unique_count: 0,
            original_bytes: 0,
            encoded_bytes: add(2, count * 2)?,
        };
        for row in 0..count {
            check_replay_cancelled(cancelled)?;
            let value = text(row);
            if value.len() > MAX_STRING_BYTES {
                return Err(error("text dictionary value exceeds its byte limit"));
            }
            result.original_bytes = add(result.original_bytes, add(4, value.len())?)?;
            let hash = blake3::hash(value.as_bytes());
            let mut slot = u64::from_le_bytes(hash.as_bytes()[..8].try_into()?) as usize % SLOTS;
            loop {
                check_replay_cancelled(cancelled)?;
                let entry = result.slots[slot];
                if entry == EMPTY {
                    let index = result.unique_count;
                    result.unique[index] = row.try_into()?;
                    result.unique_count += 1;
                    result.encoded_bytes = add(result.encoded_bytes, add(4, value.len())?)?;
                    result.slots[slot] = index.try_into()?;
                    result.rows[row] = index.try_into()?;
                    break;
                }
                if text(usize::from(result.unique[usize::from(entry)])) == value {
                    result.rows[row] = entry;
                    break;
                }
                slot = (slot + 1) % SLOTS;
            }
        }
        Ok(result)
    }

    pub fn original_bytes(&self) -> usize {
        self.original_bytes
    }
    pub fn encoded_bytes(&self) -> usize {
        self.encoded_bytes
    }
    pub fn is_smaller(&self) -> bool {
        self.encoded_bytes < self.original_bytes
    }
    pub fn write<'a>(
        &self,
        output: &mut Encoder,
        text: impl Fn(usize) -> &'a str,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), Error> {
        output.u16(self.unique_count.try_into()?);
        for source in &self.unique[..self.unique_count] {
            check_replay_cancelled(cancelled)?;
            let value = text(usize::from(*source));
            output.u32(value.len().try_into()?);
            output.bytes.extend_from_slice(value.as_bytes());
        }
        for row in &self.rows[..self.count] {
            check_replay_cancelled(cancelled)?;
            output.u16(*row);
        }
        Ok(())
    }
}

pub(in super::super) fn read_text_ranges(
    input: &mut Decoder<'_>,
    count: usize,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<Vec<std::ops::Range<usize>>, Error> {
    if count == 0 || count > MAX_STRING_ROWS {
        return Err(error("text dictionary row count exceeds its limit"));
    }
    let unique_count = input.count(count)?;
    let mut dictionary = [(0usize, 0usize); MAX_STRING_ROWS];
    for range in &mut dictionary[..unique_count] {
        check_replay_cancelled(cancelled)?;
        let size = usize::try_from(input.u32()?)?;
        if size > MAX_STRING_BYTES {
            return Err(error("text dictionary value exceeds its byte limit"));
        }
        let start = input.offset;
        std::str::from_utf8(input.take(size)?)?;
        *range = (start, input.offset);
    }
    let mut ranges = Vec::new();
    ranges.try_reserve_exact(count)?;
    for _ in 0..count {
        check_replay_cancelled(cancelled)?;
        let index = usize::from(input.u16()?);
        let (start, end) = *dictionary[..unique_count]
            .get(index)
            .ok_or_else(|| error("text dictionary row index is out of range"))?;
        ranges.push(start..end);
    }
    Ok(ranges)
}

#[cfg(test)]
mod tests;
