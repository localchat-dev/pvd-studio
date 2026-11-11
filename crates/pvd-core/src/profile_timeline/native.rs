use super::{
    Catalog, ClockRatio, GlobalPairingStats, MAX_THREAD_PAGE, MAX_WINDOW_BYTES, MAX_WINDOW_SPANS,
    THREAD_BYTES, ThreadCursor, ThreadPageReport, Window, WindowReport, WindowRequest, add, check,
    clock,
};
use crate::{
    Error, Recording,
    analysis::{ProfileSpan, view::ProfileSource},
    error,
    native::analysis_history,
};
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, Ordering},
};

/// Process-local identity of a published history root, not a digest of the source file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    end_revision: u64,
    frames: usize,
    clock: ClockRatio,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub frames_scanned: usize,
    pub total_frames: usize,
    pub spans_scanned: u64,
}

pub fn identity(recording: &Recording) -> Result<Identity, Error> {
    if !analysis_history::available(recording) {
        return Err(error(
            "complete native timeline requires persistent native history",
        ));
    }
    analysis_history::validate_complete(recording)?;
    Ok(Identity {
        end_revision: analysis_history::end_analysis(recording)?.revision(),
        frames: recording.frame_count(),
        clock: clock(&recording.header)?,
    })
}

pub fn catalog_page(
    recording: &Recording,
    cursor: Option<&ThreadCursor<Identity>>,
    limit: usize,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(Progress) -> bool,
) -> Result<ThreadPageReport<Identity>, Error> {
    check(cancelled)?;
    if !(1..=MAX_THREAD_PAGE).contains(&limit) {
        return Err(error(
            "timeline thread page limit must be between 1 and 4096",
        ));
    }
    let source = identity(recording)?;
    if cursor.is_some_and(|cursor| cursor.identity != source) {
        return Err(error(
            "timeline thread cursor belongs to a different native history",
        ));
    }
    let mut catalog = Catalog {
        after: cursor.map(|cursor| cursor.after),
        limit,
        threads: BTreeMap::new(),
        more: false,
        range: None,
        stats: GlobalPairingStats::default(),
        clock: None,
        cancelled,
    };
    scan(recording, cancelled, &mut progress, |span| {
        catalog.span(span)
    })?;
    let end = analysis_history::end_analysis(recording)?;
    catalog.finish_view(end, &recording.header)?;
    validate_identity(recording, &source, cancelled)?;
    let next = catalog.more.then(|| ThreadCursor {
        identity: source.clone(),
        after: *catalog.threads.last_key_value().unwrap().0,
    });
    Ok(ThreadPageReport {
        identity: source,
        clock: catalog.clock.unwrap(),
        completed_tick_range: catalog.range,
        global_pairing: catalog.stats,
        legacy_cuda_buffers: end.profile_summary().legacy_cuda_buffers,
        estimated_index_bytes: catalog.threads.len() * THREAD_BYTES,
        threads: catalog.threads.into_values().collect(),
        next,
    })
}

pub fn window(
    recording: &Recording,
    source: &Identity,
    request: WindowRequest,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(Progress) -> bool,
) -> Result<WindowReport<Identity>, Error> {
    validate_identity(recording, source, cancelled)?;
    if request.ticks.start > request.ticks.end {
        return Err(error("timeline window start must not exceed its end"));
    }
    if !(1..=MAX_WINDOW_SPANS).contains(&request.limits.spans)
        || !(1..=MAX_WINDOW_BYTES).contains(&request.limits.bytes)
    {
        return Err(error(
            "timeline window limits must be within 200000 spans and 32 MiB",
        ));
    }
    let mut window = Window {
        request,
        spans: Vec::new(),
        names: BTreeMap::new(),
        bytes: 0,
        stats: GlobalPairingStats::default(),
        clock: None,
        cancelled,
    };
    scan(recording, cancelled, &mut progress, |span| {
        window.span(span)
    })?;
    let end = analysis_history::end_analysis(recording)?;
    window.finish_view(end, &recording.header)?;
    validate_identity(recording, source, cancelled)?;
    Ok(WindowReport {
        identity: source.clone(),
        clock: window.clock.unwrap(),
        request,
        spans: window.spans,
        names: window.names,
        global_pairing: window.stats,
        legacy_cuda_buffers: end.profile_summary().legacy_cuda_buffers,
        estimated_index_bytes: window.bytes,
    })
}

fn validate_identity(
    recording: &Recording,
    source: &Identity,
    cancelled: &AtomicBool,
) -> Result<(), Error> {
    check(cancelled)?;
    if identity(recording)? != *source {
        return Err(error(
            "timeline query belongs to a different native history",
        ));
    }
    Ok(())
}

fn scan(
    recording: &Recording,
    cancelled: &AtomicBool,
    progress: &mut impl FnMut(Progress) -> bool,
    mut receive: impl FnMut(&ProfileSpan) -> Result<(), Error>,
) -> Result<(), Error> {
    let mut status = Progress {
        frames_scanned: 0,
        total_frames: recording.frame_count(),
        spans_scanned: 0,
    };
    let mut publish = |status| {
        check(cancelled)?;
        if progress(status) {
            Ok(())
        } else {
            Err(error("complete native profile timeline query cancelled"))
        }
    };
    publish(status)?;
    for frame in 0..=recording.frame_count() {
        let analysis = analysis_history::frame_analysis(recording, frame, &mut || {
            cancelled.load(Ordering::Relaxed)
        })?;
        // Roots overlap; only rows delivered at this frame contribute, including the final tail.
        for index in analysis_history::span_range(&analysis, frame)? {
            check(cancelled)?;
            let span = analysis
                .profile_span(index)
                .ok_or_else(|| error("native timeline history span is unavailable"))?;
            receive(span)?;
            add(&mut status.spans_scanned, 1)?;
            if status.spans_scanned.is_multiple_of(1024) {
                publish(status)?;
            }
        }
        status.frames_scanned = frame.saturating_add(1).min(status.total_frames);
        publish(status)?;
    }
    Ok(())
}
