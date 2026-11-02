#!/usr/bin/env python3
"""Validate the isolated probe and record actual commands; no whole-M0 verdict."""
from __future__ import annotations
import argparse
import json
import os
from pathlib import Path
import platform
import re
import shutil
import sys
from run_validation import ROOT, all_passed, configure_console, file_sha256, git_value, run_one

MANIFEST = 'experiments/platform_probe/native/Cargo.toml'


def elf_has_16k_alignment(text: str) -> tuple[bool, int]:
    loads = [line.split() for line in text.splitlines() if line.lstrip().startswith('LOAD ')]
    header_ok = bool(re.search(r'Class:\s+ELF64\b', text) and
                     re.search(r'Machine:\s+AArch64\b', text))
    try:
        aligned = header_ok and bool(loads)
        for line in loads:
            offset, address, alignment = int(line[1], 16), int(line[2], 16), int(line[-1], 16)
            aligned = aligned and alignment >= 16384 and alignment & (alignment - 1) == 0
            aligned = aligned and offset % 16384 == address % 16384
        return bool(aligned), len(loads)
    except (ValueError, IndexError):
        return False, len(loads)


def main() -> int:
    configure_console()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--target')
    args = parser.parse_args()
    output = args.output.resolve()
    if output.exists() and any(output.iterdir()):
        parser.error('Use an empty output directory for each attempt')
    target_dir = Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target' / 'native')).resolve()
    os.environ['CARGO_TARGET_DIR'] = str(target_dir)
    options = ['--manifest-path', MANIFEST]
    frozen = ['--locked', '--offline']
    commands = [('rustc', ['rustc', '--version', '--verbose']),
                ('cargo', ['cargo', '--version'])]
    if not args.target:
        commands += [
            ('fmt', ['cargo', 'fmt', *options, '--check']),
            ('test', ['cargo', 'test', *options, *frozen]),
            ('clippy', ['cargo', 'clippy', *options, '--all-targets', *frozen, '--', '-D', 'warnings']),
        ]
    build = ['cargo', 'build', *options, '--release', '--lib', *frozen]
    if args.target:
        build += ['--target', args.target]
    commands.append(('release-build', build))
    release = target_dir / (args.target or '') / 'release'
    target = args.target or platform.system()
    if 'android' in target or target == 'Linux':
        artifact = release / 'libak_m0_probe.so'
    elif 'ios' in target:
        artifact = release / 'libak_m0_probe.a'
    elif target == 'Windows':
        artifact = release / 'ak_m0_probe.dll'
    else:
        artifact = release / 'libak_m0_probe.dylib'
    if not args.target:
        commands.append(('ffi-smoke', [sys.executable, 'scripts/probe_ffi_smoke.py', str(artifact)]))
    elif 'android' in args.target:
        readelf = os.environ.get('AK_READELF', 'llvm-readelf')
        commands.append(('elf', [readelf, '--wide', '--file-header', '--program-headers', str(artifact)]))
    elif 'ios' in args.target:
        commands += [('sdk', ['xcrun', '--sdk', 'iphoneos', '--show-sdk-version']),
                     ('native-link', ['xcrun', '--sdk', 'iphoneos', 'clang',
                                      '-target', 'arm64-apple-ios15.0',
                                      'experiments/platform_probe/link_probe.c', str(artifact),
                                      '-framework', 'Security', '-framework', 'SystemConfiguration',
                                      '-lresolv', '-liconv', '-o', str(output / 'ios-link-probe')]),
                     ('linked-format', ['file', str(output / 'ios-link-probe')])]
    commands += [('feature-graph', ['cargo', 'tree', *options, '--edges', 'features', *frozen]),
                 ('source-clean', ['git', 'diff', '--exit-code', '--',
                                   'experiments', 'Cargo.toml', 'Cargo.lock', 'crates',
                                   'scripts', 'config', '.github'])]
    results = [run_one(name, command, ROOT, output, 900) for name, command in commands]
    if args.target and 'android' in args.target:
        # Fail if any LOAD segment lacks 16 KiB alignment. This is link evidence,
        # not a claim of successful installation on a 16 KiB-page phone.
        text = (output / 'elf.log').read_text(encoding='utf-8')
        aligned, count = elf_has_16k_alignment(text)
        results.append({'name': 'elf-16k-alignment', 'status': 'passed' if aligned else 'failed',
                        'exit_code': 0 if aligned else 1, 'load_segments': count})
    passed = all_passed(results) and artifact.is_file()
    report = {
        'module': 'M0-04', 'level': 'native-link' if args.target else 'host-ffi-loopback',
        'commit': git_value('rev-parse', 'HEAD'), 'target': args.target or platform.machine(),
        'system': platform.platform(), 'passed': passed, 'results': results,
        'artifact': {'name': artifact.name, 'bytes': artifact.stat().st_size,
                     'sha256': file_sha256(artifact)} if artifact.is_file() else None,
        'not_proven': ['physical device', 'offline LAN', 'App signing/install',
                       'directory permission', 'App download size', 'device memory/CPU/startup'],
    }
    if artifact.is_file():
        shutil.copy2(artifact, output / artifact.name)
    shutil.copy2(ROOT / 'experiments/platform_probe/include/ak_m0_probe.h', output)
    (output / 'results.json').write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
    print(json.dumps(report, indent=2))
    return 0 if passed else 1


if __name__ == '__main__':
    raise SystemExit(main())
