/// [FakeAkTaskCalls]: an in-Dart implementation of the [AkTaskCalls] seam
/// that models the wave-28 Rust task plane closely enough to drive the
/// service layer's loops — the `pairing_ffi.dart` test-fake pattern,
/// shipped in `lib/` so the Flutter app's service tests (and agent D's
/// widget tests) reuse the SAME fake instead of growing a parallel one.
///
/// What it models faithfully:
///
/// * every spec is parsed back and validated with the SAME rules as the
///   builders (an invalid spec answers `-2` with a message, exactly like
///   the engine's pre-side-effect validation);
/// * ops are ASYNC: `createCode`/`join`/`send` return an op id and the
///   result flips from pending to terminal only after
///   [FakeAkTaskCalls.pumpOps] (or automatically when
///   [autoCompleteOps] is true — the default for service-style tests);
/// * `opResult`/`pollEvent` honor the `-3` contract: a buffer smaller
///   than the payload answers `-3` and consumes NOTHING (set
///   [forceBufferTooSmallOnce] to rehearse the grow loops);
/// * the event queue is FIFO and `runtime_ready` / `op_done` / `op_failed`
///   mirror the engine's announcement order;
/// * `incoming_request` events are SCRIPTED by the test
///   ([emitIncomingRequest]) — the fake has no real network peer — and
///   carry the request's snapshotted `saveDir` (wave-30 P2-06);
/// * the native 30 s consent window is SCRIPTED via [expireConsent]
///   (wave-30 P2-11: the engine emits `consent_expired`, never a local
///   Dart timer), and a disconnect-driven pending-request closure via
///   [closePendingRequest] (wave-31 R30-P2-03: `request_closed`);
/// * the ENGINE-WIDE progress queue (`ak_poll_progress`, wave-30 P2-10)
///   is scripted via [emitProgress] and read through [pollProgress];
/// * `ak_task_disconnect` covers UN-JOINED code cancellation by
///   `code_id` (wave-30 P2-04): the code is revoked (later joins fail)
///   and a `code_cancelled` event is emitted; every disconnect call is
///   recorded in [disconnectCalls] for assertions.
///
/// What it deliberately does NOT model: real sockets, real bytes, the
/// 30 s consent timer (decide via [consent] or leave it — no auto-decline
/// fires), and the request_id registry (replays mint fresh op ids; the
/// engine-side idempotency is covered by the Rust tests).
library;

import 'dart:convert' show jsonDecode, jsonEncode, utf8;
import 'dart:math' show Random;
import 'dart:typed_data' show Uint8List;

import 'bindings.dart' show AkStatus;
import 'task_ffi.dart';

/// One pending or settled op of the fake.
final class FakeTaskOp {
  FakeTaskOp(this.opId, this.op);

  final Uint8List opId;
  final String op;

  /// The op body's terminal JSON (runs at completion, engine-style: the
  /// side effects — code registration, conn bookkeeping — happen at the
  /// call, the RESULT is async).
  Map<String, dynamic> Function()? body;
  Map<String, dynamic>? result; // null = pending
  bool ok = true;

  /// The settle event was emitted once (engine ops announce exactly once).
  bool announced = false;
}

/// The fake seam (see the library doc).
final class FakeAkTaskCalls implements AkTaskCalls {
  FakeAkTaskCalls({this.autoCompleteOps = true, Random? random})
      : _random = random ?? Random(4711);

  /// Whether op bodies complete on the NEXT `opResult` poll (service-style
  /// tests) instead of waiting for an explicit [pumpOps].
  final bool autoCompleteOps;

  final Random _random;

  /// Started runtimes by hex id.
  final Map<String, FakeTaskRuntime> runtimes = <String, FakeTaskRuntime>{};

  /// Ops by hex op id (across runtimes).
  final Map<String, FakeTaskOp> ops = <String, FakeTaskOp>{};

  /// The most recent failing status's detail (null after a clean call).
  String? _lastErrorDetail;

  /// Scripted detail for the NEXT negative return (cleared after use).
  String? nextErrorDetail;

  /// When set, the next buffer call answers `-3` ONCE (then clears) so the
  /// `-3` grow-retry loops are exercised end to end.
  bool forceBufferTooSmallOnce = false;

  /// Every spec JSON the fake accepted, newest last (assertion material).
  final List<String> acceptedSpecs = <String>[];

  /// Hex ids of EVERY `ak_task_disconnect` call, in order (connections
  /// AND un-joined code ids — the wave-30 P2-04 cancel assertion hook).
  final List<String> disconnectCalls = <String>[];

  /// When set, the NEXT disconnect call fails with this detail (the
  /// honest-cancel-failure rehearsal of wave-30 P2-10).
  String? nextDisconnectFails;

  /// The ENGINE-WIDE progress queue (`ak_poll_progress`) — FIFO lines
  /// pushed by [emitProgress] and drained by [pollProgress].
  final List<Map<String, dynamic>> progressLines = <Map<String, dynamic>>[];

  /// RAW progress lines served FIRST by [pollProgress] (FIFO, ahead of
  /// [progressLines]) — the C12 malformed-line rehearsal: any string
  /// goes out verbatim, so a line the service cannot parse is one
  /// `rawProgressLines.add('{oops')` away.
  final List<String> rawProgressLines = <String>[];

  /// C12 rehearsal: while positive, the next that-many `pollEvent` calls
  /// answer `-1` (errInternal) with [pollEventFailureDetail] — the
  /// persistent native-poll-failure script (events stay queued).
  int failPollEventCalls = 0;

  /// The detail of the scripted [failPollEventCalls] failures.
  String pollEventFailureDetail = 'fake: poll_event 持续故障';

  /// Total `pollEvent` calls served — the "native untouched" witness of
  /// the C07 lease tests and the C12 pump-stop assertion.
  int pollEventCallCount = 0;

  /// R36-P2-02 rehearsal: while positive, the next that-many
  /// `pollProgress` calls answer `-1` (errInternal) with
  /// [pollProgressFailureDetail] — the persistent native PROGRESS-poll
  /// failure script while the EVENT channel stays healthy (exactly the
  /// split-channel shape the old streak-reset bug hid).
  int failPollProgressCalls = 0;

  /// The detail of the scripted [failPollProgressCalls] failures.
  String pollProgressFailureDetail = 'fake: poll_progress 持续故障';

  /// Total `pollProgress` calls served (assertion material).
  int pollProgressCallCount = 0;

  // -- seam ------------------------------------------------------------

  @override
  String? lastError() => _lastErrorDetail;

  int _fail(int code, String detail) {
    _lastErrorDetail = detail;
    return code;
  }

  int _ok() {
    _lastErrorDetail = null;
    return AkStatus.ok;
  }

  @override
  int start(String specJson, Uint8List runtimeIdOut) {
    final parsed = _decode(specJson);
    if (parsed == null) {
      return _fail(AkStatus.errInvalid, 'fake: spec 不是合法 JSON');
    }
    for (final key in const ['registry', 'registry_id', 'id_dir']) {
      if (parsed[key] is! String || (parsed[key] as String).isEmpty) {
        return _fail(AkStatus.errInvalid, 'fake: 缺少必填字段 "$key"');
      }
    }
    final id = _newId();
    final runtime = FakeTaskRuntime(id);
    runtimes[_hex(id)] = runtime;
    runtimeIdOut.setAll(0, id);
    runtime.events.add(<String, dynamic>{
      'event': 'runtime_ready',
      'runtime': _hex(id),
    });
    acceptedSpecs.add(specJson);
    return _ok();
  }

  @override
  int createCode(Uint8List runtimeId, String specJson, Uint8List opIdOut) {
    final runtime = _runtimeOf(runtimeId);
    if (runtime == null) {
      return _fail(AkStatus.errNotFound, 'fake: 未知任务运行时');
    }
    final parsed = _decode(specJson);
    final role = parsed?['role'];
    if (role != 'sender' && role != 'receiver') {
      return _fail(AkStatus.errInvalid, 'fake: 字段 "role" 必须是 sender 或 receiver');
    }
    final op = _mintOp(runtime, 'task_create_code', opIdOut);
    final code = _mintDisplayCode();
    final codeId = _hex(_newId());
    runtime.codes.add(_codeKey(code));
    runtime.roleOfCode[_codeKey(code)] = role as String;
    // The code_id is minted ONCE and registered for the fake's cancel
    // path (wave-30 P2-04: ak_task_disconnect by code_id revokes it).
    runtime.codeIdOfCode[_codeKey(code)] = codeId;
    op.body = () => <String, dynamic>{
      'op': 'task_create_code',
      'code': code,
      'role': role,
      'code_id': codeId,
      'expires_ms': 0,
      'endpoint': '127.0.0.1:45${(400 + runtime.codes.length * 7) % 100}',
    };
    if (autoCompleteOps) {
      op.result = op.body!();
    }
    _applyScriptedFailure(runtime, op);
    acceptedSpecs.add(specJson);
    return _ok();
  }

  @override
  int join(Uint8List runtimeId, String specJson, Uint8List opIdOut) {
    final runtime = _runtimeOf(runtimeId);
    if (runtime == null) {
      return _fail(AkStatus.errNotFound, 'fake: 未知任务运行时');
    }
    final parsed = _decode(specJson);
    final code = parsed?['code'];
    if (code is! String || code.trim().isEmpty) {
      return _fail(AkStatus.errInvalid, 'fake: 缺少必填字段 "code"');
    }
    // Optional explicit joiner role (wave-29 P1-05): the engine's
    // `role_hint` — validated like `TaskRole::parse` on the Rust side.
    final roleHint = parsed?['role'];
    if (roleHint != null && roleHint != 'sender' && roleHint != 'receiver') {
      return _fail(
          AkStatus.errInvalid, 'fake: 字段 "role" 必须是 sender 或 receiver');
    }
    final normalized = _codeKey(code);
    final op = _mintOp(runtime, 'task_join', opIdOut);
    // Resolve against ANY started runtime's codes (the engine-local stand-in
    // behaves the same: one shared table per engine == one fake).
    final ownerEntry = runtimes.entries
        .where((entry) => entry.value.codes.contains(normalized))
        .toList();
    if (ownerEntry.isEmpty) {
      op.ok = false;
      op.result = <String, dynamic>{
        'op': 'task_join',
        'error': 'unknown task code "$normalized"',
      };
    } else {
      final owner = ownerEntry.first.value;
      final role = roleHint is String
          ? roleHint // explicit user intent wins
          : (owner.roleOfCode[normalized] == 'sender' ? 'receiver' : 'sender');
      final conn = _hex(_newId());
      // Wave-31 R30-P2-02: the join result and BOTH `connected` events
      // carry the pairing's `code_id` — the joiner learns the same
      // correlation key the creator minted (native lands the same field
      // in parallel).
      final codeId = owner.codeIdOfCode[normalized];
      runtime.connections[conn] = TaskConnState(role, 'probing', '127.0.0.1:45678');
      if (identical(owner, runtime)) {
        // Self-join (one runtime drives both actions, the single-runtime
        // service-test shape): ONE entry — the joiner's — with its own
        // probe pair.
        runtime.events.add(<String, dynamic>{
          'event': 'connected',
          'conn': conn,
          'role': role,
          'peer_endpoint': '127.0.0.1:45678',
          'code': normalized,
          if (codeId != null) 'code_id': codeId,
        });
        runtime.events.add(<String, dynamic>{
          'event': 'probe_result',
          'conn': conn,
          'endpoint': '127.0.0.1:45678',
          'reachable': true,
        });
      } else {
        owner.connections[conn] =
            TaskConnState(role == 'sender' ? 'receiver' : 'sender', 'connected', '127.0.0.1:45678');
        for (final side in [owner, runtime]) {
          side.events.add(<String, dynamic>{
            'event': 'connected',
            'conn': conn,
            'role': side.connections[conn]!.role,
            'peer_endpoint': '127.0.0.1:45678',
            'code': normalized,
            if (codeId != null) 'code_id': codeId,
          });
          // Both sides run their own reachability probe after the
          // `connected` bookkeeping event (the Rust owner_side_completion
          // / run_join shape).
          side.events.add(<String, dynamic>{
            'event': 'probe_result',
            'conn': conn,
            'endpoint': '127.0.0.1:45678',
            'reachable': true,
          });
        }
      }
      op.body = () => <String, dynamic>{
        'op': 'task_join',
        'conn': conn,
        'role': role,
        'peer_endpoint': '127.0.0.1:45678',
        'probe': 'verified',
        if (codeId != null) 'code_id': codeId,
      };
      if (autoCompleteOps) {
        op.result = op.body!();
      }
    }
    _applyScriptedFailure(runtime, op);
    acceptedSpecs.add(specJson);
    return _ok();
  }

  @override
  int send(Uint8List runtimeId, Uint8List connId, String pathsJson,
      Uint8List opIdOut) {
    final runtime = _runtimeOf(runtimeId);
    if (runtime == null) {
      return _fail(AkStatus.errNotFound, 'fake: 未知任务运行时');
    }
    final conn = runtime.connections[_hex(connId)];
    if (conn == null) {
      return _fail(
          AkStatus.errNotFound, 'fake: 未知连接 ${_hex(connId)}');
    }
    if (conn.state == 'disconnected') {
      return _fail(AkStatus.errInvalid, 'fake: 连接已断开');
    }
    final parsed = _decode(pathsJson);
    final paths = parsed?['paths'];
    if (paths is! List || paths.isEmpty) {
      return _fail(AkStatus.errInvalid, 'fake: 字段 "paths" 必须是非空数组');
    }
    final op = _mintOp(runtime, 'task_send', opIdOut);
    if (conn.role != 'sender') {
      op.ok = false;
      op.result = <String, dynamic>{
        'op': 'task_send',
        'error': 'only the sender side of a task connection may send',
      };
    } else {
      op.body = () => <String, dynamic>{
        'op': 'task_send',
        'conn': _hex(connId),
        'files': paths.length,
        'bytes': 1024,
        'peer': '127.0.0.1:45678',
      };
      if (autoCompleteOps) {
        op.result = op.body!();
      }
      runtime.sendDoneConn = _hex(connId);
    }
    _applyScriptedFailure(runtime, op);
    acceptedSpecs.add(pathsJson);
    return _ok();
  }

  @override
  int list(Uint8List runtimeId, Uint8List buffer, int cap) {
    final runtime = _runtimeOf(runtimeId);
    if (runtime == null) {
      return _fail(AkStatus.errNotFound, 'fake: 未知任务运行时');
    }
    final payload = jsonEncode(<String, dynamic>{
      'runtime': _hex(runtimeId),
      'save_dir': runtime.saveDir,
      'connections': [
        for (final entry in runtime.connections.entries)
          <String, dynamic>{
            'conn': entry.key,
            'role': entry.value.role,
            'state': entry.value.state,
            'peer_endpoint': entry.value.endpoint,
            'code': 'AKI1-0000-0000-0000-0000-0000-0000-0000-0000',
          },
      ],
    });
    final wrote = _writePayload(payload, buffer, cap);
    return wrote > 0 ? AkStatus.done : wrote; // -3 stays -3
  }

  @override
  int disconnect(Uint8List runtimeId, Uint8List connId) {
    final runtime = _runtimeOf(runtimeId);
    if (runtime == null) {
      return _fail(AkStatus.errNotFound, 'fake: 未知任务运行时');
    }
    final id = _hex(connId);
    disconnectCalls.add(id);
    final scripted = nextDisconnectFails;
    if (scripted != null) {
      nextDisconnectFails = null;
      return _fail(AkStatus.errInternal, scripted);
    }
    final conn = runtime.connections[id];
    if (conn == null) {
      // Wave-30 P2-04: cancelling an UN-JOINED code by its code_id
      // revokes the registry row (later joins fail) and emits
      // `code_cancelled`; repeated cancels stay silently OK; unknown
      // ids keep the engine's `-4`.
      if (runtime.cancelledCodeIds.contains(id)) {
        return _ok(); // already revoked — silent AK_OK
      }
      final keyOf = runtime.codeIdOfCode.entries
          .where((entry) => entry.value == id)
          .map((entry) => entry.key)
          .toList();
      if (keyOf.isEmpty) {
        return _fail(AkStatus.errNotFound, 'fake: 未知连接 $id');
      }
      for (final key in keyOf) {
        runtime.codes.remove(key);
        runtime.roleOfCode.remove(key);
        runtime.codeIdOfCode.remove(key);
      }
      if (runtime.cancelledCodeIds.add(id)) {
        runtime.events.add(<String, dynamic>{
          'event': 'code_cancelled',
          'code_id': id,
        });
      }
      return _ok();
    }
    if (conn.state != 'disconnected') {
      conn.state = 'disconnected';
      runtime.events.add(<String, dynamic>{
        'event': 'disconnected',
        'conn': id,
        'reason': 'local',
      });
      // Bilateral: the peer runtime hears `peer`.
      for (final other in runtimes.values) {
        if (identical(other, runtime)) continue;
        final peerConn = other.connections[id];
        if (peerConn != null && peerConn.state != 'disconnected') {
          peerConn.state = 'disconnected';
          other.events.add(<String, dynamic>{
            'event': 'disconnected',
            'conn': id,
            'reason': 'peer',
          });
        }
      }
    }
    return _ok();
  }

  @override
  int setSaveDir(Uint8List runtimeId, String path) {
    final runtime = _runtimeOf(runtimeId);
    if (runtime == null) {
      return _fail(AkStatus.errNotFound, 'fake: 未知任务运行时');
    }
    if (path.isEmpty) {
      return _fail(AkStatus.errInvalid, 'fake: save_dir 不能为空');
    }
    runtime.saveDir = path;
    return _ok();
  }

  @override
  int consent(Uint8List runtimeId, Uint8List jobId, int accept) {
    final runtime = _runtimeOf(runtimeId);
    if (runtime == null) {
      return _fail(AkStatus.errNotFound, 'fake: 未知任务运行时');
    }
    final key = _hex(jobId);
    if (!runtime.pendingConsents.contains(key)) {
      return _fail(AkStatus.errNotFound, 'fake: 未知或已决定的请求 $key');
    }
    runtime.pendingConsents.remove(key);
    runtime.decisions[key] = accept != 0;
    return _ok();
  }

  @override
  int opResult(Uint8List runtimeId, Uint8List opId, Uint8List buffer, int cap) {
    final runtime = _runtimeOf(runtimeId);
    if (runtime == null) {
      return _fail(AkStatus.errNotFound, 'fake: 未知任务运行时');
    }
    final op = ops[_hex(opId)];
    if (op == null) {
      return _fail(AkStatus.errNotFound, 'fake: 未知操作 ${_hex(opId)}');
    }
    if (op.result == null && autoCompleteOps) {
      _complete(runtime, op);
    } else if (op.result != null && !op.announced) {
      // A pre-filled body (join/send) settles on its first read: announce.
      _announce(runtime, op);
    }
    final result = op.result;
    if (result == null) {
      _lastErrorDetail = null;
      return AkStatus.pending;
    }
    final payload = jsonEncode(result);
    final code = _writePayload(payload, buffer, cap);
    if (code > 0) {
      return op.ok ? AkStatus.done : AkStatus.failed;
    }
    return code;
  }

  @override
  int pollEvent(Uint8List runtimeId, Uint8List buffer, int cap) {
    pollEventCallCount++;
    final runtime = _runtimeOf(runtimeId);
    if (runtime == null) {
      return _fail(AkStatus.errNotFound, 'fake: 未知任务运行时');
    }
    if (failPollEventCalls > 0) {
      failPollEventCalls--;
      return _fail(AkStatus.errInternal, pollEventFailureDetail);
    }
    if (runtime.events.isEmpty) {
      _lastErrorDetail = null;
      return AkStatus.pending;
    }
    // The -3 contract: the event stays queued until it actually fit.
    final payload = jsonEncode(runtime.events.first);
    final wrote = _writePayload(payload, buffer, cap);
    if (wrote > 0) {
      runtime.events.removeAt(0);
      return 1;
    }
    return wrote; // -3: nothing consumed
  }

  @override
  int pollProgress(Uint8List buffer, int cap) {
    pollProgressCallCount++;
    // R36-P2-02 rehearsal: the scripted persistent progress-poll failure
    // answers BEFORE the queue is consulted (nothing is consumed).
    if (failPollProgressCalls > 0) {
      failPollProgressCalls--;
      return _fail(AkStatus.errInternal, pollProgressFailureDetail);
    }
    // The ENGINE-WIDE queue (no runtime scoping — one fake == one engine).
    if (rawProgressLines.isNotEmpty) {
      // C12 rehearsal: raw (possibly malformed) lines go out verbatim.
      final payload = rawProgressLines.first;
      final wrote = _writePayload(payload, buffer, cap);
      if (wrote > 0) {
        rawProgressLines.removeAt(0);
        return 1;
      }
      return wrote; // -3: nothing consumed
    }
    if (progressLines.isEmpty) {
      _lastErrorDetail = null;
      return AkStatus.pending;
    }
    final payload = jsonEncode(progressLines.first);
    final wrote = _writePayload(payload, buffer, cap);
    if (wrote > 0) {
      progressLines.removeAt(0);
      return 1;
    }
    return wrote; // -3: nothing consumed
  }

  // -- scripting -------------------------------------------------------

  /// Emits one `incoming_request` on [runtimeIdHex]'s queue and registers
  /// the job for a later [consent] (the pending-request id is returned as
  /// hex). The event carries the request's snapshotted `saveDir`
  /// (wave-30 P2-06) — the runtime's save dir at OFFER time unless the
  /// test scripts an explicit target.
  String emitIncomingRequest(
    String runtimeIdHex, {
    String? connId,
    String rootName = 'photos',
    int files = 1,
    int bytes = 4096,
    String? saveDir,
  }) {
    final runtime = runtimes[runtimeIdHex];
    if (runtime == null) {
      throw StateError('fake: 未知运行时 $runtimeIdHex');
    }
    final job = _hex(_newId());
    runtime.pendingConsents.add(job);
    if (connId != null) {
      runtime.jobConn[job] = connId;
    }
    runtime.events.add(<String, dynamic>{
      'event': 'incoming_request',
      'job': job,
      if (connId != null) 'conn': connId,
      'root': rootName,
      'files': files,
      'bytes': bytes,
      'saveDir': saveDir ?? runtime.saveDir,
    });
    return job;
  }

  /// Simulates the native 30 s consent window closing on [jobId]
  /// undecided (wave-30 P2-11): the pending entry is removed (a later
  /// consent answers `-4`) and the terminal `consent_expired` event is
  /// queued. A no-op for unknown/already-decided jobs (the engine never
  /// emits the event for them).
  void expireConsent(String runtimeIdHex, String jobId) {
    final runtime = runtimes[runtimeIdHex];
    if (runtime == null) {
      throw StateError('fake: 未知运行时 $runtimeIdHex');
    }
    if (!runtime.pendingConsents.remove(jobId)) {
      return;
    }
    runtime.events.add(<String, dynamic>{
      'event': 'consent_expired',
      'job': jobId,
      for (final entry in runtime.jobConn.entries)
        if (entry.key == jobId) 'conn': entry.value,
    });
  }

  /// Pushes one line onto the ENGINE-WIDE progress queue
  /// (`ak_poll_progress`, wave-30 P2-10): task-plane rows — send AND
  /// receive alike — key `job` by the CONNECTION id, the wave-31
  /// correlation revision of ffi-v2 §8.4 (wave-30 keyed receive rows by
  /// the code id, which a JOINER receiver never knew; the connection id
  /// is the one key both sides always have).
  void emitProgress(String jobId, {String event = 'chunk_sent'}) {
    progressLines.add(<String, dynamic>{
      'job': jobId,
      'event': event,
    });
  }

  /// Simulates the native `decline_pending_of_conn` (wave-31
  /// R30-P2-03): the connection's disconnect resolves [jobId]'s parked
  /// offer with a declination — the pending entry is removed (a later
  /// consent answers `-4`) and the request-TERMINAL `request_closed`
  /// event is queued. A no-op for unknown/already-decided jobs.
  void closePendingRequest(String runtimeIdHex, String jobId,
      {String? connId, String reason = 'disconnected'}) {
    final runtime = runtimes[runtimeIdHex];
    if (runtime == null) {
      throw StateError('fake: 未知运行时 $runtimeIdHex');
    }
    if (!runtime.pendingConsents.remove(jobId)) {
      return;
    }
    runtime.events.add(<String, dynamic>{
      'event': 'request_closed',
      'job': jobId,
      if (connId != null) 'conn': connId,
      'reason': reason,
    });
  }

  /// Emits one `done` event on [runtimeIdHex]'s queue.
  void emitDone(String runtimeIdHex,
      {String? connId, int files = 1, int bytes = 4096}) {
    final runtime = runtimes[runtimeIdHex];
    if (runtime == null) {
      throw StateError('fake: 未知运行时 $runtimeIdHex');
    }
    runtime.events.add(<String, dynamic>{
      'event': 'done',
      if (connId != null) 'conn': connId,
      'files': files,
      'bytes': bytes,
    });
  }

  /// Settles every still-pending op of one runtime (for
  /// [autoCompleteOps] = false tests).
  void pumpOps(String runtimeIdHex) {
    final runtime = runtimes[runtimeIdHex];
    if (runtime == null) {
      throw StateError('fake: 未知运行时 $runtimeIdHex');
    }
    for (final op in ops.values) {
      if (op.result == null && runtime.opIds.contains(_hex(op.opId))) {
        _complete(runtime, op);
      }
    }
  }

  /// Fails the NEXT create/join/send op of [runtimeIdHex] with [message]
  /// (scripted terminal failure).
  void failNextOp(String runtimeIdHex, String message) {
    final runtime = runtimes[runtimeIdHex];
    if (runtime == null) {
      throw StateError('fake: 未知运行时 $runtimeIdHex');
    }
    runtime.nextOpFails = message;
  }

  void _complete(FakeTaskRuntime runtime, FakeTaskOp op) {
    if (runtime.nextOpFails != null) {
      op.ok = false;
      op.result = <String, dynamic>{'op': op.op, 'error': runtime.nextOpFails};
      runtime.nextOpFails = null;
    } else {
      op.result ??= op.body?.call() ?? <String, dynamic>{'op': op.op};
    }
    _announce(runtime, op);
  }

  /// Applies a scripted [FakeAkTaskCalls.failNextOp] to a freshly minted,
  /// pre-filled op body (create/join/send pre-fill their results, so the
  /// failure must replace the body before the first read).
  void _applyScriptedFailure(FakeTaskRuntime runtime, FakeTaskOp op) {
    final message = runtime.nextOpFails;
    if (message == null) {
      return;
    }
    runtime.nextOpFails = null;
    op.ok = false;
    op.result = <String, dynamic>{'op': op.op, 'error': message};
  }

  /// Emits the settle event exactly once per op (mirrors the engine's
  /// op_done/op_failed announcement).
  void _announce(FakeTaskRuntime runtime, FakeTaskOp op) {
    if (op.announced) {
      return;
    }
    op.announced = true;
    runtime.events.add(<String, dynamic>{
      'event': op.ok ? 'op_done' : 'op_failed',
      'op_id': _hex(op.opId),
      'op': op.op,
    });
    if (op.ok && op.op == 'task_send' && runtime.sendDoneConn != null) {
      runtime.events.add(<String, dynamic>{
        'event': 'done',
        'conn': runtime.sendDoneConn,
        'files': op.result?['files'] ?? 1,
        'bytes': op.result?['bytes'] ?? 0,
      });
      runtime.sendDoneConn = null;
    }
  }

  // -- internals ---------------------------------------------------------

  FakeTaskOp _mintOp(FakeTaskRuntime runtime, String opName, Uint8List opIdOut) {
    final id = _newId();
    opIdOut.setAll(0, id);
    final op = FakeTaskOp(id, opName);
    ops[_hex(id)] = op;
    runtime.opIds.add(_hex(id));
    return op;
  }

  FakeTaskRuntime? _runtimeOf(Uint8List runtimeId) =>
      runtimes[_hex(runtimeId)];

  /// Writes [payload] (+NUL) into [buffer] honoring the `-3` contract.
  int _writePayload(String payload, Uint8List buffer, int cap) {
    if (forceBufferTooSmallOnce) {
      forceBufferTooSmallOnce = false;
      _lastErrorDetail =
          'buffer too small: need ${utf8.encode(payload).length + 1} bytes '
          'including the NUL terminator (capacity $cap); retry with a larger buffer';
      return -3;
    }
    final bytes = utf8.encode(payload);
    if (bytes.length + 1 > cap) {
      _lastErrorDetail =
          'buffer too small: need ${bytes.length + 1} bytes including the NUL '
          'terminator (capacity $cap); retry with a larger buffer';
      return -3;
    }
    for (var i = 0; i < bytes.length; i++) {
      buffer[i] = bytes[i];
    }
    buffer[bytes.length] = 0;
    _lastErrorDetail = null;
    return bytes.length;
  }

  Map<String, dynamic>? _decode(String json) {
    try {
      final decoded = jsonDecode(json);
      return decoded is Map<String, dynamic> ? decoded : null;
    } on FormatException {
      return null;
    }
  }

  Uint8List _newId() =>
      Uint8List.fromList(List<int>.generate(16, (_) => _random.nextInt(256)));

  /// One real-shape display code: `AKI1-` + eight groups of four Crockford
  /// Base32 symbols (I, L, O, U excluded) — mirrors the Rust
  /// `InviteCode::to_display` presentation exactly (wave-29 P1-02).
  String _mintDisplayCode() {
    final groups = <String>[
      for (var group = 0; group < 8; group++)
        String.fromCharCodes(List<int>.generate(
            4, (_) => _crockford.codeUnitAt(_random.nextInt(32)))),
    ];
    return 'AKI1-${groups.join('-')}';
  }

  static const String _crockford = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';
}

/// The code-table key: uppercase, separators stripped — mirrors the
/// engine's `normalize_code_key` (hyphen-less join input matches).
String _codeKey(String raw) =>
    raw.trim().toUpperCase().replaceAll(RegExp(r'[- ]'), '');

String _hex(Uint8List id) =>
    id.map((b) => b.toRadixString(16).padLeft(2, '0')).join();

final class TaskConnState {
  TaskConnState(this.role, this.state, this.endpoint);

  final String role;
  String state;
  final String endpoint;
}

final class FakeTaskRuntime {
  FakeTaskRuntime(Uint8List id) : id = _hex(id);

  final String id;
  final List<Map<String, dynamic>> events = <Map<String, dynamic>>[];
  final List<String> codes = <String>[];
  final Map<String, String> roleOfCode = <String, String>{};

  /// Normalized code key → the minted code_id (wave-30 P2-04: the
  /// `ak_task_disconnect` cancel key for un-joined codes).
  final Map<String, String> codeIdOfCode = <String, String>{};

  /// Code ids whose `code_cancelled` event already went out (repeated
  /// cancels stay silently OK).
  final Set<String> cancelledCodeIds = <String>{};
  final Map<String, TaskConnState> connections = <String, TaskConnState>{};
  final Set<String> pendingConsents = <String>{};
  final Map<String, bool> decisions = <String, bool>{};

  /// Pending-request id → its connection (consent_expired attribution).
  final Map<String, String> jobConn = <String, String>{};
  final Set<String> opIds = <String>{};
  String saveDir = 'C:/ak/downloads';
  String? nextOpFails;
  String? sendDoneConn;
}
