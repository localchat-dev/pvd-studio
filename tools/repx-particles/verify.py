import argparse
import datetime
import hashlib
import importlib.util
import json
import pathlib

spec = importlib.util.spec_from_file_location("joint_verifier", pathlib.Path(__file__).parents[1] / "repx-joints/verify.py")
shared = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shared)


def load(path):
    data = json.loads(path.read_text(encoding="utf-8"))
    assert data["schema"] == 2, path
    assert data["sdk_version"] == [3, 4, 2], path
    data["systems"].sort(key=lambda value: value["id"])
    return data


def verify_import(producer, xml, truth, prefix):
    sdk_execute([producer, "--import", xml, prefix.with_suffix(".json"), prefix.with_suffix(".xml")])
    return shared.compare(truth, load(prefix.with_suffix(".json")))


def sdk_execute(arguments):
    result = shared.execute(arguments)
    assert not result.stdout.strip(), result.stdout
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--cli", type=pathlib.Path, default=pathlib.Path("target/debug/pvd-cli.exe"))
    parser.add_argument("--producer", type=pathlib.Path, default=pathlib.Path(".deps/physx-repx-particles-build/Debug/pvd-repx-particles.exe"))
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--producer-only", action="store_true")
    args = parser.parse_args()
    output = args.output or pathlib.Path("output/repx-particle-validation") / datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%d-%H%M%S-%f")
    output.mkdir(parents=True, exist_ok=False)
    capture = output / "capture.pvdstream"
    sdk_execute([args.producer, capture, output])
    official = whole = selected = exports = empty_exports = 0
    client_flags = (0, 1, 2, 3, 4, 5, 6, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 0)
    expected_counts = ([5, 32], [5, 32], [3, 32], [3, 31], [1, 28], [2, 32], [0, 0]) + ([2, 3],) * 11
    covered = {name: set() for name in ("PxParticleSystem", "PxParticleFluid")}
    if not args.producer_only:
        assert "verified_frames: 18" in shared.execute([args.cli, "verify", capture]).stdout.splitlines()
    for frame in range(18):
        truth = load(output / f"frame-{frame}-truth.json")
        assert len(truth["systems"]) == 2
        assert sorted(system["count"] for system in truth["systems"]) == expected_counts[frame]
        assert {system["type"] for system in truth["systems"]} == set(covered)
        for system in truth["systems"]:
            assert type(system["client_flags"]) is int and system["client_flags"] == client_flags[frame], (frame, system)
            if frame != 6:
                covered[system["type"]].add(system["client_flags"])
        if frame < 4:
            official += verify_import(args.producer, output / f"frame-{frame}-reference.xml", truth, output / f"frame-{frame}-official-import")
        elif frame == 6:
            result = shared.execute([args.producer, "--import", output / f"frame-{frame}-reference.xml", output / "empty-import.json", output / "empty-import.xml"], succeed=False)
            diagnostics = result.stdout + result.stderr
            assert 'ValidParticleRange' in diagnostics and 'zero' in diagnostics
        else:
            assert any(system["range"] > system["count"] for system in truth["systems"])
        if args.producer_only:
            continue
        xml = output / f"frame-{frame}-exported.xml"
        shared.execute([args.cli, "repx", capture, frame, xml])
        whole += verify_import(args.producer, xml, truth, output / f"frame-{frame}-exported-import")
        if frame == 6:
            empty_exports += 1
        for index, system in enumerate(truth["systems"]):
            xml = output / f"frame-{frame}-selected-{index}.xml"
            shared.execute([args.cli, "repx", capture, frame, xml, system["id"]])
            selected += verify_import(args.producer, xml, {**truth, "systems": [system]}, output / f"frame-{frame}-selected-{index}-import")
            exports += 1
            if frame == 6:
                empty_exports += 1
    assert all(flags == set(range(16)) for flags in covered.values()), covered
    result = {"schema": 2, "sdk_commit": "5e42a5f112351a223c19c17bb331e6c55037b8eb", "frames": 18,
              "client_flags_by_frame": client_flags, "client_flag_combinations_per_type": {name: sorted(flags) for name, flags in covered.items()},
              "final_client_flags": [system["client_flags"] for system in truth["systems"]],
              "capture_bytes": capture.stat().st_size, "capture_sha256": hashlib.sha256(capture.read_bytes()).hexdigest(),
              "official_getter_values": official, "whole_getter_values": whole,
              "selected_getter_values": selected, "selected_exports": exports, "empty_exports": empty_exports,
              "official_dense_frames": 4, "official_empty_rejected": True, "producer_only": args.producer_only}
    (output / "result.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result))


if __name__ == "__main__":
    main()
