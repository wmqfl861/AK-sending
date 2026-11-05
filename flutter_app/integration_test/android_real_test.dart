/// Android real-service integration test (wave 25): runs ON an Android
/// device/emulator against the REAL app stack — real `MainActivity` host
/// with the SAF channel `dev.ak/storage` (wave-15 Kotlin), real bundled
/// `libak_ffi.so` (tier-0 `System.loadLibrary` semantics, see
/// lib/services/dll_path.dart), real `FfiTransferService`, real pages of
/// `lib/ui/app.dart`.
///
/// Every probe step is wrapped in try/catch (see [_step]): a failure is
/// RECORDED (log + summary) but never interrupts the later independent
/// steps; the pass/fail verdict is rendered once at the end over the
/// required steps, after all evidence has been collected.
///
/// Steps:
///  a. app launch + HomePage identity card render (key-located);
///  b. `libak_ffi.so` load — `FfiTransferService.version()` is a non-error
///     string AND the identity card's pairing code fills in through the
///     engine (`identityInfo` round-trip over the bundled .so);
///  c. invite page (邀请码) navigation reachable via the NavigationRail;
///  d. OPTIONAL real-registry invite create (真 TLS, server-deployed
///     registry): only when `AK_REGISTRY_ADDR` is passed as a Dart compile
///     constant (see the runner contract below);
///  e. SAF storage channel probe — `MethodChannel('dev.ak/storage')`
///     `getStorageInfo` → appDownloadDir / freeBytes / safPersisted.
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
///     THE DEVICE (as of wave 25: `122.51.121.24:7200`) plus its pinned
///     identity, both passed as Dart compile-time constants:
///
///     flutter test integration_test/android_real_test.dart -d <device> \
///       --dart-define=AK_REGISTRY_ADDR=122.51.121.24:7200 \
///       --dart-define=AK_REGISTRY_ID=<64-hex registry fingerprint>
///
///     Without the defines step (d) records SKIP (not a failure). With
///     `AK_REGISTRY_ADDR` but no `AK_REGISTRY_ID` it also SKIPs: the pin
///     is REQUIRED by the protocol (pairing-flow.md §3 — unknown pins are
///     never auto-accepted); print the fingerprint on the server host with
///     `ak-cli registry-server`.
///
///     KNOWN CONTRACT MISMATCH (wave-25 finding, recorded honestly by the
///     step): the ENGINE requires `registry_id` as exactly 64 hex chars
///     (crates/ak-ffi/src/pairing.rs `hex32` → `[u8; 32]`; the CLI pin
///     file agrees: `AKREG1 <addr> <64hex>`), while the wave-22 DART spec
///     validator accepts only exactly 32 hex
///     (`AkPairingStartSpec.toJson`, packages/ak_ffi_dart). The two
///     acceptance ranges are disjoint, so a real 64-hex pin is rejected
///     Dart-side (`AkSpecException`) before the engine is reached, and the
///     wave-22 32-hex placeholder (`PairingStartConfig.defaults`) is
///     rejected engine-side ("must be exactly 64 hex characters"). Until
///     the Dart validator is aligned, step (d) against a real registry
///     FAILS at that boundary and the recorded error IS the evidence.
///  5. Identity isolation: on Android there is no `$AK_ID_DIR` — the app
///     sandbox dir resolved by path_provider
///     (`/data/user/0/dev.ak.ak_sending_app/files/.ak-sending`) is used
///     and PERSISTS across runs; peers/invite rows left by earlier runs
///     may show up and are recorded, never asserted absent.
///  6. The invite page starts its pairing runtime lazily from whatever
///     registry config the `FfiTransferService.inviteStartConfig` seam
///     holds at first access — this test sets the seam BEFORE the first
///     widget pump so the configured registry (not the loopback default)
///     drives every runtime start.
/// ═══════════════════════════════════════════════════════════════════════
library;

import 'dart:io' show Platform;

import 'package:ak_ffi_dart/ak_ffi_dart.dart' show normalizeInviteCode;
import 'package:ak_sending_app/services/dll_path.dart'
    show resolveAkDllPath;
import 'package:ak_sending_app/services/ffi_transfer_service.dart'
    show FfiTransferService;
import 'package:ak_sending_app/services/invite_service.dart'
    show InviteTtlKind, PairingStartConfig;
import 'package:ak_sending_app/ui/app.dart' show AkSendingApp;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart' show MethodChannel;
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

/// Registry endpoint passed at compile time (`--dart-define=`); empty when
/// the real-server step should be skipped.
const String _registryAddr = String.fromEnvironment('AK_REGISTRY_ADDR');

/// Pinned registry identity (64 hex; see the header's mismatch note),
/// passed at compile time alongside [_registryAddr].
const String _registryIdHex = String.fromEnvironment('AK_REGISTRY_ID');

/// The SAF storage channel of the real MainActivity host (wave-15 Kotlin).
const MethodChannel _storageChannel = MethodChannel('dev.ak/storage');

/// Device name this test advertises to the registry (step d evidence).
const String _deviceName = 'ak-android-itest';

late final FfiTransferService _transfer;

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
/// running [body]; a thrown error/expectation inside [body] is recorded as
/// a FAIL (with the reason) and NEVER propagates — the journey continues
/// so later independent steps still yield their evidence.
Future<void> _step(
  String name, {
  required bool alsoRequired,
  String? skipReason,
  required Future<void> Function() body,
}) async {
  if (skipReason != null) {
    _outcomes.add(_StepOutcome(name, _StepStatus.skipped, alsoRequired,
        skipReason));
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
    for (final outcome in _outcomes) if (outcome.required && outcome.status == _StepStatus.failed) outcome.name,
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
// Live-binding pump / read helpers (app_e2e_test.dart conventions).
// -----------------------------------------------------------------------------

/// Offstage-tolerant key finder — the IndexedStack shell keeps all pages
/// mounted while only the selected one is painted.
Finder _key(String value) =>
    find.byKey(ValueKey<String>(value), skipOffstage: false);

/// A rail destination label, scoped to the NavigationRail so page headings
/// with the same word cannot shadow it.
Finder _railLabel(String text) =>
    find.descendant(of: find.byType(NavigationRail), matching: find.text(text));

/// Pumps frames with real-time pacing until [isSatisfied] (Android plugin
/// round-trips and engine calls run on the real clock under the
/// integration-test binding); dumps a text sample on timeout.
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
  _log('texts now: ${_allTexts(tester).take(30).toList()}');
  fail('timed out after $timeout waiting for: $what');
}

/// Switches to a rail tab by its label, verified by an ONSTAGE marker of
/// the target page, retrying the tap — a lost tap must fail loudly.
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
    _log('goTab($label) attempt $attempt: target page not onstage yet');
  }
  fail('could not switch to tab "$label" via the NavigationRail');
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

void _log(String message) {
  // ignore: avoid_print
  print('[ak-android-real] $message');
}

// -----------------------------------------------------------------------------
// The journey.
// -----------------------------------------------------------------------------

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  // Constructed BEFORE any widget: the constructor kicks the Android
  // identity-dir resolve (path_provider) so awaiting identityDirReady
  // later guarantees the real sandbox dir is anchored before the invite
  // runtime ever starts (header note 6). Construction never throws: a
  // missing .so degrades to status=error surfaced via version().
  _transfer = FfiTransferService();

  // Wire the registry config BEFORE the first inviteService access (the
  // InvitePage resolves the service in initState — IndexedStack builds all
  // pages on the first pump — and the runtime freezes the config it was
  // constructed with).
  if (_registryAddr.isNotEmpty && _registryIdHex.isNotEmpty) {
    _transfer.inviteStartConfig = PairingStartConfig(
      registryAddress: _registryAddr,
      registryIdHex: _registryIdHex,
      deviceName: _deviceName,
    );
    _log('registry config wired: $_registryAddr (pin len '
        '${_registryIdHex.length} hex)');
  }

  tearDownAll(() {
    _transfer.dispose(); // disposes the lazily-built invite service too
    _log('tearDownAll: services disposed');
  });

  testWidgets(
    'android real services: launch+identity card, libak_ffi.so load, '
    'invite nav, [real-registry invite], SAF storage channel',
    (tester) async {
      final stopwatch = Stopwatch()..start();
      _log(
        'journey start: os=${Platform.operatingSystem} '
        '${Platform.operatingSystemVersion} '
        'registry=${_registryAddr.isEmpty ? '<unset>' : _registryAddr}',
      );
      // dll_path tier-0 evidence: on Android the probe must return the
      // bare bundled SONAME (System.loadLibrary semantics).
      _log('dll probe resolution: ${resolveAkDllPath()}');

      // -----------------------------------------------------------------
      // Step a — app launch + identity card render (key-located).
      // -----------------------------------------------------------------
      await _step(
        'app_launch_identity_card',
        alsoRequired: true,
        body: () async {
          // Anchor the REAL identity dir before any page can lazily start
          // the pairing runtime against the provisional relative probe.
          await _transfer.identityDirReady;
          _log('identity dir anchored: ${_transfer.identityDir}');

          await tester.pumpWidget(AkSendingApp(service: _transfer));
          await _pumpUntil(
            tester,
            what: 'HomePage identity card (key home_pairing_code)',
            timeout: const Duration(seconds: 30),
            isSatisfied: () =>
                _key('home_pairing_code').evaluate().isNotEmpty,
          );
          final info = _transfer.deviceInfo();
          final fingerprint = info.fingerprintHex;
          _log(
            'identity card: device=${info.deviceName} '
            'fp=${fingerprint.length >= 16 ? fingerprint.substring(0, 16) : fingerprint}… '
            'peers=${info.peersCount} '
            'firstRunBanner=${_key('home_identity_created').evaluate().isNotEmpty} '
            'peersLine=${_textData(tester, _key('home_peers_count'))}',
          );
          expect(
            _key('home_pairing_code').evaluate().isNotEmpty,
            isTrue,
            reason: 'the identity card renders with the frozen key',
          );
        },
      );

      // -----------------------------------------------------------------
      // Step b — libak_ffi.so load: version() non-error + the pairing code
      // filled in by the real engine (identityInfo over the bundled .so).
      // -----------------------------------------------------------------
      await _step(
        'libak_ffi_so_load',
        alsoRequired: true,
        body: () async {
          final versionText = _transfer.version();
          _log('engine version(): $versionText');
          expect(versionText, isNotNull);
          expect(
            versionText.contains('不可用'),
            isFalse,
            reason: 'tier-0 load of libak_ffi.so failed — the version card '
                'reports: $versionText',
          );
          await _pumpUntil(
            tester,
            what: 'pairing code to fill in (engine identityInfo via the .so)',
            timeout: const Duration(seconds: 60),
            isSatisfied: () => RegExp(r'^\d{3}-\d{3}$').hasMatch(
                _selectableData(tester, _key('home_pairing_code')) ?? ''),
          );
          _log(
            'pairing code live: '
            '${_selectableData(tester, _key('home_pairing_code'))}',
          );
        },
      );

      // -----------------------------------------------------------------
      // Step c — invite page (邀请码) navigation reachable.
      // -----------------------------------------------------------------
      await _step(
        'invite_page_navigation',
        alsoRequired: true,
        body: () async {
          await _goTab(tester, label: '邀请码', onstageMarker: '配对邀请码');
          expect(
            _key('invite_nav').evaluate().isNotEmpty,
            isTrue,
            reason: 'the rail destination carries the invite_nav key',
          );
          final fullPageRoot = _key('invite_page').evaluate().isNotEmpty;
          final unsupportedCard =
              find.textContaining('邀请码配对不可用').evaluate().isNotEmpty;
          _log(
            'invite page onstage: fullRoot=$fullPageRoot '
            'unsupportedCard=$unsupportedCard '
            'listError=${_textData(tester, _key('invite_list_error'))}',
          );
          expect(
            fullPageRoot || unsupportedCard,
            isTrue,
            reason: 'the invite page renders either its full root '
                '(invite_page, runtime supported) or the honest unsupported '
                'card — navigation itself is the required evidence',
          );
        },
      );

      // -----------------------------------------------------------------
      // Step d — OPTIONAL real-registry invite create (真 TLS). Skipped
      // unless AK_REGISTRY_ADDR (+ AK_REGISTRY_ID) were compiled in; a
      // failure here is only a verdict failure when the operator
      // configured the real server.
      // -----------------------------------------------------------------
      final registryConfigured =
          _registryAddr.isNotEmpty && _registryIdHex.isNotEmpty;
      await _step(
        'real_registry_invite_create',
        alsoRequired: registryConfigured,
        skipReason: _registryAddr.isEmpty
            ? 'AK_REGISTRY_ADDR not compiled in (--dart-define) — '
                'real-server step intentionally not attempted'
            : _registryIdHex.isEmpty
                ? 'AK_REGISTRY_ADDR set but AK_REGISTRY_ID (64-hex pinned '
                    'registry fingerprint) missing — the pin is required '
                    '(pairing-flow.md §3); print it with '
                    '`ak-cli registry-server` on the server host'
                : null,
        body: () async {
          await _transfer.identityDirReady;
          final invite = _transfer.inviteService;
          expect(
            invite,
            isNotNull,
            reason: 'the invite runtime must be constructible (engine '
                'loaded, ak_pairing_*/ak_sched_* exports present)',
          );
          // Short-lived (1 h) so an interrupted run leaves no long-lived
          // credential behind on the registry.
          final created =
              await invite!.createInvite(ttl: InviteTtlKind.shortLived);
          final code = created.invitationCode;
          _log(
            'invite created over real TLS: codeId=${created.summary.codeId} '
            'state=${created.summary.state.name} '
            'expiresMs=${created.summary.expiresMs}',
          );
          expect(
            code.startsWith('AKI1-'),
            isTrue,
            reason: 'the registry-issued code must be AKI1 Crockford form; '
                'got "$code"',
          );
          expect(
            normalizeInviteCode(code),
            code,
            reason: 'the engine-issued code is already canonical',
          );
          // Best-effort server-side cleanup; the 1 h TTL bounds the
          // residue anyway. Recorded, never fatal.
          try {
            await invite.revokeInvite(created.summary.codeId);
            _log('revoked ${created.summary.codeId} (server-side cleanup)');
          } on Object catch (error) {
            _log('cleanup revoke failed (TTL 1 h bounds the residue): $error');
          }
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
    timeout: const Timeout(Duration(minutes: 10)),
    semanticsEnabled: false,
  );
}
