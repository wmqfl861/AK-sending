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
/// Wave-11 boundaries (documented in flutter_app/README.md):
/// * `deviceInfo()` — ak-ffi exports no identity introspection yet
///   (`ak_identity_info` is the proposed follow-up API), so fingerprint and
///   pairing code are empty placeholders; the pages degrade gracefully
///   ("指纹无效" chip, empty code). deviceName uses the real hostname.
/// * `AdmissionMode.book` — the pairing book (peers.book) is not wired into
///   the app service this wave, so book == empty book: `startReceive`
///   refuses with pairing guidance, exactly what the Receive page copy
///   promises ("簿为空时会拒绝启动并提示配对步骤").
/// * `listJobs()` — always empty until the jobs checkpoint store lands.
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
        ReceiveOptions,
        SendOptions,
        TransferService,
        TransferServiceStatus;

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
    Duration pollInterval = const Duration(milliseconds: 50),
    this.resultPollInterval = const Duration(milliseconds: 200),
  }) {
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

  /// How often unfinished sessions are asked for their terminal result.
  final Duration resultPollInterval;

  RealTransferService? _core;
  late final String _versionText;
  Object? _loadError;

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
  DeviceInfo deviceInfo() {
    // Wave 11: no ak-ffi identity export (proposed: ak_identity_info).
    // Empty fingerprint/pairing code render as honest placeholders; the
    // hostname is real and is what peers see via `device_name`.
    final host = Platform.localHostname;
    return DeviceInfo(
      deviceName: host.length > 64 ? host.substring(0, 64) : host,
      fingerprintHex: '',
      pairingCode: '',
    );
  }

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
        // peers.book is not wired into the app service this wave, so the
        // book is empty by definition — refuse like ak-cli would.
        throw StateError(
          '配对簿为空（本波未接入 peers.book）：请改用"指定指纹或配对码"，'
          '或确认警告后使用"接受每一个对端"。',
        );
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
    // Wave 11: empty by contract — the jobs checkpoint store (docs/BACKLOG
    // P1-03) is not wired; see core.TransferService.listJobs.
    return const <JobRecord>[];
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
    for (final listener in List<void Function()>.of(_statusListeners)) {
      listener();
    }
  }
}
