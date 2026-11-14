// K1 item a (batch K, 2026-11-14): Windows light/dark readability shots.
//
// The J1 walkthrough found that OS-level screenshots of this host's Flutter
// windows are a flat background (both backends), so visual readability could
// not be judged. This runner captures IN-PROCESS raster images instead: the
// app is mounted inside a RepaintBoundary and every state is exported with
// RenderRepaintBoundary.toImage() — Flutter's own layer-tree raster, not an
// OS screen capture. Report wording must say exactly that.
//
// Everything else mirrors j1_u1_walkthrough_test.dart: the production
// bootstrap (real ak_ffi.dll via AK_FFI_DLL), the real window driven with
// `flutter test -d windows`, a same-machine `ak-cli task join-recv` peer
// against the LOCAL registry/STUN (AK_REGISTRY_ADDR etc. from the process
// environment), and synthetic files only (AK_K1_SYNTH dir).
//
// Per theme (light, dark — test-injected platform brightness, same seam the
// Android aux runner uses) it exports six PNGs to AK_K1_SHOTS (outside the
// repository):
//   win-<theme>-01-awaiting-consent.png   waiting for the receiver's accept
//   win-<theme>-02-transferring.png       percent + average speed + file line
//   win-<theme>-03-verifying.png          all bytes sent, awaiting verify
//   win-<theme>-04-completed.png          result card + stats + ended pill
//   win-<theme>-05-failed.png             declined transfer failure card
//   win-<theme>-06-recent-transfers.png   home history (one success + one failure)
//
// The consent prompt of the CLI is HELD until the awaiting-consent shot has
// been taken (the CLI drops answers typed before its prompt read loop starts,
// so consent is only written after the prompt was observed).
import 'dart:convert' show utf8;
import 'dart:io' show File, FileMode, IOSink, Platform, Process;
import 'dart:ui' show ImageByteFormat;

import 'package:flutter/material.dart'
    show
        Brightness,
        EditableText,
        Key,
        RepaintBoundary,
        Scaffold,
        Scrollable,
        Text,
        Theme,
        ValueKey,
        Widget;
import 'package:flutter/rendering.dart' show RenderRepaintBoundary;
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

import 'package:ak_sending_app/main.dart' as app;
import 'package:ak_sending_app/ui/app.dart' show AkSendingApp;
import 'package:ak_sending_app/ui/service/file_pick_gateway.dart'
    show FilePickGateway, PickedEntry;

const _cliPath =
    String.fromEnvironment('AK_J1_AKCLI', defaultValue: '../target/release/ak-cli.exe');
// One theme per process (AK_K1_THEME=light|dark, default light): the
// pairing that FOLLOWS a declined-and-killed pairing was observed twice to
// stall in hole punching (probes observed both ways, QUIC never completes —
// consistent with the known CR-P3-21 probe instability), so each run does
// success -> decline -> history and never pairs again after a decline.
const _theme = String.fromEnvironment('AK_K1_THEME', defaultValue: 'light');
const _work = String.fromEnvironment('AK_K1_WORK');
const _shots = String.fromEnvironment('AK_K1_SHOTS');
const _logPath = String.fromEnvironment('AK_K1_LOG');
const _registryAddr = String.fromEnvironment('AK_REGISTRY_ADDR');
const _stunAddr = String.fromEnvironment('AK_STUN_ADDR');
// The registry pin rides the process environment, like the J1 runner.
final _registryId = String.fromEnvironment('AK_REGISTRY_ID') != ''
    ? String.fromEnvironment('AK_REGISTRY_ID')
    : Platform.environment['AK_REGISTRY_ID'] ?? '';

IOSink? _evidenceSink;

void _log(String line) {
  // ignore: avoid_print
  print('K1| $line');
  if (_logPath.isEmpty) return;
  _evidenceSink ??= File(_logPath).openWrite(mode: FileMode.append);
  _evidenceSink!.writeln(line);
}

class K1Picker implements FilePickGateway {
  List<PickedEntry> files = const [];
  PickedEntry? dir;

  @override
  Future<List<PickedEntry>> pickFiles() async => files;

  @override
  Future<PickedEntry?> pickDirectory() async => dir;
}

PickedEntry _file(String name, int bytes) => PickedEntry(
    path: '$_work/$name', name: name, isDirectory: false, sizeBytes: bytes);

String? _plainTextOf(Widget w) {
  if (w is Text) return w.data ?? w.textSpan?.toPlainText();
  if (w is EditableText) return w.controller.text;
  return null;
}

String? _textOf(WidgetTester tester, Key key) {
  final finder = find.byKey(key);
  if (!tester.any(finder)) return null;
  for (final w in tester.widgetList(finder)) {
    final t = _plainTextOf(w);
    if (t != null && t.isNotEmpty) return t;
  }
  final inner = find.descendant(of: finder, matching: find.byType(Text));
  if (tester.any(inner)) {
    for (final w in tester.widgetList(inner)) {
      final t = _plainTextOf(w);
      if (t != null && t.isNotEmpty) return t;
    }
  }
  return null;
}

String _textsOf(WidgetTester tester, Key key) {
  final finder = find.byKey(key);
  if (!tester.any(finder)) return '';
  final out = <String>[];
  for (final w in tester.widgetList(finder)) {
    final t = _plainTextOf(w);
    if (t != null && t.isNotEmpty) out.add(t);
  }
  final inner = find.descendant(of: finder, matching: find.byType(Text));
  if (tester.any(inner)) {
    for (final w in tester.widgetList(inner)) {
      final t = _plainTextOf(w);
      if (t != null && t.isNotEmpty) out.add(t);
    }
  }
  return out.join(' / ');
}

bool _anyKeyPrefix(WidgetTester tester, String prefix) => tester.any(
    find.byWidgetPredicate(
        (w) => w.key != null && w.key.toString().contains(prefix)));

Future<bool> _waitFor(
    WidgetTester tester, bool Function() predicate, Duration budget) async {
  final deadline = DateTime.now().add(budget);
  while (DateTime.now().isBefore(deadline)) {
    await tester.pump(const Duration(milliseconds: 120));
    if (predicate()) return true;
  }
  return predicate();
}

/// In-process raster export: Flutter renders the boundary's layer tree into
/// an image (this bypasses the broken OS presentation of this host).
Future<void> _shot(WidgetTester tester, String name) async {
  final boundary = tester.renderObject<RenderRepaintBoundary>(
      find.byKey(const ValueKey('k1_shot_boundary')));
  final image = await boundary.toImage(
      pixelRatio: tester.view.devicePixelRatio);
  final data = await image.toByteData(format: ImageByteFormat.png);
  expect(data, isNotNull, reason: 'toImage produced no png for $name');
  final path = '$_shots/$name.png';
  File(path).writeAsBytesSync(data!.buffer.asUint8List());
  _log('SHOT|$name|bytes=${data.lengthInBytes}|path-outside-repo');
}

/// CLI receiver whose consent is held until [consent] is called; the stdout
/// listener flags the moment the `[Y/n]` prompt actually appeared (answers
/// that arrive before the prompt's read loop are dropped by the CLI).
class _HeldCli {
  _HeldCli(this.process);
  final Process process;
  bool promptSeen = false;
  bool answered = false;

  Future<void> consent(String answer) async {
    final deadline = DateTime.now().add(const Duration(seconds: 40));
    while (!promptSeen && DateTime.now().isBefore(deadline)) {
      await Future<void>.delayed(const Duration(milliseconds: 200));
    }
    expect(promptSeen, isTrue,
        reason: 'CLI consent prompt never appeared within 40s');
    await Future<void>.delayed(const Duration(milliseconds: 600));
    process.stdin.writeln(answer);
    answered = true;
    _log('cli<- typed "$answer"');
  }
}

Future<_HeldCli> _spawnHeldCli(String code, String outDir) async {
  final full = <String>[
    '--id-dir', '$_work/cli-id-$outDir',
    'task', 'join-recv', code, '--out', '$_work/$outDir',
    '--server', _registryAddr,
    '--registry-fp', _registryId,
    '--stun', _stunAddr,
  ];
  _log('cli> ak-cli task join-recv <code> --out $outDir');
  final p = await Process.start(_cliPath, full);
  final cli = _HeldCli(p);
  p.stdout.transform(utf8.decoder).listen((d) {
    _log('cli| ${d.trim()}');
    if (d.contains('accept? [Y/n]')) cli.promptSeen = true;
  });
  p.stderr.transform(utf8.decoder).listen((d) => _log('cli! ${d.trim()}'));
  return cli;
}

Future<String> _createCode(WidgetTester tester) async {
  await tester.tap(find.byKey(const ValueKey('home_create_code_button')));
  await tester.pump(const Duration(milliseconds: 400));
  await tester.tap(find.byKey(const ValueKey('role_send_button')));
  await tester.pump(const Duration(milliseconds: 300));
  final ok = await _waitFor(
      tester, () => tester.any(find.textContaining('AKI1-')), const Duration(seconds: 45));
  expect(ok, isTrue, reason: 'no AKI1 code appeared within 45s');
  for (final w in tester.widgetList(find.textContaining('AKI1-'))) {
    final t = _plainTextOf(w);
    if (t != null && t.startsWith('AKI1-')) return t;
  }
  fail('AKI1 code widget had no readable text');
}

Future<void> _openDetail(WidgetTester tester) async {
  final ok = await _waitFor(tester, () => _anyKeyPrefix(tester, 'conn_card_name_'),
      const Duration(seconds: 60));
  expect(ok, isTrue, reason: 'no connection card appeared');
  await tester.tap(find.byWidgetPredicate(
      (w) => w.key != null && w.key.toString().contains('conn_card_name_')));
  await tester.pump(const Duration(milliseconds: 500));
  expect(tester.any(find.byKey(const ValueKey('conn_detail_page'))), isTrue);
}

Future<void> _pickAndSend(WidgetTester tester, K1Picker picker) async {
  await tester.tap(find.byKey(const ValueKey('conn_pick_files_button')));
  await tester.pump(const Duration(milliseconds: 400));
  await tester.tap(find.byKey(const ValueKey('pick_files_option')));
  await tester.pump(const Duration(milliseconds: 400));
  expect(_textOf(tester, const ValueKey('picked_count')), '已选 1 项');
  await tester.ensureVisible(find.byKey(const ValueKey('conn_send_button')));
  await tester.pump(const Duration(milliseconds: 300));
  await tester.tap(find.byKey(const ValueKey('conn_send_button')));
}

Future<void> _scrollToHistory(WidgetTester tester) async {
  for (var i = 0; i < 12; i++) {
    if (tester.any(find.byKey(const ValueKey('history_section')))) return;
    await tester.scrollUntilVisible(
      find.byKey(const ValueKey('history_section')),
      240,
      scrollable: find.byType(Scrollable).first,
    );
  }
}

void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  binding.framePolicy = LiveTestWidgetsFlutterBindingFramePolicy.fullyLive;

  testWidgets('K1 windows theme shots ($_theme)', (tester) async {
    // Fail closed: never fall back to production endpoints.
    expect(_theme, anyOf('light', 'dark'), reason: 'AK_K1_THEME must be light or dark');
    expect(_registryAddr, '127.0.0.1:17200', reason: 'local registry required');
    expect(_stunAddr, '127.0.0.1:17101', reason: 'local stun required');
    expect(_registryId.length, 64, reason: 'local registry pin required');
    expect(_work, isNotEmpty, reason: 'AK_K1_WORK required');
    expect(_shots, isNotEmpty, reason: 'AK_K1_SHOTS required');
    expect(Platform.environment['AK_DIAG_STAGE'] ?? '', isEmpty,
        reason: 'this walkthrough requires the full production bootstrap');
    for (final key in ['AK_ID_DIR', 'AK_FFI_DLL', 'AK_REGISTRY_ID']) {
      expect(Platform.environment[key], isNotEmpty, reason: '$key required');
    }

    final picker = K1Picker();
    final bootstrap = app.bootstrapAppService();
    _log('bootstrap: service=${bootstrap.service != null} '
        'error=${bootstrap.error ?? '-'}');
    expect(bootstrap.service, isNotNull,
        reason: bootstrap.error ?? 'bootstrap failed');
    await tester.pumpWidget(RepaintBoundary(
      key: const ValueKey('k1_shot_boundary'),
      child: AkSendingApp(service: bootstrap.service!, picker: picker),
    ));
    final home = await _waitFor(tester,
        () => tester.any(find.byKey(const ValueKey('home_create_code_button'))),
        const Duration(seconds: 20));
    expect(home, isTrue, reason: 'home actions did not render');

    for (final mode in [_theme]) {
      _log('THEME_REQUEST|$mode');
      tester.platformDispatcher.platformBrightnessTestValue =
          mode == 'light' ? Brightness.light : Brightness.dark;
      // Wide desktop window renders a NavigationRail (not the narrow-screen
      // NavigationBar), so probe the theme on the first Scaffold instead.
      final themed = await _waitFor(
          tester,
          () => Theme.of(tester.element(find.byType(Scaffold).first)).brightness ==
              (mode == 'light' ? Brightness.light : Brightness.dark),
          const Duration(seconds: 20));
      expect(themed, isTrue, reason: 'theme did not switch to $mode');
      _log('THEME_VERIFIED|$mode|test-platform-brightness');

      // -- success flow with a held consent prompt --------------------------
      picker.files = [_file('synthetic_k1_200mib.bin', 200 * 1024 * 1024)];
      final code = await _createCode(tester);
      _log('CODE|$mode|$code');
      final out = mode == 'light' ? 'recv-light' : 'recv-dark';
      final cli = await _spawnHeldCli(code, out);
      await _openDetail(tester);
      await _pickAndSend(tester, picker);

      final awaiting = await _waitFor(
          tester,
          () => (_textOf(tester, const ValueKey('conn_send_progress_status')) ??
                  '')
              .contains('等待对方点「接受」'),
          // Same-machine multi-path hole punching occasionally needs far
          // longer than the first pairing (observed: light round connected
          // in ~9s, the next pairing was still exchanging probes at 60s).
          const Duration(seconds: 150));
      expect(awaiting, isTrue, reason: 'awaiting-consent status never showed');
      await tester.pump(const Duration(milliseconds: 300));
      await _shot(tester, 'win-$mode-01-awaiting-consent');
      _log('STATE|$mode|awaiting-consent|${_textOf(tester, const ValueKey('conn_send_progress_status'))}');
      await cli.consent('y');

      var verifyingShot = false;
      final transferDeadline = DateTime.now().add(const Duration(seconds: 240));
      while (DateTime.now().isBefore(transferDeadline)) {
        await tester.pump(const Duration(milliseconds: 120));
        final status =
            _textOf(tester, const ValueKey('conn_send_progress_status')) ?? '';
        final rate = _textOf(tester, const ValueKey('conn_send_progress_rate')) ?? '';
        if (status.contains('%') && rate.isNotEmpty && !verifyingShot) {
          // A percent line with a live average-speed line: the readability
          // state the J1 walkthrough could not capture.
          await _shot(tester, 'win-$mode-02-transferring');
          _log('STATE|$mode|transferring|$status|$rate');
          verifyingShot = true; // reused flag: transferring shot taken
        }
        if (status.contains('等待对方校验') && verifyingShot) {
          await _shot(tester, 'win-$mode-03-verifying');
          _log('STATE|$mode|verifying|$status');
          verifyingShot = false;
          break;
        }
        if (tester.any(find.byKey(const ValueKey('conn_send_result')))) break;
      }
      expect(verifyingShot, isFalse, reason: 'verifying state not reached');

      final completed = await _waitFor(tester,
          () => tester.any(find.byKey(const ValueKey('conn_send_result'))),
          const Duration(seconds: 90));
      expect(completed, isTrue, reason: 'send did not complete');
      final result = _textsOf(tester, const ValueKey('conn_send_result'));
      final stats = _textOf(tester, const ValueKey('conn_send_result_stats'));
      _log('RESULT|$mode|$result|stats=$stats');
      expect(result, anyOf(contains('本机直连'), contains('局域网直连')));
      expect(tester.any(find.byKey(const ValueKey('conn_state_ended'))), isTrue);
      await _shot(tester, 'win-$mode-04-completed');
      final exit = await cli.process.exitCode.timeout(const Duration(seconds: 30));
      _log('cli exit: $exit');

      // -- failure flow: receiver declines ----------------------------------
      await tester.tap(find.byKey(const ValueKey('conn_back_home_button')));
      await tester.pump(const Duration(milliseconds: 800));
      if (tester.any(find.byKey(const ValueKey('flow_done_button')))) {
        await tester.tap(find.byKey(const ValueKey('flow_done_button')));
        await tester.pump(const Duration(milliseconds: 400));
      }
      picker.files = [_file('synthetic_k1_small.bin', 5 * 1024 * 1024)];
      final code2 = await _createCode(tester);
      _log('CODE|$mode|$code2 (decline round)');
      final out2 = mode == 'light' ? 'recv-light-decline' : 'recv-dark-decline';
      final cli2 = await _spawnHeldCli(code2, out2);
      await _openDetail(tester);
      await _pickAndSend(tester, picker);
      await cli2.consent('n');
      final failed = await _waitFor(tester,
          () => tester.any(find.byKey(const ValueKey('conn_send_error'))),
          const Duration(seconds: 90));
      expect(failed, isTrue, reason: 'no failure card after decline');
      final error = _textsOf(tester, const ValueKey('conn_send_error'));
      _log('ERROR|$mode|$error');
      expect(tester.any(find.byKey(const ValueKey('conn_send_result'))), isFalse);
      await tester.pump(const Duration(milliseconds: 400));
      await _shot(tester, 'win-$mode-05-failed');
      cli2.process.kill();
      _log('cli killed after decline');

      // -- recent transfers on the home page --------------------------------
      await tester.pageBack();
      await tester.pump(const Duration(milliseconds: 600));
      if (tester.any(find.byKey(const ValueKey('flow_done_button')))) {
        await tester.tap(find.byKey(const ValueKey('flow_done_button')));
        await tester.pump(const Duration(milliseconds: 400));
      }
      await _scrollToHistory(tester);
      expect(tester.any(find.byKey(const ValueKey('history_section'))), isTrue);
      final hist0 = _textsOf(tester, const ValueKey('history_item_0'));
      final hist1 = _textsOf(tester, const ValueKey('history_item_1'));
      _log('HISTORY|$mode|[0]=$hist0|[1]=$hist1');
      expect(hist0, contains('失败'));
      expect(hist1, contains('完成'));
      await tester.pump(const Duration(milliseconds: 400));
      await _shot(tester, 'win-$mode-06-recent-transfers');
      _log('ROUND_DONE|$mode');
    }
    tester.platformDispatcher.clearPlatformBrightnessTestValue();
    await _evidenceSink?.flush();
    _log('DONE|$_theme round: 6 in-process raster shots exported');
  }, timeout: const Timeout(Duration(minutes: 8)));
}
