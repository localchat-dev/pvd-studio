# Additional PhysX 4.1 producer

PhysX 3.4.2 remains the primary validation baseline. This producer adds a
separate real runtime test for official PhysX **4.1.2**, pinned to
`a2c0428acab643e60618c681b501e86f7fd558cc` from the `4.1` release branch of
NVIDIAGameWorks/PhysX. It reuses the same scene and truth-export source as the
3.4 producer; no physics simulation runs inside pvd-studio.

Existing Windows C++ build tools, CMake and Git are required. All acquisition
and build output remains under the project `.deps` directory.

Run the independent file + TCP validation without opening or controlling a desktop window:

```powershell
./tools/physx41-sample/verify.ps1
```

Use `-BuildSdk` when the producer is not built. The script creates fresh baseline
and rich SDK streams, checks independent CSV/mesh truth, captures a real TCP
session on configurable port `15426`, repeats the rich truth checks against the
received bytes, validates a separate four-ray-per-frame query capture against
independent SDK hit results, verifies replay and exports actual profile/memory CSV. Outputs
are retained in `output/sdk41-validation/<timestamp>`. A private copy of the CLI
is used for listening so it does not lock the normal build output. SDK 3.4.2
remains the first compatibility baseline; this adds a separately verified row.

```powershell
powershell -ExecutionPolicy Bypass -File tools/physx41-sample/build.ps1
.deps/physx41-sample-build/Release/pvd-physx41-sample.exe --file captures/physx-4.1.pvdstream --truth captures/physx-4.1.csv --frames 120 --lifecycle --flags debug
.deps/physx41-sample-build/Release/pvd-physx41-sample.exe --file captures/physx-4.1-rich.pvdstream --truth captures/physx-4.1-rich.csv --mesh-truth captures/physx-4.1-rich.mesh.json --frames 120 --lifecycle --flags all --rich
$env:PVD_SDK34_CAPTURE = "$PWD/captures/physx-4.1.pvdstream"
$env:PVD_SDK34_RICH_CAPTURE = "$PWD/captures/physx-4.1-rich.pvdstream"
cargo test -p pvd-core --test sdk34 -- --ignored --nocapture
```

The tests compare 120 Scene frames, independent Actor poses, pointer reuse, six rich-scene shapes, cooked mesh data, contacts and joint frames. Profile and memory payloads are decoded separately. These fixture checks do not establish every SDK feature or native PVD3 container compatibility.

The query fixture adds 480 ordinary raycasts and 360 hits across 120 frames,
including misses and Actor ID reuse. Positions, normals, distances, flags and
Actor/Shape references are compared with independent SDK JSON.

## Terrain, queries and articulations

The build also produces `pvd-physx41-heightfield`,
`pvd-physx41-query-shapes` and `pvd-physx41-articulation`. Their truth files report the compiled SDK version.
Terrain validation compares 6,000 SDK triangles across 75 frames, including
signed heights, tessellation, holes, scale, local poses and shared resources.
PhysX 4.1 has no height-field thickness property; its standard triangle winding
is checked independently from the 3.4 thickness cases.

The complex-query fixture contains 204 sweeps, 192 overlaps and 24 batched rays
across 12 frames. It compares 312 hits, box/sphere/capsule/convex geometry,
negative MTD distances, misses and Actor/Shape ID reuse. The same checks run
against a fresh TCP capture with truth from that connection.

The [articulation fixture](../physx41-articulation/README.md) compares a three-link
reduced-coordinate chain across 12 frames, including sleep, wake and destruction.
Captured Base ownership, link geometry, joint relationships and replay are checked
against SDK getters and independent wire events in both file and TCP captures.
The stream does not expose the RC-specific joint configuration.

Run from the repository root after building the producers:

```powershell
cargo build --locked -p pvd-cli -p pvd-desktop
python tools/physx41-sample/verify-features.py --output output/sdk41-features --desktop target/debug/pvd-desktop.exe
```

`--output` must name a new directory. `--port` selects the loopback port
(default `15427`). Omit `--desktop` to skip desktop tree/activity tests and
offscreen rendering; when supplied, inspect the eight generated images separately.
The report records skipped desktop checks and pending image inspection separately
from successful process checks. It also records executable,
capture and truth hashes, commands and failures. These checks do not establish
native PVD3 container compatibility, RepX import or all PhysX 4.1 features.
