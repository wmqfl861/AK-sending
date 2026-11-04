import 'package:ak_sending_app/ui/app.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'fake_transfer_service.dart';
import 'test_helpers.dart';

/// Taps a destination label inside the NavigationRail only (page headings
/// reuse the same words).
Finder railLabel(String text) => find.descendant(
      of: find.byType(NavigationRail),
      matching: find.text(text),
    );

void main() {
  testWidgets('NavigationRail switches between the five tabs and keeps '
      'page state alive', (tester) async {
    final fake = FakeTransferService();
    await tester.binding.setSurfaceSize(const Size(1280, 1800));
    addTearDown(() => tester.binding.setSurfaceSize(null));
    await tester.pumpWidget(AkSendingApp(service: fake));

    // Home is the initial tab: identity card visible.
    expect(find.text('DESKTOP-AK1'), findsOneWidget);

    // Switch to Receive via the rail; the remembered out dir survives a
    // round trip through another tab (IndexedStack keeps state).
    await tester.tap(railLabel('接收'));
    await tester.pumpAndSettle();
    await enterField(
        tester, const ValueKey('receive_out_dir'), 'C:\\remembered');
    await tester.tap(railLabel('主页'));
    await tester.pumpAndSettle();
    await tester.tap(railLabel('接收'));
    await tester.pumpAndSettle();
    expect(find.widgetWithText(TextField, 'C:\\remembered'), findsOneWidget);

    // Remaining tabs render their content when selected.
    await tester.tap(railLabel('发送'));
    await tester.pumpAndSettle();
    expect(find.byKey(const ValueKey('send_start')), findsOneWidget);
    await tester.tap(railLabel('历史'));
    await tester.pumpAndSettle();
    await tester.pump(const Duration(milliseconds: 1));
    expect(find.text('还没有历史任务'), findsOneWidget);
    await tester.tap(railLabel('关于'));
    await tester.pumpAndSettle();
    expect(find.text('无中继声明'), findsOneWidget);
  });

  testWidgets('home quick entry jumps to the receive tab', (tester) async {
    final fake = FakeTransferService();
    await tester.binding.setSurfaceSize(const Size(1280, 1800));
    addTearDown(() => tester.binding.setSurfaceSize(null));
    await tester.pumpWidget(AkSendingApp(service: fake));

    await tester.tap(find.text('接收文件'));
    await tester.pumpAndSettle();

    // The receive form is interactable, proving the tab switched.
    await enterField(tester, const ValueKey('receive_out_dir'), 'C:\\ak-in');
    await tester.tap(find.text('开始监听'));
    await tester.pump();
    expect(find.byKey(const ValueKey('receive_task_card')), findsOneWidget);
  });

  testWidgets('the pairing tab (wave-14 sixth tab) renders the pairing '
      'management page; the home peers row jumps to it', (tester) async {
    final fake = FakeTransferService();
    await tester.binding.setSurfaceSize(const Size(1280, 1800));
    addTearDown(() => tester.binding.setSurfaceSize(null));
    await tester.pumpWidget(AkSendingApp(service: fake));

    // Direct rail navigation.
    await tester.tap(railLabel('配对'));
    await tester.pumpAndSettle();
    expect(find.byKey(const ValueKey('pairing_add_button')), findsOneWidget);
    expect(find.text('还没有配对设备'), findsOneWidget);

    // Home linkage: the identity card's pairing-book row jumps here too.
    await tester.tap(railLabel('主页'));
    await tester.pumpAndSettle();
    await tester.tap(find.textContaining('已配对设备'));
    await tester.pumpAndSettle();
    expect(find.byKey(const ValueKey('pairing_add_button')), findsOneWidget);
  });
}
