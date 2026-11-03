/// Final job result models (`ak_job_result`) plus the formatting helpers
/// shared by UI surfaces (duration, byte sizes, speed).
///
/// The engine writes two success bodies (crates/ak-ffi/src/core.rs):
///
/// * single connection —
///   `{"job","wire_job","files","bytes","duration_ms","resumed","peer","roots"}`
/// * concurrent receive (`once_jobs > 1`) —
///   `{"job","jobs","wire_jobs",..,"files","bytes","duration_ms","resumed","roots"}`
///
/// Failure bodies are `{"job","error"}`, with `"jobs"/"ok"/"failed"/
/// "errors"` added on a concurrent receive. [TransferReport.fromJson]
/// accepts both success shapes: `wire_job`/`peer` are nullable and the
/// multi-connection fields are optional. `files`/`bytes`/`duration_ms`
/// are required and must be non-negative integers; anything else throws
/// [FormatException]. Unknown extra fields are ignored (forward
/// compatibility, mirroring the engine's own parser policy).
library;

/// Byte-size formatting bit-for-bit aligned with ak-cli `human_bytes`:
/// binary units (B, KiB, MiB, GiB, TiB, PiB), plain integer for B, two
/// decimals otherwise, never a unit beyond PiB (values above 1 PiB keep
/// growing in PiB, exactly like the Rust loop).
///
/// ```text
/// humanBytes(0)        == "0 B"
/// humanBytes(1023)     == "1023 B"
/// humanBytes(1024)     == "1.00 KiB"
/// humanBytes(1536)     == "1.50 KiB"
/// humanBytes(1 << 30)  == "1.00 GiB"
/// ```
String humanBytes(int bytes) {
  if (bytes < 0) {
    throw ArgumentError.value(bytes, 'bytes', 'must be non-negative');
  }
  return _humanBytesOf(bytes.toDouble(), integralBase: bytes);
}

/// Duration as `mm:ss` (e.g. `01:21` for 81.2 s). Sub-second remainder is
/// truncated, minutes are *not* wrapped at 60 — a 62-minute job renders
/// `62:03` — and negative input clamps to zero.
String formatDurationMs(int durationMs) {
  final totalSeconds = durationMs <= 0 ? 0 : durationMs ~/ 1000;
  final minutes = totalSeconds ~/ 60;
  final seconds = totalSeconds % 60;
  return '${minutes.toString().padLeft(2, '0')}:${seconds.toString().padLeft(2, '0')}';
}

/// Average transfer rate in bytes/second; `null` while the duration is
/// zero (an instant loopback transfer has no meaningful rate).
double? averageBytesPerSecond(int bytes, int durationMs) {
  if (durationMs <= 0) return null;
  return bytes / (durationMs / 1000);
}

/// Speed text such as `1.00 MiB/s` (same binary units as [humanBytes]);
/// `null` when the rate is undefined.
String? formatSpeed(int bytes, int durationMs) {
  final rate = averageBytesPerSecond(bytes, durationMs);
  if (rate == null) return null;
  return '${_humanBytesOf(rate)}/s';
}

/// Shared loop of [humanBytes]/[formatSpeed]. [integralBase] lets the
/// plain-B branch print the exact integer the caller passed.
String _humanBytesOf(double value, {int? integralBase}) {
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'];
  var unit = 0;
  while (value >= 1024 && unit + 1 < units.length) {
    value /= 1024;
    unit += 1;
  }
  if (unit == 0) {
    final base = integralBase ?? value.round();
    return '$base B';
  }
  return '${value.toStringAsFixed(2)} ${units[unit]}';
}

/// Success body of `ak_job_result` (single or aggregate).
class TransferReport {
  const TransferReport({
    required this.job,
    required this.files,
    required this.bytes,
    required this.durationMs,
    this.wireJob,
    this.resumed = 0,
    this.peer,
    this.roots = const [],
    this.jobs,
    this.wireJobs,
  });

  /// Decodes one success body (either engine shape).
  factory TransferReport.fromJson(Map<String, Object?> json) {
    final job = _requiredString(json, 'job');
    final files = _requiredInt(json, 'files');
    final finalBytes = _requiredInt(json, 'bytes');
    final durationMs = _requiredInt(json, 'duration_ms');
    final resumedRaw = json['resumed'];
    if (resumedRaw != null && (resumedRaw is! int || resumedRaw < 0)) {
      throw const FormatException('field "resumed" must be a non-negative integer');
    }
    final jobsRaw = json['jobs'];
    if (jobsRaw != null && (jobsRaw is! int || jobsRaw < 0)) {
      throw const FormatException('field "jobs" must be a non-negative integer');
    }
    final wireJobRaw = json['wire_job'];
    if (wireJobRaw != null && wireJobRaw is! String) {
      throw const FormatException('field "wire_job" must be a string');
    }
    final peerRaw = json['peer'];
    if (peerRaw != null && peerRaw is! String) {
      throw const FormatException('field "peer" must be a string or null');
    }
    return TransferReport(
      job: job,
      files: files,
      bytes: finalBytes,
      durationMs: durationMs,
      wireJob: wireJobRaw as String?,
      resumed: (resumedRaw as int?) ?? 0,
      peer: peerRaw as String?,
      roots: _stringList(json, 'roots'),
      jobs: jobsRaw as int?,
      wireJobs: _optionalStringList(json, 'wire_jobs'),
    );
  }

  /// Opaque FFI job id (correlates with progress-event `job`).
  final String job;

  /// Wire-protocol job id (32 hex chars); `null` on the aggregate
  /// multi-connection body, which carries [wireJobs] instead.
  final String? wireJob;

  /// Files transferred (sum over connections on the aggregate body).
  final int files;

  /// Bytes transferred (sum over connections on the aggregate body).
  final int bytes;

  /// Wall-clock duration; max over connections on the aggregate body.
  final int durationMs;

  /// Chunks reused from a checkpoint (sum over connections).
  final int resumed;

  /// Peer fingerprint (64 hex chars) or `null` when the engine did not
  /// report one (aggregate body).
  final String? peer;

  /// Committed destination roots (e.g. `["dir", "extra"]`); empty on the
  /// aggregate body.
  final List<String> roots;

  /// Connection count of a concurrent receive; `null` otherwise.
  final int? jobs;

  /// Per-connection wire job ids of a concurrent receive; `null` otherwise.
  final List<String>? wireJobs;

  /// `durationMs` as `mm:ss`.
  String get durationText => formatDurationMs(durationMs);

  /// [bytes] as `1.20 GiB`-style text.
  String get bytesText => humanBytes(bytes);

  /// Average rate in bytes/second, `null` when duration is zero.
  double? get bytesPerSecond => averageBytesPerSecond(bytes, durationMs);

  /// Average rate as `1.00 MiB/s`-style text, `null` when undefined.
  String? get speedText => formatSpeed(bytes, durationMs);

  @override
  String toString() =>
      'TransferReport(job=$job wireJob=$wireJob files=$files bytes=$bytes '
      'durationMs=$durationMs resumed=$resumed peer=$peer roots=$roots '
      'jobs=$jobs wireJobs=$wireJobs)';

  @override
  bool operator ==(Object other) =>
      other is TransferReport &&
      other.job == job &&
      other.wireJob == wireJob &&
      other.files == files &&
      other.bytes == bytes &&
      other.durationMs == durationMs &&
      other.resumed == resumed &&
      other.peer == peer &&
      _listsEqual(other.roots, roots) &&
      other.jobs == jobs &&
      _nullableListsEqual(other.wireJobs, wireJobs);

  @override
  int get hashCode => Object.hash(
        job,
        wireJob,
        files,
        bytes,
        durationMs,
        resumed,
        peer,
        Object.hashAll(roots),
        jobs,
        wireJobs == null ? null : Object.hashAll(wireJobs!),
      );
}

/// Terminal outcome of `ak_job_result`: success body or failure body.
sealed class JobOutcome {
  const JobOutcome();

  /// Picks [JobSucceeded] or [JobFailed] by presence of `"error"`.
  factory JobOutcome.fromJson(Map<String, Object?> json) {
    if (json['error'] != null) {
      return JobFailed.fromJson(json);
    }
    return JobSucceeded(TransferReport.fromJson(json));
  }

  /// Opaque FFI job id of the finished job.
  String get job;
}

/// `ak_job_result` status 1: finished ok, [report] holds the totals.
final class JobSucceeded implements JobOutcome {
  const JobSucceeded(this.report);

  final TransferReport report;

  @override
  String get job => report.job;

  @override
  String toString() => 'JobSucceeded($report)';
}

/// `ak_job_result` status 2: finished with [error]. A concurrent receive
/// adds [jobs]/[ok]/[failed] counts and per-connection [errors] (up to 8,
/// mirroring the engine).
final class JobFailed implements JobOutcome {
  const JobFailed({
    required this.job,
    required this.error,
    this.jobs,
    this.ok,
    this.failed,
    this.errors = const [],
  });

  factory JobFailed.fromJson(Map<String, Object?> json) {
    final errorRaw = json['error'];
    if (errorRaw is! String) {
      throw const FormatException('field "error" must be a string');
    }
    final counts = <String, int?>{};
    for (final key in const ['jobs', 'ok', 'failed']) {
      final value = json[key];
      if (value != null && (value is! int || value < 0)) {
        throw FormatException('field "$key" must be a non-negative integer');
      }
      counts[key] = value as int?;
    }
    return JobFailed(
      job: _requiredString(json, 'job'),
      error: errorRaw,
      jobs: counts['jobs'],
      ok: counts['ok'],
      failed: counts['failed'],
      errors: _stringList(json, 'errors'),
    );
  }

  @override
  final String job;

  /// First error message (the aggregate failure reason).
  final String error;

  /// Expected connection count of a concurrent receive; `null` otherwise.
  final int? jobs;

  /// Connections that finished ok (concurrent receive only).
  final int? ok;

  /// Connections that failed (concurrent receive only).
  final int? failed;

  /// Per-connection error messages (concurrent receive only, max 8).
  final List<String> errors;

  @override
  String toString() =>
      'JobFailed(job=$job error=$error jobs=$jobs ok=$ok failed=$failed '
      'errors=$errors)';
}

String _requiredString(Map<String, Object?> json, String key) {
  final value = json[key];
  if (value is! String) {
    throw FormatException(
      'field "$key" must be a string${value == null ? ' (missing)' : ''}',
    );
  }
  return value;
}

int _requiredInt(Map<String, Object?> json, String key) {
  final value = json[key];
  if (value is! int || value < 0) {
    throw FormatException(
      'field "$key" must be a non-negative integer'
      '${value == null ? ' (missing)' : ''}',
    );
  }
  return value;
}

List<String> _stringList(Map<String, Object?> json, String key) {
  final value = json[key];
  if (value == null) return const [];
  if (value is! List) {
    throw FormatException('field "$key" must be an array of strings');
  }
  return _checkedStrings(value, key);
}

List<String>? _optionalStringList(Map<String, Object?> json, String key) {
  final value = json[key];
  if (value == null) return null;
  if (value is! List) {
    throw FormatException('field "$key" must be an array of strings');
  }
  return _checkedStrings(value, key);
}

List<String> _checkedStrings(List<dynamic> values, String key) {
  return List.unmodifiable(values.map((element) {
    if (element is! String) {
      throw FormatException('field "$key" must contain only strings');
    }
    return element;
  }));
}

bool _listsEqual(List<String> a, List<String> b) {
  if (a.length != b.length) return false;
  for (var i = 0; i < a.length; i++) {
    if (a[i] != b[i]) return false;
  }
  return true;
}

bool _nullableListsEqual(List<String>? a, List<String>? b) {
  if (a == null || b == null) return identical(a, b);
  return _listsEqual(a, b);
}
