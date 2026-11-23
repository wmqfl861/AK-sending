// K1 item b (batch K repair round, 2026-11-15; second half adapted to
// the W7.2 flow in batch X, then to the X4 home): the ANDROID SYSTEM
// file picker (SAF) normal flow. The test joins a receiver-side code as
// the SENDER through the send SECONDARY entry — X4: 对方已经给了你一个
// 码？输入对方的码 — the direction comes from the ENTRY (the join panel
// has no direction selector; a role-less cross-device join would default
// to receiver and be refused as a same-direction pair, W-N1). On pairing
// success the app AUTO-OPENS the connection detail page with the
// source-selection bottom sheet (W7.2); tapping the sheet's 选择文件
// option opens the platform's system picker through the app's REAL
// FilePickGateway (file_picker plugin → DocumentsUI).
// The Flutter side of the flow is driven by this test; the NATIVE
// picker activity is driven externally (adb/uiautomator works on native
// views), because Flutter renders no accessibility tree for uiautomator
// and blind adb taps into Flutter widgets proved unreliable (recorded
// in the K repair-round handoff: field focus loss, input truncation,
// layout shift).
//
// Orchestration contract (all local-only endpoints):
//   * dart-defines: AK_REGISTRY_ADDR=10.0.2.2:17200, AK_STUN_ADDR=
//     10.0.2.2:17101, AK_REGISTRY_ID=<64hex>, AK_SAF_CODE=AKI1-...
//     (the RECEIVER-side task code printed by the host CLI
//     `ak-cli task code-recv`); AK_SAF_FILE_BYTES (default 4194304).
//   * the external driver completes the system picker while this test
//     polls `picked_count` for 已选 1 项 (budget: 150 s).
//   * the host CLI's 120 s consent prompt must be answered by its own
//     feeder; this test only observes the app side.
import 'package:ak_sending_app/main.dart' as app;
import 'package:ak_sending_app/ui/app.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  binding.framePolicy = LiveTestWidgetsFlutterBindingFramePolicy.fullyLive;

  testWidgets('K1 SAF: system picker file selection and real send', (tester) async {
    const registryAddr = String.fromEnvironment('AK_REGISTRY_ADDR');
    const stunAddr = String.fromEnvironment('AK_STUN_ADDR');
    const registryId = String.fromEnvironment('AK_REGISTRY_ID');
    const safCode = String.fromEnvironment('AK_SAF_CODE');
    const safBytes =
        int.fromEnvironment('AK_SAF_FILE_BYTES', defaultValue: 4194304);
    expect(registryAddr, '10.0.2.2:17200', reason: 'local registry only');
    expect(stunAddr, '10.0.2.2:17101', reason: 'local STUN only');
    expect(registryId.length, 64, reason: 'registry pin required');
    expect(safCode.startsWith('AKI1-'), isTrue, reason: 'AK_SAF_CODE required');

    final bootstrap = app.bootstrapAppService();
    safEvidence('BOOTSTRAP|service=${bootstrap.service != null}'
        '|error=${bootstrap.error}');
    expect(bootstrap.service, isNotNull, reason: bootstrap.error);
    await tester.pumpWidget(AkSendingApp(service: bootstrap.service!));
    await safUntil(tester,
        () => tester.any(find.byKey(const ValueKey('home_send_button'))),
        'home');

    // Join the RECEIVER-side code as the SENDER via the send SECONDARY
    // entry (X4) — widget-level input, the reliable equivalent of what
    // blind adb taps could not do. The entry declares the direction;
    // the panel asks for the code only.
    await tester
        .tap(find.byKey(const ValueKey('home_send_join_code_button')));
    await tester.pump(const Duration(milliseconds: 600));
    final field = find.byKey(const ValueKey('join_code_field'));
    await safUntil(tester, () => tester.any(field), 'join panel field');
    await tester.enterText(field, safCode);
    await tester.pump(const Duration(milliseconds: 400));
    final submit = find.byKey(const ValueKey('join_submit_button'));
    expect(tester.widget<FilledButton>(submit).onPressed, isNotNull,
        reason: 'submit still disabled — code incomplete');
    await tester.tap(submit);
    safEvidence('JOIN_SUBMITTED|send-entry|$safCode');

    final connection = find.byWidgetPredicate(
        (w) => w.key != null && w.key.toString().contains('conn_card_name_'));
    await safUntil(tester, () => connection.evaluate().isNotEmpty,
        'paired connection card',
        seconds: 170);
    // W7.2: the sender flow now AUTO-OPENS the detail page and the
    // file-picker sheet once pairing succeeds — no manual card tap, no
    // manual pick button. Wait for the auto-opened sheet directly.
    await safUntil(
        tester,
        () => find
            .byKey(const ValueKey('pick_files_option'))
            .evaluate()
            .isNotEmpty,
        'auto-opened pick sheet (W7.2)',
        seconds: 30);
    expect(find.byKey(const ValueKey('conn_detail_page')), findsOneWidget);
    // The auto-opened bottom sheet animates in; settle before tapping
    // (a mid-animation tap lands outside the render tree bounds).
    for (var i = 0; i < 6; i++) {
      await tester.pump(const Duration(milliseconds: 300));
    }
    // THE SYSTEM PICKER: real FilePickGateway — no injection seam here.
    await tester
        .ensureVisible(find.byKey(const ValueKey('pick_files_option')));
    await tester.tap(find.byKey(const ValueKey('pick_files_option')));
    safEvidence('PICKER_OPENED|awaiting external DocumentsUI drive');
    // P1-2 (codex X fix): the production gateway now offers ONE optional
    // folder append INSIDE pickFiles() after a non-empty file pick — the
    // selection (picked_count) only lands after the offer resolves, so
    // decline it first to keep this walkthrough's single-file shape.
    await safUntil(tester,
        () => find.byKey(const ValueKey('folder_offer_decline_button'))
            .evaluate()
            .isNotEmpty,
        'folder offer after the system pick',
        seconds: 150);
    await tester
        .tap(find.byKey(const ValueKey('folder_offer_decline_button')));
    safEvidence('FOLDER_OFFER_DECLINED');
    await safUntil(tester,
        () => safTextAt(tester, 'picked_count') == '已选 1 项',
        'external picker selection',
        seconds: 30);
    safEvidence('PICKED|1 item|bytes=$safBytes');

    await tester.ensureVisible(find.byKey(const ValueKey('conn_send_button')));
    await tester.tap(find.byKey(const ValueKey('conn_send_button')));
    final end = DateTime.now().add(const Duration(minutes: 4));
    while (DateTime.now().isBefore(end)) {
      await tester
          .pump(const Duration(milliseconds: 300))
          .timeout(const Duration(seconds: 45));
      if (find.byKey(const ValueKey('conn_send_result')).evaluate().isNotEmpty) {
        break;
      }
    }
    final result = safTextAt(tester, 'conn_send_result');
    safEvidence(
        'RESULT|$result|stats=${safTextAt(tester, 'conn_send_result_stats')}');
    expect(result.contains('发送完成'), isTrue, reason: result);
    safEvidence('DONE|system SAF picker flow completed');
  }, timeout: const Timeout(Duration(minutes: 12)));
}

void safEvidence(String value) {
  // ignore: avoid_print
  print('K1SAF|$value');
}

Future<void> safUntil(WidgetTester tester, bool Function() ready, String what,
    {int seconds = 60}) async {
  final end = DateTime.now().add(Duration(seconds: seconds));
  while (!ready() && DateTime.now().isBefore(end)) {
    await tester
        .pump(const Duration(milliseconds: 400))
        .timeout(const Duration(seconds: 45));
  }
  expect(ready(), isTrue, reason: what);
}

String safTextAt(WidgetTester tester, String key) {
  final finder = find.byKey(ValueKey(key));
  if (finder.evaluate().isEmpty) return '';
  final widget = finder.evaluate().first.widget;
  if (widget is Text) return widget.data ?? widget.textSpan?.toPlainText() ?? '';
  return find
      .descendant(of: finder, matching: find.byType(Text))
      .evaluate()
      .map((e) => e.widget as Text)
      .map((t) => t.data ?? t.textSpan?.toPlainText() ?? '')
      .join(' / ');
}
