/// [FfiSimpleTransferService]: the REAL wave-28 service behind agent D's
/// frozen UI contract (`lib/ui/service/simple_transfer_service.dart`) —
/// a thin adapter over this package's own engine service
/// (`SimpleTransferService` in `lib/services/simple_transfer_service.dart`,
/// the wave-28 FFI/Dart deliverable), which drives the `ak_task_*` C ABI
/// through the [AkTaskCalls] seam.
///
/// Why an adapter: the UI contract and the engine service were frozen in
/// parallel (wave-28's two-agent split); this file is the single switch
/// point that reconciles them without touching either frozen shape:
///
/// ```text
/// lib/ui (agent D, codes against its frozen interface)
///        ▲ implements
/// FfiSimpleTransferService (THIS adapter)
///        ▲ delegates to
/// SimpleTransferService (services/simple_transfer_service.dart)
///        ▲
/// AkTaskCalls → ak_task_* → crates/ak-ffi task plane
/// ```
///
/// Reconciliation notes (each mapping is the honest v1 reading of the
/// user-frozen product model):
///
/// * **registry config**: [start] resolves the registry exactly like the
///   invite plane — `AK_REGISTRY_ADDR`/`AK_REGISTRY_ID` environment first
///   (tests and operator overrides), then the compiled `--dart-define`
///   defaults (the only way a standalone phone install gets a real
///   registry), the same precedence as
///   `FfiTransferService.inviteConfigFromEnvironment`; the identity
///   directory resolves like `FfiTransferService.defaultIdentityDir`
///   (`AK_ID_DIR` override, else the user-profile `.ak-sending`).
/// * **role**: `TaskRole.send`/`receive` ⇄ the engine's sender/receiver.
/// * **code**: the engine mints `AKT1-XXXX-XXXX-XXXX` (12 hex in three
///   groups — 2^48 space, no key material inside); joining tolerates
///   hyphens, spaces and lower case engine-side (the code-table key
///   strips separators).
/// * **state**: the engine's `probing` maps to the contract's `testing`
///   (the auto connectivity probe; `reachable` itself is pending
///   ak-session `probe_peer`, WAVE-28/B-PENDING); `connecting` is the
///   contract's transient that this v1 skips (a join lands directly in
///   probing); dropped rows are filtered out of the `connections`
///   snapshot per the contract.
/// * **peerName**: the zero-identity v1 model carries no peer name — the
///   row shows the task code's short label until registry-relayed device
///   names land with agent B.
/// * **consent**: with `accept: true` the returned future completes when
///   the receive finishes (the engine's `done` event for that
///   connection) and rejects on failure/timeout; declines resolve
///   immediately.
/// * **sendFiles**: resolves with a readable Chinese summary built from
///   the engine report (`已发送 N 个文件（formatBytes）`).
library;

import 'dart:async' show Stream, StreamSubscription, TimeoutException;
import 'dart:io' show Platform;

import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show AkFfiException, AkStatus, AkTaskCalls;

import '../ui/service/simple_transfer_service.dart'
    show
        IncomingRequest,
        JoinResult,
        SimpleTransferService,
        TaskCode,
        TaskConnection,
        TaskConnectionState,
        TaskRole,
        formatBytes;

import 'simple_transfer_service.dart' as task_engine;

/// The real wave-28 service for the frozen UI contract.
final class FfiSimpleTransferService implements SimpleTransferService {
  FfiSimpleTransferService({
    required AkTaskCalls calls,
    this.registry,
    this.registryIdHex,
    this.idDir,
    String? saveDir,
    Map<String, String> Function()? environment,
    Future<void> Function(Duration)? delay,
  })  : _saveDirOverride = saveDir,
        _environment = environment ?? _platformEnvironment,
        _engine = task_engine.SimpleTransferService(
          calls: calls,
          delay: delay,
          eventPollInterval: const Duration(milliseconds: 50),
        );

  /// Explicit registry endpoint override (tests/operators); null resolves
  /// from the environment/compiled defaults at [start].
  final String? registry;

  /// Explicit registry pin override (64 hex); null resolves like [registry].
  final String? registryIdHex;

  /// Explicit identity directory override; null resolves `AK_ID_DIR`,
  /// else the user-profile `.ak-sending`.
  final String? idDir;

  /// Explicit default save folder override; null keeps the engine default
  /// (`<id_dir>/downloads`).
  final String? _saveDirOverride;

  final Map<String, String> Function() _environment;
  final task_engine.SimpleTransferService _engine;

  StreamSubscription<task_engine.SimpleIncomingRequest>? _requestSub;
  final Map<String, String?> _connOfJob = <String, String?>{};

  /// The engine service (exposed for diagnostics/teardown tests).
  task_engine.SimpleTransferService get engine => _engine;

  // -- lifecycle ---------------------------------------------------------

  @override
  Future<void> start() async {
    final env = _environment();
    final address = registry ?? _envOrCompiled(env, 'AK_REGISTRY_ADDR');
    final pin = registryIdHex ?? _envOrCompiled(env, 'AK_REGISTRY_ID');
    if (address == null || pin == null) {
      throw StateError('未配置注册中心：请设置 AK_REGISTRY_ADDR 与 AK_REGISTRY_ID'
          '（或编译期 --dart-define 注入）。');
    }
    final dir = idDir ?? _resolveIdentityDir(env);
    await _engine.start(
      registry: address,
      registryIdHex: pin,
      idDir: dir,
      saveDir: _saveDirOverride,
    );
    // Remember each pending request's connection so the consent waiter
    // can watch the RIGHT receive's done event.
    _requestSub = _engine.incomingRequests.listen((request) {
      _connOfJob[request.jobId] = request.connId;
    });
  }

  @override
  void dispose() {
    _requestSub?.cancel();
    _requestSub = null;
    _engine.dispose();
  }

  // -- the three actions --------------------------------------------------

  @override
  Future<TaskCode> createCode(TaskRole role) async {
    final code = await _engine.createCode(_engineRole(role));
    return TaskCode(code: code, role: role);
  }

  @override
  Future<JoinResult> joinCode(String code) async {
    try {
      final joined = await _engine.join(code);
      return JoinResult.success(joined.connId);
    } on AkFfiException catch (error) {
      return JoinResult.failure(_readable(error));
    } on TimeoutException {
      return JoinResult.failure('加入超时：请确认任务码后重试。');
    }
  }

  @override
  Stream<List<TaskConnection>> get connections => _engine.connections.map(
      (rows) => rows
          .where((row) => row.state != 'disconnected')
          .map(_rowOf)
          .toList());

  @override
  Stream<IncomingRequest> get incomingRequests => _engine.incomingRequests
      .map((request) => IncomingRequest(
            jobId: request.jobId,
            fileCount: request.files,
            totalBytes: request.bytes,
          ));

  @override
  Future<void> consent(String jobId, bool accept) async {
    final decided = await _engine.consent(jobId, accept: accept);
    if (!decided) {
      throw StateError('该请求已失效（可能已被处理或超过 30 秒自动拒绝）。');
    }
    if (!accept) {
      return; // declined: nothing is received, resolve immediately
    }
    // Accepted: the contract completes when the receive finishes. Watch
    // the done event of THIS request's connection (any done when the
    // request carried no connection id).
    final conn = _connOfJob[jobId];
    try {
      await _engine.doneEvents
          .firstWhere((done) => conn == null || done.conn == conn)
          .timeout(const Duration(seconds: 180));
    } on TimeoutException {
      throw TimeoutException('接收未在限时内完成。', const Duration(seconds: 180));
    }
  }

  @override
  Future<String> sendFiles(String connId, List<String> paths) async {
    final report = await _engine.sendFiles(connId, paths);
    final files = report['files'] is int ? report['files'] as int : 0;
    final bytes = report['bytes'] is int ? report['bytes'] as int : 0;
    return '已发送 $files 个文件（${formatBytes(bytes)}）';
  }

  @override
  Future<void> disconnect(String connId) => _engine.disconnect(connId);

  // -- the one setting ----------------------------------------------------

  @override
  Future<void> setSaveDir(String path) => _engine.setSaveDir(path);

  @override
  String get saveDir => _engine.saveDir ?? '';

  // -- internals ----------------------------------------------------------

  static Map<String, String> _platformEnvironment() => Platform.environment;

  /// Environment first, then the compiled `--dart-define` default (empty
  /// when the build passed none — the invite plane's precedence).
  static String? _envOrCompiled(Map<String, String> env, String key) {
    final fromEnv = env[key]?.trim();
    if (fromEnv != null && fromEnv.isNotEmpty) {
      return fromEnv;
    }
    final compiled = switch (key) {
      'AK_REGISTRY_ADDR' => const String.fromEnvironment('AK_REGISTRY_ADDR'),
      _ => const String.fromEnvironment('AK_REGISTRY_ID'),
    };
    final trimmed = compiled.trim();
    return trimmed.isEmpty ? null : trimmed;
  }

  /// `AK_ID_DIR` override, else the user-profile `.ak-sending` (the
  /// `FfiTransferService.defaultIdentityDir` pattern, minus the Android
  /// path_provider tier which the app shell supplies via [idDir]).
  static String _resolveIdentityDir(Map<String, String> env) {
    final overridden = env['AK_ID_DIR']?.trim();
    if (overridden != null && overridden.isNotEmpty) {
      return overridden;
    }
    final home = env['USERPROFILE'] ?? env['HOME'] ?? '';
    if (home.isEmpty) {
      return '.ak-sending';
    }
    final separator = Platform.isWindows ? r'\' : '/';
    return '$home$separator.ak-sending';
  }

  task_engine.SimpleTransferRole _engineRole(TaskRole role) =>
      role == TaskRole.send
          ? task_engine.SimpleTransferRole.sender
          : task_engine.SimpleTransferRole.receiver;

  TaskRole _uiRole(String role) =>
      role == 'sender' ? TaskRole.send : TaskRole.receive;

  TaskConnectionState _uiState(String state) {
    switch (state) {
      case 'connected':
        return TaskConnectionState.connected;
      case 'disconnected':
        return TaskConnectionState.disconnected;
      case 'probing':
      default:
        // The auto connectivity probe (reachability pending B).
        return TaskConnectionState.testing;
    }
  }

  TaskConnection _rowOf(task_engine.SimpleConnection row) => TaskConnection(
        id: row.connId,
        peerName: _peerNameOf(row),
        role: _uiRole(row.role),
        state: _uiState(row.state),
      );

  /// The zero-identity v1 model carries no peer name: the code's short
  /// label stands in (device names arrive with the registry relay).
  String _peerNameOf(task_engine.SimpleConnection row) {
    final code = row.code;
    if (code == null || code.length < 8) {
      return '对端设备';
    }
    return '对端 ${code.substring(code.length - 8)}';
  }

  String _readable(AkFfiException error) {
    if (error.code == AkStatus.errInvalid) {
      return '任务码无效或已被使用，请确认后重试。';
    }
    return error.message;
  }
}
