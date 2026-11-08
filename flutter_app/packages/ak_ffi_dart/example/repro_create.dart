import 'dart:ffi';
import 'dart:io';
import 'package:ak_ffi_dart/ak_ffi_dart.dart';

Future<void> main() async {
  final lib = DynamicLibrary.open(
      r'C:\Users\Administrator\.zcode\workspace\eaf240b9-e924-4888-ae8f-59226602ca0c\AK-sending\target\release\ak_ffi.dll');
  final missing = AkTaskFfiBindings.missingTaskExports(lib);
  if (missing.isNotEmpty) { print('missing: $missing'); exit(1); }
  final engine = AkEngineDart();
  final alive = engine;
  final calls = AkTaskFfiBindings(
      engine: alive.nativeHandle, library: alive.bindings.library,
      isAlive: () => true);
  final svc = TaskFfi(calls: calls);
  final idDir = Directory.systemTemp.createTempSync('ak-repro').path;
  print('idDir=$idDir');
  final runtime = svc.start(AkTaskStartSpec(
    registry: '122.51.121.24:7200',
    registryIdHex: '8f012017f57cfef1a3e74e90fa1e13a5f6ffbed3a9ddeecb98f819e002f94f36',
    idDir: idDir,
    saveDir: '$idDir${Platform.pathSeparator}downloads',
  ));
  print('runtime started');
  try {
    final res = await runtime
        .createCode(AkTaskCreateCodeSpec(role: AkTaskRole.sender, candidates: []))
        .wait(timeout: const Duration(seconds: 25));
    print('CREATE OK: $res');
  } on Object catch (e) {
    print('CREATE FAILED: $e');
    print('lastError: ${calls.lastError()}');
  }
  alive.dispose();
}
