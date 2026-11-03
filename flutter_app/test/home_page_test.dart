import 'package:ak_sending_app/ui/pages/home_page.dart';
import 'package:ak_sending_app/ui/service/transfer_service.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

import 'fake_transfer_service.dart';
import 'test_helpers.dart';

void main() {
  testWidgets('renders identity card: device name, id, fingerprint chip, '
      'pairing code and status dot', (tester) async {
    final fake = FakeTransferService();
    await pumpPage(tester, HomePage(service: fake));

    expect(find.text('DESKTOP-AK1'), findsOneWidget);
    expect(find.textContaining('设备 ID：abababab'), findsOneWidget);
    // Fingerprint chip shows the short id (first 8 hex chars).
    expect(find.text('指纹 abababab'), findsOneWidget);
    expect(find.text('123-456'), findsOneWidget);
    // Status dot label for the idle state.
    expect(find.text('就绪'), findsOneWidget);
    expect(find.text('接收文件'), findsOneWidget);
    expect(find.text('发送文件'), findsOneWidget);
  });

  testWidgets('status dot follows service status changes', (tester) async {
    final fake = FakeTransferService();
    await pumpPage(tester, HomePage(service: fake));
    expect(find.text('就绪'), findsOneWidget);

    fake.startReceive(const ReceiveOptions(outDir: 'C:\\ak-in'));
    await tester.pump();
    expect(find.text('监听中'), findsOneWidget);
    await fake.stopReceive();
    await tester.pump();
    expect(find.text('就绪'), findsOneWidget);
  });

  testWidgets('copy button puts the pairing code on the clipboard',
      (tester) async {
    Object? captured;
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      SystemChannels.platform,
      (call) async {
        if (call.method == 'Clipboard.setData') captured = call.arguments;
        return null;
      },
    );
    final fake = FakeTransferService();
    await pumpPage(tester, HomePage(service: fake));

    await tester.tap(find.byTooltip('复制配对码'));
    await tester.pump();

    expect(captured, isA<Map<dynamic, dynamic>>());
    expect((captured as Map<dynamic, dynamic>)['text'], '123-456');
    expect(find.textContaining('已复制配对码'), findsOneWidget);
  });

  testWidgets('quick entries call onGoTab with the right index',
      (tester) async {
    final visited = <int>[];
    final fake = FakeTransferService();
    await pumpPage(
      tester,
      HomePage(service: fake, onGoTab: visited.add),
    );

    await tester.tap(find.text('接收文件'));
    await tester.tap(find.text('发送文件'));
    expect(visited, [1, 2]);
  });
}
