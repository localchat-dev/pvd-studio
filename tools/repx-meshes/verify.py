import argparse
import datetime
import json
import math
import pathlib
import subprocess
import xml.etree.ElementTree as ET


def execute(arguments):
    result = subprocess.run([str(part) for part in arguments], capture_output=True, text=True)
    if result.returncode or result.stderr.strip():
        raise AssertionError(f"Command failed or SDK diagnosed invalid input: {arguments}\n{result.stdout}\n{result.stderr}")
    return result


def compare(expected, actual, path=""):
    if isinstance(expected, dict):
        assert expected.keys() == actual.keys(), (path, expected.keys(), actual.keys())
        for key in expected:
            compare(expected[key], actual[key], path + "/" + key)
    elif isinstance(expected, list):
        assert len(expected) == len(actual), (path, len(expected), len(actual))
        for index, (left, right) in enumerate(zip(expected, actual)):
            compare(left, right, path + f"/{index}")
    elif isinstance(expected, int):
        assert expected == actual, (path, expected, actual)
    elif isinstance(expected, float):
        assert math.isfinite(expected) and math.isfinite(actual), path
        assert math.isclose(expected, actual, rel_tol=1e-5, abs_tol=1e-6), (path, expected, actual)
    else:
        assert expected == actual, (path, expected, actual)


def load(path):
    result = json.loads(path.read_text(encoding="utf-8"))
    result["actors"].sort(key=lambda actor: actor["name"])
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--cli", type=pathlib.Path, default=pathlib.Path("target/debug/pvd-cli.exe"))
    parser.add_argument("--producer", type=pathlib.Path, default=pathlib.Path(".deps/physx-repx-meshes-build/Release/pvd-repx-meshes.exe"))
    parser.add_argument("--output", type=pathlib.Path)
    args = parser.parse_args()
    output = args.output or pathlib.Path("output/repx-mesh-validation") / datetime.datetime.now().strftime("%Y%m%d-%H%M%S-%f")
    output.mkdir(parents=True, exist_ok=False)
    capture, reference, truth = [output / name for name in ["meshes.pvdstream", "sdk-reference.xml", "meshes.meshes.json"]]
    execute([args.producer, capture, reference, truth])
    expected = load(truth)
    reference_root = ET.parse(reference).getroot()
    assert len(reference_root.findall("PxBVH33TriangleMesh")) == 1
    assert len(reference_root.findall("PxConvexMesh")) == 1
    assert len(reference_root.findall("PxHeightField")) == 1
    assert len(reference_root.findall("PxMaterial")) == 2
    ids = {actor.findtext("Name"): actor.findtext("Id") for actor in reference_root.findall("PxRigidStatic")}
    assert len(ids) == 6
    for label, actor in [("frame", None)] + [(name, identifier) for name, identifier in sorted(ids.items())]:
        exported, observed, roundtrip = [output / (label + suffix) for suffix in [".xml", ".json", "-sdk.xml"]]
        command = [args.cli, "repx", capture, 0, exported]
        if actor is not None:
            command.append(actor)
        execute(command)
        execute([args.producer, "--import", exported, observed, roundtrip])
        wanted = expected if actor is None else {"actors": [item for item in expected["actors"] if item["name"] == label]}
        compare(wanted, load(observed))
        root = ET.parse(exported).getroot()
        assert len(root.findall("PxBVH33TriangleMesh")) == (1 if actor is None or label.startswith("triangle") else 0)
        assert len(root.findall("PxConvexMesh")) == (1 if actor is None or label.startswith("convex") else 0)
        assert len(root.findall("PxHeightField")) == (1 if actor is None or label.startswith("heightfield") else 0)
        print(f"{label}: SDK vertices, indices, convex mass/inertia, materials, poses, scale, flags and resource IDs match")
    print(f"Verified frame plus 6 Actor exports against independent SDK truth: {output}")


if __name__ == "__main__":
    main()
