import argparse
import datetime
import hashlib
import importlib.util
import json
import pathlib
import re
import xml.etree.ElementTree as ET

spec = importlib.util.spec_from_file_location("joint_verifier", pathlib.Path(__file__).parents[1] / "repx-joints/verify.py")
shared = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shared)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--cli", type=pathlib.Path, default=pathlib.Path("target/debug/pvd-cli.exe"))
    parser.add_argument("--producer", type=pathlib.Path, default=pathlib.Path(".deps/physx-repx-cloth-build/Debug/pvd-repx-cloth.exe"))
    parser.add_argument("--output", type=pathlib.Path)
    args = parser.parse_args()
    output = args.output or pathlib.Path("output/repx-cloth-validation") / datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%d-%H%M%S-%f")
    output.mkdir(parents=True, exist_ok=False)
    capture = output / "capture.pvdstream"
    shared.execute([args.producer,capture,output])
    truth = json.loads((output / "tether-truth.json").read_text())
    assert truth["tethers"] > 0
    reference = output / "reference.xml"
    fabric = ET.parse(reference).find("PxClothFabric")
    anchors = [int(value) for value in fabric.findtext("TetherAnchors").split()]
    lengths = [float(value) for value in fabric.findtext("TetherLengths").split()]
    assert len(anchors) == len(lengths) == truth["tethers"]
    compared = shared.compare(truth["anchors"],anchors) + shared.compare(truth["lengths"],lengths)
    imported = output / "imported-tether-truth.json"
    shared.execute([args.producer,"--import",reference,imported])
    compared += shared.compare(truth,json.loads(imported.read_text()))
    assert "verified_frames: 1" in shared.execute([args.cli,"verify",capture]).stdout
    state = shared.execute([args.cli,"frame",capture,0]).stdout
    (output / "frame-0-state.txt").write_text(state,encoding="utf-8")
    fabrics = re.findall(r"^0x[0-9A-F]+ generation=\d+ physx3::PxClothFabric\n((?:  .*\n)*)", state, re.MULTILINE)
    assert len(fabrics) == 1
    properties = re.findall(r"^  ([^:]+):",fabrics[0],re.MULTILINE)
    assert "NbParticles" in properties and "Restvalues" in properties
    assert all(name not in properties for name in ("NbTethers","TetherAnchors","TetherLengths"))
    result = {"sdk_commit":"5e42a5f112351a223c19c17bb331e6c55037b8eb",
              "capture_sha256":hashlib.sha256(capture.read_bytes()).hexdigest(),
              "particles":truth["particles"],"tethers":truth["tethers"],"compared_values":compared,
              "captured_fabric_properties":properties,"repx_export_supported":False,
              "reason":"Official PVD metadata omits Fabric tether count, anchors and lengths."}
    (output / "result.json").write_text(json.dumps(result,indent=2) + "\n",encoding="utf-8")
    print(json.dumps(result))


if __name__ == "__main__":
    main()
