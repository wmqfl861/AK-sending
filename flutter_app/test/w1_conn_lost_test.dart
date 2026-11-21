/// W1 (batch W, 2026-11-22, V-N2 — the zombie connection row) service
/// tests: the `conn_lost` terminal reason, the RESUME reconciliation
/// against the engine's connection snapshot, and the no-progress stall
/// watchdog. All three close the same product defect from the Dart
/// side: a connection that died while the app could not observe it (a
/// frozen process, a lost event) must not keep rendering as a live
/// transfer — measured in V2-b as a row connected at t=121 s with no
/// terminal event until `idle_expired` at t=719.6 s.
library;


import 'package:ak_ffi_dart/ak_ffi_dart.dart' show FakeAkTaskCalls;
import 'package:flutter_test/flutter_test.dart';
import 'package:ak_sending_app/services/simple_transfer_service.dart';

const String regId =
    '1111111111111111111111111111111111111111111111111111111111111111';

/// Zero-delay stand-in: op wait loops drain synchronously under the fake.
Future<void> immediate(Duration d) async {}

/// How the fake engine "loses" a connection while the app cannot watch.
enum _DropShape {
  /// The engine no longer knows the connection at all (its row vanished
  /// from `ak_task_list` without any event).
  forgotten,

  /// The engine's own entry already says `disconnected`, but the event
  /// was never delivered (lost while the process was suspended).
  stateFlipped,
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  /// One paired, probe-settled service whose single row is `connected`.
  SimpleTransferService booted(FakeAkTaskCalls fake,
      {DateTime Function()? clock}) {
    final service = SimpleTransferService(
      calls: fake,
      delay: immediate,
      clock: clock,
      eventPollInterval: const Duration(days: 1), // tests pump manually
    );
    service.start(
      registry: '127.0.0.1:7200',
      registryIdHex: regId,
      idDir: 'C:/ak/w1-test',
    );
    return service;
  }

  Future<String> pairedConnectedRow(SimpleTransferService service) async {
    final created = await service.createCode(SimpleTransferRole.sender);
    final joined = await service.join(created.code);
    service.pumpEvents();
    expect(service.currentConnections, hasLength(1));
    expect(service.currentConnections.first.state, 'connected',
        reason: 'the fixture must reach a live row first');
    return joined.connId;
  }

  test(
      'W1: a connection the engine silently dropped while backgrounded '
      'fails within one pump of the resume (both engine shapes)', () async {
    for (final shape in _DropShape.values) {
      final fake = FakeAkTaskCalls();
      final service = booted(fake);
      final connId = await pairedConnectedRow(service);
      final runtimeId = fake.runtimes.values.first.id;

      // The app goes background; the engine loses the connection WITHOUT
      // emitting any event (the frozen-process shape).
      service.setAppBackgrounded(true);
      switch (shape) {
        case _DropShape.forgotten:
          fake.runtimes[runtimeId]!.connections.remove(connId);
        case _DropShape.stateFlipped:
          fake.runtimes[runtimeId]!.connections[connId]!.state =
              'disconnected';
      }
      // Still a live row while backgrounded: nothing pumped, nothing
      // reconciled — the engine's own event is the only other source.
      expect(service.currentConnections.first.state, 'connected');

      // Foreground again: one pump (the synchronous drain + the W1
      // snapshot reconcile) turns the zombie row terminal.
      service.setAppBackgrounded(false);
      final row = service.currentConnections.first;
      expect(row.state, 'disconnected',
          reason: 'shape $shape: the zombie row dies on resume');
      expect(row.disconnectReason, 'conn_lost',
          reason: 'shape $shape: the honest reason, not idle_expired');
      expect(service.phase, SimpleTransferPhase.idle);
      service.dispose();
    }
  });

  test(
      'W1: a healthy backgrounded connection survives the resume '
      '(the reconcile never invents failures)', () async {
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    final connId = await pairedConnectedRow(service);

    service.setAppBackgrounded(true);
    // The engine still knows the connection and reports it live.
    service.setAppBackgrounded(false);
    final row = service.currentConnections.first;
    expect(row.state, 'connected');
    expect(row.disconnectReason, isNull);
    expect(connId, isNotEmpty); // the fixture's row is the one asserted
    service.dispose();
  });

  test(
      'W1: a transferring fold with NO progress for the stall budget is '
      'marked failed with conn_lost (injectable clock)', () async {
    var now = DateTime(2026, 11, 22, 10);
    final fake = FakeAkTaskCalls();
    final service = booted(fake, clock: () => now);
    final connId = await pairedConnectedRow(service);

    // A receive-side transfer starts: bytes begin to move (the fold
    // reaches `transferring`), then the peer goes silent.
    fake.emitProgress(connId, event: 'accepted');
    fake.emitProgress(connId, event: 'chunk_received');
    service.pumpEvents();
    expect(service.isTransferInProgress, isTrue,
        reason: 'the fold is live — the W7.3 shared judgment sees it');

    // Under the budget: still healthy.
    now = now.add(const Duration(seconds: 149));
    service.pumpEvents();
    expect(service.currentConnections.first.state, 'connected');

    // Past the budget with zero progress lines: the pump's watchdog
    // marks the row failed with the honest reason.
    now = now.add(const Duration(seconds: 2));
    service.pumpEvents();
    final row = service.currentConnections.first;
    expect(row.state, 'disconnected');
    expect(row.disconnectReason, 'conn_lost');
    expect(service.isTransferInProgress, isFalse,
        reason: 'the in-progress fact ends with the row');
    service.dispose();
  });

  test(
      'W1: a stalled transfer that keeps making progress is never cut '
      '(the budget renews on every progress fact)', () async {
    var now = DateTime(2026, 11, 22, 11);
    final fake = FakeAkTaskCalls();
    final service = booted(fake, clock: () => now);
    final connId = await pairedConnectedRow(service);

    fake.emitProgress(connId, event: 'accepted');
    service.pumpEvents();
    // Progress keeps landing in windows shorter than the budget — the
    // total elapsed time crosses MANY budgets, the no-progress gap never
    // does.
    for (var round = 0; round < 4; round++) {
      now = now.add(const Duration(seconds: 100));
      fake.emitProgress(connId, event: 'chunk_received');
      service.pumpEvents();
      expect(service.currentConnections.first.state, 'connected',
          reason: 'round $round: renewed progress is not a stall');
    }
    service.dispose();
  });

  test(
      'W1: an awaiting-consent wait is NOT a stall (no progress lines are '
      'the design there, the watchdog scope excludes it)', () async {
    var now = DateTime(2026, 11, 22, 12);
    final fake = FakeAkTaskCalls();
    final service = booted(fake, clock: () => now);
    final connId = await pairedConnectedRow(service);

    // The offer lands (sender-side fold parks at awaiting_consent) and
    // the receiver's user never answers: no progress lines follow — by
    // design, for up to the 120 s native window. Far past the stall
    // budget, the row must STILL be live: the watchdog only covers the
    // transferring/verifying ranks.
    fake.emitProgress(connId, event: 'offered');
    service.pumpEvents();
    now = now.add(const Duration(minutes: 10));
    service.pumpEvents();
    expect(service.currentConnections.first.state, 'connected',
        reason: 'a consent wait is not a transfer stall');
    service.dispose();
  });

  test('W1: a disconnected event with reason conn_lost goes terminal',
      () async {
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    final connId = await pairedConnectedRow(service);
    final runtimeId = fake.runtimes.values.first.id;

    fake.emitDisconnected(runtimeId, connId: connId, reason: 'conn_lost');
    service.pumpEvents();
    final row = service.currentConnections.first;
    expect(row.state, 'disconnected');
    expect(row.disconnectReason, 'conn_lost');
    service.dispose();
  });
}
