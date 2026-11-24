/// Batch Z widget test — the app root's startup cleanup of staging
/// leftovers is taken ONLY with the production gateway.
///
/// `PluginFilePickGateway.cleanupStaleStaging()` is a static with no
/// instance seam, so no double can stand in for it. Its platform fact comes
/// from the UI's platform seam and its one channel call is observed through
/// the test binding's mock handler on the real channel name — no real
/// platform channel is ever opened here.
library;

import 'package:flutter/foundation.dart' show ValueKey;
import 'package:flutter/services.dart' show MethodChannel;
import 'package:flutter_test/flutter_test.dart';

import 'package:ak_sending_app/ui/service/file_pick_gateway.dart';
import 'package:ak_sending_app/ui/service/ui_platform.dart';

import 'w28_test_helpers.dart';

/// The channel the production gateway talks to (`MainActivity` owns the
/// other half). Intercepted through the test binding's mock.
const MethodChannel _storageChannel = MethodChannel('dev.ak/storage');

void main() {
  // A fresh "process" per test: the static runs once per process, so its
  // memo must be clear for a test to observe the app start's own call.
  setUp(PluginFilePickGateway.resetProcessStateForTest);
  tearDown(PluginFilePickGateway.resetProcessStateForTest);

  /// Installs the recording mock handler on the real channel name; returns
  /// the recorded method names.
  List<String> recordStorageCalls() {
    final calls = <String>[];
    final messenger =
        TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
    messenger.setMockMethodCallHandler(_storageChannel, (call) async {
      calls.add(call.method);
      return <String, Object>{'removed': 0};
    });
    addTearDown(
        () => messenger.setMockMethodCallHandler(_storageChannel, null));
    return calls;
  }

  /// Makes the static's platform fact read "Android" — the cleanup only
  /// talks to the platform there.
  void onAndroid() {
    UiPlatform.androidOverride = true;
    addTearDown(() => UiPlatform.androidOverride = null);
  }

  testWidgets('staged_entry app shell: an injected fake gateway never takes '
      'the startup cleanup path', (tester) async {
    onAndroid();
    final calls = recordStorageCalls();
    final picker = FakeFilePickGateway();

    await pumpApp(tester, picker: picker);
    await pumpEvents(tester);

    expect(find.byKey(const ValueKey('home_send_button')), findsOneWidget,
        reason: '前置：应用已启动');
    expect(calls, isEmpty, reason: '注入的假网关绝不碰通道');
    expect(picker.released, isEmpty);
  });

  testWidgets('staged_entry app shell: the production gateway takes the '
      'startup cleanup path at app start', (tester) async {
    onAndroid();
    final calls = recordStorageCalls();

    // pumpApp's default picker IS the production gateway.
    await pumpApp(tester);

    expect(calls, ['cleanupStaleStaging'],
        reason: '应用启动时清理一次以前的暂存残留');
    expect(find.byKey(const ValueKey('home_send_button')), findsOneWidget);

    // Rebuilding the shell (a tab switch) never reaches the channel again.
    await tester.tap(find.text('设置'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    await tester.tap(find.text('连接'));
    await tester.pump();
    expect(calls, ['cleanupStaleStaging']);
  });

  testWidgets('staged_entry app shell: off Android the production '
      'gateway\'s startup path never reaches the channel', (tester) async {
    // No platform override: the real host fact (a desktop) stays in force.
    final calls = recordStorageCalls();

    await pumpApp(tester);
    await pumpEvents(tester);

    expect(find.byKey(const ValueKey('home_send_button')), findsOneWidget);
    expect(calls, isEmpty, reason: '桌面行为不变：启动不碰通道');
  });
}
