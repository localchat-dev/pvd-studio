"""Run one live probe and sample only that process using read-only Win32 APIs."""

import argparse
import ctypes
from ctypes import wintypes
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import subprocess
import sys
import time


class ProcessMemoryCounters(ctypes.Structure):
    _fields_ = [
        ("cb", wintypes.DWORD),
        ("PageFaultCount", wintypes.DWORD),
        ("PeakWorkingSetSize", ctypes.c_size_t),
        ("WorkingSetSize", ctypes.c_size_t),
        ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
        ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
        ("PagefileUsage", ctypes.c_size_t),
        ("PeakPagefileUsage", ctypes.c_size_t),
        ("PrivateUsage", ctypes.c_size_t),
    ]


class ProcessMetrics:
    def __init__(self, pid):
        self.kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        self.psapi = ctypes.WinDLL("psapi", use_last_error=True)
        self.kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        self.kernel.OpenProcess.restype = wintypes.HANDLE
        self.kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        self.kernel.CloseHandle.restype = wintypes.BOOL
        self.kernel.GetProcessTimes.argtypes = [wintypes.HANDLE] + [
            ctypes.POINTER(wintypes.FILETIME)
        ] * 4
        self.kernel.GetProcessTimes.restype = wintypes.BOOL
        self.psapi.GetProcessMemoryInfo.argtypes = [
            wintypes.HANDLE,
            ctypes.POINTER(ProcessMemoryCounters),
            wintypes.DWORD,
        ]
        self.psapi.GetProcessMemoryInfo.restype = wintypes.BOOL
        # Query and VM-read rights cannot change or terminate the child process.
        self.handle = self.kernel.OpenProcess(0x0400 | 0x0010, False, pid)
        if not self.handle:
            raise ctypes.WinError(ctypes.get_last_error())

    def read(self, exited=False):
        created, ended, kernel, user = (wintypes.FILETIME() for _ in range(4))
        if not self.kernel.GetProcessTimes(
            self.handle,
            ctypes.byref(created),
            ctypes.byref(ended),
            ctypes.byref(kernel),
            ctypes.byref(user),
        ):
            raise ctypes.WinError(ctypes.get_last_error())
        seconds = lambda value: ((value.dwHighDateTime << 32) | value.dwLowDateTime) / 10_000_000
        kernel_seconds, user_seconds = seconds(kernel), seconds(user)
        memory = ProcessMemoryCounters()
        memory.cb = ctypes.sizeof(memory)
        available = bool(self.psapi.GetProcessMemoryInfo(self.handle, ctypes.byref(memory), memory.cb))
        if not available and not exited:
            raise ctypes.WinError(ctypes.get_last_error())
        return {
            "working_set_bytes": memory.WorkingSetSize if available else None,
            "private_bytes": memory.PrivateUsage if available else None,
            "peak_working_set_bytes_os": memory.PeakWorkingSetSize if available else None,
            "kernel_cpu_seconds": kernel_seconds,
            "user_cpu_seconds": user_seconds,
            "cpu_seconds": kernel_seconds + user_seconds,
        }

    def close(self):
        if self.handle:
            self.kernel.CloseHandle(self.handle)
            self.handle = None


def monitor(command, output, environment, settings):
    if os.name != "nt":
        raise OSError("This monitor requires Windows.")
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    paths = {name: output / name for name in (
        "probe.stdout.log", "probe.stderr.log", "monitor.samples.jsonl", "monitor.json"
    )}
    for path in paths.values():
        if path.exists():
            raise FileExistsError(f"Refusing to overwrite monitoring output: {path}")
    samples, errors = [], []
    started_utc = datetime.now(timezone.utc).isoformat()
    started = time.monotonic()
    with paths["probe.stdout.log"].open("xb") as stdout, \
            paths["probe.stderr.log"].open("xb") as stderr, \
            paths["monitor.samples.jsonl"].open("x", encoding="utf-8") as sample_log:
        child = subprocess.Popen(
            command,
            stdin=subprocess.DEVNULL,
            stdout=stdout,
            stderr=stderr,
            env=environment,
            creationflags=subprocess.CREATE_NO_WINDOW,
        )
        metrics = None
        next_sample = started
        try:
            try:
                metrics = ProcessMetrics(child.pid)
            except OSError as error:
                errors.append({"elapsed_seconds": time.monotonic() - started, "error": str(error)})
            while True:
                exit_code = child.poll()
                if metrics is not None:
                    try:
                        sample = metrics.read(exited=exit_code is not None)
                        sample.update(elapsed_seconds=time.monotonic() - started, final=exit_code is not None)
                        samples.append(sample)
                        sample_log.write(json.dumps(sample, ensure_ascii=False) + "\n")
                        sample_log.flush()
                    except OSError as error:
                        if child.poll() is None:
                            errors.append({"elapsed_seconds": time.monotonic() - started, "error": str(error)})
                if exit_code is not None:
                    break
                next_sample = max(next_sample + 1.0, time.monotonic())
                try:
                    child.wait(timeout=max(0.0, next_sample - time.monotonic()))
                except subprocess.TimeoutExpired:
                    pass
        finally:
            # The probe owns its SDK child and timeout cleanup; the monitor never kills it early.
            child.wait()
            if metrics is not None:
                metrics.close()
    wall_seconds = time.monotonic() - started
    peak = lambda key: max((sample[key] for sample in samples if sample[key] is not None), default=None)
    summary = {
        "schema_version": 1,
        "pid": child.pid,
        "command": command,
        "started_utc": started_utc,
        "wall_seconds": wall_seconds,
        "exit_code": child.returncode,
        "settings": settings,
        "scope": "Spawned probe process only; SDK producer and monitor are excluded.",
        "sampling_interval_seconds": 1.0,
        "timeout_policy": "PVD_PROBE_TIMEOUT is enforced by the probe; monitor waits without terminating any process.",
        "sample_count": len(samples),
        "peak": {
            "working_set_bytes_observed": peak("working_set_bytes"),
            "private_bytes_observed": peak("private_bytes"),
            "working_set_bytes_os": peak("peak_working_set_bytes_os"),
        },
        "cpu_seconds": peak("cpu_seconds"),
        "monitor_errors": errors,
        "samples": samples,
    }
    with paths["monitor.json"].open("x", encoding="utf-8") as report:
        json.dump(summary, report, ensure_ascii=False, indent=2)
        report.write("\n")
    return summary


def nonnegative(value):
    parsed = int(value)
    if parsed < 0:
        raise argparse.ArgumentTypeError("must be zero or greater")
    return parsed


def positive(value):
    parsed = nonnegative(value)
    if parsed == 0:
        raise argparse.ArgumentTypeError("must be greater than zero")
    return parsed


def main():
    project = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", type=Path, required=True, help="Existing live_probe.exe; no build is performed")
    parser.add_argument("--sdk", type=Path, default=project / ".deps/physx-sample-build/Release/pvd-physx-sample.exe", help="PhysX SDK producer executable")
    parser.add_argument("--output", type=Path, required=True, help="Output directory; existing monitor logs are protected")
    parser.add_argument("--boxes", type=nonnegative, default=0)
    parser.add_argument("--static-boxes", type=nonnegative, default=0)
    parser.add_argument("--frames", type=positive, default=180)
    parser.add_argument("--interval-ms", type=nonnegative, default=16)
    parser.add_argument("--duration-seconds", type=nonnegative, default=0)
    parser.add_argument("--timeout", type=positive, default=60, help="Probe deadline in seconds, followed by its SDK cleanup; monitor does not kill")
    parser.add_argument("--profile", action="store_true")
    parser.add_argument("--continuous-motion", action="store_true")
    parser.add_argument("--resident-allocations", type=nonnegative, default=0)
    parser.add_argument("--analysis-batches-per-frame", type=nonnegative, default=0)
    parser.add_argument("--mode", choices=("file", "network"), default="network")
    args = parser.parse_args()
    if args.static_boxes > 20000 or args.resident_allocations > 200000 or args.analysis_batches_per_frame > 64:
        parser.error("static boxes must be <= 20000, resident allocations <= 200000 and analysis batches <= 64")
    for name in ("exe", "sdk"):
        path = getattr(args, name).resolve()
        if not path.is_file():
            parser.error(f"{name} executable not found: {path}")
        setattr(args, name, path)
    output = args.output.resolve()
    environment = os.environ.copy()
    values = {
        "PVD_PROBE_ROOT": str(project),
        "PVD_SDK34_SAMPLE": str(args.sdk),
        "PVD_PROBE_OUTPUT": str(output),
        "PVD_PROBE_BOXES": str(args.boxes),
        "PVD_PROBE_STATIC_BOXES": str(args.static_boxes) if args.static_boxes else None,
        "PVD_PROBE_FRAMES": str(args.frames),
        "PVD_PROBE_INTERVAL": str(args.interval_ms),
        "PVD_PROBE_TIMEOUT": str(args.timeout),
        "PVD_PROBE_MODE": args.mode,
        "PVD_PROBE_PROFILE": "1" if args.profile else None,
        "PVD_PROBE_CONTINUOUS": "1" if args.continuous_motion else None,
        "PVD_PROBE_DURATION_SECONDS": str(args.duration_seconds) if args.duration_seconds else None,
        "PVD_PROBE_NO_HISTORY": None,
        "PVD_PROBE_RESIDENT_ALLOCATIONS": str(args.resident_allocations) if args.resident_allocations else None,
        "PVD_PROBE_ANALYSIS_BATCHES": str(args.analysis_batches_per_frame) if args.analysis_batches_per_frame else None,
    }
    for name, value in values.items():
        if value is None:
            environment.pop(name, None)
        else:
            environment[name] = value
    try:
        summary = monitor([str(args.exe)], output, environment, values)
    except OSError as error:
        print(f"Monitor failed: {error}", file=sys.stderr)
        return 2
    print(json.dumps({key: summary[key] for key in (
        "pid", "wall_seconds", "exit_code", "sample_count", "peak", "cpu_seconds"
    )}, ensure_ascii=False))
    if summary["exit_code"]:
        return summary["exit_code"]
    return 2 if summary["monitor_errors"] or not summary["samples"] else 0


if __name__ == "__main__":
    sys.exit(main())
