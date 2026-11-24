/// Batch Z widget tests — the CONNECTION DETAIL page's ownership of staged
/// folders.
///
/// On Android a picked folder is a STAGED copy in app-private storage (the
/// engine only takes filesystem paths). The detail page owns the staged
/// folders in its review list and hands them back through `releaseStaged`
/// wherever that ownership ends: a SUCCESSFUL send (they also leave the
/// list, so nothing can send a released path again), 清空, a new pick
/// replacing them, and `dispose` while no send is in flight. A FAILED send
/// releases nothing on its own — a mounted page keeps the entries for a
/// retry, an unmounted one leaves them to the stale cleanup — except that
/// an UNCONFIRMED native cancel QUARANTINES them. Each test pins one point.
library;

import 'dart:async' show Completer, TimeoutException;

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

void main() {
  // -----------------------------------------------------------------------
  // The send settles.
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

  testWidgets('staged_entry detail: an ordinary FAILED send releases '
      'nothing while the page is mounted — the staged folder stays for the '
      'retry', (tester) async {
    final picker = FakeFilePickGateway();
    final fake = await _showDetail(tester,
        picker: picker, initialPicked: const [_stagedAlbum]);

    await tester.tap(_sendButton);
    await tester.pump();
    fake.failSend('对方拒绝了接收');
    await pumpEvents(tester);
    await tester.pump();

    expect(find.byKey(const ValueKey('conn_send_error')), findsOneWidget);
    expect(find.byKey(const ValueKey('picked_item_0')), findsOneWidget,
        reason: 'Future 返回不等于引擎已停止读取：条目留给重试');
    expect(picker.released, isEmpty);

    // The retry sends the SAME staged path…
    await tester.tap(_sendButton);
    await tester.pump();
    expect(fake.sendFilesCalls, 2);
    expect(fake.sendRequests.last.$2, [_stagedAlbum.path]);
    expect(picker.released, isEmpty);

    // …and only ITS success hands the folder back.
    fake.completeSend();
    await pumpEvents(tester);
    expect(picker.released, [[_stagedAlbum]]);
  });

  testWidgets('staged_entry detail: leaving the page while a send is in '
      'flight releases NOTHING — and neither does that send then failing '
      '(the stale cleanup owns the copy)', (tester) async {
    final picker = FakeFilePickGateway();
    final fake = await _showDetail(tester,
        picker: picker, initialPicked: const [_stagedAlbum]);

    await tester.tap(_sendButton);
    await tester.pump();
    expect(find.byKey(const ValueKey('conn_send_progress')), findsOneWidget);

    await _leavePage(tester);
    expect(picker.released, isEmpty, reason: '发送进行中：dispose 不释放');

    fake.failSend('连接已断开，发送已中断。');
    await pumpEvents(tester);
    expect(picker.released, isEmpty,
        reason: '页面已销毁 + 发送失败：交给以后的过期清理');
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

  // -----------------------------------------------------------------------
  // The UNCONFIRMED native cancel.
  // -----------------------------------------------------------------------

  testWidgets('staged_entry detail: a failed send that signals an '
      'UNCONFIRMED native cancel QUARANTINES its staged folder — gone from '
      'the picked list, never released by any later path', (tester) async {
    final picker = FakeFilePickGateway();
    final fake = _StalledSendService(cancelFailed: true);
    await _showDetail(tester,
        picker: picker,
        service: fake,
        initialPicked: const [_plainFile, _stagedAlbum]);

    await tester.tap(_sendButton);
    await tester.pump();
    expect(fake.sendRequests.single.$2, [_plainFile.path, _stagedAlbum.path]);
    await pumpEvents(tester);
    await tester.pump();

    expect(find.byKey(const ValueKey('conn_send_error')), findsOneWidget);
    expect(find.text('相册'), findsNothing,
        reason: '后台可能仍在读：暂存条目移出已选列表，不能再发');
    expect(find.byKey(const ValueKey('picked_item_0')), findsOneWidget,
        reason: '普通文件仍可重试');
    expect(find.text('已选 1 项'), findsOneWidget);
    expect(picker.released, isEmpty,
        reason: '隔离不是释放：副本交给以后的过期清理');

    // A retry cannot carry the quarantined path…
    await tester.tap(_sendButton);
    await tester.pump();
    expect(fake.sendRequests.last.$2, [_plainFile.path]);

    // …and nothing that follows may hand the copy back.
    await pumpEvents(tester);
    await tester.tap(_clearButton);
    await tester.pump();
    expect(find.byKey(const ValueKey('picked_count')), findsNothing);
    expect(picker.released, isEmpty, reason: '清空');

    // A new pick brings ANOTHER staged folder: that one is owned like any
    // other — the quarantine is per entry, not a page state.
    picker.filesResult = const [_stagedNotes];
    await _pickFiles(tester);
    expect(find.text('笔记'), findsOneWidget);
    expect(picker.released, isEmpty, reason: '新的选择');

    // On its way out the page hands back what it owns: the NEW folder only.
    await _leavePage(tester);
    expect(picker.released, [[_stagedNotes]], reason: 'dispose');
    expect(_releasedIds(picker), isNot(contains(_stagedAlbum.stagingId)),
        reason: '被隔离的副本经任何后续路径都不交还');
  });

  testWidgets('staged_entry detail: the same stall timeout WITHOUT the '
      'unconfirmed-cancel text is an ordinary failure — the staged folder '
      'stays for the retry', (tester) async {
    final picker = FakeFilePickGateway();
    final fake = _StalledSendService(cancelFailed: false);
    await _showDetail(tester,
        picker: picker, service: fake, initialPicked: const [_stagedAlbum]);

    await tester.tap(_sendButton);
    await tester.pump();
    await pumpEvents(tester);
    await tester.pump();

    expect(find.byKey(const ValueKey('conn_send_error')), findsOneWidget);
    expect(find.byKey(const ValueKey('picked_item_0')), findsOneWidget,
        reason: '已确认的取消：引擎已停手，条目仍归页面');
    expect(picker.released, isEmpty);

    await tester.tap(_sendButton);
    await tester.pump();
    expect(fake.sendRequests.last.$2, [_stagedAlbum.path]);
  });

  // -----------------------------------------------------------------------
  // The list changes without a send settling.
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

  testWidgets('staged_entry detail: 清空 after a FAILED send releases the '
      'staged folder it removes', (tester) async {
    final picker = FakeFilePickGateway();
    final fake = await _showDetail(tester,
        picker: picker, initialPicked: const [_stagedAlbum]);

    await tester.tap(_sendButton);
    await tester.pump();
    fake.failSend('对方拒绝了接收');
    await pumpEvents(tester);
    await tester.pump();
    expect(picker.released, isEmpty,
        reason: '前置：失败本身不释放');

    // The user gives up on the retry.
    await tester.tap(_clearButton);
    await tester.pump();

    expect(find.byKey(const ValueKey('conn_send_error')), findsNothing);
    expect(picker.released, [[_stagedAlbum]]);
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
        reason: '引擎可能还在读：替换列表的挑选不交还在读的副本');

    // The send's OWN completion is what hands the read folder back — once.
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
      'stale cleanup owns it)', (tester) async {
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
        reason: '失败的发送不释放；它读过的副本已不在列表里，交给过期清理');

    // Only the folder the page still lists goes back on the way out.
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

/// Fails every send the way `SimpleTransferService.sendFiles` reports a
/// stalled send: a `TimeoutException`. [cancelFailed] picks the text the
/// service uses when cancelling the native task FAILED (the engine may
/// still be reading) over the text of a confirmed cancel.
class _StalledSendService extends LocalSimpleTransferService {
  _StalledSendService({required this.cancelFailed});

  final bool cancelFailed;

  @override
  Future<String> sendFiles(String connId, List<String> paths) async {
    sendFilesCalls += 1;
    sendRequests.add((connId, List.of(paths)));
    throw TimeoutException(
      cancelFailed
          ? '发送超时（150 秒无进展），且取消原生任务失败（engine busy）'
              '——传输可能仍在后台进行，请稍后重试或手动断开连接。'
          : '发送超时（150 秒无进展），已取消原生发送任务并断开该连接。',
      const Duration(seconds: 150),
    );
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
