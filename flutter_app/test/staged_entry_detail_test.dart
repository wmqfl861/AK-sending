/// Batch Z widget tests — the CONNECTION DETAIL page's ownership of staged
/// folders.
///
/// On Android a picked folder is a STAGED copy in app-private storage (the
/// engine only takes filesystem paths). The detail page owns the staged
/// folders in its review list and hands them back through ONE release
/// point wherever its ownership ends: a SUCCESSFUL send (they also leave
/// the list, so nothing can send a released path again), 清空, a new pick
/// replacing them, and `dispose` while no send is in flight. The release
/// point never hands back a folder a STARTED send has not succeeded on.
///
/// A staged folder is released by exactly one thing: the SUCCESSFUL send
/// that carried it. No failure shape proves the engine has stopped reading
/// (U4), so a FAILED send leaves it listed for a retry — and 清空, a new
/// pick and dispose then drop it WITHOUT any release. Entries that were
/// never part of a started send are released as before. Each test pins one
/// point.
library;

import 'dart:async' show Completer, TimeoutException;

import 'package:ak_ffi_dart/ak_ffi_dart.dart' show AkFfiException;
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:ak_sending_app/services/simple_transfer_fake.dart';
import 'package:ak_sending_app/ui/pages/connection_detail_page.dart';
import 'package:ak_sending_app/ui/service/file_pick_gateway.dart';
import 'package:ak_sending_app/ui/service/simple_transfer_service.dart';

import 'w28_test_helpers.dart';

/// A folder the gateway staged before handing it over: the `stagingId` is
/// what marks it (the pages read nothing else of the staged fields).
const PickedEntry _stagedAlbum = PickedEntry(
  path: r'C:\ak-staging-test\stage-album\payload\相册',
  name: '相册',
  isDirectory: true,
  stagingId: 'stage-album',
  stagedFileCount: 3,
  stagedByteCount: 4096,
);

/// A second staged folder (what a new pick brings in).
const PickedEntry _stagedNotes = PickedEntry(
  path: r'C:\ak-staging-test\stage-notes\payload\笔记',
  name: '笔记',
  isDirectory: true,
  stagingId: 'stage-notes',
  stagedFileCount: 1,
  stagedByteCount: 64,
);

/// A plain file: the user's own path, never a staged copy.
const PickedEntry _plainFile = PickedEntry(
  path: r'C:\docs-test\照片.zip',
  name: '照片.zip',
  isDirectory: false,
  sizeBytes: 1536,
);

/// A plain folder pick (no `stagingId`): the user's own path too.
const PickedEntry _plainFolder = PickedEntry(
  path: r'C:\docs-test\资料',
  name: '资料',
  isDirectory: true,
);

/// The text `SimpleTransferService.sendFiles` throws when its stall budget
/// ran out and cancelling the native task FAILED.
final TimeoutException _stallCancelFailed = TimeoutException(
  '发送超时（150 秒无进展），且取消原生任务失败（engine busy）'
  '——传输可能仍在后台进行，请稍后重试或手动断开连接。',
  const Duration(seconds: 150),
);

/// The same stall, where the cancel call SUCCEEDED — which says nothing
/// about the blocking read (U4): it may still be inside a file.
final TimeoutException _stallCancelled = TimeoutException(
  '发送超时（150 秒无进展），已取消原生发送任务并断开该连接。',
  const Duration(seconds: 150),
);

/// How the engine-failed latch fails an in-flight waiter: a plain
/// `StateError` carrying the failure reason.
final Object _engineFailedError =
    StateError('传输引擎故障：连续 10 次任务事件轮询失败。泵已停止，请重启应用恢复。');

/// Every failure shape these tests had while the page still told them
/// apart. Under U4 none of them proves the reading has ended, so all of
/// them must behave exactly the same.
final List<(String, Object)> _failures = [
  (
    'an engine-reported AkFfiException',
    const AkFfiException(2, 'stream failed: connection lost'),
  ),
  (
    'the stall timeout whose cancel call succeeded '
    '(已取消原生发送任务并断开该连接)',
    _stallCancelled,
  ),
  (
    'the stall timeout whose cancel call failed (取消原生任务失败)',
    _stallCancelFailed,
  ),
  ('a StateError', _engineFailedError),
  ('an unknown error type', const FormatException('unexpected frame')),
  (
    'a TimeoutException carrying neither service text',
    TimeoutException('发送无进展超时', const Duration(seconds: 150)),
  ),
];

final Finder _sendButton = find.byKey(const ValueKey('conn_send_button'));
final Finder _pickButton = find.byKey(const ValueKey('conn_pick_files_button'));
final Finder _clearButton = find.byKey(const ValueKey('picked_clear_button'));

/// Pairs a SEND-direction connection (probe passed) and shows its detail
/// page with [initialPicked] seeded — the shape the home page hands over.
/// No automatic send: the tests press 发送 themselves.
Future<LocalSimpleTransferService> _showDetail(
  WidgetTester tester, {
  required FilePickGateway picker,
  List<PickedEntry> initialPicked = const [],
  LocalSimpleTransferService? service,
}) async {
  final fake = service ?? LocalSimpleTransferService();
  addTearDown(fake.dispose);
  await fake.createCode(TaskRole.send);
  final id = fake.simulatePeerJoin(peerName: '办公本-ThinkPad');
  fake.completeProbe(id);
  await tester.pumpWidget(MaterialApp(
    home: ConnectionDetailPage(
      service: fake,
      connectionId: id,
      picker: picker,
      initialPicked: initialPicked,
    ),
  ));
  await pumpEvents(tester);
  await tester.pump();
  return fake;
}

/// The page leaves (its State is disposed) while the test keeps driving
/// the service.
Future<void> _leavePage(WidgetTester tester) async {
  await tester.pumpWidget(const SizedBox.shrink());
  await tester.pump();
}

/// Presses 选择文件发送, then the sheet's multi-file option.
Future<void> _pickFiles(WidgetTester tester) async {
  await tester.tap(_pickButton);
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 400));
  await tester.tap(find.byKey(const ValueKey('pick_files_option')));
  await tester.pump();
}

/// The staging ids handed to `releaseStaged` so far, in call order.
List<String> _releasedIds(FakeFilePickGateway picker) => [
      for (final call in picker.released)
        for (final entry in call) entry.stagingId!,
    ];

/// Shows the detail page with [entries] listed and fails its FIRST send
/// with [error] — the common prefix of the U4 matrix.
Future<(FakeFilePickGateway, _ScriptedSendService)> _failFirstSend(
  WidgetTester tester,
  Object error, {
  List<PickedEntry> entries = const [_stagedAlbum],
}) async {
  final picker = FakeFilePickGateway();
  final fake = _ScriptedSendService();
  await _showDetail(tester,
      picker: picker, service: fake, initialPicked: entries);

  await tester.tap(_sendButton);
  await tester.pump();
  expect(picker.released, isEmpty, reason: '前置：发送进行中，副本不能动');
  fake.failSendWith(error);
  await pumpEvents(tester);
  await tester.pump();

  expect(find.byKey(const ValueKey('conn_send_error')), findsOneWidget,
      reason: '前置：发送已失败');
  return (picker, fake);
}

void main() {
  // -----------------------------------------------------------------------
  // A staged folder is released by exactly one thing: a SUCCESSFUL send.
  // -----------------------------------------------------------------------

  testWidgets('staged_entry detail: a SUCCESSFUL send releases its staged '
      'folder exactly once and removes it from the picked list — the result '
      'display stays', (tester) async {
    final picker = FakeFilePickGateway();
    final fake = await _showDetail(tester,
        picker: picker, initialPicked: const [_plainFile, _stagedAlbum]);
    expect(find.text('已选 2 项'), findsOneWidget);
    expect(picker.released, isEmpty);

    await tester.tap(_sendButton);
    await tester.pump();
    expect(fake.sendRequests.single.$2, [_plainFile.path, _stagedAlbum.path]);
    expect(picker.released, isEmpty,
        reason: '发送进行中：引擎还在读，副本不能动');

    fake.completeSend('已发送 2 个文件');
    await pumpEvents(tester);
    await tester.pump();

    expect(find.byKey(const ValueKey('conn_send_result')), findsOneWidget,
        reason: '结果展示保留');
    expect(picker.released, [[_stagedAlbum]],
        reason: '发送成功：这次发送的暂存条目交还一次');
    expect(find.text('相册'), findsNothing,
        reason: '已释放的路径从已选列表移除');
    expect(find.byKey(const ValueKey('picked_item_1')), findsNothing);
    expect(find.byKey(const ValueKey('picked_item_0')), findsOneWidget,
        reason: '普通文件不是暂存副本，仍留在已选列表');
    expect(find.text('已选 1 项'), findsOneWidget);

    // What is left cannot send the released path again…
    await tester.tap(_sendButton);
    await tester.pump();
    expect(fake.sendRequests.last.$2, [_plainFile.path]);
    fake.completeSend();
    await pumpEvents(tester);

    // …and leaving afterwards hands nothing back a second time.
    await _leavePage(tester);
    expect(picker.released, [[_stagedAlbum]]);
  });

  testWidgets('staged_entry detail: after a failure, a successful retry '
      'releases EXACTLY the staged entry that was sent — the plain file '
      'stays listed', (tester) async {
    final (picker, fake) = await _failFirstSend(
        tester, const AkFfiException(2, 'stream failed: connection lost'),
        entries: const [_plainFile, _stagedAlbum]);
    expect(find.text('已选 2 项'), findsOneWidget,
        reason: '失败后列表不变：留给重试');

    await tester.tap(_sendButton);
    await tester.pump();
    expect(fake.sendRequests.last.$2, [_plainFile.path, _stagedAlbum.path]);
    expect(picker.released, isEmpty);

    fake.completeSendWith();
    await pumpEvents(tester);

    expect(picker.released, [[_stagedAlbum]],
        reason: '成功：只交还这次发送里的暂存条目，恰一次');
    expect(find.byKey(const ValueKey('picked_item_0')), findsOneWidget,
        reason: '普通文件不是暂存副本，仍留在已选列表');
    expect(find.text('已选 1 项'), findsOneWidget);

    await _leavePage(tester);
    expect(picker.released, [[_stagedAlbum]]);
  });

  testWidgets('staged_entry detail: leaving the page while a send is in '
      'flight releases NOTHING — and neither does that send then failing '
      '(the copy goes with the next app start)', (tester) async {
    final picker = FakeFilePickGateway();
    final fake = _ScriptedSendService();
    await _showDetail(tester,
        picker: picker, service: fake, initialPicked: const [_stagedAlbum]);

    await tester.tap(_sendButton);
    await tester.pump();
    expect(find.byKey(const ValueKey('conn_send_progress')), findsOneWidget);

    await _leavePage(tester);
    expect(picker.released, isEmpty, reason: '发送进行中：dispose 不释放');

    // The cancel text that sounds final — the blocking read may not be.
    fake.failSendWith(_stallCancelled);
    await pumpEvents(tester);
    expect(picker.released, isEmpty,
        reason: '页面已销毁 + 发送失败：没有任何失败形态证明读取已结束');
  });

  testWidgets('staged_entry detail: a send left in flight that then '
      'SUCCEEDS is what hands the folder back — its page released nothing '
      'on the way out, so nothing else owns it', (tester) async {
    final picker = FakeFilePickGateway();
    final fake = await _showDetail(tester,
        picker: picker, initialPicked: const [_stagedAlbum]);

    await tester.tap(_sendButton);
    await tester.pump();
    await _leavePage(tester);
    expect(picker.released, isEmpty);

    fake.completeSend();
    await pumpEvents(tester);
    expect(picker.released, [[_stagedAlbum]]);
  });

  testWidgets('staged_entry detail: two _send calls in a row start ONE '
      'send', (tester) async {
    final picker = FakeFilePickGateway();
    final fake = await _showDetail(tester,
        picker: picker, initialPicked: const [_stagedAlbum]);
    expect(tester.widget<FilledButton>(_sendButton).onPressed, isNotNull);

    // Two taps on the still-mounted button — no frame in between, exactly
    // like a double tap.
    await tester.tap(_sendButton);
    await tester.tap(_sendButton);
    await tester.pump();

    expect(fake.sendFilesCalls, 1,
        reason: '发送进行中不得再启动一次发送');
    expect(fake.sendRequests.single.$2, [_stagedAlbum.path]);
    expect(find.byKey(const ValueKey('conn_send_progress')), findsOneWidget);
    expect(picker.released, isEmpty);
  });

  // U1: the single release point protects what a started send is reading.
  testWidgets('staged_entry detail: 「清空」 fired in the SAME frame as '
      '「发送」 — the callback the OLD button still holds — releases nothing '
      'of the folder the engine is reading', (tester) async {
    final picker = FakeFilePickGateway();
    final fake = await _showDetail(tester,
        picker: picker, initialPicked: const [_stagedAlbum]);

    // The 清空 button built BEFORE the send — the callback it still holds.
    final staleClear = tester.widget<TextButton>(_clearButton).onPressed!;

    // 「发送」, then — no frame in between, so the sending state (which
    // hides 清空) is not shown yet — the old 「清空」.
    await tester.tap(_sendButton);
    staleClear();
    await tester.pump();

    expect(fake.sendRequests.single.$2, [_stagedAlbum.path],
        reason: '前置：这次发送在引擎手里');
    expect(picker.released, isEmpty,
        reason: '引擎还在读这次发送的文件夹：清空不得把它交还');

    // The send's own SUCCESS is what hands it back — once.
    fake.completeSend();
    await pumpEvents(tester);
    expect(picker.released, [[_stagedAlbum]]);

    await _leavePage(tester);
    expect(picker.released, [[_stagedAlbum]]);
  });

  testWidgets('staged_entry detail: 「清空」 fired in the SAME frame as '
      '「发送」, and that send then FAILS — the folder is released by '
      'nobody', (tester) async {
    final picker = FakeFilePickGateway();
    final fake = _ScriptedSendService();
    await _showDetail(tester,
        picker: picker, service: fake, initialPicked: const [_stagedAlbum]);

    final staleClear = tester.widget<TextButton>(_clearButton).onPressed!;
    await tester.tap(_sendButton);
    staleClear();
    await tester.pump();
    expect(find.byKey(const ValueKey('picked_count')), findsNothing,
        reason: '既有行为：清空把列表清掉');

    fake.failSendWith(_stallCancelled);
    await pumpEvents(tester);
    await _leavePage(tester);

    expect(picker.released, isEmpty,
        reason: '已从列表移除的条目在发送失败后同样不交还');
  });

  // -----------------------------------------------------------------------
  // U4: no failure shape proves the reading has ended. For EVERY shape:
  // the entry stays listed for a retry, and a later 清空 / new pick / leave
  // drops it WITHOUT any release. Only a successful send releases it.
  // -----------------------------------------------------------------------

  for (final (shape, error) in _failures) {
    testWidgets('staged_entry detail: a failure — $shape — keeps the staged '
        'entry in the picked list for a retry; only a SUCCESSFUL retry '
        'releases it, exactly once', (tester) async {
      final (picker, fake) = await _failFirstSend(tester, error);

      expect(find.byKey(const ValueKey('picked_item_0')), findsOneWidget,
          reason: '没有任何失败形态能证明读取已结束：条目留给重试');
      expect(find.text('已选 1 项'), findsOneWidget);
      expect(picker.released, isEmpty, reason: '失败本身不释放');

      // The retry sends the SAME staged path…
      await tester.tap(_sendButton);
      await tester.pump();
      expect(fake.sendRequests, hasLength(2));
      expect(fake.sendRequests.last.$2, [_stagedAlbum.path]);
      expect(picker.released, isEmpty);

      // …and only ITS success hands the folder back — once.
      fake.completeSendWith();
      await pumpEvents(tester);
      expect(picker.released, [[_stagedAlbum]]);
      expect(find.text('相册'), findsNothing);

      await _leavePage(tester);
      expect(picker.released, [[_stagedAlbum]]);
    });

    testWidgets('staged_entry detail: a failure — $shape — 清空 afterwards '
        'drops the staged entry WITHOUT any release', (tester) async {
      final (picker, _) = await _failFirstSend(tester, error);

      await tester.tap(_clearButton);
      await tester.pump();

      expect(find.byKey(const ValueKey('picked_count')), findsNothing,
          reason: '清空照常清掉列表');
      expect(find.byKey(const ValueKey('conn_send_error')), findsNothing);
      expect(picker.released, isEmpty,
          reason: '引擎可能仍在读：清空只把它移出列表，副本交给下次启动的清理');

      await _leavePage(tester);
      expect(picker.released, isEmpty);
    });

    testWidgets('staged_entry detail: a failure — $shape — a new selection '
        'replacing it drops the staged entry WITHOUT any release', (tester) async {
      final (picker, _) = await _failFirstSend(tester, error);

      picker.filesResult = const [_stagedNotes];
      await _pickFiles(tester);

      expect(find.text('笔记'), findsOneWidget);
      expect(find.text('相册'), findsNothing,
          reason: '新选择照常替换列表');
      expect(picker.released, isEmpty,
          reason: '引擎可能仍在读：替换只把它移出列表，副本交给下次启动的清理');

      // The replacement was never sent: it is the page's to hand back.
      await _leavePage(tester);
      expect(picker.released, [[_stagedNotes]]);
    });

    testWidgets('staged_entry detail: a failure — $shape — leaving the '
        'page drops the staged entry WITHOUT any release', (tester) async {
      final (picker, _) = await _failFirstSend(tester, error);

      await _leavePage(tester);

      expect(picker.released, isEmpty,
          reason: '引擎可能仍在读：离开页面不交还，副本交给下次启动的清理');
    });
  }

  testWidgets('staged_entry detail: an id a started send did not succeed '
      'on stays protected for the page\'s life — even listed again by a '
      'later pick, it is never handed back', (tester) async {
    final (picker, fake) = await _failFirstSend(tester, _engineFailedError);

    // A new pick replaces the list; a later send of ANOTHER folder
    // succeeds…
    picker.filesResult = const [_stagedNotes];
    await _pickFiles(tester);
    expect(find.text('相册'), findsNothing);
    expect(picker.released, isEmpty,
        reason: '替换：失败过的条目只移出列表');
    await tester.tap(_sendButton);
    await tester.pump();
    fake.completeSendWith();
    await pumpEvents(tester);
    expect(picker.released, [[_stagedNotes]],
        reason: '新的一次发送成功：只交还它自己的副本');

    // …and a pick that lists the failed folder AGAIN cannot get it handed
    // back. (Staging ids never repeat in production — this pins the
    // protection itself, not a reachable sequence.)
    picker.filesResult = const [_stagedAlbum];
    await _pickFiles(tester);
    expect(find.text('相册'), findsOneWidget);
    await _leavePage(tester);

    expect(_releasedIds(picker), [_stagedNotes.stagingId],
        reason: '失败过的 id 不经任何路径交还');
  });

  // -----------------------------------------------------------------------
  // Entries that were never part of a started send: released as before.
  // -----------------------------------------------------------------------

  testWidgets('staged_entry detail: 清空 releases the staged folders it '
      'removes — one call, staged entries only', (tester) async {
    final picker = FakeFilePickGateway();
    await _showDetail(tester,
        picker: picker,
        initialPicked: const [_plainFile, _stagedAlbum, _stagedNotes]);
    expect(find.text('已选 3 项'), findsOneWidget);
    expect(picker.released, isEmpty);

    await tester.tap(_clearButton);
    await tester.pump();

    expect(find.byKey(const ValueKey('picked_count')), findsNothing);
    expect(picker.released, [
      [_stagedAlbum, _stagedNotes],
    ], reason: '一次清空 = 一次交还，只含暂存条目');

    // The list is empty: leaving afterwards hands nothing back.
    await _leavePage(tester);
    expect(picker.released, [
      [_stagedAlbum, _stagedNotes],
    ]);
  });

  testWidgets('staged_entry detail: a new pick REPLACES the list — the '
      'replaced staged folder is released, the new one is owned in turn',
      (tester) async {
    final picker = FakeFilePickGateway(filesResult: const [_stagedNotes]);
    await _showDetail(tester,
        picker: picker, initialPicked: const [_stagedAlbum]);
    expect(picker.released, isEmpty);

    await _pickFiles(tester);

    expect(find.text('笔记'), findsOneWidget);
    expect(find.text('相册'), findsNothing);
    expect(picker.released, [[_stagedAlbum]],
        reason: '被替换的暂存条目交还；新条目归页面');

    // The replacement is owned like the entry it replaced.
    await _leavePage(tester);
    expect(picker.released, [
      [_stagedAlbum],
      [_stagedNotes],
    ]);
  });

  testWidgets('staged_entry detail: a CANCELLED pick (an empty result) '
      'replaces the list with nothing, as before — the staged folder goes '
      'back with the list', (tester) async {
    final picker = FakeFilePickGateway(filesResult: const <PickedEntry>[]);
    await _showDetail(tester,
        picker: picker, initialPicked: const [_stagedAlbum]);

    await _pickFiles(tester);

    expect(find.byKey(const ValueKey('picked_count')), findsNothing,
        reason: '既有行为：选择器取消后已选列表为空');
    expect(picker.released, [[_stagedAlbum]],
        reason: '列表没了，其中的暂存条目不能再留在页面上');
  });

  testWidgets('staged_entry detail: a pick that lands WHILE a send is in '
      'flight replaces the list but never releases the folder the engine '
      'is still reading — that send\'s success owns it', (tester) async {
    final gateway = _HoldingFilesPickGateway();
    final fake = await _showDetail(tester,
        picker: gateway, initialPicked: const [_stagedAlbum]);

    // The pick opens BEFORE the send (the pick button is disabled only
    // while a send runs — a picker that was already open is not).
    await tester.tap(_pickButton);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    await tester.tap(find.byKey(const ValueKey('pick_files_option')));
    await tester.pump();
    expect(gateway.openPicks, 1);

    // The send starts with the list as it is…
    await tester.tap(_sendButton);
    await tester.pump();
    expect(fake.sendRequests.single.$2, [_stagedAlbum.path]);

    // …and the pick returns a NEW staged folder while that send runs.
    gateway.answer(const [_stagedNotes]);
    await pumpEvents(tester);

    expect(find.text('笔记'), findsOneWidget, reason: '既有行为：列表被新选择替换');
    expect(gateway.released, isEmpty,
        reason: '引擎还在读：替换列表的挑选不交还在读的副本');

    // The send's OWN success is what hands the read folder back — once.
    fake.completeSend();
    await pumpEvents(tester);
    expect(gateway.released, [[_stagedAlbum]]);

    // The new folder was the page's all along: it goes back on the way out.
    await _leavePage(tester);
    expect(gateway.released, [
      [_stagedAlbum],
      [_stagedNotes],
    ]);
  });

  testWidgets('staged_entry detail: when the send a late pick landed on '
      'then FAILS, the folder it was reading is released by nobody (the '
      'copy goes with the next app start)', (tester) async {
    final gateway = _HoldingFilesPickGateway();
    final fake = await _showDetail(tester,
        picker: gateway, initialPicked: const [_stagedAlbum]);

    await tester.tap(_pickButton);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    await tester.tap(find.byKey(const ValueKey('pick_files_option')));
    await tester.pump();
    await tester.tap(_sendButton);
    await tester.pump();

    gateway.answer(const [_stagedNotes]);
    await pumpEvents(tester);
    expect(gateway.released, isEmpty);

    fake.failSend('连接已断开，发送已中断。');
    await pumpEvents(tester);
    expect(find.byKey(const ValueKey('conn_send_error')), findsOneWidget);
    expect(gateway.released, isEmpty,
        reason: '失败的发送不释放；它读过的副本已不在列表里，交给下次启动的清理');

    // Only the folder the page still lists — never sent — goes back.
    await _leavePage(tester);
    expect(gateway.released, [[_stagedNotes]]);
  });

  testWidgets('staged_entry detail: after a late pick lands, 发送 starts '
      'no SECOND send while the first is still in flight', (tester) async {
    final gateway = _HoldingFilesPickGateway();
    final fake = await _showDetail(tester,
        picker: gateway, initialPicked: const [_stagedAlbum]);

    await tester.tap(_pickButton);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    await tester.tap(find.byKey(const ValueKey('pick_files_option')));
    await tester.pump();
    await tester.tap(_sendButton);
    await tester.pump();

    // The late pick replaces the list and resets the send card: the send
    // button is back, although the engine is still reading the first send.
    gateway.answer(const [_stagedNotes]);
    await pumpEvents(tester);
    expect(find.byKey(const ValueKey('conn_send_progress')), findsNothing,
        reason: '既有行为：迟到的挑选把发送卡复位');
    expect(tester.widget<FilledButton>(_sendButton).onPressed, isNotNull);

    await tester.tap(_sendButton);
    await tester.pump();
    expect(fake.sendFilesCalls, 1,
        reason: '第一次发送仍在进行：不得再启动一次');

    // Once it has settled, 发送 works again — with the list as it is now.
    fake.completeSend();
    await pumpEvents(tester);
    expect(gateway.released, [[_stagedAlbum]]);
    await tester.tap(_sendButton);
    await tester.pump();
    expect(fake.sendFilesCalls, 2);
    expect(fake.sendRequests.last.$2, [_stagedNotes.path]);
  });

  // -----------------------------------------------------------------------
  // The page itself goes away.
  // -----------------------------------------------------------------------

  testWidgets('staged_entry detail: leaving the page with no send in '
      'flight releases the staged folders it still owns — plain files are '
      'not handed over', (tester) async {
    final picker = FakeFilePickGateway();
    await _showDetail(tester,
        picker: picker, initialPicked: const [_plainFile, _stagedAlbum]);

    await _leavePage(tester);

    expect(picker.released, [[_stagedAlbum]]);
  });

  testWidgets('staged_entry detail: a pick that lands AFTER the page left '
      'releases the entries it just obtained (they were never shown)',
      (tester) async {
    final gateway = _HoldingFolderPickGateway();
    await _showDetail(tester, picker: gateway);

    // 选择文件夹: the sheet closes and the system picker is up (held).
    await tester.tap(_pickButton);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    await tester.tap(find.byKey(const ValueKey('pick_dir_option')));
    await tester.pump();
    expect(gateway.openPicks, 1, reason: '前置：选择器开着');

    // The page leaves while the picker is up.
    await _leavePage(tester);
    expect(gateway.released, isEmpty);

    // The pick then returns a STAGED folder: nobody ever showed it.
    gateway.answer(_stagedAlbum);
    await pumpEvents(tester);
    expect(gateway.released, [[_stagedAlbum]],
        reason: '页面已销毁：刚拿到的条目没有任何人接手');
  });

  testWidgets('staged_entry detail: a pick of a PLAIN folder landing after '
      'the page left asks for no release', (tester) async {
    final gateway = _HoldingFolderPickGateway();
    await _showDetail(tester, picker: gateway);

    await tester.tap(_pickButton);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    await tester.tap(find.byKey(const ValueKey('pick_dir_option')));
    await tester.pump();
    expect(gateway.openPicks, 1);

    await _leavePage(tester);
    gateway.answer(_plainFolder);
    await pumpEvents(tester);

    expect(gateway.released, isEmpty, reason: '没有暂存：没有副本要交还');
  });
}

/// Sends stay pending until the test settles them — with a summary, or
/// with a caller-chosen ERROR OBJECT. The plain fake only fails with a
/// `StateError`; these tests fail with every shape the page may see.
class _ScriptedSendService extends LocalSimpleTransferService {
  final List<Completer<String>> _pending = [];

  /// Fails the OLDEST unfinished send with [error].
  void failSendWith(Object error) {
    _pending.removeAt(0).completeError(error);
  }

  /// Completes the OLDEST unfinished send with [summary].
  void completeSendWith([String summary = '发送完成']) {
    _pending.removeAt(0).complete(summary);
  }

  @override
  Future<String> sendFiles(String connId, List<String> paths) {
    sendFilesCalls += 1;
    sendRequests.add((connId, List.of(paths)));
    final completer = Completer<String>();
    _pending.add(completer);
    return completer.future;
  }
}

/// Batch Z double: a MULTI-FILE pick that stays OPEN until the test
/// answers it, plus a record of every `releaseStaged` call.
class _HoldingFilesPickGateway implements FilePickGateway {
  final List<Completer<List<PickedEntry>>> _open = [];

  /// Picks requested so far and not yet answered.
  int get openPicks => _open.length;

  /// Answers the OLDEST open pick with [entries].
  void answer(List<PickedEntry> entries) {
    _open.removeAt(0).complete(entries);
  }

  /// Every list handed to `releaseStaged`, in call order.
  final List<List<PickedEntry>> released = [];

  @override
  Future<List<PickedEntry>> pickFiles() {
    final completer = Completer<List<PickedEntry>>();
    _open.add(completer);
    return completer.future;
  }

  @override
  Future<PickedEntry?> pickDirectory() async => null;

  @override
  Future<PickedEntry?> pickSendDirectory() => pickDirectory();

  @override
  Future<void> releaseStaged(Iterable<PickedEntry> entries) async {
    released.add(List.of(entries));
  }
}

/// Batch Z double: a send-side folder pick that stays OPEN until the test
/// answers it, plus a record of every `releaseStaged` call.
class _HoldingFolderPickGateway implements FilePickGateway {
  final List<Completer<PickedEntry?>> _open = [];

  /// Picks requested so far and not yet answered.
  int get openPicks => _open.length;

  /// Answers the OLDEST open pick with [entry] (null = the user cancelled).
  void answer(PickedEntry? entry) {
    _open.removeAt(0).complete(entry);
  }

  /// Every list handed to `releaseStaged`, in call order.
  final List<List<PickedEntry>> released = [];

  @override
  Future<PickedEntry?> pickSendDirectory() {
    final completer = Completer<PickedEntry?>();
    _open.add(completer);
    return completer.future;
  }

  @override
  Future<PickedEntry?> pickDirectory() async => null;

  @override
  Future<List<PickedEntry>> pickFiles() async => const <PickedEntry>[];

  @override
  Future<void> releaseStaged(Iterable<PickedEntry> entries) async {
    released.add(List.of(entries));
  }
}
