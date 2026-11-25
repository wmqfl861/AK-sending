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
// Two variants, chosen by dart-define AK_SAF_RECREATE_VARIANT, and two
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
//         removes the copy. (In `recreate` the retiring instance's own sweep
//         usually has the copy gone before the new isolate starts, so that
//         variant proves the end state and the same process, not the hook.)
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
// print: counts, lengths and booleans stand in. Every check is HARD — a
// failed one is collected under its name, the run goes on to gather the
// rest, and the test fails on the collected list.
import 'dart:async' show unawaited;
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

/// Phase 1 without a marker, phase 2 (`<first letter>2`) with one.
Future<void> _round(WidgetTester tester, String phaseTag, _Phase2 phase2) async {
  final stagingRoot = await _derivedRoot();
  final id = _phaseMarkerId(stagingRoot);
  if (id.isEmpty) {
    await _deliver(tester, phaseTag, stagingRoot);
  } else {
    await phase2(tester, '${phaseTag[0]}2', id, stagingRoot);
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
  _need(checks, copy.present, 'no-stage-directory');
  _need(checks, copy.marker, 'no-completion-marker');
  _need(checks, copy.owner, 'no-owner-file');
  _need(checks, copy.payload, 'no-payload');
  _need(checks, copy.note, 'no-note-in-payload');

  // The marker goes to the parent of the staging root, never into it.
  var markerFile = 'absent';
  if (copy.delivered && root.isNotEmpty) {
    try {
      final marker = _phaseMarker(root);
      marker.parent.createSync(recursive: true);
      marker.writeAsStringSync(id, flush: true);
      markerFile = _phaseMarkerId(root) == id ? 'written' : 'not-read-back';
    } on Object {
      markerFile = 'not-written';
    }
  }
  _need(checks, markerFile == 'written', 'marker-file:$markerFile');

  _say('$tag|DELIVERED|staged=$status|stageDir=${copy.present}'
      '|marker=${copy.marker}|owner=${copy.owner}|payload=${copy.payload}'
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
Future<void> _recreatePhase2(
    WidgetTester tester, String tag, String id, String root) async {
  final checks = _Checks();
  _need(checks, root.isNotEmpty, 'root-unresolved');
  final stageDir = root.isEmpty ? '' : '$root/$id';
  // null: nothing to watch.
  bool? exists = stageDir.isEmpty ? null : _stageDirExists(stageDir);
  var afterMs = -1;
  final sweep = _SweepTap();

  _say('$tag|MODE|$_variantName|marker=read|stageDirBefore=${_flag(exists)}');
  if (await _pumpRealApp(tester, checks, sweep)) {
    final clock = Stopwatch()..start();
    while (exists == true && clock.elapsedMilliseconds < _recreateWaitMs) {
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
  _need(
      checks,
      exists == false,
      exists == null
          ? 'no-stage-directory-to-watch'
          : 'stage-directory-still-there');

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
  // The start hook must have RUN: the copy is gone because the app's own
  // clean-up removed it, not because nothing was there to remove.
  _need(checks, sweep.calls >= 1, 'start-hook-not-called:${sweep.calls}');
  _need(checks, sweep.removed >= 1,
      'start-hook-removed-nothing:${sweep.removed}');
  _say('$tag|MARKER|deleted=${_flag(_deletePhaseMarker(root))}');
  _say('$tag|DONE|failed=${checks.count}');
  _failOn(checks);
}

/// Phase 2 of `neighbour`: the isolate of the second instance. Its start-up
/// cleanup runs over the first instance's delivered copy while that
/// instance is alive; then the host destroys the first instance, and the
/// copy must go. Waits on timers only — this instance is in the background
/// from the moment the first task comes to the front.
Future<void> _neighbourPhase2(
    WidgetTester tester, String tag, String id, String root) async {
  final checks = _Checks();
  _need(checks, root.isNotEmpty, 'root-unresolved');
  final stageDir = root.isEmpty ? '' : '$root/$id';
  // null: nothing to watch.
  bool? first = stageDir.isEmpty ? null : _copyState(stageDir).delivered;
  var afterMs = -1;
  final sweep = _SweepTap();

  _say('$tag|MODE|neighbour|marker=read');
  if (await _pumpRealApp(tester, checks, sweep)) {
    await Future<void>.delayed(const Duration(seconds: 5));
    first = stageDir.isEmpty ? null : _copyState(stageDir).delivered;
    _say('$tag|NEIGHBOUR|firstCopy=${_flag(first)}');
    _need(checks, first == true, 'first-copy-not-there-while-its-stager-is-alive');
    final clock = Stopwatch()..start();
    while (first == true && clock.elapsedMilliseconds < _neighbourWaitMs) {
      await Future<void>.delayed(const Duration(milliseconds: _pollMs));
      first = _copyState(stageDir).delivered;
    }
    afterMs = clock.elapsedMilliseconds;
  } else {
    _say('$tag|NEIGHBOUR|firstCopy=unknown');
  }
  // As in the recreate variant: the tap's facts, read before it goes.
  final sweepFact = sweep.fact;
  sweep.remove();

  _say('$tag|AFTER|firstCopy=${_flag(first)}'
      '|afterMs=${afterMs < 0 ? 'na' : afterMs}');
  _need(
      checks,
      first == false,
      first == null
          ? 'no-copy-to-watch'
          : 'first-copy-still-there-after-its-stager-retired');
  _say('$tag|SWEEP|$sweepFact');
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

/// The `n` of `removed=<n>`, else −1.
int _removedOf(String answer) {
  final match = RegExp(r'^removed=([0-9]+)$').firstMatch(answer);
  return match == null ? -1 : int.parse(match.group(1)!);
}

/// What the app's own start-up sweep answered — observed, never made. Every
/// message the app sends on `dev.ak/storage` is handed on to the platform
/// untouched, and the reply to `cleanupStaleStaging` is read on its way
/// back. Product code is not changed for it; a binding without the test
/// messenger leaves the channel alone (`tap=off`).
class _SweepTap {
  TestDefaultBinaryMessenger? _messenger;
  int _calls = 0;
  int _removed = -1;
  String _answer = 'none';

  bool get installed => _messenger != null;

  /// How many sweeps went through the tap, and the `n` of the last
  /// `removed=<n>` answer (−1 when the last answer was not of that form).
  int get calls => _calls;
  int get removed => _removed;

  /// One evidence fact: whether the tap is on, how many sweeps went through
  /// it and what the last one answered (`removed=<n>`, a PlatformException
  /// code, `other` or `unreadable`).
  String get fact =>
      'tap=${installed ? 'installed' : 'off'}|calls=$_calls|answer=$_answer';

  void install() {
    final messenger = ServicesBinding.instance.defaultBinaryMessenger;
    if (messenger is! TestDefaultBinaryMessenger) return;
    const codec = StandardMethodCodec();
    final name = _storageChannel.name;
    messenger.setMockMessageHandler(name, (ByteData? message) async {
      var sweep = false;
      try {
        sweep = codec.decodeMethodCall(message).method == 'cleanupStaleStaging';
      } on Object {
        sweep = false;
      }
      if (sweep) _calls += 1;
      final ByteData? reply = await messenger.delegate.send(name, message);
      if (sweep) {
        _answer = _envelope(codec, reply);
        _removed = _removedOf(_answer);
      }
      return reply;
    });
    _messenger = messenger;
  }

  void remove() {
    _messenger?.setMockMessageHandler(_storageChannel.name, null);
    _messenger = null;
  }
}

/// `<code>/<token>` of a PlatformException, else `ok`, `no-answer` or the
/// error type: never a message, which could carry more than a token.
String _envelope(StandardMethodCodec codec, ByteData? reply) {
  try {
    // A null reply is no envelope.
    if (reply == null) return 'unreadable';
    final Object? value = codec.decodeEnvelope(reply);
    return value is Map && value['removed'] is int
        ? 'removed=${value['removed']}'
        : 'other';
  } on PlatformException catch (failure) {
    return 'error:${failure.code}';
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
  });

  static const _Copy none = _Copy(
      present: false, marker: false, owner: false, payload: false, note: false);

  /// The directory exists.
  final bool present;

  /// `.ak-stage-complete` is a regular file: the copy was delivered.
  final bool marker;

  /// `.owner` is a regular file — not a link, not a directory.
  final bool owner;

  /// `payload` is a real directory.
  final bool payload;

  /// `payload/<folder>/note.txt` is a regular file holding the host's note.
  final bool note;

  bool get delivered => present && marker && owner && payload;
}

_Copy _copyState(String dir) {
  if (!_isDirectory(dir)) return _Copy.none;
  try {
    final payload = io.Directory(dir)
        .listSync(followLinks: false)
        .any((e) => e is io.Directory && _basename(e.path) == 'payload');
    return _Copy(
      present: true,
      marker: _isRegularFile('$dir/$_markerName'),
      owner: _isRegularFile('$dir/$_ownerName'),
      payload: payload,
      note: _noteHeld('$dir/payload/$_deepName/note.txt'),
    );
  } on Object {
    return const _Copy(
        present: true, marker: false, owner: false, payload: false, note: false);
  }
}

bool _stageDirExists(String dir) => _isDirectory(dir);

bool _noteHeld(String path) {
  if (!_isRegularFile(path)) return false;
  try {
    return io.File(path).readAsStringSync() == _noteText;
  } on Object {
    return false;
  }
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

_Root? _rootShape(String root) {
  try {
    final dir = io.Directory(root);
    if (!_isDirectory(dir.path)) {
      return const _Root(stageDirs: 0, other: 0, journal: 'absent');
    }
    var stageDirs = 0;
    var other = 0;
    for (final entry in dir.listSync(followLinks: false)) {
      final name = _basename(entry.path);
      if (entry is io.Directory) {
        stageDirs += 1;
      } else if (name != _journalName && name != _journalScratchName) {
        other += 1;
      }
    }
    return _Root(stageDirs: stageDirs, other: other, journal: _journalState(root));
  } on Object {
    return null;
  }
}

/// The grant journal by LINE COUNT only — a line in it is a tree URI the
/// send side still holds the read grant of; the line itself never prints.
String _journalState(String root) {
  final journal = io.File('$root/$_journalName');
  switch (io.FileSystemEntity.typeSync(journal.path, followLinks: false)) {
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

/// The stage id the phase marker holds — never printed; '' without a marker
/// or when it holds nothing usable.
String _phaseMarkerId(String stagingRoot) {
  if (stagingRoot.isEmpty) return '';
  try {
    final marker = _phaseMarker(stagingRoot);
    if (!marker.existsSync()) return '';
    final id = marker.readAsStringSync().trim();
    return _idShape.hasMatch(id) ? id : '';
  } on Object {
    return '';
  }
}

/// Removes the marker; null when the staging root could not be derived.
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

bool _isRegularFile(String path) =>
    io.FileSystemEntity.typeSync(path, followLinks: false) ==
    io.FileSystemEntityType.file;

bool _isDirectory(String path) =>
    io.FileSystemEntity.typeSync(path, followLinks: false) ==
    io.FileSystemEntityType.directory;

String _basename(String path) => path.substring(path.lastIndexOf('/') + 1);
