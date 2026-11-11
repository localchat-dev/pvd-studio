use super::{BLOCK_BYTES, Error, Limits, Member, Reader, check_progress, error};
use flate2::{Decompress, FlushDecompress, Status};
use std::io::{Read, Seek, SeekFrom};

pub(super) fn read<R: Read + Seek>(
    reader: &mut Reader<'_, R>,
    member: &Member,
    limits: &Limits,
    progress: &mut dyn FnMut(u64, u64) -> bool,
) -> Result<Vec<u8>, Error> {
    let expected = u64::from(member.uncompressed);
    if expected > limits.max_member_bytes {
        return Err(error("PVD archive recording exceeds the output limit"));
    }
    check_progress(progress, 0, expected)?;
    reader.source.seek(SeekFrom::Start(member.offset))?;
    let mut output = Vec::new();
    let mut checksum = crc32fast::Hasher::new();
    let mut input = [0; BLOCK_BYTES];
    let mut remaining = u64::from(member.compressed);
    if member.method == 0 {
        while remaining != 0 {
            check_progress(progress, output.len() as u64, expected)?;
            let count = remaining.min(BLOCK_BYTES as u64) as usize;
            reader.source.read_exact(&mut input[..count])?;
            append(&mut output, &input[..count], expected)?;
            checksum.update(&input[..count]);
            remaining -= count as u64;
        }
    } else {
        let mut decoder = Decompress::new(false);
        let mut buffer = [0; BLOCK_BYTES];
        let mut position = 0;
        let mut available = 0;
        loop {
            check_progress(progress, output.len() as u64, expected)?;
            if position == available && remaining != 0 {
                available = remaining.min(BLOCK_BYTES as u64) as usize;
                reader.source.read_exact(&mut input[..available])?;
                remaining -= available as u64;
                position = 0;
            }
            let capacity = (expected - output.len() as u64 + 1).min(BLOCK_BYTES as u64) as usize;
            let input_before = decoder.total_in();
            let output_before = decoder.total_out();
            let status = decoder.decompress(
                &input[position..available],
                &mut buffer[..capacity],
                FlushDecompress::None,
            )?;
            let consumed = (decoder.total_in() - input_before) as usize;
            let produced = (decoder.total_out() - output_before) as usize;
            position += consumed;
            append(&mut output, &buffer[..produced], expected)?;
            checksum.update(&buffer[..produced]);
            if status == Status::StreamEnd {
                if position != available || remaining != 0 {
                    return Err(error(
                        "PVD archive deflate member has trailing compressed bytes",
                    ));
                }
                break;
            }
            if consumed == 0 && produced == 0 {
                return Err(error("PVD archive deflate member is truncated or invalid"));
            }
        }
    }
    if output.len() as u64 != expected {
        return Err(error("PVD archive decoded size differs from its directory"));
    }
    if checksum.finalize() != member.crc {
        return Err(error("PVD archive recording CRC mismatch"));
    }
    check_progress(progress, output.len() as u64, expected)?;
    Ok(output)
}

fn append(output: &mut Vec<u8>, bytes: &[u8], expected: u64) -> Result<(), Error> {
    let length = output.len() as u64 + bytes.len() as u64;
    if length > expected {
        return Err(error("PVD archive actual output exceeds its declared size"));
    }
    if length as usize > output.capacity() {
        let capacity = (output.capacity() as u64)
            .saturating_mul(2)
            .max(BLOCK_BYTES as u64)
            .max(length)
            .min(expected) as usize;
        output.try_reserve_exact(capacity - output.len())?;
    }
    output.extend_from_slice(bytes);
    Ok(())
}
