/// Client UI U1 (2026-11-13) widget tests: the send card's live progress
/// (waiting for consent → bytes/percent/rate/time left/file), the ended
/// state once the one-task pairing retires (no misleading "dropped"
/// copy, the send result stays), the connection card's caption, the
/// receive banner's progress and summary, the 「最近传输」 history, and
/// the system dark mode. Driven by the fake's `emitProgress` /
/// `endConnection` knobs.
library;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:ak_sending_app/services/simple_transfer_fake.dart';
import 'package:ak_sending_app/ui/service/simple_transfer_service.dart';

import 'w28_test_helpers.dart';

const int mib = 1024 * 1024;

/// Pairs a SEND-direction connection (probe passed) and opens its detail
/// page. Returns the connection id.
Future<String> openSendDetail(
    WidgetTester tester, LocalSimpleTransferService fake) async {
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
  return id;
}

/// Picks the picker's files through the sheet and presses send.
Future<void> pickAndSend(WidgetTester tester) async {
  await tester.tap(find.byKey(const ValueKey('conn_pick_files_button')));
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 400));
  await tester.tap(find.byKey(const ValueKey('pick_files_option')));
  await tester.pump();
  await tester.tap(find.byKey(const ValueKey('conn_send_button')));
  await tester.pump();
}

double? barValue(WidgetTester tester, String key) => tester
    .widget<LinearProgressIndicator>(find.byKey(ValueKey(key)))
    .value;

void main() {
  testWidgets('send progress: consent wait, live bytes, then the ended '
      'pairing keeps its result and lands in 最近传输', (tester) async {
    final picker = FakeFilePickGateway(filesResult: kTwoFiles);
    final fake = await pumpApp(tester, picker: picker);
    final id = await openSendDetail(tester, fake);
    await pickAndSend(tester);

    // No snapshot yet: the pre-U1 wording on an indeterminate bar.
    expect(find.text('正在发送…'), findsOneWidget);
    expect(barValue(tester, 'conn_send_progress'), isNull);

    fake.emitProgress(TransferProgress(
      connId: id,
      phase: TransferPhase.awaitingConsent,
      totalFiles: 2,
      totalBytes: 4 * mib,
    ));
    await pumpEvents(tester);
    expect(find.text('已发出请求，等待对方点「接受」…'), findsOneWidget);
    expect(barValue(tester, 'conn_send_progress'), isNull,
        reason: '等待接受时不显示猜测的百分比');

    fake.emitProgress(TransferProgress(
      connId: id,
      phase: TransferPhase.transferring,
      totalFiles: 2,
      totalBytes: 4 * mib,
      transferredBytes: mib,
      currentFileName: '照片.zip',
      currentFileIndex: 0,
      bytesPerSecond: 1.0 * mib,
    ));
    await pumpEvents(tester);
    expect(find.text('已发送 1.0 MB / 4.0 MB（25%）'), findsOneWidget);
    expect(find.text('1.0 MB/s · 剩余约 3 秒'), findsOneWidget);
    expect(find.text('正在发送：照片.zip（1/2）'), findsOneWidget);
    expect(barValue(tester, 'conn_send_progress'), 0.25);

    // Another connection's progress never leaks into this page.
    fake.emitProgress(const TransferProgress(
        connId: 'conn-other', phase: TransferPhase.verifying));
    await pumpEvents(tester);
    expect(find.text('已发送 1.0 MB / 4.0 MB（25%）'), findsOneWidget);

    fake.completeSend('已发送 2 个文件（4.0 MB） · 局域网直连');
    await pumpEvents(tester);
    await tester.pump();
    expect(find.byKey(const ValueKey('conn_send_result')), findsOneWidget);
    expect(find.byKey(const ValueKey('conn_send_result_stats')),
        findsOneWidget);

    // v1: the engine retires the row once its ONE transfer finished.
    fake.endConnection(id);
    await pumpEvents(tester);
    await tester.pump();
    expect(find.byKey(const ValueKey('conn_state_ended')), findsOneWidget);
    expect(find.text('本次传输已完成，配对随之结束。如需再次传输，请重新配对。'),
        findsOneWidget);
    expect(find.textContaining('此连接已断开'), findsNothing,
        reason: '正常结束不能说成对端离开或网络变化');
    expect(find.byKey(const ValueKey('conn_send_result')), findsOneWidget,
        reason: '发送卡保留结果，不翻成接收卡');
    expect(find.text('此连接用于接收'), findsNothing);
    expect(find.byKey(const ValueKey('conn_pick_files_button')), findsNothing);
    expect(find.byKey(const ValueKey('conn_disconnect_button')), findsNothing);

    await tester.tap(find.byKey(const ValueKey('conn_back_home_button')));
    await tester.pump();
    // Let the pop transition finish (the platform default page transition
    // can outlast the push-side 400 ms the other tests pump).
    await tester.pump(const Duration(seconds: 2));
    expect(find.byKey(const ValueKey('conn_detail_page')), findsNothing);
    expect(find.byKey(ValueKey('conn_card_name_$id')), findsNothing);
    expect(find.byKey(const ValueKey('history_section')), findsOneWidget);
    expect(find.text('发送 2 个文件（4.0 MB）'), findsOneWidget);
    final item = find.byKey(const ValueKey('history_item_0'));
    expect(
        find.descendant(of: item, matching: find.textContaining('局域网直连')),
        findsOneWidget);
    expect(
        find.descendant(
            of: item, matching: find.textContaining('办公本-ThinkPad')),
        findsOneWidget);
  });

  testWidgets('a connection card captions its running transfer',
      (tester) async {
    final fake = await pumpApp(tester);
    await fake.createCode(TaskRole.send);
    final id = fake.simulatePeerJoin(peerName: '手机-Pixel');
    await pumpEvents(tester);
    await tester.pump();
    fake.completeProbe(id);
    await pumpEvents(tester);
    await tester.pump();
    expect(find.byKey(ValueKey('conn_card_caption_$id')), findsNothing);

    fake.emitProgress(
        TransferProgress(connId: id, phase: TransferPhase.awaitingConsent));
    await pumpEvents(tester);
    expect(find.text('等待对方接受…'), findsOneWidget);
    expect(barValue(tester, 'conn_card_progress_$id'), isNull);

    fake.emitProgress(TransferProgress(
      connId: id,
      phase: TransferPhase.transferring,
      totalBytes: 4 * mib,
      transferredBytes: 3 * mib,
    ));
    await pumpEvents(tester);
    expect(find.text('发送中 75%'), findsOneWidget);
    expect(barValue(tester, 'conn_card_progress_$id'), 0.75);

    fake.emitProgress(TransferProgress(
      connId: id,
      phase: TransferPhase.completed,
      totalBytes: 4 * mib,
      transferredBytes: 4 * mib,
    ));
    await pumpEvents(tester);
    expect(find.byKey(ValueKey('conn_card_caption_$id')), findsNothing);

    fake.endConnection(id);
    await pumpEvents(tester);
    expect(find.byKey(ValueKey('conn_card_name_$id')), findsNothing);
  });

  testWidgets('receive banner: live progress, then the finished summary '
      'and a 最近传输 entry naming the sender', (tester) async {
    final fake = await pumpApp(tester);
    await fake.createCode(TaskRole.receive);
    final id = fake.simulatePeerJoin(peerName: '同事的电脑');
    await pumpEvents(tester);
    await tester.pump();

    fake.emitIncomingRequest(fileCount: 3, totalBytes: 2 * mib, connId: id);
    await pumpEvents(tester);
    await tester.pump();
    await tester.tap(find.byKey(const ValueKey('incoming_accept_job-1')));
    await tester.pump();
    expect(find.text('正在接收…'), findsOneWidget);

    fake.emitProgress(TransferProgress(
      connId: id,
      phase: TransferPhase.transferring,
      totalFiles: 3,
      totalBytes: 2 * mib,
      transferredBytes: mib,
      currentFileName: 'a.txt',
      currentFileIndex: 1,
    ));
    await pumpEvents(tester);
    expect(find.text('已接收 1.0 MB / 2.0 MB（50%）'), findsOneWidget);
    expect(find.text('正在接收：a.txt（2/3）'), findsOneWidget);
    expect(barValue(tester, 'incoming_progress_job-1'), 0.5);
    expect(find.text('接收中 50%'), findsOneWidget, reason: '连接卡同步显示');

    fake.emitProgress(TransferProgress(
      connId: id,
      phase: TransferPhase.verifying,
      totalFiles: 3,
      totalBytes: 2 * mib,
      transferredBytes: 2 * mib,
    ));
    await pumpEvents(tester);
    expect(find.text('正在校验文件完整性…'), findsOneWidget);

    fake.finishIncoming('job-1');
    await pumpEvents(tester);
    await tester.pump();
    expect(find.byKey(const ValueKey('incoming_success_job-1')),
        findsOneWidget);
    expect(find.byKey(const ValueKey('incoming_summary_job-1')),
        findsOneWidget);
    expect(find.byKey(const ValueKey('history_section')), findsOneWidget);
    expect(find.text('接收 3 个文件（2.0 MB）'), findsOneWidget);
    expect(
        find.descendant(
            of: find.byKey(const ValueKey('history_item_0')),
            matching: find.textContaining('同事的电脑')),
        findsOneWidget);
  });

  testWidgets('a failed send is recorded as failed; 清空 empties the list',
      (tester) async {
    final picker = FakeFilePickGateway(filesResult: kTwoFiles);
    final fake = await pumpApp(tester, picker: picker);
    await openSendDetail(tester, fake);
    await pickAndSend(tester);

    fake.failSend('对方拒绝了接收');
    await pumpEvents(tester);
    await tester.pump();
    expect(find.byKey(const ValueKey('conn_send_error')), findsOneWidget);

    await tester.pageBack();
    await tester.pump();
    await tester.pump(const Duration(seconds: 2));
    expect(find.byKey(const ValueKey('conn_detail_page')), findsNothing);
    expect(find.byKey(const ValueKey('history_item_0')), findsOneWidget);
    // 1536 B + 512 KiB of the two picked files.
    expect(find.text('发送 2 个文件（514 KB）'), findsOneWidget);
    expect(
        find.descendant(
            of: find.byKey(const ValueKey('history_item_0')),
            matching: find.textContaining('失败')),
        findsOneWidget);

    await tester.tap(find.byKey(const ValueKey('history_clear_button')));
    await tester.pump();
    expect(find.byKey(const ValueKey('history_section')), findsNothing);
  });

  testWidgets('the app follows the system dark mode', (tester) async {
    tester.platformDispatcher.platformBrightnessTestValue = Brightness.dark;
    addTearDown(tester.platformDispatcher.clearPlatformBrightnessTestValue);
    await pumpApp(tester);
    final context = tester.element(find.byType(NavigationRail));
    expect(Theme.of(context).brightness, Brightness.dark);
  });
}
