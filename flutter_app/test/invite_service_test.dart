import 'dart:convert' show jsonDecode, jsonEncode, utf8;

import 'dart:io' show Directory, File, Platform;

import 'dart:typed_data' show Uint8List;

import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show
        AkPairingCalls,
        AkSchedCalls,
        AkStatus,
        InviteState,
        PairingCompleteEvent,
        PairingEvent,
        InvitationRevokedEvent,
        RedeemPendingEvent,
        SchedCodeMode,
        SchedulerPolicy,
        ServiceOnlineEvent,
        akIdFromHex,
        akIdToHex;

import 'package:ak_sending_app/services/invite_service.dart'
    show FfiInviteService, InviteTtlKind, PairingStartConfig,
    SchedulerPreference;
import 'package:flutter_test/flutter_test.dart';

/// Wave-22 [FfiInviteService] tests over injected fake
/// `ak_pairing_*`/`ak_sched_*` call seams (pointer-free, no DLL — the
/// real-DLL round-trip is the integrator's e2e once B8 lands): lazy
/// runtime start (registry spec + runtime_ready gate + sched start with
/// the persisted policy), op polling until done, local redeem rejection,
/// the event pump stream, synthesized timeline events, policy application
/// via snapshot, and the scheduler-preference file round-trip.
void main() {
  group('FfiInviteService（Fake 调用缝）', () {
    test('首个调用前不启动运行时；启动等待 runtime_ready 后绑定调度器', () async {
      final fake = FakePairingCalls();
      final sched = FakeSchedCalls();
      final service = FfiInviteService(
        pairingCalls: fake,
        schedCalls: sched,
        identityDirProvider: () => r'C:\id',
      );
      addTearDown(service.dispose);
      expect(fake.startCalls, 0, reason: '构造后立即返回，不触碰 FFI');

      fake.queueOp(json: jsonEncode({
        'op': 'list',
        'next_cursor': '00' * 16,
        'entries': [
          {
            'code_id': 'ab' * 16,
            'state': 'active',
            'created_ms': 1730000000000,
            'expires_ms': 0,
            'revision': 1,
          }
        ]
      }));
      final rows = await service.listInvites();
      expect(fake.startCalls, 1);
      final startSpec = jsonDecode(fake.startSpecJson!) as Map<String, dynamic>;
      expect(startSpec['id_dir'], r'C:\id');
      expect(startSpec['registry'], PairingStartConfig.defaults.registryAddress);
      expect(fake.readinessEventsDrained, 1,
          reason: 'runtime_ready 在就绪门中被消费并送入事件流');
      // 调度器在就绪后立刻以持久化偏好启动。
      expect(sched.startSpecs, hasLength(1));
      final schedSpec =
          jsonDecode(sched.startSpecs.single) as Map<String, dynamic>;
      expect(schedSpec['runtime_id'], fake.startRuntimeIdHex);
      expect(schedSpec['same_code'], 'serial');
      expect(rows, hasLength(1));
      expect(rows.single.state, InviteState.active);
    });

    test('createInvite：shortLived 以 ttl_secs=3600 发送并解码一次性明文码',
        () async {
      final fake = FakePairingCalls();
      final service = FfiInviteService(
          pairingCalls: fake,
          schedCalls: FakeSchedCalls(),
          identityDirProvider: () => newTempIdDir());
      addTearDown(service.dispose);
      fake.queueOp(
        json: jsonEncode({
          'op': 'create',
          'code': 'aki10123456789abcdefghjkmnpqrstvwxyz',
          'hint': 'AKI1-0123-456…',
          'code_id': 'ab' * 16,
          'expires_ms': 1730003600000,
          'revision': 1,
        }),
        pendingPolls: 1,
      );
      final created = await service.createInvite(ttl: InviteTtlKind.shortLived);
      expect(created.invitationCode,
          'AKI1-0123-4567-89AB-CDEF-GHJK-MNPQ-RSTV-WXYZ');
      expect(created.hint, 'AKI1-0123-456…');
      expect(created.summary.expiresMs, 1730003600000);
      final createSpec =
          jsonDecode(fake.specsBySymbol['ak_pairing_create_start']!.single)
              as Map<String, dynamic>;
      expect(createSpec['ttl_secs'], 3600);
    });

    test('redeemInvite：畸形码在本地拒绝且不启动运行时；成功合成配对完成事件',
        () async {
      final fake = FakePairingCalls();
      final service = FfiInviteService(
          pairingCalls: fake,
          schedCalls: FakeSchedCalls(),
          identityDirProvider: () => newTempIdDir());
      addTearDown(service.dispose);

      await expectLater(
          service.redeemInvite('AKI1-0123'), throwsFormatException);
      expect(fake.startCalls, 0, reason: '本地拒绝不触达 FFI');

      fake.queueOp(json: jsonEncode({
        'op': 'redeem',
        'pairing_id': 'ef' * 16,
        'code_id': 'ab' * 16,
        'peer': 'cd' * 32,
        'peer_device': '99' * 32,
        'book': r'C:\id\peers.book',
      }));
      final received = <PairingEvent>[];
      final subscription = service.events.listen(received.add);
      addTearDown(subscription.cancel);
      await service.redeemInvite('aki10123456789abcdefghjkmnpqrstvwxyz');
      final redeemSpec =
          jsonDecode(fake.specsBySymbol['ak_pairing_redeem_start']!.single)
              as Map<String, dynamic>;
      expect(redeemSpec['invite_code'],
          'AKI1-0123-4567-89AB-CDEF-GHJK-MNPQ-RSTV-WXYZ');
      await Future<void>.delayed(Duration.zero);
      expect(
        received.whereType<PairingCompleteEvent>().single.peer,
        'cd' * 32,
        reason: '本波无 pairing-queue 线名：配对完成由 op 结果合成',
      );
    });

    test('revokeInvite 规范化 code_id 并合成撤销已同步事件', () async {
      final fake = FakePairingCalls();
      final service = FfiInviteService(
          pairingCalls: fake,
          schedCalls: FakeSchedCalls(),
          identityDirProvider: () => newTempIdDir());
      addTearDown(service.dispose);
      fake.queueOp(json: jsonEncode(
          {'op': 'revoke', 'code_id': 'ab' * 16, 'revision': 2,
           'revoked_ms': 1730000001000}));
      final received = <PairingEvent>[];
      final subscription = service.events.listen(received.add);
      addTearDown(subscription.cancel);
      await service.revokeInvite('AB' * 16);
      final revokeSpec =
          jsonDecode(fake.specsBySymbol['ak_pairing_revoke_start']!.single)
              as Map<String, dynamic>;
      expect(revokeSpec['code_id'], 'ab' * 16,
          reason: 'code_id 规范化为小写');
      await Future<void>.delayed(Duration.zero);
      expect(received.whereType<InvitationRevokedEvent>().single.synced, true);
    });

    test('runtime_failed 在就绪门中转为明确错误', () async {
      final fake = FakePairingCalls()
        ..failReadinessWith = 'tls handshake failed';
      final service = FfiInviteService(
          pairingCalls: fake,
          schedCalls: FakeSchedCalls(),
          identityDirProvider: () => newTempIdDir());
      addTearDown(service.dispose);
      fake.queueOp(json: '{"op":"list","next_cursor":"00","entries":[]}');
      await expectLater(service.listInvites(),
          throwsA(isA<StateError>().having(
              (e) => e.message, 'message', contains('tls handshake failed'))));
    });

    test('事件泵把 poll_event 的行按序送入广播流（-3 无损扩容）', () async {
      final fake = FakePairingCalls()..minBufferCap = 512;
      final service = FfiInviteService(
        pairingCalls: fake,
        schedCalls: FakeSchedCalls(),
        identityDirProvider: () => newTempIdDir(),
        pollInterval: const Duration(milliseconds: 5),
      );
      addTearDown(service.dispose);
      fake.queueOp(json: '{"op":"list","next_cursor":"","entries":[]}');
      final received = <PairingEvent>[];
      final subscription = service.events.listen(received.add);
      addTearDown(subscription.cancel);
      await service.listInvites();

      fake.eventQueue.addAll([
        {'event': 'op_done', 'op_id': '05' * 16, 'op': 'create'},
        {
          'event': 'redeem_offered',
          'code_id': 'ab' * 16,
          'pairing_id': 'ef' * 16,
          'peer': 'cd' * 32,
        },
      ]);
      await Future<void>.delayed(const Duration(milliseconds: 80));
      // 就绪门的 runtime_ready 也在流上（accepted ≠ online 的在线证据）。
      expect(received.first, isA<ServiceOnlineEvent>());
      final pumped =
          received.where((e) => e is! ServiceOnlineEvent).toList();
      expect(pumped.map((e) => e.name),
          ['op_done', 'redeem_pending'],
          reason: 'redeem_offered 线名解码为 RedeemPendingEvent（语义名）');
      expect(pumped.last, isA<RedeemPendingEvent>());
      expect((pumped.last as RedeemPendingEvent).peer, 'cd' * 32,
          reason: 'redeem_offered 解码为 RedeemPendingEvent（peer 保留）');
    });
  });

  group('调度策略与偏好持久化', () {
    test('applySchedulerPolicy：set_policy 同步应用并以快照读取生效策略',
        () async {
      final pairing = FakePairingCalls();
      final sched = FakeSchedCalls();
      final service = FfiInviteService(
        pairingCalls: pairing,
        schedCalls: sched,
        identityDirProvider: () => newTempIdDir(),
      );
      addTearDown(service.dispose);
      pairing.queueOp(json: '{"op":"list","next_cursor":"","entries":[]}');
      await service.listInvites(); // start the runtime

      sched.snapshotJson = jsonEncode({
        'policy': {
          'same_code': 'serial',
          'cross_code': 'serial',
          'max_active_tasks_per_code': 4,
          'max_active_code_groups': 4,
          'max_queued_tasks': 256,
        },
        'policy_revision': 2,
        'draining': true,
        'active_tasks': 2,
        'queued_tasks': 0,
        'shutting_down': false,
      });
      final effective = await service.applySchedulerPolicy(SchedulerPolicy(
          sameCodeMode: SchedCodeMode.parallel,
          crossCodeMode: SchedCodeMode.parallel));
      expect(effective.sameCodeMode, SchedCodeMode.serial,
          reason: '对端要求串行时快照中的生效策略为串行');
      expect(
        jsonDecode(sched.policySpecs.single),
        SchedulerPolicy(
                sameCodeMode: SchedCodeMode.parallel,
                crossCodeMode: SchedCodeMode.parallel)
            .toJson(),
        reason: '提交的策略 JSON 按引擎线键（same_code/cross_code）发送',
      );
    });

    test('偏好文件往返：scheduler_prefs.json 写入并读回；坏文件回退默认',
        () async {
      final dir = Directory.systemTemp.createTempSync('ak_invite_prefs_');
      addTearDown(() => dir.deleteSync(recursive: true));
      final prefsPath =
          '${dir.path}${Platform.pathSeparator}scheduler_prefs.json';
      final service = FfiInviteService(
        pairingCalls: FakePairingCalls(),
        schedCalls: FakeSchedCalls(),
        identityDirProvider: () => dir.path,
      );
      addTearDown(service.dispose);

      expect(await service.loadSchedulerPreference(),
          SchedulerPreference.defaults,
          reason: '未保存过 → 默认 serial × parallel');

      await service.saveSchedulerPreference(const SchedulerPreference(
          sameCodeMode: SchedCodeMode.parallel,
          crossCodeMode: SchedCodeMode.serial));
      final prefsFile = File(prefsPath);
      expect(prefsFile.existsSync(), isTrue);
      expect(jsonDecode(prefsFile.readAsStringSync()),
          {'same_code_mode': 'parallel', 'cross_code_mode': 'serial'});

      expect((await service.loadSchedulerPreference()).sameCodeMode,
          SchedCodeMode.parallel);
      final second = FfiInviteService(
        pairingCalls: FakePairingCalls(),
        schedCalls: FakeSchedCalls(),
        identityDirProvider: () => dir.path,
      );
      addTearDown(second.dispose);
      expect((await second.loadSchedulerPreference()).crossCodeMode,
          SchedCodeMode.serial);

      prefsFile.writeAsStringSync('{not json');
      final third = FfiInviteService(
        pairingCalls: FakePairingCalls(),
        schedCalls: FakeSchedCalls(),
        identityDirProvider: () => dir.path,
      );
      addTearDown(third.dispose);
      expect(
          await third.loadSchedulerPreference(), SchedulerPreference.defaults);

      final memory = FfiInviteService(
          pairingCalls: FakePairingCalls(),
          schedCalls: FakeSchedCalls(),
          identityDirProvider: () => newTempIdDir());
      addTearDown(memory.dispose);
      await memory.saveSchedulerPreference(const SchedulerPreference(
          sameCodeMode: SchedCodeMode.parallel,
          crossCodeMode: SchedCodeMode.serial));
      expect((await memory.loadSchedulerPreference()).sameCodeMode,
          SchedCodeMode.parallel);
    });
  });
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

/// Fresh throwaway identity dir per service (the engine REQUIRES id_dir).
String newTempIdDir() {
  final dir = Directory.systemTemp.createTempSync('ak_invite_id_');
  addTearDown(() => dir.deleteSync(recursive: true));
  return dir.path;
}

// ---------------------------------------------------------------------------
// Fakes (compact mirrors of the package-level fakes; -3 semantics kept).
// ---------------------------------------------------------------------------

/// One scripted op outcome: [pendingPolls] polls answer `AK_PENDING`,
/// then [code] (`1` done / `2` failed) delivers [json].
final class FakeOp {
  FakeOp({required this.code, required this.json, this.pendingPolls = 0});

  final int code;
  final String json;
  int pendingPolls;
}

/// Deterministic [AkPairingCalls]: sequential op ids, per-symbol spec
/// capture, lossless `-3` buffers. [queueOp] scripts the NEXT accepted
/// `*_start` op's result; [failReadinessWith] makes the readiness gate
/// see `runtime_failed` instead of `runtime_ready`.
final class FakePairingCalls implements AkPairingCalls {
  final Map<String, List<String>> specsBySymbol = {};
  final Map<String, FakeOp> opResults = {};
  final List<FakeOp> queuedOps = [];
  final List<Map<String, Object?>> eventQueue = [];

  int minBufferCap = 0;
  int startCalls = 0;
  String? startSpecJson;
  String? failReadinessWith;
  int readinessEventsDrained = 0;

  late final String startRuntimeIdHex = '10' * 16;
  late final Uint8List runtimeId = akIdFromHex(startRuntimeIdHex);
  int _seq = 1;

  void queueOp({required String json, int pendingPolls = 0}) {
    queuedOps.add(
        FakeOp(code: AkStatus.done, json: json, pendingPolls: pendingPolls));
  }

  @override
  int start(String specJson, Uint8List runtimeIdOut) {
    startCalls += 1;
    startSpecJson = specJson;
    runtimeIdOut.setAll(0, runtimeId);
    // The engine connects in the background; the readiness phase becomes
    // observable on the event queue.
    eventQueue.add(failReadinessWith == null
        ? {'event': 'runtime_ready', 'runtime': startRuntimeIdHex}
        : {
            'event': 'runtime_failed',
            'runtime': startRuntimeIdHex,
            'error': failReadinessWith,
          });
    return AkStatus.ok;
  }

  @override
  int createStart(
          Uint8List runtimeId, String specJson, Uint8List opIdOut) =>
      _accept('ak_pairing_create_start', specJson, opIdOut);

  @override
  int redeemStart(
          Uint8List runtimeId, String specJson, Uint8List opIdOut) =>
      _accept('ak_pairing_redeem_start', specJson, opIdOut);

  @override
  int listStart(Uint8List runtimeId, String queryJson, Uint8List opIdOut) =>
      _accept('ak_pairing_list_start', queryJson, opIdOut);

  @override
  int revokeStart(
          Uint8List runtimeId, String specJson, Uint8List opIdOut) =>
      _accept('ak_pairing_revoke_start', specJson, opIdOut);

  @override
  int stopStart(Uint8List runtimeId, Uint8List opIdOut) =>
      _accept('ak_pairing_stop_start', '{"spec_version":1}', opIdOut);

  int _accept(String symbol, String specJson, Uint8List opIdOut) {
    (specsBySymbol[symbol] ??= []).add(specJson);
    final hex = (_seq++).toRadixString(16).padLeft(32, '0');
    opIdOut.setAll(0, akIdFromHex(hex));
    opResults[hex] = queuedOps.isNotEmpty
        ? queuedOps.removeAt(0)
        : FakeOp(code: AkStatus.done, json: '{}');
    return AkStatus.ok;
  }

  @override
  int opResult(
      Uint8List runtimeId, Uint8List opId, Uint8List buffer, int cap) {
    final op = opResults[akIdToHex(opId)];
    if (op == null) {
      return AkStatus.errNotFound;
    }
    if (op.pendingPolls > 0) {
      op.pendingPolls -= 1;
      return AkStatus.pending;
    }
    if (cap < minBufferCap ||
        utf8.encode(op.json).length + 1 > cap) {
      return AkStatus.errBuffer; // nothing consumed: grow and re-read
    }
    _write(buffer, op.json);
    return op.code;
  }

  @override
  int pollEvent(Uint8List runtimeId, Uint8List buffer, int cap) {
    if (eventQueue.isEmpty) {
      return AkStatus.pending;
    }
    if (cap < minBufferCap ||
        utf8.encode(jsonEncode(eventQueue.first)).length + 1 > cap) {
      return AkStatus.errBuffer; // event stays queued
    }
    final event = eventQueue.first;
    _write(buffer, jsonEncode(eventQueue.removeAt(0)));
    // Count only readiness markers (the service gate polls them first).
    if (event['event'] == 'runtime_ready' ||
        event['event'] == 'runtime_failed') {
      readinessEventsDrained += 1;
    }
    return AkStatus.event;
  }

  @override
  int cancelOp(Uint8List runtimeId, Uint8List opId) => AkStatus.ok;

  void _write(Uint8List buffer, String payload) {
    final bytes = utf8.encode(payload);
    for (var i = 0; i < bytes.length; i++) {
      buffer[i] = bytes[i];
    }
    buffer[bytes.length] = 0;
  }
}

/// Deterministic [AkSchedCalls] (the six shipped engine calls).
final class FakeSchedCalls implements AkSchedCalls {
  final List<String> startSpecs = [];
  final List<String> enqueueSpecs = [];
  final List<String> policySpecs = [];
  final List<Map<String, Object?>> eventQueue =
      <Map<String, Object?>>[];

  String snapshotJson = '{}';

  @override
  int start(String specJson) {
    startSpecs.add(specJson);
    return AkStatus.ok;
  }

  @override
  int enqueue(String specJson, Uint8List taskIdOut) {
    enqueueSpecs.add(specJson);
    taskIdOut.setAll(0, akIdFromHex('11' * 16));
    return AkStatus.ok;
  }

  @override
  int setPolicy(String policyJson) {
    policySpecs.add(policyJson);
    return AkStatus.ok;
  }

  @override
  int cancel(Uint8List taskId) => AkStatus.ok;

  @override
  int snapshot(String queryJson, Uint8List buffer, int cap) {
    _write(buffer, snapshotJson);
    return AkStatus.event;
  }

  @override
  int pollEvent(Uint8List buffer, int cap) {
    if (eventQueue.isEmpty) {
      return AkStatus.pending;
    }
    _write(buffer, jsonEncode(eventQueue.removeAt(0)));
    return AkStatus.event;
  }

  void _write(Uint8List buffer, String payload) {
    final bytes = utf8.encode(payload);
    for (var i = 0; i < bytes.length; i++) {
      buffer[i] = bytes[i];
    }
    buffer[bytes.length] = 0;
  }
}
