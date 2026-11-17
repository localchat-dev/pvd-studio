use super::*;

const WINDOW_ID: &str = "navmesh-overlay";

pub fn window(context: &egui::Context, overlay: &mut NavMeshOverlay, studio: &Studio) {
    let open_id = egui::Id::new("navmesh-window-open");
    let was_open = context.data(|data| data.get_temp::<bool>(open_id).unwrap_or(false));
    if !overlay.open {
        context.data_mut(|data| data.insert_temp(open_id, false));
        return;
    }
    let previous = settings_snapshot(overlay);
    let mut open = true;
    let screen = context.content_rect().shrink(16.0);
    let max_width = (screen.width() - 32.0).max(220.0);
    let mut window = egui::Window::new(
        studio
            .prefs
            .language
            .text("NavMesh overlay", "NavMesh 叠加"),
    )
    .id(egui::Id::new(WINDOW_ID))
    .open(&mut open)
    .pivot(egui::Align2::CENTER_CENTER)
    .default_width(600.0_f32.min(max_width))
    .default_height(600.0_f32.min((screen.height() - 64.0).max(160.0)))
    .min_width(560.0_f32.min(max_width))
    .max_width(max_width)
    .max_height((screen.height() - 64.0).max(160.0))
    .frame(egui::Frame::window(&context.style()).inner_margin(14))
    .constrain_to(screen)
    .collapsible(false)
    .resizable(true)
    .vscroll(true);
    if !was_open {
        // Recenter each opening without pinning the window while the user drags it.
        window = window.current_pos(screen.center());
    }
    window.show(context, |ui| panel(ui, overlay, studio));
    overlay.open = open;
    context.data_mut(|data| data.insert_temp(open_id, open));
    if previous != settings_snapshot(overlay) {
        // Rendering runs before egui; apply changed settings on the next frame.
        context.request_repaint();
    }
}

#[derive(PartialEq)]
struct SettingsSnapshot {
    selected: OverlayBinding,
    focus: Option<OverlayBinding>,
    entries: Vec<(OverlayBinding, OverlaySettings, u64)>,
}

fn settings_snapshot(overlay: &NavMeshOverlay) -> SettingsSnapshot {
    SettingsSnapshot {
        selected: overlay.selected_binding,
        focus: overlay.focus_requested,
        entries: overlay
            .entries
            .iter()
            .map(|entry| (entry.loaded.binding, entry.settings, entry.revision))
            .collect(),
    }
}

fn binding_label(binding: OverlayBinding, studio: &Studio) -> String {
    let language = studio.prefs.language;
    match binding {
        OverlayBinding::Global => language.text("Global overlay", "全局叠加").to_owned(),
        OverlayBinding::Room { epoch, room } if epoch == studio.room_session_epoch() => {
            studio.room_label(room)
        }
        OverlayBinding::Room { .. } => language.text("Previous capture", "上一次捕获").to_owned(),
    }
}

fn binding_detail(binding: OverlayBinding, studio: &Studio) -> String {
    match binding {
        OverlayBinding::Global => studio
            .prefs
            .language
            .text("Visible across physics scenes", "在各个物理场景中显示")
            .to_owned(),
        OverlayBinding::Room { room, .. } => {
            format!("PxScene 0x{:X} · generation {}", room.id, room.generation)
        }
    }
}

pub fn panel(ui: &mut egui::Ui, overlay: &mut NavMeshOverlay, studio: &Studio) {
    let language = studio.prefs.language;
    ui.spacing_mut().item_spacing.y = 8.0;
    ui.add(egui::Label::new(language.text(
        "Import a separate NavMesh for each physics scene. Switching scenes keeps every overlay.",
        "每个物理场景可分别导入 NavMesh，切换场景时保留各自的网格与设置。",
    )).wrap());
    ui.add_space(4.0);
    target_selector(ui, overlay, studio);
    import_controls(ui, overlay, studio);
    loaded_overlays(ui, overlay, studio);
    display_controls(ui, overlay, studio);
}

fn target_selector(ui: &mut egui::Ui, overlay: &mut NavMeshOverlay, studio: &Studio) {
    let language = studio.prefs.language;
    ui.strong(language.text("Import target", "导入目标"));
    let mut selected = overlay.selected_binding;
    let current = OverlayBinding::current(studio);
    let mut bindings = vec![OverlayBinding::Global];
    if let Some(directory) = studio.room_directory() {
        bindings.extend(directory.iter().map(|room| OverlayBinding::Room {
            epoch: studio.room_session_epoch(),
            room: room.scene,
        }));
    }
    if !bindings.contains(&current) {
        bindings.push(current);
    }
    for entry in &overlay.entries {
        if !bindings.contains(&entry.loaded.binding) {
            bindings.push(entry.loaded.binding);
        }
    }
    ui.horizontal(|ui| {
        let button_width = 120.0;
        egui::ComboBox::from_id_salt("navmesh-target")
            .selected_text(binding_label(selected, studio))
            .width((ui.available_width() - button_width - ui.spacing().item_spacing.x).max(80.0))
            .truncate()
            .show_ui(ui, |ui| {
                for binding in bindings {
                    let label = binding_label(binding, studio);
                    let label = if overlay.entry(binding).is_some() {
                        language
                            .text(&format!("{label} · loaded"), &format!("{label} · 已导入"))
                            .to_owned()
                    } else {
                        label
                    };
                    ui.selectable_value(&mut selected, binding, label)
                        .on_hover_text(binding_detail(binding, studio));
                }
            });
        if ui
            .add_sized(
                [button_width, ui.spacing().interact_size.y],
                egui::Button::new(language.text("Current scene", "当前场景")),
            )
            .clicked()
        {
            selected = current;
        }
    });
    if selected != overlay.selected_binding {
        overlay.select_binding(selected);
    }
}

fn import_controls(ui: &mut egui::Ui, overlay: &mut NavMeshOverlay, studio: &Studio) {
    let language = studio.prefs.language;
    ui.horizontal(|ui| {
        let button_width = 96.0;
        let input_width =
            (ui.available_width() - button_width - ui.spacing().item_spacing.x).max(80.0);
        ui.add_sized(
            [input_width, ui.spacing().interact_size.y],
            egui::TextEdit::singleline(&mut overlay.path_input)
                .hint_text(language.text("Detour NavMesh .bin path", "Detour NavMesh .bin 路径")),
        );
        if ui
            .add_enabled_ui(!overlay.picker.is_busy() && !overlay.is_loading(), |ui| {
                ui.add_sized(
                    [button_width, ui.spacing().interact_size.y],
                    egui::Button::new(language.text("Browse…", "浏览…")),
                )
            })
            .inner
            .clicked()
        {
            overlay.request_picker(studio, ui.ctx().clone());
        }
    });
    ui.horizontal_wrapped(|ui| {
        let has_entry = overlay.selected().is_some();
        let import_label = if has_entry {
            language.text("Replace this NavMesh", "替换此场景的 NavMesh")
        } else {
            language.text("Import NavMesh", "导入 NavMesh")
        };
        if ui
            .add_enabled(
                !overlay.path_input.trim().is_empty()
                    && !overlay.picker.is_busy()
                    && !overlay.is_loading(),
                egui::Button::new(import_label),
            )
            .clicked()
        {
            overlay.request_selected_import(
                PathBuf::from(overlay.path_input.trim()),
                studio,
                Some(ui.ctx().clone()),
            );
        }
        if ui
            .add_enabled(
                has_entry,
                egui::Button::new(language.text("Unload this overlay", "卸载此图层")),
            )
            .clicked()
        {
            overlay.unload();
        }
        if overlay.has_pending_work() {
            ui.spinner();
            let target = overlay
                .pending
                .as_ref()
                .map(|pending| pending.binding)
                .or(overlay.picker_binding);
            if let Some(target) = target {
                let target = binding_label(target, studio);
                ui.label(
                    language.text(&format!("Loading {target}…"), &format!("正在导入{target}…")),
                );
            } else {
                ui.label(language.text("Loading…", "正在导入…"));
            }
            if ui.button(language.text("Cancel", "取消")).clicked() {
                overlay.cancel_import();
            }
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
    });
    if let Some(message) = &overlay.message {
        ui.label(message.text(language));
    }
}

fn loaded_overlays(ui: &mut egui::Ui, overlay: &mut NavMeshOverlay, studio: &Studio) {
    if overlay.entries.is_empty() {
        return;
    }
    let language = studio.prefs.language;
    ui.separator();
    let count = overlay.entries.len();
    let title = language
        .text(
            &format!("Loaded overlays ({count})"),
            &format!("已导入图层（{count}）"),
        )
        .to_owned();
    let mut selected = overlay.selected_binding;
    egui::CollapsingHeader::new(title)
        .id_salt("navmesh-loaded-overlays")
        .default_open(true)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("navmesh-overlay-list")
                .max_height(108.0)
                .show(ui, |ui| {
                    for (index, entry) in overlay.entries.iter().enumerate() {
                        let loaded = &entry.loaded;
                        let filename = loaded
                            .path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy();
                        let label =
                            format!("{}  ·  {filename}", binding_label(loaded.binding, studio));
                        ui.push_id(index, |ui| {
                            if ui
                                .add(
                                    egui::Button::selectable(selected == loaded.binding, label)
                                        .truncate(),
                                )
                                .on_hover_text(format!(
                                    "{}\n{}",
                                    binding_detail(loaded.binding, studio),
                                    loaded.path.display()
                                ))
                                .clicked()
                            {
                                selected = loaded.binding;
                            }
                        });
                    }
                });
        });
    if selected != overlay.selected_binding {
        overlay.select_binding(selected);
    }
}

fn display_controls(ui: &mut egui::Ui, overlay: &mut NavMeshOverlay, studio: &Studio) {
    let language = studio.prefs.language;
    ui.separator();
    let Some(entry) = overlay.selected() else {
        ui.weak(language.text(
            "No NavMesh for this target yet. Importing here leaves other scenes unchanged.",
            "此目标尚未导入 NavMesh，导入后不会替换其他场景的网格。",
        ));
        return;
    };
    let loaded = &entry.loaded;
    ui.strong(language.text("Selected overlay", "当前图层"));
    ui.add(
        egui::Label::new(
            loaded
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
        )
        .truncate(),
    )
    .on_hover_text(loaded.path.display().to_string());
    ui.weak(language.text(
        &format!(
            "{} tiles · {} polygons · {} triangles",
            loaded.mesh.tile_count,
            loaded.mesh.polygon_count,
            loaded.mesh.indices.len() / 3
        ),
        &format!(
            "{} 个瓦片 · {} 个多边形 · {} 个三角形",
            loaded.mesh.tile_count,
            loaded.mesh.polygon_count,
            loaded.mesh.indices.len() / 3
        ),
    ));
    let can_focus = overlay.visible_for(studio);
    if entry.settings.visible && !can_focus {
        ui.weak(language.text(
            "Select the corresponding physics scene in the viewport to see this overlay.",
            "在视口切换到对应物理场景后，即可查看此图层。",
        ));
    }
    let selected = overlay.selected_binding;
    let mut focus = false;
    let settings = &mut overlay.selected_mut().unwrap().settings;
    ui.horizontal_wrapped(|ui| {
        ui.checkbox(&mut settings.visible, language.text("Show", "显示"));
        ui.checkbox(&mut settings.show_surface, language.text("Surface", "表面"));
        ui.checkbox(&mut settings.show_edges, language.text("Edges", "边线"));
        focus = ui
            .add_enabled(can_focus, egui::Button::new(language.text("Focus", "聚焦")))
            .clicked();
    });
    ui.horizontal(|ui| {
        ui.label(language.text("Opacity", "不透明度"));
        ui.spacing_mut().slider_width = (ui.available_width() - 70.0).max(80.0);
        ui.add(egui::Slider::new(&mut settings.opacity, 0.0..=1.0));
    });
    ui.collapsing(
        language.text("Alignment with physics", "与物理场景对齐"),
        |ui| alignment_controls(ui, &mut settings.alignment, language),
    );
    if focus {
        overlay.focus_requested = Some(selected);
    }
}

fn alignment_controls(
    ui: &mut egui::Ui,
    alignment: &mut Alignment,
    language: crate::i18n::Language,
) {
    ui.weak(language.text(
        "Adjust in physics source coordinates; viewport axis and unit settings also apply.",
        "在物理源坐标中调整，随后统一应用视口的坐标轴与单位设置。",
    ));
    egui::Grid::new("navmesh-alignment")
        .num_columns(4)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label(language.text("Offset", "位移"));
            for (axis, value) in ["X ", "Y ", "Z "].into_iter().zip(&mut alignment.offset) {
                ui.add(
                    egui::DragValue::new(value)
                        .speed(0.1)
                        .range(-1e9..=1e9)
                        .prefix(axis),
                );
            }
            ui.end_row();
            ui.label(language.text("Rotation", "旋转"));
            for (axis, value) in ["X ", "Y ", "Z "]
                .into_iter()
                .zip(&mut alignment.rotation_degrees)
            {
                ui.add(
                    egui::DragValue::new(value)
                        .speed(1.0)
                        .range(-360.0..=360.0)
                        .prefix(axis)
                        .suffix("°"),
                );
            }
            ui.end_row();
        });
    ui.horizontal_wrapped(|ui| {
        ui.label(language.text("Scale", "缩放"));
        ui.add(
            egui::DragValue::new(&mut alignment.scale)
                .speed(0.01)
                .range(1e-6..=1e6),
        );
        ui.checkbox(&mut alignment.flip_x, language.text("Mirror X", "镜像 X"));
        if ui
            .button(language.text("Reset alignment", "重置对齐"))
            .clicked()
        {
            *alignment = Alignment::default();
        }
    });
    *alignment = alignment.sanitized();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(context: &egui::Context, size: egui::Vec2, mut draw: impl FnMut(&egui::Context)) {
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                ..Default::default()
            },
            |context| draw(context),
        );
    }

    fn window_rect(context: &egui::Context) -> egui::Rect {
        context.memory(|memory| memory.area_rect(egui::Id::new(WINDOW_ID)).unwrap())
    }

    #[test]
    fn reopening_recenters_saved_window_position_without_pinning_an_open_window() {
        let context = egui::Context::default();
        let size = egui::vec2(1440.0, 960.0);
        let studio = Studio::default();
        let mut overlay = NavMeshOverlay {
            open: true,
            ..Default::default()
        };
        frame(&context, size, |context| {
            egui::Window::new("Old placement")
                .id(egui::Id::new(WINDOW_ID))
                .current_pos(egui::pos2(20.0, 80.0))
                .show(context, |ui| {
                    ui.label("Saved left-hand placement");
                });
        });
        for _ in 0..3 {
            frame(&context, size, |context| {
                window(context, &mut overlay, &studio)
            });
        }
        assert!((window_rect(&context).center().x - size.x / 2.0).abs() <= 1.0);
        frame(&context, size, |context| {
            egui::Window::new("Moved placement")
                .id(egui::Id::new(WINDOW_ID))
                .pivot(egui::Align2::CENTER_CENTER)
                .current_pos(egui::pos2(450.0, 480.0))
                .show(context, |ui| panel(ui, &mut overlay, &studio));
        });
        for _ in 0..2 {
            frame(&context, size, |context| {
                window(context, &mut overlay, &studio)
            });
        }
        assert!((window_rect(&context).center().x - 450.0).abs() <= 1.0);
        overlay.open = false;
        frame(&context, size, |context| {
            window(context, &mut overlay, &studio)
        });
        overlay.open = true;
        for _ in 0..2 {
            frame(&context, size, |context| {
                window(context, &mut overlay, &studio)
            });
        }
        assert!((window_rect(&context).center().x - size.x / 2.0).abs() <= 1.0);
    }

    #[test]
    fn long_paths_and_many_overlays_fit_small_windows_in_both_languages() {
        let mesh = Arc::new(
            pvd_core::navmesh::parse(
                include_bytes!("../../../pvd-core/src/navmesh/fixtures/recast-0.43.1.bin"),
                || false,
            )
            .unwrap(),
        );
        for language in [
            crate::i18n::Language::Chinese,
            crate::i18n::Language::English,
        ] {
            for size in [egui::vec2(640.0, 480.0), egui::vec2(1024.0, 720.0)] {
                let context = egui::Context::default();
                let mut studio = Studio::default();
                studio.prefs.language = language;
                let mut overlay = NavMeshOverlay {
                    open: true,
                    ..Default::default()
                };
                for id in 0..12 {
                    overlay.entries.push(OverlayEntry {
                        loaded: LoadedNavMesh {
                            mesh: mesh.clone(),
                            path: format!(
                                "C:/maps/{}/navigation.bin",
                                "very-long-map-name".repeat(16)
                            )
                            .into(),
                            binding: if id == 0 {
                                OverlayBinding::Global
                            } else {
                                OverlayBinding::Room {
                                    epoch: studio.room_session_epoch(),
                                    room: FrameObject { id, generation: 0 },
                                }
                            },
                        },
                        settings: OverlaySettings::default(),
                        revision: id,
                    });
                }
                overlay.path_input = overlay.entries[0].loaded.path.display().to_string();
                for _ in 0..4 {
                    frame(&context, size, |context| {
                        window(context, &mut overlay, &studio)
                    });
                }
                let rect = window_rect(&context);
                assert!(rect.left() >= 0.0 && rect.top() >= 0.0, "{rect:?}");
                assert!(
                    rect.right() <= size.x && rect.bottom() <= size.y,
                    "{rect:?}"
                );
                assert!((rect.center().x - size.x / 2.0).abs() <= 1.0, "{rect:?}");
            }
        }
    }
}
