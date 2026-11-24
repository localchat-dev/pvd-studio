use crate::sessions::Snapshot;
use pvd_core::capture::{CaptureLimits, CaptureRange, PortableReport, write_portable_new};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

#[derive(Default)]
pub struct SessionExport {
    session: u64,
    path: PathBuf,
    worker: Option<JoinHandle<Result<PortableReport, String>>>,
    cancel: Arc<AtomicBool>,
    result: Option<Result<PortableReport, String>>,
}

impl SessionExport {
    pub fn start(&mut self, session: u64, path: PathBuf, snapshot: Snapshot) -> Result<(), String> {
        self.poll();
        if self.worker.is_some() {
            return Err("A session export is already running.".into());
        }
        if !path.is_absolute()
            || !path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("pvdcapture"))
        {
            return Err("Choose an absolute .pvdcapture file path.".into());
        }
        let range = CaptureRange::retained(&snapshot.recording)
            .ok_or("The recording has no frames to export.")?;
        let destination = path.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = cancel.clone();
        let worker = thread::Builder::new()
            .name("pvd-unity-export".into())
            .spawn(move || {
                let report = write_portable_new(
                    &snapshot.recording,
                    &destination,
                    range,
                    &CaptureLimits::default(),
                    512 << 20,
                    |_| !cancelled.load(Ordering::Acquire),
                )
                .map_err(|e| e.to_string());
                drop(snapshot);
                report
            })
            .map_err(|e| e.to_string())?;
        *self = Self {
            session,
            path,
            worker: Some(worker),
            cancel,
            result: None,
        };
        Ok(())
    }

    fn join(&mut self) {
        if let Some(worker) = self.worker.take() {
            self.result = Some(
                worker
                    .join()
                    .unwrap_or_else(|_| Err("Export worker panicked.".into())),
            );
        }
    }

    fn poll(&mut self) {
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            self.join();
        }
    }

    pub fn cancel_and_join(&mut self) {
        self.cancel.store(true, Ordering::Release);
        self.join();
    }

    pub fn status(&mut self) -> Value {
        self.poll();
        if self.session == 0 {
            return Value::Null;
        }
        let mut status = json!({
            "running": self.worker.is_some(), "session": self.session, "path": self.path, "error": "",
        });
        match &self.result {
            Some(Ok(report)) => {
                status["frames"] = json!(report.frames);
                status["parts"] = json!(report.parts);
                status["bytes"] = json!(report.bytes);
            }
            Some(Err(error)) => status["error"] = json!(error),
            None => (),
        }
        status
    }
}

impl Drop for SessionExport {
    fn drop(&mut self) {
        self.cancel_and_join();
    }
}
