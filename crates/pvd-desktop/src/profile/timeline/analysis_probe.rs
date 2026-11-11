use super::*;
use pvd_core::analysis::ProfileSpan;
use std::time::Duration;

pub(crate) struct Window {
    pub catalog: ThreadPageReport,
    pub spans: Vec<ProfileSpan>,
    pub request: WindowRequest,
    pub stats: GlobalPairingStats,
    pub index_bytes: usize,
}

pub(crate) fn thread_window(
    studio: &mut Studio,
    context: &egui::Context,
    streaming: bool,
) -> Window {
    studio.profile_events_view = false;
    let id = super::super::analysis_probe::draw(studio, context).with("complete-thread-profile");
    let deadline = Instant::now() + Duration::from_secs(120);
    let catalog = loop {
        studio.poll(context);
        super::super::analysis_probe::draw(studio, context);
        let state = context
            .data(|data| data.get_temp::<State>(id))
            .expect("complete-file thread controls must remain available");
        assert!(state.catalog.error.is_none(), "{:?}", state.catalog.error);
        if let Some(prepared) = state.catalog.prepared {
            assert!(state.catalog.job.is_none());
            assert!(prepared.value.next.is_none());
            break prepared.value.as_ref().clone();
        }
        assert!(Instant::now() < deadline, "thread catalog worker timed out");
        std::thread::park_timeout(Duration::from_millis(1));
    };
    let request = WindowRequest {
        ticks: catalog.completed_tick_range.unwrap(),
        thread: None,
        context: None,
        limits: WindowLimits::default(),
    };
    context.data_mut(|data| {
        let mut state = data.get_temp::<State>(id).unwrap();
        state.set_read_mode(if streaming {
            ReadMode::Streaming
        } else {
            ReadMode::Indexed
        });
        state.request_window(request);
        data.insert_temp(id, state);
    });
    loop {
        studio.poll(context);
        super::super::analysis_probe::draw(studio, context);
        let state = context.data(|data| data.get_temp::<State>(id)).unwrap();
        assert!(state.window.error.is_none(), "{:?}", state.window.error);
        if let Some(prepared) = state.window.prepared {
            assert!(state.window.job.is_none());
            let spans = prepared
                .analysis
                .profile_spans()
                .cloned()
                .collect::<Vec<_>>();
            assert_eq!(prepared.value.spans, spans.len());
            assert!(state.displayed.is_some());
            return Window {
                catalog,
                spans,
                request: prepared.value.request,
                stats: prepared.value.stats,
                index_bytes: prepared.value.index_bytes,
            };
        }
        assert!(Instant::now() < deadline, "thread window worker timed out");
        std::thread::park_timeout(Duration::from_millis(1));
    }
}
