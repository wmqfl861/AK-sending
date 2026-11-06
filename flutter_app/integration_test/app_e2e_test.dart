/// Real-app smoke E2E (wave 29, rebuilt after the wave-28 UI cutover):
/// one short journey against the REAL Windows desktop app process (real
/// window, real `ak_ffi.dll`, real `FfiSimpleTransferService` via
/// `main.dart`'s own [bootstrapAppService] production wiring) through the
/// new two-destination shell (「连接」/「设置」).
///
/// Steps (every probe wrapped in try/catch — see [_step]: a failure is
/// RECORDED, never interrupts later independent steps; the verdict is
/// rendered once at the end over the required steps):
///  1. real engine bootstrap — DLL probe tier evidence + which branch
///     `bootstrapAppService` took (live service vs the readable
///     construction failure);
///  2. home page smoke — the two pairing actions (生成配对码 / 输入配对码,
///     keys `home_create_code_button` / `home_join_code_button`) plus the
///     「当前连接」 empty state (`connections_empty`);
///  3. settings page — reachable via the navigation, the save-folder card
///     (`settings_save_dir`) renders, and when the engine actually
///     started its value equals the live `service.saveDir` (the engine's
///     `<id_dir>/downloads` default, or the persisted choice);
///  4. startup tolerance — when the engine-unavailable card
///     (`app_startup_error`) shows (missing registry config is the
///     EXPECTED default-runner shape: `start()` throws `未配置注册中心…`
///     unless AK_REGISTRY_ADDR/AK_REGISTRY_ID were provided), BOTH
///     destinations must still switch; when it does not show, the engine
///     is live. Either branch passes; a visible card AND broken
///     navigation fails.
///
/// The journey does NOT depend on any network or a registry: the
/// tolerance branch (startup-error card + navigable pages) is an
/// explicitly accepted pass path, mirroring `main.dart`'s wave-29 P1-01
/// degrade semantics.
///
/// Identity-dir hygiene: without a configured registry the failed
/// `start()` throws BEFORE any disk write, so the real
/// `%USERPROFILE%\.ak-sending` is never touched. When a registry IS
/// configured (env `AK_REGISTRY_ADDR`+`AK_REGISTRY_ID`, or the compiled
/// `--dart-define` defaults), the engine boots and writes identity under
/// `$AK_ID_DIR` — the runner MUST then point `AK_ID_DIR` at a throwaway
/// directory whose path contains the marker `ak-e2e-id`; the test wipes
/// that directory's contents first and refuses to run otherwise.
///
/// Runner contract (Git Bash on the Windows build host, working dir
/// flutter_app):
///
/// ```bash
/// export PATH="$USERPROFILE/flutter-sdk/bin:$PATH"
/// # only needed when a real registry should be contacted:
/// export AK_ID_DIR="$(cygpath -w "$(mktemp -d "${TMPDIR:-/tmp}/ak-e2e-id-XXXXXX")")"
/// export AK_REGISTRY_ADDR=<host:port>
/// export AK_REGISTRY_ID=<64-hex pinned registry fingerprint>
/// flutter test integration_test/app_e2e_test.dart -d windows
/// ```
///
/// `AK_FFI_DLL` is belt-and-braces (DLL probe tier 1); without it the
/// runner's CMake step bundles `../target/release/ak_ffi.dll` next to the
/// test exe (tier 2).
library;

import 'dart:async' show TimeoutException;
import 'dart:io' show Directory, Platform;

import 'package:ak_sending_app/main.dart' as app
    show AppServiceBootstrap, UnusableSimpleTransferService, bootstrapAppService;
import 'package:ak_sending_app/services/dll_path.dart'
    show resolveAkDllPath;
import 'package:ak_sending_app/ui/app.dart' show AkSendingApp;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

/// Path-safety marker required inside `$AK_ID_DIR` before the test wipes
/// it (see the header's hygiene note).
const String _idDirMarker = 'ak-e2e-id';

/// Compiled registry constants (the FfiSimpleTransferService resolution
/// tier below the environment; empty unless `--dart-define` passed them).
const String _compiledRegistryAddr = String.fromEnvironment('AK_REGISTRY_ADDR');
const String _compiledRegistryId = String.fromEnvironment('AK_REGISTRY_ID');

/// The real production bootstrap outcome (constructed in `main` exactly
/// like `app.main` does).
late final app.AppServiceBootstrap _bootstrap;

// -----------------------------------------------------------------------------
// Step recorder: every step records an outcome; failures never interrupt.
// -----------------------------------------------------------------------------

enum _StepStatus { passed, failed, skipped }

final class _StepOutcome {
  const _StepOutcome(this.name, this.status, this.required, this.detail);

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

Future<void> _step(
  String name, {
  String? skipReason,
  required Future<void> Function() body,
}) async {
  if (skipReason != null) {
    _outcomes.add(_StepOutcome(name, _StepStatus.skipped, false, skipReason));
    _log('STEP SKIP: $name — $skipReason');
    return;
  }
  try {
    await body();
    _outcomes.add(_StepOutcome(name, _StepStatus.passed, true, 'ok'));
    _log('STEP PASS: $name');
  } on Object catch (error, stack) {
    _outcomes.add(_StepOutcome(name, _StepStatus.failed, true, '$error'));
    _log('STEP FAIL: $name — $error');
    _log('stack (head): ${stack.toString().split('\n').take(6).join(' | ')}');
  }
}

/// Renders the evidence table and the final verdict over the REQUIRED
/// steps (all evidence is already collected; a failure here never hides
/// earlier steps).
void _summarize() {
  _log('════════════ WINDOWS REAL-APP SMOKE SUMMARY ════════════');
  for (var i = 0; i < _outcomes.length; i++) {
    final outcome = _outcomes[i];
    _log('${i + 1}/${_outcomes.length} ${outcome.label}'
        '${outcome.required ? '' : ' (optional)'} ${outcome.name}'
        ' — ${outcome.detail}');
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
    reason: 'required steps failed — see the [ak-e2e] summary above for '
        'per-step evidence',
  );
}

// -----------------------------------------------------------------------------
// Live-binding pump helpers (real clock, check-first, wedged-frame guards).
// -----------------------------------------------------------------------------

/// Offstage-tolerant key finder — the IndexedStack shell keeps both pages
/// mounted while only the selected one is painted.
Finder _key(String value) =>
    find.byKey(ValueKey<String>(value), skipOffstage: false);

/// A navigation destination label, scoped to whichever nav surface the
/// shell picked at this width (NavigationBar on phones, NavigationRail on
/// wide windows) so page headings with the same word cannot shadow it.
Finder _navDestination(String label) {
  final bar = find.byType(NavigationBar);
  final scope = bar.evaluate().isNotEmpty ? bar : find.byType(NavigationRail);
  return find.descendant(of: scope, matching: find.text(label));
}

/// Waits until [isSatisfied] with real-time pacing. CHECK FIRST, pump
/// only while unsatisfied, keep the frame rate LOW (~4 fps): plugin
/// round-trips and engine calls run on the real clock under the
/// integration-test binding. Every `pump` await carries its own
/// wall-clock guard so a WEDGED frame surfaces as a recorded
/// [TimeoutException] instead of hanging the journey in silence.
Future<void> _pumpUntil(
  WidgetTester tester, {
  required bool Function() isSatisfied,
  required Duration timeout,
  required String what,
}) async {
  final deadline = DateTime.now().add(timeout);
  while (DateTime.now().isBefore(deadline)) {
    if (isSatisfied()) {
      return;
    }
    await tester.pump(const Duration(milliseconds: 250)).timeout(
        const Duration(seconds: 60),
        onTimeout: () => throw TimeoutException(
            'pump wedged for 60s waiting for: $what'));
    await Future<void>.delayed(const Duration(milliseconds: 250));
  }
  _log('TIMEOUT waiting for: $what');
  _log('onstage texts now: ${_onstageTexts(tester).take(30).toList()}');
  fail('timed out after $timeout waiting for: $what');
}

/// `pumpWidget` with the same wedged-frame guard (slow first frame / a
/// system-side window event mid-frame must fail loudly, not hang).
Future<void> _pumpWidget(WidgetTester tester, Widget app_) =>
    tester.pumpWidget(app_).timeout(
      const Duration(seconds: 120),
      onTimeout: () => throw TimeoutException(
          'pumpWidget wedged for 120s (first frame never settled)'),
    );

/// Switches to a destination by its label, verified by an ONSTAGE marker
/// of the target page, retrying the tap — a lost click must fail loudly.
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
  print('[ak-e2e] $message');
}

// -----------------------------------------------------------------------------
// Registry / identity-dir hygiene (header note).
// -----------------------------------------------------------------------------

/// Mirrors `FfiSimpleTransferService._envOrCompiled` per-key resolution:
/// environment first, then the compiled `--dart-define` default.
bool _resolvableFromEnvOrCompiled(String envValue, String compiled) {
  if (envValue.trim().isNotEmpty) return true;
  return compiled.trim().isNotEmpty;
}

/// True when `start()` will reach the engine (address AND pin resolvable
/// across the env/compiled tiers) — i.e. when identity files WILL be
/// written and the AK_ID_DIR hygiene guard must hold.
bool _registryConfigured(Map<String, String> env) =>
    _resolvableFromEnvOrCompiled(
        env['AK_REGISTRY_ADDR'] ?? '', _compiledRegistryAddr) &&
    _resolvableFromEnvOrCompiled(
        env['AK_REGISTRY_ID'] ?? '', _compiledRegistryId);

/// Wipes the contents of the throwaway identity dir (pure test
/// artifacts). Only ever called on a path containing [_idDirMarker].
void _wipeContents(Directory dir) {
  for (final entity in dir.listSync()) {
    entity.deleteSync(recursive: true);
  }
}

// -----------------------------------------------------------------------------
// The journey.
// -----------------------------------------------------------------------------

void main() {
  // Real production bootstrap, constructed BEFORE any widget exactly like
  // main() does: DLL probe → facade → borrowed engine → task-plane
  // export probe → FfiSimpleTransferService. Never throws — a missing
  // DLL or old exports degrade to `service: null` + readable error.
  _bootstrap = app.bootstrapAppService();

  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  tearDownAll(() {
    // Releases the engine too (the adapter's engineOwner hook — the
    // P1-05 borrowers-first order). UnusableSimpleTransferService is a
    // no-op dispose.
    _bootstrap.service?.dispose();
    _log('tearDownAll: service disposed');
  });

  testWidgets(
    'windows real app smoke: bootstrap, home two actions + empty '
    'connections, settings save folder, startup-error card navigable',
    (tester) async {
      final stopwatch = Stopwatch()..start();
      final env = Platform.environment;
      _log(
        'journey start: os=${Platform.operatingSystem} '
        '${Platform.operatingSystemVersion} '
        'registryConfigured=${_registryConfigured(env)}',
      );

      // Identity-dir hygiene guard (header note): the engine writes
      // identity only when a registry is configured; in that mode the
      // throwaway AK_ID_DIR (with marker) is REQUIRED and wiped first.
      final idDirOverride = env['AK_ID_DIR'];
      if (_registryConfigured(env)) {
        if (idDirOverride == null ||
            !idDirOverride.contains(_idDirMarker)) {
          fail('a registry is configured (env or --dart-define), so the '
              'engine WILL write identity files: AK_ID_DIR must point at a '
              'throwaway directory whose path contains the marker '
              '"$_idDirMarker" — refusing to touch "$idDirOverride"');
        }
        final dir = Directory(idDirOverride);
        if (dir.existsSync()) {
          _wipeContents(dir);
          _log('wiped throwaway identity dir: $idDirOverride');
        }
      } else if (idDirOverride != null && idDirOverride.contains(_idDirMarker)) {
        final dir = Directory(idDirOverride);
        if (dir.existsSync()) {
          _wipeContents(dir);
          _log('wiped throwaway identity dir (no registry configured): '
              '$idDirOverride');
        }
      } else {
        _log('no registry configured: the failed start() throws BEFORE any '
            'disk write — the real user-profile identity dir is never '
            'touched');
      }

      // dll_path tier evidence (tier 1 AK_FFI_DLL / tier 2 beside the exe
      // / tier 3 CWD-relative dev build).
      _log('dll probe resolution: ${resolveAkDllPath()}');

      // The REAL app, launched inside the test body exactly like main():
      // the constructed service, or the inert Unusable service + the
      // visible startupError card on the construction-failure branch.
      final service = _bootstrap.service ??
          app.UnusableSimpleTransferService(
              _bootstrap.error ?? '未知错误');
      final startupError = _bootstrap.error;
      await _pumpWidget(
          tester, AkSendingApp(service: service, startupError: startupError));

      // -----------------------------------------------------------------
      // Step 1 — real bootstrap evidence (which branch main() would take).
      // -----------------------------------------------------------------
      await _step(
        'bootstrap_real_stack',
        body: () async {
          _log('bootstrap: service=${_bootstrap.service != null} '
              'error=${_bootstrap.error ?? '<none>'}');
          expect(
            _bootstrap.service != null || _bootstrap.error != null,
            isTrue,
            reason: 'bootstrapAppService always yields one honest branch',
          );
        },
      );

      // -----------------------------------------------------------------
      // Step 2 — home page: the two pairing actions + the connections
      // empty state. The engine settle (startup-error card OR a started
      // service with a seeded saveDir) is awaited FIRST so the empty
      // state is read against the settled stream.
      // -----------------------------------------------------------------
      await _step(
        'home_two_actions_and_empty_connections',
        body: () async {
          await _pumpUntil(
            tester,
            what: 'home page first frame (home_page key)',
            timeout: const Duration(seconds: 60),
            isSatisfied: () => _key('home_page').evaluate().isNotEmpty,
          );
          await _pumpUntil(
            tester,
            what: 'engine settle (startup-error card or started service)',
            timeout: const Duration(seconds: 90),
            isSatisfied: () =>
                _key('app_startup_error').evaluate().isNotEmpty ||
                service.saveDir.isNotEmpty,
          );
          final cardShown = _key('app_startup_error').evaluate().isNotEmpty;
          _log('engine settle: startupErrorCard=$cardShown '
              'saveDir="${service.saveDir}"');
          expect(
            _key('home_create_code_button').evaluate(),
            isNotEmpty,
            reason: 'the 生成配对码 action renders (frozen key)',
          );
          expect(
            _key('home_join_code_button').evaluate(),
            isNotEmpty,
            reason: 'the 输入配对码 action renders (frozen key)',
          );
          expect(
            _key('connections_header').evaluate(),
            isNotEmpty,
            reason: 'the 「当前连接」 section header renders',
          );
          expect(
            _key('connections_empty').evaluate(),
            isNotEmpty,
            reason: 'with no pairing ever run the empty state renders',
          );
          final detail = _textData(tester, _key('connections_empty'));
          _log('connections empty state: $detail');
        },
      );

      // -----------------------------------------------------------------
      // Step 3 — settings page: reachable, save-folder card renders, and
      // its value equals the live service.saveDir when the engine
      // started (the engine's <id_dir>/downloads default or the persisted
      // choice — a previous run may have persisted one, so equality with
      // the live service is the honest assertion, not a fixed literal).
      // -----------------------------------------------------------------
      await _step(
        'settings_save_folder',
        body: () async {
          await _goTab(tester, label: '设置', onstageMarker: '默认保存文件夹');
          expect(
            _key('settings_page').evaluate(),
            isNotEmpty,
            reason: 'the settings page root renders (frozen key)',
          );
          expect(
            _key('settings_change_dir_button').evaluate(),
            isNotEmpty,
            reason: 'the 更改文件夹 action renders (frozen key)',
          );
          final shown = _textData(tester, _key('settings_save_dir')) ?? '';
          final live = service.saveDir;
          _log('settings save dir shown="$shown" live="$live"');
          if (live.isNotEmpty) {
            expect(
              shown,
              equals(live),
              reason: 'the save-folder card must show the live service '
                  'saveDir',
            );
            expect(
              shown,
              isNotEmpty,
              reason: 'a started engine always seeds a non-empty default '
                  'save folder',
            );
          } else {
            _log('engine not started (tolerance branch) — save-dir card '
                'shows the degraded empty value');
          }
        },
      );

      // -----------------------------------------------------------------
      // Step 4 — startup tolerance: whichever settle branch happened, the
      // shell must stay navigable. When the card shows (the expected
      // default-runner shape: 未配置注册中心), switching back to 「连接」
      // must still work; when it does not, the engine is live and the
      // card is honestly absent.
      // -----------------------------------------------------------------
      await _step(
        'startup_error_card_navigable',
        body: () async {
          final cardShown = _key('app_startup_error').evaluate().isNotEmpty;
          if (cardShown) {
            expect(
              _key('app_startup_error_detail').evaluate(),
              isNotEmpty,
              reason: 'the card carries the readable reason detail',
            );
            _log('startup error card reason: '
                '${_textData(tester, _key('app_startup_error_detail'))}');
            await _goTab(tester, label: '连接', onstageMarker: '当前连接');
            expect(
              _key('home_create_code_button').evaluate(),
              isNotEmpty,
              reason: 'navigation keeps working in the degraded state',
            );
            _log('tolerance branch verified: card shown + both tabs '
                'switchable');
          } else {
            expect(
              service,
              isNot(isA<app.UnusableSimpleTransferService>()),
              reason: 'no card means the real service constructed AND '
                  'started cleanly',
            );
            _log('engine live: no startup error card (registry configured '
                'and start() succeeded)');
          }
        },
      );

      // All evidence collected — render the table, then the verdict.
      _log('journey steps done in ${stopwatch.elapsed}');
      _summarize();
    },
    timeout: const Timeout(Duration(minutes: 10)),
    semanticsEnabled: false,
  );
}
