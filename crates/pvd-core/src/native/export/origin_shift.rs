use super::*;

mod layout;
pub(super) use layout::fixed_layout;

#[derive(Clone, Debug)]
pub(super) struct Event<'a> {
    id: u32,
    shift_bits: [u32; 3],
    shifted_properties: &'a [u8],
}

impl<'a> Event<'a> {
    pub(super) fn parse(reader: &mut Reader<'a>) -> Result<Self, Error> {
        let id = reader.u32()?;
        let shift_bits = [reader.u32()?, reader.u32()?, reader.u32()?];
        if shift_bits
            .iter()
            .any(|&bits| !f32::from_bits(bits).is_finite())
        {
            return Err(error("OriginShift displacement must be finite"));
        }
        let bytes = reader
            .count()?
            .checked_mul(8)
            .ok_or_else(|| error("OriginShift property count overflow"))?;
        let shifted_properties = reader.take(bytes)?;
        Ok(Self {
            id,
            shift_bits,
            shifted_properties,
        })
    }

    pub(super) fn scene(&self) -> u32 {
        self.id
    }

    pub(super) fn properties(&self) -> &[u8] {
        self.shifted_properties
    }

    pub(super) fn displacement_bits(&self) -> [u32; 3] {
        self.shift_bits
    }

    pub(super) fn digest(
        &self,
        hash: &mut blake3::Hasher,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        check(proceed)?;
        let length = self
            .shifted_properties
            .len()
            .checked_add(24)
            .ok_or_else(|| error("OriginShift digest length overflow"))?;
        hash.update(&(length as u64).to_le_bytes());
        for word in [
            11,
            self.id,
            self.shift_bits[0],
            self.shift_bits[1],
            self.shift_bits[2],
            u32_len(self.shifted_properties.len() / 8)?,
        ] {
            hash.update(&word.to_le_bytes());
        }
        for chunk in self.shifted_properties.chunks(64 << 10) {
            check(proceed)?;
            hash.update(chunk);
        }
        Ok(())
    }

    pub(super) fn encode(&self, output: &mut Vec<u8>) -> Result<(), Error> {
        w32(output, 11);
        w32(output, self.id);
        for bits in self.shift_bits {
            w32(output, bits);
        }
        w32(output, u32_len(self.shifted_properties.len() / 8)?);
        output.extend_from_slice(self.shifted_properties);
        Ok(())
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod prefix_digest_tests;

#[cfg(test)]
pub(super) mod layout_tests;
