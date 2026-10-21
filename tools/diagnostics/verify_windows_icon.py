"""Compare the executable's Windows icon resources with the checked-in ICO."""

import argparse
import ctypes
import json
import os
from pathlib import Path
import struct


def icon_entries(data):
    reserved, kind, count = struct.unpack_from("<HHH", data)
    if (reserved, kind) != (0, 1):
        raise ValueError("Expected an ICO file")
    entries = {}
    for index in range(count):
        width, height, colors, reserved, planes, bits, size, offset = struct.unpack_from(
            "<BBBBHHII", data, 6 + index * 16
        )
        dimensions = (width or 256, height or 256)
        if dimensions in entries or reserved or offset + size > len(data):
            raise ValueError("Invalid ICO entry")
        payload = data[offset : offset + size]
        # rc represents a PNG's single image plane as 1 even when the ICO omits it.
        if planes == 0 and payload.startswith(b"\x89PNG\r\n\x1a\n"):
            planes = 1
        entries[dimensions] = (colors, planes, bits, payload)
    expected = {(size, size) for size in (16, 24, 32, 48, 64, 128, 256)}
    if set(entries) != expected:
        raise ValueError("ICO must contain the seven expected dimensions")
    return entries


def verify(executable, ico):
    if os.name != "nt":
        raise RuntimeError("Windows is required to inspect Windows resource loading")
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.LoadLibraryExW.argtypes = [ctypes.c_wchar_p, ctypes.c_void_p, ctypes.c_uint32]
    kernel.LoadLibraryExW.restype = ctypes.c_void_p
    kernel.FreeLibrary.argtypes = [ctypes.c_void_p]
    kernel.FreeLibrary.restype = ctypes.c_int
    kernel.FindResourceW.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p]
    kernel.FindResourceW.restype = ctypes.c_void_p
    kernel.SizeofResource.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
    kernel.SizeofResource.restype = ctypes.c_uint32
    kernel.LoadResource.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
    kernel.LoadResource.restype = ctypes.c_void_p
    kernel.LockResource.argtypes = [ctypes.c_void_p]
    kernel.LockResource.restype = ctypes.c_void_p
    user = ctypes.WinDLL("user32", use_last_error=True)
    user.LoadImageW.argtypes = [
        ctypes.c_void_p,
        ctypes.c_void_p,
        ctypes.c_uint32,
        ctypes.c_int,
        ctypes.c_int,
        ctypes.c_uint32,
    ]
    user.LoadImageW.restype = ctypes.c_void_p
    user.DestroyIcon.argtypes = [ctypes.c_void_p]
    user.DestroyIcon.restype = ctypes.c_int

    expected = icon_entries(ico.read_bytes())
    # Data-file loading inspects resources without executing the application.
    module = kernel.LoadLibraryExW(str(executable), None, 0x00000002 | 0x00000020)
    if not module:
        raise ctypes.WinError(ctypes.get_last_error())

    def resource(kind, ordinal):
        found = kernel.FindResourceW(module, ordinal, kind)
        if not found:
            raise ctypes.WinError(ctypes.get_last_error())
        size = kernel.SizeofResource(module, found)
        loaded = kernel.LoadResource(module, found)
        pointer = kernel.LockResource(loaded) if loaded else None
        if not pointer or not size:
            raise ctypes.WinError(ctypes.get_last_error())
        return ctypes.string_at(pointer, size)

    verified = []
    try:
        group = resource(14, 1)
        reserved, kind, count = struct.unpack_from("<HHH", group)
        if (reserved, kind, count) != (0, 1, len(expected)):
            raise ValueError("Unexpected RT_GROUP_ICON resource")
        if len(group) != 6 + count * 14:
            raise ValueError("Unexpected RT_GROUP_ICON size")
        for index in range(count):
            width, height, colors, reserved, planes, bits, size, ordinal = struct.unpack_from(
                "<BBBBHHIH", group, 6 + index * 14
            )
            dimensions = (width or 256, height or 256)
            payload = resource(3, ordinal)
            if (
                reserved
                or size != len(payload)
                or expected.pop(dimensions, None) != (colors, planes, bits, payload)
            ):
                raise ValueError(f"Embedded icon differs from the ICO at {dimensions}")
            native_icon = user.LoadImageW(module, 1, 1, *dimensions, 0)
            if not native_icon:
                raise ctypes.WinError(ctypes.get_last_error())
            if not user.DestroyIcon(native_icon):
                raise ctypes.WinError(ctypes.get_last_error())
            verified.append(list(dimensions))
        if expected:
            raise ValueError("Not all ICO entries were embedded")
    finally:
        kernel.FreeLibrary(module)
    return {"executable": str(executable), "resource": 1, "verified_sizes": sorted(verified)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("executable", type=Path)
    parser.add_argument(
        "--ico",
        type=Path,
        default=Path(__file__).resolve().parents[2]
        / "crates/pvd-desktop/assets/pvd-studio.ico",
    )
    args = parser.parse_args()
    print(json.dumps(verify(args.executable.resolve(), args.ico.resolve()), indent=2))


if __name__ == "__main__":
    main()
