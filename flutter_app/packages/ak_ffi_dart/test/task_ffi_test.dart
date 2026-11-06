/// Wave-28 task-plane bindings tests: spec builders, the pointer-free
/// seam driven by [FakeAkTaskCalls], the friendly layer's `-3` grow loops,
/// the typed event model, and the last-error detail plumbing. Everything
/// runs WITHOUT the DLL (the same layering discipline as
/// `pairing_ffi_test.dart`).
library;

import 'dart:async' show Timer, TimeoutException;

import 'package:ak_ffi_dart/ak_ffi_dart.dart';
import 'package:test/test.dart';

const String regId64 =
    '1111111111111111111111111111111111111111111111111111111111111111';
const String rid = 'a1b2c3d4e5f60718293a4b5c6d7e8f90';

void main() {
  group('AkTaskStartSpec', () {
    test('renders the engine shape', () {
      final spec = AkTaskStartSpec(
        registry: '127.0.0.1:7200',
        registryIdHex: regId64.toUpperCase(),
        idDir: 'C:/ak/id',
        deviceName: 'phone',
        saveDir: 'D:/inbox',
      );
      final json = spec.toJson();
      expect(json['registry'], '127.0.0.1:7200');
      expect(json['registry_id'], regId64);
      expect(json['id_dir'], 'C:/ak/id');
      expect(json['device_name'], 'phone');
      expect(json['save_dir'], 'D:/inbox');
      expect(spec.toJsonString(), contains('"spec_version":1'));
    });

    test('rejects bad shapes', () {
      expect(
          () => AkTaskStartSpec(
                  registry: 'ak.example:7200', registryIdHex: regId64, idDir: 'C:/ak')
              .toJson(),
          throwsA(isA<AkSpecException>()));
      expect(
          () => AkTaskStartSpec(
                  registry: '127.0.0.1:7200', registryIdHex: 'zz', idDir: 'C:/ak')
              .toJson(),
          throwsA(isA<AkSpecException>()));
      expect(
          () => AkTaskStartSpec(
                  registry: '127.0.0.1:7200', registryIdHex: regId64, idDir: '')
              .toJson(),
          throwsA(isA<AkSpecException>()));
    });
  });

  group('AkTaskCreateCodeSpec / AkTaskJoinSpec / AkTaskSendSpec', () {
    test('create renders role + candidates + ttl', () {
      final json = AkTaskCreateCodeSpec(
        role: AkTaskRole.receiver,
        candidates: const ['192.168.1.4:7000'],
        ttlSecs: 300,
        requestId: rid,
      ).toJson();
      expect(json['role'], 'receiver');
      expect(json['candidates'], ['192.168.1.4:7000']);
      expect(json['ttl_secs'], 300);
      expect(json['request_id'], rid);
    });

    test('create rejects unknown roles via type, bad ttl and candidates', () {
      expect(() => AkTaskCreateCodeSpec(role: AkTaskRole.sender, ttlSecs: 5).toJson(),
          throwsA(isA<AkSpecException>()));
      expect(
          () => AkTaskCreateCodeSpec(
                  role: AkTaskRole.sender, candidates: const ['nope'])
              .toJson(),
          throwsA(isA<AkSpecException>()));
    });

    test('join normalizes the code, renders the optional role and '
        'validates candidates', () {
      final json =
          AkTaskJoinSpec(code: ' aki1-abcd-1234 ').toJson();
      expect(json['code'], 'AKI1-ABCD-1234');
      expect(json.containsKey('role'), isFalse, reason: '无 role 提示时省略字段');

      final withRole = AkTaskJoinSpec(
        code: 'AKI1-ABCD-1234',
        role: AkTaskRole.sender,
      ).toJson();
      expect(withRole['role'], 'sender');

      final receiverRole = AkTaskJoinSpec(
        code: 'aki1abcd1234',
        role: AkTaskRole.receiver,
      ).toJson();
      expect(receiverRole['role'], 'receiver');
      expect(receiverRole['code'], 'AKI1ABCD1234');

      expect(() => AkTaskJoinSpec(code: '  ', candidates: const ['x']).toJson(),
          throwsA(isA<AkSpecException>()));
    });

    test('send requires non-empty paths', () {
      final json = AkTaskSendSpec(paths: const ['C:/a.txt', 'C:/b.txt'], requestId: rid).toJson();
      expect(json['paths'], ['C:/a.txt', 'C:/b.txt']);
      expect(() => AkTaskSendSpec(paths: const []).toJson(),
          throwsA(isA<AkSpecException>()));
      expect(() => AkTaskSendSpec(paths: const ['']).toJson(),
          throwsA(isA<AkSpecException>()));
    });
  });

  group('FakeAkTaskCalls + TaskFfi friendly layer', () {
    test('start → runtime_ready → createCode → join end to end', () async {
      final fake = FakeAkTaskCalls();
      final runtime = TaskFfi(calls: fake).start(AkTaskStartSpec(
        registry: '127.0.0.1:7200',
        registryIdHex: regId64,
        idDir: 'C:/ak/a',
      ));
      expect(fake.runtimes, contains(runtime.runtimeIdHex));

      final ready = runtime.pollEvent();
      expect(ready, isA<TaskRuntimeReadyEvent>());
      expect((ready as TaskRuntimeReadyEvent).runtime, runtime.runtimeIdHex);

      final created = await runtime
          .createCode(AkTaskCreateCodeSpec(role: AkTaskRole.sender))
          .wait(delay: immediateDelay);
      final code = created['code'] as String;
      // Real display shape (P1-02): AKI1- + 8 groups of 4 Crockford symbols.
      expect(RegExp(r'^AKI1-[0-9A-HJKMNP-TV-Z]{4}(-[0-9A-HJKMNP-TV-Z]{4}){7}$')
          .hasMatch(code), isTrue);
      expect(created['role'], 'sender');

      final settled = runtime.pollEvent();
      expect(settled, isA<TaskOpSettledEvent>());
      expect((settled as TaskOpSettledEvent).ok, isTrue);

      final joined = await runtime
          .join(AkTaskJoinSpec(code: code))
          .wait(delay: immediateDelay);
      expect(joined['op'], 'task_join');
      expect(joined['role'], 'receiver');
      final conn = joined['conn'] as String;
      expect(conn, hasLength(32));

      // connected + probe_result events on the joiner.
      final connected = runtime.pollEvent() as TaskConnectedEvent;
      expect(connected.conn, conn);
      expect(connected.role, 'receiver');
      final probe = runtime.pollEvent() as TaskProbeResultEvent;
      expect(probe.reachable, isTrue);
    });

    test('join role hint: the explicit user intent wins over the mirror '
        'guess (P1-05 seam)', () async {
      final fake = FakeAkTaskCalls();
      final task = TaskFfi(calls: fake);
      final owner = task.start(AkTaskStartSpec(
          registry: '127.0.0.1:7200', registryIdHex: regId64, idDir: 'C:/ak/i'));
      final joiner = task.start(AkTaskStartSpec(
          registry: '127.0.0.1:7200', registryIdHex: regId64, idDir: 'C:/ak/j'));
      final created = await owner
          .createCode(AkTaskCreateCodeSpec(role: AkTaskRole.receiver))
          .wait(delay: immediateDelay);
      // The mirror-complement of a receiver code would be "sender"; the
      // joiner EXPLICITLY declared receiver — the hint must win.
      final joined = await joiner
          .join(AkTaskJoinSpec(
              code: created['code'] as String, role: AkTaskRole.receiver))
          .wait(delay: immediateDelay);
      expect(joined['role'], 'receiver');
      // The wire spec carried the hint verbatim.
      expect(fake.acceptedSpecs.last, contains('"role":"receiver"'));
      // Without a hint the mirror complement still applies.
      final secondCode = await owner
          .createCode(AkTaskCreateCodeSpec(role: AkTaskRole.sender))
          .wait(delay: immediateDelay);
      final mirrored = await joiner
          .join(AkTaskJoinSpec(code: secondCode['code'] as String))
          .wait(delay: immediateDelay);
      expect(mirrored['role'], 'receiver');
    });

    test('list snapshot reflects connections and the save dir', () async {
      final fake = FakeAkTaskCalls();
      final runtime = TaskFfi(calls: fake).start(AkTaskStartSpec(
          registry: '127.0.0.1:7200', registryIdHex: regId64, idDir: 'C:/ak/b'));
      final created = await runtime
          .createCode(AkTaskCreateCodeSpec(role: AkTaskRole.sender))
          .wait(delay: immediateDelay);
      final joined = await runtime
          .join(AkTaskJoinSpec(code: created['code'] as String))
          .wait(delay: immediateDelay);
      final conn = joined['conn'] as String;

      runtime.setSaveDir('D:/phone-inbox');
      final snapshot = runtime.list();
      expect(snapshot.saveDir, 'D:/phone-inbox');
      expect(snapshot.connections, hasLength(1));
      expect(snapshot.connections.first.connId, conn);
      expect(snapshot.connections.first.role, 'receiver');
      expect(snapshot.connections.first.state, 'probing');
    });

    test('disconnect is idempotent and notifies both runtimes', () async {
      final fake = FakeAkTaskCalls();
      final task = TaskFfi(calls: fake);
      final owner = task.start(AkTaskStartSpec(
          registry: '127.0.0.1:7200', registryIdHex: regId64, idDir: 'C:/ak/c'));
      final joiner = task.start(AkTaskStartSpec(
          registry: '127.0.0.1:7200', registryIdHex: regId64, idDir: 'C:/ak/d'));
      final created = await owner
          .createCode(AkTaskCreateCodeSpec(role: AkTaskRole.receiver))
          .wait(delay: immediateDelay);
      final joined = await joiner
          .join(AkTaskJoinSpec(code: created['code'] as String))
          .wait(delay: immediateDelay);
      final conn = joined['conn'] as String;

      joiner.disconnect(conn);
      // Owner: connected … disconnected(peer); joiner: disconnected(local).
      final ownerEvent = drainUntil(owner, (e) => e is TaskDisconnectedEvent);
      expect((ownerEvent as TaskDisconnectedEvent).reason, 'peer');
      final joinerEvent = drainUntil(joiner, (e) => e is TaskDisconnectedEvent);
      expect((joinerEvent as TaskDisconnectedEvent).reason, 'local');

      // Idempotent: second disconnect stays AK_OK (no exception).
      joiner.disconnect(conn);
      // Unknown connection surfaces as AkFfiException(-4).
      expect(() => joiner.disconnect('ff' * 16),
          throwsA(isA<AkFfiException>()));
    });

    test('consent decides scripted incoming requests', () {
      final fake = FakeAkTaskCalls();
      final runtime = TaskFfi(calls: fake).start(AkTaskStartSpec(
          registry: '127.0.0.1:7200', registryIdHex: regId64, idDir: 'C:/ak/e'));
      expect(runtime.pollEvent(), isA<TaskRuntimeReadyEvent>()); // drain
      final job = fake.emitIncomingRequest(runtime.runtimeIdHex,
          rootName: 'photos', files: 3, bytes: 123456);
      final request = runtime.pollEvent() as TaskIncomingRequestEvent;
      expect(request.job, job);
      expect(request.rootName, 'photos');
      expect(request.files, 3);
      expect(request.bytes, 123456);

      expect(runtime.consent(job, accept: true), isTrue);
      // Already decided → false (the -4 mapping).
      expect(runtime.consent(job, accept: false), isFalse);
      expect(runtime.consent('00' * 16, accept: true), isFalse);
    });

    test('send fails from the receiver side with the engine wording',
        () async {
      final fake = FakeAkTaskCalls();
      final runtime = TaskFfi(calls: fake).start(AkTaskStartSpec(
          registry: '127.0.0.1:7200', registryIdHex: regId64, idDir: 'C:/ak/f'));
      final created = await runtime
          .createCode(AkTaskCreateCodeSpec(role: AkTaskRole.sender))
          .wait(delay: immediateDelay);
      final joined = await runtime
          .join(AkTaskJoinSpec(code: created['code'] as String))
          .wait(delay: immediateDelay);
      final conn = joined['conn'] as String;
      expect(joined['role'], 'receiver'); // this runtime is the RECEIVER
      // The receiver side cannot send: the op fails with the engine's
      // wording (the fake mirrors it).
      await expectLater(
        runtime.send(conn, AkTaskSendSpec(paths: const ['C:/x.bin'])).wait(delay: immediateDelay),
        throwsA(isA<AkFfiException>()),
      );
    });

    test('-3 rehearsal: list/op/event grow loops are lossless', () async {
      final fake = FakeAkTaskCalls();
      final runtime = TaskFfi(calls: fake).start(AkTaskStartSpec(
          registry: '127.0.0.1:7200', registryIdHex: regId64, idDir: 'C:/ak/g'));
      fake.forceBufferTooSmallOnce = true;
      final snapshot = runtime.list(); // first -3, then a lossless retry
      expect(snapshot.connections, isEmpty);

      final handle = runtime.createCode(AkTaskCreateCodeSpec(role: AkTaskRole.sender));
      fake.forceBufferTooSmallOnce = true;
      final result = handle.poll();
      expect(result.isDone, isTrue);

      fake.forceBufferTooSmallOnce = true;
      final event = runtime.pollEvent();
      expect(event, isA<TaskEvent>());
    });

    test('native last-error detail rides the AkFfiException', () {
      final fake = FakeAkTaskCalls();
      final runtime = TaskFfi(calls: fake).start(AkTaskStartSpec(
          registry: '127.0.0.1:7200', registryIdHex: regId64, idDir: 'C:/ak/h'));
      runtime.pollEvent(); // drain runtime_ready
      expect(
        () => runtime.disconnect('ab' * 16),
        throwsA(isA<AkFfiException>().having((e) => e.message, 'message',
            contains('fake:'))),
      );
    });

    test('start surfaces the fake validation failures', () {
      final fake = FakeAkTaskCalls();
      expect(
        () => TaskFfi(calls: fake).start(AkTaskStartSpec(
            registry: '127.0.0.1:7200', registryIdHex: regId64, idDir: '')),
        throwsA(isA<AkSpecException>()),
      );
    });
  });

  group('TaskEvent model', () {
    test('decodes every wire shape', () {
      final events = <TaskEvent>[
        TaskEvent.fromJson({'event': 'runtime_ready', 'runtime': 'ab' * 16}),
        TaskEvent.fromJson({
          'event': 'connected',
          'conn': 'cd' * 16,
          'role': 'sender',
          'peer_endpoint': '192.168.1.2:7100',
          'code': 'AKI1-AAAA-BBBB-CCCC-DDDD-EEEE-FFFF-GGGG-HHHH',
        }),
        TaskEvent.fromJson({
          'event': 'probe_result',
          'conn': 'cd' * 16,
          'endpoint': '192.168.1.2:7100',
          'reachable': null,
        }),
        TaskEvent.fromJson({
          'event': 'incoming_request',
          'job': 'ef' * 16,
          'conn': 'cd' * 16,
          'root': 'photos',
          'files': 2,
          'bytes': 99,
          'saveDir': 'D:/inbox',
        }),
        TaskEvent.fromJson({'event': 'done', 'conn': 'cd' * 16, 'files': 2, 'bytes': 99}),
        TaskEvent.fromJson({'event': 'disconnected', 'conn': 'cd' * 16, 'reason': 'peer'}),
        TaskEvent.fromJson({'event': 'disconnected', 'conn': 'cd' * 16, 'reason': 'done'}),
        TaskEvent.fromJson(
            {'event': 'op_failed', 'op_id': '12' * 16, 'op': 'task_join'}),
        TaskEvent.fromJson({'event': 'consent_expired', 'job': 'ef' * 16}),
      ];
      expect(events[0], isA<TaskRuntimeReadyEvent>());
      expect(events[1], isA<TaskConnectedEvent>());
      expect(events[2], isA<TaskProbeResultEvent>());
      expect(events[3], isA<TaskIncomingRequestEvent>());
      expect((events[3] as TaskIncomingRequestEvent).saveDir, 'D:/inbox');
      expect(events[4], isA<TaskDoneEvent>());
      expect(events[5], isA<TaskDisconnectedEvent>());
      expect((events[6] as TaskDisconnectedEvent).reason, 'done');
      expect(events[7], isA<TaskOpSettledEvent>());
      expect((events[7] as TaskOpSettledEvent).ok, isFalse);
      expect(events[8], isA<TaskConsentExpiredEvent>());
      expect((events[8] as TaskConsentExpiredEvent).job, 'ef' * 16);
    });

    test('unknown event names fall forward, broken payloads throw', () {
      final unknown =
          TaskEvent.fromJson({'event': 'future_thing', 'x': 1});
      expect(unknown, isA<TaskUnknownEvent>());
      expect(() => TaskEvent.fromJson({'event': 'connected'}),
          throwsA(isA<FormatException>()));
      expect(() => TaskEvent.fromJson({'event': 'done', 'files': 'many'}),
          throwsA(isA<FormatException>()));
      expect(() => TaskEvent.fromJson({'event': 'consent_expired'}),
          throwsA(isA<FormatException>()));
    });
  });

  // -------------------------------------------------------------------------
  // Wave-30: progress renewal (P2-10), code-id cancel (P2-04), native
  // consent expiry (P2-11), the request's saveDir snapshot (P2-06).
  // -------------------------------------------------------------------------

  group('wave-30: engine-wide progress queue (P2-10)', () {
    test('emitProgress feeds pollProgressLine; empty answers null; the -3 '
        'grow loop is lossless', () {
      final fake = FakeAkTaskCalls();
      final runtime =
          TaskFfi(calls: fake).start(AkTaskStartSpec(
              registry: '127.0.0.1:7200', registryIdHex: regId64, idDir: 'C:/ak/p1'));
      expect(runtime.pollProgressLine(), isNull, reason: 'empty queue → null');

      fake.emitProgress('cd' * 16);
      fake.forceBufferTooSmallOnce = true; // first poll -3, retry lossless
      final line = runtime.pollProgressLine();
      expect(line, isNotNull);
      expect(line, contains('"job":"cdcd'));
      expect(fake.progressLines, isEmpty, reason: 'the line was consumed');
      expect(runtime.pollProgressLine(), isNull);
    });

    test('wait(onPoll) renews the deadline — progress outlives the nominal '
        'timeout, no progress stalls out', () async {
      final fake = FakeAkTaskCalls(autoCompleteOps: false);
      final runtime =
          TaskFfi(calls: fake).start(AkTaskStartSpec(
              registry: '127.0.0.1:7200', registryIdHex: regId64, idDir: 'C:/ak/p2'));
      final handle = runtime
          .createCode(AkTaskCreateCodeSpec(role: AkTaskRole.sender));

      // "Progress" keeps flowing while the op stays pending: 400 ms with
      // a 120 ms budget — only renewal can carry the wait that far.
      final progress = Timer.periodic(const Duration(milliseconds: 20),
          (Timer _) => fake.emitProgress('cd' * 16));
      final waiting = handle.wait(
        interval: const Duration(milliseconds: 10),
        timeout: const Duration(milliseconds: 120),
        onPoll: () => runtime.pollProgressLine() != null,
      );
      await Future<void>.delayed(const Duration(milliseconds: 400));
      fake.pumpOps(runtime.runtimeIdHex); // settle: done
      final result = await waiting;
      progress.cancel();
      expect(result['op'], 'task_create_code');

      // Without progress the same budget is a stall deadline.
      final stalled = runtime
          .createCode(AkTaskCreateCodeSpec(role: AkTaskRole.sender));
      await expectLater(
        stalled.wait(
          interval: const Duration(milliseconds: 5),
          timeout: const Duration(milliseconds: 60),
        ),
        throwsA(isA<TimeoutException>()),
      );
    });
  });

  group('wave-30: code-id cancel (P2-04) and consent expiry (P2-11)', () {
    test('disconnect(codeId) revokes an un-joined code, emits '
        'code_cancelled, repeats silently, and the code is dead',
        () async {
      final fake = FakeAkTaskCalls();
      final runtime = TaskFfi(calls: fake).start(AkTaskStartSpec(
          registry: '127.0.0.1:7200', registryIdHex: regId64, idDir: 'C:/ak/c1'));
      runtime.pollEvent(); // drain runtime_ready
      final created = await runtime
          .createCode(AkTaskCreateCodeSpec(role: AkTaskRole.receiver))
          .wait(delay: immediateDelay);
      final codeId = created['code_id'] as String;
      expect(codeId, isNotEmpty);

      runtime.disconnect(codeId); // the revoke
      expect(fake.disconnectCalls, [codeId]);
      final cancelled =
          drainUntil(runtime, (event) => event.name == 'code_cancelled');
      expect(cancelled, isNotNull, reason: '撤码必须发 code_cancelled 事件');

      runtime.disconnect(codeId); // repeated cancel: silently OK
      final joiner = TaskFfi(calls: fake).start(AkTaskStartSpec(
          registry: '127.0.0.1:7200', registryIdHex: regId64, idDir: 'C:/ak/c2'));
      await expectLater(
        joiner
            .join(AkTaskJoinSpec(code: created['code'] as String))
            .wait(delay: immediateDelay),
        throwsA(isA<AkFfiException>()),
        reason: '被撤销的码不能再兑换',
      );
    });

    test('expireConsent mirrors the native window: pending removed, '
        'consent_expired emitted, decided jobs stay quiet', () {
      final fake = FakeAkTaskCalls();
      final runtime = TaskFfi(calls: fake).start(AkTaskStartSpec(
          registry: '127.0.0.1:7200', registryIdHex: regId64, idDir: 'C:/ak/x1'));
      expect(runtime.pollEvent(), isA<TaskRuntimeReadyEvent>()); // drain

      final job = fake.emitIncomingRequest(runtime.runtimeIdHex,
          connId: 'cd' * 16, files: 1, bytes: 1);
      final request = drainUntil(
          runtime, (event) => event is TaskIncomingRequestEvent);
      expect((request as TaskIncomingRequestEvent).saveDir, 'C:/ak/downloads',
          reason: '未脚本化时 saveDir 默认为运行时当前目录');

      fake.expireConsent(runtime.runtimeIdHex, job);
      final expired =
          drainUntil(runtime, (event) => event is TaskConsentExpiredEvent);
      expect((expired as TaskConsentExpiredEvent).job, job);
      expect(runtime.consent(job, accept: true), isFalse,
          reason: '窗口已自动拒绝：迟到决定为 -4');

      fake.expireConsent(runtime.runtimeIdHex, job); // decided: silent
      expect(runtime.pollEvent(), isNull,
          reason: '已决定的请求不再产生事件');
    });

    test('emitIncomingRequest carries a scripted saveDir (P2-06)', () {
      final fake = FakeAkTaskCalls();
      final runtime = TaskFfi(calls: fake).start(AkTaskStartSpec(
          registry: '127.0.0.1:7200', registryIdHex: regId64, idDir: 'C:/ak/x2'));
      runtime.pollEvent(); // drain runtime_ready
      fake.emitIncomingRequest(runtime.runtimeIdHex,
          files: 2, bytes: 9, saveDir: 'D:/offer-time');
      final event =
          drainUntil(runtime, (e) => e is TaskIncomingRequestEvent);
      expect((event as TaskIncomingRequestEvent).saveDir, 'D:/offer-time');
    });
  });
}

/// A zero-delay `Future.delayed` stand-in (the wait loops become
/// synchronous drains under the fake).
Future<void> immediateDelay(Duration d) async {}

/// Pops events until one matches [test], returning it (null when the queue
/// ran dry).
TaskEvent? drainUntil(TaskRuntimeFfi runtime, bool Function(TaskEvent) test) {
  while (true) {
    final event = runtime.pollEvent();
    if (event == null) {
      return null;
    }
    if (test(event)) {
      return event;
    }
  }
}
