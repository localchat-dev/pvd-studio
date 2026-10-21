# Windows monitor discovery patch

This directory contains the published `bevy_winit` 0.18.1 source, with a project-local Windows monitor discovery patch. It is selected by the workspace `[patch.crates-io]` entry and excluded from workspace membership. Global Cargo registry files and Windows display settings are not modified.

## Provenance and licenses

- Upstream: <https://github.com/bevyengine/bevy/tree/v0.18.1/crates/bevy_winit>
- Published package: <https://crates.io/crates/bevy_winit/0.18.1>
- Package archive SHA-256: `f2de1c13d32ab8528435b58eca7ab874a1068184c6d6f266ee11433ae99d4069`.
- Cargo VCS metadata: commit `f667c282dad2c1419afb5836ded22a3ec263970e`, path `crates/bevy_winit`; `.cargo_vcs_info.json` is retained.
- The complete upstream MIT and Apache-2.0 licenses are retained as `LICENSE-MIT` and `LICENSE-APACHE`.
- `Cargo.toml.orig` and the upstream README are retained without changes. The normalized `Cargo.toml` is the build manifest.

## Observed failure and boundary

The Windows application encountered a winit 0.30.13 `MonitorHandle::size` panic: `GetMonitorInfoW` returned error 1461 (invalid monitor handle), and the result was unwrapped during Bevy monitor discovery.

Microsoft documents that display changes can invalidate an `HMONITOR`, and that enumeration also includes invisible monitors associated with mirroring drivers. The observed error does not establish which condition produced this specific handle.

- <https://learn.microsoft.com/en-us/windows/win32/gdi/hmonitor-and-the-device-context>
- <https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-enumdisplaymonitors>

Winit 0.30.13 is the stable release compatible with the locked Bevy dependency; the available 0.31 release is a prerelease with API changes. This patch does not switch to an upstream development branch or change either version.

## Local changes

- `Cargo.toml` adds Windows-only `windows-sys = 0.52.0`, already present in the workspace lockfile for winit, with GDI and DPI features.
- `src/lib.rs` confines the Windows FFI allowance to `windows_monitor`; unsafe code remains denied in the rest of the crate.
- `src/windows_monitor.rs` obtains geometry and the device name from one successful `MONITORINFOEXW` query. It queries DPI with a checked HRESULT, then reads current and available modes using that captured device name. It never calls winit's monitor field getters. Windows does not provide an atomic transaction across geometry, DPI, and mode APIs; subsequent failures are handled instead of assuming handle lifetime. Invalid geometry, missing mode fields, arithmetic overflow, and unbounded mode enumeration are rejected. A zero result while enumerating indexed modes is the API's normal end-of-list condition.
- `src/system.rs` skips a failed monitor and emits a diagnostic. Known monitors are validated again before being marked present; invalid known monitors are removed through the existing removal path. Expensive mode discovery runs only when registering a new monitor. Valid monitors follow the upstream registration and primary-monitor behavior; updating metadata of an already registered monitor remains outside this patch.
- `src/winit_monitors.rs` remembers only the failed handles from the preceding enumeration, suppressing repeated warnings for an unchanged failure while retrying every event-loop enumeration. Recovered handles can be registered on a later iteration.

No window creation, fullscreen, input, rendering, close handling, layout, or application preference logic is changed. This addresses the observed discovery panic; it is not a general fix for every possible winit monitor-related operation.

## Regression checks

From the workspace root:

```powershell
cargo test --locked --offline -p bevy_winit --lib windows_monitor::tests
cargo test --locked --offline -p bevy_winit --lib windows_monitor::tests -- --include-ignored
cargo build --locked --offline -p pvd-desktop
```

The five ordinary tests cover real invalid Win32 handles, a handle failing after a valid metadata snapshot, physical geometry at negative desktop coordinates, malformed monitor metadata, and mode field/overflow validation. The explicit ignored test performs read-only queries on the actual primary monitor and requires an attached Windows desktop. It does not create a window or change display state.

Native application startup must also be checked on the machine that produced error 1461. Headless rendering bypasses this discovery path and alone cannot validate this fix. Monitor hot-unplug and remote-session changes remain separate manual coverage; no automated test changes system display configuration.
