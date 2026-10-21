use super::*;

const MAX_ORIGINS: usize = 64;

pub(in super::super) struct IdentityDictionary {
    origins: [[u8; 16]; MAX_ORIGINS],
    len: usize,
}

impl IdentityDictionary {
    pub fn new() -> Self {
        Self {
            origins: [[0; 16]; MAX_ORIGINS],
            len: 0,
        }
    }

    fn index(&self, origin: &[u8; 16]) -> Option<usize> {
        self.origins[..self.len]
            .iter()
            .position(|item| item == origin)
    }

    pub fn insert(&mut self, value: identity::WireIdentity) -> Result<(), Error> {
        value.validate()?;
        if self.index(&value.origin).is_none() {
            if self.len == MAX_ORIGINS {
                return Err(error("identity page exceeds its source count limit"));
            }
            self.origins[self.len] = value.origin;
            self.len += 1;
        }
        Ok(())
    }

    pub fn encoded_bytes(&self, rows: usize) -> Result<usize, Error> {
        if self.len == 0 || self.len > rows {
            return Err(error("identity source count disagrees with page rows"));
        }
        add(
            1 + self.len * 16,
            rows.checked_mul(9)
                .ok_or_else(|| error("identity page size overflow"))?,
        )
    }

    pub fn is_smaller(&self, rows: usize) -> Result<bool, Error> {
        Ok(self.encoded_bytes(rows)?
            < rows
                .checked_mul(24)
                .ok_or_else(|| error("identity page size overflow"))?)
    }

    pub fn write(&self, output: &mut Encoder) {
        output.u8(self.len as u8);
        for origin in &self.origins[..self.len] {
            output.bytes.extend_from_slice(origin);
        }
    }

    pub fn write_row(
        &self,
        output: &mut Encoder,
        value: identity::WireIdentity,
    ) -> Result<(), Error> {
        value.validate()?;
        let index = self
            .index(&value.origin)
            .ok_or_else(|| error("identity row source is absent from its page"))?;
        output.u8(index as u8);
        output.u64(value.sequence);
        Ok(())
    }

    pub fn read(input: &mut Decoder<'_>, rows: usize) -> Result<Self, Error> {
        let count = usize::from(input.u8()?);
        if count == 0 || count > MAX_ORIGINS || count > rows {
            return Err(error("identity page has an invalid source count"));
        }
        let mut result = Self::new();
        for _ in 0..count {
            let origin: [u8; 16] = input.take(16)?.try_into()?;
            if origin == [0; 16] || result.index(&origin).is_some() {
                return Err(error("identity page source is empty or duplicated"));
            }
            result.origins[result.len] = origin;
            result.len += 1;
        }
        Ok(result)
    }

    pub fn read_row(&self, input: &mut Decoder<'_>) -> Result<identity::WireIdentity, Error> {
        let index = usize::from(input.u8()?);
        let origin = *self.origins[..self.len]
            .get(index)
            .ok_or_else(|| error("identity row source index is out of range"))?;
        let value = identity::WireIdentity {
            origin,
            sequence: input.u64()?,
        };
        value.validate()?;
        Ok(value)
    }
}

#[cfg(test)]
#[cfg(test)]
mod tests;
