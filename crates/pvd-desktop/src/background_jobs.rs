use std::{
    collections::BTreeMap,
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

const MAX_JOBS: usize = 128;
type Cancel = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum JobKind {
    Work,
    Retirement,
}

struct Job {
    name: String,
    cancel: Cancel,
    kind: JobKind,
}

#[derive(Default)]
struct State {
    closing: bool,
    next_id: u64,
    jobs: BTreeMap<u64, Job>,
    wake: Option<Cancel>,
}

#[derive(Clone, Default)]
pub struct BackgroundJobs(Arc<Mutex<State>>);

pub struct JobControl {
    owner: BackgroundJobs,
    id: u64,
}

impl JobControl {
    /// Mark only after work, permanent writes and input reception have stopped.
    /// The remainder may release ephemeral owners and temporary files only.
    pub fn begin_cleanup(self, name: &str) {
        let wake = {
            let mut state = self
                .owner
                .0
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(job) = state.jobs.get_mut(&self.id) {
                job.kind = JobKind::Retirement;
                job.name = name.into();
            }
            state.wake.clone()
        };
        if let Some(wake) = wake {
            wake();
        }
    }
}

struct Completion {
    owner: BackgroundJobs,
    id: u64,
}

impl Drop for Completion {
    fn drop(&mut self) {
        let wake = {
            let mut state = self
                .owner
                .0
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.jobs.remove(&self.id);
            state.wake.clone()
        };
        if let Some(wake) = wake {
            wake();
        }
    }
}

impl BackgroundJobs {
    pub fn active_count(&self) -> usize {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .jobs
            .len()
    }

    pub fn active_names(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .jobs
            .values()
            .map(|job| job.name.clone())
            .collect()
    }

    pub fn active_work_count(&self) -> usize {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .jobs
            .values()
            .filter(|job| job.kind == JobKind::Work)
            .count()
    }

    pub fn set_wake(&self, wake: impl Fn() + Send + Sync + 'static) {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if state.wake.is_none() {
            state.wake = Some(Arc::new(wake));
        }
    }

    pub(crate) fn wake(&self) {
        let wake = self
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .wake
            .clone();
        if let Some(wake) = wake {
            wake();
        }
    }

    pub fn spawn(
        &self,
        name: &str,
        cancelled: Arc<AtomicBool>,
        work: impl FnOnce() + Send + 'static,
    ) -> io::Result<JoinHandle<()>> {
        self.spawn_with_cancel(name, move || cancelled.store(true, Ordering::Release), work)
    }

    pub fn spawn_with_cancel(
        &self,
        name: &str,
        cancel: impl Fn() + Send + Sync + 'static,
        work: impl FnOnce() + Send + 'static,
    ) -> io::Result<JoinHandle<()>> {
        self.spawn_with_cancel_and_control(name, cancel, move |_| work())
    }

    pub fn spawn_with_cancel_and_control(
        &self,
        name: &str,
        cancel: impl Fn() + Send + Sync + 'static,
        work: impl FnOnce(JobControl) + Send + 'static,
    ) -> io::Result<JoinHandle<()>> {
        let cancel: Cancel = Arc::new(cancel);
        let completion = {
            let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
            if state.closing || state.jobs.len() >= MAX_JOBS {
                let closing = state.closing;
                drop(state);
                cancel();
                return Err(io::Error::new(
                    if closing {
                        io::ErrorKind::Interrupted
                    } else {
                        io::ErrorKind::WouldBlock
                    },
                    if closing {
                        "application shutdown has started"
                    } else {
                        "previous background jobs are still releasing resources"
                    },
                ));
            }
            let id = state.next_id;
            state.next_id = state
                .next_id
                .checked_add(1)
                .ok_or_else(|| io::Error::other("background job identifier overflow"))?;
            state.jobs.insert(
                id,
                Job {
                    name: name.into(),
                    cancel,
                    kind: JobKind::Work,
                },
            );
            Completion {
                owner: self.clone(),
                id,
            }
        };
        let control = JobControl {
            owner: self.clone(),
            id: completion.id,
        };
        thread::Builder::new().name(name.into()).spawn(move || {
            // FnOnce drops its captured resource owners before completion becomes observable.
            work(control);
            drop(completion);
        })
    }

    /// Retire an owner without running its destructor on the caller, including
    /// when thread creation fails. Shutdown may retry the returned owner.
    pub fn retire<T: Send + 'static>(&self, value: T) -> Result<(), (io::Error, T)> {
        let completion = {
            let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
            // Reserve one extra slot for releasing the application's owners;
            // ordinary work remains rejected after shutdown starts.
            if state.jobs.len() > MAX_JOBS {
                return Err((
                    io::Error::new(io::ErrorKind::WouldBlock, "resource retirement is pending"),
                    value,
                ));
            }
            let id = state.next_id;
            let Some(next_id) = id.checked_add(1) else {
                return Err((
                    io::Error::other("background job identifier overflow"),
                    value,
                ));
            };
            state.next_id = next_id;
            state.jobs.insert(
                id,
                Job {
                    name: "pvd-studio-release".into(),
                    cancel: Arc::new(|| {}),
                    kind: JobKind::Retirement,
                },
            );
            Completion {
                owner: self.clone(),
                id,
            }
        };
        let owned = Arc::new(Mutex::new(Some(value)));
        let worker_owned = owned.clone();
        match thread::Builder::new()
            .name("pvd-studio-release".into())
            .spawn(move || {
                let value = worker_owned
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take();
                drop(value);
                drop(completion);
            }) {
            Ok(worker) => {
                drop(worker);
                Ok(())
            }
            Err(error) => {
                let value = owned
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take()
                    .expect("failed retirement thread did not consume its owner");
                Err((error, value))
            }
        }
    }

    pub fn cancel_all_and_check_finished(&self) -> bool {
        let cancellations: Vec<_> = {
            let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
            state.closing = true;
            state.jobs.values().map(|job| job.cancel.clone()).collect()
        };
        for cancel in cancellations {
            cancel();
        }
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .jobs
            .is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};

    #[test]
    fn detached_job_completes_only_after_captured_resources_are_released() {
        struct Resource(mpsc::Sender<()>, mpsc::Receiver<()>);
        impl Drop for Resource {
            fn drop(&mut self) {
                self.0.send(()).unwrap();
                self.1.recv_timeout(Duration::from_secs(3)).unwrap();
            }
        }
        let jobs = BackgroundJobs::default();
        let cancelled = Arc::new(AtomicBool::new(false));
        let (started, dropping) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let resource = Resource(started, released);
        drop(
            jobs.spawn("resource-lifetime-test", cancelled.clone(), move || {
                let _ = &resource;
            })
            .unwrap(),
        );
        dropping.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(!jobs.cancel_all_and_check_finished());
        assert!(cancelled.load(Ordering::Acquire));
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !jobs.cancel_all_and_check_finished() {
            assert!(std::time::Instant::now() < deadline);
            thread::yield_now();
        }
    }

    #[test]
    fn shutdown_is_scoped_to_its_owner_and_rejects_new_jobs() {
        let first = BackgroundJobs::default();
        let second = BackgroundJobs::default();
        let first_cancel = Arc::new(AtomicBool::new(false));
        let second_cancel = Arc::new(AtomicBool::new(false));
        let (release_first, first_wait) = mpsc::channel();
        let (release_second, second_wait) = mpsc::channel();
        let a = first
            .spawn("first-owner", first_cancel.clone(), move || {
                first_wait.recv().unwrap();
            })
            .unwrap();
        let b = second
            .spawn("second-owner", second_cancel.clone(), move || {
                second_wait.recv().unwrap();
            })
            .unwrap();
        assert!(!first.cancel_all_and_check_finished());
        assert!(first_cancel.load(Ordering::Acquire));
        assert!(!second_cancel.load(Ordering::Acquire));
        assert_eq!(
            first
                .spawn(
                    "after-shutdown",
                    Arc::new(AtomicBool::new(false)),
                    || panic!("closed registry accepted work")
                )
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        release_first.send(()).unwrap();
        release_second.send(()).unwrap();
        a.join().unwrap();
        b.join().unwrap();
        assert!(first.cancel_all_and_check_finished());
        assert!(second.cancel_all_and_check_finished());
    }

    #[test]
    fn failed_retirement_returns_owner_without_running_its_destructor() {
        struct Resource(Arc<AtomicBool>);
        impl Drop for Resource {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let jobs = BackgroundJobs::default();
        jobs.0.lock().unwrap().next_id = u64::MAX;
        let released = Arc::new(AtomicBool::new(false));
        let Err((error, owned)) = jobs.retire(Resource(released.clone())) else {
            panic!("exhausted registry accepted retirement");
        };
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(!released.load(Ordering::Acquire));
        assert_eq!(jobs.active_count(), 0);
        drop(owned);
        assert!(released.load(Ordering::Acquire));
    }

    #[test]
    fn explicit_cleanup_transition_keeps_completion_tracked() {
        let jobs = BackgroundJobs::default();
        let (finish_work, working) = mpsc::channel();
        let (cleaning, cleanup) = mpsc::channel();
        let (finish_cleanup, releasing) = mpsc::channel();
        let worker = jobs
            .spawn_with_cancel_and_control(
                "input",
                || {},
                move |control| {
                    working.recv_timeout(Duration::from_secs(3)).unwrap();
                    control.begin_cleanup("temporary-files");
                    cleaning.send(()).unwrap();
                    releasing.recv_timeout(Duration::from_secs(3)).unwrap();
                },
            )
            .unwrap();
        assert_eq!(jobs.active_work_count(), 1);
        finish_work.send(()).unwrap();
        cleanup.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(jobs.active_work_count(), 0);
        assert_eq!(jobs.active_count(), 1);
        assert_eq!(jobs.active_names(), vec!["temporary-files"]);
        assert!(!jobs.cancel_all_and_check_finished());
        finish_cleanup.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(jobs.active_count(), 0);
    }
}
