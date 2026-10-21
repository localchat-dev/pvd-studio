import argparse
import copy
import datetime
import hashlib
import json
import math
import pathlib
import platform
import subprocess
import uuid
import xml.etree.ElementTree as ET


def execute(arguments, succeed=True):
    result = subprocess.run(
        [str(part) for part in arguments], capture_output=True, text=True, timeout=60,
        creationflags=subprocess.CREATE_NO_WINDOW if platform.system() == "Windows" else 0,
    )
    if succeed and (result.returncode or result.stderr.strip()):
        raise AssertionError(
            f"Command failed or diagnosed invalid input: {arguments}\n{result.stdout}\n{result.stderr}"
        )
    if not succeed and not result.returncode:
        raise AssertionError(f"Command unexpectedly succeeded: {arguments}")
    return result


def compare(expected, actual, path=""):
    if isinstance(expected, dict):
        assert isinstance(actual, dict) and expected.keys() == actual.keys(), (
            path, expected, actual
        )
        return sum(compare(expected[key], actual[key], path + "/" + key) for key in expected)
    if isinstance(expected, list):
        assert isinstance(actual, list) and len(expected) == len(actual), (path, expected, actual)
        return sum(
            compare(left, right, path + f"/{index}")
            for index, (left, right) in enumerate(zip(expected, actual))
        )
    if isinstance(expected, (int, float)) and isinstance(actual, (int, float)) and (
        isinstance(expected, float) or isinstance(actual, float)
    ):
        assert math.isfinite(expected) and math.isfinite(actual), path
        assert math.isclose(expected, actual, rel_tol=1e-5, abs_tol=1e-6), (path, expected, actual)
    else:
        assert expected == actual, (path, expected, actual)
    return 1


def load(path):
    result = json.loads(path.read_text(encoding="utf-8"))
    result["actors"].sort(key=lambda actor: actor["name"])
    result["joints"].sort(key=lambda joint: joint["name"])
    return result


def by_names(values):
    result = copy.deepcopy(values)
    names = {actor["id"]: actor["name"] for actor in result["actors"]}
    assert len(names) == len(result["actors"])
    assert len(set(names.values())) == len(names)
    names[0] = "<world>"
    for actor in result["actors"]:
        del actor["id"]
    for joint in result["joints"]:
        del joint["id"]
        for key in ("actors", "constraint_actors"):
            joint[key] = [names[identifier] for identifier in joint[key]]
    return result


def validate_dependencies(values, expected_count):
    assert len(values["joints"]) == expected_count, values
    actors = {actor["id"]: actor for actor in values["actors"]}
    joint_ids = set()
    for joint in values["joints"]:
        assert joint["id"] > 0 and joint["id"] not in actors and joint["id"] not in joint_ids
        joint_ids.add(joint["id"])
        assert joint["actors"] == joint["constraint_actors"], joint
        assert joint["actors"][0] != joint["actors"][1], joint
        assert all(identifier == 0 or identifier in actors for identifier in joint["actors"]), joint
        assert any(actors.get(identifier, {}).get("type") == "PxRigidDynamic" for identifier in joint["actors"]), joint


def import_and_compare(producer, input_path, expected, output_prefix, named_ids=False):
    observed = output_prefix.with_suffix(".json")
    roundtrip = output_prefix.with_suffix(".xml")
    execute([producer, "--import", input_path, observed, roundtrip])
    actual = load(observed)
    validate_dependencies(actual, len(expected["joints"]))
    if named_ids:
        expected, actual = by_names(expected), by_names(actual)
    return compare(expected, actual)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--cli", type=pathlib.Path, default=pathlib.Path("target/debug/pvd-cli.exe"))
    parser.add_argument("--producer", type=pathlib.Path, default=pathlib.Path(".deps/physx-repx-joints-build/Debug/pvd-repx-joints.exe"))
    parser.add_argument("--rich-producer", type=pathlib.Path)
    parser.add_argument("--producer-only", action="store_true")
    parser.add_argument("--expanded", action="store_true")
    parser.add_argument("--output", type=pathlib.Path)
    args = parser.parse_args()
    timestamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%d-%H%M%S-%f")
    output = args.output or pathlib.Path("output/repx-joint-validation") / (timestamp + "-" + uuid.uuid4().hex[:8])
    output.mkdir(parents=True, exist_ok=False)
    capture = output / "joints.pvdstream"
    execute([args.producer, *(["--expanded"] if args.expanded else []), capture, output])
    if not args.producer_only:
        replay = execute([args.cli, "verify", capture])
        assert "verified_frames: 3\n" in replay.stdout, replay.stdout
    expected_frames = []
    official_values, exported_values = 0, 0
    counts = (8, 8, 2) if args.expanded else (3, 3, 2)
    for frame, count in enumerate(counts):
        expected = load(output / f"frame-{frame}-truth.json")
        expected_frames.append(expected)
        validate_dependencies(expected, count)
        assert len(expected["actors"]) == 4
        reference = output / f"frame-{frame}-reference.xml"
        assert sum(child.tag.endswith("Joint") for child in ET.parse(reference).getroot()) == count
        official_values += import_and_compare(
            args.producer, reference, expected, output / f"frame-{frame}-reference-import"
        )
        if not args.producer_only:
            exported = output / f"frame-{frame}-exported.xml"
            execute([args.cli, "repx", capture, frame, exported])
            assert sum(child.tag.endswith("Joint") for child in ET.parse(exported).getroot()) == count
            exported_values += import_and_compare(
                args.producer, exported, expected, output / f"frame-{frame}-exported-import"
            )
        print(f"frame {frame}: {count} joints; actor dependencies, poses, limits and flags match SDK getters")
    first = {joint["name"]: joint for joint in expected_frames[0]["joints"]}
    second = {joint["name"]: joint for joint in expected_frames[1]["joints"]}
    assert first["body-joint"]["id"] == second["body-joint-updated"]["id"]
    assert first["body-joint"]["local_pose"] != second["body-joint-updated"]["local_pose"]
    assert first["body-joint"]["limit"] != second["body-joint-updated"]["limit"]
    assert first["body-joint"]["spherical_flags"] != second["body-joint-updated"]["spherical_flags"]
    assert "static-joint" not in {joint["name"] for joint in expected_frames[2]["joints"]}
    if args.expanded:
        for name, kind in (("fixed-joint", "PxFixedJoint"), ("distance-joint", "PxDistanceJoint"),
                           ("revolute-joint", "PxRevoluteJoint"), ("prismatic-joint", "PxPrismaticJoint"),
                           ("d6-joint", "PxD6Joint")):
            assert first[name]["type"] == kind and second[name]["type"] == kind
            assert first[name]["id"] == second[name]["id"]
            assert first[name]["local_pose"] != second[name]["local_pose"]
            assert name not in {joint["name"] for joint in expected_frames[2]["joints"]}
        assert first["d6-joint"]["motion"] != second["d6-joint"]["motion"]
        assert first["d6-joint"]["drives"] != second["d6-joint"]["drives"]
    types = {actor["id"]: actor["type"] for actor in expected_frames[0]["actors"]}
    assert first["world-joint"]["actors"][0] == 0
    assert all(types[identifier] == "PxRigidDynamic" for identifier in first["body-joint"]["actors"])
    assert [types[identifier] for identifier in first["static-joint"]["actors"]] == ["PxRigidStatic", "PxRigidDynamic"]
    if not args.producer_only:
        rejected = output / "connected-actor-rejected.xml"
        selected = first["world-joint"]["actors"][1]
        result = execute([args.cli, "repx", capture, 0, rejected, selected], succeed=False)
        assert "RepX" in result.stderr, result.stderr
        assert not rejected.exists(), "Rejected Actor export left an output file"
    rich_values = 0
    if args.rich_producer:
        rich_capture, reference = output / "rich.pvdstream", output / "rich-reference.xml"
        execute([args.rich_producer, "--file", rich_capture, "--rich", "--frames", 3,
                 "--export-repx", reference, "--export-frame", 0])
        rich_truth = output / "rich-reference-getters.json"
        execute([args.producer, "--import", reference, rich_truth, output / "rich-reference-roundtrip.xml"])
        expected = load(rich_truth)
        validate_dependencies(expected, 1)
        if not args.producer_only:
            exported = output / "rich-exported.xml"
            execute([args.cli, "repx", rich_capture, 0, exported])
            assert len(ET.parse(exported).getroot().findall("PxSphericalJoint")) == 1
            rich_values = import_and_compare(args.producer, exported, expected, output / "rich-exported-import", named_ids=True)
            print("rich frame: spherical joint and Actor dependencies match getters from independent SDK reference XML")
    result = {
        "sdk_commit": "5e42a5f112351a223c19c17bb331e6c55037b8eb",
        "sdk_version": "3.4.2", "producer_configuration": "Debug with checked SDK libraries",
        "machine": platform.platform(), "producer_only": args.producer_only,
        "capture_bytes": capture.stat().st_size,
        "capture_sha256": hashlib.sha256(capture.read_bytes()).hexdigest(),
        "frames": 3, "expanded": args.expanded, "joint_counts": list(counts),
        "replay_verified_frames": 0 if args.producer_only else 3,
        "official_roundtrip_getter_values": official_values,
        "rust_export_getter_values": exported_values,
        "rich_export_getter_values": rich_values,
    }
    (output / "result.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    scope = "SDK producer/importer self-check" if args.producer_only else "Rust export -> SDK importer"
    print(f"Verified {scope}; artifacts: {output}")


if __name__ == "__main__":
    main()
