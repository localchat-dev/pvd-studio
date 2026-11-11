use crate::{
    Error, FrameInfo, Recording, RecordingSource,
    analysis::{ProfileSpan, view::ProfileSource},
    error,
};
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, Ordering},
};

mod streaming;
pub use streaming::{StageSampleReport, sample_streaming};

pub const CROSS_THREAD_ID: u32 = 99_999_789;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    FrameInterval,
    Simulate,
    Collision,
    Solver,
}

impl Metric {
    pub fn name(self) -> &'static str {
        match self {
            Self::FrameInterval => "frame_end_interval",
            Self::Simulate => "Basic.simulate",
            Self::Collision => "Basic.collision",
            Self::Solver => "Basic.rigidBodySolver",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Recorded,
    Missing,
    FirstFrame,
    Ambiguous,
    CrossBoundary,
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
            Self::Ambiguous => "multiple_complete_spans",
            Self::CrossBoundary => "span_crosses_frame_boundary",
            Self::NonMonotonicClock => "non_monotonic_scene_clock",
            Self::OtherScene => "other_scene",
            Self::OtherGeneration => "other_or_missing_generation",
            Self::AnalysisExpired => "outside_current_analysis_window",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Point {
    pub frame: usize,
    pub milliseconds: Option<f64>,
    pub status: Status,
}

fn context(recording: &Recording, scene: u64, generation: u32) -> Option<u64> {
    if recording.source == RecordingSource::Pvd3Database {
        recording
            .native_scene_contexts
            .as_ref()?
            .source(scene, generation)
    } else if recording
        .frame_sources
        .source(crate::frame_sources::FrameSourceKey::Legacy)
        .is_some()
    {
        Some(scene)
    } else {
        recording
            .frame_sources
            .source(crate::frame_sources::FrameSourceKey::Captured {
                section: crate::frame_sources::FrameSection::PhysX,
                source: scene,
            })
            .filter(|source| {
                source
                    .frames()
                    .rev()
                    .any(|frame| frame.scene.is_some_and(|object| object.id == scene))
            })
            .map(|_| scene)
    }
}

pub fn supports_stages(recording: &Recording, scene: u64, generation: u32) -> bool {
    context(recording, scene, generation).is_some()
}

pub fn source_context(recording: &Recording, scene: u64, generation: u32) -> Result<u64, Error> {
    context(recording, scene, generation).ok_or_else(|| {
        error(if recording.source == RecordingSource::Pvd3Database {
            "fixed PhysX stage metrics require a unique PVD3 database Scene lifetime mapping: profile context uses SDK source pointers, while scene IDs are container-local; reused or unmapped contexts remain unavailable; use an exact event curve with an explicit reference scene window"
        } else {
            "fixed PhysX stage metrics require a captured PhysX frame source associated with a PxScene; ApexFrame clocks and unproven source objects do not define PhysX stage windows"
        })
    })
}

pub fn sample(
    recording: &Recording,
    scene: u64,
    generation: u32,
    metric: Metric,
    frames: &[usize],
    cancelled: &AtomicBool,
) -> Result<Vec<Point>, Error> {
    if cancelled.load(Ordering::Relaxed) {
        return Err(error("frame metric sampling cancelled"));
    }
    let context = if metric == Metric::FrameInterval {
        scene
    } else {
        source_context(recording, scene, generation)?
    };
    if frames.len() > 5000 || recording.frames.len() > 2_000_000 {
        return Err(error("frame metric sampling exceeds its index budget"));
    }
    if frames.windows(2).any(|pair| pair[0] >= pair[1])
        || frames
            .iter()
            .any(|frame| recording.frame_info(*frame).is_none())
    {
        return Err(error(
            "frame metric sample indices must be sorted, unique, and in range",
        ));
    }
    let scale = recording.header.timestamp_numerator as f64
        / recording.header.timestamp_denominator as f64
        / 1e6;
    if !scale.is_finite() || scale <= 0.0 {
        return Err(error("invalid frame metric clock ratio"));
    }
    let boundaries: Vec<_> = recording
        .frames
        .iter()
        .filter(|frame| recording.frame_matches_scene(frame, scene))
        .collect();
    let analysis = if metric == Metric::FrameInterval {
        &recording.analysis
    } else {
        crate::native::analysis_history::end_analysis(recording)?
    };
    let matching = |span: &ProfileSpan| {
        span.context == context
            && span.thread == CROSS_THREAD_ID
            && analysis.profile_zone_name(span.zone) == Some("PhysXSDK")
            && analysis.profile_event_name(span.zone, span.event) == Some(metric.name())
    };
    let spans = (metric != Metric::FrameInterval)
        .then(|| crate::native::analysis_history::Spans::new(recording, usize::MAX, cancelled))
        .transpose()?;
    let spans = spans.into_iter().flatten().filter_map(|span| match span {
        Ok(span) if matching(&span) => Some(Ok(span)),
        Ok(_) => None,
        Err(failure) => Some(Err(failure)),
    });
    let mut points = attribute(&boundaries, spans, metric, frames, scale, cancelled)?;
    for point in &mut points {
        if cancelled.load(Ordering::Relaxed) {
            return Err(error("frame metric sampling cancelled"));
        }
        let Some(frame) = recording.frame_info(point.frame) else {
            return Err(error("frame metric sample index is out of range"));
        };
        if !recording.frame_matches_scene(frame, scene) {
            continue;
        }
        if metric != Metric::FrameInterval
            && !crate::native::analysis_history::available(recording)
            && point.frame < recording.analysis.profile_summary().first_retained_frame
        {
            point.milliseconds = None;
            point.status = Status::AnalysisExpired;
            continue;
        }
        if recording.is_disk_backed() {
            if recording.disk_scene_generation(point.frame) != Some(generation) {
                point.milliseconds = None;
                point.status = Status::OtherGeneration;
            } else if metric == Metric::FrameInterval && point.milliseconds.is_some() {
                let position = boundaries.partition_point(|frame| frame.index < point.frame);
                if recording.disk_scene_generation(boundaries[position - 1].index)
                    != Some(generation)
                {
                    point.milliseconds = None;
                    point.status = Status::OtherGeneration;
                }
            }
            continue;
        }
        let state = recording.state_at(point.frame)?;
        if state.objects.get(&scene).is_none_or(|object| {
            object.generation != generation || object.type_name != "physx3::PxScene"
        }) {
            point.milliseconds = None;
            point.status = Status::OtherGeneration;
            continue;
        }
        if metric == Metric::FrameInterval && point.milliseconds.is_some() {
            let position = boundaries.partition_point(|frame| frame.index < point.frame);
            let previous = recording.state_at(boundaries[position - 1].index)?;
            if previous
                .objects
                .get(&scene)
                .is_none_or(|object| object.generation != generation)
            {
                point.milliseconds = None;
                point.status = Status::OtherGeneration;
            }
        }
    }
    Ok(points)
}

fn attribute(
    boundaries: &[&FrameInfo],
    spans: impl Iterator<Item = Result<ProfileSpan, Error>>,
    metric: Metric,
    requested: &[usize],
    scale: f64,
    cancelled: &AtomicBool,
) -> Result<Vec<Point>, Error> {
    let monotonic = boundaries
        .windows(2)
        .all(|pair| pair[0].timestamp < pair[1].timestamp);
    let mut points = Vec::with_capacity(requested.len());
    let mut slots = BTreeMap::new();
    for &frame in requested {
        if cancelled.load(Ordering::Relaxed) {
            return Err(error("frame metric sampling cancelled"));
        }
        let position = boundaries.partition_point(|boundary| boundary.index < frame);
        let (status, milliseconds) = if boundaries
            .get(position)
            .is_none_or(|boundary| boundary.index != frame)
        {
            (Status::OtherScene, None)
        } else if !monotonic {
            (Status::NonMonotonicClock, None)
        } else if metric != Metric::FrameInterval {
            slots.insert(frame, (points.len(), 0usize));
            (Status::Missing, None)
        } else if position == 0 {
            (Status::FirstFrame, None)
        } else {
            (
                Status::Recorded,
                Some(
                    (boundaries[position].timestamp - boundaries[position - 1].timestamp) as f64
                        * scale,
                ),
            )
        };
        points.push(Point {
            frame,
            milliseconds,
            status,
        });
    }
    if metric != Metric::FrameInterval && monotonic {
        for span in spans {
            let span = span?;
            if cancelled.load(Ordering::Relaxed) {
                return Err(error("frame metric sampling cancelled"));
            }
            let position = boundaries.partition_point(|boundary| boundary.timestamp < span.end);
            let Some(boundary) = boundaries.get(position) else {
                continue;
            };
            let Some((slot, count)) = slots.get_mut(&boundary.index) else {
                continue;
            };
            *count += 1;
            let point = &mut points[*slot];
            if *count > 1 {
                point.status = Status::Ambiguous;
                point.milliseconds = None;
            } else if span.start > span.end
                || (position > 0 && span.start < boundaries[position - 1].timestamp)
            {
                point.status = Status::CrossBoundary;
            } else {
                point.status = Status::Recorded;
                point.milliseconds = Some((span.end - span.start) as f64 * scale);
            }
        }
    }
    Ok(points)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn span(start: u64, end: u64) -> ProfileSpan {
        ProfileSpan {
            zone: 1,
            event: 2,
            thread: CROSS_THREAD_ID,
            context: 1,
            start,
            end,
            received_frame: 999,
        }
    }

    #[test]
    fn timestamp_assignment_ignores_payload_flush_and_does_not_sum_duplicates() {
        let frames = [
            FrameInfo {
                index: 0,
                timestamp: 100,
                scene_id: 1,
            },
            FrameInfo {
                index: 2,
                timestamp: 200,
                scene_id: 1,
            },
            FrameInfo {
                index: 3,
                timestamp: 300,
                scene_id: 1,
            },
            FrameInfo {
                index: 4,
                timestamp: 400,
                scene_id: 1,
            },
        ];
        let spans = [span(20, 50), span(110, 180), span(120, 150), span(180, 220)];
        let points = attribute(
            &frames.iter().collect::<Vec<_>>(),
            spans.iter().cloned().map(Ok),
            Metric::Solver,
            &[0, 1, 2, 3, 4],
            0.1,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(points[0].milliseconds, Some(3.0));
        assert_eq!(points[1].status, Status::OtherScene);
        assert_eq!(points[2].status, Status::Ambiguous);
        assert_eq!(points[3].status, Status::CrossBoundary);
        assert_eq!(points[4].status, Status::Missing);
        assert!(points[1..].iter().all(|point| point.milliseconds.is_none()));
    }

    #[test]
    fn frame_intervals_require_same_scene_monotonic_boundaries_and_cancellation() {
        let mut frames = [
            FrameInfo {
                index: 2,
                timestamp: 100,
                scene_id: 1,
            },
            FrameInfo {
                index: 5,
                timestamp: 250,
                scene_id: 1,
            },
        ];
        let evaluate = |frames: &[FrameInfo], cancel: bool| {
            attribute(
                &frames.iter().collect::<Vec<_>>(),
                std::iter::empty(),
                Metric::FrameInterval,
                &[2, 3, 5],
                0.01,
                &AtomicBool::new(cancel),
            )
        };
        let points = evaluate(&frames, false).unwrap();
        assert_eq!(points[0].status, Status::FirstFrame);
        assert_eq!(points[1].status, Status::OtherScene);
        assert_eq!(points[2].milliseconds, Some(1.5));
        frames[1].timestamp = 100;
        assert!(
            evaluate(&frames, false)
                .unwrap()
                .iter()
                .filter(|p| p.frame != 3)
                .all(|p| p.status == Status::NonMonotonicClock)
        );
        assert!(evaluate(&frames, true).is_err());
    }
}
