import collections
import hashlib
import json
from pathlib import Path
import struct
import sys

capture_path, probe_path, output_path = map(Path, sys.argv[1:])
capture = capture_path.read_bytes()
rows = [json.loads(line) for line in (probe_path / "events.jsonl").read_text(encoding="utf-8").splitlines()]
memory = [row for row in rows if row.get("class", "").endswith("::PxProfileMemoryEventBuffer")
          and row.get("property") == "events"]
dictionaries = {}
definitions = []
undefined = []
events = collections.Counter()
buffers = []

for row in memory:
    offset = row["group_offset"]
    size, count, stream, timestamp = struct.unpack_from("<IIQQ", capture, offset)
    if count != 1 or row["ordinal_in_group"] != 0 or capture[offset + 24] != 7:
        raise ValueError("memory extraction requires a one-event SetPropertyValue group")
    object_id, property_id, namespace, datatype, items, length = struct.unpack_from("<QIIIII", capture, offset + 25)
    start = offset + 53
    payload = capture[start:start + length]
    if size != length + 29 or len(payload) != length or length != row["bytes"] or object_id != row["id"]:
        raise ValueError("memory group shape disagrees with protocol probe")
    if payload[:32].hex() != row["prefix_hex"]:
        raise ValueError("memory bytes disagree with protocol probe")
    at = 0
    dictionary = dictionaries.setdefault(object_id, {})
    local_counts = collections.Counter()

    def uint(width):
        global at
        if at + width > len(payload):
            raise ValueError("truncated memory field")
        value = int.from_bytes(payload[at:at + width], "little")
        at += width
        return value

    while at < len(payload):
        position = at
        header = uint(2)
        event = header & 15
        events[event] += 1
        local_counts[event] += 1
        if event == 1:
            text_size = uint(4)
            if at + text_size > len(payload):
                raise ValueError("truncated memory string")
            raw = payload[at:at + text_size]
            at += text_size
            if not raw or raw[-1] != 0:
                raise ValueError("memory string is not NUL terminated")
            handle = uint(4)
            text = raw[:-1].decode("utf-8", errors="replace")
            dictionary[handle] = text
            definitions.append(dict(buffer=object_id, handle=handle, text=text, outer_event=row["event"],
                                    embedded_offset=position, capture_offset=start + position))
        elif event == 2:
            widths = {"address": 1 << ((header >> 4) & 3),
                      "type": min(4, 1 << ((header >> 6) & 3)),
                      "file": min(4, 1 << ((header >> 8) & 3)),
                      "size": min(4, 1 << ((header >> 10) & 3)),
                      "line": min(4, 1 << ((header >> 12) & 3))}
            values = {key: uint(widths[key]) for key in ("size", "type", "file", "line", "address")}
            missing = {key: values[key] for key in ("type", "file") if values[key] not in dictionary}
            if missing:
                undefined.append(dict(buffer=object_id, outer_event=row["event"], embedded_offset=position,
                                      capture_offset=start + position, missing=missing, allocation=values))
        elif event == 3:
            uint(1 << ((header >> 4) & 3))
        else:
            raise ValueError(f"unsupported memory event {event} at capture byte {start + position}")
    buffers.append(dict(buffer=object_id, outer_event=row["event"], group_offset=offset,
                        payload_bytes=length, sha256=hashlib.sha256(payload).hexdigest(),
                        memory_events=dict(local_counts)))

never_defined = sorted({(entry["buffer"], handle) for entry in undefined for handle in entry["missing"].values()
                        if handle not in dictionaries[entry["buffer"]]})
result = dict(capture_sha256=hashlib.sha256(capture).hexdigest(), payloads=len(buffers),
              event_counts=dict(events), undefined_allocation_events=len(undefined),
              never_defined=[dict(buffer=buffer, handle=handle) for buffer, handle in never_defined],
              first_undefined=undefined[:10], definitions=definitions, buffers=buffers)
output_path.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
print(json.dumps({key: value for key, value in result.items() if key not in ("definitions", "buffers")}, indent=2))
