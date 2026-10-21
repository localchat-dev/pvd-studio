# Official SDK camera capture

This independent Windows/MSVC producer calls PhysX 3.4's public `PxPvdSceneClient::updateCamera` and writes the SDK's file transport. It does not encode PVD events itself or use the project's decoder to generate expected results.

From the repository root, using the existing built SDK and MSVC toolchain:

```powershell
cmake -S tools/physx-cameras -B .deps/physx-cameras-build -G "Visual Studio 18 2026" -A x64 -DPHYSX34_ROOT="$PWD/.deps/physx-3.4"
cmake --build .deps/physx-cameras-build --config Release
New-Item -ItemType Directory -Path captures/cameras -Force | Out-Null
& .deps/physx-cameras-build/Release/pvd-physx-cameras.exe --file captures/cameras/cameras.pvdstream --truth captures/cameras/truth.json --frames 133
cargo test --locked --offline -p pvd-core --test source_cameras official_sdk_cameras_match_the_producer_in_memory_disk_replay_and_clips -- --ignored --exact --nocapture
```

`--frames` accepts 100 through 10,000; its default is 133. Parent output directories must exist. The executable reports SDK errors and returns a failure status if capture or truth writing fails.

The truth JSON records camera arguments at each successful API call, an independently maintained name-to-camera map for every complete frame, SDK API-call boundaries, and the camera map after trailing calls. Position, up and target preserve the supplied floating-point values, including a non-unit tilted up vector. Camera names are `Main camera` and `Inspection camera`.

The sequence covers an initially empty map, two camera names, repeated names within one frame, unchanged frames, updates before `simulate`, an update after `checkResults(true)` and before `fetchResults`, and updates after `fetchResults`. The final two camera updates occur after the last complete frame and are recorded only in `trailing_cameras`.

The pinned SDK source emits `BeginSection("frame")` from `simulate` and `EndSection("frame")` inside `fetchResults`. Frame truth is therefore captured immediately before `fetchResults`; no camera callback runs inside that call. An update after its return belongs to the next complete frame. This also distinguishes the trailing map from the last frame's map.

Relevant SDK source locations are `NpScene.cpp` (`simulateOrCollide`, `fetchResults`), `ScbScenePvdClient.cpp` (`updateCamera`, `frameStart`, `frameEnd`), `PvdMetaDataPvdBinding.cpp` (`sendBeginFrame`, `sendEndFrame`) and `PxPvdDataStream.cpp` (`updateCamera`). This fixture establishes the official SDK stream path. It does not establish original PVD3 UI behavior or `.pxd/.pxd2` container compatibility.
