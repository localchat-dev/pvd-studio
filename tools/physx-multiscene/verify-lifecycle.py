"""Check the real SDK lifecycle transport and retain same-run file/TCP input."""

import argparse
import hashlib
import json
from pathlib import Path
import queue
import socket
import subprocess
import threading
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--producer", type=Path,
        default=Path(__file__).resolve().parents[2]
        / ".deps/physx-multiscene-build/Debug/pvd-physx-multiscene.exe",
    )
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    capture = args.output / "lifecycle.pvdstream"
    truth_path = args.output / "lifecycle.json"
    received = bytearray()
    condition = threading.Condition()
    lines = queue.Queue()
    receiver_errors = []
    receiver_done = threading.Event()
    checkpoints = [
        (None, "ready", 0, [1, 2, 3]),
        ("destroy-zero", "zero-destroyed", 0, [1, 2]),
        ("step", "frames", 4, [1, 2]),
        ("destroy-b", "b-destroyed", 4, [1]),
        ("replace", "replacement", 8, [1, 4]),
        ("destroy-a", "tail-destroyed", 8, [4]),
        ("disconnect", "disconnected", 8, [4]),
    ]
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        listener.listen(1)
        listener.settimeout(15)

        def receive():
            try:
                connection, _ = listener.accept()
                with connection:
                    connection.settimeout(20)
                    while chunk := connection.recv(65536):
                        with condition:
                            received.extend(chunk)
                            condition.notify_all()
            except Exception as error:
                receiver_errors.append(str(error))
            finally:
                receiver_done.set()
                with condition:
                    condition.notify_all()

        receiver = threading.Thread(target=receive, daemon=True)
        receiver.start()
        process = subprocess.Popen(
            [str(args.producer.resolve()), "--lifecycle", str(capture.resolve()),
             str(truth_path.resolve()), "--host", "127.0.0.1", "--port",
             str(listener.getsockname()[1]), "--step-control"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        )

        def read_stdout():
            for line in process.stdout:
                lines.put(line.strip())
            lines.put(None)

        reader = threading.Thread(target=read_stdout, daemon=True)
        reader.start()
        try:
            for command, name, frames, alive in checkpoints:
                if command:
                    process.stdin.write(command + "\n")
                    process.stdin.flush()
                line = lines.get(timeout=15)
                if line is None:
                    raise RuntimeError("producer exited before " + name + ": " + process.stderr.read())
                fields = line.split()
                if len(fields) != 4 or fields[:2] != ["PVD_ROOMS", name]:
                    raise RuntimeError("unexpected producer response: " + line)
                count, actual_frames = map(int, fields[2:])
                if actual_frames != frames:
                    raise RuntimeError("unexpected completed frame count at " + name)
                deadline = time.monotonic() + 15
                with condition:
                    while len(received) < count:
                        remaining = deadline - time.monotonic()
                        if remaining <= 0 or receiver_done.is_set():
                            raise RuntimeError("TCP did not reach checkpoint " + name)
                        condition.wait(remaining)
                truth = json.loads(truth_path.read_text(encoding="utf-8"))
                expected = {"name": name, "bytes": count, "frames": frames, "alive": alive}
                if truth["checkpoints"][-1] != expected:
                    raise RuntimeError("SDK truth checkpoint mismatch at " + name)
            if not receiver_done.wait(timeout=15):
                raise RuntimeError("explicit disconnect did not close TCP")
            process.stdin.write("finish\n")
            process.stdin.flush()
            process.stdin.close()
            code = process.wait(timeout=15)
            errors = process.stderr.read()
            if code or errors or receiver_errors:
                raise RuntimeError(f"producer exit {code}: {errors}; receiver: {receiver_errors}")
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=15)
            process.stdout.close()
            process.stderr.close()
            if not process.stdin.closed:
                process.stdin.close()
        receiver.join(timeout=1)
        reader.join(timeout=1)
    payload = bytes(received)
    if payload != capture.read_bytes():
        raise RuntimeError("TCP bytes differ from the same-run transport mirror")
    (args.output / "received.pvdstream").write_bytes(payload)
    result = {
        "scope": "SDK transport checkpoints; decoder and UI validation are separate",
        "bytes": len(payload), "sha256": hashlib.sha256(payload).hexdigest(),
        "checkpoints": truth["checkpoints"],
        "replacement_reused_address": truth["replacement_reused_address"],
        "frames": len(truth["frames"]),
    }
    (args.output / "transport-check.json").write_text(
        json.dumps(result, indent=2) + "\n", encoding="utf-8",
    )
    print(json.dumps(result))


if __name__ == "__main__":
    main()
