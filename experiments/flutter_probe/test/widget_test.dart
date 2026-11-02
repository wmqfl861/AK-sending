import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import '../lib/main.dart';
import 'controller_test.dart' show FakeApi;

void main() {
  testWidgets('permission denial never starts native work', (tester) async {
    final api = FakeApi();
    await tester.pumpWidget(
      MaterialApp(
        home: ProbePage(api: api, permission: () async => 'denied'),
      ),
    );
    await tester.tap(find.text('运行合成 TLS 实验'));
    await tester.pump();
    expect(api.starts, 0);
    expect(find.text('平台：denied'), findsOneWidget);
  });
  testWidgets('allowed explicit start, cancel, release', (tester) async {
    final api = FakeApi();
    await tester.pumpWidget(
      MaterialApp(
        home: ProbePage(api: api, permission: () async => 'granted'),
      ),
    );
    await tester.tap(find.text('运行合成 TLS 实验'));
    await tester.pump();
    expect(api.starts, 1);
    api.releaseCode = -2;
    await tester.tap(find.text('取消并释放'));
    await tester.pump();
    expect(find.text('cancelled'), findsOneWidget);
    await tester.pumpWidget(const SizedBox());
  });
}
