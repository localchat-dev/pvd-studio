use super::{
    RendererPatch, RendererTransaction, SectionPlan, ValidationState, encode_append, encode_set,
    frame_section, insert_patch, renderer_instance_state, validate_renderer_datatype,
};
use crate::{Error, RecordingBuilder, error};
use pvd_protocol::{Decoder, Endian, Event, EventGroup, Header, Limits, rewrite::map_group_events};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    io::{self, Read, Seek, SeekFrom, Write},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrimPhase {
    Inspect,
    Export,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrimProgress {
    pub phase: TrimPhase,
    pub bytes_read: u64,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrimReport {
    pub source_blake3: [u8; 32],
    pub source_bytes: u64,
    pub source_frames: usize,
    pub output_bytes: u64,
    pub output_frames: usize,
}

/// Writes SDK communication bytes; publish the output only after this returns `Ok`.
/// The inclusive range follows known section End events in capture order.
/// Cancellation and validation errors can leave partial bytes in the supplied writer.
pub fn trim_scene_frames_streaming<R: Read + Seek, W: Write + ?Sized>(
    source: &mut R,
    output: &mut W,
    first: usize,
    last: usize,
    mut progress: impl FnMut(TrimProgress) -> bool,
) -> Result<TrimReport, Error> {
    if first > last {
        return Err(error("invalid inclusive frame range"));
    }
    let mut scan = Scan::new(source, TrimPhase::Inspect, &mut progress)?;
    let header_bytes = scan.header_bytes;
    let header = scan.header.clone();
    let mut builder = validator(header.clone());
    let mut plan = RendererPlan::default();
    let mut sections = SectionPlan::default();
    let mut ordinal = 0usize;
    while let Some((_, group)) = scan.next(&mut progress)? {
        for event in group.events {
            if ordinal.is_multiple_of(512) {
                scan.notify(&mut progress)?;
            }
            sections.observe(
                &event,
                &builder.strings,
                ordinal,
                frames(&builder),
                first,
                last,
            )?;
            if frames(&builder) < first {
                plan.observe(
                    &event,
                    group.stream_id,
                    ordinal,
                    &builder.strings,
                    header.endian,
                )?;
            }
            push(
                &mut builder,
                event,
                group.stream_id,
                group.timestamp,
                group.offset,
            )?;
            ordinal = ordinal
                .checked_add(1)
                .ok_or_else(|| error("event ordinal overflow"))?;
        }
    }
    ensure_complete(&builder)?;
    let source_frames = frames(&builder);
    if last >= source_frames {
        return Err(error(format!(
            "invalid inclusive frame range {first}..={last}; source has {source_frames} frames"
        )));
    }
    let source_blake3 = scan.digest();
    let source_bytes = scan.total;
    drop(scan);
    drop(builder);
    plan.transactions.clear();
    sections.finish();
    plan.patches.retain(|_, patch| {
        sections
            .renderer_boundary
            .is_some_and(|(_, end)| patch.completed <= end)
    });

    let mut scan = Scan::new(source, TrimPhase::Export, &mut progress)?;
    if scan.total != source_bytes || scan.header_bytes != header_bytes {
        return Err(error("source capture changed between clip scans"));
    }
    let mut source_state = validator(header.clone());
    let mut output_state = validator(header.clone());
    source_state.validation.as_mut().unwrap().skip_analysis = true;
    output_state.validation.as_mut().unwrap().skip_analysis = true;
    let mut patch_decoder = Decoder::default();
    patch_decoder.feed(&header_bytes)?;
    output.write_all(&header_bytes)?;
    let mut output_bytes = header_bytes.len() as u64;
    let mut ordinal = 0usize;
    let mut done = false;
    while let Some((raw_group, group)) = scan.next(&mut progress)? {
        if done {
            continue;
        }
        let mut callback_error = None;
        let rewritten = map_group_events(&raw_group, header.endian, |event, raw| {
            let result = (|| -> Result<Option<Cow<'_, [u8]>>, Error> {
                if ordinal.is_multiple_of(512) {
                    scan.notify(&mut progress)?;
                }
                let event_ordinal = ordinal;
                let patch = plan.patches.get(&event_ordinal).map(|patch| &patch.data);
                ordinal = ordinal
                    .checked_add(1)
                    .ok_or_else(|| error("event ordinal overflow"))?;
                if done {
                    return Ok(None);
                }
                let frame = frames(&source_state);
                let boundary = match event {
                    Event::BeginSection { name, .. } | Event::EndSection { name, .. } => {
                        frame_section(&source_state.strings, *name).is_some()
                    }
                    _ => false,
                };
                let keep = if boundary && matches!(event, Event::BeginSection { .. }) {
                    sections.retain_begin(event_ordinal, frame >= first)
                } else {
                    !boundary || frame >= first
                };
                if boundary && matches!(event, Event::EndSection { .. }) && keep {
                    source_state.prepare_end_view(event)?;
                    output_state.prepare_end_view(event)?;
                    if source_state.state != output_state.state {
                        return Err(error(format!(
                            "trim cannot preserve complete scene state at source frame {frame}; the source rendering window, renderer raw data or another boundary dependency is not representable"
                        )));
                    }
                    done = frame == last;
                }
                push(
                    &mut source_state,
                    event.clone(),
                    group.stream_id,
                    group.timestamp,
                    group.offset,
                )?;
                if !keep {
                    return Ok(None);
                }
                let mapped = match patch {
                    Some(patch) => {
                        let mut encoded = raw_group[..24].to_vec();
                        let (size, count) = match header.endian {
                            Endian::Little => {
                                ((patch.len() as u32).to_le_bytes(), 1u32.to_le_bytes())
                            }
                            Endian::Big => ((patch.len() as u32).to_be_bytes(), 1u32.to_be_bytes()),
                        };
                        encoded[..4].copy_from_slice(&size);
                        encoded[4..8].copy_from_slice(&count);
                        encoded.extend_from_slice(patch);
                        let mut groups = patch_decoder.feed(&encoded)?;
                        groups
                            .pop()
                            .and_then(|mut group| group.events.pop())
                            .ok_or_else(|| error("missing rewritten renderer event"))?
                    }
                    None => event.clone(),
                };
                push(
                    &mut output_state,
                    mapped,
                    group.stream_id,
                    group.timestamp,
                    group.offset,
                )?;
                Ok(Some(
                    patch.map_or(Cow::Borrowed(raw), |patch| Cow::Borrowed(patch)),
                ))
            })();
            result.map_err(|error| {
                let message = error.to_string();
                callback_error = Some(error);
                pvd_protocol::Error {
                    offset: group.offset,
                    message,
                }
            })
        });
        if let Some(error) = callback_error {
            return Err(error);
        }
        if let Some(rewritten) = rewritten? {
            output.write_all(&rewritten)?;
            output_bytes = output_bytes
                .checked_add(rewritten.len() as u64)
                .ok_or_else(|| error("clip output size overflow"))?;
        }
    }
    if scan.digest() != source_blake3 {
        return Err(error("source capture changed between clip scans"));
    }
    let output_frames = frames(&output_state);
    if !done || output_frames != last - first + 1 {
        return Err(error("trimmed frame count differs from requested range"));
    }
    ensure_complete(&output_state)?;
    Ok(TrimReport {
        source_blake3,
        source_bytes,
        source_frames,
        output_bytes,
        output_frames,
    })
}

fn validator(header: Header) -> RecordingBuilder {
    let mut builder = RecordingBuilder::new(header);
    builder.validation = Some(ValidationState::default());
    builder
}

fn frames(builder: &RecordingBuilder) -> usize {
    builder.validation.as_ref().unwrap().frames
}

fn ensure_complete(builder: &RecordingBuilder) -> Result<(), Error> {
    if !builder.partial.is_empty() || !builder.groups.is_empty() {
        return Err(error("stream ended inside a property transaction"));
    }
    Ok(())
}

fn push(
    builder: &mut RecordingBuilder,
    event: Event,
    stream_id: u64,
    timestamp: u64,
    offset: u64,
) -> Result<(), Error> {
    builder.push(EventGroup {
        stream_id,
        timestamp,
        offset,
        events: vec![event],
    })
}

struct Scan<'a, R> {
    source: &'a mut R,
    header_bytes: [u8; 28],
    header: Header,
    decoder: Decoder,
    hash: blake3::Hasher,
    read: u64,
    total: u64,
    phase: TrimPhase,
}

impl<'a, R: Read + Seek> Scan<'a, R> {
    fn new(
        source: &'a mut R,
        phase: TrimPhase,
        progress: &mut impl FnMut(TrimProgress) -> bool,
    ) -> Result<Self, Error> {
        let total = source.seek(SeekFrom::End(0))?;
        source.seek(SeekFrom::Start(0))?;
        check_progress(
            progress,
            TrimProgress {
                phase,
                bytes_read: 0,
                total_bytes: total,
            },
        )?;
        let mut header_bytes = [0; 28];
        source.read_exact(&mut header_bytes)?;
        if header_bytes.starts_with(&crate::native::MAGIC.to_le_bytes()) {
            return Err(error(
                "trimming original PVD3 containers is not supported; SDK communication bytes are required",
            ));
        }
        let mut decoder = Decoder::default();
        decoder.feed(&header_bytes)?;
        let header = decoder
            .header()
            .cloned()
            .ok_or_else(|| error("missing stream header"))?;
        let mut hash = blake3::Hasher::new();
        hash.update(&header_bytes);
        Ok(Self {
            source,
            header_bytes,
            header,
            decoder,
            hash,
            read: 28,
            total,
            phase,
        })
    }

    fn notify(&self, progress: &mut impl FnMut(TrimProgress) -> bool) -> Result<(), Error> {
        check_progress(
            progress,
            TrimProgress {
                phase: self.phase,
                bytes_read: self.read,
                total_bytes: self.total,
            },
        )
    }

    fn next(
        &mut self,
        progress: &mut impl FnMut(TrimProgress) -> bool,
    ) -> Result<Option<(Vec<u8>, EventGroup)>, Error> {
        self.notify(progress)?;
        if self.read == self.total {
            let mut extra = [0; 1];
            if self.source.read(&mut extra)? != 0 {
                return Err(error("source capture grew while trimming"));
            }
            self.decoder.finish()?;
            return Ok(None);
        }
        if self.total.saturating_sub(self.read) < 24 {
            return Err(error("truncated event group header during trim"));
        }
        let mut bytes = vec![0; 24];
        self.source.read_exact(&mut bytes)?;
        let length = match self.header.endian {
            Endian::Little => u32::from_le_bytes(bytes[..4].try_into()?),
            Endian::Big => u32::from_be_bytes(bytes[..4].try_into()?),
        } as usize;
        if length > Limits::default().max_group_bytes || length as u64 > self.total - self.read - 24
        {
            return Err(error("invalid event group size during trim"));
        }
        bytes.resize(24 + length, 0);
        self.hash.update(&bytes[..24]);
        self.read += 24;
        for chunk in bytes[24..].chunks_mut(64 << 10) {
            self.source.read_exact(chunk)?;
            self.hash.update(chunk);
            self.read += chunk.len() as u64;
            self.notify(progress)?;
        }
        let mut groups = self.decoder.feed(&bytes)?;
        let group = groups
            .pop()
            .ok_or_else(|| error("incomplete event group during trim"))?;
        Ok(Some((bytes, group)))
    }

    fn digest(&self) -> [u8; 32] {
        *self.hash.finalize().as_bytes()
    }
}

fn check_progress(
    progress: &mut impl FnMut(TrimProgress) -> bool,
    value: TrimProgress,
) -> Result<(), Error> {
    if !progress(value) {
        return Err(
            io::Error::new(io::ErrorKind::Interrupted, "scene frame trim cancelled").into(),
        );
    }
    Ok(())
}

#[derive(Default)]
struct RendererPlan {
    renderers: BTreeSet<u64>,
    transactions: BTreeMap<u64, RendererTransaction>,
    patches: BTreeMap<usize, RendererPatch>,
    pending_bytes: usize,
    pending_appends: usize,
}

impl RendererPlan {
    fn observe(
        &mut self,
        event: &Event,
        stream: u64,
        ordinal: usize,
        strings: &BTreeMap<u32, String>,
        endian: Endian,
    ) -> Result<(), Error> {
        match event {
            Event::CreateInstance { class, id }
                if strings
                    .get(&class.name)
                    .is_some_and(|name| name == "PvdUserRenderer") =>
            {
                self.renderers.insert(*id);
            }
            Event::DestroyInstance { id } => {
                self.renderers.remove(id);
            }
            Event::SetPropertyValue {
                id,
                property,
                datatype,
                items,
                data,
            } if self.renderers.contains(id)
                && strings.get(property).is_some_and(|name| name == "events") =>
            {
                validate_renderer_datatype(strings, *datatype)?;
                if *items as usize != data.len() {
                    return Err(error("renderer events are not a byte array"));
                }
                let data = renderer_instance_state(data, endian)?;
                insert_patch(
                    &mut self.patches,
                    ordinal,
                    ordinal,
                    encode_set(*id, *property, *datatype, &data, endian),
                )?;
            }
            Event::BeginSetPropertyValue {
                id,
                property,
                datatype,
            } if self.renderers.contains(id)
                && strings.get(property).is_some_and(|name| name == "events") =>
            {
                validate_renderer_datatype(strings, *datatype)?;
                if self
                    .transactions
                    .insert(
                        stream,
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
                if let Some(transaction) = self.transactions.get_mut(&stream) {
                    if *items as usize != data.len()
                        || data.len() > (64usize << 20).saturating_sub(self.pending_bytes)
                        || self.pending_appends >= 1_000_000
                    {
                        return Err(error(
                            "renderer transaction exceeds its byte-array or resource bounds",
                        ));
                    }
                    self.pending_bytes += data.len();
                    self.pending_appends += 1;
                    transaction.data.extend_from_slice(data);
                    transaction.appends.push(ordinal);
                }
            }
            Event::EndSetPropertyValue => {
                if let Some(transaction) = self.transactions.remove(&stream) {
                    self.pending_bytes -= transaction.data.len();
                    self.pending_appends -= transaction.appends.len();
                    let filtered = renderer_instance_state(&transaction.data, endian)?;
                    let completed = ordinal;
                    for (index, ordinal) in transaction.appends.iter().enumerate() {
                        let data = if index + 1 == transaction.appends.len() {
                            &filtered[..]
                        } else {
                            &[]
                        };
                        insert_patch(
                            &mut self.patches,
                            *ordinal,
                            completed,
                            encode_append(data, endian),
                        )?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}
