// Z E2E (batch Z, 2026-11-24): K1 with the folder option — the ANDROID
// SYSTEM tree picker through the app's REAL send page, in a profile build.
// The test joins a receiver-side code as the SENDER (the X4 send secondary
// entry), and on the pairing's auto-opened pick sheet (W7.2) taps 选择文件's
// sibling 选择文件夹 instead. The production gateway then runs the whole
// batch Z send path — ACTION_OPEN_DOCUMENT_TREE, the native staging behind
// `dev.ak/storage` — and hands the STAGED root to the unchanged engine.
//
// The Flutter side is driven by this test; the NATIVE picker is driven from
// the host (adb/uiautomator), for the reasons in the K1 header. K1's
// helpers are imported (its `main` is never called), so the code-entry step
// is literally K1's.
//
// Orchestration contract (all local-only endpoints):
//   * dart-defines: AK_REGISTRY_ADDR=10.0.2.2:17200, AK_STUN_ADDR=
//     10.0.2.2:17101, AK_REGISTRY_ID=<64hex>, AK_SAF_CODE=AKI1-...
//     (the RECEIVER-side task code printed by the host CLI
//     `ak-cli task code-recv`).
//   * the host drives DocumentsUI onto the folder when
//     `ZSAF|E2E|PICKER|tree|awaiting external DocumentsUI drive` shows up
//     in logcat (tag `flutter`), then compares what the receiver got.
//   * the host CLI's consent prompt is answered by its own feeder; this
//     test only observes the app side.
//
// Evidence is one `ZSAF|E2E|…` line per fact. Codes appear as their last
// four symbols only; no staged path ever prints (the walkthrough never
// holds one — it derives the staging root the way Android lays it out).
import 'dart:io' as io;

import 'package:ak_sending_app/main.dart' as app;
import 'package:ak_sending_app/ui/app.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:path_provider/path_provider.dart'
    show getApplicationSupportDirectory;

import 'k1_android_saf_test.dart' as k1;

void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  binding.framePolicy = LiveTestWidgetsFlutterBindingFramePolicy.fullyLive;

  testWidgets('Z E2E: system tree picker, staged folder, real send',
      (tester) async {
    const registryAddr = String.fromEnvironment('AK_REGISTRY_ADDR');
    const stunAddr = String.fromEnvironment('AK_STUN_ADDR');
    const registryId = String.fromEnvironment('AK_REGISTRY_ID');
    const safCode = String.fromEnvironment('AK_SAF_CODE');
    expect(registryAddr, '10.0.2.2:17200', reason: 'local registry only');
    expect(stunAddr, '10.0.2.2:17101', reason: 'local STUN only');
    expect(registryId.length, 64, reason: 'registry pin required');
    expect(safCode.startsWith('AKI1-'), isTrue, reason: 'AK_SAF_CODE required');

    final bootstrap = app.bootstrapAppService();
    zSay('E2E|BOOTSTRAP|service=${bootstrap.service != null}'
        '|error=${bootstrap.error}');
    expect(bootstrap.service, isNotNull, reason: bootstrap.error);
    await tester.pumpWidget(AkSendingApp(service: bootstrap.service!));
    await k1.safUntil(tester,
        () => tester.any(find.byKey(const ValueKey('home_send_button'))),
        'home');

    // The send SECONDARY entry (X4), as K1: the entry declares the
    // direction; the panel asks for the code only.
    await tester
        .tap(find.byKey(const ValueKey('home_send_join_code_button')));
    await tester.pump(const Duration(milliseconds: 600));
    final field = find.byKey(const ValueKey('join_code_field'));
    await k1.safUntil(tester, () => tester.any(field), 'join panel field');
    final submit = find.byKey(const ValueKey('join_submit_button'));
    await tester.enterText(field, safCode);
    // K1's code-entry step, imported rather than copied: wait for the
    // enabled 配对, else drive the panel's own callback.
    var entryPath = 'test-input-stub';
    if (!await k1.submitEnabledWithin(tester, submit, 3)) {
      final panelField = tester.widget<TextField>(field);
      panelField.controller!.value = TextEditingValue(
        text: safCode,
        selection: TextSelection.collapsed(offset: safCode.length),
      );
      panelField.onChanged!(safCode);
      entryPath = 'controller-callback';
      expect(await k1.submitEnabledWithin(tester, submit, 15), isTrue,
          reason: 'submit still disabled after the code entry');
    }
    zSay('E2E|CODE_ENTERED|path=$entryPath');
    await tester.tap(submit);
    // Codes never print in full — the last four symbols at most.
    zSay(
        'E2E|JOIN_SUBMITTED|send-entry|****${safCode.substring(safCode.length - 4)}');

    final connection = find.byWidgetPredicate(
        (w) => w.key != null && w.key.toString().contains('conn_card_name_'));
    await k1.safUntil(tester, () => connection.evaluate().isNotEmpty,
        'paired connection card',
        seconds: 170);
    // W7.2: pairing AUTO-OPENS the detail page and the pick sheet.
    await k1.safUntil(
        tester,
        () => find
            .byKey(const ValueKey('pick_files_option'))
            .evaluate()
            .isNotEmpty,
        'auto-opened pick sheet (W7.2)',
        seconds: 30);
    expect(find.byKey(const ValueKey('conn_detail_page')), findsOneWidget);
    // The bottom sheet animates in: settle before tapping.
    for (var i = 0; i < 6; i++) {
      await tester.pump(const Duration(milliseconds: 300));
    }
    // THE SYSTEM TREE PICKER: the real gateway — no injection seam. The
    // host drives DocumentsUI onto the folder; the staging runs in between.
    await tester.ensureVisible(find.byKey(const ValueKey('pick_dir_option')));
    await tester.tap(find.byKey(const ValueKey('pick_dir_option')));
    zSay('E2E|PICKER|tree|awaiting external DocumentsUI drive');

    final refusal = find.byKey(const ValueKey('staging_error_dialog'));
    final picked = await _pumpUntil(
        tester,
        () =>
            k1.safTextAt(tester, 'picked_count') == '已选 1 项' ||
            refusal.evaluate().isNotEmpty,
        150);
    final refused = refusal.evaluate().isNotEmpty;
    if (refused) {
      zSay('E2E|STAGE_ERROR|title=${k1.safTextAt(tester, 'staging_error_title')}'
          '|body=${k1.safTextAt(tester, 'staging_error_body')}');
    }
    final root = await _derivedRoot();
    final before = root.isEmpty
        ? const (children: -1, payloadOk: false, valid: false)
        : _stagingState(root, 'akstage');
    final count = before.children < 0 ? 'na' : '${before.children}';
    zSay('E2E|STAGED|picked=${picked && !refused ? 1 : 0}'
        '|name=${k1.safTextAt(tester, 'picked_item_0')}'
        '|rootChildren=$count|payloadOk=${before.payloadOk}');
    expect(picked && !refused, isTrue,
        reason: refused
            ? 'the gateway refused the folder'
            : 'picked_count never reached 已选 1 项');

    await tester.ensureVisible(find.byKey(const ValueKey('conn_send_button')));
    await tester.tap(find.byKey(const ValueKey('conn_send_button')));
    final ended = await _pumpUntil(
        tester,
        () =>
            find
                .byKey(const ValueKey('conn_send_result'))
                .evaluate()
                .isNotEmpty ||
            find.byKey(const ValueKey('conn_send_error')).evaluate().isNotEmpty,
        // The emulator's direct link moves ≈ 130 KB/s: the first complete
        // round (2026-11-25) had 31 of 32 MiB acknowledged when a 4-minute
        // budget ran out. K1's single file took 27 s; this leaves room for
        // a slower link without hiding a stalled one.
        420);
    final result = k1.safTextAt(tester, 'conn_send_result');
    zSay('E2E|RESULT|$result|stats=${k1.safTextAt(tester, 'conn_send_result_stats')}');
    if (find.byKey(const ValueKey('conn_send_error')).evaluate().isNotEmpty) {
      zSay('E2E|FAILED|title=${k1.safTextAt(tester, 'conn_send_error_title')}'
          '|reason=${k1.safTextAt(tester, 'conn_send_error_reason')}');
    }
    expect(result.contains('发送完成'), isTrue,
        reason: ended ? result : 'no result within 7 minutes');

    // The page releases the staged copy after a SUCCESSFUL send; give the
    // cleanup a moment, then count what the staging root still holds.
    for (var i = 0; i < 5; i++) {
      await tester.pump(const Duration(seconds: 1));
    }
    final rootChildren = root.isEmpty ? -1 : _childCount(io.Directory(root));
    zSay('E2E|STAGING_ROOT|children=${rootChildren < 0 ? 'na' : rootChildren}');
    if (result.contains('发送完成') && before.valid) {
      expect(rootChildren, 0,
          reason: 'the staged copy of a finished send is still there');
    }
    zSay('E2E|DONE');
  }, timeout: const Timeout(Duration(minutes: 12)));
}

void zSay(String value) {
  // ignore: avoid_print
  print('ZSAF|$value');
}

/// The one pump that may be outstanding, shared by every wait: a second
/// `pump()` on top of a stalled one would orphan the first.
Future<void>? _openPump;

/// Pumps until [ready] holds or [seconds] pass; answers whether it holds.
///
/// Unlike K1's `safUntil`, a pump that STALLS — no frame while the system
/// picker is in front of the app, or a frame that simply outlasts the poll
/// while the engine hashes — never fails the wait: at most ONE pump is
/// outstanding, the loop keeps polling [ready] around it, and the next
/// frame completes it.
///
/// [ready] runs BESIDE that open pump, and an open pump owns the guarded
/// scope of the test API: it must not call a guarded member of
/// [WidgetTester] (`any`, `widget`, …) — the first E2E round (2026-11-25)
/// died of exactly that "Guarded function conflict". Read the tree through
/// finders' `evaluate()` or a plain flag.
Future<bool> _pumpUntil(
    WidgetTester tester, bool Function() ready, int seconds) async {
  final end = DateTime.now().add(Duration(seconds: seconds));
  while (!ready() && DateTime.now().isBefore(end)) {
    _openPump ??= tester
        .pump(const Duration(milliseconds: 400))
        .whenComplete(() => _openPump = null);
    await Future.any<void>(<Future<void>>[
      _openPump!,
      Future<void>.delayed(const Duration(milliseconds: 400)),
    ]);
  }
  // A pump still open keeps the guarded scope: the caller's next line is
  // usually a guarded tap or ensureVisible. With the wait satisfied the app
  // is in the foreground and its frame is already due.
  final open = _openPump;
  if (open != null) {
    await Future.any<void>(<Future<void>>[
      open,
      Future<void>.delayed(const Duration(seconds: 20)),
    ]);
  }
  if (_openPump != null) {
    fail('a pump was still stalled 20 s after the wait ended: no frame');
  }
  return ready();
}

/// The staging root as Android lays it out: the application-support
/// directory is `<data dir>/files`, the staging root
/// `<data dir>/no_backup/ak-saf-staging`. Empty when the platform directory
/// cannot be resolved — the staging-root lines are then `na` instead of
/// failing the walkthrough.
Future<String> _derivedRoot() async {
  try {
    final support = (await getApplicationSupportDirectory()).path;
    final slash = support.lastIndexOf('/');
    final dataDir = slash > 0 ? support.substring(0, slash) : support;
    return '$dataDir/no_backup/ak-saf-staging';
  } on Object {
    return '';
  }
}

int _childCount(io.Directory dir) =>
    dir.existsSync() ? dir.listSync().length : 0;

/// What the staging root holds for this send: its stage directories, and
/// whether exactly one of them carries the `payload/<name>` copy — then
/// the count after the send is a statement about THIS send.
({int children, bool payloadOk, bool valid}) _stagingState(
    String root, String name) {
  final children = <io.FileSystemEntity>[
    if (io.Directory(root).existsSync()) ...io.Directory(root).listSync(),
  ];
  var payloadOk = false;
  for (final child in children) {
    if (io.Directory('${child.path}/payload/$name').existsSync()) {
      payloadOk = true;
    }
  }
  return (
    children: children.length,
    payloadOk: payloadOk,
    valid: children.length == 1 && payloadOk,
  );
}
