import 'package:ak_ffi_dart/ak_ffi_dart.dart';
import 'package:ak_sending_app/ui/pages/send_page.dart';
import 'package:ak_sending_app/ui/widgets/failure_advice.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'fake_transfer_service.dart';
import 'test_helpers.dart';

const _pathKey = ValueKey('send_path_input');
const _peerKey = ValueKey('send_peer');
const _fpKey = ValueKey('send_fingerprint');

Future<void> _addPath(WidgetTester tester, String path) async {
  await enterField(tester, _pathKey, path);
  await tester.tap(find.byKey(const ValueKey('send_path_add')));
  await tester.pump();
}

Future<void> _fillValidForm(WidgetTester tester) async {
  await _addPath(tester, 'C:\\Users\\me\\Photos');
  await enterField(tester, _peerKey, '192.168.1.5:52341');
  await enterField(tester, _fpKey, kPeerFingerprint);
}

void main() {
  testWidgets('renders form fields, switches off by default, fail conflict '
      'selected', (tester) async {
    final fake = FakeTransferService();
    await pumpPage(tester, SendPage(service: fake));

    expect(find.byKey(_pathKey), findsOneWidget);
    expect(find.byKey(_peerKey), findsOneWidget);
    expect(find.byKey(_fpKey), findsOneWidget);
    expect(find.byKey(const ValueKey('send_multistream')), findsOneWidget);
    expect(find.byKey(const ValueKey('send_no_internet')), findsOneWidget);
    expect(find.text('重名报错'), findsOneWidget);
    expect(find.text('自动重命名'), findsOneWidget);
    expect(find.text('开始发送'), findsOneWidget);
    // Default fingerprint helper copy.
    expect(find.text('粘贴完整指纹，或 8–63 位十六进制前缀。'), findsOneWidget);
  });

  testWidgets('adds and removes source paths', (tester) async {
    final fake = FakeTransferService();
    await pumpPage(tester, SendPage(service: fake));

    await _addPath(tester, 'C:\\Users\\me\\Photos');
    await _addPath(tester, 'D:\\backup');
    expect(find.text('C:\\Users\\me\\Photos'), findsOneWidget);
    expect(find.text('D:\\backup'), findsOneWidget);

    await tester
        .tap(find.descendant(of: find.byKey(const ValueKey('send_root_0')), matching: find.byTooltip('移除此路径')));
    await tester.pump();
    expect(find.text('C:\\Users\\me\\Photos'), findsNothing);
    expect(find.text('D:\\backup'), findsOneWidget);
  });

  testWidgets('validates paths, peer address and fingerprint before start',
      (tester) async {
    final fake = FakeTransferService();
    await pumpPage(tester, SendPage(service: fake));

    // No roots yet.
    await tester.tap(find.byKey(const ValueKey('send_start')));
    await tester.pump();
    expect(fake.startSendCalls, 0);
    expect(find.textContaining('请至少添加一个'), findsOneWidget);

    // Bad peer.
    await _addPath(tester, 'C:\\Users\\me\\Photos');
    await enterField(tester, _peerKey, 'example.com:7100');
    await enterField(tester, _fpKey, kPeerFingerprint);
    await tester.tap(find.byKey(const ValueKey('send_start')));
    await tester.pump();
    expect(fake.startSendCalls, 0);
    expect(find.textContaining('对端地址须为'), findsOneWidget);

    // Bad fingerprint.
    await enterField(tester, _peerKey, '192.168.1.5:52341');
    await enterField(tester, _fpKey, 'nothex');
    await tester.tap(find.byKey(const ValueKey('send_start')));
    await tester.pump();
    expect(fake.startSendCalls, 0);
    expect(find.textContaining('指纹须为 64 位'), findsOneWidget);
  });

  testWidgets('fingerprint prefix shows the pairing-book hint; options are '
      'captured with switches and conflict choice', (tester) async {
    final fake = FakeTransferService();
    await pumpPage(tester, SendPage(service: fake));

    await _addPath(tester, 'C:\\Users\\me\\Photos');
    await enterField(tester, _peerKey, '192.168.1.5:52341');
    await enterField(tester, _fpKey, kPeerFingerprint.substring(0, 16));
    await tester.pump();
    expect(find.textContaining('已输入前缀：将用本机配对簿查补全'), findsOneWidget);

    await tester.tap(find.byKey(const ValueKey('send_multistream')));
    await tester.pump();
    await tester.tap(find.text('自动重命名'));
    await tester.pump();

    await tester.tap(find.byKey(const ValueKey('send_start')));
    await tester.pump();

    expect(fake.startSendCalls, 1);
    final options = fake.lastSendOptions!;
    expect(options.sourceRoots, ['C:\\Users\\me\\Photos']);
    expect(options.peer, '192.168.1.5:52341');
    expect(options.fingerprint, kPeerFingerprint.substring(0, 16));
    expect(options.multistream, isTrue);
    expect(options.noInternet, isFalse);
    expect(options.conflict, 'rename');
    expect(find.byKey(const ValueKey('send_task_card')), findsOneWidget);
  });

  testWidgets('send timeline completes and shows the TransferReport with '
      'speed, duration and roots', (tester) async {
    final fake = FakeTransferService();
    await pumpPage(tester, SendPage(service: fake));
    await _fillValidForm(tester);

    await tester.tap(find.byKey(const ValueKey('send_start')));
    await tester.pump();

    fake.emitSend(const Connecting(job: 's1'));
    fake.emitSend(const Offered(job: 's1', files: 3, bytes: 2097152));
    await tester.pump();
    expect(find.text('建立连接'), findsOneWidget);

    fake.emitSend(const FileStarted(
        job: 's1', file: 0, path: 'photos/img.bin', bytes: 2097152));
    fake.emitSend(const BytesAcked(job: 's1', file: 0, bytes: 2097152));
    fake.emitSend(const FileVerified(job: 's1', file: 0));
    await tester.pump();
    expect(find.text('传输中'), findsOneWidget);

    fake.emitSend(const Verifying(job: 's1'));
    fake.emitSend(const FinalAck(job: 's1', verified: true));
    fake.emitSend(const Done(job: 's1'));
    fake.finishSend(JobSucceeded(TransferReport(
      job: 's1',
      files: 3,
      bytes: 2097152,
      durationMs: 2000,
      peer: kPeerFingerprint,
      roots: const ['C:\\Users\\me\\Photos'],
    )));
    await tester.pump();

    expect(find.text('已完成'), findsOneWidget);
    expect(find.text('传输完成'), findsOneWidget);
    expect(find.textContaining('3 个文件'), findsOneWidget);
    expect(find.textContaining('用时 00:02'), findsOneWidget);
    expect(find.textContaining('1.00 MiB/s'), findsOneWidget);
    expect(find.textContaining('目录：C:\\Users\\me\\Photos'), findsOneWidget);
  });

  testWidgets('send failure shows the red error banner with the mandated '
      'no-server copy', (tester) async {
    final fake = FakeTransferService();
    await pumpPage(tester, SendPage(service: fake));
    await _fillValidForm(tester);

    await tester.tap(find.byKey(const ValueKey('send_start')));
    await tester.pump();

    fake.emitSend(const Connecting(job: 's1'));
    fake.finishSend(const JobFailed(
        job: 's1', error: 'connect timeout: dial 192.168.1.5:52341 failed'));
    await tester.pump();

    expect(find.text('失败'), findsOneWidget);
    expect(find.textContaining('connect timeout'), findsOneWidget);
    expect(find.text(kNoRelayNotice), findsOneWidget);
    expect(find.textContaining('热点'), findsOneWidget);
    expect(find.textContaining('UPnP'), findsOneWidget);
  });
}
