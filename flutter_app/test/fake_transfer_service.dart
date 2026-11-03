import 'package:ak_ffi_dart/ak_ffi_dart.dart';
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
  })  : deviceInfoValue = deviceInfo ??
            DeviceInfo(
              deviceName: 'DESKTOP-AK1',
              fingerprintHex: 'ab' * 32,
              pairingCode: '123-456',
            ),
        _jobs = List.of(jobs ?? const <JobRecord>[]);

  /// Identity returned by [deviceInfo].
  final DeviceInfo deviceInfoValue;

  /// String returned by [version].
  final String versionText;

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
    if (receiveStartError != null) {
      throw receiveStartError!;
    }
    if (isListening) {
      throw StateError('already listening');
    }
    lastReceiveOptions = options;
    startReceiveCalls += 1;
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
  Future<List<JobRecord>> listJobs() async => List.of(_jobs);

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
