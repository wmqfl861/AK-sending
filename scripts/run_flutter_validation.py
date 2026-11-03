#!/usr/bin/env python3
"""Run exact-SDK generated Flutter build checks and preserve evidence of failures."""
from __future__ import annotations
import argparse
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import zipfile
from prepare_flutter_probe import prepare, FFI_SYMBOLS
from run_validation import ROOT, all_passed, configure_console, file_sha256, git_value, run_one


def command(name: str, *args: str) -> list[str]:
    executable = shutil.which(name)
    if not executable:
        return [name, *args]  # run_one records required-tool absence as failure.
    return [executable, *args]


def command_payload(log: Path, expected_argv: list[str]) -> str:
    """Remove only run_one's verified metadata line, preserving actual tool output."""
    header, separator, payload = log.read_text(encoding='utf-8').partition('\n')
    if not separator or not header.startswith('command='):
        raise ValueError('Missing command evidence header')
    if json.loads(header[len('command='):]) != expected_argv:
        raise ValueError('Command evidence does not match requested tool')
    return payload


def require_arm64(log: Path, argv: list[str]) -> None:
    # Exact single architecture, never a substring in a command/path or universal ABI.
    if command_payload(log, argv).strip() != 'arm64':
        raise ValueError('Declared arm64 ABI mismatch')


def require_ffi_exports(log: Path, argv: list[str]) -> None:
    exports = set()
    for line in command_payload(log, argv).splitlines():
        fields = line.split()
        if len(fields) == 3 and fields[1] == 'T':
            exports.add(fields[2])
    if not all('_' + symbol in exports for symbol in FFI_SYMBOLS):
        raise ValueError('Required native symbols stripped')


def package_zip(source: Path, output: Path) -> None:
    """Preserve bundle modes and internal symlinks, never follow links outside it."""
    root = source.resolve()
    with zipfile.ZipFile(output, 'w', zipfile.ZIP_DEFLATED) as archive:
        for path in sorted(source.rglob('*')):
            relative = path.relative_to(source.parent).as_posix()
            if path.is_symlink():
                if not path.resolve().is_relative_to(root):
                    raise ValueError('Bundle symlink escapes product root')
                info = zipfile.ZipInfo(relative)
                info.create_system = 3
                info.external_attr = (stat.S_IFLNK | 0o777) << 16
                archive.writestr(info, os.readlink(path).encode('utf-8'))
            elif path.is_file() or path.is_dir():
                archive.write(path, relative)


def main() -> int:
    configure_console()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--platform', required=True, choices=['linux', 'windows', 'macos', 'android', 'ios'])
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    platform = args.platform
    output = args.output.resolve()
    if output.exists() and any(output.iterdir()): parser.error('Use an empty evidence directory')
    output.mkdir(parents=True, exist_ok=True)
    project = ROOT / 'target/flutter-probe'
    if project.exists(): parser.error('Generated project already exists; use a fresh checkout')
    results = []
    def run(name, argv, cwd=ROOT, seconds=900):
        row = run_one(name, argv, cwd, output, seconds)
        results.append(row)
        print(f'{name}: {row["status"]}', flush=True)
        return row['status'] == 'passed'
    passed = False
    artifacts = []
    try:
        if not run('sdk', command('flutter', '--version', '--machine')): return 1
        text = (output / 'sdk.log').read_text(encoding='utf-8')
        version = json.loads(text[text.index('{'):])
        pins = json.loads((ROOT / 'config/toolchains.json').read_text())
        if version['frameworkRevision'] != pins['flutter']['commit'] or version['engineRevision'] != pins['flutter']['engine']:
            raise ValueError('Flutter SDK pin mismatch')
        target = {'android': 'aarch64-linux-android', 'ios': 'aarch64-apple-ios'}.get(platform)
        native_dir = ROOT / 'target/native-app'
        os.environ['CARGO_TARGET_DIR'] = str(native_dir)
        manifest = 'experiments/platform_probe/native/Cargo.toml'
        if not run('native-prefetch', ['cargo', 'fetch', '--manifest-path', manifest, '--locked']): return 1
        build = ['cargo', 'build', '--manifest-path', manifest, '--release', '--lib', '--locked', '--offline']
        if target: build += ['--target', target]
        if not run('native-build', build, seconds=1200): return 1
        release = native_dir / (target or '') / 'release'
        native = release / {'linux':'libak_m0_probe.so', 'windows':'ak_m0_probe.dll',
                            'android':'libak_m0_probe.so', 'ios':'libak_m0_probe.a', 'macos':'libak_m0_probe.a'}[platform]
        if platform in ['ios', 'macos']:
            regression = output / 'directory-regressions'
            if not run('directory-test-compile', ['swiftc', '-warnings-as-errors',
                    'experiments/flutter_probe/platform/DirectoryOperation.swift',
                    'experiments/flutter_probe/platform/tests/main.swift', '-o', str(regression)]): return 1
            if not run('directory-tests', [str(regression)]): return 1
        if not run('create', command('flutter', 'create', '--platforms=' + platform, '--org=dev.ak',
                '--project-name=ak_m0_probe', '--empty', '--no-pub', str(project))): return 1
        prepare(project, native, platform)
        shutil.copy2(project / 'GENERATED_PROVENANCE.json', output)
        if not run('pub-locked', command('flutter', 'pub', 'get', '--enforce-lockfile'), project): return 1
        for name, argv in [
            ('dart-format', command('dart', 'format', '--output=none', '--set-exit-if-changed', 'lib', 'tool', 'test')),
            ('dart-analyze', command('flutter', 'analyze', '--no-pub')),
            ('dart-tests', command('flutter', 'test', '--no-pub')),
        ]:
            if not run(name, argv, project): return 1
        if not target:
            shared = release / {'linux':'libak_m0_probe.so', 'windows':'ak_m0_probe.dll', 'macos':'libak_m0_probe.dylib'}[platform]
            if not run('actual-dart-ffi', command('dart', 'run', 'tool/native_smoke.dart', str(shared)), project): return 1
            if not run('actual-dart-lan-ffi', command('dart', 'run', 'tool/lan_smoke.dart', str(shared)), project): return 1
        build = command('flutter', 'build', 'apk' if platform == 'android' else platform,
                        '--release', '--no-pub')
        if platform == 'android': build += ['--target-platform=android-arm64']
        if platform == 'ios': build += ['--no-codesign']
        if not run('flutter-release', build, project, 1800): return 1
        if platform == 'android':
            apk = project / 'build/app/outputs/flutter-apk/app-release.apk'
            with zipfile.ZipFile(apk) as archive:
                payload = archive.read('lib/arm64-v8a/libak_m0_probe.so')
                import hashlib
                if hashlib.sha256(payload).hexdigest() != file_sha256(native):
                    raise ValueError('APK native payload mismatch')
            shutil.copy2(apk, output / 'probe-debug-key-signed.apk')
        else:
            paths = {'linux':'build/linux/x64/release/bundle', 'windows':'build/windows/x64/runner/Release',
                     'macos':'build/macos/Build/Products/Release/ak_m0_probe.app', 'ios':'build/ios/iphoneos/Runner.app'}
            product = project / paths[platform]
            if not product.is_dir(): raise ValueError('Release product missing')
            if platform in ['ios', 'macos']:
                binary = product / ('Runner' if platform == 'ios' else 'Contents/MacOS/ak_m0_probe')
                architecture_argv = ['lipo', '-archs', str(binary)]
                if not run('native-architecture', architecture_argv): return 1
                require_arm64(output / 'native-architecture.log', architecture_argv)
                symbols_argv = ['nm', '-gU', str(binary)]
                if not run('native-symbols', symbols_argv): return 1
                require_ffi_exports(output / 'native-symbols.log', symbols_argv)
            else:
                embedded = product / ('lib/libak_m0_probe.so' if platform == 'linux' else 'ak_m0_probe.dll')
                if file_sha256(embedded) != file_sha256(native): raise ValueError('Bundled native library mismatch')
            package_zip(product, output / f'{platform}-release-unsigned-or-local.zip')
        if (project / 'pubspec.lock').read_bytes() != (ROOT / 'experiments/flutter_probe/pubspec.lock').read_bytes():
            raise ValueError('Committed Dart lock changed during validation')
        if not run('source-clean', ['git', 'diff', '--exit-code', '--', '.']): return 1
        passed = all_passed(results)
        return 0 if passed else 1
    except Exception as error:
        (output / 'runner-error.txt').write_text(f'{type(error).__name__}: {error}\n', encoding='utf-8')
        return 1
    finally:
        if project.exists():
            # Source/config generated by the pinned SDK; exclude build outputs/cache.
            with zipfile.ZipFile(output / 'generated-project.zip', 'w', zipfile.ZIP_DEFLATED) as archive:
                for path in sorted(project.rglob('*')):
                    relative = path.relative_to(project)
                    if path.is_file() and not any(part in ['build','.dart_tool','.gradle','ephemeral','Pods','.symlinks'] for part in relative.parts):
                        archive.write(path, relative)
        for path in sorted(output.iterdir()):
            if path.is_file() and path.suffix in ['.zip', '.apk']:
                artifacts.append({'name':path.name,'bytes':path.stat().st_size,'sha256':file_sha256(path)})
        report = {'module':'M0-05','commit':git_value('rev-parse','HEAD'),'platform':platform,
                  'passed':passed,'results':results,'artifacts':artifacts,
                  'scope':'Generated release build plus tests; NOT physical-device, offline-LAN, directory semantics or performance acceptance',
                  'signing':'Android uses generated debug test key; iOS unsigned; no production signing credentials'}
        (output / 'results.json').write_text(json.dumps(report,indent=2)+'\n', encoding='utf-8')


if __name__ == '__main__':
    raise SystemExit(main())
