#!/usr/bin/env python3
"""Load the actual experimental library; output measurements, never device claims."""
from __future__ import annotations
import argparse
import ctypes
import hashlib
import json
from pathlib import Path
import platform
import time


def smoke(path: Path) -> dict:
    library = ctypes.CDLL(str(path.resolve()))
    version = library.ak_m0_probe_version
    version.argtypes, version.restype = [], ctypes.c_uint32
    start = library.ak_m0_probe_start
    start.argtypes, start.restype = [], ctypes.c_int64
    for name in ('poll', 'cancel', 'release'):
        method = getattr(library, 'ak_m0_probe_' + name)
        method.argtypes, method.restype = [ctypes.c_uint64], ctypes.c_int32
    poll, cancel, release = (getattr(library, 'ak_m0_probe_' + name)
                             for name in ('poll', 'cancel', 'release'))
    assert version() == 0x00010000, 'ABI version mismatch'
    assert poll(0) == -3 and cancel(2**64 - 1) == -3
    samples = []
    for _ in range(3):
        begin = time.monotonic()
        handle = start()
        assert handle > 0, ('start rejected', handle)
        assert start() == -4, 'slot must stay busy until released'
        deadline = begin + 15
        while poll(handle) == 0:
            assert time.monotonic() < deadline, 'native probe timeout'
            time.sleep(0.005)
        assert poll(handle) == 1, ('loopback failed', poll(handle))
        while (released := release(handle)) == -4:
            assert time.monotonic() < deadline, 'release timeout'
            time.sleep(0.005)
        assert released == 1 and poll(handle) == -3 and release(handle) == -3
        samples.append(round((time.monotonic() - begin) * 1000, 3))
    handle = start()
    assert handle > 0
    cancellation = cancel(handle)
    assert cancellation in (-2, 1), ('unexpected cancellation result', cancellation)
    deadline = time.monotonic() + 15
    while (released := release(handle)) == -4:
        assert time.monotonic() < deadline, 'cancelled worker did not exit'
        time.sleep(0.005)
    assert released == cancellation and poll(handle) == -3
    return {
        'kind': 'host_native_library_loopback',
        'system': platform.system(), 'machine': platform.machine(),
        'abi': version(), 'loopback_ms': samples,
        'cancel_result': cancellation, 'native_library_bytes': path.stat().st_size,
        'native_library_sha256': hashlib.sha256(path.read_bytes()).hexdigest(),
        'scope': 'ctypes load + synthetic same-process UDP/TLS; not App size, LAN, or device acceptance',
    }


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('library', type=Path)
    args = parser.parse_args()
    print(json.dumps(smoke(args.library), indent=2))
