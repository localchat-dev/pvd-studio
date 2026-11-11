use super::{Error, Limits, Member, Reader, check_progress, error, unsupported};
use std::collections::BTreeSet;
use std::io::{Read, Seek};

const LOCAL: u32 = 0x0403_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const END: u32 = 0x0605_4b50;
const DESCRIPTOR: u32 = 0x0807_4b50;
const MAX_NAME_BYTES: usize = 259;

struct Entry {
    name: String,
    version: u16,
    flags: u16,
    method: u16,
    timestamp: u32,
    crc: u32,
    compressed: u32,
    uncompressed: u32,
    local_offset: u32,
    regular: bool,
}

pub(super) fn select<R: Read + Seek>(
    reader: &mut Reader<'_, R>,
    limits: &Limits,
    progress: &mut dyn FnMut(u64, u64) -> bool,
) -> Result<Member, Error> {
    let tail_length = reader.length.min(22 + u64::from(u16::MAX)) as usize;
    let mut tail = vec![0; tail_length];
    reader.read_at(reader.length - tail_length as u64, &mut tail, progress)?;
    let end = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&offset| {
            u32_at(&tail, offset) == END
                && offset + 22 + usize::from(u16_at(&tail, offset + 20)) == tail.len()
        })
        .ok_or_else(|| error("PVD archive has no complete ZIP end record"))?;
    if u16_at(&tail, end + 4) != 0 || u16_at(&tail, end + 6) != 0 {
        return Err(unsupported("PVD archive split ZIP files are unsupported"));
    }
    let count = u16_at(&tail, end + 10);
    let directory_bytes = u32_at(&tail, end + 12);
    let directory_offset = u32_at(&tail, end + 16);
    if count == u16::MAX || directory_bytes == u32::MAX || directory_offset == u32::MAX {
        return Err(unsupported("PVD archive ZIP64 is unsupported"));
    }
    if u16_at(&tail, end + 8) != count {
        return Err(error("PVD archive ZIP entry counts disagree"));
    }
    if usize::from(count) > limits.max_entries {
        return Err(error("PVD archive exceeds the entry count limit"));
    }
    if u64::from(directory_bytes) > limits.max_directory_bytes {
        return Err(error("PVD archive exceeds the directory limit"));
    }
    let end_offset = reader.length - tail_length as u64 + end as u64;
    if u64::from(directory_offset) + u64::from(directory_bytes) != end_offset {
        return Err(error(
            "PVD archive directory does not end at its end record",
        ));
    }
    drop(tail);
    let mut directory = vec![0; directory_bytes as usize];
    reader.read_at(u64::from(directory_offset), &mut directory, progress)?;
    let mut entries = Vec::new();
    entries.try_reserve_exact(usize::from(count))?;
    let mut names = BTreeSet::new();
    let mut selected = None;
    let mut position = 0;
    for _ in 0..count {
        check_progress(progress, 0, 0)?;
        let header = directory
            .get(position..position + 46)
            .ok_or_else(|| error("PVD archive central header is truncated"))?;
        if u32_at(header, 0) != CENTRAL {
            return Err(error("PVD archive central header signature is invalid"));
        }
        let name_length = usize::from(u16_at(header, 28));
        let extra_length = usize::from(u16_at(header, 30));
        let comment_length = usize::from(u16_at(header, 32));
        let next = position + 46 + name_length + extra_length + comment_length;
        let variable = directory
            .get(position + 46..next)
            .ok_or_else(|| error("PVD archive central entry is truncated"))?;
        let name = parse_name(&variable[..name_length])?;
        if !names.insert(name.to_ascii_lowercase()) {
            return Err(unsupported("PVD archive member names are ambiguous"));
        }
        check_extra(&variable[name_length..name_length + extra_length])?;
        let version = u16_at(header, 6);
        let flags = u16_at(header, 8);
        let method = u16_at(header, 10);
        if !(10..=20).contains(&version) || (version < 20 && (method == 8 || flags & 8 != 0)) {
            return Err(unsupported(
                "PVD archive requires an unsupported ZIP version",
            ));
        }
        if flags & !0x080e != 0 {
            return Err(unsupported(
                "PVD archive encryption or ZIP flags are unsupported",
            ));
        }
        if !matches!(method, 0 | 8) || (method == 0 && flags & 6 != 0) {
            return Err(unsupported("PVD archive compression method is unsupported"));
        }
        if u16_at(header, 34) != 0 {
            return Err(unsupported("PVD archive split ZIP members are unsupported"));
        }
        let compressed = u32_at(header, 20);
        let uncompressed = u32_at(header, 24);
        let local_offset = u32_at(header, 42);
        if [compressed, uncompressed, local_offset].contains(&u32::MAX) {
            return Err(unsupported("PVD archive ZIP64 member is unsupported"));
        }
        if method == 0 && compressed != uncompressed {
            return Err(error("PVD archive stored member sizes disagree"));
        }
        let external = u32_at(header, 38);
        let unix_type = if u16_at(header, 4) >> 8 == 3 {
            (external >> 16) & 0o170000
        } else {
            0
        };
        if !matches!(unix_type, 0 | 0o100000 | 0o040000) {
            return Err(unsupported(
                "PVD archive special file members are unsupported",
            ));
        }
        let directory_entry = name.ends_with('/') || external & 0x10 != 0 || unix_type == 0o040000;
        if directory_entry && uncompressed != 0 {
            return Err(error("PVD archive directory contains file data"));
        }
        if name.ends_with(".pxd2") {
            if directory_entry {
                return Err(unsupported(
                    "PVD archive recording member is not a regular file",
                ));
            }
            if selected.replace(entries.len()).is_some() {
                return Err(unsupported(
                    "PVD archive has multiple .pxd2 recording members",
                ));
            }
            if u64::from(uncompressed) > limits.max_member_bytes {
                return Err(error("PVD archive recording exceeds the output limit"));
            }
        }
        entries.push(Entry {
            name,
            version,
            flags,
            method,
            timestamp: u32_at(header, 12),
            crc: u32_at(header, 16),
            compressed,
            uncompressed,
            local_offset,
            regular: !directory_entry,
        });
        position = next;
    }
    if position != directory.len() {
        return Err(error(
            "PVD archive directory length disagrees with its entries",
        ));
    }
    let selected =
        selected.ok_or_else(|| unsupported("PVD archive has no unique .pxd2 recording member"))?;
    drop(names);
    drop(directory);
    let mut ranges = Vec::new();
    ranges.try_reserve_exact(entries.len())?;
    let mut member_offset = 0;
    for (index, entry) in entries.iter().enumerate() {
        let (data_offset, end) = local_range(reader, entry, u64::from(directory_offset), progress)?;
        ranges.push((u64::from(entry.local_offset), end));
        if index == selected {
            member_offset = data_offset;
        }
    }
    ranges.sort_unstable();
    let mut end = 0;
    for (start, next) in ranges {
        if start < end {
            return Err(error("PVD archive local member ranges overlap"));
        }
        if start != end {
            return Err(unsupported(
                "PVD archive prefixes or gaps between members are unsupported",
            ));
        }
        end = next;
    }
    if end != u64::from(directory_offset) {
        return Err(unsupported(
            "PVD archive data before the directory is unindexed",
        ));
    }
    let entry = entries.swap_remove(selected);
    debug_assert!(entry.regular);
    Ok(Member {
        name: entry.name,
        method: entry.method,
        crc: entry.crc,
        compressed: entry.compressed,
        uncompressed: entry.uncompressed,
        offset: member_offset,
    })
}

fn local_range<R: Read + Seek>(
    reader: &mut Reader<'_, R>,
    entry: &Entry,
    directory_offset: u64,
    progress: &mut dyn FnMut(u64, u64) -> bool,
) -> Result<(u64, u64), Error> {
    let offset = u64::from(entry.local_offset);
    let mut header = [0; 30];
    reader.read_at(offset, &mut header, progress)?;
    if u32_at(&header, 0) != LOCAL
        || u16_at(&header, 4) != entry.version
        || u16_at(&header, 6) != entry.flags
        || u16_at(&header, 8) != entry.method
        || u32_at(&header, 10) != entry.timestamp
    {
        return Err(error("PVD archive local and central headers disagree"));
    }
    let descriptor = entry.flags & 8 != 0;
    for (at, expected) in [
        (14, entry.crc),
        (18, entry.compressed),
        (22, entry.uncompressed),
    ] {
        let actual = u32_at(&header, at);
        if actual != expected && !(descriptor && actual == 0) {
            return Err(error("PVD archive local and central CRC or sizes disagree"));
        }
    }
    let name_length = usize::from(u16_at(&header, 26));
    let extra_length = usize::from(u16_at(&header, 28));
    if name_length != entry.name.len() {
        return Err(error("PVD archive local and central names disagree"));
    }
    let data_offset = offset + 30 + name_length as u64 + extra_length as u64;
    let data_end = data_offset + u64::from(entry.compressed);
    if data_end > directory_offset {
        return Err(error("PVD archive member data overlaps its directory"));
    }
    let mut variable = vec![0; name_length + extra_length];
    reader.read_at(offset + 30, &mut variable, progress)?;
    if &variable[..name_length] != entry.name.as_bytes() {
        return Err(error("PVD archive local and central names disagree"));
    }
    check_extra(&variable[name_length..])?;
    let end = if descriptor {
        let available = directory_offset - data_end;
        if available < 12 {
            return Err(error("PVD archive data descriptor is truncated"));
        }
        let length = available.min(16) as usize;
        let mut bytes = [0; 16];
        reader.read_at(data_end, &mut bytes[..length], progress)?;
        let matches = |start| {
            u32_at(&bytes, start) == entry.crc
                && u32_at(&bytes, start + 4) == entry.compressed
                && u32_at(&bytes, start + 8) == entry.uncompressed
        };
        match (
            matches(0),
            length == 16 && u32_at(&bytes, 0) == DESCRIPTOR && matches(4),
        ) {
            (true, false) => data_end + 12,
            (false, true) => data_end + 16,
            _ => {
                return Err(error(
                    "PVD archive data descriptor is inconsistent or ambiguous",
                ));
            }
        }
    } else {
        data_end
    };
    Ok((data_offset, end))
}

fn parse_name(bytes: &[u8]) -> Result<String, Error> {
    if bytes.is_empty() || bytes.len() > MAX_NAME_BYTES {
        return Err(unsupported("PVD archive member name length is unsupported"));
    }
    let name = std::str::from_utf8(bytes)
        .map_err(|_| unsupported("PVD archive member name is not UTF-8"))?;
    let path = name.strip_suffix('/').unwrap_or(name);
    if name
        .chars()
        .any(|ch| ch.is_control() || matches!(ch, '\\' | ':'))
        || path.split('/').any(|part| matches!(part, "" | "." | ".."))
    {
        return Err(unsupported(
            "PVD archive member path is unsafe or ambiguous",
        ));
    }
    Ok(name.to_owned())
}

fn check_extra(mut bytes: &[u8]) -> Result<(), Error> {
    while !bytes.is_empty() {
        if bytes.len() < 4 {
            return Err(error("PVD archive ZIP extra field is truncated"));
        }
        if matches!(u16_at(bytes, 0), 1 | 0x9901) {
            return Err(unsupported(
                "PVD archive ZIP64 or AES extra fields are unsupported",
            ));
        }
        let length = 4 + usize::from(u16_at(bytes, 2));
        bytes = bytes
            .get(length..)
            .ok_or_else(|| error("PVD archive ZIP extra field length exceeds its header"))?;
    }
    Ok(())
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap())
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
