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
///
/// Batch X codex-review fixes (P1) — each pinned by the tests named
/// `P1-1:` / `P1-2:` below:
///
/// * P1-1: the preselected payload goes ONLY to the connection the shown
///   code produced (an unrelated pairing landing meanwhile is ignored; a
///   same-engine mirror of the same code still completes the flow), and
///   the code card's 更改 pick never lets the automatic send leave with
///   the OLD selection while a new one is being chosen;
/// * P1-2: a failed create retries with the direction it declared (a
///   接收-create retry never becomes a send-create), and the PRODUCTION
///   pick gateway really offers a whole folder after the file pick.
///
/// Third-review fixes — pinned by the tests named `P1-A:` below: a
/// picker left open across a flow reset (取消配对) or another entry's
/// flow beginning is discarded by the flow-generation guard — its
/// selection never lands in the NEXT flow's state (B's auto-send would
/// otherwise carry A's files over B's connection).
library;

import 'dart:async' show Completer, StreamController, StreamSubscription;
import 'dart:typed_data' show Uint8List;

import 'package:file_picker/file_picker.dart' as fp;
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:ak_sending_app/services/simple_transfer_fake.dart';
import 'package:ak_sending_app/ui/app.dart';
import 'package:ak_sending_app/ui/pages/connection_detail_page.dart';
import 'package:ak_sending_app/ui/service/file_pick_gateway.dart';
import 'package:ak_sending_app/ui/service/pairing_flow_controller.dart';
import 'package:ak_sending_app/ui/service/save_dir_store.dart';
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

  testWidgets('P2-E: a TTL-expired shown code resets the waiting flow and '
      'surfaces the 配对码已过期 card (W-N9-family copy)', (tester) async {
    final picker = FakeFilePickGateway(filesResult: kTwoFiles);
    final fake = await pumpApp(tester, picker: picker);

    // The primary send path waits on its code (the waiting card with the
    // 已选 summary — the shape that used to wait forever past the TTL).
    await tester.tap(find.byKey(const ValueKey('home_send_button')));
    await pumpEvents(tester);
    final codeId = fake.createdCodes.single.codeId;
    expect(codeId, isNotNull);
    expect(find.byKey(const ValueKey('flow_pairing_code')), findsOneWidget);
    expect(find.byKey(const ValueKey('flow_code_expired')), findsNothing);

    // The service purges the awaited code at its TTL and announces it.
    fake.expireCode(codeId!);
    await pumpEvents(tester);
    await tester.pump();

    expect(find.byKey(const ValueKey('flow_pairing_code')), findsNothing,
        reason: 'P2-E：过期码不得继续当作等待卡展示');
    expect(find.byKey(const ValueKey('flow_waiting')), findsNothing);
    expect(find.byKey(const ValueKey('flow_code_expired')), findsOneWidget,
        reason: 'P2-E：过期事实以失败卡形式可见');
    expect(find.text('配对码已过期'), findsOneWidget);
    expect(find.textContaining('请重新生成'), findsOneWidget);
    expect(find.byKey(const ValueKey('home_send_button')), findsOneWidget,
        reason: 'P2-E：流程已复位——主入口立即可用');
    expect(find.textContaining('已选：'), findsNothing,
        reason: '复位后不再残留已选摘要');

    // The notice retires when the next flow begins.
    await tester.tap(find.byKey(const ValueKey('home_send_button')));
    await pumpEvents(tester);
    expect(find.byKey(const ValueKey('flow_code_expired')), findsNothing,
        reason: 'P2-E：新流程开始即撤下过期通知卡');
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

  // -----------------------------------------------------------------------
  // Batch X codex review, P1-1: WHICH connection the preselected payload
  // goes to, and what the 更改 pick may hand over.
  // -----------------------------------------------------------------------

  for (final foreignRole in TaskRole.values) {
    testWidgets('P1-1: a create flow latches ONLY the connection its own '
        'code produced — an unrelated ${foreignRole.name} pairing that '
        'lands meanwhile is ignored and never auto-sent to', (tester) async {
      final picker = FakeFilePickGateway(filesResult: kTwoFiles);
      final service = await _pumpWith(tester,
          service: _ForeignConnectionService(), picker: picker);

      // 发送文件 main path: the files are preselected, OUR code is minted
      // and shown, and the flow waits for the peer.
      await tester.tap(find.byKey(const ValueKey('home_send_button')));
      await pumpEvents(tester);
      final ours = service.createdCodes.single.code;

      // Meanwhile ANOTHER pairing lands on this device — a different code,
      // already live. In the send direction this is exactly the row the
      // old first-unseen latch grabbed (and then auto-sent to).
      final foreign = service.addForeignConnection(
          code: _otherCode(ours), role: foreignRole);
      await pumpEvents(tester);
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));
      await pumpEvents(tester);

      // Still waiting on OUR code: the unrelated pairing is listed like
      // any other connection, but nothing was latched, opened or sent.
      expect(find.byKey(const ValueKey('flow_pairing_code')), findsOneWidget,
          reason: 'P1-1：别的配对码建立的连接不得完成本次发起流');
      expect(find.byKey(ValueKey('conn_card_name_$foreign')), findsOneWidget,
          reason: '无关连接照常列在「当前连接」里');
      expect(find.byKey(const ValueKey('conn_detail_page')), findsNothing);
      expect(service.sendFilesCalls, 0,
          reason: 'P1-1：预选文件绝不能发给别的连接');

      // OUR peer joins: the row carrying OUR code completes the flow.
      final connId = service.simulatePeerJoin(peerName: '小米手机-14');
      await pumpEvents(tester);
      expect(find.textContaining('已配对，正在建立连接'), findsOneWidget,
          reason: 'P1-1：同一个码建立的连接才完成发起流');
      service.completeProbe(connId);
      await pumpEvents(tester);
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));
      await pumpEvents(tester);

      expect(find.byKey(const ValueKey('conn_detail_page')), findsOneWidget);
      expect(service.sendFilesCalls, 1, reason: '自动发送恰一次');
      expect(service.sendRequests.single.$1, connId,
          reason: 'P1-1：只发给本次码建立的连接');
      expect(service.sendRequests.single.$1, isNot(foreign),
          reason: 'P1-1：无关连接从未收到文件');
      expect(service.sendRequests.single.$2,
          kTwoFiles.map((entry) => entry.path).toList());
      expect(service.connectionsSnapshot.single.code, ours,
          reason: '假服务的行带码（真实适配器同样盖戳）');
    });
  }

  testWidgets('P1-1: a same-engine MIRROR row — the SAME code string — '
      'still completes the create flow', (tester) async {
    final picker = FakeFilePickGateway(filesResult: kTwoFiles);
    final fake = await pumpApp(tester, picker: picker);

    await tester.tap(find.byKey(const ValueKey('home_send_button')));
    await pumpEvents(tester);
    final ours = fake.createdCodes.single.code;

    // The join of OUR code, mirrored on this engine: ONE row, carrying
    // the same code string (what the real adapter stamps on both sides).
    final joined = await fake.joinCode(ours, role: TaskRole.receive);
    expect(joined.ok, isTrue);
    expect(fake.connectionsSnapshot.single.code, ours);

    // The create flow completed on that row instead of waiting forever.
    await pumpEvents(tester);
    expect(find.textContaining('已配对，正在建立连接'), findsOneWidget,
        reason: 'P1-1：同码的镜像连接照常完成发起流（关联规则不误伤）');
    // The waiting card is really gone once the 200 ms switcher faded it.
    await tester.pump(const Duration(milliseconds: 400));
    expect(find.byKey(const ValueKey('flow_pairing_code')), findsNothing);
    fake.completeProbe(joined.connectionId!);
    await pumpEvents(tester);
    await tester.pump();
    expect(find.byKey(const ValueKey('flow_success')), findsOneWidget);
    // The mirror row carries the JOINER's direction (receive): the flow
    // ends on the waiting card, nothing is auto-sent.
    expect(fake.sendFilesCalls, 0);
  });

  test('P1-1 fallback: when the created code is UNKNOWN, the first unseen '
      'connection still completes the create flow (the pre-P1-1 rule)',
      () async {
    final fake = LocalSimpleTransferService();
    addTearDown(fake.dispose);
    final controller = PairingFlowController(fake);
    addTearDown(controller.dispose);

    // An implementation that cannot name the created code: the flow waits
    // for its peer but `code` never arrives. Unreachable with the current
    // model (`TaskCode.code` is non-null), hence driven through the
    // controller's own state instead of `startCreate`.
    final minted = await fake.createCode(TaskRole.send);
    controller
      ..kind = PairingFlowKind.create
      ..phase = PairingFlowPhase.awaitingPeer;
    await Future<void>.delayed(Duration.zero);

    final id = fake.simulatePeerJoin();
    await Future<void>.delayed(Duration.zero);

    expect(controller.code, isNull);
    expect(controller.connection?.id, id,
        reason: '没有可关联的码时沿用「第一个未见过的连接」');
    expect(controller.phase, PairingFlowPhase.paired);
    expect(fake.connectionsSnapshot.single.code, minted.code,
        reason: '行本身带着码——只是发起流这一侧不知道自己建的码');
  });

  for (final reachable in [true, false]) {
    final verdict = reachable ? 'completes' : 'ends unreachable';

    testWidgets('P1-1 更改 race: the connection $verdict DURING the change '
        'pick — the handover waits for the pick and sends the NEW '
        'selection, once', (tester) async {
      final gateway = _HoldingPickGateway();
      final fake = await pumpApp(tester, picker: gateway);
      await _openChangePick(tester, gateway);

      // The peer joins behind the open system picker, and the connection
      // is established (connected, or the paired-but-unreachable verdict
      // that counts as established for the send).
      final connId = fake.simulatePeerJoin(peerName: '小米手机-14');
      await pumpEvents(tester);
      if (reachable) {
        fake.completeProbe(connId);
      } else {
        fake.failProbe(connId);
      }
      await pumpEvents(tester);
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));
      await pumpEvents(tester);

      // NOTHING leaves with the OLD selection while the pick is open.
      expect(find.byKey(const ValueKey('conn_detail_page')), findsNothing,
          reason: 'P1-1：更改进行中不得用旧选择交接');
      expect(fake.sendFilesCalls, 0,
          reason: 'P1-1：更改进行中不得发出旧选择的文件');

      // The pick returns a NEW selection: the deferred handover runs now,
      // with the NEW entries — exactly once.
      gateway.answer(_replacement);
      await pumpEvents(tester);
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));
      await pumpEvents(tester);

      expect(find.byKey(const ValueKey('conn_detail_page')), findsOneWidget,
          reason: 'P1-1：选择返回后补做交接');
      expect(find.text('已选 1 项'), findsOneWidget,
          reason: 'P1-1：交接的是新选择');
      expect(find.byKey(const ValueKey('picked_item_1')), findsNothing,
          reason: 'P1-1：旧选择没有混进来');
      expect(fake.sendFilesCalls, 1, reason: '自动发送恰一次');
      expect(fake.sendRequests.single.$1, connId);
      expect(fake.sendRequests.single.$2, [_replacement.single.path]);
    });

    testWidgets('P1-1 更改 race: the connection $verdict DURING a change '
        'pick that is then CANCELLED — the KEPT selection is sent, once',
        (tester) async {
      final gateway = _HoldingPickGateway();
      final fake = await pumpApp(tester, picker: gateway);
      await _openChangePick(tester, gateway);

      final connId = fake.simulatePeerJoin();
      await pumpEvents(tester);
      if (reachable) {
        fake.completeProbe(connId);
      } else {
        fake.failProbe(connId);
      }
      await pumpEvents(tester);
      await tester.pump(const Duration(milliseconds: 400));
      expect(fake.sendFilesCalls, 0,
          reason: '更改进行中：即使连接已就绪也不先用旧选择发送');

      // The pick is cancelled: the last selection stays — and that kept
      // selection is what the deferred handover sends, once.
      gateway.answer(const <PickedEntry>[]);
      await pumpEvents(tester);
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));
      await pumpEvents(tester);

      expect(find.byKey(const ValueKey('conn_detail_page')), findsOneWidget,
          reason: '取消更改后允许交接');
      expect(find.text('已选 2 项'), findsOneWidget,
          reason: '取消更改保留上次选择');
      expect(fake.sendFilesCalls, 1, reason: '自动发送恰一次');
      expect(fake.sendRequests.single.$1, connId);
      expect(fake.sendRequests.single.$2,
          kTwoFiles.map((entry) => entry.path).toList());
    });
  }

  testWidgets('P1-1 更改 race: the pick returns FIRST — the selection is '
      'replaced and the later connection hands over THAT selection once '
      '(the deferral ends with the pick)', (tester) async {
    final gateway = _HoldingPickGateway();
    final fake = await pumpApp(tester, picker: gateway);
    await _openChangePick(tester, gateway);

    gateway.answer(_replacement);
    await pumpEvents(tester);
    expect(find.textContaining('已选：1 项'), findsOneWidget);
    expect(find.byKey(const ValueKey('conn_detail_page')), findsNothing,
        reason: '尚未连接：不交接');

    final connId = fake.simulatePeerJoin();
    await pumpEvents(tester);
    fake.completeProbe(connId);
    await pumpEvents(tester);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    await pumpEvents(tester);

    expect(find.byKey(const ValueKey('conn_detail_page')), findsOneWidget,
        reason: '更改已结束：交接不再推迟');
    expect(fake.sendFilesCalls, 1, reason: '自动发送恰一次');
    expect(fake.sendRequests.single.$2, [_replacement.single.path]);
  });

  testWidgets('P1-1 更改 race: a 更改 tap landing in the very frame the '
      'connection completes starts NO pick — the selection already belongs '
      'to the connection page', (tester) async {
    final gateway = _HoldingPickGateway();
    final fake = await pumpApp(tester, picker: gateway);
    await tester.tap(find.byKey(const ValueKey('home_send_button')));
    await tester.pump();
    gateway.answer(kTwoFiles);
    await pumpEvents(tester);
    expect(gateway.openPicks, 0);

    // Finger DOWN on 更改 — no frame in between, the button is still the
    // one on screen.
    final gesture = await tester.createGesture();
    await gesture.down(tester.getCenter(
        find.byKey(const ValueKey('flow_change_selection_button'))));

    // The peer joins and completes BEFORE the finger lifts (microtasks
    // only — still no frame): the handover takes the selection right here.
    final connId = fake.simulatePeerJoin();
    fake.completeProbe(connId);
    await tester.idle();

    // Finger UP: a tap on the not-yet-rebuilt button. Whichever came first
    // wins — the completion beat the pick, so the tap must not open a
    // picker behind the connection page (its result could be applied
    // nowhere: the selection is already that page's payload).
    await gesture.up();
    await pumpEvents(tester);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    await pumpEvents(tester);

    expect(gateway.openPicks, 0,
        reason: 'P1-1：交接之后不再为「更改」打开选择器');
    expect(find.byKey(const ValueKey('conn_detail_page')), findsOneWidget);
    expect(fake.sendFilesCalls, 1, reason: '自动发送恰一次');
    expect(fake.sendRequests.single.$2,
        kTwoFiles.map((entry) => entry.path).toList());
  });

  testWidgets('P1-1 更改 race: the connection DROPS during the change '
      'pick — nothing is handed over, and the pick opened before the drop '
      'is DISCARDED (P1-D): the retry carries the KEPT selection', (tester) async {
    final gateway = _HoldingPickGateway();
    final fake = await pumpApp(tester, picker: gateway);
    await _openChangePick(tester, gateway);

    final connId = fake.simulatePeerJoin();
    await pumpEvents(tester);
    fake.simulateNetworkChange();
    await pumpEvents(tester);
    await tester.pump();
    expect(find.textContaining('与对方的连接断开了'), findsOneWidget);
    expect(find.byKey(ValueKey('conn_card_name_$connId')), findsNothing,
        reason: '网络变化：那条连接的卡片也退场了');

    // The pick returns with a NEW selection — but it was opened by the
    // flow that just FAILED (P1-D: `_fail` moves the generation), so it
    // must be discarded whenever it returns: never into the failed flow,
    // never into its retry.
    gateway.answer(_replacement);
    await pumpEvents(tester);
    await tester.pump();

    expect(find.byKey(const ValueKey('conn_detail_page')), findsNothing,
        reason: '连接已断：没有可交接的连接');
    expect(fake.sendFilesCalls, 0);
    expect(find.byKey(const ValueKey('flow_error')), findsOneWidget,
        reason: '失败卡保持可见');

    // The failed flow still owns the KEPT selection (the one from before
    // the change attempt): its retry shows THAT — not the stale pick.
    await tester.tap(find.byKey(const ValueKey('flow_retry_button')));
    await pumpEvents(tester);
    expect(find.textContaining('已选：2 项'), findsOneWidget,
        reason: 'P1-D：失败前打开的选择绝不落入重试后的流——保留的旧选择随重试保留');
    expect(find.textContaining('已选：1 项'), findsNothing,
        reason: 'P1-D：过期选择（1 项的新文件）绝不出现在重试后的码卡片上');
  });

  testWidgets('P1-D (Windows shape): 更改 open → connection drops → 重试 → '
      'the stale pick returns — DISCARDED; the retried pairing sends the '
      'KEPT selection, never the stale pick', (tester) async {
    final gateway = _HoldingPickGateway();
    final fake = await pumpApp(tester, picker: gateway);
    await _openChangePick(tester, gateway); // 已选：2 项, change pick open

    // The connection completes... and drops: the flow fails while the
    // system picker is still up.
    final droppedConn = fake.simulatePeerJoin();
    await pumpEvents(tester);
    fake.simulateNetworkChange();
    await pumpEvents(tester);
    await tester.pump();
    expect(find.textContaining('与对方的连接断开了'), findsOneWidget);
    expect(fake.sendFilesCalls, 0);

    // 重试: a FRESH flow (a new generation — P1-D), still carrying the
    // KEPT selection the failed flow owned.
    await tester.tap(find.byKey(const ValueKey('flow_retry_button')));
    await pumpEvents(tester);
    expect(find.byKey(const ValueKey('flow_pairing_code')), findsOneWidget);
    expect(find.textContaining('已选：2 项'), findsOneWidget,
        reason: '前置：无选择进行中时，保留的选择随失败+重试存活（既有行为）');

    // The STALE pick (opened before the failure) finally returns with a
    // NEW selection — it belongs to the dead flow: discarded.
    gateway.answer(_replacement);
    await pumpEvents(tester);
    await tester.pump();
    expect(find.textContaining('已选：2 项'), findsOneWidget,
        reason: 'P1-D：失败前打开的选择绝不落入重试后的流');
    expect(find.textContaining('已选：1 项'), findsNothing);

    // The retried pairing completes: the auto-send carries the KEPT
    // selection — never the stale pick's files.
    final connId = fake.simulatePeerJoin();
    await pumpEvents(tester);
    fake.completeProbe(connId);
    await pumpEvents(tester);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    await pumpEvents(tester);

    expect(fake.sendFilesCalls, 1, reason: '重试配对成功：自动发送恰一次');
    expect(fake.sendRequests.single.$1, connId,
        reason: '发给重试码建立的连接（不是已断的那条）');
    expect(fake.sendRequests.single.$1, isNot(droppedConn));
    expect(fake.sendRequests.single.$2,
        kTwoFiles.map((entry) => entry.path).toList(),
        reason: 'P1-D：发送的是保留的旧选择，绝不是过期选择挑中的文件');
  });

  testWidgets('P2-D: a STALE flow\'s still-open 更改 window never defers '
      'the CURRENT flow\'s auto-send — B sends while A\'s picker is still '
      'open, and A\'s late pick clears nothing', (tester) async {
    final gateway = _HoldingPickGateway();
    final fake = await pumpApp(tester, picker: gateway);

    // Flow A: a sender code waits, and its 更改 pick opens and STAYS open.
    await tester.tap(find.byKey(const ValueKey('home_send_button')));
    await tester.pump();
    gateway.answer(kTwoFiles);
    await pumpEvents(tester);
    await tester
        .tap(find.byKey(const ValueKey('flow_change_selection_button')));
    await tester.pump();
    expect(gateway.openPicks, 1, reason: '前置：A 的更改选择器仍开着');

    // The user gives up on A (取消配对 resets it) and runs flow B through
    // the same main path — B's pick opens and returns while A's stale
    // window is STILL in flight.
    await tester.tap(find.byKey(const ValueKey('flow_cancel_button')));
    await pumpEvents(tester);
    await tester.tap(find.byKey(const ValueKey('home_send_button')));
    await tester.pump();
    expect(gateway.openPicks, 2, reason: '前置：B 的初选也开了（A 的还开着）');
    gateway.answerLast(_replacement); // B's pick — A's stays open
    await pumpEvents(tester);
    expect(find.textContaining('已选：1 项'), findsOneWidget,
        reason: '前置：B 的流带 B 自己的选择');

    // B's pairing completes BEHIND A's still-open picker — the handover
    // must fire NOW (P2-D: only B's OWN generation's picks defer it; the
    // single cross-generation counter used to block B until A returned).
    final connId = fake.simulatePeerJoin();
    await pumpEvents(tester);
    fake.completeProbe(connId);
    await pumpEvents(tester);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    await pumpEvents(tester);

    expect(find.byKey(const ValueKey('conn_detail_page')), findsOneWidget,
        reason: 'P2-D：过期更改窗口不得推迟 B 的交接');
    expect(fake.sendFilesCalls, 1, reason: 'B 的自动发送在 A 的窗口仍开着时就发出');
    expect(fake.sendRequests.single.$1, connId);
    expect(fake.sendRequests.single.$2, [_replacement.single.path]);

    // A's stale pick finally returns: discarded (the generation moved),
    // and it must not clear or re-trigger anything of B's page.
    gateway.answer(kTwoFiles);
    await pumpEvents(tester);
    await tester.pump();
    expect(fake.sendFilesCalls, 1, reason: 'P2-D：过期选择绝不产生第二次发送');
  });

  testWidgets('P1-A: the change pick returning after the flow reset is '
      'DISCARDED — the next flow\'s auto-send carries only ITS OWN '
      'selection', (tester) async {
    final gateway = _HoldingPickGateway();
    final fake = await pumpApp(tester, picker: gateway);

    // Flow A: pick files, the sender code card waits for its peer.
    await tester.tap(find.byKey(const ValueKey('home_send_button')));
    await tester.pump();
    gateway.answer(kTwoFiles);
    await pumpEvents(tester);
    expect(find.textContaining('已选：2 项'), findsOneWidget);

    // The 更改 pick opens and STAYS open (the system picker is up).
    await tester
        .tap(find.byKey(const ValueKey('flow_change_selection_button')));
    await tester.pump();

    // The user navigates back while the picker is up: 取消配对 resets flow
    // A (the generation moves on).
    await tester.tap(find.byKey(const ValueKey('flow_cancel_button')));
    await pumpEvents(tester);
    expect(find.byKey(const ValueKey('home_send_button')), findsOneWidget,
        reason: '前置：流 A 已复位，回到主入口');

    // Flow B starts while A's picker is still open: two picks in flight.
    await tester.tap(find.byKey(const ValueKey('home_send_button')));
    await tester.pump();
    expect(gateway.openPicks, 2, reason: '前置：A 的更改选择器仍开着，B 的初选又开了');

    // A's stale picker returns first (A's files): the generation moved on
    // — the result must die here, never reach the pending selection.
    gateway.answer(kTwoFiles);
    await pumpEvents(tester);
    await tester.pump();

    // B's pick returns: ITS selection is the one the flow carries.
    gateway.answer(_replacement);
    await pumpEvents(tester);
    await tester.pump();
    expect(find.textContaining('已选：1 项'), findsOneWidget,
        reason: 'P1-A：B 的流携带 B 自己的选择');

    // B's pairing completes: the auto-send carries B's paths — never A's
    // files over B's connection.
    final connId = fake.simulatePeerJoin();
    await pumpEvents(tester);
    fake.completeProbe(connId);
    await pumpEvents(tester);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    await pumpEvents(tester);

    expect(fake.sendFilesCalls, 1, reason: 'B 的自动发送恰一次');
    expect(fake.sendRequests.single.$2, [_replacement.single.path],
        reason: 'P1-A：绝不用 A 流的文件发送');
  });

  testWidgets('P1-A: the send pick returning after ANOTHER flow began is '
      'DISCARDED — it mints no code and leaves that flow untouched',
      (tester) async {
    final gateway = _HoldingPickGateway();
    final fake = await pumpApp(tester, picker: gateway);

    // The send main path's pick opens and STAYS open.
    await tester.tap(find.byKey(const ValueKey('home_send_button')));
    await tester.pump();

    // The user moves on: 生成接收码 starts another flow (the generation
    // moves on behind the picker).
    await tester.tap(
        find.byKey(const ValueKey('home_receive_create_code_button')));
    await pumpEvents(tester);
    expect(find.byKey(const ValueKey('flow_pairing_code')), findsOneWidget,
        reason: '前置：接收码流程已在等待');

    // The abandoned send pick returns with a real selection: the
    // generation moved on — nothing is minted with it.
    gateway.answer(kTwoFiles);
    await pumpEvents(tester);
    await tester.pump();

    expect(fake.createCodeCalls, 1, reason: 'P1-A：作废的选择不再生码');
    expect(fake.createdCodes.single.role, TaskRole.receive,
        reason: '生码的只有接收流程');
    expect(find.textContaining('已选：'), findsNothing,
        reason: '接收码卡片不显示作废选择的摘要');
    expect(find.byKey(const ValueKey('flow_pairing_code')), findsOneWidget,
        reason: '接收流程不受作废选择干扰');
  });

  testWidgets('P1-1(c): the detail page\'s automatic send fires ONLY on a '
      'SEND-direction row, and consumes initialPicked exactly once across '
      'rebuilds and duplicate snapshots', (tester) async {
    final fake = LocalSimpleTransferService();
    addTearDown(fake.dispose);
    final picker = FakeFilePickGateway(filesResult: kTwoFiles);

    // A RECEIVE-direction row handed a payload by mistake: nothing is
    // sent — `sendFiles` is invalid there — and the page shows the
    // receive card.
    await fake.createCode(TaskRole.receive);
    final receiveId = fake.simulatePeerJoin();
    fake.completeProbe(receiveId);
    await tester.pumpWidget(MaterialApp(
      home: ConnectionDetailPage(
        service: fake,
        connectionId: receiveId,
        picker: picker,
        initialPicked: kTwoFiles,
        autoSendOnReady: true,
      ),
    ));
    await pumpEvents(tester);
    await tester.pump();
    expect(fake.sendFilesCalls, 0,
        reason: 'P1-1(c)：接收方向的连接不自动发送');
    expect(find.byKey(const ValueKey('conn_send_button')), findsNothing,
        reason: '接收卡：没有发送按钮');

    // A SEND-direction row: the payload goes out once.
    await fake.createCode(TaskRole.send);
    final sendId = fake.simulatePeerJoin(peerName: '办公本-ThinkPad');
    fake.completeProbe(sendId);
    var rebuilds = 0;
    late StateSetter rebuild;
    await tester.pumpWidget(MaterialApp(
      home: StatefulBuilder(
        builder: (context, setState) {
          rebuild = setState;
          // A NEW list instance each build, exactly like the home page's
          // route builder re-running with the same `pending` closure.
          return ConnectionDetailPage(
            service: fake,
            connectionId: sendId,
            picker: picker,
            initialPicked: List.of(kTwoFiles),
            autoSendOnReady: true,
          );
        },
      ),
    ));
    await pumpEvents(tester);
    expect(fake.sendFilesCalls, 1);
    expect(fake.sendRequests.single.$1, sendId);
    expect(fake.sendRequests.single.$2,
        kTwoFiles.map((entry) => entry.path).toList());

    // The route builds again and the same snapshot arrives again: still
    // ONE send, still two entries.
    rebuild(() => rebuilds++);
    await tester.pump();
    fake.completeProbe(sendId);
    await pumpEvents(tester);
    expect(rebuilds, 1);
    expect(fake.sendFilesCalls, 1, reason: 'exactly once');
    expect(find.text('已选 2 项'), findsOneWidget,
        reason: 'initialPicked 只消费一次——重建不重复追加');
  });

  // -----------------------------------------------------------------------
  // Batch X codex review, P1-2(a): the create flow's retry direction.
  // -----------------------------------------------------------------------

  for (final role in TaskRole.values) {
    final entryKey = role == TaskRole.send
        ? 'home_send_button'
        : 'home_receive_create_code_button';
    testWidgets('P1-2: a FAILED ${role.name}-create retries with the SAME '
        'direction — the service is asked createCode(${role.name}) again, '
        'never an implicit send', (tester) async {
      final picker = FakeFilePickGateway(filesResult: kTwoFiles);
      final service = _CreateFailureService();
      await _pumpWith(tester, service: service, picker: picker);

      // The ENTRY declares the direction; the engine fails to mint the
      // code.
      await tester.tap(find.byKey(ValueKey(entryKey)));
      await pumpEvents(tester);
      await tester.pump(const Duration(milliseconds: 300));

      // The failure card: no code and no connection — nothing left to
      // read a role from.
      expect(find.byKey(const ValueKey('flow_error')), findsOneWidget);
      expect(find.byKey(const ValueKey('flow_pairing_code')), findsNothing);
      expect(service.createRoles, [role]);
      expect(service.createdCodes, isEmpty, reason: '生码失败：没有 TaskCode');
      final retry = tester.widget<FilledButton>(
          find.byKey(const ValueKey('flow_retry_button')));
      expect(retry.onPressed, isNotNull, reason: '方向已知——重试可用');

      // 重试: the SAME direction again.
      await tester.tap(find.byKey(const ValueKey('flow_retry_button')));
      await pumpEvents(tester);

      expect(service.createRoles, [role, role],
          reason: 'P1-2：重试重复本次发起流声明的方向');
      expect(service.createdCodes.single.role, role,
          reason: 'P1-2：重试生成的码与入口同向');
      expect(find.byKey(const ValueKey('flow_pairing_code')), findsOneWidget);
      if (role == TaskRole.receive) {
        // The old retry fell through to an implicit TaskRole.send here:
        // a 接收-create silently became a send-create.
        expect(service.createRoles, isNot(contains(TaskRole.send)),
            reason: 'P1-2：接收建码失败的重试绝不变为发送建码');
        expect(find.textContaining('对方输入后就可以把文件发给你'),
            findsOneWidget,
            reason: '仍是接收码的说明');
      }
    });
  }

  testWidgets('P1-2: the remembered create direction belongs to ONE flow '
      '— 返回 clears it, and the next flow remembers its own', (tester) async {
    final picker = FakeFilePickGateway(filesResult: kTwoFiles);
    final service = _CreateFailureService(failures: 2);
    await _pumpWith(tester, service: service, picker: picker);

    // A 接收-create fails; the user goes back to the home actions.
    await tester
        .tap(find.byKey(const ValueKey('home_receive_create_code_button')));
    await pumpEvents(tester);
    expect(service.createRoles, [TaskRole.receive]);
    await tester.tap(find.byKey(const ValueKey('flow_back_button')));
    await pumpEvents(tester);

    // A 发送-create fails next: its retry repeats SEND, never the stale
    // receive of the previous flow.
    await tester.tap(find.byKey(const ValueKey('home_send_button')));
    await pumpEvents(tester);
    expect(service.createRoles, [TaskRole.receive, TaskRole.send]);
    await tester.tap(find.byKey(const ValueKey('flow_retry_button')));
    await pumpEvents(tester);
    expect(service.createRoles,
        [TaskRole.receive, TaskRole.send, TaskRole.send]);
    expect(service.createdCodes.single.role, TaskRole.send);
  });

  // -----------------------------------------------------------------------
  // Batch X codex review, P1-2(b): the PRODUCTION gateway's folder offer.
  // Only the two plugin calls are faked — the offer dialog is the real
  // app-side one.
  // -----------------------------------------------------------------------

  testWidgets('P1-2: production gateway — 添加文件夹 runs the single '
      'directory pick and APPENDS the folder after the files', (tester) async {
    var directoryPicks = 0;
    List<PickedEntry>? picked;
    final gateway = PluginFilePickGateway(
      filePicker: () async => _twoPlatformFiles,
      directoryPicker: () async {
        directoryPicks += 1;
        return _folderPath;
      },
    );
    await _pumpPickHarness(tester, gateway, (entries) => picked = entries);

    await tester.tap(find.byKey(const ValueKey('harness_pick_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));

    // The offer is the REAL app-side dialog; nothing is final before the
    // answer.
    expect(find.text('还要添加整个文件夹吗？'), findsOneWidget);
    expect(
        find.text('可以再选一个文件夹，里面的全部内容会和已选的文件一起发送。'),
        findsOneWidget);
    expect(picked, isNull, reason: '回答之前 pickFiles 不返回');
    expect(directoryPicks, 0, reason: '同意之前不打开文件夹选择');

    await tester.tap(find.byKey(const ValueKey('folder_offer_add_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));

    expect(directoryPicks, 1, reason: '沿用既有的单次文件夹选择');
    expect(_shapeOf(picked), [
      (_filePath1, '照片.zip', false, 1536),
      (_filePath2, '报告.pdf', false, 512 * 1024),
      (_folderPath, '相册', true, null),
    ]);
  });

  testWidgets('P1-2: production gateway — 不用了 skips the folder: the '
      'files alone are the result', (tester) async {
    var directoryPicks = 0;
    List<PickedEntry>? picked;
    final gateway = PluginFilePickGateway(
      filePicker: () async => _twoPlatformFiles,
      directoryPicker: () async {
        directoryPicks += 1;
        return _folderPath;
      },
    );
    await _pumpPickHarness(tester, gateway, (entries) => picked = entries);

    await tester.tap(find.byKey(const ValueKey('harness_pick_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    expect(find.text('还要添加整个文件夹吗？'), findsOneWidget);

    await tester
        .tap(find.byKey(const ValueKey('folder_offer_decline_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));

    expect(find.text('还要添加整个文件夹吗？'), findsNothing);
    expect(directoryPicks, 0, reason: '不用了：不打开文件夹选择');
    expect(_shapeOf(picked), [
      (_filePath1, '照片.zip', false, 1536),
      (_filePath2, '报告.pdf', false, 512 * 1024),
    ]);
  });

  testWidgets('P1-2 (round 3): a cancelled FILE pick offers the '
      'folder-ONLY path — declining (or a cancelled folder pick) is the '
      'overall cancel', (tester) async {
    var directoryPicks = 0;
    List<PickedEntry>? picked;
    final gateway = PluginFilePickGateway(
      filePicker: () async => const <_PlatformFile>[],
      directoryPicker: () async {
        directoryPicks += 1;
        return null; // the user backs out of the folder picker
      },
    );
    await _pumpPickHarness(tester, gateway, (entries) => picked = entries);

    await tester.tap(find.byKey(const ValueKey('harness_pick_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    // The folder-ONLY offer (the round-2 semantics — cancel = leave with
    // nothing — is REPLACED by the round-3 requirement: folder-only
    // sending must be reachable from the send main entry).
    expect(find.byKey(const ValueKey('folder_only_offer_title')),
        findsOneWidget,
        reason: '取消选文件后给出只发文件夹的出口');
    await tester.tap(
        find.byKey(const ValueKey('folder_only_offer_pick_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    // The folder picker was itself cancelled: overall cancel, empty
    // result, exactly one folder pick attempted.
    expect(picked, isNotNull);
    expect(picked, isEmpty);
    expect(directoryPicks, 1);
  });

  testWidgets('P1-2 (round 3): a folder-ONLY selection from the send '
      'main entry — cancel files, choose the folder, send just it',
      (tester) async {
    List<PickedEntry>? picked;
    final gateway = PluginFilePickGateway(
      filePicker: () async => const <_PlatformFile>[], // cancelled
      directoryPicker: () async => _folderPath,
    );
    await _pumpPickHarness(tester, gateway, (entries) => picked = entries);

    await tester.tap(find.byKey(const ValueKey('harness_pick_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    await tester.tap(
        find.byKey(const ValueKey('folder_only_offer_pick_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));

    expect(picked, isNotNull);
    expect(_shapeOf(picked), [(_folderPath, '相册', true, null)],
        reason: '纯文件夹选择：一个目录条目，没有文件');
  });

  testWidgets('P1-2: production gateway — a cancelled FOLDER pick keeps '
      'the files, and dismissing the offer does too', (tester) async {
    var directoryPicks = 0;
    List<PickedEntry>? picked;
    final gateway = PluginFilePickGateway(
      filePicker: () async => _twoPlatformFiles,
      directoryPicker: () async {
        directoryPicks += 1;
        return null; // the user backed out of the folder picker
      },
    );
    await _pumpPickHarness(tester, gateway, (entries) => picked = entries);
    const filesOnly = [
      (_filePath1, '照片.zip', false, 1536),
      (_filePath2, '报告.pdf', false, 512 * 1024),
    ];

    // 添加文件夹, then the folder picker is cancelled: files kept.
    await tester.tap(find.byKey(const ValueKey('harness_pick_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    await tester.tap(find.byKey(const ValueKey('folder_offer_add_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    expect(directoryPicks, 1);
    expect(_shapeOf(picked), filesOnly,
        reason: '取消选文件夹不丢已选的文件');

    // Again, this time the offer itself is dismissed unanswered.
    await tester.tap(find.byKey(const ValueKey('harness_pick_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    expect(find.text('还要添加整个文件夹吗？'), findsOneWidget);
    await tester.tapAt(const Offset(10, 10));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    expect(directoryPicks, 1, reason: '关掉追问等于「不用了」');
    expect(_shapeOf(picked), filesOnly);
  });

  testWidgets('P1-2: X4 发送文件 main path on the PRODUCTION gateway — '
      'files plus ONE folder are preselected and BOTH are auto-sent '
      '(「可多选，含文件夹」without a fake)', (tester) async {
    final gateway = PluginFilePickGateway(
      filePicker: () async => _twoPlatformFiles,
      directoryPicker: () async => _folderPath,
    );
    final fake = await pumpApp(tester, picker: gateway);

    await tester.tap(find.byKey(const ValueKey('home_send_button')));
    await pumpEvents(tester);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    expect(find.text('还要添加整个文件夹吗？'), findsOneWidget);
    expect(fake.createCodeCalls, 0, reason: '选择定下来之前不生码');

    await tester.tap(find.byKey(const ValueKey('folder_offer_add_button')));
    await pumpEvents(tester);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    expect(find.textContaining('已选：3 项'), findsOneWidget,
        reason: '含文件夹时只显示项数（文件夹大小未知）');
    expect(fake.createdCodes.single.role, TaskRole.send);

    final connId = fake.simulatePeerJoin(peerName: '小米手机-14');
    await pumpEvents(tester);
    fake.completeProbe(connId);
    await pumpEvents(tester);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    await pumpEvents(tester);

    expect(fake.sendFilesCalls, 1, reason: '自动发送恰一次');
    expect(fake.sendRequests.single.$2, [_filePath1, _filePath2, _folderPath],
        reason: '文件在前，追加的一个文件夹在后');
    expect(find.byKey(const ValueKey('picked_item_2')), findsOneWidget);
    expect(find.text('文件夹'), findsOneWidget,
        reason: '文件夹条目按文件夹显示');
  });
}

// ---------------------------------------------------------------------------
// Test doubles and helpers for the P1-1 / P1-2 tests above.
// ---------------------------------------------------------------------------

const String _filePath1 = r'C:\docs\照片.zip';
const String _filePath2 = r'C:\docs\报告.pdf';
const String _folderPath = r'C:\docs\相册';

/// The change pick's replacement selection (one file, replacing two).
const List<PickedEntry> _replacement = [
  PickedEntry(
      path: r'C:\docs\新文件.bin',
      name: '新文件.bin',
      isDirectory: false,
      sizeBytes: 4096),
];

/// A DIFFERENT well-formed code: [code] with its first symbol flipped
/// (`AKI1-` is 5 characters, so index 5 is the first group's first symbol).
String _otherCode(String code) {
  final flipped = code.codeUnitAt(5) == 0x30 ? '1' : '0'; // '0' <-> '1'
  return code.replaceRange(5, 6, flipped);
}

/// `pumpApp` for a service the test had to build itself (the shared
/// helper always constructs the plain fake).
Future<T> _pumpWith<T extends LocalSimpleTransferService>(
  WidgetTester tester, {
  required T service,
  FilePickGateway? picker,
}) async {
  await tester.binding.setSurfaceSize(kWideSize);
  addTearDown(() => tester.binding.setSurfaceSize(null));
  addTearDown(service.dispose);
  await tester.pumpWidget(AkSendingApp(
    service: service,
    store: InMemorySaveDirStore(),
    picker: picker ?? const PluginFilePickGateway(),
  ));
  await tester.pump();
  return service;
}

/// Drives the 发送文件 main path to its shown code (the first pick answered
/// with [kTwoFiles]), then taps 更改 and leaves THAT pick open — the
/// [_HoldingPickGateway] answers only when the test says so.
Future<void> _openChangePick(
    WidgetTester tester, _HoldingPickGateway gateway) async {
  await tester.tap(find.byKey(const ValueKey('home_send_button')));
  await tester.pump();
  gateway.answer(kTwoFiles);
  await pumpEvents(tester);
  expect(find.textContaining('已选：2 项'), findsOneWidget);

  await tester.tap(find.byKey(const ValueKey('flow_change_selection_button')));
  await tester.pump();
}

/// P1-1 double: the fake plus UNRELATED live connections — rows produced
/// by OTHER pairing codes, added on demand while the UI's own flow is
/// waiting for its peer (the stock fake only ever joins its one pending
/// code, so it cannot produce such a row).
class _ForeignConnectionService extends LocalSimpleTransferService {
  _ForeignConnectionService() {
    _baseSub = super.connections.listen((snapshot) {
      _base = snapshot;
      _announce();
    });
  }

  final List<TaskConnection> _foreign = [];
  List<TaskConnection> _base = const [];
  final StreamController<List<TaskConnection>> _changes =
      StreamController<List<TaskConnection>>.broadcast();
  late final StreamSubscription<List<TaskConnection>> _baseSub;

  /// Adds an unrelated connection ([code] must differ from the UI flow's
  /// code) and announces it like any live row. Returns its id.
  String addForeignConnection({
    required String code,
    required TaskRole role,
    TaskConnectionState state = TaskConnectionState.connected,
  }) {
    final row = TaskConnection(
      id: 'foreign-${_foreign.length + 1}',
      peerName: '别的设备',
      role: role,
      state: state,
      code: code,
    );
    _foreign.add(row);
    _announce();
    return row.id;
  }

  void _announce() {
    if (!_changes.isClosed) {
      _changes.add(<TaskConnection>[..._base, ..._foreign]);
    }
  }

  /// The base contract (wave-29 P1-06) stays intact: EVERY subscriber is
  /// handed the current snapshot first, then the live changes — a detail
  /// page opened later still sees the live list immediately.
  @override
  Stream<List<TaskConnection>> get connections =>
      Stream<List<TaskConnection>>.multi((controller) {
        controller.add(<TaskConnection>[..._base, ..._foreign]);
        final forward = _changes.stream.listen(controller.add);
        controller.onCancel = forward.cancel;
      });

  @override
  void dispose() {
    _baseSub.cancel();
    _changes.close();
    super.dispose();
  }
}

/// P1-1 double: a multi-file pick that stays OPEN until the test answers
/// it — the 更改 race needs the system picker to be up while the peer
/// completes behind it.
class _HoldingPickGateway implements FilePickGateway {
  final List<Completer<List<PickedEntry>>> _open = [];

  /// Picks requested so far and not yet answered.
  int get openPicks => _open.length;

  /// Answers the OLDEST open pick with [entries] (an empty list = the user
  /// cancelled it).
  void answer(List<PickedEntry> entries) {
    _open.removeAt(0).complete(entries);
  }

  /// Answers the NEWEST open pick with [entries] — a stale EARLIER window
  /// stays open (the P2-D shape: B answers while A's picker is still up).
  void answerLast(List<PickedEntry> entries) {
    _open.removeLast().complete(entries);
  }

  @override
  Future<List<PickedEntry>> pickFiles() {
    final completer = Completer<List<PickedEntry>>();
    _open.add(completer);
    return completer.future;
  }

  @override
  Future<PickedEntry?> pickDirectory() async => null;
}

/// P1-2 double: `createCode` fails like a real engine would (the identity
/// failure the W3 card tests use) for the first [failures] calls; every
/// call's role is recorded so the test can assert what the service was
/// asked — including the calls that failed.
class _CreateFailureService extends LocalSimpleTransferService {
  _CreateFailureService({this.failures = 1});

  int failures;
  final List<TaskRole> createRoles = [];

  @override
  Future<TaskCode> createCode(TaskRole role) {
    createRoles.add(role);
    if (failures > 0) {
      failures -= 1;
      throw StateError('身份认证失败');
    }
    return super.createCode(role);
  }
}

/// Minimal `PlatformFile` double — the plugin's class can only be
/// extended, and its own tests build one the same way. Just what the
/// gateway reads: name, path, size.
base class _PlatformFile extends fp.PlatformFile {
  _PlatformFile(this.name, this.path, this.size);

  @override
  final String name;

  /// A non-null override of the plugin's nullable `path`: these doubles
  /// are always on local disk.
  @override
  final String path;
  final int size;

  @override
  Uri get uri => Uri.file(path);

  /// The gateway never opens the file.
  @override
  Never get xFile => throw UnsupportedError('not used by the gateway');

  @override
  int? lengthSync() => size;

  @override
  Future<int?> length() async => size;

  @override
  Future<Uint8List> readAsBytes() async => Uint8List(0);

  @override
  Stream<Uint8List> readAsByteStream() => const Stream.empty();
}

/// What the faked platform multi-file pick returns: the two files of
/// [kTwoFiles], in the plugin's own shape.
List<fp.PlatformFile> get _twoPlatformFiles => [
      _PlatformFile('照片.zip', _filePath1, 1536),
      _PlatformFile('报告.pdf', _filePath2, 512 * 1024),
    ];

List<(String, String, bool, int?)> _shapeOf(List<PickedEntry>? entries) => [
      for (final entry in entries ?? const <PickedEntry>[])
        (entry.path, entry.name, entry.isDirectory, entry.sizeBytes),
    ];

/// One button that runs the gateway's `pickFiles()` and parks the outcome
/// (null = the pick threw).
class _PickHarness extends StatefulWidget {
  const _PickHarness({
    required this.gateway,
    required this.onDone,
  });

  final PluginFilePickGateway gateway;
  final ValueChanged<List<PickedEntry>?> onDone;

  @override
  State<_PickHarness> createState() => _PickHarnessState();
}

class _PickHarnessState extends State<_PickHarness> {
  @override
  Widget build(BuildContext context) {
    return Scaffold(
      body: Center(
        child: FilledButton(
          key: const ValueKey('harness_pick_button'),
          onPressed: () {
            widget.gateway.pickFiles().then(
              widget.onDone,
              onError: (Object error) => widget.onDone(null),
            );
          },
          child: const Text('pick'),
        ),
      ),
    );
  }
}

/// Pumps the [_PickHarness] — a bare app whose ONLY navigator is the root
/// one the production offer has to find on its own.
Future<void> _pumpPickHarness(
  WidgetTester tester,
  PluginFilePickGateway gateway,
  ValueChanged<List<PickedEntry>?> onDone,
) async {
  await tester.pumpWidget(MaterialApp(
    home: _PickHarness(gateway: gateway, onDone: onDone),
  ));
  await tester.pump();
}
