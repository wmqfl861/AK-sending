/// Batch Z widget test — which picker the connection detail page's
/// 「选择文件夹」asks.
///
/// The send-side folder pick is `pickSendDirectory()`: on Android it goes
/// through the SAF tree picker and a staged copy, because the engine only
/// takes filesystem paths. The plain `pickDirectory()` keeps its one
/// meaning — the RECEIVE save directory of the settings page.
library;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:ak_sending_app/services/simple_transfer_fake.dart';
import 'package:ak_sending_app/ui/service/file_pick_gateway.dart';
import 'package:ak_sending_app/ui/service/simple_transfer_service.dart'
    show TaskRole;

import 'w28_test_helpers.dart';

/// What the SEND-side pick answers: a folder the gateway staged.
const PickedEntry _stagedAlbum = PickedEntry(
  path: r'C:\ak-staging-test\stage-album\payload\相册',
  name: '相册',
  isDirectory: true,
  stagingId: 'stage-album',
  stagedFileCount: 3,
  stagedByteCount: 4096,
);

/// What the RECEIVE-side pick answers: the save directory.
const PickedEntry _saveDir = PickedEntry(
  path: r'C:\接收区',
  name: '接收区',
  isDirectory: true,
);

/// A picker double whose two folder picks answer DIFFERENTLY — only the
/// page asking the right method yields the right entry.
FakeFilePickGateway _twoAnswers() => FakeFilePickGateway(
      directoryResult: _saveDir,
      sendDirectoryResult: _stagedAlbum,
    );

/// Pairs a SEND-direction connection and opens its detail page.
Future<LocalSimpleTransferService> _openSendDetail(
    WidgetTester tester, FakeFilePickGateway picker) async {
  final fake = await pumpApp(tester, picker: picker);
  await fake.createCode(TaskRole.send);
  final id = fake.simulatePeerJoin();
  await pumpEvents(tester);
  await tester.pump();
  fake.completeProbe(id);
  await pumpEvents(tester);
  await tester.pump();

  await tester.tap(find.byKey(ValueKey('conn_card_name_$id')));
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 400));
  expect(find.byKey(const ValueKey('conn_detail_page')), findsOneWidget);
  return fake;
}

/// Presses 选择文件发送, then the sheet's 选择文件夹 option.
Future<void> _chooseFolder(WidgetTester tester) async {
  await tester.tap(find.byKey(const ValueKey('conn_pick_files_button')));
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 400));
  await tester.tap(find.byKey(const ValueKey('pick_dir_option')));
  await tester.pump();
}

void main() {
  testWidgets('conn_detail_folder pick: 选择文件夹 asks pickSendDirectory() '
      '— never pickDirectory() — and the staged entry is what goes to '
      'sendFiles', (tester) async {
    final picker = _twoAnswers();
    final fake = await _openSendDetail(tester, picker);
    expect(find.byKey(const ValueKey('pick_dir_option')), findsNothing,
        reason: '前置：方式面板还没打开');

    await _chooseFolder(tester);

    expect(picker.pickSendDirectoryCalls, 1);
    expect(picker.pickDirectoryCalls, 0,
        reason: '接收保存目录的选择不被发送侧借用');
    expect(find.text('相册'), findsOneWidget,
        reason: '已选列表里是发送侧选择的结果');
    expect(find.text('接收区'), findsNothing);
    expect(find.text('文件夹'), findsOneWidget);
    expect(picker.released, isEmpty);

    await tester.tap(find.byKey(const ValueKey('conn_send_button')));
    await tester.pump();
    expect(fake.sendRequests.single.$2, [_stagedAlbum.path]);
  });

  testWidgets('conn_detail_folder pick: a cancelled 选择文件夹 picks '
      'nothing and asks for no other pick', (tester) async {
    final picker = FakeFilePickGateway(); // every folder answer is null
    await _openSendDetail(tester, picker);

    await _chooseFolder(tester);

    expect(picker.pickSendDirectoryCalls, 1);
    expect(picker.pickDirectoryCalls, 0);
    expect(picker.pickFilesCalls, 0);
    expect(find.byKey(const ValueKey('picked_count')), findsNothing);
  });

  testWidgets('conn_detail_folder pick: the settings page\'s receive '
      'save-dir pick still asks pickDirectory() — never pickSendDirectory()',
      (tester) async {
    final picker = _twoAnswers();
    final fake = await pumpApp(tester, picker: picker);

    await tester.tap(find.text('设置'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    expect(find.byKey(const ValueKey('settings_page')), findsOneWidget);

    await tester.tap(find.byKey(const ValueKey('settings_change_dir_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(picker.pickDirectoryCalls, 1);
    expect(picker.pickSendDirectoryCalls, 0);
    expect(fake.saveDir, _saveDir.path,
        reason: '接收保存目录是 pickDirectory() 的结果，不是暂存副本');
    expect(picker.released, isEmpty);
  });
}
