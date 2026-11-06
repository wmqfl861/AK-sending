/// Wave-28 bridge tests: [FfiSimpleTransferService] (the real service
/// behind agent D's frozen UI contract) driven by [FakeAkTaskCalls] —
/// the reconciliation mappings the adapter owns (role/state/code shapes,
/// consent completion semantics, registry config resolution).
library;

import 'package:ak_ffi_dart/ak_ffi_dart.dart' show FakeAkTaskCalls;
import 'package:flutter_test/flutter_test.dart';

import 'package:ak_sending_app/services/ffi_simple_transfer_service.dart';
import 'package:ak_sending_app/ui/service/simple_transfer_service.dart'
    show JoinResult, TaskConnectionState, TaskRole;

const String regId =
    '1111111111111111111111111111111111111111111111111111111111111111';

Future<void> immediate(Duration d) async {}

FfiSimpleTransferService booted(FakeAkTaskCalls fake) {
  final service = FfiSimpleTransferService(
    calls: fake,
    registry: '127.0.0.1:7200',
    registryIdHex: regId,
    idDir: 'C:/ak/bridge',
    delay: immediate,
  );
  service.start();
  return service;
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test('createCode maps both role vocabularies and returns the code',
      () async {
    final service = booted(FakeAkTaskCalls());
    final code = await service.createCode(TaskRole.receive);
    expect(code.role, TaskRole.receive);
    expect(code.code.startsWith('AKT1-'), isTrue);
    service.dispose();
  });

  test('joinCode success/failure map to the frozen JoinResult', () async {
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    final ok = await service.joinCode((await service.createCode(TaskRole.send)).code);
    expect(ok.ok, isTrue);
    expect(ok.connectionId, isNotNull);

    final failed = await service.joinCode('AKT1-0000-0000-0000');
    expect(failed.ok, isFalse);
    expect(failed.error, isNotEmpty);
    service.dispose();
  });

  test('connections rows use the contract states and filter dropped ones',
      () async {
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    final code = (await service.createCode(TaskRole.send)).code;
    final joined = await service.joinCode(code);
    service.engine.pumpEvents();

    final rows = await service.connections.first;
    expect(rows, hasLength(1));
    expect(rows.first.id, joined.connectionId);
    expect(rows.first.role, TaskRole.receive);
    expect(rows.first.state, TaskConnectionState.connected);
    expect(rows.first.peerName, isNotEmpty);

    await service.disconnect(joined.connectionId!);
    service.engine.pumpEvents();
    final after = await service.connections.first;
    expect(after, isEmpty); // dropped rows leave the snapshot
    service.dispose();
  });

  test('consent accept waits for the receive; decline resolves immediately',
      () async {
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    final code = (await service.createCode(TaskRole.receive)).code;
    final JoinResult joined = await service.joinCode(code);
    service.engine.pumpEvents();

    final job = fake.emitIncomingRequest(
      fake.runtimes.keys.first,
      connId: joined.connectionId,
      rootName: 'photos',
      files: 2,
      bytes: 2048,
    );
    service.engine.pumpEvents();
    await pumpEventQueue();

    // Decline: completes immediately, no done needed.
    final jobTwo = fake.emitIncomingRequest(fake.runtimes.keys.first,
        connId: joined.connectionId, rootName: 'x', files: 1, bytes: 1);
    service.engine.pumpEvents();
    await pumpEventQueue();
    await service.consent(jobTwo, false);

    // Accept: completes when the conn's done event lands.
    final accepted = service.consent(job, true);
    await pumpEventQueue();
    fake.emitDone(fake.runtimes.keys.first, connId: joined.connectionId);
    service.engine.pumpEvents();
    await accepted; // would time out (and fail) without the done event
    service.dispose();
  });

  test('sendFiles summarizes the engine report in Chinese', () async {
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    // A receiver-role code joined by a second runtime on the same fake.
    final receiver = FfiSimpleTransferService(
      calls: fake,
      registry: '127.0.0.1:7200',
      registryIdHex: regId,
      idDir: 'C:/ak/peer',
      delay: immediate,
    );
    receiver.start();
    final code = (await receiver.createCode(TaskRole.receive)).code;
    final joined = await service.joinCode(code);
    final summary =
        await service.sendFiles(joined.connectionId!, const ['C:/a.jpg']);
    expect(summary, contains('已发送 1 个文件'));
    expect(summary, contains('KB'));
    receiver.dispose();
    service.dispose();
  });

  test('start resolves the registry from the environment', () async {
    final service = FfiSimpleTransferService(
      calls: FakeAkTaskCalls(),
      delay: immediate,
      environment: () => const <String, String>{
        'AK_REGISTRY_ADDR': '10.1.2.3:7300',
        'AK_REGISTRY_ID': regId,
        'AK_ID_DIR': 'C:/ak/env-id',
      },
    );
    await service.start();
    expect(service.saveDir, isNotEmpty); // engine default learned
    service.dispose();

    final unconfigured = FfiSimpleTransferService(
      calls: FakeAkTaskCalls(),
      delay: immediate,
      environment: () => const <String, String>{},
    );
    await expectLater(unconfigured.start(), throwsStateError);
    unconfigured.dispose();
  });
}
