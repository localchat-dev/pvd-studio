# PhysX 3.4 geometry and debugging fixtures

These console producers use the pinned, built official SDK checkout from `../physx-sample/build.ps1`. They neither open a native window nor modify the SDK installation. Captures and independent expected results stay in this workspace.

From the repository root:

```powershell
./tools/physx-features/verify.ps1
```

The script builds all three producers, creates fresh SDK streams and truth files under `output/feature-validation`, compares them with the Rust core, then renders both desktop languages into PNGs without a window. `-SkipDesktop` runs only the SDK/core checks; `-SkipBuild` reuses existing producer executables.

| Producer | Independent reference | Scope |
| --- | --- | --- |
| `pvd-heightfield` | `PxMeshQuery::getTriangle` | Signed heights, scale, local/global pose, holes, tessellation, resource sharing and replay |
| `pvd-query-shapes` | Actual SDK query inputs and returned hits | Sweeps, overlaps and batch queries; primitives and convex query shapes; object lifetimes |
| `pvd-joint-limits` | SDK visualization helpers and `PxScene::getRenderBuffer` | Linear/angular limits, swing cones and double cones; active/inactive colors |
| `pvd-cct` | `PxController::getActor`, proxy shape geometry and pre-simulation world poses | Sustained capsule/box CCT movement in a configurable static scene; history and playback regression |

Each executable takes two positional paths: capture and truth JSON. See `docs/heightfield.md`, `docs/query-shapes.md` and `docs/joint-limits.md` for exact evidence, commands and remaining limitations. These samples establish no claim about PVD3-saved `.pxd` / `.pxd2` containers or other SDK versions.

`pvd-query-shapes --host ADDRESS --port PORT truth.json` uses a fresh SDK socket connection and writes truth for that connection. PhysX 4.1 terrain and complex-query checks use the separately built [4.1 producers](../physx41-sample/README.md).

`pvd-joint-limits` also supports a fresh SDK socket connection. Start the intended listener before running:

```powershell
./tools/physx-features/build.ps1 -Target pvd-joint-limits
./.deps/physx-features-build/Release/pvd-joint-limits.exe --host 127.0.0.1 --port 15445 --interval-ms 100 output/joint-limits-live.joints.json
```

The socket mode uses `PxDefaultPvdSocketTransportCreate` with all instrumentation enabled, produces the same six simulation frames, and writes independent SDK geometry references to the JSON path. The port defaults to 5425; the interval defaults to 16 ms and accepts 0–10000 ms. Use a tee proxy when both the exact TCP stream and an original PVD3 save are needed. Neither this command nor the producer saves the original application's database. Keep the proxy stream and truth JSON under the same basename for `PVD_JOINT_LIMIT_CAPTURE` verification. The existing two-path command continues to use `PxDefaultPvdFileTransportCreate` without frame pacing.

Socket mode flushes under the transport lock after each completed frame and before reporting completion, then checks `PxPvd::isConnected(false)`. Known disconnection returns a nonzero code; partial truth is not a completed capture. Success does not acknowledge receiver parsing, indexing, saving or later teardown.

`pvd-cct capture.pvdstream truth.json [frames static-boxes]` defaults to 1000 frames and 5000 static boxes. It uses debug instrumentation, two moving character controllers, a floor and the requested number of static actors with exclusive box shapes and a shared material. The character controller library is built from the same pinned SDK by `build.ps1 -Target pvd-cct`. It supports 1–100000 frames and 0–50000 static boxes. The independent truth is sampled before simulation because the SDK sends the end-frame marker before the following dynamic-pose update. See [CCT history fixture](../../docs/cct-live-history-fixture.md) for the large-scene reproduction and bounded random-seek check. This producer has no socket mode and does not report CCT callback hits as PVD contact events.

Use a fresh SocketTransport connection for original PVD3 pairing. Replaying one file-transport capture over TCP produced an empty original scene despite a matching header; the cause remains unknown. The native test compares the saved container, same-connection raw bytes and SDK truth, as described in [native debug draw](../../docs/native-debug-draw.md). This does not establish original support for directly opening every SDK file stream.
