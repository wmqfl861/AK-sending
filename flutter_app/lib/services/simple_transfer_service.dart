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
///   = unreachable < terminal`. A late `join` result never rebuilds a
///   row the event pump already advanced (nor revives a terminal one),
///   and a late `connected` event never downgrades a row the probe
///   already settled.
/// * `disconnected` events only kill a row for the DATA-PLANE reasons
///   (`local` | `peer` | `receiver_failed` | `done`, wave-29 P2-08 +
///   wave-30 P2-05): a coordination-plane notice (any other reason, e.g.
///   a registry link loss) leaves still-healthy direct connections
///   untouched. `done` marks the one-task transfer COMPLETED — the row
///   converges to a completed state that no longer offers sends (the
///   `done` EVENT converges the row the same way).
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

import 'dart:async' show StreamController, Timer, TimeoutException;
import 'dart:collection' show UnmodifiableListView;
import 'dart:convert' show jsonDecode;

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
        TaskConsentExpiredEvent,
        TaskDisconnectedEvent,
        TaskDoneEvent,
        TaskEvent,
        TaskIncomingRequestEvent,
        TaskProbeResultEvent,
        TaskRequestClosedEvent,
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
    this.saveDir,
  });

  /// The pending-request id — the [SimpleTransferService.consent] key.
  final String jobId;

  final String? connId;

  /// The sender's label for the payload.
  final String rootName;

  final int files;

  final int bytes;

  /// The save directory THIS request will use, snapshotted native-side at
  /// offer time (wave-30 P2-06, the event's `saveDir` field): the files
  /// land there even when the settings change before the user answers,
  /// so the UI shows this value as the request's real target. Null = an
  /// engine predating the field (fall back to the live [saveDir]).
  final String? saveDir;

  @override
  String toString() =>
      'SimpleIncomingRequest($jobId root=$rootName files=$files bytes=$bytes '
      'saveDir=$saveDir)';
}

/// The result of a successful task-code creation (wave-30 P2-04): the
/// DISPLAY code plus the engine's `code_id` — the cancel key. Calling
/// [SimpleTransferService.disconnect] with [codeId] while the code is
/// still un-joined revokes the registry row (`ak_task_disconnect` covers
/// code ids per ffi-v2 §8.4).
final class SimpleCreatedCode {
  const SimpleCreatedCode({
    required this.code,
    required this.role,
    this.codeId,
  });

  /// The display code (`AKI1-XXXX-…`, the Rust `InviteCode::to_display`).
  final String code;

  /// The creating device's role for this code.
  final SimpleTransferRole role;

  /// The engine's code id (32 hex) — null only from an engine predating
  /// the field.
  final String? codeId;
}

/// The v1 minimal-pairing service. Construct with the FFI seam
/// (`AkTaskFfiBindings`, borrowed engine) or a [FakeAkTaskCalls] for
/// tests; ONE isolate owns the seam (calls serialize on the event loop).
final class SimpleTransferService {
  SimpleTransferService({
    required this.calls,
    this.eventPollInterval = const Duration(milliseconds: 50),
    this.opPollInterval = const Duration(milliseconds: 200),
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
  // implemented by [connections] (wave-29 P1-06, wave-30 completed): a
  // broadcast controller's onListen only fires on the 0→1 listener
  // transition, which is why this controller itself carries NO onListen —
  // the getter's Stream.multi runs its per-LISTEN setup for EVERY
  // subscriber, even two listeners on the SAME saved stream object.
  final StreamController<List<SimpleConnection>> _connectionsController =
      StreamController<List<SimpleConnection>>.broadcast();
  final StreamController<SimpleIncomingRequest> _incomingController =
      StreamController<SimpleIncomingRequest>.broadcast();
  final StreamController<TaskDoneEvent> _doneController =
      StreamController<TaskDoneEvent>.broadcast();
  final StreamController<String> _expiredIncomingController =
      StreamController<String>.broadcast();

  /// Wave-30 P2-04: the code ids this runtime's pairings used, by
  /// normalized code key — the cancel keys for un-joined codes. Both
  /// sides of a pairing fill it since wave-31 (R30-P2-02): the creator
  /// at mint time, the joiner from the join result / `connected` event's
  /// `code_id` field. (Task-plane progress rows themselves are keyed by
  /// the CONNECTION id since the wave-31 ffi-v2 §8.4 revision — this
  /// table stays the pairing correlation, not the progress key.)
  final Map<String, String> _codeIdOfKey = <String, String>{};

  /// Wave-30 P2-10: monotonic per-job progress ticks fed by the
  /// ENGINE-WIDE `ak_poll_progress` queue (drained by the pump and by the
  /// send/receive stall loops). A tick advancing means "bytes moved since
  /// the last look" — the deadline-renewal fact.
  final Map<String, int> _progressTicks = <String, int>{};

  /// Wave-30 P2-11: pending (undecided) incoming requests — job id → the
  /// event's conn id. Drained by [consent] (decided), the native
  /// `consent_expired` event, and the request's connection going
  /// data-plane terminal (the engine rejects the pending offer with the
  /// connection; the banner must close, not wait for an event that will
  /// never come).
  final Map<String, String?> _pendingJobs = <String, String?>{};

  /// Wave-31 R30-P2-03: jobs whose request became TERMINAL undecided —
  /// the bounded record that refuses a LATE `incoming_request` (the
  /// native register/publish race: the request was closed before its
  /// announcement could be pumped). FIFO-bounded like every native
  /// cache; a native job id is minted per registration, so a bounded
  /// recent record is exact for the race it guards.
  final Map<String, String?> _closedJobs = <String, String?>{};

  /// The bound of [_closedJobs] (the endpoint-cache pattern).
  static const int _closedJobsCap = 256;

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
    // (3) streams (listeners get a done event; late pollers throw nothing).
    _connectionsController.close();
    _incomingController.close();
    _doneController.close();
    _expiredIncomingController.close();
  }

  // -- the three actions --------------------------------------------------

  /// Action 1 — generate a task code for [role]. Returns the DISPLAY code
  /// (`AKI1-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX`, the Rust
  /// `InviteCode::to_display` shape) plus the engine's `code_id` (wave-30
  /// P2-04: [disconnect] with that id cancels the UN-JOINED code — the
  /// registry row is revoked, so a cancelled code can never be redeemed)
  /// once the create op completed (receiver-role creations bind the
  /// resident receiver first, engine-side).
  Future<SimpleCreatedCode> createCode(
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
      final code = result['code'] as String;
      final codeId = result['code_id'] as String?;
      if (codeId != null && codeId.isNotEmpty) {
        _codeIdOfKey[_codeKey(code)] = codeId;
      }
      return SimpleCreatedCode(code: code, role: role, codeId: codeId);
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
      // Wave-31 R30-P2-02: the join result carries the pairing's
      // `code_id` (native lands the field in parallel) — a JOINER learns
      // here the same correlation key the creator minted, so both sides
      // share one key for the pairing's facts.
      final joinedCodeId = result['code_id'] as String?;
      if (joinedCodeId != null && joinedCodeId.isNotEmpty) {
        _codeIdOfKey[_codeKey(code)] = joinedCodeId;
      }
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

  /// Action 3a — the live connection list. Wave-30 P1-06 completion:
  /// EVERY LISTEN — not every getter call — first receives the CURRENT
  /// snapshot, then the live change events. The getter returns a
  /// `Stream.multi` view whose per-listen setup attaches the live tap to
  /// the fan-out controller FIRST (no gap: an event added after the tap
  /// can only be delivered after the already-enqueued snapshot) and then
  /// enqueues the snapshot, so even two subscribers of the SAME saved
  /// stream object each get their own snapshot+live pair. The snapshot
  /// is also readable synchronously via [currentConnections].
  Stream<List<SimpleConnection>> get connections =>
      Stream.multi((controller) {
        // Attach the live forwarding FIRST — a source event added after
        // this point queues behind the snapshot below (the controller
        // delivers FIFO).
        final live = _connectionsController.stream.listen(
          controller.add,
          onError: controller.addError,
          onDone: controller.close,
        );
        // The per-LISTEN snapshot replay (the reason Stream.multi, not a
        // broadcast wrapper: onListen of a broadcast wrapper fires only
        // on the 0→1 transition, leaving a second listener snapshot-less
        // — the wave-29 residual this closes).
        controller.add(currentConnections);
        // Cancel the live tap when THIS listener goes away.
        controller.onCancel = () => live.cancel();
      });

  /// Action 3b — sever one connection (idempotent; the peer side learns
  /// engine-side). Wave-30 P2-04: [connId] may ALSO be a task code's
  /// `code_id` ([SimpleCreatedCode.codeId]) — disconnecting an UN-JOINED
  /// code cancels it: the registry row is revoked (`code_cancelled`
  /// event) and the code can never be redeemed; a code id unknown to the
  /// engine (`-4`, e.g. already joined or expired) folds away benignly.
  Future<void> disconnect(String connId) async {
    final runtime = _runtimeOrThrow();
    _ensureLive();
    try {
      runtime.disconnect(connId);
    } on AkFfiException catch (error) {
      if (error.code == AkStatus.errNotFound) {
        // Already gone: fold the local view anyway (the event may have
        // raced the call; an unknown code id means the code is no longer
        // redeemable either way).
        _markTerminal(connId, 'disconnected');
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
  /// Wave-30 P2-10: [timeout] is a STALL budget, not an overall cap —
  /// every `ak_poll_progress` line keyed by this connection (the send
  /// row's job id) RENEWS the deadline, so a long transfer that keeps
  /// making progress is never cut off; only a true no-progress stall of
  /// [timeout] elapses. On stall the only FFI cancel path of the v1
  /// one-task model (`ak_task_disconnect` of the task connection) is
  /// issued, and the rethrown [TimeoutException] reports HONESTLY: when
  /// the cancel itself fails, the message says the transfer may still be
  /// running in the background instead of claiming it was cancelled.
  ///
  /// Wave-31 R30-P2-01: the renewal fact is "did bytes move since THIS
  /// waiter's previous observation" — the ledger below lives across the
  /// waiter's polls, so progress lines the 50 ms event pump drained
  /// between two waiter polls still renew (the waiter and the pump
  /// credit the SAME monotonic ticks; a per-poll baseline would fold the
  /// pump-credited ticks into the new normal and report `false`
  /// forever).
  Future<Map<String, dynamic>> sendFiles(
    String connId,
    List<String> paths, {
    Duration timeout = const Duration(seconds: 120),
  }) async {
    final runtime = _runtimeOrThrow();
    _ensureLive();
    final lastSeen = <String, int>{}; // THIS waiter's cross-poll ledger
    try {
      return await runtime.send(connId, AkTaskSendSpec(paths: paths)).wait(
            interval: opPollInterval,
            timeout: timeout,
            delay: _delay,
            ensureLive: _ensureLive,
            onPoll: () => _progressAdvancedSince(lastSeen, <String>[connId]),
          );
    } on TimeoutException {
      final cancelFailure = _tryCancelTaskConnection(runtime, connId);
      if (cancelFailure != null) {
        throw TimeoutException(
          '发送超时（${timeout.inSeconds} 秒无进展），且取消原生任务失败'
          '（$cancelFailure）——传输可能仍在后台进行，请稍后重试或手动断开连接。',
          timeout,
        );
      }
      _markTerminal(connId, 'disconnected');
      throw TimeoutException(
        '发送超时（${timeout.inSeconds} 秒无进展），已取消原生发送任务并断开该连接。',
        timeout,
      );
    }
  }

  /// The FFI cancel path (P2-10): disconnect the task connection. Returns
  /// null when the cancel went through (or the target was already gone —
  /// `-4` folds the row all the same), or the failure detail when the
  /// cancel itself failed — the caller must then NOT claim "已取消".
  Object? _tryCancelTaskConnection(TaskRuntimeFfi runtime, String connId) {
    try {
      runtime.disconnect(connId);
      return null;
    } on AkFfiException catch (error) {
      if (error.code == AkStatus.errNotFound) {
        // Already gone: the event may have raced the call — the row folds
        // just the same, and "cancelled" is not a lie.
        return null;
      }
      return error.message.isNotEmpty ? error.message : error;
    } on Object catch (error) {
      return error;
    }
  }

  /// Incoming transfer requests (receiver side) — one event per offer,
  /// waiting for [consent]; the engine auto-declines undecided offers
  /// after its 30 s window. Each request carries its native-side
  /// `saveDir` snapshot (wave-30 P2-06) — the folder THIS receive will
  /// use, not necessarily the live setting.
  Stream<SimpleIncomingRequest> get incomingRequests =>
      _incomingController.stream;

  /// Wave-30 P2-11: job ids whose consent request became TERMINAL
  /// undecided — the UI closes/removes their banners. Driven by NATIVE
  /// facts only (no Dart-side timer mirror of the 30 s window): the
  /// engine's `consent_expired` event, and the request's connection
  /// going data-plane terminal (the engine rejects its pending offers
  /// with the connection). A later decline of such a job resolves as
  /// false from [consent] and is handled idempotently by the adapter.
  Stream<String> get expiredIncomingRequests =>
      _expiredIncomingController.stream;

  /// Successful transfer completions (either side) with files/bytes.
  /// A `done` also converges its connection row to the terminal
  /// completed state (wave-30 P2-05: the row no longer offers sends).
  Stream<TaskDoneEvent> get doneEvents => _doneController.stream;

  /// Decide one pending [SimpleIncomingRequest]. Returns false when the
  /// request is unknown or already decided (including auto-declined by
  /// the engine's 30 s window — deciding such a request locally is a
  /// no-op).
  Future<bool> consent(String jobId, {required bool accept}) async {
    final runtime = _runtimeOrThrow();
    _ensureLive();
    _pendingJobs.remove(jobId); // decided — no longer closeable by expiry
    return runtime.consent(jobId, accept: accept);
  }

  /// Wave-30 P2-10 progress plumbing — drains the ENGINE-WIDE
  /// `ak_poll_progress` queue into per-job ticks. Safe to call from the
  /// event pump and from the send/receive stall loops alike (each line
  /// is consumed exactly once and credited to its own job id).
  ///
  /// Wave-31 R30-P1-01: LIVENESS GATE at the entry — dispose invalidates
  /// every call channel before the native owner is released, so a stall
  /// loop's racing tick must never re-poll a freed engine pointer.
  void drainProgress() {
    if (_disposed) {
      return;
    }
    final runtime = _runtime;
    if (runtime == null) {
      return;
    }
    while (true) {
      final line = runtime.pollProgressLine();
      if (line == null) {
        return;
      }
      try {
        final decoded = jsonDecode(line);
        if (decoded is Map<String, dynamic>) {
          final job = decoded['job'];
          if (job is String && job.isNotEmpty) {
            _progressTicks[job] = (_progressTicks[job] ?? 0) + 1;
          }
        }
      } on Object {
        // A malformed progress line never breaks the timeout machinery.
      }
    }
  }

  /// The monotonic progress tick of [job] (0 = no progress line ever
  /// seen). Compare two reads to learn "did bytes move since I looked".
  int progressTick(String job) => _progressTicks[job] ?? 0;

  /// True when any of [jobs] advanced since the CALLER's previous
  /// observation — the deadline-renewal fact of the stall budgets.
  /// [lastSeen] is the caller's own CROSS-POLL ledger, updated in place:
  /// progress lines the EVENT PUMP drained between two of the caller's
  /// polls still count, because the waiter and the pump credit the same
  /// monotonic ticks (R30-P2-01 — the waiter's own observation is the
  /// only baseline that may advance; a per-call baseline would see the
  /// pump-credited ticks as the new normal and never report progress).
  bool _progressAdvancedSince(Map<String, int> lastSeen, List<String> jobs) {
    drainProgress();
    var advanced = false;
    for (final job in jobs) {
      final tick = progressTick(job);
      if (tick > (lastSeen[job] ?? 0)) {
        advanced = true;
        lastSeen[job] = tick;
      }
    }
    return advanced;
  }

  /// The pairing's code id for [connId] — the correlation key the
  /// connection was paired through. BOTH sides know it since wave-31
  /// (R30-P2-02): the creator fills [_codeIdOfKey] at mint time, the
  /// joiner from the join result / `connected` event's `code_id` field.
  /// (Task-plane progress rows are keyed by the CONNECTION id since the
  /// wave-31 ffi-v2 §8.4 revision — the stall budgets no longer read
  /// this mapping; it stays for pairing correlation and diagnostics.
  /// Null when neither source taught this runtime the id.)
  String? codeIdOfConnection(String connId) {
    final code = _connections[connId]?.code;
    if (code == null) {
      return null;
    }
    return _codeIdOfKey[_codeKey(code)];
  }

  /// The engine's code-table key of a display code (uppercase,
  /// separators stripped — the `normalize_code_key` shape).
  static String _codeKey(String raw) =>
      raw.trim().toUpperCase().replaceAll(RegExp(r'[- ]'), '');

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
  /// verdicts (connected = unreachable) < terminal. `disconnected` and
  /// `done` (wave-30 P2-05: the one-task transfer COMPLETED — a final
  /// state that no longer offers sends) share the terminal rank. A
  /// rank-equal update overwrites (there is exactly one probe verdict
  /// per join).
  static int _stateRank(String state) => switch (state) {
        'probing' => 0,
        'connected' || 'unreachable' => 1,
        _ => 2, // disconnected / done / anything unknown stay terminal
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
  /// (wave-29 P2-08, wave-30 P2-05 adds `done` = the one-task transfer
  /// completed). Anything else (e.g. a registry/coordinator link loss
  /// surfaced by the engine) is a coordination-plane notice: the
  /// authorized direct connection may still be healthy, so the row keeps
  /// its state until a real data-plane close arrives — a registry notice
  /// never rewrites the data-plane state.
  static const Set<String> _terminalDisconnectReasons = <String>{
    'local',
    'peer',
    'receiver_failed',
    'done',
  };

  /// Converge the row to a TERMINAL state (`disconnected`, or `done` —
  /// the completed state of wave-30 P2-05) and close the pending-banner
  /// bookkeeping of every undecided request that belonged to the
  /// connection (P2-11: the engine rejects pending offers with the
  /// connection; the banner must go, not wait for a native event).
  void _markTerminal(String connId, String state) {
    final existing = _connections[connId];
    if (existing == null) {
      return;
    }
    _connections[connId] = SimpleConnection(
      connId: connId,
      role: existing.role,
      state: state,
      peerEndpoint: existing.peerEndpoint,
      code: existing.code,
    );
    _notifyConnections();
    _expirePendingJobsOf(connId);
    if (!_connections.values.any((conn) => _stateRank(conn.state) < 2)) {
      _setPhase(SimpleTransferPhase.idle);
    }
  }

  /// P2-11: fire the expiry stream for every STILL-PENDING request of
  /// [connId] (undecided here — [consent] removes decided jobs, the
  /// native `consent_expired` event removes engine-side ones).
  void _expirePendingJobsOf(String connId) {
    final doomed = _pendingJobs.entries
        .where((entry) => entry.value == connId)
        .map((entry) => entry.key)
        .toList();
    for (final job in doomed) {
      _pendingJobs.remove(job);
      _recordClosedJob(job, connId); // R30-P2-03: refuse a late incoming
      if (!_expiredIncomingController.isClosed) {
        _expiredIncomingController.add(job);
      }
    }
  }

  /// R30-P2-03: remembers that [job]'s request became TERMINAL
  /// undecided, bounded FIFO — the record a racing LATE
  /// `incoming_request` is refused against.
  void _recordClosedJob(String job, String? conn) {
    _closedJobs[job] = conn;
    while (_closedJobs.length > _closedJobsCap) {
      _closedJobs.remove(_closedJobs.keys.first);
    }
  }

  /// R30-P2-03: is this `incoming_request` LATE — its request already
  /// died before the announcement could be pumped? True when the job is
  /// in the bounded closure record (a `request_closed`/`consent_expired`
  /// /connection-terminal fact arrived first) or its connection is
  /// already data-plane terminal.
  bool _isLateIncoming(String job, String? conn) {
    if (_closedJobs.containsKey(job)) {
      return true;
    }
    final state = conn == null ? null : _connections[conn]?.state;
    return state != null && _stateRank(state) >= 2;
  }

  /// One pump tick: drain the runtime's event queue and route by type,
  /// then drain the engine-wide progress queue into the P2-10 ticks.
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
        break;
      }
      _route(event);
    }
    drainProgress(); // P2-10: keep the stall ticks fresh for the waiters
  }

  void _route(TaskEvent event) {
    if (event is TaskConnectedEvent) {
      // The engine announces the CONNECTION (pairing bookkeeping) before
      // probing: a fresh row starts at `probing`; a row the pump already
      // settled (probe verdict / terminal) keeps its state (P2-09).
      _upsert(
        connId: event.conn,
        role: event.role,
        peerEndpoint: event.peerEndpoint,
        code: event.code,
        state: 'probing',
      );
      final codeId = event.codeId;
      final code = event.code;
      if (codeId != null && code != null && code.isNotEmpty) {
        // R30-P2-02: the `connected` event teaches a JOINER the pairing's
        // code_id (the creator fills the same table at mint time).
        _codeIdOfKey[_codeKey(code)] = codeId;
      }
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
      // R30-P2-03: a LATE incoming — its request already died terminal
      // (the connection went data-plane terminal, or the engine's
      // request-closure fact arrived first) — must NOT re-open a banner.
      // The native register/publish sequence is not one critical
      // section, so this exact order is reachable.
      if (_isLateIncoming(event.job, event.conn)) {
        return;
      }
      _pendingJobs[event.job] = event.conn; // undecided until consent
      if (!_incomingController.isClosed) {
        _incomingController.add(SimpleIncomingRequest(
          jobId: event.job,
          connId: event.conn,
          rootName: event.rootName,
          files: event.files,
          bytes: event.bytes,
          saveDir: event.saveDir, // P2-06: the request's real target
        ));
      }
      return;
    }
    if (event is TaskDoneEvent) {
      // P2-05: a finished one-task transfer converges the row to the
      // completed terminal state BEFORE the fan-out — the UI must not
      // keep offering a send the engine already refuses.
      final conn = event.conn;
      if (conn != null && conn.isNotEmpty) {
        _markTerminal(conn, 'done');
      }
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
      _markTerminal(conn, event.reason == 'done' ? 'done' : 'disconnected');
      return;
    }
    if (event is TaskConsentExpiredEvent) {
      // P2-11: the NATIVE 30 s window auto-declined the request — the
      // banner closes on this engine fact, never on a Dart timer mirror.
      _pendingJobs.remove(event.job);
      _recordClosedJob(event.job, event.conn); // R30-P2-03
      if (!_expiredIncomingController.isClosed) {
        _expiredIncomingController.add(event.job);
      }
      return;
    }
    if (event is TaskRequestClosedEvent) {
      // R30-P2-03: the request's own terminal fact (the engine resolved
      // the parked offer with the connection's disconnect) — close the
      // banner AND remember the closure so the racing LATE incoming is
      // refused instead of resurrecting an accept/decline row.
      _pendingJobs.remove(event.job);
      _recordClosedJob(event.job, event.conn);
      if (!_expiredIncomingController.isClosed) {
        _expiredIncomingController.add(event.job);
      }
      return;
    }
    // runtime_ready / op_done / op_failed / code_cancelled /
    // handshake_failed / unknown: the op waiters read their own results;
    // handshake_failed is a bounded per-dial record (never a task
    // verdict); nothing to fold here.
  }
}
