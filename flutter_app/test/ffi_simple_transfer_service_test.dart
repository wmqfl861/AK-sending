/// Wave-28 bridge tests: [FfiSimpleTransferService] (the real service
/// behind agent D's frozen UI contract) driven by [FakeAkTaskCalls] —
/// the reconciliation mappings the adapter owns (role/state/code shapes,
/// consent completion semantics, registry config resolution).
library;

import 'dart:async' show Completer, TimeoutException;

import 'package:ak_ffi_dart/ak_ffi_dart.dart' show FakeAkTaskCalls;
import 'package:flutter_test/flutter_test.dart';

import 'package:ak_sending_app/services/ffi_simple_transfer_service.dart';
import 'package:ak_sending_app/ui/service/simple_transfer_service.dart'
    show JoinResult, TaskConnection, TaskConnectionState, TaskRole;

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
    expect(
        RegExp(r'^AKI1-[0-9A-HJKMNP-TV-Z]{4}(-[0-9A-HJKMNP-TV-Z]{4}){7}$')
            .hasMatch(code.code),
        isTrue);
    service.dispose();
  });

  test('joinCode success/failure map to the frozen JoinResult', () async {
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    final ok = await service.joinCode((await service.createCode(TaskRole.send)).code);
    expect(ok.ok, isTrue);
    expect(ok.connectionId, isNotNull);

    final failed = await service.joinCode('AKI1-0000-0000-0000-0000-0000-0000-0000-0000');
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

  // -------------------------------------------------------------------------
  // Wave-29 review fixes on the adapter seam.
  // -------------------------------------------------------------------------

  test('joinCode forwards the explicit user role (P1-05 seam)', () async {
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    // The creator declared RECEIVER; the joiner EXPLICITLY declares
    // receiver too (their intent) — the hint must reach the spec.
    final code = (await service.createCode(TaskRole.receive)).code;
    final ok = await service.joinCode(code, role: TaskRole.receive);
    expect(ok.ok, isTrue);
    expect(fake.acceptedSpecs.last, contains('"role":"receiver"'));
    service.dispose();
  });

  test('P1-06: a LATE connections subscriber (detail page) still sees the '
      'live list while an earlier one stays attached', () async {
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    final code = (await service.createCode(TaskRole.send)).code;
    final joined = await service.joinCode(code);
    service.engine.pumpEvents();

    final home = <List<TaskConnection>>[];
    final homeSub = service.connections.listen(home.add);
    await pumpEventQueue();
    expect(home.first, hasLength(1));

    final late = <List<TaskConnection>>[];
    final lateSub = service.connections.listen(late.add);
    await pumpEventQueue();
    expect(late, isNotEmpty, reason: '晚订阅者必须先拿到当前快照');
    expect(late.first.first.id, joined.connectionId);
    expect(late.first.first.state, TaskConnectionState.connected);

    await homeSub.cancel();
    await lateSub.cancel();
    service.dispose();
  });

  test('P2-11: declining an expired request resolves normally '
      '(idempotent), accepting one fails readably', () async {
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    final code = (await service.createCode(TaskRole.receive)).code;
    final joined = await service.joinCode(code);
    service.engine.pumpEvents();
    final runtimeId = fake.runtimes.keys.first;
    final job = fake.emitIncomingRequest(runtimeId,
        connId: joined.connectionId, rootName: 'x', files: 1, bytes: 1);
    service.engine.pumpEvents();
    await pumpEventQueue();
    // The engine auto-declined it (30 s window): the pending entry is
    // gone — the fake models that state change.
    fake.runtimes[runtimeId]!.pendingConsents.remove(job);

    await expectLater(service.consent(job, false), completes,
        reason: '过期后的拒绝必须幂等收尾（P2-11）');
    await expectLater(
      service.consent(job, true),
      throwsA(isA<StateError>()),
    );
    service.dispose();
  });

  test('P2-10: a receive that outlives receiveTimeout cancels the native '
      'task through the disconnect path', () async {
    final fake = FakeAkTaskCalls();
    final service = FfiSimpleTransferService(
      calls: fake,
      registry: '127.0.0.1:7200',
      registryIdHex: regId,
      idDir: 'C:/ak/bridge',
      delay: immediate,
      receiveTimeout: Duration.zero,
    );
    await service.start();
    final code = (await service.createCode(TaskRole.receive)).code;
    final joined = await service.joinCode(code);
    service.engine.pumpEvents();
    final runtimeId = fake.runtimes.keys.first;
    final job = fake.emitIncomingRequest(runtimeId,
        connId: joined.connectionId, rootName: 'x', files: 1, bytes: 1);
    service.engine.pumpEvents();
    await pumpEventQueue();

    await expectLater(
      service.consent(job, true),
      throwsA(isA<TimeoutException>().having(
          (e) => e.message, 'message', contains('已取消原生接收任务'))),
    );
    expect(
      fake.runtimes[runtimeId]!.connections[joined.connectionId]!.state,
      'disconnected',
      reason: '接收超时后必须走 FFI 断开取消原生任务',
    );
    service.dispose();
  });

  test('P2-10: a mid-receive disconnect rejects the consent wait with a '
      'readable reason instead of hanging to the timeout', () async {
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    final code = (await service.createCode(TaskRole.receive)).code;
    final joined = await service.joinCode(code);
    service.engine.pumpEvents();
    final runtimeId = fake.runtimes.keys.first;
    final job = fake.emitIncomingRequest(runtimeId,
        connId: joined.connectionId, rootName: 'x', files: 1, bytes: 1);
    service.engine.pumpEvents();
    await pumpEventQueue();

    final accepted = service.consent(job, true);
    await pumpEventQueue();
    // The data plane dies mid-receive.
    fake.runtimes[runtimeId]!.events.add(<String, dynamic>{
      'event': 'disconnected',
      'conn': joined.connectionId,
      'reason': 'peer',
    });
    service.engine.pumpEvents();
    await expectLater(
      accepted,
      throwsA(isA<StateError>()
          .having((e) => e.message, 'message', contains('连接已断开'))),
    );
    service.dispose();
  });

  test('P1-01: Android start gates on the path_provider identity resolve '
      'and anchors the engine id_dir on it', () async {
    final fake = FakeAkTaskCalls();
    final gate = Completer<String>();
    final service = FfiSimpleTransferService(
      calls: fake,
      registry: '127.0.0.1:7200',
      registryIdHex: regId,
      delay: immediate,
      isAndroid: true,
      androidIdentityDir: () => gate.future,
    );
    final starting = service.start();
    await Future<void>.delayed(const Duration(milliseconds: 20));
    expect(fake.runtimes, isEmpty, reason: 'identityDirReady 前不得启动引擎');

    gate.complete('/data/user/0/dev.ak/files/.ak-sending');
    await starting;
    expect(fake.runtimes, hasLength(1));
    expect(fake.acceptedSpecs.first,
        contains('/data/user/0/dev.ak/files/.ak-sending'));
    await service.identityDirReady; // never throws
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
