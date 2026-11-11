import argparse
import copy
import datetime
import hashlib
import importlib.util
import json
import pathlib
import struct
import xml.etree.ElementTree as ET

spec = importlib.util.spec_from_file_location("articulation_verifier", pathlib.Path(__file__).with_name("verify.py"))
base = importlib.util.module_from_spec(spec)
spec.loader.exec_module(base)
shared = base.shared

CASES = (
    ("target-initial", (0, 0, 0, 0x3F800000), True),
    ("error-axis", (0x3F800000, 0, 0, 0), True),
    ("error-oblique", (0x3EB851EC, 0x3EF5C28F, 0x3F4CCCCD, 0), True),
    ("error-upper-unit", (0x3F8001A3, 0, 0, 0), True),
    ("error-lower-unit", (0x3F7FFCB9, 0, 0, 0), True),
    ("error-negative-zero", (0x80000000, 0, 0xBF800000, 0x80000000), True),
    ("error-margin-upper-in", (0x3F8002F2, 0, 0, 0), True),
    ("error-margin-upper-out", (0x3F8002F4, 0, 0, 0), False),
    ("error-margin-lower-in", (0x3F7FFA1C, 0, 0, 0), True),
    ("error-margin-lower-out", (0x3F7FFA18, 0, 0, 0), False),
    ("error-sdk-upper-in", (0x3F800345, 0, 0, 0), False),
    ("error-sdk-upper-out", (0x3F800349, 0, 0, 0), False),
    ("error-sdk-lower-in", (0x3F7FF976, 0, 0, 0), False),
    ("error-sdk-lower-out", (0x3F7FF96F, 0, 0, 0), False),
    ("error-zero", (0, 0, 0, 0), False),
    ("error-half", (0x3F000000, 0, 0, 0), False),
    ("error-double", (0x40000000, 0, 0, 0), False),
    ("target-restored", (0, 0, 0, 0x3F800000), True),
)


def execute(arguments):
    result = shared.execute(arguments)
    assert not result.stdout.strip(), result.stdout
    return result


def joint(data):
    values = [link["joint"] for art in data["articulations"] for link in art["links"] if link.get("joint")]
    assert len(values) == 1
    return values[0]


def without_target(data):
    result = copy.deepcopy(data)
    value = joint(result)
    for key in ("target", "target_bits", "target_is_unit"):
        del value[key]
    return result


def imported(producer, xml, prefix):
    truth = prefix.with_suffix(".json")
    execute([producer, "--import-drive", xml, truth, prefix.with_suffix(".xml")])
    diagnostics = json.loads(pathlib.Path(str(truth) + ".diagnostics.json").read_text(encoding="utf-8"))
    return base.load(truth), diagnostics


def target_diagnostics(diagnostics):
    assert diagnostics
    assert all(item["code"] == 4 and "quat orientation is not valid" in item["message"] for item in diagnostics), diagnostics


def exact_reader_xml(reference, output, bits):
    tree = ET.parse(reference)
    values = tree.findall(".//Joint/TargetOrientation")
    assert len(values) == 1
    values[0].text = " ".join(format(struct.unpack("<f", struct.pack("<I", value))[0], ".9g") for value in bits)
    tree.write(output, encoding="utf-8", xml_declaration=False)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--producer", type=pathlib.Path, default=pathlib.Path(".deps/physx-repx-articulations-build/Debug/pvd-repx-articulations.exe"))
    parser.add_argument("--cli", type=pathlib.Path, default=pathlib.Path("target/debug/pvd-cli.exe"))
    parser.add_argument("--output", type=pathlib.Path)
    args = parser.parse_args()
    output = args.output or pathlib.Path("output/repx-error-drive-validation") / datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%d-%H%M%S-%f")
    output.mkdir(parents=True, exist_ok=False)
    capture = output / "capture.pvdstream"
    execute([args.producer, "--error-drive", capture, output])
    oracle = json.loads((output / "drive-oracle.json").read_text(encoding="utf-8"))
    assert oracle["schema"] == 1 and oracle["sdk_version"] == [3, 4, 2]
    assert len(oracle["frames"]) == len(CASES) == 18
    assert "verified_frames: 18" in shared.execute([args.cli, "verify", capture]).stdout.splitlines()
    getters = exports = rejected = readable_controls = 0
    reference_rows = []
    for frame, (row, (name, bits, supported)) in enumerate(zip(oracle["frames"], CASES)):
        mode = 0 if frame in (0, 17) else 1
        assert row["name"] == name and row["drive"] == mode and row["exportable"] is supported
        assert row["target_bits"] == list(bits) == row["poststep_target_bits"]
        assert row["sdk_is_unit"] is (frame not in (11, 13, 14, 15, 16))
        assert 0 < row["prestep_bytes"] < row["poststep_bytes"] <= capture.stat().st_size
        truth = base.load(output / f"frame-{frame}-truth.json")
        assert len(truth["articulations"]) == 1 and len(truth["articulations"][0]["links"]) == 2
        source = joint(truth)
        assert source["target_bits"] == list(bits) and source["drive"] == mode and source["target_is_unit"] is row["sdk_is_unit"]
        assert not truth["rigid"]["actors"] and not truth["rigid"]["joints"] and not truth["rigid"]["aggregates"]
        reference = output / f"frame-{frame}-reference.xml"
        reference_target = ET.parse(reference).findtext(".//Joint/TargetOrientation")
        assert reference_target is not None
        writer_bits = [struct.unpack("<I", struct.pack("<f", float(part)))[0] for part in reference_target.split()]
        assert len(writer_bits) == 4
        original, original_diagnostics = imported(args.producer, reference, output / f"frame-{frame}-original-reference")
        if original_diagnostics:
            target_diagnostics(original_diagnostics)
        shared.compare(without_target(truth), without_target(original))
        reference_rows.append({"frame": frame, "name": name, "source_bits": list(bits),
                               "writer_bits": writer_bits,
                               "original_reference_bits": joint(original)["target_bits"],
                               "original_reference_diagnostics": original_diagnostics,
                               "formatter_loss": writer_bits != list(bits),
                               "reader_loss": joint(original)["target_bits"] != writer_bits})
        precise = output / f"frame-{frame}-getter-exact-reader.xml"
        exact_reader_xml(reference, precise, bits)
        exact, diagnostics = imported(args.producer, precise, output / f"frame-{frame}-getter-exact-reader-import")
        if row["sdk_is_unit"]:
            assert not diagnostics, diagnostics
            assert joint(exact)["target_bits"] == list(bits)
            getters += shared.compare(truth, exact)
            if not supported:
                readable_controls += 1
        else:
            target_diagnostics(diagnostics)
            assert joint(exact)["target_bits"] != list(bits)
            shared.compare(without_target(truth), without_target(exact))
        for selected in (False, True):
            label = "link" if selected else "whole"
            xml = output / f"frame-{frame}-{label}.xml"
            arguments = [args.cli, "repx", capture, frame, xml]
            if selected:
                arguments.append(oracle["actor"])
            if not supported:
                result = shared.execute(arguments, succeed=False)
                assert "eERROR target" in result.stdout + result.stderr
                assert not xml.exists()
                rejected += 1
                continue
            shared.execute(arguments)
            actual, diagnostics = imported(args.producer, xml, output / f"frame-{frame}-{label}-import")
            assert not diagnostics, diagnostics
            assert joint(actual)["drive"] == mode and joint(actual)["target_bits"] == list(bits)
            getters += shared.compare(truth, actual)
            assert ET.parse(xml).findtext(".//Joint/DriveType") == ("eTARGET" if mode == 0 else "eERROR")
            exports += 1
    assert exports == 18 and rejected == 18 and readable_controls == 4
    result = {"sdk_commit": "5e42a5f112351a223c19c17bb331e6c55037b8eb", "frames": 18,
              "capture_sha256": hashlib.sha256(capture.read_bytes()).hexdigest(), "capture_bytes": capture.stat().st_size,
              "project_exports": exports, "project_rejected_no_file": rejected,
              "sdk_readable_but_outside_inner_margin": readable_controls, "compared_getter_values": getters,
              "reference_precision_controls": reference_rows,
              "scope": "DriveType1 finite original f32, w signed-zero bits, f64 length distance <9e-5; fixed Windows PhysX3.4.2 reader"}
    (output / "result.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result))


if __name__ == "__main__":
    main()
