import 'dart:async' show Completer;

import 'package:ak_ffi_dart/ak_ffi_dart.dart';
import 'package:ak_sending_app/services/ak_engine_facade.dart';
import 'package:ak_sending_app/ui/service/transfer_service.dart';

/// Deterministic in-memory [TransferService] for widget tests: pages get
/// controllers they can watch, while the test drives the event timeline
/// through [emitReceive]/[emitSend] and folds terminal results through
/// [finishReceive]/[finishSend]. No FFI, no timers.
class FakeTransferService implements TransferService {
  FakeTransferService({
    DeviceInfo? deviceInfo,
    this.versionText = 'ak-ffi 1 (wire v1, caps 0x0007)',
    List<JobRecord>? jobs,
    this.jobsGate,
    this.refreshIdentityResult,
  })  : deviceInfoValue = deviceInfo ??
            DeviceInfo(
              deviceName: 'DESKTOP-AK1',
              fingerprintHex: 'ab' * 32,
              pairingCode: '123-456',
              peersCount: 0,
            ),
        _jobs = List.of(jobs ?? const <JobRecord>[]);

  /// Identity returned by [deviceInfo]; mutated by [refreshIdentity] when
  /// [refreshIdentityResult] is set (the three identity forms of the
  /// wave-12 wiring: normal / created / degraded).
  DeviceInfo deviceInfoValue;

  /// String returned by [version].
  final String versionText;

  /// When non-null, [listJobs] awaits this completer before answering —
  /// drives the JobsPage loading state.
  final Completer<List<JobRecord>>? jobsGate;

  /// Identity handed back by the NEXT [refreshIdentity] (and cached into
  /// [deviceInfoValue]); null keeps the constructor identity.
  DeviceInfo? refreshIdentityResult;

  /// Directories captured by [setIdentityDir] calls.
  final List<String> identityDirRequests = [];

  /// Value returned by [defaultReceiveOutDir]; '' (default) disables the
  /// receive-page prefill so desktop-style tests are unaffected.
  String defaultReceiveOutDirValue = '';

  @override
  Future<String> defaultReceiveOutDir() async => defaultReceiveOutDirValue;

  /// How many times pages called [refreshIdentity].
  int refreshIdentityCalls = 0;

  final List<JobRecord> _jobs;
  final List<void Function()> _statusListeners = [];

  /// Options captured by the latest [startReceive] call.
  ReceiveOptions? lastReceiveOptions;

  /// Options captured by the latest [startSend] call.
  SendOptions? lastSendOptions;

  int startReceiveCalls = 0;
  int startSendCalls = 0;
  int stopReceiveCalls = 0;
  int disposeCalls = 0;

  /// When true, [startReceive] throws like a busy real service would.
  Object? receiveStartError;

  JobController? _activeReceive;
  JobController? _lastSend;

  TransferServiceStatus _status = TransferServiceStatus.idle;

  @override
  String version() => versionText;

  @override
  DeviceInfo deviceInfo() => deviceInfoValue;

  @override
  Future<DeviceInfo> refreshIdentity() async {
    refreshIdentityCalls += 1;
    final result = refreshIdentityResult;
    if (result != null) {
      deviceInfoValue = result;
      for (final listener in List.of(_statusListeners)) {
        listener();
      }
    }
    return deviceInfoValue;
  }

  @override
  void setIdentityDir(String dir) {
    identityDirRequests.add(dir);
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
  bool get isListening => _activeReceive != null;

  /// Controller of the active receive listener (null before start).
  JobController? get activeReceive => _activeReceive;

  /// Controller of the most recent send (null before start).
  JobController? get lastSend => _lastSend;

  @override
  JobController startReceive(ReceiveOptions options) {
    startReceiveCalls += 1; // counts attempts, including rejected ones
    if (receiveStartError != null) {
      throw receiveStartError!;
    }
    if (isListening) {
      throw StateError('already listening');
    }
    lastReceiveOptions = options;
    _activeReceive = JobController();
    _setStatus(TransferServiceStatus.listening);
    return _activeReceive!;
  }

  @override
  JobController startSend(SendOptions options) {
    lastSendOptions = options;
    startSendCalls += 1;
    _lastSend = JobController();
    _setStatus(TransferServiceStatus.busy);
    return _lastSend!;
  }

  @override
  Future<void> stopReceive() async {
    stopReceiveCalls += 1;
    _activeReceive = null;
    _setStatus(TransferServiceStatus.idle);
  }

  @override
  Future<List<JobRecord>> listJobs() async {
    final gate = jobsGate;
    if (gate != null && !gate.isCompleted) {
      await gate.future;
    }
    return List.of(_jobs);
  }

  /// Adds a record to the history fixture.
  void addJob(JobRecord record) => _jobs.insert(0, record);

  /// Feeds one progress event into the active receive controller.
  void emitReceive(ProgressEvent event) => _activeReceive?.feed(event);

  /// Feeds one progress event into the latest send controller.
  void emitSend(ProgressEvent event) => _lastSend?.feed(event);

  /// Folds a terminal outcome into the active receive controller.
  void finishReceive(JobOutcome outcome) {
    _activeReceive?.setResult(outcome);
    _setStatus(TransferServiceStatus.idle);
  }

  /// Folds a terminal outcome into the latest send controller.
  void finishSend(JobOutcome outcome) {
    _lastSend?.setResult(outcome);
    _setStatus(TransferServiceStatus.idle);
  }

  @override
  void dispose() {
    disposeCalls += 1;
    _activeReceive = null;
    _lastSend = null;
  }

  void _setStatus(TransferServiceStatus status) {
    _status = status;
    for (final listener in List.of(_statusListeners)) {
      listener();
    }
  }
}

/// A fixed 64-hex peer fingerprint used across tests.
const String kPeerFingerprint =
    '6c31041268f471609c79f5f2dbcc38e4a4ab2f4d416109a4e09fcf50fd0f0062';

/// In-memory [AkEngineFacade] double driving the three identityInfo forms
/// of the wave-12 wiring (normal / created / missing-DLL degrade via
/// [identityThrows]) and the jobsList forms (empty / rows).
///
/// Shape mirrors the frozen engine contract: identityInfo keys
/// device_id/fingerprint/pairing_code/peers/created; jobsList row keys
/// id/direction/state/files/bytes/updated_unix_ms/peer/root.
class FakeAkEngineFacade implements AkEngineFacade {
  FakeAkEngineFacade({
    Map<String, dynamic>? identityInfo,
    this.identityThrows = false,
    List<Map<String, dynamic>>? jobsRows,
    this.jobsThrows = false,
  })  : identityInfoValue = identityInfo ??
            <String, dynamic>{
              'device_id': 'abababab',
              'fingerprint': 'ab' * 32,
              'pairing_code': '123456',
              'peers': 0,
              'created': false,
            },
        _jobsRows = List.of(jobsRows ?? const <Map<String, dynamic>>[]);

  /// Answer of [identityInfo].
  final Map<String, dynamic> identityInfoValue;

  /// When true, [identityInfo] throws like a missing DLL / absent export.
  final bool identityThrows;

  final List<Map<String, dynamic>> _jobsRows;

  /// When true, [jobsList] throws like a missing DLL / absent export.
  final bool jobsThrows;

  /// idDir arguments captured per call site.
  final List<(String site, String idDir)> idDirRequests = [];

  @override
  Map<String, dynamic> identityInfo(String idDir) {
    idDirRequests.add(('identityInfo', idDir));
    if (identityThrows) {
      throw StateError('未找到 ak_ffi 动态库（fake: DLL missing）');
    }
    return identityInfoValue;
  }

  @override
  List<Map<String, dynamic>> jobsList(String idDir) {
    idDirRequests.add(('jobsList', idDir));
    if (jobsThrows) {
      throw StateError('未找到 ak_ffi 动态库（fake: DLL missing）');
    }
    return List.of(_jobsRows);
  }

  /// Appends one jobs row (test fixture helper).
  void addJobRow(Map<String, dynamic> row) => _jobsRows.insert(0, row);
}
