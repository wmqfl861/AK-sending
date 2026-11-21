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

  test('W7.3: a send turning in-progress calls on, settling calls off',
      () async {
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
    expect(mock.calls, isEmpty,
        reason: 'pairing alone never holds the screen');

    // The send op: in-progress at entry, settled when the report lands.
    await service.sendFiles(joined.connId, <String>['C:/ak/w73/a.bin']);
    await pumpEventQueue();
    expect(mock.calls, equals(<String>['on', 'off']),
        reason: 'the transfer held the screen for exactly its duration');
    expect(keep.isOn, isFalse);

    // A SECOND transfer cycle does it again (the coordinator re-arms).
    await service.sendFiles(joined.connId, <String>['C:/ak/w73/b.bin']);
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
    expect(mock.calls.last, 'off',
        reason: 'and released it when every transfer ended');
    adapter.dispose();
    mock.uninstall();
  });
}
