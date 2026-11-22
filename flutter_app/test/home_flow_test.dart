/// X4 home page tests (batch X, 2026-11-22, 审核 §4.3 / W 复核 §4): the
/// four entries declare the direction exactly ONCE —
///
/// * 发送文件（主）: multi-select pick FIRST, then a sender-role code
///   (a cancelled pick mints nothing); the code card carries the 已选
///   summary + 更改; once THIS code's connection is established
///   (connected or paired-unreachable) the send starts AUTOMATICALLY,
///   exactly once, with the right connection id and the CURRENT paths;
/// * 发送文件（次）: 输入对方的码 → join as sender (W7.2 auto-open handover);
/// * 接收文件（主）: 输入对方的码 → join as receiver (success waits);
/// * 接收文件（次）: 生成接收码 → creator is receiver.
///
/// Plus the join panel regressions (tolerant input, malformed codes, the
/// W-N9 mapped failure copy with retry), the honest probe verdict, and
/// the cancel/revoke behavior. All driven by the fake service (its
/// joinCode models the cross-device engine: role-less defaults to
/// receiver, same-direction joins are refused — batch X1).
library;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:ak_sending_app/ui/service/file_pick_gateway.dart';
import 'package:ak_sending_app/ui/service/simple_transfer_service.dart';

import 'w28_test_helpers.dart';

/// Opens the join panel via [entryKey], types [code], submits.
Future<void> joinByCode(
  WidgetTester tester, {
  required String entryKey,
  required String code,
}) async {
  await tester.tap(find.byKey(ValueKey(entryKey)));
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 300));
  await tester.enterText(find.byKey(const ValueKey('join_code_field')), code);
  await tester.pump();
  await tester.tap(find.byKey(const ValueKey('join_submit_button')));
  await tester.pump();
}

void main() {
  testWidgets('empty home: the four entries and the empty '
      'connections hint', (tester) async {
    await pumpApp(tester);

    expect(
        find.byKey(const ValueKey('home_send_button')), findsOneWidget);
    expect(find.byKey(const ValueKey('home_receive_button')), findsOneWidget);
    expect(find.byKey(const ValueKey('home_send_join_code_button')),
        findsOneWidget);
    expect(find.byKey(const ValueKey('home_receive_create_code_button')),
        findsOneWidget);
    expect(find.byKey(const ValueKey('connections_empty')), findsOneWidget);
    expect(find.byKey(const ValueKey('connections_header')), findsOneWidget);
  });

  testWidgets('X4 send primary: a cancelled pick mints NO code; a real '
      'pick mints a sender code with the 已选 summary and 更改',
      (tester) async {
    final clipboard = MockClipboard()..install();
    final picker = FakeFilePickGateway(filesResult: const <PickedEntry>[]);
    final fake = await pumpApp(tester, picker: picker);

    // Cancelled initial pick (empty result): no code, home stays.
    await tester.tap(find.byKey(const ValueKey('home_send_button')));
    await pumpEvents(tester);
    expect(fake.createCodeCalls, 0, reason: '取消初选不生码（X4）');
    expect(find.byKey(const ValueKey('home_send_button')), findsOneWidget);
    expect(find.byKey(const ValueKey('flow_pairing_code')), findsNothing);

    // A real pick mints the sender-role code; the card carries the code,
    // copy, summary and 更改.
    picker.filesResult = kTwoFiles;
    await tester.tap(find.byKey(const ValueKey('home_send_button')));
    await pumpEvents(tester);
    final codeText = find.byKey(const ValueKey('flow_pairing_code'));
    expect(codeText, findsOneWidget);
    final selectable = tester.widget<SelectableText>(codeText);
    expect(selectable.style?.fontFamily, 'monospace');
    final shownCode = selectable.data ?? '';
    expect(shownCode, fake.createdCodes.single.code,
        reason: '展示的是引擎真实 AKI1 码（P1-02）');
    expect(shownCode.startsWith('AKI1-'), isTrue);
    expect(fake.createdCodes.single.role, TaskRole.send,
        reason: 'X4：发送主路径生成为发送方的码（方向只声明一次）');
    expect(find.byKey(const ValueKey('flow_copy_code')), findsOneWidget);
    expect(find.byKey(const ValueKey('flow_waiting')), findsOneWidget);
    expect(find.textContaining('把这个码给对方'), findsOneWidget);
    expect(find.textContaining('已选：2 项'), findsOneWidget,
        reason: '码卡片显示已选摘要（X4）');
    expect(find.byKey(const ValueKey('flow_selected_summary')),
        findsOneWidget);
    expect(
        find.byKey(const ValueKey('flow_change_selection_button')),
        findsOneWidget,
        reason: '码卡片提供「更改」（X4）');

    // The copy button writes the code to the clipboard (mocked channel).
    await tester.tap(find.byKey(const ValueKey('flow_copy_code')));
    await tester.pump();
    expect(clipboard.text, fake.createdCodes.single.code);
    await tester.pump(const Duration(seconds: 5));
  });

  testWidgets('X4 send primary: connection established → the send starts '
      'AUTOMATICALLY, exactly once, on the right connection with the '
      'current paths', (tester) async {
    final picker = FakeFilePickGateway(filesResult: kTwoFiles);
    final fake = await pumpApp(tester, picker: picker);

    await tester.tap(find.byKey(const ValueKey('home_send_button')));
    await pumpEvents(tester);
    final connId = fake.simulatePeerJoin(peerName: '小米手机-14');
    await pumpEvents(tester);
    fake.completeProbe(connId);
    await pumpEvents(tester);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    await pumpEvents(tester);

    // The detail page auto-opened with the preselected payload already
    // sending — no pick sheet, no send tap (审核 §4.3).
    expect(find.byKey(const ValueKey('conn_detail_page')), findsOneWidget);
    expect(find.byKey(const ValueKey('pick_files_option')), findsNothing,
        reason: 'X4：主发路径不再弹选择面板——预选内容自动发送');
    expect(find.byKey(const ValueKey('pick_sheet_role_hint')), findsNothing);
    expect(find.text('已选 2 项'), findsOneWidget);
    expect(find.byKey(const ValueKey('conn_send_progress')), findsOneWidget,
        reason: '自动发送已在进行（发送卡进入发送中状态）');

    // Exactly once, on THIS code's connection, with the CURRENT paths.
    expect(fake.sendFilesCalls, 1, reason: '自动发送恰一次（X4）');
    expect(fake.sendRequests.single.$1, connId,
        reason: '自动发送认准本次码建立的连接（防串线）');
    expect(fake.sendRequests.single.$2,
        kTwoFiles.map((entry) => entry.path).toList());

    // Duplicate connection events (extra snapshots, repeated states)
    // never re-send.
    fake.emitProgress(TransferProgress(
      connId: connId,
      phase: TransferPhase.transferring,
      totalBytes: 1024,
      transferredBytes: 512,
    ));
    fake.completeProbe(connId);
    await pumpEvents(tester);
    await pumpEvents(tester);
    expect(fake.sendFilesCalls, 1, reason: '重复事件不得重发（X4）');

    fake.completeSend('发送完成：本机直连');
    await pumpEvents(tester);
    await tester.pump();
    expect(find.byKey(const ValueKey('conn_send_result')), findsOneWidget);
    expect(find.textContaining('发送完成'), findsOneWidget);

    // The engine retires the row once its one transfer finished — then
    // the ended pill + back button show.
    fake.endConnection(connId);
    await pumpEvents(tester);
    await tester.pump();
    expect(find.byKey(const ValueKey('conn_back_home_button')),
        findsOneWidget);

    // Back: the flow reset with the auto-open pop — home actions return.
    await tester
        .tap(find.byKey(const ValueKey('conn_back_home_button')));
    await tester.pump();
    await tester.pump(const Duration(seconds: 1));
    expect(find.byKey(const ValueKey('home_send_button')), findsOneWidget);
    expect(find.byKey(ValueKey('conn_card_name_$connId')), findsNothing,
        reason: 'v1：一次配对一个传输，完成后行退场');
  });

  testWidgets('X4 send primary: a FAILED pairing-time probe does not '
      'block the automatic send（仍可尝试发送）', (tester) async {
    final picker = FakeFilePickGateway(filesResult: kTwoFiles);
    final fake = await pumpApp(tester, picker: picker);

    await tester.tap(find.byKey(const ValueKey('home_send_button')));
    await pumpEvents(tester);
    final connId = fake.simulatePeerJoin(peerName: '办公本-ThinkPad');
    await pumpEvents(tester);
    fake.failProbe(connId);
    await pumpEvents(tester);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    await pumpEvents(tester);

    // Paired-but-unreachable still hands over to the automatic send
    // (the send phase runs its own probe round and dial — CR-P1-07).
    expect(find.byKey(const ValueKey('conn_detail_page')), findsOneWidget,
        reason: 'X4：探测未通过不阻断自动发送');
    expect(fake.sendFilesCalls, 1);
    expect(fake.sendRequests.single.$1, connId);
    expect(find.text('连通测试未通过，仍可尝试发送。'), findsOneWidget);
    // The send failure surfaces through the W3 error card.
    fake.failSend('peer sent error STORAGE_FULL: insufficient free space');
    await pumpEvents(tester);
    await tester.pump();
    expect(find.byKey(const ValueKey('conn_send_error')), findsOneWidget);
    expect(find.text('对方的设备空间不足'), findsOneWidget,
        reason: 'W-N10：发送场景的空间不足文案');
  });

  testWidgets('X4 更改: a cancelled change KEEPS the last selection; a '
      'real pick replaces it (and the auto-send uses the NEW paths)',
      (tester) async {
    final picker = FakeFilePickGateway(filesResult: kTwoFiles);
    final fake = await pumpApp(tester, picker: picker);

    await tester.tap(find.byKey(const ValueKey('home_send_button')));
    await pumpEvents(tester);
    expect(find.textContaining('已选：2 项'), findsOneWidget);

    // Cancelled change: empty result keeps the last selection.
    picker.filesResult = const <PickedEntry>[];
    await tester
        .tap(find.byKey(const ValueKey('flow_change_selection_button')));
    await pumpEvents(tester);
    expect(find.textContaining('已选：2 项'), findsOneWidget,
        reason: '取消更改保留上次选择（X4）');

    // A real pick replaces the selection.
    const replacement = [
      PickedEntry(
          path: 'C:\\docs\\新文件.bin',
          name: '新文件.bin',
          isDirectory: false,
          sizeBytes: 4096),
    ];
    picker.filesResult = replacement;
    await tester
        .tap(find.byKey(const ValueKey('flow_change_selection_button')));
    await pumpEvents(tester);
    expect(find.textContaining('已选：1 项'), findsOneWidget);

    // The automatic send carries the REPLACEMENT paths.
    final connId = fake.simulatePeerJoin();
    await pumpEvents(tester);
    fake.completeProbe(connId);
    await pumpEvents(tester);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    await pumpEvents(tester);
    expect(fake.sendRequests.single.$2, [replacement.single.path]);
  });

  testWidgets('X4 send secondary: 输入对方的码 joins with the send role '
      '(never null) and auto-opens the picker (W7.2)', (tester) async {
    final fake = await pumpApp(tester);

    // The other device minted a 我要接收 code — the send SECONDARY entry
    // declares OUR direction (send) by the entry itself.
    final minted = await fake.createCode(TaskRole.receive);
    expect(minted.code.startsWith('AKI1-'), isTrue);

    await tester.tap(find.byKey(const ValueKey('home_send_join_code_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    expect(find.byKey(const ValueKey('join_code_field')), findsOneWidget);
    expect(find.text('你将把文件发给对方。'), findsOneWidget,
        reason: '面板按入口声明说明方向（X4）');
    expect(find.byKey(const ValueKey('join_role_selector')), findsNothing,
        reason: 'X4：加入面板不再有方向选择');

    // Lowercase + ungrouped + spaced input of the FULL code.
    final tolerant = minted.code.toLowerCase().replaceAll('-', ' ');
    await tester.enterText(
        find.byKey(const ValueKey('join_code_field')), tolerant);
    await tester.pump();
    expect(find.text('36/36 位'), findsOneWidget);

    await tester.tap(find.byKey(const ValueKey('join_submit_button')));
    await tester.pump();
    expect(find.textContaining('正在建立连接'), findsOneWidget);

    fake.completeProbe('conn-1');
    await pumpEvents(tester);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    // W7.2 lives on for the join-send path: auto-open with the picker.
    expect(find.byKey(const ValueKey('conn_detail_page')), findsOneWidget);
    expect(find.byKey(const ValueKey('pick_files_option')), findsOneWidget,
        reason: 'W7.2：次要发送入口沿用自动弹出来源选择');
    expect(find.byKey(const ValueKey('pick_sheet_role_hint')), findsOneWidget);

    // One-time code consumed; the explicit (never-null) send role rode
    // the join.
    expect(fake.pendingCode, isNull);
    expect(fake.joinRequests.single, normalizeJoinCode(minted.code));
    expect(fake.joinRoleRequests.single, TaskRole.send,
        reason: 'X4：方向由入口声明，随 join 传下去且绝不为 null');
  });

  testWidgets('X4 receive primary: 接收文件 opens the code panel '
      'directly, joins with the receive role (never null), and waits',
      (tester) async {
    final fake = await pumpApp(tester);

    final minted = await fake.createCode(TaskRole.send);
    await tester.tap(find.byKey(const ValueKey('home_receive_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    expect(find.text('对方将把文件发给你。'), findsOneWidget);
    expect(find.byKey(const ValueKey('join_role_selector')), findsNothing);

    await tester.enterText(
        find.byKey(const ValueKey('join_code_field')), minted.code);
    await tester.pump();
    await tester.tap(find.byKey(const ValueKey('join_submit_button')));
    await tester.pump();

    fake.completeProbe('conn-1');
    await pumpEvents(tester);
    await tester.pump();
    // W7.2: the RECEIVER-side success waits on the card (no auto-open).
    expect(find.byKey(const ValueKey('flow_success')), findsOneWidget);
    expect(find.text('对方将把文件发给你'), findsOneWidget);
    expect(find.text('等待对方发送文件…'), findsOneWidget);
    expect(find.textContaining('Download/AK-接收'), findsOneWidget);
    expect(find.byKey(const ValueKey('conn_detail_page')), findsNothing);

    expect(fake.joinRoleRequests.single, TaskRole.receive,
        reason: 'X4：接收主路径的 join 角色为接收且绝不为 null');
    expect(fake.joinRequests.single, normalizeJoinCode(minted.code));
  });

  testWidgets('X4 receive secondary: 生成接收码 mints a receiver code '
      'with no selection summary', (tester) async {
    final fake = await pumpApp(tester);

    await tester.tap(
        find.byKey(const ValueKey('home_receive_create_code_button')));
    await pumpEvents(tester);

    expect(find.byKey(const ValueKey('flow_pairing_code')), findsOneWidget);
    expect(fake.createCodeCalls, 1);
    expect(fake.createdCodes.single.role, TaskRole.receive,
        reason: 'X4：接收次要入口生成为接收方的码（方向只声明一次）');
    expect(find.byKey(const ValueKey('flow_selected_summary')), findsNothing,
        reason: '接收码卡片没有已选摘要');
    expect(find.byKey(const ValueKey('flow_change_selection_button')),
        findsNothing);
  });

  testWidgets('join flow failure: mapped W-N9 这个码用不了 card with the '
      'reason in 详情; retry reopens the panel with the same direction',
      (tester) async {
    final fake = await pumpApp(tester);
    fake.nextJoinError = '配对码不存在或已过期';
    final minted = await fake.createCode(TaskRole.receive);

    await joinByCode(tester,
        entryKey: 'home_send_join_code_button', code: minted.code);
    await tester.pump(const Duration(milliseconds: 300));

    // W3/W-N9: the mapped card; the verbatim reason only in 详情.
    expect(find.byKey(const ValueKey('flow_error')), findsOneWidget);
    expect(find.text('这个码用不了'), findsOneWidget);
    expect(find.textContaining('同一个方向'), findsOneWidget,
        reason: 'W-N9：文案列全统一拒绝的原因（含双方同向）');
    expect(find.textContaining('保持应用打开'), findsOneWidget,
        reason: 'W-N9：文案包含对方需保持前台');
    expect(find.textContaining('配对码不存在或已过期'), findsNothing);
    await tester.tap(find.byKey(const ValueKey('flow_error_toggle')));
    await tester.pump();
    expect(find.text('配对码不存在或已过期'), findsOneWidget);

    // Retry returns to the input form with the SAME declared direction
    // (the SEND entry's — the reset must not lose it).
    await tester.tap(find.byKey(const ValueKey('flow_retry_button')));
    await tester.pump();
    expect(find.byKey(const ValueKey('join_code_field')), findsOneWidget);
    expect(find.text('你将把文件发给对方。'), findsOneWidget,
        reason: '重试沿发送次要入口的方向重新打开面板（X4）');
    expect(fake.joinRoleRequests.single, TaskRole.send);
  });

  testWidgets('join submit stays disabled on a malformed AKI1 code with '
      'a counter hint', (tester) async {
    await pumpApp(tester);

    await tester.tap(find.byKey(const ValueKey('home_receive_button')));
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

    await tester.tap(find.byKey(const ValueKey('home_send_join_code_button')));
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

  testWidgets('P2-09: a FAILED probe ends the receive-create flow with '
      'an honest 无法直接连接 verdict (never "连接成功")', (tester) async {
    final fake = await pumpApp(tester);

    await tester.tap(
        find.byKey(const ValueKey('home_receive_create_code_button')));
    await pumpEvents(tester);
    final connId = fake.simulatePeerJoin();
    await pumpEvents(tester);
    fake.failProbe(connId);
    await pumpEvents(tester);
    await tester.pump();

    expect(find.byKey(const ValueKey('flow_error')), findsOneWidget);
    expect(find.textContaining('无法直接连接到对方'), findsOneWidget,
        reason: '审核 §4.5：不可达说「无法直接连接」');
    // The list card carries the honest state chip.
    expect(find.byKey(const ValueKey('conn_state_unreachable')),
        findsOneWidget);
    expect(find.byKey(const ValueKey('flow_success')), findsNothing);
  });

  testWidgets('create flow cancel returns to the four entries without a '
      'connection', (tester) async {
    await pumpApp(tester);

    await tester.tap(
        find.byKey(const ValueKey('home_receive_create_code_button')));
    await pumpEvents(tester);
    expect(find.byKey(const ValueKey('flow_pairing_code')), findsOneWidget);

    await tester.tap(find.byKey(const ValueKey('flow_cancel_button')));
    await tester.pump();
    expect(find.byKey(const ValueKey('home_send_button')), findsOneWidget);
    expect(find.byKey(const ValueKey('flow_pairing_code')), findsNothing);
  });

  testWidgets('P2-04: cancelling the wait REVOKES the unconsumed code '
      'engine-side (disconnect by code_id)', (tester) async {
    final fake = await pumpApp(tester);

    await tester.tap(
        find.byKey(const ValueKey('home_receive_create_code_button')));
    await pumpEvents(tester);
    final code = fake.createdCodes.single;
    expect(code.codeId, isNotNull, reason: 'fake 也上交 code_id（契约先行）');

    await tester.tap(find.byKey(const ValueKey('flow_cancel_button')));
    await pumpEvents(tester);
    await tester.pump();

    expect(fake.disconnectRequests, contains(code.codeId),
        reason: '取消等待必须触发原生撤码调用（disconnect(code_id)）');
    expect(fake.pendingCode, isNull, reason: '被撤销的码不得再可兑换');
    expect(find.byKey(const ValueKey('home_send_button')), findsOneWidget);
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
    expect(find.byKey(const ValueKey('home_send_button')), findsOneWidget);
  });

  testWidgets('network change drops connections: the list follows',
      (tester) async {
    final picker = FakeFilePickGateway(filesResult: kTwoFiles);
    final fake = await pumpApp(tester, picker: picker);

    await tester.tap(find.byKey(const ValueKey('home_send_button')));
    await pumpEvents(tester);
    final connId = fake.simulatePeerJoin();
    await pumpEvents(tester);
    fake.completeProbe(connId);
    await pumpEvents(tester);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    await pumpEvents(tester);
    // X4: the sender success auto-opened the detail page (sending).
    expect(find.byKey(const ValueKey('conn_detail_page')), findsOneWidget);
    await tester.pageBack();
    await tester.pump();
    await tester.pump(const Duration(seconds: 1));
    expect(find.byKey(ValueKey('conn_card_name_$connId')), findsOneWidget);

    // A network change drops everything: the card disappears.
    fake.simulateNetworkChange();
    await pumpEvents(tester);
    await tester.pump();
    expect(find.byKey(ValueKey('conn_card_name_$connId')), findsNothing);
    expect(find.byKey(const ValueKey('connections_empty')), findsOneWidget);
  });
}
