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
    final shownCode = selectable.data ?? '';
    expect(shownCode, fake.createdCodes.single.code,
        reason: '展示的是引擎真实 AKI1 码（P1-02）');
    expect(shownCode.startsWith('AKI1-'), isTrue);
    expect(find.byKey(const ValueKey('flow_copy_code')), findsOneWidget);
    expect(find.byKey(const ValueKey('flow_waiting')), findsOneWidget);
    expect(fake.createdCodes.single.role, TaskRole.send);

    // The copy button writes the code to the clipboard (mocked channel).
    // Pump past the snackbar's 4s dismissal timer so it cannot outlive
    // the test.
    await tester.tap(find.byKey(const ValueKey('flow_copy_code')));
    await tester.pump();
    expect(clipboard.text, fake.createdCodes.single.code,
        reason: '复制按钮写入选中的完整显示码');
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

  testWidgets('join flow: full AKI1 code with case/hyphen/space '
      'tolerance, explicit direction, probe, success',
      (tester) async {
    final fake = await pumpApp(tester);

    // The other device minted a "我要接收" code — we join it declaring
    // OUR intent (send), so the connection direction is send.
    final minted = await fake.createCode(TaskRole.receive);
    expect(minted.code.startsWith('AKI1-'), isTrue);

    await tester.tap(find.byKey(const ValueKey('home_join_code_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    // Lowercase + ungrouped + spaced input of the FULL code: the
    // formatter uppercases, the controller strips separators.
    final tolerant = minted.code.toLowerCase().replaceAll('-', ' ');
    await tester.enterText(
        find.byKey(const ValueKey('join_code_field')), tolerant);
    await tester.pump();
    expect(find.text('36/36 位'), findsOneWidget);

    // The join direction picker (P1-05): submit stays disabled until the
    // user declares their intent.
    final submitDisabled = tester.widget<FilledButton>(
        find.byKey(const ValueKey('join_submit_button')));
    expect(submitDisabled.onPressed, isNull);
    await tester.tap(find.descendant(
        of: find.byKey(const ValueKey('join_role_selector')),
        matching: find.text('发送文件')));
    await tester.pump();

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
    // One-time code: consumed by the join; the typed (normalized) form
    // reached the service with the explicit role.
    expect(fake.pendingCode, isNull);
    expect(fake.joinRequests.single, normalizeJoinCode(minted.code));
    expect(fake.joinRoleRequests.single, TaskRole.send);
  });

  testWidgets('R36-P2-03: the engine-failure latch shows the recoverable '
      'engine-failed card on the home page', (tester) async {
    final fake = await pumpApp(tester);
    expect(find.byKey(const ValueKey('engine_failed_card')), findsNothing,
        reason: '健康时不显示故障卡');

    fake.failEngine('连续 10 次任务事件轮询失败（fake: poll_event 持续故障）。');
    await pumpEvents(tester);

    expect(find.byKey(const ValueKey('engine_failed_card')), findsOneWidget,
        reason: '闩上必须达 UI——主页出现故障卡');
    expect(find.textContaining('传输引擎故障'), findsOneWidget);
    expect(find.textContaining('请重启应用'), findsOneWidget,
        reason: '可恢复提示：重启应用');
    expect(find.byKey(const ValueKey('engine_failed_card_detail')),
        findsOneWidget);
    expect(find.textContaining('连续 10 次'), findsOneWidget,
        reason: '故障原因原文可见');
    // The page stays usable (navigable degraded state, like the startup
    // error card) — the actions remain, the card sits above them.
    expect(
        find.byKey(const ValueKey('home_create_code_button')), findsOneWidget);
  });

  testWidgets('join flow failure: readable reason, retry reopens the '
      'input panel', (tester) async {
    final fake = await pumpApp(tester);
    fake.nextJoinError = '配对码不存在或已过期';
    final minted = await fake.createCode(TaskRole.send);

    await tester.tap(find.byKey(const ValueKey('home_join_code_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    await tester.enterText(
        find.byKey(const ValueKey('join_code_field')), minted.code);
    await tester.pump();
    await tester.tap(find.descendant(
        of: find.byKey(const ValueKey('join_role_selector')),
        matching: find.text('接收文件')));
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
    expect(fake.joinRequests.single, normalizeJoinCode(minted.code));
  });

  testWidgets('join submit stays disabled on a malformed AKI1 code with '
      'a counter hint', (tester) async {
    await pumpApp(tester);

    await tester.tap(find.byKey(const ValueKey('home_join_code_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    // Old 8-char shape and a truncated AKI1 form are both rejected.
    await tester.enterText(
        find.byKey(const ValueKey('join_code_field')), 'AB-12');
    await tester.pump();
    final submit = tester.widget<FilledButton>(
        find.byKey(const ValueKey('join_submit_button')));
    expect(submit.onPressed, isNull);
    expect(find.textContaining('4/36 位'), findsOneWidget);

  });

  testWidgets('join submit stays disabled one symbol short of the real '
      'AKI1 format', (tester) async {
    final fake = await pumpApp(tester);
    final minted = await fake.createCode(TaskRole.send);

    await tester.tap(find.byKey(const ValueKey('home_join_code_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    // 43 of 44 display characters: one symbol short of the real format.
    await tester.enterText(
        find.byKey(const ValueKey('join_code_field')),
        minted.code.substring(0, minted.code.length - 1));
    await tester.pump();
    final shortSubmit = tester.widget<FilledButton>(
        find.byKey(const ValueKey('join_submit_button')));
    expect(shortSubmit.onPressed, isNull);
    expect(find.textContaining('35/36 位'), findsOneWidget);
  });

  testWidgets('P2-09: a FAILED probe ends the flow with an honest '
      '不可达 verdict (never "连接成功")', (tester) async {
    final fake = await pumpApp(tester);

    await chooseRole(tester, const ValueKey('role_send_button'));
    final connId = fake.simulatePeerJoin();
    await pumpEvents(tester);
    fake.failProbe(connId);
    await pumpEvents(tester);
    await tester.pump();

    expect(find.byKey(const ValueKey('flow_error')), findsOneWidget);
    expect(find.textContaining('连通测试未通过'), findsOneWidget);
    // The list card carries the honest state chip.
    expect(find.byKey(const ValueKey('conn_state_unreachable')),
        findsOneWidget);
    expect(find.byKey(const ValueKey('flow_success')), findsNothing);
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

  testWidgets('P2-04: cancelling the wait REVOKES the unconsumed code '
      'engine-side (disconnect by code_id)', (tester) async {
    final fake = await pumpApp(tester);

    await chooseRole(tester, const ValueKey('role_receive_button'));
    final code = fake.createdCodes.single;
    expect(code.codeId, isNotNull, reason: 'fake 也上交 code_id（契约先行）');

    await tester.tap(find.byKey(const ValueKey('flow_cancel_button')));
    await pumpEvents(tester);
    await tester.pump();

    expect(fake.disconnectRequests, contains(code.codeId),
        reason: '取消等待必须触发原生撤码调用（disconnect(code_id)）');
    expect(fake.pendingCode, isNull, reason: '被撤销的码不得再可兑换');
    expect(find.byKey(const ValueKey('home_create_code_button')),
        findsOneWidget);
  });

  testWidgets('R29-P2-04: re-tapping the selected role deselects cleanly '
      '(no empty-set crash) and the submit disables until re-picked',
      (tester) async {
    final fake = await pumpApp(tester);
    final minted = await fake.createCode(TaskRole.send);

    await tester.tap(find.byKey(const ValueKey('home_join_code_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    await tester.enterText(
        find.byKey(const ValueKey('join_code_field')), minted.code);
    await tester.pump();

    final segment = find.descendant(
        of: find.byKey(const ValueKey('join_role_selector')),
        matching: find.text('发送文件'));
    await tester.tap(segment);
    await tester.pump();
    var submit = tester.widget<FilledButton>(
        find.byKey(const ValueKey('join_submit_button')));
    expect(submit.onPressed, isNotNull, reason: '选定角色后提交可用');

    // Re-tap the SAME segment: the empty selection must resolve to a null
    // role (never `selection.first` on an empty set), and the submit
    // disables — an empty selection can never enter the join.
    await tester.tap(segment);
    await tester.pump();
    expect(tester.takeException(), isNull,
        reason: '再次点击已选角色不得抛出空集合异常（R29-P2-04）');
    submit = tester.widget<FilledButton>(
        find.byKey(const ValueKey('join_submit_button')));
    expect(submit.onPressed, isNull,
        reason: '取消选择后提交必须禁用（空选择不得进入配对）');
    expect(fake.joinCodeCalls, 0, reason: '空选择绝不应发起 join');

    // Re-picking restores the submit.
    await tester.tap(segment);
    await tester.pump();
    submit = tester.widget<FilledButton>(
        find.byKey(const ValueKey('join_submit_button')));
    expect(submit.onPressed, isNotNull, reason: '重新选定后提交恢复可用');
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
