import argparse
import collections
import hashlib
import json
from pathlib import Path
import zlib


class Reader:
    def __init__(self, data, base=0):
        self.data = data
        self.base = base
        self.at = 0

    def take(self, size):
        if size < 0 or size > len(self.data) - self.at:
            raise ValueError(f"truncated field at capture byte {self.base + self.at}")
        result = self.data[self.at:self.at + size]
        self.at += size
        return result

    def uint(self, width):
        return int.from_bytes(self.take(width), "little")

    def text(self):
        raw = self.take(self.uint(4))
        if not raw:
            return ""
        if raw[-1] != 0 or b"\0" in raw[:-1]:
            raise ValueError("invalid SDK string")
        return raw[:-1].decode("utf-8")

    def done(self):
        if self.at != len(self.data):
            raise ValueError(f"unconsumed bytes at capture byte {self.base + self.at}")


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def inner_events(payload, base, zone, outer_event):
    r = Reader(payload, base)
    last_timestamp = 0
    context = None
    rows = []
    while r.at < len(payload):
        position = r.at
        kind, options, event = r.uint(1), r.uint(1), r.uint(2)
        if options & 0xF0:
            raise ValueError("unsupported profile stream options")
        timestamp_width = 1 << (options & 3)
        context_width = 1 << ((options >> 2) & 3)
        row = dict(zone=zone, event=event, type=kind, outer_event=outer_event,
                   embedded_offset=position, capture_offset=base + position,
                   options=options, timestamp_width=timestamp_width)
        if kind in (1, 2):
            context = dict(thread=r.uint(4), context=r.uint(context_width),
                           priority=r.uint(1), cpu=r.uint(1))
        if kind in (1, 2, 3, 4):
            if context is None:
                raise ValueError("relative event before full context in its buffer")
            timestamp = r.uint(timestamp_width)
            if timestamp_width != 8:
                timestamp += last_timestamp
            if timestamp >= 1 << 64:
                raise ValueError("timestamp overflow")
            last_timestamp = timestamp
            row.update(context)
            row["tick"] = timestamp
            row["action"] = "start" if kind in (1, 3) else "stop"
        elif kind == 5:
            value = r.uint(timestamp_width)
            row.update(value=value, context=r.uint(context_width), thread=r.uint(4), action="value")
        else:
            raise ValueError(f"unsupported profile event type {kind}")
        rows.append(row)
    r.done()
    return rows


def read_capture(path):
    raw = path.read_bytes()
    header = Reader(raw[:28])
    stream_id, version = header.uint(4), header.uint(4)
    numerator, denominator, flags = header.uint(8), header.uint(8), header.uint(4)
    if (stream_id, version, flags) != (837150850, 1, 1) or not numerator or not denominator:
        raise ValueError("oracle requires little-endian SDK v1 with 64-bit pointers and a valid clock")
    strings, objects, generations, partial, message_groups = {}, {}, {}, {}, {}
    profile_objects, zones, names, spans, events, buffers, sections = {}, {}, {}, [], [], [], []
    group_types = collections.Counter()
    skipped_types = collections.Counter()
    outer_index, offset = 0, 28
    frames_seen = 0
    source_ordinals = collections.Counter()

    def name(r):
        return strings[r.uint(4)] + "::" + strings[r.uint(4)]

    def profile(instance):
        return objects.get(instance) == "_debugger_::PxProfileZone"

    while offset < len(raw):
        group = Reader(raw[offset:offset + 24], offset)
        size, count, stream, timestamp = group.uint(4), group.uint(4), group.uint(8), group.uint(8)
        group.done()
        if count != 1 or not size or offset + 24 + size > len(raw):
            raise ValueError(f"unsupported group shape at {offset}")
        outer_index += 1
        r = Reader(raw[offset + 24:offset + 24 + size], offset + 24)
        kind = r.uint(1)
        group_types[kind] += 1
        if kind == 1:
            text, handle = r.text(), r.uint(4)
            if handle in strings and strings[handle] != text:
                raise ValueError("string handle redefinition")
            strings[handle] = text
        elif kind == 6:
            class_name, instance = name(r), r.uint(8)
            if instance in objects:
                raise ValueError("duplicate instance")
            objects[instance] = class_name
            generations[instance] = generations.get(instance, -1) + 1
            if profile(instance):
                if instance in profile_objects:
                    raise ValueError("profile instance reuse is outside this oracle")
                profile_objects[instance] = dict(created_event=outer_index, class_name=class_name)
        elif kind == 7:
            instance, prop, dtype, items = r.uint(8), strings[r.uint(4)], name(r), r.uint(4)
            length = r.uint(4)
            start = r.base + r.at
            payload = r.take(length)
            if profile(instance) and prop == "events":
                if dtype != "physx3::PvdU8" or items != length:
                    raise ValueError("profile property has an unsupported layout")
                rows = inner_events(payload, start, instance, outer_index)
                for row in rows:
                    row["received_frame"] = frames_seen
                events.extend(rows)
                buffers.append(dict(zone=instance, outer_event=outer_index, group_offset=offset,
                                    payload_offset=start, payload_bytes=length, sha256=sha256(payload),
                                    event_counts=dict(collections.Counter(row["type"] for row in rows))))
        elif kind == 8:
            instance, prop, dtype = r.uint(8), strings[r.uint(4)], name(r)
            if stream in partial or profile(instance):
                raise ValueError("nested transaction or unsupported chunked profile delivery")
            partial[stream] = (instance, prop, dtype)
        elif kind == 9:
            r.take(r.uint(4))
            r.uint(4)
            if stream not in partial:
                raise ValueError("append outside property transaction")
        elif kind == 10:
            del partial[stream]
        elif kind == 11:
            instance = r.uint(8)
            name(r)
            r.take(r.uint(4))
            if profile(instance):
                raise ValueError("profile message delivery is outside this oracle")
        elif kind == 12:
            if stream in message_groups:
                raise ValueError("nested message group")
            message_groups[stream] = name(r)
        elif kind == 13:
            instance = r.uint(8)
            r.take(r.uint(4))
            if stream not in message_groups or profile(instance):
                raise ValueError("missing group or unsupported profile message delivery")
        elif kind == 14:
            del message_groups[stream]
        elif kind == 15:
            instance = r.uint(8)
            if profile(instance):
                profile_objects[instance]["destroyed_event"] = outer_index
            del objects[instance]
        elif kind in (18, 19):
            instance, section, tick = r.uint(8), strings[r.uint(4)], r.uint(8)
            entry = dict(kind="begin" if kind == 18 else "end", id=instance,
                         name=section, tick=tick, outer_event=outer_index, capture_offset=r.base,
                         source_class=objects.get(instance), source_generation=generations.get(instance))
            if kind == 19 and section in ("frame", "ApexFrame"):
                entry.update(frame_index=frames_seen, source_ordinal=source_ordinals[(instance, section)])
                frames_seen += 1
                source_ordinals[(instance, section)] += 1
            sections.append(entry)
        elif kind == 24:
            instance, label = r.uint(8), r.text()
            if instance in zones:
                raise ValueError("duplicate profile zone metadata")
            zones[instance] = label
        elif kind == 25:
            instance, label, event, enabled = r.uint(8), r.text(), r.uint(2), r.uint(1)
            key = (instance, event)
            if key in names or enabled not in (0, 1):
                raise ValueError("duplicate profile event metadata or invalid boolean")
            names[key] = dict(name=label, enabled=bool(enabled))
        elif kind in (2, 3, 4, 5, 16, 17, 20, 21, 22, 23, 26, 27, 28):
            skipped_types[kind] += 1
            r.take(len(r.data) - r.at)
        else:
            raise ValueError(f"unknown outer event {kind}")
        r.done()
        offset += 24 + size
    if offset != len(raw) or partial or message_groups:
        raise ValueError("incomplete capture or transaction")
    if set(zones) != set(profile_objects):
        raise ValueError("profile zone metadata/instance mismatch")

    open_events = collections.defaultdict(list)
    max_depth = collections.Counter()
    unmatched = []
    value_events = []
    for event in events:
        key = tuple(event[field] for field in ("zone", "event", "thread", "context"))
        if (event["zone"], event["event"]) not in names:
            raise ValueError("undefined profile event name")
        if event["action"] == "start":
            open_events[key].append(event)
            max_depth[key] = max(max_depth[key], len(open_events[key]))
        elif event["action"] == "stop":
            if not open_events[key]:
                unmatched.append(event)
                continue
            start = open_events[key].pop()
            if event["tick"] < start["tick"]:
                raise ValueError("negative span duration")
            spans.append(dict(zone=key[0], event=key[1], thread=key[2], context=key[3],
                              start=start["tick"], end=event["tick"],
                              received_frame=event["received_frame"],
                              start_capture_offset=start["capture_offset"],
                              end_capture_offset=event["capture_offset"],
                              start_outer_event=start["outer_event"], end_outer_event=event["outer_event"]))
        else:
            value_events.append(event)

    fifo_starts = collections.defaultdict(collections.deque)
    fifo_spans = []
    for event in events:
        key = tuple(event[field] for field in ("zone", "event", "thread", "context"))
        if event["action"] == "start":
            fifo_starts[key].append(event)
        elif event["action"] == "stop" and fifo_starts[key]:
            start = fifo_starts[key].popleft()
            fifo_spans.append(dict(zone=key[0], event=key[1], thread=key[2], context=key[3],
                                   start=start["tick"], end=event["tick"],
                                   received_frame=event["received_frame"],
                                   start_capture_offset=start["capture_offset"],
                                   end_capture_offset=event["capture_offset"],
                                   start_outer_event=start["outer_event"], end_outer_event=event["outer_event"]))
    if len(fifo_spans) != len(spans):
        raise ValueError("pairing-policy-independent event counts disagree")

    keys = sorted({tuple(event[field] for field in ("zone", "event", "thread", "context"))
                   for event in events})
    catalog = []
    for zone, event, thread, context in keys:
        matching = [row for row in events if (row["zone"], row["event"], row["thread"], row["context"])
                    == (zone, event, thread, context)]
        completed = [row for row in spans if (row["zone"], row["event"], row["thread"], row["context"])
                     == (zone, event, thread, context)]
        starts = [row["tick"] for row in matching if row["action"] == "start"]
        catalog.append(dict(zone=zone, zone_name=zones[zone], event=event, event_name=names[(zone, event)]["name"],
                            thread=thread, context=context, first_start=min(starts, default=None),
                            complete_spans=len(completed), open_spans=len(open_events[(zone, event, thread, context)]),
                            unmatched_stops=sum(row in unmatched for row in matching),
                            total_ticks=sum(row["end"] - row["start"] for row in completed)))
    summary = dict(capture_sha256=sha256(raw), capture_bytes=len(raw),
                   capture_crc32=zlib.crc32(raw),
                   source_frames=frames_seen,
                   clock=dict(numerator=numerator, denominator=denominator),
                   outer_groups=outer_index, outer_event_counts=dict(group_types),
                   opaque_non_profile_event_counts=dict(skipped_types),
                   profile_objects=profile_objects, zones=zones,
                   registered_profile_events=len(names), payloads=len(buffers),
                   nonempty_payloads=sum(row["payload_bytes"] != 0 for row in buffers),
                   profile_bytes=sum(row["payload_bytes"] for row in buffers),
                   profile_event_counts=dict(collections.Counter(row["type"] for row in events)),
                   total_profile_events=len(events), completed_spans=len(spans),
                   open_starts=sum(map(len, open_events.values())),
                   unmatched_stops=len(unmatched), values=len(value_events), exact_keys=len(catalog),
                   pairing_policy="newest pending Start for matching zone/thread/event/context, from matched PVD3 static code",
                   fifo_lifo_different_spans=sum(a != b for a, b in zip(spans, fifo_spans)),
                   overlapping_start_keys=[dict(zone=k[0], event=k[1], thread=k[2], context=k[3], maximum_depth=v)
                                           for k, v in sorted(max_depth.items()) if v > 1],
                   completed_tick_range=dict(start=min((row["start"] for row in spans), default=None),
                                             end=max((row["end"] for row in spans), default=None)),
                   section_counts=[dict(id=instance, name=section,
                                        begins=sum(row["kind"] == "begin" for row in sections
                                                   if (row["id"], row["name"]) == (instance, section)),
                                        ends=sum(row["kind"] == "end" for row in sections
                                                 if (row["id"], row["name"]) == (instance, section)))
                                   for instance, section in sorted({(row["id"], row["name"]) for row in sections})])
    invariant_spans = [a for a, b in zip(spans, fifo_spans) if a == b]
    summary["fifo_lifo_invariant_spans"] = len(invariant_spans)
    return dict(summary=summary, buffers=buffers, events=events, spans=spans, fifo_spans=fifo_spans,
                invariant_spans=invariant_spans, catalog=catalog,
                sections=sections, metadata=[dict(zone=k[0], event=k[1], **v) for k, v in sorted(names.items())])


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("capture", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--sdk-root", type=Path, default=Path(".deps/physx-3.4"))
    args = parser.parse_args()
    result = read_capture(args.capture)
    result["summary"]["sdk_sources"] = [dict(path=f"PxShared/src/pvd/src/{filename}",
                                             sha256=sha256((args.sdk_root / "PxShared/src/pvd/src" / filename).read_bytes()))
                                         for filename in ("PxPvdCommStreamEvents.h", "PxPvdCommStreamTypes.h",
                                                          "PxProfileEvents.h", "PxProfileEventParser.h",
                                                          "PxProfileEventSerialization.h")]
    args.output.mkdir(parents=True, exist_ok=True)
    for name, value in result.items():
        (args.output / f"{name}.json").write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result["summary"], ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
