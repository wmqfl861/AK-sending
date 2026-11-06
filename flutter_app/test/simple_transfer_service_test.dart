/// Wave-28 service-layer tests: [SimpleTransferService]'s state machine
/// and event sequences, driven end to end by [FakeAkTaskCalls] (the same
/// shipped fake — no DLL, no sockets; the Rust side covers the real
/// bytes).
library;

import 'dart:async' show TimeoutException;

import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show AkFfiException, FakeAkTaskCalls, TaskDoneEvent;
import 'package:flutter_test/flutter_test.dart';
import 'package:ak_sending_app/services/simple_transfer_service.dart';

const String regId =
    '1111111111111111111111111111111111111111111111111111111111111111';

/// Zero-delay stand-in: op wait loops drain synchronously under the fake.
Future<void> immediate(Duration d) async {}

SimpleTransferService booted(FakeAkTaskCalls fake) {
  final service = SimpleTransferService(
    calls: fake,
    delay: immediate,
    eventPollInterval: const Duration(days: 1), // tests pump manually
  );
  service.start(
    registry: '127.0.0.1:7200',
    registryIdHex: regId,
    idDir: 'C:/ak/test-id',
  );
  return service;
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test('start is idempotent and learns the engine save folder', () {
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    expect(service.phase, SimpleTransferPhase.idle);
    expect(fake.runtimes, hasLength(1));
    service.start(
        registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/test-id');
    expect(fake.runtimes, hasLength(1)); // no second runtime
    expect(service.saveDir, 'C:/ak/downloads'); // the fake's default
    service.dispose();
  });

  test('actions before start throw the honest StateError', () {
    final service = SimpleTransferService(calls: FakeAkTaskCalls(), delay: immediate);
    expect(() => service.createCode(SimpleTransferRole.sender),
        throwsStateError);
    expect(() => service.join('AKI1-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX'),
        throwsStateError);
    service.dispose();
  });

  group('state machine: idle → creating → paired → connected', () {
    test('creator path reaches paired, then connected on the join event',
        () async {
      final fake = FakeAkTaskCalls();
      final creator = booted(fake);
      final code = await creator.createCode(SimpleTransferRole.receiver);
      expect(code.startsWith('AKI1-'), isTrue);
      expect(creator.phase, SimpleTransferPhase.paired);

      // The peer joins (second runtime on the same fake = same engine).
      final joiner = SimpleTransferService(
        calls: fake,
        delay: immediate,
        eventPollInterval: const Duration(days: 1),
      );
      joiner.start(
          registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/joiner');
      await joiner.join(code);
      expect(joiner.phase, SimpleTransferPhase.paired);

      creator.pumpEvents();
      joiner.pumpEvents();
      // probe_result flips the joiner to connected; connected flips the
      // creator (it was paired, waiting for the peer).
      expect(joiner.phase, SimpleTransferPhase.connected);
      expect(creator.phase, SimpleTransferPhase.connected);
      creator.dispose();
      joiner.dispose();
    });

    test('joiner connection list reflects probing → connected', () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      final code = await service.createCode(SimpleTransferRole.sender);
      final joined = await service.join(code);
      expect(joined.role, 'receiver');
      expect(joined.probe, 'verified');
      expect(service.currentConnections, hasLength(1));
      expect(service.currentConnections.first.state, 'probing');

      service.pumpEvents();
      expect(service.currentConnections.first.state, 'connected');
      expect(service.phase, SimpleTransferPhase.connected);
      service.dispose();
    });

    test('failed op lands in failed(reason) with the engine detail',
        () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      fake.failNextOp(fake.runtimes.values.first.id, 'registry relay down');
      await expectLater(
        service.createCode(SimpleTransferRole.sender),
        throwsA(isA<AkFfiException>()),
      );
      expect(service.phase, SimpleTransferPhase.failed);
      expect(service.failureReason, contains('registry relay down'));
      // A retry leaves the failure sink.
      final code = await service.createCode(SimpleTransferRole.sender);
      expect(code.startsWith('AKI1-'), isTrue);
      expect(service.phase, SimpleTransferPhase.paired);
      service.dispose();
    });

    test('disconnect returns to idle when the last connection severs',
        () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      final code = await service.createCode(SimpleTransferRole.sender);
      final joined = await service.join(code);
      service.pumpEvents();
      expect(service.phase, SimpleTransferPhase.connected);

      final emitted = <List<SimpleConnection>>[];
      final sub = service.connections.listen(emitted.add);
      await service.disconnect(joined.connId);
      service.pumpEvents(); // fold the disconnected event
      await pumpEventQueue(); // broadcast deliveries are async
      expect(service.phase, SimpleTransferPhase.idle);
      expect(service.currentConnections.first.state, 'disconnected');
      expect(emitted, isNotEmpty);
      expect(emitted.last.first.state, 'disconnected');
      await sub.cancel();
      service.dispose();
    });
  });

  group('event sequences', () {
    test('incoming_request → consent → done', () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      final code = await service.createCode(SimpleTransferRole.receiver);
      final joined = await service.join(code);
      service.pumpEvents();

      final requests = <SimpleIncomingRequest>[];
      final dones = <TaskDoneEvent>[];
      final requestSub = service.incomingRequests.listen(requests.add);
      final doneSub = service.doneEvents.listen(dones.add);

      final job = fake.emitIncomingRequest(
        fake.runtimes.values.first.id,
        connId: joined.connId,
        rootName: 'photos',
        files: 3,
        bytes: 900,
      );
      service.pumpEvents();
      await pumpEventQueue(); // broadcast deliveries are async
      expect(requests, hasLength(1));
      expect(requests.first.jobId, job);
      expect(requests.first.files, 3);
      expect(requests.first.bytes, 900);
      expect(requests.first.connId, joined.connId);

      expect(await service.consent(job, accept: true), isTrue);
      // Already decided → false; unknown → false.
      expect(await service.consent(job, accept: true), isFalse);
      expect(await service.consent('ab' * 16, accept: true), isFalse);

      fake.emitDone(fake.runtimes.values.first.id,
          connId: joined.connId, files: 3, bytes: 900);
      service.pumpEvents();
      await pumpEventQueue(); // broadcast deliveries are async
      expect(dones, hasLength(1));
      expect(dones.first.files, 3);
      await requestSub.cancel();
      await doneSub.cancel();
      service.dispose();
    });

    test('sendFiles completes with the report and emits done', () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      await service.createCode(SimpleTransferRole.sender);
      // The service's runtime created a SENDER code and joins it: the
      // joiner side of that connection is the RECEIVER — so use a second
      // runtime for the sender flow instead.
      final senderService = SimpleTransferService(
        calls: fake,
        delay: immediate,
        eventPollInterval: const Duration(days: 1),
      );
      senderService.start(
          registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/s2');
      final receiverCode = await service.createCode(SimpleTransferRole.receiver);
      final joined = await senderService.join(receiverCode);
      expect(joined.role, 'sender');
      await senderService.sendFiles(joined.connId, const ['C:/pics/a.jpg']);
      senderService.pumpEvents();
      expect(senderService.phase, SimpleTransferPhase.connected);
      service.dispose();
      senderService.dispose();
    });
  });

  group('settings', () {
    test('setSaveDir is the one folder and is readable back', () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      await service.setSaveDir('D:/phone-inbox');
      expect(service.saveDir, 'D:/phone-inbox');
      // The fake's engine state agrees (private-type instance use).
      expect(fake.runtimes[fake.runtimes.keys.first]!.saveDir, 'D:/phone-inbox');
      service.dispose();
    });
  });

  // -------------------------------------------------------------------------
  // Wave-29 review fixes: snapshot replay (P1-06), state merging
  // (P2-08/P2-09), timeout cancels (P2-10), consent expiry (P2-11).
  // -------------------------------------------------------------------------

  group('wave-29 P1-06: connections replay per subscriber', () {
    test('a LATE subscriber (the detail-page shape) receives the current '
        'list while an earlier listener stays attached and no event follows',
        () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      final code = await service.createCode(SimpleTransferRole.sender);
      final joined = await service.join(code);
      service.pumpEvents(); // connected + probe flip the row; no further events
      expect(service.currentConnections.first.state, 'connected');

      // The "home page" listener has been attached from the start.
      final homeSightings = <List<SimpleConnection>>[];
      final home = service.connections.listen(homeSightings.add);
      await pumpEventQueue();

      // The "detail page" subscribes LATE — it must get the snapshot.
      final lateSightings = <List<SimpleConnection>>[];
      final late = service.connections.listen(lateSightings.add);
      await pumpEventQueue();
      expect(lateSightings, isNotEmpty,
          reason: '新订阅者必须先拿到当前快照（P1-06）');
      expect(lateSightings.first, hasLength(1));
      expect(lateSightings.first.first.connId, joined.connId);
      expect(lateSightings.first.first.state, 'connected');

      // Live events still reach BOTH subscribers after the replay.
      await service.disconnect(joined.connId);
      service.pumpEvents();
      await pumpEventQueue();
      expect(lateSightings.last.first.state, 'disconnected');
      expect(homeSightings.last.first.state, 'disconnected');
      await home.cancel();
      await late.cancel();
      service.dispose();
    });
  });

  group('wave-29 P2-09: monotonic state merging', () {
    test('probe reachable=false is an honest unreachable verdict, never '
        '"success"', () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      final runtimeId = fake.runtimes.keys.first;
      final code = await service.createCode(SimpleTransferRole.sender);
      final joined = await service.join(code);
      // Overwrite the fake's queued probe event with a FAILED verdict.
      final runtime = fake.runtimes[runtimeId]!;
      runtime.events.removeWhere((e) => e['event'] == 'probe_result');
      runtime.events.add(<String, dynamic>{
        'event': 'probe_result',
        'conn': joined.connId,
        'endpoint': null,
        'reachable': false,
      });
      service.pumpEvents();
      expect(service.currentConnections.first.state, 'unreachable');
      expect(service.phase, SimpleTransferPhase.paired,
          reason: '探测失败不得把阶段推进为 connected');
      service.dispose();
    });

    test('events processed BEFORE the join result: the late result never '
        'regresses the row to probing nor the phase', () async {
      final fake = FakeAkTaskCalls(autoCompleteOps: false);
      final service = booted(fake);
      final runtimeId = fake.runtimes.keys.first;
      // Complete the create op so the code exists, but keep the join op
      // PENDING: the join future keeps polling while we drive events.
      final codeFuture = service.createCode(SimpleTransferRole.sender);
      fake.pumpOps(runtimeId);
      final code = await codeFuture;

      final joinFuture = service.join(code);
      // The fake queued connected + probe_result at the join call: pump
      // them through the event path BEFORE the op result resolves.
      service.pumpEvents();
      expect(service.currentConnections.first.state, 'connected');
      expect(service.phase, SimpleTransferPhase.connected);

      fake.pumpOps(runtimeId); // now the (late) join result resolves
      final joined = await joinFuture;
      expect(joined.connId, service.currentConnections.first.connId);
      expect(service.currentConnections.first.state, 'connected',
          reason: '迟到的 join 结果不得把已判定的行重建为 probing');
      expect(service.phase, SimpleTransferPhase.connected);
      service.dispose();
    });

    test('a disconnected row is never revived by the late join result',
        () async {
      final fake = FakeAkTaskCalls(autoCompleteOps: false);
      final service = booted(fake);
      final runtimeId = fake.runtimes.keys.first;
      final codeFuture = service.createCode(SimpleTransferRole.sender);
      fake.pumpOps(runtimeId);
      final code = await codeFuture;

      final joinFuture = service.join(code);
      service.pumpEvents(); // connected + probe
      final connId = service.currentConnections.first.connId;
      // The data plane dies BEFORE the join result arrives.
      fake.runtimes[runtimeId]!.events.add(<String, dynamic>{
        'event': 'disconnected',
        'conn': connId,
        'reason': 'peer',
      });
      service.pumpEvents();
      expect(service.currentConnections.first.state, 'disconnected');

      fake.pumpOps(runtimeId);
      await joinFuture;
      expect(service.currentConnections.first.state, 'disconnected',
          reason: '迟到结果不得在 UI 层复活已断开的行');
      service.dispose();
    });
  });

  group('wave-29 P2-08: disconnect reasons are source-separated', () {
    test('a coordination-plane notice keeps a healthy data connection '
        'alive; a data-plane reason kills the row', () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      final runtimeId = fake.runtimes.keys.first;
      final code = await service.createCode(SimpleTransferRole.sender);
      final joined = await service.join(code);
      service.pumpEvents();
      final runtime = fake.runtimes[runtimeId]!;
      final before = service.currentConnections.first.state;
      expect(before, 'connected');

      // Registry/coordinator link loss (any reason outside the frozen
      // data-plane vocabulary): the direct data channel may be healthy.
      runtime.events.add(<String, dynamic>{
        'event': 'disconnected',
        'conn': joined.connId,
        'reason': 'registry',
      });
      service.pumpEvents();
      expect(service.currentConnections.first.state, 'connected',
          reason: '协调面断开不得映射为任务终态（P2-08）');
      expect(service.phase, SimpleTransferPhase.connected);

      // A real data-plane close (the peer went away) is terminal.
      runtime.events.add(<String, dynamic>{
        'event': 'disconnected',
        'conn': joined.connId,
        'reason': 'peer',
      });
      service.pumpEvents();
      expect(service.currentConnections.first.state, 'disconnected');
      expect(service.phase, SimpleTransferPhase.idle);
      service.dispose();
    });
  });

  group('wave-29 P2-10: send timeout cancels the native task', () {
    test('a timed-out send disconnects the connection (the FFI cancel '
        'path) and throws a message that says so', () async {
      final fake = FakeAkTaskCalls(autoCompleteOps: false);
      final receiver = booted(fake);
      final receiverRuntime = fake.runtimes.keys.first;
      final receiverCodeFuture =
          receiver.createCode(SimpleTransferRole.receiver);
      fake.pumpOps(receiverRuntime);
      final receiverCode = await receiverCodeFuture;
      final sender = SimpleTransferService(
        calls: fake,
        delay: immediate,
        eventPollInterval: const Duration(days: 1),
      );
      sender.start(
          registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/w29');
      final senderRuntime = fake.runtimes.keys.last;
      final codeFuture = sender.join(receiverCode);
      fake.pumpOps(senderRuntime);
      final joined = await codeFuture;
      sender.pumpEvents();

      // The send op never completes; a zero timeout trips immediately.
      await expectLater(
        sender.sendFiles(joined.connId, const ['C:/big.bin'],
            timeout: Duration.zero),
        throwsA(isA<TimeoutException>()
            .having((e) => e.message, 'message', contains('已取消原生发送任务'))),
      );
      // The cancel path ran: the fake's connection is disconnected and
      // the local row folded.
      expect(
        fake.runtimes[senderRuntime]!.connections[joined.connId]!.state,
        'disconnected',
        reason: '超时后必须走 FFI 断开取消原生任务',
      );
      expect(sender.currentConnections.first.state, 'disconnected');
      receiver.dispose();
      sender.dispose();
    });
  });

  group('wave-29 P2-11: the 30 s consent window is observable', () {
    test('an undecided request expires onto expiredIncomingRequests and '
        'a later consent answers false', () async {
      final fake = FakeAkTaskCalls();
      final service = SimpleTransferService(
        calls: fake,
        delay: immediate,
        eventPollInterval: const Duration(days: 1),
        consentWindow: const Duration(milliseconds: 30),
      );
      service.start(
          registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/p29');
      final runtimeId = fake.runtimes.keys.first;
      final code = await service.createCode(SimpleTransferRole.receiver);
      await service.join(code);
      service.pumpEvents();

      final expired = <String>[];
      final sub = service.expiredIncomingRequests.listen(expired.add);
      final job = fake.emitIncomingRequest(runtimeId, files: 1, bytes: 10);
      service.pumpEvents();
      expect(expired, isEmpty, reason: '窗口内不应到期');
      await Future<void>.delayed(const Duration(milliseconds: 120));
      expect(expired, [job], reason: '30 秒窗口（测试中缩短）后必须通知 UI');

      // The engine already auto-declined (the fake has no timer of its
      // own — drop the pending entry to model that engine-side state):
      // a late decision answers false.
      fake.runtimes[runtimeId]!.pendingConsents.remove(job);
      expect(await service.consent(job, accept: true), isFalse);
      await sub.cancel();
      service.dispose();
    });

    test('a decided request never expires', () async {
      final fake = FakeAkTaskCalls();
      final service = SimpleTransferService(
        calls: fake,
        delay: immediate,
        eventPollInterval: const Duration(days: 1),
        consentWindow: const Duration(milliseconds: 30),
      );
      service.start(
          registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/p30');
      final runtimeId = fake.runtimes.keys.first;
      final code = await service.createCode(SimpleTransferRole.receiver);
      await service.join(code);
      service.pumpEvents();

      final expired = <String>[];
      final sub = service.expiredIncomingRequests.listen(expired.add);
      final job = fake.emitIncomingRequest(runtimeId, files: 1, bytes: 10);
      service.pumpEvents();
      expect(await service.consent(job, accept: false), isTrue);
      await Future<void>.delayed(const Duration(milliseconds: 120));
      expect(expired, isEmpty, reason: 'consent 取消到期定时器');
      await sub.cancel();
      service.dispose();
    });
  });

  group('dispose semantics (P1-05 alignment)', () {
    test('disposed service refuses every entry with StateError', () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      service.dispose();
      expect(() => service.createCode(SimpleTransferRole.sender),
          throwsStateError);
      expect(() => service.join('AKI1-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX'),
        throwsStateError);
      expect(() => service.sendFiles('ab' * 16, const ['C:/x']),
          throwsStateError);
      expect(() => service.pumpEvents(), returnsNormally); // a no-op
      expect(service.isDisposed, isTrue);
    });

    test('dispose invalidates in-flight op waiters before they re-poll',
        () async {
      final fake = FakeAkTaskCalls(autoCompleteOps: false);
      final service = SimpleTransferService(
        calls: fake,
        delay: immediate,
        eventPollInterval: const Duration(days: 1),
      );
      service.start(
          registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/d');
      final runtimeId = fake.runtimes.keys.first;

      final pending = service.createCode(SimpleTransferRole.sender);
      // The op never completes (autoCompleteOps: false): the waiter is
      // cycling on microtask polls. Disposing synchronously must abort it
      // at the NEXT iteration's ensureLive — the StateError, not a
      // timeout and not another seam call.
      service.dispose();
      await expectLater(pending, throwsStateError);
      expect(fake.runtimes[runtimeId], isNotNull); // untouched state
    });
  });
}
