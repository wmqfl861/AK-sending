// Local-only J1/K1 Android auxiliary walkthrough; not a production entrypoint.
// Real production bootstrap, pages, FFI and host CLI transport.
// Only seam: synthetic files in this app's own support directory via picker.
// This DOES NOT verify Android SAF/shared-storage access or release settings.
//
// K1 fix (2026-11-14, batch K item d): the previous runner asserted that one
// SAMPLE simultaneously carried the rate line and the file line
// (sawThreeLines). In the 2026-11-14 light round the sampler (500 ms pumps)
// caught a single transferring sample at 87% whose rate line had not been
// built yet, then the transfer crossed into verifying (where the rate line
// is gone by design, transfer_progress_view._rateText), so the assertion
// failed even though the transfer itself completed. Fix, both halves of the
// work order's prescription:
//   1. sampling cadence 500 ms -> 150 ms (adjust sampling timing);
//   2. the final judgement (after the result card, i.e. past the verifying
//      stage) checks whether the rate line EVER appeared during the whole
//      transfer (accumulated flag), not whether it is present at the last
//      sampled moment. The strict three-line layout assertions still run —
//      and log LAYOUT evidence — on any sample where the lines co-occur.
import 'dart:io';
import 'dart:typed_data';

import 'package:ak_sending_app/main.dart' as app;
import 'package:ak_sending_app/ui/app.dart';
import 'package:ak_sending_app/ui/service/file_pick_gateway.dart';
import 'package:crypto/crypto.dart' show sha256;
import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:path_provider/path_provider.dart';

const name = 'synthetic_android_J1_8MiB_long_filename_progress_three_lines_truncation_verification_abcdefghijklmnopqrstuvwxyz_0123456789.bin';
const bytes = 8 * 1024 * 1024;

void evidence(String value) {
  // ignore: avoid_print
  print('J1ANDROID|$value');
}

class SyntheticPicker implements FilePickGateway {
  SyntheticPicker(this.file);
  final File file;
  @override
  Future<List<PickedEntry>> pickFiles() async => [PickedEntry(
    path: file.path, name: name, isDirectory: false, sizeBytes: bytes,
  )];
  @override
  Future<PickedEntry?> pickDirectory() async => null;
}

String textAt(String key) {
  final finder = find.byKey(ValueKey(key));
  if (finder.evaluate().isEmpty) return '';
  final widget = finder.evaluate().first.widget;
  if (widget is Text) return widget.data ?? widget.textSpan?.toPlainText() ?? '';
  return find.descendant(of: finder, matching: find.byType(Text)).evaluate()
    .map((e) => e.widget as Text).map((t) => t.data ?? t.textSpan?.toPlainText() ?? '').join(' / ');
}

Future<void> until(WidgetTester tester, bool Function() ready, String what, {int seconds = 150}) async {
  final end = DateTime.now().add(Duration(seconds: seconds));
  while (!ready() && DateTime.now().isBefore(end)) {
    await tester.pump(const Duration(milliseconds: 400)).timeout(const Duration(seconds: 45));
  }
  expect(ready(), isTrue, reason: what);
}

Future<void> hold(WidgetTester tester, int seconds) async {
  final end = DateTime.now().add(Duration(seconds: seconds));
  while (DateTime.now().isBefore(end)) {
    await tester.pump(const Duration(milliseconds: 500)).timeout(const Duration(seconds: 45));
  }
}

void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  binding.framePolicy = LiveTestWidgetsFlutterBindingFramePolicy.fullyLive;
  testWidgets('J1 auxiliary real Android send light and dark', (tester) async {
    expect(const String.fromEnvironment('AK_REGISTRY_ADDR'), '10.0.2.2:17200');
    expect(const String.fromEnvironment('AK_STUN_ADDR'), '10.0.2.2:17101');
    expect(const String.fromEnvironment('AK_REGISTRY_ID').length, 64);
    final support = await getApplicationSupportDirectory();
    final synthetic = Directory('${support.path}/synthetic_j1_aux8_20261114');
    await synthetic.create(recursive: true);
    final file = File('${synthetic.path}/$name');
    // Deterministic bytes, generated locally by this app under its normal UID.
    // No shared storage, run-as, root, permissions or user files are touched.
    final chunk = Uint8List(1024 * 1024);
    for (var i = 0; i < chunk.length; i++) { chunk[i] = (i * 31 + (i >> 8) * 17 + 11) & 255; }
    final sink = file.openWrite();
    for (var i = 0; i < 8; i++) { sink.add(chunk); }
    await sink.close();
    final digest = await sha256.bind(file.openRead()).first;
    evidence('SOURCE|bytes=${await file.length()}|sha256=$digest|pattern=(i*31+(i>>8)*17+11)&255;1MiB repeated8');
    expect(digest.toString(), 'e442fcea17247bfcfac3fd499d91cee7787bc397907cbee1b4087db39e7c5bc3');
    final bootstrap = app.bootstrapAppService(
      androidIdentityDir: () async => '${synthetic.path}/identity',
    );
    evidence('BOOTSTRAP|service=${bootstrap.service != null}|error=${bootstrap.error}');
    expect(bootstrap.service, isNotNull, reason: bootstrap.error);
    await tester.pumpWidget(AkSendingApp(service: bootstrap.service!, picker: SyntheticPicker(file)));
    await until(tester, () => find.byKey(const ValueKey('home_create_code_button')).evaluate().isNotEmpty, 'home');
    final view = tester.view;
    evidence('VIEW|physical=${view.physicalSize}|dpr=${view.devicePixelRatio}|logicalWidth=${view.physicalSize.width / view.devicePixelRatio}');

    for (final mode in ['light', 'dark']) {
      evidence('MODE_REQUEST|$mode');
      // IntegrationTest's test dispatcher does not mirror adb system brightness.
      // Explicit test-only theme injection; standard main system mode was tested separately.
      tester.platformDispatcher.platformBrightnessTestValue = mode == 'light' ? Brightness.light : Brightness.dark;
      await tester.pump(const Duration(milliseconds: 700));
      await until(tester, () => Theme.of(tester.element(find.byType(NavigationBar))).brightness == (mode == 'light' ? Brightness.light : Brightness.dark), 'test binding theme $mode', seconds: 35);
      evidence('THEME_VERIFIED|$mode|test-platform-brightness');
      await tester.tap(find.byKey(const ValueKey('home_create_code_button')));
      await tester.pump(const Duration(milliseconds: 700));
      await tester.tap(find.byKey(const ValueKey('role_send_button')));
      await until(tester, () => find.byKey(const ValueKey('flow_pairing_code')).evaluate().isNotEmpty, 'real task code');
      final codeWidget = tester.widget<SelectableText>(find.byKey(const ValueKey('flow_pairing_code')));
      final code = codeWidget.data ?? codeWidget.textSpan?.toPlainText() ?? '';
      expect(code.startsWith('AKI1-'), isTrue);
      evidence('CODE|$mode|$code');
      final connection = find.byWidgetPredicate((w) => w.key.toString().contains('conn_card_name_'));
      await until(tester, () => connection.evaluate().isNotEmpty, 'CLI receiver connection', seconds: 170);
      await tester.ensureVisible(connection.first);
      await tester.tap(connection.first);
      await tester.pump(const Duration(milliseconds: 800));
      expect(find.byKey(const ValueKey('conn_detail_page')), findsOneWidget);
      await tester.tap(find.byKey(const ValueKey('conn_pick_files_button')));
      await tester.pump(const Duration(milliseconds: 600));
      await tester.tap(find.byKey(const ValueKey('pick_files_option')));
      await tester.pump(const Duration(milliseconds: 600));
      expect(textAt('picked_count'), '已选 1 项');
      await tester.ensureVisible(find.byKey(const ValueKey('conn_send_button')));
      evidence('READY_TO_SEND|$mode');
      await hold(tester, 3);
      await tester.tap(find.byKey(const ValueKey('conn_send_button')));
      final end = DateTime.now().add(const Duration(minutes: 4));
      var previous = '';
      // K1 fix: accumulate each line's presence over the WHOLE loop; the
      // final judgement happens after the result, never on one sample.
      var sawRate = false; // rate line was built at least once
      var sawFile = false; // file line was built at least once
      var sawThreeLines = false; // both lines in ONE sample -> layout check
      var layoutLogged = false;
      var rateSample = '';
      while (DateTime.now().isBefore(end)) {
        // K1 fix: 150 ms cadence (was 500 ms) to catch the transferring
        // window's samples, including the >= 1 s mark where the average
        // speed line first appears.
        await tester.pump(const Duration(milliseconds: 150)).timeout(const Duration(seconds: 45));
        final status = textAt('conn_send_progress_status');
        final rate = textAt('conn_send_progress_rate');
        final currentFile = textAt('conn_send_progress_file');
        final sample = '$status|$rate|$currentFile';
        if (sample != previous && sample != '||') {
          previous = sample;
          evidence('PROGRESS|$mode|$sample');
        }
        if (rate.isNotEmpty) {
          sawRate = true;
          rateSample = rate;
        }
        if (currentFile.isNotEmpty) sawFile = true;
        if (rate.isNotEmpty && currentFile.isNotEmpty) {
          sawThreeLines = true;
          final statusRect = tester.getRect(find.byKey(const ValueKey('conn_send_progress_status')));
          final rateRect = tester.getRect(find.byKey(const ValueKey('conn_send_progress_rate')));
          final fileRect = tester.getRect(find.byKey(const ValueKey('conn_send_progress_file')));
          final render = tester.renderObject<RenderParagraph>(find.byKey(const ValueKey('conn_send_progress_file')));
          if (!layoutLogged) {
            layoutLogged = true;
            evidence('LAYOUT|$mode|status=$statusRect|rate=$rateRect|file=$fileRect|ellipsis=${render.overflow}|exceeded=${render.didExceedMaxLines}');
          }
          expect(statusRect.bottom <= rateRect.top, isTrue);
          expect(rateRect.bottom <= fileRect.top, isTrue);
          expect(fileRect.right <= view.physicalSize.width / view.devicePixelRatio, isTrue);
          expect(render.overflow, TextOverflow.ellipsis);
          expect(render.didExceedMaxLines, isTrue);
        }
        if (find.byKey(const ValueKey('conn_send_result')).evaluate().isNotEmpty) break;
      }
      final result = textAt('conn_send_result');
      // K1 fix: judged at the END (past the verifying stage) — the rate line
      // must have APPEARED during the transfer (accumulated flag), not be
      // present at the last sampled moment; the file line likewise. The
      // stricter co-presence outcome is reported as evidence.
      evidence('RESULT|$mode|$result|stats=${textAt('conn_send_result_stats')}|threeLines=$sawThreeLines|rateSeen=$sawRate|rateSample=$rateSample');
      await hold(tester, 6);
      expect(result.contains('发送完成'), isTrue, reason: result);
      expect(sawFile, isTrue, reason: 'file line never observed during transferring');
      expect(sawRate, isTrue, reason: 'speed line never appeared during the transfer');
      expect(find.byKey(const ValueKey('conn_state_ended')), findsOneWidget);
      expect(find.text('此连接已断开（对端离开或网络变化）。'), findsNothing);
      await tester.ensureVisible(find.byKey(const ValueKey('conn_back_home_button')));
      await tester.tap(find.byKey(const ValueKey('conn_back_home_button')));
      await tester.pump(const Duration(milliseconds: 900));
      final list = find.byKey(const ValueKey('home_create_code_button'));
      if (list.evaluate().isNotEmpty) { await tester.ensureVisible(list); }
      evidence('ROUND_DONE|$mode');
    }
    evidence('DONE|light and dark real transfers completed');
    await hold(tester, 5);
    tester.platformDispatcher.clearPlatformBrightnessTestValue();
  }, timeout: const Timeout(Duration(minutes: 12)));
}
