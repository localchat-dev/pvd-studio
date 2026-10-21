use super::*;
use bevy::prelude::*;
use bevy_egui::{EguiContext, EguiUserTextures, PrimaryEguiContext};
use pvd_core::analysis::{Analysis, SharedAnalysis};
use std::{sync::Weak, time::Duration};

pub(super) fn preparation_app(studio: Studio, context: &egui::Context) -> App {
    let mut app = App::new();
    app.insert_resource(studio)
        .init_resource::<EguiUserTextures>()
        .add_systems(Update, crate::ui::prepare);
    let mut entity = app.world_mut().spawn(PrimaryEguiContext);
    *entity.get_mut::<EguiContext>().unwrap().get_mut() = context.clone();
    app
}

fn fixture() -> (Arc<Recording>, Weak<Analysis>) {
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
        received_frame: 0,
    });
    let mut recording = pvd_core::RecordingBuilder::new(pvd_protocol::Header {
        version: 1,
        timestamp_numerator: 1,
        timestamp_denominator: 1,
        flags: 1,
        endian: pvd_protocol::Endian::Little,
    })
    .finish()
    .unwrap();
    let analysis = Arc::new(analysis);
    let weak = Arc::downgrade(&analysis);
    recording.analysis = SharedAnalysis::from(analysis);
    (Arc::new(recording), weak)
}

fn wait_finished(jobs: &crate::background_jobs::BackgroundJobs) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while jobs.active_count() != 0 {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
}

#[test]
fn hidden_completed_profile_roots_release_on_close_and_source_change_during_prepare() {
    for close in [false, true] {
        let context = egui::Context::default();
        let (recording, weak) = fixture();
        let mut studio = Studio::default();
        studio.persist_layout = false;
        studio.recording = Some(recording.clone());
        let mut app = preparation_app(studio, &context);
        app.update();
        let mut threads = ViewState::default();
        threads.cache.prepared = Some(Arc::new(refresh::Prepared {
            analysis: recording.analysis.clone(),
            value: Arc::new(build_layout(&recording, &AtomicBool::new(false)).unwrap()),
            frame: 0,
        }));
        let mut events = EventView::default();
        events.cache.prepared = Some(Arc::new(refresh::Prepared {
            analysis: recording.analysis.clone(),
            value: Arc::new(build_catalog(&recording, &AtomicBool::new(false)).unwrap()),
            frame: 0,
        }));
        context.data_mut(|data| {
            data.insert_temp(egui::Id::new("hidden-threads"), threads);
            data.insert_temp(egui::Id::new("hidden-events"), events);
        });
        drop(recording);
        app.update();
        assert!(weak.upgrade().is_some());
        {
            let mut studio = app.world_mut().resource_mut::<Studio>();
            if close {
                studio.close();
            } else {
                studio.recording = Some(fixture().0);
                studio.profile_view_epoch += 1;
            }
        }
        assert!(weak.upgrade().is_some());
        app.update();
        assert!(weak.upgrade().is_none());
        assert!(
            context
                .data(|data| data.get_temp::<ViewState>(egui::Id::new("hidden-threads")))
                .is_none()
        );
        assert!(
            context
                .data(|data| data.get_temp::<EventView>(egui::Id::new("hidden-events")))
                .is_none()
        );
    }
}

#[test]
fn hidden_queued_profile_result_is_dropped_without_drawing_the_panel() {
    let context = egui::Context::default();
    let (recording, weak) = fixture();
    let mut studio = Studio::default();
    studio.persist_layout = false;
    studio.recording = Some(recording.clone());
    let jobs = studio.background_jobs.clone();
    let mut app = preparation_app(studio, &context);
    app.update();
    let job =
        refresh::Job::start(recording.clone(), 0, context.clone(), build_layout, &jobs).unwrap();
    let cancelled = job.cancel.clone();
    wait_finished(&jobs);
    let mut view = ViewState::default();
    view.cache.job = Some(job);
    context.data_mut(|data| data.insert_temp(egui::Id::new("hidden-queued"), view));
    drop(recording);
    app.world_mut().resource_mut::<Studio>().close();
    assert!(weak.upgrade().is_some());
    app.update();
    assert!(cancelled.load(Ordering::Relaxed));
    assert!(weak.upgrade().is_none());
}

#[test]
fn hidden_active_worker_is_cancelled_and_releases_after_registered_completion() {
    let context = egui::Context::default();
    let (recording, weak) = fixture();
    let mut studio = Studio::default();
    studio.persist_layout = false;
    studio.recording = Some(recording.clone());
    let jobs = studio.background_jobs.clone();
    let mut app = preparation_app(studio, &context);
    app.update();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let job = refresh::Job::start_with(
        recording.clone(),
        0,
        context.clone(),
        recording.analysis.revision(),
        move |recording, cancel| {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            build_layout(recording, cancel).map(|layout| (layout, None))
        },
        &jobs,
    )
    .unwrap();
    let cancelled = job.cancel.clone();
    let mut view = ViewState::default();
    view.cache.job = Some(job);
    context.data_mut(|data| data.insert_temp(egui::Id::new("hidden-active"), view));
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(recording);
    app.world_mut().resource_mut::<Studio>().close();
    app.update();
    assert!(cancelled.load(Ordering::Relaxed));
    assert_eq!(jobs.active_count(), 1);
    assert!(weak.upgrade().is_some());
    release_tx.send(()).unwrap();
    wait_finished(&jobs);
    assert!(weak.upgrade().is_none());
}
