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

    await tester.tap(find.byKey(const ValueKey('home_join_code_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    await tester.enterText(
        find.byKey(const ValueKey('join_code_field')), minted.code);
    await tester.pump();
    // W-N1: the joiner declares its direction (the submit needs it).
    await tester.tap(find.descendant(
        of: find.byKey(const ValueKey('join_role_selector')),
        matching: find.text('接收文件')));
    await tester.pump();
    await tester.tap(find.byKey(const ValueKey('join_submit_button')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    // The frozen flow_error key now sits on the mapped title.
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
}
