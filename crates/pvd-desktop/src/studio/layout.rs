use egui_dock::{DockState, Node, NodeIndex, Surface, SurfaceIndex};

use super::{Panel, Preferences};

fn validate_structure(dock: &DockState<Panel>) -> Result<(), &'static str> {
    if !matches!(
        dock.get_surface(SurfaceIndex::main()),
        Some(Surface::Main(_))
    ) {
        return Err("dock layout has no main surface");
    }
    for (surface_index, surface) in dock.iter_surfaces().enumerate() {
        if surface_index > 0 && matches!(surface, Surface::Main(_)) {
            return Err("dock floating surface has no window state");
        }
        let Some(tree) = surface.node_tree() else {
            continue;
        };
        if matches!(surface, Surface::Window(_, _)) && tree.is_empty() {
            return Err("dock floating surface is empty");
        }
        for (index, node) in tree.iter().enumerate() {
            if node.is_empty() {
                continue;
            }
            if index > 0 && !tree[NodeIndex((index - 1) / 2)].is_parent() {
                return Err("dock node has no split parent");
            }
            match node {
                Node::Horizontal(split) | Node::Vertical(split) => {
                    if !split.fraction.is_finite() || !(0.0..=1.0).contains(&split.fraction) {
                        return Err("dock split fraction is invalid");
                    }
                    let right = index.saturating_mul(2).saturating_add(2);
                    if right >= tree.len()
                        || tree[NodeIndex(right - 1)].is_empty()
                        || tree[NodeIndex(right)].is_empty()
                    {
                        return Err("dock split has missing children");
                    }
                }
                Node::Leaf(leaf) => {
                    if leaf.tabs.is_empty()
                        || leaf.active.0 >= leaf.tabs.len()
                        || !leaf.scroll.is_finite()
                    {
                        return Err("dock leaf state is invalid");
                    }
                }
                Node::Empty => {}
            }
        }
        if let Some(focused) = tree.focused_leaf()
            && (focused.0 >= tree.len() || !tree[focused].is_leaf())
        {
            return Err("dock focused node is invalid");
        }
    }
    Ok(())
}

pub(super) fn validate(dock: &DockState<Panel>) -> Result<(), &'static str> {
    validate_structure(dock)?;
    if dock.find_tab(&Panel::Viewport).is_none() {
        return Err("dock layout is missing the scene panel");
    }
    if has_duplicates(dock) {
        return Err("dock layout contains duplicate panels");
    }
    Ok(())
}

fn has_duplicates(dock: &DockState<Panel>) -> bool {
    Panel::ALL.iter().any(|panel| {
        dock.iter_all_tabs()
            .filter(|(_, tab)| *tab == panel)
            .take(2)
            .count()
            > 1
    })
}

pub(super) fn show_panel(dock: &mut DockState<Panel>, panel: Panel) {
    if dock.find_tab(&panel).is_none() {
        let neighbor = match panel {
            Panel::Inspector => Panel::Display,
            Panel::Display => Panel::Inspector,
            Panel::Objects | Panel::Viewport => Panel::Viewport,
            _ => Panel::Timeline,
        };
        if let Some((surface, node, _)) = dock.find_tab(&neighbor) {
            dock[surface][node].append_tab(panel);
        } else {
            dock.main_surface_mut().push_to_first_leaf(panel);
        }
    }
    if let Some(location) = dock.find_tab(&panel) {
        dock.reveal_tab(location);
    }
}

pub(super) fn repair(dock: &mut DockState<Panel>) {
    for (_, node) in dock.iter_all_nodes_mut() {
        match node {
            Node::Horizontal(split) | Node::Vertical(split) => {
                if !split.fraction.is_finite() || !(0.0..=1.0).contains(&split.fraction) {
                    split.fraction = 0.5;
                }
            }
            Node::Leaf(leaf) => {
                if leaf.active.0 >= leaf.tabs.len() {
                    leaf.active.0 = 0;
                }
                if !leaf.scroll.is_finite() {
                    leaf.scroll = 0.0;
                }
            }
            Node::Empty => {}
        }
    }
    if validate_structure(dock).is_err()
        || dock.iter_all_tabs().next().is_none()
        || has_duplicates(dock)
    {
        *dock = Preferences::default().dock;
        return;
    }
    if dock.find_tab(&Panel::Viewport).is_none() {
        show_panel(dock, Panel::Viewport);
    }
}

#[cfg(test)]
mod interactions;

#[cfg(test)]
mod tests {
    use super::*;

    struct Viewer;

    impl egui_dock::TabViewer for Viewer {
        type Tab = Panel;

        fn title(&mut self, tab: &mut Panel) -> egui::WidgetText {
            format!("{tab:?}").into()
        }

        fn ui(&mut self, _: &mut egui::Ui, _: &mut Panel) {}
    }

    fn paint(dock: &mut DockState<Panel>, context: &egui::Context, size: egui::Vec2) {
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(context, |ui| {
                        egui_dock::DockArea::new(dock).show_inside(ui, &mut Viewer);
                    });
            },
        );
    }

    fn finite_splits(dock: &DockState<Panel>) -> bool {
        dock.iter_all_nodes().all(|(_, node)| match node {
            Node::Horizontal(split) | Node::Vertical(split) => split.fraction.is_finite(),
            _ => true,
        })
    }

    #[test]
    fn zero_sized_dock_restores_without_poisoning_split_fractions() {
        for zero_size in [
            egui::vec2(0.0, 800.0),
            egui::vec2(1000.0, 0.0),
            egui::Vec2::ZERO,
        ] {
            let mut dock = Preferences::default().dock;
            let context = egui::Context::default();
            for size in [
                egui::vec2(1200.0, 800.0),
                zero_size,
                egui::vec2(1200.0, 800.0),
            ] {
                paint(&mut dock, &context, size);
                assert!(finite_splits(&dock), "non-finite fractions at {size:?}");
            }
            validate(&dock).unwrap();
        }
    }

    #[test]
    fn positive_outer_rect_with_zero_nested_split_stays_finite() {
        let mut dock = DockState::new(vec![Panel::Viewport]);
        let [right, _] =
            dock.main_surface_mut()
                .split_left(NodeIndex::root(), 0.5, vec![Panel::Objects]);
        let [_, left] = dock
            .main_surface_mut()
            .split_left(right, 0.0, vec![Panel::Inspector]);
        dock.main_surface_mut()
            .split_left(left, 0.5, vec![Panel::Display]);
        let context = egui::Context::default();
        for _ in 0..3 {
            paint(&mut dock, &context, egui::vec2(1200.0, 800.0));
            assert!(finite_splits(&dock));
        }
    }

    #[test]
    fn empty_saved_layout_recovers_panels_and_keeps_other_preferences() {
        let mut value = serde_json::to_value(Preferences::default()).unwrap();
        let mut empty_dock = DockState::<Panel>::new(vec![]);
        for (_, node) in empty_dock.iter_all_nodes_mut() {
            node.set_rect(egui::Rect::ZERO);
            if let Some(leaf) = node.get_leaf_mut() {
                leaf.viewport = egui::Rect::ZERO;
            }
        }
        value["dock"] = serde_json::to_value(empty_dock).unwrap();
        value["language"] = "en".into();
        value["listen_host"] = "192.168.1.20".into();
        value["listen_port"] = 15447.into();
        let prefs = Preferences::from_bytes(&serde_json::to_vec(&value).unwrap());
        validate(&prefs.dock).unwrap();
        assert_eq!(prefs.language, crate::i18n::Language::English);
        let restored = serde_json::to_value(prefs).unwrap();
        assert_eq!(restored["listen_host"], "192.168.1.20");
        assert_eq!(restored["listen_port"], 15447);
    }

    #[test]
    fn hidden_panels_keep_existing_panel_positions_and_split() {
        let mut dock = DockState::new(vec![Panel::Viewport]);
        dock.main_surface_mut()
            .split_left(NodeIndex::root(), 0.31, vec![Panel::Objects]);
        let viewport = dock.find_tab(&Panel::Viewport).unwrap();
        let objects = dock.find_tab(&Panel::Objects).unwrap();
        repair(&mut dock);
        validate(&dock).unwrap();
        assert_eq!(dock.find_tab(&Panel::Viewport), Some(viewport));
        assert_eq!(dock.find_tab(&Panel::Objects), Some(objects));
        assert!(dock.find_tab(&Panel::Memory).is_none());
        let Node::Horizontal(split) = &dock.main_surface()[NodeIndex::root()] else {
            panic!("expected preserved split")
        };
        assert_eq!(split.fraction, 0.31);
    }

    #[test]
    fn hidden_panels_survive_preferences_and_reopen_without_duplicates() {
        let mut prefs = Preferences::default();
        for panel in Panel::ALL {
            prefs.hide_panel(panel);
        }
        assert_eq!(prefs.dock.iter_all_tabs().count(), 1);
        validate(&prefs.dock).unwrap();
        let mut restored = Preferences::from_bytes(&serde_json::to_vec(&prefs).unwrap());
        assert_eq!(restored.dock.iter_all_tabs().count(), 1);
        for panel in Panel::ALL {
            restored.show_panel(panel);
            restored.show_panel(panel);
            validate(&restored.dock).unwrap();
        }
        assert_eq!(restored.dock.iter_all_tabs().count(), Panel::ALL.len());
    }

    #[test]
    fn duplicate_panels_cannot_share_viewport_or_input_state() {
        let mut prefs = Preferences::default();
        prefs
            .dock
            .main_surface_mut()
            .push_to_first_leaf(Panel::Viewport);
        assert!(validate(&prefs.dock).is_err());
        assert!(serde_json::to_vec(&prefs).is_err());
        repair(&mut prefs.dock);
        validate(&prefs.dock).unwrap();
        assert_eq!(prefs.dock.iter_all_tabs().count(), Panel::ALL.len());
    }

    #[test]
    fn invalid_saved_surface_focus_does_not_reach_dock_indexing() {
        let mut value = serde_json::to_value(Preferences::default()).unwrap();
        value["dock"]["focused_surface"] = 999.into();
        let mut prefs = Preferences::from_bytes(&serde_json::to_vec(&value).unwrap());
        assert_eq!(prefs.dock.focused_leaf(), None);
        assert!(prefs.dock.find_active_focused().is_none());
        paint(
            &mut prefs.dock,
            &egui::Context::default(),
            egui::vec2(1200.0, 800.0),
        );
        validate(&prefs.dock).unwrap();
        prefs.show_panel(Panel::Memory);
        assert!(prefs.dock.focused_leaf().is_some());
    }

    #[test]
    fn malformed_floating_surfaces_restore_a_paintable_layout() {
        for empty_leaf in [true, false] {
            let mut prefs = Preferences::default();
            let location = prefs.dock.find_tab(&Panel::Memory).unwrap();
            let surface = prefs.dock.detach_tab(
                location,
                egui::Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(600.0, 400.0)),
            );
            let mut value = serde_json::to_value(&prefs).unwrap();
            let floating = &mut value["dock"]["surfaces"][surface.0];
            if empty_leaf {
                floating["Window"][0]["nodes"][0]["Leaf"]["tabs"] = serde_json::json!([]);
            } else {
                *floating = serde_json::json!({"Main": floating["Window"][0].clone()});
            }
            value["listen_port"] = 15447.into();
            let mut restored = Preferences::from_bytes(&serde_json::to_vec(&value).unwrap());
            validate(&restored.dock).unwrap();
            assert_eq!(restored.dock.iter_all_tabs().count(), Panel::ALL.len());
            assert_eq!(restored.listener.port, 15447);
            paint(
                &mut restored.dock,
                &egui::Context::default(),
                egui::vec2(1200.0, 800.0),
            );
        }
    }

    #[test]
    fn floating_split_reorder_and_close_preserve_remaining_layout() {
        use egui_dock::{Split, TabIndex, TabInsert};

        let mut prefs = Preferences::default();
        let memory = prefs.dock.find_tab(&Panel::Memory).unwrap();
        let surface = prefs.dock.detach_tab(
            memory,
            egui::Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(600.0, 400.0)),
        );
        let curves = prefs.dock.find_tab(&Panel::Curves).unwrap();
        prefs.dock.move_tab(
            curves,
            (surface, NodeIndex::root(), TabInsert::Split(Split::Right)),
        );
        validate(&prefs.dock).unwrap();
        let timeline = prefs.dock.find_tab(&Panel::Timeline).unwrap();
        let diagnostics = prefs.dock.find_tab(&Panel::Diagnostics).unwrap();
        prefs.dock.move_tab(
            diagnostics,
            (timeline.0, timeline.1, TabInsert::Insert(TabIndex(0))),
        );
        validate(&prefs.dock).unwrap();
        let objects = prefs.dock.find_tab(&Panel::Objects);
        let mut restored = Preferences::from_bytes(&serde_json::to_vec(&prefs).unwrap());
        assert_eq!(restored.dock.find_tab(&Panel::Memory).unwrap().0, surface);
        assert_eq!(restored.dock.find_tab(&Panel::Curves).unwrap().0, surface);
        assert_eq!(
            restored.dock.find_tab(&Panel::Diagnostics).unwrap().2,
            TabIndex(0)
        );
        restored.hide_panel(Panel::Memory);
        restored.hide_panel(Panel::Curves);
        validate(&restored.dock).unwrap();
        assert!(!restored.dock.is_surface_valid(surface));
        assert_eq!(restored.dock.find_tab(&Panel::Objects), objects);
        restored.show_panel(Panel::Curves);
        validate(&restored.dock).unwrap();
        assert!(restored.dock.find_tab(&Panel::Memory).is_none());
    }

    #[test]
    fn requested_tab_expands_collapsed_floating_group() {
        let mut prefs = Preferences::default();
        let memory = prefs.dock.find_tab(&Panel::Memory).unwrap();
        let surface = prefs.dock.detach_tab(
            memory,
            egui::Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(600.0, 400.0)),
        );
        let mut value = serde_json::to_value(&prefs).unwrap();
        value["dock"]["surfaces"][surface.0]["Window"][1]["minimized"] = true.into();
        let mut restored = Preferences::from_bytes(&serde_json::to_vec(&value).unwrap());
        let location = restored.dock.find_tab(&Panel::Memory).unwrap();
        restored.dock[location.0][location.1].set_collapsed(true);
        restored.show_panel(Panel::Memory);
        assert!(!restored.dock[location.0][location.1].is_collapsed());
        let state = serde_json::to_value(&restored).unwrap();
        assert_eq!(
            state["dock"]["surfaces"][surface.0]["Window"][1]["minimized"],
            false
        );
        assert_eq!(restored.dock.focused_leaf(), Some((location.0, location.1)));
    }

    #[test]
    fn invalid_fraction_is_repaired_without_resetting_valid_panel_positions() {
        let mut dock = Preferences::default().dock;
        let viewport = dock.find_tab(&Panel::Viewport);
        let Node::Horizontal(split) = &mut dock.main_surface_mut()[NodeIndex::root()] else {
            panic!("expected horizontal root")
        };
        split.fraction = f32::NAN;
        assert!(validate(&dock).is_err());
        repair(&mut dock);
        validate(&dock).unwrap();
        assert_eq!(dock.find_tab(&Panel::Viewport), viewport);
    }

    #[test]
    fn malformed_saved_dock_keeps_language_and_listener_preferences() {
        let mut value = serde_json::to_value(Preferences::default()).unwrap();
        value["dock"]["surfaces"][0]["Main"]["nodes"][0]["Horizontal"]["fraction"] =
            serde_json::Value::Null;
        value["language"] = "en".into();
        value["listen_port"] = 15447.into();
        let prefs = Preferences::from_bytes(&serde_json::to_vec(&value).unwrap());
        validate(&prefs.dock).unwrap();
        assert_eq!(prefs.language, crate::i18n::Language::English);
        assert_eq!(serde_json::to_value(prefs).unwrap()["listen_port"], 15447);
    }

    #[test]
    fn invalid_layout_cannot_overwrite_valid_preferences() {
        let path = std::env::temp_dir().join(format!(
            "pvd-studio-layout-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut prefs = Preferences::default();
        prefs.save_to(&path).unwrap();
        let valid_bytes = std::fs::read(&path).unwrap();
        prefs.dock = DockState::new(vec![]);
        assert!(prefs.save_to(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), valid_bytes);
        std::fs::remove_file(path).unwrap();
    }
}
