use std::sync::{Arc, Weak};

use pvd_core::analysis::view::MemorySource;
use pvd_core::{NativeMemoryTotals, Recording, SceneState};

use crate::i18n::Language;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Position {
    Initial,
    #[default]
    Current,
    End,
}

#[derive(Default)]
pub(super) struct Cache {
    source: Weak<Recording>,
    position: Option<Position>,
    frame: usize,
    revision: u64,
    value: Option<Result<Option<NativeMemoryTotals>, String>>,
    default_checked: bool,
}

impl Cache {
    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(super) fn clear_values(&mut self) {
        *self = Self {
            default_checked: self.default_checked,
            ..Self::default()
        };
    }

    pub(super) fn keep_selection(&mut self) {
        self.default_checked = true;
    }

    pub(super) fn default_position(&mut self, recording: &Arc<Recording>) -> Option<Position> {
        if self.default_checked {
            return None;
        }
        self.default_checked = true;
        let memory = recording
            .native_end_analysis()
            .unwrap_or(&recording.analysis)
            .memory_snapshot();
        let summary = memory.summary();
        if memory.event_count() != 0 || summary.allocations != 0 || summary.frees != 0 {
            return None;
        }
        let initial = recording.native_initial_memory_totals().ok().flatten();
        let end = if initial.as_ref().is_none_or(|view| view.is_empty()) {
            recording.native_end_memory_totals().ok().flatten()
        } else {
            None
        };
        let position = suggested_position(
            initial.as_ref().is_some_and(|view| !view.is_empty()),
            end.as_ref().is_some_and(|view| !view.is_empty()),
        )?;
        self.source = Arc::downgrade(recording);
        self.position = Some(position);
        self.value = Some(Ok(if position == Position::Initial {
            initial
        } else {
            end
        }));
        Some(position)
    }

    fn update(
        &mut self,
        recording: &Arc<Recording>,
        state: &SceneState,
        position: Position,
        frame: usize,
        revision: u64,
    ) -> bool {
        let same_source = Weak::ptr_eq(&self.source, &Arc::downgrade(recording));
        let unchanged = match (&self.value, position) {
            (Some(Ok(Some(view))), Position::Current) => view.matches_state(recording, state),
            (_, Position::Current) => {
                same_source && self.frame == frame && self.revision == revision
            }
            _ => same_source,
        };
        if self.position == Some(position) && unchanged {
            self.source = Arc::downgrade(recording);
            self.frame = frame;
            self.revision = revision;
            return false;
        }
        self.source = Arc::downgrade(recording);
        self.position = Some(position);
        self.frame = frame;
        self.revision = revision;
        self.value = Some(
            match position {
                Position::Initial => recording.native_initial_memory_totals(),
                Position::Current => recording.native_memory_totals_at(state),
                Position::End => recording.native_end_memory_totals(),
            }
            .map_err(|error| error.to_string()),
        );
        true
    }
}

pub(super) fn position_for_frames(position: Position, frames: usize) -> Position {
    if frames == 0 && position == Position::Current {
        Position::Initial
    } else {
        position
    }
}

pub(super) fn select_position(
    ui: &mut egui::Ui,
    selected: &mut Position,
    frames: usize,
    language: Language,
) {
    *selected = position_for_frames(*selected, frames);
    ui.selectable_value(
        selected,
        Position::Initial,
        language.text("Initial", "起点"),
    );
    ui.add_enabled_ui(frames > 0, |ui| {
        ui.selectable_value(
            selected,
            Position::Current,
            language.text("Current frame", "当前帧"),
        );
    });
    ui.selectable_value(
        selected,
        Position::End,
        language.text("Capture end", "录制结束"),
    );
}

pub(super) fn show(
    ui: &mut egui::Ui,
    studio: &mut crate::studio::Studio,
    recording: &Arc<Recording>,
) {
    let language = studio.prefs.language;
    let position =
        position_for_frames(studio.memory_panel.totals_position, recording.frame_count());
    studio.memory_panel.totals_position = position;
    studio.memory_panel.totals.update(
        recording,
        &studio.state,
        position,
        studio.frame,
        studio.revision,
    );
    let Some(result) = &studio.memory_panel.totals.value else {
        return;
    };
    match result {
        Err(detail) => {
            ui.label(language.text("Source totals are unavailable.", "来源总量不可用。"));
            ui.label(detail);
        }
        Ok(None) => {
            ui.label(language.text(
                "No source totals were captured at this position.",
                "此处未捕获来源总量。",
            ));
        }
        Ok(Some(view)) => show_rows(ui, view, language),
    }
}

fn suggested_position(initial_nonempty: bool, end_nonempty: bool) -> Option<Position> {
    if initial_nonempty {
        Some(Position::Initial)
    } else if end_nonempty {
        Some(Position::End)
    } else {
        None
    }
}

fn show_rows(ui: &mut egui::Ui, view: &NativeMemoryTotals, language: Language) {
    ui.horizontal_wrapped(|ui| {
        ui.label(format!("{} {}", view.len(), language.text("sources", "项来源")));
        if let Some(bytes) = view.nonnegative_bytes() {
            ui.label(format!("{} {bytes} B", language.text("Sum", "合计")))
                .on_hover_text(language.text("Sum of captured source values; this does not describe OS memory or a complete allocation list.", "捕获的来源值合计，不代表系统内存或完整分配列表。"));
        } else {
            ui.label(language.text("Sum unavailable", "合计不可用"))
                .on_hover_text(language.text("Negative captured values are preserved; they do not describe negative live memory.", "保留捕获中的负值，不将其解释为负的存活内存。"));
        }
    });
    if view.is_empty() {
        return;
    }
    let bytes_width = (ui.available_width() * 0.28).clamp(95.0, 135.0);
    ui.horizontal(|ui| {
        ui.add_sized(
            [bytes_width, 20.0],
            egui::Label::new(language.text("Bytes", "字节")),
        )
        .on_hover_text(language.text(
            "Signed 32-bit values are preserved.",
            "保留捕获中的有符号32位值。",
        ));
        ui.label(language.text("Source", "来源"));
    });
    egui::ScrollArea::vertical()
        .id_salt("native-memory-totals")
        .show_rows(ui, 23.0, view.len(), |ui, range| {
            for index in range {
                let Some(row) = view.row(index) else { continue };
                let file = row.file.rsplit(['/', '\\']).next().unwrap_or(row.file);
                ui.horizontal(|ui| {
                    let value = egui::RichText::new(format!("{} B", row.bytes)).monospace();
                    let value = if row.bytes < 0 {
                        value.color(egui::Color32::YELLOW)
                    } else {
                        value
                    };
                    ui.add_sized([bytes_width, 20.0], egui::Label::new(value));
                    ui.add(
                        egui::Label::new(format!("{} · {file}:{}", row.type_name, row.line))
                            .truncate(),
                    )
                    .on_hover_ui(|ui| {
                        ui.label(row.type_name);
                        ui.label(row.file);
                        ui.label(format!("{} · #{}", row.line, row.index));
                    });
                });
            }
        });
}

pub(crate) fn prepare_render(studio: &mut crate::studio::Studio) -> Result<(), String> {
    let recording = studio
        .recording
        .clone()
        .ok_or("No recording is available.")?;
    let position =
        position_for_frames(studio.memory_panel.totals_position, recording.frame_count());
    studio.memory_panel.totals.update(
        &recording,
        &studio.state,
        position,
        studio.frame,
        studio.revision,
    );
    match studio.memory_panel.totals.value.as_ref() {
        Some(Ok(Some(view))) => {
            println!(
                "Memory source totals: {}",
                serde_json::json!({"sources": view.len(), "captured_nonnegative_bytes": view.nonnegative_bytes(), "position": format!("{position:?}")})
            );
            Ok(())
        }
        Some(Err(error)) => Err(error.clone()),
        _ => Err("No source totals were captured at this position.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pvd_protocol::{Endian, Header};

    fn empty_recording() -> Arc<Recording> {
        Arc::new(
            pvd_core::RecordingBuilder::new(Header {
                version: 1,
                timestamp_numerator: 1,
                timestamp_denominator: 1,
                flags: 0,
                endian: Endian::Little,
            })
            .finish()
            .unwrap(),
        )
    }

    #[test]
    fn totals_cache_reuses_unchanged_positions_and_does_not_keep_recordings_alive() {
        let recording = empty_recording();
        let weak = Arc::downgrade(&recording);
        let state = SceneState::default();
        let mut cache = Cache::default();
        assert!(cache.update(&recording, &state, Position::Initial, 0, 0));
        assert!(!cache.update(&recording, &state, Position::Initial, 20, 1));
        assert!(cache.update(&recording, &state, Position::End, 20, 1));
        assert!(!cache.update(&recording, &state, Position::End, 21, 2));
        assert!(cache.update(&recording, &state, Position::Current, 21, 2));
        assert!(!cache.update(&recording, &state, Position::Current, 21, 2));
        assert!(cache.update(&recording, &state, Position::Current, 21, 3));
        let other = empty_recording();
        assert!(cache.update(&other, &state, Position::Current, 21, 3));
        drop(recording);
        assert!(weak.upgrade().is_none());
        cache.reset();
        assert!(cache.value.is_none() && cache.position.is_none());
    }

    #[test]
    fn zero_frame_totals_use_the_initial_position_instead_of_a_synthetic_frame() {
        assert_eq!(position_for_frames(Position::Current, 0), Position::Initial);
        assert_eq!(position_for_frames(Position::End, 0), Position::End);
        assert_eq!(position_for_frames(Position::Current, 1), Position::Current);
    }

    #[test]
    fn captures_without_events_prefer_real_initial_or_end_totals_once() {
        assert_eq!(suggested_position(true, true), Some(Position::Initial));
        assert_eq!(suggested_position(false, true), Some(Position::End));
        assert_eq!(suggested_position(false, false), None);
        let recording = empty_recording();
        let mut cache = Cache::default();
        let _ = cache.default_position(&recording);
        assert!(cache.default_checked);
        assert_eq!(cache.default_position(&recording), None);
        cache.clear_values();
        assert!(cache.default_checked && cache.value.is_none() && cache.source.upgrade().is_none());
        cache.reset();
        assert!(!cache.default_checked);
    }

    fn fixture(name: &str) -> Arc<Recording> {
        let directory = std::path::PathBuf::from(
            std::env::var_os("PVD_NATIVE_MEMORY_FIXTURE_DIRECTORY")
                .expect("constructed memory fixture directory"),
        );
        Arc::new(Recording::from_bytes(&std::fs::read(directory.join(name)).unwrap()).unwrap())
    }

    fn labels(context: &egui::Context, studio: &mut crate::studio::Studio) -> Vec<String> {
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(950.0, 240.0),
                )),
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| super::super::show(ui, studio));
            },
        );
        output
            .shapes
            .into_iter()
            .filter_map(|shape| match shape.shape {
                egui::epaint::Shape::Text(text)
                    if shape
                        .clip_rect
                        .contains(text.galley.rect.translate(text.pos.to_vec2()).center()) =>
                {
                    Some(text.galley.text().to_owned())
                }
                _ => None,
            })
            .collect()
    }

    fn advance_view_epoch(studio: &mut crate::studio::Studio, context: &egui::Context) {
        use bevy::prelude::*;
        use bevy_egui::{EguiContext, EguiUserTextures, PrimaryEguiContext};
        let mut app = App::new();
        app.insert_resource(std::mem::take(studio))
            .init_resource::<EguiUserTextures>()
            .add_systems(Update, crate::ui::prepare);
        let mut entity = app.world_mut().spawn(PrimaryEguiContext);
        *entity.get_mut::<EguiContext>().unwrap().get_mut() = context.clone();
        app.update();
        app.world_mut()
            .resource_mut::<crate::studio::Studio>()
            .profile_view_epoch += 1;
        app.update();
        *studio = app
            .world_mut()
            .remove_resource::<crate::studio::Studio>()
            .unwrap();
    }

    #[test]
    #[ignore = "requires PVD_NATIVE_MEMORY_FIXTURE_DIRECTORY constructed fixtures"]
    fn constructed_native_memory_totals_are_visible_without_frames_or_observed_events() {
        let zero = fixture("memory-initial-zero.pxd2");
        assert_eq!(zero.frame_count(), 0);
        for language in [Language::English, Language::Chinese] {
            let mut studio = crate::studio::Studio::default();
            studio.persist_layout = false;
            studio.prefs.language = language;
            studio.recording = Some(zero.clone());
            studio.state = zero.initial_state().clone();
            let context = egui::Context::default();
            let _ = labels(&context, &mut studio);
            let text = labels(&context, &mut studio);
            assert!(studio.memory_panel.source_totals);
            assert_eq!(studio.memory_panel.totals_position, Position::Initial);
            for expected in ["64 B", "16 B", language.text("Sum 80 B", "合计 80 B")] {
                assert!(text.iter().any(|label| label == expected), "{text:?}");
            }
            assert!(
                !text
                    .iter()
                    .any(|label| label.contains("instrumentation") || label.contains("启用 SDK")),
                "{text:?}"
            );
            studio.memory_panel.show_events();
            let text = labels(&context, &mut studio);
            assert!(!studio.memory_panel.source_totals);
            assert!(
                text.iter().any(|label| label.contains(
                    language.text("earlier allocations are unavailable", "起点前分配未知")
                )),
                "{text:?}"
            );
            assert!(
                text.iter().any(|label| label.contains(
                    language.text("No observable memory events", "没有可观察的内存事件")
                )),
                "{text:?}"
            );
            let _ = labels(&context, &mut studio);
            assert!(!studio.memory_panel.source_totals);
            advance_view_epoch(&mut studio, &context);
            let _ = labels(&context, &mut studio);
            assert!(!studio.memory_panel.source_totals);
            studio.close();
            assert!(studio.recording.is_none());
            studio.recording = Some(zero.clone());
            studio.state = zero.initial_state().clone();
            let _ = labels(&context, &mut studio);
            assert!(studio.memory_panel.source_totals);

            let framed = fixture("memory-initial-frames.pxd2");
            studio.close();
            studio.recording = Some(framed.clone());
            studio.state = framed.state_at(0).unwrap();
            let text = labels(&context, &mut studio);
            assert!(
                !studio.memory_panel.source_totals,
                "captured events keep the event view default"
            );
            assert!(
                text.iter().any(|label| label.contains(
                    language.text("earlier allocations are unavailable", "起点前分配未知")
                )),
                "{text:?}"
            );
            studio.memory_panel.show_events();
            let _ = labels(&context, &mut studio);
            let text = labels(&context, &mut studio);
            assert!(
                text.iter().any(|label| label == "0x000000000000002a"),
                "{text:?}"
            );
            assert!(
                text.iter()
                    .any(|label| label == language.text("Unknown allocation", "未知分配")),
                "{text:?}"
            );
        }
        let current = fixture("memory-current.pxd2");
        let mut cache = Cache::default();
        assert_eq!(cache.default_position(&current), Some(Position::End));
        assert_eq!(cache.default_position(&current), None);
    }

    #[test]
    #[ignore = "requires PVD_NATIVE_MEMORY_FIXTURE_DIRECTORY constructed fixtures"]
    fn constructed_native_memory_totals_cache_tracks_shared_values_and_same_frame_replacements() {
        let recording = fixture("memory-initial-frames.pxd2");
        let mut state = recording.state_at(0).unwrap();
        let mut cache = Cache::default();
        assert!(cache.update(&recording, &state, Position::Current, 0, 1));
        assert_eq!(
            cache
                .value
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .nonnegative_bytes(),
            Some(48)
        );
        assert!(!cache.update(&recording, &state.clone(), Position::Current, 0, 2));
        let cloned = Arc::new(recording.as_ref().clone());
        assert!(!cache.update(&cloned, &state, Position::Current, 0, 2));
        let values = |bytes| {
            pvd_core::Value::Array(
                vec![pvd_core::Value::I64(bytes), pvd_core::Value::I64(16)].into(),
            )
        };
        state
            .objects
            .get_mut(&3)
            .unwrap()
            .properties
            .insert("totals".into(), values(30));
        assert!(cache.update(&recording, &state, Position::Current, 0, 3));
        assert_eq!(
            cache
                .value
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .nonnegative_bytes(),
            Some(46)
        );
        state
            .objects
            .get_mut(&3)
            .unwrap()
            .properties
            .remove("totals");
        assert!(cache.update(&recording, &state, Position::Current, 0, 4));
        assert!(matches!(cache.value, Some(Ok(None))));
        assert!(!cache.update(&recording, &state, Position::Current, 0, 4));
        state
            .objects
            .get_mut(&3)
            .unwrap()
            .properties
            .insert("totals".into(), pvd_core::Value::U64(1));
        assert!(cache.update(&recording, &state, Position::Current, 0, 5));
        assert!(matches!(cache.value, Some(Err(_))));
        state
            .objects
            .get_mut(&3)
            .unwrap()
            .properties
            .insert("totals".into(), values(-2));
        assert!(cache.update(&recording, &state, Position::Current, 0, 6));
        let totals = cache
            .value
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap();
        assert_eq!(totals.nonnegative_bytes(), None);
        assert_eq!(totals.row(0).unwrap().bytes, -2);
        let other = fixture("memory-initial-frames.pxd2");
        assert!(cache.update(&other, &other.state_at(0).unwrap(), Position::Current, 0, 6));
        cache.reset();
        assert!(cache.value.is_none() && cache.source.upgrade().is_none());
    }

    #[test]
    #[ignore = "requires PVD_NATIVE_MEMORY_FIXTURE_DIRECTORY constructed fixtures"]
    fn constructed_native_memory_totals_hidden_panel_releases_source_views_on_reset() {
        use bevy::prelude::*;
        use bevy_egui::{EguiContext, EguiUserTextures, PrimaryEguiContext};
        let recording = fixture("memory-initial-zero.pxd2");
        let pvd_core::Value::Array(values) =
            &recording.initial_state().objects[&3].properties["totals"]
        else {
            panic!("totals array")
        };
        let weak = Arc::downgrade(values);
        let mut studio = crate::studio::Studio::default();
        studio.persist_layout = false;
        studio.recording = Some(recording.clone());
        let mut app = App::new();
        app.insert_resource(studio)
            .init_resource::<EguiUserTextures>()
            .add_systems(Update, crate::ui::prepare);
        let mut entity = app.world_mut().spawn(PrimaryEguiContext);
        *entity.get_mut::<EguiContext>().unwrap().get_mut() = egui::Context::default();
        app.update();
        {
            let mut studio = app.world_mut().resource_mut::<crate::studio::Studio>();
            assert!(studio.memory_panel.totals.update(
                &recording,
                recording.initial_state(),
                Position::Initial,
                0,
                0
            ));
            studio.recording = None;
            studio.profile_view_epoch += 1;
        }
        drop(recording);
        assert!(weak.upgrade().is_some());
        app.update();
        assert!(weak.upgrade().is_none());
        let cache = &app
            .world()
            .resource::<crate::studio::Studio>()
            .memory_panel
            .totals;
        assert!(cache.value.is_none() && cache.source.upgrade().is_none());
    }
}
