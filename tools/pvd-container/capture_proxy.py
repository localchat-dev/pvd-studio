"""Retain the exact SDK bytes forwarded to the local PVD3 application."""

import argparse
import hashlib
import json
import socket
from pathlib import Path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("output", type=Path)
    parser.add_argument("--listen-port", type=int, default=15445)
    parser.add_argument("--target-port", type=int, default=5425)
    parser.add_argument("--timeout", type=float, default=60)
    args = parser.parse_args()
    for port in (args.listen_port, args.target_port):
        if not 1 <= port <= 65535:
            parser.error("ports must be between 1 and 65535")
    if args.timeout <= 0:
        parser.error("timeout must be positive")
    digest = hashlib.sha256()
    count = 0
    partial = args.output.with_suffix(args.output.suffix + ".partial")
    if args.output.exists():
        parser.error("capture output already exists")
    with partial.open("xb") as capture, socket.socket() as listener:
        listener.settimeout(args.timeout)
        listener.bind(("127.0.0.1", args.listen_port))
        listener.listen(1)
        print(f"Listening on 127.0.0.1:{args.listen_port}", flush=True)
        source, _ = listener.accept()
        with source, socket.create_connection(("127.0.0.1", args.target_port), args.timeout) as target:
            source.settimeout(args.timeout)
            while data := source.recv(65536):
                if count + len(data) > 512 * 1024 * 1024:
                    raise ValueError("capture exceeds 512 MiB")
                target.sendall(data)
                capture.write(data)
                digest.update(data)
                count += len(data)
            target.shutdown(socket.SHUT_WR)
    partial.rename(args.output)
    print(json.dumps({"path": str(args.output.resolve()), "bytes": count, "sha256": digest.hexdigest()}), flush=True)


if __name__ == "__main__":
    main()
