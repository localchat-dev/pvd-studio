//! Opt-in aggregate diagnostics for one recording. No event payloads are retained.
//!
//! Inclusive times contain measured child stages; self times exclude children
//! from the same collector. Timings include instrumentation overhead. Snapshots
//! taken during ingestion may observe counters at different completion points.
//! Progress mode counts only coarse scopes, without a timing stack or clock reads.

use serde::Serialize;
use std::{
    cell::RefCell,
    marker::PhantomData,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Instant,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(usize)]
pub enum Stage {
    ProtocolParse,
    BuilderPush,
    PropertyMessage,
    Action,
    Analysis,
    FrameComplete,
    FrameCapture,
    HistoryReservation,
    HistoryCollect,
    HistoryCompact,
    HistoryEvict,
    HistoryAppend,
    HistoryAnalysisPrepare,
    HistorySceneCheckpoint,
    HistoryAnalysisReclaim,
    HistorySerialize,
    HistoryChecksum,
    HistoryWrite,
    HistoryFlush,
    SnapshotPublish,
    AnalysisMaintenance,
    AnalysisIngest,
    HistorySeal,
}

impl Stage {
    pub const ALL: [Self; 23] = [
        Self::ProtocolParse,
        Self::BuilderPush,
        Self::PropertyMessage,
        Self::Action,
        Self::Analysis,
        Self::FrameComplete,
        Self::FrameCapture,
        Self::HistoryReservation,
        Self::HistoryCollect,
        Self::HistoryCompact,
        Self::HistoryEvict,
        Self::HistoryAppend,
        Self::HistoryAnalysisPrepare,
        Self::HistorySceneCheckpoint,
        Self::HistoryAnalysisReclaim,
        Self::HistorySerialize,
        Self::HistoryChecksum,
        Self::HistoryWrite,
        Self::HistoryFlush,
        Self::SnapshotPublish,
        Self::AnalysisMaintenance,
        Self::AnalysisIngest,
        Self::HistorySeal,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProtocolParse => "protocol_parse",
            Self::BuilderPush => "builder_push",
            Self::PropertyMessage => "property_message",
            Self::Action => "action",
            Self::Analysis => "analysis",
            Self::FrameComplete => "frame_complete",
            Self::FrameCapture => "frame_capture",
            Self::HistoryReservation => "history_reservation",
            Self::HistoryCollect => "history_collect",
            Self::HistoryCompact => "history_compact",
            Self::HistoryEvict => "history_evict",
            Self::HistoryAppend => "history_append",
            Self::HistoryAnalysisPrepare => "history_analysis_prepare",
            Self::HistorySceneCheckpoint => "history_scene_checkpoint",
            Self::HistoryAnalysisReclaim => "history_analysis_reclaim",
            Self::HistorySerialize => "history_serialize",
            Self::HistoryChecksum => "history_checksum",
            Self::HistoryWrite => "history_write",
            Self::HistoryFlush => "history_flush",
            Self::SnapshotPublish => "snapshot_publish",
            Self::AnalysisMaintenance => "analysis_maintenance",
            Self::AnalysisIngest => "analysis_ingest",
            Self::HistorySeal => "history_seal",
        }
    }

    fn progress_observed(self) -> bool {
        matches!(
            self,
            Self::FrameComplete
                | Self::FrameCapture
                | Self::HistoryReservation
                | Self::HistoryCollect
                | Self::HistoryCompact
                | Self::HistoryEvict
                | Self::HistoryAppend
                | Self::HistoryAnalysisPrepare
                | Self::HistorySceneCheckpoint
                | Self::HistoryAnalysisReclaim
                | Self::HistorySeal
                | Self::SnapshotPublish
        )
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct StageMetrics {
    pub stage: Stage,
    pub calls: u64,
    pub started: u64,
    /// Finished scopes include errors and unwinding, rather than successful commits.
    pub completed: u64,
    pub in_flight: u64,
    pub inclusive_ns: u64,
    pub self_ns: u64,
    pub max_ns: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct MetricsSnapshot {
    pub enabled: bool,
    pub timings_enabled: bool,
    /// The last directory insertion observed by this collector, not a published network snapshot.
    /// It remains absent until a commit is observed after diagnostics are enabled.
    pub last_committed_frame: Option<usize>,
    pub stages: Vec<StageMetrics>,
}

#[derive(Default)]
struct Counter {
    calls: AtomicU64,
    started: AtomicU64,
    in_flight: AtomicU64,
    inclusive_ns: AtomicU64,
    self_ns: AtomicU64,
    max_ns: AtomicU64,
}

impl Counter {
    fn enter(&self) {
        self.started.fetch_add(1, Ordering::Relaxed);
        self.in_flight.fetch_add(1, Ordering::Relaxed);
    }

    fn complete(&self) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
    }

    fn record(&self, elapsed: u64, children: u64) {
        self.inclusive_ns.fetch_add(elapsed, Ordering::Relaxed);
        self.self_ns
            .fetch_add(elapsed.saturating_sub(children), Ordering::Relaxed);
        self.max_ns.fetch_max(elapsed, Ordering::Relaxed);
        self.complete();
    }
}

#[derive(Default)]
struct Counters {
    progress_only: bool,
    committed_frame: AtomicUsize,
    stages: [Counter; Stage::ALL.len()],
}

/// A cloneable observer; disabled collectors do not read the clock or enter TLS.
#[derive(Clone, Default)]
pub struct Metrics {
    counters: Option<Arc<Counters>>,
}

impl Metrics {
    pub(crate) fn enable(&mut self) {
        self.counters.get_or_insert_with(Default::default);
    }

    pub(crate) fn enable_progress(&mut self) {
        self.counters.get_or_insert_with(|| {
            Arc::new(Counters {
                progress_only: true,
                ..Default::default()
            })
        });
    }

    pub(crate) fn committed(&self, frame: usize) {
        if let Some(counters) = &self.counters {
            counters
                .committed_frame
                .store(frame.checked_add(1).unwrap_or(0), Ordering::Relaxed);
        }
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            enabled: self.counters.is_some(),
            timings_enabled: self
                .counters
                .as_ref()
                .is_some_and(|counters| !counters.progress_only),
            last_committed_frame: self.counters.as_ref().and_then(|counters| {
                counters
                    .committed_frame
                    .load(Ordering::Relaxed)
                    .checked_sub(1)
            }),
            stages: Stage::ALL
                .into_iter()
                .filter(|stage| {
                    self.counters
                        .as_ref()
                        .is_none_or(|counters| !counters.progress_only || stage.progress_observed())
                })
                .map(|stage| {
                    let counter = self.counters.as_ref().map(|c| &c.stages[stage as usize]);
                    let completed = counter.map_or(0, |c| c.calls.load(Ordering::Relaxed));
                    StageMetrics {
                        stage,
                        calls: completed,
                        started: counter.map_or(0, |c| c.started.load(Ordering::Relaxed)),
                        completed,
                        in_flight: counter.map_or(0, |c| c.in_flight.load(Ordering::Relaxed)),
                        inclusive_ns: counter.map_or(0, |c| c.inclusive_ns.load(Ordering::Relaxed)),
                        self_ns: counter.map_or(0, |c| c.self_ns.load(Ordering::Relaxed)),
                        max_ns: counter.map_or(0, |c| c.max_ns.load(Ordering::Relaxed)),
                    }
                })
                .collect(),
        }
    }

    pub(crate) fn measure(&self, stage: Stage) -> Guard {
        let active = self
            .counters
            .as_ref()
            .filter(|counters| !counters.progress_only || stage.progress_observed())
            .map(|counters| {
                counters.stages[stage as usize].enter();
                let counters = counters.clone();
                if counters.progress_only {
                    Active::Progress { counters, stage }
                } else {
                    let started = Instant::now();
                    STACK.with(|stack| {
                        stack.borrow_mut().push(Span {
                            owner: Arc::as_ptr(&counters) as usize,
                            children_ns: 0,
                        });
                    });
                    Active::Timed {
                        counters,
                        stage,
                        started,
                    }
                }
            });
        Guard {
            active,
            thread_bound: PhantomData,
        }
    }
}

struct Span {
    owner: usize,
    children_ns: u64,
}

thread_local! {
    static STACK: RefCell<Vec<Span>> = const { RefCell::new(Vec::new()) };
}

enum Active {
    Timed {
        counters: Arc<Counters>,
        stage: Stage,
        started: Instant,
    },
    Progress {
        counters: Arc<Counters>,
        stage: Stage,
    },
}

pub(crate) struct Guard {
    active: Option<Active>,
    // The nesting stack belongs to the thread that entered this scope.
    thread_bound: PhantomData<Rc<()>>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let Some(active) = &self.active else {
            return;
        };
        let (counters, stage, started) = match active {
            Active::Timed {
                counters,
                stage,
                started,
            } => (counters, stage, started),
            Active::Progress { counters, stage } => {
                counters.stages[*stage as usize].complete();
                return;
            }
        };
        let elapsed = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        let owner = Arc::as_ptr(counters) as usize;
        let children = STACK.with(|stack| {
            let mut stack = stack.borrow_mut();
            let span = stack.pop().expect("timing scopes must be nested");
            debug_assert_eq!(span.owner, owner);
            if let Some(parent) = stack.iter_mut().rev().find(|span| span.owner == owner) {
                parent.children_ns = parent.children_ns.saturating_add(elapsed);
            }
            span.children_ns
        });
        counters.stages[*stage as usize].record(elapsed, children);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_collectors_never_enter_the_timing_stack() {
        let metrics = Metrics::default();
        let timer = metrics.measure(Stage::Action);
        assert!(timer.active.is_none());
        assert!(STACK.with(|stack| stack.borrow().is_empty()));
        let snapshot = metrics.snapshot();
        assert!(!snapshot.enabled);
        assert!(!snapshot.timings_enabled);
        assert_eq!(snapshot.last_committed_frame, None);
        assert!(snapshot.stages.iter().all(|stage| stage.calls == 0));
    }

    #[test]
    fn self_time_excludes_children_and_observers_share_counts() {
        let mut metrics = Metrics::default();
        metrics.enable();
        let observer = metrics.clone();
        {
            let _outer = metrics.measure(Stage::BuilderPush);
            let _inner = metrics.measure(Stage::Action);
            std::hint::black_box(42);
        }
        let snapshot = observer.snapshot();
        let outer = &snapshot.stages[Stage::BuilderPush as usize];
        let inner = &snapshot.stages[Stage::Action as usize];
        assert_eq!((outer.calls, inner.calls), (1, 1));
        assert_eq!(outer.inclusive_ns - outer.self_ns, inner.inclusive_ns);
        assert_eq!(inner.self_ns, inner.inclusive_ns);
        assert_eq!(outer.max_ns, outer.inclusive_ns);
        assert!(STACK.with(|stack| stack.borrow().is_empty()));
    }

    #[test]
    fn collectors_remain_separate_on_one_thread_and_across_threads() {
        let mut first = Metrics::default();
        first.enable();
        let mut second = Metrics::default();
        second.enable();
        {
            let _outer = first.measure(Stage::BuilderPush);
            let _foreign = second.measure(Stage::Action);
            let _inner = first.measure(Stage::Action);
        }
        let snapshot = first.snapshot();
        let outer = &snapshot.stages[Stage::BuilderPush as usize];
        let inner = &snapshot.stages[Stage::Action as usize];
        assert_eq!(outer.inclusive_ns - outer.self_ns, inner.inclusive_ns);
        assert_eq!(second.snapshot().stages[Stage::Action as usize].calls, 1);
        let worker = first.clone();
        std::thread::spawn(move || {
            let _timer = worker.measure(Stage::Action);
        })
        .join()
        .unwrap();
        assert_eq!(first.snapshot().stages[Stage::Action as usize].calls, 2);
        assert_eq!(second.snapshot().stages[Stage::Action as usize].calls, 1);
    }

    #[test]
    fn failed_scopes_release_the_stack() {
        let mut metrics = Metrics::default();
        metrics.enable();
        let result = std::panic::catch_unwind(|| {
            let _timer = metrics.measure(Stage::Action);
            panic!("diagnostic test failure");
        });
        assert!(result.is_err());
        assert!(STACK.with(|stack| stack.borrow().is_empty()));
        assert_eq!(metrics.snapshot().stages[Stage::Action as usize].calls, 1);
    }

    fn stage(snapshot: &MetricsSnapshot, stage: Stage) -> &StageMetrics {
        snapshot
            .stages
            .iter()
            .find(|entry| entry.stage == stage)
            .unwrap()
    }

    fn assert_untimed(snapshot: &MetricsSnapshot) {
        assert!(snapshot.enabled);
        assert!(!snapshot.timings_enabled);
        assert!(snapshot.stages.iter().all(|entry| {
            entry.inclusive_ns == 0
                && entry.self_ns == 0
                && entry.max_ns == 0
                && entry.calls == entry.completed
                && entry.stage.progress_observed()
        }));
        assert!(STACK.with(|stack| stack.borrow().is_empty()));
    }

    #[test]
    fn progress_observers_count_nested_scopes_without_timing_or_high_frequency_entries() {
        let mut metrics = Metrics::default();
        metrics.enable_progress();
        let observer = metrics.clone();
        let outer = metrics.measure(Stage::HistoryAppend);
        let inner = metrics.measure(Stage::HistoryAnalysisPrepare);
        for stage in [Stage::BuilderPush, Stage::PropertyMessage, Stage::Action] {
            assert!(metrics.measure(stage).active.is_none());
        }
        let snapshot = observer.snapshot();
        assert_untimed(&snapshot);
        assert_eq!(snapshot.last_committed_frame, None);
        assert!(
            !snapshot
                .stages
                .iter()
                .any(|entry| entry.stage == Stage::Action)
        );
        for name in [Stage::HistoryAppend, Stage::HistoryAnalysisPrepare] {
            let entry = stage(&snapshot, name);
            assert_eq!((entry.started, entry.completed, entry.in_flight), (1, 0, 1));
        }
        drop(inner);
        let snapshot = observer.snapshot();
        assert_untimed(&snapshot);
        assert_eq!(stage(&snapshot, Stage::HistoryAppend).in_flight, 1);
        let entry = stage(&snapshot, Stage::HistoryAnalysisPrepare);
        assert_eq!((entry.started, entry.completed, entry.in_flight), (1, 1, 0));
        drop(outer);
        let snapshot = observer.snapshot();
        assert_untimed(&snapshot);
        assert!(snapshot.stages.iter().all(|entry| entry.in_flight == 0));
    }

    #[test]
    fn progress_scopes_finish_on_error_and_unwind_without_a_timing_stack() {
        let mut metrics = Metrics::default();
        metrics.enable_progress();
        let failure = || -> Result<(), ()> {
            let _guard = metrics.measure(Stage::HistoryAnalysisReclaim);
            assert_eq!(
                stage(&metrics.snapshot(), Stage::HistoryAnalysisReclaim).in_flight,
                1
            );
            Err(())
        };
        assert!(failure().is_err());
        let failure = std::panic::catch_unwind(|| {
            let _guard = metrics.measure(Stage::HistorySceneCheckpoint);
            assert_untimed(&metrics.snapshot());
            panic!("progress scope unwind");
        });
        assert!(failure.is_err());
        let snapshot = metrics.snapshot();
        assert_untimed(&snapshot);
        for name in [Stage::HistoryAnalysisReclaim, Stage::HistorySceneCheckpoint] {
            let entry = stage(&snapshot, name);
            assert_eq!((entry.started, entry.completed, entry.in_flight), (1, 1, 0));
        }
    }

    #[test]
    fn progress_observers_share_inflight_counts_and_keep_the_selected_mode() {
        let mut metrics = Metrics::default();
        metrics.enable_progress();
        metrics.enable();
        let worker = metrics.clone();
        let (started, entered) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let _guard = worker.measure(Stage::HistorySeal);
            assert_untimed(&worker.snapshot());
            started.send(()).unwrap();
            released.recv().unwrap();
        });
        entered.recv().unwrap();
        metrics.enable();
        let snapshot = metrics.snapshot();
        let entry = stage(&snapshot, Stage::HistorySeal);
        assert_eq!((entry.started, entry.completed, entry.in_flight), (1, 0, 1));
        assert_eq!(entry.stage.as_str(), "history_seal");
        assert_eq!(
            serde_json::to_string(&entry.stage).unwrap(),
            "\"history_seal\""
        );
        release.send(()).unwrap();
        thread.join().unwrap();
        let snapshot = metrics.snapshot();
        assert_untimed(&snapshot);
        let entry = stage(&snapshot, Stage::HistorySeal);
        assert_eq!((entry.started, entry.completed, entry.in_flight), (1, 1, 0));
        let mut timed = Metrics::default();
        timed.enable();
        let _timer = timed.measure(Stage::HistoryAppend);
        timed.enable_progress();
        assert!(timed.snapshot().timings_enabled);
    }
}
