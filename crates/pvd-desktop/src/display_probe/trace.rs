use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use std::time::Instant;

use serde::Serialize;

const MAX_RECORDS: usize = 65_536;
const MAX_PENDING: usize = 128;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Stamp {
    pub frame: usize,
    pub revision: u64,
    pub request: u64,
    pub tick: u64,
    pub analysis_ready: bool,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub(crate) struct Submission {
    pub serial: u64,
    pub tick: u64,
    pub phase: usize,
    pub ui_passes: u64,
    pub scene: Stamp,
    pub ui: Stamp,
}

#[derive(Serialize)]
pub(crate) struct Record {
    pub ns: u64,
    pub stage: &'static str,
    pub phase: usize,
    pub stamp: Stamp,
    pub aux: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submission: Option<Submission>,
}

struct Inner {
    started: Instant,
    records: Mutex<Vec<Record>>,
    failure: Mutex<Option<String>>,
    phase: AtomicUsize,
    request: AtomicU64,
    stop: AtomicUsize,
    pending: AtomicUsize,
}

#[derive(Clone)]
pub(crate) struct Trace(Arc<Inner>);

#[derive(Clone)]
pub(crate) struct RequestTrace {
    trace: Trace,
    pub id: u64,
    pub frame: usize,
    phase: usize,
}

impl Default for Trace {
    fn default() -> Self {
        Self(Arc::new(Inner {
            started: Instant::now(),
            records: Mutex::new(Vec::with_capacity(8_192)),
            failure: Mutex::new(None),
            phase: AtomicUsize::new(0),
            request: AtomicU64::new(0),
            stop: AtomicUsize::new(usize::MAX),
            pending: AtomicUsize::new(0),
        }))
    }
}

impl Trace {
    pub fn phase(&self) -> usize {
        self.0.phase.load(Ordering::Relaxed)
    }
    pub fn set_phase(&self, value: usize) {
        self.0.phase.store(value, Ordering::Relaxed);
    }
    pub fn stop_at(&self, frame: usize) -> bool {
        self.0.stop.load(Ordering::Relaxed) == frame
    }
    pub fn set_stop(&self, frame: usize) {
        self.0.stop.store(frame, Ordering::Relaxed);
    }
    pub fn elapsed(&self) -> std::time::Duration {
        self.0.started.elapsed()
    }
    pub fn pending(&self) -> usize {
        self.0.pending.load(Ordering::Acquire)
    }
    pub fn failure(&self) -> Option<String> {
        self.0.failure.lock().unwrap().clone()
    }
    pub fn fail(&self, error: impl Into<String>) {
        self.0
            .failure
            .lock()
            .unwrap()
            .get_or_insert_with(|| error.into());
    }
    pub fn event(&self, stage: &'static str, stamp: Stamp, aux: u64) {
        self.push(stage, self.phase(), stamp, aux, None);
    }
    fn push(
        &self,
        stage: &'static str,
        phase: usize,
        stamp: Stamp,
        aux: u64,
        submission: Option<Submission>,
    ) {
        if phase == 0 && stamp.request == 0 {
            return;
        }
        let record = Record {
            ns: self.0.started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            stage,
            phase,
            stamp,
            aux,
            submission,
        };
        let mut records = self.0.records.lock().unwrap();
        if records.len() == MAX_RECORDS {
            drop(records);
            self.fail("trace record capacity exceeded");
        } else {
            records.push(record);
        }
    }
    pub fn request(&self, frame: usize, generation: u64, analysis: bool) -> RequestTrace {
        let id = self.0.request.fetch_add(1, Ordering::Relaxed) + 1;
        let request = RequestTrace {
            trace: self.clone(),
            id,
            frame,
            phase: self.phase(),
        };
        request.event(
            if analysis {
                "request_full"
            } else {
                "request_scene"
            },
            generation,
        );
        request
    }
    pub fn submission_event(&self, stage: &'static str, submission: Submission) {
        self.push(
            stage,
            submission.phase,
            submission.scene,
            submission.serial,
            Some(submission),
        );
    }
    pub fn submitted(&self, submission: Submission) -> bool {
        if self
            .0
            .pending
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                (value < MAX_PENDING).then_some(value + 1)
            })
            .is_err()
        {
            self.fail("GPU completion capacity exceeded");
            return false;
        }
        self.push(
            "render_return_observed",
            submission.phase,
            submission.scene,
            0,
            Some(submission),
        );
        true
    }
    pub fn completed(&self, submission: Submission) {
        self.push(
            "gpu_completed",
            submission.phase,
            submission.scene,
            0,
            Some(submission),
        );
        if self
            .0
            .pending
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_sub(1)
            })
            .is_err()
        {
            self.fail("unexpected GPU completion");
        }
    }
    pub fn with_records<R>(&self, read: impl FnOnce(&[Record]) -> R) -> R {
        read(&self.0.records.lock().unwrap())
    }
}

impl RequestTrace {
    pub fn event(&self, stage: &'static str, aux: u64) {
        self.trace.push(
            stage,
            self.phase,
            Stamp {
                frame: self.frame,
                request: self.id,
                ..Default::default()
            },
            aux,
            None,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callbacks_retain_submission_identity_and_capacity_is_bounded() {
        let trace = Trace::default();
        let mut tokens = Vec::new();
        for serial in 0..MAX_PENDING as u64 {
            let token = Submission {
                serial,
                tick: serial,
                phase: 2,
                ui_passes: 1,
                scene: Stamp {
                    frame: 540 + serial as usize,
                    ..Default::default()
                },
                ui: Stamp {
                    frame: 541 + serial as usize,
                    ..Default::default()
                },
            };
            assert!(trace.submitted(token));
            tokens.push(token);
        }
        assert!(!trace.submitted(tokens[0]));
        for token in tokens.iter().rev() {
            trace.completed(*token);
        }
        assert_eq!(trace.pending(), 0);
        trace.with_records(|records| {
            assert_eq!(records.len(), MAX_PENDING * 2);
            assert_eq!(records.last().unwrap().submission.unwrap().serial, 0);
            assert_eq!(records.last().unwrap().submission.unwrap().ui.frame, 541);
        });
        assert!(trace.failure().is_some());
    }
}
