import 'package:ak_sending_app/ui/pages/home_page.dart';
import 'package:ak_sending_app/ui/service/transfer_service.dart';
import 'package:flutter/material.dart' show ValueKey;
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

  testWidgets('pairing-book row (wave-14 linkage) jumps to the pairing tab',
      (tester) async {
    final visited = <int>[];
    final fake = FakeTransferService();
    await pumpPage(
      tester,
      HomePage(service: fake, onGoTab: visited.add),
    );

    await tester.tap(find.textContaining('已配对设备'));
    expect(visited, [4], reason: '配对是 NavigationRail 的第 5 个标签（索引 4）');
  });

  testWidgets('startup refreshIdentity renders the real identity data '
      '(peersCount row)', (tester) async {
    final fake = FakeTransferService(
      refreshIdentityResult: DeviceInfo(
        deviceName: 'DESKTOP-AK1',
        fingerprintHex: '6c' * 32,
        pairingCode: '123456',
        peersCount: 3,
      ),
    );
    await pumpPage(tester, HomePage(service: fake));
    await tester.pump();

    expect(fake.refreshIdentityCalls, 1);
    expect(find.textContaining('已配对设备：3'), findsOneWidget);
    // The refreshed fingerprint/code replaced the constructor snapshot.
    expect(find.text('指纹 6c6c6c6c'), findsOneWidget);
    expect(find.text('123-456'), findsOneWidget);
  });

  testWidgets('shows the first-run hint when the engine created the '
      'identity', (tester) async {
    final fake = FakeTransferService(
      refreshIdentityResult: DeviceInfo(
        deviceName: 'DESKTOP-AK1',
        fingerprintHex: 'ab' * 32,
        pairingCode: '123-456',
        identityCreated: true,
      ),
    );
    await pumpPage(tester, HomePage(service: fake));
    await tester.pump();

    expect(find.byKey(const ValueKey('home_identity_created')), findsOneWidget);
    expect(find.textContaining('首次已创建身份'), findsOneWidget);
  });

  testWidgets('degrades gracefully when the engine cannot read identity',
      (tester) async {
    final fake = FakeTransferService(
      refreshIdentityResult: const DeviceInfo(
        deviceName: 'DESKTOP-AK1',
        fingerprintHex: '',
        pairingCode: '',
      ),
    );
    await pumpPage(tester, HomePage(service: fake));
    await tester.pump();

    expect(find.text('指纹无效'), findsOneWidget);
    expect(find.textContaining('已配对设备：0'), findsOneWidget);
    // Empty pairing code renders as the honest empty snapshot, no crash.
    expect(find.byKey(const ValueKey('home_pairing_code')), findsOneWidget);
  });
}
