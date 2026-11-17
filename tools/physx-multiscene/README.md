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

## Room lifecycle

The independent `--lifecycle` scenario uses DEBUG instrumentation and four observed Scenes. A and B have actors, C is destroyed without completing a frame, and D is created after B is destroyed. Eight interleaved boundaries are followed by A's destruction without another End. The transport then disconnects while D remains alive. The final JSON records D as not destroyed; cleanup after disconnect cannot be observed in the capture.

```powershell
New-Item -ItemType Directory -Path output/room-validation/file
& ./.deps/physx-multiscene-build/Debug/pvd-physx-multiscene.exe --lifecycle `
  output/room-validation/file/lifecycle.pvdstream output/room-validation/file/lifecycle.json
```

Each checkpoint reports the cumulative transport byte offset, completed boundary count and independently observed living room numbers. Frame poses come from SDK getters, with the same End-before-update convention as the default producer. Scene address reuse is measured by `replacement_reused_address`; it is not forced or guaranteed by the allocator. The default 144-frame producer and truth schema are unchanged.

For a controlled loopback capture with no desktop input:

```powershell
python tools/physx-multiscene/verify-lifecycle.py --output output/room-validation/tcp
```

The output directory must be new. This starts the producer in the background, holds each checkpoint, verifies the expected frame count and room lifetimes, and compares all received TCP bytes with the same-run file mirror. It writes `lifecycle.pvdstream`, `lifecycle.json`, `received.pvdstream` and `transport-check.json`. This verifies the producer and transport; decoder, room directory and desktop behavior need their own checks using these inputs.

Integration tests can start the producer with `--host 127.0.0.1 --port PORT --step-control`. It prints `PVD_ROOMS ready BYTES 0`, then accepts one stdin command at a time: `destroy-zero`, `step`, `destroy-b`, `replace`, `destroy-a`, `disconnect`, `finish`. Each command except `finish` reports a checkpoint after refreshing the truth JSON. The socket and file mirror receive identical bytes; the file is complete after the producer exits. These commands make zero-frame destruction, final-tail destruction and an unconfirmed disconnect observable separately without relying on sleeps or another physics frame.
