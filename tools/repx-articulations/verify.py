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
    data["articulations"].sort(key=lambda value: value["id"])
    for articulation in data["articulations"]:
        articulation["links"].sort(key=lambda value: value["id"])
        for link in articulation["links"]:
            link["children"].sort()
    for key in ("actors", "joints", "aggregates"):
        data["rigid"][key].sort(key=lambda value: value["id"])
    for aggregate in data["rigid"]["aggregates"]:
        aggregate["members"].sort()
    return data


def component(data, selected):
    ids, arts, groups, joints = {selected}, set(), set(), set()
    while True:
        before = len(ids), len(arts), len(groups), len(joints)
        for art in data["articulations"]:
            members = {link["id"] for link in art["links"]} | {art["id"]}
            if ids.intersection(members):
                arts.add(art["id"])
                ids.update(members)
        for group in data["rigid"]["aggregates"]:
            if ids.intersection(group["members"]):
                groups.add(group["id"])
                ids.update(group["members"])
        for joint in data["rigid"]["joints"]:
            if ids.intersection(joint["actors"]):
                joints.add(joint["id"])
                ids.update(value for value in joint["actors"] if value)
        if before == (len(ids), len(arts), len(groups), len(joints)):
            break
    return {"articulations": [art for art in data["articulations"] if art["id"] in arts],
            "rigid": {key: [value for value in data["rigid"][key] if value["id"] in selected_ids]
                      for key, selected_ids in (("actors", ids), ("joints", joints), ("aggregates", groups))}}


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
    parser.add_argument("--producer", type=pathlib.Path, default=pathlib.Path(".deps/physx-repx-articulations-build/Debug/pvd-repx-articulations.exe"))
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--producer-only", action="store_true")
    args = parser.parse_args()
    output = args.output or pathlib.Path("output/repx-articulation-validation") / datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%d-%H%M%S-%f")
    output.mkdir(parents=True, exist_ok=False)
    capture = output / "capture.pvdstream"
    sdk_execute([args.producer, capture, output])
    official = whole = selected = exports = 0
    if not args.producer_only:
        assert "verified_frames: 4" in shared.execute([args.cli, "verify", capture]).stdout
    for frame in range(4):
        truth = load(output / f"frame-{frame}-truth.json")
        assert len(truth["articulations"]) == (1 if frame == 2 else 2)
        assert all(len(art["links"]) == 4 for art in truth["articulations"])
        assert {link["client_behavior"] for art in truth["articulations"] for link in art["links"]} == set(range(frame * 4, frame * 4 + 4))
        assert {actor["type"] for actor in truth["rigid"]["actors"]} == {"PxRigidStatic", "PxRigidDynamic"}
        assert next(actor["client_behavior"] for actor in truth["rigid"]["actors"] if actor["type"] == "PxRigidStatic") == (15 if frame == 0 else 0)
        assert next(actor["client_behavior"] for actor in truth["rigid"]["actors"] if actor["type"] == "PxRigidDynamic") == (8, 3, 0, 15)[frame]
        if frame == 0:
            assert {link["flags"] for art in truth["articulations"] for link in art["links"]} == {0, 24, 48, 76}
        official += verify_import(args.producer, output / f"frame-{frame}-reference.xml", truth, output / f"frame-{frame}-official-import")
        if args.producer_only:
            continue
        xml = output / f"frame-{frame}-exported.xml"
        shared.execute([args.cli, "repx", capture, frame, xml])
        whole += verify_import(args.producer, xml, truth, output / f"frame-{frame}-exported-import")
        ids = [actor["id"] for actor in truth["rigid"]["actors"]]
        for art in truth["articulations"]:
            ids += [art["id"]] + [link["id"] for link in art["links"]]
        for index, identifier in enumerate(ids):
            xml = output / f"frame-{frame}-selected-{index}.xml"
            shared.execute([args.cli, "repx", capture, frame, xml, identifier])
            selected += verify_import(args.producer, xml, component(truth, identifier), output / f"frame-{frame}-selected-{index}-import")
            exports += 1
    result = {"sdk_commit": "5e42a5f112351a223c19c17bb331e6c55037b8eb", "frames": 4,
              "capture_bytes": capture.stat().st_size, "capture_sha256": hashlib.sha256(capture.read_bytes()).hexdigest(),
              "official_getter_values": official, "whole_getter_values": whole,
              "selected_getter_values": selected, "selected_exports": exports, "producer_only": args.producer_only}
    (output / "result.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result))


if __name__ == "__main__":
    main()
