/// W-N11 (batch X fix wave, 2026-11-23): the keep-awake gate covers the
/// two WAITING stages, not just active transfers — a created-but-unpaired
/// code and any live (non-terminal) connection hold the gate. The
/// reproduction: with the screen off during the「waiting for the peer」
/// stage the registry keeper died ~19 s in and the joiner saw
/// `unknown task code (... owner offline)` (X handoff §8, W-N11 row).
/// These tests pin the ORCHESTRATOR judgment ([transferActivity] edges —
/// the exact stream `ScreenKeepAwake` keys on; the channel half keeps its
/// own W7.3 tests).
///
/// P1-B (third review): an awaited code also EXPIRES engine-side at its
/// TTL — a code nobody joins stops being redeemable, so the gate must
/// release at the next activity evaluation without any engine event.
///
/// Fourth review: P2-H (a pairing landing across the deadline is NOT an
/// expiry — neither the connected-event drain nor the purge may announce
/// codeExpired for a just-paired code) and P2-I (the purge revokes the
/// UN-JOINED expired code engine-side: one best-effort disconnect of the
/// native listener that lives until cancel/teardown).
library;

import 'dart:async' show Completer, StreamSubscription;
import 'dart:convert' show jsonDecode, jsonEncode, utf8;
import 'dart:typed_data' show Uint8List;

import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show AkStatus, AkTaskCalls, FakeAkTaskCalls;
import 'package:flutter_test/flutter_test.dart';
import 'package:ak_sending_app/services/simple_transfer_service.dart';

const String regId =
    '1111111111111111111111111111111111111111111111111111111111111111';

Future<void> immediate(Duration d) async {}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  SimpleTransferService booted(AkTaskCalls calls) {
    final service = SimpleTransferService(
      calls: calls,
      delay: immediate,
      eventPollInterval: const Duration(days: 1), // tests pump manually
    );
    service.start(
      registry: '127.0.0.1:7200',
      registryIdHex: regId,
      idDir: 'C:/ak/wn11-test',
    );
    return service;
  }

  /// The activity state as a fresh consumer sees it: the stream's
  /// snapshot (Stream.multi replays the current value to every listener).
  Future<bool> gateHeld(SimpleTransferService service) async {
    final seen = Completer<bool>();
    late final StreamSubscription<bool> sub;
    sub = service.transferActivity.listen((v) {
      if (!seen.isCompleted) {
        seen.complete(v);
      }
    });
    final v = await seen.future.timeout(const Duration(seconds: 5));
    await sub.cancel();
    return v;
  }

  test(
      'W-N11: a created code WAITS with the gate held (waiting stage 1), '
      'and the gate opens BEFORE any connection exists', () async {
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    expect(await gateHeld(service), false, reason: 'idle start: gate open');

    final created = await service.createCode(SimpleTransferRole.sender);
    expect(created.codeId, isNotNull,
        reason: 'the fake mints a code_id (the wait keys on it)');
    expect(await gateHeld(service), true,
        reason: 'waiting for the peer to join holds the gate');
    service.dispose();
  });

  test(
      'W-N11: code_cancelled releases the gate (creator revoked the '
      'un-joined code)', () async {
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    final created = await service.createCode(SimpleTransferRole.sender);
    expect(await gateHeld(service), true);

    final runtimeId = fake.runtimes.values.first.id;
    fake.runtimes[runtimeId]!.events.add(<String, dynamic>{
      'event': 'code_cancelled',
      'code_id': created.codeId,
    });
    service.pumpEvents();
    expect(await gateHeld(service), false,
        reason: 'revoked code: no wait, no connection — gate released');
    service.dispose();
  });

  test(
      'W-N11: disconnect(codeId) releases the gate without any event '
      '(the op result is the deterministic moment)', () async {
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    final created = await service.createCode(SimpleTransferRole.sender);
    expect(await gateHeld(service), true);

    await service.disconnect(created.codeId!);
    expect(await gateHeld(service), false,
        reason: 'the revoke op itself ends the wait');
    service.dispose();
  });

  test(
      'W-N11: a PAIRED connection holds the gate through the '
      'pick-and-send wait (waiting stage 2), and the gate releases only '
      'when the last row goes terminal', () async {
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    final created = await service.createCode(SimpleTransferRole.sender);
    final joined = await service.join(created.code);
    service.pumpEvents();
    expect(service.currentConnections, hasLength(1));
    expect(service.currentConnections.first.state, 'connected',
        reason: 'the fixture must reach a live row first');
    expect(await gateHeld(service), true,
        reason: 'paired but no transfer yet: still a waiting stage');

    // The peer leaves — the row goes terminal, no other live fact exists.
    final runtimeId = fake.runtimes.values.first.id;
    final runtime = fake.runtimes[runtimeId]!;
    runtime.events.add(<String, dynamic>{
      'event': 'disconnected',
      'conn': joined.connId,
      'reason': 'peer',
    });
    runtime.connections.remove(joined.connId);
    service.pumpEvents();
    expect(
      service.currentConnections.every((row) => row.state == 'disconnected'),
      isTrue,
      reason: 'fixture: the row really went terminal',
    );
    expect(await gateHeld(service), false,
        reason: 'nothing waits, nothing transfers: gate released');
    service.dispose();
  });

  test(
      'P1-B: a create whose engine expiry (expires_ms) is already in the '
      'past NEVER holds the gate — the first activity evaluation purges '
      'the wait', () async {
    final fake = FakeAkTaskCalls();
    final service = booted(PastExpiryCreateCalls(
      fake,
      // A deadline far behind any test-run wall clock (epoch ms).
      DateTime.fromMillisecondsSinceEpoch(0)
          .add(const Duration(minutes: 1))
          .millisecondsSinceEpoch,
    ));
    final created = await service.createCode(SimpleTransferRole.sender);
    expect(created.codeId, isNotNull,
        reason: 'the fake mints a code_id (the wait keys on it)');
    expect(await gateHeld(service), false,
        reason: 'P1-B：已过期的等待码不再点亮屏幕（无需任何引擎事件）');
    service.dispose();
  });

  test(
      'P1-B: an awaited code without a usable expires_ms falls back to '
      'created-at + the engine\'s default 600 s TTL — the gate releases '
      'when the clock passes it, without any engine event', () async {
    var now = DateTime(2026, 11, 23, 12);
    final service = SimpleTransferService(
      calls: FakeAkTaskCalls(), // its create result carries expires_ms: 0
      delay: immediate,
      eventPollInterval: const Duration(days: 1), // tests pump manually
      clock: () => now,
    );
    service.start(
      registry: '127.0.0.1:7200',
      registryIdHex: regId,
      idDir: 'C:/ak/wn11-test',
    );
    final created = await service.createCode(SimpleTransferRole.sender);
    expect(created.codeId, isNotNull);
    expect(await gateHeld(service), true,
        reason: 'the young code\'s wait holds the gate');

    // Nothing happens engine-side — no join, no event, no cancel. The
    // clock alone passes the fallback deadline; the next activity
    // evaluation (the pump's cadence re-decision runs through the same
    // purge) releases the gate.
    now = now.add(const Duration(seconds: 601));
    service.setAppBackgrounded(true);
    expect(await gateHeld(service), false,
        reason: 'P1-B：TTL 已过——无事件也要释放 keep-awake 门');
    service.dispose();
  });

  test(
      'P1-E: the engine-failed latch releases the gate held by an awaited '
      'code (the dead pump can never run the TTL purge)', () async {
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    final created = await service.createCode(SimpleTransferRole.sender);
    expect(created.codeId, isNotNull);
    expect(await gateHeld(service), true,
        reason: '前置：等待对方加入的码点亮屏幕');

    // The terminal engine-failure latch (10 consecutive native poll
    // failures — the C12 mechanism).
    for (var i = 0; i < SimpleTransferService.pollFailureThreshold; i++) {
      fake.failPollEventCalls = 1;
      service.pumpEvents();
    }
    expect(service.isEngineFailed, isTrue, reason: '前置：引擎故障闩已闩上');

    // The pump is dead — the TTL purge inside it will never fire again,
    // so the LATCH itself must release the gate (P1-E).
    expect(await gateHeld(service), false,
        reason: 'P1-E：引擎已故障——任何等待不得再点亮屏幕');
    service.dispose();
  });

  test(
      'P1-E: the engine-failed latch marks every live row terminal and '
      'releases the gate (honest: the engine is gone)', () async {
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    final created = await service.createCode(SimpleTransferRole.sender);
    await service.join(created.code);
    service.pumpEvents();
    expect(service.currentConnections.first.state, 'connected',
        reason: '前置：先有一条存活连接');
    expect(await gateHeld(service), true,
        reason: '前置：配对等待阶段门保持');

    for (var i = 0; i < SimpleTransferService.pollFailureThreshold; i++) {
      fake.failPollEventCalls = 1;
      service.pumpEvents();
    }
    expect(service.isEngineFailed, isTrue);

    expect(
      service.currentConnections
          .every((row) => row.state == 'disconnected'),
      isTrue,
      reason: 'P1-E：引擎已亡——存活行必须全部终结，不得永久点亮屏幕',
    );
    expect(await gateHeld(service), false,
        reason: 'P1-E：无等待、无存活行——门立即释放');
    service.dispose();
  });

  test(
      'P2-E: the TTL purge announces the expired code on codeExpired and '
      'settles the phase to idle when nothing else is live', () async {
    var now = DateTime(2026, 11, 23, 12);
    final service = SimpleTransferService(
      calls: FakeAkTaskCalls(), // create results carry expires_ms: 0
      delay: immediate,
      eventPollInterval: const Duration(days: 1), // tests pump manually
      clock: () => now,
    );
    service.start(
      registry: '127.0.0.1:7200',
      registryIdHex: regId,
      idDir: 'C:/ak/wn11-test',
    );
    final created = await service.createCode(SimpleTransferRole.sender);
    expect(created.codeId, isNotNull);
    expect(service.phase, SimpleTransferPhase.paired,
        reason: '前置：生码后服务停在 paired（过去会一直停在这里）');

    final expired = <String>[];
    final sub = service.codeExpired.listen(expired.add);

    // The clock alone passes the fallback TTL; the next evaluation
    // purges the wait — the purge must ANNOUNCE the code id and settle
    // the phase (the home card ends on the same fact).
    now = now.add(const Duration(seconds: 601));
    service.setAppBackgrounded(true);
    await pumpEventQueue();

    expect(expired, [created.codeId],
        reason: 'P2-E：TTL 清除必须在 codeExpired 上广播被清除的 code_id');
    expect(service.phase, SimpleTransferPhase.idle,
        reason: 'P2-E：无其它活动事实时相位必须回到 idle');
    expect(await gateHeld(service), false,
        reason: 'P2-E：过期码不再点亮屏幕（门已释放）');
    await sub.cancel();
    service.dispose();
  });

  test(
    'P2-H: a connected event queued before the deadline but drained after '
    'it pairs NORMALLY — no codeExpired, the row lives',
    () async {
      var now = DateTime(2026, 11, 23, 12);
      final fake = FakeAkTaskCalls();
      final service = SimpleTransferService(
        calls: fake, // create results carry expires_ms: 0 → fallback TTL
        delay: immediate,
        eventPollInterval: const Duration(days: 1), // tests pump manually
        clock: () => now,
      );
      service.start(
        registry: '127.0.0.1:7200',
        registryIdHex: regId,
        idDir: 'C:/ak/wn11-test',
      );
      final created = await service.createCode(SimpleTransferRole.sender);
      expect(created.codeId, isNotNull);

      final expired = <String>[];
      final sub = service.codeExpired.listen(expired.add);

      // The peer joined while the code was live; the events sit in the
      // native queue while the clock passes the TTL (the drain happens
      // after it — the expiry-vs-pairing interleave).
      final runtime = fake.runtimes.values.first;
      const conn = 'cccccccccccccccccccccccccccccccc';
      runtime.events.add(<String, dynamic>{
        'event': 'connected',
        'conn': conn,
        'role': 'sender',
        'peer_endpoint': '127.0.0.1:45678',
        'code': created.code.replaceAll('-', ''),
        'code_id': created.codeId,
      });
      runtime.events.add(<String, dynamic>{
        'event': 'probe_result',
        'conn': conn,
        'endpoint': '127.0.0.1:45678',
        'reachable': true,
      });
      now = now.add(const Duration(seconds: 601));
      service.pumpEvents();
      await pumpEventQueue();

      expect(expired, isEmpty,
          reason: 'P2-H：刚配对的码不得再触发 codeExpired（配对流程不得被重置）');
      expect(service.currentConnections, hasLength(1));
      expect(service.currentConnections.single.state, 'connected',
          reason: 'P2-H：配对照常完成（探测通过）');
      expect(service.phase, SimpleTransferPhase.connected,
          reason: 'P2-H：相位照常推进到 connected');
      await sub.cancel();
      service.dispose();
    },
  );

  test(
    'P2-H (belt and braces): an expired-deadline code whose pairing is '
    'LIVE is neither announced nor disconnected (an old engine\'s '
    'connected event without code_id)',
    () async {
      var now = DateTime(2026, 11, 23, 12);
      final fake = FakeAkTaskCalls();
      final service = SimpleTransferService(
        calls: fake,
        delay: immediate,
        eventPollInterval: const Duration(days: 1),
        clock: () => now,
      );
      service.start(
        registry: '127.0.0.1:7200',
        registryIdHex: regId,
        idDir: 'C:/ak/wn11-test',
      );
      final created = await service.createCode(SimpleTransferRole.sender);
      final expired = <String>[];
      final sub = service.codeExpired.listen(expired.add);

      // The engine announces the pairing WITHOUT the wave-31 code_id
      // field: the connected branch cannot end the wait itself — only
      // the purge's live-row correlation can.
      final runtime = fake.runtimes.values.first;
      const conn = 'dddddddddddddddddddddddddddddddd';
      runtime.events.add(<String, dynamic>{
        'event': 'connected',
        'conn': conn,
        'role': 'sender',
        'peer_endpoint': '127.0.0.1:45678',
        'code': created.code.replaceAll('-', ''),
        // NO code_id — the pre-wave-31 engine shape.
      });
      now = now.add(const Duration(seconds: 601)); // deadline passes
      service.pumpEvents();
      await pumpEventQueue();

      // The row is live and carries the code → the purge SKIPS the
      // pairing: no expiry announcement, no engine disconnect.
      expect(expired, isEmpty, reason: 'P2-H：已配对的码不广播 codeExpired');
      expect(fake.disconnectCalls, isNot(contains(created.codeId)),
          reason: 'P2-I：已配对的码不走 purge 撤码（连接自身的清理路径负责）');
      expect(service.currentConnections, hasLength(1));
      expect(await gateHeld(service), true,
          reason: '等待随配对静默结束——门由存活行保持');
      await sub.cancel();
      service.dispose();
    },
  );

  test(
    'P2-I: an expired UN-JOINED code is revoked engine-side on the purge — '
    'the runtime receives disconnect(codeId)',
    () async {
      var now = DateTime(2026, 11, 23, 12);
      final fake = FakeAkTaskCalls();
      final service = SimpleTransferService(
        calls: fake,
        delay: immediate,
        eventPollInterval: const Duration(days: 1),
        clock: () => now,
      );
      service.start(
        registry: '127.0.0.1:7200',
        registryIdHex: regId,
        idDir: 'C:/ak/wn11-test',
      );
      final created = await service.createCode(SimpleTransferRole.sender);
      expect(created.codeId, isNotNull);

      now = now.add(const Duration(seconds: 601));
      final expired = <String>[];
      final sub = service.codeExpired.listen(expired.add);
      // A lifecycle flip re-runs the purge (the same trigger the P2-E
      // test uses) — the Dart-side TTL expiry must also release the
      // native listener of the un-joined code.
      service.setAppBackgrounded(true);
      await pumpEventQueue();

      expect(fake.disconnectCalls, contains(created.codeId),
          reason: 'P2-I：TTL 清除时必须尽力撤销原生未配对监听（disconnect(code_id)）');
      expect(expired, [created.codeId],
          reason: 'P2-E 语义不变：未配对的过期码照常广播 codeExpired');
      await sub.cancel();
      service.dispose();
    },
  );
}

/// P1-B test seam: rewrites every `task_create_code` op result of the
/// wrapped fake so the minted code carries an `expires_ms` the test
/// controls (the engine's epoch-ms deadline fact; the stock fake sends 0).
/// Everything else — statuses, other op results, the buffer contract —
/// passes through byte-for-byte.
class PastExpiryCreateCalls implements AkTaskCalls {
  PastExpiryCreateCalls(this._inner, this.expiresMs);

  final AkTaskCalls _inner;

  /// The epoch-ms `expires_ms` the create results are rewritten to.
  final int expiresMs;

  @override
  int opResult(
      Uint8List runtimeId, Uint8List opId, Uint8List buffer, int cap) {
    // Ask the inner fake into a big scratch buffer first (the -3
    // grow-retry contract stays the inner fake's), then rewrite.
    const scratchCap = 1 << 20;
    final scratch = Uint8List(scratchCap);
    final code = _inner.opResult(runtimeId, opId, scratch, scratchCap);
    if (code != AkStatus.done && code != AkStatus.failed) {
      return code; // pending: nothing to read yet
    }
    var text =
        utf8.decode(scratch.takeWhile((byte) => byte != 0).toList());
    try {
      final decoded = jsonDecode(text);
      if (decoded is Map<String, dynamic> &&
          decoded['op'] == 'task_create_code') {
        decoded['expires_ms'] = expiresMs;
        text = jsonEncode(decoded);
      }
    } on Object {
      // Not JSON: deliver the raw payload unchanged below.
    }
    final payload = utf8.encode(text);
    if (payload.length + 1 > cap) {
      return AkStatus.errBuffer;
    }
    buffer.setAll(0, payload);
    buffer[payload.length] = 0;
    return code;
  }

  @override
  int list(Uint8List runtimeId, Uint8List buffer, int cap) =>
      _inner.list(runtimeId, buffer, cap);

  @override
  String? lastError() => _inner.lastError();

  @override
  int start(String specJson, Uint8List runtimeIdOut) =>
      _inner.start(specJson, runtimeIdOut);

  @override
  int createCode(Uint8List runtimeId, String specJson, Uint8List opIdOut) =>
      _inner.createCode(runtimeId, specJson, opIdOut);

  @override
  int join(Uint8List runtimeId, String specJson, Uint8List opIdOut) =>
      _inner.join(runtimeId, specJson, opIdOut);

  @override
  int send(Uint8List runtimeId, Uint8List connId, String pathsJson,
          Uint8List opIdOut) =>
      _inner.send(runtimeId, connId, pathsJson, opIdOut);

  @override
  int disconnect(Uint8List runtimeId, Uint8List connId) =>
      _inner.disconnect(runtimeId, connId);

  @override
  int setSaveDir(Uint8List runtimeId, String path) =>
      _inner.setSaveDir(runtimeId, path);

  @override
  int consent(Uint8List runtimeId, Uint8List jobId, int accept) =>
      _inner.consent(runtimeId, jobId, accept);

  @override
  int serverInfo(Uint8List runtimeId, Uint8List opIdOut) =>
      _inner.serverInfo(runtimeId, opIdOut);

  @override
  int pollEvent(Uint8List runtimeId, Uint8List buffer, int cap) =>
      _inner.pollEvent(runtimeId, buffer, cap);

  @override
  int pollProgress(Uint8List buffer, int cap) =>
      _inner.pollProgress(buffer, cap);
}
