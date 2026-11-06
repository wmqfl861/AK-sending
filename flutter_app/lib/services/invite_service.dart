/// [InviteService]: the UI-facing contract for wave-22 invite-code pairing
/// (create / redeem / list / revoke plus the live pairing-event stream) and
/// the locally persisted scheduler-mode preference.
///
/// Layering (mirrors the wave-11 `TransferService` pattern; the UI layer
/// never imports `dart:ffi`):
///
/// ```text
/// lib/ui/pages/invite_page.dart
///        ▲ reads via the TransferService.inviteService seam
/// lib/ui/service/transfer_service.dart (contract extension)
///        ▲ implemented by
/// lib/services/invite_service.dart (this file: InviteService +
///        FfiInviteService over the ak_ffi_dart pairing_ffi/sched_ffi seams)
/// ```
///
/// IMPORTANT: this file must never import `ui/service/transfer_service.dart`
/// — the UI contract imports THIS abstraction, and a reverse edge would
/// close an import cycle.
///
/// Wave-22 wiring status: the FFI paths are driven through the frozen
/// `ak_pairing_*`/`ak_sched_*` signatures (docs/contracts/ffi-v2.md). The
/// Rust exports land with the B8 agent; until the new DLL exists every
/// call of [FfiInviteService] fails at the binding-lookup step with the
/// explicit "old DLL unsupported" error (ffi-v2.md §2), which pages render
/// inline — never as a silent empty state. Widget tests inject
/// `test/fake_invite_service.dart`.
library;

import 'dart:async' show StreamController, TimeoutException, Timer;
import 'dart:convert' show jsonDecode, jsonEncode;
import 'dart:io' show File, Platform;

import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show
        AkFfiException,
        AkPairingCalls,
        AkPairingFfiBindings,
        AkPairingStartSpec,
        AkSchedCalls,
        AkSchedFfiBindings,
        InviteCreated,
        InviteListStateFilter,
        InviteSummary,
        PairingCompleteEvent,
        PairingEvent,
        PairingFfi,
        PairingOpHandle,
        PairingRuntimeFfi,
        InvitationRevokedEvent,
        SchedulerPolicy,
        SchedCodeMode,
        SchedRuntimeFfi,
        ServiceOfflineEvent,
        ServiceOnlineEvent,
        normalizeInviteCode;

import 'ak_engine_facade.dart' show AkEngineDartFacade;

/// The two TTL choices of the create form (pairing-flow.md §8: `ttl_secs=0`
/// is long-lived; finite TTLs span 60 s..30 d — the short-term preset is
/// one hour).
enum InviteTtlKind {
  /// Long-lived (`ttl_secs: 0`) — the default.
  longLived,

  /// Short-term preset: one hour (`ttl_secs: 3600`).
  shortLived;

  int get ttlSecs => this == InviteTtlKind.longLived ? 0 : 3600;
}

/// The user's preferred two-mode scheduler setting, persisted locally
/// under the identity directory (`scheduler_prefs.json` — local private
/// data only, never the coordination service). The runtime's EFFECTIVE
/// policy is the intersection of both ends (scheduler.md §2); this value
/// is what the user asked for.
final class SchedulerPreference {
  const SchedulerPreference({
    required this.sameCodeMode,
    required this.crossCodeMode,
  });

  final SchedCodeMode sameCodeMode;
  final SchedCodeMode crossCodeMode;

  /// The scheduler.md §3 default: serial × parallel.
  static const SchedulerPreference defaults =
      SchedulerPreference(sameCodeMode: SchedCodeMode.serial, crossCodeMode: SchedCodeMode.parallel);

  Map<String, dynamic> toJson() => <String, dynamic>{
        'same_code_mode': sameCodeMode.wireName,
        'cross_code_mode': crossCodeMode.wireName,
      };

  /// Tolerant decode: missing/unknown fields keep the defaults (a corrupt
  /// prefs file never blocks startup).
  factory SchedulerPreference.fromJson(Map<String, dynamic> json) =>
      SchedulerPreference(
        sameCodeMode: json['same_code_mode'] is String
            ? _modeOrDefault(json['same_code_mode'] as String)
            : SchedCodeMode.serial,
        crossCodeMode: json['cross_code_mode'] is String
            ? _modeOrDefault(json['cross_code_mode'] as String)
            : SchedCodeMode.parallel,
      );

  static SchedCodeMode _modeOrDefault(String wire) =>
      wire == 'parallel' ? SchedCodeMode.parallel : SchedCodeMode.serial;

  SchedulerPolicy toPolicy() => SchedulerPolicy(
      sameCodeMode: sameCodeMode, crossCodeMode: crossCodeMode);

  @override
  bool operator ==(Object other) =>
      other is SchedulerPreference &&
      other.sameCodeMode == sameCodeMode &&
      other.crossCodeMode == crossCodeMode;

  @override
  int get hashCode => Object.hash(sameCodeMode, crossCodeMode);

  @override
  String toString() =>
      'SchedulerPreference(${sameCodeMode.wireName} × ${crossCodeMode.wireName})';
}

/// The invite-code pairing service consumed by the invite page.
abstract interface class InviteService {
  /// Lists this device's invite registry rows. [includeTerminal] adds
  /// revoked/expired rows (CLI `pair list --invites [--all]`).
  Future<List<InviteSummary>> listInvites({bool includeTerminal = false});

  /// Creates and claims a new invite code; the plaintext code is shown
  /// exactly once via [InviteCreated.invitationCode] (bearer credential).
  Future<InviteCreated> createInvite(
      {InviteTtlKind ttl = InviteTtlKind.longLived});

  /// Redeems an invite code from ANOTHER device. Throws [FormatException]
  /// for a malformed code before anything is sent.
  Future<void> redeemInvite(String inviteCode);

  /// Revokes one of this device's codes by registry id (32 hex). The
  /// server-confirm / pending-sync distinction arrives as pairing events.
  Future<void> revokeInvite(String codeId);

  /// Live pairing events (`service_online`/`redeem_pending`/
  /// `pairing_complete`/...) — a broadcast stream backed by the service's
  /// own poll pump; listen any time.
  Stream<PairingEvent> get events;

  /// The persisted scheduler-mode preference (defaults when never saved or
  /// the file is unreadable — never throws).
  Future<SchedulerPreference> loadSchedulerPreference();

  /// Persists the scheduler-mode preference locally (propagates write
  /// errors so the page can show the honest reason).
  Future<void> saveSchedulerPreference(SchedulerPreference preference);

  /// Releases the poll pump. Idempotent.
  void dispose();
}

/// The `ak_pairing_start` runtime configuration (registry endpoint of the
/// coordination service + the pinned registry identity, pairing-flow.md
/// §3; `owner_loop` keeps the resident auto-pairing loop on).
///
/// [defaults] is the loopback TEST placeholder — the integrator wires the
/// real registry address/identity from app config before shipping.
final class PairingStartConfig {
  const PairingStartConfig({
    required this.registryAddress,
    required this.registryIdHex,
    this.deviceName,
    this.ownerLoop = true,
    this.redeemWindowMs = 90000,
  });

  /// Registry endpoint as `ip:port`.
  final String registryAddress;

  /// Pinned registry identity, 64 hex characters — the registry's raw
  /// 32-byte Ed25519 public key (pairing_ffi.dart validates this shape;
  /// wave-25 aligned it with the engine's `hex32` -> `[u8; 32]` parse).
  final String registryIdHex;

  /// Device name advertised to the registry (≤64 chars).
  final String? deviceName;

  /// Resident owner loop on/off (pairing-flow.md §4).
  final bool ownerLoop;

  /// Redeem window budget, milliseconds (1000..=119000).
  final int redeemWindowMs;

  /// Loopback TEST placeholder (integrator overrides with the real
  /// registry endpoint/identity). 64-hex placeholder: the registry pin
  /// is the raw 32-byte Ed25519 public key, so the placeholder must pass
  /// the same 64-hex validation the real pin goes through (wave-25 fix:
  /// the old 32-hex placeholder failed `ak_pairing_start` spec building
  /// the moment the binding's validation was aligned with the engine).
  static const PairingStartConfig defaults = PairingStartConfig(
    registryAddress: '127.0.0.1:7101',
    registryIdHex: '00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff',
  );
}

/// [InviteService] over the shared Rust runtime:
///
/// * lazily borrows the engine + library (via [AkEngineDartFacade] or
///   injected [AkPairingCalls]/[AkSchedCalls] fakes) and starts the
///   pairing runtime ONCE (`ak_pairing_start`; "accepted" ≠ online — the
///   `service_online` event is);
/// * ops are polled with the lossless `-3`-growth loop of
///   `PairingOpHandle.wait`;
/// * a [Timer] pump drains `ak_pairing_poll_event` into a broadcast
///   [Stream] (max [maxEventsPerTick] per tick so a huge backlog never
///   stalls the event loop);
/// * the scheduler-mode preference is a small JSON file under the
///   identity directory.
///
/// Threading: like every engine owner, one instance per isolate; calls are
/// serialized by the Dart event loop.
final class FfiInviteService implements InviteService {
  /// Creates the service with INJECTED call seams (tests, or an embedding
  /// that already owns the bindings); or a [callsProvider] that resolves
  /// the pair lazily (the `fromFacade` wiring). No DLL is touched until
  /// the first call.
  FfiInviteService({
    AkPairingCalls? pairingCalls,
    AkSchedCalls? schedCalls,
    (AkPairingCalls, AkSchedCalls)? Function()? callsProvider,
    String? Function()? identityDirProvider,
    PairingStartConfig? startConfig,
    this.pollInterval = const Duration(milliseconds: 250),
    this.opPollInterval = const Duration(milliseconds: 100),
    this.opTimeout = const Duration(seconds: 30),
    this.readyTimeout = const Duration(seconds: 15),
  })  : _calls = pairingCalls,
        _schedSeam = schedCalls,
        _seamProvider = callsProvider,
        _identityDirProvider = identityDirProvider ?? (() => null),
        _startConfig = startConfig ?? PairingStartConfig.defaults;

  /// Creates the service borrowing the engine of an existing
  /// [AkEngineDartFacade] (ffi-v2.md §2: borrow the handle, never dispose
  /// it here). A missing DLL or a pre-wave-22 library surfaces on the
  /// first call as the explicit unsupported error.
  factory FfiInviteService.fromFacade({
    required AkEngineDartFacade facade,
    String? Function()? identityDirProvider,
    PairingStartConfig? startConfig,
    Duration pollInterval = const Duration(milliseconds: 250),
  }) {
    (AkPairingCalls, AkSchedCalls)? cached;
    return FfiInviteService(
      identityDirProvider: identityDirProvider,
      startConfig: startConfig,
      pollInterval: pollInterval,
      callsProvider: () {
        final cachedPair = cached;
        if (cachedPair != null) {
          return cachedPair;
        }
        final engine = facade.borrowEngine();
        final pair = (
          AkPairingFfiBindings(
              engine: engine.nativeHandle, library: engine.bindings.library),
          AkSchedFfiBindings(
              engine: engine.nativeHandle, library: engine.bindings.library),
        );
        cached = pair;
        return pair;
      },
    );
  }

  /// How often the pairing-event pump ticks.
  final Duration pollInterval;

  /// How often an accepted op is asked for its result inside `wait`.
  final Duration opPollInterval;

  /// Wall-clock budget per op wait.
  final Duration opTimeout;

  /// Budget for the background registry connect (`runtime_ready`).
  final Duration readyTimeout;

  /// Queued pairing events one pump tick may drain (backlog bound).
  static const int maxEventsPerTick = 64;

  static const String _prefsFileName = 'scheduler_prefs.json';

  final AkPairingCalls? _calls;
  final AkSchedCalls? _schedSeam;
  final (AkPairingCalls, AkSchedCalls)? Function()? _seamProvider;
  final String? Function() _identityDirProvider;
  final PairingStartConfig _startConfig;

  PairingRuntimeFfi? _runtime;
  SchedRuntimeFfi? _sched;
  Timer? _pump;
  bool _starting = false;
  bool _disposed = false;

  /// P1-05: every async continuation re-checks this BEFORE any native
  /// call — a disposed service must never reach the (possibly freed)
  /// borrowed engine pointer with a stale poll.
  void _throwIfDisposed() {
    if (_disposed) {
      throw StateError('FfiInviteService is disposed');
    }
  }

  /// A `Future.delayed` whose continuation aborts when the service was
  /// disposed during the wait (P1-05: the check runs AFTER the await, so
  /// a dispose between two polls stops the NEXT native call).
  Future<void> _delayOrDisposed(Duration duration) async {
    await Future<void>.delayed(duration);
    _throwIfDisposed();
  }

  /// Waits one op with the dispose guard injected (P1-05):
  /// `PairingOpHandle.wait` calls [guard] at the TOP of every loop
  /// iteration — i.e. after EVERY await, BEFORE each native poll — so a
  /// dispose mid-wait surfaces as [StateError] here instead of a poll on
  /// a released engine.
  Future<Map<String, dynamic>> _waitOp(PairingOpHandle op) =>
      op.wait(
          interval: opPollInterval,
          timeout: opTimeout,
          ensureLive: _throwIfDisposed);

  /// In-memory preference fallback when no identity directory is known.
  SchedulerPreference? _memoryPreference;

  /// The last preference this process saw — the initial scheduler policy
  /// of the runtime at start.
  SchedulerPreference _lastKnownPreference = SchedulerPreference.defaults;

  final StreamController<PairingEvent> _events =
      StreamController<PairingEvent>.broadcast();

  /// Last pump/FFI error, for diagnostics cards (never thrown at
  /// listeners).
  Object? lastPumpError;

  @override
  Stream<PairingEvent> get events => _events.stream;

  // -------------------------------------------------------------------------
  // Runtime lifecycle.
  // -------------------------------------------------------------------------

  /// Starts (once) the pairing runtime, waits for its READY phase and
  /// binds the scheduler — then the event pump runs. Every public call
  /// awaits this; a failure surfaces with the explicit reason (DLL
  /// missing, wave-22 exports missing, engine refusal, registry
  /// unreachable). P1-05: EVERY await here is followed by a disposed
  /// re-check, so a dispose during startup aborts before the next native
  /// call (`ak_sched_start` included) instead of racing the release of
  /// the borrowed engine.
  Future<void> _ensureStarted() async {
    _throwIfDisposed();
    if (_runtime != null) {
      return;
    }
    while (_starting) {
      await _delayOrDisposed(const Duration(milliseconds: 5));
      if (_runtime != null) {
        return;
      }
    }
    _starting = true;
    try {
      var pairingCalls = _calls;
      var schedCalls = _schedSeam;
      final provider = _seamProvider;
      if (provider != null && (pairingCalls == null || schedCalls == null)) {
        final pair = provider();
        if (pair != null) {
          pairingCalls ??= pair.$1;
          schedCalls ??= pair.$2;
        }
      }
      if (pairingCalls == null || schedCalls == null) {
        throw StateError('配对运行时不可用：未提供 ak_pairing_*/ak_sched_* 绑定。');
      }
      final identityDir = _identityDirProvider() ?? '';
      if (identityDir.isEmpty) {
        throw StateError('配对运行时需要身份目录（identityDirProvider 未提供）。');
      }
      final config = _startConfig;
      final runtime = PairingFfi(calls: pairingCalls).start(AkPairingStartSpec(
        registry: config.registryAddress,
        registryIdHex: config.registryIdHex,
        idDir: identityDir,
        deviceName: config.deviceName,
        ownerLoop: config.ownerLoop,
        redeemWindowMs: config.redeemWindowMs,
      ));
      _sched = SchedRuntimeFfi(calls: schedCalls);
      // "Accepted" ≠ online (ffi-v2.md §2): drain the pairing queue until
      // the engine reports the READY phase before anything may bind to it
      // (ak_sched_start refuses a not-yet-Ready runtime).
      await _awaitReady(runtime);
      // P1-05: disposed while waiting for readiness — do NOT hand the
      // (possibly already released) engine to ak_sched_start.
      _throwIfDisposed();
      _sched!.start(_lastKnownPreference.toPolicy(),
          runtimeIdHex: runtime.runtimeIdHex);
      _runtime = runtime;
      _pump ??= Timer.periodic(pollInterval, (_) => _drainEvents());
    } finally {
      _starting = false;
    }
  }

  /// Polls the pairing event queue until `runtime_ready` (the
  /// [ServiceOnlineEvent]) or `runtime_failed`; every event seen on the
  /// way still lands on the stream. Throws the engine's failure reason on
  /// `runtime_failed` and a timeout error past [readyTimeout]. P1-05:
  /// each idle wait re-checks disposed on wake-up, so a dispose during
  /// the readiness gate stops the next `ak_pairing_poll_event` call.
  Future<void> _awaitReady(PairingRuntimeFfi runtime) async {
    final deadline = DateTime.now().add(readyTimeout);
    while (DateTime.now().isBefore(deadline)) {
      _throwIfDisposed();
      PairingEvent? event;
      try {
        event = runtime.pollEvent();
      } on AkFfiException catch (error) {
        throw StateError('配对运行时启动失败：$error');
      }
      if (event == null) {
        await _delayOrDisposed(opPollInterval);
        continue;
      }
      _events.add(event);
      if (event is ServiceOnlineEvent) {
        return;
      }
      if (event is ServiceOfflineEvent) {
        throw StateError('配对服务连接失败：${event.reason ?? '未知原因'}');
      }
    }
    throw TimeoutException('等待配对服务就绪超时', readyTimeout);
  }

  void _drainEvents() {
    final runtime = _runtime;
    if (runtime == null || _disposed) {
      return;
    }
    try {
      for (var i = 0; i < maxEventsPerTick; i++) {
        final event = runtime.pollEvent();
        if (event == null) {
          return;
        }
        _events.add(event);
      }
    } on Object catch (error) {
      // Engine gone / transient FFI failure: record, keep the pump alive.
      lastPumpError = error;
    }
  }

  // -------------------------------------------------------------------------
  // Invite operations.
  // -------------------------------------------------------------------------

  @override
  Future<List<InviteSummary>> listInvites(
      {bool includeTerminal = false}) async {
    await _ensureStarted();
    final op = _runtime!.listInvites(
        stateFilter: includeTerminal
            ? InviteListStateFilter.all
            : InviteListStateFilter.active);
    final map = await _waitOp(op);
    return InviteSummary.listFromJson(map);
  }

  @override
  Future<InviteCreated> createInvite(
      {InviteTtlKind ttl = InviteTtlKind.longLived}) async {
    await _ensureStarted();
    final op = _runtime!.createInvite(ttlSecs: ttl.ttlSecs);
    return InviteCreated.fromJson(await _waitOp(op));
  }

  @override
  Future<void> redeemInvite(String inviteCode) async {
    // Malformed codes are rejected locally before anything is sent
    // (pairing-flow.md §11: 格式错误在本地拒绝).
    final normalized = normalizeInviteCode(inviteCode);
    await _ensureStarted();
    final op = _runtime!.redeemInvite(normalized);
    final map = await _waitOp(op);
    _throwIfDisposed(); // P1-05: no stream writes past dispose
    // No pairing-queue wire event for a finished redeem this wave —
    // synthesize the timeline event from the op result (pairing_id +
    // verified peer are right there).
    final pairingId = map['pairing_id'];
    final peer = map['peer'];
    if (pairingId is String && peer is String && peer.isNotEmpty) {
      _events.add(PairingCompleteEvent(
        pairingId: pairingId,
        peer: peer,
        atMs: DateTime.now().millisecondsSinceEpoch,
      ));
    }
  }

  @override
  Future<void> revokeInvite(String codeId) async {
    await _ensureStarted();
    final op = _runtime!.revokeInvite(codeId);
    await _waitOp(op);
    _throwIfDisposed(); // P1-05: no stream writes past dispose
    // The op completing IS the server confirm (the engine waits for the
    // registry round-trip); surface the timeline event.
    _events.add(InvitationRevokedEvent(
      codeId: codeId,
      synced: true,
      atMs: DateTime.now().millisecondsSinceEpoch,
    ));
  }

  // -------------------------------------------------------------------------
  // Scheduler policy (shared runtime; confirm via policy_changed events).
  // -------------------------------------------------------------------------

  /// Applies [policy] through `ak_sched_set_policy` and returns the
  /// EFFECTIVE policy: the engine echoes it with a `policy_changed` event
  /// (the sched family has no generic op-result poll, ffi-v2.md §5); when
  /// no confirmation arrives within [confirmTimeout], the snapshot (or the
  /// submitted policy) is the honest fallback.
  /// Applies [policy] through `ak_sched_set_policy` (synchronous status
  /// answer this wave — no op polling) and returns the EFFECTIVE policy:
  /// the snapshot's `policy` object (both ends intersect, scheduler.md
  /// §2) when the engine reports one, else the submitted policy. The
  /// `policy_changed` event follows on the event stream.
  Future<SchedulerPolicy> applySchedulerPolicy(SchedulerPolicy policy) async {
    await _ensureStarted();
    final sched = _sched!;
    sched.setPolicy(policy);
    final snapshot = sched.snapshot();
    final raw = snapshot?['policy'];
    if (raw is Map<String, dynamic>) {
      return SchedulerPolicy.fromJson(raw);
    }
    return policy;
  }

  // -------------------------------------------------------------------------
  // Scheduler-mode preference persistence.
  // -------------------------------------------------------------------------

  File? _prefsFile() {
    final dir = _identityDirProvider();
    if (dir == null || dir.isEmpty) {
      return null;
    }
    final separator = Platform.isWindows ? r'\' : '/';
    final base = dir.endsWith(separator) ? dir : '$dir$separator';
    return File('$base$_prefsFileName');
  }

  @override
  Future<SchedulerPreference> loadSchedulerPreference() async {
    final file = _prefsFile();
    if (file == null) {
      return _memoryPreference ?? SchedulerPreference.defaults;
    }
    try {
      if (!await file.exists()) {
        return _memoryPreference ?? SchedulerPreference.defaults;
      }
      final decoded = jsonDecode(await file.readAsString());
      if (decoded is Map<String, dynamic>) {
        return _lastKnownPreference = SchedulerPreference.fromJson(decoded);
      }
      return SchedulerPreference.defaults;
    } on Object {
      // Corrupt/unreadable prefs never block startup.
      return SchedulerPreference.defaults;
    }
  }

  @override
  Future<void> saveSchedulerPreference(SchedulerPreference preference) async {
    final file = _prefsFile();
    if (file == null) {
      _memoryPreference = preference;
      _lastKnownPreference = preference;
      return;
    }
    _memoryPreference = preference;
    _lastKnownPreference = preference;
    await file.writeAsString(jsonEncode(preference.toJson()), flush: true);
  }

  /// Releases the service (P1-05 sequence): the `_disposed` flag goes up
  /// FIRST — from this instant every pending readiness/op waiter aborts
  /// at its next wake-up (the post-await guards + the injected
  /// `ensureLive` of `PairingOpHandle.wait`) BEFORE making another native
  /// call — then the event pump stops and the stream closes. The CALLER
  /// (`FfiTransferService.dispose`) releases the pairing runtime's
  /// borrowed engine only after this, keeping the native engine alive
  /// until every waiter has been fenced off.
  @override
  void dispose() {
    if (_disposed) {
      return;
    }
    _disposed = true;
    _pump?.cancel();
    _pump = null;
    _events.close();
  }
}
