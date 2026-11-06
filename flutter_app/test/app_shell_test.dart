/// Wave-28 shell: exactly two first-level destinations (连接 / 设置),
/// mobile NavigationBar at phone widths + NavigationRail on wide
/// surfaces, state kept alive across tab switches, the receive banner
/// above BOTH tabs, and the 390dp overflow regression.
library;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:ak_sending_app/ui/service/simple_transfer_service.dart';

import 'w28_test_helpers.dart';

void main() {
  testWidgets('two destinations only; switching keeps page state alive '
      '(wide rail)', (tester) async {
    await pumpApp(tester, size: kWideSize);

    expect(find.byType(NavigationRail), findsOneWidget);
    expect(find.byType(NavigationBar), findsNothing);
    final rail = tester.widget<NavigationRail>(find.byType(NavigationRail));
    expect(rail.destinations.length, 2);

    // Settings is the second destination.
    await tester.tap(find.text('设置'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    expect(find.byKey(const ValueKey('settings_page')), findsOneWidget);

    // Back to 连接: the home actions are still there (IndexedStack).
    await tester.tap(find.text('连接'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    expect(find.byKey(const ValueKey('home_create_code_button')),
        findsOneWidget);
  });

  testWidgets('390dp phone regression: bottom NavigationBar, both tabs '
      'render with no overflow exceptions', (tester) async {
    await pumpApp(tester, size: kPhoneSize);

    expect(find.byType(NavigationBar), findsOneWidget);
    expect(find.byType(NavigationRail), findsNothing);
    expect(find.byKey(const ValueKey('home_create_code_button')),
        findsOneWidget);
    expect(tester.takeException(), isNull);

    // Settings also fits.
    await tester.tap(find.text('设置'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    expect(find.byKey(const ValueKey('settings_page')), findsOneWidget);
    expect(tester.takeException(), isNull);

    await tester.tap(find.text('连接'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    expect(tester.takeException(), isNull);
  });

  testWidgets('390dp phone regression: the whole create flow fits '
      '(code display + waiting)', (tester) async {
    final fake = await pumpApp(tester, size: kPhoneSize);

    await tester.tap(find.byKey(const ValueKey('home_create_code_button')));
    await tester.pump();
    await tester.pump(const Duration(seconds: 1));
    await tester.tap(find.byKey(const ValueKey('role_send_button')));
    await tester.pump();
    await tester.pump(const Duration(seconds: 1));

    expect(find.byKey(const ValueKey('flow_pairing_code')), findsOneWidget);
    expect(tester.takeException(), isNull);

    // Pairing + probe to success still fits at phone width.
    final id = fake.simulatePeerJoin(peerName: '特别长特别长的对端设备名称X');
    await pumpEvents(tester);
    await tester.pump();
    fake.completeProbe(id);
    await pumpEvents(tester);
    await tester.pump();
    expect(find.byKey(const ValueKey('flow_success')), findsOneWidget);
    expect(tester.takeException(), isNull);

    await tester.tap(find.byKey(const ValueKey('flow_done_button')));
    await tester.pump();
    expect(find.byKey(ValueKey('conn_card_name_$id')), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  testWidgets('390dp phone regression: connection detail with a picked '
      'file list does not overflow', (tester) async {
    final picker = FakeFilePickGateway(filesResult: kTwoFiles);
    final fake = await pumpApp(tester, size: kPhoneSize, picker: picker);

    await fake.createCode(TaskRole.send);
    final id = fake.simulatePeerJoin(peerName: '办公本-ThinkPad');
    await pumpEvents(tester);
    await tester.pump();
    fake.completeProbe(id);
    await pumpEvents(tester);
    await tester.pump();

    await tester.tap(find.byKey(ValueKey('conn_card_name_$id')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));

    await tester.tap(find.byKey(const ValueKey('conn_pick_files_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    await tester.tap(find.byKey(const ValueKey('pick_files_option')));
    await tester.pump();
    expect(find.text('已选 2 项'), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  testWidgets('the receive banner shows above both tabs', (tester) async {
    final fake = await pumpApp(tester, size: kWideSize);

    fake.emitIncomingRequest(fileCount: 2, totalBytes: 2048);
    await pumpEvents(tester);
    await tester.pump();
    expect(find.byKey(const ValueKey('incoming_host')), findsOneWidget);

    // Still visible after switching to 设置.
    await tester.tap(find.text('设置'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    expect(find.byKey(const ValueKey('incoming_host')), findsOneWidget);
    expect(find.byKey(const ValueKey('incoming_accept_job-1')),
        findsOneWidget);
  });
}
