#!/usr/bin/env python3
"""Verify the shipped Windows GUI subsystem, duck icon and redirected CLI.

Run on Windows: python scripts/check-windows.py target/release/ducklocal.exe
Only Python's standard library is needed; no GUI is launched.
"""

import ctypes
from ctypes import wintypes
import json
from pathlib import Path
import struct
import subprocess
import sys


def verify(exe):
    data = exe.read_bytes()
    assert data[:2] == b"MZ", "Missing DOS header"
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    assert data[pe:pe + 4] == b"PE\0\0", "Missing PE header"
    subsystem = struct.unpack_from("<H", data, pe + 24 + 68)[0]
    assert subsystem == 2, f"Expected Windows GUI subsystem (2), got {subsystem}"

    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.LoadLibraryExW.argtypes = [wintypes.LPCWSTR, ctypes.c_void_p, wintypes.DWORD]
    kernel.LoadLibraryExW.restype = ctypes.c_void_p
    kernel.FindResourceW.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p]
    kernel.FindResourceW.restype = ctypes.c_void_p
    kernel.LoadResource.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
    kernel.LoadResource.restype = ctypes.c_void_p
    kernel.LockResource.argtypes = [ctypes.c_void_p]
    kernel.LockResource.restype = ctypes.c_void_p
    kernel.SizeofResource.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
    kernel.SizeofResource.restype = wintypes.DWORD
    kernel.FreeLibrary.argtypes = [ctypes.c_void_p]
    kernel.FreeLibrary.restype = wintypes.BOOL
    # Load as data, without executing the binary's entry point.
    module = kernel.LoadLibraryExW(str(exe), None, 0x02 | 0x20)
    if not module:
        raise ctypes.WinError(ctypes.get_last_error())

    def resource(kind, identifier):
        found = kernel.FindResourceW(module, identifier, kind)
        assert found, f"Missing resource type={kind}, id={identifier}"
        size = kernel.SizeofResource(module, found)
        address = kernel.LockResource(kernel.LoadResource(module, found))
        assert address and size, "Cannot read resource"
        return ctypes.string_at(address, size)

    try:
        group = resource(14, 1)  # GPUI loads RT_GROUP_ICON 1.
        icon = (Path(__file__).resolve().parent.parent / "assets/AppIcon.ico").read_bytes()
        assert group[:6] == icon[:6], "Icon frame count/type does not match the duck icon"
        count = struct.unpack_from("<H", icon, 4)[0]
        for i in range(count):
            original = 6 + i * 16
            embedded = 6 + i * 14
            # RC normalizes the PNG icon's plane count from 0 to 1.
            assert group[embedded:embedded + 4] == icon[original:original + 4]
            assert group[embedded + 6:embedded + 12] == icon[original + 6:original + 12]
            size, offset = struct.unpack_from("<II", icon, original + 8)
            identifier = struct.unpack_from("<H", group, embedded + 12)[0]
            assert resource(3, identifier) == icon[offset:offset + size], "Icon pixels differ"
    finally:
        kernel.FreeLibrary(module)

    version = subprocess.run([str(exe), "--version"], capture_output=True, check=True, timeout=30)
    assert version.stdout.startswith(b"ducklocal ") and not version.stderr
    query = subprocess.run(
        [str(exe), "query", "--sql-file", "-"], input=b"SELECT 42 AS answer",
        capture_output=True, check=True, timeout=30,
    )
    assert json.loads(query.stdout)["rows"] == [[42]] and not query.stderr
    print(f"Verified GUI subsystem, {count} duck icon sizes, CLI stdin/stdout: {exe}")
    print(f"Executable size: {len(data) / 1024 / 1024:.2f} MiB")


if __name__ == "__main__":
    verify(Path(sys.argv[1]).resolve())
