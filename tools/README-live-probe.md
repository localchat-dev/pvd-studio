# Live receive diagnostics

The probe uses the desktop `NetworkSession`, applies captured `SceneUpdate` values and holds the displayed recording until the next update. It exercises snapshot sharing and CPU state propagation without a native window or GPU. Producer time and decode completion are separate metrics.

## Setup

Build the [official SDK producer](physx-sample/README.md) first. By default the wrapper uses `.deps/physx-sample-build/Release/pvd-physx-sample.exe` and writes under `output/live-probe/`, relative to the repository root. Override them with `-SdkSample` and `-OutputDirectory`, or process environment variables `PVD_SDK34_SAMPLE` and `PVD_PROBE_OUTPUT`. `-RuntimeRoot` / `PVD_PROBE_ROOT` optionally selects a separate data root.

The wrapper temporarily exposes `live_probe.rs` as a Cargo target and removes it on exit; it refuses to overwrite an existing source target. `-TargetDirectory` chooses the Cargo build directory. The resulting `live_probe.exe` remains in its `release/` subdirectory for direct monitoring.

## Comparable runs

Run commands separately with the same frames, flags, box counts and interval:

```powershell
python tools/diagnostics/raw_sink_probe.py --extra-boxes 10000 --frames 180 --interval-ms 16
./tools/diagnostics/live_probe.ps1 -ExtraBoxes 10000 -Frames 180 -IntervalMilliseconds 16
./tools/diagnostics/live_probe.ps1 -ExtraBoxes 10000 -Frames 180 -IntervalMilliseconds 16 -WithoutHistory
./tools/diagnostics/live_probe.ps1 -ExtraBoxes 10000 -Frames 180 -IntervalMilliseconds 16 -Mode file
```

`-WithoutHistory` uses a 4096-byte history budget to suspend retention while keeping reception and current-state processing. Do not use an impossible free-space reserve for this comparison: it would stop receive storage as well.

`received_bytes` measures socket intake. `queued_bytes` measures data waiting for decoder delivery; delivered data may still be processing. A zero queue does not prove that decoding or display has completed. The default receive quota is unlimited by configuration, with disk-space protection and a 512 MiB reserve; 256 MiB backlog raises a warning. See [receive policy](../docs/receive-backpressure.md).

## Continuous load

```powershell
./tools/diagnostics/live_probe.ps1 -OutputDirectory output/continuous-network -ExtraBoxes 128 -Frames 37500 -IntervalMilliseconds 16 -DurationSeconds 600 -TimeoutSeconds 900 -ContinuousMotion
```

`-ContinuousMotion` keeps the extra Box actors moving and checks SDK statistics. `-DurationSeconds` requests a wall-clock duration; `-Frames` is a safety cap, and exhausting it early is a failure. A 16 ms sleep does not establish a 60 Hz source.

For a mixed static-object and analysis load, build the probe through the wrapper first, then use its actual executable path:

```powershell
$probeExecutable = (Resolve-Path 'target/release/live_probe.exe').Path
$sdkProducer = (Resolve-Path '.deps/physx-sample-build/Release/pvd-physx-sample.exe').Path
python tools/diagnostics/monitor_probe.py --exe $probeExecutable --sdk $sdkProducer --output output/mixed-tcp --boxes 7 --static-boxes 16000 --frames 30000 --interval-ms 16 --duration-seconds 600 --timeout 900 --continuous-motion --resident-allocations 150000 --analysis-batches-per-frame 8
python tools/diagnostics/validate_live_probe.py output/mixed-tcp --minimum-duration 600
```

Adjust `$probeExecutable` if a different Cargo target directory was used. The monitor does not build executables. Validation checks SDK counters, completion, queue drain and cache cleanup; the queue slope excludes 60 seconds of warm-up. Inspect samples and peaks too. Source/receiver clocks are not synchronized for per-frame latency.

## Timing and CPU scene checks

`-Profile` enables aggregate core-stage timings; `PVD_PROBE_PROFILE` is the corresponding presence switch. Timings are off by default. Inclusive values contain nested stages; use `self_ns` when summing independent work. Instrumentation adds cost, so also measure with profiling disabled.

```powershell
cargo run --release --locked -p pvd-core --example throughput -- captures/physx-3.4.pvdstream output/core-history --profile
cargo test --locked --release -p pvd-desktop --bin pvd-desktop real_paced_tcp_to_persistent_bevy_transforms -- --ignored --nocapture
```

The file example is serial and does not measure network or displayed FPS. The ignored TCP test starts the SDK on a loopback port and checks persistent Bevy `GlobalTransform` against captured positions. It needs the built producer and does not create a Winit window.

For desktop diagnostics, launch with `--live-profile` or `start-with-diagnostics.cmd`, then use **Diagnostics → Copy diagnostic JSON**. Copying does not upload anything. Method and remaining product checks are described in [throughput](../docs/realtime-throughput.md) and [validation](../docs/validation.md).
