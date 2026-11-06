/// AK-sending app entry (wave-28 v1 极简配对 UI, wave-29 real wiring).
///
/// The REAL engine stack is wired here (P1-01 fix — the app no longer
/// boots the local demo fake):
///
/// ```text
/// main.dart ─▶ AkEngineDartFacade (owns AkEngineDart: DLL probe + handle)
///                     ▼ borrowEngine() (ffi-v2 §2: borrow, never dispose)
///             AkTaskFfiBindings (engine pointer + library, task-plane
///                     ▼          export probe via missingTaskExports)
///             FfiSimpleTransferService (the UI contract adapter; resolves
///                     ▼          AK_REGISTRY_ADDR/AK_REGISTRY_ID env →
///                 compiled --dart-define defaults; Android identity dir
///                 via path_provider, gated by identityDirReady)
///             SimpleTransferService engine service (ak_task_* loops)
/// ```
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

import 'package:flutter/material.dart';

import 'package:ak_ffi_dart/ak_ffi_dart.dart' show AkTaskFfiBindings;

import 'services/ak_engine_facade.dart';
import 'services/dll_path.dart';
import 'services/ffi_simple_transfer_service.dart';
import 'ui/app.dart' show AkSendingApp;
import 'ui/service/simple_transfer_service.dart';

void main() {
  final bootstrap = bootstrapAppService();
  runApp(AkSendingApp(
    service: bootstrap.service ??
        UnusableSimpleTransferService(bootstrap.error ?? '未知错误'),
    startupError: bootstrap.error,
  ));
}

/// The outcome of [bootstrapAppService]: the real service, or a readable
/// construction failure for the startup-error card.
final class AppServiceBootstrap {
  const AppServiceBootstrap({this.service, this.error});

  /// The real [SimpleTransferService]; null when construction failed.
  final SimpleTransferService? service;

  /// The readable failure reason when [service] is null.
  final String? error;
}

/// Builds the REAL transfer service over the native engine (P1-01).
///
/// Every platform seam is injectable so the tolerance branches are unit
/// testable without a DLL: [resolveDll] (three-tier probe), [facade]
/// (engine owner), [environment] (registry/identity config) and the
/// Android identity-dir resolver. Construction failures degrade to
/// `service: null` + `error: <readable reason>` — never a throw.
AppServiceBootstrap bootstrapAppService({
  Map<String, String> Function()? environment,
  DllResolution? Function()? resolveDll,
  AkEngineDartFacade? facade,
  bool? isAndroid,
  Future<String> Function()? androidIdentityDir,
}) {
  final resolution = (resolveDll ?? resolveAkDllPath)();
  if (resolution == null) {
    final candidates = akDllProbeCandidates().join('\n');
    return AppServiceBootstrap(
      service: null,
      error: '未找到 ak_ffi 动态库（已尝试：$candidates）。'
          '桌面版请将 ak_ffi.dll 放在程序目录或设置 AK_FFI_DLL。',
    );
  }
  final owner = facade ?? AkEngineDartFacade();
  try {
    final engine = owner.borrowEngine(); // throws when the DLL cannot open
    final library = engine.bindings.library;
    final missing = AkTaskFfiBindings.missingTaskExports(library);
    if (missing.isNotEmpty) {
      owner.dispose();
      return AppServiceBootstrap(
        service: null,
        error: 'ak_ffi 动态库缺少任务平面导出（${missing.join('、')}）；'
            '请更新 ak_ffi 后重试：$resolution',
      );
    }
    // The service's dispose releases the engine through `engineOwner`
    // AFTER its own borrowers (the P1-05 release order).
    final service = FfiSimpleTransferService(
      calls: AkTaskFfiBindings(engine: engine.nativeHandle, library: library),
      environment: environment,
      isAndroid: isAndroid,
      androidIdentityDir: androidIdentityDir,
      engineOwner: owner.dispose,
    );
    return AppServiceBootstrap(service: service);
  } on Object catch (error) {
    owner.dispose();
    return AppServiceBootstrap(service: null, error: '$error');
  }
}

/// The inert service handed to the shell when the real engine could not
/// be constructed: navigation keeps working, every action surfaces the
/// honest reason, streams stay empty (wave-29 P1-01 tolerance branch).
class UnusableSimpleTransferService implements SimpleTransferService {
  const UnusableSimpleTransferService(this.reason);

  /// Why the real engine is unavailable (rendered by the error card).
  final String reason;

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
