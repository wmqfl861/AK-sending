/// Wave-28 bridge tests: [FfiSimpleTransferService] (the real service
/// behind agent D's frozen UI contract) driven by [FakeAkTaskCalls] —
/// the reconciliation mappings the adapter owns (role/state/code shapes,
/// consent completion semantics, registry config resolution).
library;

import 'dart:async' show Completer, Timer, TimeoutException;
import 'dart:convert' show jsonDecode, jsonEncode, utf8;
import 'dart:typed_data' show Uint8List;


import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show AkStatus, AkTaskCalls, FakeAkTaskCalls;
import 'package:flutter/services.dart' show MethodChannel;
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
    // X5 §4.5 item 5: a nameless (old-engine) row shows the mandated
    // default 「对方的设备」, never a code-tail label.
    expect(rows.first.peerName, '对方的设备');

    await service.disconnect(joined.connectionId!);
    service.engine.pumpEvents();
    final after = await service.connections.first;
    expect(after, isEmpty); // dropped rows leave the snapshot
    service.dispose();
  });

  test('X5: the peer identity rides the connection row and the incoming '
      'request (self-reported name + verified-fp short code + root label)',
      () async {
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    final code = (await service.createCode(TaskRole.send)).code;
    final JoinResult joined = await service.joinCode(code);
    service.engine.pumpEvents();
    final runtimeId = fake.runtimes.keys.first;

    // An X5 engine's `connected` event carrying the identity fields (the
    // fake's own join events model the PRE-X5 shape — that tolerance is
    // the rows test above).
    fake.runtimes[runtimeId]!.events.add(<String, dynamic>{
      'event': 'connected',
      'conn': joined.connectionId,
      'role': 'receiver',
      'peer_name': 'phone-beta',
      'peer_fp_short': 'a1b2c3d4',
    });
    // And an X5 `incoming_request` carrying the sender's self-reported
    // name (`peer`) plus the payload's root label.
    final requests = <IncomingRequest>[];
    service.incomingRequests.listen(requests.add);
    fake.runtimes[runtimeId]!.events.add(<String, dynamic>{
      'event': 'incoming_request',
      'job': 'ab' * 16,
      'conn': joined.connectionId,
      'root': 'photos',
      'files': 3,
      'bytes': 123456,
      'saveDir': 'C:/ak/bridge/downloads',
      'windowSecs': 120,
      'peer': 'phone-beta',
    });
    service.engine.pumpEvents();
    await pumpEventQueue();

    final rows = await service.connections.first;
    expect(rows.first.peerName, 'phone-beta',
        reason: '引擎自报名优先于码尾标签');
    expect(rows.first.peerShortId, 'a1b2c3d4');
    expect(requests.single.rootName, 'photos',
        reason: 'X5 修复：映射不再丢 rootName');
    expect(requests.single.peerName, 'phone-beta');
    service.dispose();
  });

  test('X5 §4.5-5: a nameless (old-engine) row renders the mandated '
      'default 「对方的设备」— never a code-tail label', () async {
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    final code = (await service.createCode(TaskRole.send)).code;
    await service.joinCode(code);
    service.engine.pumpEvents(); // the fake's events carry NO peer name

    final rows = await service.connections.first;
    expect(rows.first.peerName, '对方的设备');
    service.dispose();
  });

  test('P1-3 (codex X fix): start(deviceName:) forwards the self-reported '
      'name into the ak_task_start spec', () async {
    final fake = FakeAkTaskCalls();
    final service = FfiSimpleTransferService(
      calls: fake,
      registry: '127.0.0.1:7200',
      registryIdHex: regId,
      idDir: 'C:/ak/bridge',
      delay: immediate,
    );
    await service.start(deviceName: 'desk-alpha');
    expect(
        fake.acceptedSpecs.single, contains('"device_name":"desk-alpha"'));
    service.dispose();
  });

  test('P1-3: a start() without a name resolves the platform default '
      '(the frozen contract call) and forwards it; a null resolution keeps '
      'the engine default (no device_name key)', () async {
    var sawAndroid = false;
    final namedFake = FakeAkTaskCalls();
    final named = FfiSimpleTransferService(
      calls: namedFake,
      registry: '127.0.0.1:7200',
      registryIdHex: regId,
      idDir: 'C:/ak/bridge',
      delay: immediate,
      deviceNameResolver: (isAndroid) async {
        sawAndroid = isAndroid;
        return 'resolved-host';
      },
    );
    await named.start();
    expect(sawAndroid, isFalse, reason: '桌面测试环境的平台分支');
    expect(namedFake.acceptedSpecs.single,
        contains('"device_name":"resolved-host"'));
    named.dispose();

    final unnamedFake = FakeAkTaskCalls();
    final unnamed = FfiSimpleTransferService(
      calls: unnamedFake,
      registry: '127.0.0.1:7200',
      registryIdHex: regId,
      idDir: 'C:/ak/bridge',
      delay: immediate,
      deviceNameResolver: (_) async => null,
    );
    await unnamed.start();
    expect(unnamedFake.acceptedSpecs.single, isNot(contains('device_name')),
        reason: '解析失败必须退回引擎默认名（ak-task），不得伪造');
    unnamed.dispose();
  });

  test('P1-3: Android resolution tolerates a missing channel method '
      '(the engine default stands)', () async {
    final fake = FakeAkTaskCalls();
    final service = FfiSimpleTransferService(
      calls: fake,
      registry: '127.0.0.1:7200',
      registryIdHex: regId,
      delay: immediate,
      isAndroid: true,
      androidIdentityDir: () async => '/data/user/0/dev.ak/.ak-sending',
      // The DEFAULT resolver: no mock handler on dev.ak/storage in tests —
      // MissingPluginException must degrade to null, never throw.
    );
    await service.start();
    expect(fake.acceptedSpecs.single, isNot(contains('device_name')));
    service.dispose();
  });

  test('P1-C: resolvePlatformDeviceName mirrors the engine\'s wire rules '
      '— trims, refuses empty/localhost, rejects the engine\'s control '
      'bytes, and caps at 64 UTF-8 BYTES on whole characters', () async {
    const channel = MethodChannel('dev.ak/storage');
    Future<String?> ask(Object? answer) async {
      TestWidgetsFlutterBinding.instance.defaultBinaryMessenger
          .setMockMethodCallHandler(channel, (call) async {
        expect(call.method, 'deviceName');
        return answer;
      });
      try {
        return await FfiSimpleTransferService.resolvePlatformDeviceName(
            true);
      } finally {
        TestWidgetsFlutterBinding.instance.defaultBinaryMessenger
            .setMockMethodCallHandler(channel, null);
      }
    }

    // Plain ASCII within the ceiling: trimmed, verbatim.
    expect(await ask('  Pixel 8 Pro '), 'Pixel 8 Pro');
    expect(await ask('X' * 64), 'X' * 64, reason: '恰 64 字节：原样保留');
    expect(await ask(''), isNull);
    expect(await ask('  '), isNull);

    // Control bytes the Rust check_display_name REJECTS (crates/
    // ak-session/src/pairing/client.rs): the name would fail the engine's
    // create AND join — it is refused entirely and the engine default
    // ('ak-task') stands.
    expect(await ask('a\u{7}b'), isNull, reason: 'BEL 是引擎拒绝的 C0 控制符');
    expect(await ask('ok\u{7F}'), isNull, reason: 'DEL（0x7F）被拒绝');
    expect(await ask('a\u{85}b'), isNull, reason: 'C1 的 UTF-8 含 0x85 字节');
    expect(await ask('\u{800}'), isNull,
        reason: 'U+0800 编码的尾字节 0x80 落在引擎拒绝的字节区间（逐字节同判）');

    // Over the ceiling: whole trailing characters drop (never a mid-rune
    // cut) until the UTF-8 form fits 64 bytes.
    final cjk = '设' * 40; // 40 × 3 = 120 bytes
    final cjkCut = await ask(cjk);
    expect(utf8.encode(cjkCut!).length, 63,
        reason: '21 × 3 = 63 字节 ≤ 64 字节上限');
    expect(cjkCut, '设' * 21, reason: '整字符截断，不切半个字符');
    expect(await ask('X' * 100), 'X' * 64,
        reason: '100 字节 ASCII 截到 64 字节');
  });

  testWidgets('P2-A: a device-name resolution that never completes cannot '
      'hang start() — the 3 s budget degrades to the engine default name',
      (tester) async {
    final fake = FakeAkTaskCalls();
    final service = FfiSimpleTransferService(
      calls: fake,
      registry: '127.0.0.1:7200',
      registryIdHex: regId,
      idDir: 'C:/ak/bridge',
      delay: immediate,
      isAndroid: true,
      androidIdentityDir: () async => '/data/user/0/dev.ak/.ak-sending',
      deviceNameResolver: (_) => Completer<String?>().future,
    );
    final starting = service.start();
    await tester.pump();
    expect(fake.acceptedSpecs, isEmpty,
        reason: '前置：start 停在永不完成的名字解析上');

    await tester.pump(FfiSimpleTransferService.deviceNameResolveTimeout +
        const Duration(milliseconds: 1));
    await starting;
    expect(fake.acceptedSpecs, hasLength(1), reason: 'P2-A：超时后 start 继续');
    expect(fake.acceptedSpecs.single, isNot(contains('device_name')),
        reason: '超时退回引擎默认名（ak-task），绝不悬挂 start');
    service.dispose();
  });

  test('P2-F: an EXPLICIT start(deviceName:) the engine would reject never '
      'reaches the engine — it degrades to null (the engine default), '
      'never truncates and never throws', () async {
    // 22 CJK chars = 66 UTF-8 bytes — over the registry client's
    // 64-byte ceiling (check_display_name); the engine rejects it on
    // create AND join, so it must never ride the start spec.
    final overFake = FakeAkTaskCalls();
    final over = FfiSimpleTransferService(
      calls: overFake,
      registry: '127.0.0.1:7200',
      registryIdHex: regId,
      idDir: 'C:/ak/bridge',
      delay: immediate,
    );
    await over.start(deviceName: '设' * 22);
    expect(overFake.acceptedSpecs.single, isNot(contains('device_name')),
        reason: 'P2-F：超长显式名字整体退回引擎默认名（不截断、不进 spec）');
    over.dispose();

    // Control characters: same refuse-to-null degrade.
    final ctrlFake = FakeAkTaskCalls();
    final ctrl = FfiSimpleTransferService(
      calls: ctrlFake,
      registry: '127.0.0.1:7200',
      registryIdHex: regId,
      idDir: 'C:/ak/bridge',
      delay: immediate,
    );
    await ctrl.start(deviceName: 'a\u{7}b');
    expect(ctrlFake.acceptedSpecs.single, isNot(contains('device_name')),
        reason: 'P2-F：控制字符显式名字同样退回引擎默认名');
    ctrl.dispose();
  });

  test('P2-F: an INJECTED resolver\'s return is validated in start() too '
      '(an over-long answer is capped by the byte-exact mirror, a '
      'control-char answer degrades to null)', () async {
    final truncFake = FakeAkTaskCalls();
    final trunc = FfiSimpleTransferService(
      calls: truncFake,
      registry: '127.0.0.1:7200',
      registryIdHex: regId,
      idDir: 'C:/ak/bridge',
      delay: immediate,
      deviceNameResolver: (_) async => '设' * 40, // 120 raw bytes
    );
    await trunc.start();
    final truncSpec =
        jsonDecode(truncFake.acceptedSpecs.single) as Map<String, dynamic>;
    final truncName = truncSpec['device_name'] as String?;
    expect(truncName, isNotNull, reason: '前置：合法（截断后）名字进了 spec');
    expect(utf8.encode(truncName!).length, 63,
        reason: 'P2-F：注入解析器的返回同样走 64 字节镜像（21×3=63）');
    trunc.dispose();

    final ctrlFake = FakeAkTaskCalls();
    final ctrl = FfiSimpleTransferService(
      calls: ctrlFake,
      registry: '127.0.0.1:7200',
      registryIdHex: regId,
      idDir: 'C:/ak/bridge',
      delay: immediate,
      deviceNameResolver: (_) async => 'ok\u{7F}',
    );
    await ctrl.start();
    expect(ctrlFake.acceptedSpecs.single, isNot(contains('device_name')),
        reason: 'P2-F：注入解析器返回的控制字符必须退回引擎默认名');
    ctrl.dispose();
  });

  test('P1-3: the resume reconcile fills peerName/peerShortId from the '
      'engine snapshot (a nameless connected event heals on resume, and a '
      'null never clobbers a learned name)', () async {
    final fake = FakeAkTaskCalls();
    final calls = PeerFactsListCalls(fake);
    final service = FfiSimpleTransferService(
      calls: calls,
      registry: '127.0.0.1:7200',
      registryIdHex: regId,
      idDir: 'C:/ak/bridge',
      delay: immediate,
      deviceNameResolver: (_) async => null,
    );
    await service.start();
    final code = (await service.createCode(TaskRole.send)).code;
    await service.joinCode(code);
    service.engine.pumpEvents(); // connected + probe land WITHOUT names

    var rows = await service.connections.first;
    expect(rows.first.peerName, '对方的设备',
        reason: '前置：恢复前引擎事件确实没带名字');
    expect(rows.first.peerShortId, isNull);

    // Resume (backgrounded → foregrounded): the reconcile re-reads the
    // engine's ak_task_list snapshot, whose rows carry the X5 facts.
    service.engine.setAppBackgrounded(true);
    service.engine.setAppBackgrounded(false);
    await pumpEventQueue();
    rows = await service.connections.first;
    expect(rows.first.peerName, 'phone-beta',
        reason: '恢复快照必须把 peer_name 刷新到本地行');
    expect(rows.first.peerShortId, 'a1b2c3d4');

    // Engine truth may not go BACKWARDS: a null snapshot fact never
    // clobbers the learned name.
    calls.injectPeerFacts = false;
    service.engine.setAppBackgrounded(true);
    service.engine.setAppBackgrounded(false);
    await pumpEventQueue();
    rows = await service.connections.first;
    expect(rows.first.peerName, 'phone-beta');
    expect(rows.first.peerShortId, 'a1b2c3d4');
    service.dispose();
  });

  test('P1-3: UI TaskConnection rows carry the pairing code end-to-end '
      '(the adapter stamps the engine event\'s normalized code fact)',
      () async {
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    final code = (await service.createCode(TaskRole.send)).code;
    await service.joinCode(code);
    service.engine.pumpEvents();

    final rows = await service.connections.first;
    expect(rows.first.code, code.replaceAll('-', ''),
        reason: 'connected 事件的 code（归一化键）要落到 UI 行上');
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

  test('joinCode forwards the explicit user role; a same-direction join '
      'is refused pre-commit and the code still pairs (P1-05 seam, X1/W-N7)',
      () async {
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    // The creator declared RECEIVER; the joiner EXPLICITLY declares
    // receiver too (their intent) — the hint must reach the spec...
    final code = (await service.createCode(TaskRole.receive)).code;
    final refused = await service.joinCode(code, role: TaskRole.receive);
    // ...but the engine vetoes a same-direction redeem BEFORE the
    // commit with its unified unavailable answer — nothing is booked
    // and the single redeem survives
    // (production_owner_loop_refuses_same_direction_joins_before_commit,
    // crates/ak-ffi/src/task/task_tests.rs:2913; the fake mirrors it).
    expect(refused.ok, isFalse);
    expect(refused.error,
        'unknown task code (or expired, revoked, owner offline)');
    // The wire spec still carried the hint verbatim.
    expect(fake.acceptedSpecs.last, contains('"role":"receiver"'));
    // No connection was booked by the refusal.
    service.engine.pumpEvents();
    await pumpEventQueue();
    expect(await service.connections.first, isEmpty);
    // The complementary role still redeems the SAME code.
    final ok = await service.joinCode(code, role: TaskRole.send);
    expect(ok.ok, isTrue);
    expect(ok.connectionId, isNotNull);
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

/// P1-3 test seam: delegates every call to the wrapped fake, but injects
/// the X5 peer-identity fields (`peer_name`/`peer_fp_short`) into the
/// `ak_task_list` snapshot rows — the resume reconcile's engine truth
/// (the low-level fake models the PRE-X5 snapshot shape). Toggling
/// [injectPeerFacts] off proves a null fact never clobbers a learned
/// name.
class PeerFactsListCalls implements AkTaskCalls {
  PeerFactsListCalls(this._inner);

  final AkTaskCalls _inner;

  /// Whether list() rows gain [peerName]/[peerFpShort].
  bool injectPeerFacts = true;
  String peerName = 'phone-beta';
  String peerFpShort = 'a1b2c3d4';

  @override
  int list(Uint8List runtimeId, Uint8List buffer, int cap) {
    // Ask the inner fake into a big scratch buffer first (the -3
    // grow-retry contract stays the inner fake's), then rewrite.
    const scratchCap = 1 << 20;
    final scratch = Uint8List(scratchCap);
    final code = _inner.list(runtimeId, scratch, scratchCap);
    if (code != AkStatus.done) {
      return code; // -3 pass-through: the caller grows and retries
    }
    final text = utf8.decode(
        scratch.takeWhile((byte) => byte != 0).toList(growable: false));
    final snapshot = jsonDecode(text) as Map<String, dynamic>;
    if (injectPeerFacts) {
      for (final row in snapshot['connections'] as List) {
        (row as Map<String, dynamic>)['peer_name'] = peerName;
        row['peer_fp_short'] = peerFpShort;
      }
    }
    final payload = utf8.encode(jsonEncode(snapshot));
    if (payload.length + 1 > cap) {
      return AkStatus.errBuffer;
    }
    buffer.setAll(0, payload);
    buffer[payload.length] = 0;
    return AkStatus.done;
  }

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
  int opResult(
          Uint8List runtimeId, Uint8List opId, Uint8List buffer, int cap) =>
      _inner.opResult(runtimeId, opId, buffer, cap);

  @override
  int pollEvent(Uint8List runtimeId, Uint8List buffer, int cap) =>
      _inner.pollEvent(runtimeId, buffer, cap);

  @override
  int pollProgress(Uint8List buffer, int cap) =>
      _inner.pollProgress(buffer, cap);
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
  int serverInfo(Uint8List runtimeId, Uint8List opIdOut) {
    _tick('serverInfo');
    return _inner.serverInfo(runtimeId, opIdOut);
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
