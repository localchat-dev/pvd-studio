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

#[derive(Default)]
struct State {
    closing: bool,
    next_id: u64,
    jobs: BTreeMap<u64, Cancel>,
    wake: Option<Cancel>,
}

#[derive(Clone, Default)]
pub struct BackgroundJobs(Arc<Mutex<State>>);

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

    pub fn set_wake(&self, wake: impl Fn() + Send + Sync + 'static) {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if state.wake.is_none() {
            state.wake = Some(Arc::new(wake));
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
            state.jobs.insert(id, cancel);
            Completion {
                owner: self.clone(),
                id,
            }
        };
        thread::Builder::new().name(name.into()).spawn(move || {
            // FnOnce drops its captured resource owners before completion becomes observable.
            work();
            drop(completion);
        })
    }

    pub fn cancel_all_and_check_finished(&self) -> bool {
        let cancellations: Vec<_> = {
            let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
            state.closing = true;
            state.jobs.values().cloned().collect()
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
}
