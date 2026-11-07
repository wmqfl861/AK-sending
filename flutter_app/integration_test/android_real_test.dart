/// Android real-service integration test (wave 29, rebuilt for the
/// wave-28 two-destination UI after the old invite/identity UI was
/// deleted): runs ON an Android device/emulator against the REAL app
/// stack — real `MainActivity` host with the SAF channel `dev.ak/storage`
/// (wave-15 Kotlin), real bundled `libak_ffi.so` (tier-0
/// `System.loadLibrary` semantics, see lib/services/dll_path.dart), real
/// `FfiSimpleTransferService` wired exactly like `main.dart`'s
/// `bootstrapAppService` (facade → borrowed engine → task-plane export
/// probe → adapter), real pages of `lib/ui/app.dart`.
///
/// Every probe step is wrapped in try/catch (see [_step]): a failure is
/// RECORDED (log + summary) but never interrupts the later independent
/// steps; the pass/fail verdict is rendered once at the end over the
/// required steps, after all evidence has been collected.
///
/// Steps:
///  a. app launch + home empty state: the two pairing actions
///     (生成配对码 / 输入配对码) and the 「当前连接」 empty state render
///     (key-located);
///  b. `libak_ffi.so` load — the borrowed engine's `version()` answers
///     the real `ak-ffi … (wire v…, caps 0x…)` string AND the task-plane
///     export probe found nothing missing (the wave-28 `ak_task_*` set);
///  c. settings page reachable + the save-folder card (`settings_save_dir`):
///     when the task runtime started, its value is the live engine
///     default (`<id_dir>/downloads` or the persisted choice), never a
///     fabricated placeholder;
///  d. OPTIONAL real-registry task-code create (真 TLS, server-deployed
///     registry): only when `AK_REGISTRY_ADDR`+`AK_REGISTRY_ID` are passed
///     as Dart compile constants — the UI path (生成配对码 → role sheet →
///     我要发送) mints a code whose NORMALIZED form is exactly 36 symbols
///     (`AKI1` + 32 Crockford Base32) and the canonical grouped display
///     regroups to itself. Followed by the OPTIONAL connection-state
///     progression watch: with no second device the one-shot code simply
///     waits — that is SKIP, not failure; only an on-screen failure
///     (`flow_error`) or a dead row is a failure;
///  e. SAF storage channel probe — `MethodChannel('dev.ak/storage')`
///     `getStorageInfo` → appDownloadDir / freeBytes / safPersisted
///     (kept from the wave-25 version).
///
/// Registry wiring without compile constants: the task runtime's
/// `ak_task_start` validates the registry endpoint + pin SPEC but dials
/// the registry lazily per create/join op (crates/ak-ffi task.rs
/// `ensure_stack`), so without constants the service is constructed with
/// a syntactically valid loopback placeholder — `start()` then boots the
/// REAL runtime on the .so and seeds the real default save folder with
/// no network dependency at all. With constants, the REAL registry
/// endpoint drives every op (step d exercises it for real).
///
/// ═══════════════════════════════════════════════════════════════════════
///  RUN PREREQUISITES (Git Bash on the Windows build host)
/// ═══════════════════════════════════════════════════════════════════════
///  1. An Android device ONLINE: a booted emulator (`adb devices` must
///     list `emulator-<port>  device`) or a phone with USB debugging
///     authorized. WHPX emulators are slow/heavy — an absent or `offline`
///     device is an environment gap to record, not a code defect.
///  2. Toolchain env (working directory flutter_app):
///
///     export PATH="$USERPROFILE/flutter-sdk/bin:$HOME/android-toolchain/jdk/bin:$PATH"
///     export ANDROID_HOME="$HOME/android-toolchain/sdk"
///
///  3. The cdylib must be inside the built APK: gradle packages
///     `android/app/src/main/jniLibs/<abi>/libak_ffi.so`; verify with
///     `unzip -l build/app/outputs/flutter-apk/app-debug.apk | grep ak_ffi`
///     (the `flutter test` invocation below rebuilds the APK first, so a
///     refreshed jniLibs copy is picked up automatically).
///  4. Step (d) additionally needs the deployed registry reachable FROM
///     THE DEVICE plus its pinned identity, both passed as Dart
///     compile-time constants:
///
///     flutter test integration_test/android_real_test.dart -d <device> \
///       --dart-define=AK_REGISTRY_ADDR=<host:port> \
///       --dart-define=AK_REGISTRY_ID=<64-hex registry fingerprint>
///
///     Without the defines step (d) records SKIP (not a failure). With
///     `AK_REGISTRY_ADDR` but no `AK_REGISTRY_ID` it also SKIPs: the pin
///     is REQUIRED by the protocol (unknown pins are never
///     auto-accepted); print the fingerprint on the server host with
///     `ak-cli registry-server`. The engine requires `registry_id` as
///     exactly 64 hex chars (crates/ak-ffi `hex32` → `[u8; 32]`).
///  5. Identity isolation: on Android there is no `$AK_ID_DIR` — the app
///     sandbox dir resolved by path_provider
///     (`/data/user/0/dev.ak.ak_sending_app/files/.ak-sending`) is used
///     and PERSISTS across runs; rows/settings left by earlier runs may
///     show up and are recorded, never asserted absent.
///  6. The service is constructed BEFORE the first widget pump (the
///     constructor kicks the Android identity-dir resolve via
///     path_provider, and the app shell's initState `start()` freezes
///     whatever registry config the constructor carries).
///  7. Residue note (step d): an unconsumed task code has NO Dart-side
///     revoke — `ak_task_disconnect` revokes a CONNECTION's registry row
///     (wave-29 P2-04), but a never-joined code's row stays on the
///     registry until the runtime dies. The row is inert without its
///     160-bit secret; runs against the real registry leave at most one
///     such row per journey.
/// ═══════════════════════════════════════════════════════════════════════
library;

import 'dart:async' show TimeoutException;
import 'dart:io' show Platform;

import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show AkEngineDart, AkTaskFfiBindings;
import 'package:ak_sending_app/main.dart' show UnusableSimpleTransferService;
import 'package:ak_sending_app/services/ak_engine_facade.dart'
    show AkEngineDartFacade;
import 'package:ak_sending_app/services/dll_path.dart' show resolveAkDllPath;
import 'package:ak_sending_app/services/ffi_simple_transfer_service.dart'
    show FfiSimpleTransferService;
import 'package:ak_sending_app/ui/app.dart' show AkSendingApp;
import 'package:ak_sending_app/ui/service/simple_transfer_service.dart'
    show
        TaskConnection,
        TaskConnectionState,
        formatTaskCode,
        isValidTaskCode,
        normalizeJoinCode;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart' show MethodChannel;
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

/// Registry endpoint passed at compile time (`--dart-define=`); empty when
/// the real-server step should be skipped.
const String _registryAddr = String.fromEnvironment('AK_REGISTRY_ADDR');

/// Pinned registry identity (64 hex; see the header's "REGISTRY ID SHAPE"
/// note), passed at compile time alongside [_registryAddr].
const String _registryIdHex = String.fromEnvironment('AK_REGISTRY_ID');

/// The SAF storage channel of the real MainActivity host (wave-15 Kotlin).
const MethodChannel _storageChannel = MethodChannel('dev.ak/storage');

/// Syntactically valid, never-dialed placeholder registry for the
/// no-constants mode (see the header's registry-wiring note): ak_task_start
/// validates the endpoint/pin SPEC only; the dial happens per create/join
/// op, which this mode never runs.
const String _loopbackRegistryAddr = '127.0.0.1:1';
final String _loopbackRegistryIdHex = '0' * 64; // 64 hex zeros: valid [u8;32]

/// The real wired stack, constructed in `main` (header note 6): null when
/// the .so could not be opened / task-plane exports are missing, with the
/// readable reason in [_engineUnavailableReason]. (The facade itself is
/// only referenced through the service's `engineOwner` release hook.)
AkEngineDart? _engine;
FfiSimpleTransferService? _service;
String? _engineUnavailableReason;

// -----------------------------------------------------------------------------
// Step recorder: every step records an outcome; failures never interrupt.
// -----------------------------------------------------------------------------

enum _StepStatus { passed, failed, skipped }

final class _StepOutcome {
  const _StepOutcome(
    this.name,
    this.status,
    this.required,
    this.detail,
  );

  final String name;
  final _StepStatus status;
  final bool required;
  final String detail;

  String get label => switch (status) {
        _StepStatus.passed => 'PASS',
        _StepStatus.failed => 'FAIL',
        _StepStatus.skipped => 'SKIP',
      };
}

final List<_StepOutcome> _outcomes = <_StepOutcome>[];

/// Runs one isolated probe step: [skipReason] records a SKIP without
/// running [body]; a thrown error/expectation inside [body] is recorded
/// as a FAIL (with the reason) and NEVER propagates — the journey
/// continues so later independent steps still yield their evidence.
Future<void> _step(
  String name, {
  required bool alsoRequired,
  String? skipReason,
  required Future<void> Function() body,
}) async {
  if (skipReason != null) {
    _outcomes.add(
        _StepOutcome(name, _StepStatus.skipped, alsoRequired, skipReason));
    _log('STEP SKIP: $name — $skipReason');
    return;
  }
  try {
    await body();
    _outcomes.add(
        _StepOutcome(name, _StepStatus.passed, alsoRequired, 'ok'));
    _log('STEP PASS: $name');
  } on Object catch (error, stack) {
    _outcomes.add(_StepOutcome(
        name, _StepStatus.failed, alsoRequired, '$error'));
    _log('STEP FAIL: $name — $error');
    _log('stack (head): ${stack.toString().split('\n').take(6).join(' | ')}');
  }
}

/// Renders the evidence table and the final verdict over the REQUIRED
/// steps (all steps have already run by this point — a failure here never
/// hides earlier evidence).
void _summarize() {
  _log('════════════ ANDROID REAL-SERVICE SUMMARY ════════════');
  for (var i = 0; i < _outcomes.length; i++) {
    final outcome = _outcomes[i];
    _log(
      '${i + 1}/${_outcomes.length} ${outcome.label}'
      '${outcome.required ? '' : ' (optional)'} ${outcome.name}'
      ' — ${outcome.detail}',
    );
  }
  final requiredFailures = <String>[
    for (final outcome in _outcomes)
      if (outcome.required && outcome.status == _StepStatus.failed)
        outcome.name,
  ];
  _log(
    requiredFailures.isEmpty
        ? 'verdict: all required steps passed '
              '(${_outcomes.where((o) => o.status == _StepStatus.passed).length} pass / '
              '${_outcomes.where((o) => o.status == _StepStatus.skipped).length} skip / '
              '${_outcomes.where((o) => o.status == _StepStatus.failed).length} fail)'
        : 'verdict: REQUIRED step failures: $requiredFailures',
  );
  expect(
    requiredFailures,
    isEmpty,
    reason: 'required steps failed — see the [ak-android-real] summary '
        'above for per-step evidence',
  );
}

// -----------------------------------------------------------------------------
// Live-binding pump / read helpers (wave-25 hardening conventions).
// -----------------------------------------------------------------------------

/// Offstage-tolerant key finder — the IndexedStack shell keeps both pages
/// mounted while only the selected one is painted.
Finder _key(String value) =>
    find.byKey(ValueKey<String>(value), skipOffstage: false);

/// A navigation destination label, scoped to whichever nav surface the
/// shell picked at this width (NavigationBar at phone widths /
/// NavigationRail on wide surfaces) so page headings with the same word
/// cannot shadow it.
Finder _navDestination(String label) {
  final bar = find.byType(NavigationBar);
  final scope = bar.evaluate().isNotEmpty ? bar : find.byType(NavigationRail);
  return find.descendant(of: scope, matching: find.text(label));
}

/// Waits until [isSatisfied] with real-time pacing (Android plugin
/// round-trips and engine calls run on the real clock under the
/// integration-test binding). CHECK FIRST, pump only while unsatisfied,
/// and keep the frame rate LOW (~4 fps): on the WHPX emulator a sustained
/// forced-frame load on the Impeller/swiftshader pipeline was observed to
/// destabilize the whole emulator (wave-25 runs 1/3 died mid-loop), so
/// this loop buys progress with as few frames as possible.
///
/// Every `pump` await carries its own wall-clock guard: a WEDGED frame
/// (slow Impeller/swiftshader first frames, or a system-side activity
/// relaunch mid-frame) surfaces as a [TimeoutException] recorded by the
/// surrounding [_step] instead of hanging the whole journey in silence.
Future<void> _pumpUntil(
  WidgetTester tester, {
  required bool Function() isSatisfied,
  required Duration timeout,
  required String what,
}) async {
  final done = await _pumpUntilOrTimeout(
    tester,
    isSatisfied: isSatisfied,
    timeout: timeout,
    what: what,
    logTimeoutTexts: true,
  );
  if (!done) {
    fail('timed out after $timeout waiting for: $what');
  }
}

/// Non-failing variant of [_pumpUntil]: returns false on timeout instead
/// of failing the test — used by OPTIONAL waits (step d's no-peer watch)
/// where a timeout is legitimate SKIP evidence, not a defect.
Future<bool> _pumpUntilOrTimeout(
  WidgetTester tester, {
  required bool Function() isSatisfied,
  required Duration timeout,
  required String what,
  bool logTimeoutTexts = false,
}) async {
  final deadline = DateTime.now().add(timeout);
  while (DateTime.now().isBefore(deadline)) {
    if (isSatisfied()) {
      return true;
    }
    await tester.pump(const Duration(milliseconds: 250)).timeout(
        const Duration(seconds: 60),
        onTimeout: () => throw TimeoutException(
            'pump wedged for 60s waiting for: $what'));
    await Future<void>.delayed(const Duration(milliseconds: 250));
  }
  if (logTimeoutTexts) {
    _log('TIMEOUT waiting for: $what');
    _log('texts now: ${_allTexts(tester).take(30).toList()}');
  }
  return false;
}

/// `pumpWidget` with the same wedged-frame guard (see [_pumpUntil]).
Future<void> _pumpWidget(WidgetTester tester, Widget app) =>
    tester.pumpWidget(app).timeout(
      const Duration(seconds: 120),
      onTimeout: () => throw TimeoutException(
          'pumpWidget wedged for 120s (first frame never settled — '
          'slow renderer or system-side activity relaunch)'),
    );

/// Switches to a destination by its label, verified by an ONSTAGE marker
/// of the target page, retrying the tap — a lost tap must fail loudly.
Future<void> _goTab(
  WidgetTester tester, {
  required String label,
  required String onstageMarker,
}) async {
  for (var attempt = 1; attempt <= 3; attempt++) {
    await tester.tap(_navDestination(label));
    await tester.pump(const Duration(milliseconds: 100));
    await Future<void>.delayed(const Duration(milliseconds: 200));
    if (find.textContaining(onstageMarker).evaluate().isNotEmpty) {
      return;
    }
    _log('goTab($label) attempt $attempt: target page not onstage yet; '
        'onstage now: ${_onstageTexts(tester).take(10).toList()}');
  }
  fail('could not switch to tab "$label"; onstage texts: '
      '${_onstageTexts(tester).take(20).toList()}');
}

/// `.data` of the first Text matching [finder] (offstage-tolerant).
String? _textData(WidgetTester tester, Finder finder) {
  if (finder.evaluate().isEmpty) return null;
  final widget = tester.widget(finder);
  return widget is Text ? widget.data : null;
}

/// `.data` of the first SelectableText matching [finder].
String? _selectableData(WidgetTester tester, Finder finder) {
  if (finder.evaluate().isEmpty) return null;
  final widget = tester.widget(finder);
  return widget is SelectableText ? widget.data : null;
}

/// Every Text/SelectableText data string in the tree (diagnostics).
List<String> _allTexts(WidgetTester tester) {
  final texts = <String>[];
  for (final element
      in find
          .byWidgetPredicate(
            (widget) =>
                (widget is Text && (widget.data?.isNotEmpty ?? false)) ||
                (widget is SelectableText &&
                    (widget.data?.isNotEmpty ?? false)),
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

/// Onstage (selected IndexedStack child) Text data only — switch
/// verification + diagnostics.
List<String> _onstageTexts(WidgetTester tester) {
  final texts = <String>[];
  for (final element in find
      .byWidgetPredicate(
        (widget) => widget is Text && (widget.data?.isNotEmpty ?? false),
      )
      .evaluate()) {
    final widget = element.widget;
    if (widget is Text && widget.data != null) {
      texts.add(widget.data!);
    }
  }
  return texts;
}

void _log(String message) {
  // ignore: avoid_print
  print('[ak-android-real] $message');
}

// -----------------------------------------------------------------------------
// The real-stack wiring (main.dart's bootstrapAppService shape, holding
// the facade so version() can be probed before dispose).
// -----------------------------------------------------------------------------

void _wireRealStack() {
  // dll_path tier-0 evidence: on Android the probe must return the bare
  // bundled SONAME (System.loadLibrary semantics).
  final resolution = resolveAkDllPath();
  _log('dll probe resolution: $resolution');
  if (resolution == null) {
    _engineUnavailableReason = '未找到 ak_ffi 动态库（Android tier-0 探测）';
    return;
  }
  final facade = AkEngineDartFacade();
  try {
    final engine = facade.borrowEngine(); // throws when the .so cannot open
    final library = engine.bindings.library;
    final missing = AkTaskFfiBindings.missingTaskExports(library);
    if (missing.isNotEmpty) {
      facade.dispose();
      _engineUnavailableReason =
          'ak_ffi 动态库缺少任务平面导出（${missing.join('、')}）；请更新 ak_ffi 后重试';
      return;
    }
    final realRegistry = _registryAddr.isNotEmpty && _registryIdHex.isNotEmpty;
    _service = FfiSimpleTransferService(
      calls: AkTaskFfiBindings(
          engine: engine.nativeHandle, library: library,
          // C07 lease probe: the facade flips not-alive BEFORE
          // engine_free, so a late poll after teardown throws a
          // StateError instead of touching a freed handle.
          isAlive: () => facade.isEngineAlive),
      // Real registry when both compile constants exist; otherwise the
      // never-dialed loopback placeholder keeps start()'s SPEC validation
      // satisfiable so the REAL runtime still boots (header note).
      registry: realRegistry ? _registryAddr : _loopbackRegistryAddr,
      registryIdHex:
          realRegistry ? _registryIdHex : _loopbackRegistryIdHex,
      // The service's dispose releases the engine AFTER its own borrowers
      // (the P1-05 order) — exactly like main.dart.
      engineOwner: facade.dispose,
    );
    _engine = engine;
    _log('real stack wired: registryMode='
        '${realRegistry ? 'real($_registryAddr)' : 'loopback-placeholder'}');
  } on Object catch (error) {
    facade.dispose();
    _engineUnavailableReason = '$error';
  }
}

// -----------------------------------------------------------------------------
// The journey.
// -----------------------------------------------------------------------------

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  // Constructed BEFORE any widget (header note 6): the constructor kicks
  // the Android identity-dir resolve (path_provider) so awaiting
  // identityDirReady later guarantees the real sandbox dir is anchored
  // before the task runtime ever starts. Construction never throws: a
  // missing/broken .so degrades to the recorded unavailable reason and
  // the app still boots on the inert service (the tolerance branch).
  _wireRealStack();
  if (_engineUnavailableReason != null) {
    _log('engine unavailable: $_engineUnavailableReason');
  }

  tearDownAll(() {
    // Releases the engine too (engineOwner: borrowers first, native owner
    // last). Also stops the task runtime's 50 ms event pump.
    _service?.dispose();
    _engine = null;
    _log('tearDownAll: services disposed');
  });

  testWidgets(
    'android real services: launch+home actions, libak_ffi.so load, '
    'settings save-dir, [real-registry task code], SAF storage channel',
    (tester) async {
      final stopwatch = Stopwatch()..start();
      _log(
        'journey start: os=${Platform.operatingSystem} '
        '${Platform.operatingSystemVersion} '
        'registry=${_registryAddr.isEmpty ? '<unset>' : _registryAddr} '
        '(pin len ${_registryIdHex.length} hex)',
      );

      // The real app — the wired service, or the inert service + the
      // visible startup-error card on the wiring-failure branch (the
      // main.dart tolerance semantics).
      final service = _service ??
          UnusableSimpleTransferService(
              _engineUnavailableReason ?? '未知错误');
      await _pumpWidget(
        tester,
        AkSendingApp(service: service, startupError: _engineUnavailableReason),
      );

      // -----------------------------------------------------------------
      // Step a — app launch + home empty state (key-located).
      // -----------------------------------------------------------------
      await _step(
        'app_launch_home_actions',
        alsoRequired: true,
        body: () async {
          await _pumpUntil(
            tester,
            what: 'home page first frame (home_page key)',
            timeout: const Duration(seconds: 90),
            isSatisfied: () => _key('home_page').evaluate().isNotEmpty,
          );
          expect(
            _key('home_create_code_button').evaluate(),
            isNotEmpty,
            reason: 'the 生成配对码 action renders with the frozen key',
          );
          expect(
            _key('home_join_code_button').evaluate(),
            isNotEmpty,
            reason: 'the 输入配对码 action renders with the frozen key',
          );
          expect(
            _key('connections_empty').evaluate(),
            isNotEmpty,
            reason: 'with no pairing ever run the connections empty state '
                'renders (earlier-run rows are recorded, never asserted '
                'absent — header note 5)',
          );
          _log('home empty state: '
              '${_textData(tester, _key('connections_empty'))}');
        },
      );

      // -----------------------------------------------------------------
      // Step b — libak_ffi.so load: the borrowed engine answers version()
      // AND the wave-28 task-plane export set is complete (probed at
      // wiring; a gap would have degraded to the unavailable reason).
      // -----------------------------------------------------------------
      await _step(
        'libak_ffi_so_load',
        alsoRequired: true,
        body: () async {
          final engine = _engine;
          expect(
            engine,
            isNotNull,
            reason: 'the real stack must be wired — unavailability reason: '
                '$_engineUnavailableReason',
          );
          final versionText = engine!.version();
          _log('engine version(): $versionText');
          expect(
            versionText.startsWith('ak-ffi '),
            isTrue,
            reason: 'tier-0 load of libak_ffi.so failed or answered an '
                'unexpected version string: "$versionText"',
          );
        },
      );

      // -----------------------------------------------------------------
      // Step c — settings page reachable + the save-folder card value.
      // When the task runtime started (wired stack + ak_task_start
      // accepted the spec), the card shows the LIVE engine value — the
      // `<id_dir>/downloads` default or the choice persisted by earlier
      // runs; only the degraded branch (wiring failure) shows empty.
      // -----------------------------------------------------------------
      await _step(
        'settings_page_and_default_save_dir',
        alsoRequired: true,
        body: () async {
          await _goTab(tester, label: '设置', onstageMarker: '默认保存文件夹');
          expect(
            _key('settings_page').evaluate(),
            isNotEmpty,
            reason: 'the settings page root renders with the frozen key',
          );
          expect(
            _key('settings_change_dir_button').evaluate(),
            isNotEmpty,
            reason: 'the 更改文件夹 action renders with the frozen key',
          );
          final shown = _textData(tester, _key('settings_save_dir')) ?? '';
          final live = _service?.saveDir ?? '';
          _log('settings save dir shown="$shown" live="$live"');
          if (live.isNotEmpty) {
            expect(
              shown,
              equals(live),
              reason: 'the save-folder card must show the live engine '
                  'saveDir',
            );
            expect(
              shown,
              isNotEmpty,
              reason: 'a started task runtime always seeds a non-empty '
                  'default save folder',
            );
          } else {
            _log('task runtime not started (tolerance branch) — save-dir '
                'card shows the degraded empty value');
          }
        },
      );

      // -----------------------------------------------------------------
      // Step d1 — OPTIONAL real-registry task-code create (真 TLS) via the
      // REAL UI path: 生成配对码 → 我要发送. Skipped unless BOTH
      // AK_REGISTRY_ADDR and AK_REGISTRY_ID were compiled in; a failure
      // here is only a verdict failure when the operator configured the
      // real server.
      // -----------------------------------------------------------------
      final registryConfigured =
          _registryAddr.isNotEmpty && _registryIdHex.isNotEmpty;
      await _step(
        'real_registry_task_code_create',
        alsoRequired: registryConfigured,
        skipReason: _registryAddr.isEmpty
            ? 'AK_REGISTRY_ADDR not compiled in (--dart-define) — '
                'real-server step intentionally not attempted'
            : _registryIdHex.isEmpty
                ? 'AK_REGISTRY_ADDR set but AK_REGISTRY_ID (64-hex pinned '
                    'registry fingerprint) missing — the pin is required; '
                    'print it with `ak-cli registry-server` on the server '
                    'host'
                : null,
        body: () async {
          final wired = _service;
          expect(
            wired,
            isNotNull,
            reason: 'the real stack must be wired for a registry journey',
          );
          // Anchor the REAL identity dir and make sure the task runtime
          // is up (the shell's initState start() may still be in flight
          // on a slow emulator; FfiSimpleTransferService.start is safe to
          // re-await — the engine's runtime start is idempotent).
          await wired!.identityDirReady;
          try {
            await wired.start();
          } on Object catch (error) {
            _log('service.start() re-await: $error (shell may have '
                'already started it — continuing)');
          }

          // Back to the home tab, then the real UI path.
          await _goTab(tester, label: '连接', onstageMarker: '当前连接');
          await tester.tap(_key('home_create_code_button'));
          await _pumpUntil(
            tester,
            what: 'the role bottom sheet (role_send_button)',
            timeout: const Duration(seconds: 30),
            isSatisfied: () =>
                _key('role_send_button').evaluate().isNotEmpty,
          );
          await tester.tap(_key('role_send_button'));
          // The real registry claim: a TLS round-trip to the deployed
          // server (budget generous for the slow emulator uplink).
          await _pumpUntil(
            tester,
            what: 'the minted task code to appear (flow_pairing_code)',
            timeout: const Duration(seconds: 120),
            isSatisfied: () {
              final code =
                  _selectableData(tester, _key('flow_pairing_code')) ?? '';
              return code.isNotEmpty && isValidTaskCode(normalizeJoinCode(code));
            },
          );
          final code =
              _selectableData(tester, _key('flow_pairing_code')) ?? '';
          final normalized = normalizeJoinCode(code);
          _log('task code minted over real TLS: $code');
          expect(
            normalized.length,
            equals(36),
            reason: 'the normalized task code is exactly 36 symbols '
                '(AKI1 + 32 Crockford Base32); got "$normalized" '
                '(${normalized.length})',
          );
          expect(
            isValidTaskCode(normalized),
            isTrue,
            reason: 'the code must match AKI1 + 32 Crockford Base32 '
                '(I/L/O/U excluded); got "$normalized"',
          );
          expect(
            formatTaskCode(normalized),
            equals(code),
            reason: 'the engine-minted display form must already be the '
                'canonical grouped AKI1-XXXX-… form',
          );
          expect(
            _key('flow_waiting').evaluate(),
            isNotEmpty,
            reason: 'after minting, the flow waits for the peer to join',
          );
        },
      );

      // -----------------------------------------------------------------
      // Step d2 — OPTIONAL connection-state progression. No second device
      // is attached to an emulator run: the one-shot code simply waits —
      // SKIP. Only evidence of a REAL failure (the on-screen flow_error,
      // or a row that died) fails; a live row in ANY non-failed state
      // (connecting/testing/connected/unreachable probe verdict) passes.
      // -----------------------------------------------------------------
      TaskConnection? appearedRow;
      if (registryConfigured && _service != null) {
        final rows = <List<TaskConnection>>[];
        final sub = _service!.connections.listen(rows.add);
        final sawRow = await _pumpUntilOrTimeout(
          tester,
          what: 'any connection row (a peer joining the minted code)',
          timeout: const Duration(seconds: 20),
          isSatisfied: () =>
              rows.any((snapshot) => snapshot.isNotEmpty),
        );
        await sub.cancel();
        for (final snapshot in rows) {
          if (snapshot.isNotEmpty) {
            appearedRow = snapshot.first;
            break;
          }
        }
        _log(sawRow
            ? 'connection row appeared: id=${appearedRow?.id} '
                'state=${appearedRow?.state.name}'
            : 'no peer joined within 20s (expected on a solo emulator)');
      }
      await _step(
        'connection_state_progression',
        alsoRequired: false,
        skipReason: !registryConfigured
            ? 'no real registry configured — no code was minted'
            : appearedRow == null
                ? 'no peer joined the one-shot code within 20s (solo '
                    'emulator run — the code simply waits; SKIP per the '
                    'wave-29 contract)'
                : null,
        body: () async {
          final row = appearedRow;
          expect(row, isNotNull);
          _log('row progression: state=${row!.state.name} '
              'role=${row.role.name} peer=${row.peerName}');
          expect(
            row.state,
            isNot(equals(TaskConnectionState.disconnected)),
            reason: 'a freshly paired row may sit in ANY live state but '
                'must not already be dead',
          );
          expect(
            _key('flow_error').evaluate(),
            isEmpty,
            reason: 'the flow must not have failed on screen — see '
                '${_textData(tester, _key('flow_error'))}',
          );
        },
      );

      // -----------------------------------------------------------------
      // Step e — SAF storage channel probe (dev.ak/storage getStorageInfo).
      // -----------------------------------------------------------------
      await _step(
        'saf_storage_channel_probe',
        alsoRequired: true,
        body: () async {
          final raw = await _storageChannel
              .invokeMethod<Map<dynamic, dynamic>>('getStorageInfo');
          _log('getStorageInfo raw: $raw');
          expect(
            raw,
            isNotNull,
            reason: 'the dev.ak/storage channel answered getStorageInfo '
                '(real MainActivity host required)',
          );
          final dir = raw!['appDownloadDir'];
          final freeBytes = raw['freeBytes'];
          final safPersisted = raw['safPersisted'];
          expect(
            dir is String && dir.contains('Download'),
            isTrue,
            reason: 'appDownloadDir must be the app-specific Download dir; '
                'got "$dir"',
          );
          expect(
            freeBytes is int,
            isTrue,
            reason: 'freeBytes must be an int (volume bytes, -1 = unknown '
                '— never fabricated); got "$freeBytes"',
          );
          expect(
            safPersisted is bool,
            isTrue,
            reason: 'safPersisted must be a bool; got "$safPersisted"',
          );
          _log(
            'SAF probe: appDownloadDir=$dir freeBytes=$freeBytes '
            'safPersisted=$safPersisted',
          );
        },
      );

      // All evidence collected — render the table, then the verdict.
      _log('journey steps done in ${stopwatch.elapsed}');
      _summarize();
    },
    timeout: const Timeout(Duration(minutes: 15)),
    semanticsEnabled: false,
  );
}
