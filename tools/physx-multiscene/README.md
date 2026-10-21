# PhysX 3.4.2 interleaved Scene capture

This independent producer uses the pinned official checked SDK and its real PVD file transport. It creates two Scenes sharing a four-thread dispatcher. Each of 48 rounds starts A and B before fetching either, alternates their fetch order, then completes another A step. A uses 1/60 s; B uses 1/30 s. The resulting merged stream has 144 frame boundaries: 96 from A and 48 from B.

The JSON records Scene and Actor pointers, the latest independently observed SDK poses published before each EndSection, QPC bounds around each fetch, and delegated SDK profiler callback bounds. The SDK emits EndSection before dynamic Actor updates; these checks validate stream-order snapshots, not simultaneous physical time across Scenes. The profiler wrapper preserves the original PVD callback and pairs starts/stops as stacks, retaining open starts. In this pinned SDK, both `Sc::Scene::islandGen` and `Sc::Scene::solver` start `Basic.rigidBodySolver`; the fixture leaves 144 starts open. They must not be silently converted into durations.

From the repository root, using the existing local MSVC/CMake toolchain and built `.deps/PhysX-3.4` checkout:

```powershell
./tools/physx-multiscene/build.ps1
New-Item -ItemType Directory -Path output/multiscene-check
& ./.deps/physx-multiscene-build/Debug/pvd-physx-multiscene.exe `
  output/multiscene-check/capture.pvdstream output/multiscene-check/capture.json
$env:PVD_MULTISCENE_CAPTURE = (Resolve-Path output/multiscene-check/capture.pvdstream).Path
cargo test --locked -p pvd-core --test multiscene -- --ignored --nocapture --test-threads=1
```

Choose new capture/truth paths: the SDK file transport and JSON writer overwrite their requested output files. No global installation or configuration is required. The tests are ignored by default because the official SDK input must first be generated.

The tests compare all 288 Actor poses, frame Scene IDs and clocks, fragmented loading, disk history, 288 continuous forward/reverse seeks, per-Scene frame intervals, 432 complete phase spans, 144 open starts, and six complete-file streaming curves. A small history segment size exercises multiple segments. The test requires zero temporary history files after releasing the disk recording and cursor.

This does not validate original PVD3 containers, renderer-event ownership, synchronized physical-time resampling, Scene destruction/reuse, another SDK, native UI input or GPU performance. See [the evidence and limitations](../../docs/multiscene-sdk.md).
