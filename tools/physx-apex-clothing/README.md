# APEX 1.4 Clothing capture

This console producer records the official CPU Clothing module and independent SDK getter results. It uses PhysX 3.4.2 and APEX 1.4 from commit `5e42a5f112351a223c19c17bb331e6c55037b8eb` of [NVIDIA's PhysX 3.4 repository](https://github.com/NVIDIAGameWorks/PhysX-3.4/tree/5e42a5f112351a223c19c17bb331e6c55037b8eb). It opens no window and does not modify the SDK.

Build the PhysX libraries with `tools/physx-sample/build.ps1`. Set `$apexRoot` to the matching APEX_1.4 directory, containing the headers, checked Framework/Clothing/Legacy binaries and `plane32.apx`. `dependencies.json` identifies the required files; the build script validates them, accepting either LF or CRLF for text. It does not download dependencies or build the APEX SDK.

## Generate inputs

From the repository root, use a new output directory:

```powershell
./tools/physx-apex-clothing/build.ps1 -ApexRoot $apexRoot
$fixtureRoot = Join-Path 'output' 'apex-clothing-validation'
foreach ($mode in @('sync', 'async')) {
    $caseRoot = Join-Path $fixtureRoot $mode
    New-Item -ItemType Directory -Path $caseRoot | Out-Null
    $fetchOptions = @()
    if ($mode -eq 'async') { $fetchOptions += '--async-fetch' }
    & ./.deps/physx-apex-clothing-build/Release/pvd-apex-clothing.exe "$caseRoot/capture.pvdstream" "$caseRoot/truth.json" "$apexRoot/samples_v2/SampleClothingBase/plane32.apx" --debug-only @fetchOptions
    cargo run --locked --manifest-path tools/physx-apex-clothing/probe/Cargo.toml -- "$caseRoot/capture.pvdstream" "$caseRoot/event-probe"
    python tools/physx-apex-clothing/verify-renderer.py "$caseRoot/truth.json" "$caseRoot/event-probe" "$caseRoot/renderer.json"
}
```

The default fixture has 36 simulation steps and two actors sharing one asset. It covers world/local simulation coordinates, actor creation, movement, destruction and replacement. `--cycles 1..32` repeats this lifecycle in one connection, with distinct actor serials and a shared asset. Truth records the cycle count, asset indices, getter positions, poses, ActorParams identities and transport byte counts around simulate, fetchResults and getters. Each capture must stay with its own truth and event directory.

The wire probe retains the source IDs and original order of `frame` / `ApexFrame`, plus renderer payloads. It does not assign APEX events to PhysX frames. Renderer verification compares every emitted triangle with the matching getter state and requires one draw for every actor/step. Asynchronous getter draws can follow both frame ends; the final uncompleted tail stays outside completed frames.

## Verify core and export

These tests are ignored by default. Run both generated inputs:

```powershell
foreach ($mode in @('sync', 'async')) {
    $env:PVD_APEX_CLOTHING_DIRECTORY = (Resolve-Path (Join-Path $fixtureRoot $mode)).Path
    cargo test --locked -p pvd-core --test apex_clothing official_apex_clothing_matches_getters_and_source_windows -- --ignored --exact --nocapture
}
$env:PVD_APEX_SYNC_CAPTURE = (Resolve-Path (Join-Path $fixtureRoot 'sync/capture.pvdstream')).Path
$env:PVD_APEX_ASYNC_CAPTURE = (Resolve-Path (Join-Path $fixtureRoot 'async/capture.pvdstream')).Path
cargo test --locked -p pvd-core --test apex_clips real_apex_prefixes_preserve_raw_payloads_endpoints_and_complete_frame_states -- --ignored --exact
```

The core test checks per-source windows against getter positions and wire order, object lifetimes, shared asset references, local-space transforms, memory/disk/live replay and PVDCAP03 reopening. The clip test checks raw prefix payloads and endpoints. Default `apex_clips` tests separately cover representable middle ranges and explicit rejection when omitted predecessor ends cannot preserve every source window. See [frame sources](../../docs/frame-sources.md) and [SDK clips](../../docs/clips.md).

## Verify a live SDK TCP connection

The socket mode uses `PxDefaultPvdSocketTransportCreate`. The ignored desktop test starts a fresh SDK process and receives its stream through the production listener. It controls simulation steps over the producer's standard input; no control messages are added to the PVD stream. Both SDK objects and getter truth belong to this connection, rather than a replayed file.

Build the producer and event probe, then run both fetch modes:

```powershell
./tools/physx-apex-clothing/build.ps1 -ApexRoot $apexRoot
cargo build --locked --release --manifest-path tools/physx-apex-clothing/probe/Cargo.toml --target-dir .deps/apex-clothing-probe-build
$env:PVD_APEX_CLOTHING_PRODUCER = (Resolve-Path '.deps/physx-apex-clothing-build/Release/pvd-apex-clothing.exe').Path
$env:PVD_APEX_CLOTHING_ASSET = (Resolve-Path (Join-Path $apexRoot 'samples_v2/SampleClothingBase/plane32.apx')).Path
$env:PVD_APEX_EVENT_PROBE = (Resolve-Path '.deps/apex-clothing-probe-build/release/pvd-apex-event-probe.exe').Path
foreach ($mode in @('sync', 'async')) {
    $env:PVD_APEX_TCP_MODE = $mode
    cargo test --locked -p pvd-desktop studio::apex_tcp_tests::official_apex_tcp_preserves_sources_playback_and_saved_frames -- --ignored --exact --nocapture --test-threads=1
    if ($LASTEXITCODE -ne 0) { throw "APEX TCP verification failed: $mode" }
}
```

Each run creates a distinct directory under `output/apex-clothing-tcp`. Set `PVD_APEX_TCP_OUTPUT` to an existing directory under the repository's `output` directory to choose another parent. Successful runs retain `capture.pvdstream`, `truth.json`, `event-probe`, `actions.json`, `result.json`, SDK logs and the saved project capture. Failures retain their partial evidence. Executable, asset, capture and truth hashes are recorded; the SDK and probe must be rebuilt from the source being tested.

The test switches PhysX/APEX sources, pauses while more steps arrive, steps in both directions, runs forward/reverse playback while requesting new SDK steps, returns to live, and checks the final source after disconnection. Displayed endpoints are compared with the same independent raw-event and getter oracle used by the core test. It also saves the raw stream and PVDCAP03, reopens both source timelines, and checks temporary history release. This is a controlled loopback check, not a throughput, long-running or cross-machine test.

For an already running listener, the producer can also run without step control:

```powershell
./.deps/physx-apex-clothing-build/Release/pvd-apex-clothing.exe --host 127.0.0.1 --port 5425 output/apex-tcp-truth.json "$apexRoot/samples_v2/SampleClothingBase/plane32.apx" --debug-only
```

The listener must retain this connection's original bytes alongside its truth. `--step-control` is intended for a controller: after `PVD_APEX_READY`, it accepts `step 0` through `step (36 * cycles - 1)`, acknowledges each flushed byte offset with `PVD_APEX_STEP`, then accepts `finish`. The acknowledgement covers producer writes; the desktop test separately waits for decoding and the selected completed endpoint.

## Rolling history

The longer desktop check uses ten lifecycle cycles in one SDK connection: 360 steps, 720 completed source boundaries, a 4 MiB history budget and four frames per segment. It checks a paused endpoint across physical eviction, source switching, navigation at the retained edges, forward/reverse playback, raw saving, PVDCAP03 reopening and history cleanup. Getter comparisons cover sampled live endpoints and the complete final retained window; the test requires cumulative debug primitives to exceed the 64 MiB source-log limit. It remains a controlled loopback check rather than a throughput or cross-machine measurement.

Use the same producer, asset and probe environment variables as the live test:

```powershell
foreach ($mode in @('sync', 'async')) {
    $env:PVD_APEX_TCP_MODE = $mode
    cargo test --locked -p pvd-desktop studio::apex_tcp_tests::rolling::official_apex_tcp_rolls_both_sources_and_reopens_the_retained_window -- --ignored --exact --nocapture --test-threads=1
    if ($LASTEXITCODE -ne 0) { throw "APEX rolling TCP verification failed: $mode" }
}
```

Each run retains its capture, truth, event probe, action trace and `retention.json` under the reported output directory. To check fragmented core decoding against one of those same inputs, set `PVD_APEX_CLOTHING_DIRECTORY` to that exact directory and run:

```powershell
cargo test --locked -p pvd-core --test apex_clothing official_apex_clothing_retained_windows_match_getters_after_log_reclamation -- --ignored --exact --nocapture --test-threads=1
```

## Verify profile analysis

Generate a separate ALL capture without `--debug-only`, then decode its profile buffers with the independent Python reader:

```powershell
$profileRoot = Join-Path 'output' 'apex-profile-validation'
New-Item -ItemType Directory -Path $profileRoot | Out-Null
& ./.deps/physx-apex-clothing-build/Release/pvd-apex-clothing.exe "$profileRoot/capture.pvdstream" "$profileRoot/truth.json" "$apexRoot/samples_v2/SampleClothingBase/plane32.apx"
python tools/physx-apex-clothing/profile-oracle.py "$profileRoot/capture.pvdstream" "$profileRoot/oracle"
$env:PVD_APEX_PROFILE_CAPTURE = (Resolve-Path "$profileRoot/capture.pvdstream").Path
$env:PVD_APEX_PROFILE_ORACLE = (Resolve-Path "$profileRoot/oracle").Path
cargo test --locked -p pvd-core --test apex_profile official_apex_profile_matches_raw_events_and_source_clocks -- --ignored --exact --nocapture --test-threads=1
```

The reader uses the fixed SDK's stream and profile layouts, without importing the project's protocol or analysis code. It records raw event offsets, zone/event/thread/context identities, ticks, refresh-frame positions and section clocks. Completed spans match the newest pending start for the same key, following the inspected PVD3 handler. The default oracle checks can run with `python tools/physx-apex-clothing/check-profile-oracle.py`.

The ignored core test compares every span and both source directories with these independent bytes, then checks the disk index against the same expected rows. This is a profile-query check: a missing memory dictionary remains an error for complete CSV export. The reader accepts the producer's little-endian v1, single-event groups and direct U8 profile properties; profile messages, chunked profile delivery, legacy GPU payloads and unknown layouts fail explicitly. Other events are traversed by their group bounds rather than fully interpreted. It does not establish complete ALL analysis or original-container interoperability.

## Scope

Instrumentation defaults to `eALL`. `--debug-only` explicitly selects `eDEBUG` for object, parameter and drawing data; truth records the selected mode. The ALL connection path can emit memory allocations whose string handles were discarded before the memory listener attached. `inspect-memory.py` diagnoses this from complete raw payloads. Such a capture does not establish memory-analysis compatibility; the strict reader can reject it.

SDK modules and scene are created before connecting PVD. Loading Clothing after connection uses inconsistent module interface IDs in this SDK version. The producer preserves wire data and does not create missing instances or infer pointer aliases. Other APEX modules, GPU data, complete analysis and native PVD3 container export require separate validation.
