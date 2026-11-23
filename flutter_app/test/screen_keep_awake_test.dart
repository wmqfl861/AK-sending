/// W7.3 (batch W hand-off §4.7 item 3): the keep-screen-on platform
/// channel tests — a FAKE handler on the real `dev.ak/screen`
/// MethodChannel (`TestDefaultBinaryMessengerBinding`'s mock) asserts
/// that a transfer starting calls `on`, everything ending calls `off`,
/// and non-Android platforms call NOTHING. The in-progress judgment is
/// the W1 state machine's shared fact (`isTransferInProgress` /
/// `transferActivity`), driven here by the shipped [FakeAkTaskCalls].
library;


import 'package:ak_ffi_dart/ak_ffi_dart.dart' show FakeAkTaskCalls;
import 'package:flutter/services.dart' show MethodChannel;
import 'package:flutter_test/flutter_test.dart';
import 'package:ak_sending_app/services/ffi_simple_transfer_service.dart';
import 'package:ak_sending_app/services/screen_keep_awake.dart';
import 'package:ak_sending_app/services/simple_transfer_service.dart';
import 'package:ak_sending_app/ui/service/simple_transfer_service.dart'
    show TaskRole;

const String regId =
    '1111111111111111111111111111111111111111111111111111111111111111';

const MethodChannel screenChannel = MethodChannel('dev.ak/screen');

/// Zero-delay stand-in: op wait loops drain synchronously under the fake.
Future<void> immediate(Duration d) async {}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  /// Installs the recording fake handler; returns the recorder and the
  /// uninstaller.
  ({List<String> calls, void Function() uninstall}) installScreenMock() {
    final calls = <String>[];
    final messenger =
        TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
    messenger.setMockMethodCallHandler(screenChannel, (call) async {
      calls.add(call.method);
      return null;
    });
    return (
      calls: calls,
      uninstall: () =>
          messenger.setMockMethodCallHandler(screenChannel, null),
    );
  }

  SimpleTransferService booted(FakeAkTaskCalls fake) {
    final service = SimpleTransferService(
      calls: fake,
      delay: immediate,
      eventPollInterval: const Duration(days: 1), // tests pump manually
    );
    service.start(
      registry: '127.0.0.1:7200',
      registryIdHex: regId,
      idDir: 'C:/ak/w73-test',
    );
    return service;
  }

  test(
      'W7.3/W-N11: the gate holds from pairing through the send and '
      'releases when the task ends (v1: one code = one task)', () async {
    final mock = installScreenMock();
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    final keep = ScreenKeepAwake(service: service, isAndroid: () => true);
    keep.start();
    expect(keep.isOn, isFalse, reason: 'nothing runs yet — no call');

    final created = await service.createCode(SimpleTransferRole.receiver);
    final joined = await service.join(created.code);
    service.pumpEvents();
    expect(service.isTransferInProgress, isFalse);
    // W-N11 (2026-11-23) reverses W7.3's "pairing alone never holds the
    // screen": the two WAITING stages hold it too — a screen-off there
    // kills the registry keeper and the code becomes unredeemable (the
    // reproduced defect; see wn11_keep_awake_test for the judgment).
    await pumpEventQueue(); // stream edges land asynchronously
    expect(mock.calls, equals(<String>['on']),
        reason: 'waiting for the peer (or paired, picking) holds the gate');

    // The send op runs to completion while the row stays live.
    await service.sendFiles(joined.connId, <String>['C:/ak/w73/a.bin']);
    await pumpEventQueue();
    expect(mock.calls, equals(<String>['on']),
        reason: 'the send adds no NEW edge — the live row already held it');

    // The task ends (v1: a settled task's connection goes terminal).
    final runtimeId = fake.runtimes.values.first.id;
    final runtime = fake.runtimes[runtimeId]!;
    runtime.events.add(<String, dynamic>{
      'event': 'disconnected',
      'conn': joined.connId,
      'reason': 'done',
    });
    runtime.connections.remove(joined.connId);
    service.pumpEvents();
    await pumpEventQueue(); // stream edges land asynchronously
    expect(mock.calls, equals(<String>['on', 'off']),
        reason: 'the terminal row released the gate');

    // A SECOND cycle pairs a NEW code (one code = one task) and the
    // coordinator re-arms.
    final created2 = await service.createCode(SimpleTransferRole.receiver);
    final joined2 = await service.join(created2.code);
    service.pumpEvents();
    await pumpEventQueue();
    expect(mock.calls, equals(<String>['on', 'off', 'on']));
    runtime.events.add(<String, dynamic>{
      'event': 'disconnected',
      'conn': joined2.connId,
      'reason': 'done',
    });
    runtime.connections.remove(joined2.connId);
    service.pumpEvents();
    await pumpEventQueue();
    expect(mock.calls, equals(<String>['on', 'off', 'on', 'off']));
    service.dispose();
    keep.dispose();
    mock.uninstall();
  });

  test('W7.3: a receive fold (progress lines) holds the screen until '
      'its row goes terminal', () async {
    final mock = installScreenMock();
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    final keep = ScreenKeepAwake(service: service, isAndroid: () => true);
    keep.start();

    final created = await service.createCode(SimpleTransferRole.receiver);
    final joined = await service.join(created.code);
    final connId = joined.connId;
    service.pumpEvents();
    expect(mock.calls, isEmpty);

    // The receive-side fold appears with the first progress line and
    // stays in progress until the connection's terminal event.
    fake.emitProgress(connId, event: 'accepted');
    service.pumpEvents();
    expect(service.isTransferInProgress, isTrue);
    await pumpEventQueue();
    expect(mock.calls, equals(<String>['on']),
        reason: 'the transfer runs — the screen stays on');

    fake.emitDone(fake.runtimes.values.first.id, connId: connId);
    service.pumpEvents();
    expect(service.isTransferInProgress, isFalse);
    await pumpEventQueue();
    expect(mock.calls, equals(<String>['on', 'off']),
        reason: 'the terminal event released the screen');
    service.dispose();
    keep.dispose();
    mock.uninstall();
  });

  test('W7.3: non-Android platforms never touch the channel', () async {
    final mock = installScreenMock();
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    final keep = ScreenKeepAwake(service: service, isAndroid: () => false);
    keep.start();

    final created = await service.createCode(SimpleTransferRole.receiver);
    final joined = await service.join(created.code);
    service.pumpEvents();
    await service.sendFiles(joined.connId, <String>['C:/ak/w73/c.bin']);
    fake.emitProgress(joined.connId, event: 'accepted');
    service.pumpEvents();
    await pumpEventQueue();
    expect(service.isTransferInProgress, isFalse); // the send settled
    expect(mock.calls, isEmpty,
        reason: 'Windows/Linux: the channel is a deliberate no-op');
    expect(keep.isOn, isFalse);
    service.dispose();
    keep.dispose();
    mock.uninstall();
  });

  test('W7.3: dispose releases a held flag (best-effort off)', () async {
    final mock = installScreenMock();
    final fake = FakeAkTaskCalls();
    final service = booted(fake);
    final keep = ScreenKeepAwake(service: service, isAndroid: () => true);
    keep.start();

    final created = await service.createCode(SimpleTransferRole.receiver);
    final joined = await service.join(created.code);
    service.pumpEvents();
    fake.emitProgress(joined.connId, event: 'accepted');
    service.pumpEvents();
    await pumpEventQueue();
    expect(keep.isOn, isTrue);

    keep.dispose();
    await pumpEventQueue();
    expect(mock.calls, equals(<String>['on', 'off']),
        reason: 'dispose must not strand a screen-on flag');
    service.dispose();
    mock.uninstall();
  });

  test('W7.3: FfiSimpleTransferService wires the coordinator itself',
      () async {
    final mock = installScreenMock();
    final fake = FakeAkTaskCalls();
    final adapter = FfiSimpleTransferService(
      calls: fake,
      registry: '127.0.0.1:7200',
      registryIdHex: regId,
      idDir: 'C:/ak/w73-ffi-test',
      delay: immediate,
      isAndroid: true,
    );
    await adapter.start();
    final created = await adapter.createCode(TaskRole.receive);
    final joined = await adapter.joinCode(created.code);
    expect(joined.connectionId, isNotNull,
        reason: 'the fixture must pair before the send');
    await adapter.sendFiles(joined.connectionId!, <String>['C:/ak/w73/w.bin']);
    await pumpEventQueue();
    expect(mock.calls, isNotEmpty,
        reason: 'the adapter-driven flow held the screen');
    // W-N11: the gate also keys on live rows — the send settling alone no
    // longer releases it; the task's terminal event does (v1: one code =
    // one task).
    final runtimeId = fake.runtimes.values.first.id;
    fake.emitDone(runtimeId, connId: joined.connectionId!);
    fake.runtimes[runtimeId]!.events.add(<String, dynamic>{
      'event': 'disconnected',
      'conn': joined.connectionId,
      'reason': 'done',
    });
    fake.runtimes[runtimeId]!.connections.remove(joined.connectionId);
    // An op forces the event pump NOW (the cadence timer is real-time in
    // this plain test); the queued terminal events settle the row inside
    // the op's pump.
    await adapter.disconnect(joined.connectionId!);
    await pumpEventQueue();
    // The off edge is stream-delivered (asynchronous by contract); under
    // a busy isolate it can land after pumpEventQueue's flush — wait for
    // it with a bound instead of asserting an instant.
    final offDeadline = DateTime.now().add(const Duration(seconds: 2));
    while (mock.calls.last != 'off' && DateTime.now().isBefore(offDeadline)) {
      await Future<void>.delayed(const Duration(milliseconds: 20));
    }
    expect(mock.calls.last, 'off',
        reason: 'and released it when the task ended');
    adapter.dispose();
    mock.uninstall();
  });
}
