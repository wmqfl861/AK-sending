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
import 'dart:typed_data' show Uint8List;

import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show
        AkFfiException,
        AkStatus,
        AkTaskCalls,
        AkTaskCreateCodeSpec,
        AkTaskDoorbell,
        AkTaskDoorbellRegistrar,
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
        TaskEventDoorbell,
        TaskRuntimeFfi,
        TaskFfi;

import 'connection_diagnostics.dart' show ConnectionDiagnosticsLog;

/// CR-P2-05 (review 2026-11-11, item 1): the adaptive EVENT-poll cadence,
/// in milliseconds, as a PURE function of observable activity — the
/// decision [SimpleTransferService] re-makes on every activity transition
/// (see [SimpleTransferService.hasActivity] and
/// [SimpleTransferService.effectiveEventPollInterval]). Tests pin the two
/// values directly; until the native-push rework lands, 50 ms while
/// anything is in flight keeps event latency at the shipped behavior and
/// 1000 ms idle cuts the resting wake-ups from 20/s to 1/s.
int eventPollMs({required bool hasActivity}) => hasActivity
    ? SimpleTransferService.activeEventPollMs
    : SimpleTransferService.idleEventPollMs;

/// CR-P2-05 (item 1): the adaptive OP-RESULT poll cadence, in
/// milliseconds — the same pure decision for the `ak_pairing_op_result`
/// waiters. A pending op is ITSELF activity (the waiters of
/// [SimpleTransferService.createCode]/[join]/[sendFiles] run while
/// [SimpleTransferService.hasActivity] is true), so the waiters always
/// observe the ACTIVE value in practice; the idle value pins the
/// symmetric decision for tests and any future always-on poller.
int opPollMs({required bool hasActivity}) => hasActivity
    ? SimpleTransferService.activeOpPollMs
    : SimpleTransferService.idleOpPollMs;

/// CR-P2-05 (item 1, native-push half — 2026-11-12): the SAFETY-NET
/// active event-poll cadence while a native doorbell is registered, in
/// milliseconds. The doorbell delivers task events the moment they
/// queue, so the periodic tick only exists to catch a MISSED ring
/// (never stall the UI): 250 ms active bounds the worst-case missed-
/// doorbell latency at a quarter second while cutting the resting
/// active-cadence wake-ups from 20/s to 4/s. An intentionally longer
/// configured [SimpleTransferService.eventPollInterval] (manual-pump
/// tests) still wins — the safety net never runs FASTER than what the
/// service was configured with.
int doorbellSafetyPollMs({required bool hasActivity}) => hasActivity
    ? SimpleTransferService.doorbellSafetyActivePollMs
    : SimpleTransferService.idleEventPollMs;

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

  /// TERMINAL engine-failure latch (C12, crash-analysis 2026-11-07):
  /// [SimpleTransferPhase.pollFailureThreshold] CONSECUTIVE native poll
  /// failures (event/progress poll exceptions and malformed progress
  /// lines alike) stopped the pump — the transfer engine is presumed
  /// broken. [SimpleTransferService.failureReason] carries the last
  /// failure detail. Recovery is SERVICE REBUILD ONLY (dispose this
  /// service and construct a new one); no phase transition ever leaves
  /// this sink.
  engineFailed,
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
    this.windowSecs,
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

  /// The consent window THIS request waits on, in whole seconds (E2/G4,
  /// the event's `windowSecs` field): the native deadline is the fact
  /// source, so the UI countdown mirrors this value. Null = an engine
  /// predating the field (the UI shows no countdown rather than a wrong
  /// one).
  final int? windowSecs;

  @override
  String toString() =>
      'SimpleIncomingRequest($jobId root=$rootName files=$files bytes=$bytes '
      'saveDir=$saveDir windowSecs=$windowSecs)';
}

/// Client UI U1 (2026-11-13): one progress snapshot of the transfer on
/// connection [connId], folded from the task-plane `ak_poll_progress`
/// lines (ffi-v2 §8.4 U1 revision: task-plane rows carry the engine's
/// payload fields — `files`/`bytes`/`file`/`chunk`/`path`). An engine
/// predating that revision only sends `job`/`event`: totals then stay
/// null and the UI shows an indeterminate bar.
final class SimpleTransferProgress {
  const SimpleTransferProgress({
    required this.connId,
    required this.phase,
    this.totalFiles,
    this.totalBytes,
    this.completedFiles = 0,
    this.transferredBytes = 0,
    this.currentFile,
    this.currentFileIndex,
    this.bytesPerSecond,
  });

  /// The connection id (send AND receive rows key by it — §8.4 wave-31).
  final String connId;

  /// `connecting` | `awaiting_consent` | `transferring` | `verifying` |
  /// `completed` — monotonic within one transfer.
  final String phase;

  final int? totalFiles;
  final int? totalBytes;

  /// Files finished so far: every byte moved, or verified/committed.
  final int completedFiles;

  /// Bytes so far: acknowledged bytes per file (the engine's checkpoint
  /// cadence), refined by the per-file chunk position (1 MiB chunks, the
  /// task plane's engine default), clamped to each file's size.
  final int transferredBytes;

  /// Display name (last path segment) of the file in flight.
  final String? currentFile;

  final int? currentFileIndex;

  /// Average throughput since the first byte moved; null until measurable.
  final double? bytesPerSecond;

  @override
  String toString() => 'SimpleTransferProgress($connId $phase '
      '$transferredBytes/$totalBytes files=$completedFiles/$totalFiles)';
}

/// The fold state of one transfer's progress lines (see
/// [SimpleTransferProgress]). Private to the service: snapshots are the
/// only thing that leaves it.
///
/// Accounting is incremental: a file whose bytes are complete moves from
/// the in-flight maps into [_doneBytes], so a snapshot costs O(files in
/// flight) — not O(files in the transfer) — however large a folder is.
final class _TransferTracker {
  _TransferTracker(this.connId);

  final String connId;

  /// 0 connecting · 1 awaiting_consent · 2 transferring · 3 verifying ·
  /// 4 completed — never decreases.
  int phaseRank = 0;

  int? totalFiles;
  int? totalBytes;

  /// In-flight files only (a finished file leaves these maps).
  final Map<int, int> _sizes = <int, int>{};
  final Map<int, int> _acked = <int, int>{};
  final Map<int, int> _chunks = <int, int>{};

  /// Every file index known finished (bytes complete, or verified).
  final Set<int> _done = <int>{};

  /// The bytes of the finished files.
  int _doneBytes = 0;

  String? currentFile;
  int? currentFileIndex;

  /// When the first transferring-phase line arrived (the throughput base).
  DateTime? transferStartedAt;

  /// Changed since the last emitted snapshot.
  bool dirty = false;

  /// The task plane's chunk size: the engine default (ak-core
  /// `default_chunk_size`, 1 MiB) — task sends never override it. Used
  /// only to refine progress between the 16-ack checkpoint reports.
  static const int chunkBytes = 1024 * 1024;

  static const List<String> phaseNames = <String>[
    'connecting',
    'awaiting_consent',
    'transferring',
    'verifying',
    'completed',
  ];

  void raiseTo(int rank) {
    if (rank > phaseRank) {
      phaseRank = rank;
      dirty = true;
    }
  }

  /// `file_started`: the file's size is known from here on (an empty
  /// file is finished at once — no chunk will ever name it).
  void fileStarted(int file, int? size) {
    if (_done.contains(file) || size == null) {
      return;
    }
    _sizes[file] = size;
    _settleIfComplete(file);
  }

  /// `bytes_acked`: the cumulative acknowledged bytes of [file].
  void acked(int file, int bytes) {
    if (_done.contains(file)) {
      return;
    }
    if (bytes > (_acked[file] ?? 0)) {
      _acked[file] = bytes;
    }
    _settleIfComplete(file);
  }

  /// `chunk_sent` / `chunk_received`: [index] is the per-file chunk
  /// index, so `index + 1` chunks of [file] have moved.
  void chunk(int file, int index) {
    if (_done.contains(file)) {
      return;
    }
    if (index + 1 > (_chunks[file] ?? 0)) {
      _chunks[file] = index + 1;
    }
    _settleIfComplete(file);
  }

  /// `file_verified` / `file_committed`: [file] is finished whatever the
  /// byte facts said.
  void markDone(int file) {
    if (!_done.add(file)) {
      return;
    }
    final size = _sizes.remove(file);
    final acked = _acked.remove(file);
    final chunks = _chunks.remove(file);
    _doneBytes += size ?? _estimate(acked, chunks, null);
  }

  /// The in-flight bytes estimate of one file: acknowledged bytes,
  /// refined by the chunk position, clamped to the file's size.
  static int _estimate(int? acked, int? chunks, int? size) {
    var done = acked ?? 0;
    if (chunks != null && chunks * chunkBytes > done) {
      done = chunks * chunkBytes;
    }
    return size != null && done > size ? size : done;
  }

  void _settleIfComplete(int file) {
    final size = _sizes[file];
    if (size != null && _estimate(_acked[file], _chunks[file], size) >= size) {
      markDone(file);
    }
  }

  int get completedFiles => _done.length;

  int get transferredBytes {
    final known = totalBytes;
    if (phaseRank >= 4 && known != null) {
      return known;
    }
    var sum = _doneBytes;
    final inFlight = <int>{..._sizes.keys, ..._acked.keys, ..._chunks.keys};
    for (final file in inFlight) {
      sum += _estimate(_acked[file], _chunks[file], _sizes[file]);
    }
    return known != null && sum > known ? known : sum;
  }

  SimpleTransferProgress snapshot(DateTime now) {
    final bytes = transferredBytes;
    double? speed;
    final started = transferStartedAt;
    if (started != null && bytes > 0) {
      final elapsedMs = now.difference(started).inMilliseconds;
      if (elapsedMs >= 1000) {
        speed = bytes * 1000 / elapsedMs;
      }
    }
    return SimpleTransferProgress(
      connId: connId,
      phase: phaseNames[phaseRank],
      totalFiles: totalFiles,
      totalBytes: totalBytes,
      completedFiles: completedFiles,
      transferredBytes: bytes,
      currentFile: currentFile,
      currentFileIndex: currentFileIndex,
      bytesPerSecond: speed,
    );
  }
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

/// R36-P2-03: the engine-health state of the replayable
/// [SimpleTransferService.engineHealth] stream.
enum EngineHealthState {
  /// The native engine answers polls (no latch tripped).
  ok,

  /// The TERMINAL C12 engine-failure latch: [pollFailureThreshold]
  /// consecutive failed native polls (or a revoked C07 lease) stopped the
  /// pump. Recovery is service rebuild only — the failure is fanned out
  /// to the UI the moment it trips, never left an internal field.
  engineFailed,
}

/// R36-P2-03: one engine-health snapshot — [state] plus the visible
/// [failureReason] when the latch tripped.
final class EngineHealth {
  const EngineHealth.ok()
      : state = EngineHealthState.ok,
        failureReason = null;

  const EngineHealth.engineFailed(String this.failureReason)
      : state = EngineHealthState.engineFailed;

  final EngineHealthState state;

  /// The latch's visible reason (null while healthy).
  final String? failureReason;

  bool get isEngineFailed => state == EngineHealthState.engineFailed;
}

/// R36-P2-02: the EXPLICIT outcome of one
/// [SimpleTransferService.drainProgress] call. Returning only the
/// malformed-line count let a healthy event poll reset the C12 streak
/// while the native progress poll kept failing (0→1→0 forever — the
/// latch could never trip); the pump now asks BOTH channels whether THIS
/// tick succeeded before clearing the streak.
final class ProgressDrainResult {
  const ProgressDrainResult({required this.polled, required this.malformed});

  /// Whether this call polled the native progress queue to its end
  /// (queue drained/empty or the per-tick budget reached) WITHOUT a poll
  /// [AkFfiException] or a revoked-lease latch ending it early. False
  /// when the call was gated (disposed / engine-failed / no runtime) or
  /// ended on an exception — such a tick NEVER resets the streak.
  final bool polled;

  /// How many malformed lines were consumed this call (each already
  /// counted toward the streak inside the call).
  final int malformed;

  /// A fully clean drain: polled to the end with nothing malformed.
  bool get clean => polled && malformed == 0;
}

/// The v1 minimal-pairing service. Construct with the FFI seam
/// (`AkTaskFfiBindings`, borrowed engine) or a [FakeAkTaskCalls] for
/// tests; ONE isolate owns the seam (calls serialize on the event loop).
///
/// C12 (crash-analysis 2026-11-07) pump discipline:
///
/// * Each pump tick processes at most [pumpEventBudget] events and
///   drains at most [pumpEventBudget] progress lines — a runaway native
///   queue can never monopolize the UI isolate between ticks; a backlog
///   simply clears over consecutive ticks (event order unchanged).
/// * [pollFailureThreshold] CONSECUTIVE failed native polls (event /
///   progress poll [AkFfiException]s and malformed progress lines alike)
///   latch the TERMINAL [SimpleTransferPhase.engineFailed] state: the
///   pump stops (no further native polling) and [failureReason] carries
///   the visible reason. One clean tick resets the streak — and "clean"
///   means the EVENT and the PROGRESS channels BOTH succeeded this tick
///   (R36-P2-02: a healthy event poll no longer rescues a persistently
///   failing progress poll). Recovery is SERVICE REBUILD ONLY: dispose
///   this service and construct a new one (the engine failure is
///   presumed native-side; [start] refuses on a latched service).
/// * R36-P2-03: the latch is DELIVERED, not just recorded —
///   [engineHealth] fans the failure out to the UI the moment it trips,
///   and every entry/op ends promptly with the same failure through
///   [_ensureLive]'s latch gate.
/// * CR-P2-05 (review 2026-11-11): the pump is ADAPTIVE — a
///   self-rescheduling one-shot timer whose cadence
///   ([effectiveEventPollInterval]) is re-decided by [_syncPump] on every
///   activity transition: [eventPollInterval] while [hasActivity], the
///   20x slower [idlePollInterval] when idle, and paused entirely while
///   the app is backgrounded with nothing in flight
///   ([setAppBackgrounded]). Activity flips re-arm IMMEDIATELY (a new op
///   or pairing never waits out a slow tick); the C12 budget/latch rules
///   above are unchanged in every mode.
/// * CR-P2-05, native-push half (2026-11-12): when the seam supports it,
///   [start] registers a NATIVE EVENT DOORBELL (`ak_task_set_event_notify`,
///   ffi-v2.md §8.6) — Rust rings it on the thread that queues each task
///   event, [_onDoorbell] drains IMMEDIATELY, and the adaptive timer
///   above degrades to a pure SAFETY NET ([doorbellSafetyActivePollMs]
///   active / [idlePollInterval] idle): a missed ring can never stall
///   the UI beyond one safety tick. Seams without the capability (test
///   fakes, old DLLs, diagnostic gates) keep the exact D1 pure-polling
///   behavior — [usesNativeEventPush] is the observable.
final class SimpleTransferService {
  SimpleTransferService({
    required this.calls,
    this.eventPollInterval =
        const Duration(milliseconds: activeEventPollMs),
    this.opPollInterval = const Duration(milliseconds: activeOpPollMs),
    this.idlePollInterval = const Duration(milliseconds: idleEventPollMs),
    AkTaskDoorbellRegistrar? doorbellRegistrar,
    Future<void> Function(Duration)? delay,
    DateTime Function()? clock,
  })  : _doorbellRegistrar = doorbellRegistrar ?? _registerNativeDoorbell,
        _delay = delay ?? Future<void>.delayed,
        _clock = clock ?? DateTime.now;

  /// C12: one pump tick handles at most this many events (and drains at
  /// most this many progress lines) — the per-tick budget that keeps a
  /// runaway native queue off the UI isolate.
  static const int pumpEventBudget = 64;

  /// C12: this many CONSECUTIVE failed native polls (event/progress poll
  /// exceptions and malformed progress lines alike) latch
  /// [SimpleTransferPhase.engineFailed] and stop the pump.
  static const int pollFailureThreshold = 10;

  /// CR-P2-05: the ACTIVE event-poll cadence (ms) — the constructor
  /// default of [eventPollInterval] and the `hasActivity: true` value of
  /// [eventPollMs] (the shipped behavior while tasks run).
  static const int activeEventPollMs = 50;

  /// CR-P2-05: the IDLE event-poll cadence (ms) — the constructor default
  /// of [idlePollInterval] and the `hasActivity: false` value of
  /// [eventPollMs]. 20x slower than [activeEventPollMs]: a resting app
  /// wakes the engine once per second instead of 20 times.
  static const int idleEventPollMs = 1000;

  /// CR-P2-05: the ACTIVE op-result poll cadence (ms) — the constructor
  /// default of [opPollInterval] and the `hasActivity: true` value of
  /// [opPollMs].
  static const int activeOpPollMs = 200;

  /// CR-P2-05: the IDLE op-result poll cadence (ms) — the
  /// `hasActivity: false` value of [opPollMs]. Unreachable by the current
  /// waiters (a pending op is itself activity); it pins the symmetric
  /// decision for tests and future always-on pollers.
  static const int idleOpPollMs = 1000;

  /// CR-P2-05 (native-push half, 2026-11-12): the SAFETY-NET active
  /// event-poll cadence (ms) while a native doorbell is registered (see
  /// [doorbellSafetyPollMs]). The doorbell itself is IMMEDIATE — this
  /// tick only bounds the worst case of a MISSED ring, so it is 5x
  /// slower than [activeEventPollMs]; the idle cadence stays
  /// [idleEventPollMs] either way. WITHOUT a doorbell the pump uses
  /// [eventPollInterval]/[idlePollInterval] exactly as before (the D1
  /// pure-polling behavior is bit-identical).
  static const int doorbellSafetyActivePollMs = 250;

  /// The task-plane call seam (FFI bindings or fake). Public read-only:
  /// the app shell wires `AkTaskFfiBindings` here; tests inject the fake.
  final AkTaskCalls calls;

  /// How often the event pump polls `ak_pairing_poll_event` — the ACTIVE
  /// cadence (CR-P2-05: used whenever [hasActivity]; see
  /// [effectiveEventPollInterval] for the full decision).
  final Duration eventPollInterval;

  /// How often op waiters poll `ak_pairing_op_result` (the ACTIVE cadence
  /// — a pending op is itself activity, so the waiters always run here).
  final Duration opPollInterval;

  /// CR-P2-05: the IDLE event-poll cadence — the slow heartbeat of the
  /// adaptive pump when nothing is in flight. It is a MINIMUM tick
  /// distance, never a speed-up: a service configured with a longer
  /// [eventPollInterval] (tests stretching it to `days:1` to pump
  /// manually) stays that slow at idle too.
  final Duration idlePollInterval;

  /// Injectable delay (tests drive the loops synchronously with a
  /// zero-delay stand-in).
  final Future<void> Function(Duration) _delay;

  /// Client UI U1: the wall clock behind the throughput figure of
  /// [transferProgress] (tests inject a scripted clock).
  final DateTime Function() _clock;

  /// CR-P2-05 (native-push half): the doorbell registrar — the native
  /// attempt by default; tests inject a fake doorbell to drive the
  /// ring/drain path without the DLL.
  final AkTaskDoorbellRegistrar _doorbellRegistrar;

  /// CR-P2-05: the registered native doorbell, or null while this
  /// service runs on PURE POLLING (a seam without the capability — a
  /// fake, an old DLL, a diagnostic gate — or before [start]).
  AkTaskDoorbell? _doorbell;

  TaskRuntimeFfi? _runtime;

  /// Connections by conn id (the list-stream's source of truth).
  final Map<String, SimpleConnection> _connections = <String, SimpleConnection>{};

  /// The current default save folder (null before [start] completed —
  /// the engine default is `<id_dir>/downloads`).
  String? _saveDir;

  SimpleTransferPhase _phase = SimpleTransferPhase.idle;
  String? _failureReason;

  /// CR-P2-05: the adaptive pump — a SELF-RESCHEDULING one-shot timer
  /// (never `Timer.periodic`: the cadence is re-decided per tick).
  /// [_pumpArmedFor] remembers the interval the pending tick was armed
  /// with, so [_syncPump] can leave an on-cadence tick alone and re-arm
  /// ONLY on a real cadence change. [_pumpGeneration] is the chain
  /// identity: a halt (cadence switch / pause / dispose / latch) may land
  /// AFTER a timer fired but BEFORE its callback ran — `Timer.cancel` is
  /// too late for that callback, so it must recognize itself as stale and
  /// do NOTHING (pumping on a dead cadence or nulling [_pump] would
  /// clobber the newly armed timer's handle and fork a second tick chain).
  Timer? _pump;
  Duration? _pumpArmedFor;
  int _pumpGeneration = 0;

  /// CR-P2-05: ops between entry and settle ([createCode]/[join]/
  /// [sendFiles]) — each one is ACTIVITY (keeps the fast cadence even
  /// before/after its phase transition lands).
  int _inFlightOps = 0;

  /// CR-P2-05: the app-lifecycle flag ([setAppBackgrounded]) — true while
  /// the app is paused/hidden: the pump drops to the idle cadence even
  /// with activity, and pauses entirely when idle.
  bool _appBackgrounded = false;
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

  /// R36-P2-03: the engine-health fan-out — the latch's DELIVERY channel
  /// (see [engineHealth]). Per-subscriber SNAPSHOT replay like
  /// [connections]: a late subscriber learns the latched state without
  /// waiting for an event that already fired.
  final StreamController<EngineHealth> _healthController =
      StreamController<EngineHealth>.broadcast();

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

  /// Client UI U1: the live transfers' progress folds, by connection id
  /// (created by the first progress line, dropped when the connection
  /// goes terminal).
  final Map<String, _TransferTracker> _transfers =
      <String, _TransferTracker>{};

  /// Client UI U1: the progress snapshot fan-out (see [transferProgress]).
  final StreamController<SimpleTransferProgress> _progressController =
      StreamController<SimpleTransferProgress>.broadcast();

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

  /// C12: consecutive failed native polls since the last clean tick
  /// (event/progress poll exceptions + malformed progress lines).
  int _consecutivePollFailures = 0;

  /// C12: the terminal engine-failure latch (see
  /// [SimpleTransferPhase.engineFailed]) — once set, the pump stays
  /// stopped and no phase transition leaves the sink.
  bool _engineFailed = false;

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
    String? stunAddr,
  }) async {
    _ensureLive();
    if (_engineFailed) {
      // C12: the engine-failed latch is terminal — recovery is service
      // REBUILD, never a restart on the same broken engine.
      throw StateError(
          '传输引擎已故障（${_failureReason ?? '连续轮询失败'}）：请新建传输服务恢复。');
    }
    if (_runtime != null) {
      return; // idempotent: one task runtime per service
    }
    final runtime = TaskFfi(calls: calls).start(AkTaskStartSpec(
      registry: registry,
      registryIdHex: registryIdHex,
      idDir: idDir,
      deviceName: deviceName,
      saveDir: saveDir,
      stunAddr: stunAddr,
    ));
    _runtime = runtime;
    // CR-P2-05 (native-push half, 2026-11-12): register the native event
    // doorbell lazily HERE (the runtime id exists only now). Graceful
    // fallback: a seam without the capability (a fake, an old DLL, a
    // diagnostic gate) or a refused registration returns null and the
    // service keeps the D1 pure-polling behavior bit-identical. Events
    // that queued before the registration (the runtime_ready of start)
    // are picked up by the synchronous drain below.
    _doorbell = _doorbellRegistrar(calls, runtime.runtimeId, _onDoorbell);
    _saveDir = saveDir;
    _setPhase(SimpleTransferPhase.idle);
    // CR-P2-05: the adaptive pump arms here (idle → the slow cadence) and
    // re-arms itself on every activity transition (see [_syncPump]).
    _syncPump();
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
    // CR-P2-05 (native-push half): unregister the doorbell BEFORE the
    // pump stops — `close()` first blocks native-side until an in-flight
    // ring returned and clears the registration (the ffi-v2 §8.6
    // lifetime guarantee), then releases the NativeCallable. The native
    // engine handle itself is NOT touched here (this service borrows the
    // seam; the owner releases it after every borrower disposed).
    _doorbell?.close();
    _doorbell = null;
    // (2) the pump.
    _haltPump();
    // (3) streams (listeners get a done event; late pollers throw nothing).
    _connectionsController.close();
    _incomingController.close();
    _doneController.close();
    _expiredIncomingController.close();
    _healthController.close();
    _progressController.close();
    _transfers.clear();
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
    _beginOp(); // CR-P2-05: a pending op is activity — fast cadence NOW
    try {
      _setPhase(SimpleTransferPhase.creating);
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
    } finally {
      _endOp();
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
    _beginOp(); // CR-P2-05: a pending op is activity — fast cadence NOW
    try {
      _setPhase(SimpleTransferPhase.joining);
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
    } finally {
      _endOp();
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
  /// receiver-side for the 120 s consent window (E2/G4) engine-side.
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
  ///
  /// E2/G4: the default stall budget is 150 s — the consent wait itself
  /// produces NO progress lines (the offer parks while the peer's user
  /// reads the banner for up to 120 s), so the sender-side budget must
  /// clear the window the same way the engine's `TASK_SEND_IDLE` (150 s)
  /// does; the old 120 s default could cancel a send whose receiver was
  /// still inside a legal window.
  Future<Map<String, dynamic>> sendFiles(
    String connId,
    List<String> paths, {
    Duration timeout = const Duration(seconds: 150),
  }) async {
    final runtime = _runtimeOrThrow();
    _ensureLive();
    _beginOp(); // CR-P2-05: a pending op is activity — fast cadence NOW
    final lastSeen = <String, int>{}; // THIS waiter's cross-poll ledger
    try {
      final report = await runtime.send(connId, AkTaskSendSpec(paths: paths)).wait(
            interval: opPollInterval,
            timeout: timeout,
            delay: _delay,
            ensureLive: _ensureLive,
            onPoll: () => _progressAdvancedSince(lastSeen, <String>[connId]),
          );
      // G2: the GUI analog of the CLI's "connected via" fact — the
      // address the engine actually transferred over (task_send's
      // `peer`), one bounded line for the diagnostics export.
      ConnectionDiagnosticsLog.shared.recordRaw('sent_via', {
        'conn': connId,
        'peer': report['peer'],
        'files': report['files'],
        'bytes': report['bytes'],
      });
      return report;
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
    } finally {
      _endOp();
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
  /// after its window (120 s since E2/G4). Each request carries its native-side
  /// `saveDir` snapshot (wave-30 P2-06) — the folder THIS receive will
  /// use, not necessarily the live setting.
  Stream<SimpleIncomingRequest> get incomingRequests =>
      _incomingController.stream;

  /// Wave-30 P2-11: job ids whose consent request became TERMINAL
  /// undecided — the UI closes/removes their banners. Driven by NATIVE
  /// facts only (no Dart-side timer mirror of the consent window): the
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

  /// R36-P2-03: the replayable engine-health stream. EVERY listen first
  /// receives the CURRENT snapshot (`ok`, or the latched failure with its
  /// visible reason), then the live changes — the C12 latch fans out HERE
  /// the moment it trips, so the UI never depends on polling a getter or
  /// on a connection-snapshot update that a broken engine never produces.
  Stream<EngineHealth> get engineHealth => Stream.multi((controller) {
        // Attach the live tap FIRST — a latch fired after this point
        // queues behind the snapshot below (FIFO delivery, no gap).
        final live = _healthController.stream.listen(
          controller.add,
          onError: controller.addError,
          onDone: controller.close,
        );
        controller.add(_currentHealth);
        controller.onCancel = () => live.cancel();
      });

  /// The health snapshot [engineHealth] replays per listen.
  EngineHealth get _currentHealth => _engineFailed
      ? EngineHealth.engineFailed(_failureReason ?? '连续轮询失败')
      : const EngineHealth.ok();

  /// Decide one pending [SimpleIncomingRequest]. Returns false when the
  /// request is unknown or already decided (including auto-declined by
  /// the engine's window — deciding such a request locally is a
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
  /// C12: one call drains at most [pumpEventBudget] lines — a runaway
  /// progress queue clears over successive calls, and each MALFORMED
  /// line (a `jsonDecode` failure) counts toward the consecutive poll
  /// failure streak; the line itself is still consumed, so one bad line
  /// never blocks the queue.
  ///
  /// Wave-31 R30-P1-01: LIVENESS GATE at the entry — dispose invalidates
  /// every call channel before the native owner is released, so a stall
  /// loop's racing tick must never re-poll a freed engine pointer. C12
  /// adds the engine-failed latch to the same gate (no polling a broken
  /// engine either).
  ///
  /// R36-P2-02: returns the EXPLICIT [ProgressDrainResult] — whether the
  /// native progress poll COMPLETED this call (not just the malformed
  /// count). The pump tick combines it with the event channel's outcome:
  /// only a tick whose BOTH channels succeeded may reset the C12 streak
  /// (a healthy event poll no longer rescues a persistently failing
  /// progress poll).
  ProgressDrainResult drainProgress() {
    if (_disposed || _engineFailed) {
      return const ProgressDrainResult(polled: false, malformed: 0);
    }
    final runtime = _runtime;
    if (runtime == null) {
      return const ProgressDrainResult(polled: false, malformed: 0);
    }
    var malformed = 0;
    var drained = 0;
    try {
      while (drained < pumpEventBudget && !_engineFailed) {
        final String? line;
        try {
          line = runtime.pollProgressLine();
        } on AkFfiException catch (error) {
          _notePollFailure('进度事件轮询失败', error);
          return ProgressDrainResult(polled: false, malformed: malformed);
        } on StateError catch (error) {
          // C07: revoked lease — the same immediate stop as the event loop.
          _latchEngineFailed('引擎租约已撤销（C07 lease）', error);
          return ProgressDrainResult(polled: false, malformed: malformed);
        }
        if (line == null) {
          return ProgressDrainResult(polled: true, malformed: malformed);
        }
        drained++;
        Map<String, dynamic>? fields;
        try {
          final decoded = jsonDecode(line);
          if (decoded is Map<String, dynamic>) {
            final job = decoded['job'];
            if (job is String && job.isNotEmpty) {
              _progressTicks[job] = (_progressTicks[job] ?? 0) + 1;
              fields = decoded;
            }
          }
        } on Object {
          // A malformed progress line never breaks the timeout machinery —
          // but it IS an engine-protocol failure: count it (C12).
          malformed++;
          // U1: rows may carry transfer-relative paths (ffi-v2 §8.4) —
          // the failure reason names the size only, never the content.
          _notePollFailure('进度行解析失败', '${line.length} 字节，内容不记录');
        }
        if (fields != null) {
          _foldProgress(fields);
        }
      }
      return ProgressDrainResult(polled: true, malformed: malformed);
    } finally {
      // Client UI U1: whatever ended the drain, the lines it consumed
      // reach the UI as at most ONE snapshot per transfer.
      _emitProgressSnapshots();
    }
  }

  /// Client UI U1: live transfer progress, one [SimpleTransferProgress]
  /// per change (at most one per transfer per drain), for sends and
  /// receives alike. A transfer that completes ends with a `completed`
  /// snapshot; an aborted one simply stops (its send/consent future
  /// carries the failure). Broadcast, no replay.
  Stream<SimpleTransferProgress> get transferProgress =>
      _progressController.stream;

  /// Folds one decoded progress line into its transfer's tracker. NEVER
  /// throws and never feeds the C12 streak: a line whose payload fields
  /// are missing (an engine predating the ffi-v2 §8.4 U1 revision) or
  /// oddly typed simply contributes less detail.
  ///
  /// Only a LIVE connection of this service gets a tracker: a line keyed
  /// by anything else (an engine-level job, a receive row that fell back
  /// to its code id) or arriving after the row went terminal is ignored,
  /// so nothing accumulates for ids no page will ever ask about.
  void _foldProgress(Map<String, dynamic> line) {
    try {
      final job = line['job'] as String;
      final event = line['event'];
      if (event is! String) {
        return;
      }
      var tracker = _transfers[job];
      if (tracker == null) {
        final row = _connections[job];
        if (row == null || _stateRank(row.state) >= 2) {
          return;
        }
        // A new transfer announces itself at once (`connecting`), so the
        // UI can say what the send waits for before any byte moves.
        tracker = _TransferTracker(job)..dirty = true;
        _transfers[job] = tracker;
      } else if (tracker.phaseRank >= 4) {
        // v1: one pairing carries ONE transfer — a completed tracker
        // stays until its row goes terminal, and nothing reopens it.
        return;
      }
      int? number(String key) {
        final value = line[key];
        return value is int && value >= 0 ? value : null;
      }

      switch (event) {
        case 'offered':
          tracker.totalFiles = number('files') ?? tracker.totalFiles;
          tracker.totalBytes = number('bytes') ?? tracker.totalBytes;
          // The SENDER now waits for the receiving user's 接受; on the
          // receiving side the offer is not a waiting state of its own.
          if (_connections[job]?.role == 'sender') {
            tracker.raiseTo(1);
          }
        case 'accepted':
          tracker.totalFiles = number('files') ?? tracker.totalFiles;
          tracker.totalBytes = number('bytes') ?? tracker.totalBytes;
          _beginTransferring(tracker);
        case 'resuming':
          _beginTransferring(tracker);
        case 'chunk_sent':
        case 'chunk_received':
          _beginTransferring(tracker);
          final file = number('file');
          final chunk = number('chunk');
          if (file != null && chunk != null) {
            tracker.chunk(file, chunk);
          }
        case 'file_started':
          _beginTransferring(tracker);
          final file = number('file');
          if (file != null) {
            tracker.currentFileIndex = file;
            tracker.fileStarted(file, number('bytes'));
          }
          final path = line['path'];
          if (path is String && path.isNotEmpty) {
            tracker.currentFile = _displayNameOf(path);
          }
        case 'bytes_acked':
          _beginTransferring(tracker);
          final file = number('file');
          final bytes = number('bytes');
          if (file != null && bytes != null) {
            tracker.acked(file, bytes);
          }
        case 'file_verified':
        case 'file_committed':
          _beginTransferring(tracker);
          final file = number('file');
          if (file != null) {
            tracker.markDone(file);
          }
        case 'verifying':
        case 'final_ack':
          tracker.raiseTo(3);
        case 'done':
          tracker.raiseTo(4);
        default:
          // connecting / connected / manifest_* / paused / resumed /
          // unknown: nothing to fold beyond the tick.
          return;
      }
      // Every byte moved: what remains is the receiver's whole-transfer
      // verification (the sender waits for its final ack).
      final total = tracker.totalBytes;
      if (tracker.phaseRank == 2 &&
          total != null &&
          total > 0 &&
          tracker.transferredBytes >= total) {
        tracker.raiseTo(3);
      }
      tracker.dirty = true;
    } on Object {
      // A fold bug must never touch the poll-failure accounting.
    }
  }

  void _beginTransferring(_TransferTracker tracker) {
    tracker.raiseTo(2);
    tracker.transferStartedAt ??= _clock();
  }

  /// The last path segment (`photos/img.bin` → `img.bin`); progress rows
  /// use `/` separators (the engine normalizes `\`).
  static String _displayNameOf(String path) {
    final trimmed =
        path.endsWith('/') ? path.substring(0, path.length - 1) : path;
    final slash = trimmed.lastIndexOf('/');
    return slash < 0 ? trimmed : trimmed.substring(slash + 1);
  }

  /// Fans out one snapshot per DIRTY tracker. Never throws. Trackers
  /// (completed ones included) live until their row goes terminal — see
  /// [_settleTransfer].
  void _emitProgressSnapshots() {
    if (_transfers.isEmpty) {
      return;
    }
    final now = _clock();
    for (final tracker in _transfers.values) {
      if (!tracker.dirty) {
        continue;
      }
      tracker.dirty = false;
      if (!_progressController.isClosed) {
        _progressController.add(tracker.snapshot(now));
      }
    }
  }

  /// A connection went terminal: its tracker goes with the row. A transfer
  /// that COMPLETED but whose `completed` snapshot was not out yet (the
  /// task `done` event can be pumped before the progress queue's own
  /// `done` line) gets it now; an aborted transfer simply stops.
  void _settleTransfer(String connId, String state) {
    final tracker = _transfers.remove(connId);
    if (tracker == null || state != 'done' || tracker.phaseRank >= 4) {
      return;
    }
    tracker.raiseTo(4);
    if (!_progressController.isClosed) {
      _progressController.add(tracker.snapshot(_clock()));
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

  /// G6b (2026-11-13): the registry server's build revision, e.g.
  /// `0.1.0+4739f4d` — one `ak_task_server_info` op (ffi-v2.md §8.7),
  /// waited like every other op. Null = "no information" and is NEVER an
  /// error surface: an old server without the revision-query capability,
  /// a failed query, an unreachable registry stack, a not-yet-started or
  /// disposed service, the engine-failure latch, or an old DLL without
  /// the export all degrade to null — the「关于」card then shows 未知.
  /// Deliberately NOT part of the phase machine: a diagnostic query
  /// never moves `creating`/`failed`.
  Future<String?> serverRevision({
    Duration timeout = const Duration(seconds: 20),
  }) async {
    final runtime = _runtime; // null: not started — nothing to ask
    if (runtime == null || _disposed || _engineFailed) {
      return null;
    }
    _beginOp(); // CR-P2-05: a pending op is activity — fast cadence NOW
    try {
      final result = await runtime.serverInfo().wait(
            interval: opPollInterval,
            timeout: timeout,
            delay: _delay,
            ensureLive: _ensureLive,
          );
      final revision = result['revision'];
      return revision is String && revision.isNotEmpty ? revision : null;
    } on Object {
      // Every failure shape degrades to "no information" (the op's own
      // `"revision":null` contract plus the wait's failure modes — op
      // error, stall timeout, revoked lease, disposed service, an old
      // DLL without the export): the About card never surfaces engine
      // noise.
      return null;
    } finally {
      _endOp();
    }
  }

  // -- state --------------------------------------------------------------

  /// The pairing phase (the state machine above).
  SimpleTransferPhase get phase => _phase;

  /// The failure detail when [phase] is [SimpleTransferPhase.failed] or
  /// [SimpleTransferPhase.engineFailed].
  String? get failureReason => _failureReason;

  /// C12: the terminal engine-failure latch — true once
  /// [pollFailureThreshold] consecutive native polls failed; the pump is
  /// stopped and only disposing + rebuilding this service recovers.
  bool get isEngineFailed => _engineFailed;

  /// The synchronous connection snapshot.
  List<SimpleConnection> get currentConnections =>
      UnmodifiableListView<SimpleConnection>(_connections.values.toList());

  bool get isDisposed => _disposed;

  // -- CR-P2-05: adaptive pump cadence -------------------------------------

  /// CR-P2-05: does anything need the FAST event cadence right now? True
  /// while a pairing or connection is in flight (phase
  /// `creating`/`joining`/`paired`/`connected` — `paired` includes the
  /// creator waiting for the peer's join and probes still pending), while
  /// an op waiter is polling ([createCode]/[join]/[sendFiles] between
  /// entry and settle), or while any LIVE connection row exists
  /// (`probing`/`connected`). A row already settled to `unreachable` is
  /// NOT activity: the probe verdict is final, nothing can arrive for it
  /// that one slow tick cannot deliver, and treating it as activity would
  /// pin the pump at 50 ms forever.
  bool get hasActivity =>
      _inFlightOps > 0 ||
      _phase == SimpleTransferPhase.creating ||
      _phase == SimpleTransferPhase.joining ||
      _phase == SimpleTransferPhase.paired ||
      _phase == SimpleTransferPhase.connected ||
      _connections.values
          .any((row) => row.state == 'probing' || row.state == 'connected');

  /// CR-P2-05: the cadence the NEXT pump tick is scheduled at — the
  /// applied form of [eventPollMs] over this service's configuration:
  /// the ACTIVE interval while in the foreground with [hasActivity],
  /// else the idle cadence. The ACTIVE interval is [eventPollInterval]
  /// — EXCEPT while a native doorbell is registered
  /// ([usesNativeEventPush]): the doorbell delivers events immediately,
  /// so the tick degrades to the pure SAFETY NET of
  /// [doorbellSafetyActivePollMs] (never faster than the configured
  /// [eventPollInterval]; see [doorbellSafetyPollMs]). Backgrounded apps
  /// ([setAppBackgrounded]) always drop to the idle cadence, activity or
  /// not.
  Duration get effectiveEventPollInterval =>
      !_appBackgrounded && hasActivity
          ? _activeEventPollInterval
          : _idleEventPollInterval;

  /// CR-P2-05: the ACTIVE tick distance in the current mode — D1's
  /// [eventPollInterval] under pure polling (bit-identical fallback),
  /// or the slower-of-both safety net while a doorbell is registered.
  Duration get _activeEventPollInterval {
    if (_doorbell == null) {
      return eventPollInterval;
    }
    final safety = const Duration(milliseconds: doorbellSafetyActivePollMs);
    return safety > eventPollInterval ? safety : eventPollInterval;
  }

  /// CR-P2-05 (native-push half): is the native event doorbell
  /// registered? False = this service runs on PURE POLLING with the
  /// exact D1 behavior (the observable the fallback tests pin).
  bool get usesNativeEventPush => _doorbell != null;

  /// CR-P2-05: is a pump tick pending? False while the pump is paused
  /// ([setAppBackgrounded] with no activity), after [dispose] and after
  /// the C12 engine latch — the observable lifecycle tests assert on.
  bool get isEventPumpArmed => _pump != null;

  /// CR-P2-05 (item 2): the app-lifecycle lever, driven by the shell's
  /// widgets-binding lifecycle observer on `paused`/`hidden` (`true`) and
  /// `resumed`/`inactive` (`false`).
  ///
  /// Backgrounded: the pump drops to the IDLE cadence even while a task
  /// runs — the data plane lives in the engine (Rust) and the op waiters
  /// poll independently, so a once-per-second heartbeat keeps events
  /// (consent offers, disconnects, `done`) visible without 20 wake-ups
  /// per second behind a hidden window. Backgrounded AND idle: the pump
  /// pauses entirely (no timer at all) — with no live row and no pairing
  /// nothing can arrive, so waking a quiet engine is pure battery cost.
  ///
  /// Foreground again: the connection/task state refreshes IMMEDIATELY
  /// (one synchronous drain of everything that queued while hidden),
  /// then the pump re-arms at the activity-decided cadence.
  void setAppBackgrounded(bool backgrounded) {
    if (_disposed || _appBackgrounded == backgrounded) {
      return;
    }
    _appBackgrounded = backgrounded;
    if (!backgrounded) {
      _pumpOnce(); // refresh first: drain what queued while hidden
    }
    _syncPump();
  }

  // -- internals ----------------------------------------------------------

  /// CR-P2-05: an op waiter entered ([createCode]/[join]/[sendFiles]) —
  /// count it as activity and re-decide the cadence immediately.
  void _beginOp() {
    _inFlightOps++;
    _syncPump();
  }

  /// CR-P2-05: an op waiter settled — drop its activity vote and let the
  /// cadence fall back if this was the last one.
  void _endOp() {
    _inFlightOps--;
    _syncPump();
  }

  /// CR-P2-05: the idle/background cadence — never FASTER than the
  /// active interval of the current mode (a test that stretches the
  /// active interval to pump manually stays inert at idle too).
  Duration get _idleEventPollInterval =>
      idlePollInterval > _activeEventPollInterval
          ? idlePollInterval
          : _activeEventPollInterval;

  /// CR-P2-05 (native-push half): the default doorbell registrar — the
  /// native attempt over the call seam
  /// ([TaskEventDoorbell.tryRegister]; [TaskRuntimeFfi
  /// .registerEventDoorbell] is the same thing for callers that already
  /// hold a runtime view), which returns null (pure polling) for every
  /// unsupported seam or refused registration.
  static AkTaskDoorbell? _registerNativeDoorbell(
          AkTaskCalls calls, Uint8List runtimeId, void Function() onRing) =>
      TaskEventDoorbell.tryRegister(calls, runtimeId, onRing);

  /// CR-P2-05 (native-push half): a native ring arrived — drain NOW.
  /// Runs as an event-loop message on this isolate (the NativeCallable
  /// listener delivers here, never concurrently), so a burst of rings
  /// serializes: the first drain empties the queue and the trailing
  /// rings each find it empty (the poll is authoritative; extra rings
  /// are harmless no-ops). Gated like a pump tick: a disposed,
  /// engine-failed, or pre-start service ignores the ring.
  void _onDoorbell() {
    if (_disposed || _engineFailed || _runtime == null) {
      return;
    }
    _pumpOnce();
  }

  /// CR-P2-05: the adaptive pump's (re)arm — the single authority over
  /// [_pump]. Idempotent per cadence: a pending tick armed at the CURRENT
  /// [effectiveEventPollInterval] is left alone (activity flapping never
  /// starves the tick); any cadence CHANGE cancels the pending tick and
  /// re-arms at the new interval, so activity starts/stops take effect
  /// IMMEDIATELY instead of waiting out the previous (possibly slow)
  /// tick. Also the pause point: backgrounded with no activity disarms
  /// the pump entirely.
  void _syncPump() {
    if (_disposed || _engineFailed || _runtime == null) {
      return;
    }
    if (_appBackgrounded && !hasActivity) {
      _haltPump(); // hidden and quiet: no wake-ups at all
      return;
    }
    final interval = effectiveEventPollInterval;
    if (_pump != null && _pumpArmedFor == interval) {
      return; // already armed at this cadence — keep the pending tick
    }
    _haltPump();
    final generation = _pumpGeneration;
    _pumpArmedFor = interval;
    _pump = Timer(interval, () => _onPumpTick(generation));
  }

  /// CR-P2-05: cancel the pending tick (if any), forget its cadence and
  /// retire its chain — a callback that already fired runs into a stale
  /// generation and turns into a no-op ([_onPumpTick]).
  void _haltPump() {
    _pumpGeneration++;
    _pump?.cancel();
    _pump = null;
    _pumpArmedFor = null;
  }

  /// CR-P2-05: one fired tick of chain [generation]. Stale fires (their
  /// chain was halted between the timer firing and the callback running)
  /// do NOTHING — they must not pump on a retired cadence nor clobber
  /// the successor chain's [_pump] handle (which would fork concurrent
  /// tick chains and storm the engine). A valid fire forgets the fired
  /// timer FIRST (events routed by this tick may flip the cadence and
  /// arm the next tick mid-tick — forgetting up front keeps exactly ONE
  /// pending timer per chain), then pumps and lets the trailing
  /// [_syncPump] re-decide from the post-tick state.
  void _onPumpTick(int generation) {
    if (generation != _pumpGeneration) {
      return; // a halted chain's late fire — the successor owns the pump
    }
    _pump = null;
    _pumpArmedFor = null;
    _pumpOnce();
    _syncPump();
  }

  void _ensureLive() {
    if (_disposed) {
      throw StateError('SimpleTransferService 已释放。');
    }
    if (_engineFailed) {
      // R36-P2-03: the latch ends every pending op and every new entry
      // PROMPTLY with the SAME failure — the op waiters' `ensureLive`
      // hook runs at the top of each poll iteration, so an in-flight
      // action aborts at its next tick instead of hanging to its own
      // timeout or polling a broken engine.
      throw StateError(_failureReason ?? '传输引擎已故障：请新建传输服务恢复。');
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
    if (_engineFailed && next != SimpleTransferPhase.engineFailed) {
      // C12: the engine-failed latch is TERMINAL — a late join result,
      // row transition or action failure never masks it. Recovery is
      // service rebuild.
      return;
    }
    if (_phase == next) {
      return;
    }
    _phase = next;
    if (next != SimpleTransferPhase.failed &&
        next != SimpleTransferPhase.engineFailed) {
      _failureReason = null;
    }
    // CR-P2-05: a phase change can flip activity — re-decide the pump
    // cadence NOW (never wait out a slow tick).
    _syncPump();
  }

  void _fail(Object error) {
    if (_engineFailed) {
      // C12: the engine-failed latch outranks the generic sink — and its
      // reason STAYS (the documented latch idempotence; a later action
      // failure must not rewrite what the UI already shows).
      return;
    }
    _failureReason = error is AkFfiException
        ? error.message
        : (error is TimeoutException ? error.message : error.toString());
    _setPhase(SimpleTransferPhase.failed);
  }

  /// C12: one more consecutive native poll failure (an event/progress
  /// poll exception or a malformed progress line). The streak spans pump
  /// ticks and stall-loop drains alike; a fully clean pump tick resets
  /// it (see [_pumpOnce]). At [pollFailureThreshold] the terminal
  /// engine-failed state latches: the reason becomes visible and the
  /// pump stops — the engine is presumed broken and only a service
  /// rebuild recovers.
  void _notePollFailure(String what, Object detail) {
    _consecutivePollFailures++;
    if (_consecutivePollFailures < pollFailureThreshold || _engineFailed) {
      return; // transient so far, or already latched (reason stays put)
    }
    _latchEngineFailed('连续 $pollFailureThreshold 次$what', detail);
  }

  /// Latches the terminal engine-failed state (C12): visible reason, pump
  /// stopped. Idempotent — the first latch's reason stays. R36-P2-03: the
  /// latch is DELIVERED — [engineHealth] fans the failure out to every
  /// listener the moment it trips (a getter-only latch never reaches a
  /// real GUI).
  void _latchEngineFailed(String what, Object detail) {
    if (_engineFailed) {
      return;
    }
    _engineFailed = true;
    final reason = detail is AkFfiException
        ? detail.message
        : (detail is TimeoutException ? detail.message : detail.toString());
    _failureReason = '传输引擎故障：$what（$reason）。泵已停止，请重启应用恢复。';
    _setPhase(SimpleTransferPhase.engineFailed);
    _haltPump(); // C12: stop polling a broken engine
    if (!_healthController.isClosed) {
      _healthController.add(_currentHealth);
    }
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
    // CR-P2-05: a row appearing/settling can flip activity — re-decide
    // the cadence (a no-op when it did not).
    _syncPump();
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
    // Client UI U1: the transfer's progress fold ends with the row.
    _settleTransfer(connId, state);
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
    // CR-P2-05: the row going terminal can end activity (the last live
    // connection settled) — drop to the idle cadence NOW.
    _syncPump();
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
  ///
  /// C12: the tick handles at most [pumpEventBudget] events — a backlog
  /// beyond the budget stays queued and clears on the following ticks
  /// (FIFO order preserved). A poll [AkFfiException] no longer retries
  /// silently forever: it counts toward the consecutive-failure streak,
  /// and [pollFailureThreshold] failures in a row latch
  /// [SimpleTransferPhase.engineFailed] and STOP the pump. A fully clean
  /// tick (queue drained, no parse failures) resets the streak.
  ///
  /// R36-P2-02: "clean" is measured PER CHANNEL — the streak clears only
  /// when the EVENT poll AND the progress drain BOTH succeeded this
  /// tick. A healthy event channel must not zero the counter while the
  /// progress channel keeps failing natively (the 0→1→0 loop that kept
  /// the latch from ever tripping).
  void _pumpOnce() {
    if (_disposed || _engineFailed) {
      return;
    }
    final runtime = _runtime;
    if (runtime == null) {
      return;
    }
    var handled = 0;
    while (handled < pumpEventBudget) {
      final TaskEvent? event;
      try {
        event = runtime.pollEvent();
      } on AkFfiException catch (error) {
        _notePollFailure('任务事件轮询失败', error);
        return; // the streak escalates in _notePollFailure
      } on StateError catch (error) {
        // C07: the owner's lease is revoked — the borrowed handle is
        // gone. Stop NOW (never retry into a dangling pointer) and
        // surface it through the same visible engine-failed latch.
        _latchEngineFailed('引擎租约已撤销（C07 lease）', error);
        return;
      }
      if (event == null) {
        break;
      }
      _route(event);
      handled++;
    }
    // The event channel completed cleanly this tick (no exception above).
    // P2-10: keep the stall ticks fresh — and P2-02: only a tick whose
    // PROGRESS drain also completed cleanly may reset the streak.
    final drain = drainProgress();
    if (drain.clean && _consecutivePollFailures > 0) {
      _consecutivePollFailures = 0; // BOTH channels clean: clear the streak
    }
  }

  void _route(TaskEvent event) {
    // G2 (batchF 2026-11-13): every polled event passes the diagnostics
    // ring buffer FIRST (it filters to the diagnostic class internally —
    // bind/probe/connect/disconnect facts ride to the settings page's
    // export; everything else drops, exactly like the branches below).
    ConnectionDiagnosticsLog.shared.record(event);
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
          windowSecs: event.windowSecs, // E2/G4: the countdown's source
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
      // P2-11: the NATIVE window (120 s) auto-declined the request — the
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
