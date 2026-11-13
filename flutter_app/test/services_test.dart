/// Service-layer tests for the DLL three-tier probe (`dll_path.dart`).
///
/// G5/E1 (2026-11-13): the old Dart service layer (wave-11/22 —
/// `ffi_transfer_service` / `real_transfer_service` / `invite_service` /
/// the old `transfer_service` contracts) was deleted after the acceptance
/// decision froze the old model; it had been unreachable from `main.dart`
/// since the v1 simple-transfer stack landed. Its dedicated tests went
/// with it. What remains here is the pure-probe coverage of the one
/// services-layer file the PRODUCTION path still uses: `dll_path.dart`
/// (resolved once per boot by `bootstrapAppService`, C08).
library;

import 'dart:io';

import 'package:flutter_test/flutter_test.dart';

import 'package:ak_sending_app/services/dll_path.dart';

void main() {
  group('resolveAkDllPath 三级探测', () {
    test('tier 1: AK_FFI_DLL wins verbatim (no existence check)', () {
      final resolution = resolveAkDllPath(
        environment: {'AK_FFI_DLL': 'X:/custom/ak_ffi.dll'},
        resolvedExecutable: '/fake/exe/app.exe',
        workingDirectory: '/fake/cwd',
        exists: (path) => throw StateError('must not touch the filesystem'),
      );
      expect(resolution!.path, 'X:/custom/ak_ffi.dll');
      expect(resolution.source, DllSource.environment);
    });

    test('tier 2: exe-directory copy beats the dev path', () {
      // The probe is platform-aware (dll/so/dylib) — first flutter-on-
      // Linux CI run caught these tests hardcoding the Windows name.
      final libName = defaultAkDllFileName();
      final exeName =
          Platform.isWindows ? 'ak_sending_app.exe' : 'ak_sending_app';
      final root = Directory.systemTemp.createTempSync('ak_probe_');
      addTearDown(() => root.deleteSync(recursive: true));
      final exeDir = Directory('${root.path}/bundle')..createSync();
      File('${exeDir.path}/$libName').writeAsBytesSync([1, 2, 3]);
      final resolution = resolveAkDllPath(
        environment: const {},
        resolvedExecutable: '${exeDir.path}/$exeName',
        workingDirectory: root.path,
      );
      expect(resolution!.source, DllSource.executableDirectory);
      expect(resolution.path, endsWith('bundle/$libName'));
    });

    test('tier 3: ../target/release from a flutter_app-like cwd', () {
      final libName = defaultAkDllFileName();
      final toolName = Platform.isWindows ? 'tool.exe' : 'tool';
      final root = Directory.systemTemp.createTempSync('ak_probe_');
      addTearDown(() => root.deleteSync(recursive: true));
      Directory('${root.path}/target/release').createSync(recursive: true);
      File('${root.path}/target/release/$libName').writeAsBytesSync([1]);
      // flutter_app sits ONE level below the repo root with target/.
      final cwd = Directory('${root.path}/flutter_app')..createSync();
      final resolution = resolveAkDllPath(
        environment: const {},
        resolvedExecutable: '${root.path}/elsewhere/$toolName',
        workingDirectory: cwd.path,
      );
      expect(resolution!.source, DllSource.workingDirectory);
      expect(resolution.path, endsWith('../target/release/$libName'));
      expect(File(resolution.path).existsSync(), isTrue);
    });

    test('returns null when no tier resolves', () {
      final root = Directory.systemTemp.createTempSync('ak_probe_');
      addTearDown(() => root.deleteSync(recursive: true));
      expect(
        resolveAkDllPath(
          environment: const {},
          resolvedExecutable: '${root.path}/exe/tool.exe',
          workingDirectory: '${root.path}/cwd',
        ),
        isNull,
      );
    });
  });

  group('Android 适配（dll_path tier-0）', () {
    test('android branch: bundled libak_ffi.so short-circuits every tier', () {
      final resolution = resolveAkDllPath(
        isAndroid: true,
        // Even an explicit AK_FFI_DLL must not win — no env vars on Android.
        environment: const {'AK_FFI_DLL': 'X:/custom/ak_ffi.dll'},
        resolvedExecutable: '/fake/exe/app',
        workingDirectory: '/fake/cwd',
        exists: (path) => throw StateError('must not touch the filesystem'),
      );
      expect(resolution!.path, androidAkLibName);
      expect(resolution.source, DllSource.bundled);
      // Probe diagnostics on Android name only the bundled SONAME.
      expect(akDllProbeCandidates(isAndroid: true), const [androidAkLibName]);
    });

    test('defaultAkDllFileName: android names the packaged cdylib', () {
      expect(defaultAkDllFileName(isAndroid: true), androidAkLibName);
    });
  });
}
