/// Wave-28 home page: empty state, 发起流 (create code → peer joins →
/// auto probe → success) and 加入流 (tolerant input → join → probe →
/// success; readable failure + retry). All driven by the fake service.
library;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:ak_sending_app/ui/service/simple_transfer_service.dart';

import 'w28_test_helpers.dart';

/// Opens the role sheet from the home action and picks [key].
Future<void> chooseRole(WidgetTester tester, Key key) async {
  await tester.tap(find.byKey(const ValueKey('home_create_code_button')));
  await tester.pump();
  await tester.pump(const Duration(seconds: 1));
  await tester.tap(find.byKey(key));
  await tester.pump();
  await tester.pump(const Duration(seconds: 1));
}

void main() {
  testWidgets('empty home: the two big actions and the empty '
      'connections hint', (tester) async {
    await pumpApp(tester);

    expect(find.byKey(const ValueKey('home_create_code_button')),
        findsOneWidget);
    expect(
        find.byKey(const ValueKey('home_join_code_button')), findsOneWidget);
    expect(find.byKey(const ValueKey('connections_empty')), findsOneWidget);
    expect(find.byKey(const ValueKey('connections_header')), findsOneWidget);
  });

  testWidgets('create flow: role sheet → big monospace code with copy → '
      'peer join → probe phases → 连接成功 → card listed', (tester) async {
    final clipboard = MockClipboard()..install();
    final fake = await pumpApp(tester);

    await chooseRole(tester, const ValueKey('role_send_button'));

    // Code shown, monospace + copy button + waiting animation.
    final codeText = find.byKey(const ValueKey('flow_pairing_code'));
    expect(codeText, findsOneWidget);
    final selectable = tester.widget<SelectableText>(codeText);
    expect(selectable.style?.fontFamily, 'monospace');
    expect(selectable.data, 'AK01-0001');
    expect(find.byKey(const ValueKey('flow_copy_code')), findsOneWidget);
    expect(find.byKey(const ValueKey('flow_waiting')), findsOneWidget);
    expect(fake.createdCodes.single.role, TaskRole.send);

    // The copy button writes the code to the clipboard (mocked channel).
    // Pump past the snackbar's 4s dismissal timer so it cannot outlive
    // the test.
    await tester.tap(find.byKey(const ValueKey('flow_copy_code')));
    await tester.pump();
    expect(clipboard.text, 'AK01-0001');
    await tester.pump(const Duration(seconds: 5));

    // Peer joins → connection appears, flow moves to 建立连接.
    final connId = fake.simulatePeerJoin(peerName: '小米手机-14');
    await pumpEvents(tester);
    await tester.pump();
    expect(find.textContaining('正在建立连接'), findsOneWidget);

    // Probe phases, then success.
    fake.beginProbe(connId);
    await pumpEvents(tester);
    await tester.pump();
    expect(find.textContaining('正在测试连通性'), findsOneWidget);
    fake.completeProbe(connId);
    await pumpEvents(tester);
    await tester.pump();
    expect(find.byKey(const ValueKey('flow_success')), findsOneWidget);
    expect(find.byKey(const ValueKey('flow_done_button')), findsOneWidget);

    // Done → back to the actions; the connection is listed with its
    // frozen direction (I declared send).
    await tester.tap(find.byKey(const ValueKey('flow_done_button')));
    await tester.pump();
    expect(find.byKey(const ValueKey('home_create_code_button')),
        findsOneWidget);
    expect(find.byKey(ValueKey('conn_card_name_$connId')),
        findsOneWidget);
    expect(find.text('我发送 → 对方'), findsOneWidget);
  });

  testWidgets('join flow: hyphen/lowercase tolerance, probe, success; '
      'joiner direction complements the creator declaration',
      (tester) async {
    final fake = await pumpApp(tester);

    // The other device minted a "我要接收" code — we joined it, so OUR
    // connection direction is send.
    await fake.createCode(TaskRole.receive);

    await tester.tap(find.byKey(const ValueKey('home_join_code_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    // Lowercase + hyphenated input: the formatter uppercases, the
    // controller strips the hyphen before joinCode.
    await tester.enterText(
        find.byKey(const ValueKey('join_code_field')), 'ak01-0001');
    await tester.pump();
    expect(find.text('8/8 位'), findsOneWidget);

    await tester.tap(find.byKey(const ValueKey('join_submit_button')));
    await tester.pump();
    expect(find.textContaining('正在建立连接'), findsOneWidget);

    fake.completeProbe('conn-1');
    await pumpEvents(tester);
    await tester.pump();
    expect(find.byKey(const ValueKey('flow_success')), findsOneWidget);

    await tester.tap(find.byKey(const ValueKey('flow_done_button')));
    await tester.pump();
    expect(find.text('我发送 → 对方'), findsOneWidget);
    // One-time code: consumed by the join.
    expect(fake.pendingCode, isNull);
    expect(fake.joinRequests.single, 'AK010001');
  });

  testWidgets('join flow failure: readable reason, retry reopens the '
      'input panel', (tester) async {
    final fake = await pumpApp(tester);
    fake.nextJoinError = '配对码不存在或已过期';

    await tester.tap(find.byKey(const ValueKey('home_join_code_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    await tester.enterText(
        find.byKey(const ValueKey('join_code_field')), 'ZZZZ-9999');
    await tester.pump();
    await tester.tap(find.byKey(const ValueKey('join_submit_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.byKey(const ValueKey('flow_error')), findsOneWidget);
    expect(find.textContaining('配对码不存在或已过期'), findsOneWidget);

    // Retry returns to the input form (not the two buttons).
    await tester.tap(find.byKey(const ValueKey('flow_retry_button')));
    await tester.pump();
    expect(find.byKey(const ValueKey('join_code_field')), findsOneWidget);
    expect(fake.joinRequests.single, 'ZZZZ9999');
  });

  testWidgets('join submit stays disabled below 8 chars with a counter '
      'hint', (tester) async {
    await pumpApp(tester);

    await tester.tap(find.byKey(const ValueKey('home_join_code_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    await tester.enterText(
        find.byKey(const ValueKey('join_code_field')), 'AB-12');
    await tester.pump();
    final submit = tester.widget<FilledButton>(
        find.byKey(const ValueKey('join_submit_button')));
    expect(submit.onPressed, isNull);
    expect(find.text('4/8 位'), findsOneWidget);
  });

  testWidgets('create flow cancel returns to the two actions without a '
      'connection', (tester) async {
    await pumpApp(tester);

    await chooseRole(tester, const ValueKey('role_receive_button'));
    expect(find.byKey(const ValueKey('flow_pairing_code')), findsOneWidget);

    await tester.tap(find.byKey(const ValueKey('flow_cancel_button')));
    await tester.pump();
    expect(find.byKey(const ValueKey('home_create_code_button')),
        findsOneWidget);
    expect(find.byKey(const ValueKey('flow_pairing_code')), findsNothing);
  });

  testWidgets('network change drops connections: the list follows',
      (tester) async {
    final fake = await pumpApp(tester);

    await chooseRole(tester, const ValueKey('role_send_button'));
    final connId = fake.simulatePeerJoin();
    await pumpEvents(tester);
    await tester.pump();
    fake.completeProbe(connId);
    await pumpEvents(tester);
    await tester.pump();
    await tester.tap(find.byKey(const ValueKey('flow_done_button')));
    await tester.pump();
    expect(find.byKey(ValueKey('conn_card_name_$connId')),
        findsOneWidget);

    // A network change drops everything: the card disappears.
    fake.simulateNetworkChange();
    await pumpEvents(tester);
    await tester.pump();
    expect(find.byKey(ValueKey('conn_card_name_$connId')),
        findsNothing);
    expect(find.byKey(const ValueKey('connections_empty')), findsOneWidget);
  });
}
