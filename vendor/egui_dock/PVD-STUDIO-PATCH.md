# Dock geometry and panel visibility

This directory contains the published `egui_dock` 0.18.0 source with project-local separator and panel-visibility support. The workspace selects it through `[patch.crates-io]` and excludes it from workspace membership. The locked egui 0.33.3 integration is retained; global Cargo registry files are unchanged.

## Provenance and license

- Upstream: <https://github.com/anhosh/egui_dock>
- Published package: <https://crates.io/crates/egui_dock/0.18.0>
- Archive SHA-256: `2fd0f689738efbd4afb811433b873379c6a8f3af48f2d296d9849fe70ee021d6`.
- Cargo VCS commit: `594b6c999a101d691b48592a70b0126c88f6a5a7`; `.cargo_vcs_info.json` is retained.
- The complete upstream MIT license is retained as `LICENSE`; original manifests and README are unchanged.

## Failure and fix

`DockArea::show_separator` divides the pointer delta by the split's extent on every frame. With a zero-width horizontal split or zero-height vertical split, even an idle pointer produces `0 / 0`. The resulting NaN persists in the split fraction and contaminates subsequent separator rectangles. A positive-sized outer window does not prevent an inner split from reaching zero width after nesting and clipping.

In `src/widgets/dock_area/show/mod.rs`, a non-finite rectangle or non-positive/non-finite split extent skips separator interaction before any division; a non-finite input delta cannot overwrite the fraction. Finite, positive extents retain the upstream pointer, keyboard, and double-click behavior.

In `src/widgets/dock_area/show/leaf.rs`, the context-menu close action follows `TabViewer::is_closeable` independently of the tab-bar cross. `DockState::reveal_tab` activates a tab, expands its group and ancestors, and restores and raises its floating group inside the application. Saved focus indices outside the surface list are treated as absent.

The pvd-studio integration draws a cloned layout and commits it only after a valid layout pass. Preference serialization rejects invalid structure, duplicate panels or a missing Scene panel before touching the destination file, and panic unwinding does not save preferences. Optional panels may remain hidden. An empty, duplicated or structurally invalid layout receives the default layout while retaining other valid preferences, such as language and listener settings.

## Reproducible regression evidence

Run from the workspace root:

```powershell
cargo test --locked --offline -p pvd-desktop studio::layout::tests -- --nocapture
cargo build --locked --offline -p pvd-desktop
```

Tests use actual `egui::Context` layout passes without an OS window or GPU. They cover zero-size restoration, nested zero-width splits, pointer-driven tab reordering and context-menu closure. Preference tests cover hidden panels, floating split groups, reopening, duplicate rejection and save protection.

These checks do not establish OS focus, dynamic DPI or native window-manager behavior.
