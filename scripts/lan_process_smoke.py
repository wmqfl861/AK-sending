#!/usr/bin/env python3
"""Two separate processes, optionally in separate pre-created Linux network stacks."""
from __future__ import annotations
import argparse
import json
from pathlib import Path
import queue
import socket
import subprocess
import sys
import threading
import time
from lan_probe import MAX_OFFER, decode_offer


class Peer:
    def __init__(self, library: Path, ip: str, port: int, namespace: str | None = None):
        command = [sys.executable, str(Path(__file__).with_name('lan_probe.py')),
                   '--library', str(library.resolve()), '--bind', ip, '--port', str(port), '--worker']
        if namespace:
            command = ['ip', 'netns', 'exec', namespace, *command]
        self.process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.DEVNULL, text=True, encoding='utf-8')
        self.lines: queue.Queue = queue.Queue(maxsize=16)
        def reader():
            try:
                for _ in range(2000):
                    line = self.process.stdout.readline(MAX_OFFER + 512)
                    if not line or not line.endswith('\n'): break
                    self.lines.put(json.loads(line), timeout=2)
            except Exception:
                pass
            finally:
                try: self.lines.put({'error': 'child-ended'}, timeout=2)
                except queue.Full: pass
        self.reader = threading.Thread(target=reader, daemon=True)
        self.reader.start()
        try:
            self.offer = self.get()['offer']
            decode_offer(self.offer)
        except BaseException:
            self.close(force=True)
            raise

    def get(self):
        try: value = self.lines.get(timeout=12)
        except queue.Empty: raise TimeoutError('child-response-timeout') from None
        if 'error' in value: raise RuntimeError('child-failed')
        return value

    def rpc(self, command, **fields):
        self.process.stdin.write(json.dumps({'command': command, **fields}) + '\n')
        self.process.stdin.flush()
        return self.get()['result']

    def close(self, force=False):
        try:
            if self.process.poll() is None and not force:
                self.rpc('quit')
                self.process.wait(timeout=5)
        except Exception:
            pass
        finally:
            if self.process.poll() is None:
                self.process.kill()
                self.process.wait(timeout=5)
            self.process.stdin.close()
            self.process.stdout.close()
            self.reader.join(timeout=3)


def port(ip):
    # Only used for loopback CI. Physical devices use explicitly chosen fixed ports.
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.bind((ip, 0))
        return sock.getsockname()[1]


def run(library: Path, ips, namespaces):
    cases = []
    for case in ('explicit-pair-success', 'wrong-pin-rejected', 'cancel-listener'):
        peers = []
        started = time.monotonic()
        try:
            ports = [port(ip) if not ns else 42424 for ip, ns in zip(ips, namespaces)]
            if ips[0] == ips[1] and ports[0] == ports[1]:
                ports[1] = port(ips[1])
            for ip, p, ns in zip(ips, ports, namespaces):
                peers.append(Peer(library, ip, p, ns))
            left, right = peers
            remote_offer = left.offer
            if case == 'wrong-pin-rejected':
                other = Peer(library, ips[0], ports[0] + 1 if ports[0] < 65535 else ports[0] - 1, namespaces[0])
                peers.append(other)
                # Public cert of a different endpoint pinned at the expected address.
                remote_offer = '|'.join(left.offer.split('|')[:3] + [other.offer.split('|')[3]])
            assert left.rpc('configure', offer=right.offer, role=1) == 12
            assert right.rpc('configure', offer=remote_offer, role=0) == 12
            # Compare real certificate IDs out of band in this trusted synthetic driver.
            assert left.rpc('fingerprint', kind=2) == right.rpc('fingerprint', kind=1)
            if case != 'wrong-pin-rejected':
                assert right.rpc('fingerprint', kind=2) == left.rpc('fingerprint', kind=1)
            assert left.rpc('poll') == right.rpc('poll') == 12  # Config alone never starts.
            assert right.rpc('start') == 13
            limit = time.monotonic() + 10
            while right.rpc('poll') == 13:
                assert time.monotonic() < limit
                time.sleep(.01)
            assert right.rpc('poll') in (14, 16)
            while right.rpc('poll') == 14:
                assert time.monotonic() < limit
                time.sleep(.01)
            assert right.rpc('poll') == 16  # Actual bound listener, not arbitrary sleep.
            if case == 'cancel-listener':
                assert right.rpc('cancel') == -2
                assert right.rpc('start') == -6
                assert left.rpc('poll') == 12
                assert left.rpc('cancel') == -2
                expected = [-2, -2]
            else:
                assert left.rpc('start') == 13
                limit = time.monotonic() + 20
                while True:
                    result = [p.rpc('poll') for p in (left, right)]
                    if all(s < 10 for s in result): break
                    assert time.monotonic() < limit, 'pair-timeout'
                    time.sleep(.02)
                if case == 'explicit-pair-success':
                    assert result == [1, 1], 'pair-failed'
                else:
                    assert all(s < 0 for s in result), 'wrong-peer-not-rejected'
                expected = result
            for peer, status in zip((left, right), expected):
                limit = time.monotonic() + 5
                while (released := peer.rpc('release')) == -4:
                    assert time.monotonic() < limit
                    time.sleep(.01)
                assert released == status
                assert peer.rpc('poll') == peer.rpc('start') == peer.rpc('release') == -3
            cases.append({'case': case, 'passed': True, 'elapsed_ms': round((time.monotonic() - started) * 1000)})
        finally:
            for peer in reversed(peers): peer.close()
    return {'scope': 'isolated-network-stacks' if any(namespaces) else 'two-process-loopback',
            'passed': True, 'cases': cases, 'physical_devices': False, 'wifi_tested': False,
            'identity_material_logged': False}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('library', type=Path)
    parser.add_argument('--left-ip', default='127.0.0.1')
    parser.add_argument('--right-ip', default='127.0.0.1')
    parser.add_argument('--left-namespace')
    parser.add_argument('--right-namespace')
    args = parser.parse_args()
    print(json.dumps(run(args.library, [args.left_ip, args.right_ip],
                         [args.left_namespace, args.right_namespace]), indent=2))
