use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
#[cfg(test)]
use std::thread;

use pvd_core::{
    Recording,
    live::{ReplayCursor, SceneUpdate},
};

type LoadResult = Result<(Recording, SceneUpdate), pvd_core::Error>;

struct Request {
    generation: u64,
    recording: Arc<Recording>,
    frame: usize,
    cancelled: Arc<AtomicBool>,
    displayed_generation: Option<u64>,
    with_analysis: bool,
    trace: Option<crate::display_probe::RequestTrace>,
}

pub(super) struct Completed {
    pub trace: Option<crate::display_probe::RequestTrace>,
    pub frame: usize,
    pub expired: bool,
    pub analysis_requested: bool,
    pub result: Result<(Recording, SceneUpdate), String>,
}

#[derive(Default)]
struct State {
    generation: u64,
    pending: Option<Request>,
    completed: Option<(u64, Completed)>,
    stopped: bool,
    active_cancel: Option<Arc<AtomicBool>>,
    displayed_generation: Option<u64>,
    reset: bool,
}

impl State {
    fn cancel_active(&mut self) {
        if let Some(cancelled) = self.active_cancel.take() {
            cancelled.store(true, Ordering::Release);
        }
    }
}

pub(super) struct Worker {
    shared: Arc<(Mutex<State>, Condvar)>,
}

trait Loader: Send + 'static {
    fn load(&mut self, request: &Request) -> LoadResult;
    fn clear(&mut self) {}
}

impl<F: FnMut(&Request) -> LoadResult + Send + 'static> Loader for F {
    fn load(&mut self, request: &Request) -> LoadResult {
        self(request)
    }
}

#[derive(Default)]
struct ReplayLoader {
    cursor: ReplayCursor,
    generation: Option<u64>,
}

impl Loader for ReplayLoader {
    fn load(&mut self, request: &Request) -> LoadResult {
        // An unpublished result cannot become the base of a display delta.
        if self.generation != request.displayed_generation {
            self.clear();
        }
        let cancelled = || request.cancelled.load(Ordering::Acquire);
        if let Some(trace) = &request.trace {
            trace.event("core_begin", 0);
        }
        let (update, analysis) = if request.recording.is_disk_backed() {
            if request.with_analysis {
                let replay = self
                    .cursor
                    .seek(&request.recording, request.frame, cancelled)?;
                (replay.update, replay.analysis)
            } else {
                let replay =
                    self.cursor
                        .seek_scene(&request.recording, request.frame, cancelled)?;
                (replay.update, Default::default())
            }
        } else {
            let scene = request
                .recording
                .state_at_with_cancel(request.frame, cancelled)?;
            let analysis = request
                .recording
                .analysis_at_with_cancel(request.frame, cancelled)?;
            (SceneUpdate::full(&scene), analysis)
        };
        if let Some(trace) = &request.trace {
            trace.event("core_end", 0);
            trace.event("recording_wrap_begin", 0);
        }
        let mut recording = (*request.recording).clone();
        recording.analysis = analysis;
        self.generation = Some(request.generation);
        if let Some(trace) = &request.trace {
            trace.event("recording_wrap_end", 0);
        }
        Ok((recording, update))
    }

    fn clear(&mut self) {
        self.cursor.clear();
        self.generation = None;
    }
}

impl Worker {
    #[cfg(test)]
    pub fn new() -> Self {
        Self::with_loader(ReplayLoader::default())
    }

    pub fn tracked(jobs: &crate::background_jobs::BackgroundJobs) -> std::io::Result<Self> {
        Self::with_registered_loader(jobs, ReplayLoader::default())
    }

    #[cfg(test)]
    fn with_loader(loader: impl Loader) -> Self {
        Self::with_registered_loader(&crate::background_jobs::BackgroundJobs::default(), loader)
            .unwrap()
    }

    fn with_registered_loader(
        jobs: &crate::background_jobs::BackgroundJobs,
        mut loader: impl Loader,
    ) -> std::io::Result<Self> {
        let shared = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let worker = shared.clone();
        let shutdown = shared.clone();
        jobs.spawn_with_cancel(
            "pvd-history-seek",
            move || stop(&shutdown),
            move || {
                let (mutex, wake) = &*worker;
                loop {
                    let request = {
                        let Ok(mut state) = mutex.lock() else { return };
                        while state.pending.is_none() && !state.stopped && !state.reset {
                            let Ok(next) = wake.wait(state) else { return };
                            state = next;
                        }
                        if state.stopped {
                            return;
                        }
                        if state.reset {
                            state.reset = false;
                            drop(state);
                            loader.clear();
                            continue;
                        }
                        state.pending.take().unwrap()
                    };
                    if let Some(trace) = &request.trace {
                        trace.event("worker_begin", 0);
                    }
                    let result = loader.load(&request).map_err(|error| error.to_string());
                    if let Some(trace) = &request.trace {
                        trace.event("worker_end", u64::from(result.is_ok()));
                    }
                    let expired =
                        result.is_err() && !request.recording.is_frame_retained(request.frame);
                    if result.is_err() {
                        loader.clear();
                    }
                    let Ok(mut state) = mutex.lock() else { return };
                    if state.stopped {
                        return;
                    }
                    // A newer cursor request supersedes completed disk work before it reaches the display.
                    if state.generation == request.generation {
                        state.active_cancel = None;
                        state.completed = Some((
                            request.generation,
                            Completed {
                                trace: request.trace,
                                frame: request.frame,
                                expired,
                                analysis_requested: request.with_analysis,
                                result,
                            },
                        ));
                    } else {
                        drop(state);
                        loader.clear();
                    }
                }
            },
        )?;
        Ok(Self { shared })
    }

    pub fn request(&self, recording: Arc<Recording>, frame: usize) {
        self.request_with_analysis(recording, frame, true, None);
    }

    pub fn request_scene(&self, recording: Arc<Recording>, frame: usize) {
        self.request_with_analysis(recording, frame, false, None);
    }

    pub fn request_profiled(
        &self,
        recording: Arc<Recording>,
        frame: usize,
        with_analysis: bool,
        trace: crate::display_probe::Trace,
    ) {
        self.request_with_analysis(recording, frame, with_analysis, Some(trace));
    }

    fn request_with_analysis(
        &self,
        recording: Arc<Recording>,
        frame: usize,
        with_analysis: bool,
        trace: Option<crate::display_probe::Trace>,
    ) {
        let (mutex, wake) = &*self.shared;
        if let Ok(mut state) = mutex.lock() {
            state.cancel_active();
            state.generation = state.generation.wrapping_add(1);
            state.completed = None;
            let cancelled = Arc::new(AtomicBool::new(false));
            state.active_cancel = Some(cancelled.clone());
            state.pending = Some(Request {
                trace: trace.map(|trace| trace.request(frame, state.generation, with_analysis)),
                generation: state.generation,
                recording,
                frame,
                cancelled,
                displayed_generation: state.displayed_generation,
                with_analysis,
            });
            wake.notify_one();
        }
    }

    pub fn cancel(&self) {
        let (mutex, wake) = &*self.shared;
        if let Ok(mut state) = mutex.lock() {
            state.cancel_active();
            state.generation = state.generation.wrapping_add(1);
            state.pending = None;
            state.completed = None;
            state.displayed_generation = None;
            state.reset = true;
            wake.notify_one();
        }
    }

    pub fn poll(&self) -> Option<Completed> {
        let mut state = self.shared.0.lock().ok()?;
        let (generation, completed) = state.completed.take()?;
        state.displayed_generation = completed.result.is_ok().then_some(generation);
        Some(completed)
    }

    #[cfg(test)]
    pub fn generation(&self) -> u64 {
        self.shared.0.lock().unwrap().generation
    }

    #[cfg(test)]
    pub fn has_completed(&self) -> bool {
        self.shared.0.lock().unwrap().completed.is_some()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        stop(&self.shared);
    }
}

fn stop(shared: &(Mutex<State>, Condvar)) {
    let (mutex, wake) = shared;
    if let Ok(mut state) = mutex.lock() {
        state.cancel_active();
        state.stopped = true;
        state.pending = None;
        state.completed = None;
        wake.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[test]
    fn superseding_cancelling_and_dropping_interrupt_inflight_loads() {
        let recording = Arc::new(
            pvd_core::RecordingBuilder::new(pvd_protocol::Header {
                version: 1,
                timestamp_numerator: 1,
                timestamp_denominator: 1,
                flags: 1,
                endian: pvd_protocol::Endian::Little,
            })
            .snapshot()
            .unwrap(),
        );
        for action in 0..3 {
            let (started, start) = mpsc::channel();
            let (cancelled, cancellation) = mpsc::channel();
            let worker = Worker::with_loader(move |request: &Request| {
                if request.frame == 0 {
                    started.send(()).unwrap();
                    let deadline = Instant::now() + Duration::from_secs(3);
                    while !request.cancelled.load(Ordering::Acquire) {
                        assert!(Instant::now() < deadline, "inflight load was not cancelled");
                        thread::sleep(Duration::from_millis(1));
                    }
                    cancelled.send(()).unwrap();
                    return Err(std::io::Error::from(std::io::ErrorKind::Interrupted).into());
                }
                Ok(((*request.recording).clone(), SceneUpdate::default()))
            });
            worker.request(recording.clone(), 0);
            start.recv_timeout(Duration::from_secs(3)).unwrap();
            if action == 2 {
                drop(worker);
                cancellation.recv_timeout(Duration::from_secs(3)).unwrap();
                continue;
            }
            if action == 0 {
                worker.request(recording.clone(), 1);
            } else {
                worker.cancel();
            }
            cancellation.recv_timeout(Duration::from_secs(3)).unwrap();
            if action == 0 {
                let deadline = Instant::now() + Duration::from_secs(3);
                let completed = loop {
                    if let Some(completed) = worker.poll() {
                        break completed;
                    }
                    assert!(Instant::now() < deadline, "replacement load did not finish");
                    thread::sleep(Duration::from_millis(1));
                };
                assert_eq!(completed.frame, 1);
                assert!(completed.result.is_ok());
            } else {
                assert!(worker.poll().is_none());
            }
        }
    }
}
