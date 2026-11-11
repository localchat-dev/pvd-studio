# PhysX 3.4 origin-shift fixture

This independent producer uses the pinned checked SDK and its file transport. Two Scenes each contain a static body, a dynamic body, a kinematic body, a two-link Articulation, CPU Cloth, ParticleSystem and ParticleFluid. The particle indices are sparse (1 and 5).

After one simulation step per Scene, the producer performs five `PxScene::shiftOrigin` calls across the two Scenes. Between them it emits 144 explicit `frame` section boundaries through the SDK's `PvdDataStream`. These markers capture the state immediately after an origin shift, before another simulation can publish replacement Actor poses. They are validation boundaries, not 144 simulated physics steps. One ordinary `setGlobalPose` call also checks that a subsequent absolute pose supersedes the shifted value.

The JSON contains independent SDK `getGlobalPose`, Shape `getLocalPose`, valid `getKinematicTarget`, particle projection planes and locked particle position/velocity getter values at every marker. It uses `max_digits10` so tests can compare f32 bits, including unmodified rotations, local poses and velocities. The merged capture has two warmup frames and 144 validation frames.

The pinned SDK does not publish `WorldBounds` by default. The producer explicitly declares this property through the SDK DataStream and submits getter values before the shifts and after an absolute pose update. It does not resend bounds after each shift. The JSON identifies this input as `explicit-sdk-getters`; comparison with independently recalculated bounds allows 0.002 per component for f32 rounding. Replay and retained-capture comparisons remain exact for all captured values.

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

The producer does not launch PVD3, open a native window, change system settings or install tools. It does not establish original-container type-11 interoperability, another SDK version, GPU particles, custom actor classes or frame-rate performance.

## Queries, contacts and rendering

`pvd-origin-shift-spatial` exercises two Scenes, four shifts and a subsequent simulation refresh. Each Scene runs one ordinary and two batched raycast/sweep/overlap queries. Its JSON records call positions and independent SDK result buffers, contact callback points, Actor getter positions, and `PxScene::getRenderBuffer()` coordinates before and after each shift.

The SDK render buffer and transmitted PVD renderer packets are separate sources. The fixture uses `PvdUserRenderer::setInstanceId` to send point, line, triangle and text packets both with instance ID zero and explicitly bound to the corresponding Scene ID. Each marker submits a new packet. Input positions are refreshed at validation markers 0, 3 and 6; intervening markers resend the same coordinates across shifts. Each source uses the drawing window since its own preceding End; another Scene's End does not clear that window. The second Scene's later frame includes six intervening packets, checked against the independent getter snapshots. The test compares emitted coordinates and IDs independently; an instance ID does not instruct the SDK serializer to transform the supplied positions. Cached query/contact records are checked against their last SDK call or callback, then checked again after the next simulation publishes replacement values.

The SDK's `PxPvdSceneClient` drawing entry points share a renderer but do not automatically set its instance ID to the Scene. Its `SceneRendererClient` serializes renderer instances and `events` without a Scene ownership property. This fixture therefore establishes explicit wire binding, not an inferred ownership rule based on which Scene created a renderer.

```powershell
./tools/physx-origin-shift/build.ps1 -Target pvd-origin-shift-spatial
New-Item -ItemType Directory -Path output/origin-shift-spatial
& ./.deps/physx-origin-shift-build/Release/pvd-origin-shift-spatial.exe `
  output/origin-shift-spatial/capture.pvdstream output/origin-shift-spatial/capture.json
$env:PVD_ORIGIN_SHIFT_SPATIAL_CAPTURE = (Resolve-Path output/origin-shift-spatial/capture.pvdstream).Path
cargo test --locked -p pvd-core --test origin_shift_spatial -- --ignored --nocapture --test-threads=1
```

This explicit test compares independent SDK truth, fragmented decoding, segmented disk history, forward/reverse/random replay and a retained capture reopened after source cleanup. It does not establish original PVD3 type-11 behavior, original-viewer drawing transformations, another SDK version or an ownership rule for source cameras.
