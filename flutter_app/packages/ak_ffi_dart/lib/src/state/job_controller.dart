/// `JobController`: a pure-Dart, FFI-free state machine folding the
/// progress-event stream of ONE FFI job into an observable view.
///
/// Inputs:
/// * [feed] — every decoded `ProgressEvent` of the job (the caller routes
///   lines by their `job` id; this controller does not filter);
/// * [setResult] / [setResultJson] — the terminal `ak_job_result` body.
///
/// Outputs: [phase] (idle → connecting → transferring → verifying →
/// done/failed), [lastEvent], per-connection views ([connections]), a
/// flattened file table ([fileTable]), aggregate byte totals, [error] and
/// [report], plus a simple listener list (`addListener`) so a UI can
/// rebuild on every change. No `dart:ffi`, no timers, no I/O — fully unit
/// testable.
///
/// ## Phase derivation (per connection, monotonic)
///
/// | events                                             | phase         |
/// |----------------------------------------------------|---------------|
/// | connecting, listening, connected, offered, accepted, manifest_*, resuming | connecting (negotiation) |
/// | file_started, chunk_sent/received, bytes_acked, file_verified, file_committed | transferring |
/// | verifying, final_ack                               | verifying     |
/// | done                                               | done          |
/// | paused / resumed                                   | flag only, phase unchanged |
/// | unknown events                                     | lastEvent only |
///
/// A connection never regresses (verifying stays verifying even if a late
/// chunk event arrives). The overall [phase] is the *slowest* stage among
/// connections that have not finished, so a concurrent receive still reads
/// `transferring` while any connection is moving data; it reaches `done`
/// when every connection saw `done`, or immediately on a success result.
/// `failed` is sticky and only ever comes from [setResult] — the event
/// stream carries no failure event.
///
/// Multi-connection receives (`once_jobs > 1`) carry `"conn"` on every
/// line; each connection index gets its own [ConnectionProgress] row and
/// its own file table (file indices are per-connection). Lines without
/// `conn` (sends, single receives) all land in connection `0`.
library;

import '../model/progress_events.dart';
import '../model/report.dart';

/// Coarse job stage derived from the event stream.
enum JobPhase {
  /// No event fed yet.
  idle,

  /// Dialing / listening / manifest negotiation.
  connecting,

  /// File chunks are moving.
  transferring,

  /// Content hashes are being checked.
  verifying,

  /// Every connection reported `done` (or a success result arrived).
  done,

  /// Terminal failure from `ak_job_result`. Sticky.
  failed,
}

/// Progress of one file inside one connection. Mutated only by
/// [JobController]; treat instances as read-only snapshots.
class FileProgress {
  FileProgress({required this.conn, required this.index});

  /// Connection this file belongs to (0 for conn-less event streams).
  final int conn;

  /// 0-based file index *within its connection*.
  final int index;

  /// Job-relative path from `file_started` (or `file_committed`).
  String? path;

  /// Total bytes from `file_started`.
  int? size;

  /// Cumulative acknowledged bytes (latest `bytes_acked`, max-seen).
  int ackedBytes = 0;

  /// Chunk send events seen for this file.
  int sentChunks = 0;

  /// Chunk receive events seen for this file.
  int receivedChunks = 0;

  /// Last chunk index sent, if any.
  int? lastChunkSent;

  /// Last chunk index received, if any.
  int? lastChunkReceived;

  /// `file_verified` seen.
  bool verified = false;

  /// `file_committed` seen.
  bool committed = false;

  @override
  String toString() =>
      'FileProgress(conn=$conn index=$index path=$path size=$size '
      'ackedBytes=$ackedBytes sentChunks=$sentChunks '
      'receivedChunks=$receivedChunks verified=$verified '
      'committed=$committed)';
}

/// State of one connection (or the implicit connection 0 of a
/// conn-less stream). Mutated only by [JobController].
class ConnectionProgress {
  ConnectionProgress(this.conn);

  /// Connection index from the event lines (0 for conn-less streams).
  final int conn;

  /// This connection's monotonic stage; [JobPhase.done] is terminal here.
  JobPhase phase = JobPhase.idle;

  /// Paused by flow control, not yet resumed.
  bool paused = false;

  /// Most recent event of this connection.
  ProgressEvent? lastEvent;

  /// Bound listen address (`listening`), receive only.
  String? addr;

  /// Peer fingerprint (`connected`), 64 hex chars.
  String? peer;

  /// File count from `offered`/`accepted`.
  int? expectedFiles;

  /// Byte total from `offered`/`accepted`.
  int? expectedBytes;

  /// Checkpoint chunks from the latest `resuming` event.
  int resumedChunks = 0;

  /// Last manifest page sent (sender side).
  int? lastManifestPage;

  /// Whole-job verification outcome from `final_ack`.
  bool? finalVerified;

  /// Per-index file rows (indices are per-connection).
  final Map<int, FileProgress> files = {};

  /// Sum of the latest acknowledged bytes across this connection's files.
  int get ackedBytes {
    var total = 0;
    for (final file in files.values) {
      total += file.ackedBytes;
    }
    return total;
  }

  @override
  String toString() =>
      'ConnectionProgress(conn=$conn phase=$phase paused=$paused '
      'expectedFiles=$expectedFiles expectedBytes=$expectedBytes '
      'ackedBytes=$ackedBytes files=${files.length})';
}

/// Observable fold of one job's progress-event stream and final result.
class JobController {
  final List<void Function()> _listeners = [];
  final Map<int, ConnectionProgress> _connections = {};

  JobPhase _phase = JobPhase.idle;
  ProgressEvent? _lastEvent;
  JobOutcome? _outcome;
  TransferReport? _report;
  String? _error;

  /// Registers a callback invoked after every [feed], [setResult] and
  /// [reset]. Callbacks may be added or removed from a notification.
  void addListener(void Function() listener) {
    _listeners.add(listener);
  }

  /// Removes a previously added listener (no-op when absent).
  void removeListener(void Function() listener) {
    _listeners.remove(listener);
  }

  /// Current overall stage; see the library docs for the derivation.
  JobPhase get phase => _phase;

  /// True while any connection sits between `paused` and `resumed`.
  bool get paused {
    for (final connection in _connections.values) {
      if (connection.paused) return true;
    }
    return false;
  }

  /// Most recently fed event (any connection), for log-style display.
  ProgressEvent? get lastEvent => _lastEvent;

  /// Error message once [setResult] saw a failure body; null otherwise.
  String? get error => _error;

  /// Success report once [setResult] saw a success body.
  TransferReport? get report => _report;

  /// True after the first [setResult], whatever it was.
  bool get hasResult => _outcome != null;

  /// Terminal outcome delivered to [setResult], if any.
  JobOutcome? get outcome => _outcome;

  /// Per-connection rows keyed by connection index (0 for conn-less
  /// streams). Read-only map; rows are mutated only by this controller.
  Map<int, ConnectionProgress> get connections =>
      Map.unmodifiable(_connections);

  /// Flattened file table keyed `"<conn>:<file index>"` (e.g. `"0:0"`,
  /// `"1:3"`), so colliding per-connection file indices stay distinct.
  Map<String, FileProgress> get fileTable {
    final table = <String, FileProgress>{};
    for (final connection in _connections.values) {
      connection.files.forEach((index, file) {
        table['${connection.conn}:$index'] = file;
      });
    }
    return Map.unmodifiable(table);
  }

  /// Every file row across connections.
  Iterable<FileProgress> get allFiles {
    return _connections.values.expand((connection) => connection.files.values);
  }

  /// Sum of `offered`/`accepted` file counts; null before any arrived.
  int? get expectedFiles {
    int? total;
    for (final connection in _connections.values) {
      final value = connection.expectedFiles;
      if (value != null) total = (total ?? 0) + value;
    }
    return total;
  }

  /// Sum of `offered`/`accepted` byte totals; null before any arrived.
  int? get expectedBytes {
    int? total;
    for (final connection in _connections.values) {
      final value = connection.expectedBytes;
      if (value != null) total = (total ?? 0) + value;
    }
    return total;
  }

  /// Sum of acknowledged bytes across every connection and file.
  int get ackedBytes {
    var total = 0;
    for (final connection in _connections.values) {
      total += connection.ackedBytes;
    }
    return total;
  }

  /// Files that passed `file_verified`.
  int get verifiedFileCount =>
      allFiles.where((file) => file.verified).length;

  /// Files that passed `file_committed`.
  int get committedFileCount =>
      allFiles.where((file) => file.committed).length;

  /// Folds one progress event into the state and notifies listeners.
  ///
  /// Late events after a terminal result are still folded (the wire can
  /// deliver them), but never revive a `failed` overall phase.
  void feed(ProgressEvent event) {
    final connIndex = event.conn ?? 0;
    final connection =
        _connections.putIfAbsent(connIndex, () => ConnectionProgress(connIndex));
    _lastEvent = event;
    connection.lastEvent = event;
    _applyEvent(connection, event);
    _recomputeOverallPhase();
    _notify();
  }

  /// Delivers the terminal result and notifies listeners.
  ///
  /// Success pins [phase] to [JobPhase.done] and fills [report]; failure
  /// pins it to [JobPhase.failed] (sticky) and fills [error].
  void setResult(JobOutcome outcome) {
    _outcome = outcome;
    switch (outcome) {
      case JobSucceeded(:final report):
        _report = report;
        _error = null;
        _phase = JobPhase.done;
      case JobFailed(:final error):
        _error = error;
        _report = null;
        _phase = JobPhase.failed;
    }
    _notify();
  }

  /// [setResult] with the raw decoded result body (decoded via
  /// [JobOutcome.fromJson]).
  void setResultJson(Map<String, Object?> json) {
    setResult(JobOutcome.fromJson(json));
  }

  /// Returns the controller to a fresh `idle` state. Listeners survive.
  void reset() {
    _connections.clear();
    _phase = JobPhase.idle;
    _lastEvent = null;
    _outcome = null;
    _report = null;
    _error = null;
    _notify();
  }

  void _applyEvent(ConnectionProgress connection, ProgressEvent event) {
    switch (event) {
      case Connecting():
        _bump(connection, JobPhase.connecting);
      case Listening(:final addr):
        connection.addr = addr;
        _bump(connection, JobPhase.connecting);
      case Connected(:final peer):
        connection.peer = peer;
        _bump(connection, JobPhase.connecting);
      case Offered(:final files, :final bytes):
        connection.expectedFiles = files;
        connection.expectedBytes = bytes;
        _bump(connection, JobPhase.connecting);
      case Accepted(:final files, :final bytes):
        connection.expectedFiles = files;
        connection.expectedBytes = bytes;
        _bump(connection, JobPhase.connecting);
      case ManifestPageSent(:final page):
        connection.lastManifestPage = page;
        _bump(connection, JobPhase.connecting);
      case ManifestReceived():
        _bump(connection, JobPhase.connecting);
      case Resuming(:final chunks):
        connection.resumedChunks = chunks;
        _bump(connection, JobPhase.connecting);
      case FileStarted(:final file, :final path, :final bytes):
        final row = _fileRow(connection, file);
        row.path = path;
        row.size = bytes;
        _bump(connection, JobPhase.transferring);
      case ChunkSent(:final file, :final chunk):
        final row = _fileRow(connection, file);
        row.sentChunks += 1;
        row.lastChunkSent = chunk;
        _bump(connection, JobPhase.transferring);
      case ChunkReceived(:final file, :final chunk):
        final row = _fileRow(connection, file);
        row.receivedChunks += 1;
        row.lastChunkReceived = chunk;
        _bump(connection, JobPhase.transferring);
      case BytesAcked(:final file, :final bytes):
        final row = _fileRow(connection, file);
        // Cumulative per session; max keeps progress monotonic even when a
        // resumed session restarts its counter.
        if (bytes > row.ackedBytes) row.ackedBytes = bytes;
        _bump(connection, JobPhase.transferring);
      case Paused():
        connection.paused = true;
      case Resumed():
        connection.paused = false;
      case FileVerified(:final file):
        _fileRow(connection, file).verified = true;
        _bump(connection, JobPhase.transferring);
      case FileCommitted(:final file, :final path):
        final row = _fileRow(connection, file);
        row.committed = true;
        row.path ??= path;
        _bump(connection, JobPhase.transferring);
      case Verifying():
        _bump(connection, JobPhase.verifying);
      case FinalAck(:final verified):
        connection.finalVerified = verified;
        _bump(connection, JobPhase.verifying);
      case Done():
        connection.phase = JobPhase.done;
      case UnknownEvent():
        // Forward-compatible event: recorded as lastEvent, nothing else.
        break;
    }
  }

  FileProgress _fileRow(ConnectionProgress connection, int index) {
    return connection.files.putIfAbsent(
      index,
      () => FileProgress(conn: connection.conn, index: index),
    );
  }

  /// Monotonic per-connection stage bump: only ever moves forward.
  void _bump(ConnectionProgress connection, JobPhase phase) {
    if (connection.phase == JobPhase.done) return;
    if (phase.index > connection.phase.index) {
      connection.phase = phase;
    }
  }

  void _recomputeOverallPhase() {
    if (_phase == JobPhase.failed) return; // sticky terminal
    if (_outcome is JobSucceeded) {
      _phase = JobPhase.done;
      return;
    }
    var slowest = JobPhase.done;
    for (final connection in _connections.values) {
      if (connection.phase == JobPhase.done) continue;
      if (connection.phase.index < slowest.index) {
        slowest = connection.phase;
      }
    }
    // No connections yet -> idle; all done -> done; else slowest active.
    _phase = _connections.isEmpty ? JobPhase.idle : slowest;
  }

  void _notify() {
    for (final listener in List<void Function()>.of(_listeners)) {
      listener();
    }
  }
}
