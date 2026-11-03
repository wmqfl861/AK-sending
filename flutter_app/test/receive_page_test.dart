import 'package:ak_ffi_dart/ak_ffi_dart.dart';
import 'package:ak_sending_app/ui/pages/receive_page.dart';
import 'package:ak_sending_app/ui/service/transfer_service.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'fake_transfer_service.dart';
import 'test_helpers.dart';

const _outDirKey = ValueKey('receive_out_dir');
const _expectKey = ValueKey('receive_expect');

Future<void> _fillBasics(WidgetTester tester) =>
    enterField(tester, _outDirKey, 'C:\\ak-in');

void main() {
  testWidgets('renders the form with book admission selected by default',
      (tester) async {
    final fake = FakeTransferService();
    await pumpPage(tester, ReceivePage(service: fake));

    expect(find.byKey(_outDirKey), findsOneWidget);
    expect(find.byKey(const ValueKey('receive_port')), findsOneWidget);
    expect(find.text('配对簿（默认）'), findsOneWidget);
    expect(find.text('指定指纹或配对码'), findsOneWidget);
    expect(find.text('接受每一个对端（any）'), findsOneWidget);
    expect(find.text('开始监听'), findsOneWidget);
    expect(find.text('停止'), findsOneWidget);
    // The pinned input only appears when that form is selected.
    expect(find.byKey(_expectKey), findsNothing);
  });

  testWidgets('out-dir field stays empty when the service has no platform '
      'default (desktop typed-path UX)', (tester) async {
    final fake = FakeTransferService(); // defaultReceiveOutDirValue: ''
    await pumpPage(tester, ReceivePage(service: fake));
    await tester.pump();

    expect(
      tester.widget<TextField>(find.byKey(_outDirKey)).controller?.text,
      '',
    );
  });

  testWidgets('out-dir field prefills from the platform default when the '
      'service offers one (Android app-specific Download)', (tester) async {
    final fake = FakeTransferService()
      ..defaultReceiveOutDirValue =
          '/storage/emulated/0/Android/data/dev.ak/files/Download';
    await pumpPage(tester, ReceivePage(service: fake));
    await tester.pump();

    expect(
      tester.widget<TextField>(find.byKey(_outDirKey)).controller?.text,
      '/storage/emulated/0/Android/data/dev.ak/files/Download',
    );
  });

  testWidgets('pinned admission validates the expect text before starting',
      (tester) async {
    final fake = FakeTransferService();
    await pumpPage(tester, ReceivePage(service: fake));
    await _fillBasics(tester);

    await tester.tap(find.text('指定指纹或配对码'));
    await tester.pump();
    expect(find.byKey(_expectKey), findsOneWidget);

    // Empty expect text.
    await tester.tap(find.text('开始监听'));
    await tester.pump();
    expect(fake.startReceiveCalls, 0);
    expect(find.text('请输入对端指纹或 6 位配对码。'), findsOneWidget);

    // Malformed expect text.
    await enterField(tester, _expectKey, 'zzzz');
    await tester.tap(find.text('开始监听'));
    await tester.pump();
    expect(fake.startReceiveCalls, 0);
    expect(find.textContaining('需为 64 位十六进制指纹'), findsOneWidget);
  });

  testWidgets('pinned pairing code starts without a warning dialog',
      (tester) async {
    final fake = FakeTransferService();
    await pumpPage(tester, ReceivePage(service: fake));
    await _fillBasics(tester);

    await tester.tap(find.text('指定指纹或配对码'));
    await tester.pump();
    await enterField(tester, _expectKey, '123 456');
    await tester.tap(find.text('开始监听'));
    await tester.pump();

    expect(fake.startReceiveCalls, 1);
    expect(find.text('确认放开准入限制？'), findsNothing);
    expect(fake.lastReceiveOptions?.expectText, '123 456');
  });

  testWidgets('any admission requires the warning dialog: cancel stops, '
      'confirm starts with expect any', (tester) async {
    final fake = FakeTransferService();
    await pumpPage(tester, ReceivePage(service: fake));
    await _fillBasics(tester);

    await tester.tap(find.text('接受每一个对端（any）'));
    await tester.pump();
    await tester.tap(find.text('开始监听'));
    await tester.pumpAndSettle();

    // Dialog is up; cancelling must not start the listener.
    expect(find.text('确认放开准入限制？'), findsOneWidget);
    await tester.tap(find.text('取消'));
    await tester.pumpAndSettle();
    expect(fake.startReceiveCalls, 0);
    expect(find.text('确认放开准入限制？'), findsNothing);

    // Second attempt, confirmed.
    await tester.tap(find.text('开始监听'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('仍要开始'));
    await tester.pumpAndSettle();

    expect(fake.startReceiveCalls, 1);
    expect(fake.lastReceiveOptions?.admission, AdmissionMode.any);
    expect(fake.lastReceiveOptions?.expectText, 'any');
    expect(find.byKey(const ValueKey('receive_task_card')), findsOneWidget);
  });

  testWidgets('listening banner shows address, full fingerprint and large '
      'pairing code', (tester) async {
    final fake = FakeTransferService();
    await pumpPage(tester, ReceivePage(service: fake));
    await _fillBasics(tester);

    await tester.tap(find.text('开始监听'));
    await tester.pump();
    fake.emitReceive(const Listening(job: 'r1', addr: '192.168.1.5:52341'));
    await tester.pump();

    expect(find.text('192.168.1.5:52341'), findsOneWidget);
    expect(find.text('ab' * 32), findsOneWidget);
    expect(find.byKey(const ValueKey('receive_listen_code')), findsOneWidget);
    expect(find.textContaining('Windows 防火墙'), findsOneWidget);
  });

  testWidgets('event timeline drives the task card: connecting, progress '
      'bar, verifying, then done with the transfer report', (tester) async {
    final fake = FakeTransferService();
    await pumpPage(tester, ReceivePage(service: fake));
    await _fillBasics(tester);

    await tester.tap(find.text('开始监听'));
    await tester.pump();

    fake.emitReceive(const Listening(job: 'r1', addr: '192.168.1.5:52341'));
    fake.emitReceive(Connected(job: 'r1', peer: kPeerFingerprint));
    fake.emitReceive(const Accepted(job: 'r1', files: 2, bytes: 2048));
    await tester.pump();
    expect(find.text('建立连接'), findsOneWidget);

    fake.emitReceive(const FileStarted(
        job: 'r1', file: 0, path: 'docs/manual.txt', bytes: 1024));
    fake.emitReceive(const BytesAcked(job: 'r1', file: 0, bytes: 1024));
    fake.emitReceive(const FileCommitted(
        job: 'r1', file: 0, path: 'docs/manual.txt'));
    await tester.pump();
    expect(find.text('传输中'), findsOneWidget);
    // progressBarText: 1024/2048 -> 50%.
    expect(find.textContaining('50%'), findsOneWidget);
    expect(find.text('docs/manual.txt'), findsOneWidget);
    expect(find.text('已提交'), findsOneWidget);

    fake.emitReceive(const Verifying(job: 'r1'));
    await tester.pump();
    expect(find.text('校验中'), findsOneWidget);

    fake.emitReceive(const Done(job: 'r1'));
    fake.finishReceive(JobSucceeded(TransferReport(
      job: 'r1',
      files: 2,
      bytes: 2048,
      durationMs: 61000,
      peer: kPeerFingerprint,
      roots: const ['C:\\ak-in'],
    )));
    await tester.pump();

    expect(find.text('已完成'), findsOneWidget);
    expect(find.text('传输完成'), findsOneWidget);
    expect(find.textContaining('2 个文件'), findsOneWidget);
    expect(find.textContaining('用时 01:01'), findsOneWidget);
    expect(find.textContaining('目录：C:\\ak-in'), findsOneWidget);
  });

  testWidgets('stop button stops the listener and clears the banner',
      (tester) async {
    final fake = FakeTransferService();
    await pumpPage(tester, ReceivePage(service: fake));
    await _fillBasics(tester);

    await tester.tap(find.text('开始监听'));
    await tester.pump();
    fake.emitReceive(const Listening(job: 'r1', addr: '192.168.1.5:52341'));
    await tester.pump();
    expect(find.byKey(const ValueKey('receive_listen_addr')), findsOneWidget);

    await tester.tap(find.text('停止'));
    await tester.pumpAndSettle();
    expect(fake.stopReceiveCalls, 1);
    expect(fake.isListening, isFalse);
    expect(find.byKey(const ValueKey('receive_listen_addr')), findsNothing);
  });

  testWidgets('book admission passes the book form through and surfaces the '
      'engine empty-book error copy', (tester) async {
    final fake = FakeTransferService();
    await pumpPage(tester, ReceivePage(service: fake));
    await _fillBasics(tester);

    // Default book tile selected: start forwards admission=book with no
    // pinned expect text (the engine sees expect:"book").
    await tester.tap(find.text('开始监听'));
    await tester.pump();
    expect(fake.startReceiveCalls, 1);
    expect(fake.lastReceiveOptions?.admission, AdmissionMode.book);
    expect(fake.lastReceiveOptions?.expectText, isNull);
    expect(find.byKey(const ValueKey('receive_task_card')), findsOneWidget);

    // Stop, then make the "engine" reject like an empty pairing book would
    // (ak_engine_last_error text): the page surfaces the message verbatim.
    await tester.tap(find.text('停止'));
    await tester.pumpAndSettle();
    fake.receiveStartError =
        StateError('配对簿为空（peers.book 无条目）：请先完成一次配对');
    await tester.tap(find.text('开始监听'));
    await tester.pump();

    expect(fake.startReceiveCalls, 2);
    expect(find.textContaining('启动失败'), findsOneWidget);
    expect(find.textContaining('配对簿为空'), findsOneWidget);
  });
}
