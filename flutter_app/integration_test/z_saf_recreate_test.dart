// Z SAF recreation round (batch Z, revision 5, T9 — the proof of O16): a
// delivered copy belongs to the stager INSTANCE that created it, and a new
// isolate's start-up cleanup removes the copies of instances that are no
// longer live, whether or not Android reused the process. The copy is made
// through the real send-side path in a profile build — the system tree
// picker (driven from the host) and the native staging behind
// `dev.ak/storage`. The cleanup is the PRODUCTION start hook alone: the
// second isolate pumps the REAL app (AkSendingApp → cleanupStaleStaging())
// and never calls the channel method itself.
//
// Three variants, chosen by dart-define AK_SAF_RECREATE_VARIANT, and two
// phases each:
//
//   recreate (the default when the define is empty)
//     R1  a fresh install: pick nested/deep through the raw channel, stage
//         it under a fresh id, confirm `.owner`, the completion marker and
//         the payload by reading the stage directory, release nothing.
//     R2  the same process after its Activity finished and was started again
//         (the host: BACK on the resumed Activity — the app's root route has
//         nothing to pop, so the Activity finishes and its engine with it —
//         then `am start`; the user's own way of leaving. Not "Don't keep
//         activities": on the Android 15 emulator that setting has no
//         run-time effect, checked 2026-11-25 20:10, and the switch itself
//         would need the Settings UI). Required: the stage directory is gone.
//   cold
//     R1  phase 1, the same.
//     R2  the host STOPS THE PROCESS (`am force-stop`): nothing retires the
//         token, so the delivered copy is still there when the app is
//         started again — a NEW process, whose isolate pumps the real app.
//         Required: the copy is gone, and the start hook answered
//         `removed=<n>` with n >= 1: here the production start hook is what
//         removes the copy.
//   neighbour
//     N1  phase 1, the same as R1.
//     N2  the isolate of a SECOND instance of the Activity, started by the
//         host in another task while the first instance is alive (the host
//         presses HOME first: a singleTop Activity that is the top-most
//         running instance only receives a launch). Required: the first
//         instance's delivered copy is still there while its stager lives,
//         and gone after the host destroyed that instance — the first task
//         to the front, BACK — which retires its token and queues a sweep.
//
// WHO REMOVES THE COPY in each variant (revision 6, T10):
//   recreate  usually the retiring instance's own sweep — the new isolate's
//             start hook then answers `removed=0`; (it answers `removed=1`
//             in the rarer case where that sweep has not run yet: any legal
//             `removed=<n>` passes this variant, the end state is its point);
//   cold      the production start hook — `removed=<n>` with n >= 1;
//   neighbour the second instance's hook leaves the live copy alone
//             (`removed=0`); the copy goes when the first instance retires.
//
// The hook's answer is READ OFF THE TAP and is REQUIRED (it used to be on
// record only): a tap that was not installed before the app was pumped, a
// hook that never ran or never answered, and an answer that is not a legal
// `removed=<n>` are failed checks by name. In `neighbour` the line
// `N2|NEIGHBOUR` — the host's cue to destroy the first instance — is said
// ONLY after the hook answered `removed=0` AND the first instance's copy
// was re-read intact: its completion marker and owner file regular files,
// its payload there, and the note of the size phase 1 established still
// holding the preset text. Without all of that no cue goes out and the host
// fails the round by itself.
//
// A MARKER FILE tells the phases apart: `ak-probe-recreate.txt` in the
// PARENT of the staging root (`<data dir>/no_backup/`), holding the stage
// id. Phase 1 writes it, phase 2 reads it. Never inside the staging root:
// the sweep deletes every non-id name there. The marker lives in the app's
// data, so a fresh install removes it and a round that starts from one
// always opens in phase 1.
//
// Orchestration contract (z-probe-recreate.sh; all local-only endpoints):
//   * the drive runs with --keep-app-running — without it `flutter drive`
//     STOPS AND UNINSTALLS the app when the run ends, and its data with it;
//   * dart-defines AK_REGISTRY_ADDR=10.0.2.2:17200 and
//     AK_STUN_ADDR=10.0.2.2:17101, as the E2E round. Phase 2 pumps the real
//     app and refuses to unless both are these local aliases: with no
//     compiled STUN address the engine would fall back to its production
//     default. No AK_REGISTRY_ID is passed, so the engine never starts and
//     the round reaches no server.
//   * every evidence line `ZSAF|<tag>|PICKER|tree|awaiting external
//     DocumentsUI drive` (logcat, tag flutter) asks the host to drive
//     DocumentsUI onto akstage/nested/deep — the tree the host's build_tree
//     makes; its nested/deep/note.txt is the payload phase 1 looks for;
//   * phase 2 runs in an isolate no driver is attached to, so nothing is
//     reported to `flutter drive`: every fact is an evidence line.
//
// Evidence is one `ZSAF|…` line per fact. A staged path, a tree URI, a
// display name, a stage id and the content of the grant journal never
// print: counts, lengths and booleans stand in. Nor does an exception: a
// call on dart:io that throws — a look, a listing, a length, a read, a
// delete — is caught where it is made and turns into a fixed check name,
// and a failed read is never taken for a copy that is gone. The tap's own
// handler never throws either. Every check is HARD — a failed one is
// collected under its name, the run goes on to gather the rest, and the
// test fails on the collected list.
import 'dart:async' show unawaited;
import 'dart:convert' show utf8;
import 'dart:io' as io;
import 'dart:math' show Random;

import 'package:ak_sending_app/main.dart' as app;
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:path_provider/path_provider.dart'
    show getApplicationSupportDirectory;

const MethodChannel _storageChannel = MethodChannel('dev.ak/storage');

/// The folder of this round — nested/deep, as the kill round's — and the
/// one file the host puts into it.
const String _deepName = 'deep';
const String _noteText = 'ak stage probe\n';

/// The note's size in bytes: the KNOWN SAMPLE. Phase 1 establishes it (the
/// note is a regular file holding exactly the preset text) and phase 2 of
/// `neighbour` requires the first instance's copy to hold a note of exactly
/// this size, with exactly this text, still.
final int _noteBytes = utf8.encode(_noteText).length;

/// The fixed names inside a stage directory and the staging root: the
/// completion marker of a delivered copy, the owner file, the grant journal
/// the send side keeps directly under the staging root and its scratch file.
const String _markerName = '.ak-stage-complete';
const String _ownerName = '.owner';
const String _journalName = '.send-grants';
const String _journalScratchName = '.send-grants.tmp';

/// The phase marker: its file name, in the parent of the staging root.
const String _phaseMarkerName = 'ak-probe-recreate.txt';

const String _variantDefine = String.fromEnvironment('AK_SAF_RECREATE_VARIANT');
const String _registryDefine = String.fromEnvironment('AK_REGISTRY_ADDR');
const String _stunDefine = String.fromEnvironment('AK_STUN_ADDR');
const String _localRegistry = '10.0.2.2:17200';
const String _localStun = '10.0.2.2:17101';

String get _variantName =>
    _variantDefine.isEmpty ? 'recreate' : _variantDefine;

/// How long a pick may stay open. The host has seen the PICKER line by
/// then, so this only bounds a picker drive that never comes.
const int _pickSeconds = 240;

/// A staging of the 15-byte tree is over at once; this bounds a worker that
/// never answers.
const int _workSeconds = 120;

/// Phase 2 of `recreate`: how long the start-up cleanup may take.
const int _recreateWaitMs = 20000;

/// Phase 2 of `neighbour`: how long the second instance's start hook may
/// take to take its call and answer it. The host waits 60 s for the cue
/// that only a legal `removed=0` lets out, so this stays well inside that.
const int _hookWaitMs = 30000;

/// Phase 2 of `neighbour`: the copy must be gone while the host destroys
/// the first instance — seconds after N2|NEIGHBOUR. This bounds a
/// destruction that never comes.
const int _neighbourWaitMs = 60000;

const int _pollMs = 500;

final RegExp _idShape = RegExp(r'^[0-9a-f]{32}$');
final Random _random = Random.secure();

void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  // As the probe: live frames while the app is in front. In the background
  // its frames are paused, so phase 2 waits on plain timers only.
  binding.framePolicy = LiveTestWidgetsFlutterBindingFramePolicy.fullyLive;

  // The phase is read off the device (the marker file), so a run decides it.
  switch (_variantName) {
    case 'recreate':
    case 'cold':
      testWidgets(
          'Z SAF recreation: a delivered copy, and the start hook of the '
          'relaunched app',
          _recreateRun,
          timeout: const Timeout(Duration(minutes: 10)));
    case 'neighbour':
      testWidgets(
          'Z SAF recreation: a live neighbour keeps its copy, a retired one '
          'loses it',
          _neighbourRun,
          timeout: const Timeout(Duration(minutes: 12)));
    default:
      testWidgets('Z SAF: unknown AK_SAF_RECREATE_VARIANT',
          (_) => fail('AK_SAF_RECREATE_VARIANT=[$_variantName] is not one of: '
              '(empty), recreate, cold, neighbour'));
  }
}

/// Phase 2 of one variant: the tag of its evidence lines, the stage id the
/// marker holds and the staging root.
typedef _Phase2 = Future<void> Function(
    WidgetTester tester, String tag, String id, String stagingRoot);

Future<void> _recreateRun(WidgetTester tester) => _round(tester, 'R1', _recreatePhase2);

Future<void> _neighbourRun(WidgetTester tester) =>
    _round(tester, 'N1', _neighbourPhase2);

/// Phase 1 without a marker, phase 2 (`<first letter>2`) with one. A marker
/// that cannot be looked at names neither: the round fails under a fixed
/// name instead of staging a second copy over one that may be there.
Future<void> _round(WidgetTester tester, String phaseTag, _Phase2 phase2) async {
  final stagingRoot = await _derivedRoot();
  final marker = _phaseMarkerState(stagingRoot);
  if (!marker.readable) {
    _say('$phaseTag|MODE|$_variantName|android=${io.Platform.isAndroid}'
        '|marker=unreadable');
    _say('$phaseTag|DONE|failed=1');
    fail('the phase marker could not be looked at: marker-unreadable');
  }
  if (marker.id.isEmpty) {
    await _deliver(tester, phaseTag, stagingRoot);
  } else {
    await phase2(tester, '${phaseTag[0]}2', marker.id, stagingRoot);
  }
}

// ---------------------------------------------------------------------------
// Phase 1 (`R1`, `N1`): a delivered copy, and the phase marker.
//
// Nothing is released here on purpose, and no tearDown cleans the id: the
// delivered copy IS what the next isolate must find. The production gateway
// is not involved — the raw channel is called from a blank app, as the
// probe does.
// ---------------------------------------------------------------------------

Future<void> _deliver(WidgetTester tester, String tag, String root) async {
  final checks = _Checks();
  _say('$tag|MODE|$_variantName|android=${io.Platform.isAndroid}');
  _need(checks, io.Platform.isAndroid, 'needs-the-android-channel');
  _need(checks, root.isNotEmpty, 'root-unresolved');

  await tester.pumpWidget(
      const MaterialApp(home: Scaffold(body: SizedBox.shrink())));

  final picked = await _rawPick(tester, tag);
  final treeUri = picked.uri;
  _need(checks, treeUri != null, 'pick-returned-nothing');
  _need(checks, picked.name == _deepName, 'pick-not-nested-deep');

  final id = _newId();
  var status = 'not-requested';
  var copy = _Copy.none;
  if (treeUri != null) {
    final staged = await _settled(tester, _stageCall(treeUri, id));
    status = _statusOf(staged);
    _need(checks, status == 'ok', 'staged-not-ok:$status');
    if (staged.error == null) copy = _copyState('$root/$id');
  }
  // A read that failed is none of the facts below: it is named first.
  _need(checks, copy.read == 'ok', 'stage-dir-read:${copy.read}');
  _need(checks, copy.present == true, 'no-stage-directory');
  _need(checks, copy.marker, 'no-completion-marker');
  _need(checks, copy.owner, 'no-owner-file');
  _need(checks, copy.payload, 'no-payload');
  // The note is the KNOWN SAMPLE: a regular file holding exactly the preset
  // text — so of exactly its size, which this phase ESTABLISHES (the
  // `noteBytes` of the line below) and phase 2 of `neighbour` compares.
  _need(checks, copy.note, 'no-note-in-payload');

  // The marker goes to the parent of the staging root, never into it.
  var markerFile = 'absent';
  if (copy.delivered && root.isNotEmpty) {
    try {
      final marker = _phaseMarker(root);
      marker.parent.createSync(recursive: true);
      marker.writeAsStringSync(id, flush: true);
      final readBack = _phaseMarkerState(root);
      markerFile =
          readBack.readable && readBack.id == id ? 'written' : 'not-read-back';
    } on Object {
      markerFile = 'not-written';
    }
  }
  _need(checks, markerFile == 'written', 'marker-file:$markerFile');

  _say('$tag|DELIVERED|staged=$status|stageDir=${copy.present ?? 'unknown'}'
      '|marker=${copy.marker}|owner=${copy.owner}|payload=${copy.payload}'
      '|note=${copy.note}|noteBytes=${copy.noteBytes}|read=${copy.read}'
      '|markerFile=$markerFile');
  _say('$tag|DONE|failed=${checks.count}');
  _failOn(checks);
}

/// A send-side pick through the raw channel: asks for the host's drive,
/// waits for it and hands back the tree URI — which never prints.
Future<_Pick> _rawPick(WidgetTester tester, String tag) async {
  _say('$tag|PICKER|tree|awaiting external DocumentsUI drive');
  final picked = await _settled(
      tester,
      _storageChannel.invokeMethod<Object?>('pickSendFolder'),
      seconds: _pickSeconds);
  final map = picked.value;
  final treeUri = map is Map && map['treeUri'] is String
      ? map['treeUri'] as String
      : null;
  final name = map is Map && map['displayName'] is String
      ? map['displayName'] as String
      : '';
  // Which folder the host's drive ended in: a match boolean, never the name.
  _say('$tag|PICKED|selected=${treeUri != null}|nameIsDeep=${name == _deepName}'
      '|threw=${picked.error == null ? 'no' : picked.verdict}');
  return _Pick(treeUri, name);
}

// ---------------------------------------------------------------------------
// Phase 2, shared: the production start hook and the evidence around it.
// ---------------------------------------------------------------------------

/// The production start hook: the REAL app, built the way main.dart builds
/// it. AkSendingApp.initState() calls cleanupStaleStaging() for the
/// production gateway — this test never calls the method. Answers whether
/// the app was pumped: it is not unless the endpoints are local-only.
Future<bool> _pumpRealApp(
    WidgetTester tester, _Checks checks, _SweepTap sweep) async {
  final local = _endpointsAreLocal();
  _need(checks, local, 'endpoints-not-local');
  if (!local) return false;
  sweep.install();
  await tester.pumpWidget(app.buildAppRoot(app.bootstrapAppService()));
  return true;
}

/// The drive names local-only endpoints, as the E2E round does.
bool _endpointsAreLocal() =>
    _stunDefine == _localStun &&
    (_registryDefine == '' || _registryDefine == _localRegistry);

/// Phase 2 of `recreate`: the start-up cleanup of a relaunched isolate in a
/// process whose Activity was destroyed. Waits on timers only — no frame.
/// The start hook's answer is read off the tap and is REQUIRED: the tap on
/// the channel before the pump, at least one call, a legal `removed=<n>`
/// (zero allowed — in `recreate` the retiring instance's own sweep usually
/// has the copy gone first).
Future<void> _recreatePhase2(
    WidgetTester tester, String tag, String id, String root) async {
  final checks = _Checks();
  _need(checks, root.isNotEmpty, 'root-unresolved');
  final stageDir = root.isEmpty ? '' : '$root/$id';
  // Whether there is a directory to watch at all.
  final watching = stageDir.isNotEmpty;
  // null: the look itself failed — never "gone".
  bool? exists = watching ? _stageDirExists(stageDir) : null;
  // What the staging root held before the app was pumped.
  final bool? before = exists;
  var afterMs = -1;
  final sweep = _SweepTap();

  _say('$tag|MODE|$_variantName|marker=read|stageDirBefore=${_flag(exists)}');
  if (await _pumpRealApp(tester, checks, sweep)) {
    final clock = Stopwatch()..start();
    // The start hook's own answer first. The copy may be gone already (the
    // retiring instance's sweep in `recreate`), and then there is nothing to
    // wait for but the answer — the proof that the hook ran. In `cold` the
    // copy is there, and the hook's sweep is what removes it.
    while (clock.elapsedMilliseconds < _recreateWaitMs &&
        (sweep.calls < 1 || sweep.removed == null)) {
      await Future<void>.delayed(const Duration(milliseconds: _pollMs));
    }
    // While the look says the directory is there — or says nothing: a look
    // that failed is not "gone".
    while (exists != false && clock.elapsedMilliseconds < _recreateWaitMs) {
      await Future<void>.delayed(const Duration(milliseconds: _pollMs));
      exists = _stageDirExists(stageDir);
    }
    afterMs = clock.elapsedMilliseconds;
  }
  // Read the tap's facts before it is taken out: off means "not installed".
  final sweepFact = sweep.fact;
  sweep.remove();

  _say('$tag|STAGEDIR|exists=${_flag(exists)}'
      '|afterMs=${afterMs < 0 ? 'na' : afterMs}');
  if (!watching) {
    _need(checks, false, 'no-stage-directory-to-watch');
  } else if (exists == null) {
    // A look that failed is not "gone".
    _need(checks, false, 'stage-dir-look-failed');
  } else if (exists != false) {
    _need(checks, false, 'stage-directory-still-there');
  }

  final shape = root.isEmpty ? null : _rootShape(root);
  _say('$tag|ROOT|stageDirs=${shape?.stageDirs ?? 'na'}'
      '|other=${shape?.other ?? 'na'}'
      '|journal=${shape == null ? 'unreadable' : shape.journal}');
  _need(checks, shape != null, 'root-unreadable');
  if (shape != null) {
    // Guards of the line above: the root holds no stage directory at all.
    _need(checks, shape.stageDirs == 0, 'root-stage-dirs:${shape.stageDirs}');
    _need(checks, shape.other == 0, 'root-other:${shape.other}');
  }
  _say('$tag|SWEEP|$sweepFact');
  // The start hook must have RUN, and ANSWERED, in either variant: the tap
  // on the channel before the app was pumped, at least one sweep through
  // it, and a legal `removed=<n>` back — anything else (no answer, another
  // shape, an error) is a failed check, never a pass.
  _need(checks, sweep.installed, 'sweep-tap-not-installed');
  _need(checks, sweep.calls >= 1, 'start-hook-not-called:${sweep.calls}');
  _need(checks, sweep.removed != null,
      'start-hook-illegal-answer:${sweep.answer}');
  // In `cold` the start hook is the only thing that could remove the copy:
  // it must have been there when the new process started, and the hook must
  // have answered removed=<n>, n >= 1.
  if (_variantName == 'cold') {
    _need(checks, before == true,
        'cold-copy-not-there-before-the-start:${_flag(before)}');
    _need(checks, (sweep.removed ?? 0) >= 1,
        'start-hook-removed-nothing:${sweep.removed ?? 'none'}');
  }
  _say('$tag|MARKER|deleted=${_flag(_deletePhaseMarker(root))}');
  _say('$tag|DONE|failed=${checks.count}');
  _failOn(checks);
}

/// Phase 2 of `neighbour`: the isolate of the second instance. Its start-up
/// cleanup runs over the first instance's delivered copy while that
/// instance is alive; then the host destroys the first instance, and the
/// copy must go. Waits on timers only — this instance is in the background
/// from the moment the first task comes to the front.
///
/// The cue `N2|NEIGHBOUR` — the host's signal to destroy the first instance
/// — goes out ONLY when all of this holds (revision 6, T10): the tap was on
/// the channel before the app was pumped; the start hook took at least one
/// call and answered a LEGAL `removed=<n>`; that answer is `removed=0` (a
/// hook that reports a removal touched something it must leave alone); and
/// the first instance's copy, re-read after that answer, is there and
/// INTACT — its completion marker and owner file regular files, its payload
/// there, and the note of the size phase 1 established still holding the
/// preset text. Otherwise no cue is said: a fixed failure name is collected,
/// the AFTER / SWEEP / DONE lines go out, and the host fails the round.
Future<void> _neighbourPhase2(
    WidgetTester tester, String tag, String id, String root) async {
  final checks = _Checks();
  _need(checks, root.isNotEmpty, 'root-unresolved');
  final stageDir = root.isEmpty ? '' : '$root/$id';
  final sweep = _SweepTap();
  // What the first instance's copy is when this phase ends: false = gone,
  // true = still there, null = it could not be looked at.
  bool? first;
  var afterMs = -1;
  // Whether the cue went out.
  var cued = false;

  _say('$tag|MODE|neighbour|marker=read');
  if (await _pumpRealApp(tester, checks, sweep)) {
    // Why the cue cannot go out; null while everything it needs still holds.
    String? notCued;
    final clock = Stopwatch()..start();
    if (!sweep.installed) {
      notCued = 'neighbour-hook-not-installed';
    } else {
      // Bounded, on timers only: the hook has taken at least one call AND
      // its answer is a legal `removed=<n>`.
      while (clock.elapsedMilliseconds < _hookWaitMs &&
          (sweep.calls < 1 || sweep.removed == null)) {
        await Future<void>.delayed(const Duration(milliseconds: _pollMs));
      }
      final removed = sweep.removed;
      if (sweep.calls < 1 || sweep.answer == 'none') {
        notCued = 'neighbour-hook-no-answer';
      } else if (removed == null) {
        notCued = 'neighbour-hook-illegal-answer:${sweep.answer}';
      } else if (removed > 0) {
        notCued = 'neighbour-hook-removed-something:${sweep.answer}';
      }
    }
    // The first instance's copy, re-read once the hook has answered.
    var copy = _copyState(stageDir);
    if (notCued == null) {
      if (copy.read != 'ok') {
        notCued = 'neighbour-first-copy-unreadable-while-its-stager-is-alive';
      } else if (copy.present != true) {
        notCued = 'neighbour-first-copy-not-there-while-its-stager-is-alive';
      } else if (!copy.intact) {
        notCued = 'neighbour-first-copy-changed-while-its-stager-is-alive';
      }
    }
    if (notCued != null) {
      // No cue goes out: the host ends the round by itself.
      _need(checks, false, notCued);
      first = copy.present;
    } else {
      cued = true;
      _say('$tag|NEIGHBOUR|firstCopy=true|hook=${sweep.answer}'
          '|copyIntact=true');
      // The cue is out: the host brings the first task to the front and
      // presses BACK, and the retiring instance's sweep must take the copy.
      final wait = Stopwatch()..start();
      while (!copy.gone && wait.elapsedMilliseconds < _neighbourWaitMs) {
        await Future<void>.delayed(const Duration(milliseconds: _pollMs));
        copy = _copyState(stageDir);
      }
      afterMs = wait.elapsedMilliseconds;
      first = copy.present;
    }
  } else {
    // The app was not pumped (`endpoints-not-local` is collected): nothing
    // was looked at again.
    first = null;
  }
  // As in the recreate variant: the tap's facts, read before it goes.
  final sweepFact = sweep.fact;
  sweep.remove();

  _say('$tag|AFTER|firstCopy=${_flag(first)}'
      '|afterMs=${afterMs < 0 ? 'na' : afterMs}');
  if (cued) {
    _need(
        checks,
        first == false,
        first == null
            ? 'first-copy-unreadable-after-its-stager-retired'
            : 'first-copy-still-there-after-its-stager-retired');
  }
  _say('$tag|SWEEP|$sweepFact');
  if (!cued) {
    // Nothing is deleted while no cue was said: the copy, if any, and the
    // marker are what the host's own look needs.
    _say('$tag|DONE|failed=${checks.count}');
    _failOn(checks);
    return;
  }
  // The marker goes only once the copy is gone: while the copy is there it
  // is what a later look needs. The uninstall at the end takes both anyway.
  final bool? deleted = first == false ? _deletePhaseMarker(root) : false;
  _say('$tag|MARKER|deleted=${_flag(deleted)}');
  _say('$tag|DONE|failed=${checks.count}');
  _failOn(checks);
}

// ---------------------------------------------------------------------------
// What one run collects.
// ---------------------------------------------------------------------------

/// The failed checks under their names.
class _Checks {
  final List<String> failed = <String>[];

  int get count => failed.length;
}

void _need(_Checks checks, bool ok, String what) {
  if (!ok) checks.failed.add(what);
}

/// The run went on to the end whatever failed; the test fails on the list.
void _failOn(_Checks checks) {
  expect(checks.failed, isEmpty,
      reason: 'failed checks: ${checks.failed.join('; ')}');
}

void _say(String fact) {
  // ignore: avoid_print
  print('ZSAF|$fact');
}

String _flag(bool? value) => value == null ? 'unknown' : '$value';

// ---------------------------------------------------------------------------
// The channel, observed or called.
// ---------------------------------------------------------------------------

/// The `n` of a LEGAL answer — `removed=<n>` with n a non-negative integer
/// — else null: no answer yet, or one of any other form.
int? _removedOf(String answer) {
  final match = RegExp(r'^removed=([0-9]+)$').firstMatch(answer);
  return match == null ? null : int.tryParse(match.group(1)!);
}

/// A PlatformException's code as it may print: only when it is a plain
/// token; `other` for anything else. Never a message.
final RegExp _codeShape = RegExp(r'^[A-Za-z0-9_]{1,47}$');

String _codeOf(String? code) =>
    code != null && _codeShape.hasMatch(code) ? code : 'other';

/// Whether the message is the app's own start-up sweep (`cleanupStaleStaging`).
bool _isSweep(StandardMethodCodec codec, ByteData? message) {
  try {
    return codec.decodeMethodCall(message).method == 'cleanupStaleStaging';
  } on Object {
    return false;
  }
}

/// What the app's own start-up sweep answered — observed, never made. Every
/// message the app sends on `dev.ak/storage` is handed on to the platform
/// untouched, and the reply to `cleanupStaleStaging` is read on its way
/// back. Product code is not changed for it; a binding without the test
/// messenger leaves the channel alone (`tap=off`).
///
/// The answer is one fixed token: `none` until one came back, `removed=<n>`
/// for the contract's map, `other` for a map of another shape, `unreadable`
/// for no envelope, `no-reply` when the platform's reply never came back
/// through the tap, `error:<code>` for a PlatformException. The handler
/// never throws: an exception here would be printed in full.
class _SweepTap {
  TestDefaultBinaryMessenger? _messenger;
  int _calls = 0;
  String _answer = 'none';

  bool get installed => _messenger != null;

  /// How many sweeps went through the tap.
  int get calls => _calls;

  /// The last answer the tap read (`none` until one came back).
  String get answer => _answer;

  /// The `n` of the last answer when it is a legal `removed=<n>`; null while
  /// no answer came back, or one of any other form did.
  int? get removed => _removedOf(_answer);

  /// One evidence fact: whether the tap is on, how many sweeps went through
  /// it and what the last one answered.
  String get fact =>
      'tap=${installed ? 'installed' : 'off'}|calls=$_calls|answer=$_answer';

  void install() {
    final messenger = ServicesBinding.instance.defaultBinaryMessenger;
    if (messenger is! TestDefaultBinaryMessenger) return;
    const codec = StandardMethodCodec();
    final name = _storageChannel.name;
    messenger.setMockMessageHandler(name, (ByteData? message) async {
      final sweep = _isSweep(codec, message);
      if (sweep) _calls += 1;
      ByteData? reply;
      try {
        reply = await messenger.delegate.send(name, message);
      } on Object {
        // The platform's reply did not come back through the tap. The app
        // gets a fixed failure in its place — never the exception — and the
        // sweep's answer is a named fact.
        reply = codec.encodeErrorEnvelope(
            code: 'error', message: 'tap-send-failed');
        if (sweep) _answer = 'no-reply';
      }
      if (sweep && _answer == 'none') _answer = _envelope(codec, reply);
      return reply;
    });
    _messenger = messenger;
  }

  void remove() {
    _messenger?.setMockMessageHandler(_storageChannel.name, null);
    _messenger = null;
  }
}

/// The reply's envelope as one fixed token — never a message, which could
/// carry more than a code.
String _envelope(StandardMethodCodec codec, ByteData? reply) {
  try {
    // A null reply is no envelope.
    if (reply == null) return 'unreadable';
    final Object? value = codec.decodeEnvelope(reply);
    return value is Map && value['removed'] is int
        ? 'removed=${value['removed']}'
        : 'other';
  } on PlatformException catch (failure) {
    return 'error:${_codeOf(failure.code)}';
  } on Object {
    return 'unreadable';
  }
}

/// The answer of a channel call, captured whatever it was: [value] on
/// success, [error] on a PlatformException or anything else — so a call can
/// be judged without an unhandled error.
class _Answer {
  Object? value;
  Object? error;
  bool answered = false;

  /// `ok`, the PlatformException code, the error type, or `no-answer`.
  String get verdict {
    if (!answered) return 'no-answer';
    final failure = error;
    if (failure == null) return 'ok';
    return failure is PlatformException
        ? failure.code
        : 'threw:${failure.runtimeType}';
  }

  static _Answer capture(Future<Object?> call) {
    final answer = _Answer();
    unawaited(call.then((Object? value) {
      answer
        ..value = value
        ..answered = true;
    }, onError: (Object failure) {
      answer
        ..error = failure
        ..answered = true;
    }));
    return answer;
  }
}

/// `ok` or the status the answer map carries; a failed or missing call
/// shows its PlatformException code instead.
String _statusOf(_Answer answer) {
  if (!answer.answered) return 'no-answer';
  if (answer.error != null) return answer.verdict;
  final map = answer.value;
  return map is Map ? '${map['status']}' : 'no-map';
}

Future<Object?> _stageCall(String uri, String stageId) =>
    _storageChannel.invokeMethod<Object?>('stageSafTree',
        <String, Object?>{'treeUri': uri, 'stageId': stageId});

/// Awaits [call] (pumping meanwhile) and hands its captured answer.
Future<_Answer> _settled(
    WidgetTester tester, Future<Object?> call,
    {int seconds = _workSeconds}) async {
  final answer = _Answer.capture(call);
  await _pumpUntil(tester, () => answer.answered, seconds);
  return answer;
}

/// The one pump that may be outstanding, shared by the single wait: a
/// second `pump()` on top of a stalled one would orphan the first.
Future<void>? _openPump;

/// Pumps until [ready] holds or [seconds] pass; answers whether it holds.
///
/// A pump STALLS while the system picker is in front of the app: a paused
/// activity schedules no frame, so `pump()` waits for one indefinitely. As
/// in the probe, at most ONE pump is ever outstanding and the loop keeps
/// polling [ready] around a stalled one.
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

class _Pick {
  const _Pick(this.uri, this.name);

  /// The tree URI of the pick — never printed; null without a selection.
  final String? uri;

  /// The display name the platform answered, compared and never printed.
  final String name;
}

// ---------------------------------------------------------------------------
// The staging root, read through dart:io.
// ---------------------------------------------------------------------------

/// What one stage directory holds — by the fixed names inside it, never by
/// the stage id in its name and never by what the owner file says.
class _Copy {
  const _Copy({
    required this.present,
    required this.marker,
    required this.owner,
    required this.payload,
    required this.note,
    required this.noteBytes,
    required this.read,
  });

  /// Nothing is at the path — or nothing was staged: every fact false, and
  /// that was read.
  static const _Copy none = _Copy(
      present: false,
      marker: false,
      owner: false,
      payload: false,
      note: false,
      noteBytes: -1,
      read: 'ok');

  /// The path could not be looked at at all: no fact here is one.
  static const _Copy unreadable = _Copy(
      present: null,
      marker: false,
      owner: false,
      payload: false,
      note: false,
      noteBytes: -1,
      read: 'look-failed');

  /// The directory is there: null when the look itself failed — never "gone".
  final bool? present;

  /// `.ak-stage-complete` is a regular file: the copy was delivered.
  final bool marker;

  /// `.owner` is a regular file — not a link, not a directory.
  final bool owner;

  /// `payload` is a real directory.
  final bool payload;

  /// `payload/<folder>/note.txt` is a regular file holding the host's note.
  final bool note;

  /// The note's size in bytes; -1 without such a regular file, or when it
  /// could not be measured.
  final int noteBytes;

  /// Whether all of this could be read: `ok`; `look-failed` — the directory
  /// itself could not be looked at; `list-failed` — it could not be listed;
  /// `read-failed` — an entry inside could not be looked at or measured.
  /// Every look is caught where it is made, and one that failed leaves its
  /// fact false: a false is "not there" only when this says `ok`.
  final String read;

  /// The look says nothing is at the path any more.
  bool get gone => read == 'ok' && present == false;

  /// Delivered: payload, the completion marker and the owner file.
  bool get delivered =>
      read == 'ok' && present == true && marker && owner && payload;

  /// Delivered, and the note is the one phase 1 established: of its size,
  /// holding its text.
  bool get intact => delivered && note && noteBytes == _noteBytes;
}

/// Never throws: a look that failed is [read], and its facts stay false.
_Copy _copyState(String dir) {
  final type = _typeOf(dir);
  if (type == null) return _Copy.unreadable;
  if (type != io.FileSystemEntityType.directory) return _Copy.none;
  final List<io.FileSystemEntity> entries;
  try {
    entries = io.Directory(dir).listSync(followLinks: false);
  } on Object {
    return const _Copy(
        present: true,
        marker: false,
        owner: false,
        payload: false,
        note: false,
        noteBytes: -1,
        read: 'list-failed');
  }
  var failed = false;
  bool regular(String path) {
    final type = _typeOf(path);
    if (type == null) failed = true;
    return type == io.FileSystemEntityType.file;
  }

  final note = _noteState('$dir/payload/$_deepName/note.txt');
  return _Copy(
    present: true,
    marker: regular('$dir/$_markerName'),
    owner: regular('$dir/$_ownerName'),
    payload: entries.any(
        (e) => e is io.Directory && _basename(e.path) == 'payload'),
    note: note.held,
    noteBytes: note.bytes,
    read: failed || !note.read ? 'read-failed' : 'ok',
  );
}

/// The known sample — the note inside a payload. [bytes]: its size, -1 when
/// it is no regular file or could not be measured; [held]: whether its
/// content is exactly the preset note; [read]: whether every look at it
/// succeeded (a content that could not be read is not held).
({int bytes, bool held, bool read}) _noteState(String path) {
  final type = _typeOf(path);
  if (type == null) return (bytes: -1, held: false, read: false);
  if (type != io.FileSystemEntityType.file) {
    return (bytes: -1, held: false, read: true);
  }
  final file = io.File(path);
  int bytes;
  try {
    bytes = file.lengthSync();
  } on Object {
    return (bytes: -1, held: false, read: false);
  }
  try {
    final held = file.readAsStringSync() == _noteText;
    return (bytes: bytes, held: held, read: true);
  } on Object {
    return (bytes: bytes, held: false, read: false);
  }
}

/// Whether the stage directory [dir] is there; null when the look itself
/// failed — never "gone" (the caller names it).
bool? _stageDirExists(String dir) {
  final type = _typeOf(dir);
  if (type == null) return null;
  return type == io.FileSystemEntityType.directory;
}

/// What the staging root holds — by KIND and count, never by name. The
/// journal and its scratch file are not `other`: the journal is reported on
/// its own.
class _Root {
  const _Root({required this.stageDirs, required this.other, required this.journal});

  /// Real directories — one staged copy each (their names are stage ids).
  final int stageDirs;

  /// Everything that is neither a stage directory nor the journal.
  final int other;

  /// `absent`, `empty`, `lines:<n>`, `not-a-file` or `unreadable`.
  final String journal;
}

/// Null when a look or a listing failed — never a root that holds nothing
/// (the caller names it `root-unreadable`).
_Root? _rootShape(String root) {
  final type = _typeOf(root);
  if (type == null) return null;
  if (type != io.FileSystemEntityType.directory) {
    return const _Root(stageDirs: 0, other: 0, journal: 'absent');
  }
  final List<io.FileSystemEntity> entries;
  try {
    entries = io.Directory(root).listSync(followLinks: false);
  } on Object {
    return null;
  }
  var stageDirs = 0;
  var other = 0;
  for (final entry in entries) {
    final name = _basename(entry.path);
    if (entry is io.Directory) {
      stageDirs += 1;
    } else if (name != _journalName && name != _journalScratchName) {
      other += 1;
    }
  }
  return _Root(
      stageDirs: stageDirs, other: other, journal: _journalState(root));
}

/// The grant journal by LINE COUNT only — a line in it is a tree URI the
/// send side still holds the read grant of; the line itself never prints.
/// Never throws: a look or a read that failed is `unreadable`.
String _journalState(String root) {
  final journal = io.File('$root/$_journalName');
  final type = _typeOf(journal.path);
  if (type == null) return 'unreadable';
  switch (type) {
    case io.FileSystemEntityType.notFound:
      return 'absent';
    case io.FileSystemEntityType.file:
      break;
    default:
      return 'not-a-file';
  }
  try {
    final lines = journal
        .readAsLinesSync()
        .where((line) => line.trim().isNotEmpty)
        .length;
    return lines == 0 ? 'empty' : 'lines:$lines';
  } on Object {
    return 'unreadable';
  }
}

// ---------------------------------------------------------------------------
// The phase marker.
// ---------------------------------------------------------------------------

/// The phase marker as a round starts: the stage id it holds — never
/// printed; '' without a marker or when it holds nothing usable — and
/// whether it could be looked at at all. Nothing here throws.
({String id, bool readable}) _phaseMarkerState(String stagingRoot) {
  // Without a staging root there is nothing to look at: phase 1 names that.
  if (stagingRoot.isEmpty) return (id: '', readable: true);
  try {
    final marker = _phaseMarker(stagingRoot);
    if (!marker.existsSync()) return (id: '', readable: true);
    final id = marker.readAsStringSync().trim();
    return (id: _idShape.hasMatch(id) ? id : '', readable: true);
  } on Object {
    return (id: '', readable: false);
  }
}

/// Removes the marker; null when the staging root could not be derived.
/// Never throws: false when the delete or the look after it failed — which
/// is not "deleted".
bool? _deletePhaseMarker(String stagingRoot) {
  if (stagingRoot.isEmpty) return null;
  try {
    final marker = _phaseMarker(stagingRoot);
    if (marker.existsSync()) marker.deleteSync();
    return !marker.existsSync();
  } on Object {
    return false;
  }
}

io.File _phaseMarker(String stagingRoot) =>
    io.File('${_parentOf(stagingRoot)}/$_phaseMarkerName');

String _parentOf(String path) => path.substring(0, path.lastIndexOf('/'));

/// The staging root as Android lays it out: the application-support
/// directory is `<data dir>/files`, the staging root
/// `<data dir>/no_backup/ak-saf-staging`. Resolved through symlinks once it
/// exists (Android's `/data/user/0` is a link to `/data/data`). Empty when
/// the platform directory cannot be resolved — which the parts that depend
/// on it count as a failure.
Future<String> _derivedRoot() async {
  try {
    final support = (await getApplicationSupportDirectory()).path;
    final slash = support.lastIndexOf('/');
    final dataDir = slash > 0 ? support.substring(0, slash) : support;
    final root = io.Directory('$dataDir/no_backup/ak-saf-staging');
    return root.existsSync() ? root.resolveSymbolicLinksSync() : root.path;
  } on Object {
    return '';
  }
}

// ---------------------------------------------------------------------------
// Small helpers.
// ---------------------------------------------------------------------------

/// A fresh staging id in the shape the platform validates: 32 lowercase hex.
String _newId() => [
      for (var i = 0; i < 16; i += 1)
        _random.nextInt(256).toRadixString(16).padLeft(2, '0'),
    ].join();

/// The type of what is at [path], no link followed — or null when the look
/// itself failed, which is no type at all: every caller names it, and none
/// reads a failed look as "gone".
io.FileSystemEntityType? _typeOf(String path) {
  try {
    return io.FileSystemEntity.typeSync(path, followLinks: false);
  } on Object {
    return null;
  }
}

String _basename(String path) => path.substring(path.lastIndexOf('/') + 1);
