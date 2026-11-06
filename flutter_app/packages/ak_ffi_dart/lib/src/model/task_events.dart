/// Typed model for the task-plane events polled via `ak_pairing_poll_event`
/// with a TASK runtime id (wave 28, `ak_task_*`). The task event queue is
/// the pairing event channel EXTENDED — same export, new `"event"` names —
/// and stays separate from the transfer progress queue (`ak_poll_progress`).
///
/// Wire names of the wave-28 Rust side (crates/ak-ffi/src/task.rs):
///
/// ```json
/// {"event":"runtime_ready","runtime":"<32hex>"}
/// {"event":"op_done","op_id":"<32hex>","op":"task_create_code|task_join|task_send"}
/// {"event":"op_failed","op_id":"<32hex>","op":"..."}
/// {"event":"connected","conn":"<32hex>","role":"sender|receiver",
///  "peer_endpoint":"ip:port","code":"AKI1-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX-XXXX"}
/// {"event":"probe_result","conn":"<32hex>","endpoint":"ip:port","reachable":true|false|null}
/// {"event":"incoming_request","job":"<32hex>","conn":"<32hex>",
///  "root":"photos","files":3,"bytes":123456,"saveDir":"D:/inbox"}
/// {"event":"done","conn":"<32hex>","files":3,"bytes":123456}
/// {"event":"disconnected","conn":"<32hex>","reason":"local|peer|receiver_failed|done"}
/// {"event":"consent_expired","job":"<32hex>"}
/// ```
///
/// Wave-30 contract-first notes (the Rust task plane lands the same
/// shapes in parallel):
/// * `incoming_request.saveDir` (camelCase key, wave-30 P2-06): the save
///   directory THIS request will use, snapshotted native-side at offer
///   time — the receive lands there even if the settings change before
///   the user answers, and the UI shows this value as the target.
/// * `consent_expired` (wave-30 P2-11): the native 30 s consent window
///   auto-declined the request — the terminal request event that closes
///   pending UI banners (no Dart-side timer mirror).
///
/// Wave-29 note on `disconnected` reasons (P2-08, wave-30 P2-05 widened):
/// `local`, `peer`, `receiver_failed` and `done` are DATA-PLANE terminal
/// verdicts — the task's direct connection is gone (`done` = the one-task
/// transfer COMPLETED, wave-30: the row converges to a completed state
/// that no longer offers sends). Any OTHER reason string is a
/// coordination-plane notice (e.g. a registry/coordinator link loss): the
/// authorized data channel may still be healthy, so consumers must keep
/// such connections alive instead of mapping the notice to a terminal
/// state. Only a real data-plane close drives the terminal state.
///
/// Parsing policy mirrors `pairing_events.dart`: an unknown `"event"` name
/// decodes to [TaskUnknownEvent] (forward compatibility); a missing or
/// wrongly typed payload field throws [FormatException] because it means
/// the contract is broken, not merely newer.
library;

/// Base of the typed task-plane events plus the unknown fallback.
///
/// Instances compare by runtime type and payload fields.
sealed class TaskEvent {
  const TaskEvent({this.atMs});

  /// Optional engine timestamp, Unix epoch milliseconds (`at_ms`).
  final int? atMs;

  /// The `"event"` name as it appears on the wire.
  String get name;

  /// Payload fields used for equality/toString.
  Map<String, Object?> get fields => const {};

  /// Decodes one task-plane event line (wire names above).
  factory TaskEvent.fromJson(Map<String, Object?> json) {
    final event = _string(json, 'event');
    final at = json['at_ms'] is int ? json['at_ms'] as int : null;
    switch (event) {
      case 'runtime_ready':
        return TaskRuntimeReadyEvent(atMs: at, runtime: _string(json, 'runtime'));
      case 'connected':
        return TaskConnectedEvent(
          atMs: at,
          conn: _string(json, 'conn'),
          role: _string(json, 'role'),
          peerEndpoint: _optionalString(json, 'peer_endpoint'),
          code: _optionalString(json, 'code'),
        );
      case 'probe_result':
        return TaskProbeResultEvent(
          atMs: at,
          conn: _string(json, 'conn'),
          endpoint: _optionalString(json, 'endpoint'),
          reachable: json['reachable'] is bool ? json['reachable'] as bool? : null,
        );
      case 'incoming_request':
        return TaskIncomingRequestEvent(
          atMs: at,
          job: _string(json, 'job'),
          conn: _optionalString(json, 'conn'),
          rootName: _optionalString(json, 'root') ?? '',
          files: _int(json, 'files'),
          bytes: _int(json, 'bytes'),
          saveDir: _optionalString(json, 'saveDir'),
        );
      case 'done':
        return TaskDoneEvent(
          atMs: at,
          conn: _optionalString(json, 'conn'),
          files: _int(json, 'files'),
          bytes: _int(json, 'bytes'),
        );
      case 'disconnected':
        return TaskDisconnectedEvent(
          atMs: at,
          conn: _optionalString(json, 'conn'),
          reason: _optionalString(json, 'reason') ?? 'unknown',
        );
      case 'consent_expired':
        return TaskConsentExpiredEvent(
          atMs: at,
          job: _string(json, 'job'),
          conn: _optionalString(json, 'conn'),
        );
      case 'op_done':
      case 'op_failed':
        return TaskOpSettledEvent(
          atMs: at,
          ok: event == 'op_done',
          opId: _string(json, 'op_id'),
          op: _string(json, 'op'),
        );
      default:
        return TaskUnknownEvent(atMs: at, name: event, fields: Map.of(json)..remove('event')..remove('at_ms'));
    }
  }

  @override
  String toString() {
    final parts = <String>[
      name,
      if (atMs != null) 'at=$atMs',
      for (final entry in fields.entries) '${entry.key}=${entry.value}',
    ];
    return '$runtimeType(${parts.join(' ')})';
  }

  @override
  bool operator ==(Object other) =>
      other.runtimeType == runtimeType &&
      other is TaskEvent &&
      other.name == name &&
      other.atMs == atMs &&
      _mapEquals(other.fields, fields);

  @override
  int get hashCode => Object.hash(runtimeType, name, atMs,
      Object.hashAllUnordered(fields.keys), Object.hashAll(fields.values));

  static bool _mapEquals(Map<String, Object?> a, Map<String, Object?> b) {
    if (a.length != b.length) return false;
    for (final entry in a.entries) {
      if (!b.containsKey(entry.key) || b[entry.key] != entry.value) return false;
    }
    return true;
  }
}

/// `ak_task_start` finished the local bootstrap (`start` returning is NOT
/// this — the runtime id is valid but the plane announces itself here).
final class TaskRuntimeReadyEvent extends TaskEvent {
  const TaskRuntimeReadyEvent({super.atMs, required this.runtime});

  /// Hex runtime id (32 chars).
  final String runtime;

  @override
  String get name => 'runtime_ready';

  @override
  Map<String, Object?> get fields => {'runtime': runtime};
}

/// One side of a pairing learned its connection (both runtimes emit it).
final class TaskConnectedEvent extends TaskEvent {
  const TaskConnectedEvent({
    super.atMs,
    required this.conn,
    required this.role,
    this.peerEndpoint,
    this.code,
  });

  /// Hex connection id (32 chars) — the UI correlation key.
  final String conn;

  /// This runtime's role for the connection: `sender` | `receiver`.
  final String role;

  /// The peer's dial endpoint (`ip:port`) when known.
  final String? peerEndpoint;

  /// The task code that produced the connection, when known.
  final String? code;

  @override
  String get name => 'connected';

  @override
  Map<String, Object?> get fields => {
        'conn': conn,
        'role': role,
        if (peerEndpoint != null) 'peer_endpoint': peerEndpoint,
        if (code != null) 'code': code,
      };
}

/// Reachability of one connection endpoint (`reachable:null` while the
/// ak-session `probe_peer` slot is pending — WAVE-28/B-PENDING).
final class TaskProbeResultEvent extends TaskEvent {
  const TaskProbeResultEvent({
    super.atMs,
    required this.conn,
    this.endpoint,
    this.reachable,
  });

  final String conn;

  final String? endpoint;

  final bool? reachable;

  @override
  String get name => 'probe_result';

  @override
  Map<String, Object?> get fields => {
        'conn': conn,
        if (endpoint != null) 'endpoint': endpoint,
        if (reachable != null) 'reachable': reachable,
      };
}

/// An incoming transfer offer is waiting for the user's accept/decline
/// (`ak_task_consent`, 30 s auto-decline engine-side).
final class TaskIncomingRequestEvent extends TaskEvent {
  const TaskIncomingRequestEvent({
    super.atMs,
    required this.job,
    this.conn,
    this.rootName = '',
    this.files = 0,
    this.bytes = 0,
    this.saveDir,
  });

  /// Hex pending-request id (32 chars) — the `ak_task_consent` key.
  final String job;

  final String? conn;

  /// The sender's label for the payload (offer root name).
  final String rootName;

  final int files;

  final int bytes;

  /// The save directory THIS request will use, snapshotted native-side at
  /// offer time (wave-30 P2-06, wire key `saveDir`). The receive lands
  /// here even if the settings change before the user answers — the UI
  /// shows this value as the request's real target. Null = an engine
  /// predating the field (callers fall back to the live setting).
  final String? saveDir;

  @override
  String get name => 'incoming_request';

  @override
  Map<String, Object?> get fields => {
        'job': job,
        if (conn != null) 'conn': conn,
        'root': rootName,
        'files': files,
        'bytes': bytes,
        if (saveDir != null) 'saveDir': saveDir,
      };
}

/// A transfer finished successfully (either side).
final class TaskDoneEvent extends TaskEvent {
  const TaskDoneEvent({super.atMs, this.conn, this.files = 0, this.bytes = 0});

  final String? conn;

  final int files;

  final int bytes;

  @override
  String get name => 'done';

  @override
  Map<String, Object?> get fields => {
        if (conn != null) 'conn': conn,
        'files': files,
        'bytes': bytes,
      };
}

/// A connection went away. `local`/`peer`/`receiver_failed`/`done` are
/// DATA-PLANE terminal verdicts (`done` = the one-task transfer completed,
/// wave-30 P2-05); any other reason is a coordination-plane notice that
/// must NOT kill a healthy data connection (see the library doc, wave-29
/// P2-08).
final class TaskDisconnectedEvent extends TaskEvent {
  const TaskDisconnectedEvent({super.atMs, this.conn, required this.reason});

  final String? conn;

  final String reason;

  @override
  String get name => 'disconnected';

  @override
  Map<String, Object?> get fields => {
        if (conn != null) 'conn': conn,
        'reason': reason,
      };
}

/// The native 30 s consent window auto-declined a pending request
/// (wave-30 P2-11): the request is TERMINAL — a request-level fact from
/// the engine that times the window, replacing any Dart-side timer
/// mirror. A later `ak_task_consent` on [job] answers `-4` (unknown or
/// already decided).
final class TaskConsentExpiredEvent extends TaskEvent {
  const TaskConsentExpiredEvent({super.atMs, required this.job, this.conn});

  /// The hex pending-request id whose window closed undecided.
  final String job;

  /// The request's connection, when the engine could attribute it.
  final String? conn;

  @override
  String get name => 'consent_expired';

  @override
  Map<String, Object?> get fields => {
        'job': job,
        if (conn != null) 'conn': conn,
      };
}

/// One async task op settled (`op_done`/`op_failed`).
final class TaskOpSettledEvent extends TaskEvent {
  const TaskOpSettledEvent({
    super.atMs,
    required this.ok,
    required this.opId,
    required this.op,
  });

  final bool ok;

  /// Hex op id (32 chars).
  final String opId;

  /// `task_create_code` | `task_join` | `task_send`.
  final String op;

  @override
  String get name => ok ? 'op_done' : 'op_failed';

  @override
  Map<String, Object?> get fields => {'op_id': opId, 'op': op};
}

/// Forward-compatibility fallback: an engine newer than this binding
/// emitted an event name it does not know.
final class TaskUnknownEvent extends TaskEvent {
  const TaskUnknownEvent({super.atMs, required this.name, required this.fields});

  @override
  final String name;

  @override
  final Map<String, Object?> fields;
}

String _string(Map<String, Object?> json, String key) {
  final value = json[key];
  if (value is! String || value.isEmpty) {
    throw FormatException('任务事件 "$key" 字段缺失或类型错误');
  }
  return value;
}

String? _optionalString(Map<String, Object?> json, String key) {
  final value = json[key];
  if (value == null) return null;
  if (value is! String) {
    throw FormatException('任务事件 "$key" 字段类型错误');
  }
  return value.isEmpty ? null : value;
}

int _int(Map<String, Object?> json, String key) {
  final value = json[key];
  if (value is! int) {
    throw FormatException('任务事件 "$key" 字段缺失或类型错误');
  }
  return value;
}
