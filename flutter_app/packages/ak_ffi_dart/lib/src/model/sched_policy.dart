/// The two-dimensional scheduler concurrency policy of `ak_sched_set_policy`
/// (wave-22). The value object carries the full §3 vocabulary of
/// `docs/contracts/scheduler.md`; the WIRE keys are the subset the wave-22
/// Rust side actually parses (`crates/ak-ffi/src/pairing.rs`, `parse_policy`
/// — `same_code`/`cross_code` plus the three caps):
///
/// ```json
/// {
///   "same_code": "serial",
///   "cross_code": "parallel",
///   "max_active_tasks_per_code": 4,
///   "max_active_code_groups": 4,
///   "max_queued_tasks": 256
/// }
/// ```
///
/// The contract-doc spellings (`same_code_mode`/`cross_code_mode`, the
/// global `max_active_tasks`, per-code queue and admission caps) are NOT
/// parsed by the current engine — they stay on the value object (equality,
/// future engines) but not on the wire. [SchedulerPolicy.fromJson] accepts
/// BOTH spellings (the snapshot renders `same_code`/`cross_code`).
///
/// `toJson` validates the frozen ranges (scheduler.md §3) and throws
/// [AkSpecException] on violation — the same early-Dart-rejection pattern
/// as `AkSendSpec` (the Rust side stays the authority).
///
/// FFI-free plain data: unit-testable without the DLL.
library;

import '../ffi_json.dart' show AkSpecException;

/// One dimension of the two-mode concurrency matrix (scheduler.md §2):
/// `serial` limits a code group (same-code) or the active group set
/// (cross-code) to one at a time; `parallel` keeps only the resource caps.
enum SchedCodeMode {
  serial,
  parallel;

  /// The wire string of this mode.
  String get wireName => this == SchedCodeMode.serial ? 'serial' : 'parallel';

  /// Decodes the wire form; unknown values throw [FormatException].
  static SchedCodeMode fromWire(String wire) => switch (wire) {
        'serial' => SchedCodeMode.serial,
        'parallel' => SchedCodeMode.parallel,
        _ => throw FormatException('未知的调度模式 "$wire"'),
      };
}

/// Validated range table entry: [min]..=[max] inclusive (scheduler.md §3).
const Map<String, (int, int)> schedulerPolicyRanges = <String, (int, int)>{
  'max_active_tasks': (1, 64),
  'max_active_tasks_per_code': (1, 16),
  'max_active_code_groups': (1, 16),
  'max_queued_tasks': (1, 4096),
  'max_queued_tasks_per_code': (1, 512),
  'max_pending_admissions': (1, 64),
};

/// The scheduler policy value object (see the library doc for the JSON).
final class SchedulerPolicy {
  const SchedulerPolicy({
    this.sameCodeMode = SchedCodeMode.serial,
    this.crossCodeMode = SchedCodeMode.parallel,
    this.maxActiveTasks = 8,
    this.maxActiveTasksPerCode = 4,
    this.maxActiveCodeGroups = 4,
    this.maxQueuedTasks = 256,
    this.maxQueuedTasksPerCode = 64,
    this.maxPendingAdmissions = 32,
  });

  /// Tasks sharing one `(registry_id, code_id)` group run one at a time
  /// (default `serial`).
  final SchedCodeMode sameCodeMode;

  /// Distinct code groups enter the transfer service one at a time
  /// (default `parallel`).
  final SchedCodeMode crossCodeMode;

  /// Global active task cap, 1..=64 (default 8).
  final int maxActiveTasks;

  /// Per-code-group active task cap, 1..=16, must not exceed
  /// [maxActiveTasks] (default 4).
  final int maxActiveTasksPerCode;

  /// Concurrently active code groups, 1..=16 (default 4).
  final int maxActiveCodeGroups;

  /// Global queued task cap, 1..=4096 (default 256).
  final int maxQueuedTasks;

  /// Per-code queued task cap, 1..=512, must not exceed [maxQueuedTasks]
  /// (default 64).
  final int maxQueuedTasksPerCode;

  /// Concurrent pending task admissions, 1..=64 (default 32).
  final int maxPendingAdmissions;

  /// The mode pair as a compact label, e.g. `serial × parallel`
  /// (scheduler.md §2 matrix row).
  String get modeLabel => '${sameCodeMode.wireName} × ${crossCodeMode.wireName}';

  /// Copies this policy replacing the two modes (resource caps unchanged).
  SchedulerPolicy withModes({
    SchedCodeMode? sameCodeMode,
    SchedCodeMode? crossCodeMode,
  }) =>
      SchedulerPolicy(
        sameCodeMode: sameCodeMode ?? this.sameCodeMode,
        crossCodeMode: crossCodeMode ?? this.crossCodeMode,
        maxActiveTasks: maxActiveTasks,
        maxActiveTasksPerCode: maxActiveTasksPerCode,
        maxActiveCodeGroups: maxActiveCodeGroups,
        maxQueuedTasks: maxQueuedTasks,
        maxQueuedTasksPerCode: maxQueuedTasksPerCode,
        maxPendingAdmissions: maxPendingAdmissions,
      );

  /// Validates every field against the frozen ranges (including the two
  /// "not above the global cap" cross rules) and returns the WIRE JSON —
  /// the keys the engine parses (`same_code`/`cross_code` + the three
  /// caps; see the library doc). Throws [AkSpecException] on violation.
  Map<String, dynamic> toJson() {
    _check('max_active_tasks', maxActiveTasks);
    _check('max_active_tasks_per_code', maxActiveTasksPerCode);
    _check('max_active_code_groups', maxActiveCodeGroups);
    _check('max_queued_tasks', maxQueuedTasks);
    _check('max_queued_tasks_per_code', maxQueuedTasksPerCode);
    _check('max_pending_admissions', maxPendingAdmissions);
    if (maxActiveTasksPerCode > maxActiveTasks) {
      throw const AkSpecException(
          '字段 "max_active_tasks_per_code" 不能超过 "max_active_tasks"');
    }
    if (maxQueuedTasksPerCode > maxQueuedTasks) {
      throw const AkSpecException(
          '字段 "max_queued_tasks_per_code" 不能超过 "max_queued_tasks"');
    }
    return <String, dynamic>{
      'same_code': sameCodeMode.wireName,
      'cross_code': crossCodeMode.wireName,
      'max_active_tasks_per_code': maxActiveTasksPerCode,
      'max_active_code_groups': maxActiveCodeGroups,
      'max_queued_tasks': maxQueuedTasks,
    };
  }

  static void _check(String field, int value) {
    final (min, max) = schedulerPolicyRanges[field]!;
    if (value < min || value > max) {
      throw AkSpecException('字段 "$field" 必须在 $min..=$max 之间，实际为 $value');
    }
  }

  /// Decodes a policy from an `ak_sched_snapshot` `policy` object (engine
  /// keys `same_code`/`cross_code`) or any future `same_code_mode`/
  /// `cross_code_mode` spelling. Tolerant: missing optional fields keep
  /// the defaults; an unknown mode value throws [FormatException] (never
  /// silently a wrong mode).
  factory SchedulerPolicy.fromJson(Map<String, Object?> json) =>
      SchedulerPolicy(
        sameCodeMode: _modeOf(json, 'same_code', 'same_code_mode',
            SchedCodeMode.serial),
        crossCodeMode: _modeOf(json, 'cross_code', 'cross_code_mode',
            SchedCodeMode.parallel),
        maxActiveTasks: _intOr(json, 'max_active_tasks', 8),
        maxActiveTasksPerCode: _intOr(json, 'max_active_tasks_per_code', 4),
        maxActiveCodeGroups: _intOr(json, 'max_active_code_groups', 4),
        maxQueuedTasks: _intOr(json, 'max_queued_tasks', 256),
        maxQueuedTasksPerCode: _intOr(json, 'max_queued_tasks_per_code', 64),
        maxPendingAdmissions: _intOr(json, 'max_pending_admissions', 32),
      );

  static SchedCodeMode _modeOf(Map<String, Object?> json, String wireKey,
      String contractKey, SchedCodeMode fallback) {
    final wire = json[wireKey];
    if (wire is String && wire.isNotEmpty) {
      return SchedCodeMode.fromWire(wire);
    }
    final contract = json[contractKey];
    if (contract is String && contract.isNotEmpty) {
      return SchedCodeMode.fromWire(contract);
    }
    return fallback;
  }

  static int _intOr(Map<String, Object?> json, String key, int fallback) =>
      json[key] is int && (json[key] as int) > 0 ? json[key] as int : fallback;

  @override
  bool operator ==(Object other) =>
      other is SchedulerPolicy &&
      other.sameCodeMode == sameCodeMode &&
      other.crossCodeMode == crossCodeMode &&
      other.maxActiveTasks == maxActiveTasks &&
      other.maxActiveTasksPerCode == maxActiveTasksPerCode &&
      other.maxActiveCodeGroups == maxActiveCodeGroups &&
      other.maxQueuedTasks == maxQueuedTasks &&
      other.maxQueuedTasksPerCode == maxQueuedTasksPerCode &&
      other.maxPendingAdmissions == maxPendingAdmissions;

  @override
  int get hashCode => Object.hash(
        sameCodeMode,
        crossCodeMode,
        maxActiveTasks,
        maxActiveTasksPerCode,
        maxActiveCodeGroups,
        maxQueuedTasks,
        maxQueuedTasksPerCode,
        maxPendingAdmissions,
      );

  @override
  String toString() => 'SchedulerPolicy($modeLabel, active<=$maxActiveTasks, '
      'queued<=$maxQueuedTasks)';
}
