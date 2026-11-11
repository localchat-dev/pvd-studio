use crate::{
    Error, Recording, RecordingBuilder,
    analysis::{
        Analysis, MemorySample, ProfileSpan, ProfileZone,
        view::{MemorySource, MemorySummary, ProfileMetadataSource, ProfileNames, ProfileSource},
    },
    clip::ValidationState,
    error,
};
use pvd_protocol::{Decoder, Event, EventGroup, Header, Name};
use std::{
    collections::BTreeMap,
    io::{self, Read, Seek, SeekFrom, Write},
    sync::Arc,
};

pub type ProfileEventKey = (u64, u16, u32, u64);

pub(crate) trait ProfileObserver {
    fn batch(&mut self, analysis: &Analysis) -> Result<(), Error>;
    fn finish(&mut self, analysis: &Analysis, header: &Header) -> Result<(), Error>;
    fn metadata(&mut self, _: &Analysis, _: u64, _: Option<u16>) -> Result<(), Error> {
        Ok(())
    }
}

pub(crate) fn scan_profile<R: Read + Seek + ?Sized>(
    source: &mut R,
    observer: &mut dyn ProfileObserver,
    progress: &mut impl FnMut(AnalysisExportProgress) -> bool,
) -> Result<AnalysisExportReport, Error> {
    scan::<R, io::Sink>(
        source,
        None,
        ScanKind::ProfileQuery,
        None,
        AnalysisExportPhase::Inspect,
        progress,
        Some(observer),
    )
}

#[derive(Clone, Debug)]
pub struct ProfileNameCollision {
    pub zone_name: Arc<str>,
    pub event_name: Arc<str>,
    pub keys: Vec<(u64, u16)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnalysisCsvKind {
    Profile,
    Memory,
}

#[derive(Clone, Copy)]
enum ScanKind {
    Csv(AnalysisCsvKind),
    ProfileQuery,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnalysisExportPhase {
    Inspect,
    Export,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnalysisExportProgress {
    pub phase: AnalysisExportPhase,
    pub bytes_read: u64,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, Default)]
pub struct AnalysisExportSummary {
    pub profile_spans: u64,
    pub profile_values: u64,
    pub legacy_cuda_buffers: u64,
    pub unmatched_stops: u64,
    pub open_spans: usize,
    pub memory_allocations: u64,
    pub memory_frees: u64,
    pub unmatched_frees: u64,
    pub replaced_addresses: u64,
    pub live_bytes: u64,
    pub live_allocations: usize,
    pub zones: BTreeMap<u64, ProfileZone>,
    pub unmatched_stop_events: BTreeMap<ProfileEventKey, u64>,
    pub open_events: BTreeMap<ProfileEventKey, usize>,
    pub ambiguous_zones: Vec<u64>,
    pub ambiguous_events: Vec<(u64, u16)>,
    pub duplicate_profile_names: Vec<ProfileNameCollision>,
}

#[derive(Clone, Debug)]
pub struct AnalysisExportReport {
    pub source_blake3: [u8; 32],
    pub source_bytes: u64,
    pub source_frames: usize,
    pub exported_rows: u64,
    pub summary: AnalysisExportSummary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnalysisCsvProgress {
    pub rows_written: u64,
    pub total_rows: u64,
}

#[derive(Clone, Debug)]
pub struct AnalysisCsvReport {
    pub exported_rows: u64,
    pub summary: AnalysisExportSummary,
}

/// Exports the loaded analysis; import compatibility diagnostics still apply to its contents.
/// Only publish the supplied staging writer after success.
pub fn write_analysis_csv<W: Write + ?Sized>(
    recording: &Recording,
    output: &mut W,
    kind: AnalysisCsvKind,
    mut progress: impl FnMut(AnalysisCsvProgress) -> bool,
) -> Result<AnalysisCsvReport, Error> {
    if kind == AnalysisCsvKind::Memory
        && recording.memory_history_scope() == crate::MemoryHistoryScope::AfterInitialSnapshot
    {
        return Err(error(
            "complete memory CSV is unavailable: allocation history before the initial snapshot is unknown",
        ));
    }
    if recording.source == crate::RecordingSource::ProjectCapture {
        return Err(error(
            "complete analysis CSV is unavailable for a retained project capture",
        ));
    }
    if let Some(diagnostic) = recording.diagnostics.iter().find(|diagnostic| {
        diagnostic.starts_with("Analysis decoding stopped at frame")
            || (diagnostic.starts_with("PVD3 initial snapshot restored")
                && diagnostic.contains("analysis contains only subsequent history events"))
    }) {
        return Err(error(format!(
            "complete analysis CSV is unavailable: {diagnostic}"
        )));
    }
    if recording.source == crate::RecordingSource::Pvd3Database && recording.is_disk_backed() {
        return write_native_history_csv(recording, output, kind, progress);
    }
    let analysis = &recording.analysis;
    let memory = analysis.memory_snapshot();
    if analysis.profile_summary().first_retained_frame != 0 {
        return Err(error(
            "complete analysis CSV requires the original history, not a retained window",
        ));
    }
    let total_rows = match kind {
        AnalysisCsvKind::Profile => analysis.profile_spans().len(),
        AnalysisCsvKind::Memory => memory.sample_count(),
    } as u64;
    let mut status = AnalysisCsvProgress {
        rows_written: 0,
        total_rows,
    };
    let check = |progress: &mut dyn FnMut(AnalysisCsvProgress) -> bool,
                 status|
     -> Result<(), Error> {
        if progress(status) {
            Ok(())
        } else {
            Err(io::Error::new(io::ErrorKind::Interrupted, "analysis CSV export cancelled").into())
        }
    };
    check(&mut progress, status)?;
    let summary = summarize(
        analysis,
        memory.summary(),
        analysis.profile_spans().len() as u64,
        analysis.profile_values().len() as u64,
    )?;
    check(&mut progress, status)?;
    write_header(output, kind)?;
    let ratio = timestamp_ratio(&recording.header);
    match kind {
        AnalysisCsvKind::Profile => {
            for span in analysis.profile_spans() {
                if status.rows_written.is_multiple_of(256) {
                    check(&mut progress, status)?;
                }
                write_span(output, span, &summary.zones, ratio)?;
                status.rows_written += 1;
            }
        }
        AnalysisCsvKind::Memory => {
            for index in 0..memory.sample_count() {
                if status.rows_written.is_multiple_of(256) {
                    check(&mut progress, status)?;
                }
                let sample = memory
                    .sample_at(index)
                    .ok_or_else(|| error("analysis CSV memory sample is unavailable"))?;
                write_sample(output, &sample)?;
                status.rows_written += 1;
            }
        }
    }
    check(&mut progress, status)?;
    Ok(AnalysisCsvReport {
        exported_rows: status.rows_written,
        summary,
    })
}

fn write_native_history_csv<W: Write + ?Sized>(
    recording: &Recording,
    output: &mut W,
    kind: AnalysisCsvKind,
    mut progress: impl FnMut(AnalysisCsvProgress) -> bool,
) -> Result<AnalysisCsvReport, Error> {
    crate::native::analysis_history::validate_complete(recording)?;
    let mut status = AnalysisCsvProgress {
        rows_written: 0,
        total_rows: 0,
    };
    let mut spans = 0u64;
    let mut values = 0u64;
    let mut samples = 0u64;
    for frame in 0..=recording.frame_count() {
        let analysis = native_frame_analysis(recording, frame, &mut progress, status)?;
        spans = spans
            .checked_add(frame_span_range(&analysis, frame)?.len() as u64)
            .ok_or_else(|| error("native profile CSV row count overflow"))?;
        values = values
            .checked_add(
                analysis
                    .profile_values()
                    .rev()
                    .take_while(|value| value.received_frame >= frame)
                    .filter(|value| value.received_frame == frame)
                    .count() as u64,
            )
            .ok_or_else(|| error("native profile value count overflow"))?;
        samples = samples
            .checked_add(frame_sample_range(&analysis.memory_snapshot(), frame)?.len() as u64)
            .ok_or_else(|| error("native memory CSV row count overflow"))?;
    }
    status.total_rows = match kind {
        AnalysisCsvKind::Profile => spans,
        AnalysisCsvKind::Memory => samples,
    };
    let analysis_end = crate::native::analysis_history::end_analysis(recording)?;
    let summary = summarize(
        analysis_end,
        analysis_end.memory_snapshot().summary(),
        spans,
        values,
    )?;
    write_header(output, kind)?;
    let ratio = timestamp_ratio(&recording.header);
    for frame in 0..=recording.frame_count() {
        let analysis = native_frame_analysis(recording, frame, &mut progress, status)?;
        match kind {
            AnalysisCsvKind::Profile => {
                let range = frame_span_range(&analysis, frame)?;
                for index in range {
                    if status.rows_written.is_multiple_of(256) {
                        check_csv_progress(&mut progress, status)?;
                    }
                    let span = analysis
                        .profile_span(index)
                        .ok_or_else(|| error("native profile CSV span is unavailable"))?;
                    write_span(output, span, &summary.zones, ratio)?;
                    status.rows_written += 1;
                }
            }
            AnalysisCsvKind::Memory => {
                let memory = analysis.memory_snapshot();
                for index in frame_sample_range(&memory, frame)? {
                    if status.rows_written.is_multiple_of(256) {
                        check_csv_progress(&mut progress, status)?;
                    }
                    let sample = memory
                        .sample_at(index)
                        .ok_or_else(|| error("native memory CSV sample is unavailable"))?;
                    write_sample(output, &sample)?;
                    status.rows_written += 1;
                }
            }
        }
    }
    if status.rows_written != status.total_rows {
        return Err(error("native analysis CSV row count changed during export"));
    }
    check_csv_progress(&mut progress, status)?;
    Ok(AnalysisCsvReport {
        exported_rows: status.rows_written,
        summary,
    })
}

fn native_frame_analysis(
    recording: &Recording,
    frame: usize,
    progress: &mut dyn FnMut(AnalysisCsvProgress) -> bool,
    status: AnalysisCsvProgress,
) -> Result<crate::analysis::SharedAnalysis, Error> {
    check_csv_progress(progress, status)?;
    crate::native::analysis_history::frame_analysis(recording, frame, &mut || !progress(status))
}

fn check_csv_progress(
    progress: &mut dyn FnMut(AnalysisCsvProgress) -> bool,
    status: AnalysisCsvProgress,
) -> Result<(), Error> {
    if !progress(status) {
        return Err(
            io::Error::new(io::ErrorKind::Interrupted, "analysis CSV export cancelled").into(),
        );
    }
    Ok(())
}

fn frame_span_range(
    analysis: &crate::analysis::SharedAnalysis,
    frame: usize,
) -> Result<std::ops::Range<usize>, Error> {
    crate::native::analysis_history::span_range(analysis, frame)
}

fn frame_sample_range(
    memory: &impl MemorySource,
    frame: usize,
) -> Result<std::ops::Range<usize>, Error> {
    crate::native::analysis_history::row_range(memory.sample_count(), frame, |index| {
        memory.sample_at(index).map(|sample| sample.received_frame)
    })
}

/// Exports completed profile spans or memory flush samples from the complete SDK stream.
/// Only publish the supplied staging writer after success; errors can leave partial CSV bytes.
pub fn export_analysis_csv_streaming<R: Read + Seek, W: Write + ?Sized>(
    source: &mut R,
    output: &mut W,
    kind: AnalysisCsvKind,
    mut progress: impl FnMut(AnalysisExportProgress) -> bool,
) -> Result<AnalysisExportReport, Error> {
    let inspected = scan::<R, W>(
        source,
        None,
        ScanKind::Csv(kind),
        None,
        AnalysisExportPhase::Inspect,
        &mut progress,
        None,
    )?;
    let exported = scan(
        source,
        Some(output),
        ScanKind::Csv(kind),
        Some(&inspected.summary.zones),
        AnalysisExportPhase::Export,
        &mut progress,
        None,
    )?;
    if inspected.source_blake3 != exported.source_blake3
        || inspected.source_bytes != exported.source_bytes
        || inspected.source_frames != exported.source_frames
    {
        return Err(error(
            "source capture changed between analysis export scans",
        ));
    }
    Ok(exported)
}

struct Exporter<'a, W: Write + ?Sized> {
    observer: Option<&'a mut dyn ProfileObserver>,
    output: Option<&'a mut W>,
    kind: AnalysisCsvKind,
    names: Option<&'a BTreeMap<u64, ProfileZone>>,
    ratio: f64,
    rows: u64,
    spans: u64,
    values: u64,
    retained_baseline: usize,
}

impl<W: Write + ?Sized> Exporter<'_, W> {
    fn drain(&mut self, analysis: &mut Analysis) -> Result<(), Error> {
        if let Some(observer) = &mut self.observer {
            observer.batch(analysis)?;
        }
        self.spans = self
            .spans
            .checked_add(analysis.spans.len() as u64)
            .ok_or_else(|| error("profile span count overflow"))?;
        self.values = self
            .values
            .checked_add(analysis.values.len() as u64)
            .ok_or_else(|| error("profile value count overflow"))?;
        let rows = match self.kind {
            AnalysisCsvKind::Profile => analysis.spans.len(),
            AnalysisCsvKind::Memory => analysis.memory.samples.len(),
        };
        self.rows = self
            .rows
            .checked_add(rows as u64)
            .ok_or_else(|| error("CSV row count overflow"))?;
        if let Some(output) = &mut self.output {
            match self.kind {
                AnalysisCsvKind::Profile => {
                    let names = self.names.unwrap();
                    for span in &analysis.spans {
                        write_span(output, span, names, self.ratio)?;
                    }
                }
                AnalysisCsvKind::Memory => {
                    for sample in &analysis.memory.samples {
                        write_sample(output, sample)?;
                    }
                }
            }
        }
        let recount = analysis
            .accounted_bytes()
            .saturating_sub(self.retained_baseline)
            >= 16 << 20
            || analysis.accounted_bytes() > 127 << 20;
        analysis.discard_completed_exports(recount);
        if recount {
            self.retained_baseline = analysis.accounted_bytes();
        }
        Ok(())
    }
}

fn csv_cell(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn timestamp_ratio(header: &Header) -> f64 {
    header.timestamp_numerator as f64 / header.timestamp_denominator as f64 / 1_000_000.0
}

fn write_header(output: &mut (impl Write + ?Sized), kind: AnalysisCsvKind) -> io::Result<()> {
    writeln!(
        output,
        "{}",
        match kind {
            AnalysisCsvKind::Profile =>
                "zone,event,thread,context,start_ticks,end_ticks,duration_ms,received_frame",
            AnalysisCsvKind::Memory =>
                "received_frame,live_bytes,live_allocations,total_allocations,total_frees",
        }
    )
}

fn write_span(
    output: &mut (impl Write + ?Sized),
    span: &ProfileSpan,
    names: &BTreeMap<u64, ProfileZone>,
    ratio: f64,
) -> io::Result<()> {
    let zone = names.get(&span.zone);
    let event_name = zone.and_then(|zone| zone.events.get(&span.event));
    let fallback = span.event.to_string();
    writeln!(
        output,
        "{},{},{},{},{},{},{:.9},{}",
        csv_cell(zone.map_or("", |zone| &zone.name)),
        csv_cell(event_name.map_or(&fallback, String::as_str)),
        span.thread,
        span.context,
        span.start,
        span.end,
        (span.end - span.start) as f64 * ratio,
        span.received_frame
    )
}

fn write_sample(output: &mut (impl Write + ?Sized), sample: &MemorySample) -> io::Result<()> {
    writeln!(
        output,
        "{},{},{},{},{}",
        sample.received_frame,
        sample.live_bytes,
        sample.live_allocations,
        sample.allocations,
        sample.frees
    )
}

fn scan<'a, R: Read + Seek + ?Sized, W: Write + ?Sized>(
    source: &mut R,
    mut output: Option<&'a mut W>,
    kind: ScanKind,
    names: Option<&'a BTreeMap<u64, ProfileZone>>,
    phase: AnalysisExportPhase,
    progress: &mut impl FnMut(AnalysisExportProgress) -> bool,
    observer: Option<&'a mut dyn ProfileObserver>,
) -> Result<AnalysisExportReport, Error> {
    let total_bytes = source.seek(SeekFrom::End(0))?;
    source.seek(SeekFrom::Start(0))?;
    let mut status = AnalysisExportProgress {
        phase,
        bytes_read: 0,
        total_bytes,
    };
    check_progress(progress, status)?;
    let profile_only = matches!(kind, ScanKind::ProfileQuery);
    let kind = match kind {
        ScanKind::Csv(kind) => kind,
        ScanKind::ProfileQuery => AnalysisCsvKind::Profile,
    };
    if let Some(output) = &mut output {
        write_header(output, kind)?;
    }
    let mut exporter = Exporter {
        observer,
        output,
        kind,
        names,
        ratio: 0.0,
        rows: 0,
        spans: 0,
        values: 0,
        retained_baseline: 0,
    };
    let mut analysis = Analysis::default();
    let mut builder = None;
    let mut decoder = Decoder::default();
    let mut hash = blake3::Hasher::new();
    let mut buffer = vec![0; 64 << 10];
    loop {
        check_progress(progress, status)?;
        let length = match source.read(&mut buffer) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if length == 0 {
            break;
        }
        status.bytes_read = status
            .bytes_read
            .checked_add(length as u64)
            .ok_or_else(|| error("source capture size overflow"))?;
        if status.bytes_read > total_bytes {
            return Err(error("source capture grew during analysis export"));
        }
        hash.update(&buffer[..length]);
        let groups = decoder.feed(&buffer[..length])?;
        if builder.is_none()
            && let Some(header) = decoder.header()
        {
            exporter.ratio = timestamp_ratio(header);
            builder = Some(validation_builder(header.clone()));
        }
        for group in groups {
            let builder = builder.as_mut().unwrap();
            for (index, event) in group.events.into_iter().enumerate() {
                if index.is_multiple_of(256) {
                    check_progress(progress, status)?;
                    exporter.drain(&mut analysis)?;
                }
                let mut drain = |analysis: &mut Analysis| {
                    check_progress(progress, status)?;
                    exporter.drain(analysis)
                };
                ingest_event(
                    &mut analysis,
                    builder,
                    &event,
                    group.stream_id,
                    profile_only,
                    &mut drain,
                )?;
                if let Some(observer) = &mut exporter.observer {
                    match &event {
                        Event::AddProfileZone { id, .. } => {
                            observer.metadata(&analysis, *id, None)?;
                        }
                        Event::AddProfileZoneEvent { id, event_id, .. } => {
                            observer.metadata(&analysis, *id, Some(*event_id))?;
                        }
                        _ => {}
                    }
                }
                builder.push(EventGroup {
                    stream_id: group.stream_id,
                    timestamp: group.timestamp,
                    offset: group.offset,
                    events: vec![event],
                })?;
            }
        }
    }
    decoder.finish()?;
    if status.bytes_read != total_bytes {
        return Err(error(
            "source capture changed length during analysis export",
        ));
    }
    let builder = builder.ok_or_else(|| error("missing SDK stream initialization"))?;
    if !builder.partial.is_empty() || !builder.groups.is_empty() {
        return Err(error("stream ended inside a property transaction"));
    }
    check_progress(progress, status)?;
    exporter.drain(&mut analysis)?;
    analysis.discard_completed_exports(true);
    if let Some(observer) = &mut exporter.observer {
        observer.finish(&analysis, &builder.recording.header)?;
    }
    let summary = summarize(
        &analysis,
        analysis.memory.as_ref().into(),
        exporter.spans,
        exporter.values,
    )?;
    Ok(AnalysisExportReport {
        source_blake3: *hash.finalize().as_bytes(),
        source_bytes: total_bytes,
        source_frames: builder.validation.as_ref().unwrap().frames,
        exported_rows: exporter.rows,
        summary,
    })
}

fn summarize(
    analysis: &impl ProfileMetadataSource,
    memory: MemorySummary,
    spans: u64,
    values: u64,
) -> Result<AnalysisExportSummary, Error> {
    let duplicate_profile_names =
        name_collisions(analysis.profile_names(), analysis.profile_metadata_budget())?;
    let ambiguous_zones = analysis.profile_ambiguous_zones().collect();
    let ambiguous_events = analysis.profile_ambiguous_events().collect();
    let open_events = analysis.profile_open_counts().collect();
    let mut zones = analysis
        .profile_zones()
        .map(|(id, name)| {
            (
                id,
                ProfileZone {
                    name: name.to_owned(),
                    events: BTreeMap::new(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    for event in analysis.profile_names() {
        zones
            .get_mut(&event.zone)
            .ok_or_else(|| error("analysis CSV profile zone is unavailable"))?
            .events
            .insert(event.event, event.event_name.to_owned());
    }
    let profile = analysis.profile_summary();
    Ok(AnalysisExportSummary {
        profile_spans: spans,
        profile_values: values,
        legacy_cuda_buffers: profile.legacy_cuda_buffers,
        unmatched_stops: profile.unmatched_stops,
        open_spans: profile.open_spans,
        memory_allocations: memory.allocations,
        memory_frees: memory.frees,
        unmatched_frees: memory.unmatched_frees,
        replaced_addresses: memory.replaced_addresses,
        live_bytes: memory.live_bytes,
        live_allocations: memory.live_allocations,
        zones,
        unmatched_stop_events: analysis.unmatched_profile_events().collect(),
        open_events,
        ambiguous_zones,
        ambiguous_events,
        duplicate_profile_names,
    })
}

fn name_collisions<'a>(
    events: impl Iterator<Item = ProfileNames<'a>>,
    available: usize,
) -> Result<Vec<ProfileNameCollision>, Error> {
    let mut remaining = available;
    let mut names = BTreeMap::<(&str, &str), Vec<(u64, u16)>>::new();
    for event in events {
        let key = (event.zone_name, event.event_name);
        if !names.contains_key(&key) {
            charge_name_metadata(&mut remaining, 128)?;
        }
        let keys = names.entry(key).or_default();
        reserve_metadata_slot(keys, &mut remaining)?;
        keys.push((event.zone, event.event));
    }
    let mut strings = BTreeMap::<Arc<str>, Arc<str>>::new();
    let mut collisions = Vec::new();
    for ((zone_name, event_name), keys) in names {
        if keys.len() > 1 {
            reserve_metadata_slot(&mut collisions, &mut remaining)?;
            let zone_name = intern_profile_name(zone_name, &mut strings, &mut remaining)?;
            let event_name = intern_profile_name(event_name, &mut strings, &mut remaining)?;
            collisions.push(ProfileNameCollision {
                zone_name,
                event_name,
                keys,
            });
        }
    }
    Ok(collisions)
}

fn charge_name_metadata(remaining: &mut usize, bytes: usize) -> Result<(), Error> {
    *remaining = remaining
        .checked_sub(bytes)
        .ok_or_else(|| error("profile CSV name collision metadata exceeds its analysis budget"))?;
    Ok(())
}

fn reserve_metadata_slot<T>(values: &mut Vec<T>, remaining: &mut usize) -> Result<(), Error> {
    if values.len() == values.capacity() {
        let capacity = values.capacity().saturating_mul(2).max(4);
        charge_name_metadata(
            remaining,
            (capacity - values.capacity()).saturating_mul(std::mem::size_of::<T>()),
        )?;
        values.reserve_exact(capacity - values.len());
    }
    Ok(())
}

fn intern_profile_name(
    text: &str,
    strings: &mut BTreeMap<Arc<str>, Arc<str>>,
    remaining: &mut usize,
) -> Result<Arc<str>, Error> {
    if let Some(value) = strings.get(text) {
        return Ok(value.clone());
    }
    charge_name_metadata(remaining, text.len().saturating_add(128))?;
    let value: Arc<str> = Arc::from(text);
    strings.insert(value.clone(), value.clone());
    Ok(value)
}

fn validation_builder(header: Header) -> RecordingBuilder {
    let mut builder = RecordingBuilder::new(header);
    builder.validation = Some(ValidationState {
        skip_analysis: true,
        ..Default::default()
    });
    builder
}

fn ingest_event(
    analysis: &mut Analysis,
    builder: &RecordingBuilder,
    event: &Event,
    stream: u64,
    profile_only: bool,
    drain: &mut dyn FnMut(&mut Analysis) -> Result<(), Error>,
) -> Result<(), Error> {
    match event {
        Event::AddProfileZone { id, name } => {
            analysis.add_zone(*id, name.clone())?;
            drain(analysis)
        }
        Event::AddProfileZoneEvent {
            id, name, event_id, ..
        } => {
            analysis.add_event(*id, *event_id, name.clone())?;
            drain(analysis)
        }
        Event::SetPropertyValue {
            id, property, data, ..
        } => ingest_property(analysis, builder, *id, *property, data, profile_only, drain),
        Event::EndSetPropertyValue => {
            if let Some(property) = builder.partial.get(&stream) {
                ingest_property(
                    analysis,
                    builder,
                    property.id,
                    property.property,
                    &property.data,
                    profile_only,
                    drain,
                )?;
            }
            Ok(())
        }
        Event::SetPropertyMessage { id, message, data } => {
            ingest_message(analysis, builder, *id, *message, data, profile_only, drain)
        }
        Event::SendPropertyMessageFromGroup { id, data } => {
            if let Some(message) = builder.groups.get(&stream) {
                ingest_message(analysis, builder, *id, *message, data, profile_only, drain)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn ingest_property(
    analysis: &mut Analysis,
    builder: &RecordingBuilder,
    id: u64,
    property: u32,
    data: &[u8],
    profile_only: bool,
    drain: &mut dyn FnMut(&mut Analysis) -> Result<(), Error>,
) -> Result<(), Error> {
    if builder
        .strings
        .get(&property)
        .is_some_and(|name| name == "events")
        && let Some(kind) = builder.object_classes.get(&id)
        && (!profile_only || kind.ends_with("PxProfileZone"))
    {
        analysis.ingest_for_export(
            id,
            kind,
            data,
            builder.recording.header.endian,
            builder.validation.as_ref().unwrap().frames,
            drain,
        )?;
    }
    Ok(())
}

fn ingest_message(
    analysis: &mut Analysis,
    builder: &RecordingBuilder,
    id: u64,
    message: Name,
    data: &[u8],
    profile_only: bool,
    drain: &mut dyn FnMut(&mut Analysis) -> Result<(), Error>,
) -> Result<(), Error> {
    if builder.object_classes.get(&id).is_none_or(|kind| {
        !kind.ends_with("PxProfileZone")
            && (profile_only || !kind.ends_with("PxProfileMemoryEventBuffer"))
    }) {
        return Ok(());
    }
    if let Some(message) = builder.messages.get(&message) {
        for arg in message.args.iter() {
            if builder.name(arg.datatype)?.ends_with("::String") {
                continue;
            }
            let end = (arg.offset as usize)
                .checked_add(arg.size as usize)
                .ok_or_else(|| error("property message range overflow"))?;
            let bytes = data
                .get(arg.offset as usize..end)
                .ok_or_else(|| error("property message exceeds its payload"))?;
            ingest_property(
                analysis,
                builder,
                id,
                arg.property,
                bytes,
                profile_only,
                drain,
            )?;
        }
    }
    Ok(())
}

fn check_progress(
    progress: &mut impl FnMut(AnalysisExportProgress) -> bool,
    status: AnalysisExportProgress,
) -> Result<(), Error> {
    if !progress(status) {
        return Err(
            io::Error::new(io::ErrorKind::Interrupted, "analysis CSV export cancelled").into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collisions_share_names_and_respect_metadata_budget() {
        let name = "same zone ".repeat(1000);
        let zone = ProfileZone {
            name,
            events: [
                (1, "a".to_owned()),
                (2, "a".to_owned()),
                (3, "b".to_owned()),
                (4, "b".to_owned()),
            ]
            .into(),
        };
        let mut analysis = Analysis::default();
        analysis.zones = [(10, zone.clone()), (20, zone)].into();
        let collisions = name_collisions(analysis.profile_names(), 16 << 10).unwrap();
        assert_eq!(collisions.len(), 2);
        assert_eq!(collisions[0].keys, [(10, 1), (10, 2), (20, 1), (20, 2)]);
        assert_eq!(collisions[1].keys, [(10, 3), (10, 4), (20, 3), (20, 4)]);
        assert!(Arc::ptr_eq(
            &collisions[0].zone_name,
            &collisions[1].zone_name
        ));
        for available in [0, 128, 1024] {
            assert!(
                name_collisions(analysis.profile_names(), available)
                    .unwrap_err()
                    .to_string()
                    .contains("analysis budget")
            );
        }
    }
}
