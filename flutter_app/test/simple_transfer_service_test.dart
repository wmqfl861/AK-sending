/// Wave-28 service-layer tests: [SimpleTransferService]'s state machine
/// and event sequences, driven end to end by [FakeAkTaskCalls] (the same
/// shipped fake — no DLL, no sockets; the Rust side covers the real
/// bytes).
///
/// Wave-30 additions: per-LISTEN snapshot replay (P1-06 completion),
/// code-id cancel (P2-04), the `done` terminal semantics (P2-05/P2-08),
/// the request's saveDir snapshot (P2-06), progress-renewed stall
/// timeouts (P2-10) and native consent expiry (P2-11).
library;

import 'dart:async' show Timer, TimeoutException;

import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show AkFfiException, AkTaskLeaseGate, FakeAkTaskCalls, TaskDoneEvent;
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
      final created = await creator.createCode(SimpleTransferRole.receiver);
      expect(created.code.startsWith('AKI1-'), isTrue);
      expect(creator.phase, SimpleTransferPhase.paired);

      // The peer joins (second runtime on the same fake = same engine).
      final joiner = SimpleTransferService(
        calls: fake,
        delay: immediate,
        eventPollInterval: const Duration(days: 1),
      );
      joiner.start(
          registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/joiner');
      await joiner.join(created.code);
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
      final created = await service.createCode(SimpleTransferRole.sender);
      final joined = await service.join(created.code);
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
      final created = await service.createCode(SimpleTransferRole.sender);
      expect(created.code.startsWith('AKI1-'), isTrue);
      expect(service.phase, SimpleTransferPhase.paired);
      service.dispose();
    });

    test('disconnect returns to idle when the last connection severs',
        () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      final created = await service.createCode(SimpleTransferRole.sender);
      final joined = await service.join(created.code);
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
      final created = await service.createCode(SimpleTransferRole.receiver);
      final joined = await service.join(created.code);
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
      final joined = await senderService.join(receiverCode.code);
      expect(joined.role, 'sender');
      await senderService.sendFiles(joined.connId, const ['C:/pics/a.jpg']);
      senderService.pumpEvents();
      // Wave-30 P2-05: the one-task transfer COMPLETED — the fake emits
      // the `done` event with the op announcement and the row converges
      // to the completed terminal state (phase back to idle).
      expect(senderService.currentConnections.first.state, 'done',
          reason: '发送完成后行收敛为完成态');
      expect(senderService.phase, SimpleTransferPhase.idle);
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
  // (P2-08/P2-09), timeout cancels (P2-10).
  // -------------------------------------------------------------------------

  group('P1-06: connections replay per LISTEN', () {
    test('a LATE subscriber (the detail-page shape) receives the current '
        'list while an earlier listener stays attached and no event follows',
        () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      final created = await service.createCode(SimpleTransferRole.sender);
      final joined = await service.join(created.code);
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

    test('wave-30 residual: TWO listens of the SAME saved stream object '
        'each receive the snapshot first (broadcast onListen only fired '
        'once — this is the exact residual shape)', () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      final created = await service.createCode(SimpleTransferRole.sender);
      final joined = await service.join(created.code);
      service.pumpEvents();
      expect(service.currentConnections.first.state, 'connected');

      // The consumer HOLDS the stream object and subscribes twice.
      final stream = service.connections;
      final firstSightings = <List<SimpleConnection>>[];
      final secondSightings = <List<SimpleConnection>>[];
      final first = stream.listen(firstSightings.add);
      final second = stream.listen(secondSightings.add);
      await pumpEventQueue();

      expect(firstSightings, isNotEmpty,
          reason: '同一流对象的第一订阅必须拿到快照');
      expect(secondSightings, isNotEmpty,
          reason: '同一流对象的第二订阅也必须拿到快照（P1-06 残项）');
      expect(firstSightings.first.first.connId, joined.connId);
      expect(secondSightings.first.first.connId, joined.connId);
      expect(secondSightings.first.first.state, 'connected');

      // Cancelling ONE listener keeps the other live (per-listen taps).
      await first.cancel();
      await service.disconnect(joined.connId);
      service.pumpEvents();
      await pumpEventQueue();
      expect(secondSightings.last.first.state, 'disconnected',
          reason: '取消一个订阅后，其余订阅仍收到 live 事件');
      await second.cancel();
      service.dispose();
    });
  });

  group('wave-29 P2-09: monotonic state merging', () {
    test('probe reachable=false is an honest unreachable verdict, never '
        '"success"', () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      final runtimeId = fake.runtimes.keys.first;
      final created = await service.createCode(SimpleTransferRole.sender);
      final joined = await service.join(created.code);
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
      final created = await codeFuture;

      final joinFuture = service.join(created.code);
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
      final created = await codeFuture;

      final joinFuture = service.join(created.code);
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

  group('P2-05/P2-08: terminal semantics are source-separated', () {
    test('a coordination-plane notice keeps a healthy data connection '
        'alive; a data-plane reason kills the row', () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      final runtimeId = fake.runtimes.keys.first;
      final created = await service.createCode(SimpleTransferRole.sender);
      final joined = await service.join(created.code);
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

    test('wave-30 P2-05: the done EVENT converges the row to the '
        'completed terminal state — no send entry is offered afterwards',
        () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      final runtimeId = fake.runtimes.keys.first;
      final created = await service.createCode(SimpleTransferRole.sender);
      final joined = await service.join(created.code);
      service.pumpEvents();
      expect(service.currentConnections.first.state, 'connected');

      fake.emitDone(runtimeId, connId: joined.connId, files: 2, bytes: 42);
      service.pumpEvents();
      expect(service.currentConnections.first.state, 'done',
          reason: 'done 事件必须把行收敛为完成态（P2-05）');
      expect(service.phase, SimpleTransferPhase.idle,
          reason: '完成态与断开同样终结核阶段');
      // The completed row is terminal: a late probe/connected event must
      // not revive it (monotonic rank).
      fake.runtimes[runtimeId]!.events.add(<String, dynamic>{
        'event': 'probe_result',
        'conn': joined.connId,
        'endpoint': null,
        'reachable': true,
      });
      service.pumpEvents();
      expect(service.currentConnections.first.state, 'done',
          reason: 'done 后行不得被复活');
      service.dispose();
    });

    test('wave-30 P2-05: the done DISCONNECT REASON converges the row '
        'the same way (registry still never does)', () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      final runtimeId = fake.runtimes.keys.first;
      final created = await service.createCode(SimpleTransferRole.sender);
      final joined = await service.join(created.code);
      service.pumpEvents();

      fake.runtimes[runtimeId]!.events.add(<String, dynamic>{
        'event': 'disconnected',
        'conn': joined.connId,
        'reason': 'done',
      });
      service.pumpEvents();
      expect(service.currentConnections.first.state, 'done');
      expect(service.phase, SimpleTransferPhase.idle);
      service.dispose();
    });
  });

  group('wave-30 P2-04: code-id cancel of an un-joined code', () {
    test('createCode hands up the engine code_id; disconnect(codeId) '
        'reaches the native revoke and the code dies', () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      final runtimeId = fake.runtimes.keys.first;
      final created = await service.createCode(SimpleTransferRole.receiver);
      expect(created.codeId, isNotNull,
          reason: 'create 结果必须上交 code_id（P2-04）');
      expect(created.codeId, isNot(created.code));

      await service.disconnect(created.codeId!);
      expect(fake.disconnectCalls, contains(created.codeId),
          reason: '取消必须走原生撤码调用（disconnect by code_id）');
      // The registry row is revoked: a later join of the cancelled code
      // fails (the fake's code table dropped it) — the code is dead.
      final joiner = SimpleTransferService(
        calls: fake,
        delay: immediate,
        eventPollInterval: const Duration(days: 1),
      );
      joiner.start(
          registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/c1');
      await expectLater(joiner.join(created.code), throwsA(isA<AkFfiException>()));
      service.dispose();
      joiner.dispose();
      expect(fake.runtimes[runtimeId], isNotNull);
    });
  });

  group('wave-30 P2-06: the incoming request carries its saveDir', () {
    test('the event field flows to the request untouched (the offer-time '
        'snapshot, not the live setting)', () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      final runtimeId = fake.runtimes.keys.first;
      final created = await service.createCode(SimpleTransferRole.receiver);
      await service.join(created.code);
      service.pumpEvents();
      await service.setSaveDir('D:/new-setting');

      final requests = <SimpleIncomingRequest>[];
      final sub = service.incomingRequests.listen(requests.add);
      fake.emitIncomingRequest(runtimeId,
          files: 1, bytes: 10, saveDir: 'D:/offer-time-folder');
      service.pumpEvents();
      await pumpEventQueue();
      expect(requests, hasLength(1));
      expect(requests.first.saveDir, 'D:/offer-time-folder',
          reason: '请求必须带 native 侧快照目录（事件字段为准）');
      await sub.cancel();
      service.dispose();
    });
  });

  group('wave-30 P2-10: progress-renewed stall timeouts', () {
    test('a send with CONTINUOUS progress outlives the nominal timeout; '
        'only a true stall is cut', () async {
      final fake = FakeAkTaskCalls(autoCompleteOps: false);
      final receiver = SimpleTransferService(
        calls: fake,
        eventPollInterval: const Duration(days: 1), // real delay below
      );
      receiver.start(
          registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/w30a');
      final receiverRuntime = fake.runtimes.keys.first;
      final receiverCodeFuture =
          receiver.createCode(SimpleTransferRole.receiver);
      fake.pumpOps(receiverRuntime);
      final receiverCode = await receiverCodeFuture;

      final sender = SimpleTransferService(
        calls: fake,
        eventPollInterval: const Duration(days: 1),
        opPollInterval: const Duration(milliseconds: 20),
      );
      sender.start(
          registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/w30b');
      final senderRuntime = fake.runtimes.keys.last;
      final joinFuture = sender.join(receiverCode.code);
      fake.pumpOps(senderRuntime);
      final joined = await joinFuture;
      sender.pumpEvents();

      // The op never completes on its own; the send is IN FLIGHT while
      // progress lines keep arriving (the send row keys them by the
      // CONNECTION id). 200 ms stall budget with 600 ms of progress —
      // the old overall cap would have cut at 200 ms; the stall budget
      // must not.
      final sendFuture = sender.sendFiles(joined.connId, const ['C:/big.bin'],
          timeout: const Duration(milliseconds: 200));
      final progress = Timer.periodic(const Duration(milliseconds: 25),
          (_) => fake.emitProgress(joined.connId));
      await Future<void>.delayed(const Duration(milliseconds: 600));
      fake.pumpOps(senderRuntime); // the long transfer finally completes
      final report = await sendFuture;
      progress.cancel();
      expect(report['files'], 1,
          reason: '持续进度的长传输不得被停滞预算误杀（P2-10）');
      receiver.dispose();
      sender.dispose();
    });

    test('a stalled send (no progress line) is cut by the stall budget '
        'and the cancel path disconnects the native connection', () async {
      final fake = FakeAkTaskCalls(autoCompleteOps: false);
      final receiver = SimpleTransferService(
        calls: fake,
        delay: immediate,
        eventPollInterval: const Duration(days: 1),
      );
      receiver.start(
          registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/w30c');
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
          registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/w30d');
      final senderRuntime = fake.runtimes.keys.last;
      final codeFuture = sender.join(receiverCode.code);
      fake.pumpOps(senderRuntime);
      final joined = await codeFuture;
      sender.pumpEvents();

      // No progress ever: the stall budget trips.
      await expectLater(
        sender.sendFiles(joined.connId, const ['C:/big.bin'],
            timeout: Duration.zero),
        throwsA(isA<TimeoutException>()
            .having((e) => e.message, 'message', contains('已取消原生发送任务'))),
      );
      expect(
        fake.runtimes[senderRuntime]!.connections[joined.connId]!.state,
        'disconnected',
        reason: '停滞超时后必须走 FFI 断开取消原生任务',
      );
      expect(sender.currentConnections.first.state, 'disconnected');
      receiver.dispose();
      sender.dispose();
    });

    test('a FAILED cancel never claims 已取消 — the error surfaces '
        'honestly', () async {
      final fake = FakeAkTaskCalls(autoCompleteOps: false);
      final receiver = SimpleTransferService(
        calls: fake,
        delay: immediate,
        eventPollInterval: const Duration(days: 1),
      );
      receiver.start(
          registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/w30e');
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
          registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/w30f');
      final senderRuntime = fake.runtimes.keys.last;
      final codeFuture = sender.join(receiverCode.code);
      fake.pumpOps(senderRuntime);
      final joined = await codeFuture;
      sender.pumpEvents();

      fake.nextDisconnectFails = 'engine busy';
      await expectLater(
        sender.sendFiles(joined.connId, const ['C:/big.bin'],
            timeout: Duration.zero),
        throwsA(isA<TimeoutException>().having((e) => e.message, 'message',
            allOf(contains('取消原生任务失败'), contains('engine busy')))),
      );
      receiver.dispose();
      sender.dispose();
    });
  });

  group('wave-30 P2-11: consent expiry is a native event, not a timer', () {
    test('the consent_expired event fires expiredIncomingRequests and a '
        'later consent answers false', () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      final runtimeId = fake.runtimes.keys.first;
      final created = await service.createCode(SimpleTransferRole.receiver);
      await service.join(created.code);
      service.pumpEvents();

      final expired = <String>[];
      final sub = service.expiredIncomingRequests.listen(expired.add);
      final job = fake.emitIncomingRequest(runtimeId, files: 1, bytes: 10);
      service.pumpEvents();
      expect(expired, isEmpty, reason: '事件到来前不应到期');

      fake.expireConsent(runtimeId, job); // the native 30 s window closed
      service.pumpEvents();
      await pumpEventQueue();
      expect(expired, [job],
          reason: '原生 consent_expired 事件必须驱动横幅关闭（P2-11）');
      expect(await service.consent(job, accept: true), isFalse,
          reason: '引擎已自动拒绝：迟到决定为 no-op');
      await sub.cancel();
      service.dispose();
    });

    test('a decided request never expires', () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      final runtimeId = fake.runtimes.keys.first;
      final created = await service.createCode(SimpleTransferRole.receiver);
      await service.join(created.code);
      service.pumpEvents();

      final expired = <String>[];
      final sub = service.expiredIncomingRequests.listen(expired.add);
      final job = fake.emitIncomingRequest(runtimeId, files: 1, bytes: 10);
      service.pumpEvents();
      expect(await service.consent(job, accept: false), isTrue);
      fake.expireConsent(runtimeId, job); // decided: the engine stays quiet
      service.pumpEvents();
      await pumpEventQueue();
      expect(expired, isEmpty, reason: '已决定的请求不再发出到期事件');
      await sub.cancel();
      service.dispose();
    });

    test('a data-plane terminal connection closes its pending banners '
        '(the engine rejects pending offers with the connection)', () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      final runtimeId = fake.runtimes.keys.first;
      final created = await service.createCode(SimpleTransferRole.receiver);
      final joined = await service.join(created.code);
      service.pumpEvents();

      final expired = <String>[];
      final sub = service.expiredIncomingRequests.listen(expired.add);
      final job = fake.emitIncomingRequest(runtimeId,
          connId: joined.connId, files: 1, bytes: 10);
      service.pumpEvents();
      expect(expired, isEmpty);

      // The connection dies before the user answers.
      fake.runtimes[runtimeId]!.events.add(<String, dynamic>{
        'event': 'disconnected',
        'conn': joined.connId,
        'reason': 'peer',
      });
      service.pumpEvents();
      await pumpEventQueue();
      expect(expired, [job],
          reason: '连接断开必须关闭其 pending 横幅（P2-11）');
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

    test('R30-P1-01: a disposed service drainProgress never touches the '
        'seam (the queued line stays queued)', () {
      final fake = FakeAkTaskCalls();
      final service = SimpleTransferService(
        calls: fake,
        delay: immediate,
        eventPollInterval: const Duration(days: 1),
      );
      service.start(
          registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/w31d');
      fake.emitProgress('cd' * 16);
      service.drainProgress();
      expect(service.progressTick('cd' * 16), 1, reason: '存活时照常入账');
      expect(fake.progressLines, isEmpty, reason: '存活时消费了该行');

      service.dispose();
      fake.emitProgress('cd' * 16);
      service.drainProgress(); // the R30-P1-01 entry gate: a no-op
      expect(service.progressTick('cd' * 16), 1,
          reason: '释放后不得再入账任何进度');
      expect(fake.progressLines, hasLength(1),
          reason: '释放后零 native 调用：排队行必须原封不动');
    });
  });

  // -------------------------------------------------------------------------
  // Wave-31 review fixes (R30): pump-consumed progress renewal
  // (R30-P2-01), request_closed + late-incoming refusal (R30-P2-03), and
  // the joiner-side code_id mapping (R30-P2-02).
  // -------------------------------------------------------------------------

  group('wave-31 R30-P2-01: pump-consumed progress still renews', () {
    test('with the event pump at the PRODUCTION cadence, progress the '
        'pump reads FIRST still renews the send stall budget', () async {
      final fake = FakeAkTaskCalls(autoCompleteOps: false);
      final receiver = SimpleTransferService(
        calls: fake,
        delay: immediate,
        eventPollInterval: const Duration(days: 1),
      );
      receiver.start(
          registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/w31a');
      final receiverRuntime = fake.runtimes.keys.first;
      final receiverCodeFuture =
          receiver.createCode(SimpleTransferRole.receiver);
      fake.pumpOps(receiverRuntime);
      final receiverCode = await receiverCodeFuture;

      // The sender keeps BOTH production cadences: the 50 ms event pump
      // reads every progress line FIRST and the 200 ms send waiter only
      // ever observes the ticks the pump already credited — the exact
      // dual-consumer timing a per-poll baseline erased (R30-P2-01).
      final sender = SimpleTransferService(calls: fake);
      sender.start(
          registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/w31b');
      final senderRuntime = fake.runtimes.keys.last;
      final joinFuture = sender.join(receiverCode.code);
      fake.pumpOps(senderRuntime);
      final joined = await joinFuture;
      sender.pumpEvents();

      final sendFuture = sender.sendFiles(joined.connId, const ['C:/big.bin'],
          timeout: const Duration(milliseconds: 200));
      final progress = Timer.periodic(const Duration(milliseconds: 25),
          (_) => fake.emitProgress(joined.connId));
      await Future<void>.delayed(const Duration(milliseconds: 600));
      fake.pumpOps(senderRuntime); // the long transfer finally completes
      final report = await sendFuture;
      progress.cancel();
      expect(report['files'], 1,
          reason: '被事件泵先读走的进度也必须续期（R30-P2-01）');
      receiver.dispose();
      sender.dispose();
    });
  });

  group('wave-31 R30-P2-02: the joiner learns the pairing code_id', () {
    test('a JOINER (receiver of a sender-minted code) fills '
        'codeIdOfConnection from the join result / connected event',
        () async {
      final fake = FakeAkTaskCalls();
      final creator = SimpleTransferService(
        calls: fake,
        delay: immediate,
        eventPollInterval: const Duration(days: 1),
      );
      creator.start(
          registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/w31c');
      final created =
          await creator.createCode(SimpleTransferRole.sender);

      final joiner = SimpleTransferService(
        calls: fake,
        delay: immediate,
        eventPollInterval: const Duration(days: 1),
      );
      joiner.start(
          registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/w31e');
      final joined = await joiner.join(created.code);
      expect(joiner.codeIdOfConnection(joined.connId), created.codeId,
          reason: '加入方也必须知道配对 code_id（join 结果/connected 事件）');
      joiner.pumpEvents(); // the connected event path fills it too
      expect(joiner.codeIdOfConnection(joined.connId), created.codeId);
      creator.dispose();
      joiner.dispose();
    });
  });

  group('wave-31 R30-P2-03: request_closed and late incomings', () {
    test('request_closed closes a pending banner (the engine fact, no '
        'connection event needed)', () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      final runtimeId = fake.runtimes.keys.first;
      final created = await service.createCode(SimpleTransferRole.receiver);
      final joined = await service.join(created.code);
      service.pumpEvents();

      final requests = <SimpleIncomingRequest>[];
      final expired = <String>[];
      final reqSub = service.incomingRequests.listen(requests.add);
      final expSub = service.expiredIncomingRequests.listen(expired.add);
      final job = fake.emitIncomingRequest(runtimeId,
          connId: joined.connId, files: 1, bytes: 10);
      service.pumpEvents();
      await pumpEventQueue();
      expect(requests, hasLength(1));

      // The engine resolved the parked offer with the connection's
      // disconnect (decline_pending_of_conn) — before/without any
      // `disconnected` event being pumped.
      fake.closePendingRequest(runtimeId, job, connId: joined.connId);
      service.pumpEvents();
      await pumpEventQueue();
      expect(expired, [job],
          reason: 'request_closed 必须关闭 pending 横幅（R30-P2-03）');
      expect(await service.consent(job, accept: true), isFalse,
          reason: '请求已终结：迟到决定为 no-op');
      await reqSub.cancel();
      await expSub.cancel();
      service.dispose();
    });

    test('terminal-first race: disconnected → request_closed → the LATE '
        'incoming is dropped — no permanent banner', () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      final runtimeId = fake.runtimes.keys.first;
      final created = await service.createCode(SimpleTransferRole.receiver);
      final joined = await service.join(created.code);
      service.pumpEvents();

      final requests = <SimpleIncomingRequest>[];
      final expired = <String>[];
      final reqSub = service.incomingRequests.listen(requests.add);
      final expSub = service.expiredIncomingRequests.listen(expired.add);

      // The exact native interleave of R30-P2-03: the register_consent
      // worker inserted the pending entry and released the lock; the
      // disconnect publishes the data-plane terminal fact, then the
      // request-closure fact; the worker's synchronous callback finishes
      // and publishes the LATE incoming announcement.
      final job = 'ab' * 16;
      fake.runtimes[runtimeId]!.events
          .add(<String, dynamic>{'event': 'disconnected', 'conn': joined.connId, 'reason': 'peer'});
      fake.runtimes[runtimeId]!.events.add(<String, dynamic>{
        'event': 'request_closed',
        'job': job,
        'conn': joined.connId,
        'reason': 'disconnected',
      });
      fake.runtimes[runtimeId]!.events.add(<String, dynamic>{
        'event': 'incoming_request',
        'job': job,
        'conn': joined.connId,
        'root': 'x',
        'files': 1,
        'bytes': 1,
        'saveDir': 'D:/x',
      });
      service.pumpEvents();
      await pumpEventQueue();

      expect(requests, isEmpty,
          reason: '迟到 incoming 不得再开待同意横幅（R30-P2-03）');
      expect(expired, [job],
          reason: 'request_closed 关闭了对应请求');
      await reqSub.cancel();
      await expSub.cancel();
      service.dispose();
    });

    test('a late incoming on an ALREADY-terminal connection is dropped '
        'even without request_closed', () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      final runtimeId = fake.runtimes.keys.first;
      final created = await service.createCode(SimpleTransferRole.receiver);
      final joined = await service.join(created.code);
      service.pumpEvents();

      final requests = <SimpleIncomingRequest>[];
      final expired = <String>[];
      final reqSub = service.incomingRequests.listen(requests.add);
      final expSub = service.expiredIncomingRequests.listen(expired.add);

      // The connection died with NO pending request yet; the worker's
      // late announcement follows on the same (now terminal) conn.
      fake.runtimes[runtimeId]!.events
          .add(<String, dynamic>{'event': 'disconnected', 'conn': joined.connId, 'reason': 'peer'});
      service.pumpEvents();
      expect(service.currentConnections.first.state, 'disconnected');

      fake.emitIncomingRequest(runtimeId,
          connId: joined.connId, files: 1, bytes: 10);
      service.pumpEvents();
      await pumpEventQueue();
      expect(requests, isEmpty,
          reason: '其连接已数据面终态的 incoming 必须丢弃');
      expect(expired, isEmpty,
          reason: '该请求从未登记，也不应发到期事件');
      // A HEALTHY connection's incoming still registers (the guard must
      // not over-block): re-join on a fresh code and offer again.
      final created2 = await service.createCode(SimpleTransferRole.receiver);
      final joined2 = await service.join(created2.code);
      service.pumpEvents();
      fake.emitIncomingRequest(runtimeId,
          connId: joined2.connId, files: 1, bytes: 10);
      service.pumpEvents();
      await pumpEventQueue();
      expect(requests, hasLength(1),
          reason: '健康连接上的 incoming 照常登记');
      await reqSub.cancel();
      await expSub.cancel();
      service.dispose();
    });
  });

  group('C12: pump budget + engine-failure latch', () {
    test('a >budget backlog clears across ticks, in order (64/tick)', () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      final runtimeId = fake.runtimes.values.first.id;
      final runtimeEvents = fake.runtimes[runtimeId]!.events;

      final doneEvents = <TaskDoneEvent>[];
      final sub = service.doneEvents.listen(doneEvents.add);

      // 100 no-op events, then ONE done event last: the done must not
      // overtake the budget — it lands only when the whole backlog did.
      for (var i = 0; i < 100; i++) {
        runtimeEvents
            .add(<String, dynamic>{'event': 'runtime_ready', 'runtime': runtimeId});
      }
      runtimeEvents.add(<String, dynamic>{
        'event': 'done',
        'files': 1,
        'bytes': 42,
      });

      service.pumpEvents(); // tick 1: at most pumpEventBudget events
      expect(
          runtimeEvents.length, 101 - SimpleTransferService.pumpEventBudget,
          reason: '一轮不清空：预算内事件已路由，其余留在队列');
      expect(doneEvents, isEmpty,
          reason: '队尾事件未越过预算被提前处理');

      service.pumpEvents(); // tick 2: the remaining backlog, in order
      await pumpEventQueue(); // broadcast deliveries are async
      expect(runtimeEvents, isEmpty,
          reason: '分轮清空：余量在后续轮次按序处理');
      expect(doneEvents, hasLength(1));
      expect(doneEvents.single.files, 1);
      expect(doneEvents.single.bytes, 42);
      expect(service.phase, SimpleTransferPhase.idle,
          reason: '正常事件流不受预算影响');
      await sub.cancel();
      service.dispose();
    });

    test('progress drain is bounded too: a >budget backlog splits across '
        'ticks', () {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      const job = 'abababababababababababababababab';
      for (var i = 0; i < 100; i++) {
        fake.emitProgress(job);
      }
      service.pumpEvents();
      expect(service.progressTick(job), SimpleTransferService.pumpEventBudget,
          reason: '一轮最多 drain 预算条进度行');
      service.pumpEvents();
      service.pumpEvents();
      expect(service.progressTick(job), 100);
      expect(service.phase, SimpleTransferPhase.idle);
      service.dispose();
    });

    test('10 consecutive poll failures latch engineFailed, stop the pump, '
        'and stay visible', () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);

      // threshold-1 failures: still the transient regime.
      for (var i = 0; i < SimpleTransferService.pollFailureThreshold - 1; i++) {
        fake.failPollEventCalls = 1;
        service.pumpEvents();
      }
      expect(service.isEngineFailed, isFalse);
      expect(service.phase, isNot(SimpleTransferPhase.engineFailed));

      fake.failPollEventCalls = 1;
      final pollsBeforeLatch = fake.pollEventCallCount;
      service.pumpEvents(); // the threshold failure
      expect(service.isEngineFailed, isTrue);
      expect(service.phase, SimpleTransferPhase.engineFailed);
      expect(service.failureReason, contains('传输引擎故障'));
      expect(service.failureReason, contains(fake.pollEventFailureDetail),
          reason: '可观察错误带上最后一次原生失败细节');

      // The pump is dead: even scripted failures and a backlog never
      // reach native again (fake counters static).
      fake.failPollEventCalls = 1000;
      final runtimeId = fake.runtimes.values.first.id;
      fake.runtimes[runtimeId]!.events
          .add(<String, dynamic>{'event': 'runtime_ready', 'runtime': runtimeId});
      service.pumpEvents();
      expect(fake.pollEventCallCount, pollsBeforeLatch + 1,
          reason: '闩上后仅本次越限轮询，其后泵不再触达原生');

      // The latch is TERMINAL: a later action ends with the SAME failure
      // (R36-P2-03 — never runs against the broken engine) and no state
      // transition ever masks it.
      await expectLater(
        service.createCode(SimpleTransferRole.sender),
        throwsA(isA<StateError>().having(
            (e) => e.message, 'message', contains('传输引擎故障'))),
      );
      expect(service.phase, SimpleTransferPhase.engineFailed,
          reason: '任何后续状态迁移都不遮蔽 engineFailed');
      service.dispose();
    });

    test('one clean tick resets the failure streak', () {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      for (var i = 0; i < SimpleTransferService.pollFailureThreshold - 1; i++) {
        fake.failPollEventCalls = 1;
        service.pumpEvents();
      }
      service.pumpEvents(); // a clean tick: queue empty, streak resets
      expect(service.isEngineFailed, isFalse);
      for (var i = 0; i < SimpleTransferService.pollFailureThreshold - 1; i++) {
        fake.failPollEventCalls = 1;
        service.pumpEvents();
      }
      expect(service.isEngineFailed, isFalse,
          reason: '9 + clean + 9 不是“连续”10 次');
      service.dispose();
    });

    test('malformed progress lines count toward the same threshold', () {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      const job = 'cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd';
      // The fake serves raw lines FIRST: one HEALTHY raw line ahead of
      // the malformed ones — it must still be credited before the latch.
      fake.rawProgressLines.add('{"job":"$job","event":"chunk_sent"}');
      const extraMalformed = 5; // beyond the threshold: never drained
      for (var i = 0;
          i < SimpleTransferService.pollFailureThreshold + extraMalformed;
          i++) {
        fake.rawProgressLines.add('{oops not json');
      }
      service.pumpEvents();
      expect(service.progressTick(job), 1,
          reason: '坏行前的好行照常计分');
      expect(service.isEngineFailed, isTrue);
      expect(service.phase, SimpleTransferPhase.engineFailed);
      expect(service.failureReason, contains('解析失败'));
      // Latched mid-drain: the malformed lines beyond the threshold are
      // never polled.
      expect(fake.rawProgressLines.length, extraMalformed,
          reason: '闩上即停：剩余坏行不再被 drain');
      service.dispose();
    });

    test('start() refuses after the latch: recovery is service rebuild',
        () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      for (var i = 0; i < SimpleTransferService.pollFailureThreshold; i++) {
        fake.failPollEventCalls = 1; // one failure per tick
        service.pumpEvents();
      }
      expect(service.isEngineFailed, isTrue);
      await expectLater(
        service.start(
            registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/x'),
        throwsStateError,
      );
      service.dispose();
    });

    testWidgets('the real 50ms timer pump stops itself at the threshold',
        (tester) async {
      final fake = FakeAkTaskCalls()..failPollEventCalls = 1000;
      final service = SimpleTransferService(
        calls: fake,
        delay: immediate,
        eventPollInterval: const Duration(milliseconds: 50),
        // CR-P2-05: the service starts IDLE, so pin the idle cadence to
        // the same 50 ms — this test drives the timer pump, not the
        // adaptive-cadence behavior (covered by the CR-P2-05 group).
        idlePollInterval: const Duration(milliseconds: 50),
      );
      await service.start(
          registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/t');
      // The startup pump already failed once; each 50ms tick adds one.
      for (var i = 0; i < 12 && !service.isEngineFailed; i++) {
        await tester.pump(const Duration(milliseconds: 50));
      }
      expect(service.isEngineFailed, isTrue,
          reason: '连续 10 次轮询异常后进入传输引擎故障态');
      final pollsAtStop = fake.pollEventCallCount;
      await tester.pump(const Duration(milliseconds: 500));
      expect(fake.pollEventCallCount, pollsAtStop,
          reason: '泵已停：50ms tick 不再触达原生');
      service.dispose();
    });

    test('C07×C12: a revoked owner lease latches engineFailed WITHOUT '
        'touching native', () {
      final fake = FakeAkTaskCalls();
      var alive = true;
      final service = SimpleTransferService(
        calls: AkTaskLeaseGate(fake, isAlive: () => alive),
        delay: immediate,
        eventPollInterval: const Duration(days: 1),
      );
      service.start(
          registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/l');
      final polls = fake.pollEventCallCount;

      alive = false; // the owner released the engine under the live pump
      service.pumpEvents();
      expect(service.isEngineFailed, isTrue,
          reason: '租约撤销即刻闩上：绝不重试悬空指针');
      expect(service.phase, SimpleTransferPhase.engineFailed);
      expect(service.failureReason, contains('租约'));
      expect(fake.pollEventCallCount, polls,
          reason: '原生从未被触达：StateError 在绑定层就挡住了');
      service.dispose();
    });

    test('R36-P2-02: healthy events + persistently failing progress poll '
        'latch engineFailed at the 10th consecutive tick', () {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      // The events channel stays healthy; ONLY the native progress poll
      // keeps failing (the exact split the old reset bug hid — each tick
      // was 0→1→0 and the latch could never trip).
      fake.failPollProgressCalls = 1000;
      for (var i = 0; i < SimpleTransferService.pollFailureThreshold - 1; i++) {
        service.pumpEvents();
        expect(
            service.isEngineFailed, isFalse,
            reason: '阈值前不闩：第 ${i + 1} 次失败仍在瞬态区间');
      }
      service.pumpEvents(); // the threshold failure
      expect(service.isEngineFailed, isTrue,
          reason: '事件通道健康不得清零进度通道的连续失败计数');
      expect(service.phase, SimpleTransferPhase.engineFailed);
      expect(service.failureReason, contains('进度事件轮询失败'));
      expect(service.failureReason, contains(fake.pollProgressFailureDetail),
          reason: '可观察错误带上最后一次原生失败细节');
      service.dispose();
    });

    test('R36-P2-02: a FULLY clean tick (events + progress both healthy) '
        'still resets the progress-failure streak', () {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      for (var i = 0; i < SimpleTransferService.pollFailureThreshold - 1; i++) {
        fake.failPollProgressCalls = 1;
        service.pumpEvents(); // events healthy, progress fails once
      }
      service.pumpEvents(); // both channels clean: the streak resets
      for (var i = 0; i < SimpleTransferService.pollFailureThreshold - 1; i++) {
        fake.failPollProgressCalls = 1;
        service.pumpEvents();
      }
      expect(service.isEngineFailed, isFalse,
          reason: '9 + 全净 + 9 不是“连续”10 次（进度通道同理）');
      service.dispose();
    });

    test('R36-P2-02: drainProgress reports the explicit poll outcome '
        '(polled=false on native failure, clean=true when healthy)', () {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      expect(service.drainProgress().clean, isTrue,
          reason: '健康时（队列空）poll 到头且无坏行');
      fake.failPollProgressCalls = 1;
      expect(service.drainProgress().polled, isFalse,
          reason: '原生轮询异常必须显式报告为“未完成”，不再折叠成 0');
      expect(service.drainProgress().clean, isTrue,
          reason: '单次失败后的干净调用照常 poll 到头');
      service.dispose();
    });

    test('R36-P2-03: the latch fans out on the replayable engineHealth '
        'stream; a late subscriber replays it; entries end with the SAME '
        'failure', () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      final health = <EngineHealth>[];
      final sub = service.engineHealth.listen(health.add);
      await pumpEventQueue();
      expect(health, hasLength(1), reason: '订阅即快照：先收到当前健康态');
      expect(health.single.isEngineFailed, isFalse);

      for (var i = 0; i < SimpleTransferService.pollFailureThreshold; i++) {
        fake.failPollEventCalls = 1;
        service.pumpEvents();
      }
      await pumpEventQueue();
      expect(health, hasLength(2), reason: '闩上即刻推送（不是只改内部字段）');
      expect(health.last.isEngineFailed, isTrue);
      expect(health.last.failureReason, service.failureReason);
      expect(health.last.failureReason, contains('传输引擎故障'));

      // A LATE subscriber replays the latched state without any new event.
      final late = <EngineHealth>[];
      final lateSub = service.engineHealth.listen(late.add);
      await pumpEventQueue();
      expect(late, hasLength(1));
      expect(late.single.isEngineFailed, isTrue,
          reason: '回放契约：迟到的订阅者也看到闩上的故障');

      // Entries after the latch end with the SAME failure (R36-P2-03).
      expect(() => service.sendFiles('ab' * 16, const ['C:/x']),
          throwsA(isA<StateError>().having(
              (e) => e.message, 'message', contains('传输引擎故障'))));
      await sub.cancel();
      await lateSub.cancel();
      service.dispose();
    });

    test('R36-P2-03: an IN-FLIGHT op ends with the same engine failure at '
        'its next poll tick (never hangs to its own timeout)', () async {
      final fake = FakeAkTaskCalls(autoCompleteOps: false);
      final service = SimpleTransferService(
        calls: fake,
        delay: immediate,
        eventPollInterval: const Duration(days: 1),
      );
      service.start(
          registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/p203');

      final pending = service.createCode(SimpleTransferRole.sender);
      // The engine latches while the op is still waiting on microtask
      // polls — the waiter's next ensureLive iteration must abort with
      // the engine failure, not a timeout and not another seam call.
      for (var i = 0; i < SimpleTransferService.pollFailureThreshold; i++) {
        fake.failPollEventCalls = 1;
        service.pumpEvents();
      }
      expect(service.isEngineFailed, isTrue);
      await expectLater(
        pending,
        throwsA(isA<StateError>().having(
            (e) => e.message, 'message', contains('传输引擎故障'))),
      );
      service.dispose();
    });
  });

  // -------------------------------------------------------------------------
  // CR-P2-05 (review 2026-11-11): adaptive event-poll cadence — idle drops
  // to 1 s, activity returns to 50 ms IMMEDIATELY (never waiting out the
  // slow tick), and the app-lifecycle lever slows/pauses the pump.
  // -------------------------------------------------------------------------

  group('CR-P2-05: adaptive event-poll cadence', () {
    test('the cadence decision is a pure function of activity', () {
      expect(eventPollMs(hasActivity: true), 50);
      expect(eventPollMs(hasActivity: false), 1000);
      expect(opPollMs(hasActivity: true), 200);
      expect(opPollMs(hasActivity: false), 1000);
      // The constructor defaults ARE the pure function's values.
      final service = SimpleTransferService(calls: FakeAkTaskCalls());
      expect(service.eventPollInterval,
          Duration(milliseconds: eventPollMs(hasActivity: true)));
      expect(service.opPollInterval,
          Duration(milliseconds: opPollMs(hasActivity: true)));
      expect(service.idlePollInterval,
          Duration(milliseconds: eventPollMs(hasActivity: false)));
      service.dispose();
    });

    test('a stretched active interval stays inert at idle (manual-pump '
        'tests keep their contract)', () {
      final service = SimpleTransferService(
        calls: FakeAkTaskCalls(),
        eventPollInterval: const Duration(days: 1),
      );
      expect(service.effectiveEventPollInterval, const Duration(days: 1),
          reason: '空闲节奏不得快于配置的活动间隔（days:1 测试语义不变）');
      service.dispose();
    });

    testWidgets('idle polls at 1 s; activity flips to 50 ms immediately; '
        'everything quiet drops back immediately', (tester) async {
      final fake = FakeAkTaskCalls();
      final service = SimpleTransferService(calls: fake, delay: immediate);
      service.start(
          registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/cr2p5');
      final pollsAtStart =
          fake.pollEventCallCount; // start()'s own synchronous drain

      // IDLE: the 1 s cadence — 500 ms of virtual time must NOT tick.
      expect(service.hasActivity, isFalse);
      expect(service.effectiveEventPollInterval, const Duration(seconds: 1));
      expect(service.isEventPumpArmed, isTrue);
      await tester.pump(const Duration(milliseconds: 500));
      expect(fake.pollEventCallCount, pollsAtStart,
          reason: '空闲 1s 节奏：500ms 内零轮询');

      // IMMEDIATE-UP: an op entering flips the cadence NOW — the pending
      // 1 s tick is abandoned, not waited out.
      final codeFuture = service.createCode(SimpleTransferRole.sender);
      expect(service.hasActivity, isTrue, reason: '进行中的操作本身就是活动');
      expect(service.effectiveEventPollInterval,
          const Duration(milliseconds: 50),
          reason: '活动开始立即切快（不等慢拍）');
      await tester.pump(const Duration(milliseconds: 150));
      expect(fake.pollEventCallCount, greaterThan(pollsAtStart),
          reason: '快节奏已生效：150ms 内已有 tick');
      final created = await codeFuture;
      expect(service.hasActivity, isTrue,
          reason: 'paired（等待对端加入）仍是活动');

      // A live connection keeps the fast cadence…
      final joined = await service.join(created.code);
      service.pumpEvents(); // the probe flips the row to connected
      expect(service.currentConnections.first.state, 'connected');
      expect(service.effectiveEventPollInterval,
          const Duration(milliseconds: 50));

      // …and everything going quiet drops back IMMEDIATELY.
      await service.disconnect(joined.connId);
      service.pumpEvents(); // fold the disconnected event
      expect(service.hasActivity, isFalse);
      expect(service.effectiveEventPollInterval, const Duration(seconds: 1),
          reason: '一切安静后立即降回慢节奏');
      final pollsAtQuiet = fake.pollEventCallCount;
      await tester.pump(const Duration(milliseconds: 500));
      expect(fake.pollEventCallCount, pollsAtQuiet,
          reason: '降回后 500ms 内零 tick（旧快拍已被替换，而非等它到期）');
      service.dispose();
    });

    testWidgets('backgrounded + idle: the pump pauses entirely; resume '
        'refreshes immediately and re-arms', (tester) async {
      final fake = FakeAkTaskCalls();
      final service = SimpleTransferService(calls: fake, delay: immediate);
      service.start(
          registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/cr2p5b');
      service.setAppBackgrounded(true);
      expect(service.isEventPumpArmed, isFalse,
          reason: '后台且无任务：泵完全暂停（零唤醒）');
      final pollsAtPause = fake.pollEventCallCount;
      await tester.pump(const Duration(seconds: 5));
      expect(fake.pollEventCallCount, pollsAtPause, reason: '暂停期间零轮询');

      // An event that queued while hidden stays queued…
      final runtimeId = fake.runtimes.keys.first;
      fake.runtimes[runtimeId]!.events.add(<String, dynamic>{
        'event': 'runtime_ready',
        'runtime': runtimeId,
      });
      await tester.pump(const Duration(seconds: 5));
      expect(fake.runtimes[runtimeId]!.events, hasLength(1),
          reason: '暂停期间不得 drain 隐藏期事件');

      // …and is drained the MOMENT the app foregrounds.
      service.setAppBackgrounded(false);
      expect(fake.runtimes[runtimeId]!.events, isEmpty,
          reason: '回前台立即刷新：排空隐藏期间排队的事件');
      expect(fake.pollEventCallCount, greaterThan(pollsAtPause));
      expect(service.isEventPumpArmed, isTrue);
      expect(service.effectiveEventPollInterval, const Duration(seconds: 1),
          reason: '空闲前台：恢复正常空闲节奏');
      service.dispose();
    });

    testWidgets('backgrounded + active: a 1 s heartbeat survives; resume '
        'restores 50 ms immediately', (tester) async {
      final fake = FakeAkTaskCalls();
      final service = SimpleTransferService(calls: fake, delay: immediate);
      service.start(
          registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/cr2p5c');
      final created = await service.createCode(SimpleTransferRole.receiver);
      expect(created.code.startsWith('AKI1-'), isTrue);
      expect(service.effectiveEventPollInterval,
          const Duration(milliseconds: 50));

      service.setAppBackgrounded(true);
      expect(service.effectiveEventPollInterval, const Duration(seconds: 1),
          reason: '后台降频（仍有任务：保留 1s 心跳）');
      expect(service.isEventPumpArmed, isTrue, reason: '有活动任务时不完全暂停');
      final pollsAtHidden = fake.pollEventCallCount;
      await tester.pump(const Duration(milliseconds: 900));
      expect(fake.pollEventCallCount, pollsAtHidden,
          reason: '1s 节奏：900ms 内零 tick');
      await tester.pump(const Duration(milliseconds: 200));
      expect(fake.pollEventCallCount, greaterThan(pollsAtHidden),
          reason: '1s 心跳仍在跳');
      service.setAppBackgrounded(false);
      expect(service.effectiveEventPollInterval,
          const Duration(milliseconds: 50),
          reason: '回前台立即恢复快节奏');
      service.dispose();
    });

    // Real timers (not the fake clock — the interleave needs a timer that
    // FIRED while its callback was still queued): every halt of a fired
    // tick must retire its chain, never fork a second one. A mishandled
    // late callback would clobber the successor's handle, duplicate the
    // chain and storm the engine with ticks.
    test('halting a fired-but-unrun tick never forks a second chain '
        '(bounded steady-state rate)', () async {
      final fake = FakeAkTaskCalls();
      final service = SimpleTransferService(calls: fake, delay: immediate);
      service.start(
          registry: '127.0.0.1:7200', registryIdHex: regId, idDir: 'C:/ak/cr2p5d');
      await service.createCode(SimpleTransferRole.receiver);
      expect(service.hasActivity, isTrue); // paired — stable fast cadence

      // Flap the background lever while ticks fire: every flip halts and
      // re-arms the chain, sometimes AFTER a timer fired but BEFORE its
      // callback ran — the exact raced interleave.
      for (var i = 0; i < 30; i++) {
        service.setAppBackgrounded(i.isEven);
        await Future<void>.delayed(const Duration(milliseconds: 7));
      }
      service.setAppBackgrounded(false);

      final before = fake.pollEventCallCount;
      await Future<void>.delayed(const Duration(milliseconds: 500));
      final ticks = fake.pollEventCallCount - before;
      expect(ticks, lessThanOrEqualTo(15),
          reason: '500ms 稳态窗口内 tick 数受 50ms 节奏约束（10 拍＋粒度余量）；'
              '双链风暴会数倍超标');
      service.dispose();
    });
  });
}
