/// Batch Z widget tests — the HOME page's ownership of staged folders.
///
/// On Android a picked folder is a STAGED copy in app-private storage (the
/// engine only takes filesystem paths). The home page owns the primary
/// send path's preselected entries while they sit in its pending selection;
/// every point where that ownership ends hands the staged folders back
/// through `releaseStaged` — exactly once each — and the automatic handover
/// to the connection detail page is a TRANSFER (nothing is released there).
/// The handover runs a frame after it is scheduled and re-checks the flow
/// it was scheduled for: a flow that ended inside that window gets no
/// detail page, and the selection goes back. Each test pins one point.
library;

import 'dart:async' show Completer;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:ak_sending_app/services/simple_transfer_fake.dart';
import 'package:ak_sending_app/ui/app.dart';
import 'package:ak_sending_app/ui/service/file_pick_gateway.dart';
import 'package:ak_sending_app/ui/service/save_dir_store.dart';
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

/// A second staged folder (what a 更改 or a late pick brings in).
const PickedEntry _stagedNotes = PickedEntry(
  path: r'C:\ak-staging-test\stage-notes\payload\笔记',
  name: '笔记',
  isDirectory: true,
  stagingId: 'stage-notes',
  stagedFileCount: 1,
  stagedByteCount: 64,
);

/// The home page's 发送文件 entry.
final Finder _sendButton = find.byKey(const ValueKey('home_send_button'));

/// The code card's 更改 affordance.
final Finder _changeButton =
    find.byKey(const ValueKey('flow_change_selection_button'));

/// The peer joins the pending code and the connection is established —
/// the moment the home page hands the preselection to the detail page.
Future<void> _establish(
    WidgetTester tester, LocalSimpleTransferService fake) async {
  final connId = fake.simulatePeerJoin();
  await pumpEvents(tester);
  fake.completeProbe(connId);
  await pumpEvents(tester);
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 400));
  await pumpEvents(tester);
}

/// `pumpApp` for a service the test had to build itself (the shared helper
/// always constructs the plain fake).
Future<T> _pumpWith<T extends LocalSimpleTransferService>(
  WidgetTester tester, {
  required T service,
  required FilePickGateway picker,
}) async {
  await tester.binding.setSurfaceSize(kWideSize);
  addTearDown(() => tester.binding.setSurfaceSize(null));
  addTearDown(service.dispose);
  await tester.pumpWidget(AkSendingApp(
    service: service,
    store: InMemorySaveDirStore(),
    picker: picker,
  ));
  await tester.pump();
  return service;
}

/// Drives the primary send path to its waiting code with a staged folder
/// selected, ends the flow with [endFlow] and expects the folder handed
/// back exactly once. [after] is what the page shows once the flow ended.
Future<void> _expectFlowEndReleases(
  WidgetTester tester, {
  required Future<void> Function(LocalSimpleTransferService fake) endFlow,
  required Finder after,
}) async {
  final picker = FakeFilePickGateway(filesResult: const [_stagedAlbum]);
  final fake = await pumpApp(tester, picker: picker);
  addTearDown(fake.dispose);

  await tester.tap(_sendButton);
  await pumpEvents(tester);
  expect(find.textContaining('已选：1 项'), findsOneWidget,
      reason: '前置：流程带着暂存文件夹在等待对方');
  expect(picker.released, isEmpty, reason: '等待期间归首页所有');

  await endFlow(fake);
  await pumpEvents(tester);
  await tester.pump();

  expect(after, findsOneWidget);
  expect(find.byKey(const ValueKey('flow_pairing_code')), findsNothing);
  expect(find.textContaining('已选：'), findsNothing);
  expect(picker.released, [[_stagedAlbum]],
      reason: '流程结束：仍归首页所有的暂存文件夹交还一次');
}

void main() {
  // -----------------------------------------------------------------------
  // The flow ends while the home page owns the pending selection.
  // -----------------------------------------------------------------------

  testWidgets('staged_entry home: the flow ending by 取消配对 releases the '
      'pending staged folder once', (tester) async {
    await _expectFlowEndReleases(
      tester,
      endFlow: (fake) async {
        await tester.tap(find.byKey(const ValueKey('flow_cancel_button')));
        await tester.pump();
      },
      after: _sendButton,
    );
  });

  testWidgets('staged_entry home: the flow ending by the code\'s TTL expiry '
      'releases the pending staged folder once', (tester) async {
    await _expectFlowEndReleases(
      tester,
      endFlow: (fake) async => fake.expireCode(fake.createdCodes.single.codeId!),
      after: find.byKey(const ValueKey('flow_code_expired')),
    );
  });

  testWidgets('staged_entry home: the flow ending by the engine-failure '
      'latch releases the pending staged folder once', (tester) async {
    await _expectFlowEndReleases(
      tester,
      endFlow: (fake) async => fake.failEngine(),
      after: find.byKey(const ValueKey('engine_failed_card')),
    );
  });

  testWidgets('staged_entry home: 返回 on the FAILED flow releases the kept '
      'staged folder — the failure itself releases nothing', (tester) async {
    final picker = FakeFilePickGateway(filesResult: const [_stagedAlbum]);
    final fake = await pumpApp(tester, picker: picker);
    addTearDown(fake.dispose);

    await tester.tap(_sendButton);
    await pumpEvents(tester);
    fake.simulatePeerJoin();
    await pumpEvents(tester);
    fake.simulateNetworkChange();
    await pumpEvents(tester);
    await tester.pump();

    expect(find.byKey(const ValueKey('flow_error')), findsOneWidget);
    expect(picker.released, isEmpty,
        reason: '失败不是流程结束：保留的选择仍归这个流程，留给重试');

    await tester.tap(find.byKey(const ValueKey('flow_back_button')));
    await pumpEvents(tester);
    expect(_sendButton, findsOneWidget);
    expect(picker.released, [[_stagedAlbum]],
        reason: '返回结束流程：保留的暂存文件夹交还一次');
  });

  // -----------------------------------------------------------------------
  // 更改 (the code card's re-pick).
  // -----------------------------------------------------------------------

  testWidgets('staged_entry home: 更改 with a real pick releases the '
      'REPLACED staged folder — the new one stays owned and is what the '
      'automatic send carries', (tester) async {
    final picker = FakeFilePickGateway(filesResult: const [_stagedAlbum]);
    final fake = await pumpApp(tester, picker: picker);
    addTearDown(fake.dispose);

    await tester.tap(_sendButton);
    await pumpEvents(tester);

    picker.filesResult = const [_stagedNotes];
    await tester.tap(_changeButton);
    await pumpEvents(tester);

    expect(find.textContaining('已选：1 项'), findsOneWidget);
    expect(picker.released, [[_stagedAlbum]],
        reason: '被替换的暂存文件夹交还；新的选择仍归首页');

    await _establish(tester, fake);
    expect(fake.sendRequests.single.$2, [_stagedNotes.path],
        reason: '自动发送用的是更改后的选择');
    expect(picker.released, [[_stagedAlbum]],
        reason: '新的暂存文件夹随交接转给详情页，首页不释放');
  });

  testWidgets('staged_entry home: a cancelled 更改 keeps the old staged '
      'folder and releases nothing', (tester) async {
    final picker = FakeFilePickGateway(filesResult: const [_stagedAlbum]);
    final fake = await pumpApp(tester, picker: picker);
    addTearDown(fake.dispose);

    await tester.tap(_sendButton);
    await pumpEvents(tester);

    picker.filesResult = const <PickedEntry>[];
    await tester.tap(_changeButton);
    await pumpEvents(tester);

    expect(find.textContaining('已选：1 项'), findsOneWidget,
        reason: '取消更改保留上次选择');
    expect(picker.released, isEmpty, reason: '取消更改不释放任何条目');

    // The kept folder is still the owned selection — and still what the
    // automatic send carries.
    await _establish(tester, fake);
    expect(fake.sendRequests.single.$2, [_stagedAlbum.path]);
    expect(picker.released, isEmpty);
  });

  // -----------------------------------------------------------------------
  // A pick result is discarded.
  // -----------------------------------------------------------------------

  testWidgets('staged_entry home: a discarded INITIAL pick is released — '
      'another flow began behind the picker (P1-A)', (tester) async {
    final gateway = _HoldingPickGateway();
    final fake = await pumpApp(tester, picker: gateway);
    addTearDown(fake.dispose);

    // The send main path's pick opens and STAYS open.
    await tester.tap(_sendButton);
    await tester.pump();
    expect(gateway.openPicks, 1);

    // The user moves on: 生成接收码 starts another flow (the generation
    // moves on behind the picker).
    await tester.tap(
        find.byKey(const ValueKey('home_receive_create_code_button')));
    await pumpEvents(tester);
    expect(find.byKey(const ValueKey('flow_pairing_code')), findsOneWidget,
        reason: '前置：接收流程已在等待');

    // The abandoned send pick returns a STAGED folder: discarded — it
    // belongs to no flow, so its copy goes back.
    gateway.answer(const [_stagedAlbum]);
    await pumpEvents(tester);
    await tester.pump();

    expect(fake.createCodeCalls, 1, reason: '作废的选择不再生码');
    expect(fake.createdCodes.single.role, TaskRole.receive);
    expect(find.textContaining('已选：'), findsNothing);
    expect(gateway.released, [[_stagedAlbum]]);
  });

  testWidgets('staged_entry home: a discarded 更改 result is released — the '
      'kept staged folder survives the failure + retry (P1-D)', (tester) async {
    final gateway = _HoldingPickGateway();
    final fake = await pumpApp(tester, picker: gateway);
    addTearDown(fake.dispose);

    await tester.tap(_sendButton);
    await tester.pump();
    gateway.answer(const [_stagedAlbum]);
    await pumpEvents(tester);
    expect(find.textContaining('已选：1 项'), findsOneWidget);
    await tester.tap(_changeButton);
    await tester.pump();
    expect(gateway.openPicks, 1);

    // The connection completes... and drops: the flow fails while the
    // system picker is still up.
    fake.simulatePeerJoin();
    await pumpEvents(tester);
    fake.simulateNetworkChange();
    await pumpEvents(tester);
    await tester.pump();
    expect(find.textContaining('与对方的连接断开了'), findsOneWidget);
    expect(gateway.released, isEmpty,
        reason: '失败不释放：保留的暂存文件夹随失败+重试存活');

    // 重试: a FRESH flow (a new generation), still carrying the kept folder.
    await tester.tap(find.byKey(const ValueKey('flow_retry_button')));
    await pumpEvents(tester);
    expect(find.byKey(const ValueKey('flow_pairing_code')), findsOneWidget);
    expect(find.textContaining('已选：1 项'), findsOneWidget);

    // The stale pick returns another staged folder: it belongs to the dead
    // flow — discarded, and its copy goes back.
    gateway.answer(const [_stagedNotes]);
    await pumpEvents(tester);
    await tester.pump();
    expect(gateway.released, [[_stagedNotes]],
        reason: '因流程代际变化而丢弃的结果：暂存文件夹交还');

    // The retried pairing sends the KEPT folder — never the stale pick's.
    await _establish(tester, fake);
    expect(fake.sendFilesCalls, 1);
    expect(fake.sendRequests.single.$2, [_stagedAlbum.path]);
    expect(gateway.released, [[_stagedNotes]]);
  });

  testWidgets('staged_entry home: the flow ending while 更改 is open '
      'releases the kept folder at the flow end AND the late result when it '
      'returns', (tester) async {
    final gateway = _HoldingPickGateway();
    final fake = await pumpApp(tester, picker: gateway);
    addTearDown(fake.dispose);

    await tester.tap(_sendButton);
    await tester.pump();
    gateway.answer(const [_stagedAlbum]);
    await pumpEvents(tester);
    await tester.tap(_changeButton);
    await tester.pump();
    expect(gateway.openPicks, 1, reason: '前置：更改的选择器开着');

    // 取消配对 resets the flow while the picker is up.
    await tester.tap(find.byKey(const ValueKey('flow_cancel_button')));
    await pumpEvents(tester);
    expect(_sendButton, findsOneWidget, reason: '前置：流程已结束');
    expect(gateway.released, [[_stagedAlbum]],
        reason: '流程结束：保留的暂存文件夹交还');

    // The late result returns into no flow.
    gateway.answer(const [_stagedNotes]);
    await pumpEvents(tester);
    await tester.pump();

    expect(fake.createCodeCalls, 1, reason: '迟到的结果不生码');
    expect(find.byKey(const ValueKey('conn_detail_page')), findsNothing);
    expect(find.textContaining('已选：'), findsNothing);
    expect(gateway.released, [
      [_stagedAlbum],
      [_stagedNotes],
    ]);
  });

  testWidgets('staged_entry home: an INITIAL pick returning after the page '
      'is gone is released', (tester) async {
    final gateway = _HoldingPickGateway();
    await pumpApp(tester, picker: gateway);

    await tester.tap(_sendButton);
    await tester.pump();
    expect(gateway.openPicks, 1);

    await tester.pumpWidget(const SizedBox.shrink());
    await tester.pump();
    expect(gateway.released, isEmpty, reason: '页面没有选中任何东西');

    gateway.answer(const [_stagedAlbum]);
    await pumpEvents(tester);
    expect(gateway.released, [[_stagedAlbum]],
        reason: '页面已离开：没有任何人能接手这个结果');
  });

  testWidgets('staged_entry home: a 更改 pick returning after the page is '
      'gone is released — once for the kept folder (dispose) and once for '
      'the result', (tester) async {
    final gateway = _HoldingPickGateway();
    await pumpApp(tester, picker: gateway);

    await tester.tap(_sendButton);
    await tester.pump();
    gateway.answer(const [_stagedAlbum]);
    await pumpEvents(tester);
    await tester.tap(_changeButton);
    await tester.pump();
    expect(gateway.openPicks, 1);

    await tester.pumpWidget(const SizedBox.shrink());
    await tester.pump();
    expect(gateway.released, [[_stagedAlbum]],
        reason: 'dispose：仍归首页所有的选择交还');

    gateway.answer(const [_stagedNotes]);
    await pumpEvents(tester);
    expect(gateway.released, [
      [_stagedAlbum],
      [_stagedNotes],
    ]);
  });

  // -----------------------------------------------------------------------
  // The page itself goes away, and the handover's own edge.
  // -----------------------------------------------------------------------

  testWidgets('staged_entry home: dispose while the page owns the staged '
      'folder releases it once', (tester) async {
    final picker = FakeFilePickGateway(filesResult: const [_stagedAlbum]);
    await pumpApp(tester, picker: picker);

    await tester.tap(_sendButton);
    await pumpEvents(tester);
    expect(find.byKey(const ValueKey('flow_pairing_code')), findsOneWidget,
        reason: '前置：流程带着暂存文件夹在等待');
    expect(picker.released, isEmpty);

    await tester.pumpWidget(const SizedBox.shrink());
    await tester.pump();

    expect(picker.released, [[_stagedAlbum]],
        reason: '页面销毁时仍归首页所有的暂存文件夹交还');
  });

  testWidgets('staged_entry home: the handover\'s post-frame callback '
      'finding the page gone releases the selection it detached', (tester) async {
    final picker = FakeFilePickGateway(filesResult: const [_stagedAlbum]);
    final fake = await pumpApp(tester, picker: picker);
    addTearDown(fake.dispose);

    await tester.tap(_sendButton);
    await pumpEvents(tester);
    final connId = fake.simulatePeerJoin();
    await pumpEvents(tester);

    // The connection completes. Microtasks only — NO frame: the handover
    // has taken the selection (detached it) and registered its post-frame
    // callback, which has not run yet.
    fake.completeProbe(connId);
    await tester.idle();
    expect(find.byKey(const ValueKey('conn_detail_page')), findsNothing,
        reason: '前置：交接的 post-frame 回调还没跑');
    expect(picker.released, isEmpty);

    // The page leaves before that frame: nothing else will ever free the
    // detached selection.
    await tester.pumpWidget(const SizedBox.shrink());
    await tester.pump();

    expect(picker.released, [[_stagedAlbum]],
        reason: '交接回调发现页面已销毁：已从待选列表摘下的条目交还');
    expect(find.byKey(const ValueKey('conn_detail_page')), findsNothing);
  });

  // -----------------------------------------------------------------------
  // U3: the handover re-checks the flow it was scheduled for.
  // -----------------------------------------------------------------------

  testWidgets('staged_entry home: a 取消配对 whose reset lands INSIDE the '
      'handover window (the pairing succeeded during the revoke '
      'round-trip) pushes no detail page and releases the folder once',
      (tester) async {
    final picker = FakeFilePickGateway(filesResult: const [_stagedAlbum]);
    final fake = _HeldRevokeService();
    await _pumpWith(tester, service: fake, picker: picker);

    await tester.tap(_sendButton);
    await pumpEvents(tester);
    expect(find.textContaining('已选：1 项'), findsOneWidget);

    // 取消配对 while the code still waits: its revoke is on its way to the
    // engine, so nothing has been reset yet.
    await tester.tap(find.byKey(const ValueKey('flow_cancel_button')));
    await pumpEvents(tester);
    expect(fake.revokeHeld, isTrue,
        reason: '前置：撤码请求已发出并被扣住，流程尚未复位');
    expect(find.byKey(const ValueKey('flow_pairing_code')), findsOneWidget);

    // The peer's join lands FIRST: the pairing succeeds and the handover
    // is scheduled — the selection is detached, nothing is pushed yet.
    final connId = fake.simulatePeerJoin();
    await pumpEvents(tester);
    fake.completeProbe(connId);
    await tester.idle(); // microtasks only — no frame
    expect(find.byKey(const ValueKey('conn_detail_page')), findsNothing,
        reason: '前置：交接的 post-frame 回调还没跑');
    expect(picker.released, isEmpty);

    // THEN the cancel's reset lands — inside the window.
    fake.finishRevoke();
    await tester.idle();

    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    await pumpEvents(tester);

    expect(find.byKey(const ValueKey('conn_detail_page')), findsNothing,
        reason: 'U3：流程已复位——不得再把旧选择交出去自动发送');
    expect(fake.sendFilesCalls, 0);
    expect(find.byKey(ValueKey('conn_card_name_$connId')), findsOneWidget,
        reason: '已建立的连接仍在，只是不再自动发送');
    expect(_sendButton, findsOneWidget);
    expect(picker.released, [[_stagedAlbum]],
        reason: 'U3：回调持有的暂存文件夹交还一次');

    // Nothing hands it back a second time.
    await tester.pumpWidget(const SizedBox.shrink());
    await tester.pump();
    expect(picker.released, [[_stagedAlbum]]);
  });

  testWidgets('staged_entry home: the OLD 取消配对 button firing AFTER the '
      'success was scheduled resets the flow — no detail page, the folder '
      'released once', (tester) async {
    final picker = FakeFilePickGateway(filesResult: const [_stagedAlbum]);
    final fake = await pumpApp(tester, picker: picker);
    addTearDown(fake.dispose);

    await tester.tap(_sendButton);
    await pumpEvents(tester);
    // The 取消配对 button built while the code waits — the callback it
    // still holds.
    final staleCancel = tester
        .widget<TextButton>(
            find.byKey(const ValueKey('flow_cancel_button')))
        .onPressed!;

    final connId = fake.simulatePeerJoin();
    await pumpEvents(tester);
    fake.completeProbe(connId);
    await tester.idle(); // microtasks only — no frame
    expect(find.byKey(const ValueKey('conn_detail_page')), findsNothing,
        reason: '前置：交接的 post-frame 回调还没跑');
    expect(picker.released, isEmpty);

    // It still fires. The pairing is already made, so the controller only
    // resets the flow — the established connection stays.
    staleCancel();
    await tester.idle();

    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    await pumpEvents(tester);

    expect(find.byKey(const ValueKey('conn_detail_page')), findsNothing,
        reason: 'U3：流程已复位——不得再把旧选择交出去自动发送');
    expect(fake.sendFilesCalls, 0);
    expect(find.byKey(ValueKey('conn_card_name_$connId')), findsOneWidget,
        reason: '已建立的连接仍在');
    expect(_sendButton, findsOneWidget);
    expect(picker.released, [[_stagedAlbum]],
        reason: 'U3：回调持有的暂存文件夹交还一次');
  });

  testWidgets('staged_entry home: the row going TERMINAL inside the '
      'handover window (unreachable verdict, then the connection drops) '
      'pushes no detail page and releases the folder once — only the '
      'GENERATION check catches it, the flow keeps the same connection',
      (tester) async {
    final picker = FakeFilePickGateway(filesResult: const [_stagedAlbum]);
    final fake = await pumpApp(tester, picker: picker);
    addTearDown(fake.dispose);

    await tester.tap(_sendButton);
    await pumpEvents(tester);
    final connId = fake.simulatePeerJoin();
    await pumpEvents(tester);

    // The probe fails: paired-but-unreachable still counts as established
    // for the send, so the handover is scheduled.
    fake.failProbe(connId);
    await tester.idle(); // microtasks only — no frame
    expect(find.byKey(const ValueKey('conn_detail_page')), findsNothing,
        reason: '前置：交接的 post-frame 回调还没跑');
    expect(picker.released, isEmpty);

    // Inside the window the connection drops. The flow FAILS on the SAME
    // connection (its row is carried as `disconnected`): the generation
    // moves, the connection id does not.
    fake.simulateNetworkChange();
    await tester.idle();

    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    await pumpEvents(tester);

    expect(find.byKey(const ValueKey('conn_detail_page')), findsNothing,
        reason: 'U3：流程已换代——不得打开一条已断开的连接的页面');
    expect(fake.sendFilesCalls, 0);
    expect(find.textContaining('与对方的连接断开了'), findsOneWidget,
        reason: '失败卡保持可见');
    expect(picker.released, [[_stagedAlbum]],
        reason: 'U3：回调持有的暂存文件夹交还一次');
  });

  // -----------------------------------------------------------------------
  // NOT a release: the handover to the connection detail page.
  // -----------------------------------------------------------------------

  testWidgets('staged_entry home: the connection completing and the detail '
      'page auto-opening is a TRANSFER — nothing is released at home', (tester) async {
    final picker = FakeFilePickGateway(filesResult: const [_stagedAlbum]);
    final fake = await pumpApp(tester, picker: picker);
    addTearDown(fake.dispose);

    await tester.tap(_sendButton);
    await pumpEvents(tester);
    expect(picker.released, isEmpty);

    await _establish(tester, fake);

    expect(find.byKey(const ValueKey('conn_detail_page')), findsOneWidget);
    expect(find.text('相册'), findsOneWidget,
        reason: '暂存文件夹随交接进了详情页的已选列表');
    expect(fake.sendRequests.single.$2, [_stagedAlbum.path]);
    expect(picker.released, isEmpty, reason: '交接是转移，不是释放');

    // From here the DETAIL page owns it: ITS successful send is what hands
    // the staged folder back — exactly once.
    fake.completeSend('发送完成');
    await pumpEvents(tester);
    await tester.pump();
    expect(find.byKey(const ValueKey('conn_send_result')), findsOneWidget);
    expect(picker.released, [[_stagedAlbum]]);
  });
}

/// Batch Z double: a multi-file pick that stays OPEN until the test answers
/// it, plus a record of every `releaseStaged` call.
class _HoldingPickGateway implements FilePickGateway {
  final List<Completer<List<PickedEntry>>> _open = [];

  /// Picks requested so far and not yet answered.
  int get openPicks => _open.length;

  /// Answers the OLDEST open pick with [entries] (an empty list = the user
  /// cancelled it).
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

/// Batch Z double (U3): holds the code revoke behind 取消配对 until the
/// test answers it — the engine round-trip during which the peer's join can
/// still land (whichever side the engine processes first wins).
class _HeldRevokeService extends LocalSimpleTransferService {
  final Completer<void> _revoke = Completer<void>();
  bool _revokeHeld = false;

  /// Whether a code revoke has reached this service and is being held.
  bool get revokeHeld => _revokeHeld;

  /// Lets the held revoke finish; the controller resets the flow after it.
  void finishRevoke() => _revoke.complete();

  @override
  Future<void> disconnect(String connId) async {
    final code = pendingCode;
    if (code != null && code.codeId == connId) {
      _revokeHeld = true;
      await _revoke.future; // the revoke is on its way to the engine
    }
    return super.disconnect(connId);
  }
}
