/// W3 (V-N4) widget tests: the four failure surfaces render the MAPPED
/// copy (title + reason + suggestion) and keep the error's ORIGINAL text
/// (error code included) inside the expandable 「详情」 only — collapsed
/// screens never show `AkFfiException`/`AK_FAILED` vocabulary, expanding
/// reveals it, and the copy button writes it to the clipboard.
///
/// Sites covered: the connection detail page's send failure, the receive
/// banner's failure, the home pairing-flow failure, and the settings
/// page's diagnostics-export failure.
library;

import 'package:ak_ffi_dart/ak_ffi_dart.dart' show AkFfiException;
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:ak_sending_app/services/connection_diagnostics.dart';
import 'package:ak_sending_app/services/simple_transfer_fake.dart';
import 'package:ak_sending_app/ui/app.dart';
import 'package:ak_sending_app/ui/pages/settings_page.dart';
import 'package:ak_sending_app/ui/service/save_dir_store.dart';
import 'package:ak_sending_app/ui/service/simple_transfer_service.dart';
import 'package:ak_sending_app/ui/service/transfer_errors.dart';

import 'w28_test_helpers.dart';

/// The V2-d raw shape: an FFI exception whose message says the
/// connection was lost mid-transfer (must map to the interruption copy).
const AkFfiException _connectionLost = AkFfiException(
  2,
  'transfer failed after connecting via 10.0.0.9:52330: '
      'stream failed: connection lost',
);

/// A fake that fails the NEXT send with a caller-chosen RAW error object
/// (the base fake only takes reason strings).
class _RawSendErrorService extends LocalSimpleTransferService {
  Object? nextSendError;

  @override
  Future<String> sendFiles(String connId, List<String> paths) async {
    final error = nextSendError;
    if (error != null) {
      throw error;
    }
    return super.sendFiles(connId, paths);
  }
}

/// A diagnostics gateway that always fails with the given error.
class _FailingExportGateway implements DiagnosticsExportGateway {
  const _FailingExportGateway(this.error);

  final Object error;

  @override
  Future<String> exportDiagnostics(String content) async => throw error;
}

/// Opens a connected SEND-direction detail page (probe passed).
Future<String> _openSendDetail(
    WidgetTester tester, LocalSimpleTransferService fake) async {
  await fake.createCode(TaskRole.send);
  final id = fake.simulatePeerJoin(peerName: '办公本-ThinkPad');
  await pumpEvents(tester);
  fake.completeProbe(id);
  await pumpEvents(tester);
  await tester.pump();
  await tester.tap(find.byKey(ValueKey('conn_card_name_$id')));
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 400));
  return id;
}

Future<void> _pickAndSend(WidgetTester tester) async {
  await tester.tap(find.byKey(const ValueKey('conn_pick_files_button')));
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 400));
  await tester.tap(find.byKey(const ValueKey('pick_files_option')));
  await tester.pump();
  await tester.tap(find.byKey(const ValueKey('conn_send_button')));
  await tester.pump();
}

void main() {
  testWidgets('send failure: mapped title + suggestion; the raw FFI text '
      'only inside the expandable 详情 (and copyable)', (tester) async {
    final clipboard = MockClipboard()..install();
    final picker = FakeFilePickGateway(filesResult: kTwoFiles);
    final fake = _RawSendErrorService();
    addTearDown(fake.dispose);
    await tester.binding.setSurfaceSize(kWideSize);
    addTearDown(() => tester.binding.setSurfaceSize(null));
    await tester.pumpWidget(AkSendingApp(
      service: fake,
      picker: picker,
      store: InMemorySaveDirStore(),
    ));
    await tester.pump();
    await _openSendDetail(tester, fake);

    fake.nextSendError = _connectionLost;
    await _pickAndSend(tester);
    await pumpEvents(tester);
    await tester.pump();

    // The mapped card: interruption title + resume suggestion (审核 §4.3).
    expect(find.byKey(const ValueKey('conn_send_error')), findsOneWidget);
    expect(find.text('传输中断了'), findsOneWidget);
    expect(find.byKey(const ValueKey('conn_send_error_suggestion')),
        findsOneWidget);
    expect(
        find.textContaining('断点续传'), findsOneWidget,
        reason: '中断类建议必须给出断点续传动作');

    // Collapsed: NO raw vocabulary anywhere on the screen.
    expect(find.textContaining('AkFfiException'), findsNothing);
    expect(find.textContaining('AK_FAILED'), findsNothing);
    expect(find.textContaining('connection lost'), findsNothing);

    // Expand: the original text (error code + message) becomes visible.
    await tester.tap(find.byKey(const ValueKey('conn_send_error_toggle')));
    await tester.pump();
    expect(find.byKey(const ValueKey('conn_send_error_detail')),
        findsOneWidget);
    expect(find.textContaining('AkFfiException(AK_FAILED 2)'), findsOneWidget);
    expect(find.textContaining('connection lost'), findsOneWidget);

    // Copy writes the ORIGINAL detail to the clipboard.
    await tester.tap(find.byKey(const ValueKey('conn_send_error_copy')));
    await tester.pump();
    expect(clipboard.text, '$_connectionLost');
    expect(clipboard.text, contains('AK_FAILED'));
  });

  testWidgets('receive banner failure: mapped copy; original only in 详情',
      (tester) async {
    final fake = await pumpApp(tester);
    addTearDown(fake.dispose);
    fake.emitIncomingRequest(fileCount: 2, totalBytes: 2048);
    await pumpEvents(tester);
    await tester.tap(find.byKey(const ValueKey('incoming_accept_job-1')));
    await tester.pump();

    fake.failIncoming('job-1', '连接已断开，接收已中断。');
    await pumpEvents(tester);
    await tester.pump();

    // Mapped interruption copy for the receiving side.
    expect(find.byKey(const ValueKey('incoming_error_job-1')),
        findsOneWidget);
    expect(find.text('传输中断了'), findsOneWidget);
    expect(find.textContaining('对方重新发送'), findsOneWidget);
    // Collapsed: the service wording never leaks (it lives in 详情).
    expect(find.textContaining('连接已断开，接收已中断'), findsNothing);

    await tester.tap(find.byKey(const ValueKey('incoming_error_job-1_toggle')));
    await tester.pump();
    expect(find.textContaining('连接已断开，接收已中断'), findsOneWidget);
  });

  testWidgets('home pairing failure: unknown-code join failure maps to '
      '这个码用不了; the verbatim reason sits in 详情', (tester) async {
    final fake = await pumpApp(tester);
    addTearDown(fake.dispose);
    fake.nextJoinError = '配对码不存在或已过期';
    final minted = await fake.createCode(TaskRole.send);

    // X4: the receive MAIN entry declares the direction (no selector).
    await tester.tap(find.byKey(const ValueKey('home_receive_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    await tester.enterText(
        find.byKey(const ValueKey('join_code_field')), minted.code);
    await tester.pump();
    await tester.tap(find.byKey(const ValueKey('join_submit_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    // The frozen flow_error key now sits on the mapped title; X4 wired
    // the JOIN scene (W-N9's three-layer copy).
    expect(find.byKey(const ValueKey('flow_error')), findsOneWidget);
    expect(find.text('这个码用不了'), findsOneWidget);
    expect(find.textContaining('重新生成'), findsOneWidget);
    // Collapsed: the verbatim service reason is NOT on screen.
    expect(find.textContaining('配对码不存在或已过期'), findsNothing);

    await tester.tap(find.byKey(const ValueKey('flow_error_toggle')));
    await tester.pump();
    expect(find.text('配对码不存在或已过期'), findsOneWidget);
  });

  testWidgets('settings export failure: inline mapped card with 复制; the '
      'raw exception only in 详情', (tester) async {
    final fake = LocalSimpleTransferService();
    addTearDown(fake.dispose);
    await tester.pumpWidget(MaterialApp(
      home: SettingsPage(
        service: fake,
        store: InMemorySaveDirStore(),
        exporter: const _FailingExportGateway(
          AkFfiException(-5, 'temp dir not writable'),
        ),
      ),
    ));
    await tester.pump();

    await tester.tap(find.byKey(const ValueKey('settings_export_diag_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.byKey(const ValueKey('settings_export_diag_error')),
        findsOneWidget);
    expect(find.text('出了点问题'), findsOneWidget);
    // codex 裁决：重启会清空进程内诊断缓冲 — 改为展开「详情」复制错误信息。
    expect(find.textContaining('请再试一次'), findsOneWidget);
    expect(find.textContaining('请展开「详情」并复制'), findsOneWidget,
        reason: '导出诊断失败时不建议重启应用，也不再建议导出诊断');
    expect(find.textContaining('重启应用'), findsNothing);
    // Collapsed: nothing raw.
    expect(find.textContaining('AkFfiException'), findsNothing);
    expect(find.textContaining('AK_ERR_INTERNAL'), findsNothing);

    await tester
        .tap(find.byKey(const ValueKey('settings_export_diag_error_toggle')));
    await tester.pump();
    expect(find.textContaining('AkFfiException(AK_ERR_INTERNAL -5)'),
        findsOneWidget);
    expect(find.textContaining('temp dir not writable'), findsOneWidget);
  });

  testWidgets('idle expiry keeps 配对已闲置 copy; a plain drop shows the '
      'W1 interruption copy instead (V-N4)', (tester) async {
    final fake = await pumpApp(tester);
    addTearDown(fake.dispose);
    final id = await _openSendDetail(tester, fake);

    // Idle expiry (the engine's own reason): the dedicated copy.
    fake.expireIdle(id);
    await pumpEvents(tester);
    await tester.pump();
    expect(find.text('配对已闲置过久，请重新配对。'), findsOneWidget);
    expect(find.textContaining('连接已中断'), findsNothing,
        reason: '闲置过期不得显示中断文案');

    // Leave the idle-expired page before opening the second connection.
    await tester.pageBack();
    await tester.pump();
    await tester.pump(const Duration(seconds: 1));

    // A plain drop (no idle reason): the interruption copy with the
    // resume suggestion — never the idle wording.
    final second = await _openSendDetail(tester, fake);
    fake.disconnect(second);
    await pumpEvents(tester);
    await tester.pump();
    expect(find.text('连接已中断，重新发送会从断点续传。'), findsOneWidget);
    expect(find.text('配对已闲置过久，请重新配对。'), findsNothing,
        reason: 'V-N4：中断不得显示闲置文案');
  });

  test('codex P2: FFI -2 on the CREATE pairing scene is a parameter '
      'error (fallback copy) — invalid-code copy is join-only', () {
    // X4 wires the home page to pass `joining` for the join flow, so a
    // failure mapped in the `pairing` scene IS the create flow: no code
    // is being typed, so -2 (AK_ERR_INVALID) must NOT claim invalidCode.
    final create = transferErrorView(
      const AkFfiException(-2, 'join spec validation failed'),
      TransferErrorScene.pairing,
    );
    expect(create.kind, TransferErrorKind.unknown);
    expect(create.title, '出了点问题');
    expect(create.title, isNot('这个码用不了'));
    expect(create.suggestion, isNot(contains('配对码')));
    // The join scene keeps the invalid-code mapping (the user typed a
    // code there).
    final join = transferErrorView(
      const AkFfiException(-2, 'join spec validation failed'),
      TransferErrorScene.joining,
    );
    expect(join.kind, TransferErrorKind.invalidCode);
    expect(join.title, '这个码用不了');
  });

  test('codex P2: the engine\'s real storage-full texts land on the copy '
      'the SCENE dictates — sending = the peer\'s disk, receiving = THIS '
      'device\'s disk', () {
    // All three real texts (provenance in the classifier): the receiver's
    // precheck frame (receiver.rs:955), the mid-transfer frame
    // (recv_single.rs:18), and the receiver's local Display (lib.rs:1046).
    const texts = [
      'peer sent error STORAGE_FULL: insufficient free space at destination',
      'peer sent error STORAGE_FULL: '
          'receiver ran out of disk space during the transfer',
      'destination storage full: 1024 bytes available, 8192 needed',
    ];
    for (final text in texts) {
      final send = transferErrorView(
          AkFfiException(2, text), TransferErrorScene.sending);
      expect(send.kind, TransferErrorKind.saveDirNotWritable, reason: text);
      expect(send.title, '对方的设备空间不足', reason: text);
      expect(send.suggestion, contains('断点续传'), reason: text);
      expect(send.suggestion, isNot(contains('更换保存文件夹')), reason: text);

      final receive = transferErrorView(
          AkFfiException(2, text), TransferErrorScene.receiving);
      expect(receive.kind, TransferErrorKind.saveDirNotWritable,
          reason: text);
      expect(receive.title, '本机存储空间不足', reason: text);
      expect(receive.reason, contains('没法保存'), reason: text);
      expect(receive.suggestion, contains('清理本机'), reason: text);
      expect(receive.suggestion, contains('更换保存文件夹'), reason: text);
      expect(receive.suggestion, contains('断点继续'), reason: text);
      expect(receive.title, isNot('对方的设备空间不足'), reason: text);
    }
  });

  testWidgets('send failure: the mid-transfer STORAGE_FULL frame keeps '
      'the peer-disk copy (对方的设备空间不足)', (tester) async {
    final picker = FakeFilePickGateway(filesResult: kTwoFiles);
    final fake = _RawSendErrorService();
    addTearDown(fake.dispose);
    await tester.binding.setSurfaceSize(kWideSize);
    addTearDown(() => tester.binding.setSurfaceSize(null));
    await tester.pumpWidget(AkSendingApp(
      service: fake,
      picker: picker,
      store: InMemorySaveDirStore(),
    ));
    await tester.pump();
    await _openSendDetail(tester, fake);

    fake.nextSendError = const AkFfiException(
      2,
      'peer sent error STORAGE_FULL: '
          'receiver ran out of disk space during the transfer',
    );
    await _pickAndSend(tester);
    await pumpEvents(tester);
    await tester.pump();

    // The PEER's disk is full: re-pair then resume — never the receiver's
    // my-disk copy or the generic folder-change boilerplate.
    expect(find.byKey(const ValueKey('conn_send_error')), findsOneWidget);
    expect(find.text('对方的设备空间不足'), findsOneWidget);
    expect(find.textContaining('断点续传'), findsOneWidget);
    expect(find.text('本机存储空间不足'), findsNothing);
    expect(find.text('无法保存到所选文件夹'), findsNothing);
    expect(find.textContaining('更换保存文件夹'), findsNothing);
  });

  testWidgets('receive banner failure: my-disk copy (本机存储空间不足) '
      'for the engine\'s real storage-full texts', (tester) async {
    final fake = await pumpApp(tester);
    addTearDown(fake.dispose);
    // Both real frame texts the receiver's side can surface
    // (receiver.rs:955 precheck + recv_single.rs:18 mid-transfer).
    const texts = [
      'insufficient free space at destination',
      'receiver ran out of disk space during the transfer',
    ];
    for (final (i, text) in texts.indexed) {
      final jobId = 'disk-$i';
      fake.emitIncomingRequest(jobId: jobId, fileCount: 2, totalBytes: 2048);
      await pumpEvents(tester);
      await tester.pump();
      await tester.tap(find.byKey(ValueKey('incoming_accept_$jobId')));
      await tester.pump();

      fake.failIncoming(jobId, text);
      await pumpEvents(tester);
      await tester.pump();

      // Receiving side: MY disk is full — clean this device or change the
      // save folder, the peer re-sends from the checkpoint.
      expect(find.byKey(ValueKey('incoming_error_$jobId')), findsOneWidget,
          reason: text);
      expect(find.text('本机存储空间不足'), findsOneWidget, reason: text);
      expect(find.textContaining('清理本机'), findsOneWidget, reason: text);
      expect(find.textContaining('断点继续'), findsOneWidget, reason: text);
      expect(find.text('对方的设备空间不足'), findsNothing, reason: text);
      expect(find.text('无法保存到所选文件夹'), findsNothing, reason: text);
      // Collapsed: the raw engine text only lives in 详情.
      expect(find.textContaining(text), findsNothing, reason: text);

      await tester
          .tap(find.byKey(ValueKey('incoming_error_dismiss_$jobId')));
      await tester.pump();
    }
  });
}
