/// Typed model for the 19 progress-event JSON lines emitted by the ak-ffi
/// C ABI (`ak_poll_progress`).
///
/// Contract (crates/ak-ffi/src/lib.rs, "# progress event lines"): one
/// compact JSON object per poll, lowercase keys, never a raw newline.
/// Every line carries `"job"` (the opaque FFI job id — correlate it with
/// the id returned by `ak_send_start`/`ak_receive_start`, not the
/// wire-protocol id). Receive lines from a multi-connection job
/// (`once_jobs > 1`) additionally carry `"conn":<index>` directly after
/// `"job"`; sends and single-connection receives carry no `"conn"` (the
/// synthetic `listening` event is only emitted on the single-connection
/// receive path, so it never carries `conn` either).
///
/// Field names and types below mirror `event_name`/`event_fields` in
/// crates/ak-ffi/src/core.rs verbatim.
///
/// Parsing policy: an unknown `"event"` name decodes to [UnknownEvent]
/// instead of throwing (forward compatibility with newer engines — the
/// engine's own spec parser likewise ignores unknown fields). A missing or
/// wrongly typed *payload* field throws [FormatException] because it means
/// the contract is broken, not merely newer.
library;

import 'dart:convert';

/// Every event name the engine emits today, in contract order.
const List<String> knownEventNames = <String>[
  'connecting',
  'listening',
  'connected',
  'offered',
  'accepted',
  'manifest_page_sent',
  'manifest_received',
  'resuming',
  'file_started',
  'chunk_sent',
  'chunk_received',
  'bytes_acked',
  'paused',
  'resumed',
  'file_verified',
  'file_committed',
  'verifying',
  'final_ack',
  'done',
];

/// Base of the 19 typed progress events plus the [UnknownEvent] fallback.
///
/// Instances compare by runtime type, [name], [job], [conn] and [fields].
sealed class ProgressEvent {
  const ProgressEvent({required this.job, this.conn});

  /// Opaque FFI job id (32 hex chars) as it appeared on the line.
  final String job;

  /// Connection index on multi-connection receives (`once_jobs > 1`);
  /// `null` on sends and single-connection receives.
  final int? conn;

  /// The `"event"` name as it appears on the wire.
  String get name;

  /// Payload fields (everything except `job`/`conn`/`event`), used for
  /// equality, hashing and [toString].
  Map<String, Object?> get fields => const {};

  /// Decodes one decoded progress line (see [parse] for the string form).
  ///
  /// Throws [FormatException] when `job`/`event` or a payload field is
  /// missing or has the wrong type; unknown event names never throw.
  factory ProgressEvent.fromJson(Map<String, Object?> json) {
    final event = _requiredString(json, 'event');
    final job = _requiredString(json, 'job');
    final conn = _optionalConn(json);
    switch (event) {
      case 'connecting':
        return Connecting(job: job, conn: conn);
      case 'listening':
        return Listening(job: job, conn: conn, addr: _requiredString(json, 'addr'));
      case 'connected':
        return Connected(job: job, conn: conn, peer: _requiredString(json, 'peer'));
      case 'offered':
        return Offered(
          job: job,
          conn: conn,
          files: _requiredInt(json, 'files'),
          bytes: _requiredInt(json, 'bytes'),
        );
      case 'accepted':
        return Accepted(
          job: job,
          conn: conn,
          files: _requiredInt(json, 'files'),
          bytes: _requiredInt(json, 'bytes'),
        );
      case 'manifest_page_sent':
        return ManifestPageSent(job: job, conn: conn, page: _requiredInt(json, 'page'));
      case 'manifest_received':
        return ManifestReceived(job: job, conn: conn);
      case 'resuming':
        return Resuming(job: job, conn: conn, chunks: _requiredInt(json, 'chunks'));
      case 'file_started':
        return FileStarted(
          job: job,
          conn: conn,
          file: _requiredInt(json, 'file'),
          path: _requiredString(json, 'path'),
          bytes: _requiredInt(json, 'bytes'),
        );
      case 'chunk_sent':
        return ChunkSent(
          job: job,
          conn: conn,
          file: _requiredInt(json, 'file'),
          chunk: _requiredInt(json, 'chunk'),
        );
      case 'chunk_received':
        return ChunkReceived(
          job: job,
          conn: conn,
          file: _requiredInt(json, 'file'),
          chunk: _requiredInt(json, 'chunk'),
        );
      case 'bytes_acked':
        return BytesAcked(
          job: job,
          conn: conn,
          file: _requiredInt(json, 'file'),
          bytes: _requiredInt(json, 'bytes'),
        );
      case 'paused':
        return Paused(job: job, conn: conn);
      case 'resumed':
        return Resumed(job: job, conn: conn);
      case 'file_verified':
        return FileVerified(job: job, conn: conn, file: _requiredInt(json, 'file'));
      case 'file_committed':
        return FileCommitted(
          job: job,
          conn: conn,
          file: _requiredInt(json, 'file'),
          path: _requiredString(json, 'path'),
        );
      case 'verifying':
        return Verifying(job: job, conn: conn);
      case 'final_ack':
        return FinalAck(job: job, conn: conn, verified: _requiredBool(json, 'verified'));
      case 'done':
        return Done(job: job, conn: conn);
      default:
        final data = Map<String, Object?>.of(json)
          ..remove('job')
          ..remove('conn')
          ..remove('event');
        return UnknownEvent(job: job, conn: conn, name: event, data: data);
    }
  }

  /// Decodes one raw progress line exactly as `ak_poll_progress` wrote it.
  ///
  /// Throws [FormatException] on invalid JSON or a non-object line;
  /// payload validation follows [ProgressEvent.fromJson].
  factory ProgressEvent.parse(String line) {
    final Object? decoded;
    try {
      decoded = jsonDecode(line);
    } on FormatException {
      rethrow;
    }
    if (decoded is! Map<String, Object?>) {
      throw const FormatException('progress event line must be a JSON object');
    }
    return ProgressEvent.fromJson(decoded);
  }

  @override
  bool operator ==(Object other) {
    if (identical(this, other)) return true;
    return other is ProgressEvent &&
        other.runtimeType == runtimeType &&
        other.name == name &&
        other.job == job &&
        other.conn == conn &&
        _mapsEqual(other.fields, fields);
  }

  @override
  int get hashCode {
    // XOR of per-entry hashes: order-independent, so two equal maps built
    // in different insertion orders still share a hashCode.
    var fieldsHash = 0;
    fields.forEach((key, value) => fieldsHash ^= Object.hash(key, value));
    return Object.hash(runtimeType, name, job, conn, fieldsHash);
  }

  @override
  String toString() {
    final connPart = conn == null ? '' : ' conn=$conn';
    final fieldPart = fields.isEmpty
        ? ''
        : ' ${fields.entries.map((entry) => '${entry.key}=${entry.value}').join(' ')}';
    return '$name job=$job$connPart$fieldPart';
  }
}

/// Dialing / binding: the job is starting. Send and receive both emit it.
final class Connecting extends ProgressEvent {
  const Connecting({required super.job, super.conn});

  @override
  String get name => 'connecting';

  @override
  Map<String, Object?> get fields => const {};
}

/// Receive-only, single connection (`once_jobs == 1`): the listen endpoint
/// is bound and [addr] can be surfaced (host:port).
final class Listening extends ProgressEvent {
  const Listening({required super.job, super.conn, required this.addr});

  /// Bound endpoint as `host:port` (e.g. `127.0.0.1:52341`).
  final String addr;

  @override
  String get name => 'listening';

  @override
  Map<String, Object?> get fields => {'addr': addr};
}

/// Transport established; [peer] is the peer's 64-hex fingerprint.
final class Connected extends ProgressEvent {
  const Connected({required super.job, super.conn, required this.peer});

  /// Peer fingerprint, 64 lowercase hex chars.
  final String peer;

  @override
  String get name => 'connected';

  @override
  Map<String, Object?> get fields => {'peer': peer};
}

/// Sender side: the offer was sent ([files] files, [bytes] total bytes).
final class Offered extends ProgressEvent {
  const Offered({required super.job, super.conn, required this.files, required this.bytes});

  /// Number of files offered.
  final int files;

  /// Total bytes offered.
  final int bytes;

  @override
  String get name => 'offered';

  @override
  Map<String, Object?> get fields => {'files': files, 'bytes': bytes};
}

/// Receiver side: the offer was accepted ([files] files, [bytes] bytes).
final class Accepted extends ProgressEvent {
  const Accepted({required super.job, super.conn, required this.files, required this.bytes});

  /// Number of files accepted.
  final int files;

  /// Total bytes accepted.
  final int bytes;

  @override
  String get name => 'accepted';

  @override
  Map<String, Object?> get fields => {'files': files, 'bytes': bytes};
}

/// Sender side: manifest page [page] (0-based) was sent.
final class ManifestPageSent extends ProgressEvent {
  const ManifestPageSent({required super.job, super.conn, required this.page});

  /// 0-based manifest page index.
  final int page;

  @override
  String get name => 'manifest_page_sent';

  @override
  Map<String, Object?> get fields => {'page': page};
}

/// Receiver side: the full manifest arrived.
final class ManifestReceived extends ProgressEvent {
  const ManifestReceived({required super.job, super.conn});

  @override
  String get name => 'manifest_received';

  @override
  Map<String, Object?> get fields => const {};
}

/// A checkpoint exists: [chunks] chunks were already transferred earlier
/// and the session resumes instead of starting clean.
final class Resuming extends ProgressEvent {
  const Resuming({required super.job, super.conn, required this.chunks});

  /// Number of chunks already present from the earlier session.
  final int chunks;

  @override
  String get name => 'resuming';

  @override
  Map<String, Object?> get fields => {'chunks': chunks};
}

/// Transfer of file [file] (job-relative [path], [bytes] long) begins.
final class FileStarted extends ProgressEvent {
  const FileStarted({
    required super.job,
    super.conn,
    required this.file,
    required this.path,
    required this.bytes,
  });

  /// 0-based file index within the job.
  final int file;

  /// Job-relative path, forward slashes, e.g. `photos/img.bin`.
  final String path;

  /// Total size of this file in bytes.
  final int bytes;

  @override
  String get name => 'file_started';

  @override
  Map<String, Object?> get fields => {'file': file, 'path': path, 'bytes': bytes};
}

/// Sender side: chunk [chunk] of file [file] was sent.
final class ChunkSent extends ProgressEvent {
  const ChunkSent({
    required super.job,
    super.conn,
    required this.file,
    required this.chunk,
  });

  /// 0-based file index.
  final int file;

  /// 0-based chunk index within the file.
  final int chunk;

  @override
  String get name => 'chunk_sent';

  @override
  Map<String, Object?> get fields => {'file': file, 'chunk': chunk};
}

/// Receiver side: chunk [chunk] of file [file] arrived.
final class ChunkReceived extends ProgressEvent {
  const ChunkReceived({
    required super.job,
    super.conn,
    required this.file,
    required this.chunk,
  });

  /// 0-based file index.
  final int file;

  /// 0-based chunk index within the file.
  final int chunk;

  @override
  String get name => 'chunk_received';

  @override
  Map<String, Object?> get fields => {'file': file, 'chunk': chunk};
}

/// Sender side: receiver acknowledged [bytes] cumulative bytes of [file].
final class BytesAcked extends ProgressEvent {
  const BytesAcked({
    required super.job,
    super.conn,
    required this.file,
    required this.bytes,
  });

  /// 0-based file index.
  final int file;

  /// Cumulative acknowledged bytes for this file (monotonic per session).
  final int bytes;

  @override
  String get name => 'bytes_acked';

  @override
  Map<String, Object?> get fields => {'file': file, 'bytes': bytes};
}

/// Flow control paused the transfer (idle timeout throttle).
final class Paused extends ProgressEvent {
  const Paused({required super.job, super.conn});

  @override
  String get name => 'paused';

  @override
  Map<String, Object?> get fields => const {};
}

/// Transfer resumed after [Paused].
final class Resumed extends ProgressEvent {
  const Resumed({required super.job, super.conn});

  @override
  String get name => 'resumed';

  @override
  Map<String, Object?> get fields => const {};
}

/// File [file] passed its content hash check.
final class FileVerified extends ProgressEvent {
  const FileVerified({required super.job, super.conn, required this.file});

  /// 0-based file index.
  final int file;

  @override
  String get name => 'file_verified';

  @override
  Map<String, Object?> get fields => {'file': file};
}

/// File [file] was committed at [path] (final name on disk).
final class FileCommitted extends ProgressEvent {
  const FileCommitted({
    required super.job,
    super.conn,
    required this.file,
    required this.path,
  });

  /// 0-based file index.
  final int file;

  /// Committed job-relative path.
  final String path;

  @override
  String get name => 'file_committed';

  @override
  Map<String, Object?> get fields => {'file': file, 'path': path};
}

/// Job-level verification phase begins (after per-file chunking).
final class Verifying extends ProgressEvent {
  const Verifying({required super.job, super.conn});

  @override
  String get name => 'verifying';

  @override
  Map<String, Object?> get fields => const {};
}

/// Final acknowledgment; [verified] reports the whole-job hash outcome.
final class FinalAck extends ProgressEvent {
  const FinalAck({required super.job, super.conn, required this.verified});

  /// True when the receiver confirmed every byte.
  final bool verified;

  @override
  String get name => 'final_ack';

  @override
  Map<String, Object?> get fields => {'verified': verified};
}

/// Transfer finished cleanly; `ak_job_result` will return the report.
final class Done extends ProgressEvent {
  const Done({required super.job, super.conn});

  @override
  String get name => 'done';

  @override
  Map<String, Object?> get fields => const {};
}

/// Fallback for event names this build does not know (newer engine).
///
/// Never thrown — [ProgressEvent.fromJson] hands this back so UI code can
/// display or ignore forward-compatible events. [data] keeps every
/// payload field except `job`/`conn`/`event` (shallow copy).
final class UnknownEvent extends ProgressEvent {
  const UnknownEvent({
    required super.job,
    super.conn,
    required this.name,
    required this.data,
  });

  @override
  final String name;

  /// Remaining payload fields of the unrecognized line.
  final Map<String, Object?> data;

  @override
  Map<String, Object?> get fields => data;
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

bool _requiredBool(Map<String, Object?> json, String key) {
  final value = json[key];
  if (value is! bool) {
    throw FormatException(
      'field "$key" must be a boolean${value == null ? ' (missing)' : ''}',
    );
  }
  return value;
}

int? _optionalConn(Map<String, Object?> json) {
  final value = json['conn'];
  if (value == null) return null;
  if (value is! int || value < 0) {
    throw const FormatException('field "conn" must be a non-negative integer');
  }
  return value;
}

bool _mapsEqual(Map<String, Object?> a, Map<String, Object?> b) {
  if (a.length != b.length) return false;
  for (final entry in a.entries) {
    if (!b.containsKey(entry.key) || b[entry.key] != entry.value) return false;
  }
  return true;
}
