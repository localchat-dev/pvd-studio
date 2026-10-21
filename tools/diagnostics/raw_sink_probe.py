"""Measure actual SDK socket arrivals without parsing, history, or rendering."""
import argparse
import json
import os
from pathlib import Path
import socket
import statistics
import subprocess
import time


def main():
    project = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--sample', default=os.getenv('PVD_SDK34_SAMPLE', str(project / '.deps/physx-sample-build/Release/pvd-physx-sample.exe')))
    parser.add_argument('--output', default=os.getenv('PVD_PROBE_OUTPUT', str(project / 'output/live-probe')))
    parser.add_argument('--extra-boxes', type=int, default=10000)
    parser.add_argument('--frames', type=int, default=180)
    parser.add_argument('--interval-ms', type=int, default=16)
    parser.add_argument('--timeout', type=float, default=60)
    args = parser.parse_args()
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    listener = socket.socket()
    listener.settimeout(args.timeout)
    listener.bind(('127.0.0.1', 0))
    listener.listen(1)
    started = time.perf_counter()
    command = [args.sample, '--host', '127.0.0.1', '--port', str(listener.getsockname()[1]), '--frames', str(args.frames), '--flags', 'all', '--rich', '--interval-ms', str(args.interval_ms), '--extra-boxes', str(args.extra_boxes)]
    producer = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
    events = []
    received = 0
    try:
        with listener:
            conn, _ = listener.accept()
            with conn:
                conn.settimeout(args.timeout)
                while chunk := conn.recv(65536):
                    received += len(chunk)
                    events.append((round((time.perf_counter() - started) * 1000, 3), len(chunk)))
        status = producer.wait(timeout=args.timeout)
    finally:
        if producer.poll() is None:
            producer.kill()
        producer.wait()
        listener.close()
    elapsed = time.perf_counter() - started
    gaps = [b[0] - a[0] for a, b in zip(events, events[1:])]
    report = dict(command=command, producer_exit=status, elapsed_seconds=elapsed, received_bytes=received, recv_calls=len(events), gap_ms_median=statistics.median(gaps) if gaps else None, gap_ms_max=max(gaps, default=0), over_100ms=sum(g > 100 for g in gaps), events=events)
    path = output / f'raw-sink-{args.extra_boxes}-{time.time_ns()}.json'
    path.write_text(json.dumps(report, indent=2), encoding='utf-8')
    print(json.dumps({key: value for key, value in report.items() if key != 'events'}, indent=2))
    print(f'Report: {path}')
    if status:
        raise SystemExit(status)


if __name__ == '__main__':
    main()
