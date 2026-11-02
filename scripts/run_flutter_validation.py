#!/usr/bin/env python3
"""Run exact-SDK generated Flutter build checks and preserve evidence of failures."""
from __future__ import annotations
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import zipfile
from prepare_flutter_probe import prepare, SYMBOLS
from run_validation import ROOT, all_passed, configure_console, file_sha256, git_value, run_one


def command(name: str, *args: str) -> list[str]:
    executable = shutil.which(name)
    if not executable:
        return [name, *args]  # run_one records required-tool absence as failure.
    return [executable, *args]


def package_zip(source: Path, output: Path) -> None:
    with zipfile.ZipFile(output, 'w', zipfile.ZIP_DEFLATED) as archive:
        for path in sorted(source.rglob('*')):
            if path.is_file(): archive.write(path, path.relative_to(source.parent))


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
                if not run('native-symbols', ['nm', '-gU', str(binary)]): return 1
                symbols = (output / 'native-symbols.log').read_text(encoding='utf-8')
                if not all('_ak_m0_probe_' + symbol in symbols for symbol in SYMBOLS):
                    raise ValueError('Required native symbols stripped')
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
