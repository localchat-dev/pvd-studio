use super::*;
use pvd_core::Object;
use pvd_protocol::{Endian, Event, EventGroup, Header};
use std::sync::Arc;

const QUERY: &str = "0X000000000000002A";

struct SearchUi {
    context: egui::Context,
    studio: Studio,
    viewport: Viewport,
    images: Assets<Image>,
    other_text: String,
    search_position: egui::Pos2,
    other_position: egui::Pos2,
    transport: (usize, i32, bool),
}

impl SearchUi {
    fn new(language: Language, playing: i32, follow_live: bool) -> Self {
        let context = egui::Context::default();
        configure(&context);
        let mut builder = pvd_core::RecordingBuilder::new(Header {
            version: 1,
            timestamp_numerator: 1,
            timestamp_denominator: 1,
            flags: 1,
            endian: Endian::Little,
        });
        builder
            .push(EventGroup {
                stream_id: 1,
                timestamp: 0,
                offset: 0,
                events: std::iter::once(Event::StringHandle {
                    handle: 1,
                    value: "frame".into(),
                })
                .chain((0..40).map(|timestamp| Event::EndSection {
                    id: 1,
                    name: 1,
                    timestamp,
                }))
                .collect(),
            })
            .unwrap();
        let mut studio = Studio::default();
        studio.prefs.language = language;
        studio.recording = Some(Arc::new(builder.finish().unwrap()));
        for (id, name) in [(1, "Initial selection"), (42, "Actor 12345"), (99, "Box")] {
            studio.state.objects.insert(
                id,
                Object {
                    id,
                    generation: 0,
                    type_name: "fixture::Actor".into(),
                    properties: [("Name".into(), Value::String(name.into()))].into(),
                },
            );
        }
        studio.selected = Some(1);
        studio.frame = 37;
        studio.playing = playing;
        studio.follow_live = follow_live;
        studio.live_session = follow_live.then_some(9);
        studio.live_connected = follow_live;
        let mut harness = Self {
            context,
            studio,
            viewport: Viewport::default(),
            images: Assets::default(),
            other_text: String::new(),
            search_position: egui::Pos2::ZERO,
            other_position: egui::Pos2::ZERO,
            transport: (37, playing, follow_live),
        };
        let _ = harness.frame(vec![], egui::Modifiers::NONE, true);
        let output = harness.frame(vec![], egui::Modifiers::NONE, true);
        harness.search_position = text_position(
            &output,
            language.text("Search name, type or ID", "搜索名称、类型或 ID"),
        );
        harness.other_position = text_position(&output, "Other field");
        harness
    }

    fn frame(
        &mut self,
        events: Vec<egui::Event>,
        modifiers: egui::Modifiers,
        focused: bool,
    ) -> egui::FullOutput {
        let context = self.context.clone();
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(700.0, 620.0),
                )),
                events,
                modifiers,
                focused,
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.other_text)
                            .id(egui::Id::new("object-search-other-field"))
                            .hint_text("Other field"),
                    );
                    Viewer {
                        studio: &mut self.studio,
                        viewport: &mut self.viewport,
                        images: &mut self.images,
                    }
                    .objects(ui);
                });
            },
        );
        assert_eq!(
            (
                self.studio.frame,
                self.studio.playing,
                self.studio.follow_live
            ),
            self.transport
        );
        assert!(!self.studio.focus_selected);
        output
    }

    fn click(&mut self, position: egui::Pos2) {
        let _ = self.frame(
            vec![egui::Event::PointerMoved(position)],
            egui::Modifiers::NONE,
            true,
        );
        for pressed in [true, false] {
            let _ = self.frame(
                vec![egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }],
                egui::Modifiers::NONE,
                true,
            );
        }
    }

    fn focus_search(&mut self) -> egui::Id {
        self.click(self.search_position);
        assert!(self.context.wants_keyboard_input());
        let id = self.context.memory(|memory| memory.focused()).unwrap();
        assert_ne!(id, egui::Id::new("object-search-other-field"));
        id
    }

    fn type_query(&mut self, text: &str) -> egui::Id {
        let id = self.focus_search();
        let _ = self.frame(
            vec![egui::Event::Text(text.into())],
            egui::Modifiers::NONE,
            true,
        );
        assert_eq!(self.studio.filter, text);
        assert_eq!(self.context.memory(|memory| memory.focused()), Some(id));
        assert_eq!(self.studio.selected, Some(1));
        id
    }

    fn enter(&mut self, modifiers: egui::Modifiers) {
        for pressed in [true, false] {
            let _ = self.frame(vec![enter(pressed, modifiers)], modifiers, true);
        }
    }
}

fn enter(pressed: bool, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers,
    }
}

fn text_position(output: &egui::FullOutput, expected: &str) -> egui::Pos2 {
    output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == expected => {
                Some(text.galley.rect.translate(text.pos.to_vec2()).center())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing text: {expected}"))
}

#[test]
fn actual_object_search_text_edit_submits_hex_ids_without_changing_transport() {
    for language in [Language::Chinese, Language::English] {
        for (playing, follow_live) in [(-1, false), (1, false), (0, true)] {
            let mut ui = SearchUi::new(language, playing, follow_live);
            let search = ui.type_query(QUERY);
            assert!(ui.studio.tree.rows.iter().any(|row| row.id == Some(42)));
            ui.enter(egui::Modifiers::NONE);
            assert_eq!(ui.studio.selected, Some(42));
            assert_eq!(ui.studio.filter, QUERY);
            assert_ne!(ui.context.memory(|memory| memory.focused()), Some(search));
        }
    }
}

#[test]
fn actual_tree_shift_click_toggles_selection_and_plain_click_replaces_it() {
    let mut ui = SearchUi::new(Language::English, 0, true);
    for (id, toggle, expected, primary) in [
        (42, true, vec![1, 42], 42),
        (99, true, vec![1, 42, 99], 99),
        (42, true, vec![1, 99], 99),
        (99, true, vec![1], 1),
        (42, true, vec![1, 42], 42),
        (99, false, vec![99], 99),
    ] {
        let modifiers = if toggle {
            egui::Modifiers::SHIFT
        } else {
            egui::Modifiers::NONE
        };
        let output = ui.frame(vec![], modifiers, true);
        let label = ui
            .studio
            .tree
            .rows
            .iter()
            .find(|row| row.id == Some(id))
            .unwrap()
            .label
            .clone();
        let position = text_position(&output, &label);
        ui.frame(vec![egui::Event::PointerMoved(position)], modifiers, true);
        for pressed in [true, false] {
            ui.frame(
                vec![egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers,
                }],
                modifiers,
                true,
            );
        }
        assert_eq!(
            ui.studio.selected_ids().into_iter().collect::<Vec<_>>(),
            expected
        );
        assert_eq!(ui.studio.selected, Some(primary));
    }
}

#[test]
fn actual_object_search_does_not_submit_a_name_or_digits_only_present_in_a_name() {
    for query in ["12345", "Actor 12345"] {
        let mut ui = SearchUi::new(Language::Chinese, -1, false);
        ui.type_query(query);
        assert_eq!(ui.studio.tree.rows.len(), 1);
        assert_eq!(ui.studio.tree.rows[0].id, Some(42));
        ui.enter(egui::Modifiers::NONE);
        assert_eq!(ui.studio.selected, Some(1));
        assert_eq!(ui.studio.filter, query);
    }
}

#[test]
fn actual_object_search_does_not_submit_modified_enter() {
    let mut ui = SearchUi::new(Language::Chinese, 0, true);
    ui.type_query(QUERY);
    for modifiers in [
        egui::Modifiers {
            ctrl: true,
            ..Default::default()
        },
        egui::Modifiers {
            alt: true,
            ..Default::default()
        },
        egui::Modifiers {
            shift: true,
            ..Default::default()
        },
        egui::Modifiers {
            command: true,
            ..Default::default()
        },
    ] {
        ui.focus_search();
        ui.enter(modifiers);
        assert_eq!(ui.studio.selected, Some(1));
        assert_eq!(ui.studio.filter, QUERY);
    }
}

#[test]
fn actual_object_search_keeps_ime_commit_and_focus_loss_separate_from_submission() {
    let mut ui = SearchUi::new(Language::Chinese, 1, false);
    ui.focus_search();
    let _ = ui.frame(
        vec![
            egui::Event::Ime(egui::ImeEvent::Enabled),
            egui::Event::Ime(egui::ImeEvent::Preedit(QUERY.into())),
        ],
        egui::Modifiers::NONE,
        true,
    );
    let _ = ui.frame(
        vec![
            egui::Event::Ime(egui::ImeEvent::Commit(QUERY.into())),
            enter(true, egui::Modifiers::NONE),
        ],
        egui::Modifiers::NONE,
        true,
    );
    assert_eq!(ui.studio.filter, QUERY);
    assert_eq!(ui.studio.selected, Some(1));
    let _ = ui.frame(
        vec![enter(false, egui::Modifiers::NONE)],
        egui::Modifiers::NONE,
        true,
    );
    ui.focus_search();
    ui.enter(egui::Modifiers::NONE);
    assert_eq!(ui.studio.selected, Some(42));

    let mut ui = SearchUi::new(Language::Chinese, -1, false);
    let search = ui.type_query(QUERY);
    ui.click(ui.other_position);
    assert_ne!(ui.context.memory(|memory| memory.focused()), Some(search));
    let _ = ui.frame(
        vec![egui::Event::Text("outside".into())],
        egui::Modifiers::NONE,
        true,
    );
    assert_eq!(ui.other_text, "outside");
    ui.enter(egui::Modifiers::NONE);
    assert_eq!(ui.studio.filter, QUERY);
    assert_eq!(ui.studio.selected, Some(1));

    let mut ui = SearchUi::new(Language::Chinese, 0, true);
    ui.type_query(QUERY);
    let _ = ui.frame(
        vec![
            egui::Event::WindowFocused(false),
            enter(true, egui::Modifiers::NONE),
        ],
        egui::Modifiers::NONE,
        false,
    );
    assert_eq!(ui.studio.filter, QUERY);
    assert_eq!(ui.studio.selected, Some(1));
}
