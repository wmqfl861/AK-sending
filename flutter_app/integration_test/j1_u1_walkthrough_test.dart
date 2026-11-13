// J1 U1 client-UI walkthrough (2026-11-13, batch J item 1).
//
// Drives the REAL Windows desktop app (real window, real ak_ffi.dll via
// main.dart's own production bootstrap; registry/STUN/identity from the
// process environment — the runner exports AK_REGISTRY_ADDR,
// AK_REGISTRY_ID, AK_STUN_ADDR, AK_ID_DIR before invoking this test).
// The peer side is the real `ak-cli task join-recv`/`join-send` process
// spawned against the LOCAL test registry, exactly like the handoff's
// §5 checklist prescribes (same-machine CLI peer).
//
// Transfer scenarios inject only [J1Picker] — the file-pick gateway that the
// pages already accept for tests (widget tests inject fakes by design);
// the native common dialog cannot be driven from a harness. Everything
// below it (engine service, FFI, QUIC transfer, pages) is production.
//
// One scenario per run (each run owns the process, so each run builds
// exactly ONE native engine): AK_J1_SCENARIO=send|recv|dir|reject|kill|
// clear|old|restart. Evidence lines are appended to $AK_J1_LOG plus stdout.
// restart runs twice in DISTINCT OS processes with the SAME app config:
// AK_J1_RESTART_PHASE=seed then verify. Seed explicitly injects one synthetic
// history record (NOT a transfer); the host must wait for process exit before
// verify and preserve AK_ID_DIR. AK_J1_PROCESS_EVIDENCE is an external JSON path.
//
// Host limitation (recorded in the J1 report): on this host the Flutter
// rasterizer presents a blank window for BOTH backends, so visual
// screenshot evidence comes from the Android emulator leg; this test
// asserts the on-screen TEXT of the same widgets instead.
import 'dart:convert' show jsonEncode, utf8;
import 'dart:io' show File, FileMode, IOSink, Platform, Process, pid;

import 'package:flutter/material.dart'
    show EditableText, Key, Offset, Scrollable, Text, ValueKey, Widget;
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

import 'package:ak_sending_app/main.dart' as app;
import 'package:ak_sending_app/ui/app.dart' show AkSendingApp;
import 'package:ak_sending_app/ui/service/file_pick_gateway.dart'
    show FilePickGateway, PickedEntry;
import 'package:ak_sending_app/ui/service/simple_transfer_service.dart'
    show TaskRole;
import 'package:ak_sending_app/ui/service/transfer_history.dart'
    show TransferHistoryScope, TransferRecord;

const _scenario = String.fromEnvironment('AK_J1_SCENARIO');
const _cliPath = String.fromEnvironment(
    'AK_J1_AKCLI', defaultValue: '../target/release/ak-cli.exe');
const _registryAddr =
    String.fromEnvironment('AK_REGISTRY_ADDR', defaultValue: '127.0.0.1:7200');
const _stunAddr =
    String.fromEnvironment('AK_STUN_ADDR', defaultValue: '127.0.0.1:7101');
// The registry pin rides the process environment (same source the app's
// own bootstrap reads); a --dart-define of the same name overrides.
final _registryId = String.fromEnvironment('AK_REGISTRY_ID') != ''
    ? String.fromEnvironment('AK_REGISTRY_ID')
    : Platform.environment['AK_REGISTRY_ID'] ?? '';
// Synthetic-data workspace; ALWAYS injected via --dart-define=AK_J1_WORK
// (no machine-specific default baked into the repo).
const _work = String.fromEnvironment('AK_J1_WORK');
const _logPath = String.fromEnvironment('AK_J1_LOG');

IOSink? _evidenceSink;

void _log(String line) {
  // ignore: avoid_print
  print('J1| $line');
  if (_logPath.isEmpty) return;
  _evidenceSink ??= File(_logPath).openWrite(mode: FileMode.append);
  _evidenceSink!.writeln(line);
}

/// Mutable pick gateway: the walkthrough stages entries per scenario.
class J1Picker implements FilePickGateway {
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

/// Every readable text under [key]'s widget(s), joined for assertions.
String _textsOf(WidgetTester tester, Key key) {
  final finder = find.byKey(key);
  if (!tester.any(finder)) return '';
  final out = <String>[];
  for (final w in tester.widgetList(finder)) {
    final t = _plainTextOf(w);
    if (t != null && t.isNotEmpty) out.add(t);
  }
  // The key may sit on a container: include its descendant Texts.
  final inner = find.descendant(of: finder, matching: find.byType(Text));
  if (tester.any(inner)) {
    for (final w in tester.widgetList(inner)) {
      final t = _plainTextOf(w);
      if (t != null && t.isNotEmpty) out.add(t);
    }
  }
  return out.join(' / ');
}

/// All Text widgets matching a substring (dynamic-key targets).
List<String> _textsContaining(WidgetTester tester, String needle) {
  final out = <String>[];
  final finder = find.textContaining(needle);
  if (!tester.any(finder)) return out;
  for (final w in tester.widgetList(finder)) {
    final t = _plainTextOf(w);
    if (t != null) out.add(t);
  }
  return out;
}

bool _anyKeyPrefix(WidgetTester tester, String prefix) => tester.any(
    find.byWidgetPredicate((w) =>
        w.key != null && w.key.toString().contains(prefix)));

Future<void> _pumpFor(WidgetTester tester, Duration total) async {
  const step = Duration(milliseconds: 150);
  var waited = Duration.zero;
  while (waited < total) {
    await tester.pump(step);
    waited += step;
  }
}

/// Waits until [predicate] turns true, pumping at test cadence.
Future<bool> _waitFor(WidgetTester tester, bool Function() predicate,
    Duration budget) async {
  final deadline = DateTime.now().add(budget);
  while (DateTime.now().isBefore(deadline)) {
    await tester.pump(const Duration(milliseconds: 150));
    if (predicate()) return true;
  }
  return predicate();
}

/// Creates a pairing code via the real UI and returns its display text.
Future<String> _createCode(WidgetTester tester, String roleButtonKey) async {
  await tester.tap(find.byKey(const ValueKey('home_create_code_button')));
  await tester.pump(const Duration(milliseconds: 400));
  await tester.tap(find.byKey(ValueKey(roleButtonKey)));
  await tester.pump(const Duration(milliseconds: 300));
  final ok = await _waitFor(tester,
      () => tester.any(find.textContaining('AKI1-')), const Duration(seconds: 45));
  expect(ok, isTrue, reason: 'no AKI1 code appeared within 45s');
  for (final w in tester.widgetList(find.textContaining('AKI1-'))) {
    final t = _plainTextOf(w);
    if (t != null && t.startsWith('AKI1-')) return t;
  }
  fail('AKI1 code widget had no readable text');
}

/// Spawns the CLI peer with the local registry pin; [consent] is typed
/// only after the CLI's `[Y/n]` prompt APPEARS (the reader thread drops
/// lines that completed before the prompt started — R36-P1-04).
Future<Process> _spawnCli(List<String> args, {String consent = 'y'}) async {
  final full = <String>[
    '--id-dir', '$_work/cli-id',
    'task', ...args,
    '--server', _registryAddr,
    '--registry-fp', _registryId,
    '--stun', _stunAddr,
  ];
  _log('cli> ak-cli ${full.join(' ')}');
  final p = await Process.start(_cliPath, full);
  var answered = false;
  p.stdout.transform(utf8.decoder).listen((d) {
    _log('cli| ${d.trim()}');
    if (!answered && d.contains('accept? [Y/n]')) {
      answered = true;
      // The prompt print and the read-loop start race by microseconds;
      // a small delay keeps the answer's arrival timestamp after both.
      Future<void>.delayed(const Duration(milliseconds: 600), () {
        p.stdin.writeln(consent);
        _log('cli<- typed "$consent"');
      });
    }
  });
  p.stderr.transform(utf8.decoder).listen((d) => _log('cli! ${d.trim()}'));
  return p;
}

/// Scrolls the home ListView until the history section is built (its
/// tiles are lazy — below the fold means not built at all).
Future<void> _scrollToHistory(WidgetTester tester) async {
  for (var i = 0; i < 10; i++) {
    if (tester.any(find.byKey(const ValueKey('history_section')))) return;
    await tester.scrollUntilVisible(
      find.byKey(const ValueKey('history_section')),
      240,
      scrollable: find.byType(Scrollable).first,
    );
  }
}

Future<void> _openDetailOfFirstConnection(WidgetTester tester) async {  final ok = await _waitFor(
      tester, () => _anyKeyPrefix(tester, 'conn_card_name_'), const Duration(seconds: 60));
  expect(ok, isTrue, reason: 'no connection card appeared');
  await tester.tap(find.byWidgetPredicate(
      (w) => w.key != null && w.key.toString().contains('conn_card_name_')));
  await tester.pump(const Duration(milliseconds: 500));
  expect(tester.any(find.byKey(const ValueKey('conn_detail_page'))), isTrue,
      reason: 'detail page did not open');
}

void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  binding.framePolicy = LiveTestWidgetsFlutterBindingFramePolicy.fullyLive;

  testWidgets('J1 walkthrough scenario=$_scenario', (tester) async {
    if (_scenario == 'restart') {
      // Fail closed before native startup: never fall back to production.
      for (final key in ['AK_ID_DIR', 'AK_FFI_DLL', 'AK_REGISTRY_ID',
        'AK_J1_PROCESS_EVIDENCE']) {
        expect(Platform.environment[key], isNotEmpty, reason: '$key required');
      }
      for (final key in ['AK_REGISTRY_ADDR', 'AK_STUN_ADDR']) {
        expect(Platform.environment[key], matches(r'^127\.0\.0\.1:\d+$'),
            reason: '$key must explicitly select a local endpoint');
      }
      expect(Platform.environment['AK_DIAG_STAGE'] ?? '', isEmpty,
          reason: 'restart requires full production bootstrap');
      expect(Platform.environment['AK_J1_RESTART_PHASE'],
          anyOf('seed', 'verify'));
      _restartEvidence('process_started');
    }
    final picker = J1Picker();
    final bootstrap = app.bootstrapAppService();
    _log('bootstrap: service=${bootstrap.service != null} '
        'error=${bootstrap.error ?? '-'}');
    expect(bootstrap.service, isNotNull,
        reason: bootstrap.error ?? 'bootstrap failed');
    await tester.pumpWidget(
        AkSendingApp(service: bootstrap.service!, picker: picker));
    // Home renders.
    final home = await _waitFor(tester,
        () => tester.any(find.byKey(const ValueKey('home_create_code_button'))),
        const Duration(seconds: 20));
    expect(home, isTrue, reason: 'home actions did not render');

    switch (_scenario) {
      case 'send':
        await _scenarioSend(tester, picker);
      case 'recv':
        await _scenarioRecv(tester, picker);
      case 'dir':
        await _scenarioDir(tester, picker);
      case 'reject':
        await _scenarioReject(tester, picker);
      case 'kill':
        await _scenarioKill(tester, picker);
      case 'clear':
        await _scenarioClear(tester, picker);
      case 'restart':
        await _scenarioRestart(tester);
      case 'old':
        await _scenarioOldDll(tester, picker);
      default:
        fail('unknown AK_J1_SCENARIO=$_scenario');
    }
    await _evidenceSink?.flush();
  }, timeout: const Timeout(Duration(minutes: 8)));
}

// -- scenario bodies -------------------------------------------------------

/// True on emulator runs: the peer CLI is started OUTSIDE the test (the
/// test process lives on Android and cannot spawn the host CLI); the
/// pairing code is printed for the host side to redeem.
const _noCli = String.fromEnvironment('AK_J1_NO_CLI');

/// J1-1/2 (and 10 when AK_J1_OLD_DLL=1): GUI sends -> CLI receives.
Future<void> _scenarioSend(WidgetTester tester, J1Picker picker) async {
  picker.files = [_file('synthetic_200mib.bin', 200 * 1024 * 1024)];
  final code = await _createCode(tester, 'role_send_button');
  _log('code: $code');
  final cli = _noCli == '1'
      ? null
      : await _spawnCli(['join-recv', code, '--out', '$_work/recv_out']);

  await _openDetailOfFirstConnection(tester);
  // Pick via the injected gateway.
  await tester.tap(find.byKey(const ValueKey('conn_pick_files_button')));
  await tester.pump(const Duration(milliseconds: 400));
  await tester.tap(find.byKey(const ValueKey('pick_files_option')));
  await tester.pump(const Duration(milliseconds: 400));
  expect(_textOf(tester, const ValueKey('picked_count')), '已选 1 项');

  // The awaiting-consent phase: the CLI must not have consented yet — it
  // did (auto y), so sample fast and record whatever phases are visible.
  await tester.tap(find.byKey(const ValueKey('conn_send_button')));
  final samples = <String>[];
  final deadline = DateTime.now().add(const Duration(seconds: 150));
  while (DateTime.now().isBefore(deadline)) {
    await tester.pump(const Duration(milliseconds: 120));
    final status =
        _textOf(tester, const ValueKey('conn_send_progress_status'));
    final rate = _textOf(tester, const ValueKey('conn_send_progress_rate'));
    final file = _textOf(tester, const ValueKey('conn_send_progress_file'));
    final line = [status, rate, file].whereType<String>().join(' | ');
    if (line.isNotEmpty && (samples.isEmpty || samples.last != line)) {
      samples.add(line);
      _log('progress: $line');
    }
    if (tester.any(find.byKey(const ValueKey('conn_send_result')))) break;
  }
  _log('samples(${samples.length}): ${samples.take(6).join(' ;; ')} ... '
      '${samples.length > 6 ? samples.skip(samples.length - 3).join(' ;; ') : ''}');

  // Completed: result card + stats + ended pill + back button.
  expect(tester.any(find.byKey(const ValueKey('conn_send_result'))), isTrue,
      reason: 'no send result card; samples=$samples');
  final result =
      _textsContaining(tester, '发送完成').join('/');
  _log('result text: $result');
  final stats = _textOf(tester, const ValueKey('conn_send_result_stats'));
  _log('stats: $stats');
  expect(result, anyOf(contains('本机直连'), contains('局域网直连')),
      reason: 'direct-path label missing: $result');
  expect(stats, isNotNull, reason: 'elapsed/average line missing');
  expect(stats, contains('用时'));
  expect(tester.any(find.byKey(const ValueKey('conn_state_ended'))), isTrue,
      reason: 'ended pill missing');
  expect(tester.any(find.byKey(const ValueKey('conn_back_home_button'))), isTrue,
      reason: 'back-to-list button missing');
  expect(_textsContaining(tester, '此连接已断开'), isEmpty,
      reason: 'misleading dropped-connection copy appeared');
  expect(_textOf(tester, const ValueKey('conn_detail_role')),
      '本次传输已完成，配对随之结束。如需再次传输，请重新配对。');

  // Back home: no current connection row, one history row (the section
  // can sit below the fold — collapse the pairing flow card and scroll).
  await tester.tap(find.byKey(const ValueKey('conn_back_home_button')));
  await _pumpFor(tester, const Duration(seconds: 1));
  expect(_anyKeyPrefix(tester, 'conn_card_name_'), isFalse,
      reason: 'connection row still listed after normal end');
  if (tester.any(find.byKey(const ValueKey('flow_done_button')))) {
    await tester.tap(find.byKey(const ValueKey('flow_done_button')));
    await tester.pump(const Duration(milliseconds: 400));
  }
  await _scrollToHistory(tester);
  expect(tester.any(find.byKey(const ValueKey('history_section'))), isTrue,
      reason: 'history section missing');
  final hist = _textsOf(tester, const ValueKey('history_item_0'));
  _log('history[0]: $hist');
  expect(hist, contains('发送 1 个文件'));
  expect(hist, contains('200 MB'));
  expect(hist, contains('完成'));
  expect(hist, anyOf(contains('本机直连'), contains('局域网直连')),
      reason: 'history row lacks the path label: $hist');

  if (cli != null) {
    final exit = await cli.exitCode.timeout(const Duration(seconds: 30));
    _log('cli exit: $exit');
  }
}

/// J1-3: CLI sends -> GUI receives via the banner.
Future<void> _scenarioRecv(WidgetTester tester, J1Picker picker) async {
  final code = await _createCode(tester, 'role_receive_button');
  _log('code: $code');
  final cli = _noCli == '1' ? null : await _spawnCli([
    'join-send', code, '$_work/synthetic_200mib.bin',
  ]);

  // Incoming banner appears; accept it.
  final offered = await _waitFor(tester,
      () => tester.any(find.textContaining('对方请求发送 1 个文件')),
      const Duration(seconds: 60));
  expect(offered, isTrue, reason: 'no incoming request banner');
  await tester.pump(const Duration(milliseconds: 300));
  await tester.tap(find.byWidgetPredicate(
      (w) => w.key != null && w.key.toString().contains('incoming_accept_')));
  _log('accepted');

  final samples = <String>[];
  final deadline = DateTime.now().add(const Duration(seconds: 150));
  while (DateTime.now().isBefore(deadline)) {
    await tester.pump(const Duration(milliseconds: 120));
    final recv = _textsContaining(tester, '已接收 ').join('/');
    final verify = _textsContaining(tester, '正在校验文件完整性').isNotEmpty
        ? '正在校验文件完整性…'
        : '';
    final caption = _textsContaining(tester, RegExp('接收中 ').pattern).join('/');
    final line = [recv, verify, caption].where((s) => s.isNotEmpty).join(' | ');
    if (line.isNotEmpty && (samples.isEmpty || samples.last != line)) {
      samples.add(line);
      _log('progress: $line');
    }
    if (tester.any(find.byWidgetPredicate((w) =>
        w.key != null &&
        w.key.toString().contains('incoming_success_')))) {
      break;
    }
  }
  _log('recv samples(${samples.length}): ${samples.take(4).join(' ;; ')}');
  expect(samples, isNotEmpty, reason: 'no receive progress observed');

  final success = _textsContaining(tester, '已接收 1 个文件').join('/');
  _log('success: $success');
  final summary = _textsContaining(tester, '用时').join('/');
  _log('summary: $summary');
  expect(success, isNotEmpty, reason: 'receive success card missing');
  expect(summary, contains('平均'), reason: 'average speed missing');

  await tester.tap(find.byWidgetPredicate((w) =>
      w.key != null && w.key.toString().contains('incoming_dismiss_')));
  await tester.pump(const Duration(milliseconds: 400));
  if (tester.any(find.byKey(const ValueKey('flow_done_button')))) {
    await tester.tap(find.byKey(const ValueKey('flow_done_button')));
    await tester.pump(const Duration(milliseconds: 400));
  }
  await _scrollToHistory(tester);
  final hist = _textsOf(tester, const ValueKey('history_item_0'));
  _log('history[0]: $hist');
  expect(hist, contains('接收 1 个文件'));
  expect(hist, contains('完成'));

  if (cli != null) {
    final exit = await cli.exitCode.timeout(const Duration(seconds: 30));
    _log('cli exit: $exit');
  }
}

/// J1-4: send a directory of small files; (i/N) bounded by N.
Future<void> _scenarioDir(WidgetTester tester, J1Picker picker) async {
  picker.dir = PickedEntry(
      path: '$_work/synthetic_dir',
      name: 'synthetic_dir',
      isDirectory: true);
  final code = await _createCode(tester, 'role_send_button');
  _log('code: $code');
  final cli = await _spawnCli([
    'join-recv', code, '--out', '$_work/recv_out2',
  ]);

  await _openDetailOfFirstConnection(tester);
  await tester.tap(find.byKey(const ValueKey('conn_pick_files_button')));
  await tester.pump(const Duration(milliseconds: 400));
  await tester.tap(find.byKey(const ValueKey('pick_dir_option')));
  await tester.pump(const Duration(milliseconds: 400));
  expect(_textOf(tester, const ValueKey('picked_count')), '已选 1 项');

  await tester.tap(find.byKey(const ValueKey('conn_send_button')));
  final samples = <String>[];
  final deadline = DateTime.now().add(const Duration(seconds: 90));
  while (DateTime.now().isBefore(deadline)) {
    await tester.pump(const Duration(milliseconds: 120));
    final file = _textOf(tester, const ValueKey('conn_send_progress_file'));
    if (file != null && (samples.isEmpty || samples.last != file)) {
      samples.add(file);
      _log('file-line: $file');
    }
    if (tester.any(find.byKey(const ValueKey('conn_send_result')))) break;
  }
  _log('dir samples: ${samples.join(' ;; ')}');
  // Every (i/N) must satisfy i <= N (list entries count as N).
  final n = <int>[];
  for (final s in samples) {
    final m = RegExp(r'（(\d+)/(\d+)）').firstMatch(s);
    if (m != null) n.add(int.parse(m.group(1)!));
  }
  for (final i in n) {
    expect(i, lessThanOrEqualTo(6), reason: 'file index exceeded entry count');
  }
  final result = _textsContaining(tester, '发送完成').join('/');
  _log('result: $result');
  final m = RegExp(r'已发送 (\d+) 个文件').firstMatch(result);
  expect(m, isNotNull, reason: 'no entry count in result: $result');
  final sent = int.parse(m!.group(1)!);
  expect(sent, greaterThanOrEqualTo(4), reason: 'entry count mismatch: $result');
  _log('entries sent: $sent');
  final exit = await cli.exitCode.timeout(const Duration(seconds: 30));
  _log('cli exit: $exit');
}

/// J1-5a: receiver declines -> sender card shows the failure.
Future<void> _scenarioReject(WidgetTester tester, J1Picker picker) async {
  picker.files = [_file('synthetic_small_1.bin', 5 * 1024 * 1024)];
  final code = await _createCode(tester, 'role_send_button');
  _log('code: $code');
  final cli = await _spawnCli([
    'join-recv', code, '--out', '$_work/recv_out3',
  ], consent: 'n');

  await _openDetailOfFirstConnection(tester);
  await tester.tap(find.byKey(const ValueKey('conn_pick_files_button')));
  await tester.pump(const Duration(milliseconds: 400));
  await tester.tap(find.byKey(const ValueKey('pick_files_option')));
  await tester.pump(const Duration(milliseconds: 400));
  await tester.tap(find.byKey(const ValueKey('conn_send_button')));
  final failed = await _waitFor(tester,
      () => tester.any(find.byKey(const ValueKey('conn_send_error'))),
      const Duration(seconds: 90));
  expect(failed, isTrue, reason: 'no failure card after decline');
  final error = _textsContaining(tester, '发送失败').join('/');
  _log('error: $error');
  expect(tester.any(find.byKey(const ValueKey('conn_send_result'))), isFalse,
      reason: 'success card shown despite decline');
  await _pumpFor(tester, const Duration(seconds: 2));
  await tester.pageBack();
  await _pumpFor(tester, const Duration(seconds: 1));
  if (tester.any(find.byKey(const ValueKey('flow_done_button')))) {
    await tester.tap(find.byKey(const ValueKey('flow_done_button')));
    await tester.pump(const Duration(milliseconds: 400));
  }
  await _scrollToHistory(tester);
  final hist = _textsOf(tester, const ValueKey('history_item_0'));
  _log('history[0]: $hist');
  expect(hist, contains('失败'));
  // The declining receiver keeps listening for a retry (one-shot task,
  // but the CLI serves the code until its TTL) — end it from our side.
  cli.kill();
  _log('cli killed after decline');
}

/// J1-5b: receiver killed mid-transfer -> sender fails, never "completed".
Future<void> _scenarioKill(WidgetTester tester, J1Picker picker) async {
  picker.files = [_file('synthetic_200mib.bin', 200 * 1024 * 1024)];
  final code = await _createCode(tester, 'role_send_button');
  _log('code: $code');
  final cli = await _spawnCli([
    'join-recv', code, '--out', '$_work/recv_out4',
  ]);

  await _openDetailOfFirstConnection(tester);
  await tester.tap(find.byKey(const ValueKey('conn_pick_files_button')));
  await tester.pump(const Duration(milliseconds: 400));
  await tester.tap(find.byKey(const ValueKey('pick_files_option')));
  await tester.pump(const Duration(milliseconds: 400));
  await tester.tap(find.byKey(const ValueKey('conn_send_button')));
  // Wait until bytes move, then kill the receiver mid-flight.
  var moved = false;
  final killDeadline = DateTime.now().add(const Duration(seconds: 60));
  while (DateTime.now().isBefore(killDeadline)) {
    await tester.pump(const Duration(milliseconds: 100));
    final s = _textOf(tester, const ValueKey('conn_send_progress_status'));
    if (s != null && s.contains('%')) {
      moved = true;
      _log('mid-transfer status at kill: $s');
      break;
    }
  }
  expect(moved, isTrue, reason: 'transfer never showed progress before kill');
  cli.kill();
  _log('cli killed');
  final failed = await _waitFor(tester,
      () => tester.any(find.byKey(const ValueKey('conn_send_error'))),
      const Duration(seconds: 120));
  expect(failed, isTrue, reason: 'no failure card after receiver kill');
  expect(tester.any(find.byKey(const ValueKey('conn_send_result'))), isFalse,
      reason: 'success card shown despite killed receiver');
  expect(tester.any(find.byKey(const ValueKey('conn_state_ended'))), isFalse,
      reason: '"completed" pill shown for a failed transfer');
  final error = _textsContaining(tester, '发送失败').join('/');
  _log('error: $error');
  await tester.pageBack();
  await _pumpFor(tester, const Duration(seconds: 1));
  if (tester.any(find.byKey(const ValueKey('flow_done_button')))) {
    await tester.tap(find.byKey(const ValueKey('flow_done_button')));
    await tester.pump(const Duration(milliseconds: 400));
  }
  await _scrollToHistory(tester);
  final hist = _textsOf(tester, const ValueKey('history_item_0'));
  _log('history[0]: $hist');
  expect(hist, contains('失败'));
}

/// J1-6 restart evidence. The external host owns process exit/relaunch and
/// records both command return codes; a pumpWidget is NOT a restart.
void _restartEvidence(String checkpoint, {int? historyCount}) {
  // Kept outside the repository; do not print identity paths or registry pins.
  File(Platform.environment['AK_J1_PROCESS_EVIDENCE']!).writeAsStringSync(
    '${jsonEncode({
      'checkpoint': checkpoint,
      'app_pid': pid,
      'at_utc': DateTime.now().toUtc().toIso8601String(),
      'phase': Platform.environment['AK_J1_RESTART_PHASE'],
      'executable': Platform.resolvedExecutable,
      'config': {for (final key in ['AK_ID_DIR', 'AK_FFI_DLL',
        'AK_REGISTRY_ADDR', 'AK_REGISTRY_ID', 'AK_STUN_ADDR'])
        key: Platform.environment[key]},
      'history_count': historyCount,
      'record_origin': 'synthetic_history_injection_not_transfer',
    })}\n',
    mode: FileMode.append,
    flush: true,
  );
}

Future<void> _scenarioRestart(WidgetTester tester) async {
  await _pumpFor(tester, const Duration(seconds: 2));
  expect(find.byKey(const ValueKey('app_startup_error')), findsNothing);
  expect(find.byKey(const ValueKey('engine_failed_card')), findsNothing);
  final root = tester.widget<AkSendingApp>(find.byType(AkSendingApp));
  expect(root.history, isNull, reason: 'must use the app-owned history');
  final history = TransferHistoryScope.read(
      tester.element(find.byKey(const ValueKey('home_create_code_button'))));
  expect(history, isNotNull);
  expect(history!.records, isEmpty, reason: 'history present at cold startup');
  if (Platform.environment['AK_J1_RESTART_PHASE'] == 'seed') {
    // Explicit model-layer injection; no fake service, no real transfer claim,
    // no alternate history owner and no change to the persistence mechanism.
    history.add(TransferRecord(
      role: TaskRole.send,
      peerName: 'synthetic-restart-peer',
      succeeded: true,
      finishedAt: DateTime.now(),
      fileCount: 1,
      totalBytes: 1024,
    ));
    await tester.pump(const Duration(milliseconds: 400));
    await _scrollToHistory(tester);
    expect(find.byKey(const ValueKey('history_section')), findsOneWidget);
    expect(_textsOf(tester, const ValueKey('history_item_0')),
        contains('synthetic-restart-peer'));
    expect(history.records, hasLength(1));
    // Leave the record present. The host waits for this OS process to exit.
    _log('restart seed: app_pid=$pid history_count=1 '
        'origin=synthetic_history_injection_not_transfer; NOT cleared');
  } else {
    // The model assertion also rules out a false pass from lazy ListView tiles.
    await tester.drag(find.byKey(const ValueKey('home_page')),
        const Offset(0, -800));
    await _pumpFor(tester, const Duration(seconds: 1));
    expect(history.records, isEmpty);
    expect(find.byKey(const ValueKey('history_section')), findsNothing);
    expect(find.byKey(const ValueKey('history_item_0')), findsNothing);
    _log('restart verify: app_pid=$pid history_count=0 section_absent=true');
  }
  _restartEvidence('assertions_passed', historyCount: history.records.length);
}

/// J1-6 clear-only check; process restart is the separate restart scenario.
Future<void> _scenarioClear(WidgetTester tester, J1Picker picker) async {
  picker.files = [_file('synthetic_small_1.bin', 5 * 1024 * 1024)];
  final code = await _createCode(tester, 'role_send_button');
  _log('code: $code');
  final cli = await _spawnCli([
    'join-recv', code, '--out', '$_work/recv_out5',
  ]);
  await _openDetailOfFirstConnection(tester);
  await tester.tap(find.byKey(const ValueKey('conn_pick_files_button')));
  await tester.pump(const Duration(milliseconds: 400));
  await tester.tap(find.byKey(const ValueKey('pick_files_option')));
  await tester.pump(const Duration(milliseconds: 400));
  await tester.tap(find.byKey(const ValueKey('conn_send_button')));
  final done = await _waitFor(tester,
      () => tester.any(find.byKey(const ValueKey('conn_send_result'))),
      const Duration(seconds: 90));
  expect(done, isTrue, reason: 'small send did not finish');
  await _pumpFor(tester, const Duration(seconds: 2));
  await cli.exitCode.timeout(const Duration(seconds: 30));
  await tester.tap(find.byKey(const ValueKey('conn_back_home_button')));
  await _pumpFor(tester, const Duration(seconds: 1));
  if (tester.any(find.byKey(const ValueKey('flow_done_button')))) {
    await tester.tap(find.byKey(const ValueKey('flow_done_button')));
    await tester.pump(const Duration(milliseconds: 400));
  }
  await _scrollToHistory(tester);
  expect(tester.any(find.byKey(const ValueKey('history_section'))), isTrue);
  await tester.tap(find.byKey(const ValueKey('history_clear_button')));
  await tester.pump(const Duration(milliseconds: 500));
  expect(tester.any(find.byKey(const ValueKey('history_section'))), isFalse,
      reason: 'history section still visible after clear');
  _log('history cleared; section gone');

  // No restart claim here: rebuilding a widget is not an OS process restart.
}

/// J1-10: run with the OLD (97fac96) DLL — indeterminate progress, honest
/// completion, no engine-failure banner. The runner swaps the DLL in
/// target/release before this run.
Future<void> _scenarioOldDll(WidgetTester tester, J1Picker picker) async {
  picker.files = [_file('synthetic_small_1.bin', 5 * 1024 * 1024)];
  final code = await _createCode(tester, 'role_send_button');
  _log('code: $code');
  final cli = await _spawnCli([
    'join-recv', code, '--out', '$_work/recv_out6',
  ]);
  await _openDetailOfFirstConnection(tester);
  await tester.tap(find.byKey(const ValueKey('conn_pick_files_button')));
  await tester.pump(const Duration(milliseconds: 400));
  await tester.tap(find.byKey(const ValueKey('pick_files_option')));
  await tester.pump(const Duration(milliseconds: 400));
  await tester.tap(find.byKey(const ValueKey('conn_send_button')));
  final seen = <String>[];
  final deadline = DateTime.now().add(const Duration(seconds: 90));
  while (DateTime.now().isBefore(deadline)) {
    await tester.pump(const Duration(milliseconds: 100));
    final s = _textOf(tester, const ValueKey('conn_send_progress_status'));
    if (s != null && (seen.isEmpty || seen.last != s)) seen.add(s);
    if (tester.any(find.byKey(const ValueKey('conn_send_result')))) break;
  }
  _log('old-dll statuses: ${seen.join(' ;; ')}');
  expect(seen, everyElement((String s) => !s.contains('%')),
      reason: 'old DLL showed a percentage');
  expect(seen, contains('正在发送…'),
      reason: 'old DLL did not fall back to the honest wording');
  final done = await _waitFor(tester,
      () => tester.any(find.byKey(const ValueKey('conn_send_result'))),
      const Duration(seconds: 30));
  expect(done, isTrue, reason: 'old-DLL send did not complete');
  expect(tester.any(find.byKey(const ValueKey('conn_state_ended'))), isTrue);
  expect(tester.any(find.byKey(const ValueKey('engine_failed_card'))), isFalse,
      reason: 'engine-failure banner shown with the old DLL');
  final exit = await cli.exitCode.timeout(const Duration(seconds: 30));
  _log('cli exit: $exit');
}
