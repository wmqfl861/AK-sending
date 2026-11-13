/// System-bar insets on the real app shell, without changing desktop layout.
library;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:ak_sending_app/services/simple_transfer_fake.dart';
import 'package:ak_sending_app/ui/app.dart';
import 'package:ak_sending_app/ui/service/save_dir_store.dart';
import 'package:ak_sending_app/ui/service/simple_transfer_service.dart';
import 'package:ak_sending_app/ui/service/transfer_history.dart';

import 'w28_test_helpers.dart' show pumpEvents;

Future<LocalSimpleTransferService> _pumpApp(
  WidgetTester tester, {
  Size size = const Size(390, 844),
  double top = 32,
  double bottom = 24,
  Brightness brightness = Brightness.light,
  TransferHistory? history,
}) async {
  // The Android screenshot is 780x1688 physical pixels at density 320.
  tester.view.devicePixelRatio = 2;
  tester.view.physicalSize = size * 2;
  tester.view.viewPadding = FakeViewPadding(top: top * 2, bottom: bottom * 2);
  tester.view.padding = FakeViewPadding(top: top * 2, bottom: bottom * 2);
  tester.view.viewInsets = FakeViewPadding.zero;
  addTearDown(tester.view.reset);
  tester.platformDispatcher.platformBrightnessTestValue = brightness;
  addTearDown(tester.platformDispatcher.clearPlatformBrightnessTestValue);

  final service = LocalSimpleTransferService();
  addTearDown(service.dispose);
  await tester.pumpWidget(AkSendingApp(
    service: service,
    store: InMemorySaveDirStore(),
    history: history,
  ));
  await tester.pumpAndSettle();
  return service;
}

Finder get _home => find.byKey(const ValueKey('home_page'));

Finder get _homeTitle =>
    find.descendant(of: _home, matching: find.text('连接'));

void main() {
  final android = TargetPlatformVariant.only(TargetPlatform.android);
  for (final top in [24.0, 32.0]) {
    for (final brightness in [Brightness.light, Brightness.dark]) {
      testWidgets('390dp ${brightness.name}: home avoids $top dp status bar '
          'and bottom navigation inset once', (tester) async {
        await _pumpApp(tester, top: top, brightness: brightness);

        final media = MediaQuery.of(tester.element(find.byType(AppShell)));
        expect(media.size, const Size(390, 844));
        expect(media.padding, EdgeInsets.only(top: top, bottom: 24));
        expect(media.viewPadding, media.padding);
        expect(Theme.of(tester.element(_home)).brightness, brightness);

        expect(tester.getTopLeft(_homeTitle), Offset(16, top + 16),
            reason: 'The title needs one status-bar inset, not just page padding.');
        final page = tester.getRect(_home);
        final navigation = tester.getRect(find.byType(NavigationBar));
        expect(page, Rect.fromLTRB(0, top, 390, 740));
        expect(navigation, const Rect.fromLTRB(0, 740, 390, 844));
        for (final destination in find.byType(NavigationDestination).evaluate()) {
          expect(tester.getRect(find.byWidget(destination.widget)).bottom,
              lessThanOrEqualTo(820));
        }
        expect(tester.takeException(), isNull);

        await tester.tap(find.descendant(
          of: find.byType(NavigationBar),
          matching: find.text('设置'),
        ));
        await tester.pumpAndSettle();
        expect(tester.getRect(find.byKey(const ValueKey('settings_page'))), page);
        expect(tester.takeException(), isNull);
      }, variant: android);
    }
  }

  testWidgets('390dp long home history scrolls inside the system bars',
      (tester) async {
    final history = TransferHistory();
    addTearDown(history.dispose);
    for (var i = 0; i < 10; i++) {
      history.add(TransferRecord(
        role: TaskRole.send,
        peerName: '合成测试设备-$i-${'很长的设备名称' * 12}',
        succeeded: true,
        finishedAt: DateTime(2026, 11, 13, 12, i),
        fileCount: 2,
        totalBytes: 2048,
        elapsed: const Duration(seconds: 2),
        pathLabel: '局域网直连',
      ));
    }
    await _pumpApp(tester, history: history);
    expect(tester.getTopLeft(_homeTitle).dy, 48);
    expect(tester.getRect(_home), const Rect.fromLTRB(0, 32, 390, 740));
    expect(tester.takeException(), isNull);

    final lastItem = find.byKey(const ValueKey('history_item_9'));
    await tester.scrollUntilVisible(
      lastItem,
      400,
      scrollable: find.descendant(of: _home, matching: find.byType(Scrollable)),
    );
    await tester.pumpAndSettle();
    final item = tester.getRect(lastItem);
    expect(item.top, greaterThanOrEqualTo(32));
    expect(item.bottom, lessThanOrEqualTo(740));
    expect(item.left, greaterThanOrEqualTo(16));
    expect(item.right, lessThanOrEqualTo(374));
    expect(lastItem.hitTestable(), findsOneWidget);
    expect(tester.takeException(), isNull);
  }, variant: android);

  testWidgets('incoming banner does not add a second top inset', (tester) async {
    final fake = await _pumpApp(tester);
    fake.emitIncomingRequest(fileCount: 2, totalBytes: 2048);
    await pumpEvents(tester);

    final banner = tester.getRect(find.byKey(const ValueKey('incoming_host')));
    expect(banner.top, 32);
    expect(tester.getRect(_home).top, banner.bottom);
    expect(tester.getTopLeft(_homeTitle).dy, banner.bottom + 16);
    expect(tester.takeException(), isNull);
    await tester.pumpWidget(const SizedBox.shrink());
  }, variant: android);

  testWidgets('role sheet keeps its own bottom inset outside the shell body',
      (tester) async {
    await _pumpApp(tester);
    await tester.tap(find.byKey(const ValueKey('home_create_code_button')));
    await tester.pumpAndSettle();

    expect(tester.getRect(find.byType(BottomSheet)).bottom, 844);
    final receive = tester.getRect(find.byKey(const ValueKey('role_receive_button')));
    expect(receive.bottom, 796,
        reason: '24dp system inset plus the existing 24dp sheet padding.');
    expect(receive.top, greaterThanOrEqualTo(32));
    expect(tester.takeException(), isNull);
  }, variant: android);

  testWidgets('pushed detail AppBar consumes the status inset only once',
      (tester) async {
    final fake = await _pumpApp(tester);
    await fake.createCode(TaskRole.send);
    final id = fake.simulatePeerJoin(peerName: '合成测试设备');
    await pumpEvents(tester);
    fake.completeProbe(id);
    await pumpEvents(tester);
    await tester.tap(find.byKey(ValueKey('conn_card_name_$id')));
    await tester.pumpAndSettle();

    expect(tester.getRect(find.byType(AppBar)).top, 0);
    expect(tester.getRect(find.byType(AppBar)).bottom, 88);
    expect(tester.getTopLeft(find.byKey(const ValueKey('conn_detail_page'))).dy,
        88, reason: 'The AppBar already accounts for 32dp + its 56dp toolbar.');
    expect(tester.takeException(), isNull);
  }, variant: android);

  for (final (width, railWidth) in [(900.0, 80.0), (1280.0, 256.0)]) {
    testWidgets('Windows $width dp with zero insets keeps full-height rail '
        'and original page spacing', (tester) async {
      await _pumpApp(
        tester,
        size: Size(width, 900),
        top: 0,
        bottom: 0,
      );
      expect(find.byType(NavigationBar), findsNothing);
      expect(tester.getRect(find.byType(NavigationRail)),
          Rect.fromLTWH(0, 0, railWidth, 900));
      expect(tester.getRect(_home),
          Rect.fromLTRB(railWidth + 1, 0, width, 900));
      expect(tester.getTopLeft(_homeTitle), Offset(railWidth + 17, 16));
      expect(tester.takeException(), isNull);
    }, variant: TargetPlatformVariant.only(TargetPlatform.windows));
  }
}
