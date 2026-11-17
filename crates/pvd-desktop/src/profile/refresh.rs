use crate::i18n::Message;
use bevy_egui::egui;
use pvd_core::{
    Recording,
    analysis::{Analysis, SharedAnalysis},
};
use std::{
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const LIVE_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Clone, Default)]
struct RefreshGate {
    epoch: Option<u64>,
    started: Option<Instant>,
}

impl RefreshGate {
    fn change_epoch(&mut self, epoch: u64) -> bool {
        if self.epoch == Some(epoch) {
            return false;
        }
        self.epoch = Some(epoch);
        self.started = None;
        true
    }

    fn due(&self, following: bool, pending: bool, changed: bool, now: Instant) -> bool {
        changed
            && !pending
            && (!following
                || self
                    .started
                    .is_none_or(|started| now.duration_since(started) >= LIVE_INTERVAL))
    }
}

pub(super) struct Prepared<T> {
    pub analysis: SharedAnalysis,
    pub value: Arc<T>,
    pub frame: usize,
}

pub(super) struct Job<T> {
    pub cancel: Arc<AtomicBool>,
    recording: Weak<Recording>,
    analysis_revision: u64,
    receiver: Mutex<mpsc::Receiver<Result<Prepared<T>, Message>>>,
}

impl<T: Send + Sync + 'static> Job<T> {
    #[cfg(test)]
    pub fn start(
        recording: Arc<Recording>,
        frame: usize,
        context: egui::Context,
        build: fn(&Recording, &AtomicBool) -> Result<T, Message>,
        jobs: &crate::background_jobs::BackgroundJobs,
    ) -> Result<Arc<Self>, Message> {
        let revision = recording.analysis.revision();
        Self::start_with(
            recording,
            frame,
            context,
            revision,
            move |recording, cancel| build(recording, cancel).map(|value| (value, None)),
            jobs,
        )
    }

    pub fn start_with(
        recording: Arc<Recording>,
        frame: usize,
        context: egui::Context,
        analysis_revision: u64,
        build: impl FnOnce(&Recording, &AtomicBool) -> Result<(T, Option<Analysis>), Message>
        + Send
        + 'static,
        jobs: &crate::background_jobs::BackgroundJobs,
    ) -> Result<Arc<Self>, Message> {
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let source = Arc::downgrade(&recording);
        jobs.spawn("pvd-profile-query", cancel.clone(), move || {
            let result = build(&recording, &worker_cancel);
            if worker_cancel.load(Ordering::Relaxed) {
                drop(sender);
                context.request_repaint();
                return;
            }
            let result = result.map(|(value, analysis)| Prepared {
                analysis: analysis
                    .map(Into::into)
                    .unwrap_or_else(|| recording.analysis.clone()),
                value: Arc::new(value),
                frame,
            });
            if !worker_cancel.load(Ordering::Relaxed) {
                let _ = sender.send(result);
            }
            drop(sender);
            context.request_repaint();
        })
        .map_err(|error| Message::detail("Could not start analysis", "无法启动分析任务", error))?;
        Ok(Arc::new(Self {
            cancel,
            recording: source,
            analysis_revision,
            receiver: Mutex::new(receiver),
        }))
    }

    fn result(&self) -> Option<Result<Prepared<T>, Message>> {
        match self.receiver.lock() {
            Ok(receiver) => match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => Some(Err(Message::new(
                    "Analysis worker ended without a result",
                    "分析任务未返回结果",
                ))),
            },
            Err(_) => Some(Err(Message::new(
                "Analysis result lock failed",
                "分析结果锁失败",
            ))),
        }
    }
}

impl<T> Drop for Job<T> {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub(super) struct Cache<T> {
    pub prepared: Option<Arc<Prepared<T>>>,
    pub job: Option<Arc<Job<T>>>,
    pub error: Option<Message>,
    recording: Weak<Recording>,
    analysis_revision: Option<u64>,
    stable_revision: bool,
    gate: RefreshGate,
}

impl<T> Default for Cache<T> {
    fn default() -> Self {
        Self {
            prepared: None,
            job: None,
            error: None,
            recording: Weak::new(),
            analysis_revision: None,
            stable_revision: false,
            gate: RefreshGate::default(),
        }
    }
}

impl<T> Clone for Cache<T> {
    fn clone(&self) -> Self {
        Self {
            prepared: self.prepared.clone(),
            job: self.job.clone(),
            error: self.error.clone(),
            recording: self.recording.clone(),
            analysis_revision: self.analysis_revision,
            stable_revision: self.stable_revision,
            gate: self.gate.clone(),
        }
    }
}

pub(super) struct Request<'a> {
    pub recording: &'a Arc<Recording>,
    pub frame: usize,
    pub epoch: u64,
    pub following: bool,
    pub now: Instant,
    pub context: &'a egui::Context,
    pub jobs: &'a crate::background_jobs::BackgroundJobs,
}

impl<T: Send + Sync + 'static> Cache<T> {
    pub fn update(
        &mut self,
        request: Request<'_>,
        build: fn(&Recording, &AtomicBool) -> Result<T, Message>,
    ) -> bool {
        self.update_with(request, None, move |recording, cancel| {
            build(recording, cancel).map(|value| (value, None))
        })
    }

    pub fn update_with(
        &mut self,
        request: Request<'_>,
        revision: Option<u64>,
        build: impl FnOnce(&Recording, &AtomicBool) -> Result<(T, Option<Analysis>), Message>
        + Send
        + 'static,
    ) -> bool {
        let mut changed = false;
        if self.gate.change_epoch(request.epoch) || self.stable_revision != revision.is_some() {
            self.stable_revision = revision.is_some();
            self.cancel();
            self.prepared = None;
            self.recording = Weak::new();
            self.analysis_revision = None;
            self.error = None;
            changed = true;
        }
        if let Some(result) = self.job.as_ref().and_then(|job| job.result()) {
            let job = self.job.take().unwrap();
            if !job.cancel.load(Ordering::Relaxed) {
                self.recording = job.recording.clone();
                self.analysis_revision = Some(job.analysis_revision);
                match result {
                    Ok(prepared) => {
                        self.prepared = Some(Arc::new(prepared));
                        self.error = None;
                        changed = true;
                    }
                    Err(error) => self.error = Some(error),
                }
            }
        }
        let revision = revision.unwrap_or_else(|| request.recording.analysis.revision());
        let source_changed = self.analysis_revision != Some(revision);
        if self.error.is_none()
            && self.gate.due(
                request.following,
                self.job.is_some(),
                source_changed,
                request.now,
            )
        {
            self.gate.started = Some(request.now);
            match Job::start_with(
                request.recording.clone(),
                request.frame,
                request.context.clone(),
                revision,
                build,
                request.jobs,
            ) {
                Ok(job) => self.job = Some(job),
                Err(error) => self.error = Some(error),
            }
        } else if self.error.is_none() && self.job.is_none() && source_changed {
            let remaining = self.gate.started.map_or(Duration::ZERO, |started| {
                LIVE_INTERVAL.saturating_sub(request.now.duration_since(started))
            });
            request.context.request_repaint_after(remaining);
        }
        changed
    }

    pub fn cancel(&mut self) {
        if let Some(job) = &self.job {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn retry(&mut self) {
        self.cancel();
        self.error = None;
        self.recording = Weak::new();
        self.analysis_revision = None;
        self.gate.started = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pvd_core::analysis::{ProfileSpan, view::ProfileSource};

    #[test]
    fn prepared_profile_keeps_the_source_revision_and_borrowed_elements_alive() {
        let mut record = recording();
        let mut analysis = Analysis::default();
        analysis.add_zone(7, "physics".into()).unwrap();
        analysis.add_event(7, 4, "simulate".into()).unwrap();
        analysis.spans.push(ProfileSpan {
            zone: 7,
            event: 4,
            thread: 2,
            context: 9,
            start: 10,
            end: 30,
            received_frame: 1,
        });
        Arc::get_mut(&mut record).unwrap().analysis = analysis.into();
        let span = record.analysis.profile_span(0).unwrap() as *const ProfileSpan;
        let zone = record.analysis.profile_zone_name(7).unwrap().as_ptr();
        let revision = record.analysis.revision();
        let jobs = crate::background_jobs::BackgroundJobs::default();
        let job = Job::start(
            record.clone(),
            1,
            egui::Context::default(),
            build_count,
            &jobs,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let prepared = loop {
            if let Some(result) = job.result() {
                break result.unwrap();
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(prepared.analysis.revision(), revision);
        assert_eq!(
            prepared.analysis.profile_span(0).unwrap() as *const ProfileSpan,
            span
        );
        assert_eq!(
            prepared.analysis.profile_zone_name(7).unwrap().as_ptr(),
            zone
        );
        while jobs.active_count() != 0 {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        drop(record);
        assert!(job.recording.upgrade().is_none());
        assert_eq!(prepared.analysis.profile_span(0).unwrap().end, 30);
        assert_eq!(prepared.analysis.profile_event_name(7, 4), Some("simulate"));
    }

    fn recording() -> Arc<Recording> {
        Arc::new(
            pvd_core::RecordingBuilder::new(pvd_protocol::Header {
                version: 1,
                timestamp_numerator: 1,
                timestamp_denominator: 1,
                flags: 1,
                endian: pvd_protocol::Endian::Little,
            })
            .finish()
            .unwrap(),
        )
    }

    fn build_count(recording: &Recording, _: &AtomicBool) -> Result<usize, Message> {
        Ok(recording.analysis.profile_spans().len())
    }

    #[test]
    fn complete_catalog_source_switch_never_publishes_the_previous_worker() {
        let (source, first, started, release) = crate::full_profile::Source::gated_test_capture();
        let second = Arc::new((*first).clone());
        let context = egui::Context::default();
        let jobs = crate::background_jobs::BackgroundJobs::default();
        let mut cache = Cache::default();
        cache.update_with(
            Request {
                recording: &first,
                frame: 0,
                epoch: 1,
                following: false,
                now: Instant::now(),
                context: &context,
                jobs: &jobs,
            },
            Some(0),
            move |_, cancelled| {
                source
                    .catalog_page(None, 1, cancelled, |_| true)
                    .map(|(_, analysis)| (11, Some(analysis)))
            },
        );
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        let cancelled = cache.job.as_ref().unwrap().cancel.clone();
        cache.update_with(
            Request {
                recording: &second,
                frame: 0,
                epoch: 2,
                following: false,
                now: Instant::now(),
                context: &context,
                jobs: &jobs,
            },
            Some(0),
            |_, _| Ok((99, None)),
        );
        assert!(cancelled.load(Ordering::Relaxed));
        assert!(cache.prepared.is_none());
        assert_eq!(jobs.active_count(), 1);
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while cache.prepared.is_none() {
            assert!(Instant::now() < deadline);
            cache.update_with(
                Request {
                    recording: &second,
                    frame: 0,
                    epoch: 2,
                    following: false,
                    now: Instant::now(),
                    context: &context,
                    jobs: &jobs,
                },
                Some(0),
                |_, _| Ok((99, None)),
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(*cache.prepared.as_ref().unwrap().value, 99);
        while jobs.active_count() != 0 {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn complete_catalog_close_and_shutdown_wait_for_the_actual_reader() {
        for shutdown in [false, true] {
            let (source, record, started, release) =
                crate::full_profile::Source::gated_test_capture();
            let context = egui::Context::default();
            let mut studio = crate::studio::Studio::default();
            let jobs = studio.background_jobs.clone();
            let mut cache = Cache::default();
            cache.update_with(
                Request {
                    recording: &record,
                    frame: 0,
                    epoch: 1,
                    following: false,
                    now: Instant::now(),
                    context: &context,
                    jobs: &jobs,
                },
                Some(0),
                move |_, cancelled| {
                    source
                        .catalog_page(None, 1, cancelled, |_| true)
                        .map(|(entries, analysis)| (entries, Some(analysis)))
                },
            );
            started.recv_timeout(Duration::from_secs(5)).unwrap();
            let cancelled = cache.job.as_ref().unwrap().cancel.clone();
            studio.recording = Some(record);
            studio.profile_catalog_cancel = Some(cancelled.clone());
            if shutdown {
                assert!(!studio.poll_shutdown());
            } else {
                studio.close();
            }
            assert!(cancelled.load(Ordering::Relaxed));
            assert!(studio.profile_catalog_cancel.is_none());
            assert!(studio.recording.is_none());
            assert_eq!(jobs.active_work_count(), 1);
            release.send(()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while jobs.active_count() != 0 {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(cache.prepared.is_none());
            assert!(cache.job.as_ref().unwrap().result().unwrap().is_err());
            if shutdown {
                assert!(studio.poll_shutdown());
            }
        }
    }

    #[test]
    fn complete_catalog_cache_ignores_window_revisions_and_separates_snapshot_mode() {
        let mut record = recording();
        let context = egui::Context::default();
        let jobs = crate::background_jobs::BackgroundJobs::default();
        let now = Instant::now();
        let (mut cache, sender) = pending_cache(&record, now);
        cache.stable_revision = true;
        Arc::get_mut(cache.job.as_mut().unwrap())
            .unwrap()
            .analysis_revision = 0;
        sender
            .send(Ok(Prepared {
                analysis: Analysis::default().into(),
                value: Arc::new(17),
                frame: 0,
            }))
            .unwrap_or_else(|_| panic!("result channel closed"));
        cache.update_with(
            Request {
                recording: &record,
                frame: 0,
                epoch: 1,
                following: false,
                now,
                context: &context,
                jobs: &jobs,
            },
            Some(0),
            |_, _| Ok((0, None)),
        );
        let first = cache.prepared.clone().unwrap();
        let mut analysis = Analysis::default();
        analysis.first_retained_frame = 99;
        Arc::make_mut(&mut record).analysis = analysis.into();
        for frame in 1..10 {
            assert!(!cache.update_with(
                Request {
                    recording: &record,
                    frame,
                    epoch: 1,
                    following: false,
                    now,
                    context: &context,
                    jobs: &jobs
                },
                Some(0),
                |_, _| Ok((0, None))
            ));
            assert!(cache.job.is_none());
            assert!(Arc::ptr_eq(cache.prepared.as_ref().unwrap(), &first));
        }
        assert!(cache.update(
            Request {
                recording: &record,
                frame: 9,
                epoch: 1,
                following: false,
                now,
                context: &context,
                jobs: &jobs
            },
            build_count
        ));
        assert!(cache.prepared.is_none());
        assert!(cache.job.is_some());
        cache.cancel();
    }

    #[test]
    fn complete_catalog_identity_error_never_enters_prepared_cache() {
        let record = recording();
        let context = egui::Context::default();
        let jobs = crate::background_jobs::BackgroundJobs::default();
        let now = Instant::now();
        let (mut cache, sender) = pending_cache(&record, now);
        cache.stable_revision = true;
        Arc::get_mut(cache.job.as_mut().unwrap())
            .unwrap()
            .analysis_revision = 0;
        sender
            .send(Err(Message::new("source identity changed", "源身份改变")))
            .unwrap_or_else(|_| panic!("result channel closed"));
        cache.update_with(
            Request {
                recording: &record,
                frame: 0,
                epoch: 1,
                following: false,
                now,
                context: &context,
                jobs: &jobs,
            },
            Some(0),
            |_, _| Ok((0, None)),
        );
        assert!(cache.prepared.is_none());
        assert!(cache.error.is_some());
        assert!(cache.job.is_none());
    }

    fn pending_cache(
        recording: &Arc<Recording>,
        now: Instant,
    ) -> (Cache<usize>, mpsc::Sender<Result<Prepared<usize>, Message>>) {
        let (sender, receiver) = mpsc::channel();
        let job = Arc::new(Job {
            cancel: Arc::new(AtomicBool::new(false)),
            recording: Arc::downgrade(recording),
            analysis_revision: recording.analysis.revision(),
            receiver: Mutex::new(receiver),
        });
        let mut cache = Cache::default();
        cache.gate.change_epoch(1);
        cache.gate.started = Some(now);
        cache.job = Some(job);
        (cache, sender)
    }

    #[test]
    fn sixty_live_snapshots_start_at_most_five_analysis_jobs_per_second() {
        let mut gate = RefreshGate::default();
        gate.change_epoch(1);
        let start = Instant::now();
        let mut starts = Vec::new();
        for frame in 0..60 {
            let now = start + Duration::from_nanos(frame * 1_000_000_000 / 60);
            if gate.due(true, false, true, now) {
                gate.started = Some(now);
                starts.push(frame);
            }
        }
        assert_eq!(starts.len(), 5);
        assert!(starts.windows(2).all(|frames| frames[1] - frames[0] >= 12));
    }

    #[test]
    fn unfinished_job_survives_new_live_frames_and_context_changes_refresh_immediately() {
        let now = Instant::now();
        let mut gate = RefreshGate::default();
        gate.change_epoch(1);
        gate.started = Some(now);
        assert!(!gate.due(true, true, true, now + Duration::from_secs(10)));
        assert!(!gate.due(true, false, false, now + Duration::from_secs(10)));
        assert!(!gate.due(true, false, true, now + Duration::from_millis(1)));
        assert!(gate.change_epoch(2));
        assert!(gate.due(true, false, true, now + Duration::from_millis(1)));
        assert!(!gate.change_epoch(2));
    }

    #[test]
    fn incoming_live_recordings_do_not_cancel_pending_work_and_completion_remains_usable() {
        let first = recording();
        let context = egui::Context::default();
        let now = Instant::now();
        let (mut cache, sender) = pending_cache(&first, now);
        let job = cache.job.clone().unwrap();
        let mut latest = first.clone();
        for frame in 1..=60 {
            latest = Arc::new((*first).clone());
            cache.update(
                Request {
                    recording: &latest,
                    frame,
                    epoch: 1,
                    following: true,
                    now: now + Duration::from_millis(frame as u64 * 17),
                    context: &context,
                    jobs: &crate::background_jobs::BackgroundJobs::default(),
                },
                build_count,
            );
            assert!(Arc::ptr_eq(cache.job.as_ref().unwrap(), &job));
            assert!(!job.cancel.load(Ordering::Relaxed));
        }
        sender
            .send(Ok(Prepared {
                analysis: Analysis::default().into(),
                value: Arc::new(7),
                frame: 0,
            }))
            .unwrap_or_else(|_| panic!("pending result channel closed"));
        assert!(cache.update(
            Request {
                recording: &latest,
                frame: 60,
                epoch: 1,
                following: true,
                now: now + Duration::from_secs(2),
                context: &context,
                jobs: &crate::background_jobs::BackgroundJobs::default(),
            },
            build_count
        ));
        assert_eq!(*cache.prepared.as_ref().unwrap().value, 7);
        assert_eq!(cache.prepared.as_ref().unwrap().frame, 0);
        assert!(cache.job.is_none());
        let mut analysis = Analysis::default();
        analysis.first_retained_frame = 1;
        Arc::make_mut(&mut latest).analysis = analysis.into();
        cache.update(
            Request {
                recording: &latest,
                frame: 61,
                epoch: 1,
                following: true,
                now: now + Duration::from_secs(3),
                context: &context,
                jobs: &crate::background_jobs::BackgroundJobs::default(),
            },
            build_count,
        );
        assert!(cache.job.is_some());
    }

    #[test]
    fn fixed_recording_is_not_rebuilt_and_new_context_discards_old_completion() {
        let record = recording();
        let context = egui::Context::default();
        let now = Instant::now();
        let (mut cache, sender) = pending_cache(&record, now);
        sender
            .send(Ok(Prepared {
                analysis: Analysis::default().into(),
                value: Arc::new(7),
                frame: 0,
            }))
            .unwrap_or_else(|_| panic!("pending result channel closed"));
        cache.update(
            Request {
                recording: &record,
                frame: 0,
                epoch: 1,
                following: false,
                now,
                context: &context,
                jobs: &crate::background_jobs::BackgroundJobs::default(),
            },
            build_count,
        );
        for frame in 1..=60 {
            assert!(!cache.update(
                Request {
                    recording: &record,
                    frame,
                    epoch: 1,
                    following: false,
                    now: now + Duration::from_secs(frame as u64),
                    context: &context,
                    jobs: &crate::background_jobs::BackgroundJobs::default(),
                },
                build_count
            ));
            assert!(cache.job.is_none());
        }
        let (mut cache, sender) = pending_cache(&record, now);
        let old_job = cache.job.clone().unwrap();
        sender
            .send(Ok(Prepared {
                analysis: Analysis::default().into(),
                value: Arc::new(7),
                frame: 0,
            }))
            .unwrap_or_else(|_| panic!("pending result channel closed"));
        cache.update(
            Request {
                recording: &record,
                frame: 10,
                epoch: 2,
                following: false,
                now,
                context: &context,
                jobs: &crate::background_jobs::BackgroundJobs::default(),
            },
            build_count,
        );
        assert!(old_job.cancel.load(Ordering::Relaxed));
        assert!(cache.prepared.is_none());
        assert!(cache.job.is_some());
        assert!(!Arc::ptr_eq(cache.job.as_ref().unwrap(), &old_job));
    }
}
