import argparse
import copy
import datetime
import hashlib
import importlib.util
import json
import pathlib
import xml.etree.ElementTree as ET

spec = importlib.util.spec_from_file_location("joint_verifier", pathlib.Path(__file__).parents[1] / "repx-joints/verify.py")
shared = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shared)


def execute(arguments):
    result = shared.execute(arguments)
    assert not result.stdout.strip(), result.stdout
    return result


def load(path):
    data = json.loads(path.read_text(encoding="utf-8"))
    data["actors"].sort(key=lambda actor: actor["name"])
    return data


def import_snapshot(producer, xml, expected, prefix):
    execute([producer, "--import", xml, prefix.with_suffix(".json"), prefix.with_suffix(".xml")])
    return shared.compare(expected, load(prefix.with_suffix(".json")))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--cli", type=pathlib.Path, default=pathlib.Path("target/debug/pvd-cli.exe"))
    parser.add_argument("--producer", type=pathlib.Path, default=pathlib.Path(".deps/physx-repx-kinematic-shapes-build/Debug/pvd-repx-kinematic-shapes.exe"))
    parser.add_argument("--output", type=pathlib.Path)
    args = parser.parse_args()
    output = args.output or pathlib.Path("output/repx-kinematic-shapes-validation") / datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%d-%H%M%S-%f")
    output.mkdir(parents=True, exist_ok=False)
    execute([args.producer, "--zero-flags-probe", output])
    zero_source = load(output / "zero-flags-truth.json")
    assert len(zero_source["actors"]) == 1 and zero_source["actors"][0]["shapes"][0]["flags"] == 0 and zero_source["actors"][0]["actor_flags"] == 0
    reference_text = (output / "zero-flags-reference.xml").read_text(encoding="utf-8")
    reference_tree = ET.fromstring(reference_text)
    probe_shape = reference_tree.find("PxRigidStatic/Shapes/PxShape")
    probe_actor = reference_tree.find("PxRigidStatic")
    assert probe_shape is not None and probe_shape.find("Flags") is None
    assert probe_actor is not None and probe_actor.find("ActorFlags") is None
    assert reference_text.count("</PxShape>") == 1
    empty = output / "zero-flags-empty.xml"
    empty.write_text(reference_text.replace("</PxShape>", "<Flags></Flags></PxShape>").replace("</PxRigidStatic>", "<ActorFlags></ActorFlags></PxRigidStatic>"), encoding="utf-8")
    zero_results = {}
    for label, xml in (("omitted", output / "zero-flags-reference.xml"), ("empty", empty)):
        prefix = output / f"zero-flags-{label}-import"
        execute([args.producer, "--import", xml, prefix.with_suffix(".json"), prefix.with_suffix(".xml")])
        observed = load(prefix.with_suffix(".json"))
        flags = observed["actors"][0]["shapes"][0]["flags"]
        actor_flags = observed["actors"][0]["actor_flags"]
        assert flags == 11 and actor_flags == 1
        assert flags != zero_source["actors"][0]["shapes"][0]["flags"] and actor_flags != zero_source["actors"][0]["actor_flags"]
        remaining = copy.deepcopy(zero_source)
        remaining["actors"][0]["shapes"][0]["flags"] = 11
        remaining["actors"][0]["actor_flags"] = 1
        shared.compare(remaining, observed)
        zero_results[label] = {"actor_flags": actor_flags, "shape_flags": flags}
    capture = output / "capture.pvdstream"
    execute([args.producer, capture, output])
    fixture = json.loads((output / "fixture.json").read_text(encoding="utf-8"))
    assert fixture == {"schema": 1, "sdk_version": [3, 4, 2], "frames": 1, "actors": 8, "kinematic_actors": 5}
    expected = load(output / "sdk-truth.json")
    kinds = {"kin-plane": 1, "query-plane": 1, "kin-triangle-exclusive": 5,
             "kin-heightfield": 6, "kin-shared-a": 5, "kin-shared-b": 5, "static-shared": 5, "zero-flags-box": 3}
    assert {actor["name"] for actor in expected["actors"]} == set(kinds)
    shared_ids = []
    resources = {5: set(), 6: set()}
    for actor in expected["actors"]:
        name = actor["name"]
        assert len(actor["shapes"]) == 1
        shape = actor["shapes"][0]
        assert shape["type"] == kinds[name]
        assert shape["flags"] == (0 if name == "zero-flags-box" else 10 if name == "query-plane" else 11)
        assert actor["actor_flags"] == (0 if name == "zero-flags-box" else 1)
        assert shape["exclusive"] == (name not in ("kin-shared-a", "kin-shared-b", "static-shared"))
        assert len(shape["materials"]) == (1 if kinds[name] in (1, 3) else 2)
        if name not in ("static-shared", "zero-flags-box"):
            assert actor["body_flags"] == (0 if name == "query-plane" else 3)
            shared.compare(2.5, actor["mass"])
            shared.compare([3, 4, 5], actor["inertia"])
        if not shape["exclusive"]:
            assert shape["shared_id"] > 0
            shared_ids.append(shape["shared_id"])
        if kinds[name] in resources:
            resources[kinds[name]].add(shape["resource"])
    assert len(shared_ids) == 3 and len(set(shared_ids)) == 1
    assert all(len(values) == 1 for values in resources.values())
    assert "verified_frames: 1" in shared.execute([args.cli, "verify", capture]).stdout.splitlines()
    reference = output / "sdk-reference.xml"
    root = ET.parse(reference).getroot()
    ids = {actor.findtext("Name"): actor.findtext("Id") for actor in root if actor.tag in ("PxRigidDynamic", "PxRigidStatic")}
    assert set(ids) == set(kinds)
    execute([args.producer, "--import", reference, output / "official-import.json", output / "official-import.xml"])
    official_observed = load(output / "official-import.json")
    ordinary_expected = {"actors": [actor for actor in expected["actors"] if actor["name"] != "zero-flags-box"]}
    ordinary_observed = {"actors": [actor for actor in official_observed["actors"] if actor["name"] != "zero-flags-box"]}
    official = shared.compare(ordinary_expected, ordinary_observed)
    zero_original = copy.deepcopy(next(actor for actor in expected["actors"] if actor["name"] == "zero-flags-box"))
    zero_actual = next(actor for actor in official_observed["actors"] if actor["name"] == "zero-flags-box")
    assert zero_actual["actor_flags"] == 1 and zero_actual["shapes"][0]["flags"] == 11
    zero_original["actor_flags"] = 1
    zero_original["shapes"][0]["flags"] = 11
    shared.compare(zero_original, zero_actual)
    compared = exports = 0
    for name, identifier in [("frame", None)] + sorted(ids.items()):
        xml = output / f"{name}-exported.xml"
        arguments = [args.cli, "repx", capture, 0, xml]
        if identifier is not None:
            arguments.append(identifier)
        result = shared.execute(arguments)
        kinematic = 5 if identifier is None else int(name.startswith("kin-"))
        assert f"kinematic_actors: {kinematic}" in result.stdout.splitlines()
        assert ("kinematic_state: targets, velocities and wake counters are not restored" in result.stdout.splitlines()) == bool(kinematic)
        wanted = expected if identifier is None else {"actors": [actor for actor in expected["actors"] if actor["name"] == name]}
        compared += import_snapshot(args.producer, xml, wanted, output / f"{name}-import")
        tree = ET.parse(xml).getroot()
        mesh_count = 1 if identifier is None or kinds[name] == 5 else 0
        field_count = 1 if identifier is None or kinds[name] == 6 else 0
        assert len(tree.findall("PxBVH33TriangleMesh")) == mesh_count
        assert len(tree.findall("PxHeightField")) == field_count
        assert len(tree.findall("PxMaterial")) == (2 if identifier is None or kinds[name] not in (1, 3) else 1)
        assert len(tree.findall("PxShape")) == (1 if identifier is None or name in ("kin-shared-a", "kin-shared-b", "static-shared") else 0)
        assert "KinematicTarget" not in xml.read_text(encoding="utf-8") and "IsSleeping" not in xml.read_text(encoding="utf-8")
        if identifier is None or name == "zero-flags-box":
            zero_element = next(actor for actor in tree.findall("PxRigidStatic") if actor.findtext("Name") == "zero-flags-box")
            assert zero_element.findtext("ActorFlags") == "0"
            assert zero_element.findtext("Shapes/PxShape/Flags") == "0"
            zero_imported = next(actor for actor in load((output / f"{name}-import").with_suffix(".json"))["actors"] if actor["name"] == "zero-flags-box")
            assert zero_imported["actor_flags"] == 0 and zero_imported["shapes"][0]["flags"] == 0
        exports += 1
    result = {"sdk_commit": "5e42a5f112351a223c19c17bb331e6c55037b8eb", "fixture": fixture,
              "capture_sha256": hashlib.sha256(capture.read_bytes()).hexdigest(), "capture_bytes": capture.stat().st_size,
              "official_getter_values": official, "project_getter_values": compared, "project_exports": exports,
              "shared_shape_owners": 3, "unique_triangle_resources": 1, "unique_heightfield_resources": 1,
              "geometry_types": [1, 5, 6], "kinematic_simulation_geometry": True, "non_kinematic_query_plane": True,
              "zero_flags_contract": {"source_actor_flags": 0, "source_shape_flags": 0,
                                      "official_default_loss": zero_results,
                                      "project_actor_flags": 0, "project_shape_flags": 0,
                                      "numeric_conversion_supported": False}}
    (output / "result.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result))


if __name__ == "__main__":
    main()
