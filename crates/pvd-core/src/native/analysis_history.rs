use crate::{
    Error, Recording, RecordingSource,
    analysis::{ProfileSpan, SharedAnalysis, view::ProfileSource},
    error,
};
use std::{
    ops::Range,
    sync::atomic::{AtomicBool, Ordering},
};

pub(crate) fn available(recording: &Recording) -> bool {
    recording.source == RecordingSource::Pvd3Database && recording.is_disk_backed()
}

pub(crate) fn end_analysis(recording: &Recording) -> Result<&SharedAnalysis, Error> {
    if available(recording) {
        recording
            .native_end_analysis()
            .ok_or_else(|| error("native recording-end analysis is unavailable"))
    } else {
        Ok(&recording.analysis)
    }
}

pub(crate) fn validate_complete(recording: &Recording) -> Result<(), Error> {
    let retained = recording
        .disk_usage()
        .ok_or_else(|| error("native disk history is unavailable"))?;
    if retained.first_frame != 0
        || retained.retained_frames != recording.frame_count()
        || retained.warning.is_some()
    {
        return Err(error(
            "complete native analysis requires every captured scene frame",
        ));
    }
    Ok(())
}

pub(crate) fn frame_analysis(
    recording: &Recording,
    frame: usize,
    cancelled: &mut dyn FnMut() -> bool,
) -> Result<SharedAnalysis, Error> {
    crate::check_replay_cancelled(&mut || cancelled())?;
    let analysis = if frame == recording.frame_count() {
        end_analysis(recording)?.clone()
    } else {
        recording.analysis_at_with_cancel(frame, &mut || cancelled())?
    };
    if analysis.profile_summary().first_retained_frame > frame {
        return Err(error(
            "native analysis history has lost the requested frame",
        ));
    }
    Ok(analysis)
}

pub(crate) fn span_range(analysis: &SharedAnalysis, frame: usize) -> Result<Range<usize>, Error> {
    row_range(analysis.profile_spans().len(), frame, |index| {
        analysis.profile_span(index).map(|span| span.received_frame)
    })
}

pub(crate) fn row_range(
    length: usize,
    frame: usize,
    mut received: impl FnMut(usize) -> Option<usize>,
) -> Result<Range<usize>, Error> {
    let mut boundary = |after: bool| -> Result<usize, Error> {
        let (mut low, mut high) = (0, length);
        while low < high {
            let middle = low + (high - low) / 2;
            let value =
                received(middle).ok_or_else(|| error("native analysis row is unavailable"))?;
            if value < frame || (after && value == frame) {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        Ok(low)
    };
    let start = boundary(false)?;
    Ok(start..boundary(true)?)
}

pub(crate) struct Spans<'a> {
    recording: &'a Recording,
    cancelled: &'a AtomicBool,
    analysis: Option<SharedAnalysis>,
    rows: Range<usize>,
    next_frame: Option<usize>,
    count: usize,
    limit: usize,
    failed: bool,
}

impl<'a> Spans<'a> {
    pub(crate) fn new(
        recording: &'a Recording,
        limit: usize,
        cancelled: &'a AtomicBool,
    ) -> Result<Self, Error> {
        crate::check_replay_cancelled(&mut || cancelled.load(Ordering::Relaxed))?;
        let (analysis, rows, next_frame) = if available(recording) {
            validate_complete(recording)?;
            end_analysis(recording)?;
            (None, 0..0, Some(0))
        } else {
            let length = recording.analysis.profile_spans().len();
            if length > limit {
                return Err(error("profile history exceeds its event budget"));
            }
            (Some(recording.analysis.clone()), 0..length, None)
        };
        Ok(Self {
            recording,
            cancelled,
            analysis,
            rows,
            next_frame,
            count: 0,
            limit,
            failed: false,
        })
    }

    fn read_next(&mut self) -> Result<Option<ProfileSpan>, Error> {
        loop {
            crate::check_replay_cancelled(&mut || self.cancelled.load(Ordering::Relaxed))?;
            if let Some(index) = self.rows.next() {
                if self.count >= self.limit {
                    return Err(error("profile history exceeds its event budget"));
                }
                let span = self
                    .analysis
                    .as_ref()
                    .and_then(|analysis| analysis.profile_span(index))
                    .ok_or_else(|| error("profile history span is unavailable"))?
                    .clone();
                self.count += 1;
                return Ok(Some(span));
            }
            let Some(frame) = self.next_frame else {
                return Ok(None);
            };
            self.analysis = None;
            let analysis = frame_analysis(self.recording, frame, &mut || {
                self.cancelled.load(Ordering::Relaxed)
            })?;
            // Flush delivery can lag Scene clocks; each root contributes only its newly received rows.
            self.rows = span_range(&analysis, frame)?;
            self.analysis = Some(analysis);
            self.next_frame = (frame < self.recording.frame_count()).then_some(frame + 1);
        }
    }
}

impl Iterator for Spans<'_> {
    type Item = Result<ProfileSpan, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        match self.read_next() {
            Ok(Some(span)) => Some(Ok(span)),
            Ok(None) => None,
            Err(failure) => {
                self.failed = true;
                Some(Err(failure))
            }
        }
    }
}

#[cfg(test)]
mod tests;
