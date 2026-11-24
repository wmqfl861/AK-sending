// Z probe (batch Z, 2026-11-24): the ANDROID SEND-SIDE folder path against
// the REAL platform in a profile build — the system tree picker
// (ACTION_OPEN_DOCUMENT_TREE), the native staging behind `dev.ak/storage`,
// the content of the staged copy, its release, and the failure paths of the
// channel. The Flutter side is driven by this test; the NATIVE picker is
// driven from the host (adb/uiautomator), for the reasons recorded in the
// header of the K1 walkthrough (k1_android_saf_test.dart).
//
// Orchestration contract:
//   * dart-define: AK_SAF_PROBE_SHA_BIG=<64 hex> — the host's SHA-256 of
//     the 32 MiB `nested/photo.bin` it pushed to /sdcard/Download/akstage.
//   * every evidence line `ZSAF|<part>|PICKER|tree|awaiting external
//     DocumentsUI drive` (logcat, tag `flutter`) asks the host to drive
//     DocumentsUI onto that folder: once for part A, once for part B and
//     once for part C.
//   * the synthetic tree, whose host manifest is the reference:
//       nested/deep/note.txt   "ak stage probe" + LF
//       nested/photo.bin       33554432 random bytes
//       empty.txt              0 bytes
//       emptydir/              an empty directory
//       sub/说明 文本.txt       "中文 名字" + LF
//
// Part A runs the PRODUCTION gateway with no injected seam: pick → stage →
// verify the staged tree → release. Parts B and C go through the raw
// channel. B is the failure paths — a malformed id, an untrusted authority,
// a cancel that must land in the copy phase, an idempotent cleanup, and
// the stale cleanup next to a RUNNING staging. C proves that a send never
// strips the RECEIVE grant of the same folder.
//
// Evidence is one `ZSAF|…` line per fact. A staged path, a tree URI and a
// stage id never print: counts, lengths and booleans stand in. One run is
// long (three picker drives), so a failed check is COLLECTED under its name
// and the run goes on; the test fails on the collected list at the end.
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

/// How long a pick may stay open. The host has seen the PICKER line by
/// then, so this only bounds a picker drive that never comes.
const int _pickSeconds = 240;
const int _workSeconds = 120;

final RegExp _idShape = RegExp(r'^[0-9a-f]{32}$');
final Random _random = Random.secure();

void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  binding.framePolicy = LiveTestWidgetsFlutterBindingFramePolicy.fullyLive;

  testWidgets(
      'Z SAF: tree pick, native staging, staged-tree verification, release and failure paths',
      (tester) async {
    const shaBig = String.fromEnvironment('AK_SAF_PROBE_SHA_BIG');
    zSay('BOOTSTRAP|android=${io.Platform.isAndroid}'
        '|shaBig=${shaBig.length}hex');
    expect(shaBig, matches(RegExp(r'^[0-9a-f]{64}$')),
        reason: 'AK_SAF_PROBE_SHA_BIG (64 lowercase hex) is required');
    expect(io.Platform.isAndroid, isTrue,
        reason: 'the probe needs the Android channel');

    final run = _Run(tester);
    addTearDown(() async {
      // Whatever went wrong, no staged copy of this run stays behind. The
      // cleanup is idempotent, and the platform queues it behind a staging
      // that is still in flight.
      for (final id in run.owned) {
        try {
          await _storageChannel.invokeMethod<void>(
              'cleanupStagedSend', <String, Object?>{'stageId': id});
        } on Object {
          // Best effort, exactly like the gateway's own release.
        }
      }
      _storageChannel.setMethodCallHandler(null);
    });

    // The production gateway reaches the root navigator for its dialogs.
    await tester.pumpWidget(
        const MaterialApp(home: Scaffold(body: SizedBox.shrink())));

    await _partA(run, shaBig);
    if (!run.pickerStalled) await _partB(run);
    if (!run.pickerStalled) await _partC(run);

    zSay('VERDICT|${run.problems.isEmpty ? 'ok' : 'failed=${run.problems.length}'}');
    expect(run.problems, isEmpty,
        reason: 'failed checks: ${run.problems.join('; ')}');
  }, timeout: const Timeout(Duration(minutes: 12)));
}

void zSay(String value) {
  // ignore: avoid_print
  print('ZSAF|$value');
}

/// What one run collects: the failed checks under their names, every stage
/// id this run may leave behind, and whether a picker drive never came —
/// the parts after that cannot run.
class _Run {
  _Run(this.tester);

  final WidgetTester tester;
  final List<String> problems = <String>[];
  final Set<String> owned = <String>{};
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
}

/// The answer of a channel call, captured whatever it was: [value] on
/// success, [error] on a PlatformException or anything else. An [_Answer]
/// itself never fails, so a call can stay unawaited without an unhandled
/// error and be judged later.
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

/// Awaits [call] (pumping meanwhile) and hands its captured answer.
Future<_Answer> _settled(_Run run, Future<Object?> call) async {
  final answer = _Answer.capture(call);
  await _pumpUntil(run.tester, () => answer.answered, _workSeconds);
  return answer;
}

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

int _childCount(io.Directory dir) =>
    dir.existsSync() ? dir.listSync().length : 0;

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
/// resolved — this is a cross-check, never a precondition.
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
Future<String?> _rawPick(_Run run, String part, String method) async {
  zSay('$part|PICKER|tree|awaiting external DocumentsUI drive');
  final picked = await _settled(
      run, _storageChannel.invokeMethod<Object?>(method));
  if (!picked.answered) {
    run.pickerStalled = true;
  }
  final map = picked.value;
  final treeUri = map is Map && map['treeUri'] is String
      ? map['treeUri'] as String
      : null;
  zSay('$part|PICKED|selected=${treeUri != null}'
      '|name=${map is Map ? map['displayName'] : ''}'
      '|uriIsTree=${treeUri?.startsWith('content://')}'
      '|threw=${picked.error == null ? 'no' : picked.verdict}');
  return treeUri;
}

/// `ok` or the status the answer map carries; a failed or missing call
/// shows its PlatformException code instead.
String _statusOf(_Answer answer) {
  if (!answer.answered) return 'no-answer';
  if (answer.error != null) return answer.verdict;
  final map = answer.value;
  return map is Map ? '${map['status']}' : 'no-map';
}

/// `rw`, `r-`, `-w` or `--` for the persisted grant of exactly [treeUri];
/// `absent` without one. The URIs themselves stay on the device.
Future<String> _grantFlags(_Run run, String treeUri) async {
  final listed = await _settled(
      run, _storageChannel.invokeMethod<Object?>('listSafPersisted'));
  final grants = listed.value;
  if (!listed.answered || grants is! List) {
    return 'error:${listed.verdict}';
  }
  for (final grant in grants) {
    if (grant is Map && grant['uri'] == treeUri) {
      return '${grant['read'] == true ? 'r' : '-'}'
          '${grant['write'] == true ? 'w' : '-'}';
    }
  }
  return 'absent';
}

/// How many persisted grants are READ-ONLY: what the send side adds, and
/// only it; a receive directory holds READ+WRITE.
Future<String> _readOnlyGrants(_Run run) async {
  final listed = await _settled(
      run, _storageChannel.invokeMethod<Object?>('listSafPersisted'));
  final grants = listed.value;
  if (!listed.answered || grants is! List) {
    return 'error:${listed.verdict}';
  }
  var left = 0;
  for (final grant in grants) {
    if (grant is Map && grant['read'] == true && grant['write'] != true) {
      left += 1;
    }
  }
  return '$left';
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
    if (entity is io.Directory) {
      zSay('A|ENTRY|rel=$rel|kind=dir');
      continue;
    }
    if (entity is! io.File) {
      // The stager never writes a link: anything else is a defect to name.
      need(false, 'kind:$rel');
      zSay('A|ENTRY|rel=$rel|kind=other');
      continue;
    }
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

  final siblings = stagingRoot.existsSync()
      ? stagingRoot.listSync()
      : const <io.FileSystemEntity>[];
  if (siblings.length != 1 || siblings.first.path != stageDir.path) {
    need(false, 'staging-root-children(${siblings.length})');
  }
  final stageChildren = stageDir.existsSync()
      ? <String>{for (final e in stageDir.listSync()) _basename(e.path)}
      : const <String>{};
  need(
      stageChildren.length == 2 &&
          stageChildren.contains('payload') &&
          stageChildren.contains('.ak-stage-complete'),
      'stage-dir-contents');

  zSay('A|CHECK|${failures.isEmpty ? 'ok' : 'failed=${failures.join(',')}'}');

  await gateway.releaseStaged(<PickedEntry>[entry]);
  final gone = !stageDir.existsSync();
  final rootChildren = _childCount(stagingRoot);
  zSay('A|RELEASED|stageDirGone=$gone|stagingRootChildren=$rootChildren');
  if (!gone) need(false, 'release-stage-dir');
  if (rootChildren != 0) need(false, 'release-staging-root($rootChildren)');

  // Cross-check of the derived root the E2E walkthrough lists (it never
  // sees the staged path): the staged copy must sit under it.
  final derived = await _derivedRoot();
  zSay('A|ROOT|derivedOk='
      '${derived.isEmpty ? 'error' : path.startsWith('$derived/')}');
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

  Future<_Answer> stage(String uri, String stageId) => _settled(
      run,
      _storageChannel.invokeMethod<Object?>('stageSafTree',
          <String, Object?>{'treeUri': uri, 'stageId': stageId}));

  /// `ok` only for an answer that is null: the cleanup returns nothing,
  /// whether or not the stage directory was still there.
  Future<String> cleanup(String stageId) async {
    final answer = await _settled(
        run,
        _storageChannel.invokeMethod<Object?>(
            'cleanupStagedSend', <String, Object?>{'stageId': stageId}));
    if (!answer.answered) return 'no-answer';
    if (answer.error != null) return answer.verdict;
    return answer.value == null ? 'ok' : 'non-null';
  }

  // A malformed id is refused before anything is read or written.
  var code = (await stage(treeUri, 'not-an-id')).verdict;
  zSay('B|BADID|code=$code');
  if (code != 'STAGE_INVALID_ID') need(false, 'badid-code:$code');

  // An authority outside the allow-list is refused before any grant.
  code = (await stage(
          'content://com.example.untrusted.documents/tree/x', run.newId()))
      .verdict;
  zSay('B|UNTRUSTED|code=$code');
  if (code != 'SAF_PROVIDER_UNTRUSTED') need(false, 'untrusted-code:$code');

  // A cancel must land while the copy is running: the listener is on the
  // channel BEFORE the staging call starts, and the cancel goes out at the
  // FIRST event whose phase is `copy`.
  final idB = run.newId();
  final phases = <String>[];
  var copyEvents = 0;
  var cancelAtEvent = 0;
  var bytesAtCancel = -1;
  _storageChannel.setMethodCallHandler((MethodCall call) async {
    final args = call.arguments;
    if (call.method != 'stagingProgress' || args is! Map) return null;
    final phase = '${args['phase']}';
    phases.add(phase);
    if (phase == 'copy') copyEvents += 1;
    if (cancelAtEvent == 0 && phase == 'copy' && args['stageId'] == idB) {
      cancelAtEvent = phases.length;
      bytesAtCancel = args['bytesCopied'] as int? ?? -1;
      _Answer.capture(_storageChannel.invokeMethod<void>(
          'cancelStaging', <String, Object?>{'stageId': idB}));
    }
    return null;
  });
  final cancelling = _Answer.capture(_storageChannel.invokeMethod<Object?>(
      'stageSafTree', <String, Object?>{'treeUri': treeUri, 'stageId': idB}));
  await _pumpUntil(run.tester, () => cancelling.answered, _workSeconds);
  zSay('B|CANCEL|code=${cancelling.verdict}|events=${phases.length}'
      '|lastPhase=${phases.isEmpty ? 'none' : phases.last}'
      '|copyEvents=$copyEvents|cancelSentAtEvent=$cancelAtEvent'
      '|bytesAtCancel=$bytesAtCancel');
  if (cancelAtEvent == 0) need(false, 'cancel-no-copy-event');
  if (cancelling.verdict != 'STAGE_CANCELLED') {
    need(false, 'cancel-code:${cancelling.verdict}');
  }

  // The cleanup answers null for an id that is already gone, every time.
  final first = await cleanup(idB);
  final second = await cleanup(idB);
  zSay('B|IDEMPOTENT|first=$first|second=$second');
  if (first != 'ok' || second != 'ok') need(false, 'idempotent:$first/$second');

  // The stale cleanup next to a RUNNING staging: it must not touch the
  // running job — its copy and its completion marker are still there once
  // both calls have answered.
  final idC = run.newId();
  final active = _Answer.capture(_storageChannel.invokeMethod<Object?>(
      'stageSafTree', <String, Object?>{'treeUri': treeUri, 'stageId': idC}));
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
  var payloadKept = false;
  var markerKept = false;
  var stageChildren = 0;
  if (activePath != null) {
    final stageDir = io.Directory(activePath).parent.parent;
    final children = stageDir.existsSync()
        ? <String>{for (final e in stageDir.listSync()) _basename(e.path)}
        : const <String>{};
    stageChildren = children.length;
    markerKept = children.contains('.ak-stage-complete');
    payloadKept =
        io.Directory('${stageDir.path}/payload/$activeName').existsSync();
  }
  zSay('B|ACTIVE'
      '|staleRemoved=${staleMap is Map ? '${staleMap['removed']}' : stale.verdict}'
      '|staged=${_statusOf(active)}'
      '|payloadKept=$payloadKept|markerKept=$markerKept'
      '|stageDirChildren=$stageChildren');
  if (stale.error != null) {
    need(false, 'active-stale-cleanup:${stale.verdict}');
  }
  if (_statusOf(active) != 'ok') {
    need(false, 'active-staged:${_statusOf(active)}');
  }
  if (!payloadKept || !markerKept) need(false, 'active-kept-copy-or-marker');
  final cleanedC = await cleanup(idC);
  final cGone = activePath != null &&
      !io.Directory(activePath).parent.parent.existsSync();
  zSay('B|ACTIVE_CLEANED|code=$cleanedC|stageDirGone=$cGone');
  if (cleanedC != 'ok' || !cGone) need(false, 'active-cleanup:$cleanedC');

  zSay('B|GRANT|left=${await _readOnlyGrants(run)}');
  _storageChannel.setMethodCallHandler(null);
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
  final staged = await _settled(
      run,
      _storageChannel.invokeMethod<Object?>('stageSafTree',
          <String, Object?>{'treeUri': treeUri, 'stageId': idD}));
  final after = await _grantFlags(run, treeUri);
  zSay('C|STAGED|status=${_statusOf(staged)}');
  zSay('C|GRANT|before=$before|after=$after');
  if (before != 'rw') need(false, 'grant-before:$before');
  if (_statusOf(staged) != 'ok') need(false, 'staged:${_statusOf(staged)}');
  if (after != 'rw') need(false, 'grant-after:$after');
  final cleaned = await _settled(
          run,
          _storageChannel.invokeMethod<Object?>(
              'cleanupStagedSend', <String, Object?>{'stageId': idD}))
      .then((answer) => answer.verdict);
  if (cleaned != 'ok') need(false, 'cleanup:$cleaned');

  zSay('C|DONE|failed=${failures.length}');
  run.problems.addAll(<String>[for (final f in failures) 'C:$f']);
}
