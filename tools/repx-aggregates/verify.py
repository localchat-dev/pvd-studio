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
    values = json.loads(path.read_text(encoding="utf-8"))
    for key in ("actors", "joints", "aggregates"):
        values[key].sort(key=lambda value: value["id"])
    for aggregate in values["aggregates"]:
        aggregate["members"].sort()
    return values


def component(values, selected):
    actors, joints, aggregates = {selected}, set(), set()
    while True:
        before = len(actors), len(joints), len(aggregates)
        for aggregate in values["aggregates"]:
            if actors.intersection(aggregate["members"]):
                aggregates.add(aggregate["id"])
                actors.update(aggregate["members"])
        for joint in values["joints"]:
            if actors.intersection(joint["actors"]):
                joints.add(joint["id"])
                actors.update(identifier for identifier in joint["actors"] if identifier)
        if before == (len(actors), len(joints), len(aggregates)):
            break
    return {key: [value for value in values[key] if value["id"] in ids]
            for key, ids in (("actors", actors), ("joints", joints), ("aggregates", aggregates))}


def import_compare(producer, xml, expected, output):
    shared.execute([producer, "--import", xml, output.with_suffix(".json"), output.with_suffix(".xml")])
    return shared.compare(expected, load(output.with_suffix(".json")))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--cli", type=pathlib.Path, default=pathlib.Path("target/debug/pvd-cli.exe"))
    parser.add_argument("--producer", type=pathlib.Path, default=pathlib.Path(".deps/physx-repx-aggregates-build/Debug/pvd-repx-aggregates.exe"))
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--producer-only", action="store_true")
    args = parser.parse_args()
    output = args.output or pathlib.Path("output/repx-aggregate-validation") / datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%d-%H%M%S-%f")
    output.mkdir(parents=True, exist_ok=False)
    capture = output / "capture.pvdstream"
    shared.execute([args.producer, capture, output])
    official, whole, selected, selected_exports = 0, 0, 0, 0
    if not args.producer_only:
        replay = shared.execute([args.cli, "verify", capture])
        assert "verified_frames: 4" in replay.stdout, replay.stdout
    for frame in range(4):
        expected = load(output / f"frame-{frame}-truth.json")
        assert len(expected["actors"]) == 5 and len(expected["aggregates"]) == 3
        assert {actor["type"] for actor in expected["actors"]} == {"PxRigidDynamic", "PxRigidStatic"}
        assert any(aggregate["capacity"] == 0 for aggregate in expected["aggregates"])
        assert len(expected["joints"]) == (1 if frame in (0, 2) else 0)
        assert any(not value["members"] for value in expected["aggregates"])
        official += import_compare(args.producer, output / f"frame-{frame}-reference.xml", expected, output / f"frame-{frame}-official-import")
        if args.producer_only:
            continue
        xml = output / f"frame-{frame}-exported.xml"
        shared.execute([args.cli, "repx", capture, frame, xml])
        whole += import_compare(args.producer, xml, expected, output / f"frame-{frame}-exported-import")
        for index, actor in enumerate(expected["actors"]):
            xml = output / f"frame-{frame}-actor-{index}.xml"
            shared.execute([args.cli, "repx", capture, frame, xml, actor["id"]])
            selected += import_compare(args.producer, xml, component(expected, actor["id"]), output / f"frame-{frame}-actor-{index}-import")
            selected_exports += 1
        print(f"frame {frame}: whole and all selected Actor dependency closures match SDK getters")
    result = {"sdk_commit": "5e42a5f112351a223c19c17bb331e6c55037b8eb", "frames": 4,
              "capture_bytes": capture.stat().st_size, "capture_sha256": hashlib.sha256(capture.read_bytes()).hexdigest(),
              "official_getter_values": official, "whole_getter_values": whole,
              "selected_getter_values": selected, "selected_exports": selected_exports,
              "producer_only": args.producer_only}
    (output / "result.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result))


if __name__ == "__main__":
    main()
