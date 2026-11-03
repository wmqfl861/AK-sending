import 'package:ak_sending_app/ui/pages/about_page.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'fake_transfer_service.dart';
import 'test_helpers.dart';

void main() {
  testWidgets('renders ADR-001 one-liner, no-relay statement and version',
      (tester) async {
    final fake = FakeTransferService();
    await pumpPage(tester, AboutPage(service: fake));

    expect(find.text('仅设备直连承载文件数据（ADR-001）'), findsOneWidget);
    expect(
      find.textContaining('允许服务器参与设备发现、信令和 NAT 穿透协调'),
      findsOneWidget,
    );
    expect(find.text('无中继声明'), findsOneWidget);
    expect(
      find.textContaining('不会把文件上传到服务器'),
      findsOneWidget,
    );
    expect(
      find.byKey(const ValueKey('about_version')),
      findsOneWidget,
    );
    expect(find.text('ak-ffi 1 (wire v1, caps 0x0007)'), findsOneWidget);
  });
}
