use super::*;
use read_source::{EventPage, Source};
use std::io::{Read, Seek};
use std::ops::Range;

const MAX_EVENT_BYTES: usize = MAX_METADATA + MAX_ITEMS * 4 + 32;

pub(super) fn read_source_with_progress<R: Read + Seek>(
    reader: R,
    history: Option<crate::live::HistoryConfig>,
    mut progress: impl FnMut(u64, u64) -> bool,
) -> Result<(Recording, ContainerInfo), Error> {
    if !progress(0, 0) {
        return Err(cancelled());
    }
    let mut source = Source::new(reader)?;
    let length = source.len();
    let mut header = source.cursor(0..length)?;
    if header.u32(&mut || progress(0, 0))? != MAGIC || header.u32(&mut || progress(0, 0))? != 1 {
        return Err(error(
            "unsupported PVD3 client database signature or version",
        ));
    }
    let object_len = header.u32(&mut || progress(0, 0))? as usize;
    let comm_len = header.u32(&mut || progress(0, 0))? as usize;
    if object_len > MAX_METADATA || comm_len > MAX_METADATA {
        return Err(error("PVD3 metadata exceeds 64 MiB"));
    }
    let object_end = 16 + object_len as u64;
    let comm_end = object_end + comm_len as u64;
    let object_bytes = source.read_block(16..object_end, MAX_METADATA, &mut || progress(0, 0))?;
    let metadata = Metadata::parse_with_progress(&object_bytes, &mut || progress(0, 0))?;
    drop(object_bytes);
    let comm_bytes =
        source.read_block(object_end..comm_end, MAX_METADATA, &mut || progress(0, 0))?;
    let mut comm = Reader::new(&comm_bytes);
    if comm.u32()? != 1 {
        return Err(error("unsupported PVD3 communication metadata version"));
    }
    let numerator = comm.u64()?;
    let denominator = comm.u64()?;
    if numerator == 0 || denominator == 0 {
        return Err(error("invalid PVD3 clock conversion"));
    }
    let count = comm.count()?;
    let mut index_allocation = 0usize;
    metadata_budget(&mut index_allocation, count, 192)?;
    let mut source_ids = BTreeMap::<u64, Vec<u64>>::new();
    let mut instance_sources = BTreeMap::new();
    let mut settings = settings::Settings::default();
    for _ in 0..count {
        if !progress(0, 0) {
            return Err(cancelled());
        }
        let id = comm.u32()?;
        let source = comm.u64()?;
        if comm.u32()? != id {
            return Err(error("PVD3 instance metadata key mismatch"));
        }
        settings.read_display(u64::from(id), &mut comm, &mut index_allocation)?;
        if instance_sources.insert(u64::from(id), source).is_some() {
            return Err(error("duplicate PVD3 instance metadata ID"));
        }
        source_ids.entry(source).or_default().push(u64::from(id));
    }
    settings.read_cameras(
        &mut comm,
        &metadata,
        &instance_sources,
        &mut index_allocation,
        &mut || progress(0, 0),
    )?;
    let catalog =
        profile_catalog_tail(&mut comm, &instance_sources, &mut index_allocation, || {
            progress(0, 0)
        })?;
    drop(comm_bytes);
    let mut directory = source.cursor(next_page_offset(comm_end)?..length)?;
    let total = directory.u64(&mut || progress(0, 0))?;
    if total > 20_000_000 {
        return Err(error("PVD3 history exceeds 20 million events"));
    }
    let count = directory_count(&mut directory, &mut index_allocation, 32, &mut || {
        progress(0, total)
    })?;
    let mut pages = Vec::with_capacity(count);
    for _ in 0..count {
        pages.push((
            directory.u64(&mut || progress(0, total))?,
            directory.u64(&mut || progress(0, total))?,
            directory.u32(&mut || progress(0, total))?,
            directory.u32(&mut || progress(0, total))?,
        ));
    }
    let count = directory_count(&mut directory, &mut index_allocation, 32, &mut || {
        progress(0, total)
    })?;
    let mut snapshots = Vec::with_capacity(count);
    for _ in 0..count {
        snapshots.push((
            directory.u64(&mut || progress(0, total))?,
            directory.u64(&mut || progress(0, total))?,
            directory.u32(&mut || progress(0, total))?,
            directory.u32(&mut || progress(0, total))?,
        ));
    }
    let count = directory_count(&mut directory, &mut index_allocation, 128, &mut || {
        progress(0, total)
    })?;
    let mut instances = BTreeMap::new();
    for _ in 0..count {
        let id = directory.u32(&mut || progress(0, total))?;
        let position = directory.u64(&mut || progress(0, total))?;
        let len = directory.u32(&mut || progress(0, total))?;
        if instances
            .insert(position, (id, len))
            .is_some_and(|previous| previous != (id, len))
        {
            return Err(error("conflicting PVD3 saved instance directory entries"));
        }
    }
    let base = next_page_offset(directory.position())?;
    source.check_range(base..length)?;
    let Some(&(event, position, len, _)) = snapshots.first() else {
        return Err(error("missing PVD3 initial snapshot"));
    };
    if event != 0 {
        return Err(error("PVD3 initial snapshot must precede event zero"));
    }
    for (_, position, len, _) in &snapshots {
        source.check_range(backing_range(base, *position, u64::from(*len))?)?;
    }
    validate_saved_source(&metadata, &mut source, base, &instances, &mut || {
        progress(0, total)
    })?;
    let initial_properties = snapshot::initial_properties(&metadata, &mut || progress(0, total))?;
    let mut builder = RecordingBuilder::new(Header {
        version: 1,
        timestamp_numerator: numerator,
        timestamp_denominator: denominator,
        flags: 0,
        endian: Endian::Little,
    });
    builder.recording.source = RecordingSource::Pvd3Database;
    if let Some(config) = history {
        let history = crate::live::HistoryWriter::new_paged(config)?;
        if let Some(analysis) = history.initial_analysis() {
            builder.recording.analysis = analysis;
        }
        builder.enable_streaming(history);
    }
    let mut memory =
        memory_totals::Collector::new(catalog.memory, &metadata, &mut builder, &mut || {
            progress(0, total)
        })?;
    metadata.register_with_progress(&mut builder, &mut || progress(0, total))?;
    register_profile_catalog_with_progress(&mut builder, catalog.profiles, &mut || {
        progress(0, total)
    })?;
    let snapshot = source.read_block(
        backing_range(base, position, u64::from(len))?,
        MAX_METADATA,
        &mut || progress(0, total),
    )?;
    let initial_instances = snapshot::restore_from_source(
        &metadata,
        &mut builder,
        &mut source,
        base..length,
        &snapshot,
        &mut index_allocation,
        &mut || progress(0, total),
    )?;
    drop(snapshot);
    memory.initialize(
        &mut builder,
        initial_instances,
        &mut index_allocation,
        &mut || progress(0, total),
    )?;
    settings.initialize(&mut builder, &mut || progress(0, total))?;
    builder.recording.diagnostics.push("PVD3 client database import: object IDs and profile zone IDs are container-local IDs; performance contexts retain SDK source values. Profile names are saved metadata, not a history of names. Generic profile curves use the explicitly selected Scene time windows; fixed simulation/collision/solver curves use verified saved Scene source mappings; source-pointer reuse across Scene lifetimes is conservatively excluded. Memory events use flush received_frame, not exact allocation timestamps; post-frame flushes remain part of recording-end state. Saved errors have no timestamps or affected Actor IDs; their independent untimed list preserves container instance, source Index, history order and received_frame. SDK debug rendering uses saved container-local instance IDs and resets at captured frame boundaries; paired original-file validation covers joint frames, four joint-limit kinds, an RGB triangle and multiple flushes per frame; points, lines, text, nonempty bulk drawing and renderer recreation still need native fixtures. Communication display properties are saved preferences without change timestamps. Named cameras follow captured Camera instance lifetimes and vector updates; no Scene association or projection is inferred. Unsupported history/user event types stop import explicitly.".into());
    let mut contexts = if builder.streaming.is_some() {
        Some(contexts::Collector::from_state(
            &mut builder,
            &instance_sources,
            &mut index_allocation,
            &mut || progress(0, total),
        )?)
    } else {
        None
    };
    let mut index = 0u64;
    let mut previous_end = 0u64;
    for (first, position, count, len) in pages.iter().copied() {
        if first != index || position < previous_end || count == 0 {
            return Err(error("invalid PVD3 page ordering"));
        }
        previous_end = position
            .checked_add(u64::from(len))
            .ok_or_else(|| error("PVD3 page length overflow"))?;
        let page = EventPage::read(
            &mut source,
            backing_range(base, position, u64::from(len))?,
            count,
            ((MAX_ITEMS + 1) * 4).min(MAX_METADATA.saturating_sub(index_allocation)),
            &mut || progress(index, total),
        )?;
        for ordinal in 0..page.event_count() {
            let bytes =
                source.read_block(page.event_range(ordinal)?, MAX_EVENT_BYTES, &mut || {
                    progress(index, total)
                })?;
            let mut event_reader = Reader::new(&bytes);
            let kind = event_reader.u32()?;
            let affected = (1..=7)
                .contains(&kind)
                .then(|| event_reader.u32())
                .transpose()?;
            let created = affected.filter(|_| kind == 1);
            validate_saved_event(&bytes, &instances)
                .and_then(|()| {
                    import_event_with_progress(
                        &metadata,
                        &source_ids,
                        &mut builder,
                        &bytes,
                        index,
                        usize::try_from(length - base)?,
                        &mut || progress(index, total),
                    )
                })
                .map_err(|failure| {
                    if failure
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|io| io.kind() == std::io::ErrorKind::Interrupted)
                    {
                        failure
                    } else {
                        error(format!("PVD3 event {index}: {failure}"))
                    }
                })?;
            if let Some(id) = created {
                initial_properties.restore_instance(&metadata, &mut builder, id, &mut || {
                    progress(index, total)
                })?;
                settings.apply_display(&mut builder, u64::from(id))?;
                memory.observe_created(&builder, id);
            }
            if let Some(id) = affected {
                settings.sync_camera(&mut builder, u64::from(id))?;
            }
            if let (Some(contexts), Some(id)) = (&mut contexts, created) {
                contexts.observe(
                    &mut builder,
                    &instance_sources,
                    u64::from(id),
                    &mut index_allocation,
                )?;
            }
            if let Some(failure) = builder
                .streaming
                .as_ref()
                .and_then(|live| live.analysis_failure.as_ref())
            {
                return Err(error(format!("PVD3 analysis import failed: {failure}")));
            }
            index += 1;
        }
    }
    if index != total {
        return Err(error("PVD3 total history event count mismatch"));
    }
    settings.validate_references()?;
    memory.finish(&mut builder, &mut index_allocation, &mut || {
        progress(index, total)
    })?;
    if let Some(contexts) = contexts {
        contexts.install(&mut builder);
    } else {
        contexts::install(
            &mut builder,
            &instance_sources,
            &mut index_allocation,
            &mut || progress(index, total),
        )?;
    }
    let source_blake3 = if length <= 512 << 20 {
        let mut hash = blake3::Hasher::new();
        let mut bytes = [0; 64 * 1024];
        let mut offset = 0;
        while offset < length {
            let size = (length - offset).min(bytes.len() as u64) as usize;
            source.read_at(offset, &mut bytes[..size], &mut || progress(index, total))?;
            hash.update(&bytes[..size]);
            offset += size as u64;
        }
        Some(*hash.finalize().as_bytes())
    } else {
        None
    };
    source.check_length()?;
    if !progress(total, total) {
        return Err(cancelled());
    }
    let recording = finish_recording(builder)?;
    if let Some(stats) = recording.disk_usage()
        && (stats.first_frame != 0
            || stats.retained_frames != recording.frame_count()
            || stats.warning.is_some())
    {
        return Err(error(format!(
            "PVD3 disk history cannot retain the complete file: {}",
            stats.warning.unwrap_or_default()
        )));
    }
    let info = ContainerInfo {
        object_metadata_bytes: object_len,
        communication_metadata_bytes: comm_len,
        classes: metadata.classes.iter().flatten().count(),
        properties: metadata.properties.iter().flatten().count(),
        property_messages: metadata.messages.iter().flatten().count(),
        history_events: total,
        history_pages: pages.len(),
        source_ids: instance_sources,
        source_bytes: length,
        source_blake3,
    };
    Ok((recording, info))
}

pub(super) fn finish_recording(mut builder: RecordingBuilder) -> Result<Recording, Error> {
    if let Some(live) = &builder.streaming
        && live.frames_seen == 0
    {
        let state_bytes = live.state_bytes;
        builder.streaming = None;
        builder.reserve_history(state_bytes)?;
    }
    builder.finish()
}

fn next_page_offset(position: u64) -> Result<u64, Error> {
    (position / 4096)
        .checked_add(1)
        .and_then(|position| position.checked_mul(4096))
        .ok_or_else(|| error("PVD3 page alignment overflow"))
}

fn directory_count<R: Read + Seek>(
    reader: &mut read_source::Cursor<'_, R>,
    allocation: &mut usize,
    width: usize,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<usize, Error> {
    let count = reader.u32(proceed)? as usize;
    if count > MAX_ITEMS {
        return Err(error("PVD3 array exceeds one million entries"));
    }
    if count as u64 > reader.remaining() {
        return Err(error("PVD3 array count exceeds containing block"));
    }
    metadata_budget(allocation, count, width)?;
    Ok(count)
}

pub(super) fn backing_range(base: u64, position: u64, length: u64) -> Result<Range<u64>, Error> {
    let start = base
        .checked_add(position)
        .ok_or_else(|| error("PVD3 backing range position overflow"))?;
    let end = start
        .checked_add(length)
        .ok_or_else(|| error("PVD3 backing range length overflow"))?;
    Ok(start..end)
}

fn validate_saved_source<R: Read + Seek>(
    metadata: &Metadata,
    source: &mut Source<R>,
    base: u64,
    instances: &BTreeMap<u64, (u32, u32)>,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    for (&position, &(id, length)) in instances {
        let range = backing_range(base, position, u64::from(length))?;
        source.check_range(range.clone())?;
        if length < 12 {
            return Err(error("PVD3 saved instance record is truncated"));
        }
        let bytes = source.read_block(range.start..range.start + 12, 12, proceed)?;
        let mut record = Reader::new(&bytes);
        metadata.class(record.u32()?)?;
        if record.u32()? != id {
            return Err(error("PVD3 saved instance ID differs from its directory"));
        }
        let body = record.u32()?;
        if body as usize > MAX_METADATA {
            return Err(error("PVD3 saved instance body exceeds 64 MiB"));
        }
        if body > length - 12 {
            return Err(error("PVD3 saved instance body is truncated"));
        }
        if body < length - 12 {
            return Err(error(
                "PVD3 saved instance record has unexplained trailing bytes",
            ));
        }
    }
    Ok(())
}
