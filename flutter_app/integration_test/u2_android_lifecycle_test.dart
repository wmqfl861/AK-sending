// U2 (batch U, 2026-11-21, handoff §2.4): Android background & lifecycle
// WALKTHROUGH runner. Emulator + `flutter test` DEBUG build against the
// same host — NOT a physical-device verification, and release-build
// behavior may differ (write both into any §8 conclusion).
//
// Division of labor: this app-side runner SENDS one synthetic file; the
// host runs `ak-cli registry-server` / `ak-cli stun-server` (U1 handoff §5
// "准备") and joins with `ak-cli --id-dir <dir> task join-recv <code>
// --out <dir> --server 127.0.0.1:17200 --registry-fp <fp>
// --stun 127.0.0.1:17101`, then agrees to receive. The lifecycle events —
// L1 `input keyevent KEYCODE_HOME` + `am start` back, L2 `KEYCODE_POWER`
// + `KEYCODE_WAKEUP` + `wm dismiss-keyguard`, L3 `am kill`, L4
// `am force-stop` — are executed by the HOST via adb at its own timing
// (L1/L2 around 20% progress, L3 backgrounded +10 s, L4 mid-transfer in
// the foreground). This runner never runs adb and never touches product
// code; it boots the REAL production stack (bootstrapAppService →
// FfiSimpleTransferService → ak_task_* on the bundled libak_ffi.so) and
// drives the real pages exactly like j1_android_u1_aux_test.dart.
//
// dart-defines (compile-time, on the `flutter test` command):
//   AK_REGISTRY_ADDR       default 10.0.2.2:17200 (local registry-server)
//   AK_STUN_ADDR           default 10.0.2.2:17101 (local stun-server)
//   AK_REGISTRY_ID         REQUIRED, 64-hex fingerprint of that registry.
//     The resolved values are re-injected through bootstrapAppService's
//     `environment` seam (env tier > --dart-define tier), so a define the
//     operator forgot can NEVER fall through to the product's compiled
//     defaults — AK_STUN_ADDR's production default is the deployed STUN
//     server, which this batch must not touch (red line).
//   AK_U2_KEEP_CHECKPOINT  "1" = KEEP the sender's checkpoint dir when the
//     send starts (L3/L4 SECOND segment: rerun, same file, resume-or-not
//     is the observation); anything else = clear it, the fresh-send
//     default of the base runner.
//   AK_U2_MIB              payload size in MiB, default 200.
//   AK_U2_CASE             optional evidence tag for log parsing (L1, L2,
//     L3-seg1, L3-seg2, ...); default "unnamed".
//
// Red lines honored in every printed line: task/pairing codes are NEVER
// printed. The one-time task code is handed to the host through a file in
// the app's own sandbox — `adb exec-out run-as dev.ak.ak_sending_app cat
// files/u2_lifecycle/task_code.txt` (uiautomator cannot read
// Flutter-rendered text, see k1_android_saf_test.dart's header) — and the
// file is deleted the moment the peer's connection arrives. Fingerprints,
// job ids and connection ids print as their first 8 hex chars only; error
// text passes through _redact() for the same reason. The full source
// SHA-256 IS printed (synthetic content digest, needed for the host's
// received-file comparison).
//
// Payload: one file at a FIXED path under getApplicationSupportDirectory()
// (<files>/u2_lifecycle/u2-payload.bin). A file that already exists with
// exactly the expected size is NEVER rewritten — the engine's job id is
// blake3(root || per-file rel_path/size/MTIME/digests), so a rewrite (new
// mtime) would mint a different job and break the L3/L4 second segment's
// resume. Generation is deterministic ((i*31 + (i>>8)*17 + block*7 + 11)
// & 255 per byte, per 1 MiB block) into a .tmp sibling that is renamed
// over the target only when complete.
//
// Background discipline (handoff runner requirement 1): from the send tap
// to the end of the run this runner performs NO tester.pump, no tap, no
// widget read — the transfer is observed purely from the service layer
// (transferProgress / connections / engineHealth / engine.doneEvents) and
// every wait is Future.delayed. A pump while the activity is hidden waits
// for a frame the engine will not deliver (no vsync) and blocks forever.
// The earlier setup pumps all go through _pump(), which degrades to a
// real-time wait whenever the app is not AppLifecycleState.resumed, so
// even a host that backgrounds the app early cannot wedge the runner.
//
// Verdicts never fail the test: a transfer that stalls or fails in the
// background, or restarts from zero in the second segment, is exactly the
// data this walkthrough collects (handoff: 走查记录实际行为). Only SETUP
// problems fail the test (bad defines, non-local endpoints, no engine,
// the host CLI never joining). The run always ends with ONE
// machine-judgeable SUMMARY line (printed from finally, so an abort
// cannot lose it): keepCheckpoint, sendCompleted, source bytes/sha256,
// transfer-resume suspicion (first byte-carrying snapshot ≥ 16 MiB — a
// resumed send's first chunk line jumps straight to the receiver's
// verified prefix; cross-check with the CHECKPOINTS lines and the host
// CLI's own "resuming after N verified chunks"), lifecycle counters and
// the byte movement measured across the background window.
//
// Caveat: after the test body returns, the flutter_test binding pumps one
// frame of its own teardown; with the app still hidden that pump blocks
// and `flutter test` hangs. The runner therefore waits (bounded, 3 min)
// for the foreground before returning; if the wait expires still hidden
// the SUMMARY line is already out and the host should stop `flutter test`
// by hand (Ctrl-C) and record that.
import 'dart:async' show StreamSubscription;
import 'dart:io' show Directory, File, FileMode, Platform;
import 'dart:typed_data' show Uint8List;

import 'package:ak_sending_app/main.dart' as app;
import 'package:ak_sending_app/services/ffi_simple_transfer_service.dart'
    show FfiSimpleTransferService;
import 'package:ak_sending_app/ui/app.dart';
import 'package:ak_sending_app/ui/service/file_pick_gateway.dart';
import 'package:ak_sending_app/ui/service/simple_transfer_service.dart'
    show TaskRole, TransferPhase, TransferProgress;
import 'package:crypto/crypto.dart' show sha256;
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:path_provider/path_provider.dart'
    show getApplicationSupportDirectory;

// -- compiled configuration --------------------------------------------------

const String _registryAddr =
    String.fromEnvironment('AK_REGISTRY_ADDR', defaultValue: '10.0.2.2:17200');
const String _stunAddr =
    String.fromEnvironment('AK_STUN_ADDR', defaultValue: '10.0.2.2:17101');
const String _registryIdHex = String.fromEnvironment('AK_REGISTRY_ID');
const bool _keepCheckpoint =
    String.fromEnvironment('AK_U2_KEEP_CHECKPOINT') == '1';
const int _mib = int.fromEnvironment('AK_U2_MIB', defaultValue: 200);
const String _caseTag =
    String.fromEnvironment('AK_U2_CASE', defaultValue: 'unnamed');

const String _workDirName = 'u2_lifecycle';
const String _payloadName = 'u2-payload.bin';
const String _appId = 'dev.ak.ak_sending_app';

/// Phase-2 budget: transfer + 60 s background + slack (the engine's own
/// send stall budget is 150 s, so a backgrounded-but-alive transfer must
/// settle well inside this).
const Duration _watchBudget = Duration(minutes: 10);

/// How long to wait for the foreground before returning (see the header's
/// teardown caveat).
const Duration _foregroundWait = Duration(minutes: 3);

/// A resumed send's first byte-carrying snapshot is at least the
/// receiver's verified prefix; a fresh one starts at the first 1 MiB
/// chunk (snapshots fold a few hundred ms of chunk lines at most).
const int _resumeSuspectBytes = 16 * 1024 * 1024;

const int _progressEveryMs = 3000;

void evidence(String value) {
  // ignore: avoid_print
  print('U2ANDROID|case=$_caseTag|$value');
}

String _wall() => DateTime.now().toUtc().toIso8601String();

String _sec(int ms) => (ms / 1000).toStringAsFixed(1);

String _pct(TransferProgress? snapshot) {
  final snap = snapshot;
  final total = snap?.totalBytes;
  if (snap == null || total == null || total <= 0) {
    return '-';
  }
  return (snap.transferredBytes * 100 / total).toStringAsFixed(1);
}

String _mbps(double? bytesPerSecond) => bytesPerSecond == null
    ? '-'
    : '${(bytesPerSecond / (1024 * 1024)).toStringAsFixed(2)}MiB/s';

bool _isLocalEndpoint(String addr) {
  if (addr.startsWith('[::1]')) {
    return true;
  }
  final host = addr.split(':').first;
  return host == '10.0.2.2' || host == '127.0.0.1' || host == 'localhost';
}

final RegExp _codeShape = RegExp(r'AKI1[0-9A-Za-z-]{8,}');
final RegExp _longHex = RegExp(r'[0-9a-fA-F]{16,}');

/// Masks task-code-shaped text and truncates identity-shaped hex (device
/// fingerprints, job/conn/record ids) to 8 chars before it can reach a
/// log line that ends up in the repository.
String _redact(String value) => value
    .replaceAllMapped(_codeShape, (m) => '<task-code-${m[0]!.length}>')
    .replaceAllMapped(_longHex, (m) => '${m[0]!.substring(0, 8)}..');

// -- the synthetic picker seam (same as the J1 base runner) ------------------

final class _SyntheticPicker implements FilePickGateway {
  _SyntheticPicker(this.file);

  final File file;

  @override
  Future<List<PickedEntry>> pickFiles() async => <PickedEntry>[
        PickedEntry(
          path: file.path,
          name: _payloadName,
          isDirectory: false,
          sizeBytes: await file.length(),
        ),
      ];

  @override
  Future<PickedEntry?> pickDirectory() async => null;
}

// -- lifecycle trace ---------------------------------------------------------

/// Pumps may only ever be issued while [state] is `resumed`: a hidden
/// activity gets no vsync, so a pending pump would block until the app
/// comes back (and the teardown pump after the test body has exactly that
/// shape — see the header's caveat).
final class _LifecycleTrace with WidgetsBindingObserver {
  _LifecycleTrace(this._onTransition);

  final void Function(AppLifecycleState state) _onTransition;

  AppLifecycleState state = AppLifecycleState.resumed;

  bool get pumpable => state == AppLifecycleState.resumed;

  @override
  void didChangeAppLifecycleState(AppLifecycleState next) {
    state = next;
    _onTransition(next);
  }
}

/// The only pump call site. Foreground: a guarded pump like the base
/// runner's. Not resumed (hidden/paused/inactive — e.g. the host already
/// pressed HOME or the keyguard is up): degrade to a real-time wait and
/// let the next iteration re-check.
Future<void> _pump(
  WidgetTester tester,
  _LifecycleTrace lifecycle,
  Duration duration,
) async {
  if (lifecycle.pumpable) {
    await tester.pump(duration).timeout(const Duration(seconds: 45));
  } else {
    await Future<void>.delayed(duration);
  }
}

/// The base runner's wait loop, on top of [_pump]: check first, pump only
/// while unsatisfied, bounded by [seconds].
Future<void> _until(
  WidgetTester tester,
  _LifecycleTrace lifecycle,
  bool Function() ready,
  String what, {
  int seconds = 150,
}) async {
  final end = DateTime.now().add(Duration(seconds: seconds));
  while (!ready() && DateTime.now().isBefore(end)) {
    await _pump(tester, lifecycle, const Duration(milliseconds: 400));
  }
  expect(ready(), isTrue, reason: what);
}

/// `.data` (or descendant Texts) of the first widget with [key]; '' when
/// absent. Read-only — never pumps, safe once the app is resumed again.
String textAt(WidgetTester tester, String key) {
  final finder = find.byKey(ValueKey(key));
  if (finder.evaluate().isEmpty) {
    return '';
  }
  final widget = finder.evaluate().first.widget;
  if (widget is Text) {
    return widget.data ?? widget.textSpan?.toPlainText() ?? '';
  }
  return find
      .descendant(of: finder, matching: find.byType(Text))
      .evaluate()
      .map((e) => e.widget as Text)
      .map((t) => t.data ?? t.textSpan?.toPlainText() ?? '')
      .join(' / ');
}

// -- payload -----------------------------------------------------------------

/// Ensures the payload exists at [file] with exactly `_mib` MiB and
/// returns its facts. REUSE rule: an existing file of the exact expected
/// size is left untouched (job id covers mtime — see the header); only a
/// missing/short/long file is regenerated, via a `.tmp` sibling that is
/// renamed into place once complete. `sha256` is recomputed every run so
/// the host can always compare the received file against THIS run's line.
Future<({bool reused, String sha256, String mtime})> _ensurePayload(
    File file) async {
  final expected = _mib * 1024 * 1024;
  var reused = true;
  if (!file.existsSync() || file.lengthSync() != expected) {
    reused = false;
    final tmp = File('${file.path}.tmp');
    final sink = await tmp.open(mode: FileMode.write);
    final block = Uint8List(1024 * 1024);
    try {
      for (var b = 0; b < _mib; b++) {
        for (var i = 0; i < block.length; i++) {
          block[i] = (i * 31 + (i >> 8) * 17 + b * 7 + 11) & 255;
        }
        await sink.writeFrom(block);
      }
    } finally {
      await sink.close();
    }
    await tmp.rename(file.path);
  }
  final digest = await sha256.bind(file.openRead()).first;
  final stat = await file.stat();
  return (
    reused: reused,
    sha256: digest.toString(),
    mtime: stat.modified.toUtc().toIso8601String(),
  );
}

// -- sender checkpoints ------------------------------------------------------

/// One redacted summary per parsable `*.akjob` record under [dir] (the
/// AKJOB1 text format of ak-store's FileCheckpointStore): job/peer ids
/// truncated to 8 hex, the `root` path hex omitted, the per-file
/// `index/size/verified-prefix-chunks` lines kept — the verified prefix
/// is how far the FIRST segment got as far as the sender's checkpoint
/// knew, the cross-check for the second segment's resume question.
List<String> checkpointRecords(Directory dir) {
  if (!dir.existsSync()) {
    return const <String>[];
  }
  final out = <String>[];
  for (final entity in dir.listSync(followLinks: false)) {
    if (entity is! File || !entity.path.endsWith('.akjob')) {
      continue;
    }
    try {
      final fields = <String, String>{};
      final files = <String>[];
      for (final line in entity.readAsStringSync().split('\n')) {
        if (line.isEmpty) {
          continue;
        }
        if (line.startsWith('file ')) {
          files.add(line.substring(5));
          continue;
        }
        final space = line.indexOf(' ');
        if (space <= 0) {
          continue;
        }
        fields[line.substring(0, space)] = line.substring(space + 1);
      }
      String eight(String value) =>
          value.length >= 8 ? value.substring(0, 8) : value;
      out.add('job=${eight(fields['job'] ?? '?')}'
          ',dir=${fields['dir'] ?? '?'}'
          ',peer=${fields['peer'] == '-' ? '-' : eight(fields['peer'] ?? '?')}'
          ',state=${fields['state'] ?? '?'}'
          ',updated=${fields['updated'] ?? '?'}'
          ',file=${files.isEmpty ? '-' : files.join(';')}');
    } on Object {
      // A record mid-rewrite (tmp+rename makes this near-impossible) or a
      // foreign file: skip it, never fail the walkthrough on evidence IO.
      out.add('unreadable:.akjob');
    }
  }
  out.sort();
  return out;
}

/// The U2 checkpoint switch (handoff runner requirement 2), applied
/// immediately before the send tap — equivalent to the base runner's
/// pre-send delete because nothing opens the checkpoint store before the
/// engine's send actually starts (`prepare_send` is the first touch).
/// Returns the record count that existed before the switch.
int applyCheckpointSwitch(Directory checkpoints) {
  final before = checkpointRecords(checkpoints);
  final detail = before.isEmpty ? '' : '|${before.join('|')}';
  if (_keepCheckpoint) {
    evidence('CHECKPOINTS|kept|before-send|records=${before.length}$detail');
  } else if (checkpoints.existsSync()) {
    checkpoints.deleteSync(recursive: true);
    evidence('CHECKPOINTS|cleared|before-send|records=${before.length}'
        '|cleared-list=$detail');
  } else {
    evidence('CHECKPOINTS|cleared|before-send|records=0|dir-absent');
  }
  return before.length;
}

// -- the walkthrough ---------------------------------------------------------

void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  binding.framePolicy = LiveTestWidgetsFlutterBindingFramePolicy.fullyLive;
  testWidgets('U2 Android background/lifecycle walkthrough ($_mib MiB)',
      (tester) async {
    // Walkthrough state — everything the finally-block SUMMARY needs is
    // declared here so an abort anywhere still prints the full line.
    final clock = Stopwatch()..start();
    var sendStarted = false;
    var sendCompleted = false;
    var ended = 'not-started';
    var sourceReused = false;
    var sourceSha256 = '';
    var firstNonZeroBytes = -1;
    var checkpointRecordsBefore = -1;
    var checkpointRecordsAfter = -1;
    var lifecycleTransitions = 0;
    var wentBackground = false;
    var lifecycleResumedAgain = false;
    var bytesAtBackground = -1;
    var bytesAtForegroundAgain = -1;
    var maxStallMs = 0;
    var resultUi = '';
    var phaseName = '';
    var connId = '';
    var rowSeen = false;
    var rowGone = false;
    var doneSeen = false;
    var engineFailure = '';
    TransferProgress? latest;
    int? lastAdvanceMs;
    final subs = <StreamSubscription<void>>[];
    _LifecycleTrace? trace;
    File? codeFile;
    try {
      evidence('CONFIG|registry=$_registryAddr|stun=$_stunAddr'
          '|registryId8=${_registryIdHex.length >= 8 ? _registryIdHex.substring(0, 8) : '-'}'
          '|keepCheckpoint=$_keepCheckpoint|mib=$_mib|bytes=${_mib * 1024 * 1024}'
          '|debug-build|emulator-same-host|not-physical-device');
      expect(RegExp(r'^[0-9a-fA-F]{64}$').hasMatch(_registryIdHex), isTrue,
          reason: 'AK_REGISTRY_ID is required: pass the local '
              'registry-server fingerprint (64 hex) via --dart-define');
      expect(_isLocalEndpoint(_registryAddr), isTrue,
          reason: 'AK_REGISTRY_ADDR must be the emulator-host loopback '
              '(10.0.2.2/127.0.0.1), never a production endpoint');
      expect(_isLocalEndpoint(_stunAddr), isTrue,
          reason: 'AK_STUN_ADDR must be the emulator-host loopback; the '
              'product default is the production STUN server (red line)');
      expect(_mib >= 1 && _mib <= 4096, isTrue,
          reason: 'AK_U2_MIB must be between 1 and 4096');

      final support = await getApplicationSupportDirectory();
      final work = Directory('${support.path}/$_workDirName');
      await work.create(recursive: true);
      final file = File('${work.path}/$_payloadName');
      final identity = Directory('${work.path}/identity');
      final checkpoints = Directory('${identity.path}/checkpoints');
      final codePath = File('${work.path}/task_code.txt');
      codeFile = codePath;

      final source = await _ensurePayload(file);
      sourceReused = source.reused;
      sourceSha256 = source.sha256;
      evidence('SOURCE|${source.reused ? 'reused' : 'generated'}'
          '|bytes=${_mib * 1024 * 1024}'
          '|sha256=${source.sha256}|mtime=${source.mtime}'
          '|pattern=(i*31+(i>>8)*17+block*7+11)&255;1MiB-blocks');

      // Real production bootstrap (J1 base path) with the resolved local
      // endpoints re-injected at the env tier — the only way a DEFAULT
      // AK_STUN_ADDR/AK_REGISTRY_ADDR can exist without ever exposing the
      // product's compiled production fallbacks.
      final bootstrap = app.bootstrapAppService(
        environment: () => <String, String>{
          ...Platform.environment,
          'AK_REGISTRY_ADDR': _registryAddr,
          'AK_REGISTRY_ID': _registryIdHex,
          'AK_STUN_ADDR': _stunAddr,
        },
        androidIdentityDir: () async => identity.path,
      );
      evidence('BOOTSTRAP|service=${bootstrap.service != null}'
          '|error=${bootstrap.error}');
      expect(bootstrap.service, isNotNull, reason: bootstrap.error);
      final service = bootstrap.service!;

      // Lifecycle evidence + the pump gate.
      final lifecycle = _LifecycleTrace((next) {
        lifecycleTransitions++;
        final bytes = latest?.transferredBytes ?? -1;
        if ((next == AppLifecycleState.hidden ||
                next == AppLifecycleState.paused) &&
            !wentBackground) {
          wentBackground = true;
          bytesAtBackground = bytes;
        }
        if (next == AppLifecycleState.resumed &&
            wentBackground &&
            !lifecycleResumedAgain) {
          lifecycleResumedAgain = true;
          bytesAtForegroundAgain = bytes;
        }
        evidence('LIFECYCLE|state=${next.name}'
            '|t=${_sec(clock.elapsedMilliseconds)}s|wall=${_wall()}'
            '|bytes=$bytes');
      });
      trace = lifecycle;
      WidgetsBinding.instance.addObserver(lifecycle);

      // Phase-2 observers: the SERVICE layer only. Subscribed before the
      // UI drive so no early event is missed (broadcast, no replay).
      subs.add(service.transferProgress.listen((p) {
        if (connId.isNotEmpty && p.connId != connId) {
          return;
        }
        final previous = latest?.transferredBytes ?? 0;
        final nowMs = clock.elapsedMilliseconds;
        latest = p;
        if (p.phase.name != phaseName) {
          phaseName = p.phase.name;
          evidence('PHASE|$phaseName'
              '|t=${_sec(nowMs)}s|wall=${_wall()}'
              '|bytes=${p.transferredBytes}|total=${p.totalBytes ?? -1}');
        }
        if (p.transferredBytes > 0 && firstNonZeroBytes < 0) {
          firstNonZeroBytes = p.transferredBytes;
          evidence('FIRST_BYTES|bytes=${p.transferredBytes}'
              '|t=${_sec(nowMs)}s'
              '|resumeSuspect=${p.transferredBytes >= _resumeSuspectBytes}');
        }
        if (p.transferredBytes > previous) {
          lastAdvanceMs = nowMs;
        }
      }));
      subs.add(service.connections.listen((rows) {
        if (connId.isEmpty) {
          for (final row in rows) {
            if (row.role == TaskRole.send) {
              connId = row.id;
              rowSeen = true;
              evidence('CONN|id8=${connId.substring(0, 8)}|role=send');
              break;
            }
          }
          return;
        }
        var present = false;
        for (final row in rows) {
          if (row.id == connId) {
            present = true;
            break;
          }
        }
        if (present) {
          rowSeen = true;
        } else if (rowSeen) {
          rowGone = true; // terminal: filtered out of the live snapshot
        }
      }));
      subs.add(service.engineHealth.listen((health) {
        if (health.isEngineFailed && engineFailure.isEmpty) {
          engineFailure = _redact(health.failureReason ?? 'engine failed');
          evidence('ENGINE_FAILED|t=${_sec(clock.elapsedMilliseconds)}s'
              '|reason=$engineFailure');
        }
      }));
      // The engine service's done event is the contract-independent
      // completion fact (a fast send can finish without a `completed`
      // snapshot ever being folded for a tracker-less connection).
      if (service is FfiSimpleTransferService) {
        subs.add(service.engine.doneEvents.listen((event) {
          final conn = event.conn;
          if (conn == null || conn != connId) {
            return;
          }
          doneSeen = true;
          evidence('DONE_EVENT|t=${_sec(clock.elapsedMilliseconds)}s'
              '|wall=${_wall()}|files=${event.files}|bytes=${event.bytes}');
        }));
      }

      // -- phase 1: the real UI drive (foreground, guarded pumps) --------
      final boot = DateTime.now().add(const Duration(seconds: 15));
      while (!lifecycle.pumpable && DateTime.now().isBefore(boot)) {
        await Future<void>.delayed(const Duration(milliseconds: 500));
      }
      await tester
          .pumpWidget(
              AkSendingApp(service: service, picker: _SyntheticPicker(file)))
          .timeout(const Duration(seconds: 120));
      await _until(
          tester,
          lifecycle,
          () => find
              .byKey(const ValueKey('home_create_code_button'))
              .evaluate()
              .isNotEmpty,
          'home');
      final homeButton = find.byKey(const ValueKey('home_create_code_button'));
      await tester.ensureVisible(homeButton);
      await tester.tap(homeButton);
      await _pump(tester, lifecycle, const Duration(milliseconds: 700));
      await tester.tap(find.byKey(const ValueKey('role_send_button')));
      await _until(
          tester,
          lifecycle,
          () => find
              .byKey(const ValueKey('flow_pairing_code'))
              .evaluate()
              .isNotEmpty,
          'real task code');
      final codeWidget = tester.widget<SelectableText>(
          find.byKey(const ValueKey('flow_pairing_code')));
      final code = codeWidget.data ?? codeWidget.textSpan?.toPlainText() ?? '';
      expect(code.startsWith('AKI1-'), isTrue,
          reason: 'task code never rendered');
      await codePath.writeAsString(code, flush: true);
      evidence('TASK_CODE_FILE|written|len=${code.length}|never-printed'
          '|host: adb exec-out run-as $_appId cat '
          'files/$_workDirName/task_code.txt');

      final connection = find.byWidgetPredicate(
          (w) => w.key.toString().contains('conn_card_name_'));
      await _until(tester, lifecycle, () => connection.evaluate().isNotEmpty,
          'CLI receiver connection',
          seconds: 170);
      expect(connId, isNotEmpty, reason: 'send-role row never observed');
      if (codePath.existsSync()) {
        codePath.deleteSync(); // consumed by the join; dies with the code
        evidence('TASK_CODE_FILE|consumed-and-deleted');
      }
      await tester.ensureVisible(connection.first);
      await tester.tap(connection.first);
      await _pump(tester, lifecycle, const Duration(milliseconds: 800));
      expect(find.byKey(const ValueKey('conn_detail_page')), findsOneWidget);
      await tester.tap(find.byKey(const ValueKey('conn_pick_files_button')));
      await _pump(tester, lifecycle, const Duration(milliseconds: 600));
      await tester.tap(find.byKey(const ValueKey('pick_files_option')));
      await _until(tester, lifecycle,
          () => textAt(tester, 'picked_count') == '已选 1 项', 'picked entry');
      await tester
          .ensureVisible(find.byKey(const ValueKey('conn_send_button')));

      // The checkpoint switch, immediately before the send.
      checkpointRecordsBefore = applyCheckpointSwitch(checkpoints);

      sendStarted = true;
      evidence('SEND_TAPPED|t=${_sec(clock.elapsedMilliseconds)}s'
          '|wall=${_wall()}|bytes=${_mib * 1024 * 1024}'
          '|no-pumps-from-here');
      await tester.tap(find.byKey(const ValueKey('conn_send_button')));

      // -- phase 2: service-layer watch — Future.delayed only, NO pumps --
      evidence('WATCH|started|budget=${_watchBudget.inSeconds}s');
      final watchStartMs = clock.elapsedMilliseconds;
      var lastPrintMs = watchStartMs;
      while (true) {
        await Future<void>.delayed(const Duration(milliseconds: 400));
        final nowMs = clock.elapsedMilliseconds;
        final snap = latest;
        if (nowMs - lastPrintMs >= _progressEveryMs) {
          lastPrintMs = nowMs;
          evidence('PROGRESS|t=${_sec(nowMs)}s|wall=${_wall()}'
              '|phase=${snap?.phase.name ?? '-'}'
              '|bytes=${snap?.transferredBytes ?? -1}'
              '|total=${snap?.totalBytes ?? -1}'
              '|pct=${_pct(snap)}'
              '|bps=${_mbps(snap?.bytesPerSecond)}'
              '|life=${lifecycle.state.name}'
              '|sinceAdvanceMs='
              '${lastAdvanceMs == null ? -1 : nowMs - lastAdvanceMs!}');
        }
        if (snap != null &&
            snap.phase == TransferPhase.transferring &&
            lastAdvanceMs != null &&
            nowMs - lastAdvanceMs! > maxStallMs) {
          maxStallMs = nowMs - lastAdvanceMs!;
        }
        if (snap != null && snap.phase == TransferPhase.completed) {
          ended = 'completed';
          break;
        }
        if (doneSeen) {
          ended = 'done-event';
          break;
        }
        if (rowGone) {
          ended = 'row-gone';
          break;
        }
        if (engineFailure.isNotEmpty) {
          ended = 'engine-failed';
          break;
        }
        if (nowMs - watchStartMs > _watchBudget.inMilliseconds) {
          ended = 'watch-deadline';
          break;
        }
      }
      sendCompleted = ended == 'completed' || ended == 'done-event';
      final snap = latest;
      evidence('ENDED|outcome=$ended'
          '|t=${_sec(clock.elapsedMilliseconds)}s|wall=${_wall()}'
          '|phase=${snap?.phase.name ?? '-'}'
          '|bytes=${snap?.transferredBytes ?? -1}'
          '|engineFailure=${engineFailure.isEmpty ? '-' : engineFailure}');

      // -- phase 3: verdict collection ------------------------------------
      if (!lifecycle.pumpable) {
        evidence('WAIT_FOREGROUND|budget=${_foregroundWait.inSeconds}s');
        final end = DateTime.now().add(_foregroundWait);
        while (!lifecycle.pumpable && DateTime.now().isBefore(end)) {
          await Future<void>.delayed(const Duration(seconds: 1));
        }
        evidence('WAIT_FOREGROUND|resumed=${lifecycle.pumpable}');
      }
      if (lifecycle.pumpable) {
        // Let live frames settle after the resume before reading widgets.
        await Future<void>.delayed(const Duration(milliseconds: 1500));
        final ok = textAt(tester, 'conn_send_result');
        final err = textAt(tester, 'conn_send_error');
        resultUi = err.isEmpty ? ok : err;
        final pillSeen =
            find.byKey(const ValueKey('conn_state_ended')).evaluate().isNotEmpty;
        evidence('RESULT_UI|ok=${_redact(ok)}|error=${_redact(err)}'
            '|endedPill=$pillSeen'
            '|role=${_redact(textAt(tester, 'conn_detail_role'))}');
      } else {
        evidence('RESULT_UI|skipped|app-still-hidden'
            '|teardown-pump-may-hang-see-file-header');
      }
      final after = checkpointRecords(checkpoints);
      checkpointRecordsAfter = after.length;
      evidence('CHECKPOINTS|after-send|records=${after.length}'
          '${after.isEmpty ? '' : '|${after.join('|')}'}');
    } finally {
      // The machine-judgeable one-liner; finally so an abort cannot lose
      // it. Walkthrough outcomes (row-gone, deadline, ...) are DATA, not
      // failures — only the propagating exception decides the verdict.
      final bgAdvanceBytes =
          bytesAtBackground >= 0 && bytesAtForegroundAgain >= 0
              ? bytesAtForegroundAgain - bytesAtBackground
              : -1;
      evidence('SUMMARY|keepCheckpoint=$_keepCheckpoint'
          '|sendStarted=$sendStarted|sendCompleted=$sendCompleted'
          '|ended=$ended|sourceBytes=${_mib * 1024 * 1024}'
          '|sourceReused=$sourceReused|sourceSha256=$sourceSha256'
          '|firstNonZeroBytes=$firstNonZeroBytes'
          '|resumeSuspected=${firstNonZeroBytes >= _resumeSuspectBytes}'
          '|lifecycleResumedAgain=$lifecycleResumedAgain'
          '|checkpointRecordsBefore=$checkpointRecordsBefore'
          '|checkpointRecordsAfter=$checkpointRecordsAfter'
          '|lifecycleTransitions=$lifecycleTransitions'
          '|wentBackground=$wentBackground'
          '|bytesAtBackground=$bytesAtBackground'
          '|bytesAtForegroundAgain=$bytesAtForegroundAgain'
          '|bgAdvanceBytes=$bgAdvanceBytes|maxStallMs=$maxStallMs'
          '|watchSecs=${_sec(clock.elapsedMilliseconds)}'
          '|result=${_redact(resultUi)}');
      for (final sub in subs) {
        await sub.cancel();
      }
      final observer = trace;
      if (observer != null) {
        WidgetsBinding.instance.removeObserver(observer);
      }
      final unconsumed = codeFile;
      if (unconsumed != null && unconsumed.existsSync()) {
        unconsumed.deleteSync(); // an unconsumed code dies with the runtime
      }
    }
  }, timeout: const Timeout(Duration(minutes: 20)));
}
