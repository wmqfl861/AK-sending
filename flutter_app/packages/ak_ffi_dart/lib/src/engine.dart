/// `AkEngineDart`: the friendly face over [AkFfiBindings] — engine
/// lifecycle with a finalizer backstop, typed start/poll/result/cancel
/// calls, id_dir inspection ([IdentityInfo]/[JobRecordJson]), automatic
/// `-3` buffer growth with lossless retry, and negative status codes turned
/// into [AkFfiException]s carrying the last-error message.
///
/// Concurrency contract (inherited from the shim): calls on ONE engine
/// handle must be serialized by the caller — keep a single engine per
/// isolate/thread.
library;

import 'dart:convert' show jsonDecode;

import 'dart:ffi';

import 'dart:typed_data' show Uint8List;

import 'bindings.dart';
import 'ffi_json.dart'
    show AkReceiveSpec, AkSendSpec, decodeFfiJson, fromNativeUtf8, toNativeUtf8;
import 'identity.dart';

/// Byte length of every FFI job id.
const int akJobIdLength = 16;

/// Terminal snapshot of one job, as returned by [AkEngineDart.jobResult].
final class AkJobResult {
  const AkJobResult._(this.state, this.map);

  /// `"running"`, `"done"`, or `"failed"`.
  final String state;

  /// Decoded report JSON for terminal states
  /// (`{"job","wire_job","files","bytes","duration_ms","resumed","peer",
  /// "roots"}` on success, `{"job","error"}` on failure); null while
  /// running.
  final Map<String, dynamic>? map;

  const AkJobResult.running() : this._('running', null);

  factory AkJobResult.done(Map<String, dynamic> map) => AkJobResult._('done', map);

  factory AkJobResult.failed(Map<String, dynamic> map) =>
      AkJobResult._('failed', map);

  bool get isRunning => state == 'running';

  bool get isDone => state == 'done';

  bool get isFailed => state == 'failed';

  /// The `error` string of a failed result (null otherwise).
  String? get error => isFailed ? map!['error'] as String? : null;
}

/// Private detach token so the finalizer knows how to free exactly this
/// engine's handle.
final class _EngineLease {
  _EngineLease(this.bindings, this.handle);

  final AkFfiBindings bindings;
  final Pointer<AkEngine> handle;

  void release() => bindings.engineFree(handle);
}

/// One embedded engine (tokio runtime + job table) owned via the C ABI.
class AkEngineDart {
  /// Backstop for leaked engines: frees the native handle when this object
  /// becomes unreachable. Explicit [dispose] is still preferred (prompt,
  /// deterministic).
  static final Finalizer<_EngineLease> _finalizer =
      Finalizer<_EngineLease>((lease) => lease.release());

  /// Defensive ceiling for the automatic `-3` grow-retry loops.
  static const int _maxBufferBytes = 16 * 1024 * 1024;

  /// The raw bindings; exposed for direct buffer-level experiments (the
  /// loopback tests drive `-3` retry paths through it).
  final AkFfiBindings bindings;

  Pointer<AkEngine> _handle = nullptr;

  bool _disposed = false;

  /// Creates the engine via `ak_engine_new`. [bindings] may be shared
  /// between engines; [dllPath] is only consulted when [bindings] is null
  /// (see [AkFfiBindings.defaultDllPath] for the resolution order).
  AkEngineDart({AkFfiBindings? bindings, String? dllPath})
      : bindings = bindings ?? AkFfiBindings(dllPath: dllPath) {
    final handle = this.bindings.engineNew();
    if (handle == nullptr) {
      throw AkFfiException(
        AkStatus.errInternal,
        'ak_engine_new returned null: '
        '${_readLastError(this.bindings, nullptr)}',
      );
    }
    _handle = handle;
    _finalizer.attach(this, _EngineLease(this.bindings, handle), detach: this);
  }

  /// The native `*mut AkEngine` for direct bindings-layer calls.
  Pointer<AkEngine> get nativeHandle {
    _ensureLive();
    return _handle;
  }

  bool get isDisposed => _disposed;

  /// Releases the engine (aborting every still-running job). Idempotent;
  /// later calls throw [StateError].
  void dispose() {
    if (_disposed) {
      return;
    }
    _finalizer.detach(this);
    final handle = _handle;
    _handle = nullptr;
    _disposed = true;
    bindings.engineFree(handle);
  }

  void _ensureLive() {
    if (_disposed) {
      throw StateError('AkEngineDart is disposed');
    }
  }

  // -------------------------------------------------------------------------
  // Non-job entry points.
  // -------------------------------------------------------------------------

  /// The library version string, e.g. `ak-ffi 1 (wire v1, caps 0x0007)`.
  String version() {
    _ensureLive();
    return _readInto(_maxVersionBytes, (buf, cap) => bindings.version(buf, cap),
        'ak_version');
  }

  static const int _maxVersionBytes = 128;

  /// The thread-local last-error message of the most recent call on this
  /// thread ('' when the last call succeeded). Reading does not clear it.
  String lastError() {
    _ensureLive();
    return _readLastError(bindings, _handle);
  }

  // -------------------------------------------------------------------------
  // Identity + persisted jobs (id_dir inspection; no engine state needed).
  // -------------------------------------------------------------------------

  /// The identity summary of [idDir] via `ak_identity_info`: creates the
  /// directory and its `device.key` when missing ([IdentityInfo.created] is
  /// then true, exactly once per directory) and reports the fingerprint,
  /// pairing code, and `peers.book` entry count.
  ///
  /// The answer comes from disk, so it is valid before any transfer. A
  /// corrupt `peers.book` still succeeds with `peers: 0` and leaves a note
  /// in [lastError] — read it right after this call. Throws
  /// [AkFfiException] (with the last-error message) for an empty or
  /// unusable [idDir]; a `-3` grows the buffer and retries losslessly.
  IdentityInfo identityInfo(String idDir) {
    final json = _inspectIdDir(idDir, bindings.identityInfo,
        'ak_identity_info', _maxIdentityJsonBytes);
    return IdentityInfo.fromJson(decodeFfiJson(json));
  }

  /// The persisted job records of [idDir] via `ak_jobs_list`: one
  /// [JobRecordJson] per parsable record under `<idDir>/checkpoints`, sorted
  /// by job id (jobs started WITHOUT an `id_dir` never persist, so a fresh
  /// directory lists `[]`).
  ///
  /// A missing checkpoint directory is the empty list. Throws
  /// [AkFfiException] (with the last-error message) for an empty or unusable
  /// [idDir]; the whole array is rendered before any write, so a `-3` retry
  /// with a larger buffer is lossless (and automatic here).
  List<JobRecordJson> jobsList(String idDir) {
    final json = _inspectIdDir(
        idDir, bindings.jobsList, 'ak_jobs_list', _maxJobsListBytes);
    final array = jsonDecode(json) as List<dynamic>;
    return [
      for (final element in array)
        JobRecordJson.fromJson(element as Map<String, dynamic>),
    ];
  }

  /// Starting buffer for identity summaries (~130 bytes of JSON).
  static const int _maxIdentityJsonBytes = 256;

  /// Starting buffer for job lists (grows by doubling; hundreds of records
  /// still fit in a few doublings).
  static const int _maxJobsListBytes = 1024;

  /// Shared id_dir-inspection call: native [idDir] in, JSON payload out,
  /// with the standard `-3` grow-retry and last-error-carrying
  /// [AkFfiException]s for the other negative codes.
  String _inspectIdDir(
    String idDir,
    int Function(Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>, int)
        invoke,
    String symbol,
    int initialCapacity,
  ) {
    _ensureLive();
    final idPointer = toNativeUtf8(idDir);
    try {
      var capacity = initialCapacity;
      while (capacity <= _maxBufferBytes) {
        final buf = processAllocator.allocate<Uint8>(capacity);
        try {
          final code = invoke(_handle, idPointer, buf, capacity);
          if (code == AkStatus.done) {
            return fromNativeUtf8(buf);
          }
          if (code == AkStatus.errBuffer) {
            capacity *= 2; // nothing was consumed; retry losslessly
            continue;
          }
          throw _exceptionFor(code, symbol);
        } finally {
          processAllocator.free(buf);
        }
      }
      throw StateError(
          '$symbol kept requesting more than $_maxBufferBytes bytes');
    } finally {
      processAllocator.free(idPointer);
    }
  }

  // -------------------------------------------------------------------------
  // Job start.
  // -------------------------------------------------------------------------

  /// Validates [spec] (throwing [AkSpecException] on any mirrored rule)
  /// and starts a send job; returns its fresh 16-byte job id.
  Uint8List sendStart(AkSendSpec spec) => _start(spec.toJsonString(), true);

  /// Same as [sendStart] for a receive job.
  Uint8List receiveStart(AkReceiveSpec spec) =>
      _start(spec.toJsonString(), false);

  /// Raw JSON escape hatch: sends [specJson] to `ak_send_start` without any
  /// Dart-side validation (exercises the Rust parser/-2 path; also usable
  /// for forward-compatible fields this builder does not know yet).
  Uint8List sendStartJson(String specJson) => _start(specJson, true);

  /// Raw JSON escape hatch for `ak_receive_start` (see [sendStartJson]).
  Uint8List receiveStartJson(String specJson) => _start(specJson, false);

  Uint8List _start(String specJson, bool send) {
    _ensureLive();
    final symbol = send ? 'ak_send_start' : 'ak_receive_start';
    final invoke =
        send ? bindings.sendStart : bindings.receiveStart;
    final specPointer = toNativeUtf8(specJson);
    final jobOut = processAllocator.allocate<Uint8>(akJobIdLength);
    try {
      final code = invoke(_handle, specPointer, jobOut);
      if (code != AkStatus.ok) {
        throw _exceptionFor(code, symbol);
      }
      final job = Uint8List(akJobIdLength);
      for (var i = 0; i < akJobIdLength; i++) {
        job[i] = jobOut[i];
      }
      return job;
    } finally {
      processAllocator.free(jobOut);
      processAllocator.free(specPointer);
    }
  }

  // -------------------------------------------------------------------------
  // Progress + results.
  // -------------------------------------------------------------------------

  /// Pops the oldest pending progress event, decoded from its compact JSON
  /// line (e.g. `{"job":"...","event":"listening","addr":"127.0.0.1:52341"}`).
  ///
  /// Returns null when the queue is empty. A `-3` (buffer too small, event
  /// NOT consumed) is retried automatically with a doubled capacity.
  Map<String, dynamic>? pollProgress() {
    _ensureLive();
    var capacity = 256;
    while (capacity <= _maxBufferBytes) {
      final buf = processAllocator.allocate<Uint8>(capacity);
      try {
        final code = bindings.pollProgress(_handle, buf, capacity);
        if (code == AkStatus.pending) {
          return null;
        }
        if (code == AkStatus.event) {
          return jsonDecode(fromNativeUtf8(buf)) as Map<String, dynamic>;
        }
        if (code == AkStatus.errBuffer) {
          capacity *= 2; // nothing was consumed; retry losslessly
          continue;
        }
        throw _exceptionFor(code, 'ak_poll_progress');
      } finally {
        processAllocator.free(buf);
      }
    }
    throw StateError(
        'ak_poll_progress kept requesting more than $_maxBufferBytes bytes');
  }

  /// Snapshots one job's terminal state (see [AkJobResult]).
  ///
  /// `done`/`failed` carry the decoded report JSON in
  /// [AkJobResult.map]; a `-4` unknown job id throws [AkFfiException];
  /// `-3` grows the buffer and retries losslessly.
  AkJobResult jobResult(Uint8List job) {
    if (job.length != akJobIdLength) {
      throw ArgumentError.value(job.length, 'job.length',
          'FFI job ids are exactly $akJobIdLength bytes');
    }
    _ensureLive();
    final jobPointer = processAllocator.allocate<Uint8>(akJobIdLength);
    try {
      for (var i = 0; i < akJobIdLength; i++) {
        jobPointer[i] = job[i];
      }
      var capacity = 256;
      while (capacity <= _maxBufferBytes) {
        final buf = processAllocator.allocate<Uint8>(capacity);
        try {
          final code = bindings.jobResult(_handle, jobPointer, buf, capacity);
          switch (code) {
            case AkStatus.pending:
              return const AkJobResult.running();
            case AkStatus.done:
              return AkJobResult.done(
                  jsonDecode(fromNativeUtf8(buf)) as Map<String, dynamic>);
            case AkStatus.failed:
              return AkJobResult.failed(
                  jsonDecode(fromNativeUtf8(buf)) as Map<String, dynamic>);
            case AkStatus.errBuffer:
              capacity *= 2; // retry losslessly with a larger buffer
              continue;
            default:
              throw _exceptionFor(code, 'ak_job_result');
          }
        } finally {
          processAllocator.free(buf);
        }
      }
      throw StateError(
          'ak_job_result kept requesting more than $_maxBufferBytes bytes');
    } finally {
      processAllocator.free(jobPointer);
    }
  }

  /// Aborts a running job: it then reports `failed` with
  /// `"cancelled by caller"` and never emits a trailing `done` event. A
  /// job that already finished is left untouched (still `AK_OK`).
  void cancel(Uint8List job) {
    if (job.length != akJobIdLength) {
      throw ArgumentError.value(job.length, 'job.length',
          'FFI job ids are exactly $akJobIdLength bytes');
    }
    _ensureLive();
    final jobPointer = processAllocator.allocate<Uint8>(akJobIdLength);
    try {
      for (var i = 0; i < akJobIdLength; i++) {
        jobPointer[i] = job[i];
      }
      final code = bindings.jobCancel(_handle, jobPointer);
      if (code != AkStatus.ok) {
        throw _exceptionFor(code, 'ak_job_cancel');
      }
    } finally {
      processAllocator.free(jobPointer);
    }
  }

  // -------------------------------------------------------------------------
  // Internals.
  // -------------------------------------------------------------------------

  AkFfiException _exceptionFor(int code, String symbol) {
    final message = _readLastError(bindings, _handle);
    return AkFfiException(
        code, message.isEmpty ? '$symbol failed (${AkStatus.name(code)})' : message);
  }

  static String _readLastError(AkFfiBindings ffi, Pointer<AkEngine> handle) =>
      _readInto(256, (buf, cap) => ffi.engineLastError(handle, buf, cap),
          'ak_engine_last_error');

  /// Shared grow-retry loop for the char-out functions. Returns the
  /// decoded NUL-terminated payload; negative codes other than `-3` throw
  /// [AkFfiException]; `-3` doubles the capacity and retries (nothing was
  /// consumed).
  static String _readInto(
    int initialCapacity,
    int Function(Pointer<Uint8> buf, int cap) invoke,
    String symbol,
  ) {
    var capacity = initialCapacity;
    while (capacity <= _maxBufferBytes) {
      final buf = processAllocator.allocate<Uint8>(capacity);
      try {
        final code = invoke(buf, capacity);
        if (code >= 0) {
          return fromNativeUtf8(buf);
        }
        if (code == AkStatus.errBuffer) {
          capacity *= 2; // stored message untouched; retry losslessly
          continue;
        }
        throw AkFfiException(code, '$symbol failed (${AkStatus.name(code)})');
      } finally {
        processAllocator.free(buf);
      }
    }
    throw StateError('$symbol kept requesting more than $_maxBufferBytes bytes');
  }
}
