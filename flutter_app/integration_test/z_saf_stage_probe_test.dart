// Z probe (batch Z, 2026-11-24; part D and the hard assertions of revision 3
// and T5–T8 of revision 4; revision 5 — no value in the output, the journal's
// four answers, T7 as two orders and part B's RETAKE, 2026-11-25; revision 6
// — E1's native order read from the stager's own trace, the journal's own
// line, and no exception in the output): the
// ANDROID SEND-SIDE folder path against the REAL platform in a profile
// build — the system tree picker (ACTION_OPEN_DOCUMENT_TREE), the native
// staging behind `dev.ak/storage`, the content of the staged copy, its
// release, and the failure paths of the channel. The Flutter side is driven
// by this test; the NATIVE picker is driven from the host (adb/uiautomator),
// for the reasons recorded in the header of the K1 walkthrough
// (k1_android_saf_test.dart).
//
// Three builds of this one file, chosen by dart-define AK_SAF_PROBE_MODE:
//
//   (empty) — the whole probe, z-probe.sh. Parts in order:
//     A  the PRODUCTION gateway, no injected seam: pick akstage → stage →
//        verify the staged tree → release.
//     B  the raw channel's failure paths: a malformed id, an untrusted
//        authority, a cancel that must land in the copy phase, an idempotent
//        cleanup. Then the RETAKE of the API 29 round: once the cancelled
//        job has released its READ, one more staging of the SAME tree URI
//        and no new pick — `ok` on API 30 and later, SAF_PERMISSION_DENIED
//        on API 24–29, where that release took READ out of what a later
//        take may persist. Then a FRESH pick (`B|PICKER2`) and the stale
//        cleanup next to a RUNNING staging.
//     C  the receive grant of the akstage tree survives a send staging (the
//        receive pick leaves READ+WRITE there; it stays for the rest).
//     D  the stager's own checks before its completion marker — a directory
//        that gained a child while the copy ran (source-changed), a file
//        that lost its enumerated size (size-mismatch) — and, on the
//        baseline staging, T6: a cleanup aimed at a RUNNING staging is
//        refused with busy at call time, and the staging still delivers.
//     E1 T7's first order, the one that loses the grant on Android 7–10
//        unless no release falls between a receive pick's grant and its
//        take (G15): the receive side upgrades the grant DURING a send
//        staging, on a folder that holds no grant yet (nested/deep — a
//        different URI from akstage; verified first through
//        listSafPersisted). Its NATIVE ORDER is judged from the stager's
//        own trace (the diagnostic channel method safGrantTrace, X1: each
//        entry a sequence number, one of six labels and an alias of the
//        tree — never a URI): `native=strict` needs `pick_result < job_end
//        < settle_refused < receive_take` under one alias and no `release`
//        or `settle_begin` of it in between. That catches a stager that has
//        the receive pick REGISTERED and still admits the settlement
//        (`native=not-held`, the failed check `native-not-held`). It does
//        NOT catch one that registers only when the result arrives: while
//        that result still precedes the send wrap-up it traces the same
//        five entries and ends `rw`. E2 — the picker parked, the pick
//        registered and unconfirmed — is what catches a late registration.
//        A window that cannot be judged is `native=not-exercised`: no
//        device failure, the host's to call inconclusive. The journal line
//        at the first copy event must be the folder's own (`line=target`).
//     E2 T7's second order, the one that tells a broken deferral from a
//        correct one on Android 15: `pickFolder` FIRST, the picker parked by
//        the host on nested/dip (no grant there either) while a send staging
//        of that folder ends. The tree with the pick registered and not yet
//        taken is the part's first observation, the tree after the confirm
//        its second. nested/dip's READ+WRITE stays for the rest of the round
//        (no channel method releases it); a fresh install clears it.
//   leave — z-probe-kill.sh, first launch: pick nested/deep, start a send
//        staging, print `ZSAF|K|COPY` at its first copy event; the host
//        force-stops the app mid-copy.
//   check — z-probe-kill.sh, second launch, same install: what the killed
//        process left (through dart:io), then cleanupStaleStaging, then
//        nothing — after which no persisted grant exists at all.
//
// Orchestration contract:
//   * dart-define AK_SAF_PROBE_SHA_BIG=<64 hex> (default mode): the host's
//     SHA-256 of the 32 MiB `nested/photo.bin` it pushed to
//     /sdcard/Download/akstage.
//   * dart-define AK_SAF_PROBE_API=<n> (default mode): the SDK level of the
//     image as the host read it (`getprop ro.build.version.sdk`). It is the
//     only source of the API level part B's RETAKE expects by: the
//     platform's version string is not a release string on Android (on the
//     x86_64 image its first number is 64 — seen on the device), so it names
//     nothing (see _apiLevelOf). Without this define RETAKE fails by name.
//   * every evidence line `ZSAF|<part>|<PICKER…>|tree|awaiting external
//     DocumentsUI drive` (logcat, tag `flutter`) asks the host to drive
//     DocumentsUI onto the folder the contract names here — never onto one a
//     line printed — and to confirm it, except where it says to park:
//     parts A, B and C onto akstage, and B once more for its fresh pick
//     (`B|PICKER2`); E1 onto akstage/nested/deep (twice: `E1|PICKER` and
//     `E1|PICKER2`); E2 onto akstage/nested/dip — `E2|PICKER0` is confirmed
//     like the others, `E2|PICKER` is PARKED on the folder (its row is
//     tapped, "Use this folder" is not) until `ZSAF|E2|HOLD|…` is in logcat,
//     and only then confirmed ("Use this folder", then "Allow"); the leave
//     mode onto akstage/nested/deep.
//   * the synthetic tree, whose host manifest is the reference for part A:
//       nested/deep/note.txt   "ak stage probe" + LF
//       nested/photo.bin       33554432 random bytes
//       empty.txt              0 bytes
//       emptydir/              an empty directory
//       sub/说明 文本.txt       "中文 名字" + LF
//     The host changes it only after part A has verified it. It enlarges
//     nested/photo.bin while the test waits on part C's picker (a longer
//     copy phase) and changes the tree at each `ZSAF|D<n>|COPY` line — the
//     device's own "enumeration over, the copy began". For part E1 it puts a
//     big file and many small ones into nested/deep while the test waits on
//     `E1|PICKER`, so that the send staging outlasts the second picker. For
//     part E2 it builds, while the test waits on `E2|PICKER0`,
//       nested/dip/note.txt    "dip note" + LF — a few bytes, so that the
//                              folder's send staging is over almost at once
//     It restores the tree once the drive has ended (nested/dip goes with
//     the rest).
//
// Evidence is one `ZSAF|…` line per fact, and no line carries a value read
// from the device: not a display name, not the relative name of a staged
// entry, not a tree URI, not a stage id, not journal content. What stands
// in are preset synthetic labels, match booleans and counts of unknown
// items; a failing check names the label it expected or the state it saw
// (a grant's bits, a journal's state) — never what the device answered.
// Nor does any line, or any failing check, carry an exception: a call on
// dart:io that throws — a look, a listing, a length, opening or reading the
// digest's stream, the journal's bytes — is caught where it is made and
// turns into a fixed check name, and a failed read is never taken for a
// directory that is gone, a journal that is clear or a digest that matches.
// The same holds inside the progress hook and in the teardown, where an
// exception would otherwise be printed in full.
// Every check is HARD — a failed one is collected under its name, the run
// goes on to gather the rest, and the test fails on the collected list at
// the end.
//
// No wait that spans the system picker waits for a Flutter frame: while
// DocumentsUI is in front the app's are paused, and a `pump()` issued then
// stays open until the picker closes — at the end of a failed round that is
// what died with `Guarded function conflict` (the round of 09:27).
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
/// of a delivered copy; the owner file every stage directory of a stager instance
/// carries (revision 4, N11).
const String _journalName = '.send-grants';
const String _journalScratchName = '.send-grants.tmp';
const String _markerName = '.ak-stage-complete';
const String _ownerName = '.owner';

/// The folder of part E1 and of the kill round — nested/deep — and the big
/// file the host puts into it to keep a staging busy.
const String _deepName = 'deep';
const String _holdName = 'hold.bin';

/// The folder of part E2 — nested/dip, which the host builds: a note of a
/// few bytes and nothing else, so that its send staging is over almost at
/// once.
const String _dipName = 'dip';

/// The mode of this build (dart-define AK_SAF_PROBE_MODE): empty = the
/// whole probe; `leave` and `check` are the two launches of the kill round.
const String _mode = String.fromEnvironment('AK_SAF_PROBE_MODE');

/// How long a pick may stay open. The host has seen the PICKER line by
/// then, so this only bounds a picker drive that never comes — and the
/// tree preparation the host does while the test waits on a picker.
const int _pickSeconds = 240;

/// Part E1's first pick: the host also moves nested/deep's payload in, in
/// that window (a big file, and thousands of small ones).
const int _t7PickSeconds = 300;

const int _workSeconds = 120;

/// A staging that copies the enlarged file: its budget is its own.
const int _bigWorkSeconds = 180;

/// Part E1's staging copies the big file AND thousands of small ones.
const int _t7WorkSeconds = 300;

/// For part E1's ORDER evidence only: how long after the send staging's
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
      // The whole default run: 11 to 14 minutes on the emulators these rounds
      // were written on. The emulator can be much slower (it is itself a
      // guest of a cloud machine whose speed varies): on 2026-11-27 a run
      // whose every part answered was cut at 20 minutes. Each part's waits
      // have their own budgets; this cap only catches a run that hangs.
      testWidgets(
          'Z SAF: tree pick, native staging, staged-tree verification, release and failure paths',
          _defaultRun,
          timeout: const Timeout(Duration(minutes: 45)));
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

/// The whole probe: parts A–E2, then the verdict on every collected check.
Future<void> _defaultRun(WidgetTester tester) async {
  const shaBig = String.fromEnvironment('AK_SAF_PROBE_SHA_BIG');
  final api = _apiLevel();
  zSay('BOOTSTRAP|android=${io.Platform.isAndroid}|mode=default'
      '|shaBig=${shaBig.length}hex|api=${api.level ?? 'unreadable'}'
      '|apiFrom=${api.source}'
      '${api.source == 'os' ? '' : '|osFirst=${api.osFirst ?? 'none'}'}');
  expect(shaBig, matches(RegExp(r'^[0-9a-f]{64}$')),
      reason: 'AK_SAF_PROBE_SHA_BIG (64 lowercase hex) is required');
  expect(io.Platform.isAndroid, isTrue,
      reason: 'the probe needs the Android channel');

  final run = _Run(tester)..apiLevel = api.level;
  // Without an API level part B's RETAKE would expect one of two answers at
  // random: it fails by name instead. Said here, at the start, so that the
  // run's verdict carries it whatever the parts find.
  if (run.apiLevel == null) run.problems.add('BOOTSTRAP:api-level-unreadable');
  addTearDown(() async {
    // Whatever went wrong, no staged copy of this run stays behind. For an
    // id whose staging is still in flight the platform answers `busy` at
    // call time and deletes nothing; that copy is left to the next start's
    // stale sweep, exactly as a killed process would leave it.
    final teardown = await _teardown(run);
    zSay('TEARDOWN|${teardown.describe}');
    // Both counts must be 0 for the test to pass, and no step may have
    // thrown (an exception in a tearDown callback fails the test — the
    // test runner's own rule — and would be printed in full).
    expect(teardown.threw, isEmpty,
        reason: 'teardown steps that threw: ${teardown.threw.join(',')}');
    expect(teardown.released?.unanswered ?? 0, 0,
        reason: 'cleanups that did not answer within 30 s');
    expect(teardown.released?.notRemoved ?? 0, 0,
        reason: 'cleanups that did not answer {"removed": true}');
  });

  // The production gateway reaches the root navigator for its dialogs.
  await tester.pumpWidget(
      const MaterialApp(home: Scaffold(body: SizedBox.shrink())));

  await _partA(run, shaBig);
  if (!run.pickerStalled) await _partB(run);
  if (!run.pickerStalled) await _partC(run);
  if (!run.pickerStalled && run.treeUri.isNotEmpty) await _partD(run);
  if (!run.pickerStalled && run.treeUri.isNotEmpty) await _partE1(run);
  if (!run.pickerStalled && run.treeUri.isNotEmpty) {
    if (run.pickerLeftOpen) {
      // A receive pick that never answered leaves its picker in front; a
      // picker drive of part E2 would land in that one.
      zSay('E2|SKIPPED|reason=picker-left-open');
      run.problems.add('E2:skipped-picker-left-open');
    } else {
      await _partE2(run);
    }
  }

  // Nothing may be left waiting for a frame when the teardown runs. A pump
  // that is still open holds the test API's guarded scope, and the runner's
  // own last guarded call then fails with `Guarded function conflict`
  // instead of the verdict — the 09:27 round. Every wait that spans the
  // system picker is frame-free for that reason; this only proves it.
  expect(_openPump, isNull, reason: 'a pump is still open at the end of the run');
  zSay('VERDICT|${run.problems.isEmpty ? 'ok' : 'failed=${run.problems.length}'}');
  expect(run.problems, isEmpty,
      reason: 'failed checks: ${run.problems.join('; ')}');
}

void zSay(String value) {
  // ignore: avoid_print
  print('ZSAF|$value');
}

/// What the teardown's own cleanups answered ([_Run.releaseOwned]).
typedef _Released = ({
  int ids,
  int done,
  int unanswered,
  int notRemoved,
  int busy,
});

/// The teardown, step by step. A step that threw is NAMED in [threw] — its
/// exception is never printed; [released] is what the cleanups answered,
/// null when even that could not be gathered; [describe] is the evidence
/// line's fields: fixed words and counts.
Future<({String describe, List<String> threw, _Released? released})>
    _teardown(_Run run) async {
  final threw = <String>[];
  _Released? released;
  try {
    released = await run.releaseOwned();
  } on Object {
    threw.add('release');
  }
  try {
    // A wait that failed by its open pump leaves that pump here. Let it
    // finish first: the expects that follow are guarded, and a guarded call
    // beside an open pump is a "Guarded function conflict" that buries the
    // real failure.
    final open = _openPump;
    if (open != null) {
      await Future.any<void>(<Future<void>>[
        open,
        Future<void>.delayed(const Duration(seconds: 60)),
      ]);
    }
  } on Object {
    threw.add('pump');
  }
  try {
    _storageChannel.setMethodCallHandler(null);
  } on Object {
    threw.add('tap');
  }
  final done = released;
  return (
    describe: 'ids=${done?.ids ?? 'na'}|done=${done?.done ?? 'na'}'
        '|unanswered=${done?.unanswered ?? 'na'}'
        '|not-removed=${done?.notRemoved ?? 'na'}|busy=${done?.busy ?? 'na'}'
        '|threw=${threw.isEmpty ? 'no' : threw.join(',')}',
    threw: threw,
    released: done,
  );
}

/// A snapshot of the stager's trace ring — the diagnostic channel method
/// `safGrantTrace` (X1). Like [_call] it pumps nothing: part E1 reads it
/// while the system picker may still be in front.
Future<_Answer> _traceRead() =>
    _call(_storageChannel.invokeMethod<Object?>('safGrantTrace'));

/// How a read of the ring went, as one fixed fact when it did not answer a
/// ring — `<which read>-<what>`: `no-answer`, `unavailable` (there is no
/// stager), `error` (any other PlatformException), `threw`, `bad-shape`
/// (anything but a list of strings) — and null when it did. No code, no
/// message. [which] is `before` or `after`.
String? _traceReadFact(_Answer read, String which) {
  if (!read.answered) return '$which-no-answer';
  final failure = read.error;
  if (failure != null) {
    if (failure is PlatformException) {
      return failure.code == 'unavailable'
          ? '$which-unavailable'
          : '$which-error';
    }
    return '$which-threw';
  }
  return _traceRing(read.value) == null ? '$which-bad-shape' : null;
}

/// What one run collects: the failed checks under their names, every stage
/// id this run may leave behind, the staging root and the tree URIs the
/// later parts work with, and whether a picker drive never came — the parts
/// after that cannot run.
class _Run {
  _Run(this.tester);

  final WidgetTester tester;
  final List<String> problems = <String>[];
  final Set<String> owned = <String>{};

  /// The staging root as the platform spelled it in part A's answer.
  String stagingRoot = '';

  /// The tree URI of part B's pick — never printed.
  String treeUri = '';

  /// The tree URI of part E1's folder — never printed.
  String deepUri = '';

  /// The display name of the last pick, as the platform answered it: which
  /// folder the host's DocumentsUI drive really ended in. Compared with the
  /// label the contract names, and never printed.
  String lastPickName = '';

  /// The API level of this image ([_apiLevelOf]); null when no source says.
  int? apiLevel;

  /// A picker drive never came: the parts that need one cannot run.
  bool pickerStalled = false;

  /// A receive pick never answered, so its picker may still be in front.
  bool pickerLeftOpen = false;

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

  /// The PlatformException code; '' for anything else.
  String get code {
    final failure = error;
    return failure is PlatformException ? failure.code : '';
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
  /// no frame to wait for, and neither has a wait that spans the system
  /// picker. Answers whether the call DID answer: a caller must not take
  /// silence for one.
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
/// completes it. No caller may span the system picker with it: a wait that
/// does is written on [_Answer.wait], which needs no frame at all.
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
      Future<void>.delayed(_pumpPatience),
    ]);
  }
  if (_openPump != null) {
    fail('a pump was still stalled ${_pumpPatience.inSeconds} s after the '
        'wait ended: no frame');
  }
  return ready();
}

/// How long a wait that is satisfied keeps waiting for the one pump it opened.
/// On a fast image the pump is over in a frame; on the Android 10 (API 29)
/// emulator in software rendering a frame takes seconds — a wait of the round
/// of 2026-11-25 21:21 saw none for more than 20 s there. The pump must be
/// over before the caller goes on: an open one keeps the guarded test API.
const Duration _pumpPatience = Duration(seconds: 120);

/// Awaits [call] (pumping meanwhile) and hands its captured answer. A call
/// that copies the enlarged file gets [seconds]. Only for a moment at which
/// the app is in front — never across a pick.
Future<_Answer> _settled(_Run run, Future<Object?> call,
    {int seconds = _workSeconds}) async {
  final answer = _Answer.capture(call);
  await _pumpUntil(run.tester, () => answer.answered, seconds);
  return answer;
}

/// A channel call that pumps nothing — in a mode without frames (the kill
/// round's `check`), in a teardown, and wherever the system picker may be
/// in front. Whether it answered is for the caller to read off the answer —
/// silence is never taken for an answer.
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

/// A cleanup answer judged by what the platform is contracted to answer: a
/// map with the boolean `removed` — `true` also when the stage directory is
/// already gone, `false` when something is left behind. For an id whose
/// staging is registered it answers `STAGE_IO_FAILED` / `busy` at call
/// time.
({String text, bool shapeOk, bool? removed}) _cleanupAnswer(_Answer answer) {
  final removed = _removedOf(answer);
  return (
    text: removed != null ? 'removed=$removed' : answer.codeToken,
    shapeOk: removed != null,
    removed: removed,
  );
}

/// A direct `cleanupStagedSend` while the app is in front.
Future<({String text, bool shapeOk, bool? removed})> _cleanup(
    _Run run, String stageId) async {
  final answer = await _settled(
      run,
      _storageChannel.invokeMethod<Object?>(
          'cleanupStagedSend', <String, Object?>{'stageId': stageId}));
  return _cleanupAnswer(answer);
}

/// The same call where no frame may be waited for (parts E1 and E2: the
/// system picker may still be in front when the part ends).
Future<({String text, bool shapeOk, bool? removed})> _cleanupNoFrame(
    String stageId) async {
  final answer = await _call(_storageChannel.invokeMethod<Object?>(
      'cleanupStagedSend', <String, Object?>{'stageId': stageId}));
  return _cleanupAnswer(answer);
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
  var _hookFailures = 0;
  String lastPhase = 'none';

  /// Runs on every copy-phase event, with the event as the platform sent it.
  /// An exception in it never leaves the tap: Flutter would print it in
  /// full. It is counted, and the part that installed the hook fails under
  /// a fixed name.
  void Function(Map<Object?, Object?> event)? onCopy;

  int get events => _events;

  /// How many copy events the tap could not get through — the hook threw,
  /// or the tap's own bookkeeping did.
  int get hookFailures => _hookFailures;
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
  /// Nothing that happens in the handler escapes as an exception.
  void install() {
    _storageChannel.setMethodCallHandler((MethodCall call) async {
      try {
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
      } on Object {
        _hookFailures += 1;
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
/// (revision 3, N2): a regular file that is deleted once every grant it
/// recorded is released. So "nothing left" means no stage directory, no
/// other entry and no journal; the journal is reported on its own, by state
/// only — its lines are tree URIs.
class _RootShape {
  const _RootShape({
    required this.exists,
    required this.children,
    required this.stageDirs,
    required this.other,
    required this.scratch,
    required this.journal,
    required this.read,
  });

  /// The root exists: null when the look itself failed — never "gone".
  final bool? exists;

  /// Every entry directly under the staging root.
  final int children;

  /// Real directories — one staged copy each (their names are stage ids).
  final int stageDirs;

  /// Everything that is neither a stage directory nor the journal.
  final int other;

  /// The journal's scratch file is still there. It exists only while the
  /// journal is being rewritten, which is over before a staging answers.
  final bool scratch;

  /// `absent`, `empty`, `lines:<n>`, `not-a-file` or `unreadable`.
  final String journal;

  /// Whether all of this could be read: `ok`; `look-failed` — the root
  /// itself could not be looked at; `list-failed` — it could not be listed.
  /// After a failed look nothing here may be read as "nothing is left".
  final String read;

  /// No stage directory and nothing else: the root holds no copy.
  bool get holdsNothing =>
      read == 'ok' && exists == true && stageDirs == 0 && other == 0;

  /// [holdsNothing], with nothing journaled and the scratch file gone.
  bool get clear =>
      holdsNothing && !scratch && _journalClear(journal);

  /// Fixed words, booleans and counts; `na` / `unknown` for what a failed
  /// look left unread.
  String get describe => read == 'ok'
      ? 'exists=$exists|children=$children|stageDirs=$stageDirs|other=$other'
          '|scratch=$scratch|journal=$journal'
      : 'exists=${exists ?? 'unknown'}|children=na|stageDirs=na|other=na'
          '|scratch=unknown|journal=$journal|read=$read';
}

/// Never throws: a look that failed is [read].
_RootShape _rootShape(String root) {
  final dir = io.Directory(root);
  bool? there;
  try {
    there = dir.existsSync();
  } on Object {
    return _RootShape(
      exists: null,
      children: 0,
      stageDirs: 0,
      other: 0,
      scratch: false,
      journal: _journalState(root),
      read: 'look-failed',
    );
  }
  if (there != true) {
    return const _RootShape(
      exists: false,
      children: 0,
      stageDirs: 0,
      other: 0,
      scratch: false,
      journal: 'absent',
      read: 'ok',
    );
  }
  final List<io.FileSystemEntity> entries;
  try {
    entries = dir.listSync(followLinks: false);
  } on Object {
    return _RootShape(
      exists: true,
      children: 0,
      stageDirs: 0,
      other: 0,
      scratch: false,
      journal: _journalState(root),
      read: 'list-failed',
    );
  }
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
    read: 'ok',
  );
}

// pure helpers: start
//
// The ones below need nothing but dart:io. They decide what a state string
// means, what the grant journal holds and what two snapshots of the stager's
// trace ring say. The host's self-test lifts them out of this file between
// these two marker lines and runs them; keep them free of the test's other
// members and of every import but dart:io. None of them throws: a call on
// dart:io that fails is answered by a fixed word.

/// The lowest API level the app is built for: a smaller number is not one.
const int _minApi = 24;

/// The API level of this image — what part B's RETAKE expects depends on
/// it. Only the host's own reading of `ro.build.version.sdk` on the image it
/// drives names the level ([fromHost], dart-define AK_SAF_PROBE_API).
/// [osVersion] is `Platform.operatingSystemVersion`; it is NOT a release
/// string on Android — on the x86_64 image its first number is 64 (seen on
/// the device) — so only that first number is recorded ([osFirst]) and
/// nothing is ever taken from it. Without the host's value the level is null
/// and the RETAKE fails by name: no answer is ever expected at random.
({int? level, String source, int? osFirst}) _apiLevelOf(
    String osVersion, int fromHost) {
  final match = RegExp(r'[0-9]+').firstMatch(osVersion);
  final first = match == null ? null : int.tryParse(match.group(0)!);
  if (fromHost >= _minApi && fromHost <= 99) {
    return (level: fromHost, source: 'host', osFirst: first);
  }
  return (level: null, source: 'none', osFirst: first);
}

/// The folder a tree URI names — for a channel that answers no display name:
/// the document id is the URI's last segment
/// (`…/tree/primary%3ADownload%2Fakstage`), and its last component is the
/// folder. Compared with a preset label, never printed.
String _folderOfTreeUri(String treeUri) {
  var decoded = '';
  try {
    decoded = Uri.decodeComponent(treeUri.split('/').last);
  } on Object {
    return '';
  }
  final cut = decoded.lastIndexOf('/');
  return cut < 0 ? decoded : decoded.substring(cut + 1);
}

/// The state of the grant journal — by LINE COUNT, never by content (a line
/// is a tree URI): `absent`, `empty`, `lines:<n>`, and anything else —
/// `not-a-file`, `unreadable`. Only a regular file is looked into, and
/// only its bytes are counted. The count is [_journalFacts]'s, read once;
/// for the places that need no more than the count.
String _journalState(String root) => _journalFacts(root, '').state;

/// What the grant journal holds, read ONCE, and nothing here throws.
///
/// [state] is the line count exactly as [_journalState] names it. [line],
/// from the same bytes, says whether the journal's one line is the folder's
/// own — a line that is EXACTLY [treeUri], whole line: `target`. A CR
/// before the LF and a missing final newline are endured; a line that
/// shares only the leaf name, or is a prefix, or carries one more character
/// does not count. `other` when there are lines and none is, `none` when
/// there is no line, and the state's own word when the journal is
/// `not-a-file` or `unreadable`. Only a regular file that is no link is
/// looked into; the content itself is never answered.
({String state, String line}) _journalFacts(String root, String treeUri) {
  final journal = io.File('$root/$_journalName');
  try {
    switch (io.FileSystemEntity.typeSync(journal.path, followLinks: false)) {
      case io.FileSystemEntityType.notFound:
        return (state: 'absent', line: 'none');
      case io.FileSystemEntityType.file:
        break;
      default:
        return (state: 'not-a-file', line: 'not-a-file');
    }
  } on Object {
    return (state: 'unreadable', line: 'unreadable');
  }
  final List<int> bytes;
  try {
    bytes = journal.readAsBytesSync();
  } on Object {
    return (state: 'unreadable', line: 'unreadable');
  }
  // The URI as the stager wrote a journal line: UTF-8.
  final want = treeUri.isEmpty ? const <int>[] : _utf8Bytes(treeUri);
  var lines = 0;
  var inLine = false;
  var target = false;
  // Where the line being counted began.
  var start = 0;
  for (var i = 0; i <= bytes.length; i += 1) {
    final last = i == bytes.length;
    if (!last && bytes[i] != 0x0a) {
      if (bytes[i] != 0x0d && bytes[i] != 0x09 && bytes[i] != 0x20) {
        inLine = true;
      }
      continue;
    }
    // The line is over: an LF, or the end of a file without a final one.
    // An empty final line (the file ends with an LF) is no line.
    if (inLine) lines += 1;
    inLine = false;
    if (last && start == bytes.length) break;
    var end = i;
    while (end > start && bytes[end - 1] == 0x0d) {
      end -= 1;
    }
    if (want.isNotEmpty && end - start == want.length) {
      var same = true;
      for (var at = 0; same && at < want.length; at += 1) {
        same = bytes[start + at] == want[at];
      }
      if (same) target = true;
    }
    start = i + 1;
  }
  return (
    state: lines == 0 ? 'empty' : 'lines:$lines',
    line: target ? 'target' : (lines == 0 ? 'none' : 'other'),
  );
}

/// [text] as UTF-8, encoded by hand: between these markers nothing but
/// dart:io may be imported. As dart:convert writes it, an unpaired
/// surrogate is U+FFFD.
List<int> _utf8Bytes(String text) {
  final bytes = <int>[];
  for (var i = 0; i < text.length; i += 1) {
    var code = text.codeUnitAt(i);
    if (code >= 0xd800 &&
        code <= 0xdbff &&
        i + 1 < text.length &&
        text.codeUnitAt(i + 1) >= 0xdc00 &&
        text.codeUnitAt(i + 1) <= 0xdfff) {
      code =
          0x10000 + ((code - 0xd800) << 10) + (text.codeUnitAt(i + 1) - 0xdc00);
      i += 1;
    }
    if (code >= 0xd800 && code <= 0xdfff) code = 0xfffd;
    if (code < 0x80) {
      bytes.add(code);
    } else if (code < 0x800) {
      bytes.add(0xc0 | (code >> 6));
      bytes.add(0x80 | (code & 0x3f));
    } else if (code < 0x10000) {
      bytes.add(0xe0 | (code >> 12));
      bytes.add(0x80 | ((code >> 6) & 0x3f));
      bytes.add(0x80 | (code & 0x3f));
    } else {
      bytes.add(0xf0 | (code >> 18));
      bytes.add(0x80 | ((code >> 12) & 0x3f));
      bytes.add(0x80 | ((code >> 6) & 0x3f));
      bytes.add(0x80 | (code & 0x3f));
    }
  }
  return bytes;
}

// ---- the stager's trace ring (X1: safGrantTrace) --------------------------
//
// A snapshot of the ring is a list of entries, oldest first, each
// `"<seq>|<event>|u<n>"`: `seq` a process-wide increasing integer, `event`
// one of six labels, `u<n>` an alias of a tree URI — one alias while the URI
// is referenced in the ring, a number that is never issued again. No entry
// carries a URI, a path, a name or a stage id, and neither does anything
// derived here.

/// The six labels the stager traces.
const List<String> _traceEvents = <String>[
  'pick_result',
  'job_end',
  'settle_refused',
  'settle_begin',
  'release',
  'receive_take',
];

/// A decimal integer as the stager writes one: no sign, no leading zero.
final RegExp _traceNumber = RegExp(r'^(0|[1-9][0-9]*)$');

/// One entry of the ring, parsed: null for anything that is not exactly
/// three such fields.
({int seq, String event, String alias})? _traceEntry(String entry) {
  final fields = entry.split('|');
  if (fields.length != 3) return null;
  if (!_traceNumber.hasMatch(fields[0])) return null;
  final seq = int.tryParse(fields[0]);
  if (seq == null) return null;
  if (!_traceEvents.contains(fields[1])) return null;
  if (!fields[2].startsWith('u')) return null;
  final number = fields[2].substring(1);
  if (!_traceNumber.hasMatch(number)) return null;
  return (seq: seq, event: fields[1], alias: fields[2]);
}

/// A snapshot of the ring as the channel answered it: its entries, oldest
/// first — or null when the snapshot is not a list of strings (no ring).
List<String>? _traceRing(Object? snapshot) {
  if (snapshot is! List) return null;
  final ring = <String>[];
  for (final entry in snapshot) {
    if (entry is! String) return null;
    ring.add(entry);
  }
  return ring;
}

/// The watermark of a part: the largest `seq` a snapshot holds — 0 when the
/// ring is empty. An entry that does not parse names no number; the window
/// judged later starts right after the largest that does.
int _traceWatermark(List<String> ring) {
  var largest = 0;
  for (final entry in ring) {
    final parsed = _traceEntry(entry);
    if (parsed != null && parsed.seq > largest) largest = parsed.seq;
  }
  return largest;
}

/// What the trace window of a part says about the stager's order. Every
/// field is a fixed word, a count or null (`na` in [describe]).
class _NativeOrder {
  const _NativeOrder({
    required this.native,
    required this.window,
    this.pickResultSeq,
    this.jobEndSeq,
    this.refusedSeq,
    this.takeSeq,
    this.releasesBetween,
    this.beginsBetween,
  });

  /// `strict`, `not-held` or `not-exercised`.
  final String native;

  /// How many entries of the snapshot are newer than the watermark; null
  /// when a snapshot was no ring at all.
  final int? window;

  /// `seq` of the part's one `pick_result`.
  final int? pickResultSeq;

  /// `seq` of the first `job_end` of its alias after it.
  final int? jobEndSeq;

  /// `seq` of the first `settle_refused` of that alias after the job's end
  /// and before the take.
  final int? refusedSeq;

  /// `seq` of the first `receive_take` of that alias after the result.
  final int? takeSeq;

  /// How many `release` / `settle_begin` of that alias lie after the result
  /// and before the take; null without a take.
  final int? releasesBetween;
  final int? beginsBetween;

  /// A snapshot was no ring: nothing can be concluded.
  static const _NativeOrder unreadable = _NativeOrder(
    native: 'not-exercised',
    window: null,
  );

  /// The evidence line's fields.
  String get describe =>
      'native=$native|window=${window ?? 'na'}'
      '|pickResultSeq=${pickResultSeq ?? 'na'}'
      '|jobEndSeq=${jobEndSeq ?? 'na'}'
      '|refusedSeq=${refusedSeq ?? 'na'}'
      '|takeSeq=${takeSeq ?? 'na'}'
      '|releasesBetween=${releasesBetween ?? 'na'}'
      '|beginsBetween=${beginsBetween ?? 'na'}';
}

/// Judges the entries of [ring] newer than [watermark] — what a part traced
/// between two snapshots. Nothing is concluded unless every entry of the
/// window parses and the window is CONTIGUOUS (its seqs run `watermark+1,
/// watermark+2, …`: a gap or a malformed entry means the ring was
/// overwritten or cannot be read) and exactly ONE `pick_result` is in it
/// (more is several candidates). With that result's alias `a`: the first
/// `job_end` of `a` after it, the first `receive_take` of `a` after it, the
/// first `settle_refused` of `a` after that end and before that take, and
/// how many `release` / `settle_begin` of `a` lie between the result and
/// the take. `strict` when the three moments stand in the order
/// `pick_result < job_end < receive_take`, the refusal lies between the
/// job's end and the take, and nothing was released or admitted in between;
/// `not-held` when they stand in that order but something was;
/// `not-exercised` in every other case — moments missing or out of order, no
/// refusal between them, a window that says nothing. Under ANOTHER alias
/// nothing counts.
_NativeOrder _nativeOrder(List<String> ring, int watermark) {
  // What the earlier snapshot can already have seen: the ring's leading
  // entries, up to the watermark.
  var seen = 0;
  while (seen < ring.length) {
    final entry = _traceEntry(ring[seen]);
    if (entry == null || entry.seq > watermark) break;
    seen += 1;
  }
  final window = ring.length - seen;
  _NativeOrder nothing() =>
      _NativeOrder(native: 'not-exercised', window: window);

  final entries = <({int seq, String event, String alias})>[];
  var expected = watermark + 1;
  for (var i = seen; i < ring.length; i += 1) {
    final entry = _traceEntry(ring[i]);
    if (entry == null || entry.seq != expected) return nothing();
    entries.add(entry);
    expected += 1;
  }

  var pickAt = -1;
  var picks = 0;
  for (var i = 0; i < entries.length; i += 1) {
    if (entries[i].event == 'pick_result') {
      picks += 1;
      pickAt = i;
    }
  }
  if (picks != 1) return nothing();

  final pick = entries[pickAt];
  final alias = pick.alias;
  int? jobEndSeq;
  int? refusedSeq;
  int? takeSeq;
  var releases = 0;
  var begins = 0;
  for (var i = pickAt + 1; i < entries.length; i += 1) {
    final entry = entries[i];
    if (entry.alias != alias) continue;
    if (entry.event == 'receive_take') {
      takeSeq = entry.seq;
      break;
    }
    if (entry.event == 'release') releases += 1;
    if (entry.event == 'settle_begin') begins += 1;
    if (entry.event == 'job_end' && jobEndSeq == null) jobEndSeq = entry.seq;
    if (entry.event == 'settle_refused' &&
        jobEndSeq != null &&
        refusedSeq == null) {
      refusedSeq = entry.seq;
    }
  }

  if (jobEndSeq == null || takeSeq == null) {
    return _NativeOrder(
      native: 'not-exercised',
      window: window,
      pickResultSeq: pick.seq,
      jobEndSeq: jobEndSeq,
      takeSeq: takeSeq,
      releasesBetween: takeSeq == null ? null : releases,
      beginsBetween: takeSeq == null ? null : begins,
    );
  }
  return _NativeOrder(
    native: releases != 0 || begins != 0
        ? 'not-held'
        : (refusedSeq == null ? 'not-exercised' : 'strict'),
    window: window,
    pickResultSeq: pick.seq,
    jobEndSeq: jobEndSeq,
    refusedSeq: refusedSeq,
    takeSeq: takeSeq,
    releasesBetween: releases,
    beginsBetween: begins,
  );
}

/// A part's order from the two snapshots of the ring: the one read before
/// the part began and the one read after its calls settled. A snapshot that
/// is no ring concludes nothing.
_NativeOrder _nativeOrderBetween(Object? before, Object? after) {
  final was = _traceRing(before);
  final now = _traceRing(after);
  if (was == null || now == null) return _NativeOrder.unreadable;
  return _nativeOrder(now, _traceWatermark(was));
}

/// Whether [state] says the send side journaled a take: `lines:` with at
/// least one line — never "anything but absent".
bool _journaled(String state) =>
    RegExp(r'^lines:([1-9][0-9]*)$').hasMatch(state);

/// Whether [state] says nothing is journaled: absent or empty, and nothing
/// else — a journal that is not a file, or cannot be read, is not clear.
bool _journalClear(String state) => state == 'absent' || state == 'empty';
// pure helpers: end

/// The type of what is at [path], no link followed — or null when the look
/// itself failed, which is no type at all: the callers that may read a
/// false as "not there" name it (see [read] of [_StageDir]).
io.FileSystemEntityType? _typeOf(String path) {
  try {
    return io.FileSystemEntity.typeSync(path, followLinks: false);
  } on Object {
    return null;
  }
}

/// Whether nothing is at [dir] any more — a stage directory: `true` or
/// `false`; null when the look itself failed, which is never "gone" (the
/// caller names it).
bool? _gone(String dir) {
  final there = _isDirectory(dir);
  return there == null ? null : !there;
}

/// Whether a directory is at [path]; null when the look itself failed.
bool? _isDirectory(String path) {
  try {
    return io.Directory(path).existsSync();
  } on Object {
    return null;
  }
}

/// The stage directories directly under the staging root [root], by a
/// listing of its own — or null when the root could not be looked at or
/// listed, which is never "no stage directory" (the caller names it).
List<io.Directory>? _stagedDirs(String root) {
  final there = _isDirectory(root);
  if (there == null) return null;
  if (there != true) return const <io.Directory>[];
  try {
    return <io.Directory>[
      for (final e in io.Directory(root).listSync(followLinks: false))
        if (e is io.Directory) e,
    ];
  } on Object {
    return null;
  }
}

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
    required this.read,
  });

  /// The directory exists: null when the look itself failed — never "gone".
  final bool? present;

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

  /// Whether all of this could be read: `ok`; `look-failed` — the
  /// directory itself could not be looked at; `list-failed` — it could not
  /// be listed; `read-failed` — an entry inside it could not be looked at
  /// or measured. Every look is caught where it is made, and one that
  /// failed leaves its fact false: a false is "not there" only when this
  /// says `ok`.
  final String read;

  /// The owner file as the stager writes it: a regular file with a token.
  bool get ownerWritten => owner && ownerBytes > 0;
}

/// The stage directory [dir]; [root] names the staged folder inside
/// `payload` (only the kill round's hold file needs it). Never throws: a
/// look that failed is [read], and its facts stay false.
_StageDir _stageDirState(String dir, {String root = _deepName}) {
  final stage = io.Directory(dir);
  bool? there;
  try {
    there = stage.existsSync();
  } on Object {
    return const _StageDir(
      present: null,
      marker: false,
      owner: false,
      ownerBytes: 0,
      payload: false,
      holdBytes: -1,
      children: 0,
      read: 'look-failed',
    );
  }
  if (there != true) {
    return const _StageDir(
      present: false,
      marker: false,
      owner: false,
      ownerBytes: 0,
      payload: false,
      holdBytes: -1,
      children: 0,
      read: 'ok',
    );
  }
  final List<io.FileSystemEntity> entries;
  try {
    entries = stage.listSync(followLinks: false);
  } on Object {
    return _StageDir(
      present: true,
      marker: false,
      owner: false,
      ownerBytes: 0,
      payload: false,
      holdBytes: -1,
      children: 0,
      read: 'list-failed',
    );
  }
  var failed = false;
  bool regular(String path) {
    final type = _typeOf(path);
    if (type == null) failed = true;
    return type == io.FileSystemEntityType.file;
  }

  int bytesOf(String path, int none) {
    try {
      return io.File(path).lengthSync();
    } on Object {
      failed = true;
      return none;
    }
  }

  final ownerFile = io.File('$dir/$_ownerName');
  final hold = io.File('$dir/payload/$root/$_holdName');
  final owner = regular(ownerFile.path);
  final held = regular(hold.path);
  return _StageDir(
    present: true,
    marker: regular('$dir/$_markerName'),
    owner: owner,
    ownerBytes: owner ? bytesOf(ownerFile.path, 0) : 0,
    payload: entries.any(
        (e) => e is io.Directory && _basename(e.path) == 'payload'),
    holdBytes: held ? bytesOf(hold.path, -1) : -1,
    children: entries.length,
    read: failed ? 'read-failed' : 'ok',
  );
}

// ---------------------------------------------------------------------------
// Small helpers.
// ---------------------------------------------------------------------------

String _basename(String path) => path.substring(path.lastIndexOf('/') + 1);

/// The digest of [file] — or null when opening it or reading it failed,
/// which is never a match (the caller names it).
Future<String?> _sha256Of(io.File file) async {
  try {
    return (await crypto.sha256.bind(file.openRead()).first).toString();
  } on Object {
    return null;
  }
}

String _sha256OfText(String text) =>
    crypto.sha256.convert(utf8.encode(text)).toString();

/// The digest the preset names for [rel]: the text's, the empty file's, or
/// — for nested/photo.bin — the host's (AK_SAF_PROBE_SHA_BIG).
String? _digestOf(String rel, String shaBig) => rel == _bigRel
    ? shaBig
    : _texts.containsKey(rel)
        ? _sha256OfText(_texts[rel]!)
        : rel == _emptyRel
            ? _emptySha
            : null;

/// The staged tree under [root]: every entry by its path relative to it —
/// or null when the walk itself failed, which is an empty tree to nobody
/// (the caller names it).
Future<Map<String, io.FileSystemEntity>?> _walk(String root) async {
  final entries = <String, io.FileSystemEntity>{};
  try {
    await for (final entity
        in io.Directory(root).list(recursive: true, followLinks: false)) {
      entries[entity.path.substring(root.length + 1)] = entity;
    }
  } on Object {
    return null;
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

/// Whether the gateway's refusal dialog is up. That is all it says: the
/// dialog's text carries the platform's reason, which may name a folder or
/// a path, and no value read from the device reaches a line.
void _refusalDialog(WidgetTester tester, String part) {
  if (find.byKey(const ValueKey('staging_error_dialog')).evaluate().isEmpty) {
    return;
  }
  zSay('$part|ERROR_DIALOG|up=true');
}

/// The API level of THIS image: the platform's version string, and the
/// host's dart-define.
({int? level, String source, int? osFirst}) _apiLevel() => _apiLevelOf(
    io.Platform.operatingSystemVersion,
    const int.fromEnvironment('AK_SAF_PROBE_API', defaultValue: 0));

/// A pick through the raw channel [method]: asks for the host's drive
/// (`<part>|<picker>|tree|…`), waits for it WITHOUT a frame — the system
/// picker is in front until the host has confirmed it — and hands back the
/// tree URI, which never prints, or null. No answer within the budget means
/// no drive came. [label] is the folder the contract names for this pick:
/// the name the platform answers is compared with it — a channel that answers
/// no name has the folder read off its tree URI — and only whether they match
/// is printed.
Future<String?> _rawPick(_Run run, String part, String method,
    {required String label,
    int seconds = _pickSeconds,
    String picker = 'PICKER'}) async {
  zSay('$part|$picker|tree|awaiting external DocumentsUI drive');
  final picked =
      _Answer.capture(_storageChannel.invokeMethod<Object?>(method));
  await picked.wait(seconds);
  if (!picked.answered) {
    run.pickerStalled = true;
  }
  final map = picked.value;
  final treeUri = map is Map && map['treeUri'] is String
      ? map['treeUri'] as String
      : null;
  // pickSendFolder answers the folder's display name; pickFolder answers
  // none — its folder is read off the tree URI's own document id instead.
  final name = map is Map && map['displayName'] is String
      ? map['displayName'] as String
      : treeUri == null
          ? ''
          : _folderOfTreeUri(treeUri);
  run.lastPickName = name;
  zSay('$part|${picker.replaceFirst('PICKER', 'PICKED')}'
      '|selected=${treeUri != null}|nameIsPreset=${name == label}'
      '|uriIsTree=${treeUri?.startsWith('content://')}'
      '|threw=${picked.error == null ? 'no' : picked.verdict}');
  return treeUri;
}

Future<_Answer> _listGrants(_Run run) => _settled(
    run, _storageChannel.invokeMethod<Object?>('listSafPersisted'));

/// The persisted grant of exactly [treeUri] as a word: `rw`, `r`, `w`,
/// `--` — or `absent` without one, `error:<verdict>` when the list itself
/// did not answer. The URIs themselves stay on the device.
Future<String> _grantFlags(_Run run, String treeUri) async =>
    _flagsIn(await _listGrants(run), treeUri);

String _flagsIn(_Answer listed, String treeUri) {
  final grants = listed.value;
  if (!listed.answered || grants is! List) return 'error:${listed.verdict}';
  for (final grant in grants) {
    if (grant is Map && grant['uri'] == treeUri) {
      final read = grant['read'] == true;
      final write = grant['write'] == true;
      if (read && write) return 'rw';
      if (read) return 'r';
      if (write) return 'w';
      return '--';
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

/// How many persisted grants there are in all; -1 when the list itself did
/// not answer.
int _grantsIn(_Answer listed) =>
    listed.answered && listed.value is List ? (listed.value as List).length : -1;

/// The READ-only count as text for the evidence lines; `error:<verdict>`
/// when the list itself did not answer.
Future<String> _readOnlyGrants(_Run run) async {
  final listed = await _listGrants(run);
  final left = _readOnlyIn(listed);
  return left < 0 ? 'error:${listed.verdict}' : '$left';
}

/// When the app came back in front after a pick was asked for — the moment
/// the host confirmed the system picker. A lifecycle message, so it needs
/// no frame (the app's are paused while the picker is in front). The count
/// it records is the caller's: of its own events, at that moment.
class _ForegroundTap with WidgetsBindingObserver {
  int Function()? _events;
  bool _away = false;
  int? _resumedAfterEvents;

  /// How many events had passed when the app was back; null if it never was.
  int? get resumedAfterEvents => _resumedAfterEvents;

  void install(int Function() events) {
    _events = events;
    _away = false;
    _resumedAfterEvents = null;
    WidgetsBinding.instance.addObserver(this);
  }

  void remove() {
    WidgetsBinding.instance.removeObserver(this);
    _events = null;
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed) {
      // The app was away (the picker in front) and is back: that is the confirm.
      if (_away && _resumedAfterEvents == null) {
        _resumedAfterEvents = _events?.call() ?? 0;
      }
      _away = false;
    } else {
      _away = true;
    }
  }
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
  // No frame is waited for: the system picker is in front from here until
  // the host has confirmed it. The live binding draws by itself once the
  // app is back, and the gateway's answer arrives without a pump.
  final pick = _Answer.capture(gateway.pickSendDirectory());
  await pick.wait(_pickSeconds);
  if (!pick.answered) {
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
  zSay('A|STAGED|nameIsPreset=${entry?.name == _rootName}'
      '|files=${entry?.stagedFileCount}|bytes=${entry?.stagedByteCount}'
      '|staged=${id != null}|pathIsFs=${entry?.path.startsWith('/')}'
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

  // The staged copy against the preset tree — by PRESET label. One line per
  // preset entry: whether the copy has it, of the kind, the size and the
  // digest the preset names; and one count for everything the copy holds
  // beyond the preset. A relative name read off the device never prints: a
  // directory where a file belongs — and the other way round — is a wrong
  // copy, not a skip (the path set alone would still be equal).
  final entries = await _walk(path);
  final preset = <String>{..._sizes.keys, ..._directories}.toList()..sort();
  if (entries == null) {
    // The tree could not be walked: nothing is known of it — not even that
    // nothing lies beyond the preset.
    need(false, 'walk-failed');
    zSay('A|EXTRA|unknown=-1');
  } else {
    var extra = 0;
    for (final rel in entries.keys) {
      if (!preset.contains(rel)) extra += 1;
    }
    for (final rel in preset) {
      final entity = entries[rel];
      final file = _sizes.containsKey(rel);
      final found = entity != null;
      var kind = false;
      var size = false;
      var sha = false;
      var lengthFailed = false;
      var hashFailed = false;
      if (entity is io.File && file) {
        kind = true;
        // A length or a digest that could not be read is no match.
        int? length;
        try {
          length = await entity.length();
        } on Object {
          length = null;
        }
        size = length != null && length == _sizes[rel];
        final digest = await _sha256Of(entity);
        sha = digest != null && digest == _digestOf(rel, shaBig);
        lengthFailed = length == null;
        hashFailed = digest == null;
      } else if (entity is io.Directory && !file) {
        kind = true;
      }
      zSay('A|ENTRY|label=$rel|found=$found|kind=$kind'
          '|size=${file ? size : 'na'}|sha=${file ? sha : 'na'}');
      need(found, 'missing:$rel');
      need(kind, 'kind:$rel');
      if (file) {
        // A length or a digest that could not be read has its own name.
        need(size, lengthFailed ? 'length-failed:$rel' : 'size:$rel');
        need(sha, hashFailed ? 'hash-failed:$rel' : 'sha:$rel');
      }
    }
    zSay('A|EXTRA|unknown=$extra');
    need(extra == 0, 'extra-entries:$extra');
  }

  // Exactly one stage directory — the one just staged — and, besides it,
  // nothing but the grant journal the send side keeps in the staging root.
  final staged = _rootShape(stagingRoot.path);
  if (staged.read != 'ok' || staged.stageDirs != 1 || staged.other != 0) {
    need(false, 'staging-root-shape(${staged.describe})');
  }
  // payload, the completion marker and the owner file of its stager instance (O16)
  // — nothing else beside them.
  Set<String>? stageChildren;
  try {
    stageChildren = <String>{
      for (final e in stageDir.listSync()) _basename(e.path),
    };
  } on Object {
    stageChildren = null;
  }
  final listed = stageChildren;
  zSay('A|STAGEDIR|children=${listed?.length ?? 'na'}'
      '|payload=${listed?.contains('payload') ?? 'unknown'}'
      '|marker=${listed?.contains(_markerName) ?? 'unknown'}'
      '|owner=${listed?.contains(_ownerName) ?? 'unknown'}');
  if (listed == null) {
    need(false, 'stage-dir-list-failed');
  } else if (listed.length != 3 ||
      !listed.contains('payload') ||
      !listed.contains(_markerName) ||
      !listed.contains(_ownerName)) {
    need(false, 'stage-dir-contents(${listed.length})');
  }

  zSay('A|CHECK|${failures.isEmpty ? 'ok' : 'failed=${failures.join(',')}'}');

  await gateway.releaseStaged(<PickedEntry>[entry]);
  final gone = _gone(stageDir.path);
  final after = _rootShape(stagingRoot.path);
  zSay('A|RELEASED|stageDirGone=${gone ?? 'unknown'}|${after.describe}');
  // A look that failed is not "gone".
  if (gone != true) {
    need(false,
        gone == null ? 'release-stage-dir-look-failed' : 'release-stage-dir');
  }
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
// Part B — the failure paths through the raw channel, and the RETAKE.
// ---------------------------------------------------------------------------

Future<void> _partB(_Run run) async {
  final failures = <String>[];
  void need(bool ok, String what) {
    if (!ok) failures.add(what);
  }

  void collect() {
    run.problems.addAll(<String>[for (final f in failures) 'B:$f']);
  }

  final treeUri = await _rawPick(run, 'B', 'pickSendFolder', label: _rootName);
  if (treeUri == null) {
    need(false, 'pick-no-tree');
    zSay('B|DONE|failed=${failures.length}');
    return collect();
  }
  run.treeUri = treeUri;
  need(run.lastPickName == _rootName, 'pick-wrong-folder');

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
  // known, or the look itself failed — never "gone".
  final bool? goneAfterCancel = root.isEmpty ? null : _gone('$root/$idB');
  zSay('B|CANCEL|code=${cancelling.codeToken}'
      '|stageDirGone=${goneAfterCancel ?? 'unknown'}'
      '|events=${tap.events}|lastPhase=${tap.lastPhase}'
      '|copyEvents=${tap.copyEvents(idB)}|cancelSentAtEvent=$cancelAtEvent'
      '|bytesAtCancel=$bytesAtCancel');
  if (cancelAtEvent == 0) need(false, 'cancel-no-copy-event');
  // The hook that sends the cancel must have got through every copy event.
  if (tap.hookFailures > 0) {
    need(false, 'cancel-hook-failed:${tap.hookFailures}');
  }
  // It answers STAGE_CANCELLED only once its directory is gone; otherwise
  // the answer says what was left (`STAGE_IO_FAILED` / `leftover`).
  if (!_failedWith(cancelling, 'STAGE_CANCELLED', 'user')) {
    need(false, 'cancel-code:${cancelling.codeToken}');
  }
  if (root.isEmpty) need(false, 'no-staging-root');
  if (goneAfterCancel == null && root.isNotEmpty) {
    need(false, 'cancel-stage-dir-look-failed');
  }
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

  // ---- the RETAKE (the API 29 round) ----
  // The cancelled job has released the READ it took. Read here, because the
  // retake's answer means something only when nothing is persisted for the
  // tree any more: with READ still there a later staging takes nothing, and
  // succeeds on every API level.
  final released = await _grantFlags(run, treeUri);
  final readOnlyReleased = await _readOnlyGrants(run);
  final journalReleased = root.isEmpty ? 'na' : _journalState(root);
  zSay('B|RELEASED|flags=$released|readOnly=$readOnlyReleased'
      '|journal=$journalReleased');
  if (root.isEmpty) need(false, 'no-staging-root');
  if (released != 'absent') need(false, 'cancel-grant-not-released:$released');
  if (readOnlyReleased != '0') {
    need(false, 'cancel-read-only-left:$readOnlyReleased');
  }
  if (!_journalClear(journalReleased)) {
    need(false, 'cancel-journal:$journalReleased');
  }

  // One more staging with the SAME tree URI and no new pick. From API 30 on
  // the platform lets the send side take READ again: ok. On API 24–29 the
  // release above took READ out of what a later take may persist — the
  // platform fact behind G15: SAF_PERMISSION_DENIED.
  final idR = run.newId();
  final retake = await stage(treeUri, idR);
  final api = run.apiLevel;
  zSay('B|RETAKE|code=${retake.codeToken}|api=${api ?? 'unknown'}');
  if (api == null) {
    // Never one of the two answers at random: the run fails under this name.
    need(false, 'retake-api-unknown');
  } else if (api < 30) {
    if (retake.code != 'SAF_PERMISSION_DENIED') {
      need(false, 'retake-code:$api:${retake.codeToken}');
    }
  } else if (retake.code != '' || _statusOf(retake) != 'ok') {
    need(false, 'retake-status:$api:${_statusOf(retake)}');
  }
  // A retake that delivered leaves a copy: it goes. (A refused one has
  // nothing to clean; the answer is on record all the same.)
  final retakeCleaned = await _cleanup(run, idR);
  final bool? rGone = root.isEmpty ? null : _gone('$root/$idR');
  zSay('B|RETAKE_CLEANED|code=${retakeCleaned.text}'
      '|stageDirGone=${rGone ?? 'unknown'}');
  if (api != null && api >= 30) {
    if (!retakeCleaned.shapeOk) {
      need(false, 'retake-cleanup-shape:${retakeCleaned.text}');
    }
    // A look that failed is not "gone".
    if (retakeCleaned.removed != true || rGone != true) {
      need(false, 'retake-cleanup:${retakeCleaned.text}/gone=${rGone ?? 'unknown'}');
    }
  }
  // Whatever the retake did, no READ-only grant and no journal line stay
  // behind — read now, before the next staging of the same tree could
  // settle a line that is not its own.
  final readOnlyRetook = await _readOnlyGrants(run);
  final journalRetook = root.isEmpty ? 'na' : _journalState(root);
  zSay('B|RETAKE_GRANT|left=$readOnlyRetook|journal=$journalRetook');
  if (readOnlyRetook != '0') need(false, 'retake-grant-left:$readOnlyRetook');
  if (!_journalClear(journalRetook)) {
    need(false, 'retake-journal:$journalRetook');
  }

  // The stale cleanup next to a RUNNING staging: it must not touch the
  // running job, and its answer must be the map the contract names. The
  // tree is picked afresh first, on every API level: that is what a real
  // flow has, and after the retake the old URI alone would be refused on
  // API 24–29.
  final fresh = await _rawPick(run, 'B', 'pickSendFolder',
      picker: 'PICKER2', label: _rootName);
  if (fresh == null) {
    need(false, 'pick2-no-tree');
  } else {
    need(run.lastPickName == _rootName, 'pick2-wrong-folder');
    final idC = run.newId();
    final active = _Answer.capture(_stageCall(fresh, idC));
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
    var markerKept = false;
    var ownerKept = false;
    // null: the stage directory could not be listed, or there is no path.
    int? stageChildren;
    // null: the look at the payload failed, or there is no path.
    bool? payloadThere;
    if (activePath != null) {
      final stageDir = io.Directory(activePath).parent.parent;
      try {
        final children = <String>{
          for (final e in stageDir.listSync()) _basename(e.path),
        };
        stageChildren = children.length;
        markerKept = children.contains(_markerName);
        ownerKept = children.contains(_ownerName);
      } on Object {
        stageChildren = null;
      }
      payloadThere = _isDirectory('${stageDir.path}/payload/$activeName');
    }
    zSay('B|ACTIVE'
        '|staleRemoved=${staleRemoved ?? stale.codeToken}'
        '|staleShape=${staleRemoved != null ? 'int' : 'bad'}'
        '|staged=${_statusOf(active)}'
        '|payloadKept=${payloadThere ?? 'unknown'}|markerKept=$markerKept'
        '|ownerKept=$ownerKept|stageDirChildren=${stageChildren ?? 'na'}');
    // Judged while the copy and its marker are still there — nothing has
    // cleaned idC yet. A delivered stage directory is payload, the completion
    // marker and the owner file: three entries (N11).
    if (staleRemoved == null || staleRemoved < 0) {
      need(false, 'active-stale-answer:${staleRemoved ?? stale.codeToken}');
    }
    if (_statusOf(active) != 'ok') {
      need(false, 'active-staged:${_statusOf(active)}');
    }
    // A listing or a look that failed is none of these facts.
    if (stageChildren == null) need(false, 'active-stage-dir-list-failed');
    if (payloadThere == null) need(false, 'active-payload-look-failed');
    if (payloadThere != true ||
        !markerKept ||
        !ownerKept ||
        stageChildren != 3) {
      need(false,
          'active-kept-copy-marker-owner:${payloadThere ?? 'unknown'}/'
          '$markerKept/$ownerKept/${stageChildren ?? 'na'}');
    }
    final cleanedC = await _cleanup(run, idC);
    // The stage directory of the active staging; null when its answer named
    // no path.
    final activeStage = activePath == null
        ? null
        : io.Directory(activePath).parent.parent.path;
    final bool? cGone = activeStage == null ? false : _gone(activeStage);
    zSay('B|ACTIVE_CLEANED|code=${cleanedC.text}'
        '|stageDirGone=${cGone ?? 'unknown'}');
    if (!cleanedC.shapeOk || cleanedC.removed != true || cGone != true) {
      need(false, 'active-cleanup:${cleanedC.text}/gone=${cGone ?? 'unknown'}');
    }
  }

  // Every send-side staging of this part is over: no read-only grant may
  // be left. The journal is on record by state only.
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
  final treeUri = await _rawPick(run, 'C', 'pickFolder', label: _rootName);
  if (treeUri == null) {
    need(false, 'pick-no-tree');
    zSay('C|DONE|failed=${failures.length}');
    run.problems.addAll(<String>[for (final f in failures) 'C:$f']);
    return;
  }
  need(run.lastPickName == _rootName, 'pick-wrong-folder');

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
  final bool? dGone = root.isEmpty ? null : _gone('$root/$idD');
  zSay('C|CLEANED|${cleaned.text}|stageDirGone=${dGone ?? 'unknown'}');
  if (!cleaned.shapeOk) need(false, 'cleanup-shape:${cleaned.text}');
  if (root.isEmpty) need(false, 'no-staging-root');
  // A look that failed is not "gone".
  if (cleaned.removed != true || dGone != true) {
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
/// registered and the copy has just begun. Neither the hook nor [atFirstCopy]
/// can end as an unhandled error: a throw is `hookFailed` in the answer.
Future<({
  _Answer answer,
  int? copyMs,
  int? afterCopyMs,
  int? estimate,
  int? entries,
  bool hookFailed,
})>
    _stagedUnderTap(_Run run, _ProgressTap tap, String step, String treeUri,
        String stageId, {void Function()? atFirstCopy}) async {
  zSay('$step|START');
  Map<Object?, Object?>? firstCopy;
  final hookFailuresBefore = tap.hookFailures;
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
    hookFailed: tap.hookFailures > hookFailuresBefore,
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
    final bool? gone = root.isEmpty ? null : _gone('$root/$id');
    zSay('$step|CLEANED|${cleaned.text}|stageDirGone=${gone ?? 'unknown'}');
    if (!cleaned.shapeOk) need(false, '$what:cleanup-shape:${cleaned.text}');
    // A look that failed is not "gone".
    if (cleaned.removed != true || (root.isNotEmpty && gone != true)) {
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
  // The COPY line and the busy call at the first copy event must have got
  // through.
  if (base.hookFailed) need(false, 'd0:copy-hook-failed');
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
  // No path in the answer: not there. A path whose look failed: unknown.
  final bool? rootExists = rootPath == null ? false : _isDirectory(rootPath);
  final delivered = root.isEmpty ? null : _stageDirState('$root/$idZ');
  zSay('D0|BUSY|code=${busy?.codeToken ?? 'not-requested'}'
      '|answeredBeforeCopyEnd=$busyBeforeCopyEnd'
      '|staged=${_statusOf(base.answer)}|rootExists=${rootExists ?? 'unknown'}'
      '|marker=${delivered?.marker}|owner=${delivered?.ownerWritten}'
      '|payload=${delivered?.payload}|read=${delivered?.read ?? 'na'}');
  if (busy == null || !_failedWith(busy, 'STAGE_IO_FAILED', 'busy')) {
    need(false, 'd0:busy:${busy?.codeToken ?? 'not-requested'}');
  }
  if (!busyBeforeCopyEnd) need(false, 'd0:busy-answered-after-the-copy');
  if (rootExists != true) {
    need(false,
        rootExists == null
            ? 'd0:delivered-root-look-failed'
            : 'd0:delivered-root-missing');
  }
  if (delivered == null) {
    need(false, 'd0:delivered-directory-incomplete');
  } else {
    // A read that failed is none of the facts below.
    if (delivered.read != 'ok') {
      need(false, 'd0:delivered-read:${delivered.read}');
    }
    if (!delivered.marker ||
        !delivered.ownerWritten ||
        !delivered.payload) {
      need(false, 'd0:delivered-directory-incomplete');
    }
  }
  await release('D0', idZ, 'd0');

  // D1 / D2: a staging under intervention.
  Future<void> judged(String step, String token) async {
    final what = 'd${step.substring(1)}';
    final id = run.newId();
    final result = await _stagedUnderTap(run, tap, step, treeUri, id);
    if (result.hookFailed) need(false, '$what:copy-hook-failed');
    final hit = _failedWith(result.answer, 'STAGE_IO_FAILED', token);
    final bool? gone = root.isEmpty ? null : _gone('$root/$id');
    zSay('$step|VERDICT|triggered=$hit|stageDirGone=${gone ?? 'unknown'}');
    // An intervention that landed too late lets the staging SUCCEED. That
    // is `not-triggered` — never a pass.
    if (!hit) need(false, '$what:not-triggered:${result.answer.codeToken}');
    // A failed job must have removed its own stage directory (else the
    // answer would have said `leftover`). A look that failed is not "gone".
    if (hit && root.isNotEmpty && gone != true) {
      need(
          false,
          gone == null
              ? '$what:stage-dir-look-failed'
              : '$what:stage-dir-left');
    }
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
// Part E1 — T7's first order: the receive side upgrades the grant while a
// send staging of the SAME folder is still copying. The folder is
// nested/deep: a different persisted-grant URI from akstage (part C leaves
// READ+WRITE there) and one that holds no grant yet — both read through
// listSafPersisted before anything is staged.
//
// The order is the host's: it confirms the receive picker while the send
// staging copies its big hold file. The result reaches the Activity, the
// take queues behind the running staging, the staging enters its wrap-up,
// the take runs — on Android 7–10 the order that loses the grant unless no
// release falls between a receive pick's grant and its take (G15).
//
// Two things are asserted. The END state: the staging answered ok, the pick
// selected the same URI, the URI holds read AND write, no READ-only grant is
// left anywhere, the journal is absent or empty. And the NATIVE ORDER, read
// from the stager's own trace (X1): the ring is snapshotted before the part
// begins and again once both calls settled, and `_nativeOrderBetween`
// judges the entries between the two — `pick_result < job_end <
// settle_refused < receive_take` under one alias, with no `release` and no
// `settle_begin` of it in between, is `native=strict` (the line
// `E1|NATIVE`). What it catches: a stager that has the receive pick
// REGISTERED and still admits the send job's settlement — a release or an
// admitted settlement then lies between the result and the take:
// `native=not-held`, the failed check `native-not-held`. What it does NOT
// catch: a stager that registers the pick only when its result arrives.
// While that result still precedes the send wrap-up it traces the same
// five entries and ends `rw` — it passes this part's native order AND its
// end state. E2 — the picker parked, the pick registered and NOT confirmed
// while the send staging ends — is what catches a late registration. A
// window that cannot be judged (no result in it, moments missing or out of
// order, the ring overwritten or unreadable) is `native=not-exercised`: no
// device failure; the host calls the round inconclusive.
//
// The journal line is the folder's OWN: at the part's first copy event the
// journal holds a line exactly equal to the folder's URI (`line=target`),
// and it held none before the staging began.
//
// On record, not judged — three order markers without values: the host
// logs at which copy event it confirmed (its own clock); this test logs at
// which copy event the app came back in front, and that the pick answered
// within the slack behind the staging's answer (a take queued behind a
// staging answers right behind it).
//
// From `E1|PICKER2` on no frame is waited for: while DocumentsUI is in
// front the app's are paused, and a pump issued then stays open until the
// picker closes.
// ---------------------------------------------------------------------------

Future<void> _partE1(_Run run) async {
  final failures = <String>[];
  void need(bool ok, String what) {
    if (!ok) failures.add(what);
  }

  void collect() {
    run.problems.addAll(<String>[for (final f in failures) 'E1:$f']);
  }

  final root = run.stagingRoot.isNotEmpty ? run.stagingRoot : await _derivedRoot();

  // The trace ring BEFORE this part starts: everything the part makes the
  // stager trace lies after the largest seq this snapshot holds.
  final traceBefore = await _traceRead();

  // The send-side pick of nested/deep — one more drive. The host moves the
  // folder's payload in while this waits.
  final deep = await _rawPick(run, 'E1', 'pickSendFolder',
      label: _deepName, seconds: _t7PickSeconds);
  if (deep == null) {
    need(false, 'pick-no-tree');
    zSay('E1|DONE|failed=${failures.length}');
    return collect();
  }
  run.deepUri = deep;

  // What this part relies on, read first. A persisted grant belongs to one
  // exact URI: akstage's READ+WRITE (part C) is not a grant on nested/deep,
  // so the send staging of nested/deep will take READ of its own. The
  // journal is read once, as this part finds it: the folder's own line must
  // not be in it yet — the one the first copy event carries is the
  // staging's.
  final akstage = run.treeUri;
  final akstageFlags =
      akstage == deep ? 'same-uri' : await _grantFlags(run, akstage);
  final deepBefore = await _grantFlags(run, deep);
  final readOnlyBefore = await _readOnlyGrants(run);
  final before = root.isEmpty ? null : _journalFacts(root, deep);
  final journalBefore = before?.state ?? 'na';
  final lineBefore = before?.line ?? 'na';
  zSay('E1|PRE|flags=$deepBefore|akstage=$akstageFlags'
      '|readOnly=$readOnlyBefore|journal=$journalBefore|line=$lineBefore');
  need(run.lastPickName == _deepName, 'pick-wrong-folder');
  need(deep != akstage, 'same-uri-as-akstage');
  need(deepBefore == 'absent', 'deep-holds-a-grant:$deepBefore');
  need(akstageFlags == 'rw', 'akstage-not-rw:$akstageFlags');
  need(readOnlyBefore == '0', 'read-only-before:$readOnlyBefore');
  if (lineBefore == 'target') need(false, 'journal-line-before-staging');
  if (root.isEmpty) need(false, 'no-staging-root');

  // The send staging takes READ and journals it. While it copies, the
  // receive side picks the SAME folder: its take is a task queued behind
  // the running staging on the stager's worker.
  final idE = run.newId();
  final tap = _ProgressTap()..install();
  var journalDuring = root.isEmpty ? 'na' : '';
  var lineDuring = 'na';
  tap.onCopy = (event) {
    if (event['stageId'] != idE) return;
    if (journalDuring == '') {
      // Read ONCE, at this part's first copy event: the journal as the
      // take left it.
      final first = root.isEmpty ? null : _journalFacts(root, deep);
      journalDuring = first?.state ?? 'unreadable';
      lineDuring = first?.line ?? 'unreadable';
    }
    // One line per copy event: the host counts them when it confirms.
    zSay('E1|COPY|n=${tap.copyEvents(idE)}|entries=${event['entriesSeen']}'
        '|estimate=${event['totalBytesEstimate']}|journal=$journalDuring'
        '|line=$lineDuring');
  };
  final front = _ForegroundTap()..install(() => tap.copyEvents(idE));
  zSay('E1|START');
  final startedAt = _clock.elapsedMilliseconds;
  final staging = _Answer.capture(_stageCall(deep, idE));
  zSay('E1|PICKER2|tree|awaiting external DocumentsUI drive');
  final pick = _Answer.capture(
      _storageChannel.invokeMethod<Object?>('pickFolder'));
  await staging.wait(_t7WorkSeconds);
  await pick.wait(_pickSeconds);
  tap.onCopy = null;
  front.remove();
  tap.remove();
  if (!pick.answered) run.pickerLeftOpen = true;

  // The ring again, now that both calls have settled: what the stager traced
  // between the two snapshots is this part's native order.
  final traceAfter = await _traceRead();

  final pickMap = pick.value;
  final pickedUri = pickMap is Map && pickMap['treeUri'] is String
      ? pickMap['treeUri'] as String
      : null;
  final stagingAt = staging.answeredAtMs;
  final pickAt = pick.answeredAtMs;
  final pickAfterStagingMs =
      stagingAt != null && pickAt != null ? pickAt - stagingAt : null;
  // Order marker, not a check: the receive take is queued behind the
  // running staging on the stager's worker, so a pick that was confirmed
  // while the copy ran answers right behind the staging — not seconds
  // later.
  final answeredRightBehind = pickAfterStagingMs != null &&
      pickAfterStagingMs >= 0 &&
      pickAfterStagingMs <= _behindSlackMs;
  final copyMs = tap.copyMs(idE);
  zSay('E1|PICKED2|selected=${pickedUri != null}|sameUri=${pickedUri == deep}'
      '|threw=${pick.error == null ? 'no' : pick.verdict}');
  zSay('E1|STAGED|status=${_statusOf(staging)}|copyMs=${copyMs ?? 'na'}'
      '|stagingMs=${stagingAt != null ? stagingAt - startedAt : 'na'}'
      '|pickMs=${pickAt != null ? pickAt - startedAt : 'na'}'
      '|pickAfterStagingMs=${pickAfterStagingMs ?? 'na'}');

  // The native order, from the two snapshots of the ring: seqs, a count
  // and a verdict — never an entry. A snapshot that was no ring adds its
  // fixed fact and nothing else can be said.
  final order = _nativeOrderBetween(traceBefore.value, traceAfter.value);
  final traceFact = _traceReadFact(traceBefore, 'before') ??
      _traceReadFact(traceAfter, 'after');
  zSay('E1|NATIVE|${order.describe}'
      '${traceFact == null ? '' : '|trace=$traceFact'}');
  zSay('E1|ORDER|confirmSeenAfterCopyEvents=${front.resumedAfterEvents ?? 'na'}'
      '|copyEventsTotal=${tap.copyEvents(idE)}'
      '|pickAnsweredAfterStagingMs=${pickAfterStagingMs ?? 'na'}'
      '|pickAnsweredRightBehind=$answeredRightBehind|native=${order.native}');

  // Both succeeded, and the grant is the receive side's now.
  final listedAfter = await _call(
      _storageChannel.invokeMethod<Object?>('listSafPersisted'));
  final deepAfter = _flagsIn(listedAfter, deep);
  final readOnlyAfter = _readOnlyIn(listedAfter);
  final after = root.isEmpty ? null : _journalFacts(root, deep);
  final journalAfter = after?.state ?? 'na';
  final lineAfter = after?.line ?? 'na';
  zSay('E1|AFTER|selected=${pickedUri != null}|sameUri=${pickedUri == deep}'
      '|flags=$deepAfter|readOnly=$readOnlyAfter|journal=$journalAfter'
      '|line=$lineAfter');

  need(_statusOf(staging) == 'ok', 'staged:${_statusOf(staging)}');
  // The take happened before the enumeration: by the first copy event the
  // journal carries the folder's line — `lines:` with a line in it, never
  // "anything but absent" — and that line is EXACTLY the folder's URI: a
  // line of another tree, one that shares only the leaf name, or one
  // character more does not count.
  if (!_journaled(journalDuring)) {
    need(false, 'send-take-not-journalled:$journalDuring');
  }
  if (lineDuring != 'target') {
    need(false, 'send-take-not-the-folders-line:$lineDuring');
  }
  if (pickedUri == null) need(false, 'pick-no-folder');
  if (pickedUri != deep) need(false, 'pick-other-folder');
  // The END state: with the receive take serialized behind the running
  // staging, the tree ends READ+WRITE, no READ-only grant is left and the
  // journal is clear.
  if (deepAfter != 'rw') need(false, 'after-flags:$deepAfter');
  if (readOnlyAfter != 0) need(false, 'after-read-only:$readOnlyAfter');
  if (!_journalClear(journalAfter)) need(false, 'after-journal:$journalAfter');
  // The NATIVE order (G15): a stager that has the pick registered and
  // still admits the send job's settlement releases or settles between the
  // result and the take — `not-held` is this part's device failure. A
  // window that was not exercised is NOT one: the host calls that round
  // inconclusive. (A stager that registers only at the result passes here
  // while the result precedes the send wrap-up; part E2 is what catches it.)
  if (order.native == 'not-held') need(false, 'native-not-held');
  // The copy hook must have got through every copy event.
  if (tap.hookFailures > 0) {
    need(false, 'copy-hook-failed:${tap.hookFailures}');
  }

  final cleaned = await _cleanupNoFrame(idE);
  final bool? gone = root.isEmpty ? null : _gone('$root/$idE');
  zSay('E1|CLEANED|${cleaned.text}|stageDirGone=${gone ?? 'unknown'}');
  if (!cleaned.shapeOk) need(false, 'cleanup-shape:${cleaned.text}');
  if (root.isEmpty) need(false, 'no-staging-root');
  // A look that failed is not "gone".
  if (cleaned.removed != true || (root.isNotEmpty && gone != true)) {
    need(false, 'cleanup:${cleaned.text}/gone=${gone ?? 'unknown'}');
  }

  zSay('E1|DONE|failed=${failures.length}');
  collect();
}

// ---------------------------------------------------------------------------
// Part E2 — T7's second order: the receive pick FIRST. It is the order that
// tells a broken deferral from a correct one on Android 15.
//
// The folder is nested/dip (built by the host, note.txt only — a few bytes),
// and it holds no persisted grant, read through listSafPersisted first; the
// part's tree URI comes from a send-side pick of it, which takes no grant.
//
// `pickFolder` is called FIRST: the platform thread registers the pick while
// it handles that message, before the staging message that follows — both
// leave back to back. The host parks the picker on the folder WITHOUT
// confirming; the send staging runs to its end meanwhile. Observation 1,
// with the picker still parked (the line `E2|HOLD`, the host's cue to
// confirm): the staging answered ok, the URI is READ-only, exactly one
// READ-only grant exists, the journal holds exactly one line and it is the
// folder's own (exactly its URI), and the pick is still unanswered.
// Observation 2, after the pick answered: the URI is READ+WRITE, no
// READ-only grant, the journal absent or empty.
//
// This is the order that catches a LATE registration. A stager that
// registers the receive pick only when its result arrives holds nothing
// here while the picker is parked: its send job's settlement is admitted,
// and the READ and the journal line are gone before the confirm — the
// observation 1 of the broken deferral. One that registers before it
// launches the picker keeps both. Part E1's native order cannot tell the
// two apart while the result still precedes the send wrap-up; this part can.
//
// No frame is waited for anywhere in this part: the system picker is in
// front from `E2|PICKER` until the host has confirmed it.
// ---------------------------------------------------------------------------

Future<void> _partE2(_Run run) async {
  final failures = <String>[];
  void need(bool ok, String what) {
    if (!ok) failures.add(what);
  }

  void collect() {
    run.problems.addAll(<String>[for (final f in failures) 'E2:$f']);
  }

  final root = run.stagingRoot.isNotEmpty ? run.stagingRoot : await _derivedRoot();

  // The part's tree URI, from a send-side pick: a pick takes no grant —
  // read below, before anything is staged.
  final dip = await _rawPick(run, 'E2', 'pickSendFolder',
      picker: 'PICKER0', label: _dipName);
  if (dip == null) {
    need(false, 'pick-no-tree');
    zSay('E2|DONE|failed=${failures.length}');
    return collect();
  }

  final dipBefore = await _grantFlags(run, dip);
  final readOnlyBefore = await _readOnlyGrants(run);
  final before = root.isEmpty ? null : _journalFacts(root, dip);
  final journalBefore = before?.state ?? 'na';
  final lineBefore = before?.line ?? 'na';
  zSay('E2|PRE|flags=$dipBefore|readOnly=$readOnlyBefore'
      '|journal=$journalBefore|line=$lineBefore');
  need(run.lastPickName == _dipName, 'pick-wrong-folder');
  need(dip != run.treeUri && dip != run.deepUri, 'same-uri-as-another-part');
  need(dipBefore == 'absent', 'dip-holds-a-grant:$dipBefore');
  need(readOnlyBefore == '0', 'read-only-before:$readOnlyBefore');
  if (root.isEmpty) need(false, 'no-staging-root');

  // The receive pick FIRST, the staging message right behind it.
  final idQ = run.newId();
  final tap = _ProgressTap()..install();
  tap.onCopy = (event) {
    if (event['stageId'] != idQ) return;
    zSay('E2|COPY|n=${tap.copyEvents(idQ)}|entries=${event['entriesSeen']}'
        '|estimate=${event['totalBytesEstimate']}');
  };
  zSay('E2|PICKER|tree|awaiting external DocumentsUI drive');
  final pick = _Answer.capture(
      _storageChannel.invokeMethod<Object?>('pickFolder'));
  final staging = _Answer.capture(_stageCall(dip, idQ));
  await staging.wait(_workSeconds);
  tap.onCopy = null;
  tap.remove();

  // Observation 1 — the picker is still parked: the send side took READ and
  // journaled it, and nothing has released it. This line is the host's cue.
  final held = await _call(
      _storageChannel.invokeMethod<Object?>('listSafPersisted'));
  final heldFlags = _flagsIn(held, dip);
  final heldReadOnly = _readOnlyIn(held);
  final hold = root.isEmpty ? null : _journalFacts(root, dip);
  final heldJournal = hold?.state ?? 'na';
  final heldLine = hold?.line ?? 'na';
  zSay('E2|HOLD|staged=${_statusOf(staging)}|flags=$heldFlags'
      '|readOnly=$heldReadOnly|journal=$heldJournal|line=$heldLine'
      '|pickAnswered=${pick.answered}');
  need(_statusOf(staging) == 'ok', 'hold-staged:${_statusOf(staging)}');
  need(heldFlags == 'r', 'hold-flags:$heldFlags');
  need(heldReadOnly == 1, 'hold-read-only:$heldReadOnly');
  need(heldJournal == 'lines:1', 'hold-journal:$heldJournal');
  // ... and that one line is EXACTLY the folder's URI — the send side's
  // take of THIS tree, not a line of another.
  need(heldLine == 'target', 'hold-journal-line:$heldLine');
  // The pick is still parked: nothing has confirmed it.
  need(!pick.answered, 'hold-pick-answered');
  // The copy hook must have got through every copy event.
  if (tap.hookFailures > 0) {
    need(false, 'copy-hook-failed:${tap.hookFailures}');
  }

  // The host confirms on that line: the take runs, and the journal is
  // settled behind it.
  await pick.wait(_pickSeconds);
  if (!pick.answered) run.pickerLeftOpen = true;

  // Observation 2 — after the pick answered.
  final pickMap = pick.value;
  final pickedUri = pickMap is Map && pickMap['treeUri'] is String
      ? pickMap['treeUri'] as String
      : null;
  final after = await _call(
      _storageChannel.invokeMethod<Object?>('listSafPersisted'));
  final dipAfter = _flagsIn(after, dip);
  final readOnlyAfter = _readOnlyIn(after);
  final afterFacts = root.isEmpty ? null : _journalFacts(root, dip);
  final journalAfter = afterFacts?.state ?? 'na';
  final lineAfter = afterFacts?.line ?? 'na';
  zSay('E2|AFTER|selected=${pickedUri != null}|sameUri=${pickedUri == dip}'
      '|flags=$dipAfter|readOnly=$readOnlyAfter|journal=$journalAfter'
      '|line=$lineAfter|threw=${pick.error == null ? 'no' : pick.verdict}');
  if (!pick.answered) need(false, 'pick-no-answer');
  if (pickedUri == null) need(false, 'pick-no-folder');
  if (pickedUri != dip) need(false, 'pick-other-folder');
  if (dipAfter != 'rw') need(false, 'after-flags:$dipAfter');
  if (readOnlyAfter != 0) need(false, 'after-read-only:$readOnlyAfter');
  // Clearly empty or missing — a journal that cannot be read is neither.
  if (!_journalClear(journalAfter)) need(false, 'after-journal:$journalAfter');

  // The copy of this part goes. The tree's READ+WRITE stays: it is the
  // receive side's now, and no channel method releases it.
  final cleaned = await _cleanupNoFrame(idQ);
  final bool? gone = root.isEmpty ? null : _gone('$root/$idQ');
  zSay('E2|CLEANED|${cleaned.text}|stageDirGone=${gone ?? 'unknown'}');
  if (!cleaned.shapeOk) need(false, 'cleanup-shape:${cleaned.text}');
  if (root.isEmpty) need(false, 'no-staging-root');
  // A look that failed is not "gone".
  if (cleaned.removed != true || (root.isNotEmpty && gone != true)) {
    need(false, 'cleanup:${cleaned.text}/gone=${gone ?? 'unknown'}');
  }

  zSay('E2|DONE|failed=${failures.length}');
  collect();
}

// ---------------------------------------------------------------------------
// The kill round, launch 1 (`leave`): a send staging of nested/deep, and the
// line at which the host stops the app mid-copy. Nothing is released here
// on purpose — what the staging has on disk when it dies is the point.
// ---------------------------------------------------------------------------

Future<void> _leaveRun(WidgetTester tester) async {
  zSay('K|MODE|leave|android=${io.Platform.isAndroid}'
      '|api=${_apiLevel().level ?? 'unreadable'}');
  expect(io.Platform.isAndroid, isTrue,
      reason: 'the kill round needs the Android channel');

  await tester.pumpWidget(
      const MaterialApp(home: Scaffold(body: SizedBox.shrink())));
  final run = _Run(tester);
  final root = await _derivedRoot();
  final tap = _ProgressTap()..install();

  final deep = await _rawPick(run, 'K', 'pickSendFolder', label: _deepName);
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
  zSay('K|START|flags=$deepBefore|readOnly=$readOnlyBefore'
      '|journal=${root.isEmpty ? 'na' : _journalState(root)}');
  expect(run.lastPickName == _deepName, isTrue,
      reason: 'the picker did not end in nested/deep');
  expect(deepBefore, 'absent',
      reason: 'nested/deep already holds a persisted grant — '
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
    // Read once; a look that failed is `read`, and its facts stay false.
    final state = root.isEmpty ? null : _stageDirState('$root/$id');
    zSay('K|HOLD|marker=${state?.marker}|owner=${state?.ownerWritten}'
        '|payload=${state?.payload}|holdBytes=${state?.holdBytes}'
        '|read=${state?.read ?? 'na'}'
        '|journal=${root.isEmpty ? 'na' : _journalState(root)}');
  };
  final staging = _Answer.capture(_stageCall(deep, id));
  // The app is stopped while this waits: the call normally never answers.
  await _pumpUntil(tester, () => staging.answered, _t7WorkSeconds);
  tap.onCopy = null;
  zSay('K|COMPLETED|code=${staging.codeToken}|copyMs=${tap.copyMs(id) ?? 'na'}'
      '|hookFailures=${tap.hookFailures}');
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

  zSay('K|MODE|check|android=${io.Platform.isAndroid}|holdFull=$holdFull'
      '|api=${_apiLevel().level ?? 'unreadable'}');
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
    // The stage directories of the root by a listing of its own; null when
    // the root could not be looked at or listed.
    final staged = _stagedDirs(root);
    final one =
        staged != null && staged.length == 1
            ? _stageDirState(staged.single.path)
            : null;
    final listedBefore = await _call(
        _storageChannel.invokeMethod<Object?>('listSafPersisted'));
    final readOnlyBefore = _readOnlyIn(listedBefore);
    final grantsBefore = _grantsIn(listedBefore);
    zSay('K|BEFORE|${before.describe}'
        '|marker=${one?.marker}|owner=${one == null ? 'absent' : one.ownerWritten}'
        '|ownerBytes=${one?.ownerBytes}|payload=${one?.payload}'
        '|holdBytes=${one?.holdBytes}|grants=$grantsBefore'
        '|readOnly=$readOnlyBefore|read=${one?.read ?? 'na'}');
    if (!listedBefore.answered || listedBefore.error != null) {
      need(false, 'before-list:${listedBefore.codeToken}');
    }
    // A read that failed is none of the facts below: it is named first.
    if (before.read != 'ok') need(false, 'before-root-read:${before.read}');
    if (staged == null) need(false, 'before-root-list-failed');
    if (before.stageDirs != 1) {
      need(false, 'before-stage-dirs:${before.stageDirs}');
    }
    if (one == null) {
      need(false, 'before-no-stage-directory');
    } else {
      if (one.read != 'ok') need(false, 'before-stage-dir-read:${one.read}');
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
    // Exactly one line — the stopped process's take — and nothing else.
    if (before.journal != 'lines:1') need(false, 'before-journal:${before.journal}');
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

  // AFTER: the staging root holds nothing — and on a fresh install no
  // persisted grant of any kind remains, READ-only or not.
  final listedAfter = await _call(
      _storageChannel.invokeMethod<Object?>('listSafPersisted'));
  final readOnlyAfter = _readOnlyIn(listedAfter);
  final grantsAfter = _grantsIn(listedAfter);
  if (root.isEmpty) {
    zSay('K|AFTER|root=unresolved|readOnly=$readOnlyAfter'
        '|grantsAfter=$grantsAfter');
  } else {
    final after = _rootShape(root);
    zSay('K|AFTER|${after.describe}|readOnly=$readOnlyAfter'
        '|grantsAfter=$grantsAfter');
    if (!after.clear) need(false, 'after-not-clear:${after.describe}');
  }
  if (readOnlyAfter != 0) need(false, 'after-read-only:$readOnlyAfter');
  if (grantsAfter != 0) need(false, 'after-grants:$grantsAfter');

  zSay('K|DONE|failed=${failures.length}');
  expect(failures, isEmpty, reason: 'failed checks: ${failures.join('; ')}');
}
