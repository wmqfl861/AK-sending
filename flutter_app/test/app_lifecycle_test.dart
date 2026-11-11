/// CR-P2-05 (review 2026-11-11, item 2) widget tests: the app shell's
/// lifecycle observer paces the REAL engine service behind the Ffi
/// adapter. Lifecycle changes are driven exactly like the framework
/// delivers them — through `tester.binding.handleAppLifecycleStateChanged`
/// — against the full app root (`AkSendingApp`), so the observer
/// registration in `AppShell` is exercised the way production wires it.
library;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:ak_ffi_dart/ak_ffi_dart.dart' show FakeAkTaskCalls;
import 'package:ak_sending_app/services/ffi_simple_transfer_service.dart';
import 'package:ak_sending_app/ui/app.dart' show AkSendingApp;
import 'package:ak_sending_app/ui/service/save_dir_store.dart'
    show InMemorySaveDirStore;
import 'package:ak_sending_app/ui/service/simple_transfer_service.dart'
    show TaskRole;

const String regId =
    '1111111111111111111111111111111111111111111111111111111111111111';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  /// Boots the REAL app root over the REAL adapter (fake seam): AppShell
  /// registers the lifecycle observer against the adapter's engine
  /// service exactly like production.
  Future<(FfiSimpleTransferService, FakeAkTaskCalls)> pumpRealApp(
      WidgetTester tester) async {
    final fake = FakeAkTaskCalls();
    final service = FfiSimpleTransferService(
      calls: fake,
      registry: '127.0.0.1:7200',
      registryIdHex: regId,
      idDir: 'C:/ak/lifecycle',
      isAndroid: false,
    );
    await tester.pumpWidget(AkSendingApp(
      service: service,
      store: InMemorySaveDirStore(),
    ));
    await tester.pump(); // the shell's start() lands, the pump arms
    return (service, fake);
  }

  testWidgets('paused while idle: the pump pauses entirely; resumed: '
      'immediate refresh + re-arm at the idle cadence', (tester) async {
    final (service, fake) = await pumpRealApp(tester);
    addTearDown(service.dispose);
    final engine = service.engine;
    expect(engine.hasActivity, isFalse, reason: '启动后空闲');
    final runtimeId = fake.runtimes.keys.first;

    // Backgrounded (hidden → paused): idle ⇒ paused entirely.
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.hidden);
    tester.binding
        .handleAppLifecycleStateChanged(AppLifecycleState.paused);
    expect(engine.isEventPumpArmed, isFalse,
        reason: '后台且无任务：泵完全暂停（零唤醒）');
    fake.runtimes[runtimeId]!.events.add(<String, dynamic>{
      'event': 'runtime_ready',
      'runtime': runtimeId,
    });
    final pollsAtPause = fake.pollEventCallCount;
    await tester.pump(const Duration(seconds: 5));
    expect(fake.pollEventCallCount, pollsAtPause, reason: '暂停期间零轮询');
    expect(fake.runtimes[runtimeId]!.events, hasLength(1),
        reason: '隐藏期间排队的事件不被消费');

    // Foreground again: the queued event is drained IMMEDIATELY…
    tester.binding
        .handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    expect(fake.runtimes[runtimeId]!.events, isEmpty,
        reason: '回前台立即刷新连接/任务状态：排空隐藏期间的事件');
    expect(fake.pollEventCallCount, greaterThan(pollsAtPause));
    // …and the pump re-arms — at the idle 1 s cadence (no activity).
    expect(engine.isEventPumpArmed, isTrue);
    expect(engine.effectiveEventPollInterval, const Duration(seconds: 1));
    final pollsAtResume = fake.pollEventCallCount;
    await tester.pump(const Duration(milliseconds: 500));
    expect(fake.pollEventCallCount, pollsAtResume,
        reason: '空闲前台保持 1s 节奏');
    service.dispose(); // in-body: the binding's pending-timer check precedes teardowns
  });

  testWidgets('paused while ACTIVE: a 1 s heartbeat survives; '
      'inactive-visible restores 50 ms immediately', (tester) async {
    final (service, fake) = await pumpRealApp(tester);
    addTearDown(service.dispose);
    final engine = service.engine;

    // A pairing is in flight (creator waiting for the join).
    final created = await service.createCode(TaskRole.send);
    expect(created.code.startsWith('AKI1-'), isTrue);
    expect(engine.hasActivity, isTrue, reason: 'paired＝配对进行中＝活动');
    expect(engine.effectiveEventPollInterval,
        const Duration(milliseconds: 50));

    tester.binding
        .handleAppLifecycleStateChanged(AppLifecycleState.paused);
    expect(engine.effectiveEventPollInterval, const Duration(seconds: 1),
        reason: '后台降频（仍有任务：保留 1s 心跳）');
    expect(engine.isEventPumpArmed, isTrue, reason: '有活动任务时不完全暂停');
    final pollsAtHidden = fake.pollEventCallCount;
    await tester.pump(const Duration(milliseconds: 900));
    expect(fake.pollEventCallCount, pollsAtHidden,
        reason: '1s 节奏：900ms 内零 tick');
    await tester.pump(const Duration(milliseconds: 200));
    expect(fake.pollEventCallCount, greaterThan(pollsAtHidden),
        reason: '1s 心跳仍在跳');

    // Foreground-again via `inactive` (the still-visible regime): the
    // fast cadence returns NOW, with the resume refresh.
    tester.binding
        .handleAppLifecycleStateChanged(AppLifecycleState.inactive);
    expect(engine.effectiveEventPollInterval,
        const Duration(milliseconds: 50),
        reason: '回前台立即恢复快节奏');
    service.dispose(); // in-body: the binding's pending-timer check precedes teardowns
  });
}
