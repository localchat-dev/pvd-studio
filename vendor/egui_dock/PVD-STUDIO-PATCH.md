# Finite separator geometry patch

This directory contains the published `egui_dock` 0.18.0 source and a project-local separator guard. The workspace selects it through `[patch.crates-io]` and excludes it from workspace membership. The locked egui 0.33.3 integration is retained; global Cargo registry files are unchanged.

## Provenance and license

- Upstream: <https://github.com/anhosh/egui_dock>
- Published package: <https://crates.io/crates/egui_dock/0.18.0>
- Archive SHA-256: `2fd0f689738efbd4afb811433b873379c6a8f3af48f2d296d9849fe70ee021d6`.
- Cargo VCS commit: `594b6c999a101d691b48592a70b0126c88f6a5a7`; `.cargo_vcs_info.json` is retained.
- The complete upstream MIT license is retained as `LICENSE`; original manifests and README are unchanged.

## Failure and fix

`DockArea::show_separator` divides the pointer delta by the split's extent on every frame. With a zero-width horizontal split or zero-height vertical split, even an idle pointer produces `0 / 0`. The resulting NaN persists in the split fraction and contaminates subsequent separator rectangles. A positive-sized outer window does not prevent an inner split from reaching zero width after nesting and clipping.

The local change is confined to `src/widgets/dock_area/show/mod.rs`: a non-finite rectangle or non-positive/non-finite split extent skips separator interaction before any division; a non-finite input delta cannot overwrite the fraction. Finite, positive extents retain the upstream pointer, keyboard, and double-click behavior. This fixes the arithmetic source, rather than repairing NaN on a subsequent frame. It does not install a panic catcher or require changing the user's display settings.

The matching pvd-studio integration draws a cloned layout and commits it only after a successful, valid layout pass. Preference serialization rejects invalid/incomplete layouts before touching the destination file, and panic unwinding does not save preferences. On load, missing panels are added without changing valid existing panel positions; a fully empty or structurally invalid layout receives the default layout while retaining other valid preferences, such as language and listener settings. No tool edits the user's saved layout file.

## Reproducible regression evidence

Run from the workspace root:

```powershell
cargo test --locked --offline -p pvd-desktop studio::layout::tests -- --nocapture
cargo build --locked --offline -p pvd-desktop
```

Tests use actual `egui::Context` layout passes without an OS window or GPU. The zero-size sequence covers normal layout, zero width, zero height or both, and restored size. A separate test creates a zero-width nested split inside a 1200-by-800-point outer rectangle.

Compare the published upstream geometry tests with the patched dependency; both zero-size cases must reject non-finite fractions. Preference tests separately cover invalid layouts, missing panels and save protection.

The arithmetic regression is reproducible, but the exact window/panel event sequence preceding the native panic was not captured. Minimize/restore and docking interactions require separate native UI coverage.