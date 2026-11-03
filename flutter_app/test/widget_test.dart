/// Minimal app smoke test: the shell boots over a fake service and the
/// Home tab renders. (Replaces the flutter-create counter test, which
/// targeted the old demo `MyApp`.)
library;

import 'package:ak_sending_app/ui/app.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'fake_transfer_service.dart';

void main() {
  testWidgets('app boots and shows the Home identity card',
      (WidgetTester tester) async {
    final fake = FakeTransferService();
    await tester.binding.setSurfaceSize(const Size(1280, 1800));
    addTearDown(() => tester.binding.setSurfaceSize(null));
    await tester.pumpWidget(AkSendingApp(service: fake));

    expect(find.text('DESKTOP-AK1'), findsOneWidget);
    expect(find.text('接收文件'), findsOneWidget);
    expect(find.byType(NavigationRail), findsOneWidget);
  });
}
