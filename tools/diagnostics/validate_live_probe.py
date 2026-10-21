import argparse
import csv
import json
import re
from pathlib import Path


def read_rows(path):
    with path.open(newline="", encoding="utf-8") as stream:
        return list(csv.DictReader(stream))


def validate(directory, minimum_duration):
    summary = json.loads((directory / "summary.json").read_text(encoding="utf-8"))
    monitor = json.loads((directory / "monitor.json").read_text(encoding="utf-8"))
    for name in ("producer_success", "completed", "decode_ok", "holds_display_recording"):
        if not summary[name]:
            raise ValueError(f"Probe did not complete {name}")
    if summary["queued_bytes_at_completion"] != 0 or summary["display_analysis_diagnostics"]:
        raise ValueError("Probe completed with queued bytes or analysis diagnostics")
    if monitor["exit_code"] != 0 or monitor["monitor_errors"] or not monitor["samples"]:
        raise ValueError("Process monitoring failed or collected no samples")
    settings = monitor["settings"]
    boxes = int(settings["PVD_PROBE_BOXES"])
    static_boxes = int(settings.get("PVD_PROBE_STATIC_BOXES") or 0)
    resident = int(settings.get("PVD_PROBE_RESIDENT_ALLOCATIONS") or 0)
    batches = int(settings.get("PVD_PROBE_ANALYSIS_BATCHES") or 0)
    statistics = read_rows(directory / "producer-stats.csv")
    frames = len(statistics)
    if not frames:
        raise ValueError("SDK step statistics are empty")
    if summary["last_frame"] + 1 != frames:
        raise ValueError("Decoded frames differ from SDK step count")
    if summary["display_static_actors"] != static_boxes + 2:
        raise ValueError("Displayed static actors differ from SDK statistics")
    for frame, row in enumerate(statistics):
        if int(row["frame"]) != frame:
            raise ValueError("SDK step statistics are not contiguous")
        if any(int(row[name]) != boxes for name in (
            "continuous_boxes", "awake_continuous_boxes", "moving_continuous_boxes"
        )) or int(row["active_dynamic"]) < boxes:
            raise ValueError(f"Continuous SDK activity was lost at step {frame}")
        if int(row["total_static"]) != static_boxes + 2:
            raise ValueError(f"SDK static body count differs at step {frame}")
    sdk_seconds = float(statistics[-1]["elapsed_ms"]) / 1000
    if sdk_seconds < minimum_duration:
        raise ValueError(f"SDK ran {sdk_seconds}s, below the required {minimum_duration}s")
    if resident or batches:
        analysis = read_rows(directory / "producer-analysis.csv")
        steps = [row for row in analysis if row["phase"] == "step"]
        if len(steps) != frames or analysis[-1]["phase"] != "released":
            raise ValueError("SDK analysis truth is incomplete")
        for frame, row in enumerate(steps):
            expected = batches * (frame + 1)
            if (int(row["frame"]), int(row["resident_live"]), int(row["profile_pairs"]),
                int(row["allocation_calls"]), int(row["free_calls"])) != (
                frame, resident, expected, resident + expected, expected
            ):
                raise ValueError(f"SDK analysis load differs at step {frame}")
        final = analysis[-1]
        if (int(final["resident_live"]) != 0 or
            int(final["allocation_calls"]) != int(final["free_calls"])):
            raise ValueError("SDK resident allocations were not released")
    if static_boxes:
        statics = read_rows(directory / "producer-stats.csv.statics.csv")
        if len(statics) != static_boxes or len({row["actor_id"] for row in statics}) != static_boxes:
            raise ValueError("SDK static getter truth has missing or duplicate actors")
    samples = []
    elapsed = None
    log = (directory / "probe.stdout.log").read_text(encoding="utf-8")
    for line in log.splitlines():
        time = re.match(r"t=([0-9.]+)s updates=([0-9]+) frame=([0-9]+) objects=([0-9]+)", line)
        if time:
            elapsed = float(time[1])
        status = re.match(r"receive_status=.*?queued_bytes: ([0-9]+), retained_disk_bytes: ([0-9]+)", line)
        if status and elapsed is not None:
            samples.append({"seconds": elapsed, "queued_bytes": int(status[1]), "disk_bytes": int(status[2])})
    post_warmup = [sample for sample in samples if sample["seconds"] >= 60]
    steady = [sample for sample in post_warmup if sample["seconds"] <= summary["producer_seconds"]]

    def queue_slope(points):
        if len(points) < 2:
            return None
        mean_time = sum(point["seconds"] for point in points) / len(points)
        mean_queue = sum(point["queued_bytes"] for point in points) / len(points)
        numerator = sum((point["seconds"] - mean_time) * (point["queued_bytes"] - mean_queue) for point in points)
        return numerator / sum((point["seconds"] - mean_time) ** 2 for point in points)
    leftovers = [str(path) for path in (directory / "history").rglob("*") if path.is_file() and path.suffix in (".pvdhistory", ".bin")]
    if leftovers:
        raise ValueError(f"Temporary history or receive files survived cleanup: {leftovers[:3]}")
    return {
        "directory": str(directory), "frames": frames, "sdk_seconds": sdk_seconds,
        "producer_seconds": summary["producer_seconds"], "completion_seconds": summary["elapsed_seconds"],
        "received_bytes": summary["received_bytes"], "sampled_max_queued_bytes": summary["max_queued_bytes"],
        "updates": summary["updates"], "updates_per_second": summary["updates"] / summary["elapsed_seconds"],
        "steady_queue_slope_bytes_per_second": queue_slope(steady), "queue_samples": samples,
        "post_warmup_including_drain_slope_bytes_per_second": queue_slope(post_warmup),
        "steady_sample_count": len(steady),
        "steady_scope": "Probe time >= 60 seconds and <= observed producer exit; all samples including drain remain in queue_samples.",
        "steady_sampled_max_queued_bytes": max((sample["queued_bytes"] for sample in steady), default=None),
        "static_boxes": static_boxes, "continuous_boxes": boxes, "resident_allocations": resident,
        "analysis_batches_per_frame": batches, "display_objects_at_completion": summary["display_objects"],
        "peak": monitor["peak"], "cpu_seconds": monitor["cpu_seconds"], "cache_files_after_exit": leftovers,
        "scope": "Loopback SDK, disk receive, decoding, disk history and CPU display-state application; excludes a native window and GPU.",
    }


def main():
    parser = argparse.ArgumentParser(description="Validate a monitored continuous rich SDK probe and its independent truth CSVs.")
    parser.add_argument("directory", type=Path)
    parser.add_argument("--minimum-duration", type=float, default=0)
    args = parser.parse_args()
    if args.minimum_duration < 0:
        parser.error("minimum duration must be zero or greater")
    directory = args.directory.resolve()
    report = validate(directory, args.minimum_duration)
    with (directory / "validation.json").open("x", encoding="utf-8") as stream:
        json.dump(report, stream, indent=2, ensure_ascii=False)
        stream.write("\n")
    print(json.dumps({key: value for key, value in report.items() if key != "queue_samples"}, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
