/// Wave-28 bridge tests: [FfiSimpleTransferService] (the real service
/// behind agent D's frozen UI contract) driven by [FakeAkTaskCalls] —
/// the reconciliation mappings the adapter owns (role/state/code shapes,
/// consent completion semantics, registry config resolution).
library;

import 'dart:async' show Completer, Timer, TimeoutException;
import 'dart:typed_data' show Uint8List;

import 'package:ak_ffi_dart/ak_ffi_dart.dart' show AkTaskCalls, FakeAkTaskCalls;
import 'package:flutter_test/flutter_test.dart';

import 'package:ak_sending_app/services/ffi_simple_transfer_service.dart';
import 'package:ak_sending_app/ui/service/simple_transfer_service.dart'
    show
        EngineHealth,
        IncomingRequest,
        JoinResult,
        TaskConnection,
        TaskConnectionState,
        TaskRole;

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

  test('P2-04: createCode hands up the engine code_id (the cancel key)',
      () async {
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    final code = await service.createCode(TaskRole.receive);
    expect(code.codeId, isNotNull,
        reason: 'TaskCode 必须携带引擎 code_id（P2-04 撤码入口）');
    expect(fake.disconnectCalls, isEmpty); // minting alone never revokes
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

  test('R36-P2-03: engineHealth forwards ok → engineFailed with the '
      'visible reason (replayable per listen)', () async {
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    final health = <EngineHealth>[];
    final sub = service.engineHealth.listen(health.add);
    await pumpEventQueue();
    expect(health, hasLength(1));
    expect(health.single.isEngineFailed, isFalse);

    for (var i = 0; i < 10; i++) {
      fake.failPollEventCalls = 1;
      service.engine.pumpEvents();
    }
    await pumpEventQueue();
    expect(health.last.isEngineFailed, isTrue,
        reason: '真实 adapter 必须转发引擎闩——故障要达 UI，不能只留内部字段');
    expect(health.last.failureReason, contains('传输引擎故障'));

    // A second listener on the SAME adapter gets the replayed latch.
    final late = <EngineHealth>[];
    final lateSub = service.engineHealth.listen(late.add);
    await pumpEventQueue();
    expect(late.single.isEngineFailed, isTrue);
    await sub.cancel();
    await lateSub.cancel();
    service.dispose();
  });

  test('R36-P2-03: an accepted receive in flight ends PROMPTLY with the '
      'engine failure when the latch trips (never waits out its stall '
      'budget)', () async {
    final fake = FakeAkTaskCalls();
    final service = FfiSimpleTransferService(
      calls: fake,
      registry: '127.0.0.1:7200',
      registryIdHex: regId,
      idDir: 'C:/ak/bridge',
      delay: immediate,
      // A huge stall budget proves the LATCH (not the timeout) ends the
      // wait.
      receiveTimeout: const Duration(days: 1),
    );
    await service.start();
    final code = (await service.createCode(TaskRole.receive)).code;
    final joined = await service.joinCode(code);
    service.engine.pumpEvents();
    final job = fake.emitIncomingRequest(fake.runtimes.keys.first,
        connId: joined.connectionId, rootName: 'x', files: 1, bytes: 1);
    service.engine.pumpEvents();
    await pumpEventQueue();

    final accepted = service.consent(job, true);
    await pumpEventQueue();
    // The engine latches while the receive is still waiting.
    for (var i = 0; i < 10; i++) {
      fake.failPollEventCalls = 1;
      service.engine.pumpEvents();
    }
    await expectLater(
      accepted,
      throwsA(isA<StateError>()
          .having((e) => e.message, 'message', contains('传输引擎故障'))),
    );
    service.dispose();
  });

  test('P2-05: a done transfer converges the row — the live snapshot no '
      'longer offers a send entry', () async {
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    final code = (await service.createCode(TaskRole.send)).code;
    final joined = await service.joinCode(code);
    service.engine.pumpEvents();
    final before = await service.connections.first;
    expect(before, hasLength(1));

    fake.emitDone(fake.runtimes.keys.first, connId: joined.connectionId);
    service.engine.pumpEvents();
    final after = await service.connections.first;
    expect(after, isEmpty,
        reason: 'done 后行必须收敛离开活动列表（不再给发送入口）');
    service.dispose();
  });

  test('P2-06: the incoming request carries its offer-time saveDir through '
      'the adapter', () async {
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    final code = (await service.createCode(TaskRole.receive)).code;
    final joined = await service.joinCode(code);
    service.engine.pumpEvents();
    await service.setSaveDir('D:/changed-later');

    final requests = <IncomingRequest>[];
    final sub = service.incomingRequests.listen(requests.add);
    fake.emitIncomingRequest(fake.runtimes.keys.first,
        connId: joined.connectionId, files: 1, bytes: 1,
        saveDir: 'D:/offer-time');
    service.engine.pumpEvents();
    await pumpEventQueue();
    expect(requests, hasLength(1));
    expect(requests.first.saveDir, 'D:/offer-time',
        reason: '横幅显示的目标必须跟随事件字段（P2-06）');
    await sub.cancel();
    service.dispose();
  });

  test('P2-10: an accepted receive with CONTINUOUS progress outlives '
      'receiveTimeout; a stalled one is cut', () async {
    final fake = FakeAkTaskCalls();
    // REAL delay (no immediate stand-in): the stall loop must yield so
    // the progress timer feeds the queue while the wait is in flight.
    final service = FfiSimpleTransferService(
      calls: fake,
      registry: '127.0.0.1:7200',
      registryIdHex: regId,
      idDir: 'C:/ak/bridge',
      receiveTimeout: const Duration(milliseconds: 200),
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

    final accepted = service.consent(job, true);
    // Wave-31 contract (R30-P2-02): receive progress rows key `job` by
    // the CONNECTION id — the one correlation key both sides hold.
    final progress = Timer.periodic(const Duration(milliseconds: 25),
        (_) => fake.emitProgress(joined.connectionId!));
    await Future<void>.delayed(const Duration(milliseconds: 600));
    fake.emitDone(runtimeId, connId: joined.connectionId); // finally done
    service.engine.pumpEvents();
    await accepted; // 600 ms > the 200 ms budget — renewal kept it alive
    progress.cancel();
    service.dispose();
  });

  test('P2-10: a stalled receive (no progress) hits the stall budget and '
      'a FAILED cancel is reported honestly (never 已取消)', () async {
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

    fake.nextDisconnectFails = 'engine busy';
    await expectLater(
      service.consent(job, true),
      throwsA(isA<TimeoutException>().having((e) => e.message, 'message',
          allOf(contains('取消原生任务失败'), contains('engine busy')))),
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

  // -------------------------------------------------------------------------
  // Wave-31 review fixes (R30): the receive wait may not outlive dispose
  // (R30-P1-01), conn-keyed receive renewal for the JOINER direction
  // (R30-P2-02), and the terminal-first banner race (R30-P2-03).
  // -------------------------------------------------------------------------

  test('R30-P1-01: disposing mid-receive ends the wait and NO native call '
      'follows (the fake call count freezes)', () async {
    final fake = FakeAkTaskCalls();
    final counting = CountingTaskCalls(fake);
    final service = FfiSimpleTransferService(
      calls: counting,
      registry: '127.0.0.1:7200',
      registryIdHex: regId,
      idDir: 'C:/ak/bridge',
      delay: immediate,
      receiveTimeout: const Duration(seconds: 30),
      engineOwner: () {}, // the native free follows the borrowers' disposal
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

    // Accept → the receive is in flight (unsettled) → dispose the whole
    // service while the wait is parked on its real 50 ms tick.
    final accepted = service.consent(job, true);
    await Future<void>.delayed(const Duration(milliseconds: 120));
    expect(counting.snapshot()['pollProgress'] ?? 0, greaterThan(0),
        reason: '前置：接收等待确实在轮询进度（悬空指针路径）');
    service.dispose(); // engine channels die FIRST, then the owner frees
    final frozen = counting.snapshot();
    final outcome = expectLater(
        accepted, throwsStateError); // the wait ends, it never hangs
    await Future<void>.delayed(const Duration(milliseconds: 250));
    expect(counting.snapshot(), frozen,
        reason: 'dispose 后不得再有任何 native 调用（R30-P1-01）');
    await outcome;
  });

  test('R30-P2-02: a JOINER receiver (the sender minted the code on '
      'another engine) renews on CONN-keyed progress', () async {
    final fake = FakeAkTaskCalls();
    // The SENDER device mints the code — a second runtime on the shared
    // fake stands in for the other engine.
    final senderService = FfiSimpleTransferService(
      calls: fake,
      registry: '127.0.0.1:7200',
      registryIdHex: regId,
      idDir: 'C:/ak/w31-sender',
      delay: immediate,
    );
    senderService.start();
    final code = await senderService.createCode(TaskRole.send);
    // CR-P2-05: the sender's role in this test ENDS at mint time — its
    // still-running adaptive pump would keep draining the fake's
    // ENGINE-WIDE progress queue and phase-lock the joiner's receive
    // watchdog out of its renewal lines (a test-only artifact of two
    // services sharing one fake; production is one service per engine).
    senderService.dispose();

    // The device under test JOINS that code as the RECEIVER: this side
    // never minted the code, so no local code-id mapping exists — the
    // conn id is the only correlation key it holds.
    final service = FfiSimpleTransferService(
      calls: fake,
      registry: '127.0.0.1:7200',
      registryIdHex: regId,
      idDir: 'C:/ak/w31-joiner',
      delay: immediate,
      receiveTimeout: const Duration(milliseconds: 200),
    );
    await service.start();
    final runtimeId = fake.runtimes.keys.last; // the joiner's runtime
    final joined = await service.joinCode(code.code, role: TaskRole.receive);
    service.engine.pumpEvents();

    final job = fake.emitIncomingRequest(runtimeId,
        connId: joined.connectionId, rootName: 'x', files: 1, bytes: 1);
    service.engine.pumpEvents();
    await pumpEventQueue();

    final accepted = service.consent(job, true);
    // The wave-31 contract: receive progress rows are keyed by the CONN id
    // (the Rust side lands the same key in parallel).
    final progress = Timer.periodic(const Duration(milliseconds: 25),
        (_) => fake.emitProgress(joined.connectionId!));
    await Future<void>.delayed(const Duration(milliseconds: 600));
    fake.emitDone(runtimeId, connId: joined.connectionId); // finally done
    service.engine.pumpEvents();
    await accepted; // 600 ms > the 200 ms budget — renewal kept it alive
    progress.cancel();
    senderService.dispose();
    service.dispose();
  });

  test('R30-P2-03: terminal-first (disconnected → request_closed → LATE '
      'incoming) never re-opens a banner', () async {
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    final code = (await service.createCode(TaskRole.receive)).code;
    final joined = await service.joinCode(code);
    service.engine.pumpEvents();
    final runtimeId = fake.runtimes.keys.first;
    final conn = joined.connectionId!;

    final requests = <IncomingRequest>[];
    final expired = <String>[];
    final reqSub = service.incomingRequests.listen(requests.add);
    final expSub = service.expiredIncomingRequests.listen(expired.add);

    final job = 'ab' * 16;
    fake.runtimes[runtimeId]!.events.add(<String, dynamic>{
      'event': 'disconnected',
      'conn': conn,
      'reason': 'peer',
    });
    fake.runtimes[runtimeId]!.events.add(<String, dynamic>{
      'event': 'request_closed',
      'job': job,
      'conn': conn,
      'reason': 'disconnected',
    });
    fake.runtimes[runtimeId]!.events.add(<String, dynamic>{
      'event': 'incoming_request',
      'job': job,
      'conn': conn,
      'root': 'x',
      'files': 1,
      'bytes': 1,
      'saveDir': 'D:/x',
    });
    service.engine.pumpEvents();
    await pumpEventQueue();

    expect(requests, isEmpty,
        reason: '迟到 incoming 不得展示待同意横幅（R30-P2-03）');
    expect(expired, [job], reason: 'request_closed 必须关闭对应请求横幅');
    await reqSub.cancel();
    await expSub.cancel();
    service.dispose();
  });
}

/// Counts every seam call that crosses to the wrapped fake (R30-P1-01:
/// the "zero native calls after dispose" assertion surface).
class CountingTaskCalls implements AkTaskCalls {
  CountingTaskCalls(this._inner);

  final AkTaskCalls _inner;
  final Map<String, int> calls = <String, int>{};

  void _tick(String name) => calls[name] = (calls[name] ?? 0) + 1;

  /// A comparable copy of the counters.
  Map<String, int> snapshot() => Map<String, int>.of(calls);

  @override
  String? lastError() => _inner.lastError();

  @override
  int start(String specJson, Uint8List runtimeIdOut) {
    _tick('start');
    return _inner.start(specJson, runtimeIdOut);
  }

  @override
  int createCode(Uint8List runtimeId, String specJson, Uint8List opIdOut) {
    _tick('createCode');
    return _inner.createCode(runtimeId, specJson, opIdOut);
  }

  @override
  int join(Uint8List runtimeId, String specJson, Uint8List opIdOut) {
    _tick('join');
    return _inner.join(runtimeId, specJson, opIdOut);
  }

  @override
  int send(
      Uint8List runtimeId, Uint8List connId, String pathsJson, Uint8List opIdOut) {
    _tick('send');
    return _inner.send(runtimeId, connId, pathsJson, opIdOut);
  }

  @override
  int list(Uint8List runtimeId, Uint8List buffer, int cap) {
    _tick('list');
    return _inner.list(runtimeId, buffer, cap);
  }

  @override
  int disconnect(Uint8List runtimeId, Uint8List connId) {
    _tick('disconnect');
    return _inner.disconnect(runtimeId, connId);
  }

  @override
  int setSaveDir(Uint8List runtimeId, String path) {
    _tick('setSaveDir');
    return _inner.setSaveDir(runtimeId, path);
  }

  @override
  int consent(Uint8List runtimeId, Uint8List jobId, int accept) {
    _tick('consent');
    return _inner.consent(runtimeId, jobId, accept);
  }

  @override
  int opResult(
      Uint8List runtimeId, Uint8List opId, Uint8List buffer, int cap) {
    _tick('opResult');
    return _inner.opResult(runtimeId, opId, buffer, cap);
  }

  @override
  int pollEvent(Uint8List runtimeId, Uint8List buffer, int cap) {
    _tick('pollEvent');
    return _inner.pollEvent(runtimeId, buffer, cap);
  }

  @override
  int pollProgress(Uint8List buffer, int cap) {
    _tick('pollProgress');
    return _inner.pollProgress(buffer, cap);
  }
}
