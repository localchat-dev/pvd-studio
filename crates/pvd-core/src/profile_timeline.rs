use crate::{
    Error,
    analysis::{Analysis, ProfileSpan},
    analysis_export::{AnalysisExportProgress, ProfileObserver, scan_profile},
    error,
    profile_curves::SourceIdentity,
};
use pvd_protocol::Header;
use std::{
    collections::{BTreeMap, btree_map::Entry},
    io::{Read, Seek},
    sync::atomic::{AtomicBool, Ordering},
};

pub const MAX_THREAD_PAGE: usize = 4096;
pub const MAX_WINDOW_SPANS: usize = 200_000;
pub const MAX_WINDOW_BYTES: usize = 32 << 20;
const THREAD_BYTES: usize = 2 * size_of::<ThreadEntry>() + 128;
const NAME_BYTES: usize = size_of::<((u64, u16), EventNames)>() + 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Both endpoints are included, including zero-duration spans at either endpoint.
pub struct TickRange {
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// The capture's nanoseconds-per-tick ratio; integer tick coordinates remain authoritative.
pub struct ClockRatio {
    pub numerator: u64,
    pub denominator: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GlobalPairingStats {
    pub complete_spans: u64,
    pub open_starts: u64,
    pub unmatched_stops: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadEntry {
    pub thread: u32,
    pub completed_tick_range: Option<TickRange>,
    pub complete_spans: u64,
    pub open_starts: u64,
    pub unmatched_stops: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadCursor {
    pub identity: SourceIdentity,
    pub after: u32,
}

#[derive(Clone, Debug)]
pub struct ThreadPageReport {
    pub identity: SourceIdentity,
    pub clock: ClockRatio,
    pub completed_tick_range: Option<TickRange>,
    pub global_pairing: GlobalPairingStats,
    pub legacy_cuda_buffers: u64,
    pub threads: Vec<ThreadEntry>,
    pub next: Option<ThreadCursor>,
    pub estimated_index_bytes: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct WindowLimits {
    pub spans: usize,
    pub bytes: usize,
}

impl Default for WindowLimits {
    fn default() -> Self {
        Self {
            spans: MAX_WINDOW_SPANS,
            bytes: MAX_WINDOW_BYTES,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct WindowRequest {
    pub ticks: TickRange,
    pub thread: Option<u32>,
    pub context: Option<u64>,
    pub limits: WindowLimits,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EventNames {
    pub zone: Option<String>,
    pub event: Option<String>,
}

#[derive(Clone, Debug)]
pub struct WindowReport {
    pub identity: SourceIdentity,
    pub clock: ClockRatio,
    pub request: WindowRequest,
    /// Completion arrival order, with original endpoints and received_frame even outside the window.
    pub spans: Vec<ProfileSpan>,
    pub names: BTreeMap<(u64, u16), EventNames>,
    /// Unfinished events have no reliable occurrence window; these counts describe the entire source.
    pub global_pairing: GlobalPairingStats,
    /// Legacy GPU payloads are counted but do not contribute CPU spans or their completed range.
    pub legacy_cuda_buffers: u64,
    pub estimated_index_bytes: usize,
}

fn check(cancelled: &AtomicBool) -> Result<(), Error> {
    if cancelled.load(Ordering::Relaxed) {
        Err(error("complete profile timeline query cancelled"))
    } else {
        Ok(())
    }
}

fn add(value: &mut u64, count: u64) -> Result<(), Error> {
    *value = value
        .checked_add(count)
        .ok_or_else(|| error("profile timeline count overflow"))?;
    Ok(())
}

fn extend(range: &mut Option<TickRange>, span: &ProfileSpan) -> Result<(), Error> {
    if span.end < span.start {
        return Err(error("profile timeline contains a reversed complete span"));
    }
    *range = Some(match *range {
        Some(old) => TickRange {
            start: old.start.min(span.start),
            end: old.end.max(span.end),
        },
        None => TickRange {
            start: span.start,
            end: span.end,
        },
    });
    Ok(())
}

fn clock(header: &Header) -> Result<ClockRatio, Error> {
    if header.timestamp_numerator == 0 || header.timestamp_denominator == 0 {
        return Err(error("profile timeline source has an invalid clock ratio"));
    }
    Ok(ClockRatio {
        numerator: header.timestamp_numerator,
        denominator: header.timestamp_denominator,
    })
}

pub fn catalog_page_streaming<R: Read + Seek + ?Sized>(
    source: &mut R,
    cursor: Option<&ThreadCursor>,
    limit: usize,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(AnalysisExportProgress) -> bool,
) -> Result<ThreadPageReport, Error> {
    check(cancelled)?;
    if !(1..=MAX_THREAD_PAGE).contains(&limit) {
        return Err(error(
            "timeline thread page limit must be between 1 and 4096",
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
    let report = scan_profile(source, &mut catalog, &mut |status| {
        !cancelled.load(Ordering::Relaxed) && progress(status)
    })?;
    let identity = SourceIdentity::from(&report);
    if cursor.is_some_and(|cursor| cursor.identity != identity) {
        return Err(error(
            "timeline thread cursor belongs to a different source",
        ));
    }
    let next = catalog.more.then(|| ThreadCursor {
        identity: identity.clone(),
        after: *catalog.threads.last_key_value().unwrap().0,
    });
    check(cancelled)?;
    Ok(ThreadPageReport {
        identity,
        clock: catalog.clock.unwrap(),
        completed_tick_range: catalog.range,
        global_pairing: catalog.stats,
        legacy_cuda_buffers: report.summary.legacy_cuda_buffers,
        estimated_index_bytes: catalog.threads.len() * THREAD_BYTES,
        threads: catalog.threads.into_values().collect(),
        next,
    })
}

struct Catalog<'a> {
    after: Option<u32>,
    limit: usize,
    threads: BTreeMap<u32, ThreadEntry>,
    more: bool,
    range: Option<TickRange>,
    stats: GlobalPairingStats,
    clock: Option<ClockRatio>,
    cancelled: &'a AtomicBool,
}

impl Catalog<'_> {
    fn entry(&mut self, thread: u32) -> Option<&mut ThreadEntry> {
        if self.after.is_some_and(|after| thread <= after) {
            return None;
        }
        if !self.threads.contains_key(&thread) && self.threads.len() == self.limit {
            self.more = true;
            if thread > *self.threads.last_key_value().unwrap().0 {
                return None;
            }
            self.threads.pop_last();
        }
        Some(self.threads.entry(thread).or_insert(ThreadEntry {
            thread,
            completed_tick_range: None,
            complete_spans: 0,
            open_starts: 0,
            unmatched_stops: 0,
        }))
    }
}

impl ProfileObserver for Catalog<'_> {
    fn batch(&mut self, analysis: &Analysis) -> Result<(), Error> {
        for span in &analysis.spans {
            check(self.cancelled)?;
            extend(&mut self.range, span)?;
            add(&mut self.stats.complete_spans, 1)?;
            if let Some(entry) = self.entry(span.thread) {
                extend(&mut entry.completed_tick_range, span)?;
                add(&mut entry.complete_spans, 1)?;
            }
        }
        Ok(())
    }

    fn finish(&mut self, analysis: &Analysis, header: &Header) -> Result<(), Error> {
        self.clock = Some(clock(header)?);
        for (&(_, _, thread, _), count) in analysis.open_events() {
            check(self.cancelled)?;
            let count = count as u64;
            add(&mut self.stats.open_starts, count)?;
            if let Some(entry) = self.entry(thread) {
                add(&mut entry.open_starts, count)?;
            }
        }
        for (&(_, _, thread, _), &count) in &analysis.unmatched_stop_events {
            check(self.cancelled)?;
            add(&mut self.stats.unmatched_stops, count)?;
            if let Some(entry) = self.entry(thread) {
                add(&mut entry.unmatched_stops, count)?;
            }
        }
        Ok(())
    }
}

/// The identity binds raw tick coordinates and thread/context IDs to a previously inspected source.
pub fn window_streaming<R: Read + Seek + ?Sized>(
    source: &mut R,
    identity: &SourceIdentity,
    request: WindowRequest,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(AnalysisExportProgress) -> bool,
) -> Result<WindowReport, Error> {
    check(cancelled)?;
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
    let report = scan_profile(source, &mut window, &mut |status| {
        !cancelled.load(Ordering::Relaxed) && progress(status)
    })?;
    if SourceIdentity::from(&report) != *identity {
        return Err(error("timeline window belongs to a different source"));
    }
    check(cancelled)?;
    Ok(WindowReport {
        identity: identity.clone(),
        clock: window.clock.unwrap(),
        request,
        spans: window.spans,
        names: window.names,
        global_pairing: window.stats,
        legacy_cuda_buffers: report.summary.legacy_cuda_buffers,
        estimated_index_bytes: window.bytes,
    })
}

struct Window<'a> {
    request: WindowRequest,
    spans: Vec<ProfileSpan>,
    names: BTreeMap<(u64, u16), EventNames>,
    bytes: usize,
    stats: GlobalPairingStats,
    clock: Option<ClockRatio>,
    cancelled: &'a AtomicBool,
}

fn limit_error() -> Error {
    error(
        "timeline window exceeds its span or byte limit; narrow the time range or filter thread/context",
    )
}

fn charge(bytes: &mut usize, extra: usize, limit: usize) -> Result<(), Error> {
    *bytes = bytes
        .checked_add(extra)
        .filter(|bytes| *bytes <= limit)
        .ok_or_else(limit_error)?;
    Ok(())
}

impl Window<'_> {
    fn reserve_span(&mut self) -> Result<(), Error> {
        if self.spans.len() == self.request.limits.spans {
            return Err(limit_error());
        }
        if self.spans.len() < self.spans.capacity() {
            return Ok(());
        }
        let old = self.spans.capacity();
        let available = (self.request.limits.bytes - self.bytes) / size_of::<ProfileSpan>();
        let capacity = old
            .saturating_mul(2)
            .max(1)
            .min(self.request.limits.spans)
            .min(available);
        if capacity <= old {
            return Err(limit_error());
        }
        // Reallocation may keep the old buffer alive until its replacement is allocated.
        charge(
            &mut self.bytes,
            capacity * size_of::<ProfileSpan>(),
            self.request.limits.bytes,
        )?;
        self.spans
            .try_reserve_exact(capacity - old)
            .map_err(|_| limit_error())?;
        charge(
            &mut self.bytes,
            self.spans.capacity().saturating_sub(capacity) * size_of::<ProfileSpan>(),
            self.request.limits.bytes,
        )?;
        self.bytes -= old * size_of::<ProfileSpan>();
        Ok(())
    }
}

impl ProfileObserver for Window<'_> {
    fn batch(&mut self, analysis: &Analysis) -> Result<(), Error> {
        for span in &analysis.spans {
            check(self.cancelled)?;
            add(&mut self.stats.complete_spans, 1)?;
            if self
                .request
                .thread
                .is_some_and(|thread| span.thread != thread)
                || self
                    .request
                    .context
                    .is_some_and(|context| span.context != context)
            {
                continue;
            }
            if span.end < span.start {
                return Err(error("profile timeline contains a reversed complete span"));
            }
            if span.end < self.request.ticks.start || span.start > self.request.ticks.end {
                continue;
            }
            if self.spans.len() == self.request.limits.spans {
                return Err(limit_error());
            }
            if let Entry::Vacant(entry) = self.names.entry((span.zone, span.event)) {
                charge(&mut self.bytes, NAME_BYTES, self.request.limits.bytes)?;
                entry.insert(EventNames::default());
            }
            self.reserve_span()?;
            self.spans.push(span.clone());
        }
        Ok(())
    }

    fn finish(&mut self, analysis: &Analysis, header: &Header) -> Result<(), Error> {
        self.clock = Some(clock(header)?);
        for (_, count) in analysis.open_events() {
            check(self.cancelled)?;
            add(&mut self.stats.open_starts, count as u64)?;
        }
        for &count in analysis.unmatched_stop_events.values() {
            check(self.cancelled)?;
            add(&mut self.stats.unmatched_stops, count)?;
        }
        for (&(zone, event), names) in &mut self.names {
            check(self.cancelled)?;
            if analysis.profile_metadata_ambiguous(zone, event) {
                return Err(error(format!(
                    "timeline window has redefined profile metadata at zone {zone}, event {event}"
                )));
            }
            if let Some(zone) = analysis.zones.get(&zone) {
                let name = zone.events.get(&event);
                charge(
                    &mut self.bytes,
                    zone.name.len().saturating_add(name.map_or(0, String::len)),
                    self.request.limits.bytes,
                )?;
                names.zone = Some(zone.name.clone());
                names.event = name.cloned();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_growth_budget_covers_both_buffers_before_reallocation() {
        let cancelled = AtomicBool::new(false);
        let make_window = |bytes| Window {
            request: WindowRequest {
                ticks: TickRange { start: 0, end: 1 },
                thread: None,
                context: None,
                limits: WindowLimits { spans: 8, bytes },
            },
            spans: Vec::new(),
            names: BTreeMap::new(),
            bytes: 0,
            stats: GlobalPairingStats::default(),
            clock: None,
            cancelled: &cancelled,
        };
        let size = size_of::<ProfileSpan>();
        let mut too_small = make_window(size - 1);
        assert!(too_small.reserve_span().is_err());
        assert_eq!(too_small.spans.capacity(), 0);
        assert_eq!(too_small.bytes, 0);

        let mut window = make_window(size);
        window.reserve_span().unwrap();
        assert_eq!(window.spans.capacity(), 1);
        assert_eq!(window.bytes, size);
        window.spans.push(ProfileSpan {
            zone: 1,
            event: 1,
            thread: 1,
            context: 1,
            start: 0,
            end: 1,
            received_frame: 0,
        });
        for budget in [2 * size, 3 * size - 1] {
            window.request.limits.bytes = budget;
            assert!(window.reserve_span().is_err());
            assert_eq!(window.spans.capacity(), 1);
            assert_eq!(window.bytes, size);
        }
        window.request.limits.bytes = 3 * size;
        window.reserve_span().unwrap();
        assert_eq!(window.spans.capacity(), 2);
        assert_eq!(window.bytes, 2 * size);
        assert_eq!(window.spans[0].end, 1);
    }

    #[test]
    fn checked_counts_and_budget_charges_do_not_wrap_or_mutate_on_failure() {
        let mut count = u64::MAX;
        assert!(add(&mut count, 1).is_err());
        assert_eq!(count, u64::MAX);
        let mut bytes = usize::MAX;
        assert!(charge(&mut bytes, 1, usize::MAX).is_err());
        assert_eq!(bytes, usize::MAX);
        let mut bytes = 128;
        assert!(charge(&mut bytes, 1, 128).is_err());
        assert_eq!(bytes, 128);
    }
}
