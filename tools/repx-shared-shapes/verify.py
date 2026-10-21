import argparse
import hashlib
import json
import math
import pathlib
import subprocess
import xml.etree.ElementTree as ET


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def compare(expected, actual, path=""):
    if isinstance(expected, dict):
        assert expected.keys() == actual.keys(), (path, expected.keys(), actual.keys())
        for key in expected:
            compare(expected[key], actual[key], path + "/" + key)
    elif isinstance(expected, list):
        assert len(expected) == len(actual), (path, len(expected), len(actual))
        for index, (left, right) in enumerate(zip(expected, actual)):
            compare(left, right, path + f"/{index}")
    elif isinstance(expected, float) or isinstance(actual, float):
        assert math.isfinite(actual) and math.isclose(expected, actual, rel_tol=1e-5, abs_tol=1e-6), (path, expected, actual)
    else:
        assert expected == actual, (path, expected, actual)


def subset(truth, actor_id):
    actors = [a for a in truth["actors"] if actor_id is None or a["id"] == actor_id]
    ids = {a["id"] for a in actors}
    shared = [dict(entry, actors=[i for i in entry["actors"] if i in ids]) for entry in truth["shared"]]
    shared = [entry for entry in shared if entry["actors"]]
    return {
        "actors": actors,
        "unique_shapes": len(shared) + sum(s["exclusive"] for a in actors for s in a["shapes"]),
        "attachments": sum(len(a["shapes"]) for a in actors),
        "shared": shared,
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--cli", required=True, type=pathlib.Path)
    parser.add_argument("--producer", required=True, type=pathlib.Path)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--produce-only", action="store_true")
    parser.add_argument("--existing-capture", action="store_true")
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=args.existing_capture)
    commands = []

    def execute(arguments, name):
        result = subprocess.run([str(p) for p in arguments], capture_output=True, text=True, creationflags=subprocess.CREATE_NO_WINDOW)
        (args.output / (name + ".stdout.log")).write_text(result.stdout, encoding="utf-8")
        (args.output / (name + ".stderr.log")).write_text(result.stderr, encoding="utf-8")
        commands.append({"argv": [str(p) for p in arguments], "exit": result.returncode})
        (args.output / "commands.json").write_text(json.dumps(commands, indent=2), encoding="utf-8")
        assert result.returncode == 0 and not result.stderr.strip(), (name, result.returncode, result.stdout, result.stderr)

    capture = args.output / "shared.pvdstream"
    if not args.existing_capture:
        execute([args.producer, capture, args.output / "sdk"], "producer")
    if args.produce_only:
        return
    rows = []
    for frame in range(5):
        truth = json.loads((args.output / f"sdk.frame-{frame}.json").read_text())
        reference = args.output / f"sdk.frame-{frame}.xml"
        observed = args.output / f"reference-{frame}.json"
        execute([args.producer, "--import", reference, observed, args.output / f"reference-{frame}-roundtrip.xml"], f"reference-{frame}")
        compare(truth, json.loads(observed.read_text()))
        for actor_id in [None] + [a["id"] for a in truth["actors"]]:
            name = f"frame-{frame}-" + ("all" if actor_id is None else str(actor_id))
            exported, actual, roundtrip = [args.output / (name + suffix) for suffix in [".xml", ".json", "-roundtrip.xml"]]
            command = [args.cli, "repx", capture, frame, exported]
            if actor_id is not None:
                command.append(actor_id)
            execute(command, name + "-export")
            execute([args.producer, "--import", exported, actual, roundtrip], name + "-import")
            expected = subset(truth, actor_id)
            compare(expected, json.loads(actual.read_text()))
            root = ET.parse(exported).getroot()
            assert [int(s.findtext("Id")) for s in root.findall("PxShape")] == [s["id"] for s in expected["shared"]]
            for actor in expected["actors"]:
                xml_actor = next(a for a in root if a.tag in ["PxRigidStatic", "PxRigidDynamic"] and int(a.findtext("Id")) == actor["id"])
                assert [s.tag for s in xml_actor.find("Shapes")] == ["PxShape" if s["exclusive"] else "PxShapeRef" for s in actor["shapes"]]
            rows.append({"frame": frame, "actor": actor_id, "actors": len(expected["actors"]), "unique_shapes": expected["unique_shapes"], "attachments": expected["attachments"], "xml_sha256": digest(exported)})
    result = {"status": "passed", "scope": "real PhysX 3.4.2 SDK capture and getter import; original PVD3 UI unverified", "capture_sha256": digest(capture), "capture_bytes": capture.stat().st_size, "producer_sha256": digest(args.producer), "cli_sha256": digest(args.cli), "official_reference_imports": 5, "candidate_imports": len(rows), "cases": rows}
    (args.output / "summary.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
