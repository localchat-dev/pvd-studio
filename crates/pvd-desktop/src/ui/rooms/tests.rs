use super::*;
use pvd_core::frame_sources::FrameObject;
use pvd_protocol::{Endian, Event, EventGroup, Header, Name};
use std::sync::Arc;

struct RoomsUi {
    context: egui::Context,
    studio: Studio,
    objects_top: f32,
}

impl RoomsUi {
    fn new(language: Language, count: usize) -> Self {
        let context = egui::Context::default();
        configure(&context);
        let mut builder = pvd_core::RecordingBuilder::new(Header {
            version: 1,
            timestamp_numerator: 1,
            timestamp_denominator: 1,
            flags: 1,
            endian: Endian::Little,
        });
        let class = Name {
            namespace: 1,
            name: 2,
        };
        let mut events: Vec<_> = [(1, "physx3"), (2, "PxScene"), (3, "frame")]
            .into_iter()
            .map(|(handle, value)| Event::StringHandle {
                handle,
                value: value.into(),
            })
            .collect();
        events.push(Event::CreateClass { name: class });
        events.extend((1..=count).map(|index| Event::CreateInstance {
            id: index as u64,
            class,
        }));
        events.extend((1..=count).map(|index| Event::EndSection {
            id: index as u64,
            name: 3,
            timestamp: index as u64,
        }));
        if count >= 5 {
            events.extend([
                Event::DestroyInstance { id: 3 },
                Event::DestroyInstance { id: 5 },
            ]);
        }
        builder
            .push(EventGroup {
                stream_id: 1,
                timestamp: 0,
                offset: 0,
                events,
            })
            .unwrap();
        let mut studio = Studio::default();
        studio.prefs.language = language;
        studio.recording = Some(Arc::new(builder.snapshot().unwrap()));
        studio.seek(0);
        let mut result = Self {
            context,
            studio,
            objects_top: 0.0,
        };
        result.frame(vec![], false);
        result.frame(vec![], false);
        result
    }

    fn frame(&mut self, events: Vec<egui::Event>, retry: bool) -> egui::FullOutput {
        let context = self.context.clone();
        context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1024.0, 720.0),
                )),
                focused: true,
                events,
                ..Default::default()
            },
            |context| {
                egui::SidePanel::left("room-test-left")
                    .exact_width(195.0)
                    .resizable(false)
                    .show(context, |ui| {
                        show(ui, &mut self.studio);
                        self.objects_top = ui.cursor().top();
                        ui.label("Object tree");
                    });
                if retry && context.current_pass_index() == 0 {
                    context.request_discard("room input across a layout retry");
                }
            },
        )
    }

    fn click(&mut self, label: &str, retry: bool) -> egui::FullOutput {
        let output = self.frame(vec![], false);
        let pos = text_rect(&output, label).center();
        self.frame(vec![egui::Event::PointerMoved(pos)], false);
        let event = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        self.frame(vec![event(true)], false);
        self.frame(vec![event(false)], retry)
    }

    fn apply(&mut self) {
        self.studio.apply_ui_actions(&self.context);
    }
}

fn text_rect(output: &egui::FullOutput, expected: &str) -> egui::Rect {
    output
        .shapes
        .iter()
        .rev()
        .find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == expected => Some(
                text.galley
                    .rect
                    .translate(text.pos.to_vec2())
                    .intersect(shape.clip_rect),
            ),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing text: {expected}"))
}

fn has_text(output: &egui::FullOutput, expected: &str) -> bool {
    output.shapes.iter().any(|shape| matches!(&shape.shape, egui::epaint::Shape::Text(text) if text.galley.text() == expected))
}

#[test]
fn collapse_and_expand_survive_layout_retry_and_return_space_to_objects() {
    for language in [Language::English, Language::Chinese] {
        let mut ui = RoomsUi::new(language, 40);
        let expanded_bottom = ui.objects_top;
        let output = ui.click(language.text("Collapse", "收起"), true);
        assert_eq!(output.platform_output.num_completed_passes, 2);
        assert!(ui.studio.room_view.expanded);
        ui.apply();
        assert!(!ui.studio.room_view.expanded);
        let output = ui.frame(vec![], false);
        assert!(has_text(&output, language.text("Expand", "展开")));
        assert!(!has_text(&output, language.text("Find scene", "搜索场景")));
        assert!(ui.objects_top + 100.0 < expanded_bottom);
        ui.click(language.text("Expand", "展开"), true);
        ui.apply();
        assert!(ui.studio.room_view.expanded);
    }
}

#[test]
fn search_selects_a_virtualized_room_once_after_layout_and_keeps_current_pass_stable() {
    let mut ui = RoomsUi::new(Language::English, 80);
    let output = ui.frame(vec![], false);
    assert!(!has_text(&output, "Physics Scene 20"));
    ui.click("Find scene", false);
    ui.frame(vec![egui::Event::Text("Physics Scene 20".into())], false);
    assert_eq!(ui.studio.room_view.filter, "Physics Scene 20");
    let output = ui.frame(vec![], false);
    assert!(has_text(&output, "Physics Scene 20"));
    assert!(!has_text(&output, "Physics Scene 1"));
    let before = (
        ui.studio.frame,
        ui.studio.revision,
        ui.studio.displayed_room(),
    );
    let output = ui.click("Physics Scene 20", true);
    assert_eq!(output.platform_output.num_completed_passes, 2);
    assert_eq!(
        (
            ui.studio.frame,
            ui.studio.revision,
            ui.studio.displayed_room()
        ),
        before
    );
    ui.apply();
    assert_eq!(
        ui.studio.displayed_room(),
        Some(FrameObject {
            id: 20,
            generation: 0
        })
    );
    assert_eq!(ui.studio.frame, 19);
    let accepted_revision = ui.studio.revision;
    ui.apply();
    assert_eq!(ui.studio.revision, accepted_revision);
}

#[test]
fn destroyed_filter_preserves_selected_room_and_all_scenes_removes_display_scope() {
    let mut ui = RoomsUi::new(Language::English, 5);
    ui.click("Physics Scene 3", false);
    ui.apply();
    assert_eq!(
        ui.studio.displayed_room(),
        Some(FrameObject {
            id: 3,
            generation: 0
        })
    );
    let output = ui.click("Destroyed", true);
    assert_eq!(output.platform_output.num_completed_passes, 2);
    assert!(ui.studio.room_view.show_destroyed);
    ui.apply();
    let output = ui.frame(vec![], false);
    assert!(!ui.studio.room_view.show_destroyed);
    assert!(has_text(&output, "Physics Scene 3"));
    assert!(!has_text(&output, "Physics Scene 5"));
    let frame = ui.studio.frame;
    ui.click("All scenes", true);
    ui.apply();
    assert_eq!(ui.studio.displayed_room(), None);
    assert_eq!(ui.studio.room_scope.room(), None);
    assert_eq!(ui.studio.frame, frame);
}

#[test]
fn narrow_room_rows_keep_titles_and_status_separate_and_leave_tree_height() {
    for language in [Language::English, Language::Chinese] {
        let mut ui = RoomsUi::new(language, 40);
        ui.studio.live_session = Some(1);
        ui.studio.live_connected = false;
        for id in 1..=40 {
            ui.studio
                .room_view
                .counts
                .insert(FrameObject { id, generation: 0 }, 123456);
        }
        let output = ui.frame(vec![], false);
        assert!(
            ui.objects_top < 360.0,
            "room list must leave at least half the left pane to objects"
        );
        let title = text_rect(&output, language.text("Physics Scene 1", "物理场景 1"));
        let status = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text)
                    if text
                        .galley
                        .text()
                        .contains(language.text("Disconnected", "已断开")) =>
                {
                    assert_eq!(text.galley.rows.len(), 1);
                    let rect = text
                        .galley
                        .rect
                        .translate(text.pos.to_vec2())
                        .intersect(shape.clip_rect);
                    ((rect.center().y - title.center().y).abs() < 2.0).then_some(rect)
                }
                _ => None,
            })
            .expect("room status");
        assert!(
            title.right() <= status.left(),
            "title and summary overlap at 1024: {title:?} {status:?}"
        );
        assert!(status.right() <= 195.0);
        let visible_rooms = output.shapes.iter().filter(|shape| matches!(&shape.shape, egui::epaint::Shape::Text(text) if text.galley.text().starts_with(language.text("Physics Scene ", "物理场景 ")))).count();
        assert!(visible_rooms < 15, "long room lists must be virtualized");
    }
}

#[test]
fn temporary_scene_status_stays_ended_while_another_connection_is_active() {
    for language in [Language::English, Language::Chinese] {
        let mut ui = RoomsUi::new(language, 5);
        ui.studio.temporary_recordings.selected = Some(1);
        ui.studio.live_session = Some(2);
        ui.studio.live_connected = true;
        let output = ui.frame(vec![], false);
        assert!(has_text(&output, language.text("Ended", "已结束")));
        assert!(has_text(&output, language.text("Destroyed", "已销毁")));
        assert!(!has_text(&output, language.text("Online", "在线")));
    }
}
