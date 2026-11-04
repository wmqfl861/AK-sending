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
/// * Wave-14 wiring (pairing book): `listPeers()`/`addPeer()`/
///   `removePeer()` go through [AkEngineFacade.peerList]/[peerAdd]/
///   [peerRemove] (`engine.peer*`, frozen contract). Unlike the history
///   read these PROPAGATE errors — a corrupt peers.book must surface as
///   an inline reason on the pairing page, not as an empty list. The
///   facade's real adapter throws [UnsupportedError] at its
///   `TODO(wave14-dart)` call sites until the bindings land; every peer
///   write refreshes the cached identity so the HomePage count follows.
///
/// Android adaptation (wave 13): there is no `Platform.environment` home on
/// Android, so the default identity directory resolves asynchronously from
/// path_provider (`getApplicationSupportDirectory()/.ak-sending`) in the
/// constructor. The service absorbs the asynchrony: `refreshIdentity()` and
/// `listJobs()` await [identityDirReady] first, and the receive-page default
/// out-dir comes from [defaultReceiveOutDir] (the app-specific `Download`
/// directory — public Downloads is not writable for arbitrary paths under
/// Android 10+ scoped storage; SAF/MediaStore exports are a later wave).
library;

import 'dart:async' show StreamSubscription, Timer;
import 'dart:io' show Directory, Platform;

import 'package:path_provider/path_provider.dart'
    show
        getApplicationDocumentsDirectory,
        getApplicationSupportDirectory,
        getDownloadsDirectory,
        getExternalStorageDirectory;

import 'package:ak_ffi_dart/ak_ffi_dart.dart'
    show
        AkFfiException,
        AkReceiveSpec,
        AkSendSpec,
        AkStatus,
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
        PeerEntryUi,
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
    bool? isAndroid,
    Future<String> Function()? androidIdentityDir,
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

    // Android: resolve the real identity directory from path_provider and
    // re-anchor every facade call / job spec on it (the sync desktop probe
    // above stays as a harmless provisional until the answer lands).
    _isAndroidPlatform = isAndroid ?? Platform.isAndroid;
    identityDirReady = _isAndroidPlatform
        ? _resolveAndroidIdentityDir(androidIdentityDir)
        : Future<void>.value();
  }

  /// Default identity directory on desktop: `$AK_ID_DIR` when set (test /
  /// operator override, wave-16), else `%USERPROFILE%\.ak-sending`
  /// (PathProvider is unavailable for this desktop shell — plain
  /// `Platform.environment`). On Android this is only the provisional value
  /// used until [identityDirReady] resolves [defaultAndroidIdentityDir].
  /// [environment] is injectable for tests.
  static String defaultIdentityDir({Map<String, String>? environment}) {
    final env = environment ?? Platform.environment;
    // Wave-16 (E2E isolation): explicit operator/test intent wins — the
    // integration test points the app at a throwaway directory instead of
    // polluting the real `%USERPROFILE%\.ak-sending`.
    final overridden = env['AK_ID_DIR'];
    if (overridden != null && overridden.isNotEmpty) {
      return overridden;
    }
    final home = env['USERPROFILE'] ?? env['HOME'] ?? '';
    if (home.isEmpty) {
      return '.ak-sending';
    }
    final separator = Platform.isWindows ? r'\' : '/';
    return '$home$separator.ak-sending';
  }

  /// Android default identity directory: `<app support>/.ak-sending` via
  /// path_provider (`/data/user/0/dev.ak.ak_sending_app/files/.ak-sending`
  /// on a typical device) — app-private storage that needs no permission
  /// and survives across runs (but not uninstall, by Android contract).
  static Future<String> defaultAndroidIdentityDir() async {
    final support = await getApplicationSupportDirectory();
    return _appendSegment(support.path, '.ak-sending');
  }

  /// Default receive out-dir, Android scoped-storage aware (ADR-002 uses
  /// only app-writable roots; exporting to public storage via SAF/MediaStore
  /// is a later wave):
  ///
  /// 1. `getDownloadsDirectory()` — on Android (path_provider ≥2.1) this is
  ///    the **app-specific external** `.../Android/data/<pkg>/files/Download`
  ///    (`getExternalFilesDirs(DIRECTORY_DOWNLOADS)[0]`), NOT the public
  ///    Downloads; writable without any permission. Used verbatim.
  /// 2. `getExternalStorageDirectory()` — the app-specific external root,
  ///    with an app-owned `Download/` subdirectory appended.
  /// 3. `getApplicationDocumentsDirectory()` — internal app documents, with
  ///    the same `Download/` subdirectory (fallback when no external
  ///    storage is mounted).
  ///
  /// Non-Android platforms return `''` — the receive page keeps its
  /// type-the-path desktop UX. Every provider is injectable for tests; a
  /// provider that throws (missing plugin on an exotic host) falls through
  /// to the next tier instead of failing the page.
  static Future<String> resolveDefaultReceiveOutDir({
    bool? isAndroid,
    Future<Directory?> Function()? downloadsDirectory,
    Future<Directory?> Function()? externalStorageDirectory,
    Future<Directory?> Function()? applicationDocumentsDirectory,
  }) async {
    if (!(isAndroid ?? Platform.isAndroid)) {
      return '';
    }
    final downloads =
        await _guardDirectoryCall(downloadsDirectory ?? getDownloadsDirectory);
    if (downloads != null) {
      return downloads.path;
    }
    final external = await _guardDirectoryCall(
        externalStorageDirectory ?? getExternalStorageDirectory);
    if (external != null) {
      return _appendSegment(external.path, 'Download');
    }
    final documents = await _guardDirectoryCall(
        applicationDocumentsDirectory ?? getApplicationDocumentsDirectory);
    if (documents != null) {
      return _appendSegment(documents.path, 'Download');
    }
    return 'Download'; // last resort (tests / headless hosts)
  }

  static Future<Directory?> _guardDirectoryCall(
      Future<Directory?> Function() provider) async {
    try {
      return await provider();
    } on Object {
      // Unsupported platform channel / storage state: try the next tier.
      return null;
    }
  }

  static String _appendSegment(String base, String segment) =>
      base.endsWith('/') || base.endsWith(r'\')
          ? '$base$segment'
          : '$base/$segment';

  /// How often unfinished sessions are asked for their terminal result.
  final Duration resultPollInterval;

  RealTransferService? _core;
  late final String _versionText;
  Object? _loadError;

  /// Engine identity/jobs-store seam (frozen wave-12 contract).
  final AkEngineFacade _facade;

  /// Identity directory used by the facade and every job spec (`id_dir`).
  String _identityDir;

  /// True on Android (constructor-injectable for tests).
  late final bool _isAndroidPlatform;

  /// The path_provider-resolved default identity dir once it lands
  /// (Android only; see [identityDirReady]).
  String? _platformIdentityDir;

  /// True once the user pinned a directory via [setIdentityDir]; the async
  /// Android resolve must not clobber an explicit user choice.
  bool _identityDirOverridden = false;

  /// Completes when the Android identity-directory resolve settles
  /// (immediately on desktop). Never throws: a path_provider failure keeps
  /// the provisional [defaultIdentityDir] and the degraded identity.
  late final Future<void> identityDirReady;

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
    // Android: wait for the real identity dir before the first engine read
    // (path_provider answers within the first frames; pages never see this).
    if (_isAndroidPlatform) {
      await identityDirReady;
    } else {
      // Wave-16 (found by the real-app E2E): never refresh synchronously.
      // Pages call this from initState — inside the build phase — and the
      // notify inside [_refreshIdentityUngated] would hit an ALREADY-BUILT
      // sibling's setState (e.g. PairingPage init → HomePage listener),
      // throwing "setState() called during build" and erroring the page.
      // One microtask hop lands the notify after the frame's build scope;
      // the identity card re-renders one frame later (pages poll, not push).
      await Future<void>.value();
    }
    return _refreshIdentityUngated();
  }

  Future<DeviceInfo> _refreshIdentityUngated() async {
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
    if (trimmed.isEmpty) {
      // Reset to the default: the Android-resolved dir when it has landed,
      // otherwise the (possibly still provisional) desktop-style probe.
      _identityDir = _platformIdentityDir ?? defaultIdentityDir();
      _identityDirOverridden = false;
    } else {
      _identityDir = trimmed;
      _identityDirOverridden = true;
    }
  }

  /// The identity directory every facade call and job spec (`id_dir`) uses.
  String get identityDir => _identityDir;

  /// Platform default receive out-dir (see [resolveDefaultReceiveOutDir]);
  /// '' on desktop — the page keeps asking the user to type a path.
  @override
  Future<String> defaultReceiveOutDir() => resolveDefaultReceiveOutDir();

  /// Resolves the Android identity dir (or [resolver], injectable for
  /// tests), re-anchors [identityDir] and refills the identity card.
  /// Never throws — see [identityDirReady].
  Future<void> _resolveAndroidIdentityDir(
      Future<String> Function()? resolver) async {
    try {
      final dir = await (resolver?.call() ?? defaultAndroidIdentityDir());
      if (dir.isEmpty) {
        return;
      }
      // Remember the platform default even when the user pinned a dir
      // meanwhile — a later setIdentityDir('') reset must land here.
      _platformIdentityDir = dir;
      if (_identityDirOverridden) {
        return; // user pinned a dir meanwhile; keep it
      }
      _identityDir = dir;
      // Ungated on purpose: this call is what identityDirReady waits for.
      await _refreshIdentityUngated();
      _notifyListeners(); // identity card re-renders on the real dir
    } on Object {
      // path_provider unavailable: keep the provisional default; identity
      // stays degraded but the app keeps working.
    }
  }

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
    if (_isAndroidPlatform) {
      await identityDirReady; // read the store under the real dir
    }
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

  // -------------------------------------------------------------------------
  // Pairing book (wave-14).
  // -------------------------------------------------------------------------

  /// Real read of the pairing book through the facade (`engine.peerList`,
  /// frozen wave-14 contract). Unlike [listJobs] errors PROPAGATE: a
  /// corrupt book or an unusable engine surfaces on the pairing page as
  /// an inline reason instead of a silently empty list — the page cannot
  /// tell "empty" from "unreadable" otherwise.
  @override
  Future<List<PeerEntryUi>> listPeers() async {
    if (_isAndroidPlatform) {
      await identityDirReady; // read the book under the real dir
    }
    _requireEngine(); // honest StateError when unusable
    final rows = _facade.peerList(_identityDir);
    final peers = <PeerEntryUi>[];
    for (final row in rows) {
      final entry = _peerEntryOf(row);
      if (entry != null) {
        peers.add(entry);
      }
    }
    peers.sort((a, b) => a.fingerprint.compareTo(b.fingerprint));
    return peers;
  }

  /// Maps one `engine.peerList` row (`fingerprint`/`name`/
  /// `paired_unix_ms`) to a [PeerEntryUi]; null for a row without a
  /// usable fingerprint (skipped defensively, like `_jobRecordOf`).
  static PeerEntryUi? _peerEntryOf(Map<String, dynamic> row) {
    final fingerprintRaw = row['fingerprint'];
    if (fingerprintRaw is! String || fingerprintRaw.isEmpty) {
      return null;
    }
    final nameRaw = row['name'];
    final paired = row['paired_unix_ms'];
    return PeerEntryUi(
      fingerprint: fingerprintRaw.toLowerCase(),
      name: nameRaw is String ? nameRaw : '',
      pairedAt: DateTime.fromMillisecondsSinceEpoch(
          paired is int && paired >= 0 ? paired : 0),
    );
  }

  /// Adds (or renames) one paired peer through the facade. Validation is
  /// mirrored locally (64 hex, no prefix resolution) so a malformed input
  /// fails before any disk write; a blank name is passed as null — the
  /// engine stores "(unnamed)" for new peers. The cached identity is
  /// refreshed afterwards so the HomePage pairing-book count follows.
  @override
  Future<int> addPeer(String fpHex, String name) async {
    if (_isAndroidPlatform) {
      await identityDirReady;
    }
    _requireEngine();
    final fingerprint = fpHex.trim().toLowerCase();
    if (!isFingerprintHex(fingerprint)) {
      throw const FormatException('指纹须为 64 位十六进制字符（前缀不能用于添加，请粘贴完整指纹）。');
    }
    final trimmedName = name.trim();
    final count = _facade.peerAdd(
      _identityDir,
      fingerprint,
      name: trimmedName.isEmpty ? null : trimmedName,
    );
    await _refreshIdentityUngated(); // HomePage peersCount follows the book
    return count;
  }

  /// Removes one paired peer through the facade. The engine's `-4`
  /// not-found form (the fingerprint is not in the book) maps to `false`;
  /// every other failure propagates. Refreshes the cached identity like
  /// [addPeer].
  @override
  Future<bool> removePeer(String fpHex) async {
    if (_isAndroidPlatform) {
      await identityDirReady;
    }
    _requireEngine();
    final fingerprint = fpHex.trim().toLowerCase();
    if (!isFingerprintHex(fingerprint)) {
      throw const FormatException('指纹须为 64 位十六进制字符。');
    }
    try {
      _facade.peerRemove(_identityDir, fingerprint);
    } on AkFfiException catch (error) {
      if (error.code == AkStatus.errNotFound) {
        return false; // not in the book — the page shows the honest case
      }
      rethrow;
    }
    await _refreshIdentityUngated();
    return true;
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
