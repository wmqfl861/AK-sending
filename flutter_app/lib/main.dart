/// AK-sending app entry (wave-28 v1 极简配对 UI, wave-29 real wiring).
///
/// The REAL engine stack is wired here (P1-01 fix — the app no longer
/// boots the local demo fake):
///
/// ```text
/// main.dart ─▶ AkEngineDartFacade (owns AkEngineDart: DLL probe + handle)
///                     ▼ borrowEngine() (ffi-v2 §2: borrow, never dispose)
///             AkTaskFfiBindings (engine pointer + library, task-plane
///                     ▼          export probe via missingTaskExports;
///                 C07: carries the facade's isEngineAlive lease probe)
///             FfiSimpleTransferService (the UI contract adapter; resolves
///                     ▼          AK_REGISTRY_ADDR/AK_REGISTRY_ID env →
///                 compiled --dart-define defaults; Android identity dir
///                 via path_provider, gated by identityDirReady)
///             SimpleTransferService engine service (ak_task_* loops)
/// ```
///
/// C08 (crash-analysis 2026-11-07): the DLL path is resolved ONCE and
/// absolutized ([absoluteAkDllLoadPath]) before the facade opens exactly
/// that file; the task-plane exports are probed on the opened library
/// BEFORE `ak_engine_new` (probe-before-create — an old DLL is rejected
/// without ever building an engine).
///
/// E4 staged diagnosis: the `AK_DIAG_STAGE` environment variable (A–G,
/// default/full = G) truncates the native startup at a chosen depth with
/// the SAME real DLL, so one binary can bisect which stage a crash
/// belongs to:
///
/// | stage | reached depth |
/// |---|---|
/// | A | nothing native — Flutter/runner baseline |
/// | B | `DynamicLibrary.open` only, no export touched |
/// | C | + task-export lookup (`missingTaskExports`) |
/// | D | + `ak_engine_new` (engine held, no task_start) |
/// | E | + `ak_task_start`, then idle — no native polling |
/// | F | + the one synchronous startup pump, then sealed |
/// | G | full path (production behavior) |
///
/// Every staged run shows a banner with the stage name (test key
/// `diag_stage_X`) and writes the stage plus per-phase millisecond
/// timestamps to `%TEMP%\ak-stage.log` (E2 — the facade/gate log the
/// phases; see [AkStageLog]).
///
/// The facade's engine handle is RELEASED by the adapter's dispose (the
/// `engineOwner` hook — borrowers first, native owner last, the P1-05
/// order). A construction-time failure (missing DLL, old library without
/// the task-plane exports) never crashes the app: [bootstrapAppService]
/// degrades to a visible error card ([AkSendingApp.startupError]) plus
/// the inert [UnusableSimpleTransferService], and a failed `start()`
/// (e.g. missing registry config) is latched by the shell the same way.
///
/// `LocalSimpleTransferService` (simple_transfer_fake.dart) stays a
/// TEST-ONLY injection (test/w28_test_helpers.dart) — nothing in the
/// production path references it anymore.
library;

import 'dart:io' show Platform;

import 'dart:typed_data' show Uint8List;

import 'package:flutter/material.dart';

import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show AkFfiException, AkStatus, AkTaskCalls, AkTaskFfiBindings;

import 'services/ak_engine_facade.dart';
import 'services/dll_path.dart';
import 'services/ffi_simple_transfer_service.dart';
import 'ui/app.dart' show AkSendingApp;
import 'ui/service/simple_transfer_service.dart';

void main() {
  final bootstrap = bootstrapAppService();
  runApp(buildAppRoot(bootstrap));
}

// ---------------------------------------------------------------------------
// E4 diagnostic stages (AK_DIAG_STAGE).
// ---------------------------------------------------------------------------

/// The native startup depth a diagnostic run reaches (E4 table A–G).
enum DiagStage { a, b, c, d, e, f, g }

extension DiagStageLetter on DiagStage {
  /// The uppercase stage letter ('A'…'G').
  String get letter => name.toUpperCase();
}

/// Parses `AK_DIAG_STAGE` (case-insensitive, trimmed).
///
/// Returns null when the variable is unset/empty — the production
/// default: full path (G), no banner, no call gate. An INVALID value
/// degrades to [DiagStage.g] (a diagnostic run that stays on the full,
/// safe path rather than guessing a truncation).
DiagStage? diagStageFromEnvironment(Map<String, String> environment) {
  final raw = environment['AK_DIAG_STAGE'];
  if (raw == null) {
    return null;
  }
  final letter = raw.trim().toUpperCase();
  if (letter.isEmpty) {
    return null;
  }
  for (final stage in DiagStage.values) {
    if (stage.letter == letter) {
      return stage;
    }
  }
  return DiagStage.g;
}

/// The banner text of [stage] (also the Unusable-service reason for the
/// truncation stages A–D).
String diagStageBanner(DiagStage stage) => switch (stage) {
      DiagStage.a => '诊断A：未加载 DLL（Flutter/runner 基线）',
      DiagStage.b => '诊断B：DLL已加载（open 后空闲，未调用任何导出）',
      DiagStage.c => '诊断C：任务平面导出探测完成（未创建引擎）',
      DiagStage.d => '诊断D：ak_engine_new 完成（未 task_start）',
      DiagStage.e => '诊断E：ak_task_start 完成（空闲，无周期轮询）',
      DiagStage.f => '诊断F：单次同步 pump 完成（已停止）',
      DiagStage.g => '诊断G：完整路径（现行为）',
    };

/// The app root for [bootstrap]: the real shell, plus the diagnostic
/// stage banner above it when the run is a staged E4 diagnostic. An
/// unset stage renders EXACTLY the pre-diagnostic root (G production).
Widget buildAppRoot(AppServiceBootstrap bootstrap) {
  final app = AkSendingApp(
    service: bootstrap.service ??
        UnusableSimpleTransferService(bootstrap.error ?? '未知错误'),
    startupError: bootstrap.error,
  );
  final stage = bootstrap.diagStage;
  if (stage == null) {
    return app;
  }
  return Directionality(
    textDirection: TextDirection.ltr,
    child: Column(
      children: [
        _DiagStageBanner(stage: stage, note: bootstrap.diagNote),
        Expanded(child: app),
      ],
    ),
  );
}

/// The always-visible stage strip of an E4 diagnostic run (outside the
/// app's MaterialApp so it survives every navigation state).
class _DiagStageBanner extends StatelessWidget {
  const _DiagStageBanner({required this.stage, this.note});

  final DiagStage stage;

  /// Optional stage qualification (e.g. the Android no-truncation note).
  final String? note;

  @override
  Widget build(BuildContext context) {
    return Container(
      width: double.infinity,
      color: const Color(0xFF6B3E00),
      padding: const EdgeInsets.fromLTRB(12, 6, 12, 6),
      child: Text(
        note == null || note!.isEmpty
            ? diagStageBanner(stage)
            : '${diagStageBanner(stage)}$note',
        key: ValueKey('diag_stage_${stage.letter}'),
        softWrap: true,
        maxLines: 2,
        overflow: TextOverflow.ellipsis,
        style: const TextStyle(fontSize: 12, color: Color(0xFFFFFFFF)),
      ),
    );
  }
}

// ---------------------------------------------------------------------------
// E4/E2 call-depth gate at the AkTaskCalls seam.
// ---------------------------------------------------------------------------

/// Intercepts the task-plane calls at chosen depths (E4 stages E/F; the
/// observation mode serves explicit diagnostic-G runs). The gate wraps
/// the REAL [AkTaskCalls] (or a test fake); the engine service keeps its
/// ordinary 50 ms pump — the gate simply refuses (or logs) what the
/// stage must not do:
///
/// * stage E (`startOnly`): `ak_task_start` passes; EVERY other export
///   call throws [AkFfiException] from construction on. The engine
///   service's own pump/startup drain swallow exactly that exception
///   shape, so the shell stays alive while nothing native is polled.
/// * stage F (default mode): everything passes until [seal] — called
///   right after the synchronous part of `service.start()` returned,
///   which is right after the one startup pump. Later ticks/ops throw.
/// * observation mode: nothing is ever blocked; the first calls are
///   logged for timing (explicit `AK_DIAG_STAGE=G` runs).
///
/// `ak_engine_last_error` ([lastError]) always passes through: it is a
/// read-only reserved-handle query reached only after a REAL native
/// failure (e.g. a refused `ak_task_start` retry), and cutting it would
/// corrupt the honest failure message.
final class DiagTaskCallsGate implements AkTaskCalls {
  DiagTaskCallsGate(
    this._inner, {
    required this.stage,
    required this.log,
    this.observeOnly = false,
  });

  final AkTaskCalls _inner;

  /// The stage this gate enforces (E/F truncation, G observation).
  final DiagStage stage;

  /// Where allowed calls are timed (E2, same `%TEMP%\ak-stage.log`).
  final AkStageLog log;

  /// Pure observation (explicit diagnostic G): never blocks.
  final bool observeOnly;

  /// Set by [seal]: everything except `ak_task_start` is refused.
  bool _sealed = false;

  /// How many calls still get a timing line (startup-sized budget — a
  /// periodic G pump must not grow the log without bound).
  int _logBudget = 24;

  /// Stage E refuses every non-start call from construction.
  bool get _startOnly => stage == DiagStage.e && !observeOnly;

  bool _refused(String phase) =>
      !observeOnly && phase != 'task_start' && (_startOnly || _sealed);

  /// Stage F: freeze the depth — called after the synchronous startup
  /// pump inside `service.start()` completed.
  void seal() => _sealed = true;

  void _timing(String phase) {
    if (_logBudget > 0) {
      _logBudget--;
      log.write(phase);
    }
  }

  int _invoke(String phase, int Function() call) {
    if (_refused(phase)) {
      throw _truncated(phase);
    }
    return call();
  }

  // `ak_task_start` is never refused (an E/F run whose task_start failed
  // honestly retries through the shell, exactly like G).
  @override
  int start(String specJson, Uint8List runtimeIdOut) {
    _timing('task_start_before');
    final code = _inner.start(specJson, runtimeIdOut);
    _timing('task_start_after');
    return code;
  }

  @override
  int createCode(Uint8List runtimeId, String specJson, Uint8List opIdOut) =>
      _invoke('task_create_code', () => _inner.createCode(runtimeId, specJson, opIdOut));

  @override
  int join(Uint8List runtimeId, String specJson, Uint8List opIdOut) =>
      _invoke('task_join', () => _inner.join(runtimeId, specJson, opIdOut));

  @override
  int send(Uint8List runtimeId, Uint8List connId, String pathsJson, Uint8List opIdOut) =>
      _invoke('task_send', () => _inner.send(runtimeId, connId, pathsJson, opIdOut));

  @override
  int list(Uint8List runtimeId, Uint8List buffer, int cap) {
    if (_refused('task_list')) {
      throw _truncated('task_list');
    }
    _timing('task_list');
    return _inner.list(runtimeId, buffer, cap);
  }

  @override
  int disconnect(Uint8List runtimeId, Uint8List connId) =>
      _invoke('task_disconnect', () => _inner.disconnect(runtimeId, connId));

  @override
  int setSaveDir(Uint8List runtimeId, String path) =>
      _invoke('task_set_save_dir', () => _inner.setSaveDir(runtimeId, path));

  @override
  int consent(Uint8List runtimeId, Uint8List jobId, int accept) =>
      _invoke('task_consent', () => _inner.consent(runtimeId, jobId, accept));

  @override
  int opResult(Uint8List runtimeId, Uint8List opId, Uint8List buffer, int cap) =>
      _invoke('pairing_op_result', () => _inner.opResult(runtimeId, opId, buffer, cap));

  @override
  int pollEvent(Uint8List runtimeId, Uint8List buffer, int cap) {
    if (_refused('poll_event')) {
      throw _truncated('poll_event');
    }
    _timing('poll_event');
    return _inner.pollEvent(runtimeId, buffer, cap);
  }

  @override
  int pollProgress(Uint8List buffer, int cap) {
    if (_refused('poll_progress')) {
      throw _truncated('poll_progress');
    }
    _timing('poll_progress');
    return _inner.pollProgress(buffer, cap);
  }

  AkFfiException _truncated(String phase) => AkFfiException(
        AkStatus.errInternal,
        '诊断阶段${stage.letter}：$phase 已被诊断门截断（E4）',
      );

  @override
  String? lastError() => _inner.lastError();
}

// ---------------------------------------------------------------------------
// Bootstrap.
// ---------------------------------------------------------------------------

/// The outcome of [bootstrapAppService]: the real service, or a readable
/// construction failure for the startup-error card.
final class AppServiceBootstrap {
  const AppServiceBootstrap({this.service, this.error, this.diagStage, this.diagNote});

  /// The real [SimpleTransferService]; null when construction failed.
  final SimpleTransferService? service;

  /// The readable failure reason when [service] is null.
  final String? error;

  /// The E4 diagnostic stage of this run — non-null ONLY when
  /// `AK_DIAG_STAGE` was explicitly set (or [bootstrapAppService.diagStage]
  /// injected): the banner renders and the startup was truncated.
  final DiagStage? diagStage;

  /// Optional qualification appended to the banner text.
  final String? diagNote;
}

/// Builds the REAL transfer service over the native engine (P1-01),
/// C08-loaded (one resolution, probe before create) and truncated at the
/// E4 diagnostic stage when `AK_DIAG_STAGE` (or [diagStage]) says so.
///
/// Every platform seam is injectable so the tolerance branches are unit
/// testable without a DLL: [resolveDll] (the ONE three-tier probe),
/// [facade] (the staged engine owner — fakes intercept at a chosen
/// depth), [taskCalls] (the task-plane seam), [environment] (registry/
/// identity/diagnostic config), [stageLog] (the E2/E4 phase log) and the
/// Android identity-dir resolver. Construction failures degrade to
/// `service: null` + `error: <readable reason>` — never a throw.
AppServiceBootstrap bootstrapAppService({
  Map<String, String> Function()? environment,
  DllResolution? Function()? resolveDll,
  AkStagedEngineOwner? facade,
  AkTaskCalls Function()? taskCalls,
  bool? isAndroid,
  Future<String> Function()? androidIdentityDir,
  DiagStage? diagStage,
  AkStageLog? stageLog,
}) {
  final env = environment?.call() ?? Platform.environment;
  final explicitStage = diagStage ?? diagStageFromEnvironment(env);
  final stage = explicitStage ?? DiagStage.g;
  final android = isAndroid ?? Platform.isAndroid;
  final log = stageLog ?? AkStageLog.shared();
  if (explicitStage != null) {
    log.write('diag_stage_${stage.letter}');
  }

  // Stage A: no resolution, no load — the Flutter/runner baseline.
  if (stage == DiagStage.a) {
    final reason = '${diagStageBanner(stage)}——本阶段刻意不解析、不加载 ak_ffi。';
    return AppServiceBootstrap(
      service: UnusableSimpleTransferService(reason),
      error: reason,
      diagStage: explicitStage,
    );
  }

  // C08: resolve ONCE; this path (absolutized) is the actual load input.
  final resolution = (resolveDll ?? resolveAkDllPath)();
  if (resolution == null) {
    final candidates = akDllProbeCandidates().join('\n');
    return AppServiceBootstrap(
      service: null,
      error: '未找到 ak_ffi 动态库（已尝试：$candidates）。'
          '桌面版请将 ak_ffi.dll 放在程序目录或设置 AK_FFI_DLL。',
      diagStage: explicitStage,
    );
  }
  final loadPath = absoluteAkDllLoadPath(resolution);
  final owner = facade ?? AkEngineDartFacade(dllPath: loadPath, stageLog: log);
  try {
    // Stage B: DynamicLibrary.open only — no lookup, no export call.
    if (stage == DiagStage.b) {
      owner.openLibrary();
      return _diagHalt(stage, owner, loadPath, explicitStage);
    }

    // Stage C+: probe the task-plane exports BEFORE any engine exists
    // (C08: an old DLL is rejected without ak_engine_new ever running).
    if (stage.index >= DiagStage.c.index) {
      final missing = owner.probeTaskExports();
      if (missing.isNotEmpty) {
        owner.dispose();
        return AppServiceBootstrap(
          service: null,
          error: 'ak_ffi 动态库缺少任务平面导出（${missing.join('、')}）；'
              '请更新 ak_ffi 后重试：$resolution',
          diagStage: explicitStage,
        );
      }
      if (stage == DiagStage.c) {
        return _diagHalt(stage, owner, loadPath, explicitStage);
      }
    }

    // Stage D+: ak_engine_new over the same opened library.
    if (stage.index >= DiagStage.d.index) {
      owner.engineNew();
      if (stage == DiagStage.d) {
        return _diagHalt(stage, owner, loadPath, explicitStage);
      }
    }

    // Stages E/F/G: build the real service over the borrowed engine.
    // C07: the bindings carry the OWNER's lease probe — the facade flips
    // it not-alive in dispose BEFORE the native engine_free, so every
    // task-plane call after the release throws the explicit StateError
    // instead of touching the freed handle.
    final innerCalls = taskCalls != null
        ? taskCalls()
        : AkTaskFfiBindings(
            engine: owner.engineHandle,
            library: owner.library,
            isAlive: () => owner.isEngineAlive,
          );
    String? note;
    DiagTaskCallsGate? gate;
    if (stage == DiagStage.e || stage == DiagStage.f) {
      if (android) {
        // The E/F truncation relies on the synchronous desktop start
        // (Android's identity-dir await defers task_start past the
        // seal point) — Android keeps the full path, honestly noted.
        note = '（Android 不支持截断：按完整路径运行）';
      } else {
        gate = DiagTaskCallsGate(innerCalls, stage: stage, log: log);
      }
    } else if (explicitStage != null) {
      // An explicit diagnostic G observes the startup call timing.
      gate = DiagTaskCallsGate(innerCalls, stage: stage, log: log, observeOnly: true);
    }
    // The service's dispose releases the engine through `engineOwner`
    // AFTER its own borrowers (the P1-05 release order).
    final service = FfiSimpleTransferService(
      calls: gate ?? innerCalls,
      environment: environment,
      isAndroid: isAndroid,
      androidIdentityDir: androidIdentityDir,
      engineOwner: owner.dispose,
    );
    if (gate != null && !gate.observeOnly) {
      // E/F: drive task_start NOW (before the shell exists) so the
      // shell's own idempotent start() adds nothing native. The whole
      // native startup sequence of FfiSimpleTransferService.start() runs
      // synchronously on desktop, so sealing right after the call
      // returns freezes exactly the intended depth. A failed start is
      // swallowed here and resurfaces through the shell's own retry.
      final starting = service.start();
      if (stage == DiagStage.f) {
        gate.seal();
      }
      starting.ignore();
    }
    return AppServiceBootstrap(
      service: service,
      diagStage: explicitStage,
      diagNote: note,
    );
  } on Object catch (error) {
    owner.dispose();
    return AppServiceBootstrap(
      service: null,
      error: '$error',
      diagStage: explicitStage,
    );
  }
}

/// The A–D truncation halt: the Unusable service holds the owner alive
/// ([UnusableSimpleTransferService.keepAlive] — dispose is a no-op), so
/// the loaded library (B/C) and the created engine (D) survive the whole
/// observation window (E4: teardown must not mix into the stage).
AppServiceBootstrap _diagHalt(
  DiagStage stage,
  AkStagedEngineOwner owner,
  String loadPath,
  DiagStage? explicitStage,
) {
  final reason = '${diagStageBanner(stage)}（$loadPath）';
  return AppServiceBootstrap(
    service: UnusableSimpleTransferService(reason, keepAlive: owner),
    error: reason,
    diagStage: explicitStage,
  );
}

/// The inert service handed to the shell when the real engine could not
/// be constructed: navigation keeps working, every action surfaces the
/// honest reason, streams stay empty (wave-29 P1-01 tolerance branch).
class UnusableSimpleTransferService implements SimpleTransferService {
  const UnusableSimpleTransferService(this.reason, {this.keepAlive});

  /// Why the real engine is unavailable (rendered by the error card).
  final String reason;

  /// E4 keep-alive for the truncation stages (B/C/D): the object whose
  /// native lifetime must outlive the observation window (the staged
  /// engine owner). [dispose] deliberately does NOT release it.
  final Object? keepAlive;

  @override
  Future<void> start() async {
    throw StateError(reason);
  }

  @override
  Future<TaskCode> createCode(TaskRole role) async {
    throw StateError(reason);
  }

  @override
  Future<JoinResult> joinCode(String code, {TaskRole? role}) async {
    return JoinResult.failure(reason);
  }

  @override
  Stream<List<TaskConnection>> get connections => const Stream.empty();

  @override
  Stream<IncomingRequest> get incomingRequests => const Stream.empty();

  @override
  Stream<String> get expiredIncomingRequests => const Stream.empty();

  @override
  Stream<EngineHealth> get engineHealth => const Stream.empty();

  @override
  Future<void> consent(String jobId, bool accept) async {
    throw StateError(reason);
  }

  @override
  Future<String> sendFiles(String connId, List<String> paths) async {
    throw StateError(reason);
  }

  @override
  Future<void> disconnect(String connId) async {}

  @override
  Future<void> setSaveDir(String path) async {}

  @override
  String get saveDir => '';

  @override
  void dispose() {}
}
