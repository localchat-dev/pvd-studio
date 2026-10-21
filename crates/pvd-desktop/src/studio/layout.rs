use egui_dock::{DockState, Node, NodeIndex, Surface, SurfaceIndex};

use super::{Panel, Preferences};

const PANELS: [Panel; 10] = [
    Panel::Objects,
    Panel::Viewport,
    Panel::Inspector,
    Panel::Timeline,
    Panel::Clips,
    Panel::Performance,
    Panel::Memory,
    Panel::Curves,
    Panel::Diagnostics,
    Panel::Display,
];

fn validate_structure(dock: &DockState<Panel>) -> Result<(), &'static str> {
    if !matches!(
        dock.get_surface(SurfaceIndex::main()),
        Some(Surface::Main(_))
    ) {
        return Err("dock layout has no main surface");
    }
    for surface in dock.iter_surfaces() {
        let Some(tree) = surface.node_tree() else {
            continue;
        };
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
                    if (!leaf.tabs.is_empty() && leaf.active.0 >= leaf.tabs.len())
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
    if PANELS.iter().any(|panel| dock.find_tab(panel).is_none()) {
        return Err("dock layout is missing required panels");
    }
    Ok(())
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
    if validate_structure(dock).is_err() || dock.iter_all_tabs().next().is_none() {
        *dock = Preferences::default().dock;
        return;
    }
    for panel in PANELS {
        if dock.find_tab(&panel).is_none() {
            dock.main_surface_mut().push_to_first_leaf(panel);
        }
    }
}

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
    fn incomplete_layout_keeps_existing_panel_positions_and_split() {
        let mut dock = DockState::new(vec![Panel::Viewport]);
        dock.main_surface_mut()
            .split_left(NodeIndex::root(), 0.31, vec![Panel::Objects]);
        let viewport = dock.find_tab(&Panel::Viewport).unwrap();
        let objects = dock.find_tab(&Panel::Objects).unwrap();
        repair(&mut dock);
        validate(&dock).unwrap();
        assert_eq!(dock.find_tab(&Panel::Viewport), Some(viewport));
        assert_eq!(dock.find_tab(&Panel::Objects), Some(objects));
        let Node::Horizontal(split) = &dock.main_surface()[NodeIndex::root()] else {
            panic!("expected preserved split")
        };
        assert_eq!(split.fraction, 0.31);
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
