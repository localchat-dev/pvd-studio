use pvd_core::analysis::view::{MemorySource, ProfileSource};
mod atomic_output;
mod cancellation;
mod profile_capture;
mod profile_pages;
mod project_capture;

use std::collections::BTreeMap;
use std::error::Error;
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use pvd_core::Recording;
use pvd_protocol::{Decoder, Event};
use pvd_transport::{CaptureServer, ListenConfig, ReceiverEvent};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("inspect") if args.len() == 2 => inspect(&args[1]),
        Some("errors") if args.len() == 2 => errors(&args[1]),
        Some("verify") if args.len() == 2 => verify(&args[1]),
        Some("bench") if (2..=3).contains(&args.len()) => bench(
            &args[1],
            args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(1000),
        ),
        Some("frame") if args.len() == 3 => frame(&args[1], args[2].parse()?),
        Some("trim") if args.len() == 5 => {
            trim(&args[1], args[2].parse()?, args[3].parse()?, &args[4])
        }
        Some("repx") if (4..=5).contains(&args.len()) => repx(
            &args[1],
            args[2].parse()?,
            &args[3],
            args.get(4).map(|s| parse_id(s)).transpose()?,
        ),
        Some("csv") if args.len() == 5 => csv(&args[1], parse_id(&args[2])?, &args[3], &args[4]),
        Some("profile-csv") if args.len() == 3 => profile_csv(&args[1], &args[2]),
        Some("profile-events") if args.len() == 2 => profile_events(&args[1]),
        Some("profile-events-page") if (3..=4).contains(&args.len()) => {
            profile_pages::run(&args[1], args[2].parse()?, args.get(3).map(String::as_str))
        }
        Some("profile-event-csv") if args.len() == 6 => profile_event_csv(
            &args[1],
            ProfileSelection::Series(args[2].parse()?),
            parse_id(&args[3])?,
            args[4].parse()?,
            &args[5],
        ),
        Some("profile-key-csv") if args.len() == 9 => profile_event_csv(
            &args[1],
            ProfileSelection::Key(pvd_core::profile_curves::ExactKey {
                zone: parse_id(&args[2])?,
                event: parse_id(&args[3])?.try_into()?,
                thread: parse_id(&args[4])?.try_into()?,
                context: parse_id(&args[5])?,
            }),
            parse_id(&args[6])?,
            args[7].parse()?,
            &args[8],
        ),
        Some("memory-csv") if args.len() == 3 => memory_csv(&args[1], &args[2]),
        Some("listen") if (3..=4).contains(&args.len()) => listen(
            &args[1],
            &args[2],
            args.get(3).map(|s| s.parse()).transpose()?,
        ),
        Some("help" | "--help" | "-h") | None => {
            usage();
            Ok(())
        }
        _ => {
            usage();
            Err("invalid command or arguments".into())
        }
    }
}

fn usage() {
    println!(
        "pvd-studio diagnostic CLI

  pvd-cli inspect <capture>
  pvd-cli errors <capture>
  pvd-cli verify <capture>
  pvd-cli bench <capture> [random-seeks]
  pvd-cli frame <capture> <absolute-frame>
  pvd-cli csv <capture> <object-id> <property> <output.csv>
  pvd-cli profile-csv <capture> <output.csv>
  pvd-cli profile-events <capture>
  pvd-cli profile-events-page <sdk-capture> <limit:1..4096> [cursor]
  pvd-cli profile-event-csv <capture> <series-index> <scene-id> <scene-generation> <output.csv>
  pvd-cli profile-key-csv <capture> <zone-id> <event-id> <thread-id> <context-id> <scene-id> <scene-generation> <output.csv>
  pvd-cli memory-csv <capture> <output.csv>
  pvd-cli trim <capture> <first-frame> <last-frame> <output.pvdstream>
  pvd-cli repx <capture> <frame> <output.repx> [actor-id]
  pvd-cli listen <ip:port> <capture-directory> [timeout-seconds]

verify compares independent sequential reconstruction with checkpoint seeks.
bench measures file indexing and deterministic random seeks in this build.
Frame numbers are zero-based original indices; project captures preserve their visible absolute range.
Project captures (.pvdcapture) are not original PVD3 files or complete raw SDK streams.
Full-source CSV/profile catalog/raw trim commands reject project captures. trim includes both endpoints.
errors prints source fields and receive positions; native errors have no timestamp.
received_frame is the number of completed scene frames, not an error timestamp.
profile-events lists exact event keys ordered by first recorded start tick.
profile-events-page scans the complete SDK source for each identity-ordered page;
its next_cursor binds the file hash/size/frame count. page_row is not a series index.
profile-event-csv uses that capture's zero-based series index and at most 5000 samples.
profile-key-csv selects the complete exact key without building the full SDK catalog;
its CSV series_index is empty. Both commands validate the full source before export.
Event durations align to the chosen scene's time windows, not scene ownership;
nested/overlapping spans are summed. Missing or ambiguous values remain empty.
listen captures one real SDK connection and exits after disconnect. An optional
timeout stops with a retained .partial file; no events are silently discarded.
Raw SDK captures use .pvdstream until original PVD3 interoperability is verified."
    );
}

fn parse_id(value: &str) -> Result<u64> {
    Ok(
        if let Some(hex) = value
            .strip_prefix("0x")
            .or_else(|| value.strip_prefix("0X"))
        {
            u64::from_str_radix(hex, 16)?
        } else {
            value.parse()?
        },
    )
}

fn trim(path: &str, first: usize, last: usize, output: &str) -> Result<()> {
    project_capture::require_full_source(path)?;
    let cancel = cancellation::Cancellation::install()?;
    if std::path::Path::new(output)
        .extension()
        .is_none_or(|e| !e.eq_ignore_ascii_case("pvdstream"))
    {
        return Err(
            "scene trimming writes .pvdstream SDK data; native PVD3 containers are not verified"
                .into(),
        );
    }
    let mut source = File::open(path)?;
    let mut previous = None;
    let mut updated = Instant::now();
    let report = atomic_output::write_new_cancellable(output, &cancel, |destination| {
        pvd_core::clip::trim_scene_frames_streaming(
            &mut source,
            destination,
            first,
            last,
            |progress| {
                if previous != Some(progress.phase) || updated.elapsed() >= Duration::from_secs(1) {
                    let phase = match progress.phase {
                        pvd_core::clip::TrimPhase::Inspect => "inspecting",
                        pvd_core::clip::TrimPhase::Export => "exporting and verifying",
                    };
                    eprintln!(
                        "{phase}: {} / {} bytes",
                        progress.bytes_read, progress.total_bytes
                    );
                    previous = Some(progress.phase);
                    updated = Instant::now();
                }
                !cancel.requested()
            },
        )
    })?;
    println!(
        "scene_frames: {}\nbytes: {}\nfile: {output}\ninitialization_and_analysis_preroll: retained\noriginal_pvd3_readability: unverified",
        report.output_frames, report.output_bytes
    );
    Ok(())
}

fn repx(path: &str, frame: usize, output: &str, actor: Option<u64>) -> Result<()> {
    let recording = Recording::from_path(path)?;
    let state = recording.state_at(frame)?;
    let selection = actor.map_or(
        pvd_core::repx::Selection::Frame,
        pvd_core::repx::Selection::Actor,
    );
    let export = pvd_core::repx::export(&state, selection)?;
    atomic_output::write_bytes_new(output, export.xml.as_bytes())?;
    println!(
        "file: {output}\nactors: {}\nshapes: {}\nmaterials: {}\njoints: {}\nformat: RepX validated snapshot subset, PhysX 3.4.2 schema",
        export.actors, export.shapes, export.materials, export.joints
    );
    Ok(())
}

fn verify(path: &str) -> Result<()> {
    let started = Instant::now();
    let recording = Recording::from_path(path)?;
    let indexed = started.elapsed();
    recording.verify_replay()?;
    project_capture::print_range(&recording);
    if recording.frame_count() == 0 {
        return Err("no completed scene frames; playback verification is not applicable".into());
    }
    println!(
        "verified_frames: {}\nstate_changes: {}\nindex_seconds: {:.6}\nverify_seconds: {:.6}",
        recording
            .frame_count()
            .saturating_sub(recording.first_frame()),
        recording.event_count(),
        indexed.as_secs_f64(),
        started.elapsed().saturating_sub(indexed).as_secs_f64()
    );
    print_recording_error_summary(&recording);
    print_history_memory_usage(&recording);
    for index in project_capture::sample_frames(&recording)? {
        let state = recording.state_at(index)?;
        let scene = state.render_scene();
        println!(
            "frame {index}: objects={}, render_shapes={}, sdk_timed_errors_in_state={}",
            state.objects.len(),
            scene.objects.len(),
            state.errors.len()
        );
        for diagnostic in scene.diagnostics {
            println!("display_diagnostic: {diagnostic}");
        }
    }
    for diagnostic in &recording.diagnostics {
        println!("diagnostic: {diagnostic}");
    }
    Ok(())
}

fn bench(path: &str, seeks: usize) -> Result<()> {
    if !(1..=100_000).contains(&seeks) {
        return Err("random-seeks must be between 1 and 100000".into());
    }
    let started = Instant::now();
    let recording = Recording::from_path(path)?;
    let indexed = started.elapsed();
    if recording.frame_count() == 0 {
        return Err("no completed scene frames".into());
    }
    let mut random = 0x5056445354554449u64;
    let mut timings = Vec::with_capacity(seeks);
    let mut object_total = 0usize;
    for _ in 0..seeks {
        random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
        let frame = recording.first_frame()
            + (random % (recording.frame_count() - recording.first_frame()) as u64) as usize;
        let started = Instant::now();
        let state = std::hint::black_box(recording.state_at(frame)?);
        timings.push(started.elapsed().as_secs_f64() * 1000.0);
        object_total += state.objects.len();
    }
    timings.sort_by(f64::total_cmp);
    println!(
        "file: {path}\ninput_bytes: {}\nframes: {}\nstate_changes: {}\nindex_seconds: {:.6}\nrandom_seeks: {seeks}\nseek_mean_ms: {:.6}\nseek_p50_ms: {:.6}\nseek_p95_ms: {:.6}\nseek_max_ms: {:.6}\nobserved_object_total: {object_total}\nbuild: {}",
        std::fs::metadata(path)?.len(),
        recording
            .frame_count()
            .saturating_sub(recording.first_frame()),
        recording.event_count(),
        indexed.as_secs_f64(),
        timings.iter().sum::<f64>() / seeks as f64,
        timings[(seeks - 1) / 2],
        timings[(seeks - 1) * 95 / 100],
        timings[seeks - 1],
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    print_history_memory_usage(&recording);
    Ok(())
}

fn frame(path: &str, index: usize) -> Result<()> {
    let recording = Recording::from_path(path)?;
    project_capture::print_range(&recording);
    let state = recording.state_at(index)?;
    println!(
        "frame: {index}\nseconds: {:.9}\nsdk_timed_errors_in_state: {}",
        recording.seconds_at(index).unwrap_or(0.0),
        state.errors.len(),
    );
    print_recording_error_summary(&recording);
    for object in state.objects.values() {
        println!(
            "0x{:X} generation={} {}",
            object.id, object.generation, object.type_name
        );
        for (name, value) in &object.properties {
            println!("  {name}: {value}");
        }
    }
    for diagnostic in &recording.diagnostics {
        println!("diagnostic: {diagnostic}");
    }
    Ok(())
}

fn csv_cell(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn report_diagnostics(recording: &Recording) {
    for diagnostic in &recording.diagnostics {
        eprintln!("diagnostic: {diagnostic}");
    }
}

fn print_recording_error_summary(recording: &Recording) {
    println!(
        "recording_timed_errors: {}\nrecording_untimed_errors: {}",
        recording.error_events().count(),
        recording.untimed_error_events().len(),
    );
    if !recording.untimed_error_events().is_empty() {
        println!(
            "untimed_error_scope: recording receive order; not assigned to a frame; use errors for details"
        );
    }
}

fn print_history_memory_usage(recording: &Recording) {
    let usage = recording.history_memory_usage();
    println!(
        "estimated_history_bytes: {}\nestimated_checkpoint_bytes: {}\nhistory_budget_bytes: {}\ncheckpoint_count: {}\nretained_state_changes: {}\nskipped_unchanged_updates: {}\nmemory_scope: indexer budget estimate, not process RSS",
        usage.estimated_history_bytes,
        usage.estimated_checkpoint_bytes,
        usage.limit_bytes,
        usage.checkpoint_count,
        usage.retained_actions,
        usage.skipped_redundant_actions,
    );
}

fn errors(path: &str) -> Result<()> {
    let recording = Recording::from_path(path)?;
    write_errors(&mut std::io::stdout().lock(), &recording)?;
    report_diagnostics(&recording);
    Ok(())
}

fn error_receive_position(received_frame: usize, frame_count: usize) -> &'static str {
    if frame_count == 0 {
        "no-completed-frames"
    } else if received_frame == frame_count {
        "after-final"
    } else if received_frame == 0 {
        "before-first-completed-frame"
    } else {
        "before-next-completed-frame"
    }
}

fn write_untimed_error(
    output: &mut impl Write,
    error: &pvd_core::UntimedErrorEvent,
    frame_count: usize,
) -> std::io::Result<()> {
    writeln!(
        output,
        "native_error:\n  code: {}\n  message: {:?}\n  file: {:?}\n  line: {}\n  index: {}\n  timestamp_ticks: unknown\n  history_event: {}\n  received_frame: {}\n  receive_position: {}",
        error.code,
        error.message,
        error.file,
        error.line,
        error.index,
        error.history_event,
        error.received_frame,
        error_receive_position(error.received_frame, frame_count),
    )
}

fn write_errors(output: &mut impl Write, recording: &Recording) -> std::io::Result<()> {
    let timed_count = recording.error_events().count();
    writeln!(
        output,
        "timed_errors: {}\nuntimed_errors: {}\ncompleted_frames: {}\nreceived_frame_semantics: completed_frame_count\naffected_actor_id: unavailable",
        timed_count,
        recording.untimed_error_events().len(),
        recording.frame_count(),
    )?;
    if timed_count > 0 {
        writeln!(
            output,
            "sdk_timestamp_ratio: {}/{}\nsdk_timestamp_source: event-group",
            recording.header.timestamp_numerator, recording.header.timestamp_denominator,
        )?;
    }
    for (frame, error) in recording.error_events() {
        let received_frame = frame.unwrap_or(recording.frame_count());
        writeln!(
            output,
            "sdk_error:\n  code: {}\n  message: {:?}\n  file: {:?}\n  line: {}\n  timestamp_ticks: {}\n  received_frame: {}\n  receive_position: {}",
            error.code,
            error.message,
            error.file,
            error.line,
            error.timestamp,
            received_frame,
            error_receive_position(received_frame, recording.frame_count()),
        )?;
    }
    for error in recording.untimed_error_events() {
        write_untimed_error(output, error, recording.frame_count())?;
    }
    Ok(())
}

fn profile_events(path: &str) -> Result<()> {
    let cancel = cancellation::Cancellation::install()?;
    let catalog = profile_capture::catalog(path, cancel.flag())?;
    println!(
        "series_index,zone_id,event_id,thread_id,context_id,zone_name,event_name,first_start_tick,complete_spans,open_spans,unmatched_stops,metadata_ambiguous"
    );
    for (index, entry) in catalog.entries.iter().enumerate() {
        cancel.check()?;
        let key = entry.key;
        let zone = (!entry.metadata_ambiguous)
            .then(|| catalog.zones.get(&key.zone))
            .flatten();
        println!(
            "{index},0x{:x},{},{},0x{:x},{},{},{},{},{},{},{}",
            key.zone,
            key.event,
            key.thread,
            key.context,
            csv_cell(zone.map_or("", |zone| zone.name.as_str())),
            csv_cell(
                zone.and_then(|zone| zone.events.get(&key.event))
                    .map_or("", String::as_str)
            ),
            entry
                .first_start
                .map(|tick| tick.to_string())
                .unwrap_or_default(),
            entry.complete_spans,
            entry.open_spans,
            entry.unmatched_stops,
            entry.metadata_ambiguous,
        );
    }
    Ok(())
}

enum ProfileSelection {
    Series(usize),
    Key(pvd_core::profile_curves::ExactKey),
}

fn profile_event_csv(
    path: &str,
    selection: ProfileSelection,
    scene: u64,
    generation: u32,
    output: &str,
) -> Result<()> {
    use pvd_core::profile_curves::{self, ReferenceScene};
    let cancel = cancellation::Cancellation::install()?;
    let mut capture = profile_capture::Capture::open(path, cancel.flag())?;
    report_diagnostics(&capture.recording);
    if capture.recording.frame_count() == 0 {
        return Err(
            "no completed scene frames; event curves require a reference scene timeline".into(),
        );
    }
    let (catalog, index, series) = match selection {
        ProfileSelection::Series(series) => {
            (capture.catalog(cancel.flag())?, series, series.to_string())
        }
        ProfileSelection::Key(key) => (capture.catalog_key(key, cancel.flag())?, 0, String::new()),
    };
    let entry = catalog.entries.get(index).ok_or(match selection {
        ProfileSelection::Series(_) => {
            "series index is out of range; use profile-events on this capture"
        }
        ProfileSelection::Key(_) => "the exact profile key is absent from this capture",
    })?;
    let key = entry.key;
    let source_frames = capture.recording.frame_count();
    let reference = ReferenceScene {
        id: scene,
        generation,
    };
    let sampling = profile_curves::sampling_frames(&capture.recording, reference, cancel.flag())?;
    let stride = sampling.stride;
    let reference_frames = sampling.reference_frame_count;
    let points = capture.sample(key, reference, &sampling.frames, cancel.flag())?;
    let zone = (!entry.metadata_ambiguous)
        .then(|| catalog.zones.get(&key.zone))
        .flatten();
    let zone_name = csv_cell(zone.map_or("", |zone| zone.name.as_str()));
    let event_name = csv_cell(
        zone.and_then(|zone| zone.events.get(&key.event))
            .map_or("", String::as_str),
    );
    atomic_output::write_new_cancellable(output, &cancel, |file| {
        writeln!(
            file,
            "series_index,zone_id,event_id,thread_id,context_id,zone_name,event_name,reference_scene,reference_scene_generation,frame,value_ms,occurrence_count,status,aggregation,association,stride,captured_frame_count,stride_basis,reference_frame_count"
        )?;
        for point in &points {
            cancel.check()?;
            writeln!(
                file,
                "{series},0x{:x},{},{},0x{:x},{zone_name},{event_name},0x{scene:x},{generation},{},{},{},{},inclusive_sum,reference_scene_time_window,{stride},{source_frames},reference_scene_generation_frames,{reference_frames}",
                key.zone,
                key.event,
                key.thread,
                key.context,
                point.frame,
                point
                    .milliseconds
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                point.occurrence_count,
                point.status.name(),
            )?;
        }
        Ok(())
    })?;
    println!(
        "exported_rows: {}\nstride: {stride}\nstride_basis: reference scene generation frames\nreference_frame_count: {reference_frames}\ncaptured_frame_count: {source_frames}\nfile: {output}\naggregation: inclusive sum; nested/overlapping spans are added, not CPU utilization\nassociation: reference scene time windows, not event scene ownership",
        points.len(),
    );
    Ok(())
}

fn csv(path: &str, id: u64, property: &str, output: &str) -> Result<()> {
    project_capture::require_full_source(path)?;
    let recording = Recording::from_path(path)?;
    report_diagnostics(&recording);
    let rows = atomic_output::write_new(output, |file| {
        writeln!(
            file,
            "frame,time_seconds,object_id,generation,property,value"
        )?;
        let mut rows = 0;
        for frame in 0..recording.frame_count() {
            let state = recording.state_at(frame)?;
            if let Some(object) = state.objects.get(&id)
                && let Some(value) = object.properties.get(property)
            {
                writeln!(
                    file,
                    "{frame},{:.9},0x{id:X},{},{},{}",
                    recording.seconds_at(frame).unwrap_or(0.0),
                    object.generation,
                    csv_cell(property),
                    csv_cell(&format!("{value:?}"))
                )?;
                rows += 1;
            }
        }
        Ok(rows)
    })?;
    println!("exported_rows: {rows}\nfile: {output}");
    Ok(())
}

fn profile_csv(path: &str, output: &str) -> Result<()> {
    analysis_csv(
        path,
        output,
        pvd_core::analysis_export::AnalysisCsvKind::Profile,
    )
}

fn memory_csv(path: &str, output: &str) -> Result<()> {
    analysis_csv(
        path,
        output,
        pvd_core::analysis_export::AnalysisCsvKind::Memory,
    )
}

fn analysis_csv(
    path: &str,
    output: &str,
    kind: pvd_core::analysis_export::AnalysisCsvKind,
) -> Result<()> {
    project_capture::require_full_source(path)?;
    if !is_native_capture(path)? {
        return analysis_csv_streaming(path, output, kind);
    }
    let recording = Recording::from_path(path)?;
    report_diagnostics(&recording);
    let cancel = cancellation::Cancellation::install()?;
    let mut updated = Instant::now();
    let report = atomic_output::write_new_cancellable(output, &cancel, |destination| {
        pvd_core::analysis_export::write_analysis_csv(&recording, destination, kind, |progress| {
            if progress.rows_written == 0
                || progress.rows_written == progress.total_rows
                || updated.elapsed() >= Duration::from_secs(1)
            {
                eprintln!(
                    "exporting loaded analysis: {} / {} rows",
                    progress.rows_written, progress.total_rows
                );
                updated = Instant::now();
            }
            !cancel.requested()
        })
    })?;
    println!("exported_rows: {}\nfile: {output}", report.exported_rows);
    report_analysis_summary(&report.summary, kind);
    Ok(())
}

fn is_native_capture(path: &str) -> Result<bool> {
    let mut signature = [0; 4];
    File::open(path)?.read_exact(&mut signature)?;
    Ok(signature == pvd_core::native::MAGIC.to_le_bytes())
}

fn analysis_csv_streaming(
    path: &str,
    output: &str,
    kind: pvd_core::analysis_export::AnalysisCsvKind,
) -> Result<()> {
    use pvd_core::analysis_export::{AnalysisExportPhase, export_analysis_csv_streaming};
    let cancel = cancellation::Cancellation::install()?;
    let mut source = File::open(path)?;
    let mut previous = None;
    let mut updated = Instant::now();
    let report = atomic_output::write_new_cancellable(output, &cancel, |destination| {
        export_analysis_csv_streaming(&mut source, destination, kind, |progress| {
            if previous != Some(progress.phase) || updated.elapsed() >= Duration::from_secs(1) {
                let phase = match progress.phase {
                    AnalysisExportPhase::Inspect => "inspecting",
                    AnalysisExportPhase::Export => "exporting and verifying",
                };
                eprintln!(
                    "{phase}: {} / {} bytes",
                    progress.bytes_read, progress.total_bytes
                );
                previous = Some(progress.phase);
                updated = Instant::now();
            }
            !cancel.requested()
        })
    })?;
    println!(
        "exported_rows: {}\nfile: {output}\nsource_bytes: {}\ncompleted_frames: {}",
        report.exported_rows, report.source_bytes, report.source_frames
    );
    report_analysis_summary(&report.summary, kind);
    Ok(())
}

fn report_analysis_summary(
    summary: &pvd_core::analysis_export::AnalysisExportSummary,
    kind: pvd_core::analysis_export::AnalysisCsvKind,
) {
    println!(
        "profile_values: {}\nlegacy_cuda_buffers: {}\nunmatched_stops: {}\nopen_spans: {}\nunmatched_frees: {}\nreplaced_addresses: {}",
        summary.profile_values,
        summary.legacy_cuda_buffers,
        summary.unmatched_stops,
        summary.open_spans,
        summary.unmatched_frees,
        summary.replaced_addresses
    );
    println!(
        "decoded_profile_spans: {}\nmemory_allocations: {}\nmemory_frees: {}\nlive_bytes: {}\nlive_allocations: {}",
        summary.profile_spans,
        summary.memory_allocations,
        summary.memory_frees,
        summary.live_bytes,
        summary.live_allocations
    );
    for ((zone, event, thread, context), count) in &summary.unmatched_stop_events {
        let name = summary
            .zones
            .get(zone)
            .and_then(|zone| zone.events.get(event));
        println!("unmatched_stop: {name:?} thread={thread} context={context} count={count}");
    }
    for ((zone, event, thread, context), count) in &summary.open_events {
        let name = summary
            .zones
            .get(zone)
            .and_then(|zone| zone.events.get(event));
        println!("open_span: {name:?} thread={thread} context={context} count={count}");
    }
    if !summary.ambiguous_zones.is_empty() || !summary.ambiguous_events.is_empty() {
        eprintln!(
            "Analysis names changed within the recording; CSV uses final metadata. Ambiguous zones: {:?}; ambiguous events: {:?}",
            summary.ambiguous_zones, summary.ambiguous_events
        );
    }
    if matches!(kind, pvd_core::analysis_export::AnalysisCsvKind::Profile)
        && !summary.duplicate_profile_names.is_empty()
    {
        eprintln!(
            "{} groups of distinct event IDs share profile names; names in this CSV do not uniquely identify events.",
            summary.duplicate_profile_names.len()
        );
    }
}

fn inspect(path: &str) -> Result<()> {
    if project_capture::is_capture(path)? {
        return project_capture::inspect(path);
    }
    let started = Instant::now();
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let mut decoder = Decoder::default();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut groups = 0u64;
    let mut events = 0u64;
    let mut frames = 0u64;
    let mut creates = 0u64;
    let mut destroys = 0u64;
    let mut streams = BTreeMap::<u64, u64>::new();
    if length > 512 << 20 {
        return Err("source exceeds the 512 MiB inspection limit".into());
    }
    let mut signature = [0; 4];
    let has_signature = match file.read_exact(&mut signature) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => false,
        Err(error) => return Err(error.into()),
    };
    file.rewind()?;
    if has_signature && signature == pvd_core::native::MAGIC.to_le_bytes() {
        return inspect_native(path, length, started);
    }
    let mut loaded = 0u64;
    let mut text_bytes = 0usize;
    let mut handles = BTreeMap::<u32, std::sync::Arc<str>>::new();
    let mut classes = BTreeMap::<std::sync::Arc<str>, u64>::new();
    let mut errors = Vec::new();
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        loaded += count as u64;
        if loaded > 512 << 20 {
            return Err("source exceeds the 512 MiB inspection limit".into());
        }
        for group in decoder.feed(&buffer[..count])? {
            groups += 1;
            *streams.entry(group.stream_id).or_default() += 1;
            for event in group.events {
                events += 1;
                match event {
                    Event::StringHandle { handle, value } => {
                        text_bytes = text_bytes.saturating_add(value.len());
                        handles.insert(handle, value.into());
                    }
                    Event::CreateInstance { class, .. } => {
                        creates += 1;
                        let name = handles
                            .get(&class.name)
                            .cloned()
                            .unwrap_or_else(|| format!("handle:{}", class.name).into());
                        *classes.entry(name).or_default() += 1;
                    }
                    Event::DestroyInstance { .. } => destroys += 1,
                    Event::EndSection { name, .. }
                        if handles.get(&name).is_some_and(|s| s.as_ref() == "frame") =>
                    {
                        frames += 1
                    }
                    Event::ErrorMessage {
                        code,
                        message,
                        file,
                        line,
                    } if errors.len() < 100 => {
                        text_bytes = text_bytes
                            .saturating_add(message.len())
                            .saturating_add(file.len());
                        errors.push(format!("{code}: {message} ({file}:{line})"));
                    }
                    _ => {}
                }
                if text_bytes > 64 << 20 || handles.len() + classes.len() + streams.len() > 200_000
                {
                    return Err(
                        "inspection metadata exceeds the 64 MiB text / 200000 entries budget"
                            .into(),
                    );
                }
            }
        }
    }
    decoder.finish()?;
    let header = decoder.header().ok_or("missing stream header")?;
    println!(
        "file: {path}\nbytes: {length}\ncommunication_version: {}\nendian: {:?}\npointer_width: {}\ntimestamp_ratio: {}/{}\ngroups: {groups}\nevents: {events}\nstreams: {}\nframe_sections: {frames}\ninstances_created: {creates}\ninstances_destroyed: {destroys}",
        header.version,
        header.endian,
        if header.flags & 1 == 1 { 64 } else { 32 },
        header.timestamp_numerator,
        header.timestamp_denominator,
        streams.len()
    );
    for (class, count) in classes {
        println!("class {class}: {count}");
    }
    for error in errors {
        println!("sdk_error: {error}");
    }
    println!("parse_seconds: {:.6}", started.elapsed().as_secs_f64());
    Ok(())
}

fn inspect_native(path: &str, length: u64, started: Instant) -> Result<()> {
    let recording = Recording::from_path(path)?;
    print_history_memory_usage(&recording);
    let final_state = if recording.frame_count() == 0 {
        recording.initial_state().clone()
    } else {
        recording.state_at(recording.frame_count() - 1)?
    };
    println!(
        "file: {path}\nbytes: {length}\nformat: NVIDIA PVD3 client database\nobject_ids: container-local\nframes: {}\nstate_changes: {}\nclass_definitions: {}\nlast_frame_objects: {}\nparse_seconds: {:.6}",
        recording.frame_count(),
        recording.event_count(),
        recording.classes.len(),
        final_state.objects.len(),
        started.elapsed().as_secs_f64()
    );
    let profile = recording.analysis.profile_summary();
    let memory = recording.analysis.memory_snapshot();
    let totals = memory.summary();
    println!(
        "profile_spans: {}\nprofile_open_spans: {}\nprofile_unmatched_stops: {}\nmemory_events: {}\nmemory_flushes: {}\nmemory_allocations: {}\nmemory_frees: {}\nfixed_phase_attribution: unavailable",
        profile.complete_spans,
        profile.open_spans,
        profile.unmatched_stops,
        memory.event_count(),
        memory.sample_count(),
        totals.allocations,
        totals.frees,
    );
    write_errors(&mut std::io::stdout().lock(), &recording)?;
    let mut classes = BTreeMap::<&str, usize>::new();
    for object in final_state.objects.values() {
        *classes.entry(&object.type_name).or_default() += 1;
    }
    for (class, count) in classes {
        println!("last_frame_class {class}: {count}");
    }
    for diagnostic in &recording.diagnostics {
        println!("diagnostic: {diagnostic}");
    }
    Ok(())
}

fn listen(address: &str, directory: &str, timeout: Option<u64>) -> Result<()> {
    let config = ListenConfig {
        address: address.parse()?,
        capture_directory: Some(PathBuf::from(directory)),
        ..Default::default()
    };
    let server = CaptureServer::bind(config, || {})?;
    println!("listening: {}", server.address());
    let started = Instant::now();
    let mut decoder = Decoder::default();
    let mut parse_error: Option<String> = None;
    let mut group_count = 0usize;
    loop {
        if timeout.is_some_and(|seconds| started.elapsed() >= Duration::from_secs(seconds)) {
            return Err(
                "capture timed out; any incomplete raw file retains the .partial suffix".into(),
            );
        }
        match server.receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(ReceiverEvent::Listening(_)) => {}
            Ok(ReceiverEvent::Connected { session, peer }) => {
                println!("connected: session={session}, peer={peer}")
            }
            Ok(ReceiverEvent::CaptureStarted { session, path }) => {
                println!(
                    "capture_started: session={session}, partial={}",
                    path.display()
                );
            }
            Ok(ReceiverEvent::Data { bytes, .. }) if parse_error.is_none() => {
                match decoder.feed(&bytes) {
                    Ok(groups) => group_count += groups.len(),
                    Err(error) => {
                        eprintln!("parse diagnostic: {error}; preserving incoming raw bytes");
                        parse_error = Some(error.to_string());
                    }
                }
            }
            Ok(ReceiverEvent::Data { .. }) => {}
            Ok(ReceiverEvent::Disconnected { bytes, capture, .. }) => {
                println!("received_bytes: {bytes}\nparsed_groups: {group_count}");
                if let Some(path) = capture {
                    println!("capture: {}", path.display());
                }
                if let Some(error) = parse_error {
                    return Err(error.into());
                }
                decoder.finish()?;
                return Ok(());
            }
            Ok(ReceiverEvent::Failed { message, .. }) => return Err(message.into()),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(test)]
mod error_tests {
    use super::*;
    use pvd_core::{RecordingBuilder, UntimedErrorEvent};
    use pvd_protocol::{Endian, EventGroup, Header};

    fn sdk_errors(with_frame: bool) -> Recording {
        let mut builder = RecordingBuilder::new(Header {
            version: 1,
            timestamp_numerator: 1000,
            timestamp_denominator: 1,
            flags: 1,
            endian: Endian::Little,
        });
        let mut events = vec![
            Event::StringHandle {
                handle: 1,
                value: "frame".into(),
            },
            Event::ErrorMessage {
                code: 4,
                message: "invalid mass\n\u{1b}[31m".into(),
                file: "example.cpp".into(),
                line: 17,
            },
        ];
        if with_frame {
            events.push(Event::EndSection {
                id: 7,
                name: 1,
                timestamp: 999,
            });
        }
        events.push(Event::ErrorMessage {
            code: 8,
            message: "tail warning".into(),
            file: "example.cpp".into(),
            line: 23,
        });
        builder
            .push(EventGroup {
                stream_id: 1,
                timestamp: 12345,
                offset: 0,
                events,
            })
            .unwrap();
        builder.finish().unwrap()
    }

    fn error_text(recording: &Recording) -> String {
        let mut bytes = Vec::new();
        write_errors(&mut bytes, recording).unwrap();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn sdk_errors_preserve_group_ticks_and_receive_boundaries() {
        let output = error_text(&sdk_errors(true));
        assert!(output.contains("timed_errors: 2\nuntimed_errors: 0\ncompleted_frames: 1"));
        assert_eq!(output.matches("timestamp_ticks: 12345").count(), 2);
        assert!(output.contains("sdk_timestamp_source: event-group"));
        assert!(!output.contains("timestamp_ticks: 999"));
        assert!(
            output.contains("received_frame: 0\n  receive_position: before-first-completed-frame")
        );
        assert!(output.contains("received_frame: 1\n  receive_position: after-final"));
        assert!(output.contains("message: \"invalid mass\\n\\u{1b}[31m\""));
        assert!(!output.contains('\u{1b}'));
        assert!(output.contains("affected_actor_id: unavailable"));
        assert!(!output.contains("history_event:"));
    }

    #[test]
    fn errors_without_completed_frames_have_no_final_frame() {
        let output = error_text(&sdk_errors(false));
        assert_eq!(
            output
                .matches("receive_position: no-completed-frames")
                .count(),
            2
        );
        assert!(!output.contains("after-final"));
    }

    #[test]
    fn native_errors_keep_tail_position_and_unknown_timestamp() {
        let mut error = UntimedErrorEvent {
            code: 4,
            message: "invalid mass".into(),
            file: "example.cpp".into(),
            line: 248,
            index: 3,
            history_event: 981,
            received_frame: 120,
            container_instance: 29,
            container_generation: 0,
        };
        let mut output = Vec::new();
        write_untimed_error(&mut output, &error, 120).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("index: 3\n  timestamp_ticks: unknown\n  history_event: 981"));
        assert!(output.contains("received_frame: 120\n  receive_position: after-final"));
        assert!(!output.contains("timestamp_ticks: 0"));
        assert!(!output.contains("actor"));
        error.received_frame = 0;
        let mut output = Vec::new();
        write_untimed_error(&mut output, &error, 0).unwrap();
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("receive_position: no-completed-frames")
        );
    }

    #[test]
    #[ignore = "requires paired rich SDK stream and original PVD3 saved container"]
    fn real_native_and_sdk_error_fields_match_without_fabricated_timing() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../captures/native");
        let native = Recording::from_path(root.join("rich-paired-native.pxd2")).unwrap();
        let raw = Recording::from_path(root.join("rich-paired.pvdstream")).unwrap();
        let native_events = native.untimed_error_events();
        let raw_events: Vec<_> = raw.error_events().collect();
        assert_eq!(native_events.len(), 1);
        assert_eq!(raw_events.len(), 1);
        let native_event = &native_events[0];
        let (raw_frame, raw_event) = raw_events[0];
        assert_eq!(native_event.code, 4);
        assert_eq!(
            native_event.message,
            "PxRigidDynamic::setMass: mass must be non-negative!"
        );
        assert_eq!(native_event.line, 248);
        assert!(native_event.file.ends_with("NpRigidBodyTemplate.h"));
        assert_eq!(native_event.index, 0);
        assert_eq!(native_event.history_event, 981);
        assert_eq!(native_event.received_frame, 30);
        assert_eq!(raw_frame, Some(30));
        assert_eq!(native_event.code, raw_event.code);
        assert_eq!(native_event.message, raw_event.message);
        assert_eq!(native_event.file, raw_event.file);
        assert_eq!(native_event.line, raw_event.line);
        let native_output = error_text(&native);
        let raw_output = error_text(&raw);
        for output in [&native_output, &raw_output] {
            assert!(output.contains("code: 4"));
            assert!(output.contains(&format!("message: {:?}", raw_event.message)));
            assert!(output.contains(&format!("file: {:?}", raw_event.file)));
            assert!(output.contains("line: 248"));
            assert!(
                output.contains(
                    "received_frame: 30\n  receive_position: before-next-completed-frame"
                )
            );
        }
        assert!(native_output.contains("timestamp_ticks: unknown"));
        assert!(native_output.contains("index: 0\n  timestamp_ticks: unknown"));
        assert!(native_output.contains("history_event: 981"));
        assert!(!raw_output.contains("timestamp_ticks: unknown"));
        assert!(raw_output.contains(&format!("timestamp_ticks: {}", raw_event.timestamp)));
    }
}
