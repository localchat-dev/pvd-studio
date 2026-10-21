use crate::i18n::Language;
use bevy_egui::egui;
use pvd_core::profile_catalog_index::FirstStartCursor;
use pvd_core::profile_curves::{CatalogCursor, CatalogEntry};
use std::collections::VecDeque;

#[derive(Clone)]
enum RenderPhase {
    Requested,
    Loading,
    Loaded { displayed: bool },
    Failed(crate::i18n::Message),
}

#[derive(Clone)]
struct RenderRequest {
    epoch: u64,
    phase: RenderPhase,
}

fn render_id() -> egui::Id {
    egui::Id::new("first-start-catalog-render")
}

pub(super) fn prepare_render(
    context: &egui::Context,
    epoch: u64,
) -> Result<bool, crate::i18n::Message> {
    let mut request = context
        .data_mut(|data| data.get_temp::<RenderRequest>(render_id()))
        .filter(|request| request.epoch == epoch)
        .unwrap_or(RenderRequest {
            epoch,
            phase: RenderPhase::Requested,
        });
    let result = match &mut request.phase {
        RenderPhase::Loaded { displayed } => {
            let ready = *displayed;
            *displayed = true;
            Ok(ready)
        }
        RenderPhase::Failed(error) => Err(error.clone()),
        _ => Ok(false),
    };
    context.data_mut(|data| data.insert_temp(render_id(), request));
    context.request_repaint();
    result
}

pub(super) fn start_render(context: &egui::Context, epoch: u64, pages: &mut Pages) -> bool {
    let Some(mut request) = context
        .data_mut(|data| data.get_temp::<RenderRequest>(render_id()))
        .filter(|request| request.epoch == epoch)
    else {
        return false;
    };
    if !matches!(request.phase, RenderPhase::Requested) {
        return false;
    }
    pages.set_order(Order::FirstStart);
    pages.first();
    request.phase = RenderPhase::Loading;
    context.data_mut(|data| data.insert_temp(render_id(), request));
    true
}

pub(super) fn poll_render(context: &egui::Context, epoch: u64, view: &super::EventView) {
    let Some(mut request) = context
        .data_mut(|data| data.get_temp::<RenderRequest>(render_id()))
        .filter(|request| request.epoch == epoch)
    else {
        return;
    };
    if !matches!(request.phase, RenderPhase::Loading) {
        return;
    }
    if let Some(error) = &view.cache.error {
        request.phase = RenderPhase::Failed(error.clone());
    } else if let Some(prepared) = &view.cache.prepared {
        eprintln!(
            "first_start_catalog_render {}",
            serde_json::json!({
                "entries": prepared.value.entries.len(),
                "first_start": prepared.value.entries.first().and_then(|entry| entry.first_start),
                "last_start": prepared.value.entries.last().and_then(|entry| entry.first_start),
                "has_next": prepared.value.next.is_some(),
            })
        );
        request.phase = RenderPhase::Loaded { displayed: false };
    }
    context.data_mut(|data| data.insert_temp(render_id(), request));
}

pub(super) fn load(
    source: &crate::full_profile::Source,
    order: Order,
    cursor: Option<&Cursor>,
    cancel: &std::sync::atomic::AtomicBool,
    mut progress: impl FnMut(crate::full_profile::TimelineProgress) -> bool,
) -> Result<(CatalogPage, pvd_core::analysis::Analysis), crate::i18n::Message> {
    use crate::full_profile::TimelineProgress;
    match (order, cursor) {
        (Order::Key, None | Some(Cursor::Key(_))) => {
            let cursor = match cursor {
                Some(Cursor::Key(cursor)) => Some(cursor),
                _ => None,
            };
            source
                .catalog_page(cursor, PAGE_SIZE, cancel, |p| {
                    progress(TimelineProgress::Streaming(p))
                })
                .map(|(page, analysis)| {
                    (
                        CatalogPage {
                            entries: page.entries,
                            next: page.next.map(Cursor::Key),
                        },
                        analysis,
                    )
                })
        }
        (Order::FirstStart, None | Some(Cursor::FirstStart(_))) => {
            let cursor = match cursor {
                Some(Cursor::FirstStart(cursor)) => Some(cursor),
                _ => None,
            };
            source
                .catalog_page_first_start(cursor, PAGE_SIZE, cancel, progress)
                .map(|(page, analysis)| {
                    (
                        CatalogPage {
                            entries: page.entries,
                            next: page.next.map(Cursor::FirstStart),
                        },
                        analysis,
                    )
                })
        }
        _ => Err(crate::i18n::Message::new(
            "The catalog cursor belongs to a different order; return to the first page.",
            "目录游标属于另一排序，请返回首页。",
        )),
    }
}

pub(super) const PAGE_SIZE: usize = 1024;
const MAX_BACK_PAGES: usize = 256;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Order {
    #[default]
    Key,
    FirstStart,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Cursor {
    Key(CatalogCursor),
    FirstStart(FirstStartCursor),
}

#[derive(Default)]
pub(super) struct CatalogPage {
    pub entries: Vec<CatalogEntry>,
    pub next: Option<Cursor>,
}

#[derive(Clone, Default)]
pub(super) struct Pages {
    scope: Option<(u64, bool)>,
    pub cursor: Option<Cursor>,
    pub order: Order,
    previous: VecDeque<Option<Cursor>>,
    number: usize,
}

impl Pages {
    pub fn sync(&mut self, epoch: u64, complete: bool) -> bool {
        if self.scope == Some((epoch, complete)) {
            return false;
        }
        *self = Self {
            scope: Some((epoch, complete)),
            number: 1,
            ..Default::default()
        };
        true
    }

    fn first(&mut self) {
        self.cursor = None;
        self.previous.clear();
        self.number = 1;
    }

    fn back(&mut self) {
        if let Some(previous) = self.previous.pop_back() {
            self.cursor = previous;
            self.number -= 1;
        }
    }

    fn next(&mut self, cursor: Cursor) {
        if self.previous.len() == MAX_BACK_PAGES {
            self.previous.pop_front();
        }
        self.previous.push_back(self.cursor.replace(cursor));
        self.number = self.number.saturating_add(1);
    }

    pub fn show(&mut self, ui: &mut egui::Ui, next: Option<&Cursor>, language: Language) -> bool {
        self.controls(ui, next, language, false)
    }

    pub fn show_compact(
        &mut self,
        ui: &mut egui::Ui,
        next: Option<&Cursor>,
        language: Language,
    ) -> bool {
        self.controls(ui, next, language, true)
    }

    fn controls(
        &mut self,
        ui: &mut egui::Ui,
        next: Option<&Cursor>,
        language: Language,
        compact: bool,
    ) -> bool {
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    self.cursor.is_some(),
                    egui::Button::new(if compact {
                        "|<"
                    } else {
                        language.text("First page", "首页")
                    }),
                )
                .on_hover_text(language.text("First page", "首页"))
                .clicked()
            {
                self.first();
                changed = true;
            }
            if ui
                .add_enabled(
                    !self.previous.is_empty(),
                    egui::Button::new(if compact {
                        "<"
                    } else {
                        language.text("Previous page", "上一页")
                    }),
                )
                .on_hover_text(language.text("Previous page", "上一页"))
                .clicked()
            {
                self.back();
                changed = true;
            }
            let page_label = format!(
                "{} {}{}",
                language.text("Page", "第"),
                self.number,
                language.text("", " 页")
            );
            ui.label(if compact {
                self.number.to_string()
            } else {
                page_label.clone()
            })
            .on_hover_text(page_label);
            if ui
                .add_enabled(
                    next.is_some(),
                    egui::Button::new(if compact {
                        ">"
                    } else {
                        language.text("Next page", "下一页")
                    }),
                )
                .on_hover_text(language.text("Next page", "下一页"))
                .clicked()
                && let Some(cursor) = next
            {
                self.next(cursor.clone());
                changed = true;
            }
        });
        changed
    }

    pub fn set_order(&mut self, order: Order) -> bool {
        if self.order == order {
            return false;
        }
        self.order = order;
        self.first();
        true
    }

    pub fn order_menu(&mut self, ui: &mut egui::Ui, language: Language, indexed: bool) -> bool {
        let mut order = self.order;
        crate::ui::settings_menu(ui, language.text("Order", "排序"), |ui| {
            ui.radio_value(
                &mut order,
                Order::Key,
                language.text("Identity order", "标识顺序"),
            );
            ui.add_enabled_ui(indexed, |ui| {
                ui.radio_value(
                    &mut order,
                    Order::FirstStart,
                    language.text("First start time", "首次开始时间"),
                );
            });
            ui.set_max_width(360.0);
            ui.label(language.text(
                "First-start order uses the earliest recorded start; events with only unmatched stops follow. Equal starts use the full identity. Filtering applies to the current page.",
                "首次开始时间取各事件最早的录制起点；仅有未匹配结束的事件排在最后，同时间按完整标识排序。筛选仅作用于当前页。",
            ));
        });
        self.set_order(order)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pvd_core::profile_curves::{ExactKey, SourceIdentity};

    fn cursor(after: u64) -> Cursor {
        Cursor::Key(CatalogCursor {
            identity: SourceIdentity {
                blake3: [1; 32],
                bytes: 10,
                frames: 1,
            },
            after: ExactKey {
                zone: 1,
                event: 1,
                thread: 1,
                context: after,
            },
        })
    }

    #[test]
    fn page_history_is_bounded_and_source_epoch_or_mode_discards_every_cursor() {
        let mut pages = Pages::default();
        assert!(pages.sync(7, true));
        for index in 1..=MAX_BACK_PAGES + 10 {
            pages.next(cursor(index as u64));
        }
        assert_eq!(pages.previous.len(), MAX_BACK_PAGES);
        assert_eq!(pages.number, MAX_BACK_PAGES + 11);
        pages.back();
        assert_eq!(pages.cursor, Some(cursor((MAX_BACK_PAGES + 9) as u64)));
        assert!(!pages.sync(7, true));
        assert!(pages.sync(8, true));
        assert!(pages.cursor.is_none() && pages.previous.is_empty());
        assert_eq!(pages.number, 1);
        pages.next(cursor(4));
        assert!(pages.sync(8, false));
        assert!(pages.cursor.is_none());
        pages.next(cursor(8));
        pages.first();
        assert!(pages.cursor.is_none() && pages.previous.is_empty());
    }

    #[test]
    fn changing_order_discards_the_complete_cursor_history_and_rejects_mixed_tokens() {
        let (source, _, _, _) = crate::full_profile::Source::gated_test_capture();
        let mut pages = Pages::default();
        pages.sync(1, true);
        pages.next(cursor(1));
        pages.next(cursor(2));
        assert!(pages.set_order(Order::FirstStart));
        assert!(pages.cursor.is_none() && pages.previous.is_empty());
        assert_eq!(pages.number, 1);
        let key_cursor = cursor(1);
        let Cursor::Key(key) = key_cursor.clone() else {
            unreachable!()
        };
        let first_cursor = Cursor::FirstStart(FirstStartCursor {
            identity: key.identity,
            sort_version: pvd_core::profile_catalog_index::SORT_VERSION,
            after: pvd_core::profile_catalog_index::FirstStartPosition {
                first_start: Some(0),
                key: key.after,
            },
        });
        let cancel = std::sync::atomic::AtomicBool::new(false);
        assert!(
            load(
                &source,
                Order::FirstStart,
                Some(&key_cursor),
                &cancel,
                |_| true
            )
            .is_err()
        );
        assert!(load(&source, Order::Key, Some(&first_cursor), &cancel, |_| true).is_err());
        pages.next(first_cursor);
        assert!(pages.set_order(Order::Key));
        assert!(pages.cursor.is_none() && pages.previous.is_empty());
        assert!(!pages.set_order(Order::Key));
        pages.set_order(Order::FirstStart);
        assert!(pages.sync(2, true));
        assert!(pages.order == Order::Key && pages.cursor.is_none());
    }

    #[test]
    fn actual_pointer_clicks_navigate_next_back_first_and_disable_absent_next() {
        let context = egui::Context::default();
        let mut pages = Pages::default();
        pages.sync(1, true);
        let mut run = |events: Vec<egui::Event>, next: Option<Cursor>| {
            let mut changed = false;
            let output = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 200.0),
                    )),
                    events,
                    ..Default::default()
                },
                |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        changed = pages.show(ui, next.as_ref(), Language::English);
                    });
                },
            );
            (output, changed, pages.cursor.clone())
        };
        for (label, expected, next) in [
            ("Next page", Some(cursor(1)), Some(cursor(1))),
            ("Previous page", None, Some(cursor(2))),
            ("Next page", Some(cursor(3)), Some(cursor(3))),
            ("First page", None, Some(cursor(4))),
            ("Next page", None, None),
        ] {
            let (output, _, _) = run(vec![], next.clone());
            let button = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                        Some(text.pos + text.galley.size() * 0.5)
                    }
                    _ => None,
                })
                .unwrap();
            for pressed in [true, false] {
                let (_, changed, actual) = run(
                    vec![
                        egui::Event::PointerMoved(button),
                        egui::Event::PointerButton {
                            pos: button,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    next.clone(),
                );
                if !pressed {
                    assert_eq!(actual, expected);
                    assert_eq!(changed, next.is_some());
                }
            }
        }
    }
}
