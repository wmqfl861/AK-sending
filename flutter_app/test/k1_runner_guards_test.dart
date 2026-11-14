/// Companion unit tests for the K1 Windows walkthrough runner's PURE
/// guard helpers (K 复核 K-R4/K-R5), imported from the runner itself so
/// the runner's guard and these tests can never drift apart:
///
/// * [K1ShotCoverage] — the per-state shot bookkeeping that replaced the
///   init-to-false/reset-after-verifying flag: a round that captures
///   neither, or only one, of the transferring/verifying states must be
///   reported INCOMPLETE, and the runner's own `expect(missing, isEmpty)`
///   must actually FAIL on it.
/// * [k1ResolveEffectiveEndpoints]/[k1LocalEndpointFindings] — the
///   effective-endpoint guard mirroring the invite plane's precedence
///   (runtime environment over compiled `--dart-define`), including the
///   review's counterexample: compiled LOCAL + runtime NON-LOCAL is
///   rejected, and so is an unset STUN (the app would fall back to its
///   production default — never a local value).
library;

import 'package:flutter_test/flutter_test.dart';

import '../integration_test/k1_windows_theme_shots_test.dart' as k1;

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  group('K1ShotCoverage (K 复核 K-R4)', () {
    test('capturing NEITHER state is incomplete — both are named missing',
        () {
      final coverage = k1.K1ShotCoverage();
      expect(coverage.missingStates(), ['transferring', 'verifying']);
      expect(coverage.isComplete, isFalse);
    });

    test('capturing only transferring is incomplete (verifying missing)', () {
      final coverage = k1.K1ShotCoverage()..recordTransferringShot();
      expect(coverage.missingStates(), ['verifying']);
      expect(coverage.isComplete, isFalse);
    });

    test('capturing only verifying is incomplete (transferring missing)', () {
      final coverage = k1.K1ShotCoverage()..recordVerifyingShot();
      expect(coverage.missingStates(), ['transferring']);
      expect(coverage.isComplete, isFalse);
    });

    test('capturing both completes; counts never reset', () {
      final coverage = k1.K1ShotCoverage()
        ..recordTransferringShot()
        ..recordTransferringShot()
        ..recordVerifyingShot();
      expect(coverage.missingStates(), isEmpty);
      expect(coverage.isComplete, isTrue);
      expect(coverage.transferringShots, 2);
      expect(coverage.verifyingShots, 1);
    });

    test("the runner's own assertion FAILS on an incomplete round", () {
      // The exact expression the runner asserts after its sampling loop:
      // it must throw when a state was never captured — this is what
      // closes the pre-fix loophole (a false-initialized flag that a
      // skipped pass satisfied).
      expect(
          () => expect(k1.K1ShotCoverage().missingStates(), isEmpty,
              reason: 'K 复核 K-R4: 未采到状态'),
          throwsA(isA<TestFailure>()));
    });
  });

  group('effective endpoints (K 复核 K-R5)', () {
    test('runtime environment values WIN over compiled defines '
        '(the invite-plane precedence)', () {
      final effective = k1.k1ResolveEffectiveEndpoints(
        environment: <String, String>{
          'AK_REGISTRY_ADDR': '127.0.0.1:17200',
          'AK_STUN_ADDR': '127.0.0.1:17101',
          'AK_REGISTRY_ID': 'a' * 64,
        },
        compiledRegistryAddr: '127.0.0.1:9999',
        compiledStunAddr: '127.0.0.1:9998',
        compiledRegistryId: 'b' * 64,
      );
      expect(effective.registryAddr, '127.0.0.1:17200');
      expect(effective.stunAddr, '127.0.0.1:17101');
      expect(effective.registryIdHex, 'a' * 64); // env tier, not 'b' * 64
    });

    test('compiled values are used when the runtime tier is unset or blank',
        () {
      final effective = k1.k1ResolveEffectiveEndpoints(
        environment: <String, String>{'AK_STUN_ADDR': '   '},
        compiledRegistryAddr: '127.0.0.1:17200',
        compiledStunAddr: '127.0.0.1:17101',
        compiledRegistryId: 'b' * 64,
      );
      expect(effective.registryAddr, '127.0.0.1:17200');
      expect(effective.stunAddr, '127.0.0.1:17101'); // blank env = unset
      expect(effective.registryIdHex, 'b' * 64);
    });

    test('compiled LOCAL + runtime NON-LOCAL is REJECTED '
        '(the K-R5 counterexample)', () {
      final effective = k1.k1ResolveEffectiveEndpoints(
        environment: <String, String>{
          'AK_STUN_ADDR': '203.0.113.9:7101', // documentation TEST-NET-3
        },
        compiledRegistryAddr: '127.0.0.1:17200',
        compiledStunAddr: '127.0.0.1:17101',
        compiledRegistryId: 'b' * 64,
      );
      final findings = k1.k1LocalEndpointFindings(effective);
      expect(findings, hasLength(1));
      expect(findings.single, contains('AK_STUN_ADDR'));
      expect(findings.single, contains('203.0.113.9:7101')); // effective, not compiled
    });

    test('an UNSET stun is REJECTED — the app would dial its '
        'production default', () {
      final effective = k1.k1ResolveEffectiveEndpoints(
        environment: <String, String>{},
        compiledRegistryAddr: '127.0.0.1:17200',
        compiledStunAddr: '',
        compiledRegistryId: 'b' * 64,
      );
      expect(effective.stunAddr, isNull);
      final findings = k1.k1LocalEndpointFindings(effective);
      expect(findings, hasLength(1));
      expect(findings.single, contains('AK_STUN_ADDR'));
    });

    test('a non-local runtime registry is REJECTED even with a local pin',
        () {
      final effective = k1.k1ResolveEffectiveEndpoints(
        environment: <String, String>{
          'AK_REGISTRY_ADDR': '203.0.113.9:17200',
          'AK_REGISTRY_ID': 'a' * 64,
        },
        compiledRegistryAddr: '127.0.0.1:17200',
        compiledStunAddr: '127.0.0.1:17101',
        compiledRegistryId: 'b' * 64,
      );
      final findings = k1.k1LocalEndpointFindings(effective);
      expect(findings, hasLength(1)); // only the registry; the stun is fine
      expect(findings.single, contains('AK_REGISTRY_ADDR'));
      expect(findings.single, contains('203.0.113.9:17200')); // effective tier
    });

    test('a pin that is not 64 hex chars is a finding on its own', () {
      final effective = k1.k1ResolveEffectiveEndpoints(
        environment: <String, String>{
          'AK_REGISTRY_ADDR': '127.0.0.1:17200',
          'AK_STUN_ADDR': '127.0.0.1:17101',
          'AK_REGISTRY_ID': 'a' * 63, // one short
        },
        compiledRegistryAddr: '127.0.0.1:17200',
        compiledStunAddr: '127.0.0.1:17101',
        compiledRegistryId: 'b' * 64,
      );
      final findings = k1.k1LocalEndpointFindings(effective);
      expect(findings, hasLength(1));
      expect(findings.single, contains('AK_REGISTRY_ID'));
    });

    test('a fully LOCAL effective configuration passes with no findings',
        () {
      final effective = k1.k1ResolveEffectiveEndpoints(
        environment: <String, String>{
          'AK_REGISTRY_ADDR': '127.0.0.1:17200',
          'AK_STUN_ADDR': '127.0.0.1:17101',
          'AK_REGISTRY_ID': 'a' * 64,
        },
        compiledRegistryAddr: '127.0.0.1:17200',
        compiledStunAddr: '127.0.0.1:17101',
        compiledRegistryId: 'b' * 64,
      );
      expect(k1.k1LocalEndpointFindings(effective), isEmpty);
    });
  });
}
