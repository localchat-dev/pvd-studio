"""Exercise the packaged Windows DLL through the same C ABI as the Editor."""

import argparse
import ctypes as ct
import json
import shutil
import socket
import statistics
import struct
import sys
import tarfile
import tempfile
import time
from pathlib import Path


class SceneEvents:
    def __init__(self, classes, fields=()):
        self.events = []
        names = ["physx3", "frame", *classes, "ObjectRef", "PxVec3", "Shapes", "Geometry",
                 "HalfExtents", "Scene", "GlobalPose", "LocalPose", "PxTransform", *fields]
        self.handles = {name: index for index, name in enumerate(names, 1)}
        for name, handle in self.handles.items():
            data = name.encode() + b"\0"
            self.events.append(struct.pack("<BI", 1, len(data)) + data + struct.pack("<I", handle))
        for name in classes:
            self.events.append(struct.pack("<BII", 2, 1, self.handles[name]))

    def create(self, oid, name):
        self.events.append(struct.pack("<BIIQ", 6, 1, self.handles[name], oid))

    def prop(self, owner, name, kind, data, items=1):
        self.events.append(struct.pack("<BQIIIII", 7, owner, self.handles[name], 1,
                                       self.handles[kind], items, len(data)) + data)

    def end_frame(self, tick):
        self.events.append(struct.pack("<BQIQ", 19, 10, self.handles["frame"], tick * 16667))

    def take_group(self):
        payload = b"".join(self.events)
        packet = struct.pack("<IIQQ", len(payload), len(self.events), 1, 0) + payload
        self.events.clear()
        return packet


STREAM_HEADER = struct.pack("<IIQQI", 837150850, 1, 1, 1000000, 1)


def stream_packets(grid=0, motion_frames=0, mesh_instances=1):
    yield STREAM_HEADER
    scene = SceneEvents(
        ["PxScene", "PxRigidStatic", "PxShape", "PxBoxGeometry", "PxTriangleMeshGeometry", "PxTriangleMesh"],
        ["TriangleMesh", "Points", "Triangles", "Scale.Scale", "Scale.Rotation", "PxQuat", "PvdU32"])
    for oid, name in [(10, "PxScene"), (20, "PxScene"), (30, "PxRigidStatic"),
                      (40, "PxShape"), (50, "PxBoxGeometry")]:
        scene.create(oid, name)
    scene.prop(30, "Shapes", "ObjectRef", struct.pack("<Q", 40))
    scene.prop(40, "Geometry", "ObjectRef", struct.pack("<Q", 50))
    for tick in range(6 + motion_frames):
        if tick == 3:
            scene.events.append(struct.pack("<BQ", 15, 40))
        if tick == 4:
            scene.create(40, "PxShape")
            scene.prop(30, "Shapes", "ObjectRef", struct.pack("<Q", 40))
            scene.prop(40, "Geometry", "ObjectRef", struct.pack("<Q", 50))
        if tick == 5:
            for oid, name in [(60, "PxTriangleMeshGeometry"), (70, "PxTriangleMesh")]:
                scene.create(oid, name)
            points = [0,0,0, 2,0,0, 2,3,0, 0,3,0]
            triangles = [0,1,2, 0,2,3]
            if grid:
                points = [value for y in range(grid + 1) for x in range(grid + 1)
                          for value in (x, y, 0)]
                triangles = []
                for y in range(grid):
                    for x in range(grid):
                        a = y * (grid + 1) + x
                        b = a + grid + 1
                        triangles.extend([a, a + 1, b, b, a + 1, b + 1])
            scene.prop(70, "Points", "PxVec3", struct.pack(f"<{len(points)}f", *points), len(points) // 3)
            scene.prop(70, "Triangles", "PvdU32", struct.pack(f"<{len(triangles)}I", *triangles), len(triangles))
            scene.prop(60, "Scale.Scale", "PxVec3", struct.pack("<3f", 1, 1, 1))
            scene.prop(60, "Scale.Rotation", "PxQuat", struct.pack("<4f", 0, 0, 0, 1))
            scene.prop(60, "TriangleMesh", "ObjectRef", struct.pack("<Q", 70))
            scene.prop(40, "Geometry", "ObjectRef", struct.pack("<Q", 60))
            for instance in range(1, mesh_instances):
                actor, shape = 100 + 2 * instance, 101 + 2 * instance
                scene.create(actor, "PxRigidStatic")
                scene.create(shape, "PxShape")
                scene.prop(actor, "Scene", "ObjectRef", struct.pack("<Q", 20))
                scene.prop(actor, "Shapes", "ObjectRef", struct.pack("<Q", shape))
                scene.prop(shape, "Geometry", "ObjectRef", struct.pack("<Q", 60))
        if tick != 3:
            scene.prop(30, "Scene", "ObjectRef", struct.pack("<Q", 10 if tick == 0 else 20))
            scene.prop(30, "GlobalPose", "PxTransform", struct.pack("<7f", 0, 0, 1, 0, 10*tick, 0, 0))
            scene.prop(40, "LocalPose", "PxTransform", struct.pack("<7f", 0, 0, 0, 1, 1, 2, 3))
            scene.prop(50, "HalfExtents", "PxVec3", struct.pack("<fff", tick + 1, 2, 3))
        scene.end_frame(tick)
        if (tick + 1) % 128 == 0:
            yield scene.take_group()
    if scene.events:
        yield scene.take_group()


def stream(grid=0, motion_frames=0, mesh_instances=1):
    return b"".join(stream_packets(grid, motion_frames, mesh_instances))


def verify(path, mode, output=None):
    dll = ct.CDLL(str(Path(path).resolve()))
    dll.pvd_request.argtypes = [ct.c_char_p]
    dll.pvd_request.restype = ct.c_void_p
    dll.pvd_free.argtypes = [ct.c_void_p]
    dll.pvd_free.restype = None
    dll.pvd_frame.argtypes = [ct.c_uint64, ct.c_uint64, ct.c_uint64, ct.POINTER(ct.c_int)]
    dll.pvd_frame.restype = ct.c_void_p
    dll.pvd_frame_free.argtypes = [ct.c_void_p, ct.c_int]
    dll.pvd_frame_free.restype = None
    cached = getattr(dll, "pvd_frame_cached", None)
    if cached:
        cached.argtypes = [ct.c_uint64] * 4 + [ct.POINTER(ct.c_int)]
        cached.restype = ct.c_void_p
    resources = getattr(dll, "pvd_frame_resources", None)
    if resources:
        resources.argtypes = [ct.c_uint64] * 4 + [ct.POINTER(ct.c_int)]
        resources.restype = ct.c_void_p

    def cached_frame(session, index, room, token):
        size = ct.c_int()
        ptr = cached(session, index, room, token, ct.byref(size))
        assert ptr and size.value >= 24, request("status")
        try:
            data = ct.string_at(ptr, size.value)
        finally:
            dll.pvd_frame_free(ptr, size.value)
        magic, token, base, count = struct.unpack_from("<4sQQI", data)
        assert magic == b"PVS2" and token != 0
        return token, base, count, data[24:]

    def request(op, expected_ok=True, **args):
        ptr = dll.pvd_request(json.dumps(dict(op=op, **args)).encode())
        try:
            data = json.loads(ct.string_at(ptr))
        finally:
            dll.pvd_free(ptr)
        assert data["ok"] == expected_ok, data
        return data

    def wait_status(predicate, timeout=10):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            status = request("status")["status"]
            if predicate(status):
                return status
            time.sleep(0.02)
        raise AssertionError(status)

    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary) / "缓存" / "PvdStudio"
        root.mkdir(parents=True)
        sentinel = root / "unowned.txt"
        sentinel.write_bytes(b"preserve")
        try:
            if mode == "--idle":
                verify_idle(root, wait_status, request)
                return
            request("start", root=str(root), address="127.0.0.1:0", recordHistory=mode != "--live-only")
            address = request("status")["status"]["address"]
            host, port = address.rsplit(":", 1)
            with socket.create_connection((host, int(port))) as peer:
                if mode == "--live-only":
                    verify_live_only(dll, peer, root, wait_status, cached_frame, request)
                    return
                if mode == "--portable":
                    verify_portable(peer, root, Path(temporary) / "单文件", output,
                                    wait_status, cached_frame, request)
                    return
                if mode == "--long-live":
                    verify_long_live(peer, root, wait_status, cached_frame)
                    return
                if mode == "--measure":
                    measure_surface(dll, peer, wait_status, request)
                    return
                if mode == "--focus":
                    verify_focus(dll, peer, wait_status, request)
                    return
                if mode == "--rays":
                    verify_rays(dll, peer, wait_status, request)
                    return
                if mode == "--queries":
                    verify_queries(dll, peer, wait_status, request)
                    return
                if mode == "--display":
                    verify_display(dll, peer, wait_status, request)
                    return
                peer.sendall(stream())
                status = wait_status(lambda s: any(x["frames"] == 6 for x in s["sessions"]))
                session = status["active"]
                assert session and request("clear")["released"] == 0
                assert cached, "expected pvd_frame_cached from the current Unity package"
                dll.pvd_frame_count.argtypes = [ct.c_uint64]
                dll.pvd_frame_count.restype = ct.c_int64
                wait_status(lambda _: dll.pvd_frame_count(session) == 6)
                assert dll.pvd_frame_count(0) == -1
                capture = Path(temporary) / "活动会话.pvdcapture"
                request("export", session=session, path=str(capture))

                extra_frame = struct.pack("<BQIQ", 19, 10, 2, 6 * 16667)
                peer.sendall(struct.pack("<IIQQ", len(extra_frame), 1, 1, 0) + extra_frame)
                wait_status(lambda s: s["sessions"][0]["frames"] == 7)

                def export_result():
                    return wait_status(lambda s: not s["export"]["running"])["export"]

                saved = export_result()
                assert saved["session"] == session and not saved["error"], saved
                capture_bytes = capture.read_bytes()
                assert saved["frames"] == 6 and saved["bytes"] == len(capture_bytes), saved
                assert capture_bytes.startswith(b"PVDCAP03")
                request("export", session=session, path=str(capture))
                assert export_result()["error"] and capture.read_bytes() == capture_bytes
                # Each record has 33 bytes of identity/state, a geometry kind and two poses.
                token, base, count, mesh = cached_frame(session, 5, 0, 0)
                assert base == 0 and count == 1 and mesh[33] == 4
                token, base, count, reused = cached_frame(session, 5, 0, token)
                assert base and count == 1 and reused == mesh[:33] + bytes([5]) + mesh[34:90]
                token, base, count, restored = cached_frame(session, 5, 0, 0)
                assert base == 0 and count == 1 and restored == mesh
                token, base, count, other_room = cached_frame(session, 5, 1, token)
                assert base == 0 and count == 0 and not other_room
                token, _, count, earlier = cached_frame(session, 2, 0, token)
                assert count == 1 and earlier[33] == 0
                token, _, count, restored = cached_frame(session, 5, 0, token)
                assert count == 1 and restored == mesh
                # Trace the Editor's resource-frame ABI through first load, rewind and scope changes.
                resource_token = 0
                for frame, room, reset, definitions, shapes, kind in [
                        (5, 0, True, 1, 1, 4), (2, 0, False, 0, 1, 0),
                        (5, 0, False, 1, 1, 4), (5, 1, True, 0, 0, None),
                        (5, 2, True, 1, 1, 4)]:
                    data = resource_frame(dll, session, frame, room, resource_token, request)
                    magic, next_token, acknowledged, count = struct.unpack_from("<4sQQI", data)
                    assert magic == b"PVS3" and next_token and count == definitions
                    assert acknowledged == (0 if reset else resource_token)
                    offset = 24
                    if definitions:
                        mesh_id, = struct.unpack_from("<Q", data, offset)
                        geometry = mesh[90:]
                        assert mesh_id != 0 and data[offset+8:offset+8+len(geometry)] == geometry
                        offset += 8 + len(geometry)
                    assert struct.unpack_from("<I", data, offset) == (shapes,)
                    records = data[offset+4:]
                    if shapes:
                        assert records[33] == kind and struct.unpack_from("<f", records, 34) == (10 * frame,)
                        if kind == 4:
                            assert struct.unpack("<Q", records[90:]) == (mesh_id,)
                        else:
                            assert struct.unpack("<3f", records[90:]) == (frame + 1, 2, 3)
                    else:
                        assert not records
                    resource_token = next_token
                for index, room, visible in [(5, 0, True), (4, 0, True), (3, 0, False), (2, 0, True),
                                              (0, 1, True), (1, 1, False), (1, 2, True)]:
                    size = ct.c_int()
                    ptr = dll.pvd_frame(session, index, room, ct.byref(size))
                    assert ptr and size.value > 0, request("status")
                    try:
                        data = ct.string_at(ptr, size.value)
                        magic, shapes = struct.unpack_from("<4sI", data)
                        assert magic == b"PVS1", "expected individual surface shapes"
                        assert shapes == int(visible)
                        if visible:
                            header = struct.Struct("<QQIQIBB")
                            keys = header.unpack_from(data, 8)
                            assert keys == (1 if index == 0 else 2, 30, 0, 40,
                                            int(index >= 4), 0, 4 if index == 5 else 0), keys
                            pose = struct.unpack_from("<14f", data, 8 + header.size)
                            assert pose[:7] == (10*index, 0, 0, 0, 0, 1, 0), pose
                            assert pose[7:14] == (1, 2, 3, 0, 0, 0, 1), pose
                            geometry = data[8 + header.size + 14*4:]
                            if index == 5:
                                assert struct.unpack("<2I12f6I", geometry) == (
                                    4, 6, 0,0,0, 2,0,0, 2,3,0, 0,3,0, 0,1,2, 0,2,3)
                            else:
                                assert struct.unpack("<3f", geometry) == (index+1, 2, 3)
                        else:
                            assert len(data) == 8
                    finally:
                        dll.pvd_frame_free(ptr, size.value)
            wait_status(lambda s: not s["active"])
            request("stop")
            finished = Path(temporary) / "已结束会话.pvdcapture"
            request("export", session=session, path=str(finished))
            result = export_result()
            assert not result["error"] and result["frames"] == 7 and finished.is_file(), result
            assert request("clear")["released"] == 0
            request("release")
            request("clear")
            assert not request("status")["status"]["sessions"]
            assert capture.read_bytes() == capture_bytes
            assert sentinel.read_bytes() == b"preserve"
        finally:
            request("shutdown")
    print("Unity DLL loopback, poses, generations, scenes, rewind, active and finished session exports, "
          "existing file preservation and cache cleanup passed")


def verify_idle(root, wait_status, request):
    windows = []
    for record_history in (False, True):
        request("start", root=str(root), address="127.0.0.1:0", recordHistory=record_history)
        address = request("status")["status"]["address"]
        host, port = address.rsplit(":", 1)
        with socket.create_connection((host, int(port))) as peer:
            status = wait_status(lambda s: s["active"] and s["sessions"])
            session = status["active"]
            assert bool(status["sessions"][-1].get("liveOnly")) != record_history, status
            wall_start = time.monotonic()
            cpu_start = time.process_time()
            time.sleep(1.5)
            cpu_seconds = time.process_time() - cpu_start
            wall_seconds = time.monotonic() - wall_start
            windows.append(dict(record_history=record_history, wall_ms=round(wall_seconds * 1000, 1),
                                cpu_ms=round(cpu_seconds * 1000, 1)))

            peer.sendall(stream())
            wait_status(lambda s: any(x["id"] == session and x["frames"] == 6 for x in s["sessions"]))
            request("stop")
            peer.settimeout(2)
            assert peer.recv(1) == b""
        assert not request("status")["status"]["listening"]
        request("clear")
    print(json.dumps(dict(idle=windows, received_after_idle=True, stop_closed_peers=True)))
    assert all(window["cpu_ms"] < window["wall_ms"] / 3 for window in windows), windows


def verify_live_only(dll, peer, root, wait_status, cached_frame, request):
    frame_count = 12000
    latest = (1 << 64) - 1
    started = time.monotonic()
    sent = 0
    for packet in stream_packets(motion_frames=frame_count - 6):
        peer.sendall(packet)
        sent += len(packet)
    status = wait_status(lambda s: s["sessions"] and s["sessions"][0]["frames"] == frame_count, timeout=90)
    elapsed = time.monotonic() - started
    session = status["sessions"][0]
    assert session.get("liveOnly") and session["active"] and status["active"] == session["id"], status
    assert status["totalBytes"] == 0 and status["clearableBytes"] == 0 and not status["error"], status
    assert list(root.iterdir()) == [root / "unowned.txt"], list(root.iterdir())

    session_id = session["id"]
    dll.pvd_frame_count.argtypes = [ct.c_uint64]
    dll.pvd_frame_count.restype = ct.c_int64
    wait_status(lambda _: dll.pvd_frame_count(session_id) == frame_count)
    size = ct.c_int()
    earlier = dll.pvd_frame(session_id, 0, 0, ct.byref(size))
    if earlier:
        dll.pvd_frame_free(earlier, size.value)
        raise AssertionError("the first frame was unexpectedly retained")
    output = root / "live.pvdcapture"
    request("export", expected_ok=False, session=session_id, path=str(output))
    assert not output.exists()

    token, base, shapes, records = cached_frame(session_id, latest, 2, 0)
    assert not base and shapes == 1 and records[33] == 4
    assert struct.unpack_from("<f", records, 34) == (10 * (frame_count - 1),)
    _, _, shapes, records = cached_frame(session_id, latest, 1, token)
    assert shapes == 0 and not records
    request("release")
    # Reopening a view while the producer is idle still sees its last complete frame.
    _, _, shapes, records = cached_frame(session_id, latest, 2, 0)
    assert shapes == 1 and struct.unpack_from("<f", records, 34) == (10 * (frame_count - 1),)
    scene_removed = struct.pack("<BQ", 15, 20) + struct.pack("<BQIQ", 19, 10, 2, frame_count * 16667)
    peer.sendall(struct.pack("<IIQQ", len(scene_removed), 2, 1, 0) + scene_removed)
    status = wait_status(lambda s: s["sessions"][0]["frames"] == frame_count + 1)
    assert status["sessions"][0]["rooms"] == [{"number": 1}], status
    peer.shutdown(socket.SHUT_WR)
    wait_status(lambda s: not s["active"] and not s["sessions"])

    address = status["address"]
    host, port = address.rsplit(":", 1)
    with socket.create_connection((host, int(port))) as reconnected:
        reconnected.sendall(shape_query_stream())
        status = wait_status(lambda s: s["sessions"] and s["sessions"][0]["frames"] == 3)
        new_session = status["active"]
        assert new_session and new_session != session_id and not status["error"], status
        data = query_frame(dll, new_session, latest)
        assert data[:4] == b"PVS5" and struct.unpack_from("<I", data, 24) == (1,)
        ray_flags, ray_count, query_flags, line_count = struct.unpack_from("<4I", data, 28 + 102)
        assert (ray_flags, ray_count, query_flags, line_count) == (0, 0, 1, 8192)
        request("stop")
    assert not request("status")["status"]["sessions"]
    assert list(root.iterdir()) == [root / "unowned.txt"]
    assert (root / "unowned.txt").read_bytes() == b"preserve"
    request("start", root=str(root), address=address, recordHistory=False)
    request("stop")
    print(json.dumps(dict(mode="live-only", frames=frame_count, wire_bytes=sent, cache_bytes=0,
                          seconds=round(elapsed, 3), latest_after_release=True, query_lines=line_count,
                          reconnect_and_rebind=True)))


def verify_portable(peer, cache_root, export_dir, output, wait_status, cached_frame, request):
    frame_count = 8200
    export_dir.mkdir()
    capture = export_dir / "会话.pvdcapture"
    for packet in stream_packets(motion_frames=frame_count - 6):
        peer.sendall(packet)
    peer.shutdown(socket.SHUT_WR)
    status = wait_status(lambda s: s["sessions"] and not s["active"], timeout=180)
    assert not status["error"] and len(status["sessions"]) == 1, status
    session = status["sessions"][0]
    assert session["frames"] == frame_count, session
    request("stop")

    checked = [0, 4095, 4096, 8191, 8192, frame_count - 1]
    for index in checked:
        _, base, count, record = cached_frame(session["id"], index, 0, 0)
        assert base == 0 and count == 1, (index, base, count)
        assert struct.unpack_from("<f", record, 34)[0] == 10 * index, index
    request("export", session=session["id"], path=str(capture))
    saved = wait_status(lambda s: not s["export"]["running"], timeout=300)["export"]
    assert not saved["error"] and saved["frames"] == frame_count and saved["parts"] > 1, saved

    request("release")
    request("clear")
    status = request("status")["status"]
    assert not status["sessions"] and not status["error"] and status["totalBytes"] == 0, status
    request("shutdown")
    cache_files = [entry for entry in cache_root.rglob("*") if entry.is_file()]
    assert cache_files == [cache_root / "unowned.txt"], cache_files
    assert list(export_dir.iterdir()) == [capture], list(export_dir.iterdir())
    assert capture.stat().st_size == saved["bytes"]
    with capture.open("rb") as source:
        assert source.read(8) == b"PVDCAP04"
        if output is not None:
            source.seek(0)
            output.parent.mkdir(parents=True, exist_ok=True)
            target = output.open("xb")
            try:
                with target:
                    shutil.copyfileobj(source, target)
            except BaseException:
                output.unlink()
                raise
    print(json.dumps(dict(format="PVDCAP04", frames=frame_count, bytes=saved["bytes"],
                          parts=saved["parts"], checked_frames=checked,
                          output=str(output) if output is not None else None)))


def resource_frame(dll, session, frame, room, token, request):
    size = ct.c_int()
    ptr = dll.pvd_frame_resources(session, frame, room, token, ct.byref(size))
    assert ptr, request("status")
    try:
        assert 28 <= size.value <= 32 * 1024**2, size.value
        return ct.string_at(ptr, size.value)
    finally:
        dll.pvd_frame_free(ptr, size.value)


def focus_stream():
    grid = 160
    instances = 40
    spacing = 10000
    scene = SceneEvents(
        ["PxScene", "PxRigidStatic", "PxRigidDynamic", "PxShape",
         "PxTriangleMeshGeometry", "PxTriangleMesh"],
        ["TriangleMesh", "Points", "Triangles", "Scale.Scale", "Scale.Rotation", "PxQuat", "PvdU32"])
    scene.create(10, "PxScene")
    scene.create(20, "PxTriangleMesh")
    points = [value for y in range(grid + 1) for x in range(grid + 1) for value in (x, y, 0)]
    indices = []
    for y in range(grid):
        for x in range(grid):
            a = y * (grid + 1) + x
            b = a + grid + 1
            indices.extend([a, a + 1, b, b, a + 1, b + 1])
    mesh_bytes = 4 * (len(points) + len(indices))
    assert instances * mesh_bytes > 32 * 1024**2
    scene.prop(20, "Points", "PxVec3", struct.pack(f"<{len(points)}f", *points), len(points) // 3)
    scene.prop(20, "Triangles", "PvdU32", struct.pack(f"<{len(indices)}I", *indices), len(indices))
    for instance in range(instances):
        actor, shape, geometry = 100 + instance, 200 + instance, 300 + instance
        scene.create(actor, "PxRigidStatic")
        scene.create(shape, "PxShape")
        scene.create(geometry, "PxTriangleMeshGeometry")
        scene.prop(actor, "Scene", "ObjectRef", struct.pack("<Q", 10))
        scene.prop(actor, "Shapes", "ObjectRef", struct.pack("<Q", shape))
        rotation_w = 1 if instance else 2
        scene.prop(actor, "GlobalPose", "PxTransform",
                   struct.pack("<7f", 0, 0, 0, rotation_w, instance * spacing, 0, 0))
        scene.prop(shape, "Geometry", "ObjectRef", struct.pack("<Q", geometry))
        if instance == 0:
            # Unity normalizes the actor pose before applying the shape's local offset.
            scene.prop(shape, "LocalPose", "PxTransform", struct.pack("<7f", 0, 0, 0, 1, 2000, 0, 0))
        scene.prop(geometry, "TriangleMesh", "ObjectRef", struct.pack("<Q", 20))
        scene.prop(geometry, "Scale.Scale", "PxVec3", struct.pack("<3f", 1 + instance / 8, 1, 1))
        scene.prop(geometry, "Scale.Rotation", "PxQuat", struct.pack("<4f", 0, 0, 0, 1))
    scene.create(50, "PxRigidDynamic")
    for frame in range(2):
        anchor_x = spacing + 100 if frame else 2100
        scene.prop(50, "Scene", "ObjectRef", struct.pack("<Q", 10))
        scene.prop(50, "GlobalPose", "PxTransform", struct.pack("<7f", 0, 0, 0, 1, anchor_x, 80, 0))
        scene.end_frame(frame)
    return STREAM_HEADER + scene.take_group(), instances, mesh_bytes


def verify_display(dll, peer, wait_status, request):
    grid = 256
    static_count = 2400
    frame_count = 25
    scene = SceneEvents(
        ["PxScene", "PxRigidStatic", "PxRigidDynamic", "PxShape", "PxBoxGeometry",
         "PxTriangleMeshGeometry", "PxTriangleMesh"],
        ["TriangleMesh", "Points", "Triangles", "Scale.Scale", "Scale.Rotation", "PxQuat", "PvdU32"])
    scene.create(10, "PxScene")
    scene.create(30, "PxBoxGeometry")
    scene.prop(30, "HalfExtents", "PxVec3", struct.pack("<3f", 1, 1, 1))
    for oid, name in [(40, "PxRigidStatic"), (41, "PxShape"), (42, "PxTriangleMeshGeometry"),
                      (43, "PxTriangleMesh"), (50, "PxRigidDynamic"), (51, "PxShape")]:
        scene.create(oid, name)
    for actor, shape, geometry in [(40, 41, 42), (50, 51, 30)]:
        scene.prop(actor, "Scene", "ObjectRef", struct.pack("<Q", 10))
        scene.prop(actor, "Shapes", "ObjectRef", struct.pack("<Q", shape))
        scene.prop(shape, "Geometry", "ObjectRef", struct.pack("<Q", geometry))
    points = [value for y in range(grid + 1) for x in range(grid + 1) for value in (x, y, 0)]
    indices = []
    for y in range(grid):
        for x in range(grid):
            a = y * (grid + 1) + x
            b = a + grid + 1
            indices.extend([a, a + 1, b, b, a + 1, b + 1])
    mesh_bytes = 4 * (len(points) + len(indices))
    scene.prop(43, "Points", "PxVec3", struct.pack(f"<{len(points)}f", *points), len(points) // 3)
    scene.prop(43, "Triangles", "PvdU32", struct.pack(f"<{len(indices)}I", *indices), len(indices))
    scene.prop(42, "TriangleMesh", "ObjectRef", struct.pack("<Q", 43))
    scene.prop(42, "Scale.Scale", "PxVec3", struct.pack("<3f", 1, 1, 1))
    scene.prop(42, "Scale.Rotation", "PxQuat", struct.pack("<4f", 0, 0, 0, 1))
    for instance in range(static_count):
        actor, shape = 100 + instance, 100 + static_count + instance
        scene.create(actor, "PxRigidStatic")
        scene.create(shape, "PxShape")
        scene.prop(actor, "Scene", "ObjectRef", struct.pack("<Q", 10))
        scene.prop(actor, "Shapes", "ObjectRef", struct.pack("<Q", shape))
        scene.prop(actor, "GlobalPose", "PxTransform",
                   struct.pack("<7f", 0, 0, 0, 1, 5000 + 25 * instance, 0, 0))
        scene.prop(shape, "Geometry", "ObjectRef", struct.pack("<Q", 30))

    def add_frame(tick):
        scene.prop(50, "GlobalPose", "PxTransform", struct.pack("<7f", 0, 0, 0, 1, 50 + tick / 16, 50, 0))
        if tick in (6, 7):
            x, y = (52, 50) if tick == 6 else (5000, 0)
            scene.prop(100, "GlobalPose", "PxTransform", struct.pack("<7f", 0, 0, 0, 1, x, y, 0))
        scene.end_frame(tick)

    add_frame(0)
    peer.sendall(STREAM_HEADER + scene.take_group())
    status = wait_status(lambda s: s["sessions"] and s["sessions"][0]["frames"] == 1, timeout=120)
    session = status["active"]
    fetch = dll.pvd_frame_resources_near
    fetch.argtypes = [ct.c_uint64] * 4 + [ct.c_uint32, ct.c_uint64, ct.c_uint32] + [ct.c_float] * 4 + [ct.POINTER(ct.c_int)]
    fetch.restype = ct.c_void_p
    token = 0

    def read(frame, x, y, actors):
        nonlocal token
        size = ct.c_int()
        before = time.perf_counter()
        ptr = fetch(session, frame, 0, token, 1, 0, 0, x, y, 0, 5, ct.byref(size))
        assert ptr, request("status")["status"]["frameError"]
        try:
            data = ct.string_at(ptr, size.value)
        finally:
            dll.pvd_frame_free(ptr, size.value)
        elapsed = 1000 * (time.perf_counter() - before)
        magic, next_token, base, definitions = struct.unpack_from("<4sQQI", data)
        assert magic == b"PVS3" and next_token and base == token
        token = next_token
        offset = 24
        points = []
        for _ in range(definitions):
            mesh_id, vertices, indices = struct.unpack_from("<QII", data, offset)
            assert mesh_id and vertices and indices
            offset += 16
            points.extend(struct.iter_unpack("<3f", data[offset:offset + 12 * vertices]))
            offset += 12 * vertices + 4 * indices
        count, = struct.unpack_from("<I", data, offset)
        offset += 4
        actual = {}
        for _ in range(count):
            actor, = struct.unpack_from("<Q", data, offset + 8)
            kind = data[offset + 33]
            assert kind in (0, 4)
            actual[actor], = struct.unpack_from("<f", data, offset + 34)
            offset += 90 + (12 if kind == 0 else 8)
        assert offset == len(data) and set(actual) == set(actors), (frame, x, y, actual)
        if 50 in actual:
            assert actual[50] == 50 + frame / 16
        return len(data), elapsed, points

    first_bytes, first_ms, first_points = read(0, 50, 50, (40, 50))
    same_frame = [read(0, 50 + (i % 5) / 8, 50, (40, 50))[1] for i in range(20)]
    for tick in range(1, frame_count):
        add_frame(tick)
    peer.sendall(scene.take_group())
    wait_status(lambda s: s["sessions"][0]["frames"] == frame_count, timeout=120)
    forward = [read(frame, 51, 50, (40, 50, 100) if frame == 6 else (40, 50))[1]
               for frame in range(1, frame_count)]
    far_bytes, _, far_points = read(frame_count - 1, 230, 230, (40,))
    read(6, 51, 50, (40, 50, 100))
    read(0, 50, 50, (40, 50))
    skipped = [read(frame, 51, 50, (40, 50, 100) if frame == 6 else (40, 50))[1]
               for frame in range(3, frame_count, 3)]
    print(json.dumps(dict(outside_static_actors=static_count, full_terrain_bytes=mesh_bytes,
                          first_bytes=first_bytes, far_bytes=far_bytes, first_ms=round(first_ms, 3),
                          same_frame_ms=round(statistics.median(same_frame), 3),
                          forward_ms=round(statistics.median(forward), 3),
                          skipped_forward_ms=round(statistics.median(skipped), 3))))
    assert first_bytes < mesh_bytes // 2 and far_bytes < mesh_bytes // 2, "local views should load only nearby terrain"
    assert first_points and min(point[0] for point in first_points) < 50 < max(point[0] for point in first_points)
    assert far_points and min(point[0] for point in far_points) < 230 < max(point[0] for point in far_points)


def verify_focus(dll, peer, wait_status, request):
    payload, instances, mesh_bytes = focus_stream()
    peer.sendall(payload)
    status = wait_status(lambda s: s["sessions"] and s["sessions"][0]["frames"] == 2, timeout=120)
    session = status["active"]
    size = ct.c_int()
    unfiltered = dll.pvd_frame_resources(session, 0, 0, 0, ct.byref(size))
    if unfiltered:
        dll.pvd_frame_free(unfiltered, size.value)
        raise AssertionError("all displayed resources must exceed the packet budget")
    assert "32 MiB" in request("status")["status"]["frameError"]
    assert hasattr(dll, "pvd_frame_resources_near"), "missing native spatial display filter"
    fetch = dll.pvd_frame_resources_near
    fetch.argtypes = [ct.c_uint64] * 4 + [ct.c_uint32, ct.c_uint64, ct.c_uint32] + [ct.c_float] * 4 + [ct.POINTER(ct.c_int)]
    fetch.restype = ct.c_void_p

    def near(frame, token, mode, actor, generation, center, radius, expected, definitions, room=0):
        ptr = fetch(session, frame, room, token, mode, actor, generation, *center, radius, ct.byref(size))
        error = request("status")["status"]["frameError"]
        if expected is None:
            if ptr:
                dll.pvd_frame_free(ptr, size.value)
                raise AssertionError("invalid focus must return an error")
            assert error and size.value == 0
            return token, 0
        assert ptr, error
        try:
            data = ct.string_at(ptr, size.value)
        finally:
            dll.pvd_frame_free(ptr, size.value)
        magic, next_token, base, count = struct.unpack_from("<4sQQI", data)
        assert magic == b"PVS3" and next_token and base == token
        assert count in (0, 1) if definitions is None else count == definitions
        offset = 24
        if count:
            mesh_id, vertices, index_count = struct.unpack_from("<QII", data, offset)
            visible_mesh_bytes = 12 * vertices + 4 * index_count
            assert mesh_id and 0 < visible_mesh_bytes <= mesh_bytes
            offset += 16 + visible_mesh_bytes
        assert struct.unpack_from("<I", data, offset) == (len(expected),)
        records = data[offset+4:]
        actors = [entry[1] for entry in struct.iter_unpack("<QQIQIBB14fQ", records)]
        assert actors == expected, (actors, expected)
        return next_token, len(data)

    token, first_bytes = near(0, 0, 1, 0, 0, (2100, 80, 0), 5, [100], 1)
    token, reused_bytes = near(0, token, 1, 0, 0, (2100, 80, 0), 5, [100], 0)
    token, _ = near(0, token, 1, 0, 0, (2110, 80, 0), 5, [100], None)
    token, _ = near(0, token, 1, 0, 0, (5000, 80, 0), 5, [], 0)
    token, _ = near(1, token, 2, 50, 0, (0, 0, 0), 5, [101], 1)
    token, _ = near(0, token, 2, 50, 0, (0, 0, 0), 5, [100], 1)
    near(0, token, 2, 50, 1, (0, 0, 0), 5, None, 0)
    near(0, token, 1, 0, 0, (0, 0, 0), 0, None, 0)
    near(0, token, 1, 0, 0, (2100, 80, 0), 5, [100], 0)
    near(0, 0, 1, 0, 0, (2100, 80, 0), 5, [100], 1, room=1)
    print(json.dumps(dict(mesh_instances=instances, unfiltered_mesh_bytes=instances * mesh_bytes,
                          local_shapes=1, first_bytes=first_bytes, retained_mesh_frame_bytes=reused_bytes,
                          checked_recorded_anchor_rewind=True)))


def ray_stream():
    scene = SceneEvents(["PxScene", "PxRigidStatic", "PxShape", "PxBoxGeometry"],
                        ["PvdRaycast", "PvdSqHit", "SceneQueries.Raycasts", "SceneQueries.Hits",
                         "BatchedQueries.Raycasts", "BatchedQueries.Hits"])
    for oid, name in [(10, "PxScene"), (20, "PxScene"), (30, "PxRigidStatic"),
                      (40, "PxShape"), (50, "PxBoxGeometry")]:
        scene.create(oid, name)
    scene.prop(30, "Scene", "ObjectRef", struct.pack("<Q", 10))
    scene.prop(30, "Shapes", "ObjectRef", struct.pack("<Q", 40))
    scene.prop(40, "Geometry", "ObjectRef", struct.pack("<Q", 50))
    scene.prop(50, "HalfExtents", "PxVec3", struct.pack("<3f", 1, 2, 3))

    def ray(prefix, origin, distance, hits=0):
        fields = struct.pack("<6I7f4xQII", 1, 0, 0, 0, 0, 0, *origin, 1, 0, 0,
                             distance, 0, 0, hits)
        return fields + (prefix + ".Hits\0").encode()

    def queries(room, prefix, records, hits=()):
        scene.prop(room, prefix + ".Raycasts", "PvdRaycast", b"".join(records), len(records))
        scene.prop(room, prefix + ".Hits", "PvdSqHit", b"".join(hits), len(hits))

    regular, batched = "SceneQueries", "BatchedQueries"
    hits = [struct.pack("<QQII9f4x", 40, 30, 0, 1, x, 0, 0, 0, 0, 0, x + 10, 0, 0)
            for x in (-5, 1)]
    queries(10, regular, [ray(regular, (-10, 0, 0), 20, len(hits)), ray(regular, (-10, 8, 0), 20)], hits)
    queries(10, batched, [ray(batched, (-4, 0, 0), 8)])
    queries(20, regular, [ray(regular, (-5, 12, 0), 10)])
    scene.end_frame(0)
    queries(10, regular, [ray(regular, (-1, 0, 1), 2)])
    queries(10, batched, [])
    queries(20, regular, [])
    scene.end_frame(1)
    queries(10, regular, [])
    scene.end_frame(2)
    queries(10, regular, [ray(regular, (-1, 0, 0), 2)] * 2049)
    scene.end_frame(3)
    return STREAM_HEADER + scene.take_group()


def verify_rays(dll, peer, wait_status, request):
    peer.sendall(ray_stream())
    status = wait_status(lambda s: s["sessions"] and s["sessions"][0]["frames"] == 4)
    session = status["active"]
    assert hasattr(dll, "pvd_frame_view"), "missing recorded raycast overlay"
    fetch = dll.pvd_frame_view
    fetch.argtypes = [ct.c_uint64] * 4 + [ct.c_uint32, ct.c_uint64, ct.c_uint32] + [ct.c_float] * 4 + [ct.POINTER(ct.c_int)]
    fetch.restype = ct.c_void_p
    ray = struct.Struct("<B9f")

    def read(frame, room, token=0, radius=0):
        size = ct.c_int()
        ptr = fetch(session, frame, room, token, int(radius > 0), 0, 0, 0, 0, 0, radius, ct.byref(size))
        assert ptr, request("status")["status"]["frameError"]
        try:
            data = ct.string_at(ptr, size.value)
        finally:
            dll.pvd_frame_free(ptr, size.value)
        magic, next_token, base, definitions, shapes = struct.unpack_from("<4sQQII", data)
        assert magic == b"PVS4" and next_token and base == token
        assert definitions == 0 and shapes == int(room != 2)
        offset = 28 + 102 * shapes  # Surface record plus three box half extents.
        flags, count = struct.unpack_from("<II", data, offset)
        assert len(data) == offset + 8 + ray.size * count and flags in (0, 1)
        return next_token, flags, list(ray.iter_unpack(data[offset+8:]))

    token, limited, first = read(0, 0)
    hit = (3, -10, 0, 0, 10, 0, 0, -5, 0, 0)
    assert limited == 0 and len(first) == 4 and hit in first, first
    token, limited, cropped = read(0, 0, token, radius=3)
    assert limited == 0 and sorted(cropped) == [
        (0, -3, 0, 0, 3, 0, 0, 0, 0, 0), (3, -3, 0, 0, 3, 0, 0, 1, 0, 0)], cropped
    token, _, next_frame = read(1, 0, token)
    assert next_frame == [(0, -1, 0, 1, 1, 0, 1, 0, 0, 0)], next_frame
    token, _, empty = read(2, 0, token)
    assert not empty
    token, limited, capped = read(3, 0, token)
    assert limited == 1 and len(capped) == 2048
    _, _, rewind = read(0, 0, token)
    assert rewind == first
    _, _, second_room = read(0, 2)
    assert second_room == [(0, -5, 12, 0, 5, 12, 0, 0, 0, 0)]
    print(json.dumps(dict(full_rays=len(first), cropped_rays=len(cropped), capped_rays=len(capped),
                          room_filter=True, recorded_hit=True, cleared_and_rewound=True)))


def shape_query_stream():
    regular, batched = "SceneQueries", "BatchedQueries"
    arrays = ("GeometryList", "PoseList", "FilterDataList", "Hits", "Sweeps", "Overlaps", "Raycasts")
    scene = SceneEvents(
        ["PxScene", "PxRigidStatic", "PxShape", "PxBoxGeometry", "PxSphereGeometry", "PxCapsuleGeometry",
         "PxConvexMeshGeometry", "PxConvexMesh"],
        ["PvdSweep", "PvdOverlap", "PvdRaycast", "PvdSqHit", "PvdF32", "U32Array4", "Radius", "HalfHeight",
         "ConvexMesh", "Scale.Scale", "Scale.Rotation", "PxQuat", "Points", "HullPolygons", "PolygonIndexes",
         "PvdHullPolygonData", "PvdU8", *(prefix + "." + suffix for prefix in (regular, batched) for suffix in arrays)])
    for oid, kind in [(10, "PxScene"), (20, "PxScene"), (30, "PxRigidStatic"), (40, "PxShape"),
                      (60, "PxCapsuleGeometry"), (61, "PxSphereGeometry"), (62, "PxBoxGeometry"),
                      (63, "PxConvexMeshGeometry"), (64, "PxConvexMesh")]:
        scene.create(oid, kind)
    scene.prop(30, "Scene", "ObjectRef", struct.pack("<Q", 10))
    scene.prop(30, "Shapes", "ObjectRef", struct.pack("<Q", 40))
    scene.prop(40, "Geometry", "ObjectRef", struct.pack("<Q", 62))
    scene.prop(60, "Radius", "PvdF32", struct.pack("<f", 1))
    scene.prop(60, "HalfHeight", "PvdF32", struct.pack("<f", 2))
    scene.prop(61, "Radius", "PvdF32", struct.pack("<f", 1))
    scene.prop(62, "HalfExtents", "PxVec3", struct.pack("<3f", 1, 2, 3))
    scene.prop(63, "ConvexMesh", "ObjectRef", struct.pack("<Q", 64))
    scene.prop(63, "Scale.Scale", "PxVec3", struct.pack("<3f", 1, 1, 1))
    scene.prop(63, "Scale.Rotation", "PxQuat", struct.pack("<4f", 0, 0, 0, 1))
    scene.prop(64, "Points", "PxVec3", struct.pack("<12f", 0, 0, 0, 2, 0, 0, 0, 2, 0, 0, 0, 2), 4)
    hull = [(0, 2, 1), (0, 1, 3), (0, 3, 2), (1, 2, 3)]
    scene.prop(64, "HullPolygons", "PvdHullPolygonData",
               b"".join(struct.pack("<HBB", 3, 3 * i, 0) for i in range(len(hull))), len(hull))
    scene.prop(64, "PolygonIndexes", "PvdU8", bytes(v for face in hull for v in face), 12)

    def ref(start, count):
        return struct.pack("<QII", 0, start, count)

    def references(prefix, fields):
        ranges = b"".join(ref(start, count) for _, start, count in fields)
        names = b"".join((prefix + "." + name).encode() + b"\0" for name, _, _ in fields)
        return ranges + names

    def prep(room, prefix, geometry, origins):
        scene.prop(room, prefix + ".GeometryList", "ObjectRef", struct.pack("<2Q", *geometry), 2)
        scene.prop(room, prefix + ".PoseList", "PxTransform",
                   b"".join(struct.pack("<7f", 0, 0, 0, 1, *p) for p in origins), 2)
        scene.prop(room, prefix + ".FilterDataList", "U32Array4", bytes(32), 2)

    def sweep(prefix, kind, index, distance, hits=0):
        return struct.pack("<II4f", kind, 0, 1, 0, 0, distance) + references(prefix, [
            ("GeometryList", index, 1), ("PoseList", index, 1), ("FilterDataList", index, 1), ("Hits", 0, hits)])

    def overlap(prefix, kind, geom, position):
        payload = struct.pack("<6I7f4x", kind, 0, 0, 0, 0, 0, 0, 0, 0, 1, *position)
        return payload + references(prefix, [("GeometryList", geom, 1), ("Hits", 0, 0)])

    prep(10, regular, [60, 62], [(-10, 0, 0), (0, 10, 0)])
    prep(10, batched, [61, 63], [(0, 30, 0), (0, 40, 0)])
    prep(20, regular, [62, 62], [(4, 60, 0), (0, 61, 0)])
    hit = struct.pack("<QQII9f4x", 40, 30, 0, 1, 0, 0, 0, 0, 0, 0, 10, 0, 0)
    scene.prop(10, regular + ".Hits", "PvdSqHit", hit)
    scene.prop(10, regular + ".Sweeps", "PvdSweep", sweep(regular, 9, 0, 20, hits=1))
    scene.prop(10, batched + ".Sweeps", "PvdSweep", sweep(batched, 9, 0, 4))
    scene.prop(10, regular + ".Overlaps", "PvdOverlap", overlap(regular, 5, 1, (0, 10, 0)))
    scene.prop(10, batched + ".Overlaps", "PvdOverlap", overlap(batched, 7, 1, (0, 40, 0)))
    scene.prop(20, regular + ".Sweeps", "PvdSweep", sweep(regular, 8, 0, 4))
    ray_fields = struct.pack("<6I7f4xQII", 1, 0, 0, 0, 0, 0, -2, 0, 1, 1, 0, 0, 4, 0, 0, 0)
    scene.prop(10, regular + ".Raycasts", "PvdRaycast", ray_fields + b"SceneQueries.Hits\0")
    scene.end_frame(0)
    for room in (10, 20):
        for prefix in (regular, batched):
            for suffix, kind in [("Sweeps", "PvdSweep"), ("Overlaps", "PvdOverlap"), ("Raycasts", "PvdRaycast")]:
                scene.prop(room, prefix + "." + suffix, kind, b"", 0)
    scene.end_frame(1)
    repeated = overlap(regular, 5, 1, (0, 10, 0))
    scene.prop(10, regular + ".Overlaps", "PvdOverlap", repeated * 1000, 1000)
    scene.end_frame(2)
    return STREAM_HEADER + scene.take_group()


def query_frame(dll, session, frame, room=0, base_token=0, radius=0, include_rays=True):
    assert hasattr(dll, "pvd_frame_queries"), "missing shape-query overlay on the packed frame ABI"
    fetch = dll.pvd_frame_queries
    fetch.argtypes = ([ct.c_uint64] * 4 + [ct.c_uint32, ct.c_uint64, ct.c_uint32] +
                      [ct.c_float] * 4 + [ct.c_uint32, ct.POINTER(ct.c_int)])
    fetch.restype = ct.c_void_p
    size = ct.c_int()
    ptr = fetch(session, frame, room, base_token, int(radius > 0), 0, 0, 0, 0, 0, radius,
                int(include_rays), ct.byref(size))
    assert ptr, "could not read a shape query frame"
    try:
        return ct.string_at(ptr, size.value)
    finally:
        dll.pvd_frame_free(ptr, size.value)


def verify_queries(dll, peer, wait_status, request):
    peer.sendall(shape_query_stream())
    status = wait_status(lambda s: s["sessions"] and s["sessions"][0]["frames"] == 3)
    session = status["active"]
    line = struct.Struct("<B6f")

    def read(frame, room, base_token=0, radius=0, include_rays=True):
        data = query_frame(dll, session, frame, room, base_token, radius, include_rays)
        magic, token, base, definitions, shapes = struct.unpack_from("<4sQQII", data)
        assert magic == b"PVS5" and token and base == base_token
        assert definitions == 0 and shapes == int(room != 2)
        offset = 28 + 102 * shapes
        ray_flags, ray_count = struct.unpack_from("<II", data, offset)
        assert ray_flags == 0 and ray_count == int(frame == 0 and room != 2 and include_rays), ray_count
        offset += 8 + 37 * ray_count
        flags, count = struct.unpack_from("<II", data, offset)
        assert flags in (0, 1) and count <= 8192
        segments = list(line.iter_unpack(data[offset + 8:]))
        assert len(segments) == count and all(record[0] in (0, 1, 2) for record in segments)
        return token, flags, segments

    token, limited, full = read(0, 0)
    assert not limited and len(full) > 30
    assert (0, -10, 0, 0, 10, 0, 0) in full, "capsule sweep trajectory"
    assert any(line[0] == 2 and line[1] < 0 < line[4] and line[2:4] == (0, 0) and line[5:7] == (0, 0)
               for line in full), "recorded position at the origin"
    assert any(line[0] == 0 and 29 <= line[2] <= 31 for line in full), "batched sphere sweep"
    assert any(line[0] == 1 and 8 <= line[2] <= 12 for line in full), "box overlap"
    assert any(line[0] == 1 and 40 <= line[2] <= 42 for line in full), "batched convex overlap"
    token, limited, cropped = read(0, 0, token, radius=3)
    assert not limited and (0, -3, 0, 0, 3, 0, 0) in cropped, "sweep crossing the local sphere"
    assert all(sum(v*v for v in point) <= 9.001 for _, *coords in cropped for point in (coords[:3], coords[3:]))
    token, _, empty = read(1, 0, token)
    assert not empty
    token, limited, dense = read(2, 0, token)
    assert limited and 0 < len(dense) <= 8192
    _, _, rewind = read(0, 0, token, include_rays=False)
    assert rewind == full
    _, _, room = read(0, 2)
    assert room and all(line[2] >= 57 and line[5] >= 57 for line in room)
    print(json.dumps(dict(shape_lines=len(full), cropped_lines=len(cropped), limited_lines=len(dense),
                          capsule_sweep=True, batched_sweep=True, convex_overlap=True, rewind=True)))


def measure_surface(dll, peer, wait_status, request):
    grid = 64
    motion_frames = 24
    instances = 240
    unique_bytes = 8 + 12 * (grid + 1)**2 + 24 * grid**2
    assert instances * unique_bytes > 32 * 1024**2
    payload = stream(grid=grid, motion_frames=motion_frames, mesh_instances=instances)
    start = time.perf_counter()
    peer.sendall(payload)
    status = wait_status(lambda s: any(x["frames"] == 6 + motion_frames for x in s["sessions"]))
    publish_ms = 1000 * (time.perf_counter() - start)
    session = status["active"]
    # Exercise the previous default path as the control on exactly the same input.
    old_size = ct.c_int()
    old = dll.pvd_frame_cached(session, 5, 0, 0, ct.byref(old_size))
    if old:
        dll.pvd_frame_free(old, old_size.value)
        raise AssertionError("fixture must exceed the PVS2 expanded-frame budget")
    assert "32 MiB" in request("status")["status"]["frameError"]
    assert hasattr(dll, "pvd_frame_resources"), "missing shared mesh/pose transfer"
    token = 0
    lengths, times = [], []
    record = struct.Struct("<QQIQIBB14fQ")
    for frame in range(5, 6 + motion_frames):
        start = time.perf_counter()
        data = resource_frame(dll, session, frame, 0, token, request)
        times.append(1000 * (time.perf_counter() - start))
        lengths.append(len(data))
        magic, next_token, base, definitions = struct.unpack_from("<4sQQI", data)
        assert magic == b"PVS3" and next_token and base == token
        assert definitions == int(frame == 5)
        offset = 24
        if definitions:
            mesh_id, vertices, indices = struct.unpack_from("<QII", data, offset)
            assert mesh_id and vertices == (grid + 1)**2 and indices == grid**2 * 6
            offset += 8 + unique_bytes
        count, = struct.unpack_from("<I", data, offset)
        assert count == instances and len(data) == offset + 4 + record.size * count
        poses = {}
        for _, actor, _, _, _, _, kind, *values in record.iter_unpack(data[offset+4:]):
            assert kind == 4 and values[-1] == mesh_id
            poses[actor] = values[0]
        assert len(poses) == instances and poses[30] == 10 * frame
        token = next_token
    print(json.dumps(dict(mesh_instances=instances, triangles_per_mesh=2*grid*grid, poses=motion_frames,
                          expanded_mesh_bytes=instances * unique_bytes, mesh_definitions_sent=1,
                          receive_to_publish_ms=round(publish_ms, 2),
                          first_frame_bytes=lengths[0], median_motion_bytes=statistics.median(lengths[1:]),
                          median_read_copy_ms=round(statistics.median(times[1:]), 3))))


def verify_long_live(peer, root, wait_status, cached_frame):
    actor_count = 6
    static_geometry_count = 76000
    frame_count = 18432
    scene = SceneEvents(
        ["PxScene", "PxRigidDynamic", "PxShape", "PxBoxGeometry"])
    scene.create(10, "PxScene")
    scene.create(50, "PxBoxGeometry")
    scene.prop(50, "HalfExtents", "PxVec3", struct.pack("<3f", 1, 2, 3))
    # Geometry present at the first boundary contributes to every bank's reader
    # directory even though the writer sees no growth from these objects later.
    for asset in range(static_geometry_count):
        scene.create(10000 + asset, "PxBoxGeometry")
        scene.prop(10000 + asset, "HalfExtents", "PxVec3", struct.pack("<3f", 1, 2, 3))
    for actor in range(actor_count):
        scene.create(100 + actor, "PxRigidDynamic")
        scene.create(1000 + actor, "PxShape")
        scene.prop(100 + actor, "Scene", "ObjectRef", struct.pack("<Q", 10))
        scene.prop(100 + actor, "Shapes", "ObjectRef", struct.pack("<Q", 1000 + actor))
        scene.prop(1000 + actor, "Geometry", "ObjectRef", struct.pack("<Q", 50))

    peer.sendall(STREAM_HEADER)
    sent = len(STREAM_HEADER)
    session = wait_status(lambda s: s["active"])["active"]

    def published(status, target):
        assert not status["error"] and not status["frameError"], status
        assert status["active"] == session, status
        return status["sessions"][0]["frames"] == target

    def check_frame(index):
        _, base, count, records = cached_frame(session, index, 0, 0)
        record = struct.Struct("<QQIQIBB17f")
        assert base == 0 and count == actor_count and len(records) == count * record.size, (index, base, count)
        actors = set()
        for room, oid, generation, shape, shape_generation, activity, kind, *pose in record.iter_unpack(records):
            actor = oid - 100
            assert (room, generation, shape, shape_generation, activity, kind) == (
                1, 0, 1000 + actor, 0, 1, 0), (index, oid)
            assert pose == [index / 16, actor, 0, 0, 0, 0, 1,
                            0, 0, 0, 0, 0, 0, 1, 1, 2, 3], (index, oid, pose)
            actors.add(actor)
        assert actors == set(range(actor_count)), (index, actors)

    start = time.monotonic()
    for tick in range(frame_count):
        for actor in range(actor_count):
            scene.prop(100 + actor, "GlobalPose", "PxTransform",
                 struct.pack("<7f", 0, 0, 0, 1, tick / 16, actor, 0))
        scene.end_frame(tick)
        if (tick + 1) % 64 == 0:
            packet = scene.take_group()
            peer.sendall(packet)
            sent += len(packet)
        if (tick + 1) % 1024 == 0:
            status = wait_status(lambda s: published(s, tick + 1), timeout=180)
            check_frame(tick)
            print(f'{tick + 1} frames published, {status["totalBytes"]} cache bytes', flush=True)

    # A PVDCAP03 bank starts with the metadata entry and its inclusive source range.
    banks = []
    for path in (root / "history").glob("*/*.pvdhistory"):
        with path.open("rb") as f:
            if f.read(8) == b"PVDCAP03":
                assert f.read(1) == bytes([5])
                f.seek(8 + 1 + 8 + 32 + 8)
                banks.append(struct.unpack("<QQ", f.read(16)))
    banks.sort()
    assert len(banks) >= 2 and banks[0][0] == 0, ("expected multiple sealed banks", banks)
    # Check both sides of the 0.2.2 fixture stop and the reported live-session stop.
    boundaries = {0, 8097, 8098, 8236, 8237, frame_count - 1}
    expected_first = 0
    for first, last in banks:
        assert first == expected_first and first <= last < frame_count, banks
        boundaries.update([first, last, min(last + 1, frame_count - 1)])
        expected_first = last + 1
    for index in sorted(boundaries, reverse=True):
        check_frame(index)

    peer.shutdown(socket.SHUT_WR)
    status = wait_status(lambda s: not s["active"], timeout=180)
    assert not status["error"] and status["sessions"][0]["frames"] == frame_count, status
    check_frame(frame_count - 1)
    print(json.dumps(dict(frames=frame_count, actors=actor_count, static_geometries=static_geometry_count, wire_bytes=sent,
                          cache_bytes=status["totalBytes"], sealed_ranges=banks,
                          checked_frames=sorted(boundaries), seconds=round(time.monotonic() - start, 1))))


def verify_package(path, mode, output=None):
    native = "package/Editor/Plugins/x86_64/pvd_unity.dll"
    required = ["package/package.json", "package/Editor/ViewerWindow.cs",
                "package/Editor/SceneOverlay.cs", "package/Editor/SurfaceFrame.cs",
                "package/Editor/SurfaceGeometry.cs", "package/Editor/FrameWorker.cs",
                "package/Editor/FrameWorker.cs.meta", native, native + ".meta"]
    with tempfile.TemporaryDirectory() as temp, tarfile.open(path, "r:gz") as archive:
        for name in required:
            assert archive.getmember(name).isfile(), name
        assert json.load(archive.extractfile(required[0]))["name"] == "com.pvdstudio.viewer"
        dll = Path(temp) / "pvd_unity.dll"
        dll.write_bytes(archive.extractfile(native).read())
        # The child must exit to unload its DLL before Windows can remove the directory.
        import subprocess
        if mode != "--portable":
            subprocess.run([sys.executable, __file__, str(dll)], check=True)
        if mode:
            command = [sys.executable, __file__, str(dll), mode]
            if output is not None:
                command.extend(["--output", str(output)])
            subprocess.run(command, check=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("path", type=Path, help="Unity package .tgz or exported native DLL")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--measure", action="store_const", dest="mode", const="--measure",
                      help="measure surface frame size and read time")
    mode.add_argument("--focus", action="store_const", dest="mode", const="--focus",
                      help="check local display, moving center and replay in an oversized scene")
    mode.add_argument("--rays", action="store_const", dest="mode", const="--rays",
                      help="check captured raycasts, hit markers, room and radius filters, and replay")
    mode.add_argument("--queries", action="store_const", dest="mode", const="--queries",
                      help="check captured swept shapes and overlaps, clipping, hits, and replay")
    mode.add_argument("--display", action="store_const", dest="mode", const="--display",
                      help="measure local terrain transfer, moving center on a paused frame and sequential native display reads")
    mode.add_argument("--long-live", action="store_const", dest="mode", const="--long-live",
                      help="check 18,432 live frames with a dense initial resource directory")
    mode.add_argument("--live-only", action="store_const", dest="mode", const="--live-only",
                      help="check latest-frame display without disk history and reject rewind or export")
    mode.add_argument("--idle", action="store_const", dest="mode", const="--idle",
                      help="check CPU use while TCP peers are idle in both receiving modes")
    mode.add_argument("--portable", action="store_const", dest="mode", const="--portable",
                      help="export 8,200 frames to one PVDCAP04 file and clear session history")
    parser.add_argument("--output", type=Path, help="keep the --portable capture at this new .pvdcapture path")
    args = parser.parse_args()
    if args.output is not None and (args.mode != "--portable" or args.output.suffix.lower() != ".pvdcapture"
                                    or args.output.exists()):
        parser.error("--output requires --portable and a new .pvdcapture file")
    if args.path.suffix == ".tgz":
        verify_package(args.path, args.mode, args.output)
    else:
        verify(args.path, args.mode, args.output)
