/// W7 (2026-11-22) widget tests for the three interaction changes:
///
/// * W7.1 — the join panel carries NO direction selector; the join runs
///   without a role and the success prompt names the ACTUAL role
///   (对方将把文件发给你 / 现在可以选择文件发给对方);
/// * W7.2 — a SENDER-side success auto-opens the connection detail page
///   with the file picker (cancel keeps the page); a RECEIVER-side
///   success waits on the card (等待对方发送文件… + the save location,
///   no 完成 button);
/// * W7.3 — Android-only 「传输期间请保持本应用在前台」 hint on the
///   running-transfer cards (injected [UiPlatform], no channel).
library;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:ak_sending_app/ui/service/simple_transfer_service.dart';
import 'package:ak_sending_app/ui/service/ui_platform.dart';

import 'w28_test_helpers.dart';

const int mib = 1024 * 1024;

void main() {
  testWidgets('W7.1: the join panel has no direction selector; a valid '
      'code alone enables 配对 and the join carries NO role', (tester) async {
    final fake = await pumpApp(tester);
    addTearDown(fake.dispose);
    final minted = await fake.createCode(TaskRole.send);

    await tester.tap(find.byKey(const ValueKey('home_join_code_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.byKey(const ValueKey('join_role_selector')), findsNothing,
        reason: 'AUD-05：加入不再选方向');
    expect(find.text('这次配对我要：'), findsNothing);

    await tester.enterText(
        find.byKey(const ValueKey('join_code_field')), minted.code);
    await tester.pump();
    final submit = tester.widget<FilledButton>(
        find.byKey(const ValueKey('join_submit_button')));
    expect(submit.onPressed, isNotNull,
        reason: '合法码本身即可提交（无角色门槛）');

    await tester.tap(find.byKey(const ValueKey('join_submit_button')));
    await tester.pump();
    await pumpEvents(tester);
    expect(fake.joinCodeCalls, 1);
    expect(fake.joinRoleRequests.single, isNull,
        reason: 'W7.1：joinCode 不传 role，由契约给对侧角色');
  });

  testWidgets('W7.1/W7.2: receiver-side join success says 对方将把文件发给 '
      '你 and waits (no 完成 button)', (tester) async {
    final fake = await pumpApp(tester);
    addTearDown(fake.dispose);
    // Creator declared SEND → the joiner receives.
    await fake.createCode(TaskRole.send);

    await tester.tap(find.byKey(const ValueKey('home_join_code_button')));
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

  testWidgets('W7.2: create-send success auto-opens the detail page with '
      'the picker; cancelling the pick keeps the page', (tester) async {
    final picker = FakeFilePickGateway(filesResult: kTwoFiles);
    final fake = await pumpApp(tester, picker: picker);
    addTearDown(fake.dispose);

    await tester.tap(find.byKey(const ValueKey('home_create_code_button')));
    await tester.pump();
    await tester.pump(const Duration(seconds: 1));
    await tester.tap(find.byKey(const ValueKey('role_send_button')));
    await tester.pump();
    await tester.pump(const Duration(seconds: 1));

    final id = fake.simulatePeerJoin(peerName: '办公本-ThinkPad');
    await pumpEvents(tester);
    await tester.pump();
    fake.completeProbe(id);
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

  testWidgets('W7.2: join-send success also auto-opens (the joiner '
      'inherited the sender role)', (tester) async {
    final fake = await pumpApp(tester);
    addTearDown(fake.dispose);
    // Creator declared RECEIVE → the joiner (us) sends.
    await fake.createCode(TaskRole.receive);

    await tester.tap(find.byKey(const ValueKey('home_join_code_button')));
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

    expect(find.byKey(const ValueKey('conn_detail_page')), findsOneWidget,
        reason: '加入方为发送方时同样直达详情页');
    expect(find.byKey(const ValueKey('conn_pick_files_button')),
        findsOneWidget);
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
}
