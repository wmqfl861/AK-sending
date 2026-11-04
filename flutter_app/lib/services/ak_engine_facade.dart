/// [AkEngineFacade]: the thin seam over the engine's identity/jobs-store
/// exports that `FfiTransferService` wires into the UI contract.
///
/// Frozen wave-12 contract (implemented in parallel by the bindings agent;
/// see the coordination notes):
///
/// ```text
/// engine.identityInfo(String idDir) → Map<String,dynamic>
///   keys: device_id / fingerprint / pairing_code / peers / created
/// engine.jobsList(String idDir) → List<Map<String,dynamic>>
///   row keys: id / direction / state / files / bytes /
///             updated_unix_ms / peer / root
/// ```
///
/// Frozen wave-14 contract (pairing book, also landing with the bindings
/// agent in parallel):
///
/// ```text
/// engine.peerList(String idDir) → List<PeerEntry{fingerprint, name,
///   pairedUnixMs}>        // sorted by fingerprint; missing book = []
/// engine.peerAdd(String idDir, String fpHex, {String? name}) → int
///   // new peer count; fpHex must be 64 hex (engine -2 otherwise);
///   // null/empty name stores "(unnamed)" for NEW peers; idempotent
/// engine.peerRemove(String idDir, String fpHex) → int
///   // new peer count; a fingerprint not in the book raises the engine's
///   // -4 AK_ERR_NOT_FOUND (surfacing as AkFfiException here)
/// ```
///
/// The real adapter below is written against those contracts; until
/// `AkEngineDart` grows a method it throws [UnsupportedError] at the
/// exact call sites marked `TODO(wave12-dart)` / `TODO(wave14-dart)` —
/// integration is a one-line switch per method. Tests inject their own
/// fake of this interface and never touch the real one.
library;

import 'package:ak_ffi_dart/ak_ffi_dart.dart' show AkEngineDart;

import 'dll_path.dart' show resolveAkDllPath;

/// The engine-side identity/jobs-store surface [FfiTransferService] needs.
abstract interface class AkEngineFacade {
  /// Reads this device's identity under [idDir]:
  /// `device_id`/`fingerprint`/`pairing_code`/`peers`/`created`.
  ///
  /// Throws like the other engine calls do (missing DLL, AkFfiException
  /// carrying `last_error`, ...); callers degrade gracefully.
  Map<String, dynamic> identityInfo(String idDir);

  /// Lists the jobs checkpoint store rows under [idDir], newest first:
  /// `id`/`direction`/`state`/`files`/`bytes`/`updated_unix_ms`/`peer`/
  /// `root` per row.
  List<Map<String, dynamic>> jobsList(String idDir);

  /// Lists the pairing book (`<idDir>/peers.book`) rows: one map per
  /// paired peer with keys `fingerprint` (64 hex)/`name`/
  /// `paired_unix_ms`, sorted by fingerprint. A missing book is the
  /// empty list; a CORRUPT book throws (the engine's "cannot load peer
  /// book" error) so callers can surface the reason inline.
  List<Map<String, dynamic>> peerList(String idDir);

  /// Pairs one peer (or renames an existing one; idempotent) and returns
  /// the NEW peer count. [fpHex] must be exactly 64 hex characters — the
  /// engine rejects anything else with `-2`. A null/empty [name] stores
  /// `"(unnamed)"` for a NEW peer; re-adding an existing fingerprint
  /// with a name renames it but keeps the original `paired_unix_ms`.
  int peerAdd(String idDir, String fpHex, {String? name});

  /// Removes one paired peer and returns the NEW peer count. A
  /// fingerprint not in the book (a missing book has no peers, so it is
  /// the same case) throws the engine's `-4` not-found error; a bad
  /// fingerprint throws like on [peerAdd].
  int peerRemove(String idDir, String fpHex);
}

/// Real [AkEngineFacade] over one lazily-opened [AkEngineDart].
///
/// The engine handle is opened on first use through the same three-tier
/// DLL probe as [AkEngineBackend] (a missing DLL therefore throws here,
/// which [FfiTransferService] turns into the degraded identity/empty
/// history). Wave-12 integration note: when the bindings land, this class
/// may instead be handed the engine already owned by `AkEngineBackend`
/// (constructor parameter) so the app keeps a single native engine.
final class AkEngineDartFacade implements AkEngineFacade {
  /// [engine] may be supplied at integration time (e.g. the handle owned
  /// by `AkEngineBackend`); when null the facade opens its own lazily.
  AkEngineDartFacade({AkEngineDart? engine}) {
    _engine = engine;
  }

  AkEngineDart? _engine;
  bool _disposed = false;

  @override
  Map<String, dynamic> identityInfo(String idDir) {
    final info = _engineOrThrow().identityInfo(idDir);
    return {
      'device_id': info.deviceId,
      'fingerprint': info.fingerprint,
      'pairing_code': info.pairingCode,
      'peers': info.peers,
      'created': info.created,
    };
  }

  @override
  List<Map<String, dynamic>> jobsList(String idDir) {
    return _engineOrThrow()
        .jobsList(idDir)
        .map(
          (record) => <String, dynamic>{
            'id': record.id,
            'direction': record.direction,
            'state': record.state,
            'files': record.files,
            'bytes': record.bytes,
            'updated_unix_ms': record.updatedUnixMs,
            'peer': record.peer,
            'root': record.root,
          },
        )
        .toList();
  }

  @override
  List<Map<String, dynamic>> peerList(String idDir) {
    return _engineOrThrow()
        .peerList(idDir)
        .map(
          (peer) => <String, dynamic>{
            'fingerprint': peer.fingerprint,
            'name': peer.name,
            'paired_unix_ms': peer.pairedUnixMs,
          },
        )
        .toList();
  }

  @override
  int peerAdd(String idDir, String fpHex, {String? name}) {
    return _engineOrThrow().peerAdd(idDir, fpHex, name: name ?? '');
  }

  @override
  int peerRemove(String idDir, String fpHex) {
    return _engineOrThrow().peerRemove(idDir, fpHex);
  }

  /// Releases the engine when this facade opened its own. Idempotent.
  void dispose() {
    if (_disposed) {
      return;
    }
    _disposed = true;
    _engine?.dispose();
    _engine = null;
  }

  /// Opens (once) and returns the engine handle; missing DLL throws so the
  /// caller can degrade. A disposed facade always throws [StateError].
  AkEngineDart _engineOrThrow() {
    if (_disposed) {
      throw StateError('AkEngineDartFacade is disposed');
    }
    final existing = _engine;
    if (existing != null && !existing.isDisposed) {
      return existing;
    }
    final resolution = resolveAkDllPath();
    if (resolution == null) {
      // Reuse AkEngineBackend's probe-diagnostics exception shape.
      throw _DllMissing();
    }
    _engine = AkEngineDart(dllPath: resolution.path);
    return _engine!;
  }
}

/// Minimal stand-in for AkEngineBackend's DllNotFoundException (which lives
/// in transfer_service.dart of this layer); kept private so callers only
/// need "it threw".
final class _DllMissing implements Exception {
  @override
  String toString() => '未找到 ak_ffi 动态库（identityInfo/jobsList 探测失败）';
}
