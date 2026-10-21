use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static ATTEMPT: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy)]
#[repr(u8)]
pub(super) enum Phase {
    RootPage,
    LiveMap,
    BaseMap,
    Sequences,
    Metadata,
    Construction,
    Validate,
    Publish,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::RootPage => "root_page",
            Self::LiveMap => "live_map",
            Self::BaseMap => "base_map",
            Self::Sequences => "sequences",
            Self::Metadata => "metadata",
            Self::Construction => "construction",
            Self::Validate => "validate",
            Self::Publish => "publish",
        }
    }
}

struct Active {
    root: u64,
    attempt: u64,
    started: Instant,
    phase_started: Instant,
    phase: Phase,
    durations: [Duration; 8],
    completed_mask: u8,
    complete: bool,
    total: Duration,
}

impl Active {
    fn record(&mut self, now: Instant, completed: bool) {
        self.durations[self.phase as usize] += now.duration_since(self.phase_started);
        if completed {
            self.completed_mask |= 1 << self.phase as u8;
        }
        self.total = now.duration_since(self.started);
    }
}

pub(super) struct ReadTrace(Option<Active>);

impl ReadTrace {
    pub(super) fn from_env(root: u64) -> Self {
        Self(std::env::var_os("PVD_SHARED_READ_DIAGNOSTICS").map(|_| {
            let started = Instant::now();
            Active {
                root,
                attempt: ATTEMPT.fetch_add(1, Ordering::Relaxed),
                started,
                phase_started: started,
                phase: Phase::RootPage,
                durations: [Duration::ZERO; 8],
                completed_mask: 0,
                complete: false,
                total: Duration::ZERO,
            }
        }))
    }

    pub(super) fn advance(&mut self, phase: Phase) {
        if let Some(active) = &mut self.0 {
            let now = Instant::now();
            active.record(now, true);
            active.phase = phase;
            active.phase_started = now;
        }
    }

    pub(super) fn finish(&mut self) {
        if let Some(active) = &mut self.0 {
            active.record(Instant::now(), true);
            active.complete = true;
        }
    }
}

impl Drop for ReadTrace {
    fn drop(&mut self) {
        let Some(active) = &mut self.0 else {
            return;
        };
        if !active.complete {
            active.record(Instant::now(), false);
        }
        let outcome = if active.complete {
            "complete"
        } else if std::thread::panicking() {
            "panic"
        } else {
            "error"
        };
        let incomplete = if active.complete {
            "none"
        } else {
            active.phase.name()
        };
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(
            stderr,
            "analysis_root_read root={} attempt={} thread={:?} outcome={} incomplete={} completed_mask={} root_page_ns={} live_map_ns={} base_map_ns={} sequences_ns={} metadata_ns={} construction_ns={} validate_ns={} publish_ns={} elapsed_ns={}",
            active.root,
            active.attempt,
            std::thread::current().id(),
            outcome,
            incomplete,
            active.completed_mask,
            active.durations[Phase::RootPage as usize].as_nanos(),
            active.durations[Phase::LiveMap as usize].as_nanos(),
            active.durations[Phase::BaseMap as usize].as_nanos(),
            active.durations[Phase::Sequences as usize].as_nanos(),
            active.durations[Phase::Metadata as usize].as_nanos(),
            active.durations[Phase::Construction as usize].as_nanos(),
            active.durations[Phase::Validate as usize].as_nanos(),
            active.durations[Phase::Publish as usize].as_nanos(),
            active.total.as_nanos(),
        );
    }
}
