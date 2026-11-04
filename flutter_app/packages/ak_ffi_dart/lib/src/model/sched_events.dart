/// Typed model for the scheduler events polled via `ak_sched_poll_event`
/// (wave-22). Like the pairing queue, this is a SEPARATE poll loop from the
/// transfer progress queue (ffi-v2.md §5).
///
/// Wire shape — the wave-22 Rust side (crates/ak-ffi/src/pairing.rs,
/// `scheduler_event_json`/`sched_event_json`):
///
/// ```json
/// {"event":"task_queued","task":"<32hex>","group":"<32hex>"}
/// {"event":"task_admitted","task":"<32hex>","group":"<32hex>"}
/// {"event":"task_started","task":"<32hex>"}
/// {"event":"task_finished","task":"<32hex>","group":"<32hex>","outcome":"completed"}
/// {"event":"task_finished","task":"<32hex>","group":"<32hex>","outcome":"failed","error":"..."}
/// {"event":"task_cancelled","task":"<32hex>","group":"<32hex>","was_running":false}
/// {"event":"policy_changed","revision":3,"draining":false}
/// {"event":"draining","active_tasks":2}
/// ```
///
/// Session-level lines (transport_disconnected, backoff_scheduled,
/// task_parked, inbound_submitted, redeem_offered, shutdown_*, ...) decode
/// to [SchedulerUnknownEvent] — they stay visible without forcing every
/// consumer to know them.
///
/// Parsing policy mirrors `pairing_events.dart` / `progress_events.dart`:
/// unknown event names decode to [SchedulerUnknownEvent]; a missing or
/// wrongly typed payload field throws [FormatException].
library;

/// Base of the seven typed scheduler events plus the unknown fallback.
sealed class SchedulerEvent {
  const SchedulerEvent();

  /// The `"event"` name as it appears on the wire.
  String get name;

  /// Decodes one `ak_sched_poll_event` line.
  factory SchedulerEvent.fromJson(Map<String, Object?> json) {
    final event = _string(json, 'event');
    switch (event) {
      case 'task_queued':
        return TaskQueuedEvent(
          taskId: _string(json, 'task'),
          codeId: _optionalString(json, 'group'),
        );
      case 'task_admitted':
        return AdmittedEvent(
          taskId: _string(json, 'task'),
          codeId: _optionalString(json, 'group'),
        );
      case 'task_started':
        return StartedEvent(taskId: _string(json, 'task'));
      case 'task_finished':
        return FinishedEvent(
          taskId: _string(json, 'task'),
          codeId: _optionalString(json, 'group'),
          outcome: _string(json, 'outcome'),
          error: _optionalString(json, 'error'),
        );
      case 'task_cancelled':
        return CancelledEvent(
          taskId: _string(json, 'task'),
          codeId: _optionalString(json, 'group'),
          wasRunning: json['was_running'] is bool
              ? json['was_running'] as bool
              : null,
        );
      case 'policy_changed':
        return PolicyChangedEvent(
          revision: _int(json, 'revision'),
          draining: json['draining'] is bool ? json['draining'] as bool : false,
        );
      case 'draining':
        return DrainingEvent(activeTasks: _int(json, 'active_tasks'));
      default:
        return SchedulerUnknownEvent(
            name: event, fields: Map.of(json)..remove('event'));
    }
  }

  @override
  String toString() {
    final parts = <String>[
      name,
      for (final entry in describe.entries) '${entry.key}=${entry.value}',
    ];
    return '$runtimeType(${parts.join(' ')})';
  }

  /// Payload fields used for equality/toString.
  Map<String, Object?> get describe => const {};

  @override
  bool operator ==(Object other) =>
      other.runtimeType == runtimeType &&
      other is SchedulerEvent &&
      other.name == name &&
      _mapEquals(other.describe, describe);

  @override
  int get hashCode => Object.hash(runtimeType, name,
      Object.hashAllUnordered(describe.keys), Object.hashAll(describe.values));

  static bool _mapEquals(Map<String, Object?> a, Map<String, Object?> b) {
    if (a.length != b.length) {
      return false;
    }
    for (final entry in a.entries) {
      if (!b.containsKey(entry.key) || b[entry.key] != entry.value) {
        return false;
      }
    }
    return true;
  }
}

/// A transfer task entered the local queue (the `task` id is the stable
/// transfer instance identity, scheduler.md §11).
final class TaskQueuedEvent extends SchedulerEvent {
  const TaskQueuedEvent({required this.taskId, this.codeId});

  final String taskId;

  /// The code group (`group`) the task belongs to, when resolved.
  final String? codeId;

  @override
  String get name => 'task_queued';

  @override
  Map<String, Object?> get describe => {
        'task': taskId,
        if (codeId != null) 'group': codeId,
      };
}

/// Both ends granted the task admission permits.
final class AdmittedEvent extends SchedulerEvent {
  const AdmittedEvent({required this.taskId, this.codeId});

  final String taskId;

  final String? codeId;

  @override
  String get name => 'task_admitted';

  @override
  Map<String, Object?> get describe => {
        'task': taskId,
        if (codeId != null) 'group': codeId,
      };
}

/// The task began moving file data.
final class StartedEvent extends SchedulerEvent {
  const StartedEvent({required this.taskId});

  final String taskId;

  @override
  String get name => 'task_started';

  @override
  Map<String, Object?> get describe => {'task': taskId};
}

/// The task reached a terminal `outcome` (`completed`/`failed`); terminal
/// states are always re-fetchable via `ak_sched_snapshot`
/// (scheduler.md §7).
final class FinishedEvent extends SchedulerEvent {
  const FinishedEvent({
    required this.taskId,
    this.codeId,
    required this.outcome,
    this.error,
  });

  final String taskId;

  final String? codeId;

  final String outcome;

  /// Engine error string of a failed outcome.
  final String? error;

  @override
  String get name => 'task_finished';

  @override
  Map<String, Object?> get describe => {
        'task': taskId,
        if (codeId != null) 'group': codeId,
        'outcome': outcome,
        if (error != null) 'error': error,
      };
}

/// The task was cancelled (terminal; checkpoint retained per the cancel
/// layers, scheduler.md §10).
final class CancelledEvent extends SchedulerEvent {
  const CancelledEvent({required this.taskId, this.codeId, this.wasRunning});

  final String taskId;

  final String? codeId;

  /// Whether the cancel hit a running transfer (vs. a queued removal).
  final bool? wasRunning;

  @override
  String get name => 'task_cancelled';

  @override
  Map<String, Object?> get describe => {
        'task': taskId,
        if (codeId != null) 'group': codeId,
        if (wasRunning != null) 'was_running': wasRunning,
      };
}

/// An `ak_sched_set_policy` took effect.
final class PolicyChangedEvent extends SchedulerEvent {
  const PolicyChangedEvent({required this.revision, required this.draining});

  /// The policy revision after the change.
  final int revision;

  /// True while a `parallel → serial` transition is draining running tasks
  /// (scheduler.md §8). The EFFECTIVE mode values live in
  /// `ak_sched_snapshot`'s `policy` object (both ends intersect).
  final bool draining;

  @override
  String get name => 'policy_changed';

  @override
  Map<String, Object?> get describe => {
        'revision': revision,
        'draining': draining,
      };
}

/// A `parallel → serial` transition entered draining: no new over-limit
/// tasks, running ones finish naturally (scheduler.md §8).
final class DrainingEvent extends SchedulerEvent {
  const DrainingEvent({required this.activeTasks});

  final int activeTasks;

  @override
  String get name => 'draining';

  @override
  Map<String, Object?> get describe => {'active_tasks': activeTasks};
}

/// Forward-compatibility fallback: session-level lines and any engine
/// newer than this binding.
final class SchedulerUnknownEvent extends SchedulerEvent {
  const SchedulerUnknownEvent({required this.name, this.fields});

  @override
  final String name;

  /// The raw payload (minus `event`) for diagnostics.
  final Map<String, Object?>? fields;

  @override
  Map<String, Object?> get describe => fields ?? const {};
}

String _string(Map<String, Object?> json, String key) {
  final value = json[key];
  if (value is! String || value.isEmpty) {
    throw FormatException('调度事件 "$key" 字段缺失或类型错误');
  }
  return value;
}

String? _optionalString(Map<String, Object?> json, String key) {
  final value = json[key];
  if (value == null) {
    return null;
  }
  if (value is! String || value.isEmpty) {
    throw FormatException('调度事件 "$key" 字段类型错误');
  }
  return value;
}

int _int(Map<String, Object?> json, String key) {
  final value = json[key];
  if (value is! int || value < 0) {
    throw FormatException('调度事件 "$key" 字段缺失或不是非负整数');
  }
  return value;
}
