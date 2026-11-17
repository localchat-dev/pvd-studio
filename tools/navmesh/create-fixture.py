#!/usr/bin/env python3
"""Create a synthetic Detour MSET v1 fixture for overlay/render checks.

This is deliberately hand-authored geometry, not a Unity export or a production
map. Two adjacent tiles exercise detail vertices, sloped surfaces, external
polygon edges, and an off-mesh connection which must not become a surface.
The layout uses little-endian 32-bit Detour references and six vertices/poly.
"""

import argparse
import json
from pathlib import Path
import struct


def polygon(vertices, neighbors, area_and_type=0):
    assert len(vertices) <= 6 and len(neighbors) == len(vertices)
    return struct.pack(
        "<I6H6HHBB",
        0xFFFFFFFF,
        *(vertices + [0] * (6 - len(vertices))),
        *(neighbors + [0] * (6 - len(neighbors))),
        1,
        len(vertices),
        area_and_type,
    )


def tile(x, with_connection):
    left = -8.0 + x * 8.0
    right = left + 8.0
    heights = [0.08, 0.08, 0.08, 0.08] if x == 0 else [0.08, 0.08, 1.08, 1.08]
    vertices = [
        (left, heights[0], -4.0),
        (left, heights[1], 4.0),
        (right, heights[2], 4.0),
        (right, heights[3], -4.0),
    ]
    neighbors = [0, 0, 0x8000, 0] if x == 0 else [0x8004, 0, 0, 0]
    polys = polygon([0, 1, 2, 3], neighbors)
    connection = b""
    if with_connection:
        vertices.extend([(-1.0, 0.08, 0.0), (1.0, 0.205, 0.0)])
        polys += polygon([4, 5], [0, 0], area_and_type=1 << 6)
        connection = struct.pack("<7fHBBI", -1.0, 0.08, 0.0, 1.0, 0.205, 0.0, 0.5, 1, 1, 0, 99)

    center = ((left + right) * 0.5, 0.8 if x == 0 else 0.58, 0.0)
    link_count = 10 if with_connection else 6
    header = struct.pack(
        "<15I10f",
        0x444E4156,  # DNAV integer magic (bytes are VAND in little endian).
        7,
        x,
        0,
        0,
        100 + x,
        2 if with_connection else 1,
        len(vertices),
        link_count,
        1,  # Only ground polygons have a detail mesh.
        1,
        4,
        0,  # No optional BV tree.
        int(with_connection),
        1,  # First off-mesh polygon index, also the ground polygon count.
        2.0,
        0.5,
        0.9,
        left,
        0.0,
        -4.0,
        right,
        2.0,
        4.0,
        1.0,
    )
    details = struct.pack("<IIBB2x", 0, 0, 1, 4)
    triangles = bytes([0, 1, 4, 1, 1, 2, 4, 1, 2, 3, 4, 1, 3, 0, 4, 1])
    data = b"".join(
        [
            header,
            b"".join(struct.pack("<3f", *vertex) for vertex in vertices),
            polys,
            bytes(link_count * 12),
            details,
            struct.pack("<3f", *center),
            triangles,
            connection,
        ]
    )
    assert len(header) == 100
    assert len(data) == 100 + len(vertices) * 12 + len(polys) + link_count * 12 + 12 + 12 + 16 + len(connection)
    surface = [[vertices[a], vertices[b], center] for a, b in [(0, 1), (1, 2), (2, 3), (3, 0)]]
    return data, surface


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", nargs="?", type=Path, default=Path("output/navmesh-validation/sample.bin"))
    args = parser.parse_args()
    parts = [struct.pack("<III5fII", 0x4D534554, 1, 2, -8.0, 0.0, -4.0, 8.0, 8.0, 2, 4)]
    surfaces = []
    for x in range(2):
        data, surface = tile(x, with_connection=x == 0)
        parts.extend([struct.pack("<II", 8 + x * 4, len(data)), data])
        surfaces.extend(surface)
    blob = b"".join(parts)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_bytes(blob)
    expected = {
        "provenance": "Synthetic fixture; not a Unity export or a production map",
        "format": "MSET v1 / DNAV v7 / little endian / 32-bit refs / 6 vertices per polygon",
        "bytes": len(blob),
        "tiles": 2,
        "ground_polygons": 2,
        "off_mesh_connections": 1,
        "surface_triangles": surfaces,
        "polygon_edge_count": 8,
        "bounds_min": [-8.0, 0.08, -4.0],
        "bounds_max": [8.0, 1.08, 4.0],
    }
    args.output.with_suffix(".expected.json").write_text(json.dumps(expected, indent=2) + "\n", encoding="utf-8")
    print(f"Created {args.output}: {len(blob)} bytes, 2 tiles, 8 triangles, 1 off-mesh connection")


if __name__ == "__main__":
    main()
