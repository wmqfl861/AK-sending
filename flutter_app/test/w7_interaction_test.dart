/// W7 (2026-11-22) widget tests, updated by X4 (batch X) for the
/// direction-declared-once home:
///
/// * W7.1/X4 — the join panel carries NO direction selector: the entry
///   the user tapped already declared the direction (发送文件 / 接收文件),
///   and the success prompt names the ACTUAL role (对方将把文件发给你 /
///   现在可以选择文件发给对方);
/// * W7.2 — a SENDER-side success auto-opens the connection detail page
///   with the file picker (cancel keeps the page). Since X4 this lives on
///   for the JOIN-send secondary path (输入对方的码 as sender) — the
///   create-send main path preselects its files and auto-SENDS instead
///   (covered by home_flow_test);
/// * W7.3 — Android-only 「传输期间请保持本应用在前台」 hint on the
///   running-transfer cards (injected [UiPlatform], no channel);
/// * P2-B (third review) — the same hint family on the WAITING stages:
///   the code card's peer-join wait and the detail page's file-selection
///   wait, each with stage-adapted copy, Android-only like W7.3.
library;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:ak_sending_app/ui/service/simple_transfer_service.dart';
import 'package:ak_sending_app/ui/service/ui_platform.dart';

import 'w28_test_helpers.dart';

const int mib = 1024 * 1024;

void main() {
  testWidgets('X4: the join panel has NO direction selector — the '
      'ENTRY\'s declared direction reaches joinCode (never null)',
      (tester) async {
    final fake = await pumpApp(tester);
    addTearDown(fake.dispose);
    final minted = await fake.createCode(TaskRole.send);

    // The receive MAIN entry opens the panel with the direction fixed.
    await tester.tap(find.byKey(const ValueKey('home_receive_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.byKey(const ValueKey('join_role_selector')), findsNothing,
        reason: 'X4：方向已由入口声明，面板不再选择');
    expect(find.text('这次配对我要：'), findsNothing);
    expect(find.text('对方将把文件发给你。'), findsOneWidget,
        reason: '面板按入口声明的方向说明');
    expect(fake.joinCodeCalls, 0, reason: '面板未提交不发起 join');

    await tester.enterText(
        find.byKey(const ValueKey('join_code_field')), minted.code);
    await tester.pump();
    var submit = tester.widget<FilledButton>(
        find.byKey(const ValueKey('join_submit_button')));
    expect(submit.onPressed, isNotNull,
        reason: 'X4：合法码即可提交——方向不是面板的一步');

    await tester.tap(find.byKey(const ValueKey('join_submit_button')));
    await tester.pump();
    await pumpEvents(tester);
    expect(fake.joinCodeCalls, 1);
    expect(fake.joinRoleRequests.single, TaskRole.receive,
        reason: 'X4：接收入口的 join 角色为接收且绝不为 null');

    // Close the receiver-side waiting card, then try the other entry.
    fake.completeProbe('conn-1');
    await pumpEvents(tester);
    await tester.pump();
    await tester.tap(find.byKey(const ValueKey('flow_back_button')));
    await tester.pump();
    final second = await fake.createCode(TaskRole.receive);
    await tester.tap(
        find.byKey(const ValueKey('home_send_join_code_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    expect(find.text('你将把文件发给对方。'), findsOneWidget);
    await tester.enterText(
        find.byKey(const ValueKey('join_code_field')), second.code);
    await tester.pump();
    submit = tester.widget<FilledButton>(
        find.byKey(const ValueKey('join_submit_button')));
    expect(submit.onPressed, isNotNull);
    await tester.tap(find.byKey(const ValueKey('join_submit_button')));
    await tester.pump();
    await pumpEvents(tester);
    expect(fake.joinRoleRequests.last, TaskRole.send,
        reason: 'X4：发送次要入口的 join 角色为发送且绝不为 null');
  });

  testWidgets('W7.1/W7.2: receiver-side join success says 对方将把文件发给 '
      '你 and waits (no 完成 button)', (tester) async {
    final fake = await pumpApp(tester);
    addTearDown(fake.dispose);
    // The receive MAIN entry joins a sender's code.
    await fake.createCode(TaskRole.send);

    await tester.tap(find.byKey(const ValueKey('home_receive_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    await tester.enterText(find.byKey(const ValueKey('join_code_field')),
        fake.createdCodes.single.code);
    await tester.pump();
    await tester.tap(find.byKey(const ValueKey('join_submit_button')));
    await tester.pump();
    fake.completeProbe('conn-1');
    await pumpEvents(tester);
    await tester.pump();

    expect(find.byKey(const ValueKey('flow_success')), findsOneWidget);
    expect(find.text('对方将把文件发给你'), findsOneWidget,
        reason: '按实际角色说明（W7.1）');
    expect(find.text('等待对方发送文件…'), findsOneWidget);
    expect(find.textContaining('Download/AK-接收'), findsOneWidget,
        reason: '等待卡片显示保存位置（W7.2）');
    expect(find.byKey(const ValueKey('flow_done_button')), findsNothing,
        reason: 'W7.2：接收方无需点「完成」');
    // No auto navigation for the receiver.
    expect(find.byKey(const ValueKey('conn_detail_page')), findsNothing);
  });

  testWidgets('W7.2 (join-send secondary path): success auto-opens the '
      'detail page with the picker; cancelling the pick keeps the page',
      (tester) async {
    final picker = FakeFilePickGateway(filesResult: kTwoFiles);
    final fake = await pumpApp(tester, picker: picker);
    addTearDown(fake.dispose);
    // Creator declared RECEIVE → the joiner (us) sends via the send
    // SECONDARY entry (输入对方的码).
    await fake.createCode(TaskRole.receive);

    await tester.tap(
        find.byKey(const ValueKey('home_send_join_code_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    await tester.enterText(find.byKey(const ValueKey('join_code_field')),
        fake.createdCodes.single.code);
    await tester.pump();
    await tester.tap(find.byKey(const ValueKey('join_submit_button')));
    await tester.pump();
    fake.completeProbe('conn-1');
    await pumpEvents(tester);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));

    expect(find.byKey(const ValueKey('conn_detail_page')), findsOneWidget);
    expect(find.byKey(const ValueKey('pick_files_option')), findsOneWidget,
        reason: '文件选择自动弹出（W7.2）');
    // The auto-opened sheet leads with the role statement (W7.1).
    expect(find.byKey(const ValueKey('pick_sheet_role_hint')), findsOneWidget);
    expect(picker.pickFilesCalls, 0, reason: '选择器还停在方式面板一步');

    // Cancel the sheet (barrier tap): the user stays on the detail page.
    await tester.tapAt(const Offset(10, 10));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    expect(find.byKey(const ValueKey('conn_detail_page')), findsOneWidget);
    expect(find.byKey(const ValueKey('pick_files_option')), findsNothing);
    expect(find.text('已选 2 项'), findsNothing,
        reason: '取消后没有已选内容');
  });

  testWidgets('W7.3: Android shows 保持前台 on the running-transfer card; '
      'other platforms do not', (tester) async {
    UiPlatform.androidOverride = true;
    addTearDown(() => UiPlatform.androidOverride = null);
    final fake = await pumpApp(tester);
    addTearDown(fake.dispose);

    await fake.createCode(TaskRole.send);
    final id = fake.simulatePeerJoin();
    await pumpEvents(tester);
    fake.completeProbe(id);
    await pumpEvents(tester);
    await tester.pump();

    // Idle card: no hint yet.
    expect(find.byKey(ValueKey('conn_card_keep_foreground_$id')),
        findsNothing,
        reason: '无传输时不提示');

    fake.emitProgress(TransferProgress(
      connId: id,
      phase: TransferPhase.transferring,
      totalBytes: 4 * mib,
      transferredBytes: mib,
    ));
    await pumpEvents(tester);
    expect(find.text('传输期间请保持本应用在前台'), findsOneWidget,
        reason: 'Android 传输卡片显示前台提示（W7.3）');

    // Not Android: the hint disappears on the next rebuild.
    UiPlatform.androidOverride = false;
    fake.emitProgress(TransferProgress(
      connId: id,
      phase: TransferPhase.transferring,
      totalBytes: 4 * mib,
      transferredBytes: 2 * mib,
    ));
    await pumpEvents(tester);
    expect(find.text('传输期间请保持本应用在前台'), findsNothing);
  });

  testWidgets('W7.3: the receive banner and the detail send progress also '
      'carry the Android hint', (tester) async {
    UiPlatform.androidOverride = true;
    addTearDown(() => UiPlatform.androidOverride = null);
    final picker = FakeFilePickGateway(filesResult: kTwoFiles);
    final fake = await pumpApp(tester, picker: picker);
    addTearDown(fake.dispose);

    // Receiving banner.
    fake.emitIncomingRequest(fileCount: 1, totalBytes: mib);
    await pumpEvents(tester);
    await tester.tap(find.byKey(const ValueKey('incoming_accept_job-1')));
    await tester.pump();
    expect(find.byKey(const ValueKey('incoming_keep_foreground_job-1')),
        findsOneWidget,
        reason: '接收横幅在 Android 上显示前台提示');

    // Sender detail progress.
    await fake.createCode(TaskRole.send);
    final id = fake.simulatePeerJoin();
    await pumpEvents(tester);
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
    await tester.tap(find.byKey(const ValueKey('conn_send_button')));
    await tester.pump();
    expect(find.byKey(const ValueKey('conn_send_keep_foreground')),
        findsOneWidget,
        reason: '发送进度区在 Android 上显示前台提示');
  });

  testWidgets('P2-B: the WAITING stages carry the Android hint too — the '
      'code card\'s peer-join wait and the detail page\'s pick wait',
      (tester) async {
    UiPlatform.androidOverride = true;
    addTearDown(() => UiPlatform.androidOverride = null);
    final picker = FakeFilePickGateway(filesResult: kTwoFiles);
    final fake = await pumpApp(tester, picker: picker);
    addTearDown(fake.dispose);

    // Waiting stage 1 — the create flow's peer-join wait on the code card.
    await tester.tap(find.byKey(const ValueKey('home_send_button')));
    await pumpEvents(tester);
    expect(find.byKey(const ValueKey('flow_waiting')), findsOneWidget,
        reason: '前置：码卡片在等待对方加入');
    expect(find.text('等待对方加入期间请保持本应用在前台'), findsOneWidget,
        reason: 'Android 等待加入期间显示前台提示（P2-B）');
    expect(find.text('传输期间请保持本应用在前台'), findsNothing,
        reason: '等待阶段不是传输中：W7.3 原文案不出现');

    // Waiting stage 2 — the paired detail page waiting for the file
    // selection (before any send starts).
    await fake.createCode(TaskRole.send);
    final id = fake.simulatePeerJoin();
    await pumpEvents(tester);
    fake.completeProbe(id);
    await pumpEvents(tester);
    await tester.pump();
    await tester.tap(find.byKey(ValueKey('conn_card_name_$id')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));

    expect(find.byKey(const ValueKey('conn_detail_page')), findsOneWidget);
    expect(find.text('等待选择文件期间请保持本应用在前台'), findsOneWidget,
        reason: 'Android 等待选择文件期间显示前台提示（P2-B）');

    // Files picked: the pick-wait hint leaves with the stage (the review
    // list shows instead; the running-transfer hint still waits for a
    // real send).
    await tester.tap(find.byKey(const ValueKey('conn_pick_files_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    await tester.tap(find.byKey(const ValueKey('pick_files_option')));
    await tester.pump();
    expect(find.text('等待选择文件期间请保持本应用在前台'), findsNothing,
        reason: '已选文件后不再是选择等待阶段');
    expect(find.text('传输期间请保持本应用在前台'), findsNothing,
        reason: '尚未发送：W7.3 的传输中提示也不出现');
  });

  testWidgets('P2-B: the waiting-stage hints stay off on other platforms',
      (tester) async {
    UiPlatform.androidOverride = false;
    addTearDown(() => UiPlatform.androidOverride = null);
    final picker = FakeFilePickGateway(filesResult: kTwoFiles);
    final fake = await pumpApp(tester, picker: picker);
    addTearDown(fake.dispose);

    await tester.tap(find.byKey(const ValueKey('home_send_button')));
    await pumpEvents(tester);
    expect(find.byKey(const ValueKey('flow_waiting')), findsOneWidget,
        reason: '前置：等待卡片在场');
    expect(find.byKey(const ValueKey('flow_waiting_keep_foreground')),
        findsNothing, reason: '非 Android：等待加入不提示');

    await fake.createCode(TaskRole.send);
    final id = fake.simulatePeerJoin();
    await pumpEvents(tester);
    fake.completeProbe(id);
    await pumpEvents(tester);
    await tester.pump();
    await tester.tap(find.byKey(ValueKey('conn_card_name_$id')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    expect(find.byKey(const ValueKey('conn_detail_page')), findsOneWidget);
    expect(find.byKey(const ValueKey('conn_pick_keep_foreground')),
        findsNothing, reason: '非 Android：等待选择文件不提示');
  });
}
