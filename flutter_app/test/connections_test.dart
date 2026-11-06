/// Wave-28 connection list + detail page: cards (name/direction/state),
/// multiple simultaneous connections, the send flow (pick → review →
/// send → progress → result), the receive-direction detail (no send
/// button), and 断开 with its confirmation dialog.
library;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:ak_sending_app/ui/service/file_pick_gateway.dart';
import 'package:ak_sending_app/ui/service/simple_transfer_service.dart';

import 'w28_test_helpers.dart';

void main() {
  testWidgets('cards list peer name, direction and live state; several '
      'connections coexist', (tester) async {
    final fake = await pumpApp(tester);

    await fake.createCode(TaskRole.send);
    final first = fake.simulatePeerJoin(peerName: '手机-Pixel');
    await pumpEvents(tester);
    await tester.pump();

    await fake.createCode(TaskRole.receive);
    final second = fake.simulatePeerJoin(peerName: '台式机-ROG');
    await pumpEvents(tester);
    await tester.pump();

    // Both listed while still probing/connecting.
    expect(find.byKey(ValueKey('conn_card_name_$first')),
        findsOneWidget);
    expect(find.byKey(ValueKey('conn_card_name_$second')),
        findsOneWidget);
    expect(find.byKey(const ValueKey('conn_state_connecting')),
        findsNWidgets(2));

    fake.completeProbe(first);
    await pumpEvents(tester);
    await tester.pump();
    expect(find.byKey(const ValueKey('conn_state_connected')), findsOneWidget);
    expect(find.byKey(const ValueKey('conn_state_connecting')),
        findsOneWidget);

    expect(find.text('我发送 → 对方'), findsOneWidget);
    expect(find.text('对方 → 我接收'), findsOneWidget);
  });

  testWidgets('send-direction detail: pick files → review list with '
      'sizes → send → progress → completion', (tester) async {
    final picker = FakeFilePickGateway(filesResult: kTwoFiles);
    final fake = await pumpApp(tester, picker: picker);

    await fake.createCode(TaskRole.send);
    final id = fake.simulatePeerJoin(peerName: '办公本-ThinkPad');
    await pumpEvents(tester);
    await tester.pump();
    fake.completeProbe(id);
    await pumpEvents(tester);
    await tester.pump();

    // Open the detail page.
    await tester.tap(find.byKey(ValueKey('conn_card_name_$id')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    expect(find.byKey(const ValueKey('conn_detail_page')), findsOneWidget);
    expect(find.byKey(const ValueKey('conn_detail_peer')), findsOneWidget);

    // Pick via the sheet: choose the multi-file option.
    await tester.tap(find.byKey(const ValueKey('conn_pick_files_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    await tester.tap(find.byKey(const ValueKey('pick_files_option')));
    await tester.pump();
    expect(picker.pickFilesCalls, 1);

    // Review list: names + human sizes, then send.
    expect(find.text('已选 2 项'), findsOneWidget);
    expect(find.byKey(const ValueKey('picked_item_0')), findsOneWidget);
    expect(find.byKey(const ValueKey('picked_item_1')), findsOneWidget);
    expect(find.text('1.5 KB'), findsOneWidget);
    expect(find.text('512 KB'), findsOneWidget);

    await tester.tap(find.byKey(const ValueKey('conn_send_button')));
    await tester.pump();
    expect(find.byKey(const ValueKey('conn_send_progress')), findsOneWidget);
    expect(fake.sendRequests.single.$1, id);
    expect(fake.sendRequests.single.$2,
        ['C:\\docs\\照片.zip', 'C:\\docs\\报告.pdf']);

    fake.completeSend('2 个文件已发送');
    await pumpEvents(tester);
    await tester.pump();
    expect(find.byKey(const ValueKey('conn_send_result')), findsOneWidget);
    expect(find.textContaining('发送完成'), findsOneWidget);
    expect(find.textContaining('2 个文件已发送'), findsOneWidget);
  });

  testWidgets('folder pick passes a single directory path to sendFiles',
      (tester) async {
    final picker = FakeFilePickGateway(
      directoryResult: const PickedEntry(
        path: 'C:\\docs\\项目资料',
        name: '项目资料',
        isDirectory: true,
      ),
    );
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
    await tester.tap(find.byKey(const ValueKey('conn_pick_files_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    await tester.tap(find.byKey(const ValueKey('pick_dir_option')));
    await tester.pump();

    expect(find.text('已选 1 项'), findsOneWidget);
    expect(find.text('文件夹'), findsOneWidget);
    expect(picker.pickDirectoryCalls, 1);

    await tester.tap(find.byKey(const ValueKey('conn_send_button')));
    await tester.pump();
    expect(fake.sendRequests.single.$2, ['C:\\docs\\项目资料']);
    fake.completeSend();
    await pumpEvents(tester);
    await tester.pump();
    expect(find.byKey(const ValueKey('conn_send_result')), findsOneWidget);
  });

  testWidgets('receive-direction detail: no send button, save folder '
      'hint instead', (tester) async {
    final fake = await pumpApp(tester);

    await fake.createCode(TaskRole.receive);
    final id = fake.simulatePeerJoin(peerName: '同事的电脑');
    await pumpEvents(tester);
    await tester.pump();
    fake.completeProbe(id);
    await pumpEvents(tester);
    await tester.pump();

    await tester.tap(find.byKey(ValueKey('conn_card_name_$id')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));

    expect(find.byKey(const ValueKey('conn_pick_files_button')), findsNothing);
    expect(find.byKey(const ValueKey('conn_send_button')), findsNothing);
    expect(find.text('此连接用于接收'), findsOneWidget);
    expect(
        find.byKey(const ValueKey('conn_detail_save_dir')), findsOneWidget);
  });

  testWidgets('disconnect asks for confirmation, then drops the card',
      (tester) async {
    final fake = await pumpApp(tester);

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

    await tester.tap(find.byKey(const ValueKey('conn_disconnect_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    expect(find.text('断开连接？'), findsOneWidget);

    // Cancel first: nothing happens.
    await tester.tap(find.byKey(const ValueKey('conn_disconnect_cancel')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    expect(find.byKey(const ValueKey('conn_detail_page')), findsOneWidget);
    expect(fake.disconnectCalls, 0);

    // Confirm: service disconnects, detail pops, card disappears.
    await tester.tap(find.byKey(const ValueKey('conn_disconnect_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    await tester.tap(find.byKey(const ValueKey('conn_disconnect_confirm')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    expect(fake.disconnectCalls, 1);
    expect(fake.disconnectRequests.single, id);
    expect(find.byKey(ValueKey('conn_card_name_$id')), findsNothing);
  });
}
