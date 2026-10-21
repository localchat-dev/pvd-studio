# PhysX 3.4 origin-shift fixture

This independent producer uses the pinned checked SDK and its file transport. Two Scenes each contain a static body, a dynamic body, a kinematic body, a two-link Articulation, CPU Cloth, ParticleSystem and ParticleFluid. The particle indices are sparse (1 and 5).

After one simulation step per Scene, the producer performs five `PxScene::shiftOrigin` calls across the two Scenes. Between them it emits 144 explicit `frame` section boundaries through the SDK's `PvdDataStream`. These markers capture the state immediately after an origin shift, before another simulation can publish replacement Actor poses. They are validation boundaries, not 144 simulated physics steps. One ordinary `setGlobalPose` call also checks that a subsequent absolute pose supersedes the shifted value.

The JSON contains independent SDK `getGlobalPose`, Shape `getLocalPose` and locked particle position/velocity getter values at every marker. It uses `max_digits10` so tests can compare f32 bits, including unmodified rotations, local poses and velocities. The merged capture has two warmup frames and 144 validation frames.

From the repository root, with the existing built SDK:

```powershell
./tools/physx-origin-shift/build.ps1
New-Item -ItemType Directory -Path output/origin-shift-check
& ./.deps/physx-origin-shift-build/Release/pvd-origin-shift.exe `
  output/origin-shift-check/capture.pvdstream output/origin-shift-check/capture.json
$env:PVD_ORIGIN_SHIFT_CAPTURE = (Resolve-Path output/origin-shift-check/capture.pvdstream).Path
cargo test --locked -p pvd-core --test origin_shift -- --ignored --nocapture --test-threads=1
```

Choose new output paths: the SDK transport and truth writer overwrite their requested files. The integration test is ignored by default and requires the generated capture and matching `.json` beside it. It checks all marker states, fragmented input, disk segments, continuous forward/reverse and random replay, then saves a retained project capture and reopens it after releasing the source history.

The producer does not launch PVD3, open a native window, change system settings or install tools. It does not establish original-container type-11 interoperability, another SDK version, GPU particles, custom actor classes, query/contact coordinate handling or frame-rate performance.
