import 'package:flutter_test/flutter_test.dart';
import 'package:ak_sending_app/main.dart' as app_main;
import 'package:ak_sending_app/services/ffi_simple_transfer_service.dart';
import 'package:ak_sending_app/services/ak_engine_facade.dart';
import 'package:ak_sending_app/ui/service/simple_transfer_service.dart'
    show TaskRole;
import 'package:ak_sending_app/services/simple_transfer_service.dart'
    show SimpleTransferService;
import 'dart:ffi';
import 'package:ak_ffi_dart/ak_ffi_dart.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  test('repro: FULL GUI bootstrap path createCode (real facade + lease)',
      () async {
    final lib = DynamicLibrary.open(
        r'C:\Users\Administrator\.zcode\workspace\eaf240b9-e924-4888-ae8f-59226602ca0c\AK-sending\target\release\ak_ffi.dll');
    // Mimic bootstrapAppService's real path with injectable seams.
    final owner = AkEngineDartFacade(
        dllPath:
            r'C:\Users\Administrator\.zcode\workspace\eaf240b9-e924-4888-ae8f-59226602ca0c\AK-sending\target\release\ak_ffi.dll');
    addTearDown(() => owner.dispose());
    final service = FfiSimpleTransferService(
      calls: AkTaskFfiBindings(
          engine: owner.engineHandle,
          library: owner.library,
          isAlive: () => owner.isEngineAlive),
      registry: '122.51.121.24:7200',
      registryIdHex:
          '8f012017f57cfef1a3e74e90fa1e13a5f6ffbed3a9ddeecb98f819e002f94f36',
      engineOwner: () {},
    );
    await service.start();
    try {
      final code = await service.createCode(TaskRole.send);
      // ignore: avoid_print
      print('FULL-PATH CREATE OK: ${code.code}');
    } on Object catch (e) {
      // ignore: avoid_print
      print('FULL-PATH CREATE FAILED: $e');
    }
    service.dispose();
  }, timeout: const Timeout(Duration(minutes: 1)));
}
