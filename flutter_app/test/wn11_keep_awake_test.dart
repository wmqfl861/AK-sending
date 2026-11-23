/// W-N11 (batch X fix wave, 2026-11-23): the keep-awake gate covers the
/// two WAITING stages, not just active transfers — a created-but-unpaired
/// code and any live (non-terminal) connection hold the gate. The
/// reproduction: with the screen off during the「waiting for the peer」
/// stage the registry keeper died ~19 s in and the joiner saw
/// `unknown task code (... owner offline)` (X handoff §8, W-N11 row).
/// These tests pin the ORCHESTRATOR judgment ([transferActivity] edges —
/// the exact stream `ScreenKeepAwake` keys on; the channel half keeps its
/// own W7.3 tests).
library;

import 'dart:async' show Completer, StreamSubscription;

import 'package:ak_ffi_dart/ak_ffi_dart.dart' show FakeAkTaskCalls;
import 'package:flutter_test/flutter_test.dart';
import 'package:ak_sending_app/services/simple_transfer_service.dart';

const String regId =
    '1111111111111111111111111111111111111111111111111111111111111111';

Future<void> immediate(Duration d) async {}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  SimpleTransferService booted(FakeAkTaskCalls fake) {
    final service = SimpleTransferService(
      calls: fake,
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
}
