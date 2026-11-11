import argparse
import datetime
import hashlib
import importlib.util
import json
import pathlib
import platform
import re
import subprocess
import xml.etree.ElementTree as ET

spec = importlib.util.spec_from_file_location("joint_verifier", pathlib.Path(__file__).parents[1] / "repx-joints/verify.py")
shared = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shared)


def load(path):
    data = json.loads(path.read_text(encoding="utf-8"))
    assert data["schema"] == 2, path
    return data


def sdk_execute(arguments):
    result = shared.execute(arguments)
    assert not result.stdout.strip(), result.stdout
    return result


def import_target(producer, path, expected, output):
    sdk_execute([producer, "--import", path, output])
    actual = load(output)
    oracle = dict(expected)
    if expected["flags"] & 1:
        oracle.update(linear=[0, 0, 0], angular=[0, 0, 0], wake=0)
        assert actual["linear"] == [0, 0, 0] and actual["angular"] == [0, 0, 0]
        assert actual["wake"] == 0
    oracle.update(target_valid=False, target=None, body_target=None, diagnostics=[])
    assert actual["target_valid"] is False and actual["target"] is None and actual["body_target"] is None
    assert not actual["diagnostics"], actual
    return shared.compare(oracle, actual)


def xml_motion(actor, expected):
    checked = 0
    for field, key in (("LinearVelocity", "linear"), ("AngularVelocity", "angular")):
        checked += shared.compare(expected[key], [float(value) for value in actor.findtext(field).split()], field)
    checked += shared.compare(expected["wake"], float(actor.findtext("WakeCounter")), "WakeCounter")
    return checked


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--cli", type=pathlib.Path, default=pathlib.Path("target/debug/pvd-cli.exe"))
    parser.add_argument("--producer", type=pathlib.Path, default=pathlib.Path(".deps/physx-repx-kinematics-build/Debug/pvd-repx-kinematics.exe"))
    parser.add_argument("--output", type=pathlib.Path)
    args = parser.parse_args()
    output = args.output or pathlib.Path("output/repx-kinematic-validation") / datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%d-%H%M%S-%f")
    output.mkdir(parents=True, exist_ok=False)
    capture = output / "capture.pvdstream"
    sdk_execute([args.producer, capture, output])
    flag_probe = json.loads((output / "flag-probe.json").read_text(encoding="utf-8"))
    assert {key: flag_probe[key] for key in ("before", "query_only", "requested_conflict", "filtered", "after_clear")} == {
        "before": 0, "query_only": 2, "requested_conflict": 38, "filtered": 6, "after_clear": 0,
    }
    assert flag_probe["kinematic_ccd_requested"] == 5 and flag_probe["kinematic_ccd_filtered"] == 1
    assert flag_probe["kinematic_speculative"] == 33 and flag_probe["kinematic_after_clear"] == 0
    assert len(flag_probe["diagnostics"]) == 2
    assert all(item["code"] == 4 for item in flag_probe["diagnostics"])
    assert all(name in flag_probe["diagnostics"][0]["message"] for name in ("eENABLE_CCD", "eENABLE_SPECULATIVE_CCD"))
    assert all(name in flag_probe["diagnostics"][1]["message"] for name in ("kinematic", "CCD"))
    pending_values = 0
    for index in (1, 2, 3):
        truth = load(output / f"pending-{index}-truth.json")
        assert truth["target_valid"] and truth["flags"] == (1 if index == 3 else 3) and not truth["diagnostics"]
        assert truth["target"] != truth["body_target"]
        reference = output / f"pending-{index}-reference.xml"
        actor = ET.parse(reference).find("PxRigidDynamic")
        assert actor is not None and actor.find("KinematicTarget") is None
        pending_values += import_target(args.producer, reference, truth, output / f"pending-{index}-imported.json")
        target_text = " ".join(str(value) for value in truth["target"][3:] + truth["target"][:3])
        reference_text = reference.read_text(encoding="utf-8")
        assert reference_text.count("</PxRigidDynamic>") == 1
        injected = output / f"pending-{index}-injected-target.xml"
        injected.write_text(reference_text.replace("</PxRigidDynamic>", f"<KinematicTarget>{target_text}</KinematicTarget>\n\t</PxRigidDynamic>"), encoding="utf-8")
        pending_values += import_target(args.producer, injected, truth, output / f"pending-{index}-injected-imported.json")
    assert "verified_frames: 8" in shared.execute([args.cli, "verify", capture]).stdout.splitlines()
    imports = getter_values = source_motion_values = 0
    wake_checks = []
    boundary_checks = []
    motion_checks = []
    previous_dynamic_poststep = None
    expected_flags = (3, 3, 3, 3, 2, 0, 1, 1)
    phases = ("prestep", "prestep", "prestep", "prestep", "prestep", "prestep", "poststep", "prestep")
    for frame in range(8):
        truth = load(output / f"frame-{frame}-truth.json")
        before = load(output / f"frame-{frame}-prestep-truth.json")
        after = load(output / f"frame-{frame}-poststep-truth.json")
        boundary = json.loads((output / f"frame-{frame}-boundary.json").read_text(encoding="utf-8"))
        assert boundary["schema"] == 2 and 0 < boundary["prestep_bytes"] < boundary["poststep_bytes"] <= capture.stat().st_size
        shared.compare(before if phases[frame] == "prestep" else after, truth, f"frame-{frame}/sampling")
        assert before["target_valid"] == (frame in (1, 2, 6)) and after["target_valid"] is False
        assert truth["flags"] == expected_flags[frame] and not truth["diagnostics"]
        kinematic = bool(truth["flags"] & 1)
        frame_result = subprocess.run(
            [str(args.cli), "frame", str(capture), str(frame)], capture_output=True,
            encoding="utf-8", check=True, timeout=60,
            creationflags=subprocess.CREATE_NO_WINDOW if platform.system() == "Windows" else 0,
        )
        assert not frame_result.stderr.strip(), frame_result.stderr
        state = frame_result.stdout
        (output / f"frame-{frame}-state.txt").write_text(state, encoding="utf-8")
        actors = re.findall(r"^0x[0-9A-F]+ generation=\d+ physx3::PxRigidDynamic\n((?:  .*\n)*)", state, re.MULTILINE)
        assert len(actors) == 1
        properties = dict(re.findall(r"^  ([^:]+): (.*)$", actors[0], re.MULTILINE))
        assert int(properties["RigidBodyFlags"]) == truth["flags"]
        assert not any("kinematic" in name.lower() and "valid" in name.lower() for name in properties)
        if frame == 0:
            assert "KinematicTarget" not in properties
        else:
            target = re.fullmatch(r"p (\[[^]]+\]) · q (\[[^]]+\])", properties["KinematicTarget"])
            assert target, properties["KinematicTarget"]
            pending = load(output / f"pending-{3 if frame >= 6 else min(frame, 2)}-truth.json")
            shared.compare(pending["body_target"], json.loads(target[1]) + json.loads(target[2]))
        reference = output / f"frame-{frame}-reference.xml"
        assert ET.parse(reference).find("PxRigidDynamic/KinematicTarget") is None
        getter_values += import_target(args.producer, reference, truth, output / f"frame-{frame}-official-imported.json")
        imports += 1
        wake_check = {"frame": frame, "captured": float(properties["WakeCounter"]), "sdk_sample": truth["wake"],
                      "official_imported": load(output / f"frame-{frame}-official-imported.json")["wake"]}
        if frame in (4, 5):
            shared.compare(0.9 if frame == 4 else 0.7, truth["wake"])
            shared.compare(truth["wake"] - 1.0 / 60.0, after["wake"])
            assert after["wake"] < truth["wake"]
            assert truth["pose"] != after["pose"] and truth["linear"] != after["linear"] and truth["angular"] != after["angular"]
            for key in truth:
                if key not in ("pose", "linear", "angular", "wake"):
                    shared.compare(truth[key], after[key], f"frame-{frame}/boundary/{key}")
            if previous_dynamic_poststep is not None:
                shared.compare(previous_dynamic_poststep["pose"], truth["pose"], "frame-4-poststep/frame-5-prestep")
            previous_dynamic_poststep = after
        if frame == 6:
            assert before["pose"] != after["pose"]
        boundary_checks.append({"frame": frame, "capture_sample": phases[frame], **boundary,
                                "prestep_pose": before["pose"], "poststep_pose": after["pose"]})
        for selected in (False, True):
            label = "selected" if selected else "whole"
            exported = output / f"frame-{frame}-{label}.xml"
            arguments = [args.cli, "repx", capture, frame, exported]
            if selected:
                arguments.append(truth["id"])
            result = shared.execute(arguments)
            assert f"kinematic_actors: {int(kinematic)}" in result.stdout.splitlines()
            assert ("kinematic_state: targets, velocities and wake counters are not restored" in result.stdout.splitlines()) == kinematic
            actor = ET.parse(exported).find("PxRigidDynamic")
            assert actor is not None and actor.find("KinematicTarget") is None and actor.find("IsSleeping") is None
            flag_text = actor.findtext("RigidBodyFlags", default="")
            assert ("eUSE_KINEMATIC_TARGET_FOR_SCENE_QUERIES" in flag_text) == bool(truth["flags"] & 2)
            assert ("eKINEMATIC" in flag_text) == kinematic
            source_motion_values += xml_motion(actor, truth)
            getter_values += import_target(args.producer, exported, truth, output / f"frame-{frame}-{label}-imported.json")
            imports += 1
            imported = load(output / f"frame-{frame}-{label}-imported.json")
            wake_check[f"{label}_imported"] = imported["wake"]
            if kinematic:
                motion_checks.append({"frame": frame, "selection": label, "source_linear": truth["linear"],
                                      "source_angular": truth["angular"], "source_wake": truth["wake"],
                                      "imported_linear": imported["linear"], "imported_angular": imported["angular"], "imported_wake": imported["wake"]})
        wake_checks.append(wake_check)
    result = {"schema": 2, "sdk_commit": "5e42a5f112351a223c19c17bb331e6c55037b8eb",
              "capture_sha256": hashlib.sha256(capture.read_bytes()).hexdigest(), "frames": 8,
              "business_frames": 7, "boundary_audit_frames": 1, "pending_targets": 3, "pending_target_official_imports": 6,
              "pending_getter_values": pending_values, "snapshot_imports": imports, "snapshot_getter_values": getter_values,
              "source_motion_values": source_motion_values, "kinematic_frames": 6, "non_kinematic_frames": 2,
              "wake_counter_checks": wake_checks, "frame_boundary_checks": boundary_checks,
              "kinematic_motion_checks": motion_checks, "flag_probe": flag_probe,
              "snapshot_parameters_supported": True, "kinematic_targets_velocities_wake_restored": False}
    (output / "result.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result))


if __name__ == "__main__":
    main()
