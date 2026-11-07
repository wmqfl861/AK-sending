/// C08 (single-resolution, probe-before-create loading) and E4 (the
/// A–G staged diagnostic host) + E2 (the `%TEMP%\ak-stage.log` phase
/// timing) coverage from the 2026-11-07 crash-analysis fix round.
///
/// The staged widget tests inject a fake [AkStagedEngineOwner] (depth
/// interception at open / probe / engine_new) and a recording
/// [AkTaskCalls] wrapper around the package fake (depth interception at
/// task_start / pump), so every stage is verifiable WITHOUT the DLL.
/// One conditional group exercises the real facade plumbing against the
/// real DLL when `target/release/ak_ffi.dll` exists (the same skip
/// pattern as services_test.dart / app_shell_test.dart).
library;

import 'dart:ffi' show DynamicLibrary, Pointer, nullptr;

import 'dart:io' show Directory, File, Platform;

import 'dart:typed_data' show Uint8List;

import 'package:flutter/material.dart';

import 'package:flutter_test/flutter_test.dart';

import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show AkEngine, AkFfiException, AkTaskCalls, FakeAkTaskCalls;

import 'package:ak_sending_app/main.dart'
    show
        AppServiceBootstrap,
        DiagStage,
        DiagTaskCallsGate,
        UnusableSimpleTransferService,
        bootstrapAppService,
        buildAppRoot,
        diagStageBanner,
        diagStageFromEnvironment;
import 'package:ak_sending_app/services/ak_engine_facade.dart'
    show AkEngineDartFacade, AkStageLog, AkStagedEngineOwner;
import 'package:ak_sending_app/services/dll_path.dart'
    show DllResolution, DllSource, absoluteAkDllLoadPath, resolveAkDllPath;
import 'package:ak_sending_app/services/ffi_simple_transfer_service.dart';

// ---------------------------------------------------------------------------
// Fakes.
// ---------------------------------------------------------------------------

/// A staged-owner fake mirroring the real facade's protocol (probe opens
/// first, the engine reuses the open) while recording every step — the
/// depth-integration point for stages B/C/D.
final class FakeStagedOwner implements AkStagedEngineOwner {
  /// Protocol call order: 'open' | 'probe' | 'engineNew' | 'handle' |
  /// 'dispose'. Mirrors the real facade's idempotency: `open` records
  /// once; later stages reuse the handle without a new entry.
  final List<String> ops = <String>[];

  /// What [probeTaskExports] reports as missing (empty = supported).
  List<String> missing = const <String>[];

  DynamicLibrary? _opened;

  /// The C07 lease probe witness: flips false in dispose, exactly like
  /// the real facade (before the native engine would be freed).
  bool disposed = false;

  @override
  bool get isEngineAlive => !disposed;

  @override
  DynamicLibrary openLibrary() {
    final existing = _opened;
    if (existing != null) {
      return existing;
    }
    ops.add('open');
    return _opened = DynamicLibrary.process();
  }

  @override
  List<String> probeTaskExports() {
    openLibrary(); // the real facade probes on the OPENED library
    ops.add('probe');
    return missing;
  }

  @override
  void engineNew() {
    openLibrary(); // idempotent reuse of the same handle in the real one
    ops.add('engineNew');
  }

  @override
  Pointer<AkEngine> get engineHandle {
    ops.add('handle');
    return nullptr;
  }

  @override
  DynamicLibrary get library => DynamicLibrary.process();

  @override
  void dispose() {
    ops.add('dispose');
    disposed = true; // C07: the lease flips BEFORE the engine would free
  }
}

/// Records every task-plane method that actually reached the (fake)
/// native seam — the depth-integration point for stages E/F/G.
final class RecordingTaskCalls implements AkTaskCalls {
  RecordingTaskCalls(this._inner);

  final AkTaskCalls _inner;

  /// Method names in native call order.
  final List<String> calls = <String>[];

  @override
  int start(String specJson, Uint8List runtimeIdOut) {
    calls.add('task_start');
    return _inner.start(specJson, runtimeIdOut);
  }

  @override
  int createCode(Uint8List runtimeId, String specJson, Uint8List opIdOut) {
    calls.add('task_create_code');
    return _inner.createCode(runtimeId, specJson, opIdOut);
  }

  @override
  int join(Uint8List runtimeId, String specJson, Uint8List opIdOut) {
    calls.add('task_join');
    return _inner.join(runtimeId, specJson, opIdOut);
  }

  @override
  int send(Uint8List runtimeId, Uint8List connId, String pathsJson,
      Uint8List opIdOut) {
    calls.add('task_send');
    return _inner.send(runtimeId, connId, pathsJson, opIdOut);
  }

  @override
  int list(Uint8List runtimeId, Uint8List buffer, int cap) {
    calls.add('task_list');
    return _inner.list(runtimeId, buffer, cap);
  }

  @override
  int disconnect(Uint8List runtimeId, Uint8List connId) {
    calls.add('task_disconnect');
    return _inner.disconnect(runtimeId, connId);
  }

  @override
  int setSaveDir(Uint8List runtimeId, String path) {
    calls.add('task_set_save_dir');
    return _inner.setSaveDir(runtimeId, path);
  }

  @override
  int consent(Uint8List runtimeId, Uint8List jobId, int accept) {
    calls.add('task_consent');
    return _inner.consent(runtimeId, jobId, accept);
  }

  @override
  int opResult(Uint8List runtimeId, Uint8List opId, Uint8List buffer, int cap) {
    calls.add('pairing_op_result');
    return _inner.opResult(runtimeId, opId, buffer, cap);
  }

  @override
  int pollEvent(Uint8List runtimeId, Uint8List buffer, int cap) {
    calls.add('poll_event');
    return _inner.pollEvent(runtimeId, buffer, cap);
  }

  @override
  int pollProgress(Uint8List buffer, int cap) {
    calls.add('poll_progress');
    return _inner.pollProgress(buffer, cap);
  }

  @override
  String? lastError() => _inner.lastError();
}

/// A registry-configured environment for the E/F/G service starts (the
/// paths are strings only — the fake seam never touches disk; the
/// registry must be the engine's literal `ip:port` form).
const Map<String, String> kDiagEnv = <String, String>{
  'AK_REGISTRY_ADDR': '127.0.0.1:7200',
  'AK_REGISTRY_ID': '00112233445566778899aabbccddeeff'
      '00112233445566778899aabbccddeeff',
  'AK_ID_DIR': 'C:/ak-diag-id',
};

/// A fresh stage log in a per-test temp dir (never the real %TEMP%).
(String, AkStageLog) freshStageLog(String name) {
  final dir = Directory.systemTemp.createTempSync('ak-diag-$name-');
  addTearDown(() {
    if (dir.existsSync()) {
      dir.deleteSync(recursive: true);
    }
  });
  final path = '${dir.path}${Platform.pathSeparator}ak-stage.log';
  return (path, AkStageLog.file(path));
}

/// A resolve seam returning one fixed (absolute) DLL resolution while
/// counting its calls — the C08 "resolve ONCE" witness.
final class CountingResolve {
  int calls = 0;

  DllResolution? resolve() {
    calls++;
    return const DllResolution(
      path: 'C:/diag/ak_ffi.dll',
      source: DllSource.environment,
    );
  }
}

// ---------------------------------------------------------------------------
// AK_DIAG_STAGE parsing.
// ---------------------------------------------------------------------------

void main() {
  group('diagStageFromEnvironment (AK_DIAG_STAGE)', () {
    test('unset/empty = null (the production default: full path, no '
        'banner, no gate)', () {
      expect(diagStageFromEnvironment(const <String, String>{}), isNull);
      expect(diagStageFromEnvironment(const <String, String>{'AK_DIAG_STAGE': ''}),
          isNull);
      expect(diagStageFromEnvironment(const <String, String>{'AK_DIAG_STAGE': '  '}),
          isNull);
    });

    test('A–G letters parse case-insensitively; invalid degrades to G',
        () {
      expect(diagStageFromEnvironment(const {'AK_DIAG_STAGE': 'A'}),
          DiagStage.a);
      expect(diagStageFromEnvironment(const {'AK_DIAG_STAGE': 'b'}),
          DiagStage.b);
      expect(diagStageFromEnvironment(const {'AK_DIAG_STAGE': ' d '}),
          DiagStage.d);
      expect(diagStageFromEnvironment(const {'AK_DIAG_STAGE': 'g'}),
          DiagStage.g);
      expect(diagStageFromEnvironment(const {'AK_DIAG_STAGE': 'X'}),
          DiagStage.g);
    });

    test('every stage names its banner (the UI strip text)', () {
      expect(diagStageBanner(DiagStage.a), contains('诊断A'));
      expect(diagStageBanner(DiagStage.b), contains('诊断B：DLL已加载'));
      expect(diagStageBanner(DiagStage.c), contains('诊断C'));
      expect(diagStageBanner(DiagStage.d), contains('诊断D'));
      expect(diagStageBanner(DiagStage.e), contains('诊断E'));
      expect(diagStageBanner(DiagStage.f), contains('诊断F'));
      expect(diagStageBanner(DiagStage.g), contains('诊断G'));
    });
  });

  group('C08: single resolution + probe-before-create', () {
    test('the DLL is resolved ONCE; the owner sees open → probe → '
        'engineNew in that order', () {
      final resolveCounter = CountingResolve();
      final owner = FakeStagedOwner();
      final (_, log) = freshStageLog('c08-order');
      final result = bootstrapAppService(
        environment: () => kDiagEnv,
        resolveDll: resolveCounter.resolve,
        facade: owner,
        taskCalls: () => RecordingTaskCalls(FakeAkTaskCalls()),
        stageLog: log,
      );
      expect(resolveCounter.calls, 1, reason: 'C08: bootstrap must resolve once');
      expect(owner.ops, ['open', 'probe', 'engineNew'],
          reason: 'open once, probe the exports, only then engine_new');
      expect(result.error, isNull);
      expect(result.service, isA<FfiSimpleTransferService>());
      expect(result.diagStage, isNull, reason: 'no AK_DIAG_STAGE → G default');
      result.service?.dispose();
      expect(owner.ops.last, 'dispose', reason: 'the service release order');
    });

    test('a library missing task exports is rejected BEFORE ak_engine_new',
        () {
      final resolveCounter = CountingResolve();
      final owner = FakeStagedOwner()..missing = ['ak_task_start', 'ak_task_send'];
      final result = bootstrapAppService(
        resolveDll: resolveCounter.resolve,
        facade: owner,
        taskCalls: () => RecordingTaskCalls(FakeAkTaskCalls()),
      );
      expect(result.service, isNull);
      expect(result.error, contains('缺少任务平面导出'));
      expect(result.error, contains('ak_task_start'));
      expect(owner.ops, ['open', 'probe', 'dispose'],
          reason: 'C08: no engine may exist for an old DLL');
    });

    test('absoluteAkDllLoadPath freezes relative tiers and keeps the '
        'Android SONAME verbatim', () {
      final cwdRelative = absoluteAkDllLoadPath(const DllResolution(
        path: 'target/release/ak_ffi.dll',
        source: DllSource.workingDirectory,
      ));
      expect(File(cwdRelative).isAbsolute, isTrue,
          reason: 'a CWD change after resolve can no longer redirect it');
      expect(cwdRelative.endsWith('ak_ffi.dll'), isTrue);
      expect(
        absoluteAkDllLoadPath(const DllResolution(
          path: 'libak_ffi.so',
          source: DllSource.bundled,
        )),
        'libak_ffi.so',
        reason: 'System.loadLibrary semantics need the bare SONAME',
      );
    });
  });

  group('C07: owner lease probe (facade + staged-owner wiring)', () {
    test('AkEngineDartFacade.isEngineAlive flips false on dispose — before '
        'the engine would free', () {
      final (_, log) = freshStageLog('c07-facade');
      final facade = AkEngineDartFacade(stageLog: log);
      expect(facade.isEngineAlive, isTrue,
          reason: '释放前置位：dispose 前探针按存活报告');
      facade.dispose();
      expect(facade.isEngineAlive, isFalse,
          reason: 'dispose 先翻转 _disposed 再释放原生引擎——借用者先看到'
              '租约撤销（C07 lease）');
      facade.dispose(); // idempotent
      expect(facade.isEngineAlive, isFalse);
    });

    test('FakeStagedOwner (the injected-owner seam) flips the same way', () {
      final owner = FakeStagedOwner();
      expect(owner.isEngineAlive, isTrue);
      owner.dispose();
      expect(owner.isEngineAlive, isFalse,
          reason: 'bootstrap 传入的 owner 探针与真实 facade 同语义');
    });
  });

  group('E4 stages A–D: truncated at native-load depth (fake owner)', () {
    Future<void> pumpDiag(WidgetTester tester, AppServiceBootstrap result) async {
      await tester.pumpWidget(buildAppRoot(result));
      await tester.pump();
    }

    testWidgets('stage A: no resolution, no load — the Flutter/runner '
        'baseline', (tester) async {
      final resolveCounter = CountingResolve();
      final owner = FakeStagedOwner();
      final result = bootstrapAppService(
        diagStage: DiagStage.a,
        resolveDll: resolveCounter.resolve,
        facade: owner,
      );
      expect(resolveCounter.calls, 0, reason: 'stage A never resolves');
      expect(owner.ops, isEmpty, reason: 'stage A never touches the owner');
      expect(result.service, isA<UnusableSimpleTransferService>());
      expect(result.error, contains('诊断A'));
      expect(result.diagStage, DiagStage.a);

      await pumpDiag(tester, result);
      expect(find.byKey(const ValueKey('diag_stage_A')), findsOneWidget);
      expect(find.byKey(const ValueKey('app_startup_error')), findsOneWidget);
      expect(find.byKey(const ValueKey('home_create_code_button')),
          findsOneWidget);
      expect(tester.takeException(), isNull);
    });

    testWidgets('stage B: DynamicLibrary.open only — no probe, no engine',
        (tester) async {
      final resolveCounter = CountingResolve();
      final owner = FakeStagedOwner();
      final result = bootstrapAppService(
        diagStage: DiagStage.b,
        resolveDll: resolveCounter.resolve,
        facade: owner,
      );
      expect(owner.ops, ['open']);
      expect(result.error, contains('诊断B：DLL已加载'));
      final service = result.service!;
      expect(service, isA<UnusableSimpleTransferService>());
      expect((service as UnusableSimpleTransferService).keepAlive, same(owner),
          reason: 'the loaded library stays alive for the observation');

      await pumpDiag(tester, result);
      expect(find.byKey(const ValueKey('diag_stage_B')), findsOneWidget);
      expect(tester.takeException(), isNull);
    });

    testWidgets('stage C: task-export lookup only — no engine created',
        (tester) async {
      final resolveCounter = CountingResolve();
      final owner = FakeStagedOwner();
      final result = bootstrapAppService(
        diagStage: DiagStage.c,
        resolveDll: resolveCounter.resolve,
        facade: owner,
      );
      expect(owner.ops, ['open', 'probe']);
      expect(result.error, contains('诊断C'));

      await pumpDiag(tester, result);
      expect(find.byKey(const ValueKey('diag_stage_C')), findsOneWidget);
      expect(tester.takeException(), isNull);
    });

    testWidgets('stage D: engine_new done, no task_start — and the engine '
        'is retained (no dispose)', (tester) async {
      final resolveCounter = CountingResolve();
      final owner = FakeStagedOwner();
      final result = bootstrapAppService(
        diagStage: DiagStage.d,
        resolveDll: resolveCounter.resolve,
        facade: owner,
      );
      expect(owner.ops, ['open', 'probe', 'engineNew']);
      expect(result.error, contains('诊断D'));
      expect(
        (result.service! as UnusableSimpleTransferService).keepAlive,
        same(owner),
        reason: 'E4/D: teardown must not mix into the observation window',
      );

      await pumpDiag(tester, result);
      expect(find.byKey(const ValueKey('diag_stage_D')), findsOneWidget);
      expect(tester.takeException(), isNull);
    });
  });

  group('E4 stages E/F/G: truncated at call depth (recording seam)', () {
    (AppServiceBootstrap, RecordingTaskCalls, FakeStagedOwner) bootstrapAt(
      DiagStage? stage, {
      Map<String, String> Function()? environment,
    }) {
      final resolveCounter = CountingResolve();
      final owner = FakeStagedOwner();
      final (_, log) = freshStageLog('stage');
      final recording = RecordingTaskCalls(FakeAkTaskCalls());
      final result = bootstrapAppService(
        environment: environment ?? () => kDiagEnv,
        resolveDll: resolveCounter.resolve,
        facade: owner,
        taskCalls: () => recording,
        isAndroid: false,
        diagStage: stage,
        stageLog: log,
      );
      return (result, recording, owner);
    }

    testWidgets('stage E: task_start reaches native, NOTHING else — not '
        'even under timer pressure', (tester) async {
      final (result, calls, _) = bootstrapAt(DiagStage.e);
      expect(result.error, isNull);
      expect(result.service, isA<FfiSimpleTransferService>());
      expect(result.diagStage, DiagStage.e);
      expect(calls.calls, ['task_start'],
          reason: 'ak_task_start completed; every poll was gated off');

      await tester.pumpWidget(buildAppRoot(result));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 200));
      expect(calls.calls, ['task_start'],
          reason: 'the shell is idempotent and the pump is gated');
      expect(find.byKey(const ValueKey('diag_stage_E')), findsOneWidget);
      expect(find.byKey(const ValueKey('app_startup_error')), findsNothing,
          reason: 'the truncated start itself succeeded');
      expect(find.byKey(const ValueKey('home_create_code_button')),
          findsOneWidget);
      expect(tester.takeException(), isNull);
      result.service?.dispose(); // cancels the (inert) pump timer
    });

    testWidgets('stage F: exactly the one synchronous startup pump '
        '(runtime_ready + empty + progress + list), then sealed',
        (tester) async {
      final (result, calls, _) = bootstrapAt(DiagStage.f);
      expect(result.error, isNull);
      expect(
        calls.calls,
        ['task_start', 'poll_event', 'poll_event', 'poll_progress', 'task_list'],
        reason: 'the startup sequence of the report, frozen after one pump',
      );

      await tester.pumpWidget(buildAppRoot(result));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 200));
      expect(
        calls.calls,
        ['task_start', 'poll_event', 'poll_event', 'poll_progress', 'task_list'],
        reason: 'sealed: later timer ticks never reach native',
      );
      expect(find.byKey(const ValueKey('diag_stage_F')), findsOneWidget);
      expect(tester.takeException(), isNull);
      result.service?.dispose(); // cancels the (sealed) pump timer
    });

    testWidgets('stage G (explicit env): the full path keeps polling — the '
        'same startup prefix, then it grows', (tester) async {
      final (result, calls, _) = bootstrapAt(
        null,
        environment: () => <String, String>{...kDiagEnv, 'AK_DIAG_STAGE': 'G'},
      );
      expect(result.diagStage, DiagStage.g,
          reason: 'the environment variable alone selects the stage');
      expect(result.service, isA<FfiSimpleTransferService>());

      await tester.pumpWidget(buildAppRoot(result));
      await tester.pump();
      expect(find.byKey(const ValueKey('diag_stage_G')), findsOneWidget);
      final prefix = calls.calls.length;
      expect(prefix, greaterThanOrEqualTo(5),
          reason: 'start + the startup pump ran');
      await tester.pump(const Duration(milliseconds: 200));
      expect(calls.calls.length, greaterThan(prefix),
          reason: 'G is the production behavior: the periodic poll lives');
      expect(tester.takeException(), isNull);
      result.service?.dispose(); // cancels the live pump timer
    });

    testWidgets('no AK_DIAG_STAGE: exactly the pre-diagnostic root (no '
        'banner), full behavior', (tester) async {
      final (result, calls, _) = bootstrapAt(null);
      expect(result.diagStage, isNull);

      await tester.pumpWidget(buildAppRoot(result));
      await tester.pump();
      expect(find.byKey(const ValueKey('diag_stage_G')), findsNothing);
      expect(find.byKey(const ValueKey('app_startup_error')), findsNothing);
      final prefix = calls.calls.length;
      expect(prefix, greaterThanOrEqualTo(5));
      await tester.pump(const Duration(milliseconds: 200));
      expect(calls.calls.length, greaterThan(prefix));
      expect(tester.takeException(), isNull);
      result.service?.dispose(); // cancels the live pump timer
    });

    test('stages E/F on Android keep the full path, honestly noted', () {
      final resolveCounter = CountingResolve();
      final owner = FakeStagedOwner();
      final recording = RecordingTaskCalls(FakeAkTaskCalls());
      final result = bootstrapAppService(
        environment: () => kDiagEnv,
        resolveDll: resolveCounter.resolve,
        facade: owner,
        taskCalls: () => recording,
        isAndroid: true,
        diagStage: DiagStage.e,
      );
      expect(result.diagStage, DiagStage.e);
      expect(result.diagNote, contains('Android'));
      expect(recording.calls, isEmpty,
          reason: 'the shell does the start later — nothing truncated here');
    });
  });

  group('DiagTaskCallsGate unit (E4 depth interceptor)', () {
    (DiagTaskCallsGate, RecordingTaskCalls) makeGate(DiagStage stage,
        {bool observeOnly = false}) {
      final recording = RecordingTaskCalls(FakeAkTaskCalls());
      final (_, log) = freshStageLog('gate');
      return (
        DiagTaskCallsGate(recording, stage: stage, log: log, observeOnly: observeOnly),
        recording,
      );
    }

    test('stage E refuses every non-start call from construction', () {
      final (gate, calls) = makeGate(DiagStage.e);
      final runtime = _startFakeRuntime(gate);
      final buffer = Uint8List(256);
      expect(() => gate.pollEvent(runtime, buffer, buffer.length),
          throwsA(isA<AkFfiException>()));
      expect(() => gate.pollProgress(buffer, buffer.length),
          throwsA(isA<AkFfiException>()));
      expect(calls.calls, ['task_start'],
          reason: 'task_start itself always passes');
    });

    test('stage F passes until seal(), then refuses (start still passes)',
        () {
      final (gate, _) = makeGate(DiagStage.f);
      final runtime = _startFakeRuntime(gate);
      final buffer = Uint8List(256);
      // Before the seal: the startup pump passes.
      expect(gate.pollEvent(runtime, buffer, buffer.length), anyOf(0, 1, -3));
      gate.seal();
      expect(() => gate.pollEvent(runtime, buffer, buffer.length),
          throwsA(isA<AkFfiException>()));
      expect(() => gate.list(runtime, buffer, buffer.length),
          throwsA(isA<AkFfiException>()));
      // A failed task_start honestly retries through the shell.
      final retried = Uint8List(16);
      expect(
        gate.start(
          '{"registry":"r","registry_id":"i","id_dir":"d"}',
          retried,
        ),
        anyOf(0, -2),
      );
    });

    test('observe mode never blocks, even sealed', () {
      final (gate, calls) = makeGate(DiagStage.g, observeOnly: true);
      gate.seal(); // no-op by contract
      final runtime = _startFakeRuntime(gate);
      final buffer = Uint8List(256);
      expect(gate.pollEvent(runtime, buffer, buffer.length), anyOf(0, 1, -3));
      expect(calls.calls, isNotEmpty);
    });
  });

  group('E2: %TEMP%\\ak-stage.log phase timing', () {
    test('a staged run logs its stage line and task_start timing '
        '(millisecond-prefixed)', () {
      final (path, log) = freshStageLog('e2');
      final resolveCounter = CountingResolve();
      final owner = FakeStagedOwner();
      final result = bootstrapAppService(
        environment: () => kDiagEnv,
        resolveDll: resolveCounter.resolve,
        facade: owner,
        taskCalls: () => RecordingTaskCalls(FakeAkTaskCalls()),
        isAndroid: false,
        diagStage: DiagStage.e,
        stageLog: log,
      );
      result.service?.dispose();
      final lines = File(path).readAsLinesSync();
      expect(lines, isNotEmpty);
      expect(lines.first, matches(RegExp(r'^\d+ diag_stage_E$')),
          reason: 'the stage line is written first, one line');
      expect(
        lines,
        containsAllInOrder(<Matcher>[
          matches(RegExp(r'^\d+ task_start_before$')),
          matches(RegExp(r'^\d+ task_start_after$')),
        ]),
        reason: 'the gate times the native phase boundaries (E2)',
      );
    });

    test('AkStageLog never throws and disables itself after a broken '
        'write', () {
      final broken = AkStageLog.file(
        '${Directory.systemTemp.path}${Platform.pathSeparator}'
        'ak-diag-no-such-dir-9f3\\ak-stage.log',
      );
      broken.write('should_not_throw');
      broken.write('still_not_throwing');
    });

    test('defaultAkStageLogPath: TEMP/TMP/TMPDIR tiers, separator-aware',
        () {
      final dir = <String>['C:', 'users', 'temp'].join(Platform.pathSeparator);
      expect(
        AkStageLog.defaultAkStageLogPath(<String, String>{'TEMP': dir}),
        <String>['C:', 'users', 'temp', 'ak-stage.log']
            .join(Platform.pathSeparator),
      );
      // A dir already ending in a separator gets no second one.
      final trailing = <String>[dir, ''].join(Platform.pathSeparator);
      expect(
        AkStageLog.defaultAkStageLogPath(<String, String>{'TMP': trailing}),
        '$trailing'
            'ak-stage.log',
      );
      expect(AkStageLog.defaultAkStageLogPath(const <String, String>{}),
          isNull);
    });
  });

  // -------------------------------------------------------------------------
  // Real-DLL (conditional): the C08 facade plumbing end to end.
  // -------------------------------------------------------------------------

  final dllProbe = resolveAkDllPath();
  test(
    'real facade (C08): opens the INJECTED absolute path once, probes '
    'before engine_new, and times every phase into the stage log',
    () {
      final resolution = dllProbe;
      final injected = absoluteAkDllLoadPath(resolution!);
      final (logPath, log) = freshStageLog('realdll');
      final facade = AkEngineDartFacade(dllPath: injected, stageLog: log);
      addTearDown(facade.dispose);

      // Stage C depth: probe on the opened library, still no engine.
      expect(facade.probeTaskExports(), isEmpty);

      // Stage D depth: the engine is created over the SAME library.
      final engine = facade.borrowEngine();
      expect(engine.bindings.source, injected,
          reason: 'the injected path was the actual load input (no '
              're-resolve)');
      expect(identical(engine.bindings.library, facade.library), isTrue,
          reason: 'open once: the engine reuses the facade library handle');

      final lines = File(logPath).readAsLinesSync();
      expect(
        lines,
        containsAllInOrder(<Matcher>[
          // The lazy open happens INSIDE the probe entry (open-once).
          matches(RegExp(r'^\d+ probe_task_exports_before$')),
          matches(RegExp(r'^\d+ dll_open_before$')),
          matches(RegExp(r'^\d+ dll_open_after$')),
          matches(RegExp(r'^\d+ probe_task_exports_after$')),
          matches(RegExp(r'^\d+ engine_new_before$')),
          matches(RegExp(r'^\d+ engine_new_after$')),
        ]),
        reason: 'E2: open/probe/engine_new phases are timed (ms prefix)',
      );
    },
    skip: dllProbe == null
        ? 'ak_ffi.dll 未找到（先 cargo build -p ak-ffi --release）'
        : false,
  );
}

/// Starts one fake runtime through [gate] (task_start passes in every
/// mode) and returns its runtime id for poll calls.
Uint8List _startFakeRuntime(DiagTaskCallsGate gate) {
  final runtimeId = Uint8List(16);
  final code = gate.start(
    '{"registry":"https://registry.example",'
    '"registry_id":"00112233445566778899aabbccddeeff'
    '00112233445566778899aabbccddeeff",'
    '"id_dir":"C:/ak-diag-id"}',
    runtimeId,
  );
  expect(code, 0, reason: 'task_start must pass the gate');
  return runtimeId;
}
