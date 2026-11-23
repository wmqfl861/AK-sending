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
/// * **deviceName**: X5 P1-3 — [start] announces THIS device's
///   self-reported name to the engine (`device_name`; the frozen contract
///   call passes none, so the adapter resolves the platform default —
///   see [resolvePlatformDeviceName]; every failure leaves the engine's
///   own default standing).
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
/// * **peerName**: X5 (batch X, 2026-11-22) — the engine's
///   self-reported peer name (`connected.peer_name`) rides the row,
///   annotated by the verified fingerprint's short code
///   (`peer_fp_short`); an engine predating the fields keeps the
///   spec-mandated no-name default 「对方的设备」 (handoff §4.5 item 5)
///   and no short code.
/// * **consent**: with `accept: true` the returned future completes when
///   the receive finishes (the engine's `done` event for that
///   connection), rejects on failure/disconnect and — past
///   [receiveTimeout] — cancels the native task through the disconnect
///   path before rejecting (wave-29 P2-10); declines resolve immediately
///   and are IDEMPOTENT for an already-expired/decided request
///   (wave-29 P2-11: the auto-decline window — 120 s since E2/G4 — may
///   have closed first).
/// * **sendFiles**: resolves with a readable Chinese summary built from
///   the engine report (`已发送 N 个文件（formatBytes）`); past
///   [sendTimeout] the engine service cancels the native op and the
///   rejection says so (wave-29 P2-10).
library;

import 'dart:async'
    show FutureOr, Stream, StreamSubscription, TimeoutException;
import 'dart:convert' show utf8;
import 'dart:io' show Platform;

import 'package:flutter/services.dart' show MethodChannel;

import 'package:path_provider/path_provider.dart'
    show getApplicationSupportDirectory;

import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show AkFfiException, AkStatus, AkTaskCalls;

import '../ui/service/simple_transfer_service.dart'
    show
        EngineHealth,
        IncomingRequest,
        JoinResult,
        SimpleTransferService,
        TaskCode,
        TaskConnection,
        TaskConnectionState,
        TaskRole,
        TransferPhase,
        TransferProgress,
        formatBytes;

import 'direct_path_label.dart' show directPathLabel;
import 'screen_keep_awake.dart' show ScreenKeepAwake;
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
    FutureOr<String?> Function(bool isAndroid)? deviceNameResolver,
    this.receiveTimeout = const Duration(seconds: 180),
    // E2/G4: a STALL budget that must clear the 120 s consent window
    // (the wait produces no progress lines) — 150 s, same headroom as
    // the engine's TASK_SEND_IDLE.
    this.sendTimeout = const Duration(seconds: 150),
    this.engineOwner,
  })  : _saveDirOverride = saveDir,
        _environment = environment ?? _platformEnvironment,
        _isAndroidPlatform = isAndroid ?? Platform.isAndroid,
        _androidIdentityDir =
            androidIdentityDir ?? defaultAndroidIdentityDir,
        _deviceNameResolver =
            deviceNameResolver ?? resolvePlatformDeviceName,
        _engine = task_engine.SimpleTransferService(
          calls: calls,
          delay: delay,
          // CR-P2-05: the PURE-POLLING active cadence (D1's 50 ms) — the
          // engine service's native doorbell registration (2026-11-12
          // 后半) slows the effective tick to its 250 ms safety net by
          // itself when the seam supports `ak_task_set_event_notify`;
          // fakes/old DLLs keep exactly this 50 ms behavior.
          eventPollInterval: const Duration(milliseconds: 50),
        ) {
    // Wave-29 P1-01: Android resolves the real identity directory from
    // path_provider; every dir consumer gates on [identityDirReady]
    // (immediately complete on desktop). Never throws — a provider
    // failure keeps the desktop-style fallback.
    identityDirReady = _isAndroidPlatform
        ? _resolveAndroidIdentityDir(_androidIdentityDir)
        : Future<void>.value();
    // W7.3 (batch W 交接 §4.7 第 3 条): the keep-screen-on coordinator
    // over the ENGINE service's shared in-progress judgment — Android
    // only (the injected gate mirrors this adapter's own platform
    // seam), best-effort channel, released in [dispose] before the
    // engine service goes.
    _screenKeepAwake = ScreenKeepAwake(
      service: _engine,
      isAndroid: () => _isAndroidPlatform,
    );
  }

  /// W7.3: the `dev.ak/screen` coordinator (see [ScreenKeepAwake]).
  late final ScreenKeepAwake _screenKeepAwake;

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

  /// X5 P1-3: resolves this device's self-reported name when [start]
  /// carries none — injectable so tests stay off the platform channel.
  final FutureOr<String?> Function(bool isAndroid) _deviceNameResolver;

  /// P2-A (third review): the budget of the Android device-name
  /// resolution — a channel round-trip against a dead platform thread
  /// must not hang [start] forever; on timeout the engine's own default
  /// name ('ak-task') stands.
  static const Duration deviceNameResolveTimeout = Duration(seconds: 3);

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

  /// X5 P1-3: [deviceName] is this device's SELF-REPORTED name for the
  /// engine's `device_name` (what the peer's UI shows — display only,
  /// never an identity proof). Null (the shell's plain contract call)
  /// resolves the platform default first — Android asks the existing
  /// `dev.ak/storage` channel (`deviceName`, MainActivity's Build.MODEL),
  /// desktop uses the local hostname — and every failure degrades to
  /// null, leaving the engine's own default ('ak-task',
  /// crates/ak-ffi/src/task/stack.rs) standing. An explicit name (tests,
  /// operator overrides) skips the resolution entirely.
  @override
  Future<void> start({String? deviceName}) async {
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
    // CR-P1-02 C2: the receiver-side STUN server for the reflexive
    // candidate. Same precedence as the registry pins (env > --dart-define),
    // defaulting to the production UDP STUN endpoint; an explicitly empty
    // value opts out (the Rust side fails open without a reflexive
    // candidate; ffi-v2.md §8.1).
    final stun = _envOrCompiled(env, 'AK_STUN_ADDR') ?? _defaultStunAddr;
    // X5 P1-3: the explicit name wins; else the platform default. The
    // DESKTOP answer resolves SYNCHRONOUSLY (see the resolver), so a
    // desktop start() still boots the engine in its synchronous prefix —
    // the fire-and-forget start pattern of the shell/tests keeps
    // working; only the Android channel round-trip awaits. P2-A: that
    // await carries a 3 s budget — a resolution that never completes
    // (a dead platform thread) degrades to null instead of hanging
    // start() forever. The `.then` re-types the future first: a
    // resolver's concrete Future<String>/Future<Null> must not
    // parametrize `timeout`'s T (the onTimeout answer is null, a String?).
    //
    // P2-F (fourth review): EVERY name is validated against the engine's
    // wire rules before it reaches the engine — the explicit param used
    // to skip [_presentableDeviceName] entirely, so an engine-rejected
    // name (66 raw bytes, control characters) broke create/join on the
    // registry. An invalid EXPLICIT name degrades to null (the engine
    // default stands; never truncates the caller's exact string, never
    // throws); a resolver's answer runs through the same byte-exact
    // mirror as the built-in platform defaults (trim / control-check /
    // truncate to the ceiling).
    final String? selfName;
    if (deviceName != null) {
      selfName = _explicitDeviceName(deviceName);
    } else {
      final resolved = _deviceNameResolver(_isAndroidPlatform);
      final String? raw = resolved is Future<String?>
          ? await resolved
              .then<String?>((name) => name)
              .timeout(deviceNameResolveTimeout, onTimeout: () => null)
          : resolved;
      selfName = _presentableDeviceName(raw);
    }
    await _engine.start(
      registry: address,
      registryIdHex: pin,
      idDir: dir,
      deviceName: selfName,
      saveDir: _saveDirOverride,
      stunAddr: stun.isEmpty ? null : stun,
    );
    // Remember each pending request's connection so the consent waiter
    // can watch the RIGHT receive's done event.
    _requestSub = _engine.incomingRequests.listen((request) {
      _connOfJob[request.jobId] = request.connId;
    });
    // W7.3: follow the transfers for the keep-screen-on flag from here
    // on (idempotent; the first event is the current snapshot).
    _screenKeepAwake.start();
  }

  @override
  void dispose() {
    _requestSub?.cancel();
    _requestSub = null;
    _screenKeepAwake.dispose(); // W7.3: release the screen flag first
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

  /// J2: pairing idle expiries pass through the engine service.
  @override
  Stream<String> get idleExpiries => _engine.idleExpiries;

  /// P2-E (fourth review): awaited-code TTL expiries pass through the
  /// engine service (the home code card's expiry fact).
  @override
  Stream<String> get codeExpired => _engine.codeExpired;

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
            windowSecs: request.windowSecs, // E2/G4: countdown's source
            connId: request.connId, // U1: this receive's progress key
            rootName: request.rootName, // X5: the payload's top label
            peerName: request.peerName, // X5: the sender's self-report
          ));

  /// Client UI U1: the engine service's folded progress, in the UI
  /// contract's vocabulary (an unknown phase name — a newer engine
  /// service — reads as `transferring`, the neutral live state).
  @override
  Stream<TransferProgress> get transferProgress =>
      _engine.transferProgress.map((progress) => TransferProgress(
            connId: progress.connId,
            phase: switch (progress.phase) {
              'connecting' => TransferPhase.connecting,
              'awaiting_consent' => TransferPhase.awaitingConsent,
              'verifying' => TransferPhase.verifying,
              'completed' => TransferPhase.completed,
              _ => TransferPhase.transferring,
            },
            totalFiles: progress.totalFiles,
            totalBytes: progress.totalBytes,
            completedFiles: progress.completedFiles,
            transferredBytes: progress.transferredBytes,
            currentFileName: progress.currentFile,
            currentFileIndex: progress.currentFileIndex,
            bytesPerSecond: progress.bytesPerSecond,
          ));

  @override
  Stream<String> get expiredIncomingRequests =>
      _engine.expiredIncomingRequests;

  @override
  Stream<EngineHealth> get engineHealth =>
      // R36-P2-03: forward the engine service's replayable health stream
      // (each getter call builds a fresh per-listen pipeline, so the
      // snapshot replay survives the mapping).
      _engine.engineHealth.map((health) => health.isEngineFailed
          ? EngineHealth.engineFailed(
              health.failureReason ?? '连续原生轮询失败。')
          : const EngineHealth.ok());

  @override
  Future<void> consent(String jobId, bool accept) async {
    final decided = await _engine.consent(jobId, accept: accept);
    if (!decided) {
      if (!accept) {
        // Wave-29 P2-11: a decline of an already-expired/auto-declined
        // request is IDEMPOTENT — the engine declined it (or the consent
        // window did); nothing was received either way.
        return;
      }
      throw StateError('该请求已失效（可能已被处理或超过 120 秒自动拒绝）。');
    }
    if (!accept) {
      return; // declined: nothing is received, resolve immediately
    }
    // Accepted: the contract completes when the receive finishes. Watch
    // the done event of THIS request's connection AND the connection
    // itself going terminal (a mid-transfer disconnect must not hang the
    // wait — P2-10). Wave-30 P2-10: [receiveTimeout] is a STALL budget,
    // not an overall cap — receive progress lines RENEW the deadline, so
    // only a true no-progress stall elapses.
    final conn = _connOfJob[jobId];
    // Wave-31 R30-P2-02: receive progress rows are keyed by the
    // CONNECTION id (the ffi-v2 §8.4 wave-31 revision; native lands the
    // job-key change in parallel): the receiver as JOINER has no local
    // code-id mapping, but every side of a connection knows its conn id
    // — ONE correlation key for both creation and join directions.
    final progressJobs = conn == null ? const <String>{} : <String>{conn};
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
  ///
  /// Wave-31 R30-P1-01: the wait may NOT outlive the service — the
  /// engine's dispose invalidates every call channel BEFORE the native
  /// owner releases the engine handle, so (a) both listens treat stream
  /// closure as settled (only dispose closes those streams), (b) every
  /// `await` is followed by a liveness gate BEFORE any FFI call, exactly
  /// like the op waiters' `ensureLive` hook, and (c) [drainProgress]
  /// itself refuses to poll a disposed engine.
  Future<void> _awaitReceiveSettled(
      String? conn, Set<String> progressJobs) async {
    var settled = false;
    Object? failure;
    final doneSub = _engine.doneEvents.listen((event) {
      if (conn == null || event.conn == conn) {
        settled = true;
      }
    }, onDone: () => settled = true); // stream closed = settled (dispose)
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
          }, onDone: () => settled = true); // stream closed = settled
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
        // R30-P1-01 (ensureLive uniformity): validate AFTER every await,
        // BEFORE any FFI call — a dispose that raced this tick must end
        // the wait here, never poll the (soon freed) native engine.
        if (_engine.isDisposed) {
          throw StateError('接收等待中止：任务服务已释放。');
        }
        // R36-P2-03: the engine-failed latch ends the receive wait
        // PROMPTLY with the SAME visible failure — never let it hang to
        // its own stall budget on an engine that stopped answering.
        if (_engine.isEngineFailed) {
          throw StateError(
              '接收等待中止：${_engine.failureReason ?? '传输引擎已故障（连续轮询失败）。'}');
        }
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
      // Settled via stream closure (onDone): the engine is disposed —
      // the wait ends NOW with the same honest verdict as the tick gate.
      if (_engine.isDisposed) {
        throw StateError('接收等待中止：任务服务已释放。');
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
    // U1: the direct path the engine actually used (task_send's `peer`),
    // as the UX's channel label — only when the address is parsable.
    final peer = report['peer'];
    final path = directPathLabel(peer is String ? peer : null);
    return '已发送 $files 个文件（${formatBytes(bytes)}）'
        '${path == null ? '' : ' · $path'}';
  }

  @override
  Future<void> disconnect(String connId) => _engine.disconnect(connId);

  // -- the one setting ----------------------------------------------------

  @override
  Future<void> setSaveDir(String path) => _engine.setSaveDir(path);

  @override
  String get saveDir => _engine.saveDir ?? '';

  /// G6b: delegate to the engine service's degrade-to-null op (the「关于」
  /// fact — null means "no information", never an error to the UI).
  @override
  Future<String?> serverRevision() => _engine.serverRevision();

  // -- internals ----------------------------------------------------------

  static Map<String, String> _platformEnvironment() => Platform.environment;

  /// Environment first, then the compiled `--dart-define` default (empty
  /// when the build passed none — the invite plane's precedence).
    /// Production default for [AkTaskStartSpec.stunAddr] (docs/STATUS: the
  /// coordination host's UDP STUN responder). Moves to a domain with
  /// CR-P3-03. Override via AK_STUN_ADDR; set it to an empty string to
  /// disable the reflexive candidate.
  static const String _defaultStunAddr = '122.51.121.24:7101';

  static String? _envOrCompiled(Map<String, String> env, String key) {
    final fromEnv = env[key]?.trim();
    if (fromEnv != null && fromEnv.isNotEmpty) {
      return fromEnv;
    }
    final compiled = switch (key) {
      'AK_REGISTRY_ADDR' => const String.fromEnvironment('AK_REGISTRY_ADDR'),
      'AK_STUN_ADDR' => const String.fromEnvironment('AK_STUN_ADDR'),
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

  /// The storage/platform channel MainActivity already owns
  /// (dev.ak/storage — its `deviceName` method is X5 P1-3's addition).
  static const String storageChannelName = 'dev.ak/storage';

  /// X5 P1-3: the default device-name resolution behind [start] — the
  /// name is a self-report for display, so EVERY failure degrades to
  /// null and the engine announces its own default ('ak-task'):
  ///
  /// * Android: the existing `dev.ak/storage` channel's `deviceName`
  ///   method (MainActivity — `Build.MODEL`, `Build.MANUFACTURER`
  ///   fallback, trimmed; a host activity without the method — an old
  ///   install — throws and degrades to null). P2-A: the round-trip
  ///   carries a 3 s budget — a dead platform thread degrades to null
  ///   instead of hanging [start] forever.
  /// * every other platform: `Platform.localHostname` when non-empty and
  ///   not 'localhost' — answered SYNCHRONOUSLY (a plain getter), so a
  ///   desktop [start] keeps booting the engine in its synchronous
  ///   prefix (the shell's fire-and-forget contract call). P2-A: the
  ///   getter can THROW on some hosts — its own try degrades to null
  ///   instead of throwing out of [start].
  /// * the result is validated against the ENGINE's wire rules (see
  ///   [_presentableDeviceName]) — display-only, never a fatal start.
  static FutureOr<String?> resolvePlatformDeviceName(bool isAndroid) {
    if (!isAndroid) {
      try {
        return _presentableDeviceName(Platform.localHostname);
      } on Object {
        return null; // localHostname failure — engine default
      }
    }
    return _androidDeviceName();
  }

  static Future<String?> _androidDeviceName() async {
    try {
      return _presentableDeviceName(await const MethodChannel(
              storageChannelName)
          .invokeMethod<String>('deviceName')
          .timeout(deviceNameResolveTimeout, onTimeout: () => null));
    } on Object {
      return null; // missing method / channel failure / timeout — engine default
    }
  }

  /// The wire-legal ceiling of a self-reported name (`device_name`): the
  /// registry client's `check_display_name` (crates/ak-session/src/
  /// pairing/client.rs — `MAX_NAME_BYTES`) — at most 64 UTF-8 BYTES and
  /// NO C0 control, DEL or C1 byte — is enforced engine-side on create
  /// AND join, so a name the engine would REJECT must never reach it
  /// (P1-C, third review: a platform's raw name broke both flows).
  ///
  /// Trims and validates the raw platform answer: empty/'localhost' →
  /// null; any rejected control byte → null (the engine default stands
  /// instead of a start-breaking name); an over-long name drops whole
  /// trailing characters until its UTF-8 form fits the byte ceiling
  /// (never a mid-rune cut); nothing presentable left → null.
  static const int _deviceNameMaxBytes = 64;

  static String? _presentableDeviceName(String? raw) {
    final name = (raw ?? '').trim();
    if (name.isEmpty || name == 'localhost') {
      return null;
    }
    final bytes = utf8.encode(name);
    for (final byte in bytes) {
      // The Rust side reads the same bytes: b < 0x20, DEL (0x7F), and
      // the C1 range as UTF-8 continuation bytes (0x80..=0x9F) are all
      // wire-illegal.
      if (byte < 0x20 || byte == 0x7F || (byte >= 0x80 && byte <= 0x9F)) {
        return null;
      }
    }
    if (bytes.length <= _deviceNameMaxBytes) {
      return name;
    }
    final kept = StringBuffer();
    var budget = _deviceNameMaxBytes;
    for (final rune in name.runes) {
      final char = String.fromCharCode(rune);
      final size = utf8.encode(char).length;
      if (size > budget) {
        break; // drop whole trailing characters — no mid-rune cut
      }
      budget -= size;
      kept.write(char);
    }
    final cut = kept.toString();
    return cut.isEmpty ? null : cut;
  }

  /// P2-F (fourth review): the EXPLICIT [start] name — the same engine
  /// wire rules as [_presentableDeviceName] (trim, empty/'localhost'
  /// refuse, control bytes refuse), but a name OVER the byte ceiling is
  /// refused ENTIRELY rather than truncated: an explicit name is the
  /// caller's exact string (a test identity, an operator override), and
  /// silently renaming it would hide the very thing the caller tried to
  /// announce. An invalid name degrades to null — the engine's own
  /// default ('ak-task') stands, [start] never throws on it.
  static String? _explicitDeviceName(String? raw) {
    final name = (raw ?? '').trim();
    if (utf8.encode(name).length > _deviceNameMaxBytes) {
      return null;
    }
    return _presentableDeviceName(name);
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
        // X5: the engine's self-reported peer name wins; an engine
        // predating the field (an old DLL) gets the spec-mandated
        // no-name default 「对方的设备」 (handoff §4.5 item 5 — never a
        // code-tail label).
        peerName: row.peerName ?? '对方的设备',
        role: _uiRole(row.role),
        state: _uiState(row.state),
        // J2: the idle-budget expiry keeps its own visible copy.
        idleExpired: row.disconnectReason == 'idle_expired',
        // X5: the verified fingerprint's short code (null on old engines).
        peerShortId: row.peerShortId,
        // The task code that produced this connection — the correlation
        // key shared by both sides of the pairing.
        code: row.code,
      );

  String _readable(AkFfiException error) {
    if (error.code == AkStatus.errInvalid) {
      return '任务码无效或已被使用，请确认后重试。';
    }
    return error.message;
  }
}
