use super::*;
use crate::{
    Action,
    analysis::Analysis,
    analysis_export::{AnalysisExportPhase, AnalysisExportProgress, ProfileObserver, scan_profile},
    profile_curves::{ReferenceScene, SourceIdentity},
};
use pvd_protocol::Header;
use std::{
    collections::BTreeSet,
    io::{Read, Seek},
};

const MAX_INDEX_BYTES: usize = 64 << 20;
const KEY_BYTES: usize = size_of::<(u64, u16)>() + 128;

#[derive(Clone, Debug)]
pub struct StageSampleReport {
    pub identity: SourceIdentity,
    pub points: Vec<Point>,
    pub open_spans: usize,
    pub unmatched_stops: u64,
    /// Decoder state and its metadata have independent budgets.
    pub estimated_index_bytes: usize,
}

fn check(cancelled: &AtomicBool) -> Result<(), Error> {
    if cancelled.load(Ordering::Relaxed) {
        Err(error("complete stage sampling cancelled"))
    } else {
        Ok(())
    }
}

fn charge(bytes: &mut usize, extra: usize) -> Result<(), Error> {
    *bytes = bytes
        .checked_add(extra)
        .filter(|value| *value <= MAX_INDEX_BYTES)
        .ok_or_else(|| error("complete stage sampling exceeds its 64 MiB index budget"))?;
    Ok(())
}

/// Match the returned identity to the frame directory's original source before publishing.
pub fn sample_streaming<R: Read + Seek + ?Sized>(
    source: &mut R,
    recording: &Recording,
    scene: ReferenceScene,
    metric: Metric,
    frames: &[usize],
    cancelled: &AtomicBool,
    mut progress: impl FnMut(AnalysisExportProgress) -> bool,
) -> Result<StageSampleReport, Error> {
    check(cancelled)?;
    if metric == Metric::FrameInterval {
        return Err(error(
            "frame intervals use the frame directory; request a fixed stage metric",
        ));
    }
    if recording.source != RecordingSource::SdkStream
        || recording.frames.len() != recording.frame_count()
        || recording.frames.len() > 2_000_000
        || recording
            .frames
            .iter()
            .enumerate()
            .any(|(index, frame)| index != frame.index)
    {
        return Err(error(
            "complete stage sampling requires a complete SDK frame directory",
        ));
    }
    if frames.len() > 5000
        || frames.windows(2).any(|pair| pair[0] >= pair[1])
        || frames
            .iter()
            .any(|frame| recording.frame_info(*frame).is_none())
    {
        return Err(error(
            "stage sample indices must be sorted, unique, and within the 5000 point budget",
        ));
    }
    let ReferenceScene {
        id: scene,
        generation,
    } = scene;
    let mut bytes = 0;
    charge(
        &mut bytes,
        recording
            .frames
            .len()
            .saturating_mul(size_of::<&FrameInfo>()),
    )?;
    charge(
        &mut bytes,
        frames.len().saturating_mul(size_of::<Point>() + 160),
    )?;
    let mut metadata = Metadata {
        metric,
        keys: BTreeSet::new(),
        bytes,
        cancelled,
    };
    let inspected = scan_profile(source, &mut metadata, &mut |status| {
        !cancelled.load(Ordering::Relaxed) && progress(status)
    })?;
    let identity = SourceIdentity::from(&inspected);
    if identity.frames != recording.frame_count() {
        return Err(error(
            "stage source frame count differs from its frame directory",
        ));
    }
    let mut open_spans = 0usize;
    let mut unmatched_stops = 0u64;
    for (&(zone, event, thread, context), &count) in &inspected.summary.open_events {
        check(cancelled)?;
        if thread == CROSS_THREAD_ID && context == scene && metadata.keys.contains(&(zone, event)) {
            open_spans = open_spans
                .checked_add(count)
                .ok_or_else(|| error("stage open span count overflow"))?;
        }
    }
    for (&(zone, event, thread, context), &count) in &inspected.summary.unmatched_stop_events {
        check(cancelled)?;
        if thread == CROSS_THREAD_ID && context == scene && metadata.keys.contains(&(zone, event)) {
            unmatched_stops = unmatched_stops
                .checked_add(count)
                .ok_or_else(|| error("stage unmatched stop count overflow"))?;
        }
    }
    drop(inspected);
    let mut boundaries = Vec::with_capacity(recording.frames.len());
    boundaries.extend(
        recording
            .frames
            .iter()
            .filter(|frame| frame.scene_id == scene),
    );
    let scale = recording.header.timestamp_numerator as f64
        / recording.header.timestamp_denominator as f64
        / 1e6;
    if !scale.is_finite() || scale <= 0.0 {
        return Err(error("invalid frame metric clock ratio"));
    }
    let monotonic = boundaries
        .windows(2)
        .all(|pair| pair[0].timestamp < pair[1].timestamp);
    let mut sampler = StageSampler {
        scene,
        keys: &metadata.keys,
        boundaries,
        points: Vec::with_capacity(frames.len()),
        slots: BTreeMap::new(),
        monotonic,
        scale,
        cancelled,
    };
    for &frame in frames {
        check(cancelled)?;
        let status = if recording.frames[frame].scene_id != scene {
            Status::OtherScene
        } else if !monotonic {
            Status::NonMonotonicClock
        } else {
            sampler.slots.insert(frame, (sampler.points.len(), false));
            Status::Missing
        };
        sampler.points.push(Point {
            frame,
            milliseconds: None,
            status,
        });
    }
    let sampled = scan_profile(source, &mut sampler, &mut |mut status| {
        status.phase = AnalysisExportPhase::Export;
        !cancelled.load(Ordering::Relaxed) && progress(status)
    })?;
    if identity != SourceIdentity::from(&sampled) {
        return Err(error("source capture changed between stage sampling scans"));
    }
    validate_generations(recording, scene, generation, &mut sampler.points, cancelled)?;
    check(cancelled)?;
    Ok(StageSampleReport {
        identity,
        points: sampler.points,
        open_spans,
        unmatched_stops,
        estimated_index_bytes: metadata.bytes,
    })
}

struct Metadata<'a> {
    metric: Metric,
    keys: BTreeSet<(u64, u16)>,
    bytes: usize,
    cancelled: &'a AtomicBool,
}

impl Metadata<'_> {
    fn inspect(&mut self, analysis: &Analysis, zone: u64, event: u16) -> Result<(), Error> {
        check(self.cancelled)?;
        let key = (zone, event);
        let matches = analysis.zones.get(&zone).is_some_and(|zone| {
            zone.name == "PhysXSDK"
                && zone
                    .events
                    .get(&event)
                    .is_some_and(|name| name == self.metric.name())
        });
        if matches || self.keys.contains(&key) {
            if analysis.profile_metadata_ambiguous(zone, event) {
                return Err(error(format!(
                    "fixed stage {} has redefined profile metadata at zone {zone}, event {event}",
                    self.metric.name()
                )));
            }
            if !self.keys.contains(&key) {
                charge(&mut self.bytes, KEY_BYTES)?;
                self.keys.insert(key);
            }
        }
        Ok(())
    }
}

impl ProfileObserver for Metadata<'_> {
    fn batch(&mut self, _: &Analysis) -> Result<(), Error> {
        check(self.cancelled)
    }
    fn finish(&mut self, _: &Analysis, _: &Header) -> Result<(), Error> {
        check(self.cancelled)
    }
    fn metadata(
        &mut self,
        analysis: &Analysis,
        zone: u64,
        event: Option<u16>,
    ) -> Result<(), Error> {
        if let Some(event) = event {
            self.inspect(analysis, zone, event)?;
        } else if let Some(value) = analysis.zones.get(&zone) {
            for &event in value.events.keys() {
                self.inspect(analysis, zone, event)?;
            }
        }
        Ok(())
    }
}

struct StageSampler<'a> {
    scene: u64,
    keys: &'a BTreeSet<(u64, u16)>,
    boundaries: Vec<&'a FrameInfo>,
    points: Vec<Point>,
    slots: BTreeMap<usize, (usize, bool)>,
    monotonic: bool,
    scale: f64,
    cancelled: &'a AtomicBool,
}

impl ProfileObserver for StageSampler<'_> {
    fn batch(&mut self, analysis: &Analysis) -> Result<(), Error> {
        if !self.monotonic {
            return Ok(());
        }
        for span in &analysis.spans {
            check(self.cancelled)?;
            if span.context != self.scene
                || span.thread != CROSS_THREAD_ID
                || !self.keys.contains(&(span.zone, span.event))
            {
                continue;
            }
            let position = self
                .boundaries
                .partition_point(|frame| frame.timestamp < span.end);
            let Some(boundary) = self.boundaries.get(position) else {
                continue;
            };
            let Some((slot, occupied)) = self.slots.get_mut(&boundary.index) else {
                continue;
            };
            let point = &mut self.points[*slot];
            if *occupied {
                point.status = Status::Ambiguous;
                point.milliseconds = None;
            } else if span.start > span.end
                || (position > 0 && span.start < self.boundaries[position - 1].timestamp)
            {
                point.status = Status::CrossBoundary;
            } else {
                point.status = Status::Recorded;
                point.milliseconds = Some((span.end - span.start) as f64 * self.scale);
            }
            *occupied = true;
        }
        Ok(())
    }
    fn finish(&mut self, _: &Analysis, _: &Header) -> Result<(), Error> {
        check(self.cancelled)
    }
}

fn validate_generations(
    recording: &Recording,
    scene: u64,
    generation: u32,
    points: &mut [Point],
    cancelled: &AtomicBool,
) -> Result<(), Error> {
    let mark = |point: &mut Point, current: Option<u32>| {
        if point.status != Status::OtherScene && current != Some(generation) {
            point.milliseconds = None;
            point.status = Status::OtherGeneration;
        }
    };
    if recording.is_disk_backed() {
        for point in points {
            check(cancelled)?;
            let current = recording.disk_scene_generation(point.frame);
            mark(point, current);
        }
        return Ok(());
    }
    if recording.frames.len() != recording.frame_ends.len() {
        return Err(error("stage reference frame history is inconsistent"));
    }
    let mut cursor = 0;
    let mut next_point = 0;
    let mut current = None;
    for (index, &end) in recording.frame_ends.iter().enumerate() {
        check(cancelled)?;
        if next_point == points.len() {
            break;
        }
        let actions = recording
            .actions
            .get(cursor..end)
            .ok_or_else(|| error("stage reference action range is invalid"))?;
        for action in actions {
            check(cancelled)?;
            match action {
                Action::Create(object) if object.id == scene => {
                    current = (object.type_name == "physx3::PxScene").then_some(object.generation)
                }
                Action::Destroy(id) if *id == scene => current = None,
                _ => {}
            }
        }
        cursor = end;
        if points[next_point].frame == index {
            mark(&mut points[next_point], current);
            next_point += 1;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_key_budget_is_checked_before_inserting_a_new_identity() {
        let cancelled = AtomicBool::new(false);
        let mut analysis = Analysis::default();
        analysis.add_zone(1, "PhysXSDK".into()).unwrap();
        analysis
            .add_event(1, 1, Metric::Simulate.name().into())
            .unwrap();
        let mut metadata = Metadata {
            metric: Metric::Simulate,
            keys: BTreeSet::new(),
            bytes: MAX_INDEX_BYTES,
            cancelled: &cancelled,
        };
        assert!(
            metadata
                .metadata(&analysis, 1, Some(1))
                .unwrap_err()
                .to_string()
                .contains("index budget")
        );
        assert!(metadata.keys.is_empty());
        let mut overflow = usize::MAX;
        assert!(charge(&mut overflow, 1).is_err());
    }
}
