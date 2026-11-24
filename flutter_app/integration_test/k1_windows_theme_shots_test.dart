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
// X4 (batch X, 2026-11-22): both rounds ride the new home's 发送文件 MAIN
// path — the staged synthetic file rides the entry's pick (先选文件再生
// 码), and once the CLI peer joins the app sends it AUTOMATICALLY (审核
// §4.3: no pick sheet, no send button). The awaiting-consent / decline
// shots therefore watch the auto-opened detail page from the send's
// natural start.
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
//
// K 复核 K-R4/K-R5: the transferring/verifying shots are counted PER STATE
// (a round that samples neither, or only one, FAILS — the exported PNGs are
// re-checked on disk too), and the local-endpoint guard validates the
// EFFECTIVE endpoints (runtime environment over compiled defines — the
// invite plane's precedence), handing the same verified values to the app
// bootstrap and the CLI peer.
import 'dart:convert' show utf8;
import 'dart:io' show File, FileMode, IOSink, Platform, Process;
import 'dart:typed_data' show Uint8List;
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
import 'package:ak_sending_app/services/connection_diagnostics.dart'
    show ConnectionDiagnosticsLog;
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
const _registryIdCompiled = String.fromEnvironment('AK_REGISTRY_ID');

// ---------------------------------------------------------------------------
// K 复核 K-R5 guard helpers (pure; the companion unit tests live in
// test/k1_runner_guards_test.dart and import this file so they cannot
// drift from the runner's own guard).
// ---------------------------------------------------------------------------

/// The same-machine endpoints this walkthrough may talk to (loopback
/// only — anything else must fail the run).
const k1ExpectedRegistryAddr = '127.0.0.1:17200';
const k1ExpectedStunAddr = '127.0.0.1:17101';

/// Runtime environment first, then the compiled `--dart-define` value —
/// EXACTLY the invite plane's precedence (`FfiSimpleTransferService`'s
/// `_envOrCompiled`: the trimmed runtime value wins when non-empty; an
/// unset, empty or whitespace-only one falls through to the trimmed
/// compiled value; both-empty is null). Keep in sync with the service's
/// resolution — the runner's guard is only honest while this mirrors it.
String? k1EnvOrCompiled(
    Map<String, String> environment, String key, String compiled) {
  final fromEnv = environment[key]?.trim();
  if (fromEnv != null && fromEnv.isNotEmpty) return fromEnv;
  final trimmed = compiled.trim();
  return trimmed.isEmpty ? null : trimmed;
}

/// The endpoints the app under test will ACTUALLY dial (K 复核 K-R5):
/// the per-key [k1EnvOrCompiled] results. A null [stunAddr] means the
/// invite plane falls back to its PRODUCTION default inside the service
/// — never a local value; null [registryAddr]/[registryIdHex] means
/// `start()` would refuse to boot at all.
final class K1EffectiveEndpoints {
  const K1EffectiveEndpoints(
      this.registryAddr, this.stunAddr, this.registryIdHex);

  final String? registryAddr;
  final String? stunAddr;
  final String? registryIdHex;
}

/// Resolves [K1EffectiveEndpoints] from the same two tiers the app
/// reads: the process environment and the compiled defines.
K1EffectiveEndpoints k1ResolveEffectiveEndpoints(
    {required Map<String, String> environment,
    required String compiledRegistryAddr,
    required String compiledStunAddr,
    required String compiledRegistryId}) {
  return K1EffectiveEndpoints(
    k1EnvOrCompiled(environment, 'AK_REGISTRY_ADDR', compiledRegistryAddr),
    k1EnvOrCompiled(environment, 'AK_STUN_ADDR', compiledStunAddr),
    k1EnvOrCompiled(environment, 'AK_REGISTRY_ID', compiledRegistryId),
  );
}

/// K 复核 K-R5 guard predicate: names every reason the EFFECTIVE
/// endpoints are not the intended local ones (empty list = the run dials
/// exactly the loopback registry/STUN with a 64-hex pin). The runner
/// fails on ANY finding BEFORE the app boots or the CLI spawns —
/// including the review's counterexample: a verified compiled local
/// value silently shadowed by an inherited non-local runtime value.
List<String> k1LocalEndpointFindings(K1EffectiveEndpoints effective) {
  final findings = <String>[];
  if (effective.registryAddr != k1ExpectedRegistryAddr) {
    findings.add('AK_REGISTRY_ADDR 生效值=${effective.registryAddr ?? "(未配置)"}'
        '，应为 $k1ExpectedRegistryAddr');
  }
  if (effective.stunAddr != k1ExpectedStunAddr) {
    // null 时应用回退内置生产默认 STUN —— 同样不是本地值。
    findings.add('AK_STUN_ADDR 生效值='
        '${effective.stunAddr ?? "(未配置，应用回退生产默认)"}'
        '，应为 $k1ExpectedStunAddr');
  }
  final pin = effective.registryIdHex;
  if (pin == null || pin.length != 64) {
    findings.add('AK_REGISTRY_ID 生效钉扎长度=${pin?.length ?? 0}，应为 64');
  }
  return findings;
}

/// The effective endpoints of THIS process (runtime env over the
/// compiled defines): what the guard asserts, what the app bootstrap is
/// handed and what the CLI peer dials — one resolution, three uses.
final _effective = k1ResolveEffectiveEndpoints(
    environment: Platform.environment,
    compiledRegistryAddr: _registryAddr,
    compiledStunAddr: _stunAddr,
    compiledRegistryId: _registryIdCompiled);

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
  Future<PickedEntry?> pickSendDirectory() => pickDirectory();

  @override
  Future<void> releaseStaged(Iterable<PickedEntry> entries) async {}

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

/// K 复核 K-R4: per-state export bookkeeping for the two SAMPLED shots
/// (transferring/verifying). The pre-fix single flag was INITIALIZED to
/// false and reset after the verifying shot, so a sampling pass that
/// skipped both states and jumped straight to the result card still
/// satisfied the final `expect(flag, isFalse)` while the run claimed a
/// complete shot set. These counters only ever move UP on a real
/// export, so there is no reset path back to "nothing captured";
/// [missingStates] names whatever was not captured and the runner fails
/// on ANY missing state.
class K1ShotCoverage {
  int _transferringShots = 0;
  int _verifyingShots = 0;

  /// Exported transferring shots so far (>= 1 required for the round).
  int get transferringShots => _transferringShots;

  /// Exported verifying shots so far (>= 1 required for the round).
  int get verifyingShots => _verifyingShots;

  /// Whether the transferring percent+rate state was captured at least
  /// once (also the sampling loop's once-only latch).
  bool get hasTransferring => _transferringShots > 0;

  /// Whether the verifying state was captured at least once.
  bool get hasVerifying => _verifyingShots > 0;

  /// Count one exported `win-<theme>-02-transferring` shot.
  void recordTransferringShot() => _transferringShots++;

  /// Count one exported `win-<theme>-03-verifying` shot.
  void recordVerifyingShot() => _verifyingShots++;

  /// The sampled states with ZERO exports (empty iff both captured) —
  /// the message names exactly what the round is missing.
  List<String> missingStates() => [
        if (!hasTransferring) 'transferring',
        if (!hasVerifying) 'verifying',
      ];

  /// True iff [missingStates] is empty — what the runner asserts on
  /// BEFORE the run may claim a complete shot set.
  bool get isComplete => missingStates().isEmpty;
}

/// K 复核 K-R4「并检查目标文件」: the coverage counters prove the two
/// SAMPLED states were reached; this re-checks the actual PNGs on disk
/// (the exported set lives in AK_K1_SHOTS outside the repository) before
/// the run may claim the complete six-shot set — including the two
/// conditional shots, so an export that never landed fails HERE.
void _requireRoundShotFiles(String mode) {
  const names = [
    '01-awaiting-consent',
    '02-transferring',
    '03-verifying',
    '04-completed',
    '05-failed',
    '06-recent-transfers',
  ];
  for (final name in names) {
    final file = File('$_shots/win-$mode-$name.png');
    expect(file.existsSync() && file.lengthSync() > 0, isTrue,
        reason: 'K 复核 K-R4: 截图文件缺失或为空 win-$mode-$name.png');
  }
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
  // K 复核 K-R5: the CLI peer dials the SAME verified EFFECTIVE
  // endpoints the app bootstrap was handed — never the raw compiled
  // defines, which an inherited runtime variable could have overridden.
  final full = <String>[
    '--id-dir', '$_work/cli-id-$outDir',
    'task', 'join-recv', code, '--out', '$_work/$outDir',
    '--server', _effective.registryAddr!,
    '--registry-fp', _effective.registryIdHex!,
    '--stun', _effective.stunAddr!,
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

/// X4 发送文件主路径: the caller STAGES picker.files BEFORE this call;
/// the tap runs the multi-select pick and only the picked selection
/// mints the sender-role code.
Future<String> _createSendCode(WidgetTester tester) async {
  await tester.tap(find.byKey(const ValueKey('home_send_button')));
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

/// X4: once the peer's join establishes the connection, the detail page
/// AUTO-OPENS with the preselected payload and the send starts by
/// itself (审核 §4.3: no pick sheet, no send button). Waits for the
/// page; the awaiting-consent watch below observes the send.
Future<void> _waitForAutoOpen(WidgetTester tester) async {
  final ok = await _waitFor(
      tester,
      () => tester.any(find.byKey(const ValueKey('conn_detail_page'))),
      const Duration(seconds: 170));
  expect(ok, isTrue, reason: 'detail page did not auto-open (X4)');
  _log('AUTO_OPEN|detail page (X4)');
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
    // K 复核 K-R5: guard the EFFECTIVE endpoints, not the compiled ones —
    // the app reads the runtime environment BEFORE the compiled defines,
    // so asserting the compiled locals alone let an inherited non-local
    // runtime AK_REGISTRY_ADDR/AK_STUN_ADDR/AK_REGISTRY_ID silently
    // redirect the app while the guard still passed. Any finding fails
    // the run before the app boots or the CLI is spawned.
    final findings = k1LocalEndpointFindings(_effective);
    expect(findings, isEmpty,
        reason: 'K 复核 K-R5: 生效端点非本地：${findings.join('；')}');
    expect(_work, isNotEmpty, reason: 'AK_K1_WORK required');
    expect(_shots, isNotEmpty, reason: 'AK_K1_SHOTS required');
    expect(Platform.environment['AK_DIAG_STAGE'] ?? '', isEmpty,
        reason: 'this walkthrough requires the full production bootstrap');
    for (final key in ['AK_ID_DIR', 'AK_FFI_DLL', 'AK_REGISTRY_ID']) {
      expect(Platform.environment[key], isNotEmpty, reason: '$key required');
    }

    // K1R self-contained fixtures (2026-11-14 rerun finding): the picker
    // hands the engine paths under AK_K1_WORK, but nothing guaranteed the
    // files EXISTED — an externally pre-created fixture was silently
    // assumed, and a wiped work dir turned every candidate's transfer
    // into "storage error: path not found" while pairing/probing all
    // stayed green. Create the deterministic synthetic files here so the
    // run is reproducible from an empty work dir. No user data.
    final synth = <String, int>{
      'synthetic_k1_200mib.bin': 200 * 1024 * 1024,
      'synthetic_k1_small.bin': 5 * 1024 * 1024,
    };
    final chunk = Uint8List(1024 * 1024);
    for (var i = 0; i < chunk.length; i++) {
      chunk[i] = (i * 31 + (i >> 8) * 17 + 11) & 255;
    }
    for (final entry in synth.entries) {
      final f = File('$_work/${entry.key}');
      if (!f.existsSync() || f.lengthSync() != entry.value) {
        final sink = f.openWrite();
        for (var written = 0;
            written < entry.value;
            written += chunk.length) {
          sink.add(chunk);
        }
        await sink.flush();
        await sink.close();
        _log('SYNTH_CREATED|${entry.key}|${entry.value} bytes');
      }
    }

    final picker = K1Picker();
    // K 复核 K-R5「将同一份已验证配置传给应用和 CLI」: the verified
    // effective values ride the bootstrap's environment seam (the invite
    // plane's WINNING tier), so the app dials exactly what the guard
    // above asserted — the CLI peer gets the same strings. Every other
    // process variable (AK_FFI_DLL, AK_ID_DIR, …) passes through the
    // copy unchanged, keeping the production bootstrap intact.
    final verifiedEnvironment = Map<String, String>.of(Platform.environment)
      ..['AK_REGISTRY_ADDR'] = _effective.registryAddr!
      ..['AK_REGISTRY_ID'] = _effective.registryIdHex!
      ..['AK_STUN_ADDR'] = _effective.stunAddr!;
    final bootstrap =
        app.bootstrapAppService(environment: () => verifiedEnvironment);
    _log('bootstrap: service=${bootstrap.service != null} '
        'error=${bootstrap.error ?? '-'}');
    expect(bootstrap.service, isNotNull,
        reason: bootstrap.error ?? 'bootstrap failed');
    await tester.pumpWidget(RepaintBoundary(
      key: const ValueKey('k1_shot_boundary'),
      child: AkSendingApp(service: bootstrap.service!, picker: picker),
    ));
    final home = await _waitFor(tester,
        () => tester.any(find.byKey(const ValueKey('home_send_button'))),
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
      // X4: the staged files ride the home send entry's pick; once the
      // CLI joins, the app sends them AUTOMATICALLY.
      picker.files = [_file('synthetic_k1_200mib.bin', 200 * 1024 * 1024)];
      final code = await _createSendCode(tester);
      _log('CODE|$mode|$code');
      final out = mode == 'light' ? 'recv-light' : 'recv-dark';
      final cli = await _spawnHeldCli(code, out);
      await _waitForAutoOpen(tester);

      // K1R diagnostics (2026-11-14 rerun): two light rounds timed out
      // here with the CLI fully paired/probed but no app-side state ever
      // visible; log every status-text change so the evidence shows where
      // the app-side send actually went (connecting → … → failure).
      var lastStatus = '';
      var lastResult = '';
      final awaiting = await _waitFor(
          tester,
          () {
            final status = _textOf(
                    tester, const ValueKey('conn_send_progress_status')) ??
                '';
            if (status != lastStatus) {
              lastStatus = status;
              _log('SEND_STATUS|$mode|status=$status');
            }
            final result =
                _textOf(tester, const ValueKey('conn_send_result')) ?? '';
            if (result != lastResult) {
              lastResult = result;
              _log('SEND_RESULT|$mode|$result');
            }
            final error =
                _textOf(tester, const ValueKey('conn_send_error')) ?? '';
            if (error.isNotEmpty && error != lastResult) {
              _log('SEND_ERROR|$mode|$error');
            }
            return status.contains('等待对方点「接受」');
          },
          // Same-machine multi-path hole punching occasionally needs far
          // longer than the first pairing (observed: light round connected
          // in ~9s, the next pairing was still exchanging probes at 60s).
          const Duration(seconds: 150));
      // K1R diagnostics: the engine's own connect/probe/disconnect facts
      // (recorded at event time) — names the disconnect/handshake reason
      // when the wait above fails instead of leaving an empty status.
      for (final line in ConnectionDiagnosticsLog.shared.lines) {
        _log('DIAG|$mode|$line');
      }
      expect(awaiting, isTrue, reason: 'awaiting-consent status never showed');
      await tester.pump(const Duration(milliseconds: 300));
      await _shot(tester, 'win-$mode-01-awaiting-consent');
      _log('STATE|$mode|awaiting-consent|${_textOf(tester, const ValueKey('conn_send_progress_status'))}');
      await cli.consent('y');

      final coverage = K1ShotCoverage();
      final transferDeadline = DateTime.now().add(const Duration(seconds: 240));
      while (DateTime.now().isBefore(transferDeadline)) {
        await tester.pump(const Duration(milliseconds: 120));
        final status =
            _textOf(tester, const ValueKey('conn_send_progress_status')) ?? '';
        final rate = _textOf(tester, const ValueKey('conn_send_progress_rate')) ?? '';
        if (status.contains('%') && rate.isNotEmpty && !coverage.hasTransferring) {
          // A percent line with a live average-speed line: the readability
          // state the J1 walkthrough could not capture.
          await _shot(tester, 'win-$mode-02-transferring');
          _log('STATE|$mode|transferring|$status|$rate');
          coverage.recordTransferringShot();
        }
        if (status.contains('等待对方校验') && coverage.hasTransferring) {
          await _shot(tester, 'win-$mode-03-verifying');
          _log('STATE|$mode|verifying|$status');
          coverage.recordVerifyingShot();
          break;
        }
        if (tester.any(find.byKey(const ValueKey('conn_send_result')))) break;
      }
      // K 复核 K-R4: the round is only complete when BOTH sampled states
      // were actually captured. The pre-fix flag was initialized false and
      // reset after the verifying shot, so a pass that sampled neither
      // state (straight to the result card) used to satisfy a flag
      // assertion here; the counters cannot reset, and any missing state
      // is named and FAILS the run before it may claim the shot set.
      final missingShots = coverage.missingStates();
      expect(missingShots, isEmpty,
          reason: 'K 复核 K-R4: 未采到状态：${missingShots.join('、')}'
              '（transferring=${coverage.transferringShots}，'
              'verifying=${coverage.verifyingShots}）');
      _log('COVERAGE|$mode|transferring=${coverage.transferringShots}'
          '|verifying=${coverage.verifyingShots}');

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
      picker.files = [_file('synthetic_k1_small.bin', 5 * 1024 * 1024)];
      final code2 = await _createSendCode(tester);
      _log('CODE|$mode|$code2 (decline round)');
      final out2 = mode == 'light' ? 'recv-light-decline' : 'recv-dark-decline';
      final cli2 = await _spawnHeldCli(code2, out2);
      await _waitForAutoOpen(tester);
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
      // K 复核 K-R4: the round's DONE claim below is only made after all
      // six exported PNGs (the two sampled ones included) exist non-empty
      // on disk — the counters prove the states were reached, this proves
      // the files arrived.
      _requireRoundShotFiles(mode);
      _log('ROUND_DONE|$mode');
    }
    tester.platformDispatcher.clearPlatformBrightnessTestValue();
    await _evidenceSink?.flush();
    _log('DONE|$_theme round: 6 in-process raster shots exported');
  }, timeout: const Timeout(Duration(minutes: 8)));
}
