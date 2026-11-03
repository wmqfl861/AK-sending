#!/usr/bin/env python3
"""Explicit two-device synthetic LAN experiment; never transfers user files."""
from __future__ import annotations
import argparse
import ctypes
import ipaddress
import json
from pathlib import Path
import sys
import time

LIMIT = 4096
MAX_OFFER = LIMIT * 2 + 100


def ipv4(text: str) -> int:
    value = ipaddress.IPv4Address(text)
    # Explicit RFC1918, not Python's wider is_private special-address definition.
    allowed = any(value in ipaddress.IPv4Network(net) for net in
                  ('10.0.0.0/8', '172.16.0.0/12', '192.168.0.0/16', '127.0.0.1/32'))
    if not allowed:
        raise ValueError('address-outside-experiment-scope')
    return int(value)


def decode_offer(text: str) -> tuple[str, int, bytes]:
    if len(text) > MAX_OFFER:
        raise ValueError('offer-too-large')
    fields = text.strip().split('|')
    if len(fields) != 4 or fields[0] != 'AKL1':
        raise ValueError('offer-version')
    ip, port, der = fields[1:]
    ipv4(ip)
    if not port.isascii() or not port.isdecimal() or not 1024 <= int(port) <= 65535:
        raise ValueError('offer-port')
    if not der or len(der) > LIMIT * 2 or len(der) % 2 or any(c not in '0123456789abcdefABCDEF' for c in der):
        raise ValueError('offer-certificate')
    return ip, int(port), bytes.fromhex(der)


class LanNative:
    def __init__(self, path: Path):
        self.library = ctypes.CDLL(str(path.resolve()))
        u32, u64, i32, i64 = ctypes.c_uint32, ctypes.c_uint64, ctypes.c_int32, ctypes.c_int64
        signatures = {
            'version': ([], u32), 'prepare': ([u32, u32], i64),
            'poll': ([u64], i32), 'read': ([u64, u32, u32], i32),
            'push': ([u64, u32, u32], i32), 'configure': ([u64, u32, u32, u32], i32),
            'start': ([u64], i32), 'cancel': ([u64], i32), 'release': ([u64], i32),
        }
        for name, (args, result) in signatures.items():
            fn = getattr(self.library, 'ak_m0_lan_' + name)
            fn.argtypes, fn.restype = args, result
            setattr(self, name, fn)
        if self.version() != 1:
            raise RuntimeError('lan-abi-version')

    def data(self, handle: int, kind: int) -> bytes:
        count = self.read(handle, kind, 2**32 - 1)
        if not 0 < count <= LIMIT:
            raise RuntimeError('public-data-unavailable')
        values = [self.read(handle, kind, i) for i in range(count)]
        if any(not 0 <= byte < 256 for byte in values):
            raise RuntimeError('public-data-read-failed')
        return bytes(values)

    def wait_prepared(self, handle: int) -> None:
        deadline = time.monotonic() + 10
        while self.poll(handle) == 10:
            if time.monotonic() > deadline:
                raise TimeoutError('identity-timeout')
            time.sleep(0.01)
        if self.poll(handle) != 11:
            raise RuntimeError('identity-failed')

    def configure_offer(self, handle: int, offer: str, role: int) -> int:
        ip, port, der = decode_offer(offer)
        for index, byte in enumerate(der):
            if self.push(handle, index, byte) != 0:
                raise RuntimeError('certificate-staging-rejected')
        return self.configure(handle, ipv4(ip), port, role)

    def drain(self, handle: int) -> int:
        self.cancel(handle)
        deadline = time.monotonic() + 15
        while (result := self.release(handle)) == -4:
            if time.monotonic() >= deadline:
                raise TimeoutError('worker-not-released-do-not-unload')
            time.sleep(0.01)
        return result


def worker(api: LanNative, ip: str, port: int) -> int:
    """Machine driver for synthetic tests; public offers travel only over private pipes."""
    handle = api.prepare(ipv4(ip), port)
    if handle <= 0:
        raise RuntimeError('prepare-rejected')
    try:
        api.wait_prepared(handle)
        print(json.dumps({'offer': f'AKL1|{ip}|{port}|{api.data(handle, 0).hex()}'}), flush=True)
        for _ in range(2000):  # Test control protocol is bounded too.
            line = sys.stdin.readline(MAX_OFFER + 512)
            if not line:
                break
            if not line.endswith('\n'):
                raise ValueError('control-line-too-long')
            request = json.loads(line)
            name = request.get('command')
            if name == 'configure':
                result = api.configure_offer(handle, request['offer'], request['role'])
            elif name == 'fingerprint':
                result = api.data(handle, request['kind']).hex()
            elif name in ('poll', 'start', 'cancel', 'release'):
                result = getattr(api, name)(handle)
            elif name == 'quit':
                result = api.drain(handle)
                print(json.dumps({'result': result}), flush=True)
                return 0
            else:
                raise ValueError('control-command-rejected')
            print(json.dumps({'result': result}), flush=True)
    finally:
        api.drain(handle)
    return 0


def interactive(api: LanNative, ip: str, port: int, role: int) -> int:
    handle = api.prepare(ipv4(ip), port)
    if handle <= 0:
        raise RuntimeError('prepare-rejected')
    try:
        api.wait_prepared(handle)
        print('Synthetic LAN probe only; no file access. Offers contain public identity/address.')
        print(f'AKL1|{ip}|{port}|{api.data(handle, 0).hex()}')
        print('Local certificate SHA256: ' + api.data(handle, 1).hex())
        print('Paste peer offer (trusted exchange; never upload private keys):', flush=True)
        peer = sys.stdin.readline(MAX_OFFER + 2)
        if api.configure_offer(handle, peer, role) != 12:
            raise RuntimeError('pairing-rejected')
        print('PEER certificate SHA256: ' + api.data(handle, 2).hex())
        print('Compare that entire digest on the OTHER DEVICE. Type CONFIRM to authorize this one '
              + ('synthetic receive/listen' if role == 0 else 'synthetic send') + ':', flush=True)
        if sys.stdin.readline(32).strip() != 'CONFIRM':
            return 2
        if api.start(handle) != 13:
            raise RuntimeError('start-rejected')
        deadline = time.monotonic() + 190
        last = None
        while (status := api.poll(handle)) >= 10:
            if status != last:
                print('status=' + str(status), flush=True)
                last = status
            if time.monotonic() >= deadline:
                raise TimeoutError('experiment-timeout')
            time.sleep(0.2)
        print('status=' + str(status))
        return 0 if status == 1 else 1
    finally:
        api.drain(handle)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--library', type=Path, required=True)
    parser.add_argument('--bind', required=True)
    parser.add_argument('--port', type=int, required=True)
    parser.add_argument('--role', choices=['receive', 'send'], default='receive')
    parser.add_argument('--worker', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    if not 1024 <= args.port <= 65535:
        parser.error('port-outside-experiment-scope')
    try:
        api = LanNative(args.library)
        return worker(api, args.bind, args.port) if args.worker else interactive(api, args.bind, args.port, int(args.role == 'send'))
    except (Exception, KeyboardInterrupt):
        # Do not put addresses, certificates, raw provider errors or user paths in logs.
        print('LAN_PROBE_FAILED_OR_CANCELLED', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
