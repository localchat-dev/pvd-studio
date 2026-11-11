use super::*;
use pvd_core::profile_timeline::EventNames;
use std::time::Duration;

pub(crate) use super::timeline::analysis_probe::thread_window;

pub(crate) struct Catalog {
    pub entries: Vec<CatalogEntry>,
    pub names: BTreeMap<(u64, u16), EventNames>,
}

pub(crate) fn draw(studio: &mut Studio, context: &egui::Context) -> egui::Id {
    let panel = std::cell::Cell::new(None);
    let _ = context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(960.0, 720.0),
            )),
            ..Default::default()
        },
        |context| {
            egui::CentralPanel::default().show(context, |ui| {
                panel.set(Some(ui.id()));
                show(ui, studio);
            });
        },
    );
    panel.get().unwrap()
}

pub(crate) fn catalog(studio: &mut Studio, context: &egui::Context, first_start: bool) -> Catalog {
    studio.profile_events_view = true;
    let id = draw(studio, context).with("profile-event-view");
    let mut view = context
        .data(|data| data.get_temp::<EventView>(id))
        .expect("complete-file event controls must remain available");
    let order = if first_start {
        pages::Order::FirstStart
    } else {
        pages::Order::Key
    };
    if view.pages.set_order(order) {
        view.restart_page();
        context.data_mut(|data| data.insert_temp(id, view));
    }
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        studio.poll(context);
        assert_eq!(draw(studio, context).with("profile-event-view"), id);
        let view = context.data(|data| data.get_temp::<EventView>(id)).unwrap();
        assert!(view.cache.error.is_none(), "{:?}", view.cache.error);
        if let Some(prepared) = view.cache.prepared {
            assert!(view.cache.job.is_none());
            assert!(prepared.value.next.is_none());
            let entries = prepared.value.entries.clone();
            let names = entries
                .iter()
                .map(|entry| {
                    let (zone, event) = (entry.key.zone, entry.key.event);
                    (
                        (zone, event),
                        EventNames {
                            zone: prepared.analysis.profile_zone_name(zone).map(str::to_owned),
                            event: prepared
                                .analysis
                                .profile_event_name(zone, event)
                                .map(str::to_owned),
                        },
                    )
                })
                .collect();
            return Catalog { entries, names };
        }
        assert!(Instant::now() < deadline, "catalog worker timed out");
        std::thread::park_timeout(Duration::from_millis(1));
    }
}
