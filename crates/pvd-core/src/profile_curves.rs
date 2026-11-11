use crate::{Action, Error, Recording, analysis::view::ProfileSource, error};
use std::{
    collections::{BTreeMap, btree_map::Entry},
    sync::atomic::{AtomicBool, Ordering},
};

mod streaming;
#[cfg(test)]
mod view_tests;
pub use streaming::{
    CatalogCursor, CatalogPageReport, CatalogReport, MAX_CATALOG_PAGE, SampleReport,
    SourceIdentity, catalog_key_streaming, catalog_page_streaming, catalog_streaming,
    sample_streaming,
};

pub const MAX_SERIES: usize = 100_000;
pub const MAX_SAMPLES: usize = 5_000;
const MAX_RECORDS: usize = 2_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExactKey {
    pub zone: u64,
    pub event: u16,
    pub thread: u32,
    pub context: u64,
}

impl From<(u64, u16, u32, u64)> for ExactKey {
    fn from((zone, event, thread, context): (u64, u16, u32, u64)) -> Self {
        Self {
            zone,
            event,
            thread,
            context,
        }
    }
}

impl ExactKey {
    fn tuple(self) -> (u64, u16, u32, u64) {
        (self.zone, self.event, self.thread, self.context)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReferenceScene {
    pub id: u64,
    pub generation: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SamplingFrames {
    pub frames: Vec<usize>,
    pub stride: usize,
    pub reference_frame_count: usize,
}

pub fn sampling_frames(
    recording: &Recording,
    scene: ReferenceScene,
    cancelled: &AtomicBool,
) -> Result<SamplingFrames, Error> {
    let reference_frames = reference_frame_indices(recording, scene, cancelled)?;
    let reference_frame_count = reference_frames.len();
    let Some(&last) = reference_frames.last() else {
        return Err(error(
            "no completed frames match the reference scene ID and generation",
        ));
    };
    let stride = reference_frame_count
        .saturating_sub(1)
        .div_ceil(MAX_SAMPLES - 1)
        .max(1);
    let mut frames: Vec<_> = reference_frames.into_iter().step_by(stride).collect();
    if frames.last() != Some(&last) {
        frames.push(last);
    }
    check_cancelled(cancelled)?;
    Ok(SamplingFrames {
        frames,
        stride,
        reference_frame_count,
    })
}

fn reference_frame_indices(
    recording: &Recording,
    scene: ReferenceScene,
    cancelled: &AtomicBool,
) -> Result<Vec<usize>, Error> {
    check_cancelled(cancelled)?;
    if recording.frames.len() > MAX_RECORDS {
        return Err(error("profile curve sampling exceeds its index budget"));
    }
    if recording.is_disk_backed() {
        let mut frames = Vec::new();
        for frame in &recording.frames {
            check_cancelled(cancelled)?;
            if recording.frame_matches_scene(frame, scene.id)
                && recording.disk_scene_generation(frame.index) == Some(scene.generation)
            {
                frames.push(frame.index);
            }
        }
        return Ok(frames);
    }
    if recording.frames.len() != recording.frame_ends.len() {
        return Err(error("profile reference frame history is inconsistent"));
    }
    let mut reference_frames = Vec::new();
    let mut action_cursor = 0;
    let mut live_generation = None;
    for (index, (frame, &action_end)) in recording
        .frames
        .iter()
        .zip(&recording.frame_ends)
        .enumerate()
    {
        check_cancelled(cancelled)?;
        if frame.index != index {
            return Err(error("profile reference frame indices are inconsistent"));
        }
        let actions = recording
            .actions
            .get(action_cursor..action_end)
            .ok_or_else(|| error("profile reference frame action range is invalid"))?;
        for action in actions {
            check_cancelled(cancelled)?;
            match action {
                Action::Create(object) if object.id == scene.id => {
                    live_generation =
                        (object.type_name == "physx3::PxScene").then_some(object.generation);
                }
                Action::Destroy(id) if *id == scene.id => live_generation = None,
                _ => {}
            }
        }
        action_cursor = action_end;
        if recording.frame_matches_scene(frame, scene.id)
            && live_generation == Some(scene.generation)
        {
            reference_frames.push(frame.index);
        }
    }
    check_cancelled(cancelled)?;
    Ok(reference_frames)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogEntry {
    pub key: ExactKey,
    pub first_start: Option<u64>,
    pub complete_spans: usize,
    pub open_spans: usize,
    pub unmatched_stops: u64,
    pub metadata_ambiguous: bool,
}

pub fn catalog(recording: &Recording, cancelled: &AtomicBool) -> Result<Vec<CatalogEntry>, Error> {
    check_cancelled(cancelled)?;
    if !crate::native::analysis_history::available(recording) {
        return catalog_source(&recording.analysis, cancelled);
    }
    let analysis = crate::native::analysis_history::end_analysis(recording)?;
    let mut entries = BTreeMap::new();
    for span in crate::native::analysis_history::Spans::new(recording, MAX_RECORDS, cancelled)? {
        catalog_span(&mut entries, analysis, &span?)?;
    }
    finish_catalog(entries, analysis, cancelled)
}

fn catalog_source(
    analysis: &impl ProfileSource,
    cancelled: &AtomicBool,
) -> Result<Vec<CatalogEntry>, Error> {
    check_cancelled(cancelled)?;
    if analysis.profile_summary().complete_spans > MAX_RECORDS {
        return Err(error("profile catalog exceeds its event budget"));
    }
    let mut entries = BTreeMap::new();
    for span in analysis.profile_spans() {
        check_cancelled(cancelled)?;
        catalog_span(&mut entries, analysis, span)?;
    }
    finish_catalog(entries, analysis, cancelled)
}

fn catalog_span(
    entries: &mut BTreeMap<ExactKey, CatalogEntry>,
    analysis: &impl ProfileSource,
    span: &crate::analysis::ProfileSpan,
) -> Result<(), Error> {
    let key = ExactKey::from((span.zone, span.event, span.thread, span.context));
    let entry = catalog_entry(entries, key, analysis)?;
    entry.complete_spans += 1;
    entry.first_start = Some(
        entry
            .first_start
            .map_or(span.start, |tick| tick.min(span.start)),
    );
    Ok(())
}

fn finish_catalog(
    mut entries: BTreeMap<ExactKey, CatalogEntry>,
    analysis: &impl ProfileSource,
    cancelled: &AtomicBool,
) -> Result<Vec<CatalogEntry>, Error> {
    for event in analysis.open_profile_events() {
        check_cancelled(cancelled)?;
        let entry = catalog_entry(&mut entries, event.key.into(), analysis)?;
        entry.open_spans = event.count;
        entry.first_start = Some(
            entry
                .first_start
                .map_or(event.earliest, |tick| tick.min(event.earliest)),
        );
    }
    for (key, count) in analysis.unmatched_profile_events() {
        check_cancelled(cancelled)?;
        catalog_entry(&mut entries, key.into(), analysis)?.unmatched_stops = count;
    }
    let mut entries: Vec<_> = entries.into_values().collect();
    entries.sort_by_key(|entry| (entry.first_start.is_none(), entry.first_start, entry.key));
    check_cancelled(cancelled)?;
    Ok(entries)
}

fn catalog_entry<'a>(
    entries: &'a mut BTreeMap<ExactKey, CatalogEntry>,
    key: ExactKey,
    analysis: &impl ProfileSource,
) -> Result<&'a mut CatalogEntry, Error> {
    let full = entries.len() >= MAX_SERIES;
    Ok(match entries.entry(key) {
        Entry::Occupied(entry) => entry.into_mut(),
        Entry::Vacant(entry) => {
            if full {
                return Err(error("profile catalog exceeds 100000 exact event series"));
            }
            entry.insert(CatalogEntry {
                key,
                first_start: None,
                complete_spans: 0,
                open_spans: 0,
                unmatched_stops: 0,
                metadata_ambiguous: analysis.profile_metadata_ambiguous(key.zone, key.event),
            })
        }
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Recorded,
    Missing,
    FirstFrame,
    CrossBoundary,
    OpenSpan,
    UnmatchedStop,
    MetadataAmbiguous,
    NonMonotonicClock,
    OtherScene,
    OtherGeneration,
    AnalysisExpired,
}

impl Status {
    pub fn name(self) -> &'static str {
        match self {
            Self::Recorded => "recorded",
            Self::Missing => "no_complete_span",
            Self::FirstFrame => "no_previous_frame",
            Self::CrossBoundary => "span_crosses_frame_boundary",
            Self::OpenSpan => "open_span_may_overlap_window",
            Self::UnmatchedStop => "unmatched_stop_has_unknown_start",
            Self::MetadataAmbiguous => "metadata_ambiguous",
            Self::NonMonotonicClock => "non_monotonic_scene_clock",
            Self::OtherScene => "other_scene",
            Self::OtherGeneration => "other_or_missing_generation",
            Self::AnalysisExpired => "outside_current_analysis_window",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Point {
    pub frame: usize,
    pub milliseconds: Option<f64>,
    pub occurrence_count: usize,
    pub status: Status,
}

struct Window {
    point: usize,
    lower: u64,
    upper: u64,
}

/// The scene defines clock windows only; an opaque event context need not identify that scene.
/// Inclusive durations count nested or overlapping occurrences separately, not CPU utilization.
pub fn sample(
    recording: &Recording,
    key: ExactKey,
    scene: ReferenceScene,
    frames: &[usize],
    cancelled: &AtomicBool,
) -> Result<Vec<Point>, Error> {
    check_cancelled(cancelled)?;
    if frames.len() > MAX_SAMPLES
        || recording.frames.len() > MAX_RECORDS
        || recording.analysis.profile_summary().complete_spans > MAX_RECORDS
    {
        return Err(error("profile curve sampling exceeds its index budget"));
    }
    if frames.windows(2).any(|pair| pair[0] >= pair[1])
        || frames
            .iter()
            .any(|frame| recording.frame_info(*frame).is_none())
    {
        return Err(error(
            "profile curve sample indices must be sorted, unique, and in range",
        ));
    }
    let scale = recording.header.timestamp_numerator as f64
        / recording.header.timestamp_denominator as f64
        / 1e6;
    if !scale.is_finite() || scale <= 0.0 {
        return Err(error("invalid profile curve clock ratio"));
    }
    let boundaries: Vec<_> = recording
        .frames
        .iter()
        .filter(|frame| recording.frame_matches_scene(frame, scene.id))
        .collect();
    let reference_frames = reference_frame_indices(recording, scene, cancelled)?;
    let reference_times: Vec<_> = reference_frames
        .iter()
        .map(|&frame| recording.frame_info(frame).unwrap().timestamp)
        .collect();
    let monotonic = reference_frames.windows(2).all(|pair| {
        recording.frame_info(pair[0]).unwrap().timestamp
            < recording.frame_info(pair[1]).unwrap().timestamp
    });
    let analysis = crate::native::analysis_history::end_analysis(recording)?;
    let ambiguous = analysis.profile_metadata_ambiguous(key.zone, key.event);
    let unmatched = analysis.unmatched_profile_event(key.tuple()) != 0;
    let earliest_open = analysis
        .profile_open_event(key.tuple())
        .map(|event| event.earliest);
    let mut points = Vec::with_capacity(frames.len());
    let mut windows = Vec::with_capacity(frames.len());
    for &frame in frames {
        check_cancelled(cancelled)?;
        let position = boundaries.partition_point(|boundary| boundary.index < frame);
        let status = if boundaries
            .get(position)
            .is_none_or(|boundary| boundary.index != frame)
        {
            Status::OtherScene
        } else if !crate::native::analysis_history::available(recording)
            && frame < analysis.profile_summary().first_retained_frame
        {
            Status::AnalysisExpired
        } else if ambiguous {
            Status::MetadataAmbiguous
        } else if unmatched {
            // A missing start can precede any known boundary; its stop cannot localize the loss.
            Status::UnmatchedStop
        } else if !monotonic {
            Status::NonMonotonicClock
        } else if position == 0 {
            Status::FirstFrame
        } else if reference_frames.binary_search(&frame).is_err()
            || reference_frames
                .binary_search(&boundaries[position - 1].index)
                .is_err()
        {
            Status::OtherGeneration
        } else {
            windows.push(Window {
                point: points.len(),
                lower: boundaries[position - 1].timestamp,
                upper: boundaries[position].timestamp,
            });
            Status::Missing
        };
        points.push(Point {
            frame,
            milliseconds: None,
            occurrence_count: 0,
            status,
        });
    }
    if windows.is_empty() {
        return Ok(points);
    }
    let mut ticks = vec![0u128; points.len()];
    let mut crossing = vec![0i64; windows.len() + 1];
    for span in crate::native::analysis_history::Spans::new(recording, MAX_RECORDS, cancelled)? {
        let span = span?;
        if (span.zone, span.event, span.thread, span.context) != key.tuple() {
            continue;
        }
        if span.start > span.end {
            return Err(error("profile curve span ends before its start"));
        }
        let boundary = reference_times.partition_point(|&timestamp| timestamp < span.end);
        if boundary > 0
            && boundary < reference_frames.len()
            && span.start >= reference_times[boundary - 1]
        {
            let slot = windows.partition_point(|window| window.upper < span.end);
            if let Some(window) = windows
                .get(slot)
                .filter(|window| window.upper == reference_times[boundary])
            {
                points[window.point].occurrence_count += 1;
                ticks[window.point] += u128::from(span.end - span.start);
            }
        } else {
            let first = windows.partition_point(|window| window.upper <= span.start);
            let after = windows.partition_point(|window| window.lower < span.end);
            if first < after {
                crossing[first] += 1;
                crossing[after] -= 1;
            }
        }
    }
    let mut crossing_count = 0i64;
    for (index, window) in windows.iter().enumerate() {
        crossing_count += crossing[index];
        let point = &mut points[window.point];
        if crossing_count > 0 {
            point.status = Status::CrossBoundary;
        } else if earliest_open.is_some_and(|start| start <= window.upper) {
            point.status = Status::OpenSpan;
        } else if point.occurrence_count > 0 {
            point.status = Status::Recorded;
            point.milliseconds = Some(ticks[window.point] as f64 * scale);
        }
    }
    Ok(points)
}

fn check_cancelled(cancelled: &AtomicBool) -> Result<(), Error> {
    if cancelled.load(Ordering::Relaxed) {
        Err(error("profile curve operation cancelled"))
    } else {
        Ok(())
    }
}
