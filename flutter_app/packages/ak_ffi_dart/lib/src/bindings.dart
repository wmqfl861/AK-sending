/// Raw dart:ffi bindings for the fourteen exported `ak_*` functions of the
/// ak-ffi C ABI (see `crates/ak-ffi/src/shim.rs` for the frozen signatures
/// and docs/adr/003 for the boundary decisions).
///
/// This layer is pointers and status codes only: strings cross as
/// NUL-terminated UTF-8 (`toNativeUtf8`/`fromNativeUtf8` from ffi_json.dart,
/// allocated with [processAllocator]), and every negative return is an
/// [AkFfiException]-worthy condition the caller (engine.dart) translates.
library;

import 'dart:ffi';
import 'dart:io' show Platform;

import 'package:ffi/ffi.dart' show malloc;

/// The opaque `AkEngine` handle owned by `ak_engine_new` and released only
/// by `ak_engine_free`.
final class AkEngine extends Opaque {}

/// Status codes of the frozen contract (see crates/ak-ffi/src/core.rs).
abstract final class AkStatus {
  /// `ak_poll_progress`: no event pending; `ak_job_result`: still running.
  static const int ok = 0;

  /// `ak_poll_progress`: one event written into the caller buffer.
  static const int event = 1;

  /// `ak_job_result`: finished ok (same numeric value as [event]).
  static const int done = 1;

  /// `ak_job_result`: still running (same numeric value as [ok]).
  static const int pending = 0;

  /// `ak_job_result`: finished with an error (error JSON written).
  static const int failed = 2;

  static const int errNull = -1;
  static const int errInvalid = -2;
  static const int errBuffer = -3;
  static const int errNotFound = -4;
  static const int errInternal = -5;

  static String name(int code) => switch (code) {
        ok => 'AK_OK',
        event => 'AK_EVENT/AK_DONE',
        failed => 'AK_FAILED',
        errNull => 'AK_ERR_NULL',
        errInvalid => 'AK_ERR_INVALID',
        errBuffer => 'AK_ERR_BUFFER',
        errNotFound => 'AK_ERR_NOT_FOUND',
        errInternal => 'AK_ERR_INTERNAL',
        _ => 'AK_UNKNOWN_$code',
      };
}

/// A negative status code from the ak-ffi boundary together with the
/// thread-local last-error message captured right after the failing call.
class AkFfiException implements Exception {
  const AkFfiException(this.code, this.message);

  final int code;

  final String message;

  @override
  String toString() => 'AkFfiException(${AkStatus.name(code)} $code): $message';
}

// ---------------------------------------------------------------------------
// The package's single C heap allocator (C03).
// ---------------------------------------------------------------------------

/// The ONE allocator every native buffer in this package is allocated and
/// freed with: `package:ffi`'s [malloc].
///
/// C03 (codex-crash-analysis-2026-11-07.md): the previous implementation
/// resolved `malloc` and `free` as two INDEPENDENT process-wide symbol
/// lookups, so on a Windows host carrying several CRTs the pair could be
/// served by different modules — freeing on a foreign heap corrupts it.
/// [malloc] is one implementation object whose `allocate` and `free` are
/// matched by construction (Windows: `CoTaskMemAlloc`/`CoTaskMemFree` from
/// ole32.dll; POSIX: the libc `malloc`/`free` pair bound together), which
/// removes that ambiguity. The DLL only reads/writes these buffers inside
/// a call and never frees them, so Dart stays the owning side.
///
/// The name is historical; call sites must keep routing through this single
/// alias instead of opening their own allocator.
final Allocator processAllocator = malloc;

/// Human-readable provenance of [processAllocator]'s allocate/free pair —
/// recorded by tests and diagnostics so the "one matched pair" property of
/// C03 stays auditable.
const String processAllocatorSource =
    'package:ffi malloc (Windows: ole32!CoTaskMemAlloc/CoTaskMemFree; '
    'POSIX: libc malloc/free) — matched allocate/free pair';

/// The native boundary broke its own OUTPUT contract (C05/C06 of the
/// crash analysis): a returned byte length outside `0 <= len < cap`, a
/// missing NUL terminator inside the caller buffer, or a similar reply
/// malformation.
///
/// Distinct from [AkFfiException] — that one means the engine REPORTED a
/// negative status; this one means the reply itself cannot be trusted and
/// reading it on would be an out-of-bounds access. Throwing here is the
/// deliberate substitute for the old unbounded NUL scans.
class AkFfiProtocolException implements Exception {
  const AkFfiProtocolException(this.message);

  final String message;

  @override
  String toString() => 'AkFfiProtocolException: $message';
}

// ---------------------------------------------------------------------------
// Native signatures of the fourteen exports (shim.rs, verbatim).
// ---------------------------------------------------------------------------

typedef _EngineNewC = Pointer<AkEngine> Function();
typedef _EngineNewDart = Pointer<AkEngine> Function();

typedef _EngineFreeC = Void Function(Pointer<AkEngine>);
typedef _EngineFreeDart = void Function(Pointer<AkEngine>);

typedef _EngineLastErrorC = Int32 Function(
    Pointer<AkEngine>, Pointer<Uint8>, IntPtr);
typedef _EngineLastErrorDart = int Function(
    Pointer<AkEngine>, Pointer<Uint8>, int);

typedef _SendStartC = Int32 Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>);
typedef _SendStartDart = int Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>);

typedef _ReceiveStartC = Int32 Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>);
typedef _ReceiveStartDart = int Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>);

typedef _PollProgressC = Int32 Function(
    Pointer<AkEngine>, Pointer<Uint8>, IntPtr);
typedef _PollProgressDart = int Function(
    Pointer<AkEngine>, Pointer<Uint8>, int);

typedef _JobResultC = Int32 Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>, IntPtr);
typedef _JobResultDart = int Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>, int);

typedef _JobCancelC = Int32 Function(Pointer<AkEngine>, Pointer<Uint8>);
typedef _JobCancelDart = int Function(Pointer<AkEngine>, Pointer<Uint8>);

typedef _VersionC = Int32 Function(Pointer<Uint8>, IntPtr);
typedef _VersionDart = int Function(Pointer<Uint8>, int);

typedef _IdentityInfoC = Int32 Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>, IntPtr);
typedef _IdentityInfoDart = int Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>, int);

typedef _JobsListC = Int32 Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>, IntPtr);
typedef _JobsListDart = int Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>, int);

typedef _PeerListC = Int32 Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>, IntPtr);
typedef _PeerListDart = int Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>, int);

typedef _PeerAddC = Int32 Function(Pointer<AkEngine>, Pointer<Uint8>,
    Pointer<Uint8>, Pointer<Uint8>, Pointer<Uint8>, IntPtr);
typedef _PeerAddDart = int Function(Pointer<AkEngine>, Pointer<Uint8>,
    Pointer<Uint8>, Pointer<Uint8>, Pointer<Uint8>, int);

typedef _PeerRemoveC = Int32 Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>, Pointer<Uint8>, IntPtr);
typedef _PeerRemoveDart = int Function(
    Pointer<AkEngine>, Pointer<Uint8>, Pointer<Uint8>, Pointer<Uint8>, int);

/// Lookup of the fourteen `ak_*` exports from one `DynamicLibrary`.
class AkFfiBindings {
  /// Opens the DLL at [dllPath] (or [library] directly when injecting a
  /// preopened `DynamicLibrary`, e.g. from tests or another embedding).
  ///
  /// DLL location precedence: explicit [dllPath] > `AK_FFI_DLL` environment
  /// variable > [defaultDllPath] (a path relative to the process CWD; tests
  /// run from the package root, so `../../../target/release/ak_ffi.dll`
  /// reaches the workspace build).
  AkFfiBindings({DynamicLibrary? library, String? dllPath})
      : source = dllPath != null && dllPath.isNotEmpty
            ? dllPath
            : (library != null ? '<injected DynamicLibrary>' : defaultDllPath()),
        library = _open(library, dllPath) {
    engineNew = this.library.lookupFunction<_EngineNewC, _EngineNewDart>(
        'ak_engine_new');
    engineFree = this.library.lookupFunction<_EngineFreeC, _EngineFreeDart>(
        'ak_engine_free');
    engineLastError =
        this.library.lookupFunction<_EngineLastErrorC, _EngineLastErrorDart>(
            'ak_engine_last_error');
    sendStart = this.library.lookupFunction<_SendStartC, _SendStartDart>(
        'ak_send_start');
    receiveStart =
        this.library.lookupFunction<_ReceiveStartC, _ReceiveStartDart>(
            'ak_receive_start');
    pollProgress =
        this.library.lookupFunction<_PollProgressC, _PollProgressDart>(
            'ak_poll_progress');
    jobResult = this.library.lookupFunction<_JobResultC, _JobResultDart>(
        'ak_job_result');
    jobCancel = this.library.lookupFunction<_JobCancelC, _JobCancelDart>(
        'ak_job_cancel');
    version = this.library
        .lookupFunction<_VersionC, _VersionDart>('ak_version');
    identityInfo =
        this.library.lookupFunction<_IdentityInfoC, _IdentityInfoDart>(
            'ak_identity_info');
    jobsList = this.library
        .lookupFunction<_JobsListC, _JobsListDart>('ak_jobs_list');
    peerList = this.library
        .lookupFunction<_PeerListC, _PeerListDart>('ak_peer_list');
    peerAdd = this.library
        .lookupFunction<_PeerAddC, _PeerAddDart>('ak_peer_add');
    peerRemove = this.library
        .lookupFunction<_PeerRemoveC, _PeerRemoveDart>('ak_peer_remove');
  }

  /// The loaded library; kept alive so the resolved symbols stay valid.
  final DynamicLibrary library;

  /// Human-readable origin of [library] (path, env override, or injected).
  final String source;

  /// `ak_engine_new()`: an owned handle, or null on failure (last error on
  /// the calling thread).
  late final Pointer<AkEngine> Function() engineNew;

  /// `ak_engine_free(handle)`: the only matching destructor; null is a no-op.
  late final void Function(Pointer<AkEngine> engine) engineFree;

  /// `ak_engine_last_error(engine, buf, cap)`: message byte length, or
  /// -1/-3 (the message stays stored, retry losslessly). `engine` is not
  /// dereferenced and may be null.
  late final int Function(
      Pointer<AkEngine> engine, Pointer<Uint8> buf, int cap) engineLastError;

  /// `ak_send_start(engine, spec_json, job_out)`: 0 ok (16-byte id written),
  /// negative on error (job_out untouched).
  late final int Function(Pointer<AkEngine> engine, Pointer<Uint8> specJson,
      Pointer<Uint8> jobOut) sendStart;

  /// `ak_receive_start(engine, spec_json, job_out)`: same contract as
  /// [sendStart].
  late final int Function(Pointer<AkEngine> engine, Pointer<Uint8> specJson,
      Pointer<Uint8> jobOut) receiveStart;

  /// `ak_poll_progress(engine, buf, cap)`: 1 event written, 0 queue empty,
  /// -3 too small (the event stays queued; nothing consumed).
  late final int Function(
      Pointer<AkEngine> engine, Pointer<Uint8> buf, int cap) pollProgress;

  /// `ak_job_result(engine, job, buf, cap)`: 1 done, 0 running, 2 failed,
  /// -3 too small (retry losslessly), -4 unknown job.
  late final int Function(Pointer<AkEngine> engine, Pointer<Uint8> job,
      Pointer<Uint8> buf, int cap) jobResult;

  /// `ak_job_cancel(engine, job)`: 0 ok, -4 unknown job.
  late final int Function(Pointer<AkEngine> engine, Pointer<Uint8> job)
      jobCancel;

  /// `ak_version(buf, cap)`: written byte length (excluding NUL), or
  /// -1/-3.
  late final int Function(Pointer<Uint8> buf, int cap) version;

  /// `ak_identity_info(engine, id_dir, buf, cap)`: 1 with the identity
  /// summary JSON written, -2 for an empty/unusable `id_dir`, -3 too small
  /// (retry losslessly; the needed size is in the last-error message), -1
  /// null pointer arguments. `engine` is reserved and never dereferenced —
  /// the answer comes from disk, not engine state (see [engineLastError]
  /// for the same reserved-handle pattern).
  late final int Function(Pointer<AkEngine> engine, Pointer<Uint8> idDir,
      Pointer<Uint8> buf, int cap) identityInfo;

  /// `ak_jobs_list(engine, id_dir, buf, cap)`: 1 with the persisted-jobs
  /// JSON array written (`[]` for a missing checkpoint directory), same
  /// error contract as [identityInfo]. `engine` is reserved and never
  /// dereferenced.
  late final int Function(Pointer<AkEngine> engine, Pointer<Uint8> idDir,
      Pointer<Uint8> buf, int cap) jobsList;

  /// `ak_peer_list(engine, id_dir, buf, cap)`: 1 with the paired-peers JSON
  /// array written (`[]` for a missing book; a corrupt book is -2 with a
  /// "cannot load peer book" message), same error contract as
  /// [identityInfo]. Reads never create anything on disk. `engine` is
  /// reserved and never dereferenced.
  late final int Function(Pointer<AkEngine> engine, Pointer<Uint8> idDir,
      Pointer<Uint8> buf, int cap) peerList;

  /// `ak_peer_add(engine, id_dir, fingerprint_hex, name, buf, cap)`: 1 with
  /// `{"peers":N}` (the new count) written. CLI TOFU semantics:
  /// `fingerprint_hex` must be 64 hex chars (else -2); `name` is the ABI's
  /// one OPTIONAL string argument — a null pointer means "no name given"
  /// (a new peer is then stored as "(unnamed)", an existing one is left
  /// untouched); a given name renames while keeping the original pairing
  /// time. Names with control characters or over 4096 bytes are -2; a -3 is
  /// raised before anything is persisted. `engine` is reserved.
  late final int Function(
      Pointer<AkEngine> engine,
      Pointer<Uint8> idDir,
      Pointer<Uint8> fingerprintHex,
      Pointer<Uint8> name,
      Pointer<Uint8> buf,
      int cap) peerAdd;

  /// `ak_peer_remove(engine, id_dir, fingerprint_hex, buf, cap)`: 1 with
  /// `{"peers":N}` (the remaining count) written. A fingerprint not in the
  /// book (a missing book has no peers) is -4; a bad fingerprint is -2 like
  /// on add; the save/buffer rules match [peerAdd]. `engine` is reserved.
  late final int Function(
      Pointer<AkEngine> engine,
      Pointer<Uint8> idDir,
      Pointer<Uint8> fingerprintHex,
      Pointer<Uint8> buf,
      int cap) peerRemove;

  /// Where the DLL is loaded from when neither [AkFfiBindings.library] nor
  /// an explicit path is given: `$AK_FFI_DLL` wins, else a path relative to
  /// the process working directory that matches running `dart test`/the
  /// Flutter app from this package layout.
  static String defaultDllPath() {
    final fromEnv = Platform.environment['AK_FFI_DLL'];
    if (fromEnv != null && fromEnv.isNotEmpty) {
      return fromEnv;
    }
    final name = Platform.isWindows
        ? 'ak_ffi.dll'
        : Platform.isMacOS
            ? 'libak_ffi.dylib'
            : 'libak_ffi.so';
    return '../../../target/release/$name';
  }

  static DynamicLibrary _open(DynamicLibrary? library, String? dllPath) {
    if (library != null) {
      return library;
    }
    final path = dllPath != null && dllPath.isNotEmpty
        ? dllPath
        : defaultDllPath();
    return DynamicLibrary.open(path);
  }
}
