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
Actor/Shape references pass the independent SDK JSON comparison. This fixture covers ordinary raycasts; batched queries, sweeps and overlaps need separate version-specific checks.
