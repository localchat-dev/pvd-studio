use super::*;
use pvd_protocol::{Endian, Event, EventGroup, Header, Name};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

fn recording() -> Recording {
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
    let mut events: Vec<_> = [
        (1, "physx3"),
        (2, "PropertyContextFixture"),
        (3, "Scalar"),
        (4, "Vectors"),
        (5, "Label"),
        (6, "Target"),
        (7, "frame"),
        (10, "PvdF32"),
        (11, "PxVec3"),
        (12, "StringHandle"),
        (13, "ObjectRef"),
        (20, "captured text"),
    ]
    .into_iter()
    .map(|(handle, value)| Event::StringHandle {
        handle,
        value: value.into(),
    })
    .collect();
    events.push(Event::CreateClass { name: class });
    for (name, datatype, kind) in [(3, 10, 1), (4, 11, 2), (5, 12, 1), (6, 13, 1)] {
        events.push(Event::CreateProperty {
            class,
            name,
            semantic: 0,
            datatype: Name {
                namespace: 1,
                name: datatype,
            },
            kind,
            named_values: vec![],
        });
    }
    for id in [1, 7, 9] {
        events.push(Event::CreateInstance { class, id });
    }
    for (property, datatype, items, data) in [
        (3, 10, 1, 3.25f32.to_le_bytes().to_vec()),
        (
            4,
            11,
            2,
            [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0]
                .into_iter()
                .flat_map(f32::to_le_bytes)
                .collect(),
        ),
        (5, 12, 1, 20u32.to_le_bytes().to_vec()),
        (6, 13, 1, 9u64.to_le_bytes().to_vec()),
    ] {
        events.push(Event::SetPropertyValue {
            id: 7,
            property,
            datatype: Name {
                namespace: 1,
                name: datatype,
            },
            items,
            data: data.into(),
        });
    }
    events.extend((0..4).map(|timestamp| Event::EndSection {
        id: 1,
        name: 7,
        timestamp,
    }));
    builder
        .push(EventGroup {
            stream_id: 1,
            timestamp: 0,
            offset: 0,
            events,
        })
        .unwrap();
    builder.finish().unwrap()
}

struct InspectorUi {
    context: egui::Context,
    studio: Studio,
    viewport: Viewport,
    images: Assets<Image>,
    menu_id: egui::Id,
    main_layer: egui::LayerId,
    time: f64,
}

impl InspectorUi {
    fn new(language: Language) -> Self {
        let context = egui::Context::default();
        configure(&context);
        let mut studio = Studio::default();
        studio.prefs.language = language;
        studio.recording = Some(Arc::new(recording()));
        studio.seek(1);
        studio.selected = Some(7);
        let mut ui = Self {
            context,
            studio,
            viewport: Viewport::default(),
            images: Assets::default(),
            menu_id: egui::Id::NULL,
            main_layer: egui::LayerId::background(),
            time: 0.0,
        };
        let _ = ui.frame(vec![]);
        let _ = ui.frame(vec![]);
        ui
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
        self.time += 0.05;
        let context = self.context.clone();
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(760.0, 740.0),
                )),
                events,
                time: Some(self.time),
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    self.menu_id = ui.id().with("property-context");
                    self.main_layer = ui.layer_id();
                    Viewer {
                        studio: &mut self.studio,
                        viewport: &mut self.viewport,
                        images: &mut self.images,
                    }
                    .inspector(ui);
                });
            },
        );
        assert_eq!(
            (
                self.studio.frame,
                self.studio.playing,
                self.studio.follow_live
            ),
            (1, 0, false)
        );
        assert!(!self.studio.focus_selected);
        output
    }

    fn click(&mut self, point: egui::Pos2, button: egui::PointerButton) -> egui::FullOutput {
        self.time += 1.0;
        let _ = self.frame(vec![egui::Event::PointerMoved(point)]);
        let _ = self.frame(vec![pointer(point, button, true)]);
        self.frame(vec![pointer(point, button, false)])
    }

    fn text_position(
        &self,
        output: &egui::FullOutput,
        expected: &str,
        root_menu: bool,
    ) -> Option<egui::Pos2> {
        let popup = root_menu
            .then(|| self.context.read_response(self.menu_id))
            .flatten();
        output
            .shapes
            .iter()
            .rev()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == expected => {
                    let point = text.galley.rect.translate(text.pos.to_vec2()).center();
                    let visible = shape.clip_rect.contains(point)
                        && match &popup {
                            Some(popup) => {
                                popup.rect.contains(point)
                                    && self.context.layer_id_at(point) == Some(popup.layer_id)
                            }
                            None if root_menu => false,
                            None => self.context.layer_id_at(point).is_some(),
                        };
                    visible.then_some(point)
                }
                _ => None,
            })
    }

    fn settle(&mut self, expected: &str, root_menu: bool) -> (egui::FullOutput, egui::Pos2) {
        let mut previous = None;
        let mut stable = 0;
        for _ in 0..12 {
            let output = self.frame(vec![]);
            if let Some(point) = self.text_position(&output, expected, root_menu) {
                if previous == Some(point) {
                    stable += 1;
                    if stable == 2 {
                        return (output, point);
                    }
                } else {
                    stable = 0;
                }
                previous = Some(point);
            }
        }
        panic!("visible text did not settle: {expected}, last={previous:?}")
    }

    fn right_property(&mut self, displayed: &str) {
        let (_, point) = self.settle(displayed, false);
        assert_eq!(self.context.layer_id_at(point), Some(self.main_layer));
        let _ = self.click(point, egui::PointerButton::Secondary);
        let label = self.studio.prefs.language.text("Copy", "复制");
        let _ = self.settle(label, true);
        assert!(egui::Popup::is_id_open(&self.context, self.menu_id));
    }

    fn right_array_element(&mut self, index: usize) {
        let language = self.studio.prefs.language;
        let title = language.text("Array · 2 values", "数组 · 2 项");
        let (_, point) = self.settle(title, false);
        let _ = self.click(point, egui::PointerButton::Primary);
        let (_, point) = self.settle(&format!("[{index}]"), false);
        assert_ne!(self.context.layer_id_at(point), Some(self.main_layer));
        let _ = self.click(point, egui::PointerButton::Secondary);
        let _ = self.settle(&format!("Vectors[{index}]"), true);
    }

    fn menu_click(&mut self, label: &str) -> egui::FullOutput {
        let (_, point) = self.settle(label, true);
        self.click(point, egui::PointerButton::Primary)
    }

    fn choose_component(&mut self, component: &str) {
        let label = self.studio.prefs.language.text("Plot", "曲线");
        let _ = self.menu_click(label);
        let (_, point) = self.settle(component, false);
        assert_ne!(self.context.layer_id_at(point), Some(self.main_layer));
        let _ = self.click(point, egui::PointerButton::Primary);
    }

    fn escape(&mut self) {
        for pressed in [true, false] {
            let _ = self.frame(vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }]);
        }
        let _ = self.frame(vec![]);
        assert!(!egui::Popup::is_id_open(&self.context, self.menu_id));
    }

    fn wait_curves(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.studio.curve_job.is_some()
            || !self.studio.curves.pending.is_empty()
            || self.studio.background_jobs.active_count() != 0
        {
            self.studio.poll(&self.context);
            assert!(Instant::now() < deadline);
            std::thread::park_timeout(Duration::from_millis(1));
        }
    }
}

fn pointer(pos: egui::Pos2, button: egui::PointerButton, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}

fn copied(output: &egui::FullOutput) -> Vec<&str> {
    output
        .platform_output
        .commands
        .iter()
        .filter_map(|command| match command {
            egui::OutputCommand::CopyText(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn property_context_copy_and_plot_use_the_actual_property_and_array_index() {
    for language in [Language::Chinese, Language::English] {
        let mut ui = InspectorUi::new(language);
        ui.right_property("3.250000");
        let output = ui.menu_click(language.text("Copy", "复制"));
        let scalar_copy = crate::property_copy::text(&Value::F32(3.25)).unwrap();
        assert_eq!(copied(&output), vec![scalar_copy.as_str()]);
        assert_eq!(ui.studio.selected, Some(7));
        ui.right_property("Scalar");
        ui.choose_component(language.text("value", "值"));
        assert_eq!(ui.studio.curves.curves.len(), 1);
        let scalar = &ui.studio.curves.curves[0].spec;
        assert_eq!(
            (
                scalar.object,
                scalar.generation,
                scalar.property.as_str(),
                scalar.component
            ),
            (7, 0, "Scalar", 0)
        );
        assert_eq!(scalar.source, crate::curve::Source::Property);
        ui.right_array_element(1);
        let output = ui.menu_click(language.text("Copy", "复制"));
        let vector_copy = crate::property_copy::text(&Value::Vec3([4.0, 5.0, 6.0])).unwrap();
        assert_eq!(copied(&output), vec![vector_copy.as_str()]);
        ui.right_array_element(1);
        ui.choose_component("y");
        assert_eq!(ui.studio.curves.curves.len(), 2);
        let vector = &ui.studio.curves.curves[1].spec;
        assert_eq!(
            (
                vector.object,
                vector.generation,
                vector.property.as_str(),
                vector.component
            ),
            (7, 0, "Vectors", 1)
        );
        assert_eq!(
            vector.source,
            crate::curve::Source::PropertyElement {
                index: 1,
                value_type: crate::curve::ElementType::Vec3
            }
        );
        ui.wait_curves();
        for (curve, expected) in ui.studio.curves.curves.iter().zip([3.25, 5.0]) {
            assert_eq!(
                curve.data.as_ref().unwrap().samples,
                vec![
                    (0, Some(expected)),
                    (1, Some(expected)),
                    (2, Some(expected)),
                    (3, Some(expected))
                ]
            );
        }
    }
}

#[test]
fn property_context_keeps_non_numeric_plot_disabled_and_reference_navigation_unchanged() {
    let mut ui = InspectorUi::new(Language::Chinese);
    ui.right_property("captured text");
    let output = ui.menu_click("曲线");
    assert!(copied(&output).is_empty());
    assert!(ui.studio.curves.curves.is_empty());
    ui.escape();
    ui.right_property("→ 0x0000000000000009");
    assert_eq!(ui.studio.selected, Some(7));
    ui.escape();
    assert_eq!(ui.studio.selected, Some(7));
    let (_, point) = ui.settle("→ 0x0000000000000009", false);
    let _ = ui.click(point, egui::PointerButton::Primary);
    assert_eq!(ui.studio.selected, Some(9));
    assert!(ui.studio.curves.curves.is_empty());
}

#[test]
fn property_context_rejects_source_generation_index_and_type_changes_after_opening() {
    for stale in 0..5 {
        let mut ui = InspectorUi::new(Language::Chinese);
        ui.right_array_element(1);
        if stale == 4 {
            let (_, old_copy) = ui.settle("复制", true);
            ui.studio.state.objects.remove(&7);
            let _ = ui.frame(vec![]);
            assert!(!egui::Popup::is_id_open(&ui.context, ui.menu_id));
            let output = ui.click(old_copy, egui::PointerButton::Primary);
            assert!(copied(&output).is_empty());
            assert!(ui.studio.curves.curves.is_empty());
            continue;
        }
        match stale {
            0 => {
                let recording = ui.studio.recording.clone().unwrap();
                ui.studio.close();
                ui.studio.recording = Some(recording);
                ui.studio.seek(1);
                ui.studio.selected = Some(7);
            }
            1 => ui.studio.state.objects.get_mut(&7).unwrap().generation += 1,
            2 => {
                ui.studio
                    .state
                    .objects
                    .get_mut(&7)
                    .unwrap()
                    .properties
                    .insert(
                        "Vectors".into(),
                        Value::Array(vec![Value::Vec3([1.0, 2.0, 3.0])].into()),
                    );
            }
            3 => {
                ui.studio
                    .state
                    .objects
                    .get_mut(&7)
                    .unwrap()
                    .properties
                    .insert(
                        "Vectors".into(),
                        Value::Array(
                            vec![
                                Value::Vec3([1.0, 2.0, 3.0]),
                                Value::Quat([4.0, 5.0, 6.0, 1.0]),
                            ]
                            .into(),
                        ),
                    );
            }
            _ => unreachable!(),
        }
        let _ = ui.settle("此帧中该属性不可用。", true);
        for action in ["复制", "曲线"] {
            let output = ui.menu_click(action);
            assert!(copied(&output).is_empty());
            assert!(ui.studio.curves.curves.is_empty());
            assert_eq!(ui.studio.selected, Some(7));
            assert_eq!(ui.studio.background_jobs.active_count(), 0);
        }
        ui.escape();
    }
}
