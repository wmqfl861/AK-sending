/// W1 (batch W, 2026-11-22, V-N2 — the zombie connection row) service
/// tests: the `conn_lost` terminal reason, the RESUME reconciliation
/// against the engine's connection snapshot, and the no-progress stall
/// watchdog. All three close the same product defect from the Dart
/// side: a connection that died while the app could not observe it (a
/// frozen process, a lost event) must not keep rendering as a live
/// transfer — measured in V2-b as a row connected at t=121 s with no
/// terminal event until `idle_expired` at t=719.6 s.
///
/// W-N2 (batch X, 2026-11-22): the stall watchdog no longer marks a
/// silent transfer directly — the budget expiring first asks the
/// engine's `ak_task_list` snapshot (the verifying phase can be
/// legitimately silent for far longer than the budget since Q-N2 made
/// verification parallel). The engine-gated tests below pin all three
/// outcomes: vouched → re-armed, gone → `conn_lost`, snapshot read
/// failure → deferred retry.
library;

import 'dart:typed_data' show Uint8List;

import 'package:ak_ffi_dart/ak_ffi_dart.dart' show AkStatus, FakeAkTaskCalls;
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

/// W-N2 (batch X): a fake whose `ak_task_list` can be scripted to FAIL —
/// the snapshot-read exception rehearsal for the engine-gated watchdog.
/// Everything else stays the stock [FakeAkTaskCalls] behavior (the class
/// is extendable by design, the doorbell fakes do the same).
final class _ListFailingFake extends FakeAkTaskCalls {
  /// While true, every `list` call answers errInternal — the FFI facade
  /// turns that into an `AkFfiException`, the real shape of a failed
  /// snapshot read.
  bool failList = false;

  @override
  int list(Uint8List runtimeId, Uint8List buffer, int cap) =>
      failList ? AkStatus.errInternal : super.list(runtimeId, buffer, cap);
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
      'W1/W-N2: a fold silent for the stall budget is marked conn_lost '
      'only once the ENGINE stops vouching for it (both engine shapes)',
      () async {
    for (final shape in _DropShape.values) {
      var now = DateTime(2026, 11, 22, 10);
      final fake = FakeAkTaskCalls();
      final service = booted(fake, clock: () => now);
      final connId = await pairedConnectedRow(service);
      final runtimeId = fake.runtimes.values.first.id;

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

      // W-N2: the engine must stop vouching too (the W1 zombie shape —
      // the row vanished from `ak_task_list` or already says
      // `disconnected`, with the event lost). While the engine still
      // listed the connection the snapshot check would legitimately
      // re-arm the watchdog instead of marking.
      switch (shape) {
        case _DropShape.forgotten:
          fake.runtimes[runtimeId]!.connections.remove(connId);
        case _DropShape.stateFlipped:
          fake.runtimes[runtimeId]!.connections[connId]!.state =
              'disconnected';
      }
      // Past the budget with zero progress lines AND the engine's
      // verdict gone: the pump's watchdog marks the row failed with
      // the honest reason.
      now = now.add(const Duration(seconds: 2));
      service.pumpEvents();
      final row = service.currentConnections.first;
      expect(row.state, 'disconnected', reason: 'shape $shape');
      expect(row.disconnectReason, 'conn_lost', reason: 'shape $shape');
      expect(service.isTransferInProgress, isFalse,
          reason: 'shape $shape: the in-progress fact ends with the row');
      service.dispose();
    }
  });

  test(
      'W-N2: a VERIFYING fold silent past the budget is NOT marked while '
      'the engine vouches — the watchdog re-arms (a second silent budget '
      'still passes)', () async {
    var now = DateTime(2026, 11, 22, 13);
    final fake = FakeAkTaskCalls();
    final service = booted(fake, clock: () => now);
    final connId = await pairedConnectedRow(service);
    final runtimeId = fake.runtimes.values.first.id;

    // Bytes move, then the fold enters VERIFYING (Q-N2: the receiver
    // digests files IN PARALLEL — progress lands only per completed
    // file, and the sender waits for the final ack with none at all).
    // The line then goes legitimately silent FAR past the budget.
    fake.emitProgress(connId, event: 'accepted');
    fake.emitProgress(connId, event: 'chunk_received');
    fake.emitProgress(connId, event: 'verifying');
    service.pumpEvents();
    // The engine still knows the connection and never reports it
    // `disconnected` (the fake's row stays at its join-time state).
    expect(fake.runtimes[runtimeId]!.connections, contains(connId),
        reason: 'the fixture must keep the engine row alive');

    // First expiry: the watchdog asks the engine, the engine vouches →
    // no terminal state, the budget RE-ARMS.
    now = now.add(const Duration(seconds: 151));
    service.pumpEvents();
    expect(service.currentConnections.first.state, 'connected',
        reason: 'a silent verify the engine vouches for is not a loss');
    expect(service.currentConnections.first.disconnectReason, isNull);
    expect(service.isTransferInProgress, isTrue);

    // A SECOND full budget of silence: still not marked (the re-armed
    // watchdog asked again; the engine still vouches).
    now = now.add(const Duration(seconds: 150));
    service.pumpEvents();
    expect(service.currentConnections.first.state, 'connected',
        reason: 'the second silent budget expires — still no mark');
    expect(service.currentConnections.first.disconnectReason, isNull);

    // Proof the baseline actually RESET (not "asks every tick"): within
    // the new budget window the watchdog has no candidate, so it never
    // consults the engine — the connection row can be REMOVED from the
    // fake engine and the transfer still survives this tick.
    fake.runtimes[runtimeId]!.connections.remove(connId);
    now = now.add(const Duration(seconds: 100));
    service.pumpEvents();
    expect(service.currentConnections.first.state, 'connected',
        reason: 'inside the re-armed budget there is no candidate at all');

    // The re-armed budget expires with the engine row gone: NOW the
    // honest conn_lost lands.
    now = now.add(const Duration(seconds: 51));
    service.pumpEvents();
    final row = service.currentConnections.first;
    expect(row.state, 'disconnected');
    expect(row.disconnectReason, 'conn_lost');
    service.dispose();
  });

  test(
      'W-N2: a FAILING engine snapshot at the stall budget marks nothing '
      'and retries on the next tick', () async {
    var now = DateTime(2026, 11, 22, 14);
    final fake = _ListFailingFake();
    final service = booted(fake, clock: () => now);
    final connId = await pairedConnectedRow(service);
    final runtimeId = fake.runtimes.values.first.id;

    fake.emitProgress(connId, event: 'accepted');
    fake.emitProgress(connId, event: 'chunk_received');
    service.pumpEvents();
    expect(service.isTransferInProgress, isTrue);

    // The budget expires AND the engine no longer knows the connection,
    // but the snapshot read itself fails: nothing is marked AND nothing
    // is reset (the transfer stays expired for the retry).
    fake.runtimes[runtimeId]!.connections.remove(connId);
    fake.failList = true;
    now = now.add(const Duration(seconds: 151));
    service.pumpEvents();
    expect(service.currentConnections.first.state, 'connected',
        reason: 'a failed snapshot read never marks (nor re-arms)');

    // The very next tick — same clock, the baseline was untouched —
    // retries the snapshot; with the read healthy again the SAME
    // expired transfer is marked conn_lost (the deferred verdict).
    fake.failList = false;
    service.pumpEvents();
    final row = service.currentConnections.first;
    expect(row.state, 'disconnected');
    expect(row.disconnectReason, 'conn_lost');
    expect(service.isTransferInProgress, isFalse);
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
