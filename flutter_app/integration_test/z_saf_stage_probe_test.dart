// Z probe (batch Z, 2026-11-24; part D and the hard assertions of revision 3
// and T5–T8 of revision 4, 2026-11-25): the ANDROID SEND-SIDE folder path
// against the REAL platform in a profile build — the system tree picker
// (ACTION_OPEN_DOCUMENT_TREE), the native staging behind `dev.ak/storage`,
// the content of the staged copy, its release, and the failure paths of the
// channel. The Flutter side is driven by this test; the NATIVE picker is
// driven from the host (adb/uiautomator), for the reasons recorded in the
// header of the K1 walkthrough (k1_android_saf_test.dart).
//
// Three builds of this one file, chosen by dart-define AK_SAF_PROBE_MODE:
//
//   (empty) — the whole probe, z-probe.sh. Parts in order:
//     A  the PRODUCTION gateway, no injected seam: pick akstage → stage →
//        verify the staged tree → release.
//     B  the raw channel's failure paths: a malformed id, an untrusted
//        authority, a cancel that must land in the copy phase, an idempotent
//        cleanup, the stale cleanup next to a RUNNING staging.
//     C  the receive grant of the akstage tree survives a send staging (the
//        receive pick leaves READ+WRITE there; it stays for the rest).
//     D  the stager's own checks before its completion marker — a directory
//        that gained a child while the copy ran (source-changed), a file
//        that lost its enumerated size (size-mismatch) — and, on the
//        baseline staging, T6: a cleanup aimed at a RUNNING staging is
//        refused with busy at call time, and the staging still delivers.
//     E  T7: the receive side upgrades the grant DURING a send staging, on
//        a folder that holds no grant yet (nested/deep — a different URI
//        from akstage; verified first through listSafPersisted).
//   leave — z-probe-kill.sh, first launch: pick nested/deep, start a send
//        staging, print `ZSAF|K|COPY` at its first copy event; the host
//        force-stops the app mid-copy.
//   check — z-probe-kill.sh, second launch, same install: what the killed
//        process left (through dart:io), then cleanupStaleStaging, then
//        nothing.
//
// Orchestration contract:
//   * dart-define AK_SAF_PROBE_SHA_BIG=<64 hex> (default mode): the host's
//     SHA-256 of the 32 MiB `nested/photo.bin` it pushed to
//     /sdcard/Download/akstage.
//   * every evidence line `ZSAF|…|PICKER…|tree|awaiting external DocumentsUI
//     drive` (logcat, tag `flutter`) asks the host to drive DocumentsUI:
//     parts A, B and C onto akstage; part E onto akstage/nested/deep
//     (twice: `E|PICKER` and `E|PICKER2`); the leave mode onto
//     akstage/nested/deep.
//   * the synthetic tree, whose host manifest is the reference for part A:
//       nested/deep/note.txt   "ak stage probe" + LF
//       nested/photo.bin       33554432 random bytes
//       empty.txt              0 bytes
//       emptydir/              an empty directory
//       sub/说明 文本.txt       "中文 名字" + LF
//     The host changes it only after part A has verified it. It enlarges
//     nested/photo.bin while the test waits on part C's picker (a longer
//     copy phase) and changes the tree at each `ZSAF|D<n>|COPY` line — the
//     device's own "enumeration over, the copy began". For part E it puts a
//     big file and many small ones into nested/deep while the test waits on
//     `E|PICKER`, so that the send staging outlasts the second picker. It
//     restores the tree once the drive has ended.
//
// Evidence is one `ZSAF|…` line per fact. A staged path, a tree URI, a
// stage id and the content of the grant journal never print: counts,
// lengths and booleans stand in. Every check is HARD — a failed one is
// collected under its name, the run goes on to gather the rest, and the
// test fails on the collected list at the end.
import 'dart:async' show unawaited;
import 'dart:convert' show utf8;
import 'dart:io' as io;
import 'dart:math' show Random;

import 'package:ak_sending_app/ui/service/file_pick_gateway.dart';
import 'package:crypto/crypto.dart' as crypto;
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:path_provider/path_provider.dart'
    show getApplicationSupportDirectory;

const MethodChannel _storageChannel = MethodChannel('dev.ak/storage');

/// The synthetic tree: where the host pushed it, and what it holds.
const String _rootName = 'akstage';
const String _noteRel = 'nested/deep/note.txt';
const String _bigRel = 'nested/photo.bin';
const String _emptyRel = 'empty.txt';
const String _ziwenRel = 'sub/说明 文本.txt';
const int _bigBytes = 33554432;
const List<String> _directories = <String>[
  'emptydir',
  'nested',
  'nested/deep',
  'sub',
];
const Map<String, String> _texts = <String, String>{
  _noteRel: 'ak stage probe\n',
  _ziwenRel: '中文 名字\n',
};
final Map<String, int> _sizes = <String, int>{
  for (final text in _texts.entries) text.key: utf8.encode(text.value).length,
  _bigRel: _bigBytes,
  _emptyRel: 0,
};
final int _expectedBytes =
    _sizes.values.fold<int>(0, (sum, size) => sum + size);
final String _emptySha = crypto.sha256.convert(const <int>[]).toString();

/// The fixed names inside the staging root and a stage directory: the grant
/// journal the send side keeps directly under the staging root (revision 3,
/// N2) and the scratch file it is rewritten through; the completion marker
/// of a delivered copy; the owner file every stage directory of a process
/// carries (revision 4, N11).
const String _journalName = '.send-grants';
const String _journalScratchName = '.send-grants.tmp';
const String _markerName = '.ak-stage-complete';
const String _ownerName = '.owner';

/// The folder of part E and of the kill round — nested/deep — and the big
/// file the host puts into it to keep a staging busy.
const String _deepName = 'deep';
const String _holdName = 'hold.bin';

/// The mode of this build (dart-define AK_SAF_PROBE_MODE): empty = the
/// whole probe; `leave` and `check` are the two launches of the kill round.
const String _mode = String.fromEnvironment('AK_SAF_PROBE_MODE');

/// How long a pick may stay open. The host has seen the PICKER line by
/// then, so this only bounds a picker drive that never comes — and the
/// tree preparation the host does while the test waits on part C's picker.
const int _pickSeconds = 240;

/// Part E's first pick: the host also builds nested/deep's payload in that
/// window (a big file, and thousands of small ones).
const int _t7PickSeconds = 300;

const int _workSeconds = 120;

/// A staging that copies the enlarged file: its budget is its own.
const int _bigWorkSeconds = 180;

/// Part E's staging copies the big file AND thousands of small ones.
const int _t7WorkSeconds = 300;

/// For part E's TIMING evidence only: how long after the send staging's
/// answer the receive pick may answer and still read as "confirmed while
/// the staging ran". The receive take is queued behind the running staging
/// on the stager's worker, so such a pick answers right behind it.
const int _behindSlackMs = 1500;

final RegExp _idShape = RegExp(r'^[0-9a-f]{32}$');
final Random _random = Random.secure();

/// One clock for the evidence timings (the copy phase of a staging).
final Stopwatch _clock = Stopwatch()..start();

void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  binding.framePolicy = LiveTestWidgetsFlutterBindingFramePolicy.fullyLive;

  switch (_mode) {
    case '':
      testWidgets(
          'Z SAF: tree pick, native staging, staged-tree verification, release and failure paths',
          _defaultRun,
          timeout: const Timeout(Duration(minutes: 20)));
    case 'leave':
      testWidgets('Z SAF kill: a send staging, stopped by the host mid-copy',
          _leaveRun,
          timeout: const Timeout(Duration(minutes: 15)));
    case 'check':
      testWidgets(
          'Z SAF kill: what the stopped staging left, and the stale sweep',
          _checkRun,
          timeout: const Timeout(Duration(minutes: 5)));
    default:
      testWidgets(
          'Z SAF: unknown AK_SAF_PROBE_MODE',
          (_) => fail(
              'AK_SAF_PROBE_MODE=[$_mode] is not one of: (empty), leave, check'));
  }
}

/// The whole probe: parts A–E, then the verdict on every collected check.
Future<void> _defaultRun(WidgetTester tester) async {
  const shaBig = String.fromEnvironment('AK_SAF_PROBE_SHA_BIG');
  zSay('BOOTSTRAP|android=${io.Platform.isAndroid}|mode=default'
      '|shaBig=${shaBig.length}hex');
  expect(shaBig, matches(RegExp(r'^[0-9a-f]{64}$')),
      reason: 'AK_SAF_PROBE_SHA_BIG (64 lowercase hex) is required');
  expect(io.Platform.isAndroid, isTrue,
      reason: 'the probe needs the Android channel');

  final run = _Run(tester);
  addTearDown(() async {
    // Whatever went wrong, no staged copy of this run stays behind. For an
    // id whose staging is still in flight the platform answers `busy` at
    // call time and deletes nothing; that copy is left to the next start's
    // stale sweep, exactly as a killed process would leave it.
    final released = await run.releaseOwned();
    _storageChannel.setMethodCallHandler(null);
    zSay('TEARDOWN|ids=${released.ids}|done=${released.done}'
        '|unanswered=${released.unanswered}'
        '|not-removed=${released.notRemoved}|busy=${released.busy}');
    // Both must be 0 for the test to pass: an exception in a tearDown
    // callback fails the test (the test runner's own rule).
    expect(released.unanswered, 0,
        reason: 'cleanups that did not answer within 30 s');
    expect(released.notRemoved, 0,
        reason: 'cleanups that did not answer {"removed": true}');
  });

  // The production gateway reaches the root navigator for its dialogs.
  await tester.pumpWidget(
      const MaterialApp(home: Scaffold(body: SizedBox.shrink())));

  await _partA(run, shaBig);
  if (!run.pickerStalled) await _partB(run);
  if (!run.pickerStalled) await _partC(run);
  if (!run.pickerStalled && run.treeUri.isNotEmpty) await _partD(run);
  if (!run.pickerStalled && run.treeUri.isNotEmpty) await _partE(run);

  zSay('VERDICT|${run.problems.isEmpty ? 'ok' : 'failed=${run.problems.length}'}');
  expect(run.problems, isEmpty,
      reason: 'failed checks: ${run.problems.join('; ')}');
}

void zSay(String value) {
  // ignore: avoid_print
  print('ZSAF|$value');
}

/// What one run collects: the failed checks under their names, every stage
/// id this run may leave behind, the staging root and the tree URI the
/// later parts work with, and whether a picker drive never came — the
/// parts after that cannot run.
class _Run {
  _Run(this.tester);

  final WidgetTester tester;
  final List<String> problems = <String>[];
  final Set<String> owned = <String>{};

  /// The staging root as the platform spelled it in part A's answer.
  String stagingRoot = '';

  /// The tree URI of part B's pick — never printed.
  String treeUri = '';

  /// The display name of the last pick, as the platform answered it: which
  /// folder the host's DocumentsUI drive really ended in.
  String lastPickName = '';
  bool pickerStalled = false;

  /// A fresh staging id in the shape the platform validates.
  String newId() {
    final id = [
      for (var i = 0; i < 16; i += 1)
        _random.nextInt(256).toRadixString(16).padLeft(2, '0'),
    ].join();
    owned.add(id);
    return id;
  }

  /// The safety net of the default mode: releases every stage id this run
  /// asked for. A cleanup counts as done only when it answered AND answered
  /// `{"removed": true}` — a missing answer and any other answer are
  /// counted, never hidden.
  Future<({int ids, int done, int unanswered, int notRemoved, int busy})>
      releaseOwned() async {
    var done = 0;
    var unanswered = 0;
    var notRemoved = 0;
    var busy = 0;
    for (final id in owned) {
      final answer = _Answer.capture(_storageChannel.invokeMethod<Object?>(
          'cleanupStagedSend', <String, Object?>{'stageId': id}));
      final answered = await answer.wait(30);
      if (!answered) {
        unanswered += 1;
      } else if (_removedOf(answer) == true) {
        done += 1;
      } else {
        notRemoved += 1;
        if (_failedWith(answer, 'STAGE_IO_FAILED', 'busy')) busy += 1;
      }
    }
    return (
      ids: owned.length,
      done: done,
      unanswered: unanswered,
      notRemoved: notRemoved,
      busy: busy,
    );
  }
}

/// The answer of a channel call, captured whatever it was: [value] on
/// success, [error] on a PlatformException or anything else. An [_Answer]
/// itself never fails, so a call can stay unawaited without an unhandled
/// error and be judged later.
class _Answer {
  Object? value;
  Object? error;
  bool answered = false;

  /// When the call answered, on the test's clock.
  int? answeredAtMs;

  /// `ok`, the PlatformException code, the error type, or `no-answer`.
  String get verdict {
    if (!answered) return 'no-answer';
    final failure = error;
    if (failure == null) return 'ok';
    return failure is PlatformException
        ? failure.code
        : 'threw:${failure.runtimeType}';
  }

  /// The reason token of a PlatformException (`leftover`, `source-changed`,
  /// …). Anything that is not a plain lowercase token reads `?`: a message
  /// that carries more than the contract's fixed token never prints.
  String get token {
    final failure = error;
    if (failure is! PlatformException) return '';
    final message = failure.message ?? '';
    return RegExp(r'^[a-z][a-z0-9-]{0,31}$').hasMatch(message) ? message : '?';
  }

  /// `<code>/<token>` of a PlatformException, else [verdict].
  String get codeToken {
    final failure = error;
    return failure is PlatformException ? '${failure.code}/$token' : verdict;
  }

  /// Waits up to [seconds] for the answer without pumping — a teardown has
  /// no frame to wait for. Answers whether the call DID answer: a caller
  /// must not take silence for one.
  Future<bool> wait(int seconds) async {
    final end = DateTime.now().add(Duration(seconds: seconds));
    while (!answered && DateTime.now().isBefore(end)) {
      await Future<void>.delayed(const Duration(milliseconds: 100));
    }
    return answered;
  }

  static _Answer capture(Future<Object?> call) {
    final answer = _Answer();
    unawaited(call.then((Object? value) {
      answer
        ..value = value
        ..answeredAtMs = _clock.elapsedMilliseconds
        ..answered = true;
    }, onError: (Object failure) {
      answer
        ..error = failure
        ..answeredAtMs = _clock.elapsedMilliseconds
        ..answered = true;
    }));
    return answer;
  }
}

/// The one pump that may be outstanding, shared by every wait: a second
/// `pump()` on top of a stalled one would orphan the first.
Future<void>? _openPump;

/// Pumps until [ready] holds or [seconds] pass; answers whether it holds.
///
/// A pump STALLS while the system picker is in front of the app: a paused
/// activity schedules no frame, so `pump()` waits for one indefinitely. K1
/// bounded that wait at 45 s; here the picker may legally stay open longer
/// (the host scrolls and dumps), so at most ONE pump is ever outstanding,
/// the loop keeps polling [ready] around a stalled one, and the next frame
/// completes it.
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
  // A pump still open keeps the guarded scope of the test API: the
  // caller's next line is usually a guarded tap. With the wait satisfied
  // the app is in the foreground and its frame is already due. [ready]
  // itself runs beside the open pump, so it must stay off the guarded
  // members of [WidgetTester] (a plain flag here).
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

/// Awaits [call] (pumping meanwhile) and hands its captured answer. A call
/// that copies the enlarged file gets [seconds].
Future<_Answer> _settled(_Run run, Future<Object?> call,
    {int seconds = _workSeconds}) async {
  final answer = _Answer.capture(call);
  await _pumpUntil(run.tester, () => answer.answered, seconds);
  return answer;
}

/// A channel call in a mode that pumps nothing (the kill round's `check`
/// mode): no frame to wait for. Whether it answered is for the caller to
/// read off the answer — silence is never taken for an answer.
Future<_Answer> _call(Future<Object?> call) async {
  final answer = _Answer.capture(call);
  await answer.wait(60);
  return answer;
}

/// Whether the call failed with exactly `<code>/<token>`.
bool _failedWith(_Answer answer, String code, String token) {
  final failure = answer.error;
  return failure is PlatformException &&
      failure.code == code &&
      answer.token == token;
}

/// `ok` or the status the answer map carries; a failed or missing call
/// shows its PlatformException code instead.
String _statusOf(_Answer answer) {
  if (!answer.answered) return 'no-answer';
  if (answer.error != null) return answer.verdict;
  final map = answer.value;
  return map is Map ? '${map['status']}' : 'no-map';
}

// ---------------------------------------------------------------------------
// The channel calls of parts B–D, and what their answers must look like.
// ---------------------------------------------------------------------------

Future<Object?> _stageCall(String uri, String stageId) =>
    _storageChannel.invokeMethod<Object?>('stageSafTree',
        <String, Object?>{'treeUri': uri, 'stageId': stageId});

/// The `removed` flag of a cleanup answer: true or false for the contract's
/// `{"removed": …}` map, null for any other answer — an error, `busy`, a
/// map of another shape, no answer at all.
bool? _removedOf(_Answer answer) {
  final value = answer.value;
  if (answer.error != null || value is! Map || value['removed'] is! bool) {
    return null;
  }
  return value['removed'] as bool;
}

/// A direct `cleanupStagedSend`, judged by what the platform is contracted
/// to answer: a map with the boolean `removed` — `true` also when the stage
/// directory is already gone, `false` when something is left behind. For an
/// id whose staging is registered it answers `STAGE_IO_FAILED` / `busy` at
/// call time.
Future<({String text, bool shapeOk, bool? removed})> _cleanup(
    _Run run, String stageId) async {
  final answer = await _settled(
      run,
      _storageChannel.invokeMethod<Object?>(
          'cleanupStagedSend', <String, Object?>{'stageId': stageId}));
  final removed = _removedOf(answer);
  return (
    text: removed != null ? 'removed=$removed' : answer.codeToken,
    shapeOk: removed != null,
    removed: removed,
  );
}

/// The `stagingProgress` events of the raw channel while the tap is on it:
/// per stage id when its copy phase began and ended (ms on the test's
/// clock) and its last event — and, for the probes that act on a copy
/// event, a hook that runs on every one of them.
class _ProgressTap {
  final Map<String, int> _copyStartedAt = <String, int>{};
  final Map<String, int> _copyEndedAt = <String, int>{};
  final Map<String, int> _copyEvents = <String, int>{};
  final Map<String, Map<Object?, Object?>> _last = <String, Map<Object?, Object?>>{};
  var _events = 0;
  String lastPhase = 'none';

  /// Runs on every copy-phase event, with the event as the platform sent it.
  void Function(Map<Object?, Object?> event)? onCopy;

  int get events => _events;
  int? copyStartedAt(String id) => _copyStartedAt[id];
  int? copyEndedAt(String id) => _copyEndedAt[id];
  int copyEvents(String id) => _copyEvents[id] ?? 0;
  Map<Object?, Object?>? last(String id) => _last[id];

  /// How long the copy phase of [id] lasted: first copy event to last.
  int? copyMs(String id) {
    final started = _copyStartedAt[id];
    final ended = _copyEndedAt[id];
    return started == null || ended == null ? null : ended - started;
  }

  /// From the last copy event of [id] to the moment [answer] came back:
  /// the membership check, the marker and the grant release.
  int? afterCopyMs(String id, _Answer answer) {
    final ended = _copyEndedAt[id];
    final at = answer.answeredAtMs;
    return ended == null || at == null ? null : at - ended;
  }

  /// Puts the tap on the channel — BEFORE the staging call it watches.
  void install() {
    _storageChannel.setMethodCallHandler((MethodCall call) async {
      final args = call.arguments;
      if (call.method != 'stagingProgress' || args is! Map) return null;
      final event = Map<Object?, Object?>.from(args);
      final id = '${event['stageId']}';
      final now = _clock.elapsedMilliseconds;
      _events += 1;
      lastPhase = '${event['phase']}';
      _last[id] = event;
      if (event['phase'] == 'copy') {
        _copyStartedAt.putIfAbsent(id, () => now);
        _copyEndedAt[id] = now;
        _copyEvents[id] = copyEvents(id) + 1;
        onCopy?.call(event);
      }
      return null;
    });
  }

  void remove() {
    onCopy = null;
    _storageChannel.setMethodCallHandler(null);
  }
}

// ---------------------------------------------------------------------------
// The staging root: what may be in it, and the grant journal.
// ---------------------------------------------------------------------------

/// What the staging root holds — by KIND and count, never by name. The
/// send side keeps its grant journal directly under the staging root
/// (revision 3, N2): a regular file that stays behind EMPTY once every
/// grant it recorded is released. So "nothing left" means no stage
/// directory and no other entry; the journal is reported on its own, by
/// size only — its lines are tree URIs.
class _RootShape {
  const _RootShape({
    required this.exists,
    required this.children,
    required this.stageDirs,
    required this.other,
    required this.scratch,
    required this.journal,
  });

  final bool exists;

  /// Every entry directly under the staging root.
  final int children;

  /// Real directories — one staged copy each (their names are stage ids).
  final int stageDirs;

  /// Everything that is neither a stage directory nor the journal.
  final int other;

  /// The journal's scratch file is still there. It exists only while the
  /// journal is being rewritten, which is over before a staging answers.
  final bool scratch;

  /// `absent`, `empty`, `<n> bytes` or `not-a-file`.
  final String journal;

  /// No stage directory and nothing else: the root holds no copy.
  bool get holdsNothing => exists && stageDirs == 0 && other == 0;

  /// [holdsNothing], with the journal empty and its scratch file gone.
  bool get clear =>
      holdsNothing && !scratch && (journal == 'absent' || journal == 'empty');

  String get describe =>
      'exists=$exists|children=$children|stageDirs=$stageDirs|other=$other'
      '|scratch=$scratch|journal=$journal';
}

_RootShape _rootShape(String root) {
  final dir = io.Directory(root);
  if (!dir.existsSync()) {
    return const _RootShape(
      exists: false,
      children: 0,
      stageDirs: 0,
      other: 0,
      scratch: false,
      journal: 'absent',
    );
  }
  final entries = dir.listSync(followLinks: false);
  var stageDirs = 0;
  var other = 0;
  var scratch = false;
  for (final entry in entries) {
    final name = _basename(entry.path);
    if (entry is io.Directory) {
      stageDirs += 1;
    } else if (entry is io.File && name == _journalScratchName) {
      scratch = true;
    } else if (name != _journalName) {
      other += 1;
    }
  }
  return _RootShape(
    exists: true,
    children: entries.length,
    stageDirs: stageDirs,
    other: other,
    scratch: scratch,
    journal: _journalState(root),
  );
}

/// The grant journal by SIZE only — a line in it is a tree URI the send
/// side still holds the read grant of; the line itself never prints.
String _journalState(String root) {
  final journal = io.File('$root/$_journalName');
  switch (io.FileSystemEntity.typeSync(journal.path, followLinks: false)) {
    case io.FileSystemEntityType.notFound:
      return 'absent';
    case io.FileSystemEntityType.file:
      final bytes = journal.lengthSync();
      return bytes == 0 ? 'empty' : '$bytes bytes';
    default:
      return 'not-a-file';
  }
}

/// How many lines the grant journal holds — counted, never read out loud
/// (a line is a tree URI). -1 when it cannot be read.
int _journalLines(String root) {
  final journal = io.File('$root/$_journalName');
  if (!_isRegularFile(journal.path)) return 0;
  try {
    return journal
        .readAsLinesSync()
        .where((line) => line.trim().isNotEmpty)
        .length;
  } on Object {
    return -1;
  }
}

bool _isRegularFile(String path) =>
    io.FileSystemEntity.typeSync(path, followLinks: false) ==
    io.FileSystemEntityType.file;

/// What one stage directory holds — by the fixed names inside it, never by
/// the stage id in its name and never by what the owner file says.
class _StageDir {
  const _StageDir({
    required this.present,
    required this.marker,
    required this.owner,
    required this.ownerBytes,
    required this.payload,
    required this.holdBytes,
    required this.children,
  });

  /// The directory exists.
  final bool present;

  /// `.ak-stage-complete` is a regular file: the copy was delivered.
  final bool marker;

  /// `.owner` is a regular file — not a link, not a directory.
  final bool owner;
  final int ownerBytes;

  /// `payload` is a directory.
  final bool payload;

  /// Size of `payload/<root>/hold.bin`; -1 without such a file.
  final int holdBytes;

  /// Entry count directly under the stage directory.
  final int children;

  /// The owner file as the stager writes it: a regular file with a token.
  bool get ownerWritten => owner && ownerBytes > 0;
}

/// The stage directory [dir]; [root] names the staged folder inside
/// `payload` (only the kill round's hold file needs it).
_StageDir _stageDirState(String dir, {String root = _deepName}) {
  final stage = io.Directory(dir);
  if (!stage.existsSync()) {
    return const _StageDir(
      present: false,
      marker: false,
      owner: false,
      ownerBytes: 0,
      payload: false,
      holdBytes: -1,
      children: 0,
    );
  }
  final entries = stage.listSync(followLinks: false);
  final ownerFile = io.File('$dir/$_ownerName');
  final hold = io.File('$dir/payload/$root/$_holdName');
  return _StageDir(
    present: true,
    marker: _isRegularFile('$dir/$_markerName'),
    owner: _isRegularFile(ownerFile.path),
    ownerBytes: _isRegularFile(ownerFile.path) ? ownerFile.lengthSync() : 0,
    payload: entries.any(
        (e) => e is io.Directory && _basename(e.path) == 'payload'),
    holdBytes: _isRegularFile(hold.path) ? hold.lengthSync() : -1,
    children: entries.length,
  );
}

// ---------------------------------------------------------------------------
// Small helpers.
// ---------------------------------------------------------------------------

/// The text of the widget under [key] (the key's own text, else its texts
/// joined with " / "); empty when there is no such widget).
String _textAt(WidgetTester tester, String key) {
  final finder = find.byKey(ValueKey<String>(key));
  if (finder.evaluate().isEmpty) return '';
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

String _basename(String path) => path.substring(path.lastIndexOf('/') + 1);

Future<String> _sha256Of(io.File file) async =>
    (await crypto.sha256.bind(file.openRead()).first).toString();

String _sha256OfText(String text) =>
    crypto.sha256.convert(utf8.encode(text)).toString();

/// The staged tree under [root]: every entry by its path relative to it.
Future<Map<String, io.FileSystemEntity>> _walk(String root) async {
  final entries = <String, io.FileSystemEntity>{};
  await for (final entity
      in io.Directory(root).list(recursive: true, followLinks: false)) {
    entries[entity.path.substring(root.length + 1)] = entity;
  }
  return entries;
}

/// The staging root as Android lays it out: the application-support
/// directory is `<data dir>/files`, the staging root
/// `<data dir>/no_backup/ak-saf-staging`. Resolved through symlinks once it
/// exists: Android's `/data/user/0` is a link to `/data/data`, and the
/// stager answers the resolved form, so the unresolved spelling never
/// prefixes a staged path. Empty when the platform directory cannot be
/// resolved — which the parts that depend on it count as a failure.
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

/// The refusal dialog of the gateway, when one is up. Its copy is static
/// UI text and says why the folder was refused.
void _refusalDialog(WidgetTester tester, String part) {
  if (find.byKey(const ValueKey('staging_error_dialog')).evaluate().isEmpty) {
    return;
  }
  zSay('$part|ERROR_DIALOG|title=${_textAt(tester, 'staging_error_title')}'
      '|body=${_textAt(tester, 'staging_error_body')}');
}

/// A pick through the raw channel [method]: asks for the host's drive,
/// waits for it and hands back the tree URI — which never prints — or
/// null. No answer within the budget means no drive came.
Future<String?> _rawPick(_Run run, String part, String method,
    {int seconds = _pickSeconds}) async {
  zSay('$part|PICKER|tree|awaiting external DocumentsUI drive');
  final picked = await _settled(
      run, _storageChannel.invokeMethod<Object?>(method),
      seconds: seconds);
  if (!picked.answered) {
    run.pickerStalled = true;
  }
  final map = picked.value;
  final treeUri = map is Map && map['treeUri'] is String
      ? map['treeUri'] as String
      : null;
  final name = map is Map && map['displayName'] is String
      ? map['displayName'] as String
      : '';
  run.lastPickName = name;
  zSay('$part|PICKED|selected=${treeUri != null}'
      '|name=${map is Map ? map['displayName'] : ''}'
      '|uriIsTree=${treeUri?.startsWith('content://')}'
      '|threw=${picked.error == null ? 'no' : picked.verdict}');
  return treeUri;
}

Future<_Answer> _listGrants(_Run run) => _settled(
    run, _storageChannel.invokeMethod<Object?>('listSafPersisted'));

/// `rw`, `r-`, `-w` or `--` for the persisted grant of exactly [treeUri];
/// `absent` without one. The URIs themselves stay on the device.
Future<String> _grantFlags(_Run run, String treeUri) async =>
    _flagsIn(await _listGrants(run), treeUri);

String _flagsIn(_Answer listed, String treeUri) {
  final grants = listed.value;
  if (!listed.answered || grants is! List) return 'error:${listed.verdict}';
  for (final grant in grants) {
    if (grant is Map && grant['uri'] == treeUri) {
      return '${grant['read'] == true ? 'r' : '-'}'
          '${grant['write'] == true ? 'w' : '-'}';
    }
  }
  return 'absent';
}

/// How many persisted grants are READ-ONLY (as [listGrants] lists them):
/// what the send side adds, and only it; a receive directory holds
/// READ+WRITE. -1 when the list itself did not answer.
int _readOnlyIn(_Answer listed) {
  final grants = listed.value;
  if (!listed.answered || grants is! List) return -1;
  var left = 0;
  for (final grant in grants) {
    if (grant is Map && grant['read'] == true && grant['write'] != true) {
      left += 1;
    }
  }
  return left;
}

/// The same count as text for the evidence lines; `error:<verdict>` when
/// the list itself did not answer.
Future<String> _readOnlyGrants(_Run run) async {
  final listed = await _listGrants(run);
  final left = _readOnlyIn(listed);
  return left < 0 ? 'error:${listed.verdict}' : '$left';
}

// ---------------------------------------------------------------------------
// Part A — the real pick → stage → verify → release, through the production
// gateway.
// ---------------------------------------------------------------------------

Future<void> _partA(_Run run, String shaBig) async {
  final tester = run.tester;
  final failures = <String>[];
  void need(bool ok, String what) {
    if (!ok) failures.add(what);
  }

  void collect() {
    run.problems.addAll(<String>[for (final f in failures) 'A:$f']);
  }

  zSay('A|PICKER|tree|awaiting external DocumentsUI drive');
  final gateway = const PluginFilePickGateway();
  final pick = _Answer.capture(gateway.pickSendDirectory());
  if (!await _pumpUntil(tester, () => pick.answered, _pickSeconds)) {
    // No drive came: free the gateway from a dialog it may be waiting on.
    // The picker is left as it is — the host ends the round.
    run.pickerStalled = true;
    _refusalDialog(tester, 'A');
    need(false, 'pick-no-answer');
    zSay('A|CHECK|failed=${failures.join(',')}');
    final cancel = find.byKey(const ValueKey('staging_error_cancel_button'));
    if (cancel.evaluate().isNotEmpty) {
      await tester.tap(cancel);
      await _pumpUntil(tester, () => pick.answered, 15);
    }
    return collect();
  }

  final entry = pick.value as PickedEntry?;
  final id = entry?.stagingId;
  zSay('A|STAGED|name=${entry?.name}|files=${entry?.stagedFileCount}'
      '|bytes=${entry?.stagedByteCount}|staged=${id != null}'
      '|pathIsFs=${entry?.path.startsWith('/')}'
      '|threw=${pick.error == null ? 'no' : pick.verdict}');
  if (id != null) run.owned.add(id);
  if (entry == null || id == null) {
    need(false, 'pick-returned-no-staged-entry');
    _refusalDialog(tester, 'A');
    zSay('A|CHECK|failed=${failures.join(',')}');
    return collect();
  }

  final path = entry.path;
  // `<staging root>/<id>/payload/<name>`: the parent is `payload`, the
  // grandparent the stage directory, the great-grandparent the staging
  // root.
  final stageDir = io.Directory(path).parent.parent;
  final stagingRoot = stageDir.parent;
  run.stagingRoot = stagingRoot.path;
  need(entry.isDirectory, 'entry-is-directory');
  need(entry.name == _rootName, 'entry-name');
  need(
      path.startsWith('/') &&
          !path.contains('content:') &&
          !path.contains('msd:'),
      'entry-path');
  need(_basename(path) == entry.name, 'layout-name');
  need(_basename(io.Directory(path).parent.path) == 'payload',
      'layout-payload');
  need(_basename(stageDir.path) == id, 'layout-stage-dir');
  need(_idShape.hasMatch(id), 'id-format');
  need(entry.stagedFileCount == _sizes.length, 'file-count');
  need(entry.stagedByteCount == _expectedBytes, 'byte-count');

  final entries = await _walk(path);
  final rels = entries.keys.toList()..sort();
  for (final rel in rels) {
    final entity = entries[rel]!;
    // What the tree says must be here: a file, a directory, or nothing.
    final wanted = _sizes.containsKey(rel)
        ? 'file'
        : _directories.contains(rel)
            ? 'dir'
            : null;
    if (entity is io.Directory) {
      zSay('A|ENTRY|rel=$rel|kind=dir');
      // A directory where a file belongs is a wrong copy, not a skip: the
      // path set alone would still be equal.
      if (wanted != null && wanted != 'dir') need(false, 'kind:$rel(file)');
      continue;
    }
    if (entity is! io.File) {
      // The stager never writes a link: anything else is a defect to name.
      need(false, 'kind:$rel');
      zSay('A|ENTRY|rel=$rel|kind=other');
      continue;
    }
    if (wanted != null && wanted != 'file') need(false, 'kind:$rel(dir)');
    final size = await entity.length();
    final digest = await _sha256Of(entity);
    zSay('A|ENTRY|rel=$rel|kind=file|size=$size|sha256=$digest');
    if (size != _sizes[rel]) need(false, 'size:$rel');
    final expectedSha = rel == _bigRel
        ? shaBig
        : _texts.containsKey(rel)
            ? _sha256OfText(_texts[rel]!)
            : rel == _emptyRel
                ? _emptySha
                : null;
    if (expectedSha != null && digest != expectedSha) need(false, 'sha:$rel');
  }

  final expected = <String>{..._sizes.keys, ..._directories};
  final missing = expected
      .where((rel) => !entries.containsKey(rel))
      .toList()
    ..sort();
  final extra = entries.keys
      .where((rel) => !expected.contains(rel))
      .toList()
    ..sort();
  if (missing.isNotEmpty || extra.isNotEmpty) {
    need(false, 'rel-set');
    zSay('A|RELSET|missing=${missing.join(',')}|extra=${extra.join(',')}');
  }

  // Exactly one stage directory — the one just staged — and, besides it,
  // nothing but the grant journal the send side keeps in the staging root.
  final staged = _rootShape(stagingRoot.path);
  if (staged.stageDirs != 1 || staged.other != 0) {
    need(false, 'staging-root-shape(${staged.describe})');
  }
  // payload, the completion marker and the owner file of this process (N11)
  // — nothing else beside them.
  final stageChildren = <String>{
    for (final e in stageDir.listSync()) _basename(e.path),
  };
  zSay('A|STAGEDIR|children=${stageChildren.length}'
      '|payload=${stageChildren.contains('payload')}'
      '|marker=${stageChildren.contains(_markerName)}'
      '|owner=${stageChildren.contains(_ownerName)}');
  need(
      stageChildren.length == 3 &&
          stageChildren.contains('payload') &&
          stageChildren.contains(_markerName) &&
          stageChildren.contains(_ownerName),
      'stage-dir-contents(${stageChildren.length})');

  zSay('A|CHECK|${failures.isEmpty ? 'ok' : 'failed=${failures.join(',')}'}');

  await gateway.releaseStaged(<PickedEntry>[entry]);
  final gone = !stageDir.existsSync();
  final after = _rootShape(stagingRoot.path);
  zSay('A|RELEASED|stageDirGone=$gone|${after.describe}');
  if (!gone) need(false, 'release-stage-dir');
  if (!after.holdsNothing) need(false, 'release-staging-root(${after.describe})');

  // The derived root the E2E walkthrough lists (it never sees the staged
  // path) must resolve, and the staged copy must lie under it.
  final derived = await _derivedRoot();
  final under = derived.isNotEmpty && path.startsWith('$derived/');
  zSay('A|ROOT|derivedOk=$under|resolved=${derived.isNotEmpty}');
  if (derived.isEmpty) need(false, 'derived-root-unresolved');
  if (!under) need(false, 'derived-root-not-above-staged-path');
  zSay('A|DONE|failed=${failures.length}');
  collect();
}

// ---------------------------------------------------------------------------
// Part B — the failure paths through the raw channel.
// ---------------------------------------------------------------------------

Future<void> _partB(_Run run) async {
  final failures = <String>[];
  void need(bool ok, String what) {
    if (!ok) failures.add(what);
  }

  void collect() {
    run.problems.addAll(<String>[for (final f in failures) 'B:$f']);
  }

  final treeUri = await _rawPick(run, 'B', 'pickSendFolder');
  if (treeUri == null) {
    need(false, 'pick-no-tree');
    zSay('B|DONE|failed=${failures.length}');
    return collect();
  }
  run.treeUri = treeUri;

  Future<_Answer> stage(String uri, String stageId) =>
      _settled(run, _stageCall(uri, stageId));

  // The staging root as the platform spelled it in part A; without part A's
  // answer it is derived the way Android lays it out.
  final root = run.stagingRoot.isNotEmpty ? run.stagingRoot : await _derivedRoot();

  // A malformed id is refused before anything is read or written. The
  // answer is checked as code/token: both the code and the contract's
  // reason token must be exactly right.
  var code = (await stage(treeUri, 'not-an-id')).codeToken;
  zSay('B|BADID|code=$code');
  if (code != 'STAGE_INVALID_ID/format') need(false, 'badid-code:$code');

  // An authority outside the allow-list is refused before any grant.
  code = (await stage(
          'content://com.example.untrusted.documents/tree/x', run.newId()))
      .codeToken;
  zSay('B|UNTRUSTED|code=$code');
  if (code != 'SAF_PROVIDER_UNTRUSTED/authority') {
    need(false, 'untrusted-code:$code');
  }

  // A cancel must land while the copy is running: the tap is on the channel
  // BEFORE the staging call starts, and the cancel goes out at the FIRST
  // event whose phase is `copy`.
  final idB = run.newId();
  final tap = _ProgressTap();
  var cancelAtEvent = 0;
  var bytesAtCancel = -1;
  tap.onCopy = (event) {
    if (cancelAtEvent != 0 || event['stageId'] != idB) return;
    cancelAtEvent = tap.events;
    bytesAtCancel = event['bytesCopied'] as int? ?? -1;
    _Answer.capture(_storageChannel.invokeMethod<void>(
        'cancelStaging', <String, Object?>{'stageId': idB}));
  };
  tap.install();
  final cancelling = _Answer.capture(_stageCall(treeUri, idB));
  await _pumpUntil(run.tester, () => cancelling.answered, _workSeconds);
  // Right at the answer — before any explicit cleanup — a cancelled job
  // must have removed its own stage directory. Null: no staging root is
  // known, so there is nothing this test can say about the directory.
  final goneAfterCancel =
      root.isEmpty ? null : !io.Directory('$root/$idB').existsSync();
  zSay('B|CANCEL|code=${cancelling.codeToken}'
      '|stageDirGone=${goneAfterCancel ?? 'unknown'}'
      '|events=${tap.events}|lastPhase=${tap.lastPhase}'
      '|copyEvents=${tap.copyEvents(idB)}|cancelSentAtEvent=$cancelAtEvent'
      '|bytesAtCancel=$bytesAtCancel');
  if (cancelAtEvent == 0) need(false, 'cancel-no-copy-event');
  // It answers STAGE_CANCELLED only once its directory is gone; otherwise
  // the answer says what was left (`STAGE_IO_FAILED` / `leftover`).
  if (!_failedWith(cancelling, 'STAGE_CANCELLED', 'user')) {
    need(false, 'cancel-code:${cancelling.codeToken}');
  }
  if (root.isEmpty) need(false, 'no-staging-root');
  if (goneAfterCancel == false) need(false, 'cancel-stage-dir-left');

  // The cleanup answers `{"removed": true}` for an id that is already gone,
  // every time.
  final first = await _cleanup(run, idB);
  final second = await _cleanup(run, idB);
  zSay('B|IDEMPOTENT|first=${first.text}|second=${second.text}');
  if (!first.shapeOk || !second.shapeOk) {
    need(false, 'idempotent-shape:${first.text}/${second.text}');
  }
  if (first.removed != true || second.removed != true) {
    need(false, 'idempotent-value:${first.text}/${second.text}');
  }

  // The stale cleanup next to a RUNNING staging: it must not touch the
  // running job, and its answer must be the map the contract names.
  final idC = run.newId();
  final active = _Answer.capture(_stageCall(treeUri, idC));
  final stale = _Answer.capture(
      _storageChannel.invokeMethod<Object?>('cleanupStaleStaging'));
  await _pumpUntil(
      run.tester, () => active.answered && stale.answered, _workSeconds);
  final activeMap = active.value;
  final activePath = activeMap is Map && activeMap['path'] is String
      ? activeMap['path'] as String
      : null;
  final activeName = activeMap is Map && activeMap['displayName'] is String
      ? activeMap['displayName'] as String
      : _rootName;
  final staleMap = stale.value;
  final staleRemoved =
      stale.error == null && staleMap is Map && staleMap['removed'] is int
          ? staleMap['removed'] as int?
          : null;
  var payloadKept = false;
  var markerKept = false;
  var ownerKept = false;
  var stageChildren = 0;
  if (activePath != null) {
    final stageDir = io.Directory(activePath).parent.parent;
    final children = <String>{
      for (final e in stageDir.listSync()) _basename(e.path),
    };
    stageChildren = children.length;
    markerKept = children.contains(_markerName);
    ownerKept = children.contains(_ownerName);
    payloadKept =
        io.Directory('${stageDir.path}/payload/$activeName').existsSync();
  }
  zSay('B|ACTIVE'
      '|staleRemoved=${staleRemoved ?? stale.codeToken}'
      '|staleShape=${staleRemoved != null ? 'int' : 'bad'}'
      '|staged=${_statusOf(active)}'
      '|payloadKept=$payloadKept|markerKept=$markerKept'
      '|ownerKept=$ownerKept|stageDirChildren=$stageChildren');
  // Judged while the copy and its marker are still there — nothing has
  // cleaned idC yet. A delivered stage directory is payload, the completion
  // marker and the owner file: three entries (N11).
  if (staleRemoved == null || staleRemoved < 0) {
    need(false, 'active-stale-answer:${staleRemoved ?? stale.codeToken}');
  }
  if (_statusOf(active) != 'ok') {
    need(false, 'active-staged:${_statusOf(active)}');
  }
  if (!payloadKept || !markerKept || !ownerKept || stageChildren != 3) {
    need(false,
        'active-kept-copy-marker-owner:$payloadKept/$markerKept/$ownerKept/$stageChildren');
  }
  final cleanedC = await _cleanup(run, idC);
  final cGone =
      activePath != null && !io.Directory(activePath).parent.parent.existsSync();
  zSay('B|ACTIVE_CLEANED|code=${cleanedC.text}|stageDirGone=$cGone');
  if (!cleanedC.shapeOk || cleanedC.removed != true || !cGone) {
    need(false, 'active-cleanup:${cleanedC.text}/gone=$cGone');
  }

  // Every send-side staging of this part is over: no read-only grant may
  // be left. The journal is on record by size only.
  final left = await _readOnlyGrants(run);
  zSay('B|GRANT|left=$left|journal=${root.isEmpty ? 'na' : _journalState(root)}');
  if (left != '0') need(false, 'grant-left:$left');
  tap.remove();
  zSay('B|DONE|failed=${failures.length}');
  collect();
}

// ---------------------------------------------------------------------------
// Part C — the receive grant of the same folder survives a send.
// ---------------------------------------------------------------------------

Future<void> _partC(_Run run) async {
  final failures = <String>[];
  void need(bool ok, String what) {
    if (!ok) failures.add(what);
  }

  // The RECEIVE save-dir pick of the existing channel: it persists
  // READ+WRITE for the very same tree.
  final treeUri = await _rawPick(run, 'C', 'pickFolder');
  if (treeUri == null) {
    need(false, 'pick-no-tree');
    zSay('C|DONE|failed=${failures.length}');
    run.problems.addAll(<String>[for (final f in failures) 'C:$f']);
    return;
  }

  final before = await _grantFlags(run, treeUri);
  final idD = run.newId();
  // The host may have enlarged nested/photo.bin for part D while this
  // test waited on the picker: the staging gets the longer budget.
  final staged = await _settled(run, _stageCall(treeUri, idD),
      seconds: _bigWorkSeconds);
  final after = await _grantFlags(run, treeUri);
  zSay('C|STAGED|status=${_statusOf(staged)}');
  zSay('C|GRANT|before=$before|after=$after');
  if (before != 'rw') need(false, 'grant-before:$before');
  if (_statusOf(staged) != 'ok') need(false, 'staged:${_statusOf(staged)}');
  if (after != 'rw') need(false, 'grant-after:$after');
  final root = run.stagingRoot.isNotEmpty ? run.stagingRoot : await _derivedRoot();
  final cleaned = await _cleanup(run, idD);
  final dGone = root.isEmpty ? null : !io.Directory('$root/$idD').existsSync();
  zSay('C|CLEANED|${cleaned.text}|stageDirGone=${dGone ?? 'unknown'}');
  if (!cleaned.shapeOk) need(false, 'cleanup-shape:${cleaned.text}');
  if (root.isEmpty) need(false, 'no-staging-root');
  if (cleaned.removed != true || dGone == false) {
    need(false, 'cleanup:${cleaned.text}/gone=${dGone ?? 'unknown'}');
  }

  zSay('C|DONE|failed=${failures.length}');
  run.problems.addAll(<String>[for (final f in failures) 'C:$f']);
}

// ---------------------------------------------------------------------------
// Part D — the stager's own checks before the completion marker: the tree
// must still be the one it enumerated. The tree URI is part B's; the host
// changes the tree at each `D<n>|COPY` line.
// ---------------------------------------------------------------------------

/// One staging of part D under the tap. `<step>|START` goes out first;
/// `<step>|COPY` at the first copy event — the enumeration is over and the
/// copy began, which is the host's cue; `<step>|RESULT` once the call
/// answered. [atFirstCopy] runs right after the COPY line: the id is
/// registered and the copy has just begun.
Future<({_Answer answer, int? copyMs, int? afterCopyMs, int? estimate, int? entries})>
    _stagedUnderTap(_Run run, _ProgressTap tap, String step, String treeUri,
        String stageId, {void Function()? atFirstCopy}) async {
  zSay('$step|START');
  Map<Object?, Object?>? firstCopy;
  tap.onCopy = (event) {
    if (firstCopy != null || event['stageId'] != stageId) return;
    firstCopy = event;
    zSay('$step|COPY|entries=${event['entriesSeen']}'
        '|estimate=${event['totalBytesEstimate']}');
    atFirstCopy?.call();
  };
  final answer = _Answer.capture(_stageCall(treeUri, stageId));
  await _pumpUntil(run.tester, () => answer.answered, _bigWorkSeconds);
  tap.onCopy = null;
  final copyMs = tap.copyMs(stageId);
  final afterCopyMs = tap.afterCopyMs(stageId, answer);
  final last = tap.last(stageId);
  zSay('$step|RESULT|code=${answer.codeToken}'
      '|copySeen=${firstCopy != null}'
      '|copyMs=${copyMs ?? 'na'}|afterCopyMs=${afterCopyMs ?? 'na'}'
      '|copyEvents=${tap.copyEvents(stageId)}'
      '|filesCopied=${last?['filesCopied']}|bytesCopied=${last?['bytesCopied']}'
      '|estimate=${last?['totalBytesEstimate']}');
  final first = firstCopy;
  return (
    answer: answer,
    copyMs: copyMs,
    afterCopyMs: afterCopyMs,
    estimate: first == null ? null : first['totalBytesEstimate'] as int?,
    entries: first == null ? null : first['entriesSeen'] as int?,
  );
}

Future<void> _partD(_Run run) async {
  final failures = <String>[];
  void need(bool ok, String what) {
    if (!ok) failures.add(what);
  }

  final treeUri = run.treeUri;
  final root = run.stagingRoot.isNotEmpty ? run.stagingRoot : await _derivedRoot();
  final tap = _ProgressTap()..install();
  // Without a staging root the directories below cannot be looked at; that
  // is a failure of its own, and the directory checks say `unknown`.
  if (root.isEmpty) need(false, 'no-staging-root');

  // The cleanup after [step]: its answer must be the map with `removed:
  // true`, and the stage directory of [id] must be gone afterwards.
  Future<void> release(String step, String id, String what) async {
    final cleaned = await _cleanup(run, id);
    final gone = root.isEmpty ? null : !io.Directory('$root/$id').existsSync();
    zSay('$step|CLEANED|${cleaned.text}|stageDirGone=${gone ?? 'unknown'}');
    if (!cleaned.shapeOk) need(false, '$what:cleanup-shape:${cleaned.text}');
    if (cleaned.removed != true || gone == false) {
      need(false, '$what:cleanup:${cleaned.text}/gone=${gone ?? 'unknown'}');
    }
  }

  // D0 — the baseline: the same staging with NO intervention. It proves
  // the tree still stages, and it measures the copy phase the two
  // interventions have to land in (the host reads `copyMs`). It is also
  // T6: the one staging whose copy is long enough to act on while it runs.
  final idZ = run.newId();
  _Answer? targeted;
  final base = await _stagedUnderTap(run, tap, 'D0', treeUri, idZ,
      atFirstCopy: () {
    // T6: a cleanup aimed at the RUNNING baseline. The platform must refuse
    // it at call time — a cleanup queued behind the staging used to delete
    // the copy that staging had just delivered.
    targeted = _Answer.capture(_storageChannel.invokeMethod<Object?>(
        'cleanupStagedSend', <String, Object?>{'stageId': idZ}));
  });
  zSay('D0|BASELINE|status=${_statusOf(base.answer)}'
      '|copyMs=${base.copyMs ?? 'na'}|afterCopyMs=${base.afterCopyMs ?? 'na'}'
      '|estimate=${base.estimate ?? 'na'}|entries=${base.entries ?? 'na'}');
  if (_statusOf(base.answer) != 'ok') {
    need(false, 'd0:baseline:${base.answer.codeToken}');
  }
  // T6, judged through dart:io BEFORE the normal cleanup takes the copy.
  final busy = targeted;
  await busy?.wait(30);
  final busyAt = busy?.answeredAtMs;
  final copyEnd = tap.copyEndedAt(idZ);
  final busyBeforeCopyEnd =
      busyAt != null && copyEnd != null && busyAt <= copyEnd;
  final okMap = base.answer.value;
  final rootPath =
      okMap is Map && okMap['path'] is String ? okMap['path'] as String : null;
  final rootExists =
      rootPath != null && io.Directory(rootPath).existsSync();
  final delivered = root.isEmpty ? null : _stageDirState('$root/$idZ');
  zSay('D0|BUSY|code=${busy?.codeToken ?? 'not-requested'}'
      '|answeredBeforeCopyEnd=$busyBeforeCopyEnd'
      '|staged=${_statusOf(base.answer)}|rootExists=$rootExists'
      '|marker=${delivered?.marker}|owner=${delivered?.ownerWritten}'
      '|payload=${delivered?.payload}');
  if (busy == null || !_failedWith(busy, 'STAGE_IO_FAILED', 'busy')) {
    need(false, 'd0:busy:${busy?.codeToken ?? 'not-requested'}');
  }
  if (!busyBeforeCopyEnd) need(false, 'd0:busy-answered-after-the-copy');
  if (rootPath == null || !rootExists) need(false, 'd0:delivered-root-missing');
  if (delivered == null ||
      !delivered.marker ||
      !delivered.ownerWritten ||
      !delivered.payload) {
    need(false, 'd0:delivered-directory-incomplete');
  }
  await release('D0', idZ, 'd0');

  // D1 / D2: a staging under intervention.
  Future<void> judged(String step, String token) async {
    final what = 'd${step.substring(1)}';
    final id = run.newId();
    final result = await _stagedUnderTap(run, tap, step, treeUri, id);
    final hit = _failedWith(result.answer, 'STAGE_IO_FAILED', token);
    final gone = root.isEmpty ? null : !io.Directory('$root/$id').existsSync();
    zSay('$step|VERDICT|triggered=$hit|stageDirGone=${gone ?? 'unknown'}');
    // An intervention that landed too late lets the staging SUCCEED. That
    // is `not-triggered` — never a pass.
    if (!hit) need(false, '$what:not-triggered:${result.answer.codeToken}');
    // A failed job must have removed its own stage directory (else the
    // answer would have said `leftover`).
    if (hit && gone == false) need(false, '$what:stage-dir-left');
    await release(step, id, what);
  }

  await judged('D1', 'source-changed');
  await judged('D2', 'size-mismatch');

  // On record, not judged: with part C's read+write grant on the same tree
  // the send side takes no grant of its own here.
  final left = await _readOnlyGrants(run);
  zSay('D|GRANT|left=$left|journal=${root.isEmpty ? 'na' : _journalState(root)}');
  tap.remove();
  zSay('D|DONE|failed=${failures.length}');
  run.problems.addAll(<String>[for (final f in failures) 'D:$f']);
}

// ---------------------------------------------------------------------------
// Part E — T7: the receive side upgrades the grant DURING a send staging.
// The folder is nested/deep: a different persisted-grant URI from akstage
// (part C leaves READ+WRITE there) and one that holds no grant yet — both
// read through listSafPersisted before anything is staged.
//
// What is asserted is the END state: both calls succeeded, the URI holds
// read AND write, no READ-only grant is left anywhere, the journal line is
// gone. The receive take is serialized behind the running staging on the
// stager's worker, so the platform's order is "send release first, then
// receive take" — but no interleaving is asserted here, only the end state.
// That the pick was confirmed while the staging still copied is reported as
// timing evidence (E|TIMING).
// ---------------------------------------------------------------------------

Future<void> _partE(_Run run) async {
  final failures = <String>[];
  void need(bool ok, String what) {
    if (!ok) failures.add(what);
  }

  void collect() {
    run.problems.addAll(<String>[for (final f in failures) 'E:$f']);
  }

  final root = run.stagingRoot.isNotEmpty ? run.stagingRoot : await _derivedRoot();
  final journal = root.isEmpty ? 'na' : _journalState(root);

  // The send-side pick of nested/deep — one more drive. The host builds the
  // folder's payload while this waits.
  final deep =
      await _rawPick(run, 'E', 'pickSendFolder', seconds: _t7PickSeconds);
  if (deep == null) {
    need(false, 'pick-no-tree');
    zSay('E|DONE|failed=${failures.length}');
    return collect();
  }

  // What this part relies on, read first. A persisted grant belongs to one
  // exact URI: akstage's READ+WRITE (part C) is not a grant on nested/deep,
  // so the send staging of nested/deep will take READ of its own.
  final akstage = run.treeUri;
  need(run.lastPickName == _deepName, 'pick-wrong-folder:${run.lastPickName}');
  final akstageFlags =
      akstage == deep ? 'same-uri' : await _grantFlags(run, akstage);
  final deepBefore = await _grantFlags(run, deep);
  final readOnlyBefore = await _readOnlyGrants(run);
  zSay('E|PRE|deep=$deepBefore|akstage=$akstageFlags'
      '|readOnly=$readOnlyBefore|journal=$journal');
  need(deep != akstage, 'same-uri-as-akstage');
  need(deepBefore == 'absent', 'deep-holds-a-grant:$deepBefore');
  need(akstageFlags == 'rw', 'akstage-not-rw:$akstageFlags');
  need(readOnlyBefore == '0', 'read-only-before:$readOnlyBefore');

  // The send staging takes READ and journals it. While it copies, the
  // receive side picks the SAME folder: its take is a task queued behind
  // the running staging on the stager's worker.
  final idE = run.newId();
  final tap = _ProgressTap()..install();
  String? journalDuring;
  tap.onCopy = (event) {
    if (journalDuring != null || event['stageId'] != idE) return;
    // The take happened before the enumeration: by the first copy event
    // the journal carries the folder's line.
    journalDuring = root.isEmpty ? 'na' : _journalState(root);
    zSay('E|COPY|entries=${event['entriesSeen']}'
        '|estimate=${event['totalBytesEstimate']}|journal=$journalDuring');
  };
  zSay('E|START');
  final startedAt = _clock.elapsedMilliseconds;
  final staging = _Answer.capture(_stageCall(deep, idE));
  zSay('E|PICKER2|tree|awaiting external DocumentsUI drive');
  final pick = _Answer.capture(
      _storageChannel.invokeMethod<Object?>('pickFolder'));
  await _pumpUntil(
      run.tester, () => staging.answered && pick.answered, _t7WorkSeconds);
  tap.onCopy = null;
  final pickMap = pick.value;
  final pickedUri = pickMap is Map && pickMap['treeUri'] is String
      ? pickMap['treeUri'] as String
      : null;
  final stagingAt = staging.answeredAtMs;
  final pickAt = pick.answeredAtMs;
  final pickAfterStagingMs =
      stagingAt != null && pickAt != null ? pickAt - stagingAt : null;
  // Evidence, not a check: the receive take is queued behind the running
  // staging on the stager's worker, so a pick that was confirmed while the
  // copy ran answers right behind the staging — not seconds later.
  final answeredRightBehind = pickAfterStagingMs != null &&
      pickAfterStagingMs >= 0 &&
      pickAfterStagingMs <= _behindSlackMs;
  final copyMs = tap.copyMs(idE);
  zSay('E|PICKED2|selected=${pickedUri != null}|sameUri=${pickedUri == deep}'
      '|threw=${pick.error == null ? 'no' : pick.verdict}');
  zSay('E|STAGED|status=${_statusOf(staging)}|copyMs=${copyMs ?? 'na'}'
      '|stagingMs=${stagingAt != null ? stagingAt - startedAt : 'na'}'
      '|pickMs=${pickAt != null ? pickAt - startedAt : 'na'}'
      '|pickAfterStagingMs=${pickAfterStagingMs ?? 'na'}');
  zSay('E|TIMING|pickAnsweredRightBehindStaging=$answeredRightBehind'
      '|pickAfterStagingMs=${pickAfterStagingMs ?? 'na'}');

  // Both succeeded, and the grant is the receive side's now.
  final listedAfter = await _listGrants(run);
  final deepAfter = _flagsIn(listedAfter, deep);
  final readOnlyAfter = _readOnlyIn(listedAfter);
  final journalAfter = root.isEmpty ? 'na' : _journalState(root);
  zSay('E|AFTER|deep=$deepAfter|readOnly=$readOnlyAfter'
      '|journal=$journalAfter');
  tap.remove();

  need(_statusOf(staging) == 'ok', 'staged:${_statusOf(staging)}');
  final took = journalDuring != null &&
      journalDuring != 'absent' &&
      journalDuring != 'empty' &&
      journalDuring != 'na';
  if (!took) need(false, 'send-take-not-journalled:$journalDuring');
  if (pickedUri == null) need(false, 'pick-no-folder');
  if (pickedUri != deep) need(false, 'pick-other-folder');
  // The END state is what this part asserts: with the receive take
  // serialized behind the running staging, the platform's order is "send
  // release first, then receive take", and no interleaving is asserted.
  if (deepAfter != 'rw') need(false, 'deep-after:$deepAfter');
  if (readOnlyAfter != 0) need(false, 'read-only-after:$readOnlyAfter');
  if (journalAfter != 'absent' && journalAfter != 'empty') {
    need(false, 'journal-after:$journalAfter');
  }

  final cleaned = await _cleanup(run, idE);
  final gone = root.isEmpty ? null : !io.Directory('$root/$idE').existsSync();
  zSay('E|CLEANED|${cleaned.text}|stageDirGone=${gone ?? 'unknown'}');
  if (!cleaned.shapeOk) need(false, 'cleanup-shape:${cleaned.text}');
  if (root.isEmpty) need(false, 'no-staging-root');
  if (cleaned.removed != true || gone == false) {
    need(false, 'cleanup:${cleaned.text}/gone=${gone ?? 'unknown'}');
  }

  zSay('E|DONE|failed=${failures.length}');
  collect();
}

// ---------------------------------------------------------------------------
// The kill round, launch 1 (`leave`): a send staging of nested/deep, and the
// line at which the host stops the app mid-copy. Nothing is released here
// on purpose — what the staging has on disk when it dies is the point.
// ---------------------------------------------------------------------------

Future<void> _leaveRun(WidgetTester tester) async {
  zSay('K|MODE|leave|android=${io.Platform.isAndroid}');
  expect(io.Platform.isAndroid, isTrue,
      reason: 'the kill round needs the Android channel');

  await tester.pumpWidget(
      const MaterialApp(home: Scaffold(body: SizedBox.shrink())));
  final run = _Run(tester);
  final root = await _derivedRoot();
  final tap = _ProgressTap()..install();

  final deep = await _rawPick(run, 'K', 'pickSendFolder');
  if (deep == null) {
    // Nothing is started; the host reads this and ends the launch.
    zSay('K|ABORT|pick-returned-nothing');
    return;
  }

  // A fresh install is what this round is about: the folder must hold no
  // grant yet, and no READ-only grant may exist anywhere. And the host's
  // drive must have ended in nested/deep.
  final listed = await _listGrants(run);
  final deepBefore = _flagsIn(listed, deep);
  final readOnlyBefore = _readOnlyIn(listed);
  zSay('K|START|deep=$deepBefore|readOnly=$readOnlyBefore'
      '|journal=${root.isEmpty ? 'na' : _journalState(root)}');
  expect(run.lastPickName, _deepName,
      reason: 'the picker did not end in nested/deep (${run.lastPickName})');
  expect(deepBefore, 'absent',
      reason: 'nested/deep already holds a persisted grant ($deepBefore) — '
          'the kill round needs a fresh install');
  expect(readOnlyBefore, 0,
      reason: 'a READ-only grant exists before the staging — not a fresh install');

  final id = run.newId();
  var seen = false;
  tap.onCopy = (event) {
    if (seen || event['stageId'] != id) return;
    seen = true;
    // This line goes out FIRST — the host stops the app the moment it has
    // it. Then what the stager has on disk at this very moment: what the
    // next process finds if the stop lands now.
    zSay('K|COPY|entries=${event['entriesSeen']}'
        '|estimate=${event['totalBytesEstimate']}');
    final state = root.isEmpty ? null : _stageDirState('$root/$id');
    zSay('K|HOLD|marker=${state?.marker}|owner=${state?.ownerWritten}'
        '|payload=${state?.payload}|holdBytes=${state?.holdBytes}'
        '|journalLines=${root.isEmpty ? 'na' : _journalLines(root)}');
  };
  final staging = _Answer.capture(_stageCall(deep, id));
  // The app is stopped while this waits: the call normally never answers.
  await _pumpUntil(tester, () => staging.answered, _t7WorkSeconds);
  tap.onCopy = null;
  zSay('K|COMPLETED|code=${staging.codeToken}|copyMs=${tap.copyMs(id) ?? 'na'}');
  fail('the staging completed before the app was stopped — the kill landed too late');
}

// ---------------------------------------------------------------------------
// The kill round, launch 2 (`check`): the same install, a new process. What
// the stopped staging left, then the stale sweep, then nothing.
//
// The staging root and listSafPersisted are read BEFORE
// cleanupStaleStaging is called, and nothing has swept before that: the
// app's start-up cleanup lives in AkSendingApp, which this mode never pumps
// (nothing is pumped here at all), and the native side sweeps only through
// the channel method. So the test sees exactly what the stopped process
// left; an app start would have swept it first.
// ---------------------------------------------------------------------------

Future<void> _checkRun(WidgetTester tester) async {
  // The size of nested/deep/hold.bin as the host made it, when it says so.
  const holdFull =
      int.fromEnvironment('AK_SAF_PROBE_HOLD_BYTES', defaultValue: 0);
  final failures = <String>[];
  void need(bool ok, String what) {
    if (!ok) failures.add(what);
  }

  zSay('K|MODE|check|android=${io.Platform.isAndroid}|holdFull=$holdFull');
  expect(io.Platform.isAndroid, isTrue,
      reason: 'the kill round needs the Android channel');

  final root = await _derivedRoot();
  if (root.isEmpty) {
    need(false, 'root-unresolved');
    zSay('K|BEFORE|root=unresolved');
  } else {
    // BEFORE the sweep: exactly one PARTIAL stage directory with the dead
    // process's owner file, the journal line, and one READ-only grant.
    final before = _rootShape(root);
    final stageDirs = <io.Directory>[
      if (io.Directory(root).existsSync())
        for (final e in io.Directory(root).listSync(followLinks: false))
          if (e is io.Directory) e,
    ];
    final one =
        stageDirs.length == 1 ? _stageDirState(stageDirs.single.path) : null;
    final listedBefore = await _call(
        _storageChannel.invokeMethod<Object?>('listSafPersisted'));
    final readOnlyBefore = _readOnlyIn(listedBefore);
    final grantsBefore =
        listedBefore.value is List ? (listedBefore.value as List).length : -1;
    zSay('K|BEFORE|${before.describe}|journalLines=${_journalLines(root)}'
        '|marker=${one?.marker}|owner=${one == null ? 'absent' : one.ownerWritten}'
        '|ownerBytes=${one?.ownerBytes}|payload=${one?.payload}'
        '|holdBytes=${one?.holdBytes}|grants=$grantsBefore'
        '|readOnly=$readOnlyBefore');
    if (!listedBefore.answered || listedBefore.error != null) {
      need(false, 'before-list:${listedBefore.codeToken}');
    }
    if (before.stageDirs != 1) {
      need(false, 'before-stage-dirs:${before.stageDirs}');
    }
    if (one == null) {
      need(false, 'before-no-stage-directory');
    } else {
      if (one.marker) need(false, 'before-delivered-not-partial');
      if (!one.payload) need(false, 'before-no-payload');
      if (!one.ownerWritten) {
        need(false, 'before-owner-file:${one.owner}/${one.ownerBytes}');
      }
      if (holdFull > 0) {
        if (one.holdBytes < 0) need(false, 'before-no-hold-file');
        if (one.holdBytes >= holdFull) {
          need(false, 'before-copy-complete:${one.holdBytes}/$holdFull');
        }
      }
    }
    if (_journalLines(root) != 1) {
      need(false, 'before-journal-lines:${_journalLines(root)}');
    }
    if (readOnlyBefore != 1) need(false, 'before-read-only:$readOnlyBefore');
  }

  // The sweep — what the app start would have run.
  final sweep = await _call(
      _storageChannel.invokeMethod<Object?>('cleanupStaleStaging'));
  final sweepMap = sweep.value;
  final removed = sweep.error == null && sweepMap is Map && sweepMap['removed'] is int
      ? sweepMap['removed'] as int?
      : null;
  zSay('K|SWEEP|removed=${removed ?? sweep.codeToken}'
      '|shape=${removed != null ? 'int' : 'bad'}');
  if (removed == null) {
    need(false, 'sweep-shape:${sweep.codeToken}');
  } else if (removed < 1) {
    need(false, 'sweep-removed:$removed');
  }

  // AFTER: the staging root holds nothing, and no READ-only grant remains.
  final listedAfter = await _call(
      _storageChannel.invokeMethod<Object?>('listSafPersisted'));
  final readOnlyAfter = _readOnlyIn(listedAfter);
  if (root.isEmpty) {
    zSay('K|AFTER|root=unresolved|readOnly=$readOnlyAfter');
  } else {
    final after = _rootShape(root);
    zSay('K|AFTER|${after.describe}|journalLines=${_journalLines(root)}'
        '|readOnly=$readOnlyAfter');
    if (!after.clear) need(false, 'after-not-clear:${after.describe}');
  }
  if (readOnlyAfter != 0) need(false, 'after-read-only:$readOnlyAfter');

  zSay('K|DONE|failed=${failures.length}');
  expect(failures, isEmpty, reason: 'failed checks: ${failures.join('; ')}');
}

