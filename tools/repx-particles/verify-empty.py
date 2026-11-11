import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import struct
import xml.etree.ElementTree as ET

spec = importlib.util.spec_from_file_location("particle_verifier", Path(__file__).with_name("verify.py"))
shared = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shared)

NAMES = (
    "cycle-system", "cycle-system-rest", "cycle-fluid", "cycle-fluid-rest",
    "empty-system-default", "empty-system-rest-read", "empty-fluid-density", "empty-fluid-rest-read",
)
READ_FLAGS = (11, 15, 11, 15, 9, 63, 73, 127)


def float_bits(expected, actual):
    if isinstance(expected, dict):
        return sum(float_bits(value, actual[key]) for key, value in expected.items())
    if isinstance(expected, list):
        return sum(float_bits(left, right) for left, right in zip(expected, actual))
    if isinstance(expected, float):
        assert struct.pack("<f", expected) == struct.pack("<f", actual), (expected, actual)
        return 1
    return 0


def check_xml(xml, truth):
    collection = ET.parse(xml).getroot()
    actors = [value for value in collection if value.tag in ("PxParticleSystem", "PxParticleFluid")]
    assert len(actors) == len(truth["systems"])
    by_id = {int(actor.findtext("Id")): actor for actor in actors}
    assert len(by_id) == len(actors)
    empty = 0
    for system in truth["systems"]:
        actor = by_id[system["id"]]
        assert actor.tag == system["type"]
        assert int(actor.findtext("NbParticles")) == system["count"]
        if system["count"]:
            assert int(actor.findtext("ValidParticleRange")) == system["range"]
            bitmap = [0] * ((system["range"] + 31) // 32)
            for particle in system["particles"]:
                bitmap[particle["index"] // 32] |= 1 << (particle["index"] % 32)
            assert [int(word) for word in actor.findtext("ValidParticleBitmap").split()] == bitmap
            continue
        empty += 1
        assert actor.findtext("ValidParticleRange") == "1"
        assert actor.findtext("ValidParticleBitmap") == "0"
        assert all(actor.find(field) is None for field in ("Positions", "Velocities", "Flags"))
        assert actor.findtext("RestOffsets") == ("0" if system["base_flags"] & 16 else None)
    return empty


def verify_import(producer, xml, truth, prefix):
    empty = check_xml(xml, truth)
    shared.sdk_execute([producer, "--import", xml, prefix.with_suffix(".json"), prefix.with_suffix(".xml")])
    actual = shared.load(prefix.with_suffix(".json"))
    comparisons = shared.shared.compare(truth, actual)
    bits = float_bits(truth, actual)
    assert sum(system["count"] == 0 and system["range"] == 0 and not system["particles"] for system in actual["systems"]) == empty
    return comparisons, bits, empty


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--cli", type=Path, default=Path("target/debug/pvd-cli.exe"))
    parser.add_argument("--producer", type=Path, default=Path(".deps/physx-repx-particles-build/Debug/pvd-repx-particles.exe"))
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    capture = args.output / "capture.pvdstream"
    shared.sdk_execute([args.producer, "--empty", capture, args.output])
    assert "verified_frames: 4" in shared.shared.execute([args.cli, "verify", capture]).stdout.splitlines()
    comparisons = bits = empty_imports = selected_exports = 0
    empty_clients = set()
    for frame in range(4):
        truth = shared.load(args.output / f"frame-{frame}-truth.json")
        assert len(truth["systems"]) == 8
        by_name = {system["name"]: system for system in truth["systems"]}
        assert set(by_name) == set(NAMES)
        for index, name in enumerate(NAMES):
            system = by_name[name]
            active = index < 4 and frame in (1, 3)
            assert system["type"] == ("PxParticleFluid" if index % 4 >= 2 else "PxParticleSystem")
            assert system["capacity"] == 64
            assert system["count"] == (3 if active else 0)
            assert system["range"] == (64 if active else 0)
            assert system["read_flags"] == READ_FLAGS[index]
            assert system["client_flags"] == frame * 4 + index % 4
            assert system["actor_flags"] == frame % 2
            assert system["base_flags"] == 38 | (17 if index % 2 else 0) | (8 if index % 4 >= 2 else 0)
            assert [particle["index"] for particle in system["particles"]] == ([0, 31, 63] if active and frame == 1 else [1, 32, 63] if active else [])
            if not active:
                empty_clients.add(system["client_flags"])
        xml = args.output / f"frame-{frame}-exported.xml"
        shared.shared.execute([args.cli, "repx", capture, frame, xml])
        values, floats, empty = verify_import(args.producer, xml, truth, args.output / f"frame-{frame}-imported")
        comparisons += values
        bits += floats
        empty_imports += empty
        for index, system in enumerate(truth["systems"]):
            xml = args.output / f"frame-{frame}-selected-{index}.xml"
            shared.shared.execute([args.cli, "repx", capture, frame, xml, system["id"]])
            values, floats, empty = verify_import(args.producer, xml, {**truth, "systems": [system]}, args.output / f"frame-{frame}-selected-{index}-imported")
            comparisons += values
            bits += floats
            empty_imports += empty
            selected_exports += 1
    assert empty_clients == set(range(16))
    assert selected_exports == 32 and empty_imports == 48
    result = {
        "schema": 1, "sdk_commit": "5e42a5f112351a223c19c17bb331e6c55037b8eb",
        "frames": 4, "systems_per_frame": 8, "whole_exports": 4, "selected_exports": selected_exports,
        "empty_system_imports": empty_imports, "empty_client_flags": sorted(empty_clients),
        "read_flags": READ_FLAGS, "getter_values": comparisons, "exact_f32_values": bits,
        "capture_bytes": capture.stat().st_size, "capture_sha256": hashlib.sha256(capture.read_bytes()).hexdigest(),
        "producer_sha256": hashlib.sha256(args.producer.read_bytes()).hexdigest(),
        "cli_sha256": hashlib.sha256(args.cli.read_bytes()).hexdigest(),
    }
    (args.output / "result.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result))


if __name__ == "__main__":
    main()
