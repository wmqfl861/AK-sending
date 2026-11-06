/// [SimpleTransferService]: the wave-28 v1 minimal-pairing service for the
/// UI layer (parallel agent D codes against this surface — the FROZEN Dart
/// API of the wave-28 hand-off).
///
/// The three actions of the frozen v1 product model:
///
/// 1. **生成任务码（选角色）** — [createCode] with a [SimpleTransferRole];
/// 2. **输码加入** — [join] with the displayed code;
/// 3. **连接列表 + 断开** — the [connections] stream + [disconnect].
///
/// After connecting, the sender sends ([sendFiles]) and the receiver
/// answers incoming requests ([incomingRequests] + [consent]); the ONE
/// default save folder is [setSaveDir]/[saveDir]. Zero IP/port/fingerprint
/// ever crosses this API.
///
/// ```text
/// lib/ui (agent D)  ──▶  simple_transfer_service.dart (THIS file)
///                              ▲
/// flutter_app/packages/ak_ffi_dart  (AkTaskCalls seam / AkTaskFfiBindings
///        ▲                        over ak_task_* / FakeAkTaskCalls)
///        └── crates/ak-ffi task plane (Rust)
/// ```
///
/// State machine (frozen + wave-29 P2-09 merge): `idle → creating |
/// joining → paired (probing) → connected`, with `failed(reason)` as the
/// error sink and `unreachable` as the probe-FAILED row verdict:
///
/// ```text
///        ┌────────── createCode ──────────┐
///        │            join                │
///      idle ────────► creating/joining ──► paired ──(probe reachable=true)──► connected
///        ▲                │                │  ▲      probe reachable=false → row `unreachable`
///        │                ▼                │  └──── last conn disconnected (data-plane reason)
///        └────────────────┴──── failed(reason) ◄── op/engine failures
/// ```
///
/// * `createCode`: idle→creating→paired (the creator reaches `connected`
///   when the peer's join lands and the probe passes).
/// * `join`: idle→joining→paired, then `probe_result` flips the row:
///   `reachable:true` → connected, `false` → unreachable, `null` keeps
///   probing (verdict pending).
/// * Row state merges are MONOTONIC (wave-29 P2-09): `probing < connected
///   = unreachable < disconnected`. A late `join` result never rebuilds a
///   row the event pump already advanced (nor revives a disconnected
///   one), and a late `connected` event never downgrades a row the probe
///   already settled.
/// * `disconnected` events only kill a row for the DATA-PLANE reasons
///   (`local` | `peer` | `receiver_failed`, wave-29 P2-08): a
///   coordination-plane notice (any other reason, e.g. a registry link
///   loss) leaves still-healthy direct connections untouched.
/// * disconnecting the last live connection returns to `idle`;
///   `failed(reason)` carries the engine's last-error detail.
///
/// Dispose follows the wave-26 P1-05 sequence: invalidate every waiter
/// FIRST (the `_disposed` flag gates all entries and the op `wait` loops
/// carry the `ensureLive` hook, so a dispose between two polls can never
/// reach the engine with a stale runtime id), then stop the pump and
/// close the streams. This service BORROWS the [AkTaskCalls] seam — the
/// native engine handle's release stays with its owner (the app shell's
/// facade), exactly like `FfiInviteService` borrows the facade's engine.
library;

import 'dart:async'
    show StreamController, StreamSubscription, Timer, TimeoutException;
import 'dart:collection' show UnmodifiableListView;

import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show
        AkFfiException,
        AkStatus,
        AkTaskCalls,
        AkTaskCreateCodeSpec,
        AkTaskJoinSpec,
        AkTaskRole,
        AkTaskSendSpec,
        AkTaskStartSpec,
        TaskConnectedEvent,
        TaskDisconnectedEvent,
        TaskDoneEvent,
        TaskEvent,
        TaskIncomingRequestEvent,
        TaskProbeResultEvent,
        TaskRuntimeFfi,
        TaskFfi;

/// The role picked when generating a task code.
enum SimpleTransferRole {
  /// This device will send files after the pairing.
  sender,

  /// This device will receive files (and answer incoming requests).
  receiver;

  AkTaskRole get _wire => this == SimpleTransferRole.sender
      ? AkTaskRole.sender
      : AkTaskRole.receiver;
}

/// The phases of the v1 pairing state machine.
enum SimpleTransferPhase {
  /// No runtime or no pairing activity yet.
  idle,

  /// A task-code creation op is in flight.
  creating,

  /// A join op is in flight.
  joining,

  /// Paired; reachability probing still pending (`probe_result` awaited).
  paired,

  /// At least one live connection.
  connected,

  /// A pairing-level failure occurred; [SimpleTransferService.failureReason]
  /// carries the detail. A new createCode/join leaves the sink.
  failed,
}

/// One connection as the UI renders it (the `ak_task_list` row shape).
final class SimpleConnection {
    const SimpleConnection({
      required this.connId,
      required this.role,
      required this.state,
      this.peerEndpoint,
      this.code,
    });

    /// Hex connection id (32 chars) — the send/disconnect key.
    final String connId;

    /// This device's role: `sender` | `receiver`.
    final String role;

    /// `probing` | `connected` | `unreachable` | `disconnected`. The two
    /// probe verdicts (`connected`/`unreachable`) are terminal for the
    /// probe and rank equally; `disconnected` is the overall terminal
    /// state (see the state-machine doc for the monotonic merge rules).
    final String state;

  final String? peerEndpoint;

  final String? code;

  @override
  String toString() =>
      'SimpleConnection($connId $role $state→$peerEndpoint)';
}

/// The result of a successful join.
final class SimpleJoinResult {
  const SimpleJoinResult({
    required this.connId,
    required this.role,
    this.peerEndpoint,
    this.probe = 'unverified',
  });

  final String connId;

  /// This device's role for the new connection.
  final String role;

  final String? peerEndpoint;

  /// `unverified` until the ak-session `probe_peer` slot lands
  /// (WAVE-28/B-PENDING).
  final String probe;
}

/// One incoming transfer request awaiting the user's accept/decline.
final class SimpleIncomingRequest {
  const SimpleIncomingRequest({
    required this.jobId,
    this.connId,
    required this.rootName,
    required this.files,
    required this.bytes,
  });

  /// The pending-request id — the [SimpleTransferService.consent] key.
  final String jobId;

  final String? connId;

  /// The sender's label for the payload.
  final String rootName;

  final int files;

  final int bytes;

  @override
  String toString() =>
      'SimpleIncomingRequest($jobId root=$rootName files=$files bytes=$bytes)';
}

/// The v1 minimal-pairing service. Construct with the FFI seam
/// (`AkTaskFfiBindings`, borrowed engine) or a [FakeAkTaskCalls] for
/// tests; ONE isolate owns the seam (calls serialize on the event loop).
final class SimpleTransferService {
  SimpleTransferService({
    required this.calls,
    this.eventPollInterval = const Duration(milliseconds: 50),
    this.opPollInterval = const Duration(milliseconds: 200),
    this.consentWindow = const Duration(seconds: 30),
    Future<void> Function(Duration)? delay,
  })  : _delay = delay ?? Future<void>.delayed;

  /// The task-plane call seam (FFI bindings or fake). Public read-only:
  /// the app shell wires `AkTaskFfiBindings` here; tests inject the fake.
  final AkTaskCalls calls;

  /// How often the event pump polls `ak_pairing_poll_event`.
  final Duration eventPollInterval;

  /// How often op waiters poll `ak_pairing_op_result`.
  final Duration opPollInterval;

  /// Injectable delay (tests drive the loops synchronously with a
  /// zero-delay stand-in).
  final Future<void> Function(Duration) _delay;

  TaskRuntimeFfi? _runtime;

  /// Connections by conn id (the list-stream's source of truth).
  final Map<String, SimpleConnection> _connections = <String, SimpleConnection>{};

  /// The current default save folder (null before [start] completed —
  /// the engine default is `<id_dir>/downloads`).
  String? _saveDir;

  SimpleTransferPhase _phase = SimpleTransferPhase.idle;
  String? _failureReason;

  Timer? _pump;
  // The live connection-change fan-out. Per-subscriber SNAPSHOT replay is
  // implemented by [connections] (wave-29 P1-06): a broadcast controller's
  // onListen only fires on the 0→1 listener transition, which is why this
  // controller itself carries NO onListen — every new subscriber gets a
  // dedicated wrapper stream that first yields the current snapshot and
  // then forwards this controller's events.
  final StreamController<List<SimpleConnection>> _connectionsController =
      StreamController<List<SimpleConnection>>.broadcast();
  final StreamController<SimpleIncomingRequest> _incomingController =
      StreamController<SimpleIncomingRequest>.broadcast();
  final StreamController<TaskDoneEvent> _doneController =
      StreamController<TaskDoneEvent>.broadcast();

  /// Wave-29 P2-11: the 30 s engine auto-decline window, mirrored
  /// locally per pending request so the UI can close banners the engine
  /// already auto-declined. A timer fires only when the request is still
  /// undecided here; [consent] cancels its timer.
  final Map<String, Timer> _consentTimers = <String, Timer>{};
  final StreamController<String> _expiredIncomingController =
      StreamController<String>.broadcast();

  /// Engine-side auto-decline window (tests may shorten construction —
  /// the fake engine never auto-declines on its own).
  final Duration consentWindow;

  bool _disposed = false;

  // -- lifecycle ---------------------------------------------------------

  /// `ak_task_start`: boots the task runtime (registry config comes from
  /// the caller — the app resolves `AK_REGISTRY_ADDR`/`AK_REGISTRY_ID`
  /// exactly like the invite plane). Idempotent: a second call on a live
  /// service is a no-op.
  Future<void> start({
    required String registry,
    required String registryIdHex,
    required String idDir,
    String? deviceName,
    String? saveDir,
  }) async {
    _ensureLive();
    if (_runtime != null) {
      return; // idempotent: one task runtime per service
    }
    final runtime = TaskFfi(calls: calls).start(AkTaskStartSpec(
      registry: registry,
      registryIdHex: registryIdHex,
      idDir: idDir,
      deviceName: deviceName,
      saveDir: saveDir,
    ));
    _runtime = runtime;
    _saveDir = saveDir;
    _setPhase(SimpleTransferPhase.idle);
    _pump ??= Timer.periodic(eventPollInterval, (_) => _pumpOnce());
    // Drain any events that queued before the pump armed, then learn the
    // engine's effective save folder (`<id_dir>/downloads` default).
    _pumpOnce();
    try {
      _saveDir ??= runtime.list().saveDir;
    } on Object {
      // transient snapshot failure — the next setSaveDir/refresh wins
    }
  }

  /// Drains the task event queue ONCE, synchronously (the pump's tick as a
  /// public hook: tests drive the loops deterministically and pages can
  /// refresh eagerly after user actions).
  void pumpEvents() => _pumpOnce();

  /// P1-05-aligned dispose: (1) invalidate every waiter and channel (the
  /// disposed flag gates all entries; in-flight op `wait` loops abort at
  /// their next tick through `ensureLive`), (2) stop the pump, (3) close
  /// the streams. The native engine handle is NOT released here — this
  /// service borrows the seam; the owner releases it after every borrower
  /// is disposed (the `FfiInviteService` pattern).
  void dispose() {
    if (_disposed) {
      return;
    }
    // (1) waiters / call channels.
    _disposed = true;
    // (2) the pump.
    _pump?.cancel();
    _pump = null;
    // Consent-window timers (P2-11) go with the channels.
    for (final timer in _consentTimers.values) {
      timer.cancel();
    }
    _consentTimers.clear();
    // (3) streams (listeners get a done event; late pollers throw nothing).
    _connectionsController.close();
    _incomingController.close();
    _doneController.close();
    _expiredIncomingController.close();
  }

  // -- the three actions --------------------------------------------------

  /// Action 1 — generate a task code for [role]. Returns the DISPLAY code
  /// (`AKI1-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX`, the Rust
  /// `InviteCode::to_display` shape) once the create op completed
  /// (receiver-role creations bind the resident receiver first,
  /// engine-side).
  Future<String> createCode(
    SimpleTransferRole role, {
    List<String>? candidates,
    Duration timeout = const Duration(seconds: 30),
  }) async {
    final runtime = _runtimeOrThrow();
    _setPhase(SimpleTransferPhase.creating);
    try {
      final result = await runtime
          .createCode(AkTaskCreateCodeSpec(
            role: role._wire,
            candidates: candidates ?? const <String>[],
          ))
          .wait(
            interval: opPollInterval,
            timeout: timeout,
            delay: _delay,
            ensureLive: _ensureLive,
          );
      _setPhase(SimpleTransferPhase.paired);
      return result['code'] as String;
    } on Object catch (error) {
      _fail(error);
      rethrow;
    }
  }

  /// Action 2 — join by entering the displayed code. [role] is the
  /// joiner's EXPLICIT direction (the UI user's intent, wave-29 P1-05):
  /// it rides the join spec so cross-device joins never guess the
  /// direction; null keeps the engine's resolution (same-engine mirror
  /// complement, else the v1 receiver default).
  ///
  /// The result row merges MONOTONICALLY (P2-09): when the event pump
  /// already advanced the row (probe verdict) or even killed it
  /// (`disconnected`), the late join result only fills role/endpoint/code
  /// — it never rebuilds the row to `probing` nor regresses the phase.
  Future<SimpleJoinResult> join(
    String code, {
    SimpleTransferRole? role,
    List<String>? candidates,
    Duration timeout = const Duration(seconds: 30),
  }) async {
    final runtime = _runtimeOrThrow();
    _setPhase(SimpleTransferPhase.joining);
    try {
      final result = await runtime.join(AkTaskJoinSpec(
        code: code,
        role: role?._wire,
        candidates: candidates ?? const <String>[],
      )).wait(
        interval: opPollInterval,
        timeout: timeout,
        delay: _delay,
        ensureLive: _ensureLive,
      );
      final connId = result['conn'] as String;
      _upsert(
        connId: connId,
        role: result['role'] as String?,
        peerEndpoint: result['peer_endpoint'] as String?,
        code: code.trim().toUpperCase(),
        state: 'probing',
      );
      // Only the joining→paired step regresses nothing; if events already
      // moved the phase (connected) or the pairing died (idle/failed),
      // the late result keeps the phase as is (P2-09).
      if (_phase == SimpleTransferPhase.joining) {
        _setPhase(SimpleTransferPhase.paired);
      }
      return SimpleJoinResult(
        connId: connId,
        role: result['role'] as String? ?? '',
        peerEndpoint: result['peer_endpoint'] as String?,
        probe: result['probe'] as String? ?? 'unverified',
      );
    } on Object catch (error) {
      _fail(error);
      rethrow;
    }
  }

  /// Action 3a — the live connection list. Wave-29 P1-06 semantics: EVERY
  /// subscription (each call of this getter returns an independent
  /// stream) first receives the CURRENT snapshot, then the live change
  /// events — a detail page opened while the home page is already
  /// listening still sees the live list immediately, with no gap between
  /// the snapshot and the forwarded events (the live tap is attached
  /// synchronously before the snapshot is emitted). The same snapshot is
  /// also readable synchronously via [currentConnections].
  Stream<List<SimpleConnection>> get connections {
    final out = StreamController<List<SimpleConnection>>.broadcast();
    StreamSubscription<List<SimpleConnection>>? live;
    out.onListen = () {
      live = _connectionsController.stream.listen(
        out.add,
        onError: out.addError,
        onDone: out.close,
      );
      out.add(currentConnections); // the per-subscriber snapshot replay
    };
    out.onCancel = () {
      live?.cancel();
      live = null;
    };
    return out.stream;
  }

  /// Action 3b — sever one connection (idempotent; the peer side learns
  /// engine-side).
  Future<void> disconnect(String connId) async {
    final runtime = _runtimeOrThrow();
    _ensureLive();
    try {
      runtime.disconnect(connId);
    } on AkFfiException catch (error) {
      if (error.code == AkStatus.errNotFound) {
        // Already gone: fold the local view anyway (the event may have
        // raced the call).
        _markDisconnected(connId);
        return;
      }
      _fail(error);
      rethrow;
    }
  }

  // -- transfers ----------------------------------------------------------

  /// The sender picks files and sends them over [connId]. Resolves with
  /// the engine's report map (`files`/`bytes`/`peer`); the offer waits
  /// receiver-side for the 30 s consent window engine-side.
  ///
  /// Wave-29 P2-10: when [timeout] elapses the UI wait does NOT simply
  /// give up while the native task keeps writing — the only FFI cancel
  /// path of the v1 one-task model (`ak_task_disconnect` of the task
  /// connection) is issued best-effort and the rethrown
  /// [TimeoutException] says so. The disconnect event then folds the row
  /// to `disconnected` through the normal pump.
  Future<Map<String, dynamic>> sendFiles(
    String connId,
    List<String> paths, {
    Duration timeout = const Duration(seconds: 120),
  }) async {
    final runtime = _runtimeOrThrow();
    _ensureLive();
    try {
      return await runtime.send(connId, AkTaskSendSpec(paths: paths)).wait(
            interval: opPollInterval,
            timeout: timeout,
            delay: _delay,
            ensureLive: _ensureLive,
          );
    } on TimeoutException {
      await _cancelTaskConnection(runtime, connId);
      throw TimeoutException(
        '发送等待超时（${timeout.inSeconds} 秒），已取消原生发送任务并断开该连接。',
        timeout,
      );
    }
  }

  /// The FFI cancel path (P2-10): disconnect the task connection and fold
  /// the local row. Best-effort by design — the op may complete on its
  /// own in the race window, in which case the event pump owns the final
  /// state and a disconnect error is deliberately swallowed.
  Future<void> _cancelTaskConnection(TaskRuntimeFfi runtime, String connId) async {
    try {
      runtime.disconnect(connId);
    } on Object {
      // Raced the op's own completion/teardown — nothing else to do.
      return;
    }
    _markDisconnected(connId);
  }

  /// Incoming transfer requests (receiver side) — one event per offer,
  /// waiting for [consent]; the engine auto-declines after 30 s.
  Stream<SimpleIncomingRequest> get incomingRequests =>
      _incomingController.stream;

  /// Wave-29 P2-11: job ids whose consent window closed undecided — the
  /// engine auto-declined them, so the UI closes/removes their banners.
  /// Firing is driven by the local mirror of the 30 s window (started at
  /// event-pump time, cancelled by [consent]); a decline click that
  /// races the window resolves as false from [consent] and is handled
  /// idempotently by the adapter.
  Stream<String> get expiredIncomingRequests =>
      _expiredIncomingController.stream;

  /// Successful transfer completions (either side) with files/bytes.
  Stream<TaskDoneEvent> get doneEvents => _doneController.stream;

  /// Decide one pending [SimpleIncomingRequest]. Returns false when the
  /// request is unknown or already decided (including auto-declined by
  /// the 30 s window — deciding such a request locally is a no-op).
  Future<bool> consent(String jobId, {required bool accept}) async {
    final runtime = _runtimeOrThrow();
    _ensureLive();
    _consentTimers.remove(jobId)?.cancel();
    return runtime.consent(jobId, accept: accept);
  }

  // -- the one setting ----------------------------------------------------

  /// The ONE default save folder (the engine creates it eagerly; receiver
  /// role files land there).
  Future<void> setSaveDir(String path) async {
    final runtime = _runtimeOrThrow();
    _ensureLive();
    runtime.setSaveDir(path);
    _saveDir = path;
  }

  /// The current default save folder (the engine default
  /// `<id_dir>/downloads` is learned at [start]; [setSaveDir] updates it).
  String? get saveDir => _saveDir;

  // -- state --------------------------------------------------------------

  /// The pairing phase (the state machine above).
  SimpleTransferPhase get phase => _phase;

  /// The failure detail when [phase] is [SimpleTransferPhase.failed].
  String? get failureReason => _failureReason;

  /// The synchronous connection snapshot.
  List<SimpleConnection> get currentConnections =>
      UnmodifiableListView<SimpleConnection>(_connections.values.toList());

  bool get isDisposed => _disposed;

  // -- internals ----------------------------------------------------------

  void _ensureLive() {
    if (_disposed) {
      throw StateError('SimpleTransferService 已释放。');
    }
  }

  TaskRuntimeFfi _runtimeOrThrow() {
    _ensureLive();
    final runtime = _runtime;
    if (runtime == null) {
      throw StateError('任务运行时尚未启动：请先调用 start()。');
    }
    return runtime;
  }

  void _setPhase(SimpleTransferPhase next) {
    if (_phase == next) {
      return;
    }
    _phase = next;
    if (next != SimpleTransferPhase.failed) {
      _failureReason = null;
    }
  }

  void _fail(Object error) {
    _failureReason = error is AkFfiException
        ? error.message
        : (error is TimeoutException ? error.message : error.toString());
    _setPhase(SimpleTransferPhase.failed);
  }

  void _notifyConnections() {
    if (!_connectionsController.isClosed) {
      _connectionsController.add(currentConnections);
    }
  }

  /// The monotonic state order of the P2-09 merge: probing < the probe
  /// verdicts (connected = unreachable) < disconnected. A rank-equal
  /// update overwrites (there is exactly one probe verdict per join).
  static int _stateRank(String state) => switch (state) {
        'probing' => 0,
        'connected' || 'unreachable' => 1,
        _ => 2, // disconnected and anything unknown stay terminal
      };

  /// Monotonic row upsert (P2-09): fills the optional fields, never
  /// downgrades an existing state, notifies on change, and advances the
  /// global phase to `connected` when a row's probe PASSED.
  void _upsert({
    required String connId,
    required String state,
    String? role,
    String? peerEndpoint,
    String? code,
  }) {
    final existing = _connections[connId];
    if (existing == null) {
      _connections[connId] = SimpleConnection(
        connId: connId,
        role: role ?? '',
        state: state,
        peerEndpoint: peerEndpoint,
        code: code,
      );
      _notifyConnections();
    } else {
      final merged = _stateRank(state) >= _stateRank(existing.state)
          ? state
          : existing.state;
      if (merged == existing.state &&
          (role == null || role == existing.role) &&
          (peerEndpoint == null || peerEndpoint == existing.peerEndpoint) &&
          (code == null || code == existing.code)) {
        return; // nothing new — no notification
      }
      _connections[connId] = SimpleConnection(
        connId: connId,
        role: role ?? existing.role,
        state: merged,
        peerEndpoint: peerEndpoint ?? existing.peerEndpoint,
        code: code ?? existing.code,
      );
      _notifyConnections();
    }
    if (_connections[connId]?.state == 'connected' &&
        (_phase == SimpleTransferPhase.paired ||
            _phase == SimpleTransferPhase.joining)) {
      _setPhase(SimpleTransferPhase.connected);
    }
  }

  /// The `disconnected` reasons that are DATA-PLANE terminal verdicts
  /// (wave-29 P2-08). Anything else (e.g. a registry/coordinator link
  /// loss surfaced by the engine) is a coordination-plane notice: the
  /// authorized direct connection may still be healthy, so the row keeps
  /// its state until a real data-plane close arrives.
  static const Set<String> _terminalDisconnectReasons = <String>{
    'local',
    'peer',
    'receiver_failed',
  };

  void _markDisconnected(String connId) {
    final existing = _connections[connId];
    if (existing == null) {
      return;
    }
    _connections[connId] = SimpleConnection(
      connId: connId,
      role: existing.role,
      state: 'disconnected',
      peerEndpoint: existing.peerEndpoint,
      code: existing.code,
    );
    _notifyConnections();
    if (!_connections.values.any((conn) => conn.state != 'disconnected')) {
      _setPhase(SimpleTransferPhase.idle);
    }
  }

  /// One pump tick: drain the runtime's event queue and route by type.
  void _pumpOnce() {
    if (_disposed) {
      return;
    }
    final runtime = _runtime;
    if (runtime == null) {
      return;
    }
    while (true) {
      final TaskEvent? event;
      try {
        event = runtime.pollEvent();
      } on AkFfiException {
        return; // transient poll failure — the next tick retries
      }
      if (event == null) {
        return;
      }
      _route(event);
    }
  }

  void _route(TaskEvent event) {
    if (event is TaskConnectedEvent) {
      // The engine announces the CONNECTION (pairing bookkeeping) before
      // probing: a fresh row starts at `probing`; a row the pump already
      // settled (probe verdict / disconnected) keeps its state (P2-09).
      _upsert(
        connId: event.conn,
        role: event.role,
        peerEndpoint: event.peerEndpoint,
        code: event.code,
        state: 'probing',
      );
      return;
    }
    if (event is TaskProbeResultEvent) {
      // The probe's TRUTH drives the verdict (P2-09): reachable=true →
      // connected, false → unreachable (never "success"), null → the
      // verdict is still pending, keep probing.
      final existing = _connections[event.conn];
      if (existing != null) {
        _upsert(
          connId: event.conn,
          peerEndpoint: event.endpoint,
          state: switch (event.reachable) {
            true => 'connected',
            false => 'unreachable',
            null => existing.state,
          },
        );
      }
      return;
    }
    if (event is TaskIncomingRequestEvent) {
      if (!_incomingController.isClosed) {
        _incomingController.add(SimpleIncomingRequest(
          jobId: event.job,
          connId: event.conn,
          rootName: event.rootName,
          files: event.files,
          bytes: event.bytes,
        ));
      }
      // Mirror the engine's 30 s auto-decline window (P2-11): when it
      // closes with the request still undecided here, tell the UI so the
      // banner goes away instead of clicking into a dead request.
      if (!_expiredIncomingController.isClosed &&
          !_consentTimers.containsKey(event.job)) {
        _consentTimers[event.job] = Timer(consentWindow, () {
          _consentTimers.remove(event.job);
          if (!_disposed && !_expiredIncomingController.isClosed) {
            _expiredIncomingController.add(event.job);
          }
        });
      }
      return;
    }
    if (event is TaskDoneEvent) {
      if (!_doneController.isClosed) {
        _doneController.add(event);
      }
      return;
    }
    if (event is TaskDisconnectedEvent) {
      final conn = event.conn;
      if (conn == null || conn.isEmpty) {
        return;
      }
      if (!_terminalDisconnectReasons.contains(event.reason)) {
        // Coordination-plane notice (P2-08): the data channel is
        // independent — a still-healthy direct connection stays live.
        return;
      }
      _markDisconnected(conn);
      return;
    }
    // runtime_ready / op_done / op_failed / unknown: the op waiters read
    // their own results; nothing to fold here.
  }
}
