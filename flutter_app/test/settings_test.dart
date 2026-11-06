/// Wave-28 settings: the single save-folder setting — display, change
/// via the directory picker, persistence through the store, and restore
/// into the service on shell startup.
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
}
