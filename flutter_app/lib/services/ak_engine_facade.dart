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

import 'dart:ffi' show DynamicLibrary, Pointer;

import 'dart:io' show File, FileMode, Platform;

import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show AkEngine, AkEngineDart, AkFfiBindings, AkTaskFfiBindings;

import 'dll_path.dart' show resolveAkDllPath;

// ---------------------------------------------------------------------------
// E2/E4 startup phase log (`%TEMP%\ak-stage.log`).
// ---------------------------------------------------------------------------

/// Append-only startup phase log (crash-analysis E2/E4): one line per
/// event, `<epoch-milliseconds> <phase>`, written SYNCHRONOUSLY (flushed)
/// so the last line brackets a native crash against wall-clock dump
/// times. Best-effort by contract — a broken log (missing `%TEMP%`,
/// unwritable) disables itself and NEVER disturbs startup.
final class AkStageLog {
  AkStageLog._(this._path);

  /// The log file path, or null when the environment names no temp dir
  /// (the log is then a permanent no-op).
  final String? _path;

  /// Set after the first failed write; further [write]s are skipped.
  bool _broken = false;

  static AkStageLog? _shared;

  /// The process-wide instance (`%TEMP%\ak-stage.log`, resolved lazily
  /// from [Platform.environment]; resolved ONCE so every writer agrees).
  static AkStageLog shared() =>
      _shared ??= AkStageLog._(defaultAkStageLogPath(Platform.environment));

  /// A log writing to an explicit [path] (tests inject a temp file).
  factory AkStageLog.file(String path) => AkStageLog._(path);

  /// Appends one `<ms> <phase>` line. Never throws; failures disable the
  /// log permanently (there is nowhere left to report a broken log).
  void write(String phase) {
    final path = _path;
    if (path == null || _broken) {
      return;
    }
    try {
      File(path).writeAsStringSync(
        '${DateTime.now().millisecondsSinceEpoch} $phase\n',
        mode: FileMode.append,
        flush: true,
      );
    } on Object {
      _broken = true;
    }
  }

  /// The default log location: `ak-stage.log` under the platform temp
  /// dir (`TEMP`/`TMP` on Windows, `TMPDIR` elsewhere).
  static String? defaultAkStageLogPath(Map<String, String> environment) {
    final dir = environment['TEMP'] ?? environment['TMP'] ?? environment['TMPDIR'];
    if (dir == null || dir.isEmpty) {
      return null;
    }
    final separator = dir.endsWith('/') || dir.endsWith(r'\')
        ? ''
        : Platform.pathSeparator;
    return '$dir${separator}ak-stage.log';
  }
}

/// The native-lifecycle steps `bootstrapAppService` drives, one method
/// per E4 diagnostic stage (crash-analysis report, E4 table):
///
/// | stage | owner call | native effect |
/// |---|---|---|
/// | B | [openLibrary] | `DynamicLibrary.open` only — no lookup, no call |
/// | C | [probeTaskExports] | task-plane export LOOKUP, no engine |
/// | D | [engineNew] | `ak_engine_new`; the engine is HELD by the owner |
///
/// C08 ordering is baked into the protocol the real facade implements:
/// open once at the ONE injected path, probe the task-plane exports, and
/// only then create the engine over the SAME opened library (probe before
/// `engine_new`, single resolution). [AkEngineDartFacade] is the real
/// implementation; tests inject fakes that intercept at a chosen depth.
abstract interface class AkStagedEngineOwner {
  /// Stage B: opens the DLL (idempotent — later stages reuse the handle)
  /// without looking up or calling ANY export. Throws the loader's own
  /// error when the path cannot be opened.
  DynamicLibrary openLibrary();

  /// Stage C: probes the ten task-plane exports and returns the MISSING
  /// names (empty = fully supported). Lookup only — no export is called,
  /// no engine is created.
  List<String> probeTaskExports();

  /// Stage D: `ak_engine_new` over the already-open library. Idempotent;
  /// the engine stays owned (and alive) until [dispose].
  void engineNew();

  /// The live engine's native handle (after [engineNew]).
  Pointer<AkEngine> get engineHandle;

  /// C07 (crash-analysis 2026-11-07): the owner's liveness view of the
  /// engine handle borrowers hold. `true` while this owner keeps the
  /// engine alive; [dispose] flips it `false` BEFORE the native
  /// `engine_free` runs, so a task-plane borrower that races the release
  /// observes the revoked lease (a clear [StateError]) instead of
  /// calling into a freed handle. Bootstrap wires this into
  /// `AkTaskFfiBindings` as its `isAlive` probe.
  bool get isEngineAlive;

  /// The opened library (after [openLibrary]).
  DynamicLibrary get library;

  /// Releases the engine when this owner created it. The loaded library
  /// itself is never unloaded (DynamicLibrary has no unload). Idempotent.
  void dispose();
}

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

/// Real [AkEngineFacade] over one lazily-opened [AkEngineDart], and the
/// real [AkStagedEngineOwner] the app bootstrap drives (E4 staged
/// diagnosis + C08 single-resolution loading).
///
/// C08 loading contract: when [dllPath] is injected, it is the ONE
/// resolution — the facade opens exactly that path (`DynamicLibrary.open`
/// once), probes the task-plane exports on the SAME opened library, and
/// creates the engine over that library (`ak_engine_new`), never
/// re-resolving. Without an injected path the legacy lazy behavior is
/// preserved (the three-tier probe runs on first use) for the invite /
/// transfer services that construct this facade bare.
///
/// The engine handle is opened on first use through the same three-tier
/// DLL probe as [AkEngineBackend] when no [dllPath] is given (a missing
/// DLL therefore throws here, which [FfiTransferService] turns into the
/// degraded identity/empty history).
final class AkEngineDartFacade implements AkEngineFacade, AkStagedEngineOwner {
  /// [engine] may be supplied at integration time (e.g. the handle owned
  /// by `AkEngineBackend`); when null the facade opens its own lazily.
  ///
  /// [dllPath] (C08) pins the exact library file to open — the bootstrap
  /// resolves once and hands the absolute path here, eliminating the
  /// second, potentially different, probe this facade used to run.
  /// [stageLog] (E2) receives the startup phase timing lines; it defaults
  /// to the process-wide [AkStageLog.shared].
  AkEngineDartFacade({AkEngineDart? engine, String? dllPath, AkStageLog? stageLog})
      : _dllPath = (dllPath == null || dllPath.isEmpty) ? null : dllPath,
        _log = stageLog ?? AkStageLog.shared() {
    _engine = engine;
  }

  AkEngineDart? _engine;

  /// The injected single resolution (C08); null = resolve lazily.
  String? _dllPath;

  /// The opened library — opened ONCE, reused by every later stage.
  DynamicLibrary? _library;

  final AkStageLog _log;

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
  /// The loaded library itself is never unloaded (DynamicLibrary exposes
  /// no unload — Windows keeps the module mapped for the process life).
  ///
  /// C07 lease order: `_disposed` (what [isEngineAlive] reads) flips
  /// FIRST; the native `engine_free` runs after it — every borrower
  /// already sees a revoked lease before the handle dies.
  @override
  void dispose() {
    if (_disposed) {
      return;
    }
    _disposed = true;
    _engine?.dispose();
    _engine = null;
  }

  // -- AkStagedEngineOwner (E4 stages B/C/D; C08 open-once) --------------

  @override
  DynamicLibrary get library => openLibrary();

  @override
  DynamicLibrary openLibrary() {
    if (_disposed) {
      throw StateError('AkEngineDartFacade is disposed');
    }
    final opened = _library;
    if (opened != null) {
      return opened; // open ONCE — every stage reuses this handle
    }
    final injected = _engine;
    if (injected != null && !injected.isDisposed) {
      // Engine-injected seam: its library is already open.
      return _library = injected.bindings.library;
    }
    final path = _dllPathForOpen();
    _log.write('dll_open_before');
    final library = DynamicLibrary.open(path);
    _log.write('dll_open_after');
    return _library = library;
  }

  /// The one open path: the injected [AkStagedEngineOwner] resolution
  /// (C08), else the legacy lazy three-tier probe.
  String _dllPathForOpen() {
    final injected = _dllPath;
    if (injected != null) {
      return injected;
    }
    final resolution = resolveAkDllPath();
    if (resolution == null) {
      // Reuse AkEngineBackend's probe-diagnostics exception shape.
      throw _DllMissing();
    }
    return _dllPath = resolution.path;
  }

  @override
  List<String> probeTaskExports() {
    _log.write('probe_task_exports_before');
    final missing = AkTaskFfiBindings.missingTaskExports(openLibrary());
    _log.write('probe_task_exports_after');
    return missing;
  }

  @override
  void engineNew() {
    _engineOrThrow();
  }

  @override
  Pointer<AkEngine> get engineHandle => _engineOrThrow().nativeHandle;

  /// C07 lease probe: alive until THIS facade's dispose — which sets
  /// `_disposed` FIRST and only then runs the native `engine_free` — and
  /// while the held engine itself is not disposed out-of-band. Borrowers
  /// of the handle handed out before that moment observe the flip.
  @override
  bool get isEngineAlive {
    if (_disposed) {
      return false;
    }
    final engine = _engine;
    return engine == null || !engine.isDisposed;
  }

  /// Opens (once) and returns the engine handle; missing DLL throws so the
  /// caller can degrade. A disposed facade always throws [StateError].
  ///
  /// The engine is created OVER the already-open library (C08: probe the
  /// exports first, then `ak_engine_new` on the same handle — no second
  /// `DynamicLibrary.open`, no re-resolution of the path).
  AkEngineDart _engineOrThrow() {
    if (_disposed) {
      throw StateError('AkEngineDartFacade is disposed');
    }
    final existing = _engine;
    if (existing != null && !existing.isDisposed) {
      return existing;
    }
    final library = openLibrary();
    _log.write('engine_new_before');
    final created = AkEngineDart(
      bindings: AkFfiBindings(library: library, dllPath: _dllPath),
    );
    _log.write('engine_new_after');
    return _engine = created;
  }

  /// The live engine handle for wave-22 borrowers: the pairing/scheduler
  /// bindings need the engine's native pointer AND its loaded
  /// `DynamicLibrary` to look up the `ak_pairing_*`/`ak_sched_*` exports
  /// (ffi-v2.md §2 — the facade borrows the handle and never
  /// double-disposes; the borrowers must NOT call `engine.dispose`).
  ///
  /// Throws like the facade's other calls when the DLL is missing; a
  /// missing wave-22 export then surfaces from the binding lookup as the
  /// explicit "old DLL unsupported" error.
  AkEngineDart borrowEngine() => _engineOrThrow();
}

/// Minimal stand-in for AkEngineBackend's DllNotFoundException (which lives
/// in transfer_service.dart of this layer); kept private so callers only
/// need "it threw".
final class _DllMissing implements Exception {
  @override
  String toString() => '未找到 ak_ffi 动态库（identityInfo/jobsList 探测失败）';
}
