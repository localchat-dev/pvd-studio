# PhysX 3.4 CCT movement and large-history fixture

This fixture uses the official PhysX 3.4.2 checkout at `5e42a5f112351a223c19c17bb331e6c55037b8eb`. It records the SDK stream with `PxPvdInstrumentationFlag::eDEBUG`; it is an independent validation program, not a simulation inside pvd-studio. No original PVD3 container compatibility is established by this fixture.

## SDK evidence

- `PhysX_3.4/Source/PhysXCharacterKinematic/src/CctController.cpp`, `createProxyActor`: a controller creates a `PxRigidDynamic` with an exclusive shape and `eKINEMATIC`, then adds that actor to the scene. CCT is consequently represented by ordinary rigid actor and shape records in the PVD stream.
- `CctCharacterController.cpp`, end of `Controller::move`: movement updates the proxy through `setKinematicTarget`.
- `CctCapsuleController.cpp`, `CCTtoProxyRadius` / `CCTtoProxyHeight`, and `CctBoxController.cpp`, `CCTtoProxyExtents`: proxy geometry uses the controller's `scaleCoeff`. The SDK default is `0.8` in `Include/characterkinematic/PxController.h`. A visible proxy is therefore not the full controller contact shell.
- `Source/PhysX/src/buffering/ScbScenePvdClient.cpp`, `frameEnd`: `sendEndFrame` is called before `updateDynamicActorsAndArticulations`. The truth records `getGlobalPose() * getLocalPose()` before each simulation call, matching the completed PVD frame's actor state.
- The CCT module's narrow-phase sweeps use `PxGeometryQuery`, while its candidate collection uses `PxScene::overlap`. No generic CCT hit callback event binding was found in this module. This validation checks proxy movement and geometry; it does not claim CCT hit-point or normal reporting.

## Reproduction

Run from the workspace root, using the existing local MSVC toolchain:

```powershell
./tools/physx-features/build.ps1 -Target pvd-cct
New-Item -ItemType Directory -Path captures -Force | Out-Null
./.deps/physx-features-build/Release/pvd-cct.exe captures/physx-3.4-cct.pvdstream captures/physx-3.4-cct.json 120 2000
cargo test --locked --offline -p pvd-core --test sdk_cct -- --ignored

./.deps/physx-features-build/Release/pvd-cct.exe captures/physx-3.4-cct-large.pvdstream captures/physx-3.4-cct-large.json 1000 16000
$env:PVD_CCT_CAPTURE = (Resolve-Path captures/physx-3.4-cct-large.pvdstream).Path
cargo test --locked --offline --release -p pvd-core --test sdk_cct -- --ignored
Remove-Item Env:PVD_CCT_CAPTURE
```

The 16000 static boxes produce approximately 48000 persistent actor/shape/geometry objects. A floor and two controllers are added separately. Frame zero is stationary; controllers then move back and forth every 100 frames. The static boxes are placed away from their path. The JSON comes directly from SDK getters and includes proxy actor/shape IDs, geometry, position and quaternion for every frame.

Frame zero is stationary so initialization is not confused with the later pre-simulation sampling convention. Truth comes from SDK getters at the published frame boundary; it must not be reconstructed from decoded output.

`sdk_cct` loads the exact file through the file parser and through 8191-byte live chunks, compares the resulting states, and checks the render mapping against SDK truth. The 120-frame fixture also runs the full sequential-versus-checkpoint verification. The large fixture checks frames 0, 59, 60, 119, 120, 355, 356, 357 and the final frame, plus 16 deterministic random indices, in forward and reverse orders. It does not perform a 1000-frame full-scene comparison at every index.

Generated addresses and hashes vary between runs. Always use the matching SDK truth JSON. These checks do not establish original-container, native-window or performance parity.
