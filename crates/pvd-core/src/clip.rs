use crate::{Error, Recording, error};
use pvd_protocol::{Decoder, Endian, Event, Name, rewrite::map_group_events};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
};
mod streaming;
mod validation;
pub use streaming::{TrimPhase, TrimProgress, TrimReport, trim_scene_frames_streaming};
pub(crate) use validation::ValidationState;

pub fn trim_scene_frames(bytes: &[u8], first: usize, last: usize) -> Result<Vec<u8>, Error> {
    if bytes.starts_with(&crate::native::MAGIC.to_le_bytes()) {
        return Err(error(
            "trimming original PVD3 containers is not supported; .pvdstream export requires SDK communication bytes",
        ));
    }
    let source = Recording::from_bytes(bytes)?;
    if first > last || last >= source.frame_count() {
        return Err(error(format!(
            "invalid inclusive frame range {first}..={last}; source has {} frames",
            source.frame_count()
        )));
    }
    let endian = source.header.endian;
    let patches = preroll_renderer_patches(bytes, endian, first)?;
    let mut output = bytes
        .get(..28)
        .ok_or_else(|| error("truncated header"))?
        .to_vec();
    let mut decoder = Decoder::default();
    let mut strings = BTreeMap::new();
    let mut frame = 0usize;
    let mut done = false;
    let mut ordinal = 0usize;
    for chunk in bytes.chunks(64 * 1024) {
        for group in decoder.feed(chunk)? {
            let offset = group.offset as usize;
            let size: [u8; 4] = bytes
                .get(offset..offset + 4)
                .ok_or_else(|| error("invalid source group offset"))?
                .try_into()?;
            let length = match endian {
                Endian::Little => u32::from_le_bytes(size),
                Endian::Big => u32::from_be_bytes(size),
            } as usize;
            let raw = bytes
                .get(offset..offset + 24 + length)
                .ok_or_else(|| error("invalid source group boundary"))?;
            let retained = map_group_events(raw, endian, |event, raw| {
                let patch = patches.get(&ordinal);
                ordinal += 1;
                if done {
                    return Ok(None);
                }
                let keep = match event {
                    Event::StringHandle { handle, value } => {
                        strings.insert(*handle, value.clone());
                        true
                    }
                    Event::BeginSection { name, .. }
                        if strings.get(name).is_some_and(|s| s == "frame") =>
                    {
                        frame >= first
                    }
                    Event::EndSection { name, .. }
                        if strings.get(name).is_some_and(|s| s == "frame") =>
                    {
                        let keep = frame >= first;
                        done = frame == last;
                        frame += 1;
                        keep
                    }
                    _ => true,
                };
                Ok(keep
                    .then(|| patch.map_or(Cow::Borrowed(raw), |patch| Cow::Owned(patch.clone()))))
            })?;
            if let Some(retained) = retained {
                output.extend(retained);
            }
            if done {
                break;
            }
        }
        if done {
            break;
        }
    }
    let trimmed = Recording::from_bytes(&output)?;
    if trimmed.frame_count() != last - first + 1 {
        return Err(error("trimmed frame count differs from requested range"));
    }
    for frame in 0..trimmed.frame_count() {
        if trimmed.state_at(frame)? != source.state_at(first + frame)? {
            return Err(error(format!(
                "trim cannot preserve complete scene state at source frame {}; a renderer with no replacement payload may retain an earlier raw event buffer, or the selected boundary may have another unrepresentable dependency",
                first + frame
            )));
        }
    }
    Ok(output)
}

struct RendererTransaction {
    data: Vec<u8>,
    appends: Vec<usize>,
}

fn preroll_renderer_patches(
    bytes: &[u8],
    endian: Endian,
    first: usize,
) -> Result<BTreeMap<usize, Vec<u8>>, Error> {
    if first == 0 {
        return Ok(BTreeMap::new());
    }
    let mut decoder = Decoder::default();
    let mut strings = BTreeMap::new();
    let mut renderers = BTreeSet::new();
    let mut transactions = BTreeMap::<u64, RendererTransaction>::new();
    let mut patches = BTreeMap::new();
    let mut ordinal = 0usize;
    let mut frame = 0;
    let mut pending_bytes = 0usize;
    let mut pending_appends = 0usize;
    for chunk in bytes.chunks(64 * 1024) {
        for group in decoder.feed(chunk)? {
            for event in group.events {
                match event {
                    Event::StringHandle { handle, value } => {
                        strings.insert(handle, value);
                    }
                    Event::CreateInstance { class, id }
                        if strings
                            .get(&class.name)
                            .is_some_and(|name| name == "PvdUserRenderer") =>
                    {
                        renderers.insert(id);
                    }
                    Event::DestroyInstance { id } => {
                        renderers.remove(&id);
                    }
                    Event::SetPropertyValue {
                        id,
                        property,
                        datatype,
                        items,
                        data,
                    } if renderers.contains(&id)
                        && strings.get(&property).is_some_and(|name| name == "events") =>
                    {
                        validate_renderer_datatype(&strings, datatype)?;
                        if items as usize != data.len() {
                            return Err(error("renderer events are not a byte array"));
                        }
                        let filtered = renderer_instance_state(&data, endian)?;
                        let patch = encode_set(id, property, datatype, &filtered, endian);
                        insert_patch(&mut patches, ordinal, patch)?;
                    }
                    Event::BeginSetPropertyValue {
                        id,
                        property,
                        datatype,
                    } if renderers.contains(&id)
                        && strings.get(&property).is_some_and(|name| name == "events") =>
                    {
                        validate_renderer_datatype(&strings, datatype)?;
                        if transactions
                            .insert(
                                group.stream_id,
                                RendererTransaction {
                                    data: Vec::new(),
                                    appends: Vec::new(),
                                },
                            )
                            .is_some()
                        {
                            return Err(error("nested renderer property transaction"));
                        }
                    }
                    Event::AppendPropertyValueData { data, items } => {
                        if let Some(transaction) = transactions.get_mut(&group.stream_id) {
                            if items as usize != data.len()
                                || data.len() > (64usize << 20).saturating_sub(pending_bytes)
                                || pending_appends >= 1_000_000
                            {
                                return Err(error(
                                    "renderer transaction exceeds its byte-array or resource bounds",
                                ));
                            }
                            pending_bytes += data.len();
                            pending_appends += 1;
                            transaction.data.extend_from_slice(&data);
                            transaction.appends.push(ordinal);
                        }
                    }
                    Event::EndSetPropertyValue => {
                        if let Some(transaction) = transactions.remove(&group.stream_id) {
                            pending_bytes -= transaction.data.len();
                            pending_appends -= transaction.appends.len();
                            let filtered = renderer_instance_state(&transaction.data, endian)?;
                            for (index, ordinal) in transaction.appends.iter().enumerate() {
                                let data = if index + 1 == transaction.appends.len() {
                                    &filtered[..]
                                } else {
                                    &[][..]
                                };
                                insert_patch(&mut patches, *ordinal, encode_append(data, endian))?;
                            }
                        }
                    }
                    Event::EndSection { name, .. }
                        if strings.get(&name).is_some_and(|name| name == "frame") =>
                    {
                        frame += 1;
                        if frame == first {
                            return Ok(patches);
                        }
                    }
                    _ => {}
                }
                ordinal += 1;
            }
        }
    }
    Err(error("requested preroll boundary is absent"))
}

fn insert_patch(
    patches: &mut BTreeMap<usize, Vec<u8>>,
    ordinal: usize,
    data: Vec<u8>,
) -> Result<(), Error> {
    if patches.len() >= (64usize << 20) / 128 {
        return Err(error("renderer rewrite plan exceeds 64 MiB"));
    }
    patches.insert(ordinal, data);
    Ok(())
}

fn validate_renderer_datatype(
    strings: &BTreeMap<u32, String>,
    datatype: Name,
) -> Result<(), Error> {
    if strings
        .get(&datatype.name)
        .is_none_or(|name| name != "PvdU8")
    {
        return Err(error(
            "renderer preroll requires the SDK PvdU8 property encoding",
        ));
    }
    Ok(())
}

fn write_u32(output: &mut Vec<u8>, value: u32, endian: Endian) {
    output.extend(match endian {
        Endian::Little => value.to_le_bytes(),
        Endian::Big => value.to_be_bytes(),
    });
}
fn encode_set(id: u64, property: u32, datatype: Name, data: &[u8], endian: Endian) -> Vec<u8> {
    let mut output = vec![7];
    output.extend(match endian {
        Endian::Little => id.to_le_bytes(),
        Endian::Big => id.to_be_bytes(),
    });
    for value in [
        property,
        datatype.namespace,
        datatype.name,
        data.len() as u32,
        data.len() as u32,
    ] {
        write_u32(&mut output, value, endian);
    }
    output.extend(data);
    output
}
fn encode_append(data: &[u8], endian: Endian) -> Vec<u8> {
    let mut output = vec![9];
    write_u32(&mut output, data.len() as u32, endian);
    output.extend(data);
    write_u32(&mut output, data.len() as u32, endian);
    output
}

fn renderer_instance_state(bytes: &[u8], endian: Endian) -> Result<Vec<u8>, Error> {
    let mut cursor = RendererCursor {
        bytes,
        position: 0,
        endian,
    };
    let mut state = Vec::new();
    while cursor.position < bytes.len() {
        match cursor.take(1)?[0] {
            1 => {
                state.clear();
                state.push(1);
                state.extend(cursor.take(8)?);
            }
            2 => cursor.array(16)?,
            3 => cursor.array(32)?,
            4 => cursor.array(48)?,
            5 => {
                cursor.take(56)?;
            }
            6 => {
                cursor.take(61)?;
            }
            7 | 8 => {
                cursor.take(37)?;
            }
            9 => {
                cursor.take(33)?;
            }
            10 => {
                cursor.take(20)?;
                let count = cursor.u32()?;
                if count > 1 << 20 {
                    return Err(error("renderer text exceeds one MiB"));
                }
                cursor.take(count as usize)?;
            }
            11 => {
                cursor.array(16)?;
                cursor.array(32)?;
                cursor.array(48)?;
            }
            kind => {
                return Err(error(format!(
                    "cannot locate unknown renderer event {kind} boundary"
                )));
            }
        }
    }
    Ok(state)
}

struct RendererCursor<'a> {
    bytes: &'a [u8],
    position: usize,
    endian: Endian,
}
impl<'a> RendererCursor<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], Error> {
        let end = self
            .position
            .checked_add(count)
            .ok_or_else(|| error("renderer event length overflow"))?;
        let data = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| error("truncated renderer event during trim"))?;
        self.position = end;
        Ok(data)
    }
    fn u32(&mut self) -> Result<u32, Error> {
        let bytes = self.take(4)?.try_into()?;
        Ok(match self.endian {
            Endian::Little => u32::from_le_bytes(bytes),
            Endian::Big => u32::from_be_bytes(bytes),
        })
    }
    fn array(&mut self, stride: usize) -> Result<(), Error> {
        let count = self.u32()? as usize;
        if count > 1_000_000 {
            return Err(error("renderer array exceeds one million primitives"));
        }
        self.take(
            count
                .checked_mul(stride)
                .ok_or_else(|| error("renderer array length overflow"))?,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debug_draw::{Primitive, RenderDecoder};

    fn renderer_event(kind: u8, endian: Endian) -> Vec<u8> {
        let mut data = vec![kind];
        match kind {
            1 => data.extend([33; 8]),
            2..=4 => {
                write_u32(&mut data, 1, endian);
                data.resize(data.len() + 16 * (kind as usize - 1), 0);
            }
            5..=9 => data.resize(1 + [56, 61, 37, 37, 33][kind as usize - 5], 0),
            10 => {
                data.resize(21, 0);
                write_u32(&mut data, 2, endian);
                data.extend(b"x\0");
            }
            11 => {
                for stride in [16, 32, 48] {
                    write_u32(&mut data, 1, endian);
                    data.resize(data.len() + stride, 0);
                }
            }
            _ => unreachable!(),
        }
        data
    }

    #[test]
    fn preroll_retains_only_persistent_renderer_instance() {
        for endian in [Endian::Little, Endian::Big] {
            let mut buffer = Vec::new();
            for kind in 1..=11 {
                let event = renderer_event(kind, endian);
                RenderDecoder::default().decode(&event, endian).unwrap();
                for length in 1..event.len() {
                    assert!(renderer_instance_state(&event[..length], endian).is_err());
                }
                buffer.extend(event);
            }
            let mut final_id = vec![1];
            final_id.extend([77; 8]);
            buffer.extend_from_slice(&final_id);
            let filtered = renderer_instance_state(&buffer, endian).unwrap();
            assert_eq!(filtered, final_id);
            let mut decoder = RenderDecoder::default();
            assert!(decoder.decode(&filtered, endian).unwrap().is_empty());
            let points = decoder.decode(&renderer_event(2, endian), endian).unwrap();
            assert!(
                matches!(points[..], [Primitive::Point { instance, .. }] if instance == u64::from_le_bytes([77; 8]))
            );
            assert!(
                renderer_instance_state(&renderer_event(2, endian), endian)
                    .unwrap()
                    .is_empty()
            );
            assert!(renderer_instance_state(&[], endian).unwrap().is_empty());
            assert!(renderer_instance_state(&[255], endian).is_err());
            for kind in [2, 3, 4, 11] {
                let mut hostile = vec![kind];
                write_u32(&mut hostile, u32::MAX, endian);
                assert!(renderer_instance_state(&hostile, endian).is_err());
            }
            let mut hostile_text = vec![10; 21];
            write_u32(&mut hostile_text, u32::MAX, endian);
            assert!(renderer_instance_state(&hostile_text, endian).is_err());
        }
    }
}
