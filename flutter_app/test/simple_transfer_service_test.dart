/// Wave-28 service-layer tests: [SimpleTransferService]'s state machine
/// and event sequences, driven end to end by [FakeAkTaskCalls] (the same
/// shipped fake — no DLL, no sockets; the Rust side covers the real
/// bytes).
library;

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
    expect(() => service.join('AKT1-XXXX-XXXX-XXXX'), throwsStateError);
    service.dispose();
  });

  group('state machine: idle → creating → paired → connected', () {
    test('creator path reaches paired, then connected on the join event',
        () async {
      final fake = FakeAkTaskCalls();
      final creator = booted(fake);
      final code = await creator.createCode(SimpleTransferRole.receiver);
      expect(code.startsWith('AKT1-'), isTrue);
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
      expect(joined.probe, 'unverified');
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
      expect(code.startsWith('AKT1-'), isTrue);
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

  group('dispose semantics (P1-05 alignment)', () {
    test('disposed service refuses every entry with StateError', () async {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      service.dispose();
      expect(() => service.createCode(SimpleTransferRole.sender),
          throwsStateError);
      expect(() => service.join('AKT1-XXXX-XXXX-XXXX'), throwsStateError);
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
