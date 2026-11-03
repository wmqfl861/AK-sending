/// `FfiTransferService`: the authoritative implementation of the UI contract
/// `lib/ui/service/transfer_service.dart` (written by the parallel UI agent;
/// see the integration note there) on top of [RealTransferService].
///
/// Layering:
///
/// ```text
/// lib/ui pages  ──▶  ui/service/transfer_service.dart (contract)
///                           ▲
/// lib/services/ffi_transfer_service.dart   (this file: sessions/status)
///                           ▲
/// lib/services/real_transfer_service.dart  (job registry + event pump)
///                           ▲
/// ak_ffi_dart (AkEngineDart) ── ak_ffi.dll ── ak-engine
/// ```
///
/// Per job (send or receive) it owns one `JobController`; the core pump's
/// events are routed by job id and folded in, and a slow result poller
/// delivers the terminal `ak_job_result` body via `setResultJson` — the
/// poll → JobController pattern of packages/ak_ffi_dart's loopback demo.
///
/// Wave-12 wiring (this file):
/// * `deviceInfo()`/`refreshIdentity()` — real identity via
///   [AkEngineFacade.identityInfo] (`engine.identityInfo(idDir)`, frozen
///   contract); the cached [DeviceInfo] carries fingerprint, pairing code,
///   pairing-book size and the first-run "created" flag. A missing DLL or a
///   not-yet-exported engine API degrades to the hostname-only identity.
/// * `AdmissionMode.book` — the pairing book is wired engine-side:
///   `startReceive` sends `expect:"book"` straight to the engine (spec
///   `id_dir` points at the same identity store), and an empty book is
///   rejected by the engine with its `last_error` text passed through to
///   the page copy.
/// * `listJobs()` — a real read of the jobs checkpoint store through
///   [AkEngineFacade.jobsList] (`engine.jobsList(idDir)`), mapped to
///   [JobRecord] rows, newest first.
library;

import 'dart:async' show StreamSubscription, Timer;
import 'dart:io' show Platform;

import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show
        AkReceiveSpec,
        AkSendSpec,
        JobController,
        JobPhase,
        ProgressEvent,
        isFingerprintHex;

import '../ui/service/transfer_service.dart'
    show
        AdmissionMode,
        DeviceInfo,
        JobDirection,
        JobRecord,
        JobRecordState,
        ReceiveOptions,
        SendOptions,
        TransferService,
        TransferServiceStatus;

import 'ak_engine_facade.dart';
import 'real_transfer_service.dart';

/// One live job: its core job id, folding controller and terminal state.
final class _Session {
  _Session({required this.jobId, required this.controller, required this.direction});

  final String jobId;
  final JobController controller;
  final JobDirection direction;
  bool hasResult = false;
}

/// See the library doc. Construction never throws: an engine-open failure
/// (missing DLL) degrades to [TransferServiceStatus.error] with the probe
/// candidates surfaced through [version].
final class FfiTransferService implements TransferService {
  FfiTransferService({
    EngineBackend? backend,
    AkEngineFacade? engineFacade,
    Duration pollInterval = const Duration(milliseconds: 50),
    this.resultPollInterval = const Duration(milliseconds: 200),
  })  : _facade = engineFacade ?? AkEngineDartFacade(),
        _identityDir = defaultIdentityDir() {
    RealTransferService? core;
    try {
      core = backend == null
          ? RealTransferService(pollInterval: pollInterval)
          : RealTransferService(backend: backend, pollInterval: pollInterval);
      _versionText = core.version();
    } on Object catch (error) {
      // DllNotFoundException and friends: keep the app usable, surface the
      // reason on the status dot / version card, refuse starts.
      _loadError = error;
      _versionText = '传输引擎不可用：$error';
    }
    _core = core;
    _eventsSubscription = core?.events.listen(_onEvent);
    _resultPoller = Timer.periodic(resultPollInterval, (_) => _pollResults());
    _status = _deriveStatus(); // e.g. error right away on an open failure
  }

  /// Default identity directory: `%USERPROFILE%\.ak-sending` (PathProvider
  /// is unavailable for this desktop shell — plain `Platform.environment`).
  /// [environment] is injectable for tests.
  static String defaultIdentityDir({Map<String, String>? environment}) {
    final env = environment ?? Platform.environment;
    final home = env['USERPROFILE'] ?? env['HOME'] ?? '';
    if (home.isEmpty) {
      return '.ak-sending';
    }
    final separator = Platform.isWindows ? r'\' : '/';
    return '$home$separator.ak-sending';
  }

  /// How often unfinished sessions are asked for their terminal result.
  final Duration resultPollInterval;

  RealTransferService? _core;
  late final String _versionText;
  Object? _loadError;

  /// Engine identity/jobs-store seam (frozen wave-12 contract).
  final AkEngineFacade _facade;

  /// Identity directory used by the facade and every job spec (`id_dir`).
  String _identityDir;

  /// Cached identity; null until the first [deviceInfo]/[refreshIdentity].
  DeviceInfo? _identity;

  StreamSubscription<ProgressEvent>? _eventsSubscription;
  late final Timer _resultPoller;

  /// Active receive listener (null while none). Single listener this wave,
  /// mirroring the once_jobs=1 single-connection receive path.
  _Session? _receive;

  /// Finished + live send sessions, newest last, bounded.
  final List<_Session> _sends = [];
  static const int _maxRememberedSends = 16;

  final List<void Function()> _statusListeners = [];

  TransferServiceStatus _status = TransferServiceStatus.idle;
  bool _disposed = false;

  // -------------------------------------------------------------------------
  // Identity / version / status.
  // -------------------------------------------------------------------------

  @override
  String version() => _versionText;

  @override
  DeviceInfo deviceInfo() => _identity ??= _degradedIdentity();

  @override
  Future<DeviceInfo> refreshIdentity() async {
    DeviceInfo next;
    if (_disposed || _loadError != null) {
      // No engine to ask: degrade to the honest hostname-only card.
      next = _degradedIdentity();
    } else {
      try {
        final info = _facade.identityInfo(_identityDir);
        final fingerprint = _stringOf(info, 'fingerprint');
        next = DeviceInfo(
          deviceName: _hostDisplayName(),
          fingerprintHex: fingerprint.toLowerCase(),
          pairingCode: _stringOf(info, 'pairing_code'),
          peersCount: _intOf(info, 'peers'),
          identityCreated: info['created'] == true,
        );
      } on Object {
        // Missing DLL, engine error, or the identityInfo export not landed
        // yet (AkEngineDartFacade's wave-12 TODO): degrade, never throw.
        next = _degradedIdentity();
      }
    }
    _identity = next;
    _notifyListeners(); // identity card/banner re-render
    return next;
  }

  @override
  void setIdentityDir(String dir) {
    final trimmed = dir.trim();
    _identityDir = trimmed.isEmpty ? defaultIdentityDir() : trimmed;
  }

  /// The identity directory every facade call and job spec (`id_dir`) uses.
  String get identityDir => _identityDir;

  /// Degraded identity: real hostname (what peers see via `device_name`),
  /// empty fingerprint/pairing code rendered as honest placeholders.
  DeviceInfo _degradedIdentity() => DeviceInfo(
        deviceName: _hostDisplayName(),
        fingerprintHex: '',
        pairingCode: '',
      );

  static String _hostDisplayName() {
    final host = Platform.localHostname;
    return host.length > 64 ? host.substring(0, 64) : host;
  }

  static String _stringOf(Map<String, dynamic> map, String key) =>
      map[key] is String ? map[key] as String : '';

  static int _intOf(Map<String, dynamic> map, String key) =>
      map[key] is int ? map[key] as int : 0;

  @override
  TransferServiceStatus get status => _status;

  @override
  void addStatusListener(void Function() listener) {
    _statusListeners.add(listener);
  }

  @override
  void removeStatusListener(void Function() listener) {
    _statusListeners.remove(listener);
  }

  @override
  bool get isListening {
    final receive = _receive;
    return receive != null && !receive.hasResult;
  }

  // -------------------------------------------------------------------------
  // Receive.
  // -------------------------------------------------------------------------

  @override
  JobController startReceive(ReceiveOptions options) {
    final core = _requireEngine();
    if (isListening) {
      throw StateError('已有监听在进行；请先停止当前监听。');
    }
    String? expect;
    switch (options.admission) {
      case AdmissionMode.book:
        // Pairing-book admission goes straight to the engine
        // (expect:"book", frozen wave-12 contract; the model layer accepts
        // any string). An empty book is rejected engine-side and the
        // engine's last_error text propagates up to the page copy.
        expect = 'book';
      case AdmissionMode.any:
        expect = 'any';
      case AdmissionMode.pinned:
        final text = options.expectText?.trim() ?? '';
        if (text.isEmpty) {
          throw ArgumentError('指定准入需要指纹或 6 位配对码。');
        }
        expect = text;
    }
    final deviceName = deviceInfo().deviceName;
    final jobId = core.startReceive(AkReceiveSpec(
      outDir: options.outDir,
      port: options.port,
      expect: expect,
      conflict: options.conflict,
      timeoutSecs: options.timeoutSecs,
      idDir: _identityDir,
      deviceName: deviceName.isEmpty ? null : deviceName,
    ));
    final session = _Session(
      jobId: jobId,
      controller: JobController(),
      direction: JobDirection.receive,
    );
    _receive = session;
    _setStatus(_deriveStatus());
    return session.controller;
  }

  @override
  Future<void> stopReceive() async {
    final core = _core;
    final receive = _receive;
    if (core == null || receive == null) {
      return;
    }
    // Cancel is AK_OK even for a finished job; wait for the terminal body
    // so the card shows the final state before the page unmounts it.
    try {
      core.cancel(receive.jobId);
    } on Exception {
      // already-finished races: the result poll below is the source of truth
    }
    await _awaitTerminal(core, receive,
        const Duration(seconds: 5));
    _receive = null;
    _setStatus(_deriveStatus());
  }

  // -------------------------------------------------------------------------
  // Send.
  // -------------------------------------------------------------------------

  @override
  JobController startSend(SendOptions options) {
    final core = _requireEngine();
    final fingerprint = options.fingerprint.trim();
    if (!isFingerprintHex(fingerprint)) {
      // A prefix would need pairing-book resolution (ak-cli `send --fp`);
      // without peers.book this wave only the full 64-hex form can start.
      throw StateError(
        '指纹前缀需经配对簿查补全：本波未接入 peers.book，请粘贴完整 64 位指纹。',
      );
    }
    final jobId = core.startSend(AkSendSpec(
      sourceRoots: options.sourceRoots,
      peer: options.peer,
      fpHex: fingerprint.toLowerCase(),
      multistream: options.multistream,
      noInternet: options.noInternet,
      conflict: options.conflict,
      timeoutSecs: options.timeoutSecs,
      idDir: _identityDir,
      deviceName: deviceInfo().deviceName,
    ));
    final session = _Session(
      jobId: jobId,
      controller: JobController(),
      direction: JobDirection.send,
    );
    _sends.add(session);
    if (_sends.length > _maxRememberedSends) {
      _sends.removeRange(0, _sends.length - _maxRememberedSends);
    }
    _setStatus(_deriveStatus());
    return session.controller;
  }

  // -------------------------------------------------------------------------
  // History / lifecycle.
  // -------------------------------------------------------------------------

  @override
  Future<List<JobRecord>> listJobs() async {
    // Real read of the jobs checkpoint store through the facade (frozen
    // wave-12 contract). Until the engine export lands — or when the DLL
    // is missing — the facade throws and we degrade to an honest empty
    // list rather than surfacing an error on the history page.
    if (_disposed || _loadError != null) {
      return const <JobRecord>[];
    }
    final List<Map<String, dynamic>> rows;
    try {
      rows = _facade.jobsList(_identityDir);
    } on Object {
      return const <JobRecord>[];
    }
    final records = <JobRecord>[];
    for (final row in rows) {
      final record = _jobRecordOf(row);
      if (record != null) {
        records.add(record);
      }
    }
    records.sort((a, b) => b.startedAt.compareTo(a.startedAt));
    return records;
  }

  /// Maps one `engine.jobsList` row (`id`/`direction`/`state`/`files`/
  /// `bytes`/`updated_unix_ms`/`peer`/`root`) to a [JobRecord]; null for a
  /// row without a usable id (skipped defensively).
  ///
  /// The engine persists checkpoint states as snake_case strings
  /// (`connecting`/`transferring`/`completed`/`failed`/`failed_storage_full`
  /// /...); terminal mapping: `completed` → done, `failed*`/`cancelled` →
  /// failed, everything else → running. `peer` arrives as `""` when a
  /// record has none — normalized to null.
  static JobRecord? _jobRecordOf(Map<String, dynamic> row) {
    final idRaw = row['id'];
    if (idRaw is! String || idRaw.isEmpty) {
      return null;
    }
    final updated = row['updated_unix_ms'];
    final direction =
        row['direction'] == 'send' ? JobDirection.send : JobDirection.receive;
    final stateText = row['state'] is String ? row['state'] as String : '';
    final state = switch (stateText) {
      'completed' || 'done' => JobRecordState.done,
      _ when stateText.startsWith('failed') || stateText == 'cancelled' =>
        JobRecordState.failed,
      _ => JobRecordState.running,
    };
    final peerRaw = row['peer'];
    final rootRaw = row['root'];
    return JobRecord(
      id: idRaw.length > 8 ? idRaw.substring(0, 8) : idRaw,
      direction: direction,
      startedAt: DateTime.fromMillisecondsSinceEpoch(
          updated is int && updated >= 0 ? updated : 0),
      state: state,
      files: row['files'] is int ? row['files'] as int : 0,
      bytes: row['bytes'] is int ? row['bytes'] as int : 0,
      peer: peerRaw is String && peerRaw.isNotEmpty ? peerRaw : null,
      root: rootRaw is String && rootRaw.isNotEmpty ? rootRaw : null,
      error: row['error'] is String ? row['error'] as String? : null,
    );
  }

  @override
  void dispose() {
    if (_disposed) {
      return;
    }
    _disposed = true;
    _resultPoller.cancel();
    _eventsSubscription?.cancel();
    _core?.dispose();
    final facade = _facade;
    if (facade is AkEngineDartFacade) {
      facade.dispose();
    }
    _receive = null;
    _sends.clear();
    _status = TransferServiceStatus.idle;
  }

  // -------------------------------------------------------------------------
  // Internals: event routing, result folding, status derivation.
  // -------------------------------------------------------------------------

  RealTransferService _requireEngine() {
    if (_disposed) {
      throw StateError('FfiTransferService 已释放。');
    }
    final error = _loadError;
    if (error != null) {
      throw StateError('传输引擎不可用：$error');
    }
    return _core!;
  }

  Iterable<_Session> get _allSessions sync* {
    final receive = _receive;
    if (receive != null) yield receive;
    yield* _sends;
  }

  void _onEvent(ProgressEvent event) {
    for (final session in _allSessions) {
      if (session.jobId == event.job) {
        session.controller.feed(event);
        break;
      }
    }
    _setStatus(_deriveStatus());
  }

  /// One result-poller tick: fold terminal bodies of unfinished sessions.
  void _pollResults() {
    final core = _core;
    if (core == null) {
      return;
    }
    for (final session in _allSessions) {
      if (session.hasResult) {
        continue;
      }
      try {
        _foldIfTerminal(core, session);
      } on Exception {
        // transient poll failure — next tick retries
      }
    }
    _setStatus(_deriveStatus());
  }

  void _foldIfTerminal(RealTransferService core, _Session session) {
    final result = core.result(session.jobId);
    if (result.isRunning) {
      return;
    }
    session.hasResult = true;
    session.controller.setResultJson(result.map!);
  }

  Future<void> _awaitTerminal(
    RealTransferService core,
    _Session session,
    Duration timeout,
  ) async {
    final deadline = DateTime.now().add(timeout);
    while (!session.hasResult && DateTime.now().isBefore(deadline)) {
      try {
        _foldIfTerminal(core, session);
      } on Exception {
        // retry below
      }
      if (session.hasResult) {
        return;
      }
      await Future<void>.delayed(const Duration(milliseconds: 20));
    }
  }

  TransferServiceStatus _deriveStatus() {
    if (_loadError != null) {
      return TransferServiceStatus.error;
    }
    for (final session in _sends) {
      if (!session.hasResult) {
        return TransferServiceStatus.busy;
      }
    }
    final receive = _receive;
    if (receive != null && !receive.hasResult) {
      final phase = receive.controller.phase;
      return phase == JobPhase.transferring || phase == JobPhase.verifying
          ? TransferServiceStatus.busy
          : TransferServiceStatus.listening;
    }
    return TransferServiceStatus.idle;
  }

  void _setStatus(TransferServiceStatus status) {
    if (_status == status) {
      return;
    }
    _status = status;
    _notifyListeners();
  }

  /// Fires every status listener — status changes AND identity refreshes
  /// (pages listen to re-render the identity card / listening banner).
  void _notifyListeners() {
    for (final listener in List<void Function()>.of(_statusListeners)) {
      listener();
    }
  }
}
