"""Read-only exploration of PVD3 container metadata and bounded history pages."""
import argparse
import collections
import hashlib
import json
from pathlib import Path
import struct


class Reader:
    def __init__(self, data):
        self.data, self.pos = data, 0

    def take(self, count):
        if count < 0 or self.pos + count > len(self.data):
            raise ValueError(f"truncated at {self.pos}: {count} bytes")
        result = self.data[self.pos:self.pos + count]
        self.pos += count
        return result

    def u32(self):
        return struct.unpack('<I', self.take(4))[0]

    def array(self, read):
        count = self.u32()
        if count > 1_000_000:
            raise ValueError(f"array count exceeds limit: {count}")
        return [read() for _ in range(count)]


def metadata(data):
    r = Reader(data)
    version, next_class = r.u32(), r.u32()
    count, next_string = r.u32(), r.u32()
    strings = {0: ''}
    for _ in range(count):
        handle, length = r.u32(), r.u32()
        raw = r.take(length)
        if not raw or raw[-1] != 0:
            raise ValueError('unterminated metadata string')
        strings[handle] = raw[:-1].decode('utf-8')

    def name():
        namespace, label = strings[r.u32()], strings[r.u32()]
        return f'{namespace}::{label}' if namespace else label

    def optional(read):
        marker = r.u32()
        if marker not in (0, 1):
            raise ValueError(f"invalid pointer marker {marker} at {r.pos - 4}")
        return read() if marker else None

    def prop():
        p = {'owner_name': name(), 'owner': r.u32(), 'semantic_handle': r.u32(),
             'datatype': r.u32(), 'datatype_name': name(), 'kind': r.u32(),
             'id': r.u32(), 'offset32': r.u32(), 'offset64': r.u32()}
        p['values'] = r.array(lambda: (r.u32(), strings[r.u32()]))
        p['name'] = strings[r.u32()]
        p['semantic'] = strings.get(p['semantic_handle'])
        return p

    def cls():
        c = {'name': name(), 'id': r.u32(), 'base': r.u32(),
             'packed_width': r.u32(), 'packed_type': r.u32()}
        c['flags'] = list(r.take(2))
        c['sizes'] = [r.u32() for _ in range(6)]
        c['properties'] = r.array(r.u32)
        c['pointers32'] = r.array(lambda: (r.u32(), r.u32()))
        c['pointers64'] = r.array(lambda: (r.u32(), r.u32()))
        return c

    def message():
        m = {'class_name': name(), 'class_id': r.u32(), 'name': name(),
             'id': r.u32(), 'size': r.u32()}
        m['entries'] = r.array(lambda: {'datatype_name': name(), 'datatype': r.u32(),
                                      'offset': r.u32(), 'size': r.u32(),
                                      'dest_size': r.u32(), 'property': r.u32()})
        m['strings'] = r.array(r.u32)
        return m

    properties = r.array(lambda: optional(prop))
    classes = r.array(lambda: optional(cls))
    messages = r.array(lambda: optional(message))
    if r.pos != len(data):
        raise ValueError(f"metadata trailing bytes {len(data) - r.pos}")
    return {'version': version, 'next_class': next_class, 'next_string': next_string,
            'strings': strings, 'properties': properties, 'classes': classes,
            'messages': messages}


def inspect(data):
    r = Reader(data)
    magic, version, object_size, comm_size = [r.u32() for _ in range(4)]
    if magic != 0x36101BB0 or version != 1:
        raise ValueError('unsupported container header')
    model = metadata(r.take(object_size))
    communication = r.take(comm_size)
    r.take((r.pos // 4096 + 1) * 4096 - r.pos)
    total_events = struct.unpack('<Q', r.take(8))[0]
    pages = r.array(lambda: struct.unpack('<QQII', r.take(24)))
    snapshots = r.array(lambda: struct.unpack('<QQII', r.take(24)))
    instances = r.array(lambda: struct.unpack('<iQI', r.take(16)))
    base = (r.pos // 4096 + 1) * 4096
    events = []
    for first, position, count, length in pages:
        page = Reader(data[base + position:base + position + length])
        offsets = page.array(page.u32)
        if len(offsets) != count or first != len(events):
            raise ValueError('page event count or index mismatch')
        payload = page.take(len(page.data) - page.pos)
        for start, end in zip(offsets, offsets[1:] + [len(payload)]):
            if start > end or end > len(payload):
                raise ValueError('invalid event offset')
            events.append(payload[start:end])
    if len(events) != total_events:
        raise ValueError('event count mismatch')
    return model, communication, events, {'pages': pages, 'snapshots': snapshots,
                                        'instances': instances, 'data_base': base}


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('capture', type=Path)
    parser.add_argument('--metadata-json', type=Path)
    args = parser.parse_args()
    data = args.capture.read_bytes()
    model, communication, events, history = inspect(data)
    print('SHA256', hashlib.sha256(data).hexdigest())
    print('metadata', {k: len(model[k]) for k in ('strings', 'properties', 'classes', 'messages')})
    print('history', history)
    kinds = collections.Counter(struct.unpack_from('<I', event)[0] for event in events)
    print('event kinds', dict(kinds))
    for kind in kinds:
        samples = [e for e in events if struct.unpack_from('<I', e)[0] == kind][:3]
        print('kind', kind, [(len(e), e[:120].hex()) for e in samples])
    if args.metadata_json:
        with args.metadata_json.open('x', encoding='utf-8') as out:
            json.dump(model, out, indent=2, ensure_ascii=False)
