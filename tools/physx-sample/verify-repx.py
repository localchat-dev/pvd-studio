import argparse
import csv
import datetime
import math
import pathlib
import subprocess
import xml.etree.ElementTree as ET


def execute(arguments, succeed=True):
    result = subprocess.run([str(value) for value in arguments], capture_output=True, text=True)
    if succeed and result.returncode:
        raise AssertionError(f"Command failed: {arguments}\n{result.stdout}\n{result.stderr}")
    if not succeed and not result.returncode:
        raise AssertionError(f"Command unexpectedly succeeded: {arguments}")
    return result


def leaves(root, prefix=""):
    result = {}
    for index, child in enumerate(root):
        label = child.tag
        if child.find("Id") is not None:
            label += f"[{child.findtext('Id')}]"
        elif sum(sibling.tag == child.tag for sibling in root) > 1:
            label += f"[{index}]"
        path = prefix + "/" + label
        if len(child):
            result.update(leaves(child, path))
        else:
            result[path] = (child.text or "").strip()
    return result


def compare_roundtrip(expected_path, actual_path, selector=None):
    expected_root = ET.parse(expected_path).getroot()
    actual_root = ET.parse(actual_path).getroot()
    assert expected_root.tag == actual_root.tag == "PhysX30Collection"
    assert expected_root.attrib["version"] == actual_root.attrib["version"] == "3.4.2"
    if selector is not None:
        expected_root = expected_root.find(selector)
        actual_root = actual_root.find(selector)
        assert expected_root is not None and actual_root is not None, selector
    expected, actual = leaves(expected_root), leaves(actual_root)
    compared = 0
    for path, value in expected.items():
        if not value and path.rsplit("/", 1)[-1].endswith("Flags") and path not in actual:
            continue
        assert path in actual, f"SDK discarded property {path}: {value}"
        if value == actual[path]:
            compared += 1
            continue
        try:
            left = [float(part) for part in value.split()]
            right = [float(part) for part in actual[path].split()]
        except ValueError as error:
            raise AssertionError(f"SDK changed {path}: {value!r} -> {actual[path]!r}") from error
        assert len(left) == len(right), path
        assert all(math.isfinite(item) for item in left + right), path
        assert all(math.isclose(a, b, abs_tol=1e-5, rel_tol=1e-5) for a, b in zip(left, right)), (path, left, right)
        compared += 1
    assert not (actual.keys() - expected.keys()), "SDK supplied unspecified serialized properties"
    return compared


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--cli", type=pathlib.Path, required=True)
    parser.add_argument("--producer", type=pathlib.Path, required=True)
    parser.add_argument("--capture", type=pathlib.Path, required=True)
    parser.add_argument("--rich-capture", type=pathlib.Path)
    parser.add_argument("--output", type=pathlib.Path)
    args = parser.parse_args()
    output = args.output or pathlib.Path("output/repx-validation") / datetime.datetime.now().strftime("%Y%m%d-%H%M%S-%f")
    output.mkdir(parents=True, exist_ok=False)
    with args.capture.with_suffix(".csv").open(newline="", encoding="utf-8") as truth:
        rows = list(csv.DictReader(truth))
    box_id = next(row["actor_id"] for row in rows if row["frame"] == "60" and row["name"] == "PvdStudioBoxRenamed")
    total_properties = 0
    for frame, selected in [(0, None), (60, None), (90, None), (119, None), (60, box_id)]:
        name = f"frame-{frame}" + ("-actor" if selected else "")
        repx, roundtrip = output / f"{name}.xml", output / f"{name}-sdk.xml"
        command = [args.cli, "repx", args.capture, frame, repx]
        if selected:
            command.append(selected)
        execute(command)
        imported = execute([args.producer, "--import-repx", repx, "--roundtrip-repx", roundtrip])
        assert not imported.stderr.strip(), imported.stderr
        assert f"actors={1 if selected else 3}" in imported.stdout, imported.stdout
        assert f"shapes={1 if selected else 3}" in imported.stdout, imported.stdout
        assert "materials=1" in imported.stdout, imported.stdout
        compared = compare_roundtrip(repx, roundtrip)
        total_properties += compared
        print(f"{name}: SDK imported actors/shapes/materials; {compared} serialized properties matched")
    if args.rich_capture:
        repx, roundtrip = output / "rich-frame-0.xml", output / "rich-frame-0-sdk.xml"
        execute([args.cli, "repx", args.rich_capture, 0, repx])
        imported = execute([args.producer, "--import-repx", repx, "--roundtrip-repx", roundtrip])
        assert not imported.stderr.strip(), imported.stderr
        assert "actors=6 shapes=6 materials=1" in imported.stdout, imported.stdout
        original_root, imported_root = ET.parse(repx).getroot(), ET.parse(roundtrip).getroot()
        assert len(original_root.findall("PxSphericalJoint")) == len(imported_root.findall("PxSphericalJoint")) == 1
        compared = compare_roundtrip(repx, roundtrip, "PxSphericalJoint")
        total_properties += compared
        selected = output / "unsupported-connected-actor.xml"
        actor_id = original_root.findtext("PxSphericalJoint/Actors/actor1")
        result = execute([args.cli, "repx", args.rich_capture, 0, selected, actor_id], succeed=False)
        assert "RepX" in result.stderr, result.stderr
        assert not selected.exists(), "Unsupported connected Actor export left an output file"
        print(f"rich frame: SDK imported all 6 Actors and spherical joint; {compared} joint properties matched; connected Actor selection explicitly refused")
    missing = output / "missing-actor.xml"
    execute([args.cli, "repx", args.capture, 0, missing, 1], succeed=False)
    assert not missing.exists()
    print(f"Verified {total_properties} properties through the independent PhysX 3.4.2 importer; artifacts: {output}")


if __name__ == "__main__":
    main()
