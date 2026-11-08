import 'dart:ffi';
import 'package:flutter_test/flutter_test.dart';
import 'package:ak_sending_app/services/ffi_simple_transfer_service.dart';
import 'package:ak_sending_app/services/simple_transfer_service.dart';
import 'package:ak_sending_app/ui/service/simple_transfer_service.dart' show TaskRole;
import 'package:ak_ffi_dart/ak_ffi_dart.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  test('repro: GUI service layer createCode against the real registry',
      () async {
    final lib = DynamicLibrary.open(
        r'C:\Users\Administrator\.zcode\workspace\eaf240b9-e924-4888-ae8f-59226602ca0c\AK-sending\target\release\ak_ffi.dll');
    expect(AkTaskFfiBindings.missingTaskExports(lib), isEmpty);
    final engine = AkEngineDart(bindings: AkFfiBindings(library: lib));
    addTearDown(() => engine.dispose());
    final calls = AkTaskFfiBindings(
        engine: engine.nativeHandle,
        library: engine.bindings.library,
        isAlive: () => true);
    final service = FfiSimpleTransferService(
      calls: calls,
      registry: '122.51.121.24:7200',
      registryIdHex:
          '8f012017f57cfef1a3e74e90fa1e13a5f6ffbed3a9ddeecb98f819e002f94f36',
      idDir:
          r'C:\Users\Administrator\.ak-sending',
      saveDir:
          r'C:\Users\Administrator\AppData\Local\Temp\ak-gui-repro-dl',
      engineOwner: () {},
    );
    await service.start();
    try {
      final created = await service.createCode(TaskRole.send);
      // ignore: avoid_print
      print('CREATE OK: ${created.code}');
    } on Object catch (e) {
      // ignore: avoid_print
      print('CREATE FAILED: $e');
    }
    service.dispose();
  }, timeout: const Timeout(Duration(minutes: 1)));
}
