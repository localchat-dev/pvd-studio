# pvd-studio

**English** | [简体中文](README.zh-CN.md)

Windows desktop tool for PhysX data capture, playback, and scene inspection, built on Rust, Bevy, and egui, following the features and workflow of NVIDIA PhysX Visual Debugger 3 (PVD3).

Primary validation target is PhysX 3.4.2, with partial PhysX 4.1.2 support. Recording formats and the supported feature range are listed in the [compatibility matrix](docs/compatibility.md).

> Documents linked below are currently written in Chinese.

## Features

- TCP receive, raw recording, and rolling on-disk history.
- Live follow, pause, frame stepping, random seek, forward and reverse playback.
- Object tree, property, array, and reference inspection, with scene selection linkage.
- Rigid body, mesh, heightfield, joint, contact, and scene query display; CPU cloth and particle display have separate scope limits.
- Thread performance, event curves, memory, and error analysis, with CSV export.
- SDK stream trimming, project capture save and reopen, limited original container saving, and PhysX 3.4.2 RepX export.

Display state comes from captures; the viewer does not re-run the physics simulation. PVD3 database v1 supports reading a subset of layouts, whole-file saving, and range trimming; broader reconstruction of earlier state and round-trip verification against the original application are not yet complete. Details are in [Saving and trimming original containers](docs/native-export.md).

## Build and run

Rust and the Windows MSVC C++ toolchain are required. The Rust version is pinned by `rust-toolchain.toml` and dependencies by `Cargo.lock`; running the viewer does not require the PhysX SDK.

```powershell
cargo run --locked -p pvd-desktop
cargo run --locked -p pvd-desktop -- --language en
```

After startup, open a capture file or click "Listen". The default bind is `0.0.0.0:5425`; on the SDK side use the actual IP reachable from the debug machine and the same port. Simplified Chinese, English, and connection settings can be switched in the program.

Captures and the SDK are not distributed with the source. Test inputs can be generated following the [SDK producer instructions](tools/physx-sample/README.md); desktop operation is covered in the [Windows usage notes](docs/distribution.md).

## Package the Unity plugin

The plugin receives and plays back PVD sessions in the Scene view of the Windows Unity Editor. On a build machine with Rust and the Windows MSVC toolchain, run this command from the repository root.

```powershell
./tools/package-unity.ps1
```

The script builds `pvd_unity.dll` for Windows x64 and creates `output/unity/<timestamp>/com.pvdstudio.viewer-<version>.tgz`. The unpacked `package/` directory sits alongside the archive. Use `-OutputDirectory` to choose a different output directory.

In Unity's Package Manager, choose **Add package from tarball** and open the generated `.tgz`. Rust is required only on the build machine. For installation, listening, and cache details, see the [Unity plugin guide](docs/unity.md).

## Files and storage

| Data | Default location and purpose |
| --- | --- |
| Playback history | `pvd-cache/history/` under the current working directory; default rolling cap 8 GiB, configurable |
| Receive cache | `receive/` under the history directory; independent of playback history and guarded by available space |
| Permanent recording | `captures/`; a complete SDK raw stream requires enabling automatic recording before listening |
| Project capture | Unity and the desktop app save retained history as a single `.pvdcapture`. Long recordings are [segmented automatically](docs/retained-capture.md#分段导入与导出) inside the file and play back continuously on the desktop |

Pausing does not stop receiving or history writes. Temporary history evicts old frames and cannot replace permanent recording. Saves go to `captures/` in the current working directory by default, and another location can be chosen; existing files are not overwritten.

The SDK communication stream, PVD3 databases, and project captures are distinct formats and cannot be exchanged by changing the file extension. Original databases read events on demand and use on-disk history; saving and trimming have separate limits, as detailed in the compatibility matrix.

## Command line

After generating inputs, run from the repository root:

```powershell
cargo run --locked -p pvd-cli -- inspect captures/physx-3.4.pvdstream
cargo run --locked -p pvd-cli -- verify captures/physx-3.4.pvdstream
cargo run --locked -p pvd-cli -- listen 0.0.0.0:5425 captures/tcp 60
```

Other query, trim, and export commands are listed in the [Windows usage notes](docs/distribution.md).

## Development

```powershell
cargo fmt --all --check
cargo test --locked -p pvd-protocol -p pvd-core -p pvd-transport -p pvd-cli
cargo test --locked -p pvd-desktop -- --test-threads=1
```

Real SDK checks require generating inputs and explicitly running the ignored tests, see the [validation notes](docs/validation.md). Windows distribution packages are produced with `tools/package-windows.ps1`; build and compression options are in the [packaging spec](docs/packaging.md).

| Document | Content |
| --- | --- |
| [Usage notes](docs/distribution.md) | Connection, recording, playback, and analysis |
| [Documentation index](docs/README.md) | Design and implementation topics |
| [Architecture](docs/architecture.md) | Module responsibilities, data paths, and resource boundaries |
| [Feature checklist](docs/manual-checklist.md) | Implementation and acceptance status |
| [Roadmap](docs/roadmap.md) | Remaining work and completion criteria |

The repository keeps source, tests, tools, docs, and dependency patch licenses; build artifacts, the SDK, captures, and runtime caches are excluded by `.gitignore`.
