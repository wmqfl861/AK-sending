// Z E2E (batch Z, 2026-11-24): K1 with the folder option — the ANDROID
// SYSTEM tree picker through the app's REAL send page, in a profile build.
// The test joins a receiver-side code as the SENDER (the X4 send secondary
// entry), and on the pairing's auto-opened pick sheet (W7.2) taps 选择文件's
// sibling 选择文件夹 instead. The production gateway then runs the whole
// batch Z send path — ACTION_OPEN_DOCUMENT_TREE, the native staging behind
// `dev.ak/storage` — and hands the STAGED root to the unchanged engine.
//
// The Flutter side is driven by this test; the NATIVE picker is driven from
// the host (adb/uiautomator), for the reasons in the K1 header. K1's
// helpers are imported (its `main` is never called), so the code-entry step
// is literally K1's.
//
// Orchestration contract (all local-only endpoints):
//   * dart-defines: AK_REGISTRY_ADDR=10.0.2.2:17200, AK_STUN_ADDR=
//     10.0.2.2:17101, AK_REGISTRY_ID=<64hex>, AK_SAF_CODE=AKI1-...
//     (the RECEIVER-side task code printed by the host CLI
//     `ak-cli task code-recv`).
//   * the host drives DocumentsUI onto the folder when
//     `ZSAF|E2E|PICKER|tree|awaiting external DocumentsUI drive` shows up
//     in logcat (tag `flutter`), then compares what the receiver got.
//   * the host CLI's consent prompt is answered by its own feeder; this
//     test only observes the app side.
//
// Evidence is one `ZSAF|E2E|…` line per fact, and every field of every line
// is a fixed label, a boolean or a count (batch Z, revision 6, T10):
//   * NO part of a task code prints — not even its last four symbols;
//   * no staged path ever prints (the walkthrough never holds one — it
//     derives the staging root the way Android lays it out);
//   * what the UI shows is compared ON THE DEVICE with what it is expected
//     to be, and only whether it matches prints: the picked item's name
//     against the preset folder's (`nameIsPreset`), the staging-error
//     dialog's title and body against the fixed copy (`titleIsExpected`,
//     `bodyIsExpected`), a failure card's lines against its own mapping;
//   * a failure's reason is a fixed enum, and a readable reason text —
//     the bootstrap's, which may name a path the loader tried — is only said
//     to be there (`error=none|present`).
// `expect` reasons are booleans and fixed names. A call on dart:io that
// throws is caught where it is made and named; a failed read is never "the
// staging root is clear".
//
// The staging root is a HARD check (batch Z, revision 3, T2): it must
// resolve, hold exactly the one staged copy while the send runs, and hold
// nothing once the send succeeded — no stage directory, no other entry, an
// empty grant journal. An unresolvable root, an unreadable one or an
// unexpected layout fails the test; the walkthrough still runs to the end
// first, so the send's own evidence stays on record.
import 'dart:io' as io;

import 'package:ak_sending_app/main.dart' as app;
import 'package:ak_sending_app/ui/app.dart';
import 'package:ak_sending_app/ui/service/saf_send_dialogs.dart';
import 'package:ak_sending_app/ui/service/transfer_errors.dart';
import 'package:ak_sending_app/ui/widgets/transfer_error_card.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:path_provider/path_provider.dart'
    show getApplicationSupportDirectory;

import 'k1_android_saf_test.dart' as k1;

/// The synthetic tree's root, as the host pushed it: the name the picked
/// item must show.
const String _rootName = 'akstage';

/// The staging-error dialog's title as the production words it. Compared
/// with what the dialog shows; never printed with what it shows beside it.
const String _stagingErrorTitle = '无法发送此文件夹';

/// A PlatformException code / reason token as it may print: only when it is
/// a plain token, else `other`. Never a message.
final RegExp _codeShape = RegExp(r'^[A-Z][A-Z0-9_]{0,47}$');
final RegExp _tokenShape = RegExp(r'^[a-z][a-z0-9-]{0,31}$');

String _codeOf(String code) => _codeShape.hasMatch(code) ? code : 'other';

String _tokenOf(String token) =>
    _tokenShape.hasMatch(token) ? token : 'other';

void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  binding.framePolicy = LiveTestWidgetsFlutterBindingFramePolicy.fullyLive;

  testWidgets('Z E2E: system tree picker, staged folder, real send',
      (tester) async {
    const registryAddr = String.fromEnvironment('AK_REGISTRY_ADDR');
    const stunAddr = String.fromEnvironment('AK_STUN_ADDR');
    const registryId = String.fromEnvironment('AK_REGISTRY_ID');
    const safCode = String.fromEnvironment('AK_SAF_CODE');
    expect(registryAddr, '10.0.2.2:17200', reason: 'local registry only');
    expect(stunAddr, '10.0.2.2:17101', reason: 'local STUN only');
    expect(registryId.length, 64, reason: 'registry pin required');
    expect(safCode.startsWith('AKI1-'), isTrue, reason: 'AK_SAF_CODE required');

    final bootstrap = app.bootstrapAppService();
    // The readable reason may name a path the loader tried: only whether
    // there is one prints.
    zSay('E2E|BOOTSTRAP|service=${bootstrap.service != null}'
        '|error=${bootstrap.error == null ? 'none' : 'present'}');
    expect(bootstrap.service, isNotNull,
        reason: 'the app service did not come up (a readable failure is on '
            'the startup card)');
    await tester.pumpWidget(AkSendingApp(service: bootstrap.service!));
    await k1.safUntil(tester,
        () => tester.any(find.byKey(const ValueKey('home_send_button'))),
        'home');

    // The send SECONDARY entry (X4), as K1: the entry declares the
    // direction; the panel asks for the code only.
    await tester
        .tap(find.byKey(const ValueKey('home_send_join_code_button')));
    await tester.pump(const Duration(milliseconds: 600));
    final field = find.byKey(const ValueKey('join_code_field'));
    await k1.safUntil(tester, () => tester.any(field), 'join panel field');
    final submit = find.byKey(const ValueKey('join_submit_button'));
    await tester.enterText(field, safCode);
    // K1's code-entry step, imported rather than copied: wait for the
    // enabled 配对, else drive the panel's own callback.
    var entryPath = 'test-input-stub';
    if (!await k1.submitEnabledWithin(tester, submit, 3)) {
      final panelField = tester.widget<TextField>(field);
      panelField.controller!.value = TextEditingValue(
        text: safCode,
        selection: TextSelection.collapsed(offset: safCode.length),
      );
      panelField.onChanged!(safCode);
      entryPath = 'controller-callback';
      expect(await k1.submitEnabledWithin(tester, submit, 15), isTrue,
          reason: 'submit still disabled after the code entry');
    }
    zSay('E2E|CODE_ENTERED|path=$entryPath');
    await tester.tap(submit);
    // A fixed label only: no part of the code, not even its last four symbols.
    zSay('E2E|JOIN_SUBMITTED|send-entry');

    final connection = find.byWidgetPredicate(
        (w) => w.key != null && w.key.toString().contains('conn_card_name_'));
    await k1.safUntil(tester, () => connection.evaluate().isNotEmpty,
        'paired connection card',
        seconds: 170);
    // W7.2: pairing AUTO-OPENS the detail page and the pick sheet.
    await k1.safUntil(
        tester,
        () => find
            .byKey(const ValueKey('pick_files_option'))
            .evaluate()
            .isNotEmpty,
        'auto-opened pick sheet (W7.2)',
        seconds: 30);
    expect(find.byKey(const ValueKey('conn_detail_page')), findsOneWidget);
    // The bottom sheet animates in: settle before tapping.
    for (var i = 0; i < 6; i++) {
      await tester.pump(const Duration(milliseconds: 300));
    }
    // THE SYSTEM TREE PICKER: the real gateway — no injection seam. The
    // host drives DocumentsUI onto the folder; the staging runs in between.
    await tester.ensureVisible(find.byKey(const ValueKey('pick_dir_option')));
    await tester.tap(find.byKey(const ValueKey('pick_dir_option')));
    zSay('E2E|PICKER|tree|awaiting external DocumentsUI drive');

    final refusal = find.byKey(const ValueKey('staging_error_dialog'));
    final picked = await _pumpUntil(
        tester,
        () =>
            k1.safTextAt(tester, 'picked_count') == '已选 1 项' ||
            refusal.evaluate().isNotEmpty,
        150);
    final refused = refusal.evaluate().isNotEmpty;
    if (refused) {
      // What the dialog shows, compared here with the fixed copy it is
      // expected to show: only whether it matches, and which fixed refusal
      // it was, print. (The refusal itself is the failure, named below.)
      final shown = _stagingErrorShown(tester);
      zSay('E2E|STAGE_ERROR|up=true|titleIsExpected=${shown.titleIsExpected}'
          '|bodyIsExpected=${shown.bodyIsExpected}|reason=${shown.reason}');
    }
    // The staging root the walkthrough never sees a path of: what it holds
    // right after staging is the layout this send starts from.
    final root = await _derivedRoot();
    final before = root.isEmpty ? null : _rootState(root, _rootName);
    // What the UI shows for the picked item, compared here with the preset
    // folder's name: only whether it matches prints — never the name.
    final nameIsPreset = k1.safTextAt(tester, 'picked_item_0') == _rootName;
    zSay('E2E|STAGED|picked=${picked && !refused ? 1 : 0}'
        '|nameIsPreset=$nameIsPreset'
        '|${before?.describe ?? 'root=unresolved'}');
    expect(picked && !refused, isTrue,
        reason: refused
            ? 'the gateway refused the folder'
            : 'picked_count never reached 已选 1 项');
    expect(nameIsPreset, isTrue,
        reason: 'the picked item is not the preset folder');

    await tester.ensureVisible(find.byKey(const ValueKey('conn_send_button')));
    await tester.tap(find.byKey(const ValueKey('conn_send_button')));
    final resultCard = find.byKey(const ValueKey('conn_send_result'));
    final errorCard = find.byKey(const ValueKey('conn_send_error'));
    final ended = await _pumpUntil(
        tester,
        () =>
            resultCard.evaluate().isNotEmpty ||
            errorCard.evaluate().isNotEmpty,
        // The emulator's direct link moves ≈ 130 KB/s: the first complete
        // round (2026-11-25) had 31 of 32 MiB acknowledged when a 4-minute
        // budget ran out. K1's single file took 27 s; this leaves room for
        // a slower link without hiding a stalled one.
        420);
    // What the page shows once the send is over, compared on the device
    // with the fixed word the result starts with: only whether it does, and
    // whether the statistics line is there at all, print.
    final sent =
        k1.safTextAt(tester, 'conn_send_result').contains('发送完成');
    final failed = errorCard.evaluate().isNotEmpty;
    final statsShown =
        k1.safTextAt(tester, 'conn_send_result_stats').isNotEmpty;
    zSay('E2E|RESULT|done=$sent|errorCardUp=$failed|ended=$ended'
        '|statsShown=$statsShown');
    if (failed) {
      // A failure's reason as a fixed enum — the classification the card
      // itself made — and whether the card shows exactly what that
      // classification words. (The failed send is the failure, named below.)
      final failure = _failureShown(tester);
      zSay('E2E|FAILED|up=true|kind=${failure.kind}'
          '|titleIsExpected=${failure.titleIsExpected}'
          '|reasonIsExpected=${failure.reasonIsExpected}');
    }
    expect(sent, isTrue,
        reason: ended
            ? 'the send did not end with its fixed result word'
            : 'no result within 7 minutes');

    // The page releases the staged copy after a SUCCESSFUL send; give the
    // cleanup a moment, then look at what the staging root holds.
    for (var i = 0; i < 5; i++) {
      await tester.pump(const Duration(seconds: 1));
    }
    final after = root.isEmpty ? null : _rootState(root, _rootName);
    // A root that could not be read is none of these layouts: it is named.
    final rootProblems = <String>[
      if (root.isEmpty) 'the staging root could not be resolved',
      if (before != null && before.read != 'ok')
        'the staging root could not be read before the send: ${before.read}',
      if (before != null && before.read == 'ok' && !before.oneCopy)
        'unexpected layout in the staging root before the send: '
            '${before.describe}',
      if (sent && after != null && after.read != 'ok')
        'the staging root could not be read after the send: ${after.read}',
      if (sent && after != null && after.read == 'ok' && !after.clear)
        'the staging root still holds something after the send: '
            '${after.describe}',
    ];
    zSay('E2E|STAGING_ROOT|${after?.describe ?? 'root=unresolved'}');
    expect(rootProblems, isEmpty, reason: rootProblems.join('; '));
    zSay('E2E|DONE');
  }, timeout: const Timeout(Duration(minutes: 12)));
}

/// What the staging-error dialog shows, compared on the device with what it
/// is expected to show. The title is the production's fixed title. The body
/// is the one sentence the production maps the dialog's OWN code and token
/// to — the mapping is called with the widget's own fields, so no copy is
/// duplicated and none prints. [reason] is that code and token when each is
/// a plain token, else `other`.
({bool titleIsExpected, bool bodyIsExpected, String reason})
    _stagingErrorShown(WidgetTester tester) {
  var code = '';
  var token = '';
  final dialog = find.byType(SafSendStagingErrorDialog);
  if (dialog.evaluate().isNotEmpty) {
    final widget = tester.widget<SafSendStagingErrorDialog>(dialog.first);
    code = widget.code;
    token = widget.token;
  }
  return (
    titleIsExpected:
        k1.safTextAt(tester, 'staging_error_title') == _stagingErrorTitle,
    bodyIsExpected: k1.safTextAt(tester, 'staging_error_body') ==
        safSendStagingErrorBody(code, token),
    reason: '${_codeOf(code)}/${_tokenOf(token)}',
  );
}

/// What the failure card shows, compared on the device with its own
/// mapping: the card classifies the error it was given, and its title and
/// reason must be what that classification words. [kind] is the
/// classification's fixed name — the failure's reason as an enum, never as
/// a text; `unknown` when the card or its classification is not there.
({String kind, bool titleIsExpected, bool reasonIsExpected})
    _failureShown(WidgetTester tester) {
  const none = (
    kind: 'unknown',
    titleIsExpected: false,
    reasonIsExpected: false
  );
  try {
    final card = find.descendant(
        of: find.byKey(const ValueKey('conn_send_error')),
        matching: find.byType(TransferErrorCard));
    if (card.evaluate().isEmpty) return none;
    final widget = tester.widget<TransferErrorCard>(card.first);
    final view = transferErrorView(widget.error, widget.scene);
    return (
      kind: view.kind.name,
      titleIsExpected:
          k1.safTextAt(tester, 'conn_send_error_title') == view.title,
      reasonIsExpected:
          k1.safTextAt(tester, 'conn_send_error_reason') == view.reason,
    );
  } on Object {
    // Nothing of the exception prints: the card is simply not as expected.
    return none;
  }
}

void zSay(String value) {
  // ignore: avoid_print
  print('ZSAF|$value');
}

/// The one pump that may be outstanding, shared by every wait: a second
/// `pump()` on top of a stalled one would orphan the first.
Future<void>? _openPump;

/// Pumps until [ready] holds or [seconds] pass; answers whether it holds.
///
/// Unlike K1's `safUntil`, a pump that STALLS — no frame while the system
/// picker is in front of the app, or a frame that simply outlasts the poll
/// while the engine hashes — never fails the wait: at most ONE pump is
/// outstanding, the loop keeps polling [ready] around it, and the next
/// frame completes it.
///
/// [ready] runs BESIDE that open pump, and an open pump owns the guarded
/// scope of the test API: it must not call a guarded member of
/// [WidgetTester] (`any`, `widget`, …) — the first E2E round (2026-11-25)
/// died of exactly that "Guarded function conflict". Read the tree through
/// finders' `evaluate()` or a plain flag.
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
  // A pump still open keeps the guarded scope: the caller's next line is
  // usually a guarded tap or ensureVisible. With the wait satisfied the app
  // is in the foreground and its frame is already due.
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

/// The staging root as Android lays it out: the application-support
/// directory is `<data dir>/files`, the staging root
/// `<data dir>/no_backup/ak-saf-staging`. Resolved through symlinks once it
/// exists: Android's `/data/user/0` is a link to `/data/data`, and the
/// stager answers the resolved form. Empty when the platform directory
/// cannot be resolved — which the staging-root check counts as a failure.
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

/// The grant journal the send side keeps directly under the staging root
/// (batch Z, revision 3, N2), and the scratch file it is rewritten through.
const String _journalName = '.send-grants';
const String _journalScratchName = '.send-grants.tmp';

/// What the staging root holds — by KIND and count, never by name. The
/// journal is a regular file that is gone or EMPTY once every grant it
/// recorded is released; so "nothing left" is no stage directory, no other
/// entry and an empty (or absent) journal.
class _RootState {
  const _RootState({
    required this.exists,
    required this.children,
    required this.stageDirs,
    required this.other,
    required this.scratch,
    required this.journal,
    required this.payloadOk,
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

  /// The journal's scratch file is still there (it exists only while the
  /// journal is rewritten, which is over before a staging answers).
  final bool scratch;

  /// `absent`, `empty`, `<n> bytes`, `not-a-file` or `unreadable` — a size,
  /// never a line.
  final String journal;

  /// A stage directory carries the `payload/<name>` copy of this send.
  final bool payloadOk;

  /// Whether all of this could be read: `ok`; `look-failed` — the root
  /// itself could not be looked at; `list-failed` — it could not be listed;
  /// `read-failed` — an entry inside could not be looked at. After a failed
  /// look nothing here may be read as "the root is clear".
  final String read;

  /// The layout while a staged copy is waiting for its send: exactly one.
  bool get oneCopy =>
      read == 'ok' &&
      exists == true &&
      stageDirs == 1 &&
      other == 0 &&
      payloadOk;

  /// Nothing is left: no stage directory, nothing else, the journal empty.
  bool get clear =>
      read == 'ok' &&
      exists == true &&
      stageDirs == 0 &&
      other == 0 &&
      !scratch &&
      (journal == 'absent' || journal == 'empty');

  /// Fixed words, booleans and counts; `na` / `unknown` for what a failed
  /// look left unread.
  String get describe => read == 'ok'
      ? 'exists=$exists|children=$children|stageDirs=$stageDirs|other=$other'
          '|scratch=$scratch|journal=$journal|payloadOk=$payloadOk'
      : 'exists=${exists ?? 'unknown'}|children=na|stageDirs=na|other=na'
          '|scratch=unknown|journal=$journal|payloadOk=unknown|read=$read';
}

/// Never throws: a look that failed is [read].
_RootState _rootState(String root, String name) {
  final dir = io.Directory(root);
  bool? there;
  try {
    there = dir.existsSync();
  } on Object {
    return _RootState(
      exists: null,
      children: 0,
      stageDirs: 0,
      other: 0,
      scratch: false,
      journal: _journalState(root),
      payloadOk: false,
      read: 'look-failed',
    );
  }
  if (there != true) {
    return const _RootState(
      exists: false,
      children: 0,
      stageDirs: 0,
      other: 0,
      scratch: false,
      journal: 'absent',
      payloadOk: false,
      read: 'ok',
    );
  }
  final List<io.FileSystemEntity> entries;
  try {
    entries = dir.listSync(followLinks: false);
  } on Object {
    return _RootState(
      exists: true,
      children: 0,
      stageDirs: 0,
      other: 0,
      scratch: false,
      journal: _journalState(root),
      payloadOk: false,
      read: 'list-failed',
    );
  }
  var stageDirs = 0;
  var other = 0;
  var scratch = false;
  var payloadOk = false;
  var failed = false;
  for (final entry in entries) {
    final base = entry.path.substring(entry.path.lastIndexOf('/') + 1);
    if (entry is io.Directory) {
      stageDirs += 1;
      // null: the look at the payload itself failed — never "not there".
      final payloadThere = _isDirectory('${entry.path}/payload/$name');
      if (payloadThere == null) failed = true;
      payloadOk = payloadOk || payloadThere == true;
    } else if (entry is io.File && base == _journalScratchName) {
      scratch = true;
    } else if (base != _journalName) {
      other += 1;
    }
  }
  return _RootState(
    exists: true,
    children: entries.length,
    stageDirs: stageDirs,
    other: other,
    scratch: scratch,
    journal: _journalState(root),
    payloadOk: payloadOk,
    read: failed ? 'read-failed' : 'ok',
  );
}

/// Whether a directory is at [path]; null when the look itself failed.
bool? _isDirectory(String path) {
  try {
    return io.Directory(path).existsSync();
  } on Object {
    return null;
  }
}

/// The type of what is at [path], no link followed — or null when the look
/// itself failed.
io.FileSystemEntityType? _typeOf(String path) {
  try {
    return io.FileSystemEntity.typeSync(path, followLinks: false);
  } on Object {
    return null;
  }
}

/// The grant journal by SIZE only — its lines are tree URIs and never
/// print. Never throws: a look or a measure that failed is `unreadable`.
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
    final bytes = journal.lengthSync();
    return bytes == 0 ? 'empty' : '$bytes bytes';
  } on Object {
    return 'unreadable';
  }
}
