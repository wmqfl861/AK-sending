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
///   `FfiTransferService.inviteConfigFromEnvironment`.
/// * **identity directory** (wave-29 P1-01): desktop resolves `AK_ID_DIR`,
///   else the user-profile `.ak-sending` (`FfiTransferService
///   .defaultIdentityDir`'s pattern); Android has NO usable environment,
///   so the default resolves asynchronously from path_provider
///   (`getApplicationSupportDirectory()/.ak-sending`) — [start] (and
///   [identityDirReady]) gate on that resolve exactly like
///   `FfiTransferService.identityDirReady`. An explicit [idDir] still
///   wins over every tier.
/// * **role**: `TaskRole.send`/`receive` ⇄ the engine's sender/receiver;
///   [joinCode]'s optional role is the joiner's EXPLICIT direction (the
///   UI user's intent) riding the join spec (wave-29 P1-05).
/// * **code**: the engine mints the REAL display form
///   `AKI1-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX` (the Rust
///   `InviteCode::to_display` shape: 160-bit secret in Crockford Base32,
///   I/L/O/U excluded); joining tolerates case, hyphens and spaces
///   engine-side (the code-table key strips separators).
/// * **state**: the engine's `probing` maps to the contract's `testing`
///   (the auto connectivity probe); a FAILED probe maps to the
///   contract's `unreachable` (wave-29 P2-09 — never to "success");
///   dropped rows are filtered out of the `connections` snapshot per the
///   contract.
/// * **peerName**: the zero-identity v1 model carries no peer name — the
///   row shows the task code's short label until registry-relayed device
///   names land with agent B.
/// * **consent**: with `accept: true` the returned future completes when
///   the receive finishes (the engine's `done` event for that
///   connection), rejects on failure/disconnect and — past
///   [receiveTimeout] — cancels the native task through the disconnect
///   path before rejecting (wave-29 P2-10); declines resolve immediately
///   and are IDEMPOTENT for an already-expired/decided request
///   (wave-29 P2-11: the 30 s auto-decline window may have closed first).
/// * **sendFiles**: resolves with a readable Chinese summary built from
///   the engine report (`已发送 N 个文件（formatBytes）`); past
///   [sendTimeout] the engine service cancels the native op and the
///   rejection says so (wave-29 P2-10).
library;

import 'dart:async' show Stream, StreamSubscription, TimeoutException;
import 'dart:io' show Platform;

import 'package:path_provider/path_provider.dart'
    show getApplicationSupportDirectory;

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
    bool? isAndroid,
    Future<String> Function()? androidIdentityDir,
    this.receiveTimeout = const Duration(seconds: 180),
    this.sendTimeout = const Duration(seconds: 120),
    this.engineOwner,
  })  : _saveDirOverride = saveDir,
        _environment = environment ?? _platformEnvironment,
        _isAndroidPlatform = isAndroid ?? Platform.isAndroid,
        _androidIdentityDir =
            androidIdentityDir ?? defaultAndroidIdentityDir,
        _engine = task_engine.SimpleTransferService(
          calls: calls,
          delay: delay,
          eventPollInterval: const Duration(milliseconds: 50),
        ) {
    // Wave-29 P1-01: Android resolves the real identity directory from
    // path_provider; every dir consumer gates on [identityDirReady]
    // (immediately complete on desktop). Never throws — a provider
    // failure keeps the desktop-style fallback.
    identityDirReady = _isAndroidPlatform
        ? _resolveAndroidIdentityDir(_androidIdentityDir)
        : Future<void>.value();
  }

  /// Explicit registry endpoint override (tests/operators); null resolves
  /// from the environment/compiled defaults at [start].
  final String? registry;

  /// Explicit registry pin override (64 hex); null resolves like [registry].
  final String? registryIdHex;

  /// Explicit identity directory override; null resolves `AK_ID_DIR`,
  /// then the Android path_provider tier, else the user-profile
  /// `.ak-sending`.
  final String? idDir;

  /// Explicit default save folder override; null keeps the engine default
  /// (`<id_dir>/downloads`).
  final String? _saveDirOverride;

  /// Wall-clock budget for the receive half of an accepted consent
  /// (wave-29 P2-10; on expiry the native task is cancelled through the
  /// disconnect path before the future rejects).
  final Duration receiveTimeout;

  /// Wall-clock budget handed to the engine's send wait (P2-10).
  final Duration sendTimeout;

  /// Optional owner of the native engine handle behind [AkTaskCalls] —
  /// released AFTER the engine service in [dispose] (borrowers-first,
  /// the P1-05 release order). The app shell passes the facade it opened
  /// in `main.dart`.
  final void Function()? engineOwner;

  final Map<String, String> Function() _environment;
  final bool _isAndroidPlatform;
  final Future<String> Function() _androidIdentityDir;
  final task_engine.SimpleTransferService _engine;

  /// How often the accepted-receive stall loop wakes to check settled
  /// state and drain progress (wave-30 P2-10).
  static const Duration receivePollInterval = Duration(milliseconds: 50);

  /// The path_provider-resolved default identity dir once it lands
  /// (Android only; null until then).
  String? _platformIdentityDir;

  /// Completes when the Android identity-directory resolve settles
  /// (immediately on desktop). Never throws.
  late final Future<void> identityDirReady;

  StreamSubscription<task_engine.SimpleIncomingRequest>? _requestSub;
  final Map<String, String?> _connOfJob = <String, String?>{};

  /// The engine service (exposed for diagnostics/teardown tests).
  task_engine.SimpleTransferService get engine => _engine;

  // -- lifecycle ---------------------------------------------------------

  @override
  Future<void> start() async {
    if (_isAndroidPlatform) {
      await identityDirReady; // the real dir gates the engine's id_dir
    }
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
    engineOwner?.call(); // the native engine goes LAST (borrowers first)
  }

  // -- the three actions --------------------------------------------------

  @override
  Future<TaskCode> createCode(TaskRole role) async {
    final created = await _engine.createCode(_engineRole(role));
    // Wave-30 P2-04: the code_id rides up with the display code — it is
    // the cancel key (`disconnect(codeId)` revokes the un-joined code).
    return TaskCode(code: created.code, role: role, codeId: created.codeId);
  }

  @override
  Future<JoinResult> joinCode(String code, {TaskRole? role}) async {
    try {
      final joined = await _engine.join(code, role: _engineRoleOrNull(role));
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
            .where((row) =>
                row.state != 'disconnected' && row.state != 'done')
            .map(_rowOf)
            .toList());

  @override
  Stream<IncomingRequest> get incomingRequests => _engine.incomingRequests
      .map((request) => IncomingRequest(
            jobId: request.jobId,
            fileCount: request.files,
            totalBytes: request.bytes,
            saveDir: request.saveDir, // P2-06: the request's real target
          ));

  @override
  Stream<String> get expiredIncomingRequests =>
      _engine.expiredIncomingRequests;

  @override
  Future<void> consent(String jobId, bool accept) async {
    final decided = await _engine.consent(jobId, accept: accept);
    if (!decided) {
      if (!accept) {
        // Wave-29 P2-11: a decline of an already-expired/auto-declined
        // request is IDEMPOTENT — the engine declined it (or the 30 s
        // window did); nothing was received either way.
        return;
      }
      throw StateError('该请求已失效（可能已被处理或超过 30 秒自动拒绝）。');
    }
    if (!accept) {
      return; // declined: nothing is received, resolve immediately
    }
    // Accepted: the contract completes when the receive finishes. Watch
    // the done event of THIS request's connection AND the connection
    // itself going terminal (a mid-transfer disconnect must not hang the
    // wait — P2-10). Wave-30 P2-10: [receiveTimeout] is a STALL budget,
    // not an overall cap — receive progress lines (keyed by the pairing
    // code id; the connection id also watched defensively) RENEW the
    // deadline, so only a true no-progress stall elapses.
    final conn = _connOfJob[jobId];
    final progressJobs = <String>{?conn};
    final codeId = conn == null ? null : _engine.codeIdOfConnection(conn);
    if (codeId != null) {
      progressJobs.add(codeId);
    }
    try {
      await _awaitReceiveSettled(conn, progressJobs);
    } on TimeoutException {
      final cancelFailure = await _cancelReceiveTask(conn);
      if (cancelFailure != null) {
        throw TimeoutException(
          '接收超时（${receiveTimeout.inSeconds} 秒无进展），且取消原生任务失败'
          '（$cancelFailure）——接收可能仍在后台进行，请稍后重试或手动断开连接。',
          receiveTimeout,
        );
      }
      throw TimeoutException(
        '接收超时（${receiveTimeout.inSeconds} 秒无进展），已取消原生接收任务并断开该连接。',
        receiveTimeout,
      );
    }
  }

  /// The accepted-receive wait (P2-10 stall semantics): settles when the
  /// connection's `done` event lands or the row goes terminal, and
  /// throws [TimeoutException] only after [receiveTimeout] WITHOUT any
  /// progress line for [progressJobs] (every observed line renews the
  /// deadline — a long, healthy receive is never cut by total duration).
  Future<void> _awaitReceiveSettled(
      String? conn, Set<String> progressJobs) async {
    var settled = false;
    Object? failure;
    final doneSub = _engine.doneEvents.listen((event) {
      if (conn == null || event.conn == conn) {
        settled = true;
      }
    });
    final lostSub = conn == null
        ? null
        : _engine.connections.listen((rows) {
            for (final row in rows) {
              if (row.connId != conn) {
                continue;
              }
              if (row.state == 'done') {
                // P2-05: the one-task transfer COMPLETED — the row
                // converging to `done` is the success verdict itself
                // (it may beat the done event through the pump).
                settled = true;
                return;
              }
              if (row.state == 'disconnected') {
                settled = true;
                failure = StateError('连接已断开，接收已中断。');
                return;
              }
            }
          });
    try {
      var baseline = <String, int>{
        for (final job in progressJobs) job: _engine.progressTick(job),
      };
      var deadline = DateTime.now().add(receiveTimeout);
      while (!settled) {
        // A REAL timer yield (not the injectable op delay): the watchdog
        // must give the event loop room to deliver the stream events it
        // watches and let tests drive the fake synchronously around it.
        await Future<void>.delayed(receivePollInterval);
        _engine.drainProgress();
        var advanced = false;
        for (final job in progressJobs) {
          final tick = _engine.progressTick(job);
          if (tick > (baseline[job] ?? 0)) {
            advanced = true;
            baseline[job] = tick;
          }
        }
        if (advanced) {
          deadline = DateTime.now().add(receiveTimeout); // bytes moved
        }
        if (DateTime.now().isAfter(deadline)) {
          throw TimeoutException('接收无进展超时', receiveTimeout);
        }
      }
      final error = failure;
      if (error != null) {
        throw error;
      }
    } finally {
      await doneSub.cancel();
      await lostSub?.cancel();
    }
  }

  /// P2-10's receive-side cancel: the FFI disconnect path stops the
  /// native receive. Returns null when the cancel went through (or the
  /// connection was already gone — `-4` is benign); the failure detail
  /// when the cancel itself failed — the caller must then NOT claim
  /// "已取消". The engine call is AWAITED: its failure lives in the
  /// future (an async method), not in a synchronous throw.
  Future<Object?> _cancelReceiveTask(String? connId) async {
    final id = connId;
    if (id == null) {
      return null;
    }
    try {
      await _engine.disconnect(id);
      return null;
    } on AkFfiException catch (error) {
      return error.message.isNotEmpty ? error.message : error;
    } on Object catch (error) {
      return error;
    }
  }

  @override
  Future<String> sendFiles(String connId, List<String> paths) async {
    final report =
        await _engine.sendFiles(connId, paths, timeout: sendTimeout);
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

  /// `AK_ID_DIR` override, then the Android path_provider tier (already
  /// resolved into [_platformIdentityDir] by [identityDirReady]), else
  /// the user-profile `.ak-sending` (the
  /// `FfiTransferService.defaultIdentityDir` pattern).
  String _resolveIdentityDir(Map<String, String> env) {
    final overridden = env['AK_ID_DIR']?.trim();
    if (overridden != null && overridden.isNotEmpty) {
      return overridden;
    }
    final platform = _platformIdentityDir;
    if (platform != null && platform.isNotEmpty) {
      return platform;
    }
    final home = env['USERPROFILE'] ?? env['HOME'] ?? '';
    if (home.isEmpty) {
      return '.ak-sending';
    }
    final separator = Platform.isWindows ? r'\' : '/';
    return '$home$separator.ak-sending';
  }

  /// Resolves (or runs [resolver], injectable for tests) the Android
  /// identity directory and anchors [_platformIdentityDir]. Never throws
  /// — see [identityDirReady].
  Future<void> _resolveAndroidIdentityDir(
      Future<String> Function() resolver) async {
    try {
      final dir = await resolver();
      if (dir.isEmpty) {
        return;
      }
      _platformIdentityDir = dir;
    } on Object {
      // path_provider unavailable: keep the desktop-style fallback.
    }
  }

  /// Android default identity directory: `<app support>/.ak-sending` via
  /// path_provider (the `FfiTransferService.defaultAndroidIdentityDir`
  /// pattern — app-private storage, no permission, survives runs).
  static Future<String> defaultAndroidIdentityDir() async {
    final support = await getApplicationSupportDirectory();
    final base = support.path;
    return base.endsWith('/') || base.endsWith(r'\')
        ? '$base.ak-sending'
        : '$base/.ak-sending';
  }

  task_engine.SimpleTransferRole _engineRole(TaskRole role) =>
      role == TaskRole.send
          ? task_engine.SimpleTransferRole.sender
          : task_engine.SimpleTransferRole.receiver;

  task_engine.SimpleTransferRole? _engineRoleOrNull(TaskRole? role) =>
      role == null ? null : _engineRole(role);

  TaskRole _uiRole(String role) =>
      role == 'sender' ? TaskRole.send : TaskRole.receive;

  TaskConnectionState _uiState(String state) {
    switch (state) {
      case 'connected':
        return TaskConnectionState.connected;
      case 'unreachable':
        // The probe FAILED (P2-09): an honest negative verdict, never
        // "success" and not the terminal `disconnected` either.
        return TaskConnectionState.unreachable;
      case 'disconnected':
      case 'done':
        // `done` (P2-05): the one-task transfer completed — terminal for
        // the pairing; the UI labels it like a dropped row (the live
        // snapshot filters it out entirely).
        return TaskConnectionState.disconnected;
      case 'probing':
      default:
        // The auto connectivity probe (verdict pending).
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
