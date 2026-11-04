/// Real-app E2E (wave 16): one full user journey against the REAL Windows
/// desktop app process (real window, real `FfiTransferService`, real
/// `ak_ffi.dll`), driven through the real UI:
///
/// 1. first run → HomePage identity card (engine `identityInfo` creates the
///    identity: fingerprint short id, `123-456` pairing code, peers 0,
///    first-run banner),
/// 2. Receive page → out-dir + port 0 + admission `any` (warning dialog
///    confirmed) → `开始监听` → the listening banner reports the bound
///    address / full fingerprint / pairing code,
/// 3. an in-process SECOND engine (`AkEngineDart`, its own identity
///    directory — standing in for the sending device) sends a deterministic
///    tree to the address read from the UI; the receive task card must fold
///    to 已完成/传输完成 and the files must land byte-identical on disk,
/// 4. History page refresh shows the `completed` receive record naming the
///    sender fingerprint; the Pairing page shows the sender after the
///    manual pairing-book add, and the HomePage pairing-book count follows
///    0 → 1.
///
/// Identity isolation: the app's identity directory comes from the
/// `AK_ID_DIR` environment override (`FfiTransferService.defaultIdentityDir`
/// reads it first, wave-16). The runner MUST point it at a throwaway
/// directory whose path contains the safety marker `ak-e2e-id` — the test
/// wipes that directory's contents before the journey (device.key /
/// peers.book / checkpoints are pure test artifacts) and refuses to run
/// otherwise, so the real `%USERPROFILE%\.ak-sending` is never touched.
///
/// Runner contract (Git Bash, working directory flutter_app):
///
/// ```bash
/// export PATH="$USERPROFILE/flutter-sdk/bin:$PATH"
/// export AK_ID_DIR="$(cygpath -w "$(mktemp -d "${TMPDIR:-/tmp}/ak-e2e-id-XXXXXX")")"
/// export AK_FFI_DLL="$(cd .. && pwd -W)/target/release/ak_ffi.dll"
/// flutter test integration_test/app_e2e_test.dart -d windows
/// ```
///
/// `AK_FFI_DLL` is belt-and-braces (DLL probe tier 1); without it the CMake
/// step has already bundled the DLL next to the test exe (tier 2).
///
/// Known limitation (honest evidence, AGENTS.md): the engine does NOT
/// auto-write the sender's fingerprint into the receiver's `peers.book`
/// after a successful transfer (the pairing page's "自动记入（TOFU）" copy
/// is aspirational this wave; verified against crates/ak-ffi core.rs — only
/// `peer_add`/`peer_remove` write the book). The checkpoint store DOES
/// record the sender fingerprint as the receive record's `peer` (asserted
/// on the History page), and this test completes the trust story through
/// the pairing page's manual add form, which is the real UI path a user
/// has today.
library;

import 'dart:io' show Directory, File, Platform;
import 'dart:typed_data' show Uint8List;

import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show AkEngineDart, AkSendSpec, JobController, JobPhase, ProgressEvent;

import 'package:ak_sending_app/main.dart' as app;
import 'package:ak_sending_app/ui/pages/home_page.dart' show HomePage;
import 'package:ak_sending_app/ui/pages/pairing_page.dart' show PairingPage;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

/// Path-safety marker required inside `$AK_ID_DIR` before the test wipes it.
const String _idDirMarker = 'ak-e2e-id';

/// Sender device name (history/pairing evidence names it).
const String _senderDeviceName = 'ak-e2e-sender';

late final Directory _receiverIdDir;
late final Directory _senderIdDir;
late final Directory _outDir;
late final Directory _srcTree;
AkEngineDart? _senderEngine;
String _senderFp = '';

/// The deterministic source tree handed to the sender (path → exact bytes).
final Map<String, Uint8List> _expectedFiles = <String, Uint8List>{};

/// A rail destination label, scoped to the NavigationRail so page headings
/// with the same word cannot shadow it (pattern of app_shell_test.dart).
Finder _railLabel(String text) => find.descendant(
      of: find.byType(NavigationRail),
      matching: find.text(text),
    );

/// Every finder used for READING is offstage-tolerant: IndexedStack keeps
/// all six pages mounted (state alive) but only the selected one is
/// onstage/painted, and default finders skip the rest.
Finder _key(String value) =>
    find.byKey(ValueKey<String>(value), skipOffstage: false);

/// Switches to a rail tab by its label, verified by an ONSATGE marker of
/// the target page (default finders only see the selected IndexedStack
/// child), retrying the tap — a lost click must fail loudly with evidence,
/// not stall the journey.
Future<void> _goTab(
  WidgetTester tester, {
  required String label,
  required String onstageMarker,
}) async {
  for (var attempt = 1; attempt <= 3; attempt++) {
    await tester.tap(_railLabel(label));
    await tester.pump(const Duration(milliseconds: 100));
    await Future<void>.delayed(const Duration(milliseconds: 200));
    if (find.textContaining(onstageMarker).evaluate().isNotEmpty) {
      return;
    }
    _log('goTab($label) attempt $attempt: target page not onstage yet; '
        'onstage now: ${_onstageTexts(tester).take(10).toList()}');
  }
  fail('could not switch to tab "$label" via the NavigationRail; onstage '
      'texts: ${_onstageTexts(tester).take(20).toList()}');
}

/// Onstage (visible page) Text data only — selected-tab diagnostics.
List<String> _onstageTexts(WidgetTester tester) {
  final texts = <String>[];
  for (final element in find
      .byWidgetPredicate(
        (widget) =>
            (widget is Text && (widget.data?.isNotEmpty ?? false)) ||
            (widget is SelectableText && (widget.data?.isNotEmpty ?? false)),
      )
      .evaluate()) {
    final widget = element.widget;
    if (widget is Text && widget.data != null) {
      texts.add(widget.data!);
    } else if (widget is SelectableText && widget.data != null) {
      texts.add(widget.data!);
    }
  }
  return texts;
}

void main() {
  // Fixture setup runs BEFORE app.main(): the first frame's HomePage
  // initState triggers the engine's identityInfo, which CREATES the identity
  // on disk — the wipe must win that race deterministically (a synchronous
  // main() body cannot be interleaved with frames).
  _setUpFixture();

  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  tearDownAll(() async {
    // Best-effort cleanup of pure test artifacts (everything lives under
    // the system temp root or the runner-created AK_ID_DIR).
    try {
      _senderEngine?.dispose();
    } on Exception {
      // already gone
    }
    for (final dir in [_srcTree, _outDir, _senderIdDir]) {
      try {
        dir.deleteSync(recursive: true);
      } on Exception {
        // Windows may hold a handle briefly; the OS temp root cleans up —
        // never fatal after the journey itself passed.
      }
    }
    try {
      if (_receiverIdDir.existsSync()) {
        for (final entity in _receiverIdDir.listSync()) {
          entity.deleteSync(recursive: true);
        }
        _receiverIdDir.deleteSync(); // leave no empty shell behind
      }
    } on Exception {
      // same best-effort rule
    }
    _log('tearDownAll: temp artifacts cleaned (best effort)');
  });

  testWidgets(
    'E2E journey: identity → listen(any) → in-process send → bytes on disk '
    '→ history → pairing',
    (tester) async {
      final stopwatch = Stopwatch()..start();
      _log('journey start');

      // The real app, launched INSIDE the test body: the binding resets the
      // widget tree to a placeholder before every testWidgets body
      // (flutter_test `_runTestBody`), so a top-level `app.main()` would be
      // wiped — the canonical integration-test pattern calls it here.
      // main() constructs FfiTransferService (real DLL, real AK_ID_DIR
      // identity dir) and hands it to the NavigationRail shell.
      app.main();

      // ---------------------------------------------------------------------
      // Step 1 — HomePage identity card (first run: engine creates identity).
      // ---------------------------------------------------------------------
      await _pumpUntil(
        tester,
        what: 'home pairing code to fill in (engine identityInfo)',
        timeout: const Duration(seconds: 30),
        isSatisfied: () =>
            RegExp(r'^\d{3}-\d{3}$').hasMatch(
                _selectableData(tester, _key('home_pairing_code')) ?? ''),
      );
      final homeCode =
          _selectableData(tester, _key('home_pairing_code')) ?? '';
      expect(RegExp(r'^\d{3}-\d{3}$').hasMatch(homeCode), isTrue,
          reason: 'pairing code must render in the canonical ddd-ddd form '
              '(got "$homeCode")');

      // Fingerprint chip: short id form `指纹 xxxxxxxx` (8 hex) on the card.
      final homeChipFinder = find.descendant(
        of: find.byType(HomePage, skipOffstage: false),
        matching: find.textContaining(RegExp(r'^指纹 [0-9a-f]{8}$'),
            skipOffstage: false),
      );
      await _pumpUntil(
        tester,
        what: 'home fingerprint chip (non-empty short id)',
        timeout: const Duration(seconds: 10),
        isSatisfied: () => homeChipFinder.evaluate().isNotEmpty,
      );

      // First-run effect: the wiped AK_ID_DIR must now hold a real
      // `device.key` (the engine's identityInfo created it). NOTE the
      // `home_identity_created` banner itself is racy by current app
      // semantics — HomePage AND PairingPage each fire refreshIdentity on
      // initState and the second (created:false) call overwrites the
      // service's cached DeviceInfo before the frame reads it — so the
      // banner is deliberately NOT a hard assertion; disk truth is.
      await _pumpUntil(
        tester,
        what: 'first-run device.key to be created under AK_ID_DIR',
        timeout: const Duration(seconds: 10),
        isSatisfied: () => File(
                '${_receiverIdDir.path}${Platform.pathSeparator}device.key')
            .existsSync(),
      );

      // Pairing book count starts at 0.
      expect(
        _textData(tester, _key('home_peers_count')),
        contains('已配对设备：0 台'),
        reason: 'a wiped identity dir has an empty pairing book',
      );
      _log('step1 ok: pairing code $homeCode, peers 0, device.key created '
          '(${stopwatch.elapsed})');

      // ---------------------------------------------------------------------
      // Step 2 — Receive page: out-dir + port 0 + admission any + confirm.
      // ---------------------------------------------------------------------
      await _goTab(tester, label: '接收', onstageMarker: '监听设置');

      await tester.enterText(_key('receive_out_dir'), _outDir.path);
      await tester.enterText(_key('receive_port'), '0');
      await _tapVisible(tester, _key('receive_admission_any'));

      await _tapVisible(tester, find.text('开始监听'));
      await _pumpUntil(
        tester,
        what: '`any` admission warning dialog',
        timeout: const Duration(seconds: 10),
        isSatisfied: () => find.text('仍要开始').evaluate().isNotEmpty,
      );
      await tester.tap(find.text('仍要开始'));

      // Listening banner: bound address (port 0 → ephemeral, learned from
      // the engine's `listening` progress event folded into the session).
      await _pumpUntil(
        tester,
        what: 'bound ip:port to appear in the listening banner',
        timeout: const Duration(seconds: 30),
        isSatisfied: () => RegExp(r'^\d{1,3}(\.\d{1,3}){3}:\d+$').hasMatch(
            _selectableData(tester, _key('receive_listen_addr')) ?? ''),
      );
      final addr = _selectableData(tester, _key('receive_listen_addr')) ?? '';
      final listenFp =
          _selectableData(tester, _key('receive_listen_fp')) ?? '';
      final listenCode =
          _selectableData(tester, _key('receive_listen_code')) ?? '';
      expect(RegExp(r'^[0-9a-f]{64}$').hasMatch(listenFp), isTrue,
          reason: "the banner shows this device's full 64-hex fingerprint "
              '(got "$listenFp")');
      expect(listenCode.replaceAll(RegExp(r'[^0-9]'), ''),
          homeCode.replaceAll(RegExp(r'[^0-9]'), ''),
          reason: 'banner pairing code matches the HomePage card');
      expect(
        find
            .text('指纹 ${listenFp.substring(0, 8)}', skipOffstage: false)
            .evaluate()
            .isNotEmpty,
        isTrue,
        reason: 'banner full fingerprint agrees with the home chip short id',
      );
      _log('step2 ok: listening on $addr (fp ${listenFp.substring(0, 8)}…) '
          '(${stopwatch.elapsed})');

      // ---------------------------------------------------------------------
      // Step 3 — In-process sender engine → transfer → byte-exact files.
      // ---------------------------------------------------------------------
      // The banner reports the BIND address (0.0.0.0:<ephemeral>); a real
      // sender on another machine would use the host's LAN address. This
      // in-process sender runs on the same host, so it targets loopback
      // with the banner's port (the loopback_demo pattern).
      final listenPort = addr.split(':').last;
      expect(RegExp(r'^\d+$').hasMatch(listenPort), isTrue,
          reason: 'the banner address must carry a numeric port');
      final sender = _openSenderEngine();
      final sendJob = sender.sendStart(AkSendSpec(
        sourceRoots: [_srcTree.path],
        peer: '127.0.0.1:$listenPort',
        fpHex: listenFp,
        chunkKb: 64,
        deviceName: _senderDeviceName,
        timeoutSecs: 60,
        idDir: _senderIdDir.path,
      ));
      final sendController = JobController();
      var senderTerminal = false;
      final transferDeadline = DateTime.now().add(const Duration(seconds: 90));
      while (DateTime.now().isBefore(transferDeadline)) {
        _drainSender(sender, sendController); // engine queue → controller
        if (!senderTerminal) {
          final result = sender.jobResult(sendJob);
          if (!result.isRunning) {
            sendController.setResultJson(result.map!);
            senderTerminal = true;
          }
        }
        await tester.pump();
        await Future<void>.delayed(const Duration(milliseconds: 25));
        if (senderTerminal &&
            _cardHas(tester, '已完成') &&
            _cardHas(tester, '传输完成')) {
          break;
        }
        if (senderTerminal && sendController.phase == JobPhase.failed) {
          break; // fail fast with the error evidence below
        }
      }
      _log('step3 transfer loop: senderTerminal=$senderTerminal, sender '
          'phase=${sendController.phase.name}, sender error='
          '${sendController.error}, card 已完成='
          '${_cardHas(tester, '已完成')}, 传输完成=${_cardHas(tester, '传输完成')} '
          '(${stopwatch.elapsed})');

      if (!(_cardHas(tester, '已完成') && _cardHas(tester, '传输完成'))) {
        _log('receive card texts (incomplete): ${_cardTexts(tester)}');
      }
      expect(senderTerminal, isTrue,
          reason: 'the sender engine job must reach a terminal state');
      expect(sendController.phase, JobPhase.done,
          reason: 'the sender job must succeed (real loopback transfer)');
      _log('receive card texts: ${_cardTexts(tester)}');
      expect(_cardHas(tester, '已完成'), isTrue,
          reason: 'receive task card folds to the done phase chip');
      expect(_cardHas(tester, '传输完成'), isTrue,
          reason: 'receive task card shows the terminal TransferReport');
      expect(
        find
            .descendant(
              of: _key('receive_task_card'),
              matching:
                  find.textContaining(RegExp(r'^\d+ 个文件 · '), skipOffstage: false),
            )
            .evaluate()
            .isNotEmpty,
        isTrue,
        reason: 'the report panel shows a file count summary '
            '(exact disk truth is asserted below)');

      expect(_key('receive_task_card').evaluate().isNotEmpty, isTrue,
          reason: 'the live receive card stays mounted after completion');

      // Files on disk: byte-exact against the expected map (not a re-read of
      // the source — that also rules out sender-side mutation).
      final received = _scanFiles(_outDir);
      expect(received.keys, unorderedEquals(_expectedFiles.keys),
          reason: 'the received tree must hold exactly the sent relative '
              'paths; got ${received.keys.toList()}');
      for (final entry in _expectedFiles.entries) {
        expect(received[entry.key], entry.value,
            reason: 'file ${entry.key} must be byte-identical on disk');
      }
      _log('step3 ok: ${received.length} files byte-identical under '
          '${_outDir.path} (${stopwatch.elapsed})');

      // ---------------------------------------------------------------------
      // Step 4a — History page: completed receive record naming the sender.
      // ---------------------------------------------------------------------
      await _goTab(tester, label: '历史', onstageMarker: '历史任务');
      await _tapVisible(tester, _key('jobs_refresh'));
      await _pumpUntil(
        tester,
        what: 'history refresh to show the completed receive record',
        timeout: const Duration(seconds: 20),
        isSatisfied: () =>
            find
                .descendant(
                  of: find.byType(ListTile, skipOffstage: false),
                  matching: find
                      .textContaining('已完成', skipOffstage: false),
                )
                .evaluate()
                .isNotEmpty &&
            find
                .textContaining('接收 · ', skipOffstage: false)
                .evaluate()
                .isNotEmpty,
      );
      final peerLine = _textData(tester, _key('job_peer_line')) ?? '';
      expect(peerLine, contains('对端 ${_senderFp.substring(0, 8)}'),
          reason: 'the record names the sender fingerprint (the engine '
              'records the admitted peer in the checkpoint store)');
      _log('step4a ok: history record completed, peer line "$peerLine" '
          '(${stopwatch.elapsed})');

      // ---------------------------------------------------------------------
      // Step 4b — Pairing page: sender fingerprint lands in the book.
      // ---------------------------------------------------------------------
      await _goTab(tester, label: '配对', onstageMarker: '添加配对设备');
      // The pairing page is one tall lazy ListView: the book section lives
      // below the fold and is NOT BUILT until scrolled into view.
      final pairingScrollable = find
          .descendant(
            of: find.byType(PairingPage, skipOffstage: false),
            matching: find.byType(Scrollable, skipOffstage: false),
          )
          .first;
      await tester.scrollUntilVisible(
        find.text('还没有配对设备'),
        250,
        scrollable: pairingScrollable,
      );
      await tester.pump(const Duration(milliseconds: 100));
      expect(find.text('还没有配对设备').evaluate().isNotEmpty, isTrue,
          reason: 'no engine-side auto-TOFU this wave — the book stays '
              'empty until the user pairs (documented known limitation)');
      _log('step4b: pairing book initially empty (no auto-TOFU) '
          '(${stopwatch.elapsed})');

      // Scroll back up to the add form and pair the sender by its full
      // fingerprint (the real UI path a user has today).
      await tester.scrollUntilVisible(
        _key('pairing_add_fp'),
        -250,
        scrollable: pairingScrollable,
      );
      await tester.pump(const Duration(milliseconds: 100));
      await tester.enterText(_key('pairing_add_fp'), _senderFp);
      await tester.enterText(_key('pairing_add_name'), _senderDeviceName);
      await _tapVisible(tester, _key('pairing_add_button'));
      // The service's add → listPeers → refreshIdentity chain ends with a
      // confirmation snackbar (an overlay — always built, no scrolling).
      await _pumpUntil(
        tester,
        what: '已添加配对设备 snackbar after the pairing add',
        timeout: const Duration(seconds: 20),
        isSatisfied: () =>
            find.text('已添加配对设备').evaluate().isNotEmpty,
      );

      // The new peer tile renders at the BOTTOM of the lazy list.
      await tester.scrollUntilVisible(
        _key('pairing_peer_name_$_senderFp'),
        250,
        scrollable: pairingScrollable,
      );
      await tester.pump(const Duration(milliseconds: 100));
      expect(
        _textData(tester, _key('pairing_peer_name_$_senderFp')),
        _senderDeviceName,
        reason: 'the paired tile shows the stored name',
      );
      expect(
        _textData(tester, _key('pairing_peers_header')),
        contains('已配对设备（1 台）'),
        reason: 'the pairing page header follows the book size',
      );
      // Disk truth: peers.book now exists under AK_ID_DIR.
      expect(
        File('${_receiverIdDir.path}${Platform.pathSeparator}peers.book')
            .existsSync(),
        isTrue,
        reason: 'peerAdd persisted peers.book in the identity directory',
      );

      // HomePage pairing-book count follows the mutation (0 → 1) — the
      // page is offstage but stays mounted in the IndexedStack.
      await _pumpUntil(
        tester,
        what: 'HomePage pairing-book count to follow the add (0 → 1)',
        timeout: const Duration(seconds: 10),
        isSatisfied: () => (_textData(tester, _key('home_peers_count')) ?? '')
            .contains('已配对设备：1 台'),
      );
      _log('step4b ok: sender paired, home count 0→1 (${stopwatch.elapsed})');

      _log('journey complete in ${stopwatch.elapsed}');
    },
    timeout: const Timeout(Duration(minutes: 5)),
    semanticsEnabled: false,
  );
}

// -----------------------------------------------------------------------------
// Fixture: identity isolation + deterministic source tree.
// -----------------------------------------------------------------------------

/// Validates `AK_ID_DIR` (marker-guarded), wipes it for a true first run,
/// creates the sender identity dir and the deterministic source tree.
void _setUpFixture() {
  final idDirEnv = Platform.environment['AK_ID_DIR'] ?? '';
  if (idDirEnv.isEmpty) {
    throw StateError(
        'AK_ID_DIR is not set. Isolation contract: point it at a throwaway '
        'directory (path must contain "$_idDirMarker") so the real '
        '%USERPROFILE%\\.ak-sending is never touched. See the runner '
        'contract in this file\'s library doc.');
  }
  if (!idDirEnv.contains(_idDirMarker)) {
    throw StateError('AK_ID_DIR "$idDirEnv" lacks the safety marker '
        '"$_idDirMarker"; refusing to wipe an unrecognized directory.');
  }
  _receiverIdDir = Directory(idDirEnv);
  if (_receiverIdDir.existsSync()) {
    for (final entity in _receiverIdDir.listSync()) {
      entity.deleteSync(recursive: true); // device.key/peers.book/checkpoints
    }
  } else {
    _receiverIdDir.createSync(recursive: true);
  }

  // Sender identity dir (separate; drives its own engine).
  _senderIdDir = Directory.systemTemp.createTempSync('ak-e2e-send-id-');

  // Receive out-dir + deterministic source tree.
  final stamp = DateTime.now().millisecondsSinceEpoch;
  _outDir = Directory(
      '${Directory.systemTemp.path}${Platform.pathSeparator}ak-e2e-out-$stamp')
    ..createSync(recursive: true);
  _srcTree = Directory(
      '${Directory.systemTemp.path}${Platform.pathSeparator}ak-e2e-src-$stamp');
  _makeTree(_srcTree);

  _log('fixture: AK_ID_DIR=${_receiverIdDir.path} '
      'AK_FFI_DLL=${Platform.environment['AK_FFI_DLL'] ?? '<unset>'} '
      'out=${_outDir.path} src=${_srcTree.path}');
}

// -----------------------------------------------------------------------------
// Sender engine + tree helpers.
// -----------------------------------------------------------------------------

/// Opens the sender's engine (its own native handle + identity) and reads
/// back the sender fingerprint for the pairing step.
AkEngineDart _openSenderEngine() {
  final existing = _senderEngine;
  if (existing != null && !existing.isDisposed) {
    return existing;
  }
  final engine = AkEngineDart(); // DLL probe: AK_FFI_DLL → exe dir → CWD
  _senderEngine = engine;
  final identity = engine.identityInfo(_senderIdDir.path);
  _senderFp = identity.fingerprint.toLowerCase();
  _log('sender engine open: ${engine.version()} fp '
      '${_senderFp.substring(0, 8)}… code ${identity.pairingCode}');
  return engine;
}

/// Drains the sender engine's progress queue into [controller]; a transient
/// poll failure only skips this tick.
void _drainSender(AkEngineDart engine, JobController controller) {
  try {
    Map<String, dynamic>? line = engine.pollProgress();
    while (line != null) {
      try {
        controller.feed(ProgressEvent.fromJson(line));
      } on FormatException {
        // forward-incompatible line: never fatal for the journey
      }
      line = engine.pollProgress();
    }
  } on Exception {
    // transient engine poll error — the next tick retries
  }
}

/// Deterministic tree: CRLF text, a 4 MiB binary LCG pattern, an empty dir.
void _makeTree(Directory root) {
  void write(String rel, List<int> data) {
    final file = File(
        '${root.path}${Platform.pathSeparator}$rel'
            .replaceAll('/', Platform.pathSeparator));
    file.parent.createSync(recursive: true);
    file.writeAsBytesSync(data);
    _expectedFiles[rel] = Uint8List.fromList(data);
  }

  write('docs/manual; v2 (draft).txt',
      'AK sending E2E\r\nwindows integration journey\r\n'.codeUnits);
  write('photos/raw/payload-4MiB.bin', _patternBytes(4 << 20));
  Directory(
          '${root.path}${Platform.pathSeparator}photos${Platform.pathSeparator}empty-album')
      .createSync(recursive: true);
}

/// Deterministic LCG bytes (64-bit wrapping multiply on the Dart VM).
Uint8List _patternBytes(int length) {
  final data = Uint8List(length);
  var state = 0x414B7365_6E64_E2E5; // "AKsend" + e2e marker seed
  for (var i = 0; i < length; i++) {
    state = state * 6364136223846793005 + 1442695040888963407;
    data[i] = (state >> 56) & 0xFF;
  }
  return data;
}

/// Flattened `relative path -> bytes` view of a received tree (files only,
/// `/`-normalized keys matching [_expectedFiles]).
Map<String, Uint8List> _scanFiles(Directory root) {
  final files = <String, Uint8List>{};
  for (final entity in root.listSync(recursive: true, followLinks: false)) {
    if (entity is! File) continue;
    final relative = entity.path.substring(root.path.length + 1);
    files[relative.replaceAll(r'\', '/')] = entity.readAsBytesSync();
  }
  return files;
}

// -----------------------------------------------------------------------------
// Live-binding pump / read helpers.
// -----------------------------------------------------------------------------

/// Pumps frames (with real-time pacing so the app's engine pollers and
/// async refreshes run) until [isSatisfied] returns true; dumps page text
/// on timeout for diagnosis.
Future<void> _pumpUntil(
  WidgetTester tester, {
  required bool Function() isSatisfied,
  required Duration timeout,
  required String what,
}) async {
  final deadline = DateTime.now().add(timeout);
  while (DateTime.now().isBefore(deadline)) {
    await tester.pump(const Duration(milliseconds: 50));
    await Future<void>.delayed(const Duration(milliseconds: 50));
    if (isSatisfied()) {
      return;
    }
  }
  _log('TIMEOUT waiting for: $what');
  _log('texts now: ${_allTexts(tester)}');
  fail('timed out after $timeout waiting for: $what');
}

/// Scrolls [finder] into view inside its ListView, then taps it (the long
/// receive/pairing forms exceed a 720 px window).
Future<void> _tapVisible(WidgetTester tester, Finder finder) async {
  await tester.ensureVisible(finder);
  await tester.pump(const Duration(milliseconds: 50));
  await tester.tap(finder);
  await tester.pump(const Duration(milliseconds: 50));
}

/// Whether the live receive task card contains [needle] (phase chip /
/// report panel / any folded text).
bool _cardHas(WidgetTester tester, String needle) {
  final card = _key('receive_task_card');
  if (card.evaluate().isEmpty) return false;
  return find
      .descendant(
        of: card,
        matching: find.textContaining(needle, skipOffstage: false),
      )
      .evaluate()
      .isNotEmpty;
}

/// Every Text inside the live receive task card (diagnostics).
List<String> _cardTexts(WidgetTester tester) {
  final card = _key('receive_task_card');
  if (card.evaluate().isEmpty) return const <String>['<no receive card>'];
  return [
    for (final element in find
        .descendant(
          of: card,
          matching: find.byType(Text, skipOffstage: false),
        )
        .evaluate())
      if ((element.widget as Text).data?.isNotEmpty ?? false)
        (element.widget as Text).data!,
  ];
}

/// `.data` of the first SelectableText matching [finder] (null if absent).
String? _selectableData(WidgetTester tester, Finder finder) {
  if (finder.evaluate().isEmpty) return null;
  final widget = tester.widget(finder);
  return widget is SelectableText ? widget.data : null;
}

/// `.data` of the first Text matching [finder].
String? _textData(WidgetTester tester, Finder finder) {
  if (finder.evaluate().isEmpty) return null;
  final widget = tester.widget(finder);
  return widget is Text ? widget.data : null;
}

/// Every Text/SelectableText data string in the tree (timeout diagnostics).
List<String> _allTexts(WidgetTester tester) {
  final texts = <String>[];
  for (final element in find
      .byWidgetPredicate(
        (widget) =>
            (widget is Text && (widget.data?.isNotEmpty ?? false)) ||
            (widget is SelectableText && (widget.data?.isNotEmpty ?? false)),
        skipOffstage: false,
      )
      .evaluate()) {
    final widget = element.widget;
    if (widget is Text && widget.data != null) {
      texts.add(widget.data!);
    } else if (widget is SelectableText && widget.data != null) {
      texts.add(widget.data!);
    }
  }
  return texts;
}

void _log(String message) {
  // ignore: avoid_print
  print('[ak-e2e] $message');
}
