/// W-N11 repro runner (codex P1-5, batch X fix wave, 2026-11-22): the app
/// mints a SENDER code and then WAITS — through a natural screen sleep —
/// for a receiver to join, auto-sends on pairing, and reports what
/// survived. Pure-FUTURE waits only: `tester.pump` stalls the moment the
/// activity pauses (screen off stops rendering; the u2 runner died exactly
/// there in three attempts), while engine events keep arriving on the
/// Dart isolate — observing them is the point of this experiment.
///
/// Orchestration (host side: the orchestration scripts live outside the
/// repo — placeholder `<host-script-dir>/wn11.sh`): screen stays awake until
/// this test's code file appears, then the host sets a 30 s sleep, waits
/// for `mWakefulness=Asleep`, and joins ~60 s later with
/// `ak-cli task join-recv` (the code's COMPLEMENT — a same-direction
/// join-send would be refused for the wrong reason, codex P1-5).
///
/// dart-defines: AK_REGISTRY_ADDR / AK_STUN_ADDR / AK_REGISTRY_ID (local
/// endpoints only), AK_WN11_MIB (payload MiB, default 4). Evidence lines
/// are `WN11|...` — codes are never printed; the payload is synthetic.
library;

import 'dart:io' show Directory, File, FileMode, Platform;

import 'package:ak_sending_app/main.dart' as app;
import 'package:ak_sending_app/ui/service/simple_transfer_service.dart'
    show
        SimpleTransferService,
        TaskCode,
        TaskConnection,
        TaskConnectionState,
        TaskRole;

import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:path_provider/path_provider.dart'
    show getApplicationSupportDirectory;

const int _mib = int.fromEnvironment('AK_WN11_MIB', defaultValue: 4);

void _log(String m) {
  // ignore: avoid_print
  print('WN11|$m');
}

void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  binding.framePolicy = LiveTestWidgetsFlutterBindingFramePolicy.fullyLive;

  testWidgets('W-N11: pairing waits out a natural screen sleep', (tester) async {
    const registryAddr = String.fromEnvironment('AK_REGISTRY_ADDR');
    const stunAddr = String.fromEnvironment('AK_STUN_ADDR');
    const registryId = String.fromEnvironment('AK_REGISTRY_ID');
    expect(registryAddr, '10.0.2.2:17200', reason: 'local registry only');
    expect(stunAddr, '10.0.2.2:17101', reason: 'local STUN only');
    expect(registryId.length, 64, reason: 'registry pin required');

    // No pumpWidget at all: the UI is irrelevant to this observation and
    // would die with the screen. The SERVICE (Dart isolate + native
    // engine threads) is the thing W-N11 asks about.
    final support = await getApplicationSupportDirectory();
    final round = Directory(
        '${support.path}/wn11-${DateTime.now().millisecondsSinceEpoch}');
    round.createSync(recursive: true);
    final identity = Directory('${round.path}/identity')
      ..createSync(recursive: true);
    final bootstrap = app.bootstrapAppService(
      environment: () => <String, String>{
        ...Platform.environment,
        'AK_REGISTRY_ADDR': registryAddr,
        'AK_REGISTRY_ID': registryId,
        'AK_STUN_ADDR': stunAddr,
      },
      androidIdentityDir: () async => identity.path,
    );
    _log('BOOTSTRAP|service=${bootstrap.service != null}'
        '|error=${bootstrap.error}');
    expect(bootstrap.service, isNotNull, reason: bootstrap.error);
    final SimpleTransferService service = bootstrap.service!;
    // The shell's fire-and-forget start() swallows failures
    // (`starting.ignore()` in main.dart) and its retry lives in the UI —
    // which this runner never builds. Drive start() EXPLICITLY so a
    // failure is VISIBLE here instead of an endless not-started loop.
    try {
      await service.start().timeout(const Duration(seconds: 60));
      _log('START|ok');
    } on Object catch (e) {
      _log('START|failed|$e');
    }
    final payload = File('${round.path}/wn11-payload.bin');
    final seed = 0x5A11;
    final block = List<int>.generate(
        1024 * 1024, (i) => (i * 31 + seed) & 255,
        growable: false);
    final w = payload.openSync(mode: FileMode.write);
    for (var i = 0; i < _mib; i++) {
      w.writeFromSync(block);
    }
    w.closeSync();

    // The host hand-off file (app→host contract; machine form). P2-C
    // (third review): a ROUND-UNIQUE path — the shared u2_lifecycle dir
    // let a stale script from another round race this file.
    Directory('${support.path}/wn11_round').createSync(recursive: true);
    final codePath = File('${support.path}/wn11_round/task_code.txt');

    final conns = <String, TaskConnection>{};
    final sub = service.connections.listen((rows) {
      for (final row in rows) {
        conns[row.id] = row;
      }
    });

    // The shell's start() is fire-and-forget (main.dart keeps the future);
    // the UI drive gives it time in every other walkthrough — this runner
    // has no pumps, so wait out the runtime here (bounded 30 s).
    TaskCode? code;
    for (var attempt = 0; ; attempt++) {
      try {
        code = await service.createCode(TaskRole.send);
        break;
      } on StateError catch (e) {
        if (attempt >= 60) rethrow;
        _log('START_WAIT|attempt=$attempt|$e');
        await Future<void>.delayed(const Duration(milliseconds: 500));
      } on Object catch (e) {
        // Registry/TLS hiccups right after a cold emulator boot: retry a
        // few times so a transient dial failure doesn't kill the round
        // (each retry is logged with the raw error for the record).
        if (attempt >= 6) rethrow;
        _log('CODE_RETRY|attempt=$attempt|$e');
        await Future<void>.delayed(const Duration(seconds: 5));
      }
    }
    await codePath.writeAsString(code.code, flush: true);
    _log('CODE_FILE|written|len=${code.code.length}|never-printed');

    // NO pumps from here: wait on the service stream instead. The watch
    // budget (5 min) covers the screen sleep + the host's +60 s join.
    final deadline = DateTime.now().add(const Duration(minutes: 5));
    String? connId;
    var pairedState = 'unknown';
    while (DateTime.now().isBefore(deadline)) {
      await Future<void>.delayed(const Duration(milliseconds: 500));
      for (final row in conns.values) {
        if (row.role == TaskRole.send &&
            row.state != TaskConnectionState.testing) {
          connId = row.id;
          pairedState = 'state=${row.state.name}';
          break;
        }
      }
      if (connId != null) break;
    }
    if (connId == null) {
      _log('RESULT|no-pairing|no sender-direction connection left its probe '
          'within the budget (screen slept through the wait — the pairing '
          'died with the paused/frozen app, or the host join failed)');
      sub.cancel();
      // P2-C (third review): a no-pairing round is a FAILED round — the
      // W-N11 verdict is exactly what this runner exists to observe, so
      // returning normally (a green exit) lied about the outcome.
      fail('W-N11: no pairing landed within the budget — the round FAILED. '
          'Either the screen-off killed the pairing (the bug this runner '
          'guards against), or the host join never completed. See the '
          'WN11|RESULT line above for the detail.');
    }
    _log('PAIRED|$pairedState|conn(8)=${connId.substring(0, 8)}');

    // Auto-send the payload (the X4 main-path behaviour; service-side).
    try {
      final summary = await service.sendFiles(connId, [payload.path]);
      _log('SEND|done|summary=$summary');
    } on Object catch (e) {
      _log('SEND|failed|$e');
      sub.cancel();
      // P2-C: the send was the point of the paired connection — a throw
      // on a PAIRED connection fails the round, never a green exit.
      fail('W-N11: the send threw on a PAIRED connection: $e');
    }
    sub.cancel();
    try {
      round.deleteSync(recursive: true);
    } on Object {
      // best effort cleanup; the dir dies with the app data anyway
    }
    _log('RUNNER_DONE');
  }, timeout: const Timeout(Duration(minutes: 8)));
}
