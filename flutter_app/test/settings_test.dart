/// Wave-28 settings: the single save-folder setting — display, change
/// via the directory picker, persistence through the store, and restore
/// into the service on shell startup — plus the G6b (batch G6,
/// 2026-11-13)「关于」card: the registry server's build revision.
library;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:ak_sending_app/services/simple_transfer_fake.dart';
import 'package:ak_sending_app/ui/app.dart';
import 'package:ak_sending_app/ui/service/file_pick_gateway.dart';
import 'package:ak_sending_app/ui/service/save_dir_store.dart';

import 'w28_test_helpers.dart';

void main() {
  testWidgets('shows the service save dir; changing it updates the '
      'service and persists', (tester) async {
    final store = InMemorySaveDirStore();
    final picker = FakeFilePickGateway(
      directoryResult: const PickedEntry(
        path: 'D:\\接收区',
        name: '接收区',
        isDirectory: true,
      ),
    );
    final fake = await pumpApp(tester, store: store, picker: picker);

    // Switch to the settings tab (bottom bar on phone, rail on wide —
    // pumpApp defaults to the wide surface).
    await tester.tap(find.text('设置'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.byKey(const ValueKey('settings_page')), findsOneWidget);
    expect(
        find.byKey(const ValueKey('settings_save_dir')), findsOneWidget);
    expect(find.text('Download/AK-接收'), findsOneWidget);

    await tester.tap(find.byKey(const ValueKey('settings_change_dir_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(picker.pickDirectoryCalls, 1);
    expect(fake.saveDir, 'D:\\接收区');
    expect(store.value, 'D:\\接收区');
    expect(find.text('D:\\接收区'), findsOneWidget);
    expect(find.textContaining('已更新'), findsOneWidget);
  });

  testWidgets('cancelling the picker keeps the current folder',
      (tester) async {
    final store = InMemorySaveDirStore();
    final picker = FakeFilePickGateway(); // directoryResult == null
    final fake = await pumpApp(tester, store: store, picker: picker);

    await tester.tap(find.text('设置'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    await tester.tap(find.byKey(const ValueKey('settings_change_dir_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(picker.pickDirectoryCalls, 1);
    expect(fake.setSaveDirCalls, 0);
    expect(store.value, isNull);
    expect(find.text('Download/AK-接收'), findsOneWidget);
  });

  testWidgets('a persisted folder is re-applied to the service at '
      'startup', (tester) async {
    final store = InMemorySaveDirStore()..value = 'E:\\上次选的目录';

    await tester.binding.setSurfaceSize(kWideSize);
    addTearDown(() => tester.binding.setSurfaceSize(null));
    final fake = LocalSimpleTransferService();
    await tester.pumpWidget(AkSendingApp(service: fake, store: store));
    await tester.pump();
    await tester.pump();

    expect(fake.saveDir, 'E:\\上次选的目录');
    expect(fake.saveDirRequests.single, 'E:\\上次选的目录');
  });

  // -- G6b (batch G6, 2026-11-13): the「关于」card -----------------------

  testWidgets('关于 card shows the registry revision and the honest '
      'unknown degrade', (tester) async {
    final store = InMemorySaveDirStore();
    final fake = await pumpApp(tester, store: store);
    // The fake defaults to null — the old-server / no-information shape.
    fake.serverRevisionValue = null;

    await tester.tap(find.text('设置'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(
        find.byKey(const ValueKey('settings_server_revision')), findsOneWidget);
    // The SettingsPage is BUILT inside an IndexedStack at app pump, but
    // its query is gated on first tab selection — the tap above ran it
    // exactly once.
    expect(fake.serverRevisionCalls, 1);
    // null answers render the honest unknown copy (never an error).
    expect(find.text('未知（服务器未报告版本）'), findsOneWidget);

    // A server that reports a build renders it VERBATIM after 刷新.
    fake.serverRevisionValue = '0.1.0+g6b4e19';
    await tester
        .tap(find.byKey(const ValueKey('settings_refresh_revision_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.text('0.1.0+g6b4e19'), findsOneWidget);
    expect(find.text('未知（服务器未报告版本）'), findsNothing);
    expect(fake.serverRevisionCalls, 2);
  });
}
