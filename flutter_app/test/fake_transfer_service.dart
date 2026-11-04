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
    List<PeerEntryUi>? peers,
    this.listPeersError,
  })  : deviceInfoValue = deviceInfo ??
            DeviceInfo(
              deviceName: 'DESKTOP-AK1',
              fingerprintHex: 'ab' * 32,
              pairingCode: '123-456',
              peersCount: peers?.length ?? 0,
            ),
        _jobs = List.of(jobs ?? const <JobRecord>[]),
        _peers = List.of(peers ?? const <PeerEntryUi>[]);

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

  // -------------------------------------------------------------------------
  // Pairing book (wave-14) — an in-memory peers.book double.
  // -------------------------------------------------------------------------

  final List<PeerEntryUi> _peers;

  /// When non-null, [listPeers] throws it (the corrupt-book / engine
  /// error forms the pairing page must render inline).
  Object? listPeersError;

  /// Timestamp stored as pairedAt by [addPeer] (fixed for determinism).
  DateTime peerAddClock = DateTime.fromMillisecondsSinceEpoch(1730000000000);

  int listPeersCalls = 0;
  int addPeerCalls = 0;
  int removePeerCalls = 0;

  /// Arguments captured by the latest [addPeer] call.
  String? lastAddPeerFingerprint;
  String? lastAddPeerName;

  /// Argument captured by the latest [removePeer] call.
  String? lastRemovePeerFingerprint;

  /// Live pairing-book fixture; mutations are visible to the next
  /// [listPeers] (sorted copy) like the engine's disk read.
  List<PeerEntryUi> get peers => _peers;

  @override
  Future<List<PeerEntryUi>> listPeers() async {
    listPeersCalls += 1;
    final error = listPeersError;
    if (error != null) {
      throw error;
    }
    final copy = List.of(_peers)
      ..sort((a, b) => a.fingerprint.compareTo(b.fingerprint));
    return copy;
  }

  @override
  Future<int> addPeer(String fpHex, String name) async {
    addPeerCalls += 1;
    lastAddPeerFingerprint = fpHex;
    lastAddPeerName = name;
    final fingerprint = fpHex.trim().toLowerCase();
    if (!isFingerprintHex(fingerprint)) {
      // Mirrors the real adapter's local validation (64 hex only).
      throw const FormatException('指纹须为 64 位十六进制字符。');
    }
    final trimmed = name.trim();
    final index =
        _peers.indexWhere((peer) => peer.fingerprint == fingerprint);
    if (index >= 0) {
      if (trimmed.isNotEmpty) {
        // Rename keeps the original pairing time (engine semantics).
        _peers[index] = PeerEntryUi(
          fingerprint: fingerprint,
          name: trimmed,
          pairedAt: _peers[index].pairedAt,
        );
      } // re-add without a name: record untouched (idempotent)
    } else {
      _peers.add(PeerEntryUi(
        fingerprint: fingerprint,
        name: trimmed.isEmpty ? '(unnamed)' : trimmed,
        pairedAt: peerAddClock,
      ));
    }
    return _peers.length;
  }

  @override
  Future<bool> removePeer(String fpHex) async {
    removePeerCalls += 1;
    lastRemovePeerFingerprint = fpHex;
    final fingerprint = fpHex.trim().toLowerCase();
    final index =
        _peers.indexWhere((peer) => peer.fingerprint == fingerprint);
    if (index < 0) {
      return false; // engine -4 not-found form mapped by the contract
    }
    _peers.removeAt(index);
    return true;
  }

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
/// [identityThrows]) and the jobsList forms (empty / rows), plus the
/// wave-14 pairing-book forms (rows / corrupt-book error via
/// [peerListError], add/rename/remove idempotence).
///
/// Shape mirrors the frozen engine contract: identityInfo keys
/// device_id/fingerprint/pairing_code/peers/created; jobsList row keys
/// id/direction/state/files/bytes/updated_unix_ms/peer/root; peerList row
/// keys fingerprint/name/paired_unix_ms.
class FakeAkEngineFacade implements AkEngineFacade {
  FakeAkEngineFacade({
    Map<String, dynamic>? identityInfo,
    this.identityThrows = false,
    List<Map<String, dynamic>>? jobsRows,
    this.jobsThrows = false,
    List<Map<String, dynamic>>? peerRows,
    this.peerListError,
  })  : identityInfoValue = identityInfo ??
            <String, dynamic>{
              'device_id': 'abababab',
              'fingerprint': 'ab' * 32,
              'pairing_code': '123456',
              'peers': peerRows?.length ?? 0,
              'created': false,
            },
        _jobsRows = List.of(jobsRows ?? const <Map<String, dynamic>>[]),
        _peerRows = List.of(peerRows ?? const <Map<String, dynamic>>[]);

  /// Answer of [identityInfo].
  final Map<String, dynamic> identityInfoValue;

  /// When true, [identityInfo] throws like a missing DLL / absent export.
  final bool identityThrows;

  final List<Map<String, dynamic>> _jobsRows;

  /// When true, [jobsList] throws like a missing DLL / absent export.
  final bool jobsThrows;

  final List<Map<String, dynamic>> _peerRows;

  /// When non-null, [peerList] throws it (the corrupt peers.book form:
  /// the engine's AK_ERR_INVALID "cannot load peer book").
  final Object? peerListError;

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

  @override
  List<Map<String, dynamic>> peerList(String idDir) {
    idDirRequests.add(('peerList', idDir));
    final error = peerListError;
    if (error != null) {
      throw error;
    }
    final copy = List.of(_peerRows)
      ..sort((a, b) => (a['fingerprint'] as String)
          .toLowerCase()
          .compareTo((b['fingerprint'] as String).toLowerCase()));
    return copy;
  }

  @override
  int peerAdd(String idDir, String fpHex, {String? name}) {
    idDirRequests.add(('peerAdd', idDir));
    final fingerprint = fpHex.trim().toLowerCase();
    if (!isFingerprintHex(fingerprint)) {
      throw const FormatException('指纹须为 64 位十六进制字符。');
    }
    final trimmed = name?.trim() ?? '';
    final index = _peerRows.indexWhere(
        (row) => (row['fingerprint'] as String).toLowerCase() == fingerprint);
    if (index >= 0) {
      if (trimmed.isNotEmpty) {
        _peerRows[index] = <String, dynamic>{
          ..._peerRows[index],
          'name': trimmed, // rename keeps paired_unix_ms (engine rule)
        };
      } // re-add without a name: untouched (idempotent)
    } else {
      _peerRows.add(<String, dynamic>{
        'fingerprint': fingerprint,
        'name': trimmed.isEmpty ? '(unnamed)' : trimmed,
        'paired_unix_ms': 1730000000000,
      });
    }
    return _peerRows.length;
  }

  @override
  int peerRemove(String idDir, String fpHex) {
    idDirRequests.add(('peerRemove', idDir));
    final fingerprint = fpHex.trim().toLowerCase();
    if (!isFingerprintHex(fingerprint)) {
      throw const FormatException('指纹须为 64 位十六进制字符。');
    }
    final index = _peerRows.indexWhere(
        (row) => (row['fingerprint'] as String).toLowerCase() == fingerprint);
    if (index < 0) {
      throw const AkFfiException(
          AkStatus.errNotFound, 'fingerprint not in peer book');
    }
    _peerRows.removeAt(index);
    return _peerRows.length;
  }

  /// Appends one jobs row (test fixture helper).
  void addJobRow(Map<String, dynamic> row) => _jobsRows.insert(0, row);
}
