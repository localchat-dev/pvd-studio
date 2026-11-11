from collections import Counter
import json
import math
from pathlib import Path
import struct
import sys

truth_path, probe_path, output_path = map(Path, sys.argv[1:])
truth = json.loads(truth_path.read_text(encoding="utf-8"))
rows = [json.loads(line) for line in (probe_path / "events.jsonl").read_text(encoding="utf-8").splitlines()]
indices = truth["physical_indices"]
candidates = {}
expected = set()
for step in truth["steps"]:
    for actor in step["actors"]:
        identity = (step["step"], actor["serial"])
        if identity in expected:
            raise ValueError(f"duplicate actor/step in getter truth: {identity}")
        expected.add(identity)
        candidates.setdefault(actor["actor_params_id"], []).append((step["step"], actor))

renderer_instances = {}
actor_parameters = {}
last_sections = {}
results = []

for row in rows:
    if row["kind"] == "reference" and row["property"] == "ActorParams":
        actor_parameters[row["id"]] = row["target"]
    if row["kind"] == "destroy":
        actor_parameters.pop(row["id"], None)
    if row["kind"] == "section_end":
        last_sections[row["name"]] = {key: row[key] for key in ("source", "event", "group_offset")}
    if row["kind"] != "renderer_flush":
        continue
    payload = (probe_path / row["payload"]).read_bytes()
    at = 0
    instance = renderer_instances.get(row["id"], 0)
    while at < len(payload):
        event = payload[at]
        at += 1
        if event == 1:
            instance, = struct.unpack_from("<Q", payload, at)
            at += 8
        elif event in (2, 3, 4, 11):
            if event == 11:
                for stride in (16, 32):
                    count, = struct.unpack_from("<I", payload, at)
                    at += 4 + count * stride
                    if at > len(payload):
                        raise ValueError("truncated aggregate renderer payload")
            count, = struct.unpack_from("<I", payload, at)
            at += 4
            if event in (2, 3):
                at += count * (16 if event == 2 else 32)
                if at > len(payload):
                    raise ValueError("truncated renderer payload")
                continue
            if count == 0:
                continue
            if count > 1_000_000:
                raise ValueError("renderer triangle count exceeds probe budget")
            points = []
            colors = set()
            for _ in range(count * 3):
                x, y, z, color = struct.unpack_from("<3fI", payload, at)
                at += 16
                if not all(map(math.isfinite, (x, y, z))):
                    raise ValueError("non-finite renderer vertex")
                points.append((x, y, z))
                colors.add(color)
            parameter_id = actor_parameters.get(instance)
            phases = []
            for step in truth["steps"]:
                if step["before_simulate_bytes"] <= row["group_offset"] < step["after_getters_bytes"]:
                    phases.append(step["step"])
            if len(phases) != 1:
                raise ValueError("renderer draw does not belong to one transport step interval")
            matches = []
            if len(points) == len(indices):
                for step, actor in candidates.get(parameter_id, []):
                    if step != phases[0]:
                        continue
                    vertices = actor["world_positions"]
                    if max(indices) >= len(vertices):
                        continue
                    sampled = [0, len(points) // 2, len(points) - 1]
                    if any(abs(points[index][axis] - vertices[indices[index]][axis]) > 0.0001
                           for index in sampled for axis in range(3)):
                        continue
                    difference = max(abs(point[axis] - vertices[index][axis])
                                     for point, index in zip(points, indices) for axis in range(3))
                    if difference <= 0.0001:
                        matches.append({"step": step, "serial": actor["serial"], "max_error": difference})
            results.append(dict(event=row["event"], group_offset=row["group_offset"],
                                instance=instance, actor_params_id=parameter_id, triangles=count,
                                colors=sorted(colors), getter_matches=matches, transport_step_intervals=phases,
                                preceding_sections=dict(last_sections)))
        else:
            raise ValueError(f"unsupported renderer event {event} at {at - 1}")
        if at > len(payload):
            raise ValueError("truncated renderer payload")
    renderer_instances[row["id"]] = instance

unmatched = [row for row in results if not row["getter_matches"]]
ambiguous = [row for row in results if len(row["getter_matches"]) > 1]
coverage = Counter((row["getter_matches"][0]["step"], row["getter_matches"][0]["serial"])
                   for row in results if len(row["getter_matches"]) == 1)
missing = sorted(expected - coverage.keys())
duplicates = [dict(step=step, serial=serial, draws=count)
              for (step, serial), count in sorted(coverage.items()) if count != 1]
summary = dict(draw_calls=len(results), triangles=sum(row["triangles"] for row in results),
               expected_actor_steps=len(expected), covered_actor_steps=len(coverage),
               unmatched=len(unmatched), ambiguous=len(ambiguous), missing_actor_steps=missing,
               duplicate_actor_steps=duplicates,
               colors=sorted({color for row in results for color in row["colors"]}),
               result_scope="raw renderer payload versus direct SDK getters; no frame reassignment",
               draws=results)
output_path.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
print(json.dumps({key: value for key, value in summary.items() if key != "draws"}, indent=2))
if unmatched or ambiguous or missing or duplicates:
    print(json.dumps((unmatched + ambiguous)[:3], indent=2))
    sys.exit(1)
