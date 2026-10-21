use crate::{Error, error};
use pvd_protocol::{Endian, Header};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Contact {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub shapes: [u64; 2],
    pub separation: f32,
    pub normal_force: f32,
    pub faces: [u32; 2],
    pub normal_force_available: bool,
}
pub(crate) fn decode(data: &[u8], items: u32, header: &Header) -> Result<Vec<Contact>, Error> {
    let pointer = if header.flags & 1 != 0 { 8 } else { 4 };
    let stride = if pointer == 8 { 64 } else { 52 };
    if items > 1_000_000 || data.len() != items as usize * stride {
        return Err(error(
            "PvdContact array length does not match producer pointer width",
        ));
    }
    let uint = |b: &[u8]| {
        let mut bytes = [0u8; 8];
        match header.endian {
            Endian::Little => {
                bytes[..b.len()].copy_from_slice(b);
                u64::from_le_bytes(bytes)
            }
            Endian::Big => {
                bytes[8 - b.len()..].copy_from_slice(b);
                u64::from_be_bytes(bytes)
            }
        }
    };
    let mut result = Vec::with_capacity(items as usize);
    for b in data.chunks_exact(stride) {
        let f = |i: usize| f32::from_bits(uint(&b[i..i + 4]) as u32);
        let tail = 24 + 2 * pointer;
        result.push(Contact {
            position: [f(0), f(4), f(8)],
            normal: [f(12), f(16), f(20)],
            shapes: [uint(&b[24..24 + pointer]), uint(&b[24 + pointer..tail])],
            separation: f(tail),
            normal_force: f(tail + 4),
            faces: [
                uint(&b[tail + 8..tail + 12]) as u32,
                uint(&b[tail + 12..tail + 16]) as u32,
            ],
            normal_force_available: b[tail + 16] != 0,
        });
    }
    Ok(result)
}
